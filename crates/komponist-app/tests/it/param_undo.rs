//! Device parameters are undoable (owner decision 2026-11-15): a knob drag, a wheel burst, a
//! sample load or a plugin's snapshot is one undo step, and undo, redo and a cancelled gesture
//! restore the document **and** the running processor. The engine is held and rendered by the
//! test, so the checks listen to the processors.
//!
//! Gestures are driven two ways: through the real hexa card (a drag and a wheel burst on its
//! knob, which checks the cells' wiring) and as the action sequence the cells send
//! (`BeginParamGesture`, `SetParam` per frame, `EndGesture`).

use crate::common::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::device_host::PluginHost;
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, EditAction, PluginAction, ProjectAction, UiAction};
use komponist_builtin::Internal;
use komponist_engine::Command;
use komponist_hexa::Patch;
use komponist_hexa::device::{self, Field};
use komponist_model::fixtures::demo_project;
use komponist_model::params::ParamId;
use komponist_model::{ChannelRef, DeviceId, Project, TrackId};
use komponist_plugins::PluginManagerEvent;
use komponist_sampler as sampler;
use komponist_sampler::{PadField, SampleLink, links, testkit};

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

fn hexa_app(preset: usize) -> (App, TrackId, DeviceId) {
    let mut app = App::headless(Project::new("Synth"));
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    let device = app.project().tracks[0].channel.devices[0].id;
    app.dispatch(Action::Plugin(PluginAction::ApplyHexaPreset {
        device,
        preset,
    }));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    (app, track, device)
}

fn patch_of(app: &App, device: DeviceId) -> Patch {
    komponist_hexa::decode_state(state_of(app, device).as_deref()).unwrap()
}

fn wire(key: &str) -> u32 {
    device::param(&ParamId::new(key))
        .expect("a hexa parameter")
        .wire
}

// ------------------------------------------------------------------------------------------------
// hexa

#[test]
fn a_hexa_drag_is_one_step_that_never_recreates_the_processor_and_undo_follows_it() {
    let (mut app, track, synth) = hexa_app(1);
    hold(&mut app);
    let (before_patch, before_state) = (patch_of(&app, synth), state_of(&app, synth));
    let start_depth = depth(&mut app);
    let quiet = note_peak(&mut app, track, 60);
    assert!(quiet > 0.0);

    // A note sounds while the knob is dragged: the processor must be the same one afterwards
    // (a fresh processor has no note and is silent).
    app.engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 100,
    });
    app.engine_mut().run_held(4800, &[], 0);
    let reloads = app.engine().device_reloads();
    let volume = wire("volume");
    let values: Vec<f64> = (0..10).map(|i| 0.9 - 0.05 * f64::from(i)).collect();
    drag_param(&mut app, synth, volume, &values);
    let (l, r) = app.engine_mut().run_held_capture(4800, &[], 0).unwrap();
    let held = l.iter().chain(&r).fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(held > 1e-4, "the note sounds on after the drag: {held}");
    assert_eq!(
        app.engine().device_reloads(),
        reloads,
        "a drag does not recreate the processor"
    );
    app.engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 0,
    });
    app.engine_mut().run_held(96_000, &[], 0);

    // One step, named after the parameter; the value is in the document.
    assert_eq!(depth(&mut app), start_depth + 1);
    assert_eq!(app.undo_text().as_deref(), Some("Lautstärke ändern"));
    let after = patch_of(&app, synth);
    assert_ne!(after, before_patch);
    // `depth` undid and redid everything: the redone drag reached the processor as a parameter
    // (#19), and hexa glides the volume over 10 ms, so the sound is compared once the glide is
    // over (here and after undo and redo below).
    let settle = |app: &mut App| app.engine_mut().run_held(4800, &[], 0);
    settle(&mut app);
    let loud = note_peak(&mut app, track, 60);
    assert_ne!(loud, quiet, "the drag changed the sound");

    // Undo: the document and the sound are back. Redo: both again. Both reach the same
    // processor as parameters.
    let reloads = app.engine().device_reloads();
    undo(&mut app);
    assert_eq!(state_of(&app, synth), before_state);
    settle(&mut app);
    assert_eq!(note_peak(&mut app, track, 60), quiet, "undo: the old sound");
    redo(&mut app);
    assert_eq!(patch_of(&app, synth), after);
    settle(&mut app);
    assert_eq!(note_peak(&mut app, track, 60), loud, "redo: the new sound");
    assert_eq!(
        app.engine().device_reloads(),
        reloads,
        "undo and redo neither"
    );
    assert!(app.doc().is_dirty());
}

#[test]
fn a_parameter_without_a_gesture_is_a_step_of_its_own_and_a_preset_then_a_knob_undoes_in_two() {
    let (mut app, track, synth) = hexa_app(0);
    hold(&mut app);
    let original = state_of(&app, synth);
    let start = depth(&mut app);
    let quiet = note_peak(&mut app, track, 60);

    // Preset, then a knob turn (a key step or a click: no gesture).
    app.dispatch(Action::Plugin(PluginAction::ApplyHexaPreset {
        device: synth,
        preset: 17,
    }));
    app.step();
    let preset_state = state_of(&app, synth);
    let preset_peak = note_peak(&mut app, track, 60);
    set(&mut app, synth, wire("volume"), 0.2);
    app.step();
    set(&mut app, synth, wire("glide"), 0.3);
    app.step();
    assert_eq!(depth(&mut app), start + 3, "preset and two knob steps");
    assert_ne!(preset_peak, quiet);

    undo(&mut app);
    undo(&mut app);
    assert_eq!(
        state_of(&app, synth),
        preset_state,
        "the knob steps are gone"
    );
    assert_eq!(note_peak(&mut app, track, 60), preset_peak);
    undo(&mut app);
    assert_eq!(state_of(&app, synth), original, "back before the preset");
    assert_eq!(note_peak(&mut app, track, 60), quiet);
}

#[test]
fn escape_during_a_drag_puts_the_value_back_in_the_document_and_the_engine() {
    let (mut app, track, synth) = hexa_app(1);
    hold(&mut app);
    let before = state_of(&app, synth);
    let start = depth(&mut app);
    let quiet = note_peak(&mut app, track, 60);

    begin(&mut app, synth, wire("volume"));
    for v in [0.3, 0.2, 0.1] {
        set(&mut app, synth, wire("volume"), v);
        app.step();
    }
    assert_ne!(state_of(&app, synth), before);
    // Esc, as the app's key handling turns it during a gesture.
    app.dispatch(Action::Edit(EditAction::CancelGesture));
    app.step();
    assert!(!app.doc().in_gesture());
    assert_eq!(state_of(&app, synth), before, "the document is back");
    assert_eq!(note_peak(&mut app, track, 60), quiet, "and so is the sound");
    assert_eq!(depth(&mut app), start, "no step");
}

#[test]
fn a_drag_that_changes_nothing_leaves_no_step() {
    let (mut app, _, synth) = hexa_app(1);
    let start = depth(&mut app);
    let value = Field::Macro(komponist_hexa::Macro::Feedback).get(&patch_of(&app, synth));
    drag_param(&mut app, synth, wire("volume"), &[]);
    set(
        &mut app,
        synth,
        Field::Macro(komponist_hexa::Macro::Feedback).wire(),
        value,
    );
    app.step();
    assert_eq!(depth(&mut app), start);
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
    let (mut app, track, synth) = hexa_app(1);
    hold(&mut app);
    let start = depth(&mut app);
    let quiet = note_peak(&mut app, track, 60);
    let before = state_of(&app, synth);
    let mut h = harness(app);
    let reloads = h.state().engine().device_reloads();

    let glide = device::param(&ParamId::new("glide")).unwrap();
    let knob = knob_rect(&h, "Glide");
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    let started = komponist_hexa::decode_state(before.as_deref()).unwrap();
    assert!(glide.field.get(&patch_of(h.state(), synth)) > glide.field.get(&started));
    assert_eq!(h.state().undo_text().as_deref(), Some("Glide ändern"));
    assert_eq!(
        h.state().engine().device_reloads(),
        reloads,
        "no new processor while dragging"
    );
    assert_eq!(depth(h.state_mut()), start + 1, "the drag is one step");

    // A burst of three notches over another knob: one more step.
    let tune = knob_rect(&h, "Tune");
    let tuned = patch_of(h.state(), synth);
    wheel(&mut h, tune.center(), &[1.0, 1.0, 1.0]);
    h.run_steps(60);
    assert_ne!(
        patch_of(h.state(), synth),
        tuned,
        "the wheel turned the knob"
    );
    assert_eq!(h.state().undo_text().as_deref(), Some("Tune ändern"));
    assert_eq!(depth(h.state_mut()), start + 2, "the burst is one step");

    // Two undos, and the document and the sound are as at the start.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(patch_of(h.state(), synth), tuned);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
    assert_eq!(state_of(h.state(), synth), before);
    assert_eq!(note_peak(h.state_mut(), track, 60), quiet);
}

// ------------------------------------------------------------------------------------------------
// An internal device

fn limiter_app() -> (App, TrackId, DeviceId) {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Limiter.plugin_ref(),
        target: DropTarget::Chain {
            channel: ChannelRef::Master,
            index: 0,
        },
    }));
    let limiter = app.project().master.devices[0].id;
    let track = app
        .project()
        .tracks
        .iter()
        .find(|t| {
            t.channel
                .devices
                .iter()
                .any(|d| komponist_app::device_host::is_hexa(d))
        })
        .unwrap()
        .id;
    (app, track, limiter)
}

#[test]
fn an_internal_devices_drag_is_one_step_and_undo_redo_restore_the_sound() {
    let (mut app, track, limiter) = limiter_app();
    let id = |key: &str| Internal::Limiter.wire(&ParamId::new(key)).unwrap();
    // Drive it hard into a high ceiling first.
    set(&mut app, limiter, id("gain"), 24.0);
    set(&mut app, limiter, id("ceiling"), -0.3);
    hold(&mut app);
    let start = depth(&mut app);
    let before = state_of(&app, limiter);
    let loud = note_peak(&mut app, track, 60);

    let reloads = app.engine().device_reloads();
    drag_param(&mut app, limiter, id("ceiling"), &[-3.0, -6.0, -9.0, -12.0]);
    assert_eq!(app.engine().device_reloads(), reloads);
    let quiet = note_peak(&mut app, track, 60);
    assert!(quiet <= 0.26 && loud > quiet * 1.5, "{loud} {quiet}");
    assert_eq!(depth(&mut app), start + 1);
    let label = app.undo_text().unwrap();
    assert!(label.ends_with(" ändern"), "{label}");

    undo(&mut app);
    assert_eq!(state_of(&app, limiter), before);
    assert!(
        note_peak(&mut app, track, 60) > quiet * 1.5,
        "undo: the old ceiling"
    );
    redo(&mut app);
    assert!(
        note_peak(&mut app, track, 60) <= 0.26,
        "redo: the new ceiling"
    );
}

// ------------------------------------------------------------------------------------------------
// The Sampler: pad parameters and samples

fn sampler_app(name: &str) -> (App, TrackId, DeviceId, PathBuf) {
    let dir = scratch(name);
    let project = komponist_app::samples::demo_with_kit(demo_project(), &dir).unwrap();
    let track = project.tracks[0].id;
    let drums = project.tracks[0].channel.devices[0].id;
    (App::headless(project), track, drums, dir)
}

fn pad_names(app: &App, drums: DeviceId) -> Vec<String> {
    let handle = app.engine().host().sampler(drums).expect("the sampler");
    assert!(handle.wait_idle(PATIENCE), "samples loading");
    handle.views().iter().map(|v| v.name.clone()).collect()
}

#[test]
fn a_sampler_pad_param_drag_is_one_step_and_undo_restores_the_pads_level() {
    let (mut app, track, drums, _) = sampler_app("param-undo-pad");
    app.step();
    pad_names(&app, drums);
    hold(&mut app);
    let key = sampler::note_of_pad(0);
    let level = PadField::Level.wire(0) as u32;
    let before = state_of(&app, drums);
    let start = depth(&mut app);
    let full = note_peak(&mut app, track, key);
    assert!(full > 0.0);

    drag_param(&mut app, drums, level, &[-3.0, -6.0, -12.0, -24.0]);
    assert_eq!(depth(&mut app), start + 1);
    let quiet = note_peak(&mut app, track, key);
    assert!(quiet < full * 0.2, "{full} → {quiet}");
    undo(&mut app);
    assert_eq!(state_of(&app, drums), before);
    assert_eq!(note_peak(&mut app, track, key), full, "undo: the old level");
    redo(&mut app);
    assert_eq!(
        note_peak(&mut app, track, key),
        quiet,
        "redo: the new level"
    );
}

#[test]
fn clearing_and_relinking_a_sample_are_one_step_each_and_undo_reaches_the_sampler() {
    let (mut app, _, drums, dir) = sampler_app("param-undo-sample");
    app.step();
    let names = pad_names(&app, drums);
    assert!(names.iter().all(|n| !n.is_empty()), "{names:?}");
    let start = depth(&mut app);

    app.dispatch(Action::Plugin(PluginAction::ClearSample {
        device: drums,
        pad: 0,
    }));
    app.step();
    assert_eq!(app.undo_text().as_deref(), Some("Sample entfernen"));
    assert!(pad_names(&app, drums)[0].is_empty());

    // A sample on a pad whose start was moved: loading it resets the start (a second change) and
    // links the file, still one step.
    let level = PadField::Start.wire(0) as u32;
    set(&mut app, drums, level, 50.0);
    app.step();
    let other = testkit::write_kit(&dir).unwrap()[3].clone();
    app.dispatch(Action::Plugin(PluginAction::LoadSample {
        device: drums,
        pad: 0,
        path: other.clone(),
    }));
    app.step();
    assert_eq!(app.undo_text().as_deref(), Some("Sample laden"));
    assert_eq!(
        pad_names(&app, drums)[0],
        SampleLink::new(&other, None).name
    );
    assert_eq!(depth(&mut app), start + 3, "clear, start, load");

    undo(&mut app);
    assert!(
        pad_names(&app, drums)[0].is_empty(),
        "undo: the pad is empty"
    );
    let start_value = |app: &App| {
        Internal::Sampler
            .decode_state(state_of(app, drums).as_deref())
            .unwrap()[level as usize]
    };
    assert_eq!(start_value(&app), 50.0, "and its start is back");
    undo(&mut app);
    undo(&mut app);
    assert_eq!(
        pad_names(&app, drums),
        names,
        "undo: the first sample is back"
    );
    redo(&mut app);
    assert!(pad_names(&app, drums)[0].is_empty(), "redo: empty again");
}

// ------------------------------------------------------------------------------------------------
// The Sampler: saving rewrites the sample paths of every undo step, each from its own state (#86)

/// The synthesized kit written into `dir` (created).
fn kit_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    testkit::write_kit(dir).unwrap()
}

/// A project with one sampler track whose pads link the kit in `kit` (absolute paths only).
fn sampler_project(kit: &Path) -> Project {
    use komponist_model::edits::{self, NewTrack};
    let spec = NewTrack {
        name: Some("Drums".into()),
        hue: None,
        instrument: Some(Internal::Sampler.plugin_ref()),
        index: None,
        arm_exclusive: false,
    };
    let (mut p, _) = edits::add_track(&Project::new("Song"), spec).unwrap();
    let mut state: Option<Vec<u8>> = None;
    for (pad, path) in kit_in(kit).iter().enumerate() {
        state = Some(links::write(
            state.as_deref(),
            pad,
            Some(&SampleLink::new(path, None)),
        ));
    }
    let track = Arc::make_mut(&mut p.tracks[0]);
    Arc::make_mut(&mut track.channel.devices[0]).state = state.map(Arc::from);
    p
}

/// [`sampler_project`] with its kit in `kit`, written to `file` and opened from there: the
/// history starts empty. The sample of pad 8 is deleted (missing) before.
fn opened_sampler_app(kit: &Path, file: &Path) -> (App, DeviceId) {
    let project = sampler_project(kit);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, komponist_model::persist::to_json(&project).unwrap()).unwrap();
    std::fs::remove_file(kit.join(format!("{}.wav", testkit::NAMES[7]))).unwrap();
    let mut app = App::headless(Project::new("leer"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.to_owned())));
    app.step();
    let drums = app.project().tracks[0].channel.devices[0].id;
    (app, drums)
}

/// The links of the sampler in the document, by pad.
fn links_of(app: &App, drums: DeviceId) -> Vec<(usize, SampleLink)> {
    links::read(state_of(app, drums).as_deref())
}

fn link_of(app: &App, drums: DeviceId, pad: usize) -> SampleLink {
    links_of(app, drums)
        .into_iter()
        .find(|(p, _)| *p == pad)
        .map(|(_, l)| l)
        .unwrap_or_else(|| panic!("no link on pad {pad}"))
}

/// The tuning of `pad` (a state without parameters has the defaults).
fn tune_of(app: &App, drums: DeviceId, pad: usize) -> f64 {
    Internal::Sampler
        .decode_state(state_of(app, drums).as_deref())
        .unwrap_or_else(|_| Internal::Sampler.defaults())[PadField::Tune.wire(pad)]
}

/// Tunes pad 1 to +12 (one step), then links `other` to pad 2 (one step).
fn tune_and_relink(app: &mut App, drums: DeviceId, other: &Path) {
    set(app, drums, PadField::Tune.wire(0) as u32, 12.0);
    app.step();
    app.dispatch(Action::Plugin(PluginAction::LoadSample {
        device: drums,
        pad: 1,
        path: other.to_owned(),
    }));
    app.step();
    assert_eq!(tune_of(app, drums, 0), 12.0);
    assert_eq!(link_of(app, drums, 1).abs, other);
}

/// `dir.join(rel)` with `..` resolved: where a relative link points from `dir`.
fn resolved(dir: &Path, link: &SampleLink) -> Option<PathBuf> {
    link.resolve(Some(dir))
}

#[test]
fn undo_after_save_restores_the_samplers_parameters_and_samples() {
    let dir = scratch("param-undo-save-rebase");
    let one = dir.join("one");
    let (mut app, drums) = opened_sampler_app(&one.join("Kit"), &one.join("Song.komponist"));
    let first = links_of(&app, drums);
    assert_eq!(first.len(), 8);
    let other = kit_in(&one.join("More"))[3].clone();
    tune_and_relink(&mut app, drums, &other);

    // Save: the links get their paths relative to the file's folder.
    app.dispatch(Action::Project(ProjectAction::Save));
    assert!(!app.doc().is_dirty());
    let kick = Path::new("Kit").join(format!("{}.wav", testkit::NAMES[0]));
    let snare = Path::new("Kit").join(format!("{}.wav", testkit::NAMES[1]));
    assert_eq!(link_of(&app, drums, 0).rel, kick);
    assert_eq!(
        link_of(&app, drums, 1).rel,
        Path::new("More").join(format!("{}.wav", testkit::NAMES[3]))
    );

    // Undo the link: the first sample is back, with its relative path for the file too; the
    // tuning stays.
    undo(&mut app);
    assert_eq!(
        link_of(&app, drums, 1).abs,
        first[1].1.abs,
        "undo: pad 2's sample"
    );
    assert_eq!(
        link_of(&app, drums, 1).rel,
        snare,
        "rebased in this step too"
    );
    assert_eq!(tune_of(&app, drums, 0), 12.0);
    // Undo the tuning: back to 0, the links still rebased.
    undo(&mut app);
    assert_eq!(tune_of(&app, drums, 0), 0.0, "undo: the old tuning");
    assert_eq!(link_of(&app, drums, 0).rel, kick);
    // The missing sample's link is as it was, in every step.
    assert_eq!(link_of(&app, drums, 7), first[7].1);
    // Redo: the tuning and the other sample again.
    redo(&mut app);
    assert_eq!(tune_of(&app, drums, 0), 12.0, "redo: the tuning");
    assert_eq!(link_of(&app, drums, 1).abs, first[1].1.abs);
    redo(&mut app);
    assert_eq!(link_of(&app, drums, 1).abs, other, "redo: the other sample");
    assert_eq!(link_of(&app, drums, 7), first[7].1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_after_save_as_into_another_folder_resolves_the_old_steps_links_from_there() {
    let dir = scratch("param-undo-save-as-rebase");
    let one = dir.join("one");
    let (mut app, drums) = opened_sampler_app(&one.join("Kit"), &one.join("Song.komponist"));
    let first = links_of(&app, drums);
    let other = kit_in(&one.join("More"))[3].clone();
    tune_and_relink(&mut app, drums, &other);

    // "Speichern unter" into another folder, through the file browser.
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
        link_of(app, drums, 0).rel,
        Path::new("../one/Kit").join(format!("{}.wav", testkit::NAMES[0]))
    );

    undo(app);
    assert_eq!(tune_of(app, drums, 0), 12.0);
    let pad2 = link_of(app, drums, 1);
    assert_eq!(pad2.abs, first[1].1.abs, "undo: pad 2's sample");
    // The old step's relative link resolves from the new folder, not only through the absolute
    // path.
    assert_eq!(
        resolved(
            &two,
            &SampleLink {
                abs: PathBuf::new(),
                ..pad2
            }
        ),
        Some(first[1].1.abs.clone())
    );
    undo(app);
    assert_eq!(tune_of(app, drums, 0), 0.0, "undo: the old tuning");
    assert_eq!(link_of(app, drums, 7), first[7].1, "the missing link stays");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_after_collect_and_save_keeps_each_steps_samples_and_copies_only_the_current_ones() {
    let dir = scratch("param-undo-collect");
    let library = dir.join("library");
    let home = dir.join("proj");
    let (mut app, drums) = opened_sampler_app(&library, &home.join("Song.komponist"));
    let first = links_of(&app, drums);
    let other = kit_in(&dir.join("more"))[3].clone();
    tune_and_relink(&mut app, drums, &other);

    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    let folder = home.join("Song Samples");
    let mut copied: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    copied.sort();
    // Pads 1, 3 … 7 (the other sample on pad 2 is the same sound as pad 4's: one copy); not pad
    // 2's first sample (only the history has it), not the missing one.
    let mut expected: Vec<String> = [0, 2, 3, 4, 5, 6]
        .iter()
        .map(|&i| format!("{}.wav", testkit::NAMES[i]))
        .collect();
    expected.sort();
    assert_eq!(copied, expected);
    let other_copy = folder.join(format!("{}.wav", testkit::NAMES[3]));
    assert_eq!(link_of(&app, drums, 1).abs, other_copy);

    undo(&mut app);
    assert_eq!(tune_of(&app, drums, 0), 12.0);
    let pad2 = link_of(&app, drums, 1);
    assert_eq!(pad2.abs, first[1].1.abs, "undo: pad 2's first sample");
    assert_eq!(resolved(&home, &pad2), Some(first[1].1.abs.clone()));
    // The samples the project still uses point into the samples folder in the old step too.
    let kick = format!("{}.wav", testkit::NAMES[0]);
    assert_eq!(link_of(&app, drums, 0).abs, folder.join(&kick));
    assert_eq!(
        link_of(&app, drums, 0).rel,
        Path::new("Song Samples").join(&kick)
    );
    undo(&mut app);
    assert_eq!(tune_of(&app, drums, 0), 0.0, "undo: the old tuning");
    assert_eq!(link_of(&app, drums, 0).abs, folder.join(&kick));
    assert_eq!(
        link_of(&app, drums, 7),
        first[7].1,
        "the missing link stays"
    );
    redo(&mut app);
    redo(&mut app);
    assert_eq!(tune_of(&app, drums, 0), 12.0);
    assert_eq!(link_of(&app, drums, 1).abs, other_copy);
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// A bridged plugin: its parameter on the card, through its snapshot

fn bridged_app(name: &str) -> Option<(App, DeviceId, u32)> {
    let dirs = fixture_dirs()?;
    let dir = scratch(name);
    let mut app = app_with(
        Project::new("Bridge"),
        PluginHost::with_reference_devices(config(&dir)),
    );
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
            PluginManagerEvent::StateSnapshot { device, state } if device == synth => Some(state),
            _ => None,
        })
        .unwrap()
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
