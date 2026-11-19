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
//! Realtime rules (CLAUDE.md rule 4): everything is allocated before the loop; inside it there is
//! no allocation, lock, logging or IO, and the only blocking call is the futex wait with a deadline.
//! Debug builds run the loop inside `assert_no_alloc`, so an allocation aborts the bridge. Plugin
//! log lines from this thread are dropped ([`crate::notify`]).
//!
//! Fallback output (a block the plugin fails, a frame count above the activated maximum, a request
//! that races with a stop): passthrough for effects, silence for instruments; the result carries
//! `PluginError` or `Inactive`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use komponist_hosting::{EventOut, HostedProcessor, HostingError, ProcessBlock, ProcessStatus};
use komponist_ipc::shm::{BLOCK_FLAG_RESET, INPUT_EVENT_CAPACITY, OUTPUT_EVENT_CAPACITY};
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
pub const PARAM_QUEUE_CAPACITY: usize = 512;

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
    entries: Vec<ControllerValue>,
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
            entries: ids.into_iter().map(ControllerValue::new).collect(),
            generation: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            notify,
        }
    }

    fn entry(&self, id: u32) -> Option<&ControllerValue> {
        self.entries
            .binary_search_by_key(&id, |slot| slot.id)
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Invalidate earlier publications for one parameter, or all of them after a state load.
    pub(crate) fn invalidate(&self, id: Option<u32>) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
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
        if changed && !self.dirty.swap(true, Ordering::Release) {
            self.notify.controller_values_ready();
        }
    }

    pub(crate) fn drain(&self) -> impl Iterator<Item = (u32, f64)> + '_ {
        self.dirty.swap(false, Ordering::Acquire);
        self.entries.iter().filter_map(ControllerValue::take)
    }
}

/// A running RT thread with its processor.
#[derive(Debug)]
pub struct RtThread {
    handle: JoinHandle<Returned>,
    stop: Arc<AtomicBool>,
    params: Producer<QueuedParam>,
    sync: Arc<RtParamSync>,
}

#[derive(Debug)]
struct RtParamSync {
    activity: AtomicU8,
    state_generation: AtomicU64,
    controller: Option<Arc<ControllerValues>>,
}

const PROCESSING: u8 = 1;
const RESTORING: u8 = 2;

struct Activity<'a> {
    state: &'a AtomicU8,
    bit: u8,
}

impl Drop for Activity<'_> {
    fn drop(&mut self) {
        self.state.fetch_and(!self.bit, Ordering::Release);
    }
}

impl RtParamSync {
    /// RT never waits for a state restore. The request gets fallback audio instead.
    fn try_process(&self) -> Option<Activity<'_>> {
        self.activity
            .compare_exchange(0, PROCESSING, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Activity {
                state: &self.activity,
                bit: PROCESSING,
            })
    }

    /// Prevent a new block from staging parameter edits while the control thread waits for
    /// the current block to finish. Never load state if a stalled processor still owns it.
    fn pause_for_restore(&self) -> Option<Activity<'_>> {
        self.activity.fetch_or(RESTORING, Ordering::AcqRel);
        let guard = Activity {
            state: &self.activity,
            bit: RESTORING,
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

#[derive(Debug)]
struct QueuedParam {
    id: u32,
    value: f64,
    state_generation: u64,
}

/// What the thread hands back when it ends.
type Returned = (Box<dyn HostedProcessor>, Consumer<QueuedParam>);

/// Buffers the loop works in, allocated before it starts.
struct Scratch {
    events_in: Vec<WireEvent>,
    events_out: Vec<WireEvent>,
    reset_after_restore: bool,
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
    ) -> Result<(RtThread, StartReport), StartFailure> {
        let (params_tx, params_rx) = RingBuffer::new(PARAM_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        if controller.is_some() {
            processor.use_host_controller_sync();
        }
        let sync = Arc::new(RtParamSync {
            activity: AtomicU8::new(0),
            state_generation: AtomicU64::new(0),
            controller,
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
                    params_rx,
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
                    sync,
                },
                report,
            )),
            Ok((report, Err(error))) => {
                let (processor, _) = join(handle);
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
        self.params
            .push(QueuedParam {
                id,
                value,
                state_generation: self.sync.state_generation.load(Ordering::Relaxed),
            })
            .is_ok()
    }

    /// Serializes a successful restore with processing and supersedes queued control edits.
    pub fn restore<T>(&self, load: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let _pause = self
            .sync
            .pause_for_restore()
            .ok_or_else(|| "timed out waiting for the current audio block".to_owned())?;
        let result = load()?;
        self.sync.state_generation.fetch_add(1, Ordering::Relaxed);
        if let Some(controller) = &self.sync.controller {
            controller.invalidate(None);
        }
        Ok(result)
    }

    /// Stops the thread (within about [`STOP_POLL`]) and returns the stopped processor and the
    /// parameter values queued after the last block.
    pub fn stop(self) -> (Box<dyn HostedProcessor>, Vec<(u32, f64)>) {
        self.stop.store(true, Ordering::Release);
        let (processor, mut params) = join(self.handle);
        let generation = self.sync.state_generation.load(Ordering::Relaxed);
        let leftover = std::iter::from_fn(|| params.pop().ok())
            .filter(|param| param.state_generation == generation)
            .map(|param| (param.id, param.value))
            .collect();
        (processor, leftover)
    }
}

fn join(handle: JoinHandle<Returned>) -> Returned {
    // A panic on the RT thread aborts the bridge through the panic hook before this is reached.
    handle.join().unwrap_or_else(|_| std::process::abort())
}

fn rt_main(
    mut server: BridgeServer,
    mut processor: Box<dyn HostedProcessor>,
    mut params: Consumer<QueuedParam>,
    stop: &AtomicBool,
    config: RtConfig,
    report: mpsc::SyncSender<(StartReport, Result<(), HostingError>)>,
    sync: &RtParamSync,
) -> Returned {
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
        events_in: vec![WireEvent::default(); PARAM_QUEUE_CAPACITY + INPUT_EVENT_CAPACITY],
        events_out: vec![WireEvent::default(); OUTPUT_EVENT_CAPACITY],
        reset_after_restore: false,
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
                        &mut params,
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
    (processor, params)
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
    params: &mut Consumer<QueuedParam>,
    scratch: &mut Scratch,
    request: &BlockRequest,
    config: &RtConfig,
    sync: &RtParamSync,
) {
    let Some(_processing) = sync.try_process() else {
        // Recovery clears voices that could otherwise miss a note-off in this discarded block.
        scratch.reset_after_restore = true;
        write_fallback(server, request.frames as usize, config.fallback);
        server.finish(result(BlockStatus::Inactive, 0));
        return;
    };
    let controller = sync.controller.as_deref();
    // Snapshot before accepting block input. A control edit racing with this block can still
    // supersede its controller notifications when the audio thread completes later.
    let generation = controller.map_or(0, |values| values.generation.load(Ordering::Acquire));
    // Values from `SetParam` first: offset 0 keeps the list sorted.
    let mut count = 0;
    while count < PARAM_QUEUE_CAPACITY {
        let Ok(QueuedParam {
            id,
            value,
            state_generation: queued,
        }) = params.pop()
        else {
            break;
        };
        if queued != sync.state_generation.load(Ordering::Acquire) {
            continue;
        }
        scratch.events_in[count] = WireEvent::new(0, WireEventKind::ParamValue { id, value });
        count += 1;
    }
    let queued = count;
    while let Some(event) = server.pop_event() {
        if let Some(slot) = scratch.events_in.get_mut(count) {
            *slot = event;
            count += 1;
        }
    }

    let frames = request.frames as usize;
    if request.frames == 0 || request.frames > config.max_frames {
        write_fallback(server, frames, config.fallback);
        server.finish(result(BlockStatus::PluginError, 0));
        return;
    }
    let after_restore = std::mem::take(&mut scratch.reset_after_restore);
    if after_restore || request.flags & BLOCK_FLAG_RESET != 0 {
        processor.reset();
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
    if status == BlockStatus::Ok
        && let Some(controller) = controller
    {
        controller.publish(
            generation,
            &scratch.events_in[queued..count],
            processor.controller_updates(),
        );
    }
    server.finish(result(status, dropped));
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn state_restore_waits_for_a_staged_block_and_supersedes_older_edits() {
        use komponist_ipc::shm::{BlockSpec, BridgeState};
        use komponist_ipc::{BridgeClient, ShmRegion, WireTransport};

        struct Probe {
            value: Arc<AtomicU64>,
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
                    if let Some(WireEventKind::ParamValue { value, .. }) = event.decode() {
                        self.value.store(value.to_bits(), Ordering::Relaxed);
                    }
                }
                ProcessStatus::Continue
            }
        }

        let value = Arc::new(AtomicU64::new(0.0f64.to_bits()));
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
                resets: resets.clone(),
                staged: staged.clone(),
                proceed: proceed.clone(),
            }),
            config,
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
        assert!(rt.set_param(0, 0.125));
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
                    Ok(())
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
                .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
                .unwrap();
            let fallback = wait(&mut client, racing);
            restored.store(true, Ordering::Release);
            assert_eq!(fallback, BlockStatus::Inactive as u32);
            runner.join().unwrap()
        });
        let after_restore = client
            .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
            .unwrap();
        assert_eq!(wait(&mut client, after_restore), BlockStatus::Ok as u32);
        assert_eq!(f64::from_bits(value.load(Ordering::Relaxed)), 0.5);
        assert_eq!(resets.load(Ordering::Relaxed), 2);

        assert!(rt.set_param(0, 0.875));
        let after_edit = client
            .begin_block(&spec, [&[0.0], &[0.0]], None, &[])
            .unwrap();
        assert_eq!(wait(&mut client, after_edit), BlockStatus::Ok as u32);
        assert_eq!(f64::from_bits(value.load(Ordering::Relaxed)), 0.875);
        let _ = rt.stop();
    }

    #[test]
    fn control_edits_invalidate_older_controller_publications() {
        let notify = Arc::new(notify::BridgeNotify::new().unwrap());
        let values = ControllerValues::new(vec![1, 2], notify);
        let event = |id, value| WireEvent::new(0, WireEventKind::ParamValue { id, value });
        let older_block = values.generation.load(Ordering::Acquire);
        values.publish(older_block, &[event(1, 0.25)], &[]);
        values.invalidate(Some(1));
        // This block took its generation before SetParam but finished after it.
        values.publish(older_block, &[event(1, 0.5), event(2, 0.4)], &[]);
        assert_eq!(values.drain().collect::<Vec<_>>(), [(2, 0.4)]);
        let newer_block = values.generation.load(Ordering::Acquire);
        values.publish(newer_block, &[event(1, 0.75)], &[]);
        assert_eq!(values.drain().collect::<Vec<_>>(), [(1, 0.75)]);
        values.publish(newer_block, &[event(1, 0.25), event(2, 0.5)], &[]);
        values.invalidate(None);
        assert_eq!(values.drain().count(), 0);
    }
}
