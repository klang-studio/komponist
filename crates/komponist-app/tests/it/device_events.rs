//! How the app handles its device host (WP-46), with a scripted host and no processes: state
//! snapshots go into the document as coalesced undo steps, latency changes recompile, a finished
//! scan loads the devices it found, crashes and warnings reach the status bar, plugin actions,
//! saving and exit reach the host.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use komponist_app::actions::{EditAction, PluginAction, ProjectAction, SettingsAction};
use komponist_app::device_host::DeviceHost;
use komponist_app::engine_sync::EngineSync;
use komponist_app::settings::Settings;
use komponist_app::ui_state::{PluginView, StatusKind};
use komponist_app::{Action, App};
use komponist_engine::Processor;
use komponist_ipc::{LogLevel, PluginDescriptor, PluginFormat as BundleFormat, PluginKind};
use komponist_model::edits::ProjectProp;
use komponist_model::fixtures::demo_project;
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginRef, TrackId, persist};
use komponist_plugins::manager::STATE_LOG_PREFIX;
use komponist_plugins::{
    Catalog, DeviceStatus, FailReason, PluginDirs, PluginError, PluginManagerEvent, ProcessorSetup,
    ScanReport,
};

/// What the test scripts and observes.
#[derive(Default)]
struct Script {
    /// Returned by the next `poll`.
    events: Vec<PluginManagerEvent>,
    /// Plugins the next `poll` "finds" (then `ScanFinished`).
    found: Option<Vec<PluginDescriptor>>,
    latencies: HashMap<DeviceId, u32>,
    /// Answer of `snapshot_states`.
    snapshot: Vec<(DeviceId, Arc<[u8]>)>,
    // Observed:
    created: Vec<DeviceId>,
    released: Vec<DeviceId>,
    scans: Vec<PluginDirs>,
    params: Vec<(DeviceId, u32, f64)>,
    closed_guis: Vec<DeviceId>,
    snapshot_calls: usize,
    shut_down: bool,
}

/// A device host without processors: devices whose plugin the catalog knows are `Ok` (and have a
/// GUI unless they are built in), the others `Missing`.
struct FakeHost {
    script: Rc<RefCell<Script>>,
    catalog: Catalog,
    statuses: HashMap<DeviceId, DeviceStatus>,
}

impl DeviceHost for FakeHost {
    fn create_processor(
        &mut self,
        device: &Device,
        _setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        self.script.borrow_mut().created.push(device.id);
        let status = match self.catalog.resolve(&device.plugin) {
            Some(_) => DeviceStatus::Ok,
            None => DeviceStatus::Missing,
        };
        self.statuses.insert(device.id, status);
        None
    }

    fn release(&mut self, device: DeviceId) {
        self.script.borrow_mut().released.push(device);
        self.statuses.remove(&device);
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.statuses
            .get(&device)
            .cloned()
            .unwrap_or(DeviceStatus::Ok)
    }

    fn latency(&self, device: DeviceId) -> u32 {
        self.script
            .borrow()
            .latencies
            .get(&device)
            .copied()
            .unwrap_or(0)
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        let mut script = self.script.borrow_mut();
        let mut events = std::mem::take(&mut script.events);
        if let Some(found) = script.found.take() {
            self.catalog.set_scanned(&found);
            events.push(PluginManagerEvent::ScanFinished(ScanReport {
                found: found.len(),
                per_dir: Vec::new(),
                skipped: Vec::new(),
                finished: SystemTime::now(),
            }));
        }
        events
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.catalog.resolve(plugin).map(|e| match e.kind {
            PluginKind::Effect => DeviceRole::Effect,
            _ => DeviceRole::Instrument,
        })
    }

    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn start_scan(&mut self, dirs: &PluginDirs) {
        self.script.borrow_mut().scans.push(dirs.clone());
    }

    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        // hexa goes through the engine.
        if self.is_builtin(device) {
            return false;
        }
        self.script.borrow_mut().params.push((device, id, value));
        true
    }

    fn has_gui(&self, device: DeviceId) -> bool {
        !self.is_builtin(device) && self.status(device) == DeviceStatus::Ok
    }

    fn open_gui(
        &mut self,
        device: DeviceId,
        _target: komponist_ipc::GuiTarget,
    ) -> Result<(), PluginError> {
        Err(PluginError::UnknownDevice(device))
    }

    fn close_gui(&mut self, device: DeviceId) {
        self.script.borrow_mut().closed_guis.push(device);
    }

    fn snapshot_states(&mut self, _timeout: Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        let mut script = self.script.borrow_mut();
        script.snapshot_calls += 1;
        script.snapshot.clone()
    }

    fn shutdown(&mut self) {
        self.script.borrow_mut().shut_down = true;
    }
}

impl FakeHost {
    /// Whether `device` is the demo's hexa (ids of the demo: Bass 9, Lead 94).
    fn is_builtin(&self, device: DeviceId) -> bool {
        demo_project()
            .tracks
            .iter()
            .flat_map(|t| &t.channel.devices)
            .any(|d| d.id == device && d.plugin.name == "Hexa")
    }
}

/// The demo on the null backend with a [`FakeHost`] that knows only hexa.
fn app() -> (App, Rc<RefCell<Script>>) {
    let script = Rc::new(RefCell::new(Script::default()));
    let host = FakeHost {
        script: Rc::clone(&script),
        catalog: Catalog::builtin(),
        statuses: HashMap::new(),
    };
    let engine = EngineSync::null(Box::new(host));
    let mut app = App::new(
        Document::new(demo_project()),
        engine,
        Settings::default(),
        None,
    );
    app.step();
    (app, script)
}

/// The demo's devices by plugin name (first one each).
fn device(app: &App, name: &str) -> DeviceId {
    app.project()
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .find(|d| d.plugin.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
        .id
}

fn state(app: &App, device: DeviceId) -> Option<Arc<[u8]>> {
    app.project()
        .find_device(device)
        .and_then(|(_, _, d)| d.state.clone())
}

fn push(script: &Rc<RefCell<Script>>, event: PluginManagerEvent) {
    script.borrow_mut().events.push(event);
}

fn status_text(app: &App) -> Option<(String, StatusKind)> {
    app.ui().status.as_ref().map(|s| (s.text.clone(), s.kind))
}

#[test]
fn a_finished_scan_loads_the_devices_it_found() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let vital = device(&app, "Vital");
    assert_eq!(app.engine().device_status(surge), DeviceStatus::Missing);
    assert_eq!(script.borrow().created.len(), 13, "every device once");

    // The app scans the default folders plus the settings' ones.
    app.dispatch(Action::Settings(SettingsAction::RescanPlugins));
    assert_eq!(script.borrow().scans, [Settings::default().plugin_dirs()]);
    let plugin = app.project().find_device(surge).unwrap().2.plugin.clone();
    script.borrow_mut().found = Some(vec![PluginDescriptor {
        format: BundleFormat::Clap,
        id: plugin.id.clone(),
        name: plugin.name.clone(),
        vendor: plugin.vendor.clone(),
        version: plugin.version.clone(),
        kind: PluginKind::Instrument,
        features: vec!["instrument".into()],
        bundle: "/usr/lib/clap/Surge XT.clap".into(),
        sidechain: false,
    }]);
    app.step();
    assert!(
        app.device_events()
            .iter()
            .any(|e| matches!(e, PluginManagerEvent::ScanFinished(_)))
    );
    // Surge XT gets a processor into the slot it has; Vital stays missing.
    let created = script.borrow().created.clone();
    assert_eq!(created.len(), 14, "{created:?}");
    assert_eq!(created.last(), Some(&surge));
    assert_eq!(app.engine().device_status(surge), DeviceStatus::Ok);
    assert_eq!(app.engine().device_status(vital), DeviceStatus::Missing);
    app.step();
    assert!(app.device_events().is_empty(), "events live one frame");
}

/// Undo steps of `app` that are left, counted by undoing them all (and redoing them again).
fn undo_depth(app: &mut App) -> usize {
    let mut n = 0;
    while app.doc().undo_label().is_some() {
        app.dispatch(Action::Edit(EditAction::Undo));
        n += 1;
    }
    for _ in 0..n {
        app.dispatch(Action::Edit(EditAction::Redo));
    }
    n
}

fn snapshot(script: &Rc<RefCell<Script>>, device: DeviceId, bytes: &[u8]) {
    push(
        script,
        PluginManagerEvent::StateSnapshot {
            device,
            state: Arc::from(bytes),
        },
    );
}

#[test]
fn state_snapshots_are_undo_steps_that_follow_each_other_into_one() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let original = state(&app, surge);
    app.dispatch(Action::Edit(EditAction::SetProjectProp(ProjectProp::Name(
        "Night Drive 2".into(),
    ))));
    let depth = undo_depth(&mut app);

    // The plugin's own edit is a step, named after the plugin.
    let blob: Arc<[u8]> = Arc::from(&[1u8, 2, 3][..]);
    snapshot(&script, surge, &blob);
    app.step();
    assert_eq!(state(&app, surge).as_deref(), Some(&blob[..]));
    assert_eq!(app.undo_text().as_deref(), Some("Surge XT: Zustand ändern"));
    assert!(app.doc().is_dirty());

    // More edits right after it amend the same step.
    snapshot(&script, surge, &[1, 2, 4]);
    app.step();
    snapshot(&script, surge, &[1, 2, 5]);
    app.step();
    assert_eq!(state(&app, surge).as_deref(), Some(&[1u8, 2, 5][..]));

    // The same state again, and one for a device that is gone, change nothing.
    let revision = app.doc().revision();
    snapshot(&script, surge, &[1, 2, 5]);
    snapshot(&script, DeviceId(99_999), &blob);
    app.step();
    assert_eq!(app.doc().revision(), revision);
    // (Counting undoes the steps; a state handed back to the plugin starts an echo window.)
    assert_eq!(undo_depth(&mut app), depth + 1, "one step for the burst");

    // Undo reverts the plugin's state first, then the rename.
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(state(&app, surge), original);
    assert_eq!(app.project().name, "Night Drive 2");
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(app.project().name, "Night Drive");
    app.dispatch(Action::Edit(EditAction::Redo));
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(state(&app, surge).as_deref(), Some(&[1u8, 2, 5][..]));
}

#[test]
fn a_snapshot_after_the_window_is_a_step_of_its_own_and_the_echo_of_an_undo_is_no_step() {
    let (mut app, script) = app();
    app.set_snapshot_window(Duration::ZERO);
    let surge = device(&app, "Surge XT");
    let depth = undo_depth(&mut app);
    snapshot(&script, surge, &[1]);
    app.step();
    snapshot(&script, surge, &[2]);
    app.step();
    assert_eq!(undo_depth(&mut app), depth + 2, "the window is over");

    // Undo hands the plugin its old state; the snapshot it answers with (other bytes, same
    // sound) is stored without a step and does not end the redo.
    app.dispatch(Action::Edit(EditAction::Undo));
    snapshot(&script, surge, &[9, 9]);
    app.step();
    assert_eq!(state(&app, surge).as_deref(), Some(&[9u8, 9][..]));
    assert!(app.doc().redo_label().is_some(), "redo survives the echo");
    assert_eq!(undo_depth(&mut app), depth + 1);
}

#[test]
fn the_echo_of_an_undo_changes_no_other_step() {
    let (mut app, script) = app();
    app.set_snapshot_window(Duration::ZERO);
    let surge = device(&app, "Surge XT");
    let original = state(&app, surge);
    snapshot(&script, surge, &[1]);
    app.step();
    snapshot(&script, surge, &[2]);
    app.step();
    // Undo, and the plugin's echo of the state it got.
    app.dispatch(Action::Edit(EditAction::Undo));
    snapshot(&script, surge, &[9, 9]);
    app.step();
    assert_eq!(state(&app, surge).as_deref(), Some(&[9u8, 9][..]));
    // Redo still restores the later state, and undo the earlier ones: the echo stays where it
    // came.
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(state(&app, surge).as_deref(), Some(&[2u8][..]), "redo");
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(state(&app, surge).as_deref(), Some(&[9u8, 9][..]));
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(state(&app, surge), original, "undo to the start");
}

#[test]
fn saving_a_devices_baseline_changes_no_other_step() {
    let (mut app, script) = app();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "komponist-app-device-events-baseline-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Night Drive.komponist");
    std::fs::write(&file, persist::to_json(app.project()).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.step();
    app.set_snapshot_window(Duration::ZERO);
    let surge = device(&app, "Surge XT");
    assert!(state(&app, surge).is_none());
    // The plugin comes up; what it holds then is its baseline, kept aside.
    push(
        &script,
        PluginManagerEvent::StatusChanged {
            device: surge,
            status: DeviceStatus::Ok,
        },
    );
    app.step();
    let baseline: &[u8] = b"baseline";
    snapshot(&script, surge, baseline);
    app.step();
    assert!(state(&app, surge).is_none(), "the baseline is no edit");
    // An edit, undone: the device holds its baseline again.
    snapshot(&script, surge, &[1]);
    app.step();
    app.dispatch(Action::Edit(EditAction::Undo));
    assert!(state(&app, surge).is_none());

    // Saving takes the baseline into the document and the file, outside the history …
    script.borrow_mut().snapshot = vec![(surge, Arc::from(baseline))];
    app.dispatch(Action::Project(ProjectAction::Save));
    assert_eq!(state(&app, surge).as_deref(), Some(baseline));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let (_, _, d) = saved.find_device(surge).unwrap();
    assert_eq!(d.state.as_deref(), Some(baseline));
    // … and the undone edit is still there to redo.
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(state(&app, surge).as_deref(), Some(&[1u8][..]), "redo");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_snapshot_during_another_gesture_waits_for_its_end() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let track = app.project().tracks[0].id;
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Lautstärke".into(),
    }));
    app.dispatch(Action::Edit(EditAction::SetTrackProp {
        tracks: vec![track],
        prop: komponist_model::edits::TrackProp::Channel(
            komponist_model::edits::ChannelProp::VolumeDb(-6.0),
        ),
    }));
    snapshot(&script, surge, &[7]);
    app.step();
    assert_ne!(state(&app, surge).as_deref(), Some(&[7u8][..]), "waits");
    app.dispatch(Action::Edit(EditAction::EndGesture));
    app.step();
    assert_eq!(state(&app, surge).as_deref(), Some(&[7u8][..]));
    // The fader and the plugin are two steps.
    assert_eq!(app.undo_text().as_deref(), Some("Surge XT: Zustand ändern"));
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(app.undo_text().as_deref(), Some("Lautstärke"));
}

#[test]
fn a_latency_change_recompiles_the_program() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let pdc = |app: &App, track: TrackId| {
        let program = app.engine().program().unwrap();
        program
            .tracks
            .iter()
            .find(|t| t.id == track)
            .unwrap()
            .pdc_delay
    };
    let pad = app.project().tracks[3].id;
    let keys = app.project().tracks[4].id;
    assert_eq!(pdc(&app, keys), 0);
    script.borrow_mut().latencies.insert(surge, 100);
    push(
        &script,
        PluginManagerEvent::LatencyChanged {
            device: surge,
            latency: 100,
        },
    );
    let created = script.borrow().created.len();
    app.step();
    assert_eq!(pdc(&app, keys), 100);
    assert_eq!(pdc(&app, pad), 0);
    assert_eq!(script.borrow().created.len(), created, "slots stay");
    assert!(script.borrow().released.is_empty());
}

#[test]
fn crashes_and_warnings_go_to_the_status_bar() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let mut expect = |event: PluginManagerEvent, text: Option<&str>, kind: StatusKind| {
        app.ui_mut().status = None;
        push(&script, event.clone());
        app.step();
        assert_eq!(app.device_events(), std::slice::from_ref(&event));
        assert_eq!(
            status_text(&app),
            text.map(|t| (t.to_owned(), kind)),
            "{event:?}"
        );
    };
    let crashed = |reason| PluginManagerEvent::StatusChanged {
        device: surge,
        status: DeviceStatus::Crashed(reason),
    };
    let log = |device, level, message: &str| PluginManagerEvent::Log {
        device,
        level,
        message: message.to_owned(),
    };
    expect(
        crashed(FailReason::Exited {
            code: None,
            signal: Some(11),
        }),
        Some("Surge XT abgestürzt · die Spur läuft ohne dieses Gerät weiter"),
        StatusKind::Error,
    );
    expect(
        crashed(FailReason::Unresponsive),
        Some("Surge XT hängt · die Spur läuft ohne dieses Gerät weiter"),
        StatusKind::Error,
    );
    expect(
        crashed(FailReason::LoadFailed("no such file".into())),
        Some("Surge XT lässt sich nicht laden"),
        StatusKind::Error,
    );
    expect(
        log(
            Some(surge),
            LogLevel::Warning,
            &format!("{STATE_LOG_PREFIX} version 2 expected"),
        ),
        Some("Surge XT: Zustand konnte nicht geladen werden"),
        StatusKind::Error,
    );
    expect(
        log(
            Some(surge),
            LogLevel::Warning,
            "RT priority: SCHED_FIFO 98 refused (EPERM); the audio thread runs without realtime \
             priority and may miss deadlines under load",
        ),
        Some("Surge XT läuft ohne Echtzeit-Priorität"),
        StatusKind::Info,
    );
    expect(
        log(Some(surge), LogLevel::Warning, "careful"),
        Some("Surge XT: careful"),
        StatusKind::Info,
    );
    expect(
        log(
            None,
            LogLevel::Warning,
            "plugin scan cache not written: EROFS",
        ),
        Some("Plugin-Scan: plugin scan cache not written: EROFS"),
        StatusKind::Info,
    );
    // Errors accompany a failure the status shows; info lines stay out of the status bar.
    expect(
        log(Some(surge), LogLevel::Error, "control channel: closed"),
        None,
        StatusKind::Info,
    );
    expect(
        log(Some(surge), LogLevel::Info, "RT priority: SCHED_FIFO 98"),
        None,
        StatusKind::Info,
    );
    expect(
        PluginManagerEvent::StatusChanged {
            device: surge,
            status: DeviceStatus::Ok,
        },
        None,
        StatusKind::Info,
    );
}

#[test]
fn plugin_actions_reach_the_host() {
    let (mut app, script) = app();
    // Make Surge XT installed (it then has a GUI in the fake).
    let surge = device(&app, "Surge XT");
    let plugin = app.project().find_device(surge).unwrap().2.plugin.clone();
    script.borrow_mut().found = Some(vec![PluginDescriptor {
        format: BundleFormat::Clap,
        id: plugin.id,
        name: plugin.name,
        vendor: plugin.vendor,
        version: plugin.version,
        kind: PluginKind::Instrument,
        features: Vec::new(),
        bundle: "/usr/lib/clap/Surge XT.clap".into(),
        sidechain: false,
    }]);
    app.step();
    assert_eq!(app.engine().device_status(surge), DeviceStatus::Ok);
    let hexa = device(&app, "Hexa");

    // Parameters: bridged devices through the host, hexa through the engine.
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: surge,
        id: 7,
        value: 0.25,
    }));
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: hexa,
        id: 0,
        value: 0.5,
    }));
    assert_eq!(script.borrow().params, [(surge, 7, 0.25)]);

    // Windows: the plugin's GUI if it has one; closing it closes the GUI (state snapshot).
    app.dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    app.dispatch(Action::Plugin(PluginAction::OpenWindow(hexa)));
    let views: Vec<(DeviceId, PluginView)> = app
        .ui()
        .plugin_windows
        .iter()
        .map(|w| (w.device, w.view))
        .collect();
    assert_eq!(
        views,
        [(surge, PluginView::Gui), (hexa, PluginView::Params)]
    );
    app.dispatch(Action::Plugin(PluginAction::CloseWindow(surge)));
    app.dispatch(Action::Plugin(PluginAction::CloseWindow(hexa)));
    assert!(app.ui().plugin_windows.is_empty());
    assert_eq!(script.borrow().closed_guis, [surge]);

    // "Neu laden": a new processor from the host.
    let created = script.borrow().created.len();
    app.dispatch(Action::Plugin(PluginAction::Reload(surge)));
    assert_eq!(script.borrow().created.len(), created + 1);
}

#[test]
fn saving_snapshots_the_states_and_exit_shuts_the_host_down() {
    let (mut app, script) = app();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "komponist-app-device-events-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Night Drive.komponist");
    std::fs::write(&file, persist::to_json(app.project()).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.step();
    let surge = device(&app, "Surge XT");
    let blob: Arc<[u8]> = Arc::from(&b"surge state"[..]);
    script.borrow_mut().snapshot = vec![(surge, Arc::clone(&blob))];

    app.dispatch(Action::Project(ProjectAction::Save));
    assert_eq!(script.borrow().snapshot_calls, 1);
    assert!(!app.doc().is_dirty());
    assert_eq!(state(&app, surge).as_deref(), Some(&blob[..]));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let (_, _, d) = saved.find_device(surge).unwrap();
    assert_eq!(d.state.as_deref(), Some(&blob[..]));

    assert!(!script.borrow().shut_down);
    app.shutdown();
    assert!(script.borrow().shut_down);
    let _ = std::fs::remove_dir_all(&dir);
}

/// WP-65 follow-up: workers without realtime priority say so in the status bar, in the same kind
/// as the bridge's hint, and the notice is shown once (a later frame leaves the status alone).
#[test]
fn workers_without_realtime_priority_reach_the_status_bar() {
    let (mut app, _script) = app();
    // Whatever the null stream's own worker start left behind is not under test.
    app.ui_mut().status = None;
    app.engine().take_worker_notice();
    app.report_workers_without_realtime();
    assert_eq!(
        status_text(&app),
        Some((
            "Audio-Threads laufen ohne Echtzeit-Priorität".to_owned(),
            StatusKind::Info
        ))
    );
    app.ui_mut().status = None;
    app.step();
    assert_eq!(status_text(&app), None);
}

/// #79: workers that could not join the audio workgroup (macOS) say so in the status bar, once.
#[test]
fn workers_without_workgroup_reach_the_status_bar() {
    let (mut app, _script) = app();
    app.ui_mut().status = None;
    app.engine().take_workgroup_notice();
    app.report_workers_without_workgroup();
    assert_eq!(
        status_text(&app),
        Some((
            "Audio-Threads konnten der Audio-Workgroup nicht beitreten".to_owned(),
            StatusKind::Info
        ))
    );
    app.ui_mut().status = None;
    app.step();
    assert_eq!(status_text(&app), None);
}
