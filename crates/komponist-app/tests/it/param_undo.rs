//! Device parameters are undoable (owner decision 2026-11-15): a knob drag, a wheel burst, a
//! sample load or a plugin's snapshot is one undo step, and undo, redo and a cancelled gesture
//! restore the document **and** the running processor. The engine is held and rendered by the
//! test, so the checks listen to the processors.
//!
//! Gestures are driven two ways: through the real hexa card (a drag and a wheel burst on its
//! knob, which checks the cells' wiring) and as the action sequence the cells send
//! (`BeginParamGesture`, `SetParam` per frame, `EndGesture`).

use crate::common::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, EditAction, PluginAction, ProjectAction, UiAction};
use komponist_audiofile::fixtures as testkit;
use komponist_engine::Command;
use komponist_model::edits::{self, NewDevice, NewTrack};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{ChannelRef, DeviceId, Project, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};
use komponist_session::device_host::PluginHost;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

// ------------------------------------------------------------------------------------------------
// Helpers

fn begin(app: &mut App, device: DeviceId, id: u32) {
    app.dispatch(Action::Plugin(PluginAction::BeginParamGesture {
        device,
        id,
    }));
}

fn end(app: &mut App) {
    app.dispatch(Action::Edit(EditAction::EndGesture));
}

fn set(app: &mut App, device: DeviceId, id: u32, value: f64) {
    app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
}

/// A drag as the card sends it: a gesture and one `SetParam` per frame, the frame stepping the
/// app (which syncs the engine) in between.
fn drag_param(app: &mut App, device: DeviceId, id: u32, values: &[f64]) {
    begin(app, device, id);
    for v in values {
        set(app, device, id, *v);
        app.step();
    }
    end(app);
    app.step();
}

/// How many steps `app` can undo (undoes them all and redoes them again; leaves the engine
/// following the document).
fn depth(app: &mut App) -> usize {
    let mut n = 0;
    while app.doc().undo_label().is_some() {
        app.dispatch(Action::Edit(EditAction::Undo));
        n += 1;
    }
    for _ in 0..n {
        app.dispatch(Action::Edit(EditAction::Redo));
    }
    app.step();
    n
}

fn undo(app: &mut App) {
    app.dispatch(Action::Edit(EditAction::Undo));
    app.step();
}

fn redo(app: &mut App) {
    app.dispatch(Action::Edit(EditAction::Redo));
    app.step();
}

fn state_of(app: &App, device: DeviceId) -> Option<Arc<[u8]>> {
    app.project()
        .find_device(device)
        .and_then(|(_, _, d)| d.state.clone())
}

/// One live note on `track`, rendered for a while: the peak of the output; the note is released
/// and its tail rendered, so the next one starts from silence.
fn note_peak(app: &mut App, track: TrackId, key: u8) -> f32 {
    let sync = app.engine_mut();
    sync.send(Command::LiveNote {
        track,
        key,
        velocity: 100,
    });
    let (l, r) = sync.run_held_capture(9600, &[], 0).expect("held engine");
    sync.send(Command::LiveNote {
        track,
        key,
        velocity: 0,
    });
    sync.run_held(96_000, &[], 0);
    l.iter().chain(&r).fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn hold(app: &mut App) {
    app.step();
    app.engine_mut().hold_engine().expect("engine held");
}

fn hexa_app(preset: &str) -> (App, TrackId, DeviceId) {
    let host = komponist_session::device_host::PluginHost::headless_fixture();
    let prepared = host
        .manager()
        .prepare_bundled_device("org.klang-studio.komponist.hexa", Some(preset), &[], &[])
        .unwrap();
    let (mut project, track, device) = instrument_project("Synth", prepared.plugin.clone());
    Arc::make_mut(&mut project.tracks[0]).channel.devices[0] =
        Arc::new(prepared.into_device(device));
    let mut app = app_with(project, host);
    wait_for_layout(&mut app, device);
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    (app, track, device)
}

fn peak(app: &mut App, track: TrackId) -> f32 {
    hold(app);
    let result = note_peak(app, track, 60);
    app.engine_mut().release_engine();
    result
}

fn capture(app: &mut App, device: DeviceId) -> Arc<[u8]> {
    super::bundled_synth_ui::capture_document_state(
        app,
        device,
        "the parameter edit in the project",
    )
    .bytes
}

#[test]
fn a_hexa_drag_is_one_step_that_never_recreates_the_processor_and_undo_follows_it() {
    let (mut app, track, synth) = hexa_app("E-Piano");
    let before = state_of(&app, synth);
    let original_volume = author_base(&app, synth, "volume");
    let quiet = peak(&mut app, track);
    assert!(quiet > 1e-4);

    hold(&mut app);
    app.engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 100,
    });
    assert!(app.engine_mut().run_held(4800, &[], 0));
    let reloads = app.engine().device_reloads();
    let volume = author_parameter(&app, synth, "volume");
    let values: Vec<_> = (0..5).map(|index| 0.9 - 0.05 * f64::from(index)).collect();
    drag_param(&mut app, synth, volume, &values);
    let (l, r) = app.engine_mut().run_held_capture(4800, &[], 0).unwrap();
    let held = l
        .iter()
        .chain(&r)
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(held > 1e-4, "the note sounds on after the drag: {held}");
    app.engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 0,
    });
    app.engine_mut().run_held(96_000, &[], 0);
    app.engine_mut().release_engine();

    let after = capture(&mut app, synth);
    assert_ne!(Some(&after), before.as_ref());
    assert_eq!(app.undo_text().as_deref(), Some("Volume ändern"));
    let loud = peak(&mut app, track);
    assert!(loud > quiet * 1.2, "{quiet} → {loud}");

    undo(&mut app);
    assert!(app.undo_text().is_none(), "one gesture made one step");
    wait_for_param_value(&mut app, synth, volume, original_volume);
    assert!(peak(&mut app, track) > 1e-4, "undo still plays");
    redo(&mut app);
    wait_for_param_value(&mut app, synth, volume, *values.last().unwrap());
    assert!(peak(&mut app, track) > 1e-4, "redo still plays");
    assert_eq!(app.engine().device_reloads(), reloads);
}

#[test]
fn a_preset_and_a_discrete_edit_are_separate_undo_steps() {
    let (mut app, _, synth) = hexa_app("Init");
    capture(&mut app, synth);
    let baseline = app.undo_text();
    let preset_name = |app: &App| {
        let device = app.project().find_device(synth).unwrap().2;
        let host = app.engine().host();
        let inventory = host.clap_presets(device);
        let (index, _) = komponist_session::device_host::selected_preset(device, inventory)?;
        Some(inventory[index].name.clone())
    };
    let original_preset = preset_name(&app);
    let init_ratio = author_base(&app, synth, "op2_ratio");
    let device = app.project().find_device(synth).unwrap().2;
    let source = app
        .engine()
        .host()
        .clap_presets(device)
        .iter()
        .find(|preset| preset.name == "E-Piano")
        .unwrap()
        .source
        .clone();
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device: synth,
        source,
    }));
    step_until(&mut app, "the Hexa preset in the document", |app| {
        preset_name(app).as_deref() == Some("E-Piano")
    });
    let preset_ratio = author_base(&app, synth, "op2_ratio");
    let preset = state_of(&app, synth);
    let ratio = author_parameter(&app, synth, "op2_ratio");
    set(&mut app, synth, ratio, 0.7);
    let edited = capture(&mut app, synth);
    assert_ne!(Some(&edited), preset.as_ref());

    undo(&mut app);
    wait_for_param_value(&mut app, synth, ratio, preset_ratio);
    undo(&mut app);
    wait_for_param_value(&mut app, synth, ratio, init_ratio);
    assert_eq!(preset_name(&app), original_preset);
    assert_eq!(app.undo_text(), baseline);
}

#[test]
fn escape_during_a_drag_puts_the_value_back_in_the_document_and_the_engine() {
    let (mut app, track, synth) = hexa_app("E-Piano");
    let before = state_of(&app, synth);
    let volume = author_parameter(&app, synth, "volume");
    let original_volume = author_base(&app, synth, "volume");
    let quiet = peak(&mut app, track);

    begin(&mut app, synth, volume);
    for value in [0.3, 0.2, 0.1] {
        set(&mut app, synth, volume, value);
        wait_for_param_value(&mut app, synth, volume, value);
    }
    app.dispatch(Action::Edit(EditAction::CancelGesture));
    wait_for_param_value(&mut app, synth, volume, original_volume);
    assert!(!app.doc().in_gesture());
    assert_eq!(state_of(&app, synth), before);
    assert!(quiet > 1e-4 && peak(&mut app, track) > 1e-4);
    assert!(app.undo_text().is_none());
}

// ------------------------------------------------------------------------------------------------
// The real card: a knob drag and a wheel burst (the cells' gesture wiring)

fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(W, H))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    press(h, from, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, false);
    h.run_steps(3);
}

fn wheel(h: &mut Harness<'_, App>, pos: Pos2, notches: &[f32]) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for n in notches {
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, *n),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(2);
    }
}

fn knob_rect(h: &Harness<'_, App>, name: &str) -> egui::Rect {
    h.get_by_role_and_label(Role::Group, "Gerät Hexa")
        .get_by_role_and_label(Role::Slider, name)
        .rect()
}

#[test]
fn dragging_a_card_knob_and_a_wheel_burst_are_one_undo_step_each() {
    let (app, _, synth) = hexa_app("E-Piano");
    let before = state_of(&app, synth);
    let mut h = harness(app);
    let reloads = h.state().engine().device_reloads();

    let knob = knob_rect(&h, "Glide");
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    let dragged = capture(h.state_mut(), synth);
    assert_ne!(Some(&dragged), before.as_ref());
    assert_eq!(h.state().undo_text().as_deref(), Some("Glide ändern"));
    assert_eq!(h.state().engine().device_reloads(), reloads);

    let tune = knob_rect(&h, "Tune");
    wheel(&mut h, tune.center(), &[1.0, 1.0, 1.0]);
    h.run_steps(60);
    step_for(h.state_mut(), Duration::from_millis(400));
    let wheeled = capture(h.state_mut(), synth);
    assert_ne!(wheeled, dragged);
    assert_eq!(h.state().undo_text().as_deref(), Some("Tune ändern"));

    undo(h.state_mut());
    assert_eq!(state_of(h.state(), synth), Some(dragged));
    undo(h.state_mut());
    assert_eq!(state_of(h.state(), synth), before);
    assert!(h.state().undo_text().is_none());
}

// ------------------------------------------------------------------------------------------------
// The shipped Limiter: a ceiling gesture audibly follows undo and redo.

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn packaged_limiter_drag_is_one_step_and_undo_redo_restore_the_sound() {
    let launcher = PathBuf::from(
        std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").expect("packaged launcher"),
    );
    let work = tempfile::tempdir().unwrap();
    let host = super::bundled_current::scan(&launcher, work.path());
    let hexa = host
        .manager()
        .prepare_bundled_device("org.klang-studio.komponist.hexa", Some("E-Piano"), &[], &[])
        .unwrap();
    // Drive the limiter hard enough to reach its ceiling. The CLAP gain control is normalized.
    let limiter = host
        .manager()
        .prepare_bundled_device(
            "org.klang-studio.komponist.limiter",
            None,
            &[("gain", 1.0)],
            &[],
        )
        .unwrap();
    let (project, track) = edits::add_track(
        &Project::new("Limiter undo"),
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(hexa.plugin),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let synth = project
        .track(track)
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1
        .id;
    let project = edits::set_device_state(&project, synth, hexa.state).unwrap();
    let (project, limiter) = edits::insert_device(
        &project,
        ChannelRef::Master,
        0,
        NewDevice {
            role: limiter.role,
            plugin: limiter.plugin,
            state: limiter.state,
            sidechain: None,
            preset: None,
        },
    )
    .unwrap();
    let mut app = app_with(project, host);
    step_until(&mut app, "both authenticated devices", |app| {
        let host = app.engine().host();
        [synth, limiter].into_iter().all(|id| {
            host.status(id) == DeviceStatus::Ok
                && host.verified_bundle(id).is_some()
                && host.native_layout(id).is_some()
        })
    });
    for id in [synth, limiter] {
        assert!(app.engine().host().bridge_pid(id).is_none());
    }

    let ceiling = app
        .engine()
        .host()
        .native_layout(limiter)
        .unwrap()
        .author_ids()
        .unwrap()["ceiling"];
    let original_ceiling = app
        .engine()
        .host()
        .param_value(limiter, ceiling)
        .unwrap()
        .value;
    let before = state_of(&app, limiter);
    let peak = |app: &mut App| {
        // Hold only for the probe. The live callback keeps running while CLAP applies edits
        // and publishes their state.
        hold(app);
        let peak = note_peak(app, track, 60);
        app.engine_mut().release_engine();
        peak
    };
    let loud = peak(&mut app);
    let reloads = app.engine().device_reloads();

    // The ceiling's normalized CLAP domain is -12 dB to 0 dB.
    drag_param(&mut app, limiter, ceiling, &[0.75, 0.5, 0.25, 0.0]);
    let after = super::bundled_synth_ui::capture_document_state(
        &mut app,
        limiter,
        "the final ceiling captured in the document",
    );
    assert_ne!(before.as_ref(), Some(&after.bytes));
    let quiet = peak(&mut app);
    assert!(
        (0.2..=0.26).contains(&quiet) && loud > quiet * 1.5,
        "{loud} → {quiet}"
    );
    let label = app.undo_text().unwrap();
    assert!(label.ends_with(" ändern"), "{label}");

    undo(&mut app);
    assert_eq!(app.doc().undo_label(), None, "the drag made one undo step");
    assert_eq!(state_of(&app, limiter), before);
    wait_for_param_value(&mut app, limiter, ceiling, original_ceiling);
    assert!(peak(&mut app) > quiet * 1.5, "undo: the old ceiling");

    redo(&mut app);
    assert!(
        app.project()
            .find_device(limiter)
            .unwrap()
            .2
            .has_saved_state(&after)
    );
    wait_for_param_value(&mut app, limiter, ceiling, 0.0);
    let redone = peak(&mut app);
    assert!(
        (0.2..=0.26).contains(&redone),
        "redo: the new ceiling, peak {redone}"
    );
    assert_eq!(app.engine().device_reloads(), reloads);
}

// ------------------------------------------------------------------------------------------------
// The Sampler: saving rewrites the sample paths of every undo step, each from its own state (#86)

/// The synthesized kit written into `dir` (created).
fn kit_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    testkit::write_kit(dir).unwrap()
}

/// Opens a current-format project whose sampler links a real kit, including one missing file.
fn opened_sampler_app(kit: &Path, file: &Path) -> Option<(App, DeviceId, u32)> {
    let dir = file.parent().unwrap();
    let (host, plugin) = native_fixture(dir, "klopfer.clap", "org.klang-studio.komponist.sampler")?;
    let (project, track) = edits::add_track(
        &Project::new("Song"),
        NewTrack {
            name: Some("Drums".into()),
            hue: None,
            instrument: Some(plugin),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let drums = project
        .track(track)
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1
        .id;
    let files = kit_in(kit);
    let links = files
        .iter()
        .enumerate()
        .map(|(slot, path)| (slot as u32, AssetLink::new(path, None)))
        .collect();
    let project = edits::set_device_asset_links(&project, drums, links).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    komponist_app::project_io::save_project(file, &project).unwrap();
    std::fs::remove_file(&files[7]).unwrap();

    let mut app = app_with(Project::new("leer"), host);
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.to_owned())));
    step_until(&mut app, "the sampler layout and asset slots", |app| {
        let host = app.engine().host();
        host.status(drums) == DeviceStatus::Ok
            && host.native_layout(drums).is_some()
            && host.asset_slots(drums).is_some()
    });
    let tune = app
        .engine()
        .host()
        .native_layout(drums)
        .unwrap()
        .author_ids()
        .unwrap()["pad1_tune"];
    // Let the initial snapshot establish the undo baseline before sending a parameter edit.
    plugin_state(&mut app, drums);
    assert_eq!(app.doc().undo_label(), None);
    assert_eq!(
        app.engine().host().param_base(drums, tune),
        Some(TUNE_DEFAULT)
    );
    Some((app, drums, tune))
}

const TUNE_DEFAULT: f64 = 0.5;
const TUNE_OCTAVE_UP: f64 = 0.75; // The CLAP tuning parameter is normalized from -24 to +24 st.

fn asset_links(app: &App, drums: DeviceId) -> &BTreeMap<u32, AssetLink> {
    &app.project().find_device(drums).unwrap().2.asset_links
}

fn asset_link(app: &App, drums: DeviceId, slot: u32) -> &AssetLink {
    &asset_links(app, drums)[&slot]
}

fn wait_for_param_value(app: &mut App, device: DeviceId, id: u32, value: f64) {
    step_until(app, "the live parameter value", |app| {
        app.engine()
            .host()
            .param_value(device, id)
            .is_some_and(|param| (param.value - value).abs() < 1e-6)
    });
}

fn tune_and_relink(app: &mut App, drums: DeviceId, tune: u32, other: &Path) {
    let before = state_of(app, drums);
    set(app, drums, tune, TUNE_OCTAVE_UP);
    wait_for_param_value(app, drums, tune, TUNE_OCTAVE_UP);
    step_until(app, "the tuning edit was saved in the document", |app| {
        state_of(app, drums).is_some_and(|state| Some(state) != before)
    });
    app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
        device: drums,
        slot: 1,
        path: Some(other.to_owned()),
    }));
    step_until(app, "the sampler file edit was committed", |app| {
        asset_link(app, drums, 1).abs == other
    });
}

fn kit_file(pad: usize) -> String {
    format!("{}.wav", testkit::NAMES[pad])
}

fn assert_missing_link(app: &App, drums: DeviceId, original: &AssetLink, relative: &Path) {
    let link = asset_link(app, drums, 7);
    assert_eq!(link.abs, original.abs);
    assert_eq!(link.rel, relative);
    assert!(
        link.resolve(app.project_path().and_then(Path::parent))
            .is_none()
    );
}

#[test]
fn a_file_edit_right_after_a_knob_turn_waits_for_the_snapshot_instead_of_failing() {
    let dir = scratch("param-undo-edit-after-knob");
    let one = dir.join("one");
    let Some((mut app, drums, tune)) =
        opened_sampler_app(&one.join("Kit"), &one.join("Song.komponist"))
    else {
        return;
    };
    let other = kit_in(&one.join("More"))[3].clone();
    set(&mut app, drums, tune, TUNE_OCTAVE_UP);
    app.step();
    // No wait for the live value or the snapshot: the edit arrives while both are in flight.
    app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
        device: drums,
        slot: 1,
        path: Some(other.clone()),
    }));
    step_until(&mut app, "the sampler file edit was committed", |app| {
        asset_link(app, drums, 1).abs == other
    });
    wait_for_param_value(&mut app, drums, tune, TUNE_OCTAVE_UP);
    drop(app);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn undo_after_save_restores_the_samplers_parameters_and_samples() {
    let dir = scratch("param-undo-save-rebase");
    let one = dir.join("one");
    let Some((mut app, drums, tune)) =
        opened_sampler_app(&one.join("Kit"), &one.join("Song.komponist"))
    else {
        return;
    };
    let first = asset_links(&app, drums).clone();
    assert_eq!(first.len(), 8);
    let other = kit_in(&one.join("More"))[3].clone();
    tune_and_relink(&mut app, drums, tune, &other);

    // Save rebases absolute-only fixture links in the current state and in undo history.
    app.dispatch(Action::Project(ProjectAction::Save));
    assert!(!app.doc().is_dirty());
    let kick = Path::new("Kit").join(kit_file(0));
    let snare = Path::new("Kit").join(kit_file(1));
    let missing = Path::new("Kit").join(kit_file(7));
    assert_eq!(asset_link(&app, drums, 0).rel, kick);
    assert_eq!(
        asset_link(&app, drums, 1).rel,
        Path::new("More").join(kit_file(3))
    );

    undo(&mut app);
    assert_eq!(asset_link(&app, drums, 1).abs, first[&1].abs);
    assert_eq!(asset_link(&app, drums, 1).rel, snare);
    wait_for_param_value(&mut app, drums, tune, TUNE_OCTAVE_UP);
    undo(&mut app);
    wait_for_param_value(&mut app, drums, tune, TUNE_DEFAULT);
    assert_eq!(app.doc().undo_label(), None);
    assert_eq!(asset_link(&app, drums, 0).rel, kick);
    assert_missing_link(&app, drums, &first[&7], &missing);

    redo(&mut app);
    wait_for_param_value(&mut app, drums, tune, TUNE_OCTAVE_UP);
    assert_eq!(asset_link(&app, drums, 1).abs, first[&1].abs);
    redo(&mut app);
    assert_eq!(asset_link(&app, drums, 1).abs, other);
    assert_missing_link(&app, drums, &first[&7], &missing);
    drop(app);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn undo_after_save_as_into_another_folder_resolves_the_old_steps_links_from_there() {
    let dir = scratch("param-undo-save-as-rebase");
    let one = dir.join("one");
    let Some((mut app, drums, tune)) =
        opened_sampler_app(&one.join("Kit"), &one.join("Song.komponist"))
    else {
        return;
    };
    let first = asset_links(&app, drums).clone();
    let other = kit_in(&one.join("More"))[3].clone();
    tune_and_relink(&mut app, drums, tune, &other);

    let two = dir.join("two");
    std::fs::create_dir_all(&two).unwrap();
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::SaveAs));
    h.run_steps(3);
    h.get_by_label("Dateiname").focus();
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Dateiname")
        .type_text(two.join("Kopie").to_str().unwrap());
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    let app = h.state_mut();
    assert_eq!(
        app.project_path(),
        Some(two.join("Kopie.komponist").as_path())
    );
    assert_eq!(
        asset_link(app, drums, 0).rel,
        Path::new("../one/Kit").join(kit_file(0))
    );

    undo(app);
    wait_for_param_value(app, drums, tune, TUNE_OCTAVE_UP);
    let pad2 = asset_link(app, drums, 1);
    assert_eq!(pad2.abs, first[&1].abs);
    let relative_only = AssetLink {
        abs: PathBuf::new(),
        ..pad2.clone()
    };
    assert_eq!(
        relative_only.resolve(Some(&two)),
        Some(first[&1].abs.clone())
    );
    undo(app);
    wait_for_param_value(app, drums, tune, TUNE_DEFAULT);
    assert_missing_link(
        app,
        drums,
        &first[&7],
        &Path::new("../one/Kit").join(kit_file(7)),
    );
    drop(h);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn undo_after_collect_and_save_keeps_each_steps_samples_and_copies_only_the_current_ones() {
    let dir = scratch("param-undo-collect");
    let library = dir.join("library");
    let home = dir.join("proj");
    let Some((mut app, drums, tune)) = opened_sampler_app(&library, &home.join("Song.komponist"))
    else {
        return;
    };
    let first = asset_links(&app, drums).clone();
    let other = kit_in(&dir.join("more"))[3].clone();
    tune_and_relink(&mut app, drums, tune, &other);

    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    assert!(!app.doc().is_dirty());
    let folder = home.join("Song Samples");
    let mut copied: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    copied.sort();
    // The replacement matches pad 4, which shares its copy. The old pad 2 sample and
    // the missing pad 8 sample are absent from the collected folder.
    let mut expected: Vec<String> = [0, 2, 3, 4, 5, 6].into_iter().map(kit_file).collect();
    expected.sort();
    assert_eq!(copied, expected);
    let other_copy = folder.join(kit_file(3));
    assert_eq!(asset_link(&app, drums, 1).abs, other_copy);

    undo(&mut app);
    wait_for_param_value(&mut app, drums, tune, TUNE_OCTAVE_UP);
    let pad2 = asset_link(&app, drums, 1);
    assert_eq!(pad2.abs, first[&1].abs);
    assert_eq!(pad2.resolve(Some(&home)), Some(first[&1].abs.clone()));
    // Samples that are still in use point into the collected folder in old steps too.
    let kick = kit_file(0);
    assert_eq!(asset_link(&app, drums, 0).abs, folder.join(&kick));
    assert_eq!(
        asset_link(&app, drums, 0).rel,
        Path::new("Song Samples").join(&kick)
    );
    undo(&mut app);
    wait_for_param_value(&mut app, drums, tune, TUNE_DEFAULT);
    assert_eq!(asset_link(&app, drums, 0).abs, folder.join(&kick));
    assert_missing_link(
        &app,
        drums,
        &first[&7],
        &Path::new("../library").join(kit_file(7)),
    );
    redo(&mut app);
    wait_for_param_value(&mut app, drums, tune, TUNE_OCTAVE_UP);
    redo(&mut app);
    assert_eq!(asset_link(&app, drums, 1).abs, other_copy);
    drop(app);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ------------------------------------------------------------------------------------------------
// A bridged plugin: its parameter on the card, through its snapshot

fn bridged_app(name: &str) -> Option<(App, DeviceId, u32)> {
    let dirs = fixture_dirs()?;
    let dir = scratch(name);
    let mut app = app_with(Project::new("Bridge"), PluginHost::new(config(&dir)));
    app.engine_mut().host_mut().start_scan(&dirs);
    step_until(&mut app, "scan", |a| {
        a.engine()
            .host()
            .catalog()
            .entries()
            .iter()
            .any(|e| e.plugin.id == SYNTH)
    });
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: clap_plugin(SYNTH),
        target: DropTarget::NewTrack { index: None },
    }));
    let synth = app.project().tracks[0].channel.devices[0].id;
    step_until(&mut app, "the plugin", |a| {
        a.engine().device_status(synth) == komponist_plugins::DeviceStatus::Ok
    });
    let gain = app
        .engine()
        .host()
        .params(synth)
        .unwrap()
        .iter()
        .find(|p| p.name == "Gain")
        .expect("a gain parameter")
        .id;
    Some((app, synth, gain))
}

/// The plugin's own state, asked for: what its next snapshot says.
fn plugin_state(app: &mut App, synth: DeviceId) -> Arc<[u8]> {
    app.engine_mut().host_mut().request_state(synth);
    let events = step_until(app, "a snapshot", |a| {
        a.device_events().iter().any(
            |e| matches!(e, PluginManagerEvent::StateSnapshot { device, .. } if *device == synth),
        )
    });
    events
        .into_iter()
        .rev()
        .find_map(|e| match e {
            PluginManagerEvent::StateSnapshot { device, state } if device == synth => {
                Some(state.bytes)
            }
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_drag_that_changes_nothing_leaves_no_step() {
    let Some((mut app, synth, gain)) = bridged_app("param-undo-unchanged") else {
        return;
    };
    let baseline = plugin_state(&mut app, synth);
    let start = app.doc().top_step();
    let value = app.engine().host().param_value(synth, gain).unwrap().value;

    drag_param(&mut app, synth, gain, &[]);
    assert!(!app.doc().in_gesture());
    set(&mut app, synth, gain, value);

    assert_eq!(plugin_state(&mut app, synth), baseline);
    assert!(state_of(&app, synth).is_none());
    assert_eq!(app.doc().top_step(), start);
}

#[test]
fn a_bridged_plugins_card_drag_is_one_step_and_undo_restores_the_plugin() {
    let Some((mut app, synth, gain)) = bridged_app("param-undo-bridge") else {
        return;
    };
    // The plugin's baseline snapshot (asked for when it came up) and its debounce quiet; its
    // gain starts at the default, -12 dB (value 0.8).
    step_for(&mut app, Duration::from_millis(1300));
    let baseline = plugin_state(&mut app, synth);
    assert!(
        state_of(&app, synth).is_none(),
        "nothing in the document yet"
    );
    let start = depth(&mut app);

    // A drag: a gesture with a few turns; the step arrives with the plugin's snapshot, which the
    // end of the gesture asks for.
    begin(&mut app, synth, gain);
    for v in [0.9, 0.7, 0.6, 0.5] {
        set(&mut app, synth, gain, v);
        step_for(&mut app, Duration::from_millis(20));
    }
    end(&mut app);
    step_until(&mut app, "the snapshot of the drag", |a| {
        state_of(a, synth).is_some()
    });
    // The debounced snapshot of the same turns follows; it must not make a second step.
    step_for(&mut app, Duration::from_millis(1500));
    assert_eq!(depth(&mut app), start + 1, "the drag is one step");
    assert_eq!(app.undo_text().as_deref(), Some("Gain ändern"));
    let turned = state_of(&app, synth).unwrap();
    assert_ne!(*turned, *baseline);
    assert_eq!(
        plugin_state(&mut app, synth),
        turned,
        "the document has what the plugin holds"
    );

    // Undo: the plugin is back at what it held before the drag.
    undo(&mut app);
    assert!(state_of(&app, synth).is_none());
    step_for(&mut app, Duration::from_millis(100));
    assert_eq!(
        plugin_state(&mut app, synth),
        baseline,
        "undo: the plugin follows"
    );
    redo(&mut app);
    step_for(&mut app, Duration::from_millis(100));
    assert_eq!(state_of(&app, synth).unwrap(), turned);
    assert_eq!(
        plugin_state(&mut app, synth),
        turned,
        "redo: the plugin follows"
    );
    // The echoes of undo and redo made no steps.
    step_for(&mut app, Duration::from_millis(1500));
    assert_eq!(depth(&mut app), start + 1);
}

#[test]
fn a_bridged_plugins_single_turn_is_a_step_named_after_the_parameter() {
    let Some((mut app, synth, gain)) = bridged_app("param-undo-bridge-single") else {
        return;
    };
    step_for(&mut app, Duration::from_millis(1300));
    let start = depth(&mut app);
    set(&mut app, synth, gain, 0.7);
    step_until(&mut app, "the debounced snapshot", |a| {
        state_of(a, synth).is_some()
    });
    assert_eq!(depth(&mut app), start + 1);
    assert_eq!(app.undo_text().as_deref(), Some("Gain ändern"));
}
