//! The plugin frame (S05, WP-50) without X11: header and body as pictures next to S05, what
//! their controls report, and the frame's flow in the app (embedded viewports in `egui_kittest`)
//! with a scripted device host: open, view switch, knobs, presets, bypass, loading, crashed with
//! "Neu laden", missing, close by button and by Esc.
//!
//! Real windows with real plugins: `tests/plugin_frame_gui.rs` (Xvfb only).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use egui::accesskit::Role;
use egui::{Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{EditAction, PluginAction, ProjectAction};
use komponist_app::device_host::DeviceHost;
use komponist_app::engine_sync::EngineSync;
use komponist_app::settings::Settings;
use komponist_app::ui_state::PluginView;
use komponist_app::views::devices::model::hexa_macro_params;
use komponist_app::windows::plugin_frame::body::{self, BodyEvent, BodyModel, ParamGrid};
use komponist_app::windows::plugin_frame::header::{self, HeaderEvent, HeaderModel, Mark};
use komponist_app::windows::plugin_frame::metrics;
use komponist_app::{Action, App};
use komponist_engine::Processor;
use komponist_i18n::{Lang, de};
use komponist_ipc::frame::{FrameChord, FrameKeyEvent, FrameRect, KeyMods};
use komponist_ipc::{
    GuiSize, GuiTarget, ParamInfo, ParamValue, PluginDescriptor, PluginFormat as BundleFormat,
    PluginKind,
};
use komponist_model::fixtures::demo_project;
use komponist_model::{Device, DeviceId, DeviceRole, Document, PluginRef};
use komponist_plugins::{
    Catalog, DeviceStatus, FailReason, PluginError, PluginManagerEvent, ProcessorSetup,
};

/// The title the device card gives a crashed device (`views::devices::model::failure_texts`, #2).
const CRASHED_TITLE: &str = "Abgestürzt";

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

// ------------------------------------------------------------------------------------------------
// Header and body as pictures
// ------------------------------------------------------------------------------------------------

fn surge_header() -> HeaderModel {
    HeaderModel {
        name: "Surge XT".into(),
        sub: "Pad · CLAP".into(),
        mark: Mark::Active,
        bypassed: false,
        presets: Vec::new(),
        preset: None,
        preset_changed: false,
        can_save: false,
        view: PluginView::Gui,
        has_gui: true,
        keep_on_top: false,
        close_shortcut: "Esc".into(),
    }
}

fn hexa_header() -> HeaderModel {
    HeaderModel {
        name: "Hexa".into(),
        sub: "Lead · INTERN".into(),
        presets: komponist_builtin::presets()
            .map(|p| p.name.to_owned())
            .collect(),
        preset: Some(3),
        preset_changed: true,
        view: PluginView::Params,
        has_gui: false,
        ..surge_header()
    }
}

/// hexa's macros as the frame shows them (WP-44's model, read from the device state).
fn macro_grid() -> ParamGrid {
    let project = demo_project();
    let hexa = project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .find(|d| d.plugin.name == "Hexa")
        .unwrap();
    ParamGrid {
        params: hexa_macro_params(Lang::De, hexa),
    }
}

/// What a frame harness shows and collects.
struct Frame {
    header: HeaderModel,
    body: BodyModel,
    header_events: Vec<HeaderEvent>,
    body_events: Vec<BodyEvent>,
    natural_width: f32,
}

fn frame(header: HeaderModel, body: BodyModel) -> Frame {
    Frame {
        header,
        body,
        header_events: Vec::new(),
        body_events: Vec::new(),
        natural_width: 0.0,
    }
}

/// A frame of `size` points (header included) at `ppp`.
fn frame_harness(state: Frame, size: [f32; 2], ppp: f32) -> Harness<'static, Frame> {
    komponist_ui::snapshot::builder::<Frame>(ppp)
        .with_size(vec2(size[0], size[1]))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            |ui, f: &mut Frame| {
                let rect = ui.ctx().content_rect();
                let head = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::HEADER));
                let out = header::show(ui, head, &f.header);
                f.natural_width = out.natural_width;
                f.header_events.extend(out.events);
                let body_rect = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
                let events = body::show(ui, body_rect, &mut f.body);
                f.body_events.extend(events);
            },
            state,
        )
}

#[test]
fn frame_states_next_to_s05() {
    let gui_size = [
        metrics::GUI_AREA[0],
        metrics::GUI_AREA[1] / 2.0 + metrics::HEADER,
    ];
    let params_size = [
        metrics::PARAMS_AREA[0] + 180.0,
        metrics::PARAMS_AREA[1] + metrics::HEADER,
    ];
    let mut bypassed = surge_header();
    bypassed.mark = Mark::Bypassed;
    bypassed.bypassed = true;
    bypassed.keep_on_top = true;
    let mut crashed = surge_header();
    crashed.mark = Mark::Inactive;
    let cases: Vec<(&str, Frame, [f32; 2])> = vec![
        (
            "plugin-frame-gui",
            frame(
                surge_header(),
                BodyModel::Gui {
                    native: false,
                    name: "Surge XT".into(),
                },
            ),
            gui_size,
        ),
        (
            "plugin-frame-bypassed-on-top",
            frame(
                bypassed,
                BodyModel::Gui {
                    native: true,
                    name: "Surge XT".into(),
                },
            ),
            [gui_size[0], metrics::HEADER + 40.0],
        ),
        (
            "plugin-frame-crashed",
            frame(crashed.clone(), BodyModel::Crashed(exited())),
            gui_size,
        ),
        (
            "plugin-frame-loading",
            frame(crashed.clone(), BodyModel::Loading),
            gui_size,
        ),
        (
            "plugin-frame-missing",
            frame(
                crashed,
                BodyModel::Missing {
                    name: "Surge XT".into(),
                },
            ),
            gui_size,
        ),
        (
            "plugin-frame-params",
            frame(hexa_header(), BodyModel::Params(macro_grid())),
            params_size,
        ),
    ];
    let mut results = SnapshotResults::new();
    for (name, state, size) in cases {
        for ppp in [1.0, 2.0] {
            let mut h = frame_harness(
                Frame {
                    header: state.header.clone(),
                    body: state.body.clone(),
                    ..frame(state.header.clone(), state.body.clone())
                },
                size,
                ppp,
            );
            h.run();
            results.add(h.try_snapshot(format!("{name}-{ppp}x")));
        }
    }
}

#[test]
fn the_header_fits_the_s05_frame_width() {
    let mut h = frame_harness(
        frame(surge_header(), BodyModel::Loading),
        [metrics::GUI_AREA[0], 200.0],
        1.0,
    );
    h.run();
    let width = h.state().natural_width;
    assert!(
        width > 500.0 && width <= metrics::GUI_AREA[0],
        "header needs {width} pt"
    );
}

// ------------------------------------------------------------------------------------------------
// Header and body controls
// ------------------------------------------------------------------------------------------------

#[test]
fn header_controls_report_what_the_user_did() {
    let mut h = frame_harness(
        frame(
            surge_header(),
            BodyModel::Gui {
                native: true,
                name: String::new(),
            },
        ),
        [900.0, 100.0],
        1.0,
    );
    h.run();
    h.get_by_label(de::PLUGIN_FRAME_BYPASS).click();
    h.run();
    h.get_by_label(de::PLUGIN_FRAME_KEEP_ON_TOP).click();
    h.run();
    h.get_by_role_and_label(Role::RadioButton, de::PLUGIN_FRAME_VIEW_PARAMS)
        .click();
    h.run();
    h.get_by_label(de::DIALOG_CLOSE).click();
    h.run();
    assert_eq!(
        h.state().header_events,
        [
            HeaderEvent::ToggleBypass,
            HeaderEvent::KeepOnTop(true),
            HeaderEvent::View(PluginView::Params),
            HeaderEvent::Close
        ]
    );
    // Without presets the preset controls and "Speichern" are disabled.
    for name in [
        de::PLUGIN_FRAME_PRESET_PREV,
        de::PLUGIN_FRAME_PRESET_NEXT,
        de::PLUGIN_FRAME_SAVE,
    ] {
        assert!(
            h.get_by_label(name).accesskit_node().is_disabled(),
            "{name}"
        );
    }
}

#[test]
fn a_plugin_without_gui_cannot_switch_to_the_plugin_view() {
    let mut h = frame_harness(
        frame(hexa_header(), BodyModel::Params(macro_grid())),
        [900.0, 300.0],
        1.0,
    );
    h.run();
    let plugin = h.get_by_role_and_label(Role::RadioButton, de::PLUGIN_FRAME_VIEW_PLUGIN);
    assert!(plugin.accesskit_node().is_disabled());
    h.get_by_label(de::PLUGIN_FRAME_PRESET_NEXT).click();
    h.run();
    h.get_by_label(de::PLUGIN_FRAME_PRESET_PREV).click();
    h.run();
    assert_eq!(
        h.state().header_events,
        [HeaderEvent::PresetNext, HeaderEvent::PresetPrev]
    );
    // The changed preset carries " *" (prototype `pl.changed`).
    let name = komponist_builtin::presets().nth(3).unwrap().name;
    let field = h.get_by_role_and_label(Role::ComboBox, de::PLUGIN_FRAME_PRESET);
    assert_eq!(field.value().as_deref(), Some(format!("{name} *").as_str()));
}

#[test]
fn knobs_report_plain_values_on_the_step_grid() {
    let mut h = frame_harness(
        frame(hexa_header(), BodyModel::Params(macro_grid())),
        [900.0, 300.0],
        1.0,
    );
    h.run();
    let knob = h.get_by_role_and_label(Role::Slider, "Algorithmus");
    let at = knob.rect().center();
    h.drag_at(at);
    h.run();
    h.hover_at(at - vec2(0.0, 40.0));
    h.run();
    h.drop_at(at - vec2(0.0, 40.0));
    h.run();
    let events = &h.state().body_events;
    assert!(!events.is_empty(), "the drag set the parameter");
    let algorithm = komponist_builtin::Macro::Algorithm.id();
    let mut sets = 0;
    for e in events {
        match *e {
            // The drag is one undo gesture around the values.
            BodyEvent::BeginGesture { id } => assert_eq!(id, algorithm),
            BodyEvent::EndGesture => {}
            BodyEvent::SetParam { id, value } => {
                assert_eq!(id, algorithm);
                assert_eq!(value.fract(), 0.0, "stepped: {value}");
                sets += 1;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(sets > 0, "{events:?}");
    let count = |f: fn(&BodyEvent) -> bool| events.iter().filter(|e| f(e)).count();
    assert_eq!(count(|e| matches!(e, BodyEvent::BeginGesture { .. })), 1);
    assert_eq!(count(|e| matches!(e, BodyEvent::EndGesture)), 1);
}

fn exited() -> FailReason {
    FailReason::Exited {
        code: None,
        signal: Some(11),
    }
}

#[test]
fn a_failed_load_or_a_hang_does_not_read_abgestuerzt() {
    for (reason, title, detail) in [
        (
            FailReason::LoadFailed("bridge missing".into()),
            de::DEVICES_LOAD_FAILED,
            "bridge missing",
        ),
        (
            FailReason::Unresponsive,
            de::DEVICES_UNRESPONSIVE,
            de::DEVICES_UNRESPONSIVE_TEXT,
        ),
    ] {
        let mut h = frame_harness(
            frame(surge_header(), BodyModel::Crashed(reason)),
            [820.0, 400.0],
            1.0,
        );
        h.run();
        assert!(h.query_by_label(CRASHED_TITLE).is_none());
        assert!(h.query_by_label(title).is_some(), "{title}");
        assert!(h.query_by_label_contains(detail).is_some(), "{detail}");
        assert!(h.query_by_label(de::PLUGIN_FRAME_RELOAD).is_some());
    }
}

#[test]
fn the_crashed_body_offers_neu_laden() {
    let mut h = frame_harness(
        frame(surge_header(), BodyModel::Crashed(exited())),
        [820.0, 400.0],
        1.0,
    );
    h.run();
    assert!(h.query_by_label(CRASHED_TITLE).is_some());
    h.get_by_label(de::PLUGIN_FRAME_RELOAD).click();
    h.run();
    assert_eq!(h.state().body_events, [BodyEvent::Reload]);
}

// ------------------------------------------------------------------------------------------------
// The frame in the app (embedded viewports, scripted host)
// ------------------------------------------------------------------------------------------------

/// What the test scripts and observes.
#[derive(Default)]
struct Script {
    statuses: HashMap<DeviceId, DeviceStatus>,
    /// Devices of the scripted plugin (they have a GUI and parameters).
    known: Vec<DeviceId>,
    events: Vec<PluginManagerEvent>,
    set: Vec<(DeviceId, u32, f64)>,
    created: Vec<DeviceId>,
    opened: Vec<DeviceId>,
    closed: Vec<DeviceId>,
    sizes: Vec<(DeviceId, GuiSize)>,
    targets: Vec<GuiTarget>,
    headers: Vec<(DeviceId, HeaderModel, String)>,
    activated: Vec<DeviceId>,
    raised: Vec<DeviceId>,
}

/// Devices whose plugin the catalog knows run and have a GUI; the rest are missing.
struct FakeHost {
    script: Rc<RefCell<Script>>,
    catalog: Catalog,
    params: Vec<ParamInfo>,
    values: HashMap<u32, ParamValue>,
}

impl FakeHost {
    fn known(&self, device: DeviceId) -> bool {
        self.script.borrow().known.contains(&device)
    }
}

impl DeviceHost for FakeHost {
    fn create_processor(
        &mut self,
        device: &Device,
        _: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        let mut s = self.script.borrow_mut();
        s.created.push(device.id);
        let status = if self.catalog.resolve(&device.plugin).is_some() {
            if device.plugin.format != komponist_model::PluginFormat::Builtin {
                s.known.push(device.id);
            }
            DeviceStatus::Ok
        } else {
            DeviceStatus::Missing
        };
        s.statuses.insert(device.id, status);
        None
    }
    fn release(&mut self, device: DeviceId) {
        self.script.borrow_mut().statuses.remove(&device);
    }
    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.script
            .borrow()
            .statuses
            .get(&device)
            .cloned()
            .unwrap_or(DeviceStatus::Ok)
    }
    fn latency(&self, _: DeviceId) -> u32 {
        0
    }
    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        std::mem::take(&mut self.script.borrow_mut().events)
    }
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        self.catalog.resolve(plugin).map(|_| DeviceRole::Instrument)
    }
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        self.known(device).then_some(self.params.as_slice())
    }
    fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        self.known(device).then(|| self.values.get(&id)).flatten()
    }
    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        // hexa goes through the engine (and into the document), as with the real host.
        if !self.known(device) {
            return false;
        }
        self.script.borrow_mut().set.push((device, id, value));
        true
    }
    fn has_gui(&self, device: DeviceId) -> bool {
        self.status(device) == DeviceStatus::Ok && self.known(device)
    }
    fn open_gui(&mut self, device: DeviceId, target: GuiTarget) -> Result<(), PluginError> {
        let mut s = self.script.borrow_mut();
        s.opened.push(device);
        s.targets.push(target);
        Ok(())
    }
    fn set_frame_header(&mut self, device: DeviceId, header: HeaderModel, lang: &str) {
        self.script
            .borrow_mut()
            .headers
            .push((device, header, lang.to_owned()));
    }
    fn activate_frame(&mut self, device: DeviceId) {
        self.script.borrow_mut().activated.push(device);
    }
    fn raise_frame(&mut self, device: DeviceId) {
        self.script.borrow_mut().raised.push(device);
    }
    fn set_gui_size(&mut self, device: DeviceId, size: GuiSize) {
        self.script.borrow_mut().sizes.push((device, size));
    }
    fn close_gui(&mut self, device: DeviceId) {
        self.script.borrow_mut().closed.push(device);
    }
}

/// The demo with Surge XT "installed" (two parameters) on the scripted host.
fn app() -> (App, Rc<RefCell<Script>>, DeviceId) {
    let project = demo_project();
    let surge = project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .find(|d| d.plugin.name == "Surge XT")
        .unwrap();
    let mut catalog = Catalog::builtin();
    catalog.set_scanned(&[PluginDescriptor {
        format: BundleFormat::Clap,
        id: surge.plugin.id.clone(),
        name: surge.plugin.name.clone(),
        vendor: surge.plugin.vendor.clone(),
        version: surge.plugin.version.clone(),
        kind: PluginKind::Instrument,
        features: Vec::new(),
        bundle: "/usr/lib/clap/Surge XT.clap".into(),
        sidechain: false,
    }]);
    let script = Rc::new(RefCell::new(Script::default()));
    let params = vec![
        ParamInfo {
            id: 7,
            name: "Cutoff".into(),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            step_count: 0,
            flags: 0,
        },
        ParamInfo {
            id: 8,
            name: "Versteckt".into(),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            step_count: 0,
            flags: komponist_ipc::control::param_flags::HIDDEN,
        },
    ];
    let values = HashMap::from([(
        7,
        ParamValue {
            id: 7,
            value: 0.25,
            text: "250 Hz".into(),
        },
    )]);
    let host = FakeHost {
        script: Rc::clone(&script),
        catalog,
        params,
        values,
    };
    let engine = EngineSync::null(Box::new(host));
    let surge = surge.id;
    let app = App::new(Document::new(project), engine, Settings::default(), None);
    (app, script, surge)
}

fn harness(mut app: App) -> Harness<'static, App> {
    app.show_embedded_plugin_frames(true);
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_max_steps(64)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

fn device_named(app: &App, name: &str) -> DeviceId {
    app.project()
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .find(|d| d.plugin.name == name)
        .unwrap()
        .id
}

#[test]
fn a_plugin_window_switches_views_sets_parameters_and_closes() {
    let (app, script, surge) = app();
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(3);
    assert!(h.state().plugin_frame(surge).is_some());
    // No X11 in the harness: the placeholder stands in for the plugin's own GUI.
    assert!(
        h.query_by_label_contains("Plugin-GUI · zeichnet Surge XT selbst")
            .is_some()
    );
    // Parameter view: visible parameters only, the plugin's text.
    h.get_by_role_and_label(Role::RadioButton, de::PLUGIN_FRAME_VIEW_PARAMS)
        .click();
    h.run_steps(3);
    assert_eq!(h.state().ui().plugin_windows[0].view, PluginView::Params);
    assert!(h.state().dispatched().any(|a| *a
        == Action::Ui(komponist_app::UiAction::SetPluginView {
            device: surge,
            view: PluginView::Params
        })));
    assert!(h.query_by_role_and_label(Role::Slider, "Cutoff").is_some());
    assert!(h.query_by_label("Versteckt").is_none());
    assert!(h.query_by_label("250 Hz").is_some());
    let knob = h
        .get_by_role_and_label(Role::Slider, "Cutoff")
        .rect()
        .center();
    h.drag_at(knob);
    h.run();
    h.hover_at(knob - vec2(0.0, 20.0));
    h.run();
    h.drop_at(knob - vec2(0.0, 20.0));
    h.run_steps(2);
    let set = script.borrow().set.clone();
    assert!(
        !set.is_empty()
            && set
                .iter()
                .all(|(d, id, v)| *d == surge && *id == 7 && *v > 0.25)
    );
    // Bypass and close through the header.
    h.get_by_label(de::PLUGIN_FRAME_BYPASS).click();
    h.run_steps(2);
    let bypassed = h.state().project().find_device(surge).unwrap().2.bypass;
    assert!(bypassed);
    assert!(h.state().dispatched().any(|a| *a
        == Action::Edit(EditAction::SetDeviceBypass {
            device: surge,
            bypass: true
        })));
    h.get_by_label(de::DIALOG_CLOSE).click();
    h.run_steps(3);
    assert!(h.state().ui().plugin_windows.is_empty());
    assert!(h.state().plugin_frame(surge).is_none());
    assert!(h.query_by_label(de::DIALOG_CLOSE).is_none());
}

#[test]
fn oben_halten_and_the_position_are_remembered_per_plugin() {
    let (app, _, surge) = app();
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(3);
    assert!(!h.state().ui().plugin_windows[0].keep_on_top);
    h.get_by_label(de::PLUGIN_FRAME_KEEP_ON_TOP).click();
    h.run_steps(3);
    assert!(h.state().ui().plugin_windows[0].keep_on_top);
    assert!(h.state().plugin_frame(surge).unwrap().keep_on_top);
    // The frame stood at (120, 80) when it closed.
    let plugin = h
        .state()
        .project()
        .find_device(surge)
        .unwrap()
        .2
        .plugin
        .clone();
    h.state_mut()
        .settings_mut()
        .update_frame_memory(&plugin, |m| m.pos = Some((120, 80)));
    h.get_by_label(de::DIALOG_CLOSE).click();
    h.run_steps(3);
    assert!(h.state().ui().plugin_windows.is_empty());
    // Opened again, it comes back on top and where it stood.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(3);
    let w = h.state().ui().plugin_windows[0];
    assert!(w.keep_on_top);
    assert_eq!(w.pos, Some((120, 80)));
    // Another plugin has no memory.
    let other = device_named(h.state(), "Hexa");
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(other)));
    h.run_steps(3);
    let w = h
        .state()
        .ui()
        .plugin_windows
        .iter()
        .find(|w| w.device == other)
        .copied()
        .unwrap();
    assert!(!w.keep_on_top && w.pos.is_none());
}

#[test]
fn the_frame_shows_loading_crashed_and_reloads() {
    let (app, script, surge) = app();
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(2);
    script.borrow_mut().statuses.insert(
        surge,
        DeviceStatus::Crashed(FailReason::Exited {
            code: None,
            signal: Some(9),
        }),
    );
    h.run_steps(3);
    assert!(h.query_by_label(CRASHED_TITLE).is_some());
    let created = script.borrow().created.len();
    h.get_by_label(de::PLUGIN_FRAME_RELOAD).click();
    h.run_steps(3);
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Plugin(PluginAction::Reload(surge)))
    );
    assert_eq!(script.borrow().created.len(), created + 1);
    script
        .borrow_mut()
        .statuses
        .insert(surge, DeviceStatus::Loading);
    h.run_steps(3);
    assert!(h.query_by_label(de::PLUGIN_FRAME_LOADING).is_some());
}

#[test]
fn hexa_opens_its_parameters_and_steps_through_its_presets() {
    let (app, _script, _) = app();
    let mut h = harness(app);
    let lead = device_named(h.state(), "Hexa");
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(lead)));
    h.run_steps(3);
    assert_eq!(h.state().ui().plugin_windows[0].view, PluginView::Params);
    for m in komponist_builtin::Macro::ALL {
        assert!(
            h.query_by_role_and_label(Role::Slider, m.name()).is_some(),
            "{}",
            m.name()
        );
    }
    h.get_by_label(de::PLUGIN_FRAME_PRESET_NEXT).click();
    h.run_steps(3);
    let applied = h.state().dispatched().find_map(|a| match a {
        Action::Plugin(PluginAction::ApplyHexaPreset { device, preset }) if *device == lead => {
            Some(*preset)
        }
        _ => None,
    });
    let preset = applied.unwrap_or_else(|| {
        panic!(
            "› applies a preset; dispatched {:?}",
            h.state().dispatched().collect::<Vec<_>>()
        )
    });
    let state = h
        .state()
        .project()
        .find_device(lead)
        .unwrap()
        .2
        .state
        .clone();
    let expected =
        komponist_builtin::encode_state(&komponist_builtin::preset_patch(preset).unwrap());
    assert_eq!(state.as_deref(), Some(&expected[..]));
    let name = komponist_builtin::presets().nth(preset).unwrap().name;
    let field = h.get_by_role_and_label(Role::ComboBox, de::PLUGIN_FRAME_PRESET);
    assert_eq!(field.value().as_deref(), Some(name));

    // A knob writes into the device state (WP-44); the frame reads it back from there and marks
    // the preset as changed.
    let attack = komponist_builtin::Macro::Attack;
    let knob = h
        .get_by_role_and_label(Role::Slider, attack.name())
        .rect()
        .center();
    h.drag_at(knob);
    h.run();
    h.hover_at(knob - vec2(0.0, 30.0));
    h.run();
    h.drop_at(knob - vec2(0.0, 30.0));
    h.run_steps(3);
    let state = h
        .state()
        .project()
        .find_device(lead)
        .unwrap()
        .2
        .state
        .clone();
    let patch = komponist_builtin::decode_state(state.as_deref()).unwrap();
    let stored = attack.get(&patch);
    let preset_value = attack.get(&komponist_builtin::preset_patch(preset).unwrap());
    assert!(stored > preset_value, "{stored} > {preset_value}");
    let shown = h.get_by_role_and_label(Role::Slider, attack.name());
    assert_eq!(shown.value().as_deref(), Some(attack.text(stored).as_str()));
    let field = h.get_by_role_and_label(Role::ComboBox, de::PLUGIN_FRAME_PRESET);
    assert_eq!(field.value().as_deref(), Some(format!("{name} *").as_str()));
}

#[test]
fn a_missing_plugin_says_so_and_esc_in_the_main_window_leaves_the_frame_open() {
    let (app, _script, _) = app();
    let mut h = harness(app);
    let dexed = device_named(h.state(), "Dexed");
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(dexed)));
    h.run_steps(3);
    assert!(
        h.query_by_label(&Lang::De.plugin_frame_missing("Dexed"))
            .is_some()
    );
    // Frames are windows of their own: Esc in the main window only clears the selection.
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().ui().plugin_windows.len(), 1);
    assert!(h.state().plugin_frame(dexed).is_some());
}

#[test]
fn another_project_closes_every_plugin_frame() {
    let (app, script, surge) = app();
    let mut h = harness(app);
    let hexa = device_named(h.state(), "Hexa");
    for device in [surge, hexa] {
        h.state_mut()
            .dispatch(Action::Plugin(PluginAction::OpenWindow(device)));
    }
    h.run_steps(3);
    assert!(h.state().plugin_frame(surge).is_some() && h.state().plugin_frame(hexa).is_some());
    h.state_mut().dispatch(Action::Project(ProjectAction::New));
    h.run_steps(3);
    assert!(h.state().ui().plugin_windows.is_empty());
    assert!(h.state().plugin_frame(surge).is_none());
    assert!(h.state().plugin_frame(hexa).is_none());
    assert!(h.query_by_label(de::DIALOG_CLOSE).is_none());
    // No GUI was open (no X11 here), so none had to close.
    assert!(script.borrow().closed.is_empty());
}

// ------------------------------------------------------------------------------------------------
// The bridge's own frame (macOS, #52), forced on here
// ------------------------------------------------------------------------------------------------

fn bridge_frame_harness() -> (Harness<'static, App>, Rc<RefCell<Script>>, DeviceId) {
    let (mut app, script, surge) = app();
    app.use_bridge_frames(true);
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(3);
    (h, script, surge)
}

fn push(script: &Rc<RefCell<Script>>, event: PluginManagerEvent) {
    script.borrow_mut().events.push(event);
}

#[test]
fn the_bridge_opens_its_own_frame_and_the_app_shows_none() {
    let (mut h, script, surge) = bridge_frame_harness();
    let target = script.borrow().targets.last().cloned();
    let Some(GuiTarget::OwnFrame { header, lang, rect }) = target else {
        panic!("expected OpenGui with OwnFrame, got {target:?}");
    };
    assert_eq!(header.name, "Surge XT");
    assert_eq!(header.view, PluginView::Gui);
    assert!(header.has_gui);
    assert_eq!(lang, "de");
    assert_eq!(rect, None, "no remembered position");
    assert!(h.state().plugin_frame_in_bridge(surge));
    // No app viewport: neither the header nor the placeholder is drawn by the app.
    assert!(h.query_by_label(de::PLUGIN_FRAME_BYPASS).is_none());
    // Opened: the frame comes to the front.
    push(
        &script,
        PluginManagerEvent::GuiOpened {
            device: surge,
            size: GuiSize {
                width: 1141,
                height: 711,
            },
            resizable: true,
        },
    );
    h.run_steps(2);
    assert_eq!(script.borrow().activated, vec![surge]);
    assert!(h.state().plugin_frame(surge).unwrap().gui_open);
    // Opening it again only brings it forward.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(2);
    assert_eq!(script.borrow().activated, vec![surge, surge]);
    assert_eq!(script.borrow().targets.len(), 1);
}

#[test]
fn header_clicks_in_the_bridge_frame_are_decided_by_the_app() {
    let (mut h, script, surge) = bridge_frame_harness();
    push(
        &script,
        PluginManagerEvent::GuiOpened {
            device: surge,
            size: GuiSize::default(),
            resizable: false,
        },
    );
    h.run_steps(2);
    let headers_before = script.borrow().headers.len();
    push(
        &script,
        PluginManagerEvent::FrameHeaderAction {
            device: surge,
            event: HeaderEvent::ToggleBypass,
        },
    );
    h.run_steps(3);
    assert!(h.state().project().find_device(surge).unwrap().2.bypass);
    // The new header goes back to the bridge.
    let sent = script.borrow().headers[headers_before..].to_vec();
    let (device, header, lang) = sent.last().cloned().expect("HeaderState after bypass");
    assert_eq!(device, surge);
    assert!(header.bypassed && header.mark == Mark::Bypassed);
    assert_eq!(lang, "de");
    // "Oben halten" is the app's state too.
    push(
        &script,
        PluginManagerEvent::FrameHeaderAction {
            device: surge,
            event: HeaderEvent::KeepOnTop(true),
        },
    );
    h.run_steps(3);
    assert!(h.state().ui().plugin_windows[0].keep_on_top);
    assert!(script.borrow().headers.last().unwrap().1.keep_on_top);
    // "Parameter": the bridge closes its frame, the app shows the parameters.
    push(
        &script,
        PluginManagerEvent::FrameHeaderAction {
            device: surge,
            event: HeaderEvent::View(PluginView::Params),
        },
    );
    h.run_steps(3);
    assert_eq!(script.borrow().closed, vec![surge]);
    assert!(!h.state().plugin_frame_in_bridge(surge));
    assert!(h.query_by_role_and_label(Role::Slider, "Cutoff").is_some());
    // And back: the bridge opens it again.
    h.get_by_role_and_label(Role::RadioButton, de::PLUGIN_FRAME_VIEW_PLUGIN)
        .click();
    h.run_steps(3);
    assert_eq!(script.borrow().targets.len(), 2);
    assert!(h.state().plugin_frame_in_bridge(surge));
}

#[test]
fn keys_of_the_bridge_frame_act_as_in_the_app() {
    let (mut h, script, surge) = bridge_frame_harness();
    // ⌘S taken from the plugin saves; Esc taken from the plugin closes the frame.
    push(
        &script,
        PluginManagerEvent::FrameShortcut {
            device: surge,
            chord: FrameChord::plain("Escape"),
        },
    );
    h.run_steps(3);
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Plugin(PluginAction::CloseWindow(surge)))
    );
    assert!(h.state().ui().plugin_windows.is_empty());
    assert_eq!(script.borrow().closed, vec![surge]);

    // Space in the focused header is the global Play.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(3);
    for pressed in [true, false] {
        push(
            &script,
            PluginManagerEvent::FrameKey {
                device: surge,
                key: FrameKeyEvent {
                    key: "Space".into(),
                    physical_key: Some("Space".into()),
                    pressed,
                    repeat: false,
                    mods: KeyMods::default(),
                },
            },
        );
    }
    h.run_steps(3);
    let plays = h
        .state()
        .dispatched()
        .filter(|a| **a == Action::Transport(komponist_app::actions::TransportAction::TogglePlay))
        .count();
    assert_eq!(plays, 1, "Leertaste in the header is Play, once");
    // The window's close button closes it like ×.
    push(
        &script,
        PluginManagerEvent::FrameCloseRequested { device: surge },
    );
    h.run_steps(3);
    assert!(h.state().ui().plugin_windows.is_empty());
}

#[test]
fn the_bridge_frame_reopens_where_it_was() {
    let (mut h, script, surge) = bridge_frame_harness();
    let rect = FrameRect {
        x: 300.0,
        y: 120.0,
        width: 1141.0,
        height: 739.0,
    };
    push(
        &script,
        PluginManagerEvent::FrameMoved {
            device: surge,
            rect,
        },
    );
    h.run_steps(2);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::CloseWindow(surge)));
    h.run_steps(2);
    // The bridge's answer carries where it really was last.
    let last = FrameRect { x: 310.0, ..rect };
    push(
        &script,
        PluginManagerEvent::GuiClosed {
            device: surge,
            rect: Some(last),
        },
    );
    h.run_steps(2);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(3);
    let target = script.borrow().targets.last().cloned();
    let Some(GuiTarget::OwnFrame { rect: Some(at), .. }) = target else {
        panic!("expected a position, got {target:?}");
    };
    assert_eq!((at.x, at.y), (310.0, 120.0));
}

#[test]
fn a_crash_takes_the_bridge_frame_and_the_app_shows_the_state() {
    let (mut h, script, surge) = bridge_frame_harness();
    assert!(h.state().plugin_frame_in_bridge(surge));
    script.borrow_mut().statuses.insert(
        surge,
        DeviceStatus::Crashed(FailReason::Exited {
            code: None,
            signal: Some(11),
        }),
    );
    h.run_steps(3);
    assert!(!h.state().plugin_frame_in_bridge(surge));
    assert!(h.query_by_label(CRASHED_TITLE).is_some());
    // Running again: the bridge opens its frame again.
    script.borrow_mut().statuses.insert(surge, DeviceStatus::Ok);
    h.run_steps(3);
    assert_eq!(script.borrow().targets.len(), 2);
}

/// The tooltip of the header's × after the hover delay: its rect, or `None` if it did not show.
fn close_tooltip(side: komponist_ui::tooltip::HoverSide) -> (Rect, Option<Rect>) {
    let mut h = frame_harness(
        frame(
            surge_header(),
            BodyModel::Gui {
                native: true,
                name: String::new(),
            },
        ),
        [900.0, 200.0],
        1.0,
    );
    komponist_ui::tooltip::set_hover_side(&h.ctx, side);
    h.run();
    let close = h.get_by_label(de::DIALOG_CLOSE).rect();
    h.hover_at(close.center());
    for _ in 0..240 {
        h.step();
        if let Some(tip) = h.query_by_label_contains("Esc") {
            return (close, Some(tip.rect()));
        }
    }
    (close, None)
}

/// The bridge's frame on macOS (#52): the plugin's `NSView` covers everything below the header,
/// so header tooltips go beside their element, inside the header strip.
#[test]
fn header_tooltips_stay_in_the_header_where_a_native_view_lies_below() {
    use komponist_ui::tooltip::HoverSide;
    let (_, below) = close_tooltip(HoverSide::Below);
    let below = below.expect("the tooltip of × shows");
    assert!(
        below.top() >= metrics::HEADER,
        "below the header: {below:?}"
    );
    let (close, beside) = close_tooltip(HoverSide::Beside);
    let beside = beside.expect("the tooltip of × shows beside it");
    assert!(
        beside.top() >= 0.0 && beside.bottom() <= metrics::HEADER,
        "inside the header strip: {beside:?}"
    );
    assert!(
        beside.right() <= close.left(),
        "left of ×: {beside:?} {close:?}"
    );
}

/// The app became active (Dock icon, ⌘-Tab): the frames the bridges show come forward, not the
/// ones still opening, and nothing takes the activation from the app.
#[test]
fn becoming_active_raises_the_open_bridge_frames() {
    let (mut h, script, surge) = bridge_frame_harness();
    h.state_mut().raise_bridge_frames();
    assert!(script.borrow().raised.is_empty(), "still opening");
    push(
        &script,
        PluginManagerEvent::GuiOpened {
            device: surge,
            size: GuiSize::default(),
            resizable: false,
        },
    );
    push(
        &script,
        PluginManagerEvent::FrameWindow {
            device: surge,
            number: 4711,
        },
    );
    h.run_steps(2);
    let activated = script.borrow().activated.len();
    h.state_mut().raise_bridge_frames();
    assert_eq!(script.borrow().raised, vec![surge]);
    assert_eq!(script.borrow().activated.len(), activated);
}
