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
use komponist_ipc::{LogLevel, PluginDescriptor, PluginFormat as BundleFormat, PluginKind};
use komponist_model::edits::ProjectProp;
use komponist_model::fixtures::demo_project;
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginRef, TrackId, persist};
use komponist_plugin_api::clap_presets::{
    ClapPreset, ClapPresetLocation, ClapPresetSource, DevicePreset,
};
use komponist_plugin_api::params::ParamInfo;
use komponist_plugins::manager::STATE_LOG_PREFIX;
use komponist_plugins::{
    Catalog, DeviceSnapshots, DeviceStatus, FailReason, PluginDirs, PluginError,
    PluginManagerEvent, ProcessorSetup, SaveSnapshots, ScanReport,
};
use komponist_processor::processor::{ParameterEdit, ParameterEditKind};
use komponist_processor::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
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
    snapshot: DeviceSnapshots,
    snapshot_unavailable: Vec<DeviceId>,
    snapshot_blocked: Vec<DeviceId>,
    events_during_capture: Vec<PluginManagerEvent>,
    retiring_snapshots: SaveSnapshots,
    retiring_snapshot_pending: Option<DeviceId>,
    // Observed:
    created: Vec<DeviceId>,
    released: Vec<DeviceId>,
    scans: Vec<PluginDirs>,
    params: Vec<(DeviceId, u32, f64)>,
    gui_edits: Vec<(DeviceId, u32, f64)>,
    gui_previews: Vec<(DeviceId, u32, f64)>,
    closed_guis: Vec<DeviceId>,
    snapshot_calls: usize,
    presets: Vec<(DeviceId, ClapPresetSource)>,
    restores: Vec<(DeviceId, Arc<[u8]>)>,
    restored_bases: Vec<(DeviceId, u32, f64)>,
    scripted_bases: HashMap<(DeviceId, u32), f64>,
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
    clap_presets: Vec<ClapPreset>,
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

    fn clap_presets(&self, device: &Device) -> &[ClapPreset] {
        if self
            .clap_presets
            .first()
            .is_some_and(|preset| preset.plugin_id == device.plugin.id)
        {
            &self.clap_presets
        } else {
            &[]
        }
    }

    fn load_clap_preset(&mut self, device: &Device, source: &ClapPresetSource) -> bool {
        self.script
            .borrow_mut()
            .presets
            .push((device.id, source.clone()));
        true
    }

    fn set_state(&mut self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        self.script
            .borrow_mut()
            .restores
            .push((device, Arc::clone(state)));
        true
    }

    fn restore_saved_state(
        &mut self,
        device: DeviceId,
        state: &komponist_model::project::SavedPluginState,
    ) -> bool {
        self.script.borrow_mut().restored_bases.extend(
            state
                .parameter_bases
                .iter()
                .map(|(&id, &value)| (device, id, value)),
        );
        self.set_state(device, &state.bytes)
    }

    fn start_scan(&mut self, dirs: &PluginDirs) {
        self.script.borrow_mut().scans.push(dirs.clone());
    }

    fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        let gui = self.gui.as_ref().filter(|gui| gui.device == device)?;
        Some(std::slice::from_ref(&gui.info))
    }

    fn param_base(&self, device: DeviceId, id: u32) -> Option<f64> {
        if let Some(&value) = self.script.borrow().scripted_bases.get(&(device, id)) {
            return Some(value);
        }
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

    fn edit_param_through_render(
        &mut self,
        device: DeviceId,
        id: u32,
        value: f64,
    ) -> Option<std::num::NonZeroU32> {
        // This scripted host records edits without sending them to a running plugin.
        self.edited_param(device, id, value)
            .then_some(std::num::NonZeroU32::MIN)
    }

    fn preview_param(&mut self, device: DeviceId, id: u32, value: f64) {
        self.script
            .borrow_mut()
            .gui_previews
            .push((device, id, value));
    }

    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        // hexa goes through the engine.
        if self.is_demo_instrument(device) {
            return false;
        }
        self.script.borrow_mut().params.push((device, id, value));
        true
    }

    fn has_gui(&self, device: DeviceId) -> bool {
        !self.is_demo_instrument(device) && self.status(device) == DeviceStatus::Ok
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

    fn snapshot_states(&mut self, _timeout: Duration) -> DeviceSnapshots {
        let mut script = self.script.borrow_mut();
        script.snapshot_calls += 1;
        script.snapshot.clone()
    }

    fn snapshot_for_save(&mut self, timeout: Duration) -> SaveSnapshots {
        let captured = self.snapshot_states(timeout);
        let mut script = self.script.borrow_mut();
        let events = std::mem::take(&mut script.events_during_capture);
        script.events.extend(events);
        SaveSnapshots {
            captured,
            unavailable: script.snapshot_unavailable.clone(),
            blocked: script.snapshot_blocked.clone(),
        }
    }

    fn snapshot_available_states(
        &mut self,
        _devices: &[DeviceId],
        _timeout: Duration,
    ) -> SaveSnapshots {
        std::mem::take(&mut self.script.borrow_mut().retiring_snapshots)
    }

    fn schedule_retiring_snapshot(&mut self, device: DeviceId) {
        self.script.borrow_mut().retiring_snapshot_pending = Some(device);
    }

    fn retiring_snapshot_pending(&self, device: DeviceId) -> bool {
        self.script.borrow().retiring_snapshot_pending == Some(device)
    }

    fn shutdown(&mut self) {
        self.script.borrow_mut().shut_down = true;
    }
}

impl FakeHost {
    /// Whether `device` is the demo's hexa (ids of the demo: Bass 9, Lead 94).
    fn is_demo_instrument(&self, device: DeviceId) -> bool {
        demo_project()
            .tracks
            .iter()
            .flat_map(|t| &t.channel.devices)
            .any(|d| d.id == device && d.plugin.name == "Hexa")
    }
}

/// The demo on the null backend with a [`FakeHost`] that knows only hexa.
fn app() -> (App, Rc<RefCell<Script>>) {
    app_with_presets(Vec::new())
}

fn app_with_presets(clap_presets: Vec<ClapPreset>) -> (App, Rc<RefCell<Script>>) {
    let script = Rc::new(RefCell::new(Script::default()));
    let mut catalog = Catalog::default();
    if let Some(preset) = clap_presets.first() {
        catalog.set_scanned(&[super::fake_host::descriptor(
            &preset.plugin_id,
            "Surge XT",
            PluginKind::Instrument,
            &["instrument"],
        )]);
    }
    let host = FakeHost {
        script: Rc::clone(&script),
        catalog,
        statuses: HashMap::new(),
        gui: None,
        clap_presets,
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
        localized: None,
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

#[test]
fn a_missing_bundle_keeps_running_until_its_state_can_be_captured() {
    for (unchanged, capture_succeeds) in [(true, true), (false, true), (true, false)] {
        check_deferred_retirement(unchanged, capture_succeeds);
    }
}

fn check_deferred_retirement(unchanged: bool, capture_succeeds: bool) {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let plugin = app.project().find_device(surge).unwrap().2.plugin.clone();
    script.borrow_mut().found = Some(vec![super::fake_host::descriptor(
        &plugin.id,
        &plugin.name,
        PluginKind::Instrument,
        &["instrument"],
    )]);
    app.step();
    assert_eq!(app.engine().device_status(surge), DeviceStatus::Ok);
    push(
        &script,
        PluginManagerEvent::StatusChanged {
            device: surge,
            status: DeviceStatus::Ok,
        },
    );
    app.step();
    snapshot(&script, surge, &[8]);
    app.step();
    assert!(
        state(&app, surge).is_none(),
        "the startup state is only the baseline"
    );
    let created = script.borrow().created.len();

    script.borrow_mut().found = Some(Vec::new());
    script.borrow_mut().retiring_snapshots = if unchanged {
        SaveSnapshots {
            unavailable: vec![surge],
            ..Default::default()
        }
    } else {
        SaveSnapshots {
            blocked: vec![surge],
            ..Default::default()
        }
    };
    app.step();
    app.step();
    assert_eq!(app.engine().device_status(surge), DeviceStatus::Ok);
    assert_eq!(script.borrow().created.len(), created);
    assert_eq!(script.borrow().retiring_snapshot_pending, Some(surge));

    let captured = [if unchanged { 8 } else { 51 }];
    if capture_succeeds {
        snapshot(&script, surge, &captured);
    }
    script.borrow_mut().retiring_snapshot_pending = None;
    app.step();
    assert_eq!(app.engine().device_status(surge), DeviceStatus::Missing);
    assert_eq!(script.borrow().created.last(), Some(&surge));
    assert_eq!(
        state(&app, surge),
        capture_succeeds.then(|| Arc::<[u8]>::from(captured))
    );
    assert_eq!(undo_depth(&mut app), usize::from(!unchanged));
    assert!(
        matches!(&app.ui().dialog, Some(komponist_app::ui_state::Dialog::MissingPlugins(plugins))
            if plugins.iter().any(|plugin| plugin.device == surge))
    );
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
            state: Arc::<[u8]>::from(bytes).into(),
        },
    );
}

#[test]
fn discovered_clap_preset_menu_loads_a_localized_preset_in_one_undo_step() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;
    use komponist_app::actions::{SelectMode, SelectionChange, UiAction};
    use komponist_app::ui_state::DockTab;
    use komponist_i18n::Lang;
    use std::path::PathBuf;

    let demo = demo_project();
    let (track, synth) = demo
        .tracks
        .iter()
        .find_map(|track| {
            track
                .channel
                .devices
                .iter()
                .find(|d| d.plugin.name == "Surge XT")
                .map(|d| (track.id, d))
        })
        .unwrap();
    let source = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(vec![0xc9, 1]),
    };
    let preset = ClapPreset {
        plugin_id: synth.plugin.id.clone(),
        source: source.clone(),
        name: "Warm Amp".into(),
        localized_names: [("de".into(), "Warmer Verstärker".into())].into(),
        features: Vec::new(),
        factory: true,
    };

    for (lang, preset_label, earlier_presets) in [
        (Lang::De, "Warmer Verstärker", 0_u8),
        (Lang::En, "Warm Amp", 48),
    ] {
        let mut presets = (0..earlier_presets)
            .map(|index| ClapPreset {
                name: format!("Other preset {index}"),
                localized_names: Default::default(),
                source: ClapPresetSource {
                    location: ClapPresetLocation::Plugin,
                    load_key: Some(vec![index]),
                },
                ..preset.clone()
            })
            .collect::<Vec<_>>();
        presets.push(preset.clone());
        let (app, script) = app_with_presets(presets);
        let mut app = app.with_lang(lang);
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![track],
            mode: SelectMode::Replace,
        })));
        app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
        let original = state(&app, synth.id);
        let mut harness = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        harness.run_steps(3);

        let card_label = lang.devices_card("Surge XT");
        harness
            .get_by_role_and_label(Role::Group, card_label.as_str())
            .get_by_role_and_label(Role::Button, lang.devices_preset())
            .click();
        harness.run_steps(3);
        if earlier_presets > 0 {
            for _ in 0..=earlier_presets {
                harness.key_press(egui::Key::ArrowDown);
                harness.step();
            }
            harness.run_steps(3);
        }
        if let Some(dir) = std::env::var_os("KOMPONIST_CLAP_PRESET_PNG_DIR") {
            harness
                .render()
                .unwrap()
                .save(PathBuf::from(dir).join(format!("clap-preset-menu-{}.png", lang.code())))
                .unwrap();
        }
        let row = harness.get_by_role_and_label(Role::MenuItem, preset_label);
        assert!((0.0..1080.0).contains(&row.rect().center().y));
        row.click();
        harness.run_steps(3);
        assert_eq!(script.borrow().presets, vec![(synth.id, source.clone())]);
        assert_eq!(state(harness.state(), synth.id), original);

        let previous_state: Arc<[u8]> = Arc::from([12_u8]);
        let loaded_state: Arc<[u8]> = Arc::from([34_u8]);
        push(
            &script,
            PluginManagerEvent::ClapPresetLoaded {
                device: synth.id,
                source: Some(source.clone()),
                previous_state: Arc::clone(&previous_state),
                state: Arc::clone(&loaded_state),
            },
        );
        let app = harness.state_mut();
        app.step();
        assert_eq!(state(app, synth.id), Some(Arc::clone(&loaded_state)));
        let selection = |app: &App| {
            app.project()
                .find_device(synth.id)
                .unwrap()
                .2
                .preset
                .clone()
        };
        assert_eq!(selection(app), Some(DevicePreset::loaded(source.clone())));
        harness.run_steps(2);
        harness
            .get_by_role_and_label(Role::Group, card_label.as_str())
            .get_by_role_and_label(Role::Button, &lang.devices_preset_name(preset_label));
        let app = harness.state_mut();
        assert!(
            script.borrow().restores.is_empty(),
            "the live instance already loaded it"
        );
        app.dispatch(Action::Edit(EditAction::Undo));
        assert_eq!(state(app, synth.id), original);
        assert_eq!(selection(app), None);
        let expected = original.unwrap_or(previous_state);
        assert_eq!(script.borrow().restores.last(), Some(&(synth.id, expected)));
        app.dispatch(Action::Edit(EditAction::Redo));
        assert_eq!(state(app, synth.id), Some(Arc::clone(&loaded_state)));
        assert_eq!(selection(app), Some(DevicePreset::loaded(source.clone())));
        assert_eq!(
            script.borrow().restores.last(),
            Some(&(synth.id, loaded_state))
        );
    }
}

#[test]
fn preset_undo_preserves_a_preceding_live_edit_without_a_snapshot() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    let baseline = [11_u8];
    snapshot(&script, device, &baseline);
    app.step();
    let edited: Arc<[u8]> = Arc::from([22_u8]);
    let preset: Arc<[u8]> = Arc::from([33_u8]);
    push(
        &script,
        PluginManagerEvent::ClapPresetLoaded {
            device,
            source: None,
            previous_state: Arc::clone(&edited),
            state: Arc::clone(&preset),
        },
    );
    app.step();
    assert_eq!(state(&app, device), Some(preset));
    assert!(script.borrow().restores.is_empty());
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(state(&app, device), Some(Arc::clone(&edited)));
    assert_eq!(script.borrow().restores.last(), Some(&(device, edited)));
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(state(&app, device).as_deref(), Some(baseline.as_slice()));
}

#[test]
fn the_preset_a_plugin_starts_in_is_no_step_and_never_replaces_a_recorded_one() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let selection = |app: &App| app.project().find_device(surge).unwrap().2.preset.clone();
    let source = |key: &[u8]| ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(key.to_vec()),
    };
    let start = |key: &[u8]| PluginManagerEvent::ClapPresetReported {
        device: surge,
        source: source(key),
        startup: true,
    };
    assert_eq!(selection(&app), None);
    app.dispatch(Action::Edit(EditAction::SetProjectProp(ProjectProp::Name(
        "Renamed".into(),
    ))));
    let undo = app.doc().undo_label().map(str::to_owned);

    // Without a recorded preset, the start preset becomes the selection outside the history,
    // also in the steps before: undoing an unrelated edit keeps it.
    push(&script, start(b"Init"));
    app.step();
    assert_eq!(selection(&app), Some(DevicePreset::loaded(source(b"Init"))));
    assert_eq!(
        app.doc().undo_label().map(str::to_owned),
        undo,
        "no undo step"
    );
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(selection(&app), Some(DevicePreset::loaded(source(b"Init"))));
    app.dispatch(Action::Edit(EditAction::Redo));

    // A recorded preset and its mark stay: a restart reports the preset it starts in again.
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: surge,
        id: 3,
        value: 0.5,
    }));
    let modified = DevicePreset {
        source: source(b"Init"),
        modified: true,
    };
    assert_eq!(selection(&app), Some(modified.clone()));
    push(&script, start(b"Init"));
    push(&script, start(b"Other"));
    app.step();
    assert_eq!(selection(&app), Some(modified));
}

#[test]
fn a_reported_preset_and_its_snapshot_are_one_step_and_an_edit_marks_it_modified() {
    let (mut app, script) = app();
    let surge = device(&app, "Surge XT");
    let original = state(&app, surge);
    let selection = |app: &App| app.project().find_device(surge).unwrap().2.preset.clone();
    let source = ClapPresetSource {
        location: ClapPresetLocation::File("/presets/Warm Amp.fxp".into()),
        load_key: None,
    };
    let loaded = DevicePreset::loaded(source.clone());
    let depth = undo_depth(&mut app);

    // The plugin's own browser loads a preset and reports it; the selection waits for the
    // state of the load and enters with it as one step.
    let report = PluginManagerEvent::ClapPresetReported {
        device: surge,
        source: source.clone(),
        startup: false,
    };
    push(&script, report.clone());
    app.step();
    assert_eq!(selection(&app), None, "not before its state");
    assert_eq!(undo_depth(&mut app), depth);
    snapshot(&script, surge, &[4, 4]);
    app.step();
    assert_eq!(state(&app, surge).as_deref(), Some(&[4u8, 4][..]));
    assert_eq!(app.undo_text().as_deref(), Some("Preset laden"));
    // The same report again changes nothing.
    let revision = app.doc().revision();
    push(&script, report.clone());
    app.step();
    assert_eq!(app.doc().revision(), revision);

    // A card edit marks the preset as modified at once; the snapshot of the edit joins its step.
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: surge,
        id: 3,
        value: 0.5,
    }));
    assert!(selection(&app).unwrap().modified);
    snapshot(&script, surge, &[5, 5]);
    app.step();
    // Further edits change the state, not the mark.
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: surge,
        id: 3,
        value: 0.25,
    }));
    snapshot(&script, surge, &[6, 6]);
    app.step();
    assert!(selection(&app).unwrap().modified);
    assert_eq!(undo_depth(&mut app), depth + 2);

    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(
        selection(&app),
        Some(loaded.clone()),
        "undo takes the mark back"
    );
    assert_eq!(state(&app, surge).as_deref(), Some(&[4u8, 4][..]));
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(selection(&app), None);
    assert_eq!(state(&app, surge), original);
    for _ in 0..2 {
        app.dispatch(Action::Edit(EditAction::Redo));
    }
    assert!(selection(&app).unwrap().modified);
    assert_eq!(state(&app, surge).as_deref(), Some(&[6u8, 6][..]));

    // Loading the same preset again in the plugin starts it unmodified.
    app.set_snapshot_window(Duration::ZERO);
    push(&script, report.clone());
    snapshot(&script, surge, &[7, 7]);
    app.step();
    assert_eq!(selection(&app), Some(loaded.clone()));
    assert_eq!(state(&app, surge).as_deref(), Some(&[7u8, 7][..]));

    // Undo before the state of a reported load arrives hands the plugin the document's state:
    // the load no longer holds, and its late snapshot (the echo) brings no selection.
    app.set_snapshot_window(komponist_app::param_undo::COALESCE);
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: surge,
        id: 3,
        value: 0.75,
    }));
    snapshot(&script, surge, &[8, 8]);
    app.step();
    let other = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(b"Other".to_vec()),
    };
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device: surge,
            source: other,
            startup: false,
        },
    );
    app.step();
    script.borrow_mut().restores.clear();
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(selection(&app), Some(loaded.clone()));
    assert_eq!(
        script.borrow().restores.as_slice(),
        [(surge, Arc::<[u8]>::from([7, 7]))]
    );
    snapshot(&script, surge, &[7, 7]);
    app.step();
    assert_eq!(selection(&app), Some(loaded));
    assert_eq!(state(&app, surge).as_deref(), Some(&[7u8, 7][..]));

    // A failed load the plugin reports reaches the status bar.
    push(
        &script,
        PluginManagerEvent::ClapPresetFailed {
            device: surge,
            message: "no such file".into(),
        },
    );
    app.step();
    assert_eq!(
        status_text(&app),
        Some((
            "Das Preset lässt sich nicht laden.".to_owned(),
            StatusKind::Error
        ))
    );
}

#[test]
fn a_marked_preset_edit_is_undoable_before_its_snapshot() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: ClapPresetSource {
                location: ClapPresetLocation::Plugin,
                load_key: Some(b"Warm".to_vec()),
            },
            startup: false,
        },
    );
    snapshot(&script, device, &[1]);
    app.step();

    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device,
        id: 3,
        value: 0.75,
    }));
    let edited = app.project().find_device(device).unwrap().2;
    assert!(edited.preset.as_ref().unwrap().modified);
    assert_eq!(edited.parameter_bases.get(&3), Some(&0.75));
    script.borrow_mut().restores.clear();

    app.dispatch(Action::Edit(EditAction::Undo));
    let restored = app.project().find_device(device).unwrap().2;
    assert!(!restored.preset.as_ref().unwrap().modified);
    assert!(restored.parameter_bases.is_empty());
    assert_eq!(
        script.borrow().restores.as_slice(),
        [(device, Arc::<[u8]>::from([1]))]
    );
    app.dispatch(Action::Edit(EditAction::Redo));
    let edited = app.project().find_device(device).unwrap().2;
    assert!(edited.preset.as_ref().unwrap().modified);
    assert_eq!(edited.parameter_bases.get(&3), Some(&0.75));
}

#[test]
fn a_later_preset_report_waits_for_its_own_deferred_snapshot() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    let source = |key: &[u8]| ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(key.to_vec()),
    };
    let report = |source| PluginManagerEvent::ClapPresetReported {
        device,
        source,
        startup: false,
    };
    let first = DevicePreset::loaded(source(b"A"));
    let second = DevicePreset::loaded(source(b"B"));
    push(&script, report(first.source.clone()));
    snapshot(&script, device, &[1]);
    app.step();

    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Unrelated".into(),
    }));
    snapshot(&script, device, &[2]);
    push(&script, report(second.source.clone()));
    app.step();
    app.dispatch(Action::Edit(EditAction::EndGesture));
    let restored = app.project().find_device(device).unwrap().2;
    assert_eq!(restored.preset.as_ref(), Some(&first));
    assert_eq!(restored.state.as_deref(), Some(&[2][..]));

    snapshot(&script, device, &[3]);
    app.step();
    let loaded = app.project().find_device(device).unwrap().2;
    assert_eq!(loaded.preset.as_ref(), Some(&second));
    assert_eq!(loaded.state.as_deref(), Some(&[3][..]));
}

#[test]
fn deferred_edits_mark_the_loaded_preset_and_keep_the_last_edit_across_redo() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    let source = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(b"Warm".to_vec()),
    };
    let preset = DevicePreset::loaded(source.clone());
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Unrelated".into(),
    }));
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source,
            startup: false,
        },
    );
    snapshot(&script, device, &[1]);
    snapshot(&script, device, &[2]);
    app.step();

    for value in [0.25, 0.75] {
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: 3,
            value,
        }));
    }
    // A capture that predates the last turn arrives late. It has no explicit base for 3.
    snapshot(&script, device, &[3]);
    app.step();
    app.dispatch(Action::Edit(EditAction::EndGesture));
    let edited = app.project().find_device(device).unwrap().2;
    assert_eq!(edited.preset.as_ref().unwrap().source, preset.source);
    assert!(edited.preset.as_ref().unwrap().modified);
    assert_eq!(edited.state.as_deref(), Some(&[3][..]));
    assert_eq!(edited.parameter_bases.get(&3), Some(&0.75));

    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(
        app.project().find_device(device).unwrap().2.preset.as_ref(),
        Some(&preset)
    );
    app.dispatch(Action::Edit(EditAction::Redo));
    let edited = app.project().find_device(device).unwrap().2;
    assert!(edited.preset.as_ref().unwrap().modified);
    assert_eq!(edited.parameter_bases.get(&3), Some(&0.75));
}

#[test]
fn a_parameter_edit_waits_behind_a_pending_preset_without_a_gesture() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    let source = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(b"Warm".to_vec()),
    };
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: source.clone(),
            startup: false,
        },
    );
    app.step();
    script.borrow_mut().scripted_bases.insert((device, 3), 0.25);
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device,
        id: 3,
        value: 0.75,
    }));
    // The plugin has accepted the later edit by the time it serializes the opaque state.
    let opaque = 0.75_f64.to_ne_bytes();
    snapshot(&script, device, &opaque);
    app.step();

    let marked = app.project().find_device(device).unwrap().2;
    assert_eq!(
        marked.preset,
        Some(DevicePreset {
            source: source.clone(),
            modified: true
        })
    );
    assert_eq!(marked.parameter_bases.get(&3), Some(&0.75));
    app.dispatch(Action::Edit(EditAction::Undo));
    let loaded = app.project().find_device(device).unwrap().2;
    assert_eq!(loaded.preset, Some(DevicePreset::loaded(source)));
    assert_eq!(loaded.parameter_bases.get(&3), Some(&0.25));
    assert_eq!(script.borrow().restores.last().unwrap().1.as_ref(), &opaque);
    assert_eq!(
        script.borrow().restored_bases.last(),
        Some(&(device, 3, 0.25))
    );
}

#[test]
fn reloading_the_same_preset_supersedes_a_deferred_parameter_edit() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    let source = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(b"Warm".to_vec()),
    };
    let report = PluginManagerEvent::ClapPresetReported {
        device,
        source,
        startup: false,
    };
    push(&script, report.clone());
    snapshot(&script, device, &[1]);
    app.step();
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Unrelated".into(),
    }));
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device,
        id: 3,
        value: 0.75,
    }));
    push(&script, report);
    snapshot(&script, device, &[1]);
    app.step();
    app.dispatch(Action::Edit(EditAction::EndGesture));

    let current = app.project().find_device(device).unwrap().2;
    assert!(!current.preset.as_ref().unwrap().modified);
    assert!(current.parameter_bases.is_empty());
    app.dispatch(Action::Edit(EditAction::Undo));
    let edited = app.project().find_device(device).unwrap().2;
    assert!(edited.preset.as_ref().unwrap().modified);
    assert_eq!(edited.parameter_bases.get(&3), Some(&0.75));
}

#[test]
fn saving_a_fresh_snapshot_keeps_the_deferred_preset_source() {
    let (mut app, script) = app();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("Deferred.komponist");
    std::fs::write(&file, persist::to_json(app.project()).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    let device = device(&app, "Surge XT");
    let source = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(b"Warm".to_vec()),
    };
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Unrelated".into(),
    }));
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: source.clone(),
            startup: false,
        },
    );
    snapshot(&script, device, &[1]);
    app.step();
    assert!(
        app.project()
            .find_device(device)
            .unwrap()
            .2
            .preset
            .is_none()
    );

    script.borrow_mut().snapshot = vec![(device, Arc::<[u8]>::from([2]).into())];
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let saved = saved.find_device(device).unwrap().2;
    assert_eq!(saved.preset, Some(DevicePreset::loaded(source)));
    assert_eq!(saved.state.as_deref(), Some(&[2][..]));
}

#[test]
fn save_withholds_a_capture_that_races_a_preset_report() {
    let (mut app, script) = app();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("Before load.komponist");
    let original = persist::to_json(app.project()).unwrap();
    std::fs::write(&file, &original).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    let device = device(&app, "Surge XT");
    {
        let mut script = script.borrow_mut();
        script.snapshot = vec![(device, Arc::<[u8]>::from([1]).into())];
        script
            .events_during_capture
            .push(PluginManagerEvent::ClapPresetReported {
                device,
                source: ClapPresetSource {
                    location: ClapPresetLocation::Plugin,
                    load_key: Some(b"Newly loaded".to_vec()),
                },
                startup: false,
            });
    }
    app.dispatch(Action::Project(ProjectAction::Save));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    assert_eq!(status_text(&app).unwrap().1, StatusKind::Error);
    assert!(state(&app, device).is_none());
}

#[test]
fn save_reports_missing_state_when_a_pending_preset_withholds_the_deferred_state() {
    let (mut app, script) = app();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("Missing state.komponist");
    std::fs::write(&file, persist::to_json(app.project()).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    let device = device(&app, "Surge XT");
    push(
        &script,
        PluginManagerEvent::StartupState {
            device,
            state: Arc::<[u8]>::from([1]).into(),
        },
    );
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: ClapPresetSource {
                location: ClapPresetLocation::Plugin,
                load_key: Some(b"Warm".to_vec()),
            },
            startup: false,
        },
    );
    app.step();
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device,
        id: 3,
        value: 0.75,
    }));

    script.borrow_mut().snapshot_unavailable.push(device);
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(saved.find_device(device).unwrap().2.state.is_none());
    let missing = app
        .lang()
        .project_save_plugin_missing_state("Surge XT".to_owned());
    assert!(status_text(&app).unwrap().0.contains(missing.as_str()));
}

#[test]
fn undoing_the_first_report_restores_the_startup_state() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    assert!(state(&app, device).is_none());

    let initial: Arc<[u8]> = Arc::from([1_u8]);
    push(
        &script,
        PluginManagerEvent::StartupState {
            device,
            state: Arc::clone(&initial).into(),
        },
    );
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: ClapPresetSource {
                location: ClapPresetLocation::Plugin,
                load_key: Some(b"Selected".to_vec()),
            },
            startup: false,
        },
    );
    // The report can arrive before any readiness notification or requested state snapshot.
    snapshot(&script, device, &[2]);
    app.step();

    let model = app.project().find_device(device).unwrap().2;
    assert!(model.preset.is_some());
    assert_eq!(model.state.as_deref(), Some(&[2_u8][..]));
    app.dispatch(Action::Edit(EditAction::Undo));
    let model = app.project().find_device(device).unwrap().2;
    assert!(model.preset.is_none());
    assert!(model.state.is_none());
    assert_eq!(script.borrow().restores.last(), Some(&(device, initial)));
}

#[test]
fn a_host_preset_load_supersedes_a_report_awaiting_its_state() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    let source = |key: &[u8]| ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(key.to_vec()),
    };
    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: source(b"Plugin"),
            startup: false,
        },
    );
    app.step();

    let host_source = source(b"Host");
    push(
        &script,
        PluginManagerEvent::ClapPresetLoaded {
            device,
            source: Some(host_source.clone()),
            previous_state: Arc::from([4_u8]),
            state: Arc::from([5_u8]),
        },
    );
    app.step();
    snapshot(&script, device, &[5]);
    app.step();
    let device = app.project().find_device(device).unwrap().2;
    assert_eq!(device.preset, Some(DevicePreset::loaded(host_source)));
    assert_eq!(device.state.as_deref(), Some(&[5_u8][..]));
}

#[test]
fn a_reported_preset_ends_the_undo_echo_window() {
    let (mut app, script) = app();
    let device = device(&app, "Surge XT");
    app.set_snapshot_window(Duration::ZERO);
    snapshot(&script, device, &[1]);
    app.step();
    snapshot(&script, device, &[2]);
    app.step();
    app.dispatch(Action::Edit(EditAction::Undo));

    push(
        &script,
        PluginManagerEvent::ClapPresetReported {
            device,
            source: ClapPresetSource {
                location: ClapPresetLocation::Plugin,
                load_key: Some(b"Warm".to_vec()),
            },
            startup: false,
        },
    );
    snapshot(&script, device, &[3]);
    app.step();
    snapshot(&script, device, &[4]);
    app.step();
    assert_eq!(state(&app, device).as_deref(), Some(&[4_u8][..]));
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(state(&app, device).as_deref(), Some(&[3_u8][..]));
}

#[test]
fn deferred_preset_marks_and_snapshots_undo_together_for_each_device() {
    let (mut app, script) = app();
    let synth = device(&app, "Surge XT");
    let eq = device(&app, "LSP Para EQ x8");
    let source = ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(b"Init".to_vec()),
    };
    for device in [synth, eq] {
        push(
            &script,
            PluginManagerEvent::ClapPresetReported {
                device,
                source: source.clone(),
                startup: false,
            },
        );
        snapshot(&script, device, &[1]);
    }
    app.step();
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Unrelated".into(),
    }));
    for device in [synth, eq] {
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: 3,
            value: 0.5,
        }));
        snapshot(&script, device, &[2]);
    }
    app.step();
    app.dispatch(Action::Edit(EditAction::EndGesture));

    let modified = |app: &App, device| {
        app.project()
            .find_device(device)
            .unwrap()
            .2
            .preset
            .as_ref()
            .unwrap()
            .modified
    };
    for device in [synth, eq] {
        assert!(modified(&app, device));
        assert_eq!(state(&app, device).as_deref(), Some(&[2_u8][..]));
    }
    for device in [eq, synth] {
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(!modified(&app, device));
        assert_eq!(state(&app, device).as_deref(), Some(&[1_u8][..]));
    }
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
    script.borrow_mut().snapshot = vec![(surge, Arc::<[u8]>::from(baseline).into())];
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
            komponist_model::edits::ChannelProp::VolumeDb(-7.5),
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
        localized: None,
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
    script.borrow_mut().snapshot = vec![(surge, Arc::clone(&blob).into())];

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
    let captured: Arc<[u8]> = Arc::from(&b"fresh effect state"[..]);
    let project =
        komponist_model::edits::set_device_state(app.project(), surge, Some(cached)).unwrap();
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.step();
    {
        let mut script = script.borrow_mut();
        script.snapshot = vec![(fresh, Arc::clone(&captured).into())];
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

    let next: Arc<[u8]> = Arc::from(&b"new effect state"[..]);
    {
        let mut script = script.borrow_mut();
        script.snapshot = vec![(fresh, Arc::clone(&next).into())];
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
    use komponist_model::Tick;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
    use komponist_model::edits::automation::{add_lane, set_lane_mode, set_lane_points};
    use komponist_model::params::ParamId;
    use komponist_plugin_api::params::param_flags;

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
    let project = komponist_model::edits::set_device_preset(
        &project,
        device,
        Some(DevicePreset::loaded(ClapPresetSource {
            location: ClapPresetLocation::Plugin,
            load_key: Some(b"init".to_vec()),
        })),
    )
    .unwrap();
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
        catalog: Catalog::default(),
        statuses: HashMap::new(),
        gui: Some(gui.clone()),
        clap_presets: Vec::new(),
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
    // A temporary automation value is no edit of the loaded preset.
    let modified = |app: &App| {
        app.project()
            .find_device(device)
            .unwrap()
            .2
            .preset
            .as_ref()
            .unwrap()
            .modified
    };
    assert!(!modified(&app));

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
    // An edit in the plugin's GUI modifies the loaded preset like a card edit.
    assert!(modified(&app));
}

#[test]
fn a_plugin_gui_edit_during_a_fader_drag_marks_the_preset_after_it() {
    use komponist_model::automation::AutomationMode;

    let (mut app, _script, gui, _) = plugin_gui_automation(AutomationMode::Off);
    let device = gui.device;
    let modified = |app: &App| {
        app.project()
            .find_device(device)
            .unwrap()
            .2
            .preset
            .as_ref()
            .unwrap()
            .modified
    };
    let track = app.project().find_device(device).unwrap().0;
    let komponist_model::ChannelRef::Track(track) = track else {
        panic!("the synth is on a track");
    };
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "Lautstärke".into(),
    }));
    app.dispatch(Action::Edit(EditAction::SetTrackProp {
        tracks: vec![track],
        prop: komponist_model::edits::TrackProp::Channel(
            komponist_model::edits::ChannelProp::VolumeDb(-7.5),
        ),
    }));
    gui.pending.store(0.5_f64.to_bits(), Ordering::Relaxed);
    assert!(app.engine_mut().run_held(256, &[], 0));
    app.step();
    assert!(!modified(&app), "not inside the fader's step");
    app.dispatch(Action::Edit(EditAction::EndGesture));
    assert!(modified(&app));
    // The fader and the plugin edit are two steps.
    assert_eq!(app.undo_text().as_deref(), Some("Surge XT: Zustand ändern"));
    app.dispatch(Action::Edit(EditAction::Undo));
    assert!(!modified(&app));
    assert_eq!(app.undo_text().as_deref(), Some("Lautstärke"));
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
