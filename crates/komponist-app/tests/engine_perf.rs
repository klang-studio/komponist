//! Engine CPU in the real callback path (WP-61): the app on the null backend at 48 kHz / 256
//! frames plays 16 pisynth tracks, then the same with a kp-test-fx bridge on 8 of them. The
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
//! them. Debug builds run too, but their numbers mean little (pisynth unoptimised).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use komponist_app::device_host::{BuiltinHost, DeviceHost, PluginHost};
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::settings::Settings;
use komponist_app::{Action, App, TransportAction};
use komponist_audio::{HostKind, StreamConfig};
use komponist_builtin::{encode_state, preset_patch, presets};
use komponist_engine::TransportState;
use komponist_model::edits::{self, NewClip, NewClipContent, NewNote, NewTrack};
use komponist_model::project::BUILTIN_PISYNTH;
use komponist_model::{
    Device, DeviceId, DeviceRole, Document, PluginFormat, PluginRef, Project, Tick, TickRange,
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

fn pisynth() -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: BUILTIN_PISYNTH.into(),
        name: "pisynth".into(),
        vendor: "klang-studio".into(),
        version: "0.1.0".into(),
        module: Box::default(),
    }
}

/// 16 pisynth tracks (presets E-Piano, Strings, Supersaw, Init in turn), each with a 4-bar
/// looped clip: a four-note chord per half bar, held, plus a 1/16 line (up to ~8 voices per
/// track, 128 in all); loop over the 4 bars. With `bridges`, the first 8 tracks get a kp-test-fx.
fn project(bridges: bool) -> Project {
    let mut p = Project::new("Perf");
    let names = ["E-Piano", "Strings", "Supersaw", "Init"];
    for i in 0..TRACKS {
        let spec = NewTrack {
            name: Some(format!("Spur {}", i + 1)),
            hue: None,
            instrument: Some(pisynth()),
            index: None,
            arm_exclusive: false,
        };
        let (next, track) = edits::add_track(&p, spec).unwrap();
        p = next;
        let preset = presets().find(|x| x.name == names[i % 4]).unwrap().index;
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
        let state: Arc<[u8]> = encode_state(&preset_patch(preset).unwrap()).into();
        let fx = (bridges && i < BRIDGES).then(|| Device {
            id: DeviceId(p.alloc_id()),
            role: DeviceRole::Effect,
            plugin: clap_plugin(FX),
            bypass: false,
            state: None,
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
    let config = StreamConfig {
        host: HostKind::Null,
        buffer_frames: BUFFER,
        ..StreamConfig::default()
    };
    let (mut engine, started) = EngineSync::new(&config, MidiSetup::None, host);
    started.expect("null backend");
    let status = engine.set_workers(Some(workers)).expect("workers");
    eprintln!("workers: {status:?}");
    App::new(Document::new(project), engine, Settings::default(), None)
}

/// Plays for [`MEASURE`] and prints the callback load.
fn measure(name: &str, app: &mut App) {
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(app, "playback", |a| {
        a.transport() == TransportState::Playing
    });
    // Settle (voices up, caches warm), then measure.
    step_for(app, Duration::from_secs(1));
    let stats = app.engine().stats();
    let (callbacks, overruns, xruns) = (stats.callbacks(), stats.overruns(), stats.xruns());
    stats.take_peak_load();
    let worker_stats = app.engine().worker_stats();
    let (batches, late) = (worker_stats.batches(), worker_stats.late());
    let t0 = Instant::now();
    let mut mean = Vec::new();
    let measure = measure_time();
    while t0.elapsed() < measure {
        step_for(app, Duration::from_millis(100));
        mean.push(app.engine().stats().cpu_load());
    }
    let stats = app.engine().stats();
    let peak = stats.take_peak_load();
    mean.sort_by(f32::total_cmp);
    eprintln!(
        "{name}: {} callbacks of {BUFFER} frames, load mean {:.1} % (median of 100 ms samples), \
         peak {:.1} %, overruns {}, xruns {}",
        stats.callbacks() - callbacks,
        mean[mean.len() / 2] * 100.0,
        peak * 100.0,
        stats.overruns() - overruns,
        stats.xruns() - xruns,
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
fn engine_cpu_16_pisynth_tracks() {
    for workers in worker_counts() {
        let mut app = app(project(false), Box::new(BuiltinHost::new()), workers);
        measure(&format!("16 pisynth tracks, {workers} workers"), &mut app);
        app.shutdown();
    }
}

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn engine_cpu_16_pisynth_tracks_and_8_bridges() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    for workers in worker_counts() {
        bridges_case(&dirs, workers);
    }
}

fn bridges_case(dirs: &komponist_plugins::PluginDirs, workers: u32) {
    let dir = scratch(&format!("engine-perf-{workers}"));
    let mut host = PluginHost::new(config(&dir));
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
    let mut app = app(project(true), Box::new(host), workers);
    step_until(&mut app, "bridges", all_ok);
    measure(
        &format!("16 pisynth tracks + 8 kp-test-fx bridges, {workers} workers"),
        &mut app,
    );
    let late: u64 = bridged(app.project())
        .iter()
        .map(|&d| app.engine().host().late_blocks(d))
        .sum();
    eprintln!("late bridge blocks: {late}");
    app.shutdown();
}
