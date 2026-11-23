//! How the app handles its device host (WP-46), with a scripted host and no processes: state
//! snapshots go into the document as coalesced undo steps, latency changes recompile, a finished
//! scan loads the devices it found, crashes and warnings reach the status bar, plugin actions,
//! saving and exit reach the host.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use komponist_app::actions::{EditAction, PluginAction, ProjectAction, SettingsAction};
use komponist_app::device_host::DeviceHost;
use komponist_app::engine_sync::EngineSync;
use komponist_app::settings::Settings;
use komponist_app::ui_state::{PluginView, StatusKind};
use komponist_app::{Action, App};
use komponist_engine::processor::{ParameterEdit, ParameterEditKind};
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_ipc::{
    LogLevel, ParamInfo, PluginDescriptor, PluginFormat as BundleFormat, PluginKind,
};
use komponist_model::edits::ProjectProp;
use komponist_model::fixtures::demo_project;
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginRef, TrackId, persist};
use komponist_plugins::manager::STATE_LOG_PREFIX;
use komponist_plugins::{
    Catalog, DeviceStatus, FailReason, PluginDirs, PluginError, PluginManagerEvent, ProcessorSetup,
    SaveSnapshots, ScanReport,
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
    snapshot_unavailable: Vec<DeviceId>,
    snapshot_blocked: Vec<DeviceId>,
    // Observed:
    created: Vec<DeviceId>,
    released: Vec<DeviceId>,
    scans: Vec<PluginDirs>,
    params: Vec<(DeviceId, u32, f64)>,
    gui_edits: Vec<(DeviceId, u32, f64)>,
    gui_previews: Vec<(DeviceId, u32, f64)>,
    closed_guis: Vec<DeviceId>,
    snapshot_calls: usize,
    shut_down: bool,
}

/// A synthetic GUI with one parameter and an RT-safe handoff to its processor.
#[derive(Clone)]
struct GuiEditSource {
    device: DeviceId,
    info: ParamInfo,
    pending: Arc<AtomicU64>,
    applied: Arc<AtomicU64>,
}

const GUI_TOUCH_BEGIN: u64 = u64::MAX;
const GUI_TOUCH_END: u64 = u64::MAX - 1;

struct GuiProcessor {
    id: u32,
    pending: Arc<AtomicU64>,
    applied: Arc<AtomicU64>,
}

impl Processor for GuiProcessor {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        _: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        for event in events {
            if let EventKind::Param { id, value } = event.kind
                && id == self.id
            {
                self.applied.store(value.to_bits(), Ordering::Relaxed);
            }
        }
        io.output[0].fill(0.0);
        io.output[1].fill(0.0);
        ProcessStatus::Continue
    }

    fn pop_parameter_edit(&mut self) -> Option<ParameterEdit> {
        let bits = self.pending.swap(f64::NAN.to_bits(), Ordering::Relaxed);
        let kind = match bits {
            GUI_TOUCH_BEGIN | GUI_TOUCH_END => ParameterEditKind::Gesture {
                id: self.id,
                begin: bits == GUI_TOUCH_BEGIN,
            },
            _ => {
                let value = f64::from_bits(bits);
                if !value.is_finite() {
                    return None;
                }
                self.applied.store(bits, Ordering::Relaxed);
                ParameterEditKind::Value { id: self.id, value }
            }
        };
        Some(ParameterEdit { offset: 0, kind })
    }

    fn latency(&self) -> u32 {
        0
    }
    fn reset(&mut self) {}
}

/// A scripted host, with a processor only for the optional synthetic GUI.
struct FakeHost {
    script: Rc<RefCell<Script>>,
    catalog: Catalog,
    statuses: HashMap<DeviceId, DeviceStatus>,
    gui: Option<GuiEditSource>,
}

impl DeviceHost for FakeHost {
    fn create_processor(
        &mut self,
        device: &Device,
        _setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        self.script.borrow_mut().created.push(device.id);
        if let Some(gui) = self.gui.as_ref().filter(|gui| gui.device == device.id) {
            self.statuses.insert(device.id, DeviceStatus::Ok);
            return Some(Box::new(GuiProcessor {
                id: gui.info.id,
                pending: Arc::clone(&gui.pending),
                applied: Arc::clone(&gui.applied),
            }));
        }
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

    fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        let gui = self.gui.as_ref().filter(|gui| gui.device == device)?;
        Some(std::slice::from_ref(&gui.info))
    }

    fn param_base(&self, device: DeviceId, id: u32) -> Option<f64> {
        let gui = self
            .gui
            .as_ref()
            .filter(|gui| gui.device == device && gui.info.id == id)?;
        Some(
            self.script
                .borrow()
                .gui_edits
                .last()
                .map_or(gui.info.default, |edit| edit.2),
        )
    }

    fn edited_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        if !self.gui.as_ref().is_some_and(|gui| gui.device == device) {
            return false;
        }
        self.script.borrow_mut().gui_edits.push((device, id, value));
        true
    }

    fn preview_param(&mut self, device: DeviceId, id: u32, value: f64) {
        self.script
            .borrow_mut()
            .gui_previews
            .push((device, id, value));
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

    fn snapshot_for_save(&mut self, timeout: Duration) -> SaveSnapshots {
        let captured = self.snapshot_states(timeout);
        let script = self.script.borrow();
        SaveSnapshots {
            captured,
            unavailable: script.snapshot_unavailable.clone(),
            blocked: script.snapshot_blocked.clone(),
        }
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
        gui: None,
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

#[test]
fn saving_reports_cached_and_missing_plugin_states_without_losing_fresh_states() {
    let (mut app, script) = app();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Night Drive.komponist");
    let surge = device(&app, "Surge XT");
    let vital = device(&app, "Vital");
    let fresh = app.project().tracks[0].channel.devices[1].id;
    let cached = Arc::from(&b"previous surge state"[..]);
    let captured = Arc::from(&b"fresh effect state"[..]);
    let project =
        komponist_model::edits::set_device_state(app.project(), surge, Some(cached)).unwrap();
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.step();
    {
        let mut script = script.borrow_mut();
        script.snapshot = vec![(fresh, Arc::clone(&captured))];
        script.snapshot_unavailable = vec![surge, vital];
    }

    app.dispatch(Action::Project(ProjectAction::Save));
    let (status, kind) = status_text(&app).unwrap();
    assert_eq!(kind, StatusKind::Error);
    assert!(
        status.contains("Vorheriger Plug-in-Zustand gespeichert: Surge XT"),
        "{status}"
    );
    assert!(
        status.starts_with("Kein Plug-in-Zustand verfügbar: Vital"),
        "{status}"
    );
    let saved = komponist_app::project_io::load_project(&file).unwrap();
    assert_eq!(
        saved.find_device(surge).unwrap().2.state.as_deref(),
        Some(&b"previous surge state"[..])
    );
    assert_eq!(saved.find_device(vital).unwrap().2.state, None);
    assert_eq!(
        saved.find_device(fresh).unwrap().2.state.as_ref(),
        Some(&captured)
    );

    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    let (status, kind) = status_text(&app).unwrap();
    assert_eq!(kind, StatusKind::Error);
    assert!(
        status.contains("Vorheriger Plug-in-Zustand gespeichert: Surge XT"),
        "{status}"
    );
    assert!(
        status.starts_with("Kein Plug-in-Zustand verfügbar: Vital"),
        "{status}"
    );

    let next = Arc::from(&b"new effect state"[..]);
    {
        let mut script = script.borrow_mut();
        script.snapshot = vec![(fresh, Arc::clone(&next))];
        script.snapshot_unavailable.clear();
        script.snapshot_blocked = vec![surge];
    }
    app.dispatch(Action::Project(ProjectAction::Save));
    let (status, kind) = status_text(&app).unwrap();
    assert_eq!(kind, StatusKind::Error);
    assert!(status.starts_with("Speichern fehlgeschlagen"), "{status}");
    assert!(status.contains("Surge XT"), "{status}");
    assert_eq!(state(&app, fresh), Some(next));
    assert_eq!(
        komponist_app::project_io::load_project(&file).unwrap(),
        saved
    );
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

/// #210: an engine whose `process` panicked is replaced by a new one; every device gets a fresh
/// processor from the document (none is released), and the status bar says so as an error.
#[test]
fn a_panicked_engine_is_rebuilt_and_reported() {
    let (mut app, script) = app();
    app.ui_mut().status = None;
    let lead = app.project().tracks[2].channel.devices[0].id;
    let (created, released) = {
        let script = script.borrow();
        (script.created.len(), script.released.len())
    };
    let engine = app.engine_mut();
    // Single-threaded, so the panic is on this thread and outside `assert_no_alloc`.
    assert_eq!(engine.set_workers(Some(0)).unwrap().unwrap().threads, 0);
    let slot = engine.program().unwrap().slots.slot(lead).unwrap();
    engine.hold_engine().unwrap();
    engine.send(komponist_engine::Command::InsertProcessor {
        slot,
        processor: Box::new(komponist_engine::testing::TestPanic),
    });
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.run_held(256, &[], 0);
    }));
    assert!(caught.is_err(), "the processor ran");
    engine.release_engine();
    app.step();
    assert_eq!(
        status_text(&app),
        Some((
            "Audio-Engine abgestürzt · aus dem Projekt neu aufgebaut, die Wiedergabe ist gestoppt"
                .to_owned(),
            StatusKind::Error
        ))
    );
    let script = script.borrow();
    assert_eq!(script.created.len(), 2 * created, "every device again");
    assert_eq!(script.released.len(), released);
    assert!(app.engine().program().unwrap().slots.slot(lead).is_some());
}

/// #210, owner 2026-11-23: a MIDI take running when the engine panics ends at the crash like an
/// interruption and keeps what was recorded so far, also the notes still queued in the old
/// engine; the new engine stays stopped at the old playhead.
#[test]
fn a_take_running_when_the_engine_panics_is_kept() {
    use komponist_app::{EditAction, TransportAction};
    use komponist_engine::{MidiInEvent, MidiPort, TransportState};
    use komponist_i18n::Lang;
    use komponist_model::ClipContent;

    // 120 bpm at 48 kHz: 25 frames per tick.
    let frame = |tick: u32| tick * 25;
    let mut app = App::headless(komponist_model::fixtures::empty_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.dispatch(Action::Transport(TransportAction::ToggleCountIn));
    let track = app.project().tracks[0].id;
    let instrument = app.project().tracks[0].channel.devices[0].id;
    // Single-threaded, so the panic is on this thread and outside `assert_no_alloc`.
    assert_eq!(
        app.engine_mut()
            .set_workers(Some(0))
            .unwrap()
            .unwrap()
            .threads,
        0
    );
    app.engine_mut().hold_engine().unwrap();
    app.dispatch(Action::Transport(TransportAction::ToggleRecord));
    assert!(app.engine_mut().run_held(frame(40) as usize, &[], 0));
    app.step();
    assert!(app.recording().is_some());
    let note = |tick, data| MidiInEvent {
        frame: frame(tick),
        port: MidiPort(1),
        data,
    };
    let c4 = [note(60, [0x90, 60, 100]), note(540, [0x80, 60, 0])];
    assert!(app.engine_mut().run_held(frame(1000) as usize, &c4, 0));
    app.step();
    // E4 starts and is still held; the app has not seen it yet when the engine panics.
    let e4 = [note(10, [0x90, 64, 80])];
    assert!(app.engine_mut().run_held(frame(100) as usize, &e4, 0));
    let at = app.engine().playhead();
    let slot = app
        .engine()
        .program()
        .unwrap()
        .slots
        .slot(instrument)
        .unwrap();
    let engine = app.engine_mut();
    engine.send(komponist_engine::Command::InsertProcessor {
        slot,
        processor: Box::new(komponist_engine::testing::TestPanic),
    });
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.run_held(256, &[], 0);
    }));
    assert!(caught.is_err(), "the processor ran");
    engine.release_engine();
    app.step();

    assert!(app.recording().is_none(), "the take ended");
    let clips = &app.project().track(track).unwrap().clips;
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].name, Lang::De.record_take_incomplete(1));
    let ClipContent::Midi(midi) = &clips[0].content else {
        panic!("a MIDI clip")
    };
    let notes: Vec<_> = midi.takes[0]
        .notes
        .iter()
        .map(|n| (n.start.0, n.pitch))
        .collect();
    assert_eq!(notes, [(100, 60), (1050, 64)]);
    // E4 was held: it ends at the crash.
    let e4 = &midi.takes[0].notes[1];
    assert_eq!(clips[0].start + e4.start + e4.length, at);
    assert_eq!(
        status_text(&app),
        Some((
            Lang::De.status_engine_rebuilt_take().to_owned(),
            StatusKind::Error
        ))
    );
    // The new engine: stopped where the old one was.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.engine().playhead() != at {
        assert!(std::time::Instant::now() < deadline, "not located");
        std::thread::sleep(Duration::from_millis(5));
        app.step();
    }
    assert_eq!(app.transport(), TransportState::Stopped);
    assert!(app.recording().is_none());
}

fn plugin_gui_automation(
    mode: komponist_model::automation::AutomationMode,
) -> (
    App,
    Rc<RefCell<Script>>,
    GuiEditSource,
    komponist_model::automation::LaneId,
) {
    use komponist_ipc::control::param_flags;
    use komponist_model::Tick;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
    use komponist_model::edits::automation::{add_lane, set_lane_mode, set_lane_points};
    use komponist_model::params::ParamId;

    let project = demo_project();
    let (track, device) = project
        .tracks
        .iter()
        .find_map(|track| {
            track
                .channel
                .devices
                .iter()
                .find(|d| d.plugin.name == "Surge XT")
                .map(|device| (track.id, device.id))
        })
        .unwrap();
    let owner = AutomationOwner::Track(track);
    let id = 42;
    let target = AutomationTarget::Param {
        device,
        param: ParamId::plugin(id),
    };
    let (project, lane) = add_lane(&project, owner, target.clone(), "Cutoff".into()).unwrap();
    let project = set_lane_points(
        &project,
        owner,
        lane,
        Points::new(vec![Point::linear(Tick::ZERO, 0.75)]),
    )
    .unwrap();
    let project = set_lane_mode(&project, owner, lane, mode).unwrap();
    let script = Rc::new(RefCell::new(Script::default()));
    let pending = Arc::new(AtomicU64::new(f64::NAN.to_bits()));
    let applied = Arc::new(AtomicU64::new(0.25_f64.to_bits()));
    let gui = GuiEditSource {
        device,
        info: ParamInfo {
            id,
            name: "Cutoff".into(),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.25,
            step_count: 0,
            flags: param_flags::AUTOMATABLE,
        },
        pending,
        applied,
    };
    let host = FakeHost {
        script: Rc::clone(&script),
        catalog: Catalog::builtin(),
        statuses: HashMap::new(),
        gui: Some(gui.clone()),
    };
    let mut app = App::headless(project).with_device_host(Box::new(host));
    app.engine_mut().hold_engine().unwrap();
    app.step();
    (app, script, gui, lane)
}

#[test]
fn plugin_gui_read_override_remains_temporary_and_off_restores_the_old_base() {
    use komponist_app::views::automation::Action as Automation;
    use komponist_model::automation::{AutomationMode, AutomationOwner};

    let (mut app, script, gui, lane) = plugin_gui_automation(AutomationMode::Read);
    let device = gui.device;
    let id = gui.info.id;
    let (channel, _, _) = app.project().find_device(device).unwrap();
    let owner: AutomationOwner = channel.into();
    let GuiEditSource {
        pending, applied, ..
    } = gui;
    let run = |app: &mut App| {
        assert!(app.engine_mut().run_held(256, &[], 0));
        app.step();
    };
    run(&mut app);
    assert_eq!(f64::from_bits(applied.load(Ordering::Relaxed)), 0.75);

    pending.store(0.6_f64.to_bits(), Ordering::Relaxed);
    run(&mut app);
    assert_eq!(script.borrow().gui_previews, [(device, id, 0.6)]);
    assert!(script.borrow().gui_edits.is_empty());
    assert!(app.doc().undo_label().is_none());

    app.dispatch(Action::Automation(Automation::Mode(
        owner,
        lane,
        AutomationMode::Off,
    )));
    app.step();
    run(&mut app);
    assert_eq!(f64::from_bits(applied.load(Ordering::Relaxed)), 0.25);

    pending.store(0.5_f64.to_bits(), Ordering::Relaxed);
    run(&mut app);
    assert_eq!(script.borrow().gui_edits, [(device, id, 0.5)]);
}

#[test]
fn native_macro_learning_observes_a_touch_release_before_the_next_gesture() {
    use komponist_app::TransportAction;
    use komponist_app::views::track_macros::Action as Macro;
    use komponist_model::automation::{AutomationMode, AutomationTarget};
    use komponist_model::params::ParamId;

    let (mut app, script, gui, _) = plugin_gui_automation(AutomationMode::Touch);
    let (komponist_model::ChannelRef::Track(track), _, _) =
        app.project().find_device(gui.device).unwrap()
    else {
        unreachable!()
    };
    let target = AutomationTarget::Param {
        device: gui.device,
        param: ParamId::plugin(gui.info.id),
    };
    let send = |app: &mut App, bits| {
        gui.pending.store(bits, Ordering::Relaxed);
        assert!(app.engine_mut().run_held(256, &[], 0));
    };
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    send(&mut app, GUI_TOUCH_BEGIN);
    send(&mut app, 0.4_f64.to_bits());
    app.step();

    app.dispatch(Action::Macro(Macro::ToggleMapping { track }));
    send(&mut app, 0.53_f64.to_bits());
    app.step();
    assert_eq!(
        app.project().track(track).unwrap().macros.driver(&target),
        None
    );

    // Both the old release and the next complete native gesture reach the same UI drain.
    send(&mut app, GUI_TOUCH_END);
    send(&mut app, GUI_TOUCH_BEGIN);
    send(&mut app, 0.95_f64.to_bits());
    send(&mut app, GUI_TOUCH_END);
    app.step();
    assert_eq!(
        app.project().track(track).unwrap().macros.driver(&target),
        Some(0)
    );
    assert!(script.borrow().gui_edits.is_empty());
    assert!(
        script
            .borrow()
            .gui_previews
            .contains(&(gui.device, gui.info.id, 0.53))
    );

    app.dispatch(Action::Transport(TransportAction::Stop));
    assert!(app.engine_mut().run_held(256, &[], 0));
    app.step();
    let owner = app.project().track(track).unwrap();
    let points = &owner.automation.lane(&target).unwrap().points;
    assert!(
        points
            .as_slice()
            .iter()
            .any(|point| (point.value - 0.53).abs() < 1e-6)
    );
    assert!(points.as_slice().iter().all(|point| point.value < 0.9));
}
