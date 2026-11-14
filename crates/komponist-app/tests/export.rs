//! WP-53b through the headless app on the null backend and `egui_kittest`: the S08 dialog, the
//! export of song, loop and selection (length = range + tail, normalised peak), another sample
//! rate, cancel, the engine afterwards, the wait for devices, a device that drops out, the
//! persisted settings, the status bar link and Projekt › Exporte, and snapshots next to S08.
//!
//! Exports and settings go to scratch folders under `CARGO_TARGET_TMPDIR`, never to the real
//! home; "Im Ordner zeigen" records the folder instead of running `xdg-open`.

mod fake_host;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{BitDepth, ExportRange, ExportSettings};
use komponist_app::device_host::{BuiltinHost, DeviceHost, is_pisynth};
use komponist_app::dialogs::export::{self as view, names};
use komponist_app::engine_sync::EngineSync;
use komponist_app::settings::{self, Settings};
use komponist_app::ui_state::{Dialog, FocusArea, StatusKind};
use komponist_app::views::browser::model::names as browser;
use komponist_app::{Action, App, ProjectAction, TransportAction, UiAction};
use komponist_engine::export::{ExportJob, NORMALIZE_TARGET_DB};
use komponist_engine::{
    Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo, TransportState,
};
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::{
    ChannelRef, Device, DeviceId, DeviceRole, Document, PluginFormat, PluginRef, Project, Tick,
    TickRange,
};
use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};

use fake_host::FakeHost;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// How long an export may take in a debug build on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(120);

/// One bar at 4/4 in ticks.
const BAR: i64 = 3840;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-export-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// The demo "Nachtfahrt" with pisynth for every instrument and no effects, so that everything
/// sounds on the builtin host and no device is missing.
fn pisynth_demo() -> Project {
    let mut p = demo_project();
    for track in &mut p.tracks {
        let track = Arc::make_mut(track);
        track
            .channel
            .devices
            .retain(|d| d.role == DeviceRole::Instrument);
        for device in &mut track.channel.devices {
            if !is_pisynth(device) {
                let device = Arc::make_mut(device);
                device.plugin = komponist_app::app::pisynth_ref();
                device.state = None;
            }
        }
    }
    p
}

/// [`pisynth_demo`] cut to its first bar (debug builds render slowly): the clips that start at
/// tick 0, each one bar long. The song ends after bar 1.
fn one_bar_song() -> Project {
    let mut p = pisynth_demo();
    for track in &mut p.tracks {
        let track = Arc::make_mut(track);
        track.clips.retain(|c| c.start == Tick::ZERO);
        for clip in &mut track.clips {
            Arc::make_mut(clip).length = Tick(BAR);
        }
    }
    p
}

/// An effect device for `plugin` at the end of the first track's chain.
fn with_effect(project: Project, plugin: PluginRef) -> (Project, DeviceId) {
    let track = project.tracks[1].id;
    let len = project.tracks[1].channel.devices.len();
    let spec = NewDevice {
        role: DeviceRole::Effect,
        plugin,
        state: None,
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

fn app_in(dir: &Path, project: Project) -> (App, Opened) {
    app_with(dir, project, Box::new(BuiltinHost::new()))
}

/// Export settings into `dir/out`: the defaults, `range`, "Nachtfahrt.wav".
fn export_settings(dir: &Path, range: ExportRange) -> ExportSettings {
    ExportSettings {
        range,
        file_name: "Nachtfahrt.wav".into(),
        ..settings::default_export("Nachtfahrt", dir.join("out"))
    }
}

/// Steps `app` until `done` holds; panics after [`PATIENCE`].
fn step_until(app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
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
fn export(app: &mut App, settings: ExportSettings) {
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    step_until(app, "end of the export", |a| !a.is_exporting());
}

/// A WAV file as f32 samples.
struct Wav {
    spec: hound::WavSpec,
    frames: u64,
    samples: Vec<f32>,
}

fn read(path: &Path) -> Wav {
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

fn peak_db(samples: &[f32]) -> f32 {
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
fn the_song_exports_as_range_plus_tail_normalised_to_minus_one_dbfs() {
    let dir = scratch("song");
    let (mut app, opened) = app_in(&dir, one_bar_song());
    app.step();
    // Playing before: the export stops the transport.
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(&mut app, "playback", |a| a.engine().playhead() > Tick::ZERO);
    export(&mut app, export_settings(&dir, ExportRange::Song));

    let path = dir.join("out/Nachtfahrt.wav");
    assert_eq!(files(&dir.join("out")), ["Nachtfahrt.wav"], "no part file");
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
            "Nachtfahrt.wav exportiert · 00:04 · 24 bit".to_owned(),
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

#[test]
fn loop_and_selection_set_the_range() {
    let dir = scratch("ranges");
    let mut project = pisynth_demo();
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
    let mut project = pisynth_demo();
    project.loop_range = TickRange::new(Tick(8 * BAR), Tick(9 * BAR));
    let (mut app, _) = app_in(&dir, project);
    app.step();
    let settings = ExportSettings {
        sample_rate: 44_100,
        tail_seconds: 0.5,
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, settings);
    let wav = read(&dir.join("out/Nachtfahrt.wav"));
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
// Cancel
// ------------------------------------------------------------------------------------------------

#[test]
fn cancel_leaves_no_file_and_the_engine_plays_again() {
    let dir = scratch("cancel");
    // The whole demo song: 33 s, long enough to cancel in the middle.
    let (mut app, _) = app_in(&dir, pisynth_demo());
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
    assert!(!dir.join("out/Nachtfahrt.wav").exists());
    // Esc in the dialog cancels.
    assert_eq!(app.ui().focus, FocusArea::Dialog);
    app.dispatch(Action::Ui(UiAction::CloseDialog));
    step_until(&mut app, "end of the export", |a| !a.is_exporting());
    assert_eq!(files(&dir.join("out")), Vec::<String>::new());
    assert_eq!(
        status(&app),
        (names::CANCELLED.to_owned(), StatusKind::Info)
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
    assert_eq!(files(&dir.join("out")), ["Nachtfahrt.wav"]);
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
    // The plain demo on the builtin host: klopfer, Surge XT, Dexed, Vital and the effects are
    // missing.
    let mut project = demo_project();
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
    h.get_by_label(&names::exporting("Nachtfahrt.wav"));
    h.get_by_label(names::WAITING);
    h.get_by_role_and_label(Role::ListItem, "klopfer · Drums · fehlt");
    h.get_by_role_and_label(Role::ListItem, "Vital · Arp · fehlt");
    assert!(!h.state().engine().is_detached());
    // "Abbrechen": nothing written.
    h.get_by_role_and_label(Role::Button, names::CANCEL).click();
    h.run_steps(3);
    assert!(!h.state().is_exporting());
    assert!(h.state().ui().dialog.is_none());
    assert!(files(&dir.join("out")).is_empty());

    // Again, "Ohne sie exportieren".
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::StartExport(settings)));
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, names::WITHOUT)
        .click();
    run_until(&mut h, "end of the export", |a| !a.is_exporting());
    assert_eq!(files(&dir.join("out")), ["Nachtfahrt.wav"]);
    let wav = read(&dir.join("out/Nachtfahrt.wav"));
    // Bass, Lead and the muted Arp's neighbours: pisynth sounds.
    assert!(peak_db(&wav.samples) > -40.0);
}

/// pisynth through [`BuiltinHost`]; a device with plugin id "kaputt" gets a processor that
/// fails as soon as it renders offline (a bridge that dies during the export).
struct FailingHost {
    builtin: BuiltinHost,
}

const KAPUTT: &str = "kaputt";

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

impl DeviceHost for FailingHost {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        if device.plugin.id == KAPUTT {
            return Some(Box::new(Failing));
        }
        self.builtin.create_processor(device, setup)
    }

    fn release(&mut self, device: DeviceId) {
        self.builtin.release(device);
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.builtin.status(device)
    }

    fn latency(&self, _device: DeviceId) -> u32 {
        0
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.builtin.poll()
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.builtin.role(plugin)
    }

    fn catalog(&self) -> &Catalog {
        self.builtin.catalog()
    }
}

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
    let (project, _) = with_effect(project, kaputt);
    let host = FailingHost {
        builtin: BuiltinHost::new(),
    };
    let (mut app, _) = app_with(&dir, project, Box::new(host));
    app.step();
    let settings = ExportSettings {
        tail_seconds: 0.0,
        ..export_settings(&dir, ExportRange::Loop)
    };
    export(&mut app, settings);
    // The file is complete; the message names the device.
    assert_eq!(files(&dir.join("out")), ["Nachtfahrt.wav"]);
    assert_eq!(
        status(&app),
        (
            "Nachtfahrt.wav exportiert · 00:01 · 24 bit · Kaputt-Hall ist ausgefallen".to_owned(),
            StatusKind::Error
        )
    );
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
    let engine = EngineSync::null(Box::new(BuiltinHost::new()));
    let mut app = App::new(Document::new(project), engine, saved, None);
    app.step();
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    let Some(Dialog::Export(d)) = &app.ui().dialog else {
        panic!("export dialog");
    };
    assert_eq!(
        d.settings,
        ExportSettings {
            file_name: "Nachtfahrt.wav".into(),
            ..used
        }
    );
    assert_eq!(app.ui().focus, FocusArea::Dialog);

    // An empty project has nothing to export.
    let mut app = App::headless(Project::new("Leer"));
    app.dispatch(Action::Project(ProjectAction::OpenExportDialog));
    assert!(app.ui().dialog.is_none());
    assert_eq!(status(&app).0, names::EMPTY);
}

// ------------------------------------------------------------------------------------------------
// The dialog through kittest
// ------------------------------------------------------------------------------------------------

const CTRL_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: true,
    mac_cmd: false,
    command: true,
};

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
    h.key_press_modifiers(CTRL_SHIFT, Key::E);
    h.run_steps(3);
    h.get_by_label(names::TITLE);
    h.get_by_role_and_label(Role::RadioButton, names::SONG);
    let selection = h.get_by_role_and_label(Role::RadioButton, names::SELECTION);
    assert!(selection.accesskit_node().is_disabled());
    h.get_by_role_and_label(Role::ComboBox, names::RATE);
    let value = |h: &Harness<'_, App>, name: &str| {
        h.get_by_role_and_label(Role::TextInput, name)
            .accesskit_node()
            .value()
            .unwrap_or_default()
    };
    assert_eq!(value(&h, view::names::FROM), "1.1.1");
    assert_eq!(value(&h, view::names::TO), "2.1.1");
    // Choose the loop, 16 bit, normalise off.
    h.get_by_role_and_label(Role::RadioButton, "Loop 1–1")
        .click();
    h.run_steps(2);
    assert_eq!(value(&h, view::names::TO), "1.3.1");
    h.get_by_role_and_label(Role::RadioButton, "16 bit").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::Switch, names::NORMALIZE)
        .click();
    h.run_steps(2);
    h.get_by_label(names::NORMALIZE_OFF);
    // The folder through "Ändern": the file browser starts in the dialog's folder.
    if let Some(Dialog::Export(d)) = &mut h.state_mut().ui_mut().dialog {
        d.settings.folder = dir.clone();
    }
    h.run_steps(1);
    h.get_by_role_and_label(Role::Button, names::CHANGE).click();
    h.run_steps(3);
    h.get_by_value(names::FOLDER_TITLE);
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
    assert_eq!(files(&target), ["Nachtfahrt.wav"]);
    assert_eq!(
        read(&target.join("Nachtfahrt.wav")).spec.bits_per_sample,
        16
    );

    // The status bar: S08's message and the link.
    // Half a bar and the default 2 s tail.
    h.get_by_label("Nachtfahrt.wav exportiert · 00:03 · 16 bit");
    h.get_by_role_and_label(Role::Link, names::SHOW_IN_FOLDER)
        .click();
    h.run_steps(2);
    assert_eq!(*opened.lock().unwrap(), std::slice::from_ref(&target));

    // Projekt › Exporte lists it (segment "Dateien").
    h.get_by_role_and_label(Role::RadioButton, "Dateien")
        .click();
    h.run_steps(3);
    h.get_by_label("Nachtfahrt.wav");
    assert!(h.query_by_label(browser::NO_EXPORTS).is_none());
}

#[test]
fn esc_and_the_cancel_button_end_a_running_export() {
    let dir = scratch("esc");
    let (app, _) = app_in(&dir, pisynth_demo());
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
    h.get_by_label(&names::exporting("Nachtfahrt.wav"));
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
    h.get_by_role_and_label(Role::Button, names::CANCEL).click();
    run_until(&mut h, "end of the export", |a| !a.is_exporting());
    h.run_steps(2);
    assert!(files(&dir.join("out")).is_empty());
    assert!(h.state().ui().dialog.is_none());
    h.get_by_label(names::CANCELLED);
}

// ------------------------------------------------------------------------------------------------
// Snapshots next to S08
// ------------------------------------------------------------------------------------------------

/// The CPU readout depends on the machine; masked in the snapshots.
fn mask_cpu(h: &mut Harness<'_, App>) {
    use komponist_app::panels::transport::names as tp;
    if let Some(cpu) = h.query_by_role_and_label(Role::Status, tp::CPU) {
        let rect = cpu.rect();
        h.mask(rect.expand(2.0));
    }
}

/// S08's settings: the defaults into "~/Musik/Exporte".
fn s08_settings() -> ExportSettings {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/home"), PathBuf::from);
    settings::default_export("Nachtfahrt", home.join("Musik/Exporte"))
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
        let engine = EngineSync::null(Box::new(BuiltinHost::new()));
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
    // The wait for devices: the demo on the builtin host misses its plugins (it never starts,
    // and the folder is a scratch one anyway).
    let dir = scratch("snapshot");
    let mut app = App::headless(demo_project());
    let settings = export_settings(&dir, ExportRange::Song);
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    let mut h = harness(app);
    h.run_steps(3);
    mask_cpu(&mut h);
    h.snapshot("export-devices-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
