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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Latest engine value per VST3 parameter. The NaN sentinel is outside the value domain.
#[derive(Debug)]
pub(crate) struct ControllerValues {
    entries: Vec<(u32, AtomicU64)>,
    dirty: AtomicBool,
    notify: Arc<notify::BridgeNotify>,
}

impl ControllerValues {
    const CLEAN: u64 = f64::NAN.to_bits();

    pub(crate) fn new(mut ids: Vec<u32>, notify: Arc<notify::BridgeNotify>) -> Self {
        ids.sort_unstable();
        ids.dedup();
        Self {
            entries: ids
                .into_iter()
                .map(|id| (id, AtomicU64::new(Self::CLEAN)))
                .collect(),
            dirty: AtomicBool::new(false),
            notify,
        }
    }

    fn publish(&self, events: &[WireEvent]) {
        let mut changed = false;
        for event in events {
            let Some(WireEventKind::ParamValue { id, value }) = event.decode() else {
                continue;
            };
            if value.is_finite()
                && let Ok(i) = self.entries.binary_search_by_key(&id, |(id, _)| *id)
            {
                self.entries[i].1.store(value.to_bits(), Ordering::Relaxed);
                changed = true;
            }
        }
        if changed && !self.dirty.swap(true, Ordering::Release) {
            self.notify.controller_values_ready();
        }
    }

    pub(crate) fn drain(&self) -> impl Iterator<Item = (u32, f64)> + '_ {
        self.dirty.swap(false, Ordering::Acquire);
        self.entries.iter().filter_map(|(id, slot)| {
            let value = slot.swap(Self::CLEAN, Ordering::Relaxed);
            (value != Self::CLEAN).then(|| (*id, f64::from_bits(value)))
        })
    }
}

/// A running RT thread with its processor.
#[derive(Debug)]
pub struct RtThread {
    handle: JoinHandle<Returned>,
    stop: Arc<AtomicBool>,
    params: Producer<(u32, f64)>,
}

/// What the thread hands back when it ends.
type Returned = (Box<dyn HostedProcessor>, Consumer<(u32, f64)>);

/// Buffers the loop works in, allocated before it starts.
struct Scratch {
    events_in: Vec<WireEvent>,
    events_out: Vec<WireEvent>,
}

impl RtThread {
    /// Spawns the RT thread for `processor` (activated, not processing yet) and waits until it
    /// has set its priority and started processing. On a refused start the processor comes back
    /// with the error.
    pub fn start(
        server: BridgeServer,
        processor: Box<dyn HostedProcessor>,
        config: RtConfig,
        controller: Option<Arc<ControllerValues>>,
    ) -> Result<(RtThread, StartReport), StartFailure> {
        let (params_tx, params_rx) = RingBuffer::new(PARAM_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
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
                    controller,
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
        self.params.push((id, value)).is_ok()
    }

    /// Stops the thread (within about [`STOP_POLL`]) and returns the stopped processor and the
    /// parameter values queued after the last block.
    pub fn stop(self) -> (Box<dyn HostedProcessor>, Vec<(u32, f64)>) {
        self.stop.store(true, Ordering::Release);
        let (processor, mut params) = join(self.handle);
        let leftover = std::iter::from_fn(|| params.pop().ok()).collect();
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
    mut params: Consumer<(u32, f64)>,
    stop: &AtomicBool,
    config: RtConfig,
    report: mpsc::SyncSender<(StartReport, Result<(), HostingError>)>,
    controller: Option<Arc<ControllerValues>>,
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
                        controller.as_deref(),
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
    params: &mut Consumer<(u32, f64)>,
    scratch: &mut Scratch,
    request: &BlockRequest,
    config: &RtConfig,
    controller: Option<&ControllerValues>,
) {
    // Values from `SetParam` first: offset 0 keeps the list sorted.
    let mut count = 0;
    while count < PARAM_QUEUE_CAPACITY {
        let Ok((id, value)) = params.pop() else {
            break;
        };
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
    if request.flags & BLOCK_FLAG_RESET != 0 {
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
        controller.publish(&scratch.events_in[queued..count]);
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
            values.publish(&[event(2, 0.3), event(1, 0.4), event(2, 0.7), event(99, 0.1)]);
        });
        assert_ne!(notify.take().0 & notify::CONTROLLER_VALUES_PENDING, 0);
        assert_eq!(values.drain().collect::<Vec<_>>(), [(1, 0.4), (2, 0.7)]);
        assert_eq!(values.drain().count(), 0);
    }
}
