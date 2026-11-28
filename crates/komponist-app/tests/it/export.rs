//! WP-53b through the headless app on the null backend and `egui_kittest`: the S08 dialog, the
//! export of song, loop and selection (length = range + tail, normalised peak), another sample
//! rate, cancel, the engine afterwards, the wait for devices, a device that drops out, the
//! persisted settings, the status bar link and Projekt › Exporte, and snapshots next to S08.
//!
//! Exports and settings go to scratch folders under `CARGO_TARGET_TMPDIR`, never to the real
//! home; "Im Ordner zeigen" records the folder instead of running `xdg-open`.

use crate::fake_host;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{BitDepth, ExportRange, ExportSettings};
use komponist_app::dialogs::export::{self as view};
use komponist_app::settings::{self, Settings};
use komponist_app::ui_state::{Dialog, FocusArea, StatusKind};
use komponist_app::{Action, App, ProjectAction, TransportAction, UiAction};
use komponist_engine::TransportState;
use komponist_engine::export::{ExportJob, NORMALIZE_TARGET_DB};
use komponist_engine::testing::TestPanic;
use komponist_i18n::{Lang, de};
use komponist_ipc::RenderMode;
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::{
    ChannelRef, Device, DeviceId, DeviceRole, Document, PluginFormat, PluginRef, Project, Tick,
    TickRange,
};
use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};
use komponist_processor::{
    Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_session::device_host::{
    AssetHost, DeviceHost, DevicePresentationHost, DeviceStateHost, OfflineRenderHost,
    ParameterHost, PluginCatalogHost, PluginHost, PluginWindowHost, PresetHost, ProcessorHost,
};
use komponist_session::engine_sync::EngineSync;

use fake_host::FakeHost;

/// How long an export may take in a debug build on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(120);

/// One bar at 4/4 in ticks.
const BAR: i64 = 3840;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

/// A fresh scratch folder for one test.
pub(crate) fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-export-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// The demo "Night Drive" with the shipped Hexa for every instrument and no effects, so that
/// everything sounds on the packaged host and no device is missing.
fn hexa_demo() -> Project {
    let hexa = crate::common::shipped(
        &komponist_session::device_host::PluginHost::headless_fixture(),
        "hexa",
    );
    let mut p = demo_project();
    for track in &mut p.tracks {
        let track = Arc::make_mut(track);
        track
            .channel
            .devices
            .retain(|d| d.role == DeviceRole::Instrument);
        for device in &mut track.channel.devices {
            let device = Arc::make_mut(device);
            device.plugin = hexa.clone();
            device.state = None;
        }
    }
    p
}

/// [`hexa_demo`] cut to its first bar (debug builds render slowly): the clips that start at
/// tick 0, each one bar long. The song ends after bar 1.
fn one_bar_song() -> Project {
    let mut p = hexa_demo();
    for track in &mut p.tracks {
        let track = Arc::make_mut(track);
        track.clips.retain(|c| c.start == Tick::ZERO);
        for clip in &mut track.clips {
            Arc::make_mut(clip).length = Tick(BAR);
        }
    }
    p
}

/// The shipped Presse.
fn presse() -> PluginRef {
    crate::common::shipped(
        &komponist_session::device_host::PluginHost::headless_fixture(),
        "presse",
    )
}

/// An effect device for `plugin` at the end of the first track's chain.
fn with_effect(project: Project, plugin: PluginRef) -> (Project, DeviceId) {
    let track = project.tracks[1].id;
    let len = project.tracks[1].channel.devices.len();
    let spec = NewDevice {
        role: DeviceRole::Effect,
        plugin,
        state: None,
        sidechain: None,
        preset: None,
    };
    edits::insert_device(&project, ChannelRef::Track(track), len, spec).unwrap()
}

/// Folders "Im Ordner zeigen" opened.
type Opened = Arc<Mutex<Vec<PathBuf>>>;

/// The app on the null backend with `host`, settings in `dir/config`.
fn app_with(dir: &Path, project: Project, host: Box<dyn DeviceHost>) -> (App, Opened) {
    let mut settings = Settings::default();
    settings.audio.host = komponist_audio::HostKind::Null;
    let engine = EngineSync::null(host);
    let opened: Opened = Arc::default();
    let log = Arc::clone(&opened);
    let app = App::new(
        Document::new(project),
        engine,
        settings,
        Some(settings_path(dir)),
    )
    .with_folder_opener(Box::new(move |folder| {
        log.lock().unwrap().push(folder.to_path_buf());
        Ok(())
    }));
    (app, opened)
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("config").join(settings::FILE_NAME)
}

pub(crate) fn app_in(dir: &Path, project: Project) -> (App, Opened) {
    app_with(
        dir,
        project,
        Box::new(komponist_session::device_host::PluginHost::headless_fixture()),
    )
}

/// Export settings into `dir/out`: the defaults, `range`, "Night Drive.wav".
pub(crate) fn export_settings(dir: &Path, range: ExportRange) -> ExportSettings {
    ExportSettings {
        range,
        file_name: "Night Drive.wav".into(),
        ..settings::default_export("Night Drive", dir.join("out"))
    }
}

/// Steps `app` until `done` holds; panics after [`PATIENCE`].
pub(crate) fn step_until(app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        app.step();
        if done(app) {
            return;
        }
        assert!(Instant::now() < deadline, "no {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Starts the export of `settings` and steps until the engine is back.
pub(crate) fn export(app: &mut App, settings: ExportSettings) {
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    step_until(app, "end of the export", |a| !a.is_exporting());
}

#[test]
fn an_unreadable_clap_pad_blocks_wav_export() {
    use komponist_model::plugin_assets::AssetLink;
    use komponist_plugin_api::device_assets::AssetStatus;

    let dir = scratch("unreadable-clap-pad");
    let Some((host, plugin)) =
        crate::common::native_fixture(&dir, "klopfer.clap", "org.klang-studio.komponist.sampler")
    else {
        return;
    };
    let name = plugin.name.clone();
    let (project, track, drums) = crate::common::instrument_project("Sampler", plugin);
    let path = dir.join("corrupt.wav");
    std::fs::write(&path, b"unreadable sample").unwrap();
    let project =
        edits::set_device_asset_links(&project, drums, [(0, AssetLink::new(&path, None))].into())
            .unwrap();
    let project = super::flatten::clip(&project, track, Tick(0), 36, Tick(BAR));
    let (mut app, _) = app_with(&dir, project, Box::new(host));
    step_until(&mut app, "failed CLAP sample", |app| {
        app.engine()
            .host()
            .asset_slots(drums)
            .is_some_and(|inventory| {
                inventory
                    .slots
                    .iter()
                    .any(|slot| slot.id == 0 && matches!(slot.status, AssetStatus::Failed(_)))
            })
    });

    let settings = export_settings(&dir, ExportRange::Song);
    let output = settings.folder.join(&settings.file_name);
    export(&mut app, settings);
    assert!(
        !output.exists(),
        "the failed sample must prevent publishing a WAV"
    );
    assert!(!app.engine().is_detached());
    let lang = app.lang();
    let status = app.ui().status.as_ref().unwrap();
    assert_eq!(status.kind, StatusKind::Error);
    assert_eq!(
        status.text,
        lang.export_failed(lang.flatten_pad_failed(name, 1, "corrupt"))
    );
}

/// A WAV file as f32 samples.
pub(crate) struct Wav {
    pub(crate) spec: hound::WavSpec,
    pub(crate) frames: u64,
    pub(crate) samples: Vec<f32>,
}

pub(crate) fn read(path: &Path) -> Wav {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    let frames = u64::from(reader.duration());
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, _) => reader.samples::<f32>().map(Result::unwrap).collect(),
        (hound::SampleFormat::Int, bits) => {
            let full = (1_i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.unwrap() as f32 / full)
                .collect()
        }
    };
    Wav {
        spec,
        frames,
        samples,
    }
}

pub(crate) fn peak_db(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    20.0 * peak.log10()
}

/// Files in `dir` (hidden ones too), sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The frames of `ticks` at `rate` in `app`'s program.
fn frames_of(app: &App, ticks: TickRange, rate: u32) -> u64 {
    let program = app.engine().program().unwrap();
    let r = ExportJob::frames_of(&program.tempo_map, ticks, rate);
    r.end - r.start
}

/// Whether the engine plays: Play, the playhead moves, Stop.
fn plays(app: &mut App) -> bool {
    let from = app.engine().playhead();
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(app, "playback", |a| {
        a.transport() == TransportState::Playing && a.engine().playhead() > from
    });
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(app, "stop", |a| a.transport() == TransportState::Stopped);
    true
}

fn status(app: &App) -> (String, StatusKind) {
    let s = app.ui().status.clone().expect("a status message");
    (s.text, s.kind)
}

// ------------------------------------------------------------------------------------------------
// Ranges, length, peak, rate
// ------------------------------------------------------------------------------------------------

#[test]
fn missing_audio_aborts_export_with_a_localized_status_and_restores_the_engine() {
    for lang in [Lang::De, Lang::En] {
        let dir = scratch(&format!("missing-audio-{lang:?}"));
        let path = dir.join("missing.wav");
        let (project, track) =
            edits::add_audio_track(&Project::new("Audio"), edits::NewAudioTrack::default())
                .unwrap();
        let (project, _) = edits::add_clip(
            &project,
            track,
            edits::NewClip {
                start: Tick::ZERO,
                length: Tick(BAR),
                name: "Missing".into(),
                content: edits::NewClipContent::Audio {
                    file: komponist_model::AudioFile::new(&path, None, 96_000, 48_000, 2),
                    offset: 0,
                },
            },
        )
        .unwrap();
        let (app, _) = app_in(&dir, project);
        let mut app = app.with_lang(lang);
        app.step();
        export(&mut app, export_settings(&dir, ExportRange::Song));
        assert_eq!(
            status(&app),
            (lang.export_missing_audio(path.display()), StatusKind::Error)
        );
        assert!(app.exported().is_empty());
        assert!(!dir.join("out/Night Drive.wav").exists());
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
        step_until(&mut app, "playback after failed export", |app| {
            app.engine().playhead() > Tick::ZERO
        });
    }
}

#[test]
fn a_warp_error_names_the_clip_and_localizes_the_reason() {
    for (case, (lang, name)) in [
        (Lang::De, "Broken take"),
        (Lang::En, "Broken take"),
        (Lang::De, ""),
        (Lang::En, "   "),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = scratch(&format!("warp-error-{case}"));
        let path = dir.join("invalid.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for _ in 0..48000 {
            writer.write_sample(2048.0_f32).unwrap();
        }
        writer.finalize().unwrap();
        let (project, track) =
            edits::add_audio_track(&Project::new("Audio"), edits::NewAudioTrack::default())
                .unwrap();
        let (project, clip) = edits::add_clip(
            &project,
            track,
            edits::NewClip {
                start: Tick::ZERO,
                length: Tick(960),
                name: name.into(),
                content: edits::NewClipContent::Audio {
                    file: komponist_model::AudioFile::new(&path, None, 48000, 48000, 1),
                    offset: 0,
                },
            },
        )
        .unwrap();
        let project = edits::set_audio_clip_warp(&project, clip, true).unwrap();
        let (app, _) = app_in(&dir, project);
        let mut app = app.with_lang(lang);
        app.step();
        export(&mut app, export_settings(&dir, ExportRange::Song));
        let label = if name.trim().is_empty() {
            clip.0.to_string()
        } else {
            name.to_owned()
        };
        let expected = match lang {
            Lang::De => format!(
                "Export fehlgeschlagen · Warp im Audioclip „{label}“ · Die Audiodaten enthalten ungültige Samples"
            ),
            Lang::En => format!(
                "Export failed · Warp in audio clip “{label}” · The audio contains invalid samples"
            ),
        };
        assert_eq!(status(&app), (expected, StatusKind::Error));
        assert!(app.exported().is_empty());
        assert!(!dir.join("out/Night Drive.wav").exists());
    }
}

#[test]
fn the_song_exports_as_range_plus_tail_normalised_to_minus_one_dbfs() {
    let dir = scratch("song");
    let (mut app, opened) = app_in(&dir, one_bar_song());
    app.step();
    // Playing before: the export stops the transport.
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(&mut app, "playback", |a| a.engine().playhead() > Tick::ZERO);
    export(&mut app, export_settings(&dir, ExportRange::Song));

    let path = dir.join("out/Night Drive.wav");
    assert_eq!(files(&dir.join("out")), ["Night Drive.wav"], "no part file");
    let wav = read(&path);
    assert_eq!(
        (
            wav.spec.sample_rate,
            wav.spec.bits_per_sample,
            wav.spec.channels
        ),
        (48_000, 24, 2)
    );
    // Ganzer Song: tick 0 to the end of the last clip (bar 1), plus 2 s.
    let song = frames_of(&app, TickRange::new(Tick::ZERO, Tick(BAR)), 48_000);
    assert_eq!(wav.frames, song + 96_000);
    let peak = peak_db(&wav.samples);
    assert!(
        (peak - NORMALIZE_TARGET_DB).abs() < 0.01,
        "peak {peak} dBFS"
    );
    // Music in the bar, quiet at the end of the tail.
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let body = rms(&wav.samples[..2 * song as usize]);
    let end = rms(&wav.samples[wav.samples.len() - 9_600..]);
    assert!(body > 0.05 && end < body / 10.0, "rms {body}, tail {end}");

    // Done: S08's status bar message with the link, the dialog closed, the transport stopped.
    assert_eq!(
        status(&app),
        (
            "Night Drive.wav exportiert · 00:04 · 24 bit".to_owned(),
            StatusKind::Success
        )
    );
    assert!(app.ui().dialog.is_none());
    assert_eq!(app.transport(), TransportState::Stopped);
    assert_eq!(app.exported(), std::slice::from_ref(&path));
    let link = app.ui().status.as_ref().and_then(|s| s.link.clone());
    app.dispatch(*link.expect("the status message carries the link").1);
    assert_eq!(*opened.lock().unwrap(), [dir.join("out")]);
    // And the engine plays again.
    assert!(!app.engine().is_detached());
    assert!(plays(&mut app));
}

/// #92: the finished message names what the rendered program left out, as an error.
#[test]
fn the_finished_message_names_the_tracks_the_engine_left_out() {
    let dir = scratch("not-played");
    let mut project = one_bar_song();
    while project.tracks.len() <= komponist_engine::MAX_TRACKS {
        let spec = edits::NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        };
        project = edits::add_track(&project, spec).unwrap().0;
    }
    let (mut app, _) = app_in(&dir, project);
    app.step();
    export(&mut app, export_settings(&dir, ExportRange::Song));
    assert_eq!(
        status(&app),
        (
            "Night Drive.wav exportiert · 00:04 · 24 bit · 1 Spur spielt nicht: mehr als 128 \
             Spuren"
                .to_owned(),
            StatusKind::Error
        )
    );
}

#[test]
fn loop_and_selection_set_the_range() {
    let dir = scratch("ranges");
    let mut project = hexa_demo();
    // The demo's loop 9–13, shortened to bar 9.
    project.loop_range = TickRange::new(Tick(8 * BAR), Tick(9 * BAR));
    let (mut app, _) = app_in(&dir, project);
    app.step();

    // Loop, 16 bit, not normalised, a short tail.
    let settings = ExportSettings {
        tail_seconds: 0.25,
        bit_depth: BitDepth::Int16,
        normalize: false,
        file_name: "Loop".into(),
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, settings);
    let wav = read(&dir.join("out/Loop.wav"));
    assert_eq!(wav.spec.bits_per_sample, 16);
    let bar9 = frames_of(&app, TickRange::new(Tick(8 * BAR), Tick(9 * BAR)), 48_000);
    assert_eq!(wav.frames, bar9 + 12_000);
    let peak = peak_db(&wav.samples);
    assert!(
        peak < -1.5 && peak > -40.0,
        "the level as rendered: {peak} dBFS"
    );

    // Selection: half of bar 3, 32 bit float, no tail.
    let half = TickRange::new(Tick(2 * BAR), Tick(2 * BAR + BAR / 2));
    app.ui_mut().selection.time_range = Some(half);
    let settings = ExportSettings {
        tail_seconds: 0.0,
        bit_depth: BitDepth::Float32,
        file_name: "Auswahl.wav".into(),
        ..export_settings(&dir, ExportRange::Selection)
    };
    export(&mut app, settings);
    let wav = read(&dir.join("out/Auswahl.wav"));
    assert_eq!(wav.spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(wav.frames, frames_of(&app, half, 48_000));
    assert!((peak_db(&wav.samples) - NORMALIZE_TARGET_DB).abs() < 0.01);
    assert_eq!(
        status(&app).0,
        "Auswahl.wav exportiert · 00:01 · 32 bit",
        "{:?}",
        app.ui().status
    );
    // Both are listed, oldest first.
    let names: Vec<_> = app
        .exported()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Loop.wav", "Auswahl.wav"]);

    // Without a loop the dialog falls back to the song and "Loop" is disabled.
    app.dispatch(Action::Transport(TransportAction::ToggleLoop));
    app.ui_mut().selection.time_range = None;
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    match &app.ui().dialog {
        Some(Dialog::Export(d)) => assert_eq!(d.settings.range, ExportRange::Song),
        other => panic!("{other:?}"),
    }
}

#[test]
fn another_sample_rate_renders_at_it_and_the_device_rate_comes_back() {
    let dir = scratch("rate");
    let mut project = hexa_demo();
    project.loop_range = TickRange::new(Tick(8 * BAR), Tick(9 * BAR));
    let (mut app, _) = app_in(&dir, project);
    app.step();
    let settings = ExportSettings {
        sample_rate: 44_100,
        tail_seconds: 0.5,
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, settings);
    let wav = read(&dir.join("out/Night Drive.wav"));
    assert_eq!(wav.spec.sample_rate, 44_100);
    let bar9 = frames_of(&app, TickRange::new(Tick(8 * BAR), Tick(9 * BAR)), 44_100);
    assert_eq!(wav.frames, bar9 + 22_050);
    assert!((peak_db(&wav.samples) - NORMALIZE_TARGET_DB).abs() < 0.01);
    // Back at the device's rate, and playing.
    assert_eq!(app.engine().sample_rate(), 48_000);
    assert!(plays(&mut app));
    // Again at 96 kHz.
    let settings = ExportSettings {
        sample_rate: 96_000,
        tail_seconds: 0.0,
        file_name: "96.wav".into(),
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, settings);
    let wav = read(&dir.join("out/96.wav"));
    assert_eq!(wav.spec.sample_rate, 96_000);
    assert_eq!(
        wav.frames,
        frames_of(&app, TickRange::new(Tick(8 * BAR), Tick(9 * BAR)), 96_000)
    );
    assert!(plays(&mut app));
}

// ------------------------------------------------------------------------------------------------
// Project latency
// ------------------------------------------------------------------------------------------------

/// The first frame of `wav` with a sample above the noise floor.
fn onset(wav: &Wav) -> usize {
    wav.samples
        .chunks(2)
        .position(|f| f.iter().any(|s| s.abs() > 1e-6))
        .expect("the file has sound")
}

#[test]
fn the_export_is_aligned_with_the_song_despite_the_project_latency() {
    let dir = scratch("latency");
    let float = |name: &str| ExportSettings {
        bit_depth: BitDepth::Float32,
        normalize: false,
        tail_seconds: 0.25,
        file_name: name.into(),
        ..export_settings(&dir, ExportRange::Song)
    };
    // The same song with a presse on one track: once without lookahead (no latency), once with
    // its 3 ms lookahead (144 frames at 48 kHz) that the delay compensation puts on every path.
    let mut files = Vec::new();
    for (name, on) in [("plain.wav", false), ("lookahead.wav", true)] {
        let (project, device) = with_effect(one_bar_song(), presse());
        let (mut app, _) = app_in(&dir, project);
        crate::common::wait_for_layout(&mut app, device);
        if on {
            let id = crate::common::author_parameter(&app, device, "lookahead");
            app.dispatch(Action::Plugin(komponist_app::PluginAction::SetParam {
                device,
                id,
                value: 1.0,
            }));
            // The processor publishes its latency when it applies the parameter on the engine.
            step_until(&mut app, "the latency", |a| {
                a.engine().host().latency(device) == 144
            });
        }
        export(&mut app, float(name));
        files.push(read(&dir.join("out").join(name)));
    }
    let [plain, late] = &files[..] else { panic!() };
    // Same length (song + tail), and the sound starts on the same frame: the latency is gone
    // from the file's start instead of delaying the whole song.
    assert_eq!(plain.frames, late.frames);
    assert_eq!(onset(late), onset(plain));
    assert!(onset(plain) < 48, "the song starts at frame 0 (± a few)");
}

/// How [`latency_pair`] sets up the song.
#[derive(Clone, Copy)]
struct Pair {
    /// A send from the presse track to a bus: the program has a routing graph.
    routed: bool,
    /// Only the presse track plays, so its own arrival time decides the onset (with the other
    /// tracks sounding, the onset hides how the presse track lines up with them).
    only_presse_track: bool,
    /// Wait for the processor to publish its latency before the export starts (the app's usual
    /// state); `false` starts right after the parameter change.
    wait: bool,
    rate: u32,
}

/// The same song once with and once without the presse lookahead (3 ms), exported at `pair.rate`
/// while the null backend runs at its own rate. Returns (plain, with lookahead).
fn latency_pair(name: &str, pair: Pair) -> (Wav, Wav) {
    use komponist_model::edits::NewBus;
    let dir = scratch(name);
    let mut files = Vec::new();
    for (file, on) in [("plain.wav", false), ("lookahead.wav", true)] {
        let (mut project, device) = with_effect(one_bar_song(), presse());
        let presse_track = project.tracks[1].id;
        if pair.routed {
            project = edits::add_bus(&project, NewBus::effect(vec![presse_track]))
                .unwrap()
                .0;
        }
        if pair.only_presse_track {
            for track in &mut project.tracks {
                if track.id != presse_track {
                    Arc::make_mut(track).clips.clear();
                }
            }
        }
        let (mut app, _) = app_in(&dir, project);
        crate::common::wait_for_layout(&mut app, device);
        if on {
            let id = crate::common::author_parameter(&app, device, "lookahead");
            app.dispatch(Action::Plugin(komponist_app::PluginAction::SetParam {
                device,
                id,
                value: 1.0,
            }));
            if pair.wait {
                // The processor publishes its latency when it applies the parameter on the engine.
                step_until(&mut app, "the latency", |a| {
                    a.engine().host().latency(device) == 144
                });
            }
        }
        let settings = ExportSettings {
            bit_depth: BitDepth::Float32,
            normalize: false,
            tail_seconds: 0.25,
            sample_rate: pair.rate,
            file_name: file.into(),
            ..export_settings(&dir, ExportRange::Song)
        };
        export(&mut app, settings);
        files.push(read(&dir.join("out").join(file)));
    }
    let late = files.pop().unwrap();
    let plain = files.pop().unwrap();
    (plain, late)
}

/// The two files have the same length, and the sound starts on the same frame.
fn assert_aligned(what: &str, plain: &Wav, late: &Wav) {
    assert_eq!(plain.frames, late.frames, "{what}: same length");
    assert_eq!(onset(late), onset(plain), "{what}: same onset");
    let limit = plain.spec.sample_rate as usize / 1000;
    assert!(onset(plain) < limit, "{what}: the song starts at frame 0");
}

#[test]
fn a_routed_export_at_another_rate_is_aligned_like_at_the_device_rate() {
    // The routing latency and the delay compensation are compiled in frames of one rate: the
    // export worker compiles again at the export rate, so 96 kHz lines up like 48 kHz.
    for rate in [48_000, 96_000] {
        let pair = Pair {
            routed: true,
            only_presse_track: true,
            wait: true,
            rate,
        };
        let (plain, late) = latency_pair(&format!("routed-{rate}"), pair);
        assert_eq!(plain.spec.sample_rate, rate);
        assert_aligned(&format!("{rate} Hz"), &plain, &late);
    }
}

#[test]
fn an_export_right_after_a_latency_change_is_aligned() {
    // The processor has not applied the parameter when the export starts: the skip and the delay
    // compensation must still agree (they come from the same processors in the worker). Repeated
    // because the engine thread of the null backend races with the start.
    for round in 0..6 {
        for routed in [false, true] {
            let pair = Pair {
                routed,
                only_presse_track: round % 2 == 0,
                wait: false,
                rate: 48_000,
            };
            let (plain, late) = latency_pair(&format!("immediate-{round}-{routed}"), pair);
            assert_aligned(&format!("round {round}, routed {routed}"), &plain, &late);
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Cancel
// ------------------------------------------------------------------------------------------------

#[test]
fn cancel_leaves_no_file_and_the_engine_plays_again() {
    let dir = scratch("cancel");
    // The whole demo song: 33 s, long enough to cancel in the middle.
    let (mut app, _) = app_in(&dir, hexa_demo());
    app.step();
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    let settings = export_settings(&dir, ExportRange::Song);
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    step_until(&mut app, "progress", |a| {
        matches!(&a.ui().dialog, Some(Dialog::Export(d))
            if d.progress.is_some_and(|p| p.fraction > 0.02))
    });
    assert!(app.engine().is_detached());
    // A part file is being written meanwhile, never the target.
    assert!(!dir.join("out/Night Drive.wav").exists());
    // Esc in the dialog cancels.
    assert_eq!(app.ui().focus, FocusArea::Dialog);
    app.dispatch(Action::Ui(UiAction::CloseDialog));
    step_until(&mut app, "end of the export", |a| !a.is_exporting());
    assert_eq!(files(&dir.join("out")), Vec::<String>::new());
    assert_eq!(
        status(&app),
        (de::EXPORT_CANCELLED.to_owned(), StatusKind::Info)
    );
    assert!(app.ui().dialog.is_none());
    assert_ne!(app.ui().focus, FocusArea::Dialog);
    assert!(app.exported().is_empty());
    assert!(plays(&mut app));
}

// ------------------------------------------------------------------------------------------------
// Devices
// ------------------------------------------------------------------------------------------------

#[test]
fn the_export_waits_for_loading_devices() {
    let dir = scratch("loading");
    let mut project = one_bar_song();
    project.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 2));
    let surge = fake_host::plugin(&fake_host::screens_catalog(), "hall");
    let (project, hall) = with_effect(project, surge);
    let host = FakeHost::new().status(hall, DeviceStatus::Loading);
    let statuses = Arc::clone(&host.statuses);
    let (mut app, _) = app_with(&dir, project, Box::new(host));
    app.step();
    let settings = ExportSettings {
        tail_seconds: 0.0,
        ..export_settings(&dir, ExportRange::Loop)
    };
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    for _ in 0..20 {
        app.step();
    }
    // Waiting: the engine stays in the stream, nothing is written.
    assert!(app.is_exporting());
    assert!(!app.engine().is_detached());
    assert!(!dir.join("out").exists() || files(&dir.join("out")).is_empty());
    // The device runs: the export goes on by itself.
    statuses.lock().unwrap().insert(hall, DeviceStatus::Ok);
    step_until(&mut app, "end of the export", |a| !a.is_exporting());
    assert_eq!(files(&dir.join("out")), ["Night Drive.wav"]);
    assert_eq!(status(&app).1, StatusKind::Success);
}

#[test]
fn another_dialog_waits_while_the_export_dialog_runs() {
    use komponist_app::UiAction;
    use komponist_app::ui_state::SettingsSection;
    let dir = scratch("queue");
    let mut project = one_bar_song();
    project.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 2));
    let surge = fake_host::plugin(&fake_host::screens_catalog(), "hall");
    let (project, hall) = with_effect(project, surge);
    let host = FakeHost::new().status(hall, DeviceStatus::Loading);
    let (mut app, _) = app_with(&dir, project, Box::new(host));
    app.step();
    let settings = ExportSettings {
        tail_seconds: 0.0,
        ..export_settings(&dir, ExportRange::Loop)
    };
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    for _ in 0..5 {
        app.step();
    }
    assert!(app.is_exporting());
    assert!(matches!(&app.ui().dialog, Some(Dialog::Export(d))
        if d.stage == komponist_app::ui_state::ExportStage::Devices));
    // Strg , while the export waits: the export dialog stays on top.
    app.dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
    assert!(matches!(app.ui().dialog, Some(Dialog::Export(_))));
    // Cancelling closes it; the queued dialog opens in its place.
    app.dispatch(Action::Project(ProjectAction::CancelExport));
    step_until(&mut app, "end of the export", |a| !a.is_exporting());
    assert!(matches!(app.ui().dialog, Some(Dialog::Settings(_))));
}

fn harness(app: App) -> Harness<'static, App> {
    harness_at(app, 1.0)
}

fn harness_at(app: App, ppp: f32) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(ppp)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

/// Runs frames (with a little real time for the worker) until `done` holds.
fn run_until(h: &mut Harness<'_, App>, what: &str, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "no {what}");
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn missing_devices_are_listed_and_the_export_goes_on_without_them_or_not_at_all() {
    let dir = scratch("missing");
    // The demo on the packaged host: Surge XT, Dexed, Vital and the third-party effects are
    // missing (the Sampler and Hexa are the shipped ones).
    let mut project = crate::common::song_with_shipped_instruments(
        &komponist_session::device_host::PluginHost::headless_fixture(),
    );
    project.loop_range = TickRange::new(Tick(8 * BAR), Tick(8 * BAR + BAR / 2));
    let (app, _) = app_in(&dir, project);
    let mut h = harness(app);
    let settings = ExportSettings {
        tail_seconds: 0.0,
        ..export_settings(&dir, ExportRange::Loop)
    };
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::StartExport(
            settings.clone(),
        )));
    h.run_steps(3);
    h.get_by_label(&Lang::De.export_exporting("Night Drive.wav"));
    h.get_by_label(de::EXPORT_WAITING);
    h.get_by_role_and_label(Role::ListItem, "LSP Compressor · Drums · fehlt");
    h.get_by_role_and_label(Role::ListItem, "Vital · Arp · fehlt");
    assert!(!h.state().engine().is_detached());
    // "Abbrechen": nothing written.
    h.get_by_role_and_label(Role::Button, de::DIALOG_CANCEL)
        .click();
    h.run_steps(3);
    assert!(!h.state().is_exporting());
    assert!(h.state().ui().dialog.is_none());
    assert!(files(&dir.join("out")).is_empty());

    // Again, "Ohne sie exportieren".
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::StartExport(settings)));
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, de::EXPORT_WITHOUT)
        .click();
    run_until(&mut h, "end of the export", |a| !a.is_exporting());
    assert_eq!(files(&dir.join("out")), ["Night Drive.wav"]);
    let wav = read(&dir.join("out/Night Drive.wav"));
    // Bass, Lead and the muted Arp's neighbours: hexa sounds.
    assert!(peak_db(&wav.samples) > -40.0);
}

/// The packaged host; a device with plugin id "kaputt" gets a processor that
/// fails as soon as it renders offline (a bridge that dies during the export).
struct FailingHost {
    packaged: PluginHost,
    scripted: DeviceId,
    /// The latest global render mode (for the panic test).
    mode: Arc<Mutex<Option<RenderMode>>>,
}

const KAPUTT: &str = "kaputt";
/// Plugin ids of the devices that panic: in `activate` at the export's sample rate (the worker
/// outside the engine's render), or in the one-frame render that ends the export (inside it).
const PANIK_ACTIVATE: &str = "panik-activate";
const PANIK_RENDER: &str = "panik-render";
/// Panics in `activate` when the devices go back to the device rate after an export at
/// [`PANIC_RATE`] (the file is written then).
const PANIK_RESTORE: &str = "panik-restore";

/// The export rate at which [`PANIK_ACTIVATE`] panics (the null backend runs at 48 kHz).
const PANIC_RATE: f32 = 44_100.0;

/// Passes audio through and panics where the plugin id says. The export's blocks are far longer
/// than one frame, and they run inside `assert_no_alloc`, whose allocator aborts on the
/// allocation of a panic (and on the frees while unwinding): a panic there cannot be tested in a
/// debug build, so the render panic is in the one-frame render the worker does outside of it.
struct Panicky {
    in_render: bool,
    /// Panic in `activate` once the device rate comes back after [`PANIC_RATE`].
    on_restore: bool,
    exported: bool,
    panic: TestPanic,
}

impl Processor for Panicky {
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        if self.on_restore {
            assert!(!self.exported, "Panicky::activate back at {sample_rate}");
            self.exported |= sample_rate == PANIC_RATE;
        } else {
            assert!(
                self.in_render || sample_rate != PANIC_RATE,
                "Panicky::activate at {sample_rate}"
            );
        }
        self.panic.activate(sample_rate, max_block)
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        if self.in_render && ctx.offline && ctx.frames == 1 {
            return self.panic.process(ctx, events, io);
        }
        let [left, right] = &mut io.output;
        left.copy_from_slice(io.input[0]);
        right.copy_from_slice(io.input[1]);
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

struct Failing;

impl Processor for Failing {
    fn activate(&mut self, _sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        _: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let [left, right] = &mut io.output;
        left.copy_from_slice(io.input[0]);
        right.copy_from_slice(io.input[1]);
        if ctx.offline {
            ProcessStatus::Failed
        } else {
            ProcessStatus::Continue
        }
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

impl ProcessorHost for FailingHost {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        if device.plugin.id == KAPUTT {
            return Some(Box::new(Failing));
        }
        if [PANIK_ACTIVATE, PANIK_RENDER, PANIK_RESTORE].contains(&device.plugin.id.as_str()) {
            return Some(Box::new(Panicky {
                in_render: device.plugin.id == PANIK_RENDER,
                on_restore: device.plugin.id == PANIK_RESTORE,
                exported: false,
                panic: TestPanic,
            }));
        }
        self.packaged.create_processor(device, setup)
    }

    fn release(&mut self, device: DeviceId) {
        self.packaged.release(device);
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        if device == self.scripted {
            DeviceStatus::Ok
        } else {
            self.packaged.status(device)
        }
    }

    fn latency(&self, _device: DeviceId) -> u32 {
        0
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.packaged.poll()
    }
}

impl ParameterHost for FailingHost {}

impl DeviceStateHost for FailingHost {}

impl PluginCatalogHost for FailingHost {
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.packaged.role(plugin)
    }

    fn catalog(&self) -> &Catalog {
        self.packaged.catalog()
    }
}

impl PresetHost for FailingHost {}

impl AssetHost for FailingHost {}

impl OfflineRenderHost for FailingHost {
    fn set_render_mode(&mut self, mode: RenderMode) {
        *self.mode.lock().unwrap() = Some(mode);
        self.packaged.set_render_mode(mode);
    }
}

impl PluginWindowHost for FailingHost {}

impl DevicePresentationHost for FailingHost {}

#[test]
fn a_device_that_drops_out_of_the_render_is_named() {
    let dir = scratch("dropout");
    let mut project = one_bar_song();
    project.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 2));
    let kaputt = PluginRef {
        format: PluginFormat::Clap,
        id: KAPUTT.into(),
        name: "Kaputt-Hall".into(),
        vendor: "Test".into(),
        version: "1.0".into(),
        module: Box::default(),
    };
    let (project, scripted) = with_effect(project, kaputt);
    let host = FailingHost {
        packaged: komponist_session::device_host::PluginHost::headless_fixture(),
        scripted,
        mode: Arc::default(),
    };
    let (mut app, _) = app_with(&dir, project, Box::new(host));
    app.step();
    let settings = ExportSettings {
        tail_seconds: 0.0,
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, settings);
    // The file is complete; the message names the device.
    assert_eq!(files(&dir.join("out")), ["Night Drive.wav"]);
    assert_eq!(
        status(&app),
        (
            "Night Drive.wav exportiert · 00:01 · 24 bit · Kaputt-Hall ist ausgefallen".to_owned(),
            StatusKind::Error
        )
    );
}

/// #211: a panic on the export worker (a device while the export re-activates it, or inside the
/// engine's render, which poisons the engine) is not left to silence the session: the engine is
/// rebuilt and attached, the bridges render in realtime again, nothing piles up in the command
/// queue, and the status says what happened.
#[test]
fn a_panic_on_the_export_worker_leaves_a_playing_session() {
    for (lang, plugin) in [
        (Lang::De, PANIK_ACTIVATE),
        (Lang::En, PANIK_ACTIVATE),
        (Lang::De, PANIK_RENDER),
        (Lang::En, PANIK_RENDER),
        (Lang::De, PANIK_RESTORE),
        (Lang::En, PANIK_RESTORE),
    ] {
        let dir = scratch(&format!("panic-{lang:?}-{plugin}"));
        let mut project = one_bar_song();
        project.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 2));
        let panik = PluginRef {
            format: PluginFormat::Clap,
            id: plugin.into(),
            name: "Panik-Hall".into(),
            vendor: "Test".into(),
            version: "1.0".into(),
            module: Box::default(),
        };
        let (project, scripted) = with_effect(project, panik);
        let mode = Arc::new(Mutex::new(None));
        let host = FailingHost {
            packaged: komponist_session::device_host::PluginHost::headless_fixture(),
            scripted,
            mode: Arc::clone(&mode),
        };
        let (app, _) = app_with(&dir, project, Box::new(host));
        let mut app = app.with_lang(lang);
        // Single-threaded, so the render panics on the export worker itself.
        app.engine_mut().set_workers(Some(0)).unwrap();
        app.step();
        let settings = ExportSettings {
            tail_seconds: 0.0,
            sample_rate: if plugin == PANIK_ACTIVATE || plugin == PANIK_RESTORE {
                PANIC_RATE as u32
            } else {
                48_000
            },
            ..export_settings(&dir, ExportRange::Loop)
        };
        export(&mut app, settings);

        // A panic after the file was written (going back to the device rate, or in the closing
        // render) keeps the export.
        let written = plugin != PANIK_ACTIVATE;
        let crashed = if written {
            lang.export_finished_worker_crashed("Night Drive.wav")
        } else {
            lang.export_failed(lang.export_worker_crashed())
        };
        assert_eq!(
            status(&app),
            (crashed.clone(), StatusKind::Error),
            "{plugin}"
        );
        assert!(app.ui().dialog.is_none());
        // The render panic comes after the export wrote its file (the closing one-frame render).
        assert_eq!(
            dir.join("out/Night Drive.wav").exists(),
            written,
            "{plugin}"
        );
        // A file that was written stays in Projekt › Exporte.
        assert_eq!(
            app.exported(),
            if written {
                vec![dir.join("out/Night Drive.wav")]
            } else {
                Vec::new()
            }
        );
        assert_eq!(*mode.lock().unwrap(), Some(RenderMode::Realtime));
        assert!(!app.engine().is_detached());
        assert_eq!(app.engine().pending_commands(), 0);

        // The status stays (the rebuild notice was taken with it) and the session plays audio.
        app.step();
        assert_eq!(status(&app).0, crashed);
        app.engine_mut().hold_engine().unwrap();
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
        app.step();
        let (left, right) = app.engine_mut().run_held_capture(24_000, &[], 0).unwrap();
        assert!(
            left.iter().chain(&right).any(|s| s.abs() > 1e-4),
            "playback is silent after {plugin}"
        );
        assert_eq!(app.engine().pending_commands(), 0);
        app.engine_mut().release_engine();
    }
}

// ------------------------------------------------------------------------------------------------
// Settings
// ------------------------------------------------------------------------------------------------

#[test]
fn the_last_settings_are_saved_and_the_dialog_opens_with_them() {
    let dir = scratch("settings");
    let mut project = one_bar_song();
    project.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 4));
    let (mut app, _) = app_in(&dir, project.clone());
    app.step();
    let used = ExportSettings {
        tail_seconds: 0.0,
        sample_rate: 44_100,
        bit_depth: BitDepth::Int16,
        normalize: false,
        dither: true,
        file_name: "Skizze.wav".into(),
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, used.clone());
    let saved = Settings::load(&settings_path(&dir)).unwrap();
    assert_eq!(saved.export.as_ref(), Some(&used));

    // A new session with these settings: S08 opens with them, the file named after the project.
    let engine = EngineSync::null(Box::new(
        komponist_session::device_host::PluginHost::headless_fixture(),
    ));
    let mut app = App::new(Document::new(project), engine, saved, None);
    app.step();
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    let Some(Dialog::Export(d)) = &app.ui().dialog else {
        panic!("export dialog");
    };
    assert_eq!(
        d.settings,
        ExportSettings {
            file_name: "Night Drive.wav".into(),
            ..used
        }
    );
    assert_eq!(app.ui().focus, FocusArea::Dialog);

    // An empty project has nothing to export.
    let mut app = App::headless(Project::new("Leer"));
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    assert!(app.ui().dialog.is_none());
    assert_eq!(status(&app).0, de::EXPORT_EMPTY);
}

// ------------------------------------------------------------------------------------------------
// The dialog through kittest
// ------------------------------------------------------------------------------------------------

/// The shortcut modifier (Ctrl on Linux, ⌘ on macOS) and Shift.
const COMMAND_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

#[test]
fn the_dialog_exports_and_the_status_bar_link_opens_the_folder() {
    let dir = scratch("dialog");
    let target = dir.join("Mixdowns");
    std::fs::create_dir_all(&target).unwrap();
    let mut project = one_bar_song();
    project.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 2));
    let (app, opened) = app_in(&dir, project);
    let mut h = harness(app);

    // Strg Shift E: S08 with the defaults.
    h.key_press_modifiers(COMMAND_SHIFT, Key::E);
    h.run_steps(3);
    h.get_by_label(de::EXPORT_TITLE);
    h.get_by_role_and_label(Role::RadioButton, de::EXPORT_SONG);
    let selection = h.get_by_role_and_label(Role::RadioButton, de::EXPORT_SELECTION);
    assert!(selection.accesskit_node().is_disabled());
    h.get_by_role_and_label(Role::ComboBox, de::EXPORT_RATE);
    let value = |h: &Harness<'_, App>, name: &str| {
        h.get_by_role_and_label(Role::TextInput, name)
            .accesskit_node()
            .value()
            .unwrap_or_default()
    };
    assert_eq!(value(&h, de::EXPORT_FROM), "1.1.1");
    assert_eq!(value(&h, de::EXPORT_TO), "2.1.1");
    // Choose the loop, 16 bit, normalise off.
    h.get_by_role_and_label(Role::RadioButton, "Loop 1–1")
        .click();
    h.run_steps(2);
    assert_eq!(value(&h, de::EXPORT_TO), "1.3.1");
    h.get_by_role_and_label(Role::RadioButton, "16 bit").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::Switch, de::EXPORT_NORMALIZE)
        .click();
    h.run_steps(2);
    h.get_by_label(de::EXPORT_NORMALIZE_OFF);
    // The folder through "Ändern": the file browser starts in the dialog's folder.
    if let Some(Dialog::Export(d)) = &mut h.state_mut().ui_mut().dialog {
        d.settings.folder = dir.clone();
    }
    h.run_steps(1);
    h.get_by_role_and_label(Role::Button, de::EXPORT_CHANGE)
        .click();
    h.run_steps(3);
    h.get_by_value(de::EXPORT_FOLDER_TITLE);
    h.get_by_role_and_label(Role::ListItem, "Mixdowns").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "Ordner wählen")
        .click();
    h.run_steps(3);
    let Some(Dialog::Export(d)) = &h.state().ui().dialog else {
        panic!("export dialog");
    };
    assert_eq!(d.settings.folder, target);
    assert_eq!(d.settings.range, ExportRange::Loop);
    assert_eq!(d.settings.bit_depth, BitDepth::Int16);
    assert!(!d.settings.normalize);
    // Enter exports.
    h.key_press(Key::Enter);
    h.run_steps(1);
    run_until(&mut h, "end of the export", |a| !a.is_exporting());
    h.run_steps(2);
    assert_eq!(files(&target), ["Night Drive.wav"]);
    assert_eq!(
        read(&target.join("Night Drive.wav")).spec.bits_per_sample,
        16
    );

    // The status bar: S08's message and the link.
    // Half a bar and the default 2 s tail.
    h.get_by_label("Night Drive.wav exportiert · 00:03 · 16 bit");
    h.get_by_role_and_label(Role::Link, de::EXPORT_SHOW_IN_FOLDER)
        .click();
    h.run_steps(2);
    assert_eq!(*opened.lock().unwrap(), std::slice::from_ref(&target));

    // Projekt › Exporte lists it (segment "Dateien").
    h.get_by_role_and_label(Role::RadioButton, "Dateien")
        .click();
    h.run_steps(3);
    h.get_by_label("Night Drive.wav");
    assert!(h.query_by_label(de::BROWSER_NO_EXPORTS).is_none());
}

#[test]
fn esc_and_the_cancel_button_end_a_running_export() {
    let dir = scratch("esc");
    let (app, _) = app_in(&dir, hexa_demo());
    let mut h = harness(app);
    let settings = export_settings(&dir, ExportRange::Song);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::OpenExportDialog));
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::StartExport(
            settings.clone(),
        )));
    run_until(&mut h, "progress", |a| {
        matches!(&a.ui().dialog, Some(Dialog::Export(d))
            if d.progress.is_some_and(|p| p.fraction > 0.0))
    });
    h.run_steps(1);
    h.get_by_label(&Lang::De.export_exporting("Night Drive.wav"));
    h.get_by_role(Role::ProgressIndicator);
    h.key_press(Key::Escape);
    run_until(&mut h, "end of the export", |a| !a.is_exporting());
    assert!(files(&dir.join("out")).is_empty());
    // The button does the same.
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::StartExport(settings)));
    run_until(&mut h, "progress", |a| {
        matches!(&a.ui().dialog, Some(Dialog::Export(d))
            if d.progress.is_some_and(|p| p.fraction > 0.0))
    });
    h.run_steps(1);
    h.get_by_role_and_label(Role::Button, de::DIALOG_CANCEL)
        .click();
    run_until(&mut h, "end of the export", |a| !a.is_exporting());
    h.run_steps(2);
    assert!(files(&dir.join("out")).is_empty());
    assert!(h.state().ui().dialog.is_none());
    h.get_by_label(de::EXPORT_CANCELLED);
}

// ------------------------------------------------------------------------------------------------
// Snapshots next to S08
// ------------------------------------------------------------------------------------------------

/// The CPU readout depends on the machine; masked in the snapshots.
fn mask_cpu(h: &mut Harness<'_, App>) {
    if let Some(cpu) = h.query_by_role_and_label(Role::Status, de::TRANSPORT_CPU) {
        let rect = cpu.rect();
        h.mask(rect.expand(2.0));
    }
}

/// S08's settings: the defaults into "~/Musik/Exporte".
fn s08_settings() -> ExportSettings {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/home"), PathBuf::from);
    settings::default_export("Night Drive", home.join("Musik/Exporte"))
}

/// The settings dialog in English (#5).
#[test]
fn snapshot_next_to_s08_in_english() {
    let settings = Settings {
        export: Some(s08_settings()),
        ..Settings::default()
    };
    let engine = EngineSync::null(Box::new(
        komponist_session::device_host::PluginHost::headless_fixture(),
    ));
    let mut app = App::new(Document::new(demo_project()), engine, settings, None)
        .with_lang(komponist_i18n::Lang::En);
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    if let Some(cpu) = h.query_by_role_and_label(Role::Status, komponist_i18n::en::TRANSPORT_CPU) {
        let rect = cpu.rect();
        h.mask(rect.expand(2.0));
    }
    h.snapshot("export-s08-en-1x");
}

#[test]
fn snapshots_next_to_s08() {
    let mut results = SnapshotResults::new();
    // The settings dialog as S08 shows it: the demo, loop 9–13, the defaults.
    for ppp in [1.0, 2.0] {
        let settings = Settings {
            export: Some(s08_settings()),
            ..Settings::default()
        };
        let engine = EngineSync::null(Box::new(
            komponist_session::device_host::PluginHost::headless_fixture(),
        ));
        let mut app = App::new(Document::new(demo_project()), engine, settings, None);
        app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
        let mut h = harness_at(app, ppp);
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot(format!("export-s08-{ppp}x"));
        results.extend_harness(&mut h);
    }
    // "danach · Fortschritt": 62 % of 00:33.0 at 6.2 × real time, peak −2.4 dBFS.
    let model = view::Model {
        settings: s08_settings(),
        stage: view::Stage::Progress(view::ProgressView {
            fraction: 0.62,
            done: "00:20.5".into(),
            total: "00:33.0".into(),
            speed: Some(6.2),
            peak_db: -2.4,
            remaining: Some(2.0),
            cancelling: false,
        }),
        keys: true,
    };
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            move |ui, app: &mut App| {
                app.show(ui);
                let _ = view::show(ui.ctx(), &model);
            },
            App::headless(demo_project()),
        );
    h.run_steps(4);
    mask_cpu(&mut h);
    h.snapshot("export-progress-1x");
    results.extend_harness(&mut h);
    // The demo references external devices that the packaged fixture catalog cannot resolve.
    let dir = scratch("snapshot");
    let mut app = App::headless(demo_project());
    let settings = export_settings(&dir, ExportRange::Song);
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    assert!(matches!(&app.ui().dialog, Some(Dialog::Export(dialog))
        if dialog.stage == komponist_app::ui_state::ExportStage::Devices));
    let mut h = harness(app);
    h.run_steps(3);
    mask_cpu(&mut h);
    h.snapshot("export-devices-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
