//! The app ↔ bridge link across two processes (PLAN WP-16, "Done when").
//!
//! The bridge side runs in a child process: this test binary, re-executed (fork + exec) with
//! [`BRIDGE_ENV`] set so that libtest runs only [`child_bridge_entry`]. It inherits the bridge end
//! of a control socket, says `Hello`, receives the memfd over the socket, answers `Activate` and
//! serves blocks on an RT thread until `Shutdown` or until the socket closes. Its "plugin" halves
//! the input, adds `(channel + 1) × event count`, and reports the event sum, the steady time and
//! the tempo as one output event per block, so every block's data can be checked exactly.
//!
//! Children get `RLIMIT_CORE = 0` and make themselves non-dumpable (systemd-coredump is active and
//! some are killed on purpose).
//! RT priority is not required; for numbers at SCHED_FIFO run the ignored `latency_report` under
//! `chrt -f 80` (the bridge child inherits the policy).
//!
//! Windows: the child is spawned (no fork) with the bridge end of the control pipe as its stdin,
//! the app sends the region's section and wake events with `send_region` (duplicated into the
//! child through the watcher's process handle), and the child turns off the Windows Error
//! Reporting dialog (`SetErrorMode`) before it may crash. This is the handle-passing and
//! cross-process wait/wake test on Windows; the RT path runs inside `assert_no_alloc` on both
//! sides as on Unix.

#[cfg(unix)]
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;
use komponist_ipc::control::{PROTOCOL_VERSION, socket_pair};
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    AppChannel, BlockError, BlockResult, BlockSpec, BlockStatus, BridgeChannel, BridgeClient,
    BridgeMonitor, BridgeMsg, BridgeServer, BridgeState, ControlMsg, ProcessExit, ProcessWatcher,
    ShmRegion, WireEvent, WireEventKind, WireTransport,
};

// Debug builds only: in release builds `assert_no_alloc` is a no-op without an allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Set in the child: the number of the inherited control socket fd (Windows: `stdin`).
const BRIDGE_ENV: &str = "KOMPONIST_IPC_TEST_BRIDGE";

/// The exit of a bridge that aborts: SIGABRT on Unix, the fast-fail NTSTATUS on Windows.
#[cfg(unix)]
const ABORTED: i32 = 6;
#[cfg(windows)]
const ABORTED: i32 = 0xC000_0409_u32 as i32;

/// Input-event parameter ids the test bridge treats as commands.
const SLEEP_MS: u32 = 0xFFFF_0001;
/// Aborts the bridge in the middle of the block (a crashing plugin).
const CRASH: u32 = 0xFFFF_0002;

const LATENCY: u32 = 64;

// ------------------------------------------------------------------------------------------------
// Bridge side (child process)
// ------------------------------------------------------------------------------------------------

/// Entry point of the child process; does nothing in a normal test run.
#[test]
#[cfg(windows)]
fn child_bridge_entry() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    if std::env::var(BRIDGE_ENV).is_err() {
        return;
    }
    // A crashing bridge must exit, not wait behind an error-reporting dialog (this process only).
    // SAFETY: plain FFI call.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
    // The parent gave us the bridge end of the pipe as stdin.
    let pipe = std::io::stdin()
        .as_handle()
        .try_clone_to_owned()
        .expect("stdin handle");
    bridge_main(BridgeChannel::from_handle(pipe));
    std::process::exit(0);
}

#[test]
#[cfg(unix)]
fn child_bridge_entry() {
    let Ok(fd) = std::env::var(BRIDGE_ENV) else {
        return;
    };
    let fd: i32 = fd.parse().expect("fd number");
    // RLIMIT_CORE = 0 (set before exec) keeps cores off the disk, but the kernel still runs the
    // systemd-coredump pipe helper for a crashing process, which delays its exit by tens of
    // milliseconds and pings crash reporters. Not dumpable skips the core dump path entirely
    // (exec resets this flag, so it is set here and not in `pre_exec`).
    #[cfg(target_os = "linux")]
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        .expect("PR_SET_DUMPABLE");
    // SAFETY: the parent made this inherited fd ours (CLOEXEC cleared) and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    bridge_main(BridgeChannel::from_fd(fd));
    std::process::exit(0);
}

fn bridge_main(channel: BridgeChannel) {
    let hello = BridgeMsg::Hello {
        protocol: PROTOCOL_VERSION,
        pid: std::process::id(),
    };
    if channel.send(&hello).is_err() {
        return;
    }
    #[cfg(unix)]
    let Ok(handles) = channel.recv_fd() else {
        return;
    };
    // SAFETY: the peer is the test process that spawned us, which duplicated the handles into
    // this process for these messages.
    #[cfg(windows)]
    let Ok(handles) = (unsafe { channel.recv_region() }) else {
        return;
    };
    let region = Arc::new(ShmRegion::open(handles).expect("open shared memory"));
    let stop = Arc::new(AtomicBool::new(false));
    let mut rt = None;
    // Control loop on the main thread; `Closed` (the app died) ends it like `Shutdown`.
    while let Ok(msg) = channel.recv() {
        match msg {
            ControlMsg::Activate { .. } if rt.is_none() => {
                region.set_latency(LATENCY);
                let server = BridgeServer::new(Arc::clone(&region));
                let stop = Arc::clone(&stop);
                rt = Some(std::thread::spawn(move || rt_loop(server, &stop)));
                region.set_bridge_state(BridgeState::Active);
                let _ = channel.send(&BridgeMsg::Activated { latency: LATENCY });
            }
            ControlMsg::SetState { request, state } => {
                let _ = channel.send(&BridgeMsg::State {
                    request,
                    state,
                    verified: Vec::new(),
                });
            }
            ControlMsg::Shutdown => break,
            _ => {}
        }
    }
    region.set_bridge_state(BridgeState::Loaded);
    stop.store(true, Ordering::Release);
    if let Some(rt) = rt {
        rt.join().expect("RT thread");
    }
}

/// The bridge RT thread: everything inside `assert_no_alloc`.
fn rt_loop(mut server: BridgeServer, stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        // The deadline only lets the loop look at `stop` now and then.
        assert_no_alloc(|| serve_one(&mut server, Instant::now() + Duration::from_millis(20)));
    }
}

fn serve_one(server: &mut BridgeServer, deadline: Instant) {
    let Some(request) = server.wait_request(Some(deadline)) else {
        return;
    };
    let mut sum = 0.0;
    let mut count = 0u32;
    let mut sleep_ms = 0.0;
    while let Some(event) = server.pop_event() {
        match event.decode() {
            Some(WireEventKind::ParamValue {
                id: SLEEP_MS,
                value,
            }) => sleep_ms = value,
            Some(WireEventKind::ParamValue { id: CRASH, .. }) => std::process::abort(),
            _ => {}
        }
        sum += event.value + f64::from(event.offset);
        count += 1;
    }
    let (input, output) = server.audio(request.frames as usize);
    for (channel, (out, inp)) in output.into_iter().zip(input).enumerate() {
        let offset = (channel + 1) as f32 * count as f32;
        for (o, i) in out.iter_mut().zip(inp) {
            *o = i * 0.5 + offset;
        }
    }
    if sleep_ms > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(sleep_ms / 1000.0));
    }
    let value = sum + request.steady_time as f64 + request.transport.tempo_bpm;
    let event = WireEvent::new(
        0,
        WireEventKind::ParamValue {
            id: request.seq,
            value,
        },
    );
    let dropped = u32::from(!server.push_output_event(event));
    server.finish(BlockResult {
        seq: 0,
        status: BlockStatus::Ok as u32,
        dropped_events: dropped,
        reserved: 0,
    });
}

// ------------------------------------------------------------------------------------------------
// App side (test process)
// ------------------------------------------------------------------------------------------------

/// A running child bridge, activated, with the app's three ends of it.
struct TestBridge {
    channel: AppChannel,
    watcher: ProcessWatcher,
    client: BridgeClient,
    monitor: BridgeMonitor,
}

impl TestBridge {
    /// The child bridge, running `child_bridge_entry`, with its end of the control socket.
    fn command() -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "child_bridge_entry",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdout(Stdio::null());
        command
    }

    /// Spawns the child bridge and watches it (Unix: inherits the socket fd).
    #[cfg(unix)]
    fn spawn() -> (AppChannel, ProcessWatcher) {
        let (channel, bridge_fd) = socket_pair().unwrap();
        let raw = bridge_fd.as_raw_fd();
        let mut command = Self::command();
        command.env(BRIDGE_ENV, raw.to_string());
        // SAFETY: runs between fork and exec and only makes two async-signal-safe syscalls.
        unsafe {
            command.pre_exec(move || {
                // No core dumps from children killed on purpose (systemd-coredump is active).
                let none = rustix::process::Rlimit {
                    current: Some(0),
                    maximum: Some(0),
                };
                rustix::process::setrlimit(rustix::process::Resource::Core, none)?;
                // Let the bridge end of the socket survive exec.
                let fd = BorrowedFd::borrow_raw(raw);
                rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty())?;
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn the bridge child");
        let watcher = ProcessWatcher::new(child.id()).expect("pidfd");
        // The watcher reaps the child; our copy of the bridge end must go, or a dead bridge would
        // never close the socket.
        drop(child);
        drop(bridge_fd);
        (channel, watcher)
    }

    /// Spawns the child bridge and watches it (Windows: the pipe end is its stdin).
    #[cfg(windows)]
    fn spawn() -> (AppChannel, ProcessWatcher) {
        let (channel, bridge_end) = socket_pair().unwrap();
        let mut command = Self::command();
        command
            .env(BRIDGE_ENV, "stdin")
            .stdin(Stdio::from(bridge_end));
        let child = command.spawn().expect("spawn the bridge child");
        // While `child` holds its handle, the pid cannot be reused.
        let watcher = ProcessWatcher::new(child.id()).expect("process handle");
        drop(child);
        // The command owns our copy of the bridge end: it must go, or a dead bridge would never
        // close the pipe.
        drop(command);
        (channel, watcher)
    }

    /// Spawns the child and runs the start-up protocol: `Hello`, memfd, `Activate`.
    fn start() -> TestBridge {
        let (channel, watcher) = Self::spawn();

        match channel.recv().expect("Hello") {
            BridgeMsg::Hello { protocol, pid } => {
                assert_eq!(protocol, PROTOCOL_VERSION);
                assert_eq!(pid, watcher.pid());
            }
            other => panic!("expected Hello, got {other:?}"),
        }
        let region = Arc::new(ShmRegion::create().unwrap());
        #[cfg(unix)]
        channel.send_fd(region.fd()).unwrap();
        #[cfg(windows)]
        channel.send_region(&region, watcher.as_handle()).unwrap();
        channel
            .send(&ControlMsg::Activate {
                sample_rate: 48_000.0,
                max_block: MAX_BLOCK as u32,
            })
            .unwrap();
        assert_eq!(
            channel.recv().unwrap(),
            BridgeMsg::Activated { latency: LATENCY }
        );
        assert_eq!(region.bridge_state(), BridgeState::Active);
        assert_eq!(region.latency(), LATENCY);
        TestBridge {
            channel,
            watcher,
            client: BridgeClient::new(Arc::clone(&region)),
            monitor: BridgeMonitor::new(region),
        }
    }

    /// `Shutdown`, then the clean exit of the child.
    fn shutdown(mut self) {
        self.channel.send(&ControlMsg::Shutdown).unwrap();
        let exit = self
            .watcher
            .wait_exit(Some(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(
            exit,
            Some(ProcessExit {
                code: Some(0),
                signal: None,
            })
        );
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        // Never leave a child behind, also when a test fails.
        let _ = self.watcher.kill();
        let _ = self.watcher.wait_exit(Some(Duration::from_secs(5)));
    }
}

/// One block's input, kept outside the RT closure.
struct Block {
    spec: BlockSpec,
    input: Box<[[f32; MAX_BLOCK]; 2]>,
    events: Vec<WireEvent>,
}

impl Block {
    fn new() -> Block {
        Block {
            spec: BlockSpec::default(),
            input: Box::new([[0.0; MAX_BLOCK]; 2]),
            events: Vec::with_capacity(16),
        }
    }

    /// Deterministic content for block `k` with `frames` frames and `n_events` events.
    fn fill(&mut self, k: u64, frames: u32, n_events: usize) {
        self.spec = BlockSpec {
            frames,
            flags: 0,
            steady_time: k * 256,
            deadline: None,
            transport: WireTransport {
                tempo_bpm: 120.0 + (k % 7) as f64,
                ..WireTransport::default()
            },
        };
        let mut x = k.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        for channel in self.input.iter_mut() {
            for sample in &mut channel[..frames as usize] {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                *sample = (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
            }
        }
        self.events.clear();
        for j in 0..n_events {
            let offset = (j as u32 * 7).min(frames - 1);
            let value = (k * 10 + j as u64) as f64;
            self.events.push(WireEvent::new(
                offset,
                WireEventKind::ParamValue {
                    id: j as u32,
                    value,
                },
            ));
        }
    }

    /// The output the test bridge computes for this block.
    fn expected(&self, channel: usize, frame: usize) -> f32 {
        let count = self.events.len() as f32;
        self.input[channel][frame] * 0.5 + (channel + 1) as f32 * count
    }

    /// The value of the output event the test bridge reports for this block.
    fn expected_event_value(&self) -> f64 {
        let mut sum = 0.0;
        for e in &self.events {
            sum += e.value + f64::from(e.offset);
        }
        sum + self.spec.steady_time as f64 + self.spec.transport.tempo_bpm
    }

    fn inputs(&self) -> [&[f32]; 2] {
        let frames = self.spec.frames as usize;
        [&self.input[0][..frames], &self.input[1][..frames]]
    }
}

/// `begin_block` + `wait_done` inside `assert_no_alloc`, as `ExternalProcessor::process` will.
fn round_trip(
    client: &mut BridgeClient,
    block: &Block,
    output: &mut [[f32; MAX_BLOCK]; 2],
    deadline: Instant,
) -> Result<BlockResult, BlockError> {
    let [left, right] = output;
    assert_no_alloc(|| {
        let seq = client.begin_block(&block.spec, block.inputs(), None, &block.events)?;
        client.wait_done(seq, deadline, [left, right])
    })
}

/// Mismatching output samples of `block`.
fn output_errors(block: &Block, output: &[[f32; MAX_BLOCK]; 2]) -> usize {
    let frames = block.spec.frames as usize;
    (0..2)
        .map(|c| {
            (0..frames)
                .filter(|&i| output[c][i] != block.expected(c, i))
                .count()
        })
        .sum()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn summary(times: &mut [Duration]) -> String {
    times.sort_unstable();
    format!(
        "n={} p50={:.1?} p90={:.1?} p99={:.1?} p99.9={:.1?} max={:.1?}",
        times.len(),
        percentile(times, 0.5),
        percentile(times, 0.9),
        percentile(times, 0.99),
        percentile(times, 0.999),
        percentile(times, 1.0),
    )
}

// ------------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------------

#[test]
fn ten_thousand_blocks_round_trip_with_data_checks() {
    const BLOCKS: u64 = 10_000;
    let mut bridge = TestBridge::start();
    let mut block = Block::new();
    let mut output = Box::new([[0.0f32; MAX_BLOCK]; 2]);
    let mut times = Vec::with_capacity(BLOCKS as usize);
    let mut bad_samples = 0;
    let mut bad_events = 0;
    let mut echoed = 0u64;
    for k in 0..BLOCKS {
        let frames = match k % 4 {
            0 => 256,
            1 => 64,
            2 => MAX_BLOCK as u32,
            _ => 1 + (k * 37 % MAX_BLOCK as u64) as u32,
        };
        block.fill(k, frames, (k % 9) as usize);
        let t0 = Instant::now();
        // Generous deadline: this test checks data, not timing, on a shared machine.
        let result = round_trip(
            &mut bridge.client,
            &block,
            &mut output,
            t0 + Duration::from_secs(2),
        );
        times.push(t0.elapsed());
        let result = result.unwrap_or_else(|e| panic!("block {k}: {e:?}"));
        assert_eq!(result.seq as u64, k + 1);
        assert_eq!(result.status, BlockStatus::Ok as u32);
        assert_eq!(result.dropped_events, 0);
        bad_samples += output_errors(&block, &output);
        while let Some(event) = assert_no_alloc(|| bridge.monitor.pop_output_event()) {
            echoed += 1;
            let expected = WireEventKind::ParamValue {
                id: echoed as u32,
                value: block.expected_event_value(),
            };
            bad_events += usize::from(echoed != k + 1 || event.decode() != Some(expected));
        }
    }
    assert_eq!(bad_samples, 0);
    assert_eq!(bad_events, 0);
    assert_eq!(echoed, BLOCKS);
    assert!(bridge.client.is_idle());

    // A large state through the control socket in both directions (fragmented, 3 MiB).
    let state: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    bridge
        .channel
        .send(&ControlMsg::SetState {
            request: 1,
            state: state.clone(),
        })
        .unwrap();
    assert_eq!(
        bridge.channel.recv().unwrap(),
        BridgeMsg::State {
            request: 1,
            state,
            verified: Vec::new(),
        }
    );

    eprintln!(
        "round trip, {BLOCKS} blocks of 1 ..= {MAX_BLOCK} frames: {}",
        summary(&mut times)
    );
    bridge.shutdown();
}

#[test]
fn late_block_and_resynchronisation() {
    let mut bridge = TestBridge::start();
    let mut block = Block::new();
    let mut output = Box::new([[0.0f32; MAX_BLOCK]; 2]);
    let generous = || Instant::now() + Duration::from_secs(2);

    block.fill(1, 256, 2);
    round_trip(&mut bridge.client, &block, &mut output, generous()).unwrap();

    // The bridge sleeps 60 ms in this block; the app gives it 5 ms.
    block.fill(2, 256, 1);
    block.events.push(WireEvent::new(
        0,
        WireEventKind::ParamValue {
            id: SLEEP_MS,
            value: 60.0,
        },
    ));
    let t0 = Instant::now();
    let late = bridge
        .client
        .begin_block(&block.spec, block.inputs(), None, &block.events);
    let late = late.unwrap();
    let [left, right] = &mut *output;
    let deadline = t0 + Duration::from_millis(5);
    let outcome = assert_no_alloc(|| bridge.client.wait_done(late, deadline, [left, right]));
    let waited = t0.elapsed();
    assert_eq!(outcome, Err(BlockError::Late));
    assert!(waited >= Duration::from_millis(5), "{waited:?}");
    assert!(
        waited < Duration::from_millis(60),
        "returned only after the bridge: {waited:?}"
    );

    // The next callbacks find the bridge busy and write nothing, until it finished the late block.
    block.fill(3, 128, 3);
    let mut busy = 0;
    let next = loop {
        match assert_no_alloc(|| {
            bridge
                .client
                .begin_block(&block.spec, block.inputs(), None, &block.events)
        }) {
            Err(BlockError::Busy) => {
                busy += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            other => break other.unwrap(),
        }
    };
    let resync_after = t0.elapsed();
    assert!(busy >= 1);
    assert_eq!(next, late.wrapping_add(1), "Busy blocks published nothing");
    let [left, right] = &mut *output;
    let result = bridge
        .client
        .wait_done(next, generous(), [left, right])
        .unwrap();
    assert_eq!(result.seq, next);
    assert_eq!(output_errors(&block, &output), 0, "output is from block 3");
    // The stale request stays stale.
    let [left, right] = &mut *output;
    assert_eq!(
        bridge.client.wait_done(late, generous(), [left, right]),
        Err(BlockError::Late)
    );
    // Output events of all three blocks arrived, the late one included.
    let ids: Vec<u32> = std::iter::from_fn(|| bridge.monitor.pop_output_event())
        .map(|e| e.param_id)
        .collect();
    assert_eq!(ids, [1, 2, 3]);
    eprintln!(
        "late: Late after {waited:.1?} (deadline 5 ms), {busy} Busy blocks, \
         resynchronised after {resync_after:.1?}"
    );
    bridge.shutdown();
}

#[test]
fn killed_bridge_is_detected_by_the_pidfd_within_10ms() {
    let mut bridge = TestBridge::start();
    let mut block = Block::new();
    let mut output = Box::new([[0.0f32; MAX_BLOCK]; 2]);
    for k in 0..10 {
        block.fill(k, 256, 1);
        let deadline = Instant::now() + Duration::from_secs(2);
        round_trip(&mut bridge.client, &block, &mut output, deadline).unwrap();
    }
    assert_eq!(bridge.watcher.try_exit().unwrap(), None);

    let t0 = Instant::now();
    #[cfg(unix)]
    {
        let pid = rustix::process::Pid::from_raw(bridge.watcher.pid() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
    }
    // Windows: TerminateProcess through the watcher (reported as signal 9).
    #[cfg(windows)]
    bridge.watcher.kill().unwrap();
    let exit = bridge
        .watcher
        .wait_exit(Some(Duration::from_secs(1)))
        .unwrap();
    let detected = t0.elapsed();
    assert_eq!(
        exit,
        Some(ProcessExit {
            code: None,
            signal: Some(9),
        })
    );
    assert!(
        detected < Duration::from_millis(10),
        "detected after {detected:?}"
    );

    // The RT side degrades without blocking: the block is late, then the bridge stays busy.
    block.fill(10, 256, 1);
    let t1 = Instant::now();
    let outcome = round_trip(
        &mut bridge.client,
        &block,
        &mut output,
        t1 + Duration::from_millis(2),
    );
    assert_eq!(outcome, Err(BlockError::Late));
    assert!(t1.elapsed() < Duration::from_millis(50));
    let busy = bridge
        .client
        .begin_block(&block.spec, block.inputs(), None, &block.events);
    assert_eq!(busy, Err(BlockError::Busy));
    assert_eq!(
        bridge.channel.recv(),
        Err(komponist_ipc::ChannelError::Closed)
    );
    eprintln!("pidfd: SIGKILL detected after {detected:.1?}");
}

#[test]
fn bridge_crashing_mid_block_is_late_and_detected() {
    let mut bridge = TestBridge::start();
    let mut block = Block::new();
    let mut output = Box::new([[0.0f32; MAX_BLOCK]; 2]);
    block.fill(0, 256, 0);
    block.events.push(WireEvent::new(
        0,
        WireEventKind::ParamValue {
            id: CRASH,
            value: 1.0,
        },
    ));
    let t0 = Instant::now();
    let outcome = round_trip(
        &mut bridge.client,
        &block,
        &mut output,
        t0 + Duration::from_millis(3),
    );
    assert_eq!(outcome, Err(BlockError::Late));
    let exit = bridge
        .watcher
        .wait_exit(Some(Duration::from_secs(5)))
        .unwrap();
    let detected = t0.elapsed();
    assert_eq!(
        exit,
        Some(ProcessExit {
            code: None,
            signal: Some(ABORTED),
        })
    );
    eprintln!("pidfd: abort() in the bridge detected {detected:.1?} after the request");
}

/// Latency numbers like the WP-15 spike (256 frames, deadline 3.2 ms = 60 % of 5.33 ms). Run:
/// `cargo test --release -p komponist-ipc --test bridge_process -- --ignored --nocapture`, under
/// `chrt -f 80` for SCHED_FIFO. `KOMPONIST_IPC_BLOCKS` sets the number of blocks,
/// `KOMPONIST_IPC_PACED_US` paces the blocks like a callback (e.g. 5333 for 256 frames at 48 kHz;
/// default back to back), `KOMPONIST_IPC_SPIN_US` makes the engine side spin up to that long on
/// `is_idle` before the futex wait (WP-16's optional spin, measured in WP-61).
#[test]
#[ignore = "measurement, run by hand"]
fn latency_report() {
    let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let blocks = env("KOMPONIST_IPC_BLOCKS").unwrap_or(100_000);
    let paced = env("KOMPONIST_IPC_PACED_US").map(Duration::from_micros);
    let spin = Duration::from_micros(env("KOMPONIST_IPC_SPIN_US").unwrap_or(0));
    let mut next = Instant::now();
    let mut bridge = TestBridge::start();
    let mut block = Block::new();
    let mut output = Box::new([[0.0f32; MAX_BLOCK]; 2]);
    let mut times = Vec::with_capacity(blocks as usize);
    let (mut late, mut busy, mut bad) = (0, 0, 0);
    for k in 0..1_000 + blocks {
        block.fill(k, 256, 4);
        if let Some(period) = paced {
            next += period;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_micros(3_200);
        let client = &mut bridge.client;
        let [left, right] = &mut *output;
        let result = assert_no_alloc(|| {
            let seq = client.begin_block(&block.spec, block.inputs(), None, &block.events)?;
            let until = Instant::now() + spin;
            while !client.is_idle() && Instant::now() < until {
                std::hint::spin_loop();
            }
            client.wait_done(seq, deadline, [left, right])
        });
        let dt = t0.elapsed();
        match result {
            Ok(_) => bad += output_errors(&block, &output),
            Err(BlockError::Late) => late += 1,
            Err(BlockError::Busy) => busy += 1,
            Err(e) => panic!("{e:?}"),
        }
        while bridge.monitor.pop_output_event().is_some() {}
        if k >= 1_000 {
            times.push(dt);
        }
    }
    let policy = std::fs::read_to_string("/proc/thread-self/sched")
        .ok()
        .and_then(|s| {
            let line = s.lines().find(|l| l.starts_with("policy"))?;
            Some(match line.split_whitespace().last()? {
                "0" => "SCHED_OTHER",
                "1" => "SCHED_FIFO",
                "2" => "SCHED_RR",
                _ => "other",
            })
        })
        .unwrap_or("unknown");
    eprintln!(
        "latency (policy {policy}, 256 frames, 4 events, {}, spin {spin:?}): {} late={late} \
         busy={busy} bad={bad}",
        paced.map_or("back to back".to_owned(), |p| format!("paced {p:?}")),
        summary(&mut times)
    );
    assert_eq!(bad, 0);
    bridge.shutdown();
}
