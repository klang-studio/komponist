//! The step sequencer (WP-42) through `egui_kittest`: one test per row of the S03 notes, the
//! toolbar, the lane, the accessibility tree, and the snapshot next to the S03 reference.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::SelectionChange;
use komponist_app::panels::transport::names as tp;
use komponist_app::ui_state::{FocusArea, Panel, StepLane};
use komponist_app::views::step_seq::names;
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_engine::TransportState;
use komponist_model::edits::{StepClipProp, StepProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipContent, ClipId, Project, StepClip, Tempo, Tick, TrackId};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
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
    h.state_mut().clear_dispatched();
    h
}

fn ui(a: UiAction) -> Action {
    Action::Ui(a)
}

fn edit(e: EditAction) -> Action {
    Action::Edit(e)
}

/// The drum track and its "Beat B" clip at bar 9 (S03).
fn beat_b(app: &App) -> (TrackId, ClipId) {
    let drums = &app.project().tracks[0];
    (drums.id, drums.clips[2].id)
}

/// The step pattern of `clip`.
fn pattern(app: &App, clip: ClipId) -> StepClip {
    let (_, c) = app.project().find_clip(clip).expect("clip");
    match &c.content {
        ClipContent::Step(s) => s.clone(),
        ClipContent::Midi(_) => panic!("not a step clip"),
    }
}

/// `project` with "Beat B" open in the step sequencer.
fn opened_in(project: Project) -> App {
    let mut app = App::headless(project);
    let (_, clip) = beat_b(&app);
    app.dispatch(ui(UiAction::OpenClip(clip)));
    app
}

/// The demo with "Beat B" open in the step sequencer.
fn opened() -> App {
    opened_in(demo_project())
}

/// The S03 state in `project` (the demo): "Beat B" with the pattern of `Screens.dc.html`
/// `PADS`, `prob`, `ratch` and swing 12 %, the cursor on step 11 of "Hat zu", the dock 380 high
/// as in the screens, playhead at 10.2.120.
fn s03_in(project: Project) -> App {
    let mut app = opened_in(project);
    let (_, clip) = beat_b(&app);
    let set = |row, step, prop| {
        edit(EditAction::SetStep {
            clip,
            row,
            step,
            prop,
        })
    };
    let actions = [
        set(3, 15, StepProp::On(true)),
        set(3, 15, StepProp::Velocity(60)),
        set(6, 7, StepProp::On(true)),
        set(6, 7, StepProp::Velocity(64)),
        set(7, 5, StepProp::On(true)),
        set(7, 5, StepProp::Velocity(60)),
        set(7, 9, StepProp::On(true)),
        set(7, 9, StepProp::Velocity(58)),
        set(3, 2, StepProp::Probability(50)),
        set(3, 10, StepProp::Probability(50)),
        set(7, 9, StepProp::Probability(75)),
        set(3, 15, StepProp::Ratchet(3)),
        set(0, 14, StepProp::Ratchet(2)),
        edit(EditAction::SetStepClipProp {
            clip,
            prop: StepClipProp::Swing(0.12),
        }),
        ui(UiAction::Select(SelectionChange::Step {
            clip,
            row: 3,
            step: Some(10),
        })),
        ui(UiAction::ResizePanel {
            panel: Panel::Dock,
            size: 380.0,
        }),
        Action::Transport(TransportAction::Locate(Tick(36_000))),
    ];
    for a in actions {
        app.dispatch(a);
    }
    app
}

fn s03() -> App {
    s03_in(demo_project())
}

/// The CPU readout depends on the machine; masked in the snapshots.
fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h.get_by_role_and_label(Role::Status, tp::CPU).rect();
    h.mask(cpu.expand(2.0));
}

/// Steps the harness until `done` holds (the null backend runs on its own thread).
fn step_until(h: &mut Harness<'_, App>, mut done: impl FnMut(&App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        h.step();
        if done(h.state()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn snapshots_next_to_s03() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let mut h = harness_at(s03(), ppp);
        assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(36_000)));
        h.run_steps(2);
        mask_cpu(&mut h);
        h.snapshot(format!("step-seq-s03-{ppp}x"));
        results.extend_harness(&mut h);
    }

    // The running light: playing at 1 BPM (a step lasts 15 s), the playhead in step 5 of
    // "Beat B". The transport readouts move and are masked.
    let mut project = demo_project();
    project.tempo = Tempo::new(1.0);
    let mut app = s03_in(project);
    app.dispatch(Action::Transport(TransportAction::Locate(Tick(
        30_720 + 4 * 240 + 20,
    ))));
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    let mut h = harness_at(app, 1.0);
    assert!(step_until(&mut h, |a| a.transport() == TransportState::Playing));
    h.run_steps(2);
    mask_cpu(&mut h);
    let transport = Rect::from_min_size(Pos2::ZERO, vec2(W, komponist_tokens::size::TRANSPORT));
    h.mask(transport);
    h.snapshot("step-seq-running-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
