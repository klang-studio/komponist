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
use komponist_app::views::devices::model::{Param, all_plugin_params};
use komponist_app::windows::plugin_frame::body::{self, BodyEvent, BodyModel, ParamGrid};
use komponist_app::windows::plugin_frame::header::{self, HeaderEvent, HeaderModel, Mark};
use komponist_app::windows::plugin_frame::metrics;
use komponist_app::{Action, App};
use komponist_i18n::{Lang, de};
use komponist_ipc::clap_presets::{ClapPreset, ClapPresetLocation, ClapPresetSource};
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
use komponist_processor::Processor;

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
        mapping_chip: None,
    }
}

fn hexa_header() -> HeaderModel {
    let host = App::headless_plugin_host();
    let hexa = crate::common::shipped(&host, "hexa");
    let entry = host.catalog().resolve(&hexa).unwrap();
    HeaderModel {
        name: "Hexa".into(),
        sub: "Lead · CLAP".into(),
        presets: entry.clap_presets.iter().map(|p| p.name.clone()).collect(),
        preset: Some(3),
        preset_changed: true,
        view: PluginView::Params,
        has_gui: false,
        ..surge_header()
    }
}

/// A compact grid using the main controls advertised by the Hexa fixture.
fn macro_params() -> Vec<Param> {
    let host = App::headless_plugin_host();
    let plugin = crate::common::shipped(&host, "hexa");
    let (project, _, device) = crate::common::instrument_project("Hexa", plugin);
    let mut app = crate::common::app_with(project, host);
    crate::common::wait_for_layout(&mut app, device);
    let host = app.engine().host();
    let params = all_plugin_params(Lang::De, host.params(device).unwrap(), |id| {
        host.param_value(device, id)
    });
    let ids = host.native_layout(device).unwrap().author_ids().unwrap();
    [
        "algorithm",
        "feedback",
        "volume",
        "glide",
        "voices",
        "mono",
        "spread",
        "tune",
    ]
    .iter()
    .map(|key| params.iter().find(|p| p.wire == ids[*key]).unwrap().clone())
    .collect()
}

fn macro_grid() -> ParamGrid {
    ParamGrid::new(macro_params())
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
    let name = hexa_header().presets[3].clone();
    let field = h.get_by_role_and_label(Role::ComboBox, de::PLUGIN_FRAME_PRESET);
    assert_eq!(field.value().as_deref(), Some(format!("{name} *").as_str()));
}

#[test]
fn knobs_report_plain_values_on_the_step_grid() {
    let params = macro_params();
    let (algorithm, label) = (params[0].wire, params[0].name().to_owned());
    let mut h = frame_harness(
        frame(hexa_header(), BodyModel::Params(ParamGrid::new(params))),
        [900.0, 300.0],
        1.0,
    );
    h.run();
    let knob = h.get_by_role_and_label(Role::Slider, &label);
    let at = knob.rect().center();
    h.drag_at(at);
    h.run();
    h.hover_at(at - vec2(0.0, 40.0));
    h.run();
    h.drop_at(at - vec2(0.0, 40.0));
    h.run();
    let events = &h.state().body_events;
    assert!(!events.is_empty(), "the drag set the parameter");
    let mut sets = 0;
    for e in events {
        match *e {
            // The drag is one undo gesture around the values.
            BodyEvent::BeginGesture { id } => assert_eq!(id, algorithm),
            BodyEvent::EndGesture => {}
            BodyEvent::ControlAction(ref action) => panic!("unexpected macro action {action:?}"),
            BodyEvent::SetParam { id, value } => {
                assert_eq!(id, algorithm);
                assert_eq!(value.fract(), 0.0, "stepped: {value}");
                sets += 1;
            }
            ref other => panic!("{other:?}"),
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
    new_presets: Option<Vec<ClapPreset>>,
    preset_loads: Vec<ClapPresetSource>,
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
    native_layout: Option<std::sync::Arc<komponist_ipc::native_layout::NativeLayout>>,
    presets: Vec<ClapPreset>,
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
            if !komponist_plugins::requires_bundled(&device.plugin) {
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
        let mut script = self.script.borrow_mut();
        if let Some(presets) = script.new_presets.take() {
            self.presets = presets;
        }
        std::mem::take(&mut script.events)
    }
    fn clap_presets(&self, device: &Device) -> &[ClapPreset] {
        if self.known(device.id) {
            &self.presets
        } else {
            self.catalog
                .resolve(&device.plugin)
                .filter(|entry| komponist_plugins::requires_bundled(&entry.plugin))
                .map_or(&[], |entry| entry.clap_presets.as_slice())
        }
    }
    fn load_clap_preset(&mut self, device: &Device, source: &ClapPresetSource) -> bool {
        if !self.known(device.id) || !self.presets.iter().any(|entry| &entry.source == source) {
            return false;
        }
        self.script.borrow_mut().preset_loads.push(source.clone());
        true
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
    fn native_layout(
        &self,
        device: DeviceId,
    ) -> Option<&std::sync::Arc<komponist_ipc::native_layout::NativeLayout>> {
        self.known(device).then_some(())?;
        self.native_layout.as_ref()
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
    scripted_app(None)
}

fn scripted_app(layout: Option<&[u8]>) -> (App, Rc<RefCell<Script>>, DeviceId) {
    let project = demo_project();
    let surge = project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .find(|d| d.plugin.name == "Surge XT")
        .unwrap();
    let host = App::headless_plugin_host();
    let catalog = crate::common::fixture_catalog(
        host.catalog(),
        &[PluginDescriptor {
            localized: None,
            format: BundleFormat::Clap,
            id: surge.plugin.id.clone(),
            name: surge.plugin.name.clone(),
            vendor: surge.plugin.vendor.clone(),
            version: surge.plugin.version.clone(),
            kind: PluginKind::Instrument,
            features: Vec::new(),
            bundle: "/usr/lib/clap/Surge XT.clap".into(),
            sidechain: false,
        }],
    );
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
        native_layout: layout.map(|bytes| {
            std::sync::Arc::new(komponist_ipc::native_layout::NativeLayout::parse(bytes).unwrap())
        }),
        presets: Vec::new(),
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
fn two_frame_preset_steps_in_one_event_batch_advance_twice() {
    let (mut h, script, device) = bridge_frame_harness();
    let plugin_id = h
        .state()
        .project()
        .find_device(device)
        .unwrap()
        .2
        .plugin
        .id
        .clone();
    let preset = |name: &str| ClapPreset {
        plugin_id: plugin_id.clone(),
        source: ClapPresetSource {
            location: ClapPresetLocation::Plugin,
            load_key: Some(name.as_bytes().to_vec()),
        },
        name: name.into(),
        localized_names: Default::default(),
        features: Vec::new(),
        factory: true,
    };
    let (a, b, c) = (preset("A"), preset("B"), preset("C"));
    script.borrow_mut().new_presets = Some(vec![a.clone(), b, c.clone()]);
    h.run_steps(2);
    push(
        &script,
        PluginManagerEvent::FrameHeaderAction {
            device,
            event: HeaderEvent::Preset(0),
        },
    );
    h.run_steps(3);
    let loaded = |n: u8, source: &ClapPresetSource| PluginManagerEvent::ClapPresetLoaded {
        device,
        source: Some(source.clone()),
        previous_state: std::sync::Arc::from([n]),
        state: std::sync::Arc::from([n + 1]),
    };
    push(&script, loaded(0, &a.source));
    h.run_steps(3);
    for _ in 0..2 {
        push(
            &script,
            PluginManagerEvent::FrameHeaderAction {
                device,
                event: HeaderEvent::PresetNext,
            },
        );
    }
    h.run_steps(3);
    // Both steps land on one load of the preset two further on.
    assert_eq!(
        script.borrow().preset_loads,
        [a.source.clone(), c.source.clone()]
    );
    push(&script, loaded(1, &c.source));
    h.run_steps(3);
    let script = script.borrow();
    let (_, header, _) = script.headers.last().unwrap();
    assert_eq!(header.preset, Some(2));
}

#[test]
fn frame_keeps_queued_and_applied_preset_identity_when_discovery_reorders_entries() {
    let (mut h, script, device) = bridge_frame_harness();
    let plugin_id = h
        .state()
        .project()
        .find_device(device)
        .unwrap()
        .2
        .plugin
        .id
        .clone();
    let preset = |name: &str| ClapPreset {
        plugin_id: plugin_id.clone(),
        source: ClapPresetSource {
            location: ClapPresetLocation::Plugin,
            load_key: Some(name.as_bytes().to_vec()),
        },
        name: name.into(),
        localized_names: Default::default(),
        features: Vec::new(),
        factory: true,
    };
    let (a, b, inserted) = (preset("A"), preset("B"), preset("Inserted"));
    script.borrow_mut().new_presets = Some(vec![a.clone(), b.clone()]);
    h.run_steps(2);
    push(
        &script,
        PluginManagerEvent::FrameHeaderAction {
            device,
            event: HeaderEvent::Preset(0),
        },
    );
    h.run_steps(2);
    push(
        &script,
        PluginManagerEvent::FrameHeaderAction {
            device,
            event: HeaderEvent::PresetNext,
        },
    );
    h.run_steps(2);
    assert_eq!(
        script.borrow().preset_loads,
        std::slice::from_ref(&a.source)
    );
    script.borrow_mut().new_presets = Some(vec![inserted, a.clone(), b.clone()]);
    push(
        &script,
        PluginManagerEvent::ClapPresetLoaded {
            device,
            source: Some(a.source.clone()),
            previous_state: std::sync::Arc::from([1_u8]),
            state: std::sync::Arc::from([2_u8]),
        },
    );
    h.run_steps(2);
    let script = script.borrow();
    assert_eq!(script.preset_loads, [a.source, b.source]);
    let (_, header, _) = script.headers.last().unwrap();
    assert_eq!(header.preset, Some(1));
    assert_eq!(header.presets[1], "A");
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
fn bridge_frame_shows_mapping_and_escape_first_ends_it() {
    use komponist_app::views::track_macros::Action as Macro;
    use komponist_model::ChannelRef;

    let (mut h, script, surge) = bridge_frame_harness();
    let (ChannelRef::Track(track), _, _) = h.state().project().find_device(surge).unwrap() else {
        panic!("Surge belongs to a track");
    };
    push(
        &script,
        PluginManagerEvent::GuiOpened {
            device: surge,
            size: GuiSize::default(),
            resizable: false,
        },
    );
    h.run_steps(3);
    h.state_mut()
        .dispatch(Action::Macro(Macro::ToggleMapping { track }));
    h.run_steps(3);
    let expected = Lang::De.macros_map_chip(Lang::De.macros_slot(1));
    assert_eq!(
        script
            .borrow()
            .headers
            .last()
            .unwrap()
            .1
            .mapping_chip
            .as_deref(),
        Some(expected.as_str())
    );
    for _ in 0..2 {
        push(
            &script,
            PluginManagerEvent::FrameShortcut {
                device: surge,
                chord: FrameChord::plain("Escape"),
            },
        );
    }
    h.run_steps(3);
    assert_eq!(h.state().ui().track_macros.mapping, None);
    assert!(h.state().plugin_frame_in_bridge(surge));
    assert!(script.borrow().closed.is_empty());
    assert!(
        script
            .borrow()
            .headers
            .last()
            .unwrap()
            .1
            .mapping_chip
            .is_none()
    );
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

// ------------------------------------------------------------------------------------------------
// The Window, Dock and "⋯" menus (#95)
// ------------------------------------------------------------------------------------------------

#[test]
fn the_window_list_names_the_open_windows_in_project_order() {
    use komponist_app::ui_state::PluginWindow;
    use komponist_app::windows::window_list::{self, WindowEntry};

    let mut project = demo_project();
    let mut on_master = Device::clone(&project.tracks[3].channel.devices[0]);
    on_master.id = DeviceId(9_000);
    std::sync::Arc::make_mut(&mut project.master)
        .devices
        .push(std::sync::Arc::new(on_master));
    let id = |track: usize, slot: usize| project.tracks[track].channel.devices[slot].id;
    let (surge, hexa, eq) = (id(3, 0), id(1, 0), id(1, 1));
    // Opened master first, then Surge XT, Hexa, the Bass EQ; one window of a device that is gone.
    let windows: Vec<PluginWindow> = [DeviceId(9_000), surge, hexa, eq, DeviceId(77_777)]
        .into_iter()
        .map(|d| PluginWindow::new(d, PluginView::Params, None))
        .collect();
    let entry = |device, label: &str| WindowEntry {
        device,
        label: label.to_owned(),
    };
    assert_eq!(
        window_list::entries(Lang::De, &project, &windows, &Catalog::default()),
        vec![
            entry(hexa, "Hexa · Bass"),
            entry(eq, "LSP Para EQ x8 · Bass"),
            entry(surge, "Surge XT · Pad"),
            entry(DeviceId(9_000), "Surge XT · Master"),
        ]
    );
    let en = window_list::entries(Lang::En, &project, &windows, &Catalog::default());
    assert_eq!(en[0].label, "Hexa · Bass");
    assert!(window_list::entries(Lang::De, &project, &[], &Catalog::default()).is_empty());
}

#[test]
fn the_window_menu_has_minimize_zoom_and_the_plugin_windows() {
    use komponist_app::windows::window_list::{self, WindowEntry, WindowMenuItem};

    assert_eq!(
        window_list::window_menu(Lang::De, &[]),
        vec![
            WindowMenuItem::Minimize("Im Dock ablegen"),
            WindowMenuItem::Zoom("Zoomen"),
        ]
    );
    let surge = WindowEntry {
        device: DeviceId(4),
        label: "Surge XT · Pad".into(),
    };
    assert_eq!(
        window_list::window_menu(Lang::En, std::slice::from_ref(&surge)),
        vec![
            WindowMenuItem::Minimize("Minimize"),
            WindowMenuItem::Zoom("Zoom"),
            WindowMenuItem::Separator,
            WindowMenuItem::Window(&surge),
        ]
    );
    assert_eq!(Lang::De.app_menu_window(), "Fenster");
    assert_eq!(Lang::En.app_menu_window(), "Window");
}

#[test]
fn the_app_lists_its_plugin_windows_as_they_open_and_close() {
    let (app, _, surge) = app();
    let mut h = harness(app);
    assert!(h.state().plugin_window_entries().is_empty());
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(surge)));
    h.run_steps(2);
    let entries = h.state().plugin_window_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].device, surge);
    assert_eq!(entries[0].label, "Surge XT · Pad");
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::CloseWindow(surge)));
    h.run_steps(2);
    assert!(h.state().plugin_window_entries().is_empty());
}

#[test]
fn a_window_picked_in_a_menu_comes_to_the_front() {
    let (mut h, script, surge) = bridge_frame_harness();
    // Still opening: `GuiOpened` brings it forward, the pick adds nothing.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::FocusWindow(surge)));
    h.run_steps(2);
    assert!(script.borrow().activated.is_empty());
    push(
        &script,
        PluginManagerEvent::GuiOpened {
            device: surge,
            size: GuiSize::default(),
            resizable: false,
        },
    );
    h.run_steps(2);
    assert_eq!(script.borrow().activated, vec![surge]);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::FocusWindow(surge)));
    h.run_steps(2);
    assert_eq!(script.borrow().activated, vec![surge, surge]);
    assert_eq!(script.borrow().targets.len(), 1, "no second OpenGui");
    // A window that closed meanwhile (a stale menu entry) is not opened again.
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::CloseWindow(surge)));
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::FocusWindow(surge)));
    h.run_steps(2);
    assert!(h.state().ui().plugin_windows.is_empty());
    assert_eq!(script.borrow().activated.len(), 2);
}

#[test]
fn failure_to_open_the_plugin_editor_shows_the_native_layout_instead() {
    let layout = br#"{
        "default_locale":"en","locales":["en"],
        "pages":[
            {"key":"main","labels":{"en":"Main"},"columns":4,"cells":[
                {"widget":"number","row":0,"col":0,"rows":1,"cols":1,"half":"top","param_id":7,
                 "labels":{"en":"Native cutoff"}}
            ]},
            {"key":"other","labels":{"en":"Other"},"columns":4,"cells":[
                {"widget":"number","row":0,"col":0,"rows":1,"cols":1,"half":"top","param_id":7,
                 "labels":{"en":"Alternate cutoff"}}
            ]}
        ]
    }"#;
    let (mut app, script, device) = scripted_app(Some(layout));
    app.use_bridge_frames(true);
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::OpenWindow(device)));
    h.run_steps(3);
    assert_eq!(script.borrow().targets.len(), 1);
    push(
        &script,
        PluginManagerEvent::GuiClosed { device, rect: None },
    );
    h.run_steps(4);
    let window = h
        .state()
        .ui()
        .plugin_windows
        .iter()
        .find(|w| w.device == device)
        .unwrap();
    assert_eq!(window.view, PluginView::Params);
    let first = h
        .get_by_role_and_label(Role::SpinButton, "Native cutoff")
        .accesskit_node()
        .id();
    h.get_by_label("Other").click_accesskit();
    h.run_steps(3);
    let second = h
        .get_by_role_and_label(Role::SpinButton, "Alternate cutoff")
        .accesskit_node()
        .id();
    assert_ne!(
        first, second,
        "number entry state must be scoped to the page"
    );
}
