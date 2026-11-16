//! Drives a sine generator through the engine slot mechanism and switches the buffer size every
//! 2 s, printing callbacks, xruns, CPU load and whether the engine object survived each rebuild.
//!
//! **Silent by default:** the generator runs and its phase advances, but it writes zeros. `--tone`
//! makes it audible at −40 dBFS.
//!
//! ```text
//! cargo run -p komponist-audio --example sine -- [--tone] [--seconds N]
//!     [--host coreaudio|wasapi|pipewire|jack|alsa|null] [--device NAME] [--rate HZ]
//! ```
//!
//! Debug builds install `assert_no_alloc`'s allocator: any allocation or free inside the audio
//! callback aborts the run.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use komponist_audio::{
    AudioProcess, AudioState, AudioSystem, CallbackInfo, HostKind, MidiIn, MidiInputHub,
    OutputBuffer, StreamConfig, StreamParams,
};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Tone frequency in Hz.
const FREQUENCY: f32 = 440.0;
/// −40 dBFS.
const TONE_GAIN: f32 = 0.01;
/// Buffer sizes in the order the example switches through them.
const BUFFERS: [u32; 5] = [256, 128, 512, 64, 1024];
/// Time per buffer size.
const PHASE: Duration = Duration::from_secs(2);

/// Sine generators ever constructed.
static INSTANCES: AtomicU32 = AtomicU32::new(0);

/// What the generator reports to the main thread.
#[derive(Debug, Default)]
struct Probe {
    /// Address of the generator, written in every `prepare`.
    address: AtomicUsize,
    /// `prepare` calls.
    prepares: AtomicU32,
    /// Distinct addresses seen in `prepare` (1 = the same object every time).
    addresses: AtomicU32,
    /// Frames rendered.
    frames: AtomicU64,
    /// Live MIDI events received.
    midi_events: AtomicU32,
}

/// The "engine": a sine oscillator on a `u32` phase.
struct Sine {
    phase: u32,
    increment: u32,
    gain: f32,
    left: Vec<f32>,
    right: Vec<f32>,
    probe: Arc<Probe>,
}

impl Sine {
    fn new(gain: f32, probe: Arc<Probe>) -> Sine {
        INSTANCES.fetch_add(1, Ordering::Relaxed);
        Sine {
            phase: 0,
            increment: 0,
            gain,
            left: Vec::new(),
            right: Vec::new(),
            probe,
        }
    }
}

impl AudioProcess for Sine {
    fn prepare(&mut self, params: &StreamParams) {
        self.increment = klang_dsp::phase::increment(FREQUENCY, params.sample_rate as f32);
        self.left.resize(params.max_frames as usize, 0.0);
        self.right.resize(params.max_frames as usize, 0.0);
        let address = self as *const Sine as usize;
        if self.probe.address.swap(address, Ordering::Relaxed) != address {
            self.probe.addresses.fetch_add(1, Ordering::Relaxed);
        }
        self.probe.prepares.fetch_add(1, Ordering::Relaxed);
    }

    fn process(&mut self, out: &mut OutputBuffer<'_>, _info: &CallbackInfo, midi: &mut MidiIn<'_>) {
        while midi.pop().is_some() {
            self.probe.midi_events.fetch_add(1, Ordering::Relaxed);
        }
        let frames = out.frames().min(self.left.len());
        for (l, r) in self.left[..frames]
            .iter_mut()
            .zip(&mut self.right[..frames])
        {
            let sample = klang_dsp::sine::sine(self.phase) * self.gain;
            *l = sample;
            *r = sample;
            self.phase = self.phase.wrapping_add(self.increment);
        }
        out.write_stereo(&self.left[..frames], &self.right[..frames]);
        self.probe
            .frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
}

struct Args {
    tone: bool,
    seconds: u64,
    host: HostKind,
    device: Option<String>,
    rate: Option<u32>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        tone: false,
        seconds: 10,
        host: HostKind::platform_default(),
        device: None,
        rate: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--tone" => args.tone = true,
            "--seconds" => {
                args.seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?
            }
            "--rate" => args.rate = Some(value()?.parse().map_err(|e| format!("--rate: {e}"))?),
            "--device" => args.device = Some(value()?),
            "--host" => {
                args.host = match value()?.as_str() {
                    "coreaudio" => HostKind::CoreAudio,
                    "wasapi" => HostKind::Wasapi,
                    "pipewire" => HostKind::PipeWire,
                    "jack" => HostKind::Jack,
                    "alsa" => HostKind::Alsa,
                    "null" => HostKind::Null,
                    other => return Err(format!("unknown host {other}")),
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

/// Threads of this process with a real-time scheduling policy, as "name (policy prio)".
fn realtime_threads() -> Vec<String> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    let mut threads = Vec::new();
    for task in tasks.flatten() {
        let Ok(stat) = std::fs::read_to_string(task.path().join("stat")) else {
            continue;
        };
        // Fields after the parenthesised name start at field 3; rt_priority is 40, policy 41.
        let Some((head, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let name = head.split_once('(').map_or("?", |(_, n)| n);
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let (Some(prio), Some(policy)) = (fields.get(37), fields.get(38)) else {
            continue;
        };
        let policy = match *policy {
            "1" => "SCHED_FIFO",
            "2" => "SCHED_RR",
            "6" => "SCHED_DEADLINE",
            _ => continue,
        };
        threads.push(format!("{name} ({policy} {prio})"));
    }
    threads
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let hosts = AudioSystem::<Sine>::available_hosts();
    println!("hosts: {hosts:?}");
    for &host in &hosts {
        match AudioSystem::<Sine>::devices(host) {
            Ok(devices) => {
                for d in devices {
                    println!(
                        "  {host:?}: {:?}{} · id {} · {} ch · rates {:?} · buffers {:?}",
                        d.name,
                        if d.is_default { " (default)" } else { "" },
                        d.id.as_ref().map_or("none".to_owned(), ToString::to_string),
                        d.channels,
                        d.sample_rates,
                        d.buffer_range,
                    );
                }
            }
            Err(e) => println!("  {host:?}: {e}"),
        }
    }

    let (mut hub, midi) = match MidiInputHub::new() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("MIDI: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = hub.refresh() {
        println!("MIDI refresh: {e}");
    }
    for port in hub.ports() {
        println!(
            "MIDI in {:?}: {:?}, {}, {}",
            port.id,
            port.name,
            if port.enabled { "enabled" } else { "disabled" },
            if port.connected { "present" } else { "absent" },
        );
    }

    let sample_rate = args.rate.unwrap_or_else(|| {
        AudioSystem::<Sine>::devices(args.host)
            .ok()
            .and_then(|devices| {
                let device = devices.into_iter().find(|d| match &args.device {
                    Some(name) => d.name == *name,
                    None => d.is_default,
                })?;
                let rates = device.sample_rates;
                rates
                    .contains(&48_000)
                    .then_some(48_000)
                    .or(rates.first().copied())
            })
            .unwrap_or(48_000)
    });

    let probe = Arc::new(Probe::default());
    let gain = if args.tone { TONE_GAIN } else { 0.0 };
    let mut system = AudioSystem::new(Box::new(Sine::new(gain, Arc::clone(&probe))), midi);
    println!(
        "output: {} · {sample_rate} Hz · switching {BUFFERS:?} every {} s",
        if args.tone {
            "tone at −40 dBFS"
        } else {
            "silence"
        },
        PHASE.as_secs()
    );

    let started = Instant::now();
    let end = started + Duration::from_secs(args.seconds);
    let mut starts = 0u32;
    let mut rt_threads = Vec::new();
    for &buffer in BUFFERS.iter().cycle() {
        if Instant::now() >= end {
            break;
        }
        let config = StreamConfig {
            host: args.host,
            device: args.device.clone(),
            device_id: None,
            sample_rate,
            buffer_frames: buffer,
        };
        let stats = Arc::clone(system.stats());
        let (callbacks0, xruns0, frames0) = (
            stats.callbacks(),
            stats.xruns(),
            probe.frames.load(Ordering::Relaxed),
        );
        let switch = Instant::now();
        let result = system.start(&config);
        let switch_time = switch.elapsed();
        starts += 1;
        let phase_end = (Instant::now() + PHASE).min(end);
        let mut max_load = 0.0f32;
        while Instant::now() < phase_end {
            std::thread::sleep(Duration::from_millis(20));
            system.poll();
            max_load = max_load.max(stats.cpu_load());
            if rt_threads.is_empty() {
                rt_threads = realtime_threads();
            }
        }
        let callbacks = stats.callbacks() - callbacks0;
        let frames = probe.frames.load(Ordering::Relaxed) - frames0;
        let state = match system.state() {
            AudioState::Running { config, latency } => format!(
                "Running {:?} {} Hz / {} frames, latency {:.1} ms",
                config.host,
                config.sample_rate,
                config.buffer_frames,
                latency.as_secs_f64() * 1000.0
            ),
            other => format!("{other:?}"),
        };
        println!(
            "[{:4.1} s] buffer {buffer:4}: {} (rebuild {:.0} ms) · {state} · callbacks {callbacks} ({} frames/callback) · xruns {} · CPU {:.2} % (max {:.2} %) · prepares {} · engine objects {} at {:#x}",
            started.elapsed().as_secs_f64(),
            if result.is_ok() { "ok" } else { "FAILED" },
            switch_time.as_secs_f64() * 1000.0,
            frames.checked_div(callbacks).unwrap_or(0),
            stats.xruns() - xruns0,
            stats.cpu_load() * 100.0,
            max_load * 100.0,
            probe.prepares.load(Ordering::Relaxed),
            probe.addresses.load(Ordering::Relaxed),
            probe.address.load(Ordering::Relaxed),
        );
        if let Err(e) = result {
            println!("  error: {e}");
        }
    }

    system.stop();
    let stats = Arc::clone(system.stats());
    let engine = system
        .detach(Duration::from_secs(1))
        .expect("engine back after stop");
    let frames = probe.frames.load(Ordering::Relaxed);
    let expected_phase = (frames as u32).wrapping_mul(engine.increment);
    println!(
        "real-time threads: {}",
        if rt_threads.is_empty() {
            "none".to_owned()
        } else {
            rt_threads.join(", ")
        }
    );
    println!(
        "summary: {} s · {starts} starts ({} rebuilds) · {} callbacks · {} xruns · CPU {:.2} % · {} MIDI events",
        started.elapsed().as_secs(),
        starts.saturating_sub(1),
        stats.callbacks(),
        stats.xruns(),
        stats.cpu_load() * 100.0,
        probe.midi_events.load(Ordering::Relaxed),
    );
    println!(
        "engine: {} instance(s) constructed · same object in all {} prepares: {} · generator state kept (phase {:#010x} == frames × increment {:#010x}): {}",
        INSTANCES.load(Ordering::Relaxed),
        probe.prepares.load(Ordering::Relaxed),
        probe.addresses.load(Ordering::Relaxed) == 1
            && probe.address.load(Ordering::Relaxed) == &*engine as *const Sine as usize,
        engine.phase,
        expected_phase,
        engine.phase == expected_phase,
    );
}
