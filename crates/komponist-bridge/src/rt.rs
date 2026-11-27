//! The bridge RT thread (ARCHITECTURE §8.1, "RT handshake per block").
//!
//! Started per activation by [`RtThread::start`]: it switches itself to realtime scheduling
//! (`SCHED_FIFO` on Linux, the time-constraint policy on macOS), on macOS creates and joins its own
//! audio work interval (#45), starts processing (CLAP wants that on the audio thread) and then
//! serves one block per request:
//! futex-wait on `request_seq` → the parameter values the app set over the control socket
//! ([`RtThread::set_param`]) at offset 0, then the block's input events → `process` (with the
//! block's sidechain input, if the request carries one, #6) → output events into the shared ring →
//! `finish`. [`RtThread::stop`] ends it and hands the processor back for
//! deactivation.
//!
//! **Work interval (macOS, spike #43):** without an audio workgroup, a lightly loaded Apple
//! silicon Mac runs the RT thread on efficiency cores or down-clocked performance cores and blocks
//! come late. The thread therefore creates its own interval (`AudioWorkIntervalCreate`), joins it,
//! and marks every block: `start` from now to the app's deadline
//! ([`komponist_ipc::BlockRequest::deadline_ticks`]) before serving it, `finish` after. Blocks
//! without a deadline (offline render) are not marked. It leaves the interval before it ends.
//!
//! Realtime rules (AGENTS.md rule 4): everything is allocated before the loop; inside it there is
//! no allocation, lock, logging or IO, and the only blocking call is the futex wait with a deadline.
//! Debug builds run the loop inside `assert_no_alloc`, so an allocation aborts the bridge. Plugin
//! log lines from this thread are dropped ([`crate::notify`]).
//!
//! Fallback output (a block the plugin fails, a frame count above the activated maximum, a request
//! that races with a stop): passthrough for effects, silence for instruments; the result carries
//! `PluginError` or `Inactive`.

use std::cell::UnsafeCell;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use komponist_hosting::{EventOut, HostedProcessor, HostingError, ProcessBlock, ProcessStatus};
use komponist_ipc::shm::{
    BLOCK_FLAG_EFFECT_DETECTOR, BLOCK_FLAG_EFFECT_METER, BLOCK_FLAG_RESET, INPUT_EVENT_CAPACITY,
    OUTPUT_EVENT_CAPACITY,
};
use komponist_ipc::{
    BlockRequest, BlockResult, BlockStatus, BridgeServer, WireEvent, WireEventKind,
};
use komponist_rt::WorkInterval;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::notify;

/// How long one futex wait lasts before the loop looks at the stop flag (bounds the time
/// [`RtThread::stop`] takes).
const STOP_POLL: Duration = Duration::from_millis(10);
/// Bound how long the control thread waits for an in-flight plug-in block.
const RESTORE_WAIT: Duration = Duration::from_secs(1);

/// Parameter values queued from the main thread between two blocks.
pub const PARAM_QUEUE_CAPACITY: usize = komponist_ipc::control::MAX_STATE_PARAMETER_BASES;

/// Stack of the RT thread: plugins may use large stack buffers in `process`, so the size of a
/// typical main thread (8 MiB, committed lazily) rather than std's 2 MiB.
const STACK_SIZE: usize = 8 << 20;

/// What the output holds when the plugin did not produce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The input passes through (effects).
    Passthrough,
    /// Silence (instruments).
    Silence,
}

/// Settings of one RT thread.
#[derive(Debug, Clone, Copy)]
pub struct RtConfig {
    /// `SCHED_FIFO` priority (Linux).
    pub priority: i32,
    /// The period of the time-constraint policy (macOS): the activated maximum block at the
    /// activated rate. The bridge does not know the app's callback period; the work interval
    /// carries each block's real deadline.
    pub period: Duration,
    /// The activated maximum block size.
    pub max_frames: u32,
    /// Output for blocks the plugin did not process.
    pub fallback: Fallback,
}

/// What the RT thread reports once it runs (or failed to start processing).
#[derive(Debug)]
pub struct StartReport {
    /// Outcome of switching to realtime scheduling (`SCHED_FIFO` at [`RtConfig::priority`] on
    /// Linux, the time-constraint policy on macOS).
    pub priority: std::io::Result<()>,
    /// The work interval (macOS): `None` where there is none, else whether the thread created and
    /// joined it (the `errno` of a refused join, or 0 if the system created none).
    pub work_interval: Option<Result<(), i32>>,
}

/// A refused start: the processor comes back for deactivation.
pub struct StartFailure {
    /// The processor, not processing.
    pub processor: Box<dyn HostedProcessor>,
    /// The priority outcome (the thread got that far).
    pub report: StartReport,
    /// Why processing did not start.
    pub error: HostingError,
}

/// Latest block value per VST3 parameter. The RT writer stamps values with the control
/// generation it observed at the start of the block. A later control edit invalidates only
/// earlier writes to the affected parameter (or all parameters when restoring a state).
#[derive(Debug)]
pub(crate) struct ControllerValues {
    entries: Vec<Arc<ControllerValue>>,
    signal: Arc<ControllerSignal>,
}

#[derive(Debug)]
struct ControllerSignal {
    generation: AtomicU64,
    dirty: AtomicBool,
    notify: Arc<notify::BridgeNotify>,
}

#[derive(Debug)]
struct ControllerValue {
    id: u32,
    /// A sequence lock: odd while the single RT writer stores the generation and value.
    sequence: AtomicU64,
    published_generation: AtomicU64,
    value: AtomicU64,
    /// These two fields are owned by the main-thread consumer.
    consumed: AtomicU64,
    minimum_generation: AtomicU64,
}

impl ControllerValue {
    fn new(id: u32) -> Self {
        Self {
            id,
            sequence: AtomicU64::new(0),
            published_generation: AtomicU64::new(0),
            value: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            minimum_generation: AtomicU64::new(0),
        }
    }

    fn publish(&self, generation: u64, value: f64) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        self.sequence
            .store(sequence.wrapping_add(1), Ordering::SeqCst);
        self.published_generation
            .store(generation, Ordering::SeqCst);
        self.value.store(value.to_bits(), Ordering::SeqCst);
        self.sequence
            .store(sequence.wrapping_add(2), Ordering::SeqCst);
    }

    fn take(&self) -> Option<(u32, f64)> {
        let sequence = self.sequence.load(Ordering::SeqCst);
        if sequence & 1 != 0 || sequence == self.consumed.load(Ordering::Relaxed) {
            return None;
        }
        let generation = self.published_generation.load(Ordering::SeqCst);
        let value = self.value.load(Ordering::SeqCst);
        if self.sequence.load(Ordering::SeqCst) != sequence {
            return None;
        }
        self.consumed.store(sequence, Ordering::Relaxed);
        (generation >= self.minimum_generation.load(Ordering::Relaxed))
            .then(|| (self.id, f64::from_bits(value)))
    }
}

impl ControllerValues {
    pub(crate) fn new(mut ids: Vec<u32>, notify: Arc<notify::BridgeNotify>) -> Self {
        ids.sort_unstable();
        ids.dedup();
        Self {
            entries: ids
                .into_iter()
                .map(|id| Arc::new(ControllerValue::new(id)))
                .collect(),
            signal: Arc::new(ControllerSignal {
                generation: AtomicU64::new(0),
                dirty: AtomicBool::new(false),
                notify,
            }),
        }
    }

    fn with_ids(&self, ids: &[u32]) -> Self {
        Self {
            entries: ids
                .iter()
                .map(|&id| {
                    self.entry(id)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(ControllerValue::new(id)))
                })
                .collect(),
            signal: self.signal.clone(),
        }
    }

    fn entry(&self, id: u32) -> Option<&Arc<ControllerValue>> {
        self.entries
            .binary_search_by_key(&id, |slot| slot.id)
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Invalidate earlier publications for one parameter, or all of them after a state load.
    pub(crate) fn invalidate(&self, id: Option<u32>) {
        let generation = self.signal.generation.fetch_add(1, Ordering::AcqRel) + 1;
        match id {
            Some(id) => {
                if let Some(slot) = self.entry(id) {
                    slot.minimum_generation.store(generation, Ordering::Relaxed);
                }
            }
            None => {
                for slot in &self.entries {
                    slot.minimum_generation.store(generation, Ordering::Relaxed);
                }
            }
        }
    }

    fn publish(&self, generation: u64, input: &[WireEvent], output: &[(u32, f64)]) {
        let mut changed = false;
        let mut publish = |id, value: f64| {
            if value.is_finite()
                && let Some(slot) = self.entry(id)
            {
                slot.publish(generation, value);
                changed = true;
            }
        };
        for event in input {
            if let Some(WireEventKind::ParamValue { id, value }) = event.decode()
                && output.binary_search_by_key(&id, |&(id, _)| id).is_err()
            {
                publish(id, value);
            }
        }
        // An output from this block is authoritative for that parameter. Never publish a
        // mirrored input first, since the control thread can already be draining these slots.
        for &(id, value) in output {
            publish(id, value);
        }
        if changed && !self.signal.dirty.swap(true, Ordering::Release) {
            self.signal.notify.controller_values_ready();
        }
    }

    pub(crate) fn drain(&self) -> impl Iterator<Item = (u32, f64)> + '_ {
        self.signal.dirty.swap(false, Ordering::Acquire);
        self.entries.iter().filter_map(|slot| slot.take())
    }
}

/// The current audio block did not finish within [`RESTORE_WAIT`]; nothing was paused.
#[derive(Debug)]
pub(crate) struct PauseTimeout;

/// A running RT thread with its processor.
#[derive(Debug)]
pub struct RtThread {
    handle: JoinHandle<Box<dyn HostedProcessor>>,
    stop: Arc<AtomicBool>,
    params: Producer<WireEvent>,
    parameters: Arc<ParameterTable>,
    parameter_updates: Producer<Arc<ParameterTable>>,
    retired_parameters: Consumer<Arc<ParameterTable>>,
    parameter_update_pending: bool,
    sync: Arc<RtParamSync>,
}

/// Successful audio input and plugin output carry different evidence. A report from the same
/// block can describe how the plugin rounded an input. A later report supersedes that evidence.
#[derive(Debug, Default)]
pub(crate) struct ParamDelivery {
    input: Option<(u32, f64)>,
    report: Option<(u32, f64)>,
    /// The plugin's last reported value. Unlike `report`, an input does not clear it, so an echo
    /// of an unchanged value after a repeated input is not counted as a change.
    last_reported: Option<f64>,
    /// The last input value that was counted as a change: like `last_reported`, the baseline moves
    /// only with a counted change, so small input steps cannot creep past the tolerance.
    last_counted_input: Option<f64>,
    /// Values this parameter took since the table was created: each input or report that
    /// differs from the previous one of its kind. Read under the pause, so a snapshot can tell
    /// that a value left and came back while the plugin serialized (#215).
    changes: u32,
}

impl ParamDelivery {
    fn record_input(&mut self, block: u32, value: f64) {
        if self
            .last_counted_input
            .is_none_or(|previous| !same_param_value(value, previous))
        {
            self.changes = self.changes.wrapping_add(1);
            self.last_counted_input = Some(value);
        }
        self.input = Some((block, value));
        self.report = None;
    }

    fn record_report(&mut self, block: u32, value: f64) {
        // The baseline moves only with a counted change, so small steps cannot drift past the
        // tolerance unnoticed.
        if self
            .last_reported
            .is_none_or(|previous| !same_param_value(value, previous))
        {
            self.changes = self.changes.wrapping_add(1);
            self.last_reported = Some(value);
        }
        // Some plugins repeat their last output. Keep the block that established that value.
        if self
            .report
            .is_none_or(|(_, previous)| !same_param_value(value, previous))
        {
            self.report = Some((block, value));
        }
    }

    pub fn confirms_input(&self, expected: f64, actual: f64) -> bool {
        let Some((block, value)) = self.input else {
            return false;
        };
        value == expected
            && self.report.is_none_or(|(reported_in, reported)| {
                reported_in == block && same_param_value(actual, reported)
            })
    }
}

pub(crate) fn same_param_value(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-6 * expected.abs().max(1.0)
}

#[derive(Debug, Default)]
struct DeliverySlot(UnsafeCell<ParamDelivery>, AtomicU64);

/// A successful durable write's serial and preset epoch, read as one atomic value.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ParamEditStamp(u64);

impl ParamEditStamp {
    pub(crate) fn new(serial: NonZeroU32, epoch: u64) -> Self {
        Self((u64::from(epoch as u32) << 32) | u64::from(serial.get()))
    }

    pub(crate) fn superseded_by(self, epoch: u64) -> Option<NonZeroU32> {
        let delta = (epoch as u32).wrapping_sub((self.0 >> 32) as u32);
        if delta == 0 || delta >= (1 << 31) {
            return None;
        }
        NonZeroU32::new(self.0 as u32)
    }
}

pub(crate) struct StoppedRt {
    pub processor: Box<dyn HostedProcessor>,
    pub queued: Vec<WireEvent>,
    pub durable: BTreeMap<u32, ParamEditStamp>,
}

// SAFETY: Slot contents are accessed only under the activity guard of their owning RT
// thread, including during snapshot and restore. Rescans only clone or drop their Arcs.
unsafe impl Sync for DeliverySlot {}

#[derive(Debug)]
struct ParameterTable {
    delivered: BTreeMap<u32, Arc<DeliverySlot>>,
    controller: Option<Arc<ControllerValues>>,
}

/// A read-only view of delivery evidence while audio processing is paused.
pub(crate) struct DeliveredParams<'a>(&'a BTreeMap<u32, Arc<DeliverySlot>>);

impl DeliveredParams<'_> {
    pub fn confirms_input(&self, id: u32, expected: f64, actual: f64) -> bool {
        self.0.get(&id).is_some_and(|slot| {
            // SAFETY: The view cannot outlive the exclusive activity guard.
            unsafe { &*slot.0.get() }.confirms_input(expected, actual)
        })
    }
}

impl DeliveredParams<'_> {
    /// How often the value of `id` changed so far (see [`ParamDelivery`]); 0 for an unknown ID.
    pub fn changes(&self, id: u32) -> u32 {
        self.0.get(&id).map_or(0, |slot| {
            // SAFETY: The view cannot outlive the exclusive activity guard.
            unsafe { &*slot.0.get() }.changes
        })
    }
}

#[derive(Debug)]
struct RtParamSync {
    // The low bits are the activity flags. The upper bits count successful state restores.
    activity: AtomicU64,
    params: UnsafeCell<Consumer<WireEvent>>,
    preset_epoch: Option<Arc<AtomicU64>>,
    parameters: UnsafeCell<Arc<ParameterTable>>,
    restore_fence: UnsafeCell<RestoreFence>,
    parameter_updates: UnsafeCell<Consumer<Arc<ParameterTable>>>,
    retired_parameters: UnsafeCell<Producer<Arc<ParameterTable>>>,
    #[cfg(test)]
    dequeue_probe: DequeueProbe,
}

#[derive(Debug, Default)]
struct RestoreFence {
    replayed_generation: u64,
    superseded: BTreeMap<u32, u64>,
}

// SAFETY: The live table, restore fence and the RT ends of the queues are only accessed through an
// Activity guard. Processing and a completed restore pause are mutually exclusive.
unsafe impl Sync for RtParamSync {}

const PROCESSING: u64 = 1;
const RESTORING: u64 = 2;
/// A checked snapshot's short capture. Blocks are answered with the fallback meanwhile, but
/// no state is replaced, so nothing needs a voice reset.
const SNAPSHOT: u64 = 4;
const ACTIVITY_BITS: u64 = PROCESSING | RESTORING | SNAPSHOT;
const RESTORE_GENERATION_STEP: u64 = ACTIVITY_BITS + 1;

struct Activity<'a> {
    sync: &'a RtParamSync,
    bit: u64,
    replaced_state: bool,
}

impl Activity<'_> {
    fn params(&mut self) -> &mut Consumer<WireEvent> {
        // SAFETY: Successful acquisition excludes every other consumer accessor, and this
        // borrow cannot outlive the guard.
        unsafe { &mut *self.sync.params.get() }
    }

    fn parameters(&self) -> &ParameterTable {
        // SAFETY: This guard excludes both audio processing and restore from other threads.
        unsafe { &*self.sync.parameters.get() }
    }

    fn restore_fence(&mut self) -> &mut RestoreFence {
        // SAFETY: This activity guard excludes audio and the control thread from each other.
        unsafe { &mut *self.sync.restore_fence.get() }
    }

    fn mark_restored_parameters(&mut self) {
        let generation = (self.sync.activity.load(Ordering::Relaxed) & !ACTIVITY_BITS)
            .wrapping_add(RESTORE_GENERATION_STEP);
        let sync = self.sync;
        // SAFETY: This guard is exclusive. The table and fence are separate fields.
        let fence = unsafe { &mut *sync.restore_fence.get() };
        fence
            .superseded
            .retain(|_, generation| *generation > fence.replayed_generation);
        for &id in self.parameters().delivered.keys() {
            fence.superseded.insert(id, generation);
        }
    }

    fn install_parameter_update(&mut self) {
        // SAFETY: The guard gives exclusive access to each RT endpoint and the live table.
        let updates = unsafe { &mut *self.sync.parameter_updates.get() };
        if let Ok(next) = updates.pop() {
            let previous = std::mem::replace(unsafe { &mut *self.sync.parameters.get() }, next);
            let retired = unsafe { &mut *self.sync.retired_parameters.get() };
            // The control thread never sends another table before taking the retired one.
            if let Err(rtrb::PushError::Full(previous)) = retired.push(previous) {
                // This is unreachable under that protocol. Never free a table on the RT thread.
                std::mem::forget(previous);
            }
        }
    }

    fn update_delivery(&mut self, id: u32, update: impl FnOnce(&mut ParamDelivery)) {
        if let Some(slot) = self.parameters().delivered.get(&id) {
            // SAFETY: The guard is exclusive and the mutable borrow prevents an active view.
            update(unsafe { &mut *slot.0.get() });
        }
    }
}

impl Drop for Activity<'_> {
    fn drop(&mut self) {
        if self.replaced_state {
            debug_assert_eq!(self.bit, RESTORING);
            // End the pause and publish the new generation in the same atomic step.
            self.sync
                .activity
                .fetch_add(RESTORE_GENERATION_STEP - RESTORING, Ordering::Release);
        } else {
            self.sync.activity.fetch_and(!self.bit, Ordering::Release);
        }
    }
}

impl RtParamSync {
    /// RT never waits for the control thread. A refused block carries the activity and restore
    /// generation it encountered, so its deferred writes can be checked when audio resumes.
    fn try_process(&self) -> Result<Activity<'_>, u64> {
        let state = self.activity.load(Ordering::Acquire);
        if state & ACTIVITY_BITS != 0 {
            return Err(state);
        }
        self.activity
            .compare_exchange(
                state,
                state | PROCESSING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .map(|_| Activity {
                sync: self,
                bit: PROCESSING,
                replaced_state: false,
            })
            .map_err(|observed| {
                if (observed ^ state) & !ACTIVITY_BITS != 0 {
                    // A restore completed while claiming this block. Its writes predate it.
                    state | RESTORING
                } else {
                    observed
                }
            })
    }

    /// Prevent a new block from staging parameter edits while the control thread waits for
    /// the current block to finish. Never load state if a stalled processor still owns it.
    fn pause_for_restore(&self) -> Option<Activity<'_>> {
        self.pause(RESTORING)
    }

    /// Like [`Self::pause_for_restore`] for a snapshot capture.
    fn pause_for_snapshot(&self) -> Option<Activity<'_>> {
        self.pause(SNAPSHOT)
    }

    fn pause(&self, bit: u64) -> Option<Activity<'_>> {
        self.activity.fetch_or(bit, Ordering::AcqRel);
        let guard = Activity {
            sync: self,
            bit,
            replaced_state: false,
        };
        let deadline = Instant::now() + RESTORE_WAIT;
        while self.activity.load(Ordering::Acquire) & PROCESSING != 0 {
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_micros(100));
        }
        Some(guard)
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct DequeueProbe {
    armed: AtomicBool,
    entered: AtomicBool,
    proceed: AtomicBool,
}

#[cfg(test)]
impl DequeueProbe {
    fn wait_if_armed(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            self.entered.store(true, Ordering::Release);
            let deadline = Instant::now() + RESTORE_WAIT;
            while !self.proceed.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::yield_now();
            }
        }
    }
}

/// Buffers the loop works in, allocated before it starts.
struct Scratch {
    events_in: Vec<WireEvent>,
    events_out: Vec<WireEvent>,
    reset_after_restore: bool,
    retry_params: Vec<DeferredParam>,
    retry_indices: HashMap<u32, usize>,
}

struct DeferredParam {
    id: u32,
    generation: u64,
    event: WireEvent,
}

impl RtThread {
    /// Spawns the RT thread for `processor` (activated, not processing yet) and waits until it
    /// has set its priority and started processing. On a refused start the processor comes back
    /// with the error.
    pub fn start(
        server: BridgeServer,
        mut processor: Box<dyn HostedProcessor>,
        config: RtConfig,
        controller: Option<Arc<ControllerValues>>,
        param_ids: Vec<u32>,
        preset_epoch: Option<Arc<AtomicU64>>,
    ) -> Result<(RtThread, StartReport), StartFailure> {
        let (params_tx, params_rx) = RingBuffer::new(PARAM_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        if controller.is_some() {
            processor.use_host_controller_sync();
        }
        // Exactly one table is in flight, including the trip back for destruction.
        let (updates_tx, updates_rx) = RingBuffer::new(1);
        let (retired_tx, retired_rx) = RingBuffer::new(1);
        let parameters = Arc::new(ParameterTable {
            delivered: param_ids
                .into_iter()
                .map(|id| (id, Arc::new(DeliverySlot::default())))
                .collect(),
            controller,
        });
        let sync = Arc::new(RtParamSync {
            activity: AtomicU64::new(0),
            params: UnsafeCell::new(params_rx),
            preset_epoch,
            parameters: UnsafeCell::new(parameters.clone()),
            restore_fence: UnsafeCell::new(RestoreFence::default()),
            parameter_updates: UnsafeCell::new(updates_rx),
            retired_parameters: UnsafeCell::new(retired_tx),
            #[cfg(test)]
            dequeue_probe: DequeueProbe::default(),
        });
        let worker_sync = sync.clone();
        let (report_tx, report_rx) = mpsc::sync_channel(1);
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("kp-bridge-rt".into())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                rt_main(
                    server,
                    processor,
                    &thread_stop,
                    config,
                    report_tx,
                    &worker_sync,
                )
            })
            .expect("spawn the RT thread");
        let started = report_rx.recv();
        match started {
            Ok((report, Ok(()))) => Ok((
                RtThread {
                    handle,
                    stop,
                    params: params_tx,
                    parameters,
                    parameter_updates: updates_tx,
                    retired_parameters: retired_rx,
                    parameter_update_pending: false,
                    sync,
                },
                report,
            )),
            Ok((report, Err(error))) => {
                let processor = join(handle);
                Err(StartFailure {
                    processor,
                    report,
                    error,
                })
            }
            // The thread died before reporting: only a panic does that, and the panic hook
            // aborts the bridge first.
            Err(_) => unreachable!("the RT thread ended without a report"),
        }
    }

    /// Queues a parameter value for the next block (offset 0). `false` if the queue is full.
    pub fn set_param(&mut self, id: u32, value: f64) -> bool {
        self.set_param_edit(id, value, None)
    }

    pub fn set_param_edit(&mut self, id: u32, value: f64, serial: Option<NonZeroU32>) -> bool {
        let mut event = WireEvent::new(0, WireEventKind::ParamValue { id, value });
        event.reserved = serial.map_or(0, NonZeroU32::get);
        self.params.push(event).is_ok()
    }

    pub(crate) fn durable_deliveries(&self) -> impl Iterator<Item = (u32, ParamEditStamp)> + '_ {
        self.parameters.delivered.iter().filter_map(|(&id, slot)| {
            let stamp = slot.1.load(Ordering::Acquire);
            (stamp != 0).then_some((id, ParamEditStamp(stamp)))
        })
    }

    pub fn has_queued_params(&self) -> bool {
        self.params.slots() < PARAM_QUEUE_CAPACITY
    }

    pub fn controller(&self) -> Option<Arc<ControllerValues>> {
        self.parameters.controller.clone()
    }

    /// Prepares parameter entries on the control thread. Audio takes the table between blocks.
    pub fn refresh_parameter_ids(&mut self, mut ids: Vec<u32>) -> bool {
        if self.retired_parameters.pop().is_ok() {
            self.parameter_update_pending = false;
        }
        ids.sort_unstable();
        ids.dedup();
        if self
            .parameters
            .delivered
            .keys()
            .copied()
            .eq(ids.iter().copied())
        {
            return true;
        }
        if self.parameter_update_pending {
            return false;
        }
        let controller = self
            .parameters
            .controller
            .as_ref()
            .map(|values| Arc::new(values.with_ids(&ids)));
        let delivered = ids
            .into_iter()
            .map(|id| {
                let slot = self
                    .parameters
                    .delivered
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                (id, slot)
            })
            .collect();
        let next = Arc::new(ParameterTable {
            delivered,
            controller,
        });
        self.parameter_updates
            .push(next.clone())
            .expect("one parameter table in flight");
        self.parameters = next;
        self.parameter_update_pending = true;
        true
    }

    /// Runs `capture` at a block boundary: after any in-flight block has finished applying its
    /// parameters, with new blocks answered by the fallback until it returns. Keep it short; a
    /// state save belongs between two captures, not inside one.
    pub fn snapshot<T>(
        &mut self,
        capture: impl FnOnce(DeliveredParams<'_>) -> T,
    ) -> Result<T, PauseTimeout> {
        let mut pause = self.sync.pause_for_snapshot().ok_or(PauseTimeout)?;
        pause.install_parameter_update();
        Ok(capture(DeliveredParams(&pause.parameters().delivered)))
    }

    /// Serializes a state load with processing. A new parameter state supersedes queued edits.
    pub fn restore<T>(&mut self, load: impl FnOnce() -> (T, bool)) -> Result<(T, bool), String> {
        let mut pause = self
            .sync
            .pause_for_restore()
            .ok_or_else(|| "timed out waiting for the current audio block".to_owned())?;
        pause.install_parameter_update();
        let result = load();
        if result.1 {
            pause.mark_restored_parameters();
            while pause.params().pop().is_ok() {}
            for slot in pause.parameters().delivered.values() {
                // SAFETY: The pause excludes audio access while clearing the delivery evidence.
                unsafe { *slot.0.get() = ParamDelivery::default() };
                slot.1.store(0, Ordering::Release);
            }
            if let Some(controller) = &pause.parameters().controller {
                controller.invalidate(None);
            }
            pause.replaced_state = true;
        }
        Ok(result)
    }

    /// Stops the thread (within about [`STOP_POLL`]) and returns the stopped processor and the
    /// parameter values queued after the last block.
    pub fn stop(self) -> StoppedRt {
        self.stop.store(true, Ordering::Release);
        let durable = Arc::clone(&self.parameters);
        let processor = join(self.handle);
        let mut guard = self.sync.try_process().expect("stopped RT thread");
        let queued = std::iter::from_fn(|| guard.params().pop().ok()).collect();
        let durable = durable
            .delivered
            .iter()
            .filter_map(|(&id, slot)| {
                let stamp = slot.1.load(Ordering::Acquire);
                (stamp != 0).then_some((id, ParamEditStamp(stamp)))
            })
            .collect();
        StoppedRt {
            processor,
            queued,
            durable,
        }
    }
}

fn join(handle: JoinHandle<Box<dyn HostedProcessor>>) -> Box<dyn HostedProcessor> {
    // A panic on the RT thread aborts the bridge through the panic hook before this is reached.
    handle.join().unwrap_or_else(|_| std::process::abort())
}

fn rt_main(
    mut server: BridgeServer,
    mut processor: Box<dyn HostedProcessor>,
    stop: &AtomicBool,
    config: RtConfig,
    report: mpsc::SyncSender<(StartReport, Result<(), HostingError>)>,
    sync: &RtParamSync,
) -> Box<dyn HostedProcessor> {
    notify::mark_rt_thread();
    let priority = komponist_rt::set_current_thread_realtime(config.period, config.priority);
    // Created here, not in the loop: creating allocates. `None` outside macOS.
    let interval = WorkInterval::new(c"komponist-bridge");
    let joined = interval.as_ref().map(WorkInterval::join);
    let work_interval = if cfg!(target_os = "macos") {
        Some(match &joined {
            Some(Ok(_)) => Ok(()),
            Some(Err(errno)) => Err(*errno),
            None => Err(0),
        })
    } else {
        None
    };
    let (interval, token) = match (interval, joined) {
        (Some(interval), Some(Ok(token))) => (Some(interval), Some(token)),
        _ => (None, None),
    };
    let started = processor.start_processing();
    let ok = started.is_ok();
    let mut scratch = Scratch {
        events_in: vec![WireEvent::default(); PARAM_QUEUE_CAPACITY + 2 * INPUT_EVENT_CAPACITY],
        events_out: vec![WireEvent::default(); OUTPUT_EVENT_CAPACITY],
        reset_after_restore: false,
        retry_params: Vec::with_capacity(INPUT_EVENT_CAPACITY),
        retry_indices: HashMap::with_capacity(INPUT_EVENT_CAPACITY),
    };
    if ok {
        // Before the report: once it arrives the main thread publishes `Active`, and every request
        // after that is a fresh one.
        serve_stale(&mut server, config.fallback);
    }
    // The receiver waits for exactly this message.
    let _ = report.send((
        StartReport {
            priority,
            work_interval,
        },
        started,
    ));
    drop(report);
    if ok {
        assert_no_alloc::assert_no_alloc(|| {
            while !stop.load(Ordering::Acquire) {
                if let Some(request) = server.wait_request(Some(Instant::now() + STOP_POLL)) {
                    // From now to the app's deadline; a block without one is not a cycle.
                    let cycle = interval.as_ref().is_some_and(|interval| {
                        request.deadline_ticks != 0
                            && interval.start_now(request.deadline_ticks).is_ok()
                    });
                    serve(
                        &mut server,
                        processor.as_mut(),
                        &mut scratch,
                        &request,
                        &config,
                        sync,
                    );
                    if cycle && let Some(interval) = &interval {
                        let _ = interval.finish();
                    }
                }
            }
            serve_stale(&mut server, config.fallback);
        });
        processor.stop_processing();
    }
    if let (Some(interval), Some(token)) = (&interval, token) {
        interval.leave(token);
    }
    processor
}

/// Answers a request that is already waiting with the fallback and `Inactive`: one published
/// while no RT thread ran (racing with a deactivation), or one that raced with the stop.
fn serve_stale(server: &mut BridgeServer, fallback: Fallback) {
    if let Some(request) = server.wait_request(Some(Instant::now())) {
        write_fallback(server, request.frames as usize, fallback);
        server.finish(result(BlockStatus::Inactive, 0));
    }
}

fn result(status: BlockStatus, dropped_events: u32) -> BlockResult {
    BlockResult {
        seq: 0,
        status: status as u32,
        dropped_events,
        reserved: 0,
    }
}

fn write_fallback(server: &mut BridgeServer, frames: usize, fallback: Fallback) {
    let (input, output) = server.audio(frames);
    for (out, inp) in output.into_iter().zip(input) {
        match fallback {
            Fallback::Passthrough => out.copy_from_slice(inp),
            Fallback::Silence => out.fill(0.0),
        }
    }
}

/// One block (step 2 of the handshake).
fn serve(
    server: &mut BridgeServer,
    processor: &mut dyn HostedProcessor,
    scratch: &mut Scratch,
    request: &BlockRequest,
    config: &RtConfig,
    sync: &RtParamSync,
) {
    let mut processing = match sync.try_process() {
        Ok(processing) => processing,
        Err(observed) => {
            let generation = observed & !ACTIVITY_BITS;
            let activity = observed & ACTIVITY_BITS;
            // A restore replaces the plugin's state, so recovery clears its voices. A snapshot pause
            // changes nothing: reset only if a note event is discarded with this block (a missed
            // note-off could leave a voice hanging). Inactive blocks never reach this function.
            let snapshot_only = activity != 0 && activity & !SNAPSHOT == 0;
            // A reset the client asked for with this block is applied by the next processed block
            // (the client does not repeat it after an `Inactive` reply).
            scratch.reset_after_restore |= !snapshot_only || request.flags & BLOCK_FLAG_RESET != 0;
            // The client may already have timed out. Retain the latest parameter values here
            // so one-shot writes survive even if the client never consumes this reply.
            while let Some(event) = server.pop_event() {
                if !matches!(event.decode(), Some(WireEventKind::ParamValue { .. })) {
                    scratch.reset_after_restore = true;
                }
                if let Some(WireEventKind::ParamValue { id, .. }) = event.decode() {
                    let retry = DeferredParam {
                        id,
                        generation,
                        event: WireEvent { offset: 0, ..event },
                    };
                    if let Some(&index) = scratch.retry_indices.get(&id) {
                        scratch.retry_params[index] = retry;
                    } else if scratch.retry_params.len() < INPUT_EVENT_CAPACITY {
                        scratch.retry_indices.insert(id, scratch.retry_params.len());
                        scratch.retry_params.push(retry);
                    }
                }
            }
            write_fallback(server, request.frames as usize, config.fallback);
            server.finish(result(BlockStatus::Inactive, 0));
            return;
        }
    };
    let frames = request.frames as usize;
    if request.frames == 0 || request.frames > config.max_frames {
        write_fallback(server, frames, config.fallback);
        server.finish(result(BlockStatus::PluginError, 0));
        return;
    }
    let controller = processing.parameters().controller.as_deref();
    // Snapshot before accepting block input. Replacing the table preserves its generation. A control edit racing with this block can still
    // supersede its controller notifications when the audio thread completes later.
    let generation =
        controller.map_or(0, |values| values.signal.generation.load(Ordering::Acquire));
    #[cfg(test)]
    sync.dequeue_probe.wait_if_armed();
    // Values from `SetParam` first: offset 0 keeps the list sorted.
    let mut count = 0;
    while count < PARAM_QUEUE_CAPACITY {
        let Ok(event) = processing.params().pop() else {
            break;
        };
        scratch.events_in[count] = event;
        count += 1;
    }
    let queued = count;
    // Check the IDs known to a successful restore, even when a rescan has installed a new table.
    let restore_generation = sync.activity.load(Ordering::Relaxed) & !ACTIVITY_BITS;
    let fence = processing.restore_fence();
    for retry in &scratch.retry_params {
        if fence
            .superseded
            .get(&retry.id)
            .is_none_or(|&restored| restored <= retry.generation)
        {
            scratch.events_in[count] = retry.event;
            count += 1;
        }
    }
    fence.replayed_generation = restore_generation;
    scratch.retry_params.clear();
    scratch.retry_indices.clear();
    while let Some(event) = server.pop_event() {
        scratch.events_in[count] = event;
        count += 1;
    }
    // A rescan is published before writes for its new IDs. Install after both input queues
    // have been read so this block can record those writes and publish controller updates.
    processing.install_parameter_update();

    let after_restore = std::mem::take(&mut scratch.reset_after_restore);
    if after_restore || request.flags & BLOCK_FLAG_RESET != 0 {
        processor.reset();
    }

    let capture_meter = request.flags & BLOCK_FLAG_EFFECT_METER != 0;
    if capture_meter {
        processor.begin_effect_meter(request.flags & BLOCK_FLAG_EFFECT_DETECTOR != 0);
    }
    let taps = request.signal_taps();
    if taps != 0 {
        processor.begin_signal_taps(taps);
    }
    let (status, produced, mut dropped) = {
        let (input, sidechain, output) = server.audio_with_sidechain(frames);
        let mut block = ProcessBlock {
            frames: request.frames,
            input,
            sidechain,
            output,
            events_in: &scratch.events_in[..count],
            events_out: EventOut::new(&mut scratch.events_out),
            transport: &request.transport,
            steady_time: request.steady_time,
        };
        let status = processor.process(&mut block);
        (
            status,
            block.events_out.as_slice().len(),
            block.events_out.dropped(),
        )
    };
    let meter = capture_meter
        .then(|| processor.end_effect_meter())
        .flatten();
    for event in &scratch.events_out[..produced] {
        if !server.push_output_event(WireEvent {
            reserved: request.seq,
            ..*event
        }) {
            dropped += 1;
        }
    }
    let status = match status {
        ProcessStatus::Continue | ProcessStatus::Sleep => BlockStatus::Ok,
        ProcessStatus::Error => {
            write_fallback(server, frames, config.fallback);
            BlockStatus::PluginError
        }
    };
    if status == BlockStatus::Ok {
        // An overlapping block enters the new epoch. Its edits are kept pending because
        // the plugin may have applied them after reporting the preset.
        let epoch = sync
            .preset_epoch
            .as_ref()
            .map(|epoch| epoch.load(Ordering::Acquire));
        for event in &scratch.events_in[..count] {
            if let Some(WireEventKind::ParamValue { id, value }) = event.decode() {
                processing.update_delivery(id, |last| last.record_input(request.seq, value));
                if let (Some(epoch), Some(serial)) = (epoch, NonZeroU32::new(event.reserved))
                    && let Some(slot) = processing.parameters().delivered.get(&id)
                {
                    slot.1
                        .store(ParamEditStamp::new(serial, epoch).0, Ordering::Release);
                }
            }
        }
        for event in &scratch.events_out[..produced] {
            if let Some(WireEventKind::ParamValue { id, value }) = event.decode() {
                processing.update_delivery(id, |last| last.record_report(request.seq, value));
            }
        }
        let controller_updates = processor.controller_updates();
        for &(id, value) in controller_updates {
            processing.update_delivery(id, |last| last.record_report(request.seq, value));
        }
        if let Some(controller) = processing.parameters().controller.as_deref() {
            controller.publish(
                generation,
                &scratch.events_in[queued..count],
                controller_updates,
            );
        }
    }
    let wrote_signal_taps = taps != 0
        && status == BlockStatus::Ok
        && server.signal_taps_mut().is_some_and(|out| {
            processor.read_signal_taps(taps, out);
            true
        });
    server.finish_with_telemetry(result(status, dropped), meter, wrote_signal_taps);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_supersession_is_bound_to_the_delivered_serial_and_epoch() {
        let first = NonZeroU32::new(1).unwrap();
        let repeated = NonZeroU32::new(2).unwrap();
        let before = ParamEditStamp::new(first, 4);
        assert_eq!(before.superseded_by(5), Some(first));

        // A block that completed after the report (even if it began earlier) belongs to
        // the report's epoch. A repeated value with a new serial remains a separate edit.
        let straddled = ParamEditStamp::new(repeated, 5);
        assert_eq!(straddled.superseded_by(5), None);
        assert_eq!(straddled.superseded_by(6), Some(repeated));
        assert_eq!(straddled.superseded_by(4), None);
        assert_eq!(
            ParamEditStamp::new(first, u64::from(u32::MAX)).superseded_by(1_u64 << 32),
            Some(first)
        );
    }

    #[test]
    fn a_later_plugin_report_supersedes_input_evidence() {
        let mut delivery = ParamDelivery::default();
        delivery.record_input(1, 0.403);
        delivery.record_report(1, 0.4);
        assert!(delivery.confirms_input(0.403, 0.4));
        delivery.record_report(2, 0.4);
        assert!(delivery.confirms_input(0.403, 0.4));
        delivery.record_report(3, 0.8);
        assert!(!delivery.confirms_input(0.403, 0.8));
    }

    /// The host repeating an unchanged value and the plugin echoing it is no change: only values
    /// that differ from the previous one of their kind count (#215 review).
    #[test]
    fn repeated_inputs_and_echoes_count_only_real_changes() {
        let mut delivery = ParamDelivery::default();
        delivery.record_input(1, 0.403);
        delivery.record_report(1, 0.4);
        let first = delivery.changes;
        for block in 2..10 {
            delivery.record_input(block, 0.403);
            delivery.record_report(block, 0.4);
        }
        assert_eq!(delivery.changes, first, "echoes of an unchanged value");
        delivery.record_report(10, 0.8);
        assert_eq!(delivery.changes, first + 1, "a new reported value");
        delivery.record_input(11, 0.9);
        assert_eq!(delivery.changes, first + 2, "a new input");
        assert!(!delivery.confirms_input(0.403, 0.4));
    }

    /// Steps inside the tolerance that add up to a change beyond it are counted: the baseline
    /// moves only when a change is counted.
    #[test]
    fn creeping_reports_are_counted_once_they_leave_the_tolerance() {
        let mut delivery = ParamDelivery::default();
        delivery.record_report(1, 0.4);
        let first = delivery.changes;
        let mut value = 0.4;
        for block in 2..200 {
            value += 1e-7;
            delivery.record_report(block, value);
        }
        assert!(
            delivery.changes > first,
            "a drift beyond the tolerance counts"
        );

        // The same for inputs a plugin accepts without reporting them.
        let mut delivery = ParamDelivery::default();
        delivery.record_input(1, 0.4);
        let first = delivery.changes;
        let mut value = 0.4;
        for block in 2..200 {
            value += 1e-7;
            delivery.record_input(block, value);
        }
        assert!(
            delivery.changes > first,
            "an input drift beyond the tolerance counts"
        );
    }

    #[test]
    fn controller_updates_coalesce_without_allocating() {
        let notify = Arc::new(notify::BridgeNotify::new().unwrap());
        let values = ControllerValues::new(vec![2, 1], notify.clone());
        let event = |id, value| WireEvent::new(0, WireEventKind::ParamValue { id, value });
        assert_no_alloc::assert_no_alloc(|| {
            values.publish(
                0,
                &[event(2, 0.3), event(1, 0.4), event(2, 0.7), event(99, 0.1)],
                &[],
            );
        });
        assert_ne!(notify.take().0 & notify::CONTROLLER_VALUES_PENDING, 0);
        assert_eq!(values.drain().collect::<Vec<_>>(), [(1, 0.4), (2, 0.7)]);
        assert_eq!(values.drain().count(), 0);
    }
    #[test]
    fn processor_outputs_replace_mirrored_inputs_for_the_same_parameter() {
        let notify = Arc::new(notify::BridgeNotify::new().unwrap());
        let values = ControllerValues::new(vec![1, 2, 3], notify);
        let input = [
            WireEvent::new(0, WireEventKind::ParamValue { id: 1, value: 0.75 }),
            WireEvent::new(0, WireEventKind::ParamValue { id: 2, value: 0.25 }),
        ];
        assert_no_alloc::assert_no_alloc(|| values.publish(0, &input, &[(1, 0.5), (3, 0.1)]));
        assert_eq!(
            values.drain().collect::<Vec<_>>(),
            [(1, 0.5), (2, 0.25), (3, 0.1)]
        );
    }

    /// #215: a block that meets a snapshot pause is answered with the fallback. It asks for a
    /// voice reset only when it discards a note event (a restore keeps asking always), and the
    /// pause sees how often a parameter changed, including a change that came back.
    #[test]
    fn snapshot_pauses_reset_voices_only_after_a_discarded_note_and_count_changes() {
        use komponist_ipc::shm::{BlockSpec, BridgeState};
        use komponist_ipc::{BridgeClient, ShmRegion, WireTransport};

        struct Probe(Arc<AtomicU64>);

        impl HostedProcessor for Probe {
            fn start_processing(&mut self) -> Result<(), HostingError> {
                Ok(())
            }

            fn stop_processing(&mut self) {}

            fn reset(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }

            fn process(&mut self, _block: &mut ProcessBlock<'_>) -> ProcessStatus {
                ProcessStatus::Continue
            }
        }

        let resets = Arc::new(AtomicU64::new(0));
        let region = Arc::new(ShmRegion::create().unwrap());
        let mut client = BridgeClient::new(region.clone());
        let notify = Arc::new(notify::BridgeNotify::new().unwrap());
        let controller = Arc::new(ControllerValues::new(vec![1], notify));
        let (mut rt, _) = RtThread::start(
            BridgeServer::new(region.clone()),
            Box::new(Probe(resets.clone())),
            RtConfig {
                priority: 0,
                period: Duration::from_millis(10),
                max_frames: 1,
                fallback: Fallback::Silence,
            },
            Some(controller),
            vec![1],
            None,
        )
        .unwrap_or_else(|failure| panic!("could not start probe: {}", failure.error));
        region.set_bridge_state(BridgeState::Active);
        let spec = BlockSpec {
            frames: 1,
            flags: 0,
            steady_time: 0,
            deadline: None,
            transport: WireTransport::default(),
        };
        let block = |client: &mut BridgeClient, events: &[WireEvent]| {
            let seq = client
                .begin_block(&spec, [&[0.0], &[0.0]], None, events)
                .unwrap();
            client
                .wait_done(seq, Instant::now() + RESTORE_WAIT, [&mut [0.0], &mut [0.0]])
                .unwrap()
                .status
        };
        let param = |value| WireEvent::new(0, WireEventKind::ParamValue { id: 1, value });
        let note_on = WireEvent::new(
            0,
            WireEventKind::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100,
                note_id: u32::MAX,
            },
        );
        let inactive = BlockStatus::Inactive as u32;
        let ok = BlockStatus::Ok as u32;

        // Value changes: 0.25, the same again (no change), 0.5, and back to 0.25.
        for (value, changes) in [(0.25, 1), (0.25, 1), (0.5, 2), (0.25, 3)] {
            assert_eq!(block(&mut client, &[param(value)]), ok);
            assert_eq!(rt.snapshot(|p| p.changes(1)).unwrap(), changes);
        }

        // A parameter-only block that meets a snapshot pause loses nothing.
        let status = rt.snapshot(|_| block(&mut client, &[param(0.75)])).unwrap();
        assert_eq!(status, inactive);
        assert_eq!(block(&mut client, &[]), ok);
        assert_eq!(resets.load(Ordering::Relaxed), 0);

        // A discarded note-on may have a note-off after it that never arrives.
        let status = rt.snapshot(|_| block(&mut client, &[note_on])).unwrap();
        assert_eq!(status, inactive);
        assert_eq!(block(&mut client, &[]), ok);
        assert_eq!(resets.load(Ordering::Relaxed), 1);

        // A reset the client asked for with a block that meets the pause is applied next.
        let reset_spec = BlockSpec {
            flags: BLOCK_FLAG_RESET,
            ..spec
        };
        let status = rt
            .snapshot(|_| {
                let seq = client
                    .begin_block(&reset_spec, [&[0.0], &[0.0]], None, &[])
                    .unwrap();
                client
                    .wait_done(seq, Instant::now() + RESTORE_WAIT, [&mut [0.0], &mut [0.0]])
                    .unwrap()
                    .status
            })
            .unwrap();
        assert_eq!(status, inactive);
        assert_eq!(block(&mut client, &[]), ok);
        assert_eq!(resets.load(Ordering::Relaxed), 2);

        // A restore always resets, as before.
        let (status, _) = rt
            .restore(|| (block(&mut client, &[param(0.5)]), false))
            .unwrap();
        assert_eq!(status, inactive);
        assert_eq!(block(&mut client, &[]), ok);
        assert_eq!(resets.load(Ordering::Relaxed), 3);
        assert_eq!(rt.snapshot(|p| p.changes(1)).unwrap(), 5);

        // The new plug-in state supersedes a cancelled write that met the restore pause.
        let (status, _) = rt
            .restore(|| (block(&mut client, &[param(0.9)]), true))
            .unwrap();
        assert_eq!(status, inactive);
        assert_eq!(block(&mut client, &[]), ok);
        assert_eq!(rt.snapshot(|p| p.changes(1)).unwrap(), 0);
        let _ = rt.stop();
    }

    #[test]
    fn a_parameter_rescan_preserves_notes_and_synchronizes_new_controller_values() {
        use komponist_ipc::shm::{BlockSpec, BridgeState};
        use komponist_ipc::{BridgeClient, ShmRegion, WireTransport};

        #[derive(Default)]
        struct Calls {
            note_ons: AtomicU64,
            note_offs: AtomicU64,
            resets: AtomicU64,
        }

        struct Probe(Arc<Calls>);

        impl HostedProcessor for Probe {
            fn start_processing(&mut self) -> Result<(), HostingError> {
                Ok(())
            }

            fn stop_processing(&mut self) {}

            fn reset(&mut self) {
                self.0.resets.fetch_add(1, Ordering::Relaxed);
            }

            fn process(&mut self, block: &mut ProcessBlock<'_>) -> ProcessStatus {
                for event in block.events_in {
                    match event.decode() {
                        Some(WireEventKind::NoteOn { .. }) => {
                            self.0.note_ons.fetch_add(1, Ordering::Relaxed);
                        }
                        Some(WireEventKind::NoteOff { .. }) => {
                            self.0.note_offs.fetch_add(1, Ordering::Relaxed);
                        }
                        Some(WireEventKind::ParamValue {
                            id: 7,
                            value: 0.403,
                        }) => {
                            block.events_out.push(WireEvent::new(
                                0,
                                WireEventKind::ParamValue { id: 7, value: 0.4 },
                            ));
                        }
                        _ => {}
                    }
                }
                ProcessStatus::Continue
            }
        }

        let calls = Arc::new(Calls::default());
        let region = Arc::new(ShmRegion::create().unwrap());
        let mut client = BridgeClient::new(region.clone());
        let notify = Arc::new(notify::BridgeNotify::new().unwrap());
        let controller = Arc::new(ControllerValues::new(vec![1], notify));
        let (mut rt, _) = RtThread::start(
            BridgeServer::new(region.clone()),
            Box::new(Probe(calls.clone())),
            RtConfig {
                priority: 0,
                period: Duration::from_millis(10),
                max_frames: 1,
                fallback: Fallback::Silence,
            },
            Some(controller),
            vec![1],
            None,
        )
        .unwrap_or_else(|failure| panic!("could not start probe: {}", failure.error));
        region.set_bridge_state(BridgeState::Active);

        let spec = BlockSpec {
            frames: 1,
            flags: 0,
            steady_time: 0,
            deadline: None,
            transport: WireTransport::default(),
        };
        let sync = rt.sync.clone();
        sync.dequeue_probe.armed.store(true, Ordering::Release);
        let first = client
            .begin_block(
                &spec,
                [&[0.0], &[0.0]],
                None,
                &[
                    WireEvent::new(0, WireEventKind::ParamValue { id: 1, value: 0.25 }),
                    WireEvent::new(
                        0,
                        WireEventKind::NoteOn {
                            channel: 0,
                            key: 60,
                            velocity: 100,
                            note_id: u32::MAX,
                        },
                    ),
                ],
            )
            .unwrap();
        let deadline = Instant::now() + RESTORE_WAIT;
        while !sync.dequeue_probe.entered.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(sync.dequeue_probe.entered.load(Ordering::Acquire));

        // The block has already begun but has yet to read the parameter queue.
        let accepted = rt.refresh_parameter_ids(vec![1, 7]);
        let queued = rt.set_param(7, 0.403);
        let activity = rt.sync.activity.load(Ordering::Acquire);
        sync.dequeue_probe.proceed.store(true, Ordering::Release);
        assert!(accepted && queued);
        assert_eq!(activity, PROCESSING);

        let wait = |client: &mut BridgeClient, seq| {
            client
                .wait_done(seq, Instant::now() + RESTORE_WAIT, [&mut [0.0], &mut [0.0]])
                .unwrap()
                .status
        };
        assert_eq!(wait(&mut client, first), BlockStatus::Ok as u32);
        rt.snapshot(|params| assert!(params.confirms_input(7, 0.403, 0.4)))
            .unwrap();
        let second = client
            .begin_block(
                &spec,
                [&[0.0], &[0.0]],
                None,
                &[
                    WireEvent::new(0, WireEventKind::ParamValue { id: 7, value: 0.5 }),
                    WireEvent::new(
                        0,
                        WireEventKind::NoteOff {
                            channel: 0,
                            key: 60,
                            note_id: u32::MAX,
                        },
                    ),
                ],
            )
            .unwrap();
        assert_eq!(wait(&mut client, second), BlockStatus::Ok as u32);
        assert_eq!(calls.note_ons.load(Ordering::Relaxed), 1);
        assert_eq!(calls.note_offs.load(Ordering::Relaxed), 1);
        assert_eq!(calls.resets.load(Ordering::Relaxed), 0);
        assert_eq!(
            rt.controller().unwrap().drain().collect::<Vec<_>>(),
            [(1, 0.25), (7, 0.5)]
        );
        rt.snapshot(|params| {
            assert!(params.confirms_input(1, 0.25, 0.25));
            assert!(params.confirms_input(7, 0.5, 0.5));
        })
        .unwrap();
        assert!(rt.refresh_parameter_ids(vec![7]));
        assert_no_alloc::assert_no_alloc(|| {
            rt.snapshot(|params| {
                assert!(!params.confirms_input(1, 0.25, 0.25));
                assert!(params.confirms_input(7, 0.5, 0.5));
            })
            .unwrap();
        });
        let _ = rt.stop();
    }

    #[test]
    fn state_restore_waits_for_a_staged_block_and_supersedes_older_edits() {
        use komponist_ipc::shm::{BlockSpec, BridgeState};
        use komponist_ipc::{BridgeClient, ShmRegion, WireTransport};

        struct Probe {
            value: Arc<AtomicU64>,
            retried: Arc<AtomicU64>,
            note_ons: Arc<AtomicU64>,
            resets: Arc<AtomicU64>,
            staged: Arc<AtomicBool>,
            proceed: Arc<AtomicBool>,
        }

        impl HostedProcessor for Probe {
            fn start_processing(&mut self) -> Result<(), HostingError> {
                Ok(())
            }
            fn stop_processing(&mut self) {}

            fn reset(&mut self) {
                self.resets.fetch_add(1, Ordering::Relaxed);
                // serve has already copied the block's parameter prefix when reset is called.
                self.staged.store(true, Ordering::Release);
                let deadline = Instant::now() + RESTORE_WAIT;
                while !self.proceed.load(Ordering::Acquire) && Instant::now() < deadline {
                    std::thread::yield_now();
                }
            }

            fn process(&mut self, block: &mut ProcessBlock<'_>) -> ProcessStatus {
                for event in block.events_in {
                    match event.decode() {
                        Some(WireEventKind::ParamValue { id: 0, value }) => {
                            self.value.store(value.to_bits(), Ordering::Relaxed);
                        }
                        Some(WireEventKind::ParamValue { id: 1, value }) => {
                            self.retried.store(value.to_bits(), Ordering::Relaxed);
                            block.events_out.push(WireEvent::new(
                                0,
                                WireEventKind::ParamValue {
                                    id: 1,
                                    value: (value * 10.0).round() / 10.0,
                                },
                            ));
                        }
                        Some(WireEventKind::NoteOn { .. }) => {
                            self.note_ons.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
                ProcessStatus::Continue
            }
        }

        let value = Arc::new(AtomicU64::new(0.0f64.to_bits()));
        let retried = Arc::new(AtomicU64::new(0));
        let note_ons = Arc::new(AtomicU64::new(0));
        let resets = Arc::new(AtomicU64::new(0));
        let staged = Arc::new(AtomicBool::new(false));
        let proceed = Arc::new(AtomicBool::new(false));
        let region = Arc::new(ShmRegion::create().unwrap());
        let mut client = BridgeClient::new(region.clone());
        let config = RtConfig {
            priority: 0,
            period: Duration::from_millis(10),
            max_frames: 1,
            fallback: Fallback::Silence,
        };
        let (mut rt, _) = RtThread::start(
            BridgeServer::new(region.clone()),
            Box::new(Probe {
                value: value.clone(),
                retried: retried.clone(),
                note_ons: note_ons.clone(),
                resets: resets.clone(),
                staged: staged.clone(),
                proceed: proceed.clone(),
            }),
            config,
            None,
            vec![0],
            None,
        )
        .unwrap_or_else(|failure| panic!("could not start probe: {}", failure.error));
        region.set_bridge_state(BridgeState::Active);
        let wait_for = |ready: &dyn Fn() -> bool| {
            let deadline = Instant::now() + RESTORE_WAIT;
            while !ready() && Instant::now() < deadline {
                std::thread::yield_now();
            }
            ready()
        };
        let mut spec = BlockSpec {
            frames: 1,
            flags: BLOCK_FLAG_RESET,
            steady_time: 0,
            deadline: None,
            transport: WireTransport::default(),
        };
        assert!(rt.set_param(0, 0.75));
        let first = client
            .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
            .unwrap();
        assert!(wait_for(&|| staged.load(Ordering::Acquire)));
        for _ in 0..PARAM_QUEUE_CAPACITY {
            assert!(rt.set_param(0, 0.125));
        }
        let sync = rt.sync.clone();

        let restoring = AtomicBool::new(false);
        let restored = AtomicBool::new(false);
        let mut output = ([0.0], [0.0]);
        let mut wait = |client: &mut BridgeClient, seq| {
            client
                .wait_done(
                    seq,
                    Instant::now() + RESTORE_WAIT,
                    [&mut output.0, &mut output.1],
                )
                .unwrap()
                .status
        };
        spec.flags = 0;
        let mut rt = std::thread::scope(|scope| {
            let runner = scope.spawn(|| {
                rt.restore(|| {
                    assert_eq!(f64::from_bits(value.load(Ordering::Relaxed)), 0.75);
                    value.store(0.5f64.to_bits(), Ordering::Relaxed);
                    restoring.store(true, Ordering::Release);
                    assert!(wait_for(&|| restored.load(Ordering::Acquire)));
                    ((), true)
                })
                .unwrap();
                rt
            });
            let blocked =
                wait_for(&|| sync.activity.load(Ordering::Acquire) == PROCESSING | RESTORING);
            proceed.store(true, Ordering::Release);
            assert!(blocked, "restore did not wait for the staged block");
            assert!(wait_for(&|| restoring.load(Ordering::Acquire)));
            assert_eq!(wait(&mut client, first), BlockStatus::Ok as u32);
            // Model a client that had already checked the bridge state when the pause began.
            let racing = client
                .begin_block(
                    &spec,
                    [&[0.0], &[0.0]],
                    None,
                    &[
                        WireEvent::new(0, WireEventKind::ParamValue { id: 0, value: 0.1 }),
                        WireEvent::new(0, WireEventKind::ParamValue { id: 1, value: 0.25 }),
                        WireEvent::new(
                            0,
                            WireEventKind::ParamValue {
                                id: 1,
                                value: 0.625,
                            },
                        ),
                        WireEvent::new(
                            0,
                            WireEventKind::NoteOn {
                                channel: 0,
                                key: 60,
                                velocity: 100,
                                note_id: u32::MAX,
                            },
                        ),
                    ],
                )
                .unwrap();
            let fallback = wait(&mut client, racing);
            restored.store(true, Ordering::Release);
            assert_eq!(fallback, BlockStatus::Inactive as u32);
            runner.join().unwrap()
        });
        assert_eq!(rt.params.slots(), PARAM_QUEUE_CAPACITY);
        // A snapshot after the rescan installs the new ID before replay. The restore
        // superseded only ID 0, so the deferred one-shot write for ID 1 must survive.
        assert!(rt.refresh_parameter_ids(vec![0, 1]));
        rt.snapshot(|_| {}).unwrap();
        let after_restore = client
            .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
            .unwrap();
        assert_eq!(wait(&mut client, after_restore), BlockStatus::Ok as u32);
        assert_eq!(f64::from_bits(value.load(Ordering::Relaxed)), 0.5);
        assert_eq!(resets.load(Ordering::Relaxed), 2);
        assert_eq!(f64::from_bits(retried.load(Ordering::Relaxed)), 0.625);
        assert_eq!(note_ons.load(Ordering::Relaxed), 0);

        assert!(rt.set_param(0, 0.875));
        rt.restore(|| ((), false)).unwrap();
        assert_eq!(rt.params.slots(), PARAM_QUEUE_CAPACITY - 1);
        let after_edit = client
            .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
            .unwrap();
        assert_eq!(wait(&mut client, after_edit), BlockStatus::Ok as u32);
        assert_eq!(f64::from_bits(value.load(Ordering::Relaxed)), 0.875);
        assert!(rt.set_param(1, 0.403));
        let new_param = client
            .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
            .unwrap();
        assert_eq!(wait(&mut client, new_param), BlockStatus::Ok as u32);
        rt.snapshot(|params| {
            assert!(params.confirms_input(0, 0.875, 0.875));
            assert!(params.confirms_input(1, 0.403, 0.4));
            assert!(!params.confirms_input(1, 0.403, 0.6));
        })
        .unwrap();
        rt.restore(|| ((), true)).unwrap();
        rt.snapshot(|params| {
            assert!(!params.confirms_input(0, 0.875, 0.875));
            assert!(!params.confirms_input(1, 0.403, 0.4));
        })
        .unwrap();
        let _ = rt.stop();
    }

    #[test]
    fn control_edits_invalidate_older_controller_publications() {
        let notify = Arc::new(notify::BridgeNotify::new().unwrap());
        let values = ControllerValues::new(vec![1, 2], notify);
        let event = |id, value| WireEvent::new(0, WireEventKind::ParamValue { id, value });
        let older_block = values.signal.generation.load(Ordering::Acquire);
        values.publish(older_block, &[event(1, 0.25)], &[]);
        values.invalidate(Some(1));
        // This block took its generation before SetParam but finished after it.
        values.publish(older_block, &[event(1, 0.5), event(2, 0.4)], &[]);
        assert_eq!(values.drain().collect::<Vec<_>>(), [(2, 0.4)]);
        let newer_block = values.signal.generation.load(Ordering::Acquire);
        values.publish(newer_block, &[event(1, 0.75)], &[]);
        assert_eq!(values.drain().collect::<Vec<_>>(), [(1, 0.75)]);
        values.publish(newer_block, &[event(1, 0.25), event(2, 0.5)], &[]);
        values.invalidate(None);
        assert_eq!(values.drain().count(), 0);
    }
}
