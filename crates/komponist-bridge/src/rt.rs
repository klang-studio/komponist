//! The bridge RT thread (ARCHITECTURE §8.1, "RT handshake per block").
//!
//! Started per activation by [`RtThread::start`]: it switches itself to `SCHED_FIFO`, starts
//! processing (CLAP wants that on the audio thread) and then serves one block per request:
//! futex-wait on `request_seq` → the parameter values the app set over the control socket
//! ([`RtThread::set_param`]) at offset 0, then the block's input events → `process` → output events
//! into the shared ring → `finish`. [`RtThread::stop`] ends it and hands the processor back for
//! deactivation.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use komponist_hosting::{EventOut, HostedProcessor, HostingError, ProcessBlock, ProcessStatus};
use komponist_ipc::shm::{BLOCK_FLAG_RESET, INPUT_EVENT_CAPACITY, OUTPUT_EVENT_CAPACITY};
use komponist_ipc::{
    BlockRequest, BlockResult, BlockStatus, BridgeServer, WireEvent, WireEventKind,
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::notify;
use crate::sched;

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
    /// `SCHED_FIFO` priority.
    pub priority: i32,
    /// The activated maximum block size.
    pub max_frames: u32,
    /// Output for blocks the plugin did not process.
    pub fallback: Fallback,
}

/// What the RT thread reports once it runs (or failed to start processing).
#[derive(Debug)]
pub struct StartReport {
    /// Outcome of switching to `SCHED_FIFO` at [`RtConfig::priority`].
    pub priority: std::io::Result<()>,
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
) -> Returned {
    notify::mark_rt_thread();
    let priority = sched::set_current_thread_fifo(config.priority);
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
    let _ = report.send((StartReport { priority }, started));
    drop(report);
    if ok {
        assert_no_alloc::assert_no_alloc(|| {
            while !stop.load(Ordering::Acquire) {
                if let Some(request) = server.wait_request(Some(Instant::now() + STOP_POLL)) {
                    serve(
                        &mut server,
                        processor.as_mut(),
                        &mut params,
                        &mut scratch,
                        &request,
                        &config,
                    );
                }
            }
            serve_stale(&mut server, config.fallback);
        });
        processor.stop_processing();
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
        let (input, output) = server.audio(frames);
        let mut block = ProcessBlock {
            frames: request.frames,
            input,
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
        if !server.push_output_event(*event) {
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
    server.finish(result(status, dropped));
}
