//! WP-52 through `egui_kittest`: the start screen (S09), new project (S10), the file browser,
//! save → open round trip, the unsaved-changes prompt, S06 with a plugin that does not exist,
//! S07, the engine reset on open, and snapshots next to S06, S07, S09, S10.
//!
//! Settings and the recent list go to scratch folders under `CARGO_TARGET_TMPDIR`, never to the
//! real `~/.config`; the engine runs on the null backend.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::SettingsAction;
use komponist_app::device_host::{BuiltinHost, DeviceHost};
use komponist_app::dialogs::{missing_plugins, new_project, settings, start};
use komponist_app::engine_sync::EngineSync;
use komponist_app::project_io::{self, RecentList};
use komponist_app::settings::{Language, Settings};
use komponist_app::ui_state::{Dialog, FocusArea, Screen, SettingsSection, StartView};
use komponist_app::{Action, App, EditAction, ProjectAction, UiAction};
use komponist_audio::{
    DeviceId as AudioDeviceId, DeviceInfo, HostKind, JackDirection, JackPort, StreamConfig,
};
use komponist_engine::Processor;
use komponist_i18n::{Lang, de, en};
use komponist_model::fixtures::demo_project;
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginFormat, PluginRef, Project};
use komponist_plugins::{Catalog, DeviceStatus, MissingPlugin, PluginManagerEvent, ProcessorSetup};

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-dialogs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn harness_at(app: App, ppp: f32) -> Harness<'static, App> {
    komponist_ui::snapshot::builder::<App>(ppp)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = harness_at(app, 1.0);
    h.run_steps(2);
    h
}

/// The app on the null backend with settings (and so the recent list) in `dir`.
fn app_in(dir: &Path, project: Project) -> App {
    let mut settings = Settings::default();
    settings.audio.host = HostKind::Null;
    let engine = EngineSync::null(Box::new(BuiltinHost::new()));
    App::new(
        Document::new(project),
        engine,
        settings,
        Some(dir.join("config/settings.json")),
    )
}

fn press(h: &mut Harness<'_, App>, mods: Modifiers, key: Key) {
    h.key_press_modifiers(mods, key);
    h.run_steps(2);
}

/// Replaces the text of the focused text field with `text` and presses Enter.
fn type_into(h: &mut Harness<'_, App>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    h.run_steps(1);
    press(h, Modifiers::COMMAND, Key::A);
    h.get_by_label(label).type_text(text);
    h.run_steps(1);
}

fn save_and_open(path: &Path, project: &Project) {
    project_io::save_project(path, project).unwrap();
}

// ------------------------------------------------------------------------------------------------
// New, save, open
// ------------------------------------------------------------------------------------------------

#[test]
fn new_save_open_round_trip_through_the_file_browser() {
    let dir = scratch("round-trip");
    let mut h = harness(app_in(&dir, demo_project()));
    // Ctrl N: S10, empty and clean.
    press(&mut h, Modifiers::COMMAND, Key::N);
    assert_eq!(h.state().project().name, "Unbenannt");
    assert!(h.state().project().tracks.is_empty());
    assert!(!h.state().doc().is_dirty());
    // Ctrl T, then Ctrl S: no file yet, so the save dialog asks for one.
    press(&mut h, Modifiers::COMMAND, Key::T);
    assert_eq!(h.state().project().tracks.len(), 1);
    press(&mut h, Modifiers::COMMAND, Key::S);
    h.get_by_value("Projekt speichern");
    assert_eq!(h.state().ui().focus, FocusArea::Dialog);
    let file = dir.join("Lieder/Hafenlicht");
    std::fs::create_dir_all(dir.join("Lieder")).unwrap();
    type_into(&mut h, "Dateiname", file.to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    let saved = dir.join("Lieder/Hafenlicht.komponist");
    assert!(saved.exists(), "saved with the extension");
    // Saved under a new name: the project is called after the file, and clean.
    assert_eq!(h.state().project().name, "Hafenlicht");
    assert!(!h.state().doc().is_dirty());
    assert_eq!(h.state().project_path(), Some(saved.as_path()));
    assert_eq!(h.state().recent(), std::slice::from_ref(&saved));
    let on_disk = RecentList::load(&dir.join("config/recent.json")).unwrap();
    assert_eq!(on_disk.projects, std::slice::from_ref(&saved));

    // Ctrl N again, then Ctrl O: the browser opens in the last project's folder.
    press(&mut h, Modifiers::COMMAND, Key::N);
    press(&mut h, Modifiers::COMMAND, Key::O);
    h.get_by_value("Projekt öffnen");
    h.get_by_label("Hafenlicht.komponist").click();
    h.run_steps(2);
    h.get_by_label("Öffnen").click();
    h.run_steps(3);
    let p = h.state().project();
    assert_eq!(p.name, "Hafenlicht");
    assert_eq!(p.tracks.len(), 1);
    assert_eq!(**p, project_io::load_project(&saved).unwrap());
    assert_eq!(h.state().ui().focus, FocusArea::Arrangement);
    assert!(h.state().ui().dialog.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_as_asks_before_replacing_a_file() {
    let dir = scratch("save-as");
    let existing = dir.join("Alt.komponist");
    std::fs::write(&existing, "{}").unwrap();
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Project(ProjectAction::SaveAs));
    let mut h = harness(app);
    type_into(&mut h, "Dateiname", existing.to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    // Not replaced yet: the dialog asks.
    h.get_by_label_contains("gibt es schon");
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "{}");
    h.get_by_label("Ersetzen").click();
    h.run_steps(3);
    assert_eq!(
        project_io::load_project(&existing).unwrap().tracks.len(),
        demo_project().tracks.len()
    );
    assert_eq!(h.state().project().name, "Alt");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Undo back to the saved state clears the unsaved mark and the prompt; redo and any other edit
/// bring them back (#20).
#[test]
fn undo_back_to_the_saved_state_is_clean_again() {
    let dir = scratch("undo-clean");
    let mut h = harness(app_in(&dir, demo_project()));
    // The unsaved mark is the project node's accessible name.
    let unsaved = |h: &Harness<'_, App>| {
        let name = h.state().project().name.clone();
        h.query_by_label(&Lang::De.transport_project_unsaved(&name))
            .is_some()
    };
    let action = |h: &mut Harness<'_, App>, a: EditAction| {
        h.state_mut().dispatch(Action::Edit(a));
        h.run_steps(2);
    };
    // Save: one edit, then Ctrl S into a new file.
    action(&mut h, EditAction::NewMidiTrack);
    assert!(h.state().doc().is_dirty() && unsaved(&h));
    press(&mut h, Modifiers::COMMAND, Key::S);
    std::fs::create_dir_all(&dir).unwrap();
    type_into(&mut h, "Dateiname", dir.join("Clean").to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(!h.state().doc().is_dirty() && !unsaved(&h));

    // Edit, then undo: clean again; redo: unsaved again.
    action(&mut h, EditAction::NewMidiTrack);
    assert!(h.state().doc().is_dirty() && unsaved(&h));
    action(&mut h, EditAction::Undo);
    assert!(!h.state().doc().is_dirty() && !unsaved(&h));
    action(&mut h, EditAction::Redo);
    assert!(h.state().doc().is_dirty() && unsaved(&h));
    action(&mut h, EditAction::Undo);

    // Clean: Ctrl N does not ask.
    press(&mut h, Modifiers::COMMAND, Key::N);
    assert!(h.query_by_value("Änderungen speichern?").is_none());
    assert_eq!(h.state().project().name, "Unbenannt");

    // Undoing past the saved state and editing makes it unreachable: dirty, and Ctrl N asks.
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let mut h = harness(app);
    press(&mut h, Modifiers::COMMAND, Key::S);
    type_into(&mut h, "Dateiname", dir.join("Clean2").to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(!h.state().doc().is_dirty());
    action(&mut h, EditAction::Undo);
    action(&mut h, EditAction::NewMidiTrack);
    action(&mut h, EditAction::Undo);
    action(&mut h, EditAction::Redo);
    assert!(h.state().doc().is_dirty());
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_value("Änderungen speichern?");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unsaved_changes_ask_before_a_new_project() {
    let dir = scratch("unsaved");
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    assert!(app.doc().is_dirty());
    let mut h = harness(app);
    let tracks = h.state().project().tracks.len();
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_value("Änderungen speichern?");
    // Esc keeps the project.
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert_eq!(h.state().project().tracks.len(), tracks);
    assert!(h.query_by_value("Änderungen speichern?").is_none());
    // "Nicht speichern" discards it.
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_label("Nicht speichern").click();
    h.run_steps(2);
    assert_eq!(h.state().project().name, "Unbenannt");
    // "Speichern" without a file asks for one and then continues: Ctrl N on a dirty S10 project.
    h.state_mut()
        .dispatch(Action::Edit(EditAction::NewMidiTrack));
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_label("Speichern").click();
    h.run_steps(2);
    // The file browser opens in the frame after the click and shows one frame later.
    h.run_steps(1);
    h.get_by_value("Projekt speichern");
    let file = dir.join("Skizze");
    type_into(&mut h, "Dateiname", file.to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(dir.join("Skizze.komponist").exists());
    assert_eq!(
        h.state().project().name,
        "Unbenannt",
        "continued with Ctrl N"
    );
    assert!(h.state().project().tracks.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_the_window_asks_for_unsaved_changes() {
    let dir = scratch("quit");
    let mut app = app_in(&dir, demo_project());
    // A clean project closes at once.
    assert!(app.close_requested());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    assert!(!app.close_requested(), "a dirty project asks first");
    let mut h = harness(app);
    h.run_steps(1);
    h.get_by_value("Änderungen speichern?");
    // Cancel keeps the window.
    h.get_by_label("Abbrechen").click();
    h.run_steps(2);
    assert!(h.query_by_value("Änderungen speichern?").is_none());
    assert!(!h.state_mut().close_requested());
    h.run_steps(2);
    // "Nicht speichern": the window may close.
    h.get_by_label("Nicht speichern").click();
    h.run_steps(2);
    assert!(h.state_mut().close_requested());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_quit_action_asks_for_unsaved_changes_like_the_close_button() {
    let dir = scratch("quit-action");
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.dispatch(Action::Project(ProjectAction::Quit));
    let mut h = harness(app);
    h.run_steps(2);
    h.get_by_value("Änderungen speichern?");
    // Cancel keeps the app running: the window may not close.
    h.get_by_label("Abbrechen").click();
    h.run_steps(2);
    assert!(h.query_by_value("Änderungen speichern?").is_none());
    assert!(!h.state_mut().close_requested());
    // Asked again and answered "Nicht speichern": the quit goes through.
    h.state_mut().dispatch(Action::Project(ProjectAction::Quit));
    h.run_steps(2);
    h.get_by_label("Nicht speichern").click();
    h.run_steps(2);
    assert!(h.state_mut().close_requested());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_cannot_be_read_keeps_the_project() {
    let dir = scratch("broken");
    let broken = dir.join("Kaputt.komponist");
    std::fs::write(&broken, "kein json").unwrap();
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Project(ProjectAction::OpenPath(broken)));
    assert_eq!(app.project().name, "Night Drive");
    let status = app.ui().status.as_ref().unwrap();
    assert!(
        status
            .text
            .starts_with("„Kaputt.komponist“ lässt sich nicht öffnen")
    );
    assert!(app.recent().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// Engine reset on open
// ------------------------------------------------------------------------------------------------

/// hexa through [`BuiltinHost`], recording which devices got a processor and which left.
struct Recording {
    inner: BuiltinHost,
    log: Arc<Mutex<Vec<(&'static str, DeviceId)>>>,
}

impl DeviceHost for Recording {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        self.log.lock().unwrap().push(("create", device.id));
        self.inner.create_processor(device, setup)
    }
    fn release(&mut self, device: DeviceId) {
        self.log.lock().unwrap().push(("release", device));
        self.inner.release(device);
    }
    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.inner.status(device)
    }
    fn latency(&self, device: DeviceId) -> u32 {
        self.inner.latency(device)
    }
    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.inner.poll()
    }
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.inner.role(plugin)
    }
    fn catalog(&self) -> &Catalog {
        self.inner.catalog()
    }
}

#[test]
fn opening_a_project_resets_the_engine() {
    let dir = scratch("reset");
    let log = Arc::new(Mutex::new(Vec::new()));
    let host = Recording {
        inner: BuiltinHost::new(),
        log: Arc::clone(&log),
    };
    let mut app =
        app_in(&dir, new_project::empty_project(Lang::De)).with_device_host(Box::new(host));
    // The first project, opened from a file.
    let one_track = |name: &str| {
        komponist_model::edits::add_track(
            &new_project::empty_project(Lang::De),
            komponist_model::edits::NewTrack {
                name: Some(name.into()),
                hue: None,
                instrument: Some(komponist_app::app::hexa_ref()),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap()
        .0
    };
    let first_file = dir.join("Erstes.komponist");
    save_and_open(&first_file, &one_track("Erste"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(first_file)));
    app.step();
    let first: Vec<DeviceId> = app.project().tracks[0]
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect();
    // A second project whose ids are the same numbers (ids restart at 1 in every project).
    let other = one_track("Andere");
    let second: Vec<DeviceId> = other.tracks[0]
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(first, second, "the same device ids in both projects");
    let file = dir.join("Zweites.komponist");
    save_and_open(&file, &other);
    log.lock().unwrap().clear();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    app.step();
    let log = log.lock().unwrap().clone();
    for id in &first {
        assert!(log.contains(&("release", *id)), "{log:?}");
    }
    for id in &second {
        let created = log.iter().position(|e| *e == ("create", *id));
        let released = log.iter().position(|e| *e == ("release", *id));
        assert!(
            created > released,
            "a fresh processor after the release: {log:?}"
        );
    }
    let program = app.engine().program().unwrap();
    assert_eq!(program.tracks.len(), 1);
    assert_eq!(app.project().tracks[0].name, "Andere");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_another_project_drops_a_running_recording() {
    use komponist_engine::{MidiInEvent, MidiPort, TransportState};
    use komponist_model::Tick;
    let dir = scratch("record");
    let mut other = new_project::empty_project(Lang::De);
    other.name = "Anderes".into();
    let file = dir.join("Anderes.komponist");
    save_and_open(&file, &other);
    // An armed track, count-in off, the engine held so the test runs it (WP-51 `hold_engine`).
    let mut app = app_in(&dir, new_project::empty_project(Lang::De));
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.dispatch(Action::Transport(
        komponist_app::TransportAction::ToggleCountIn,
    ));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    let frames = 48_000 / 10;
    let run = |h: &mut Harness<'_, App>, midi: &[MidiInEvent]| {
        assert!(h.state_mut().engine_mut().run_held(frames, midi, 0));
        h.step();
    };
    run(&mut h, &[]);
    press(&mut h, Modifiers::NONE, Key::R);
    let note = MidiInEvent {
        frame: 100,
        port: MidiPort(1),
        data: [0x90, 60, 100],
    };
    run(&mut h, &[note]);
    run(&mut h, &[]);
    assert!(h.state().recording().is_some());
    assert_eq!(h.state().transport(), TransportState::Recording);
    // Open another project ("Nicht speichern"): the take goes, the engine stops recording.
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::OpenPath(file)));
    h.run_steps(2);
    h.get_by_label("Nicht speichern").click();
    h.run_steps(2);
    assert_eq!(h.state().project().name, "Anderes");
    assert!(h.state().recording().is_none());
    // The old capture's note-offs and the stop arrive later; no take starts.
    run(&mut h, &[]);
    run(
        &mut h,
        &[MidiInEvent {
            frame: 50,
            port: MidiPort(1),
            data: [0x80, 60, 0],
        }],
    );
    assert!(h.state().recording().is_none());
    assert_eq!(h.state().transport(), TransportState::Stopped);
    assert_eq!(h.state().engine().playhead(), Tick::ZERO);
    assert!(h.state().project().tracks.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// #85: a running take is unsaved material, also in a saved project
// ------------------------------------------------------------------------------------------------

/// Runs the held engine for a tenth of a second with `midi`, then one frame.
fn run_held(h: &mut Harness<'_, App>, midi: &[komponist_engine::MidiInEvent]) {
    assert!(h.state_mut().engine_mut().run_held(48_000 / 10, midi, 0));
    h.step();
}

/// Key `key` down (`true`) or up on the virtual input.
fn key_event(key: u8, down: bool) -> komponist_engine::MidiInEvent {
    komponist_engine::MidiInEvent {
        frame: 100,
        port: komponist_engine::MidiPort(1),
        data: if down {
            [0x90, key, 100]
        } else {
            [0x80, key, 0]
        },
    }
}

/// Recording into "Aufnahme" (`dir/sub/Aufnahme.komponist`): the project was just opened from its
/// file (clean), its one track is armed, the count-in off, the engine held; one note (C4) was
/// played and released.
fn recording_into_a_saved_project(dir: &Path) -> (Harness<'static, App>, PathBuf) {
    use komponist_model::edits::{self, ExclusiveFlag, NewTrack};
    let (project, track) = edits::add_track(
        &new_project::empty_project(Lang::De),
        NewTrack {
            name: Some("Keys".into()),
            hue: None,
            instrument: Some(komponist_app::app::hexa_ref()),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let mut project = edits::set_exclusive(&project, track, ExclusiveFlag::Arm).unwrap();
    project.count_in_bars = 0;
    project.name = "Aufnahme".into();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let file = dir.join("sub/Aufnahme.komponist");
    save_and_open(&file, &project);
    let mut app = app_in(dir, new_project::empty_project(Lang::De));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    run_held(&mut h, &[]);
    press(&mut h, Modifiers::NONE, Key::R);
    run_held(&mut h, &[key_event(60, true)]);
    run_held(&mut h, &[key_event(60, false)]);
    assert_eq!(
        h.state().transport(),
        komponist_engine::TransportState::Recording
    );
    assert!(h.state().recording().is_some());
    assert!(
        !h.state().doc().is_dirty(),
        "the take is not in the document"
    );
    (h, file)
}

/// The pitches of the notes in the clips of the first track of `project`, clip by clip (active
/// takes).
fn recorded(project: &Project) -> Vec<Vec<u8>> {
    project.tracks[0]
        .clips
        .iter()
        .map(|c| match &c.content {
            komponist_model::ClipContent::Midi(m) => m.takes[m.active_take]
                .notes
                .iter()
                .map(|n| n.pitch)
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn recorded_in(file: &Path) -> Vec<Vec<u8>> {
    recorded(&project_io::load_project(file).unwrap())
}

#[test]
fn a_running_take_in_a_saved_project_asks_before_a_new_project() {
    let dir = scratch("take-new");
    let (mut h, file) = recording_into_a_saved_project(&dir);
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_value("Änderungen speichern?");
    // Cancel: the take keeps recording.
    h.get_by_label("Abbrechen").click();
    h.run_steps(2);
    assert!(h.query_by_value("Änderungen speichern?").is_none());
    assert!(h.state().recording().is_some());
    assert_eq!(
        h.state().transport(),
        komponist_engine::TransportState::Recording
    );
    assert_eq!(h.state().project().name, "Aufnahme");
    // "Nicht speichern": the take goes, the new project opens, the file stays as it was.
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_label("Nicht speichern").click();
    h.run_steps(2);
    assert!(h.state().recording().is_none());
    assert_eq!(h.state().project().name, "Unbenannt");
    assert_eq!(recorded_in(&file), Vec::<Vec<u8>>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_running_take_in_a_saved_project_asks_before_opening_another() {
    let dir = scratch("take-open");
    let mut other = new_project::empty_project(Lang::De);
    other.name = "Anderes".into();
    let other_file = dir.join("Anderes.komponist");
    save_and_open(&other_file, &other);
    let (mut h, file) = recording_into_a_saved_project(&dir);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::OpenPath(other_file.clone())));
    h.run_steps(2);
    h.get_by_value("Änderungen speichern?");
    h.get_by_label("Abbrechen").click();
    h.run_steps(2);
    assert!(h.state().recording().is_some());
    assert_eq!(h.state().project().name, "Aufnahme");
    // "Speichern": the take goes into the file, then the other project opens.
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::OpenPath(other_file)));
    h.run_steps(2);
    h.get_by_label("Speichern").click();
    h.run_steps(2);
    assert_eq!(h.state().project().name, "Anderes");
    assert!(h.state().recording().is_none());
    assert_eq!(recorded_in(&file), vec![vec![60]]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_running_take_in_a_saved_project_asks_before_quitting() {
    let dir = scratch("take-quit");
    let (mut h, file) = recording_into_a_saved_project(&dir);
    assert!(!h.state_mut().close_requested(), "the take asks first");
    h.run_steps(2);
    h.get_by_value("Änderungen speichern?");
    h.get_by_label("Abbrechen").click();
    h.run_steps(2);
    assert!(h.state().recording().is_some());
    // The quit action asks the same; "Speichern" writes the take, then the window may close.
    h.state_mut().dispatch(Action::Project(ProjectAction::Quit));
    h.run_steps(2);
    h.get_by_label("Speichern").click();
    h.run_steps(2);
    assert_eq!(recorded_in(&file), vec![vec![60]]);
    assert!(h.state_mut().close_requested());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_save_of_a_running_take_does_not_quit() {
    let dir = scratch("take-save-fails");
    let (mut h, file) = recording_into_a_saved_project(&dir);
    // The project's folder is gone: saving fails (saving never creates folders).
    std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
    h.state_mut().dispatch(Action::Project(ProjectAction::Quit));
    h.run_steps(2);
    h.get_by_label("Speichern").click();
    h.run_steps(2);
    assert!(!file.exists());
    assert!(
        !h.state_mut().close_requested(),
        "no quit after a failed save"
    );
    // Nothing of the take is lost: it is in the document, which is still unsaved.
    assert_eq!(recorded(h.state().project()), vec![vec![60]]);
    assert!(h.state().doc().is_dirty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saving_while_recording_writes_the_take_and_records_on() {
    let dir = scratch("take-save");
    let (mut h, file) = recording_into_a_saved_project(&dir);
    // Ctrl S while capturing: the file holds the take so far, capture goes on.
    press(&mut h, Modifiers::COMMAND, Key::S);
    assert_eq!(recorded_in(&file), vec![vec![60]]);
    assert!(h.state().recording().is_some());
    assert_eq!(
        h.state().transport(),
        komponist_engine::TransportState::Recording
    );
    // The file holds more than the document: still unsaved, so leaving asks.
    assert!(h.state().doc().is_dirty());
    run_held(&mut h, &[key_event(64, true)]);
    run_held(&mut h, &[key_event(64, false)]);
    // Stop: one take with both notes; saving again writes it once.
    press(&mut h, Modifiers::NONE, Key::R);
    assert!(h.state().recording().is_none());
    assert_eq!(recorded(h.state().project()), vec![vec![60, 64]]);
    assert!(h.state().doc().is_dirty());
    press(&mut h, Modifiers::COMMAND, Key::S);
    assert_eq!(recorded_in(&file), vec![vec![60, 64]]);
    assert!(!h.state().doc().is_dirty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_take_discarded_after_saving_it_leaves_the_project_unsaved() {
    let dir = scratch("take-save-discard");
    let (mut h, file) = recording_into_a_saved_project(&dir);
    press(&mut h, Modifiers::COMMAND, Key::S);
    assert_eq!(recorded_in(&file), vec![vec![60]]);
    // Esc throws the take away; the file still has it, so the project differs from its file.
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().recording().is_none());
    assert!(h.state().project().tracks[0].clips.is_empty());
    assert!(h.state().doc().is_dirty());
    assert!(!h.state_mut().close_requested(), "leaving asks");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// S06: a plugin that does not exist
// ------------------------------------------------------------------------------------------------

fn nonexistent_plugin() -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: "com.example.gibt-es-nicht".into(),
        name: "Gibt es nicht".into(),
        vendor: "Niemand".into(),
        version: "1.0".into(),
        module: Box::default(),
    }
}

/// An empty project with one track whose instrument is a plugin nobody has, with a state.
fn project_with_missing_plugin() -> (Project, Vec<u8>) {
    let p = new_project::empty_project(Lang::De);
    let (p, track) = komponist_model::edits::add_track(
        &p,
        komponist_model::edits::NewTrack {
            name: Some("Arp".into()),
            hue: None,
            instrument: Some(nonexistent_plugin()),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let device = p.track(track).unwrap().channel.devices[0].id;
    let state = vec![1u8, 2, 3, 250, 0, 7];
    let p =
        komponist_model::edits::set_device_state(&p, device, Some(state.clone().into())).unwrap();
    (p, state)
}

#[test]
fn a_missing_plugin_opens_s06_and_keeps_its_state() {
    let dir = scratch("missing");
    let (project, state) = project_with_missing_plugin();
    let file = dir.join("Fehlt.komponist");
    save_and_open(&file, &project);
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    let Some(Dialog::MissingPlugins(missing)) = &app.ui().dialog else {
        panic!("S06 expected, got {:?}", app.ui().dialog);
    };
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].plugin, nonexistent_plugin());
    let status = app.ui().status.as_ref().unwrap();
    assert_eq!(status.text, "Unbenannt geöffnet · 1 Plugin fehlt");
    let mut h = harness(app);
    h.get_by_value("1 Plugin fehlt");
    h.get_by_label_contains("Spur Arp · Instrument");
    h.get_by_label_contains("com.example.gibt-es-nicht");
    // "Plugin-Ordner prüfen": the settings at "Plugins".
    h.get_by_label("Plugin-Ordner prüfen").click();
    h.run_steps(3);
    assert_eq!(
        h.state().ui().dialog,
        Some(Dialog::Settings(SettingsSection::Plugins))
    );
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().ui().dialog.is_none());
    assert_eq!(h.state().ui().focus, FocusArea::Arrangement);
    // The device stays a placeholder with its state; saving keeps it byte for byte.
    let device = &h.state().project().tracks[0].channel.devices[0];
    assert_eq!(
        h.state().engine().device_status(device.id),
        DeviceStatus::Missing
    );
    let again = dir.join("Wieder.komponist");
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::SaveAs));
    h.run_steps(2);
    type_into(&mut h, "Dateiname", again.to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    let reloaded = project_io::load_project(&again).unwrap();
    assert_eq!(
        reloaded.tracks[0].channel.devices[0].state.as_deref(),
        Some(&state[..])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// hexa only, with a scan the test finishes by hand.
struct Scanning {
    inner: BuiltinHost,
    scanning: Arc<Mutex<bool>>,
    finished: bool,
}

impl DeviceHost for Scanning {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        self.inner.create_processor(device, setup)
    }
    fn release(&mut self, device: DeviceId) {
        self.inner.release(device);
    }
    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.inner.status(device)
    }
    fn latency(&self, device: DeviceId) -> u32 {
        self.inner.latency(device)
    }
    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        let mut events = self.inner.poll();
        if !*self.scanning.lock().unwrap() && !self.finished {
            self.finished = true;
            events.push(PluginManagerEvent::ScanFinished(
                komponist_plugins::ScanReport {
                    found: 0,
                    per_dir: Vec::new(),
                    skipped: Vec::new(),
                    finished: std::time::SystemTime::now(),
                },
            ));
        }
        events
    }
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.inner.role(plugin)
    }
    fn catalog(&self) -> &Catalog {
        self.inner.catalog()
    }
    fn is_scanning(&self) -> bool {
        *self.scanning.lock().unwrap()
    }
}

#[test]
fn s06_waits_for_the_running_scan() {
    let dir = scratch("scan");
    let (project, _) = project_with_missing_plugin();
    let file = dir.join("Fehlt.komponist");
    save_and_open(&file, &project);
    let scanning = Arc::new(Mutex::new(true));
    let host = Scanning {
        inner: BuiltinHost::new(),
        scanning: Arc::clone(&scanning),
        finished: false,
    };
    let mut app =
        app_in(&dir, new_project::empty_project(Lang::De)).with_device_host(Box::new(host));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    // The scan may still find the plugin: no dialog yet.
    assert!(app.ui().dialog.is_none());
    assert_eq!(app.ui().status.as_ref().unwrap().text, "Unbenannt geöffnet");
    app.step();
    assert!(app.ui().dialog.is_none());
    // The scan ends without it: now S06.
    *scanning.lock().unwrap() = false;
    app.step();
    assert!(matches!(app.ui().dialog, Some(Dialog::MissingPlugins(ref m)) if m.len() == 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn continuing_closes_s06() {
    let dir = scratch("continue");
    let (project, _) = project_with_missing_plugin();
    let file = dir.join("Fehlt.komponist");
    save_and_open(&file, &project);
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    let mut h = harness(app);
    h.get_by_label("Ohne dieses Plugin fortfahren").click();
    h.run_steps(3);
    assert!(h.state().ui().dialog.is_none());
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Project(ProjectAction::ContinueWithMissing))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// S09 start screen
// ------------------------------------------------------------------------------------------------

#[test]
fn the_start_screen_opens_recent_projects_with_the_keyboard() {
    let dir = scratch("start");
    let mut a = demo_project();
    a.name = "Night Drive".into();
    let mut b = demo_project();
    b.name = "Hafenlicht".into();
    let (pa, pb) = (
        dir.join("Night Drive.komponist"),
        dir.join("Hafenlicht.komponist"),
    );
    save_and_open(&pa, &a);
    save_and_open(&pb, &b);
    RecentList {
        projects: vec![pa.clone(), pb.clone(), dir.join("Weg.komponist")],
    }
    .save(&dir.join("config/recent.json"))
    .unwrap();
    let mut app = app_in(&dir, new_project::empty_project(Lang::De));
    app.show_start_screen();
    let mut h = harness(app);
    assert_eq!(h.state().ui().screen, Screen::Start);
    // The last project is preselected; the missing file is listed and says so.
    h.get_by_value("komponist");
    let rows: Vec<String> = h
        .get_all_by_role(Role::ListItem)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows[0].starts_with("Night Drive, 124 BPM · 6 Spuren"));
    assert!(rows[2].starts_with("Weg, Datei fehlt"));
    assert_eq!(h.state().ui().start.selected, 0);
    // Entf removes the missing file from the list (not from disk).
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    assert_eq!(h.state().ui().start.selected, 2);
    press(&mut h, Modifiers::NONE, Key::Delete);
    assert_eq!(h.state().recent(), [pa.clone(), pb.clone()]);
    // The filter, then ↑ and Enter.
    type_into(&mut h, "Filtern", "hafen");
    let rows = h.get_all_by_role(Role::ListItem).count();
    assert_eq!(rows, 1);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().ui().screen, Screen::Project);
    assert_eq!(h.state().project().name, "Hafenlicht");
    // Opened files move to the front of the list.
    assert_eq!(h.state().recent(), [pb, pa]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_start_screen_creates_projects_and_opens_the_settings() {
    let dir = scratch("start-new");
    let mut app = app_in(&dir, new_project::empty_project(Lang::De));
    app.show_start_screen();
    let mut h = harness(app);
    // A system row opens the settings at its section; Esc closes them again.
    h.get_by_label_contains("MIDI ").click();
    h.run_steps(3);
    assert_eq!(
        h.state().ui().dialog,
        Some(Dialog::Settings(SettingsSection::Midi))
    );
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().ui().dialog.is_none());
    assert_eq!(h.state().ui().screen, Screen::Start);
    // "Aus Vorlage": the template.
    h.get_by_label("Aus Vorlage").click();
    h.run_steps(3);
    assert_eq!(h.state().ui().screen, Screen::Project);
    assert_eq!(h.state().project().name, "Beat-Skizze");
    assert_eq!(h.state().project().tracks.len(), 3);
    // Back on the start screen, Ctrl N: S10.
    h.state_mut().show_start_screen();
    h.run_steps(2);
    press(&mut h, Modifiers::COMMAND, Key::N);
    assert_eq!(h.state().ui().screen, Screen::Project);
    assert_eq!(h.state().project().name, "Unbenannt");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// S07 settings
// ------------------------------------------------------------------------------------------------

#[test]
fn settings_apply_at_once() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
    let mut h = harness(app);
    h.get_by_value("Einstellungen");
    h.get_by_value(de::SETTINGS_NO_MIDI);
    // Buffer 512: the stream is rebuilt on the null driver, the engine stays.
    h.get_by_label("512").click();
    h.run_steps(3);
    assert_eq!(h.state().settings().audio.buffer_frames, 512);
    match h.state().engine().audio_state() {
        komponist_audio::AudioState::Running { config, .. } => {
            assert_eq!(config.buffer_frames, 512);
        }
        other => panic!("{other:?}"),
    }
    h.get_by_value("10.7 ms Latenz");
    // Navigation: "Plugins" highlights the section.
    h.get_by_label("Plugins").click();
    h.run_steps(3);
    assert_eq!(
        h.state().ui().dialog,
        Some(Dialog::Settings(SettingsSection::Plugins))
    );
    // The keyboard switch.
    assert!(!h.state().ui().keyboard_midi);
    // "Tastatur" is at the end of the page.
    h.get_by_role_and_label(Role::Switch, de::SETTINGS_KEYBOARD_MIDI)
        .scroll_to_me();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Switch, de::SETTINGS_KEYBOARD_MIDI)
        .click();
    h.run_steps(2);
    assert!(h.state().ui().keyboard_midi);
    // Esc closes and gives the focus back.
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().ui().dialog.is_none());
}

#[test]
fn without_a_device_play_is_off_and_s07_offers_a_retry() {
    // A configuration no backend runs: the stream fails, the engine runs on the null backend.
    let config = StreamConfig {
        host: HostKind::Null,
        buffer_frames: 0,
        ..StreamConfig::default()
    };
    let (engine, started) = EngineSync::new(
        &config,
        komponist_app::engine_sync::MidiSetup::None,
        Box::new(BuiltinHost::new()),
    );
    assert!(started.is_err());
    let settings = Settings {
        audio: config,
        ..Settings::default()
    };
    let mut app = App::new(Document::new(demo_project()), engine, settings, None);
    assert!(!app.audio_ok());
    app.dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
    let mut h = harness(app);
    // The red notice in the transport bar, the status bar and S07.
    assert!(h.get_all_by_label("Kein Audio-Gerät").count() >= 2);
    h.get_by_value(de::SETTINGS_NO_DEVICE_NOTICE);
    h.state_mut().clear_dispatched();
    h.get_by_label(de::SETTINGS_RETRY).click();
    h.run_steps(2);
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Settings(SettingsAction::RetryAudio))
    );
}

/// S07 "Darstellung" › Sprache: the segments switch the UI language in the next frame, the
/// choice is saved in `settings.json`, the AccessKit root names the language.
#[test]
fn the_language_row_switches_the_ui_at_once_and_is_saved() {
    let dir = scratch("language-row");
    let file = dir.join("config/settings.json");
    let mut app = app_in(&dir, demo_project());
    app.dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Display)));
    let mut h = harness(app);
    assert_eq!(h.root().accesskit_node().language(), Some("de"));
    h.get_by_role_and_label(Role::Label, de::SETTINGS_LANGUAGE);
    h.get_by_role_and_label(Role::RadioGroup, de::SETTINGS_LANGUAGE);
    h.get_by_role_and_label(Role::RadioButton, de::SETTINGS_LANGUAGE_SYSTEM);
    h.get_by_label(de::SETTINGS_TITLE);

    h.get_by_role_and_label(Role::RadioButton, en::SETTINGS_LANGUAGE_EN)
        .click();
    // One frame after the click: the dialog, the transport bar and the root are English.
    h.run_steps(2);
    assert_eq!(h.state().lang(), Lang::En);
    h.get_by_label(en::SETTINGS_TITLE);
    h.get_by_role_and_label(Role::RadioGroup, en::SETTINGS_LANGUAGE);
    h.get_by_role_and_label(Role::Button, en::TRANSPORT_PLAY);
    assert!(h.query_by_label(de::SETTINGS_TITLE).is_none());
    assert!(h.query_by_label(de::TRANSPORT_PLAY).is_none());
    assert_eq!(h.root().accesskit_node().language(), Some("en"));
    assert_eq!(h.state().settings().language, Language::En);
    assert_eq!(Settings::load(&file).unwrap().language, Language::En);

    h.get_by_role_and_label(Role::RadioButton, de::SETTINGS_LANGUAGE_DE)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().lang(), Lang::De);
    h.get_by_label(de::SETTINGS_TITLE);
    assert_eq!(h.root().accesskit_node().language(), Some("de"));
    assert_eq!(Settings::load(&file).unwrap().language, Language::De);
}

/// A status message is text of the old language: a switch clears it.
#[test]
fn a_language_switch_clears_the_status_message() {
    let dir = scratch("language-status");
    let mut app = app_in(&dir, demo_project());
    app.set_status("Gespeichert", komponist_app::ui_state::StatusKind::Success);
    app.dispatch(Action::Settings(SettingsAction::SetLanguage(Language::En)));
    assert_eq!(app.lang(), Lang::En);
    assert!(app.ui().status.is_none());
}

// ------------------------------------------------------------------------------------------------
// Snapshots next to S06, S07, S09, S10
// ------------------------------------------------------------------------------------------------

/// The CPU readout depends on the machine; masked in the snapshots.
fn mask_cpu(h: &mut Harness<'_, App>) {
    if let Some(cpu) = h.query_by_role_and_label(Role::Status, de::TRANSPORT_CPU) {
        let rect = cpu.rect();
        h.mask(rect.expand(2.0));
    }
}

/// The S06 situation: the demo project with Pad's chain as in S05 (TAL-Chorus-LX and
/// TAL-Reverb-4 after Dragonfly), and the two plugins S06 lists missing: Vital on Arp,
/// TAL-Reverb-4 as Pad's 4th effect.
fn s06() -> (Project, Vec<MissingPlugin>) {
    use komponist_model::ChannelRef;
    use komponist_model::edits::{NewDevice, insert_device};
    let plugin = |format, id: &str, name: &str| PluginRef {
        format,
        id: id.into(),
        name: name.into(),
        vendor: "TAL".into(),
        version: "1.0".into(),
        module: match format {
            PluginFormat::Vst3 => format!("{name}.vst3"),
            _ => format!("{name}.clap"),
        }
        .into(),
    };
    let mut p = demo_project();
    let pad = p.tracks.iter().find(|t| t.name == "Pad").unwrap().id;
    let mut reverb = None;
    for (format, id, name) in [
        (PluginFormat::Vst3, "com.tal.chorus-lx", "TAL-Chorus-LX"),
        (PluginFormat::Clap, "com.tal.reverb4", "TAL-Reverb-4"),
    ] {
        let len = p.track(pad).unwrap().channel.devices.len();
        let spec = NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin(format, id, name),
            state: None,
            sidechain: None,
        };
        let (next, device) = insert_device(&p, ChannelRef::Track(pad), len, spec).unwrap();
        p = next;
        reverb = Some(device);
    }
    let arp = p.tracks.iter().find(|t| t.name == "Arp").unwrap();
    let pad = p.track(pad).unwrap();
    let reverb = reverb.unwrap();
    let index = pad
        .channel
        .devices
        .iter()
        .position(|d| d.id == reverb)
        .unwrap();
    let missing = vec![
        MissingPlugin {
            track: Some(arp.id),
            device: arp.channel.devices[0].id,
            role: DeviceRole::Instrument,
            index: 0,
            plugin: arp.channel.devices[0].plugin.clone(),
        },
        MissingPlugin {
            track: Some(pad.id),
            device: reverb,
            role: DeviceRole::Effect,
            index,
            plugin: pad.channel.devices[index].plugin.clone(),
        },
    ];
    (p, missing)
}

/// The dialog shows the module file name; a VST3 from an old project (no module name) shows no
/// line, in particular not the hex class id.
#[test]
fn s06_shows_the_module_file_name_and_never_the_class_id() {
    let hex = "56535456697461766974616C00000000";
    let (project, missing) = s06();
    let show = |project: Project, missing: Vec<MissingPlugin>| {
        let mut app = App::headless(project);
        app.ui_mut().dialog = Some(Dialog::MissingPlugins(missing));
        app.ui_mut().focus = FocusArea::Dialog;
        let mut h = harness_at(app, 1.0);
        h.run_steps(3);
        h
    };
    let h = show(project.clone(), missing.clone());
    h.get_by_label_contains("Vital VST3, Spur Arp · Instrument, Vital.vst3");
    assert!(h.query_by_label_contains(hex).is_none());
    // The same project as an old file: no "module" anywhere.
    let old = project;
    let mut old_missing = missing;
    for m in &mut old_missing {
        m.plugin.module = Box::default();
    }
    let h = show(old, old_missing);
    h.get_by_label_contains("Vital VST3, Spur Arp · Instrument");
    assert!(h.query_by_label_contains("Vital.vst3").is_none());
    assert!(h.query_by_label_contains(hex).is_none());
}

#[test]
fn snapshots_of_s06_and_s10() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let (project, missing) = s06();
        let mut app = App::headless(project);
        app.ui_mut().dialog = Some(Dialog::MissingPlugins(missing));
        app.ui_mut().focus = FocusArea::Dialog;
        app.set_status(
            "Night Drive geöffnet · 2 Plugins fehlen",
            komponist_app::ui_state::StatusKind::Error,
        );
        let mut h = harness_at(app, ppp);
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot(format!("dialogs-s06-{ppp}x"));
        results.extend_harness(&mut h);
    }
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Project(ProjectAction::New));
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    mask_cpu(&mut h);
    h.snapshot("dialogs-s10-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

/// S07 as the design shows it: Focusrite unplugged, KeyStep active, four plugin folders.
fn s07_model() -> settings::Model {
    s07_model_in(Lang::De)
}

/// S07 in `lang`: the same situation, the texts of the model in that language.
fn s07_model_in(lang: Lang) -> settings::Model {
    let config = StreamConfig {
        host: HostKind::PipeWire,
        device: Some("Focusrite Scarlett 2i2".into()),
        device_id: Some(AudioDeviceId(
            "pipewire:alsa_output.usb-Focusrite_Scarlett_2i2-00.analog-stereo".into(),
        )),
        sample_rate: 48_000,
        buffer_frames: 256,
        jack_routes: Default::default(),
    };
    let (devices, device) = settings::device_entries(lang, HostKind::PipeWire, &[], &config);
    let dir = |clap: bool, label: &str, n: usize| settings::DirRow {
        clap,
        path: PathBuf::from(label),
        label: label.to_owned(),
        count: Some(n),
        removable: false,
    };
    settings::Model {
        input_devices: vec![settings::InputEntry {
            label: lang.audio_input_none().to_owned(),
            config: None,
        }],
        input_device: 0,
        jack_ports: None,
        jack_scan_failed: false,
        jack_playback_connections: None,
        capture_format: komponist_app::settings::CaptureFormat::Pcm24,
        section: SettingsSection::Audio,
        hosts: vec![
            HostKind::PipeWire,
            HostKind::Jack,
            HostKind::Alsa,
            HostKind::Null,
        ],
        config,
        devices,
        device,
        rates: vec![44_100, 48_000, 96_000],
        no_device: Some("no audio device".into()),
        buffer: settings::BufferLimit::Free,
        latency: match lang {
            Lang::De => "5.3 ms Latenz",
            Lang::En => "5.3 ms latency",
        }
        .into(),
        realtime_denied: false,
        midi: Some(vec![
            settings::MidiRow {
                name: "Arturia KeyStep 37 MIDI 1".into(),
                label: "Arturia KeyStep 37".into(),
                enabled: true,
                connected: true,
                active: true,
                busy: false,
            },
            settings::MidiRow {
                name: "Midi Through Port-0".into(),
                label: "Midi Through Port-0".into(),
                enabled: false,
                connected: true,
                active: false,
                busy: false,
            },
        ]),
        dirs: vec![
            dir(true, "~/.clap", 38),
            dir(true, "/usr/lib/clap", 61),
            dir(false, "~/.vst3", 47),
            dir(false, "/usr/lib/vst3", 68),
        ],
        scan: match lang {
            Lang::De => {
                "214 Plugins gefunden · letzter Scan heute 09:12 · 1 Plugin übersprungen (lässt \
                 sich nicht laden)"
            }
            Lang::En => {
                "214 plugins found · last scan today 09:12 · 1 plugin skipped (cannot be loaded)"
            }
        }
        .into(),
        scanning: false,
        ui_scale: None,
        language: Language::System,
        keyboard_midi: false,
    }
}

#[test]
fn snapshot_of_s07() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        // The S07 situation behind the dialog: no audio device.
        let config = StreamConfig {
            host: HostKind::Null,
            buffer_frames: 0,
            ..StreamConfig::default()
        };
        let (engine, _) = EngineSync::new(
            &config,
            komponist_app::engine_sync::MidiSetup::None,
            Box::new(BuiltinHost::new()),
        );
        let app = App::new(
            Document::new(demo_project()),
            engine,
            Settings::default(),
            None,
        );
        let model = s07_model();
        let mut h = komponist_ui::snapshot::builder::<App>(ppp)
            .with_size(vec2(W, H))
            .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
            .build_ui_state(
                move |ui, app: &mut App| {
                    app.show(ui);
                    let _ = settings::show(ui.ctx(), &model, None);
                },
                app,
            );
        h.run_steps(4);
        mask_cpu(&mut h);
        h.snapshot(format!("dialogs-s07-{ppp}x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

/// S07 in English (#5): the same situation, English texts.
#[test]
fn snapshot_of_s07_in_english() {
    let config = StreamConfig {
        host: HostKind::Null,
        buffer_frames: 0,
        ..StreamConfig::default()
    };
    let (engine, _) = EngineSync::new(
        &config,
        komponist_app::engine_sync::MidiSetup::None,
        Box::new(BuiltinHost::new()),
    );
    let app = App::new(
        Document::new(demo_project()),
        engine,
        Settings::default(),
        None,
    )
    .with_lang(Lang::En);
    let model = s07_model_in(Lang::En);
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            move |ui, app: &mut App| {
                app.show(ui);
                let _ = settings::show(ui.ctx(), &model, None);
            },
            app,
        );
    h.run_steps(4);
    let cpu = h
        .get_by_role_and_label(Role::Status, en::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    h.snapshot("dialogs-s07-en-1x");
}

/// S07 with WASAPI selected (Windows): the buffer size is the device period, so its segments are
/// disabled and a hint names the real size; a MIDI input another program holds says so instead of
/// "active". The devices are fake (rule 14). No reference snapshot: this is rendered, not compared;
/// set `KOMPONIST_RENDER_DIR` to also write the PNGs (`s07-wasapi-de.png`, `s07-wasapi-en.png`).
#[test]
fn s07_with_wasapi_shows_the_fixed_buffer_and_a_busy_midi_input() {
    for lang in [Lang::De, Lang::En] {
        let config = StreamConfig {
            host: HostKind::Wasapi,
            device: None,
            device_id: None,
            sample_rate: 48_000,
            buffer_frames: 256,
            jack_routes: Default::default(),
        };
        let (devices, device) = settings::device_entries(lang, HostKind::Wasapi, &[], &config);
        let mut model = s07_model_in(lang);
        model.hosts = vec![HostKind::Wasapi, HostKind::Null];
        model.config = config;
        model.devices = devices;
        model.device = device;
        model.rates = vec![44_100, 48_000, 96_000];
        model.no_device = None;
        model.buffer = settings::BufferLimit::Fixed(480);
        model.latency = match lang {
            Lang::De => "10.0 ms Latenz",
            Lang::En => "10.0 ms latency",
        }
        .into();
        if let Some(rows) = model.midi.as_mut() {
            rows[0].busy = true;
            rows[0].active = false;
        }
        let hint = match lang {
            Lang::De => "Der Treiber gibt 480 Samples vor.",
            Lang::En => "The driver sets 480 samples.",
        };
        let busy = match lang {
            Lang::De => "belegt · ein anderes Programm nutzt den Eingang",
            Lang::En => "in use · another program holds the input",
        };
        assert_eq!(model.midi.as_ref().unwrap()[0].info(lang), busy);
        let app = App::headless(demo_project()).with_lang(lang);
        let mut h = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(vec2(W, H))
            .build_ui_state(
                move |ui, app: &mut App| {
                    app.show(ui);
                    let _ = settings::show(ui.ctx(), &model, None);
                },
                app,
            );
        h.run_steps(4);
        // The hint is a label of the page; the segments are disabled, none is selected.
        h.get_by_label_contains(hint);
        h.get_by_label_contains(busy);
        if let Some(dir) = std::env::var_os("KOMPONIST_RENDER_DIR") {
            let img = h.render().expect("render");
            let name = format!(
                "s07-wasapi-{}.png",
                if lang == Lang::De { "de" } else { "en" }
            );
            img.save(std::path::Path::new(&dir).join(name)).unwrap();
        }
    }
}

/// S07 with ASIO listed (Windows build with the feature `asio`): the "ASIO Compatible" logo sits
/// in the driver row with an accessible name; without ASIO in the build, it is not drawn at all
/// (Linux, macOS, Windows without the feature). Fake devices (rule 14), nothing is opened. Set
/// `KOMPONIST_RENDER_DIR` to also write the PNGs (`s07-asio-de.png`, `s07-asio-en.png`).
#[test]
fn s07_shows_the_asio_logo_where_asio_is_offered() {
    for lang in [Lang::De, Lang::En] {
        let config = StreamConfig {
            host: HostKind::Asio,
            device: Some("ASIO4ALL v2".into()),
            device_id: None,
            sample_rate: 48_000,
            buffer_frames: 256,
            jack_routes: Default::default(),
        };
        let (devices, device) = settings::device_entries(lang, HostKind::Asio, &[], &config);
        let mut model = s07_model_in(lang);
        model.hosts = vec![HostKind::Wasapi, HostKind::Asio, HostKind::Null];
        model.config = config;
        model.devices = devices;
        model.device = device;
        model.rates = vec![44_100, 48_000, 96_000];
        model.no_device = None;
        model.buffer = settings::BufferLimit::Range(64, 2048);
        let name = match lang {
            Lang::De => de::SETTINGS_ASIO_LOGO,
            Lang::En => en::SETTINGS_ASIO_LOGO,
        };
        let app = App::headless(demo_project()).with_lang(lang);
        let mut h = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(vec2(W, H))
            .build_ui_state(
                move |ui, app: &mut App| {
                    app.show(ui);
                    let _ = settings::show(ui.ctx(), &model, None);
                },
                app,
            );
        h.run_steps(4);
        let logo = h.query_by_label(name);
        assert_eq!(
            logo.is_some(),
            HostKind::Asio.is_supported_here(),
            "logo only where the ASIO segment is shown"
        );
        if let Some(logo) = logo {
            assert_eq!(logo.accesskit_node().role(), Role::Image);
        }
        if let Some(dir) = std::env::var_os("KOMPONIST_RENDER_DIR") {
            let img = h.render().expect("render");
            let tag = if lang == Lang::De { "de" } else { "en" };
            img.save(std::path::Path::new(&dir).join(format!("s07-asio-{tag}.png")))
                .unwrap();
        }
    }
}
#[test]
fn buffer_limits_follow_the_driver() {
    let device = |host, range| DeviceInfo {
        host,
        id: Some(AudioDeviceId("d".into())),
        name: "Out".into(),
        is_default: true,
        channels: 2,
        sample_rates: vec![48_000],
        buffer_range: range,
    };
    let config = StreamConfig::default();
    let entry = |h, r| {
        let devices = vec![device(h, r)];
        let (entries, i) = settings::device_entries(Lang::En, h, &devices, &config);
        (devices, entries[i].clone())
    };
    // WASAPI: the device period, or the one the stream reports.
    let (d, e) = entry(HostKind::Wasapi, Some((480, 480)));
    assert_eq!(
        settings::buffer_limit(HostKind::Wasapi, &d, &e, None),
        settings::BufferLimit::Fixed(480)
    );
    let running = StreamConfig {
        host: HostKind::Wasapi,
        buffer_frames: 528,
        ..StreamConfig::default()
    };
    assert_eq!(
        settings::buffer_limit(HostKind::Wasapi, &d, &e, Some(&running)),
        settings::BufferLimit::Fixed(528)
    );
    let (d, e) = entry(HostKind::Wasapi, None);
    assert_eq!(
        settings::buffer_limit(HostKind::Wasapi, &d, &e, None),
        settings::BufferLimit::Free
    );
    // ASIO: the driver's range; one size when min == max.
    let (d, e) = entry(HostKind::Asio, Some((64, 2048)));
    assert_eq!(
        settings::buffer_limit(HostKind::Asio, &d, &e, None),
        settings::BufferLimit::Range(64, 2048)
    );
    let (d, e) = entry(HostKind::Asio, Some((480, 480)));
    assert_eq!(
        settings::buffer_limit(HostKind::Asio, &d, &e, None),
        settings::BufferLimit::Fixed(480)
    );
    // The other drivers are not limited by the dialog.
    let (d, e) = entry(HostKind::Jack, Some((256, 256)));
    assert_eq!(
        settings::buffer_limit(HostKind::Jack, &d, &e, None),
        settings::BufferLimit::Free
    );
}

/// S07 scrolled to "Appearance": the language row (#5), German and English.
#[test]
fn snapshots_of_the_appearance_section() {
    let mut results = SnapshotResults::new();
    for (lang, name) in [
        (Lang::De, "dialogs-s07-display-1x"),
        (Lang::En, "dialogs-s07-display-en-1x"),
    ] {
        let engine = EngineSync::null(Box::new(BuiltinHost::new()));
        let app = App::new(
            Document::new(demo_project()),
            engine,
            Settings::default(),
            None,
        )
        .with_lang(lang);
        let model = settings::Model {
            section: SettingsSection::Display,
            ..s07_model_in(lang)
        };
        let mut h = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(vec2(W, H))
            .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
            .build_ui_state(
                move |ui, app: &mut App| {
                    app.show(ui);
                    let _ = settings::show(ui.ctx(), &model, Some(SettingsSection::Display));
                },
                app,
            );
        h.run_steps(4);
        let cpu = h
            .get_by_role_and_label(
                Role::Status,
                if lang == Lang::De {
                    de::TRANSPORT_CPU
                } else {
                    en::TRANSPORT_CPU
                },
            )
            .rect();
        h.mask(cpu.expand(2.0));
        h.snapshot(name);
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

/// S09's recent list, verbatim from `S09.recentList` (paths with `.komponist`).
fn s09_model() -> start::Model {
    let row = |name: &str, path: &str, meta: &str, date: &str, hues: &[u16]| start::RecentRow {
        path: PathBuf::from(path),
        name: name.to_owned(),
        path_label: path.to_owned(),
        meta: meta.to_owned(),
        date: date.to_owned(),
        hues: hues.to_vec(),
        missing: false,
    };
    start::Model {
        version: start::version_line(Lang::De),
        new_shortcut: "Strg N".into(),
        open_shortcut: "Strg O".into(),
        recent: vec![
            row(
                "Night Drive",
                "~/Musik/klang/Night Drive.komponist",
                "124 BPM · 6 Spuren",
                "vor 2 Stunden",
                &[45, 250, 150, 300, 95, 200],
            ),
            row(
                "Hafenlicht",
                "~/Musik/klang/Hafenlicht.komponist",
                "98 BPM · 9 Spuren",
                "gestern",
                &[25, 70, 175, 250, 325, 45, 150, 95, 225],
            ),
            row(
                "Beat-Skizze 14",
                "~/Musik/klang/skizzen/beat-14.komponist",
                "140 BPM · 3 Spuren",
                "12. Okt.",
                &[45, 250, 300],
            ),
            row(
                "FM-Glocken",
                "~/Musik/klang/FM-Glocken.komponist",
                "72 BPM · 5 Spuren",
                "8. Okt.",
                &[200, 225, 150, 275, 350],
            ),
            row(
                "Live-Set Oktober",
                "~/Musik/klang/live/oktober.komponist",
                "126 BPM · 12 Spuren",
                "2. Okt.",
                &[45, 25, 250, 150, 300, 95, 200, 70, 175, 325, 225, 125],
            ),
        ],
        system: vec![
            start::SystemRow {
                label: "Audio",
                info: "PipeWire · Scarlett 2i2 · 48 kHz · 256".into(),
                health: start::Health::Ok,
                section: SettingsSection::Audio,
            },
            start::SystemRow {
                label: "MIDI",
                info: "Arturia KeyStep 37".into(),
                health: start::Health::Ok,
                section: SettingsSection::Midi,
            },
            start::SystemRow {
                label: "Plugins",
                info: "214 gefunden · Einstellungen Strg ,".into(),
                health: start::Health::Idle,
                section: SettingsSection::Plugins,
            },
        ],
        message: None,
    }
}

/// S09 in English (#5): the same list, English texts and dates.
#[test]
fn snapshot_of_s09_in_english() {
    let mut model = s09_model();
    model.version = start::version_line(Lang::En);
    model.new_shortcut = "Ctrl+N".into();
    model.open_shortcut = "Ctrl+O".into();
    let texts = [
        ("124 BPM · 6 tracks", "2 hours ago"),
        ("98 BPM · 9 tracks", "yesterday"),
        ("140 BPM · 3 tracks", "Oct 12"),
        ("72 BPM · 5 tracks", "Oct 8"),
        ("126 BPM · 12 tracks", "Oct 2"),
    ];
    for (row, (meta, date)) in model.recent.iter_mut().zip(texts) {
        row.meta = meta.into();
        row.date = date.into();
    }
    model.system[2].info = "214 found · Settings Ctrl+,".into();
    let mut h = komponist_ui::snapshot::builder::<StartView>(1.0)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            move |ui, view: &mut StartView| {
                komponist_ui::set_lang(ui.ctx(), Lang::En);
                let rect = ui.ctx().content_rect();
                let _ = start::show(ui, rect, &model, view, true);
            },
            StartView::default(),
        );
    h.run_steps(3);
    h.snapshot("dialogs-s09-en-1x");
}

#[test]
fn snapshot_of_s09() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let model = s09_model();
        let mut h = komponist_ui::snapshot::builder::<StartView>(ppp)
            .with_size(vec2(W, H))
            .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
            .build_ui_state(
                move |ui, view: &mut StartView| {
                    let rect = ui.ctx().content_rect();
                    let _ = start::show(ui, rect, &model, view, true);
                },
                StartView::default(),
            );
        h.run_steps(3);
        h.snapshot(format!("dialogs-s09-{ppp}x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

#[test]
fn snapshots_of_the_file_browser_and_the_prompt() {
    use komponist_app::dialogs::file::{Entry, FileBrowser, Purpose};
    let mut results = SnapshotResults::new();
    let mut browser = FileBrowser::new(
        Lang::De,
        Purpose::Save,
        PathBuf::from("/gibt/es/nicht"),
        String::new(),
    );
    let dir = PathBuf::from("/home/kim/Musik/klang");
    browser.dir = dir.clone();
    browser.location = "~/Musik/klang".into();
    browser.message = None;
    browser.name = "Hafenlicht.komponist".into();
    browser.entries = ["live", "skizzen"]
        .iter()
        .map(|n| Entry {
            name: (*n).into(),
            path: dir.join(n),
            is_dir: true,
        })
        .chain(
            [
                "FM-Glocken.komponist",
                "Hafenlicht.komponist",
                "Night Drive.komponist",
            ]
            .iter()
            .map(|n| Entry {
                name: (*n).into(),
                path: dir.join(n),
                is_dir: false,
            }),
        )
        .collect();
    browser.selected = Some(3);
    let mut h = komponist_ui::snapshot::builder::<FileBrowser>(1.0)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            |ui, b: &mut FileBrowser| {
                let _ = komponist_app::dialogs::file::show(ui.ctx(), b);
            },
            browser,
        );
    h.run_steps(3);
    h.snapshot("dialogs-file-1x");
    results.extend_harness(&mut h);

    let mut h = komponist_ui::snapshot::builder::<()>(1.0)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            |ui, _: &mut ()| {
                let _ = new_project::show_unsaved(ui.ctx(), "Night Drive");
            },
            (),
        );
    h.run_steps(3);
    h.snapshot("dialogs-unsaved-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

/// The S06 model of the demo with S06's two plugins says what the screen says.
#[test]
fn s06_texts_match_the_screen() {
    let (project, missing) = s06();
    let model = missing_plugins::model(Lang::De, &project, &missing);
    assert_eq!(model.rows[0].name, "Vital");
    // Under the name stands the module file name (owner decision Q4, 2026-11-14), not the VST3
    // class id and not S06's mock-up id "audio.vital.synth".
    assert_eq!(model.rows[0].module, "Vital.vst3");
    assert_eq!(model.rows[0].place, "Spur Arp · Instrument");
    assert_eq!(model.rows[0].format, "VST3");
    assert_eq!(model.rows[1].place, "Spur Pad · Effekt 4");
    assert_eq!(
        missing_plugins::title(Lang::De, model.rows.len()),
        "2 Plugins fehlen"
    );
}

/// Drivers and devices that take long to enumerate, like ALSA's devices (WP-61 measured
/// 170–200 ms) or the JACK probe; records the threads that asked.
struct SlowDevices {
    threads: Arc<Mutex<Vec<String>>>,
}

impl SlowDevices {
    fn record(&self) {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let name = std::thread::current().name().unwrap_or("").to_owned();
        self.threads.lock().unwrap().push(name);
    }
}

impl settings::AudioDevices for SlowDevices {
    fn hosts(&self) -> Vec<HostKind> {
        self.record();
        vec![HostKind::Null]
    }

    fn devices(&self, _host: HostKind) -> Vec<komponist_audio::DeviceInfo> {
        self.record();
        Vec::new()
    }

    fn is_slow(&self) -> bool {
        true
    }
}

/// S07 enumerates drivers and devices on a worker thread, so opening it never stalls a frame
/// (WP-52 note, WP-61).
#[test]
fn s07_enumerates_devices_off_the_ui_thread() {
    let threads = Arc::new(Mutex::new(Vec::new()));
    let app = App::headless(demo_project()).with_audio_devices(Box::new(SlowDevices {
        threads: Arc::clone(&threads),
    }));
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
    let t0 = std::time::Instant::now();
    h.run_steps(2);
    // On the UI thread the two enumerations would take at least 600 ms (2 × 300 ms); two
    // debug-build frames on a loaded machine took up to 475 ms (2026-11-15). The thread names
    // below are the exact check.
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(590),
        "opening S07 waited for the enumeration: {:?}",
        t0.elapsed()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while threads.lock().unwrap().len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "enumeration never ran"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_steps(1);
    }
    let threads = threads.lock().unwrap().clone();
    assert_eq!(threads, ["komponist-audio-devices"; 2]);
}

// ------------------------------------------------------------------------------------------------
// Files from the desktop (#96: Finder, the Dock icon, a launch with files)
// ------------------------------------------------------------------------------------------------

#[test]
fn a_file_from_the_desktop_opens_like_one_of_the_recent_list() {
    let dir = scratch("desktop-open");
    let mut other = new_project::empty_project(Lang::De);
    other.name = "Hafenlicht".into();
    let file = dir.join("Hafenlicht.komponist");
    save_and_open(&file, &other);
    // From the start screen (a launch with the file): it opens and joins the recent list.
    let mut app = app_in(&dir, new_project::empty_project(Lang::De));
    app.show_start_screen();
    app.open_files(std::slice::from_ref(&file));
    assert_eq!(app.ui().screen, Screen::Project);
    assert_eq!(app.project().name, "Hafenlicht");
    assert_eq!(app.recent(), std::slice::from_ref(&file));
    // The open project's file again (a second double-click): nothing is loaded again.
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.open_files(std::slice::from_ref(&file));
    assert_eq!(app.project().tracks.len(), 1, "the unsaved track stays");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_from_the_desktop_asks_to_save_a_dirty_project_first() {
    let dir = scratch("desktop-dirty");
    let mut other = new_project::empty_project(Lang::De);
    other.name = "Hafenlicht".into();
    let file = dir.join("Hafenlicht.komponist");
    save_and_open(&file, &other);
    let mut app = app_in(&dir, new_project::empty_project(Lang::De));
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let mut h = harness(app);
    // Several files: the first one is opened, after the unsaved-changes prompt.
    h.state_mut()
        .open_files(&[file.clone(), dir.join("Zweites.komponist")]);
    h.run_steps(2);
    assert_ne!(h.state().project().name, "Hafenlicht");
    h.get_by_label("Nicht speichern").click();
    h.run_steps(2);
    assert_eq!(h.state().project().name, "Hafenlicht");
    // A file that cannot be read: the reason in the status bar, the project stays.
    let broken = dir.join("Kaputt.komponist");
    std::fs::write(&broken, "kein json").unwrap();
    h.state_mut().open_files(&[broken]);
    h.run_steps(2);
    assert_eq!(h.state().project().name, "Hafenlicht");
    let status = h.state().ui().status.as_ref().unwrap();
    assert!(
        status
            .text
            .starts_with("„Kaputt.komponist“ lässt sich nicht öffnen")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn s07_jack_ports_show_direction_missing_ports_and_update_routes_in_both_languages() {
    for lang in [Lang::De, Lang::En] {
        let mut model = s07_model_in(lang);
        model.config.host = HostKind::Jack;
        model.config.jack_routes.capture =
            Some(vec!["Interface:right".into(), "Missing:left".into()]);
        model.config.jack_routes.playback = [Some("Interface:speaker_left".into()), None];
        model.no_device = None;
        model.jack_playback_connections = Some([false, false]);
        model.buffer = settings::BufferLimit::Fixed(64);
        model.jack_ports = Some(vec![
            JackPort {
                name: "Interface:left".into(),
                alias: Some("Hotone MP80".into()),
                direction: JackDirection::Capture,
            },
            JackPort {
                name: "Interface:right".into(),
                alias: Some("Hotone MP80".into()),
                direction: JackDirection::Capture,
            },
            JackPort {
                name: "Interface:speaker_left".into(),
                alias: None,
                direction: JackDirection::Playback,
            },
            JackPort {
                name: "Interface:speaker_right".into(),
                alias: None,
                direction: JackDirection::Playback,
            },
        ]);
        let app = App::headless(demo_project()).with_lang(lang);
        let mut h = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(vec2(W, H))
            .build_ui_state(
                move |ui, app: &mut App| {
                    app.show(ui);
                    for output in settings::show(ui.ctx(), &model, None) {
                        match output {
                            settings::Output::Act(Action::Settings(SettingsAction::SetAudio(
                                config,
                            ))) => {
                                model.config = config;
                            }
                            settings::Output::Act(Action::Settings(
                                SettingsAction::SetJackCaptureAutomatic,
                            )) => {
                                model.config.jack_routes.capture = None;
                            }
                            _ => {}
                        }
                    }
                },
                app,
            );
        h.run_steps(4);
        let combo_value = |h: &Harness<'_, App>, label: &str| {
            h.get_by_role_and_label(Role::ComboBox, label)
                .accesskit_node()
                .value()
                .unwrap()
                .to_owned()
        };
        let left = lang.settings_jack_playback_left();
        let right = lang.settings_jack_playback_right();
        let capture = lang.settings_jack_capture_channel(2);
        assert_eq!(combo_value(&h, left), "Interface:speaker_left");
        assert_eq!(combo_value(&h, right), lang.settings_jack_disconnected());
        assert_eq!(
            combo_value(&h, &capture),
            lang.settings_jack_port_missing("Missing:left")
        );

        h.get_by_role_and_label(
            Role::Label,
            &lang.settings_jack_playback_unconnected("Interface:speaker_left"),
        );

        if let Some(dir) = std::env::var_os("KOMPONIST_RENDER_DIR") {
            let filename = if lang == Lang::De {
                "s07-jack-de.png"
            } else {
                "s07-jack-en.png"
            };
            h.render()
                .expect("render")
                .save(Path::new(&dir).join(filename))
                .unwrap();
        }

        h.get_by_role_and_label(Role::ComboBox, &capture).click();
        h.run_steps(4);
        for name in [
            "Hotone MP80 · Interface:left",
            "Hotone MP80 · Interface:right",
        ] {
            h.get_by_role_and_label(Role::ListBoxOption, name);
        }
        assert!(
            h.query_by_role_and_label(Role::ListBoxOption, "Interface:speaker_right")
                .is_none()
        );
        h.get_by_role_and_label(Role::ListBoxOption, "Hotone MP80 · Interface:right")
            .click();
        h.run_steps(4);
        assert_eq!(
            combo_value(&h, &lang.settings_jack_capture_channel(1)),
            lang.settings_jack_port_missing("Missing:left")
        );
        assert_eq!(combo_value(&h, &capture), "Hotone MP80 · Interface:right");

        h.get_by_role_and_label(Role::ComboBox, right).click();
        h.run_steps(4);
        assert!(
            h.query_by_role_and_label(Role::ListBoxOption, "Hotone MP80 · Interface:right")
                .is_none()
        );
        h.get_by_role_and_label(Role::ListBoxOption, "Interface:speaker_right")
            .click();
        h.run_steps(4);
        assert_eq!(combo_value(&h, right), "Interface:speaker_right");
        h.get_by_role_and_label(Role::Button, lang.settings_jack_auto_capture())
            .click();
        h.run_steps(4);
        h.get_by_role_and_label(Role::ComboBox, lang.settings_input_device());
        assert!(
            h.query_by_role_and_label(Role::ComboBox, &capture)
                .is_none()
        );
    }
}

#[test]
fn jack_port_choices_survive_a_settings_file_round_trip() {
    let dir = scratch("jack-routes");
    let path = dir.join("settings.json");
    let config = StreamConfig {
        host: HostKind::Jack,
        jack_routes: komponist_audio::JackRoutes {
            capture: Some(vec!["usb:rear".into(), "usb:front".into()]),
            playback: [Some("mixer:left".into()), Some("mixer:right".into())],
        },
        ..StreamConfig::default()
    };
    let settings = Settings {
        audio: config,
        ..Default::default()
    };
    settings.save(&path).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert_eq!(loaded.audio.jack_routes, settings.audio.jack_routes);
    std::fs::remove_dir_all(dir).unwrap();
}
