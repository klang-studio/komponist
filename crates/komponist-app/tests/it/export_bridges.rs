//! WP-53b with real bridges (the WP-27 fixtures kp-test-synth and kp-test-fx): the export waits
//! while the bridges load, renders offline at another sample rate once they re-activated, and
//! gives the engine back. Skips with a note without the fixtures (`scripts/build-test-plugins.sh`).

use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use komponist_app::actions::{ExportRange, ExportSettings};
use komponist_app::device_host::PluginHost;
use komponist_app::settings;
use komponist_app::{Action, EditAction, ProjectAction, TransportAction};
use komponist_engine::TransportState;
use komponist_engine::export::ExportJob;
use komponist_model::{ClipContent, Project, Tick, TickRange};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};

const BAR: i64 = 3840;

/// "Pad" of the demo with test plugins (kp-test-synth, two kp-test-fx), its clip cut to one bar
/// (chords from tick 0): the song is one bar.
fn pad_bar() -> Project {
    let mut p = demo_with_test_plugins();
    p.tracks.retain(|t| t.name == "Pad");
    let track = Arc::make_mut(&mut p.tracks[0]);
    for clip in &mut track.clips {
        Arc::make_mut(clip).length = Tick(BAR);
    }
    p
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// The left and right samples of a 24-bit file as f32.
fn read(path: &std::path::Path) -> (hound::WavSpec, Vec<f32>) {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    let samples = reader
        .samples::<i32>()
        .map(|s| s.unwrap() as f32 / 8_388_608.0)
        .collect();
    (spec, samples)
}

fn scanned_host(dir: &std::path::Path, dirs: &komponist_plugins::PluginDirs) -> PluginHost {
    let mut host = PluginHost::new(config(dir));
    komponist_app::device_host::PluginCatalogHost::start_scan(&mut host, dirs);
    let deadline = Instant::now() + PATIENCE;
    'scan: loop {
        for event in komponist_app::device_host::ProcessorHost::poll(&mut host) {
            if matches!(event, PluginManagerEvent::ScanFinished(_)) {
                break 'scan;
            }
        }
        assert!(Instant::now() < deadline, "scan");
        std::thread::sleep(Duration::from_millis(5));
    }
    host
}

#[test]
fn bridges_load_reactivate_at_the_export_rate_and_sound_from_the_first_note() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("export-bridges");
    let host = scanned_host(&dir, &dirs);
    let mut app = app_with(pad_bar(), host);
    app.step();
    let bridged = bridged(app.project());
    assert_eq!(bridged.len(), 3);
    assert!(
        bridged
            .iter()
            .any(|&d| app.engine().device_status(d) == DeviceStatus::Loading),
        "{:?}",
        statuses(&app)
    );

    // Right away, while the bridges load: the export waits for them, then renders at 44.1 kHz
    // (the stream runs at 48 kHz, so every processor re-activates).
    let export = |rate: u32, name: &str| ExportSettings {
        range: ExportRange::Song,
        sample_rate: rate,
        tail_seconds: 0.5,
        normalize: false,
        file_name: name.into(),
        ..settings::default_export("Pad", dir.join("out"))
    };
    app.dispatch(Action::Project(ProjectAction::StartExport(export(
        44_100, "44k.wav",
    ))));
    app.step();
    assert!(app.is_exporting());
    // A bridge that finished loading since the last frame lets the export start at once; only
    // while one still loads must the export wait.
    if bridged
        .iter()
        .any(|&d| app.engine().device_status(d) == DeviceStatus::Loading)
    {
        assert!(!app.engine().is_detached(), "waits for the bridges");
    }
    step_until(&mut app, "end of the export", |a| !a.is_exporting());
    // Back at 48 kHz the bridges re-activate again: `Loading` until they run at it (WP-61), so a
    // second export right away waits for them instead of rendering their silent fallback.
    assert!(
        bridged
            .iter()
            .any(|&d| app.engine().device_status(d) == DeviceStatus::Loading),
        "{:?}",
        statuses(&app)
    );

    for (rate, name) in [(44_100, "44k.wav"), (48_000, "48k.wav")] {
        if rate == 48_000 {
            app.dispatch(Action::Project(ProjectAction::StartExport(export(
                rate, name,
            ))));
            step_until(&mut app, "end of the export", |a| !a.is_exporting());
        }
        let (spec, samples) = read(&dir.join("out").join(name));
        assert_eq!(spec.sample_rate, rate);
        let program = app.engine().program().unwrap();
        let song = ExportJob::frames_of(
            &program.tempo_map,
            TickRange::new(Tick::ZERO, Tick(BAR)),
            rate,
        );
        let tail = ExportJob::tail_frames(0.5, rate);
        assert_eq!(samples.len() as u64, 2 * (song.end + tail), "{name}");
        // The chord sounds from its start (after the two effects' 64-frame delays): the bridges
        // ran at the export rate from the first block on, not their silent fallback.
        let first = rms(&samples[2 * 256..2 * 4_096]);
        let body = rms(&samples[..2 * song.end as usize]);
        assert!(first > 0.01, "{name}: first 80 ms rms {first}, body {body}");
        assert!(first > body / 4.0, "{name}: first {first}, body {body}");
        let status = app.ui().status.clone().unwrap();
        assert!(
            status.text.starts_with(&format!("{name} exportiert")),
            "{}",
            status.text
        );
    }

    // Back in the stream at 48 kHz with the bridges running: it plays.
    assert_eq!(app.engine().sample_rate(), 48_000);
    step_until(&mut app, "bridges back at 48 kHz", all_ok);
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(&mut app, "playback", |a| {
        a.transport() == TransportState::Playing && a.engine().playhead() > Tick::ZERO
    });
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    app.shutdown();
}

#[test]
fn flatten_reactivates_the_bridges_and_undo_restores_their_states() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let host = scanned_host(dir.path(), &dirs);
    let project = pad_bar();
    let track = project.tracks[0].id;
    let mut app =
        app_with(project.clone(), host).with_recording_directory(dir.path().join("audio"));
    app.step();
    app.dispatch(Action::Edit(EditAction::FlattenMidiTrack(track)));
    step_until(&mut app, "flatten finished", |app| !app.is_exporting());
    let clip = &app.project().track(track).unwrap().clips[0];
    let ClipContent::Audio(audio) = &clip.content else {
        panic!("flatten failed: {:?}", app.ui().status);
    };
    let wav = crate::export::read(&audio.file.abs);
    assert!(rms(&wav.samples[512..8192]) > 0.01);
    app.dispatch(Action::Edit(EditAction::Undo));
    let restored = app.project().track(track).unwrap();
    assert_eq!(restored.clips, project.tracks[0].clips);
    assert_eq!(restored.channel.devices.len(), 3);
    assert!(
        restored
            .channel
            .devices
            .iter()
            .all(|device| device.state.is_some())
    );
    step_until(&mut app, "undo bridge state", all_ok);
    app.shutdown();
}
