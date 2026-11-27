//! Engine CPU in the real callback path (WP-61): the app on the null backend at 48 kHz / 256
//! frames plays 16 packaged Hexa tracks with a kp-test-fx bridge on 8 of them (the instruments
//! alone are measured by `engine_perf/bundled.rs` from a verified package). The
//! callback load is the time inside the audio callback over the buffer period (5.33 ms), read
//! from `StreamStats` (smoothed mean, peak of single callbacks, overruns = callbacks longer than
//! their period). Bridge waits count as callback time: that is what decides an xrun.
//!
//! Each case runs once per worker count (WP-65): `KOMPONIST_PERF_WORKERS` (default `0,4`: single
//! threaded, then 4 worker threads). `KOMPONIST_PERF_SECS` sets the measuring time (default 10 s).
//!
//! ```text
//! cargo test --release -p komponist-app --test engine_perf -- --ignored --nocapture
//! KOMPONIST_PERF_WORKERS=4 KOMPONIST_PERF_SECS=300 cargo test --release …   # 5-minute run
//! ```
//!
//! The bridge case needs the WP-27 fixtures (`scripts/build-test-plugins.sh`) and skips without
//! them. Debug builds run too, but their numbers mean little (hexa unoptimised).

#[path = "engine_perf/bundled.rs"]
mod bundled;
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use komponist_app::device_host::{DeviceHost, PluginHost, ProcessorHost};
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::settings::Settings;
use komponist_app::{Action, App, TransportAction};
use komponist_audio::{AudioState, HostKind, StreamConfig};
use komponist_engine::TransportState;
use komponist_model::edits::{self, NewClip, NewClipContent, NewNote, NewTrack};
use komponist_model::{
    Device, DeviceId, DeviceRole, Document, PluginRef, Project, Tick, TickRange,
};
use komponist_plugins::PluginManagerEvent;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const BAR: i64 = 3840;
const TRACKS: usize = 16;
/// Tracks that get a kp-test-fx insert (one bridge each) in the second case.
const BRIDGES: usize = 8;
const BUFFER: u32 = 256;

/// Measuring time (`KOMPONIST_PERF_SECS`, default 10 s).
fn measure_time() -> Duration {
    let secs = std::env::var("KOMPONIST_PERF_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    Duration::from_secs(secs)
}

/// Worker counts to measure (`KOMPONIST_PERF_WORKERS`, default `0,4`).
fn worker_counts() -> Vec<u32> {
    std::env::var("KOMPONIST_PERF_WORKERS")
        .ok()
        .map(|s| s.split(',').filter_map(|w| w.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0, 4])
}

/// 16 Hexa tracks (presets E-Piano, Strings, Supersaw, Init in turn) of the packaged `host`, each
/// with a 4-bar looped clip: a four-note chord per half bar, held, plus a 1/16 line (up to ~8
/// voices per track, 128 in all); loop over the 4 bars. With `bridges`, the first 8 tracks get a
/// kp-test-fx.
fn project(bridges: bool, host: &PluginHost) -> Project {
    project_with_hexa(bridges, |name| {
        let hexa = host
            .manager()
            .prepare_bundled_device("org.klang-studio.komponist.hexa", Some(name), &[], &[])
            .unwrap_or_else(|error| panic!("Hexa preset {name}: {error}"));
        (hexa.plugin, hexa.state.expect("factory preset state"))
    })
}

const HEXA_PRESETS: [&str; 4] = ["E-Piano", "Strings", "Supersaw", "Init"];

fn project_with_hexa(
    bridges: bool,
    instrument: impl Fn(&str) -> (PluginRef, Arc<[u8]>),
) -> Project {
    let mut p = Project::new("Perf");
    for i in 0..TRACKS {
        let (plugin, state) = instrument(HEXA_PRESETS[i % HEXA_PRESETS.len()]);
        let spec = NewTrack {
            name: Some(format!("Spur {}", i + 1)),
            hue: None,
            instrument: Some(plugin),
            index: None,
            arm_exclusive: false,
        };
        let (next, track) = edits::add_track(&p, spec).unwrap();
        p = next;
        let mut notes = Vec::new();
        for half in 0..8i64 {
            let root = 48 + ((half as usize * 5 + i) % 12) as u8;
            for (k, interval) in [0, 4, 7, 11].into_iter().enumerate() {
                notes.push(NewNote {
                    start: Tick(half * BAR / 2 + k as i64 * 10),
                    length: Tick(BAR / 2 - 30),
                    pitch: root + interval,
                    velocity: 90,
                });
            }
        }
        for step in 0..64 {
            notes.push(NewNote {
                start: Tick(step * BAR / 16),
                length: Tick(BAR / 32),
                pitch: 72 + ((step * 7 + i as i64) % 12) as u8,
                velocity: 70,
            });
        }
        let (next, _) = edits::add_clip(
            &p,
            track,
            NewClip {
                start: Tick::ZERO,
                length: Tick(4 * BAR),
                name: "Last".into(),
                content: NewClipContent::Midi {
                    notes,
                    take_name: String::new(),
                },
            },
        )
        .unwrap();
        p = next;
        let fx = (bridges && i < BRIDGES).then(|| Device {
            id: DeviceId(p.alloc_id().expect("fixture identifiers")),
            role: DeviceRole::Effect,
            plugin: clap_plugin(FX),
            bypass: false,
            parameter_bases: Default::default(),
            asset_links: Default::default(),
            state: None,
            sidechain: None,
            preset: None,
        });
        let t = Arc::make_mut(&mut p.tracks[i]);
        Arc::make_mut(&mut t.channel.devices[0]).state = Some(state);
        t.channel.devices.extend(fx.map(Arc::new));
    }
    p.loop_range = TickRange::new(Tick::ZERO, Tick(4 * BAR));
    p.loop_on = true;
    p
}

fn app(project: Project, host: Box<dyn DeviceHost>, workers: u32) -> App {
    app_at_buffer(project, host, workers, HostKind::Null, BUFFER)
}

fn app_at_buffer(
    project: Project,
    host: Box<dyn DeviceHost>,
    workers: u32,
    audio_host: HostKind,
    buffer_frames: u32,
) -> App {
    let config = StreamConfig {
        host: audio_host,
        buffer_frames,
        ..StreamConfig::default()
    };
    let (mut engine, started) = EngineSync::new(&config, MidiSetup::None, host);
    started.unwrap_or_else(|error| panic!("could not start {audio_host:?}: {error}"));
    require_audio_config(&engine, audio_host, config.sample_rate, buffer_frames);
    let status = engine.set_workers(Some(workers)).expect("workers");
    eprintln!("workers: {status:?}");
    let settings = Settings {
        audio: config,
        ..Settings::default()
    };
    App::new(Document::new(project), engine, settings, None)
}

fn require_audio_config(engine: &EngineSync, host: HostKind, sample_rate: u32, frames: u32) {
    let state = engine.audio_state();
    assert!(
        matches!(state, AudioState::Running { config, .. }
            if (config.host, config.sample_rate, config.buffer_frames) == (host, sample_rate, frames)),
        "{host:?} did not provide {sample_rate} Hz and a {frames}-frame period: {state:?}"
    );
}

fn step_for_measurement(app: &mut App, config: &StreamConfig, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        app.step();
        require_audio_config(
            app.engine(),
            config.host,
            config.sample_rate,
            config.buffer_frames,
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Plays for the configured interval and prints the callback counters.
fn measure(name: &str, app: &mut App, buffer: u32, devices: &[DeviceId]) {
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(app, "playback", |a| {
        a.transport() == TransportState::Playing
    });
    // Settle (voices up, caches warm), then measure.
    let config = app.settings().audio.clone();
    step_for_measurement(app, &config, Duration::from_secs(1));
    let stats = app.engine().stats();
    let (callbacks, overruns, xruns) = (stats.callbacks(), stats.overruns(), stats.xruns());
    stats.take_peak_load();
    stats.start_callback_timing();
    let worker_stats = app.engine().worker_stats();
    let (batches, late) = (worker_stats.batches(), worker_stats.late());
    let missed = |app: &App| {
        devices
            .iter()
            .map(|&id| app.engine().host().late_blocks(id))
            .sum::<u64>()
    };
    let missed_before = missed(app);
    let t0 = Instant::now();
    let mut sampled = Vec::new();
    let measure = measure_time();
    while t0.elapsed() < measure {
        step_for_measurement(app, &config, Duration::from_millis(100));
        sampled.push(app.engine().stats().cpu_load());
    }
    let stats = app.engine().stats();
    let peak = stats.take_peak_load();
    let [p95, p99] = stats
        .callback_percentiles()
        .expect("recorded audio callbacks");
    assert!(
        !sampled.is_empty(),
        "the measurement interval must be positive"
    );
    sampled.sort_by(f32::total_cmp);
    eprintln!(
        "{name}: {} callbacks of {buffer} frames, load {:.1} % (median of smoothed 100 ms samples), \
         callback p95/p99 {p95}/{p99} % (400 % includes overflow), peak {:.1} %, \
         overruns {}, xruns {}, late bridge blocks {}",
        stats.callbacks() - callbacks,
        sampled[sampled.len() / 2] * 100.0,
        peak * 100.0,
        stats.overruns() - overruns,
        stats.xruns() - xruns,
        missed(app).saturating_sub(missed_before),
    );
    let worker_stats = app.engine().worker_stats();
    eprintln!(
        "{name}: {} batches, {} late waits for workers",
        worker_stats.batches() - batches,
        worker_stats.late() - late
    );
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    app.step();
}

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn engine_cpu_16_hexa_tracks_and_8_bridges() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    for workers in worker_counts() {
        bridges_case(&dirs, workers);
    }
}

fn bridges_case(dirs: &komponist_plugins::PluginDirs, workers: u32) {
    let mut host = App::headless_plugin_host();
    host.start_scan(dirs);
    let deadline = Instant::now() + PATIENCE;
    'scan: loop {
        for event in host.poll() {
            if matches!(event, PluginManagerEvent::ScanFinished(_)) {
                break 'scan;
            }
        }
        assert!(Instant::now() < deadline, "scan");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut app = app(project(true, &host), Box::new(host), workers);
    step_until(&mut app, "bridges", all_ok);
    let ids = bridged(app.project());
    measure(
        &format!("16 hexa tracks + 8 kp-test-fx bridges, {workers} workers"),
        &mut app,
        BUFFER,
        &ids,
    );
    app.shutdown();
}

/// Measures worst-case block-rate fan-out with each of 16 × 8 inputs changing at once.
/// Two precompiled programs alternate so the command budget does not limit the number
/// of changed controls. The baseline exercises the same program swaps and processors.
#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn engine_cpu_16_tracks_with_track_macros() {
    use komponist_engine::automation::{Parameter, PlainMap};
    use komponist_engine::compile::{self, CompileEnv};
    use komponist_engine::testing::TestGain;
    use komponist_engine::{Command, Engine, EngineConfig, LiveInput, MidiPort};
    use komponist_model::automation::AutomationTarget;
    use komponist_model::macros::Mapping;
    use komponist_model::params::ParamId;

    struct Parameters;
    impl CompileEnv for Parameters {
        fn midi_port(&self, _: &str) -> Option<MidiPort> {
            None
        }

        fn device_latency(&self, _: DeviceId) -> u32 {
            0
        }

        fn automation_parameter(&self, _: &Device, param: &ParamId) -> Option<Parameter> {
            Some(Parameter {
                wire_id: param.plugin_id()?,
                map: PlainMap::linear(0.0, 1.0, 0),
                base: 0.0,
            })
        }
    }

    const RATE: f32 = 48_000.0;
    const BLOCKS: usize = 1_000;
    let measure = |mapped: bool| {
        let mut project = project_with_hexa(false, |_| {
            (
                common::clap_plugin("org.klang-studio.komponist.hexa"),
                Arc::from(&[][..]),
            )
        });
        for track in &mut project.tracks {
            let track = Arc::make_mut(track);
            let device = track.channel.devices[0].id;
            for (index, slot) in track.macros.0.iter_mut().enumerate() {
                slot.value = 0.1;
                if mapped {
                    slot.mappings.extend((0..4).map(|wire| Mapping {
                        target: AutomationTarget::Param {
                            device,
                            param: ParamId::plugin((index * 4 + wire) as u32),
                        },
                        label: String::new(),
                        min: 0.0,
                        max: 1.0,
                        invert: false,
                        bend: 0.4,
                    }));
                }
            }
        }
        let low = compile::build(&project, None, &Parameters);
        for track in &mut project.tracks {
            for slot in &mut Arc::make_mut(track).macros.0 {
                slot.value = 0.9;
            }
        }
        let high = compile::build(&project, Some(&low), &Parameters);
        let (mut engine, mut handle) = Engine::new(EngineConfig {
            sample_rate: RATE,
            max_block: BUFFER as usize,
            seed: 0,
        });
        for track in &low.tracks {
            handle
                .send(Command::InsertProcessor {
                    slot: track.chain[0].slot,
                    processor: Box::new(TestGain::new(1.0)),
                })
                .unwrap();
        }
        handle
            .send(Command::SetProgram(Box::new(low.clone())))
            .unwrap();
        handle.send(Command::Play).unwrap();
        let mut left = [0.0; BUFFER as usize];
        let mut right = [0.0; BUFFER as usize];
        let input = LiveInput {
            midi: &[],
            output_latency: 0,
        };
        let mut total = Duration::ZERO;
        for block in 0..(BLOCKS + 50) {
            let program = if block % 2 == 0 { &high } else { &low };
            handle
                .send(Command::SetProgram(Box::new(program.clone())))
                .unwrap();
            let start = Instant::now();
            assert_no_alloc::assert_no_alloc(|| engine.process(&mut left, &mut right, &input));
            let elapsed = start.elapsed();
            if block >= 50 {
                total += elapsed;
            }
            while handle.poll().is_some() {}
        }
        assert_eq!(handle.telemetry().dropped_events(), 0);
        total / BLOCKS as u32
    };
    let baseline = measure(false);
    let mapped = measure(true);
    let extra = mapped.saturating_sub(baseline);
    let period = Duration::from_secs_f32(BUFFER as f32 / RATE);
    eprintln!(
        "16 tracks × 8 macros × 4 mappings: baseline {baseline:?}, fan-out {mapped:?}, \
         added {extra:?} ({:.2}% of the period)",
        100.0 * extra.as_secs_f64() / period.as_secs_f64(),
    );
    if !cfg!(debug_assertions) {
        assert!(
            extra <= period.mul_f64(0.02),
            "macro fan-out exceeds 2% of a block"
        );
    }
}
