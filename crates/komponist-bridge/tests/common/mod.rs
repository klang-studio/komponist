//! Shared harness of the integration tests: spawns the real `komponist-bridge` binary the way the
//! plugin manager will (inherited control socket, memfd over SCM_RIGHTS, pidfd watcher) and plays
//! the app's side of the protocol with komponist-ipc's `AppChannel` and `BridgeClient`.
//!
//! Bridges get `RLIMIT_CORE = 0` on top of their own `PR_SET_DUMPABLE = 0`: some are crashed on
//! purpose (systemd-coredump is active on this machine).

#![allow(dead_code)]

use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_ipc::control::{PROTOCOL_VERSION, socket_pair};
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    AppChannel, BlockError, BlockResult, BlockSpec, BridgeClient, BridgeMonitor, BridgeMsg,
    ControlMsg, LoadedInfo, LogLevel, PluginFormat, ProcessExit, ProcessWatcher, ShmRegion,
    WireEvent, WireEventKind, WireTransport,
};
use rustix::event::{PollFd, PollFlags, Timespec};

/// The binary under test.
pub const BRIDGE: &str = env!("CARGO_BIN_EXE_komponist-bridge");

pub const SAMPLE_RATE: f64 = 48_000.0;
/// Frames per block in the tests.
pub const BLOCK: usize = 256;
/// 60 % of a 256-frame block at 48 kHz, the engine's deadline (ARCHITECTURE §8.1).
pub const RT_DEADLINE: Duration = Duration::from_micros(3_200);
/// For blocks whose data is checked, not their timing (the machine is shared with other builds).
pub const GENEROUS: Duration = Duration::from_secs(2);
/// How long to wait for a control answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The `Log` prefix of the bridge's RT priority report (`instance::RT_LOG_PREFIX`).
pub const RT_LOG_PREFIX: &str = "RT priority:";

pub const SYNTH: &str = "de.klang-studio.kp-test-synth";
pub const FX: &str = "de.klang-studio.kp-test-fx";
pub const CRASH: &str = "de.klang-studio.kp-test-crash";
pub const SLOW: &str = "de.klang-studio.kp-test-slow";
pub const SC: &str = "de.klang-studio.kp-test-sc";

/// VST3 class ids of the fixtures (their 16 ASCII bytes in hex, test-plugins/README.md).
pub const SYNTH_VST3: &str = "6B705465737453796E74683030303031";
pub const FX_VST3: &str = "6B705465737446783030303030303031";
pub const CRASH_VST3: &str = "6B705465737443726173683030303031";
pub const SLOW_VST3: &str = "6B7054657374536C6F77303030303031";
pub const SC_VST3: &str = "6B705465737453633030303030303031";

/// A WP-27 fixture bundle (`scripts/build-test-plugins.sh`), or `None` with a note on stderr.
pub fn fixture(name: &str) -> Option<PathBuf> {
    fixture_in("clap", name)
}

/// A fixture bundle of `format` ("clap" or "vst3").
pub fn fixture_in(format: &str, name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-plugins")
        .join(std::env::consts::OS)
        .join(format)
        .join(format!("{name}.{format}"));
    if path.exists() {
        Some(std::fs::canonicalize(path).unwrap())
    } else {
        eprintln!(
            "skipped: {} is missing; run scripts/build-test-plugins.sh",
            path.display()
        );
        None
    }
}

/// An installed bundle (`~/` expanded), or `None` with a note if it is not installed.
pub fn installed(path: &str) -> Option<PathBuf> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => Path::new(&std::env::var("HOME").ok()?).join(rest),
        None => PathBuf::from(path),
    };
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {} is not installed", path.display());
        None
    }
}

pub fn note_on(offset: u32, key: u8) -> WireEvent {
    WireEvent::new(
        offset,
        WireEventKind::NoteOn {
            channel: 0,
            key,
            velocity: 127,
            note_id: u32::MAX,
        },
    )
}

pub fn note_off(offset: u32, key: u8) -> WireEvent {
    WireEvent::new(
        offset,
        WireEventKind::NoteOff {
            channel: 0,
            key,
            note_id: u32::MAX,
        },
    )
}

pub fn param(offset: u32, id: u32, value: f64) -> WireEvent {
    WireEvent::new(offset, WireEventKind::ParamValue { id, value })
}

/// How to start a bridge.
#[derive(Debug, Default, Clone)]
pub struct SpawnOptions {
    /// Extra command-line arguments.
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    /// `RLIMIT_RTPRIO` for the bridge (`Some(0)` forbids SCHED_FIFO).
    pub rtprio_limit: Option<u64>,
}

/// A running bridge and the app's ends of it.
pub struct Bridge {
    pub channel: AppChannel,
    pub watcher: ProcessWatcher,
    pub client: BridgeClient,
    pub monitor: BridgeMonitor,
    /// Messages `expect` passed over (logs, values), in order.
    pub seen: Vec<BridgeMsg>,
    pub input: [Vec<f32>; 2],
    /// The sidechain sent with every block (`None`: none, #6).
    pub sidechain: Option<[Vec<f32>; 2]>,
    pub output: [Vec<f32>; 2],
    pub steady_time: u64,
    /// `BLOCK_FLAG_*` bits for the next block only.
    pub next_flags: u32,
    ping_seq: u32,
}

impl Bridge {
    pub fn spawn() -> Bridge {
        Bridge::spawn_with(SpawnOptions::default())
    }

    /// Spawns the binary and runs the start-up protocol up to the memfd: `Hello`, memfd.
    pub fn spawn_with(options: SpawnOptions) -> Bridge {
        let (channel, bridge_fd) = socket_pair().unwrap();
        let raw = bridge_fd.as_raw_fd();
        let mut command = Command::new(BRIDGE);
        command
            .args(["--instance", "--control-fd", &raw.to_string()])
            .args(&options.args)
            .stdout(Stdio::null());
        for (key, value) in &options.env {
            command.env(key, value);
        }
        let rtprio = options.rtprio_limit;
        // SAFETY: runs between fork and exec and only makes async-signal-safe syscalls.
        unsafe {
            command.pre_exec(move || {
                let limit = |n| rustix::process::Rlimit {
                    current: Some(n),
                    maximum: Some(n),
                };
                rustix::process::setrlimit(rustix::process::Resource::Core, limit(0))?;
                // RLIMIT_RTPRIO is Linux only; macOS has no equivalent to limit.
                #[cfg(target_os = "linux")]
                if let Some(n) = rtprio {
                    rustix::process::setrlimit(rustix::process::Resource::Rtprio, limit(n))?;
                }
                #[cfg(not(target_os = "linux"))]
                let _ = rtprio;
                // Let the bridge end of the socket survive exec.
                let fd = BorrowedFd::borrow_raw(raw);
                rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty())?;
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn komponist-bridge");
        let watcher = ProcessWatcher::new(child.id()).expect("pidfd");
        // The watcher reaps the child; our copy of the bridge end must go, or a dead bridge would
        // never close the socket.
        drop(child);
        drop(bridge_fd);

        match channel.recv().expect("Hello") {
            BridgeMsg::Hello { protocol, pid } => {
                assert_eq!(protocol, PROTOCOL_VERSION);
                assert_eq!(pid, watcher.pid());
            }
            other => panic!("expected Hello, got {other:?}"),
        }
        let region = Arc::new(ShmRegion::create().unwrap());
        channel.send_fd(region.fd()).unwrap();
        Bridge {
            channel,
            watcher,
            client: BridgeClient::new(Arc::clone(&region)),
            monitor: BridgeMonitor::new(region),
            seen: Vec::new(),
            input: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
            sidechain: None,
            output: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
            steady_time: 0,
            next_flags: 0,
            ping_seq: 0,
        }
    }

    pub fn pid(&self) -> u32 {
        self.watcher.pid()
    }

    pub fn region(&self) -> &Arc<ShmRegion> {
        self.client.region()
    }

    pub fn send(&self, msg: ControlMsg) {
        self.channel.send(&msg).unwrap();
    }

    /// The next message within `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<BridgeMsg> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(msg) = self.channel.try_recv().expect("control channel") {
                return Some(msg);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let ts = Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: i64::from(left.subsec_nanos()),
            };
            let mut fds = [PollFd::from_borrowed_fd(
                self.channel.as_fd(),
                PollFlags::IN,
            )];
            let _ = rustix::event::poll(&mut fds, Some(&ts));
        }
    }

    /// Waits for the first message `pick` accepts; the messages before it go to `seen`. Panics on
    /// a timeout and on an `Error` that `pick` does not accept.
    pub fn expect<T>(&mut self, what: &str, mut pick: impl FnMut(&BridgeMsg) -> Option<T>) -> T {
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(msg) = self.recv_timeout(left) else {
                panic!("no {what} within {ANSWER_TIMEOUT:?}; seen {:?}", self.seen);
            };
            if let Some(picked) = pick(&msg) {
                return picked;
            }
            if let BridgeMsg::Error { .. } = msg {
                panic!("waiting for {what}: unexpected {msg:?}");
            }
            self.seen.push(msg);
        }
    }

    /// Expects an `Error` of `context` and returns its message.
    pub fn expect_error(&mut self, context: komponist_ipc::ErrorContext) -> String {
        self.expect("an error", |m| match m {
            BridgeMsg::Error {
                context: c,
                message,
                fatal: false,
            } if *c == context => Some(message.clone()),
            _ => None,
        })
    }

    /// Waits until the bridge has handled everything sent before (a `Ping` round trip); the
    /// messages before the `Pong` go to `seen`.
    pub fn sync(&mut self) {
        self.heartbeat();
    }

    /// The watchdog heartbeat: `Ping` → the `Pong` with the same `seq`; returns the round trip.
    pub fn heartbeat(&mut self) -> Duration {
        self.ping_seq = self.ping_seq.wrapping_add(1);
        let seq = self.ping_seq;
        let t0 = Instant::now();
        self.send(ControlMsg::Ping { seq });
        self.expect("Pong", |m| (*m == BridgeMsg::Pong { seq }).then_some(()));
        t0.elapsed()
    }

    /// `RtPriority` messages seen so far: (granted, priority, detail).
    pub fn rt_status(&self) -> Vec<(bool, u32, String)> {
        self.seen
            .iter()
            .filter_map(|m| match m {
                BridgeMsg::RtPriority {
                    granted,
                    priority,
                    detail,
                } => Some((*granted, *priority, detail.clone())),
                _ => None,
            })
            .collect()
    }

    /// Loads `plugin_id` from `bundle`; the format comes from the extension (`.clap`, `.vst3`).
    pub fn load(&mut self, bundle: &Path, plugin_id: &str) -> LoadedInfo {
        self.send(ControlMsg::Load {
            format: komponist_hosting::bundle_format(bundle).unwrap_or(PluginFormat::Clap),
            bundle: bundle.to_path_buf(),
            plugin_id: plugin_id.into(),
        });
        self.expect("Loaded", |m| match m {
            BridgeMsg::Loaded(info) => Some((**info).clone()),
            _ => None,
        })
    }

    /// Activates at 48 kHz for blocks up to `MAX_BLOCK`; returns the reported latency.
    pub fn activate(&mut self) -> u32 {
        self.activate_at(SAMPLE_RATE)
    }

    pub fn activate_at(&mut self, sample_rate: f64) -> u32 {
        self.activate_with(sample_rate, MAX_BLOCK as u32)
    }

    pub fn activate_with(&mut self, sample_rate: f64, max_block: u32) -> u32 {
        self.send(ControlMsg::Activate {
            sample_rate,
            max_block,
        });
        self.expect("Activated", |m| match m {
            BridgeMsg::Activated { latency } => Some(*latency),
            _ => None,
        })
    }

    /// Id of the parameter called `name` in `info`.
    pub fn param_id(info: &LoadedInfo, name: &str) -> u32 {
        info.params
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no parameter {name} in {:?}", info.params))
            .id
    }

    /// Waits for a `ParamValues` that carries parameter `id`; returns its value and text.
    pub fn expect_value(&mut self, id: u32) -> (f64, String) {
        self.expect("ParamValues", |m| match m {
            BridgeMsg::ParamValues(values) => values
                .iter()
                .find(|v| v.id == id)
                .map(|v| (v.value, v.text.clone())),
            _ => None,
        })
    }

    /// Waits for a `ParamValues` that reports parameter `id` at `value` (within 1e-6). Unlike
    /// [`Bridge::expect_value`] this is a sync point after a `SetParam` or `SetState`: the bridge
    /// also reports values on its own (a state load once right away and again, up to
    /// `VALUES_INTERVAL` later, after the plugin's own rescan), and such a report with the old
    /// value can arrive after the `SetParam` was sent but before the bridge read it (#51).
    pub fn expect_value_of(&mut self, id: u32, value: f64) -> String {
        self.expect("ParamValues", |m| match m {
            BridgeMsg::ParamValues(values) => values
                .iter()
                .find(|v| v.id == id && (v.value - value).abs() < 1e-6)
                .map(|v| v.text.clone()),
            _ => None,
        })
    }

    /// `RT priority` log lines seen so far.
    pub fn rt_logs(&self) -> Vec<(LogLevel, String)> {
        self.seen
            .iter()
            .filter_map(|m| match m {
                BridgeMsg::Log { level, message } if message.starts_with(RT_LOG_PREFIX) => {
                    Some((*level, message.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Publishes one block of `BLOCK` frames with `events` and waits up to `timeout` for it.
    pub fn block_with(
        &mut self,
        events: &[WireEvent],
        timeout: Duration,
    ) -> Result<BlockResult, BlockError> {
        let seq = self.begin(events)?;
        let [left, right] = &mut self.output;
        self.client
            .wait_done(seq, Instant::now() + timeout, [left, right])
    }

    /// Publishes one block and returns its sequence number.
    pub fn begin(&mut self, events: &[WireEvent]) -> Result<u32, BlockError> {
        let spec = BlockSpec {
            frames: BLOCK as u32,
            flags: std::mem::take(&mut self.next_flags),
            steady_time: self.steady_time,
            // Marks the block's work interval in the bridge (macOS); the waits use their own.
            deadline: Some(Instant::now() + GENEROUS),
            transport: WireTransport {
                tempo_bpm: 120.0,
                meter_num: 4,
                meter_den: 4,
                ..WireTransport::default()
            },
        };
        let seq = self.client.begin_block(
            &spec,
            [&self.input[0][..BLOCK], &self.input[1][..BLOCK]],
            self.sidechain
                .as_ref()
                .map(|[l, r]| [&l[..BLOCK], &r[..BLOCK]]),
            events,
        )?;
        self.steady_time += BLOCK as u64;
        Ok(seq)
    }

    /// One block whose data matters; it must succeed.
    pub fn block(&mut self, events: &[WireEvent]) -> BlockResult {
        self.block_with(events, GENEROUS)
            .unwrap_or_else(|e| panic!("block: {e:?}"))
    }

    /// Largest absolute output sample of both channels in `range` of the last block.
    pub fn peak(&self, range: std::ops::Range<usize>) -> f32 {
        self.output
            .iter()
            .flat_map(|c| c[range.clone()].iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// `Shutdown` and the exit status.
    pub fn shutdown(&mut self) -> ProcessExit {
        self.send(ControlMsg::Shutdown);
        self.wait_exit()
    }

    pub fn wait_exit(&mut self) -> ProcessExit {
        self.watcher
            .wait_exit(Some(Duration::from_secs(10)))
            .unwrap()
            .expect("the bridge did not exit within 10 s")
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // Never leave a bridge behind, also when a test fails.
        let _ = self.watcher.kill();
        let _ = self.watcher.wait_exit(Some(Duration::from_secs(5)));
    }
}

/// A clean exit.
pub const EXIT_OK: ProcessExit = ProcessExit {
    code: Some(0),
    signal: None,
};

/// Scheduling policy and RT priority of the bridge's RT thread (`kp-bridge-rt`), from
/// `/proc/<pid>/task/*/stat`.
pub fn rt_thread_policy(pid: u32) -> Option<(u32, u32)> {
    for entry in std::fs::read_dir(format!("/proc/{pid}/task")).ok()? {
        let dir = entry.ok()?.path();
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
        if comm.trim() != "kp-bridge-rt" {
            continue;
        }
        let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
        let fields: Vec<&str> = stat[stat.rfind(')')? + 2..].split_whitespace().collect();
        // Fields 41 (policy) and 40 (rt_priority); `fields[0]` is field 3.
        return Some((fields[38].parse().ok()?, fields[37].parse().ok()?));
    }
    None
}

/// CPU time of process `pid` (all threads) in seconds.
pub fn cpu_seconds(pid: u32) -> f64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..]
        .split_whitespace()
        .collect();
    // utime and stime: fields 14 and 15.
    let ticks: f64 = fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap();
    ticks / 100.0
}
