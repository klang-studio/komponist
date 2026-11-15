//! The step sequencer (WP-42) through `egui_kittest`: one test per row of the S03 notes, the
//! toolbar, the lane, the accessibility tree, and the snapshot next to the S03 reference.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::SelectionChange;
use komponist_app::ui_state::{FocusArea, Panel, StepLane};
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_engine::TransportState;
use komponist_i18n::de;
use komponist_model::edits::{StepClipProp, StepProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipContent, ClipId, Project, StepClip, Tick, TickRange, TrackId};

const W: f32 = 1920.0;
const H: f32 = 1080.0;
/// Dock height of the screens (`ks-view.js` `geo`: 380); the app's default is 320.
const S03_DOCK: f32 = 380.0;
/// Start of "Beat B" (bar 9).
const BEAT_B_START: Tick = Tick(8 * 3840);

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

/// Node `role` / `label` in the dock: the inspector (WP-43) shows the step clip's fields under
/// the same names, so take the one left of it.
fn in_dock<'h>(h: &'h Harness<'_, App>, role: Role, label: &'h str) -> egui_kittest::Node<'h> {
    let inspector = W - h.state().ui().layout.inspector_width;
    h.query_all_by_role_and_label(role, label)
        .find(|n| n.rect().right() <= inspector)
        .unwrap_or_else(|| panic!("no {label} in the dock"))
}

/// The step check boxes ("Step 5, Kick, an, …"); the inspector has check boxes too.
fn step_boxes(h: &Harness<'_, App>) -> usize {
    h.get_all_by_role(Role::CheckBox)
        .filter(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("Step "))
        })
        .count()
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
            size: S03_DOCK,
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
    let cpu = h
        .get_by_role_and_label(Role::Status, de::TRANSPORT_CPU)
        .rect();
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

/// The same screen in English (#5).
#[test]
fn snapshot_next_to_s03_in_english() {
    let mut h = harness_at(s03().with_lang(komponist_i18n::Lang::En), 1.0);
    assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(36_000)));
    h.run_steps(2);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::en::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    h.snapshot("step-seq-s03-en-1x");
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

    // The running light: playing in a loop over step 5 of "Beat B", so the playhead stays in
    // it. Everything above the dock (transport readouts, arrangement playhead) moves and is
    // masked.
    let mut app = s03();
    let step5 = Tick(BEAT_B_START.0 + 4 * 240);
    for a in [
        TransportAction::SetLoopRange(TickRange::new(step5, Tick(step5.0 + 240))),
        TransportAction::Locate(step5),
        TransportAction::TogglePlay,
    ] {
        app.dispatch(Action::Transport(a));
    }
    let mut h = harness_at(app, 1.0);
    assert!(step_until(&mut h, |a| a.transport()
        == TransportState::Playing
        && a.engine().playhead() > step5));
    h.run_steps(2);
    let p = h.state().engine().playhead();
    assert!(p >= step5 && p.0 < step5.0 + 240, "{p:?}");
    let dock_top = H - komponist_tokens::size::STATUSBAR - S03_DOCK;
    h.mask(Rect::from_min_size(Pos2::ZERO, vec2(W, dock_top)));
    h.snapshot("step-seq-running-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// Pointer helpers

/// The AccessKit node name prefix of step `step` (0-based) of `row`: "Step 5, Kick, ".
fn step_label(row: &str, step: usize) -> String {
    format!("Step {}, {row}, ", step + 1)
}

/// Centre of step `step` of `row` on screen.
fn step_pos(h: &Harness<'_, App>, row: &str, step: usize) -> Pos2 {
    h.get_by_label_contains(&step_label(row, step))
        .rect()
        .center()
}

/// The accessible name of step `step` of `row`.
fn step_name(h: &Harness<'_, App>, row: &str, step: usize) -> String {
    h.get_by_label_contains(&step_label(row, step))
        .accesskit_node()
        .label()
        .unwrap_or_default()
}

fn button(h: &Harness<'_, App>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
}

/// Presses at `from`, moves through `path` one frame per point, releases at the last point.
fn drag(h: &mut Harness<'_, App>, from: Pos2, path: &[Pos2], modifiers: Modifiers) {
    h.event(Event::PointerMoved(from));
    button(h, from, true, modifiers);
    h.step();
    let mut last = from;
    for p in path {
        h.event(Event::PointerMoved(*p));
        h.step();
        last = *p;
    }
    button(h, last, false, modifiers);
    h.run_steps(2);
}

/// A click at `pos` with `modifiers` (press and release in one frame, as kittest and egui-mcp
/// click).
fn click_at(h: &mut Harness<'_, App>, pos: Pos2, modifiers: Modifiers) {
    h.event(Event::PointerMoved(pos));
    button(h, pos, true, modifiers);
    button(h, pos, false, modifiers);
    h.run_steps(2);
}

fn step_of(app: &App, clip: ClipId, row: usize, step: usize) -> komponist_model::Step {
    pattern(app, clip).rows[row].steps[step]
}

// ------------------------------------------------------------------------------------------------
// S03 notes, one test per row

/// "Klick / Ziehen ↔: Step an oder aus / mehrere Steps in einer Zeile malen."
#[test]
fn click_and_sideways_drag_paint_steps_in_one_row() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    // Click: Kick step 2 turns on (velocity 100), a second click turns it off.
    assert!(!step_of(h.state(), clip, 0, 1).on);
    h.get_by_label_contains(&step_label("Kick", 1)).click();
    h.run_steps(2);
    assert!(step_of(h.state(), clip, 0, 1).on);
    assert_eq!(step_name(&h, "Kick", 1), "Step 2, Kick, an, Velocity 100");
    h.get_by_label_contains(&step_label("Kick", 1)).click();
    h.run_steps(2);
    assert!(!step_of(h.state(), clip, 0, 1).on);
    assert_eq!(step_name(&h, "Kick", 1), "Step 2, Kick, aus");

    // Drag ↔ from an off step paints on; leaving the row does not paint other rows.
    let from = step_pos(&h, "Snare", 1);
    let path = [
        step_pos(&h, "Snare", 2),
        step_pos(&h, "Clap", 3),
        step_pos(&h, "Clap", 4),
    ];
    drag(&mut h, from, &path, Modifiers::NONE);
    let snare = pattern(h.state(), clip).rows[1].steps;
    assert!((1..=4).all(|i| snare[i].on), "Snare 2–5 painted on");
    assert!(
        !pattern(h.state(), clip).rows[2].steps[3].on,
        "Clap untouched"
    );
    // One drag, one undo step: undo takes the whole stroke back, nothing before it.
    h.state_mut().dispatch(edit(EditAction::Undo));
    let snare = pattern(h.state(), clip).rows[1].steps;
    assert!(
        (1..=3).all(|i| !snare[i].on),
        "one undo removes the whole stroke"
    );
    assert!(snare[4].on, "Snare step 5 was set before");
    h.state_mut().dispatch(edit(EditAction::Undo));
    assert!(
        step_of(h.state(), clip, 0, 1).on,
        "the next undo is the click before"
    );
    h.state_mut().dispatch(edit(EditAction::Redo));
    h.run_steps(2);

    // Drag ↔ from a set step paints off: Hat zu steps 1 and 3 are set, 2 is not.
    assert!(step_of(h.state(), clip, 3, 0).on && step_of(h.state(), clip, 3, 2).on);
    let from = step_pos(&h, "Hat zu", 0);
    let path = [step_pos(&h, "Hat zu", 1), step_pos(&h, "Hat zu", 2)];
    drag(&mut h, from, &path, Modifiers::NONE);
    let hat = pattern(h.state(), clip).rows[3].steps;
    assert!(!hat[0].on && !hat[1].on && !hat[2].on);
    assert!(hat[4].on, "steps the pointer did not reach stay");
}

/// "Ziehen ↕ auf Step: Velocity, Helligkeit folgt dem Wert, Lane zeigt die Zeile."
#[test]
fn vertical_drag_on_a_step_sets_its_velocity_and_the_lane_shows_the_row() {
    let mut app = opened();
    app.dispatch(ui(UiAction::ResizePanel {
        panel: Panel::Dock,
        size: S03_DOCK,
    }));
    let mut h = harness(app);
    let (_, clip) = beat_b(h.state());
    // Snare step 5 has velocity 110; 30 px down in small moves: 80.
    let from = step_pos(&h, "Snare", 4);
    let path: Vec<Pos2> = (1..=6).map(|k| from + vec2(0.0, 5.0 * k as f32)).collect();
    drag(&mut h, from, &path, Modifiers::NONE);
    let s = step_of(h.state(), clip, 1, 4);
    assert!(s.on, "a velocity drag keeps the step on");
    assert_eq!(s.velocity, 80);
    assert_eq!(step_name(&h, "Snare", 4), "Step 5, Snare, an, Velocity 80");
    // Up past the top: clamped to 127.
    let from = step_pos(&h, "Snare", 4);
    let path: Vec<Pos2> = (1..=12).map(|k| from - vec2(0.0, 6.0 * k as f32)).collect();
    drag(&mut h, from, &path, Modifiers::NONE);
    assert_eq!(step_of(h.state(), clip, 1, 4).velocity, 127);

    // The lane shows the pressed row: its name and its velocities.
    h.get_by_role_and_label(Role::Label, "Zeile Snare");
    let bar = h.get_by_label("Vel Step 5");
    assert_eq!(bar.accesskit_node().numeric_value(), Some(127.0));
    let bar = h.get_by_label("Vel Step 13");
    assert_eq!(bar.accesskit_node().numeric_value(), Some(116.0));
    // Brightness follows the velocity: the cell's colour is the note colour of the value
    // (`grid::cell_look`, unit-tested); here the step cursor moved with the press.
    let cursor = h.state().ui().selection.step.unwrap();
    assert_eq!((cursor.row, cursor.step), (1, Some(4)));
}

/// "Alt-Ziehen ↕: Wahrscheinlichkeit, Wert unten rechts im Step."
#[test]
fn alt_drag_sets_the_probability_and_alt_click_cycles_it() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    // Alt-drag 40 px down on Kick step 1: 100 → 60 %.
    let from = step_pos(&h, "Kick", 0);
    let path: Vec<Pos2> = (1..=8).map(|k| from + vec2(0.0, 5.0 * k as f32)).collect();
    drag(&mut h, from, &path, Modifiers::ALT);
    let s = step_of(h.state(), clip, 0, 0);
    assert!(s.on);
    assert_eq!(s.probability, 60);
    assert_eq!(
        step_name(&h, "Kick", 0),
        "Step 1, Kick, an, Velocity 120, Wahrscheinlichkeit 60 %"
    );
    // Alt-click without dragging: 100 → 75 → 50 → 25 → 100 (a value off the cycle goes to 100).
    let at = step_pos(&h, "Kick", 0);
    let mut seen = Vec::new();
    for _ in 0..5 {
        click_at(&mut h, at, Modifiers::ALT);
        seen.push(step_of(h.state(), clip, 0, 0).probability);
    }
    assert_eq!(seen, [100, 75, 50, 25, 100]);
    // Alt on an off step just sets it.
    let at = step_pos(&h, "Kick", 1);
    click_at(&mut h, at, Modifiers::ALT);
    assert!(step_of(h.state(), clip, 0, 1).on);
}

/// "Shift-Klick: Ratchet 1 → 2 → 3 → 4, Step teilt sich sichtbar."
#[test]
fn shift_click_cycles_the_ratchet() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    let mut seen = Vec::new();
    for _ in 0..4 {
        h.get_by_label_contains(&step_label("Kick", 0))
            .click_modifiers(Modifiers::SHIFT);
        h.run_steps(2);
        seen.push(step_of(h.state(), clip, 0, 0).ratchet);
    }
    assert_eq!(seen, [2, 3, 4, 1]);
    h.get_by_label_contains(&step_label("Kick", 0))
        .click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    assert_eq!(
        step_name(&h, "Kick", 0),
        "Step 1, Kick, an, Velocity 120, Ratchet 2"
    );
    assert!(
        step_of(h.state(), clip, 0, 0).on,
        "Shift-click keeps the step on"
    );
}

/// WP-47a: a header click keeps the step column of the cursor (prototype `onHdr` sets only
/// `sel.row`), so the toolbar shows the same step of the new row.
#[test]
fn a_header_click_keeps_the_step_column() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    h.get_by_label_contains(&step_label("Hat zu", 10))
        .click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    let cursor = h.state().ui().selection.step.unwrap();
    assert_eq!((cursor.row, cursor.step), (3, Some(10)));
    h.get_by_role_and_label(Role::Button, "Zeile Snare").click();
    h.run_steps(2);
    let cursor = h.state().ui().selection.step.unwrap();
    assert_eq!((cursor.clip, cursor.row, cursor.step), (clip, 1, Some(10)));
    h.get_by_label("Step 11 · Snare");
}

/// WP-47a: velocity, probability and ratchet changes from the grid are announced in the status
/// bar like the prototype's `app.status`.
#[test]
fn step_edits_are_announced_in_the_status_bar() {
    let mut h = harness(opened());
    h.get_by_label_contains(&step_label("Kick", 0))
        .click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    let status = h.state().ui().status.clone().expect("status");
    assert_eq!(status.text, "Ratchet ×2");
    h.get_by_label_contains(&step_label("Kick", 0))
        .click_modifiers(Modifiers::ALT);
    h.run_steps(2);
    assert_eq!(
        h.state().ui().status.clone().unwrap().text,
        "Wahrscheinlichkeit 75 %"
    );
}

/// WP-47a: the toolbar fields are the design system's number field: a double-click opens the
/// text entry, which starts right of the key.
#[test]
fn a_toolbar_field_takes_typed_values() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    let field = in_dock(&h, Role::SpinButton, de::STEP_SEQ_LENGTH);
    let at = field.rect().center();
    h.run_steps(30);
    h.event(Event::PointerMoved(at));
    for pressed in [true, false, true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(2);
    let entry = h.get_by_label("Länge Eingabe");
    assert!(entry.is_focused());
    h.event(Event::Text("8".to_owned()));
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(pattern(h.state(), clip).length_steps, 8);
}

/// "Zeilen-Header: Klick spielt das Pad an und wählt die Zeile; Reihenfolge wie klopfer-Pads."
#[test]
fn a_row_header_sounds_its_pad_and_selects_the_row() {
    let mut h = harness(opened());
    let (drums, clip) = beat_b(h.state());
    h.get_by_role_and_label(Role::Button, "Zeile Snare").click();
    h.run_steps(2);
    let note = |velocity| {
        Action::Transport(TransportAction::LiveNote {
            track: Some(drums),
            key: 37,
            velocity,
        })
    };
    let got: Vec<Action> = h.state().dispatched().cloned().collect();
    let on = got.iter().position(|a| *a == note(100)).expect("note on");
    let off = got.iter().position(|a| *a == note(0)).expect("note off");
    assert!(on < off);
    let cursor = h.state().ui().selection.step.unwrap();
    assert_eq!((cursor.clip, cursor.row, cursor.step), (clip, 1, None));
    assert_eq!(
        h.get_by_role_and_label(Role::Button, "Zeile Snare")
            .accesskit_node()
            .is_selected(),
        None,
        "buttons carry the selection as toggled"
    );
    // The toolbar names the row.
    h.get_by_label("Snare");

    // A held press sounds until the release.
    let at = h
        .get_by_role_and_label(Role::Button, "Zeile Kick")
        .rect()
        .center();
    h.state_mut().clear_dispatched();
    h.event(Event::PointerMoved(at));
    button(&h, at, true, Modifiers::NONE);
    h.run_steps(3);
    let kick = |velocity| {
        Action::Transport(TransportAction::LiveNote {
            track: Some(drums),
            key: 36,
            velocity,
        })
    };
    assert!(h.state().dispatched().any(|a| *a == kick(100)));
    assert!(!h.state().dispatched().any(|a| *a == kick(0)));
    button(&h, at, false, Modifiers::NONE);
    h.run_steps(2);
    assert!(h.state().dispatched().any(|a| *a == kick(0)));

    // Rows in pad order: top to bottom, notes rising from 36 (klopfer pads 1–8).
    let names = [
        "Kick",
        "Snare",
        "Clap",
        "Hat zu",
        "Hat offen",
        "Tom",
        "Rim",
        "Perc",
    ];
    let tops: Vec<f32> = names
        .iter()
        .map(|n| {
            h.get_by_role_and_label(Role::Button, &format!("Zeile {n}"))
                .rect()
                .top()
        })
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]));
    let notes: Vec<u8> = pattern(h.state(), clip)
        .rows
        .iter()
        .map(|r| r.note)
        .collect();
    assert_eq!(notes, (36..44).collect::<Vec<u8>>());
}

/// The row header's M and S: click mutes, Alt-click S solos only this row, the others show
/// "stumm durch Solo".
#[test]
fn row_mute_and_solo() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    h.get_by_label("Mute · Kick").click();
    h.run_steps(2);
    assert!(pattern(h.state(), clip).rows[0].mute);
    h.get_by_label("Solo · Hat zu").click();
    h.run_steps(2);
    h.get_by_label("Solo · Snare")
        .click_modifiers(Modifiers::ALT);
    h.run_steps(2);
    let solo: Vec<bool> = pattern(h.state(), clip)
        .rows
        .iter()
        .map(|r| r.solo)
        .collect();
    assert_eq!(
        solo,
        [false, true, false, false, false, false, false, false]
    );
    // Undo takes the exclusive solo back in one step.
    h.state_mut().dispatch(edit(EditAction::Undo));
    let solo: Vec<bool> = pattern(h.state(), clip)
        .rows
        .iter()
        .map(|r| r.solo)
        .collect();
    assert_eq!(
        solo,
        [false, false, false, true, false, false, false, false]
    );
}

/// "Lauflicht: Spalte des aktuellen Steps hellt auf, gesetzte Steps leuchten mit Kontur."
/// The look is unit-tested (`grid::cell_look`, `pattern::current_step`) and shown in the
/// snapshot `step-seq-running-1x`; here: playback leaves the pattern alone and the view keeps
/// its nodes while the light runs.
#[test]
fn the_running_light_runs_without_editing() {
    let mut app = opened();
    let step5 = Tick(BEAT_B_START.0 + 4 * 240);
    for a in [
        TransportAction::SetLoopRange(TickRange::new(step5, Tick(step5.0 + 480))),
        TransportAction::Locate(step5),
        TransportAction::TogglePlay,
    ] {
        app.dispatch(Action::Transport(a));
    }
    let mut h = harness(app);
    assert!(step_until(&mut h, |a| a.transport() == TransportState::Playing));
    let revision = h.state().doc().revision();
    h.run_steps(10);
    assert_eq!(h.state().doc().revision(), revision);
    assert_eq!(step_boxes(&h), 8 * 16);
}

/// "Als MIDI-Clip: Wandelt das Pattern in einen normalen MIDI-Clip um."
#[test]
fn as_midi_clip_converts_the_pattern() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    let hits = pattern(h.state(), clip).hits(Tick(4 * 3840)).len();
    h.get_by_role_and_label(Role::Button, de::STEP_SEQ_TO_MIDI)
        .click();
    h.run_steps(3);
    let (_, c) = h.state().project().find_clip(clip).unwrap();
    let ClipContent::Midi(midi) = &c.content else {
        panic!("still a step clip");
    };
    assert_eq!(midi.active().notes.len(), hits);
    assert_eq!(
        h.state().ui().editor_area(h.state().project()),
        FocusArea::PianoRoll
    );
    assert!(h.query_by_label_contains(&step_label("Kick", 0)).is_none());
}

// ------------------------------------------------------------------------------------------------
// Toolbar, lane, accessibility

#[test]
fn toolbar_fields_change_the_pattern_and_the_selected_step() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    // Länge 16 → 17, then 32 (the new steps show up as nodes).
    in_dock(&h, Role::SpinButton, de::STEP_SEQ_LENGTH).increment_accesskit();
    h.run_steps(2);
    assert_eq!(pattern(h.state(), clip).length_steps, 17);
    in_dock(&h, Role::SpinButton, de::STEP_SEQ_LENGTH).set_value_accesskit(32.0);
    h.run_steps(2);
    assert_eq!(pattern(h.state(), clip).length_steps, 32);
    assert_eq!(step_boxes(&h), 8 * 32);
    // Swing 12 %.
    in_dock(&h, Role::SpinButton, de::STEP_SEQ_SWING).set_value_accesskit(12.0);
    h.run_steps(2);
    assert!((pattern(h.state(), clip).swing - 0.12).abs() < 1e-6);
    // Auflösung: 1/8.
    in_dock(&h, Role::ComboBox, de::STEP_SEQ_RESOLUTION).click();
    h.run_steps(3);
    h.get_by_label("1/8").click();
    h.run_steps(3);
    assert_eq!(
        pattern(h.state(), clip).resolution,
        komponist_model::StepResolution::Eighth
    );

    // No step selected: no step fields.
    assert!(h.query_by_label(de::STEP_SEQ_VELOCITY).is_none());
    // Select Hat zu step 11 (set): "Step 11 · Hat zu", Vel 52, Wahr. 100 %, Ratch. 1.
    h.get_by_label_contains(&step_label("Hat zu", 10))
        .click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    h.get_by_label("Step 11 · Hat zu");
    let vel = h.get_by_role_and_label(Role::SpinButton, de::STEP_SEQ_VELOCITY);
    assert_eq!(vel.accesskit_node().numeric_value(), Some(52.0));
    vel.increment_accesskit();
    h.run_steps(2);
    assert_eq!(step_of(h.state(), clip, 3, 10).velocity, 53);
    h.get_by_role_and_label(Role::SpinButton, de::STEP_SEQ_PROBABILITY)
        .set_value_accesskit(50.0);
    h.run_steps(2);
    assert_eq!(step_of(h.state(), clip, 3, 10).probability, 50);
    let ratchet = h.get_by_role_and_label(Role::SpinButton, de::STEP_SEQ_RATCHET);
    assert_eq!(ratchet.accesskit_node().numeric_value(), Some(2.0));
    ratchet.decrement_accesskit();
    h.run_steps(2);
    assert_eq!(step_of(h.state(), clip, 3, 10).ratchet, 1);
    // An off step under the cursor: the fields go away.
    h.get_by_label_contains(&step_label("Hat zu", 10)).click();
    h.run_steps(2);
    assert!(!step_of(h.state(), clip, 3, 10).on);
    assert!(h.query_by_label(de::STEP_SEQ_VELOCITY).is_none());
}

#[test]
fn lane_tabs_switch_and_a_drag_in_the_lane_sets_values() {
    let mut app = opened();
    app.dispatch(ui(UiAction::ResizePanel {
        panel: Panel::Dock,
        size: S03_DOCK,
    }));
    let mut h = harness(app);
    let (_, clip) = beat_b(h.state());
    // The lane shows the first row until a row is selected.
    h.get_by_role_and_label(Role::Label, "Zeile Kick");
    // Drag across Kick steps 1 and 5 near the bottom: low velocities, the cursor follows.
    let b1 = h.get_by_label("Vel Step 1").rect();
    let b5 = h.get_by_label("Vel Step 5").rect();
    let low = |r: Rect| pos2(r.center().x, r.bottom() - 4.0);
    drag(&mut h, low(b1), &[low(b5)], Modifiers::NONE);
    let v1 = step_of(h.state(), clip, 0, 0).velocity;
    let v5 = step_of(h.state(), clip, 0, 4).velocity;
    assert!(v1 < 20 && v5 < 20, "{v1} {v5}");
    assert_eq!(h.state().ui().selection.step.unwrap().step, Some(4));
    // Off steps keep their value.
    assert_eq!(step_of(h.state(), clip, 0, 1).velocity, 100);
    // AccessKit: a lane bar takes increments.
    h.get_by_label("Vel Step 5").increment_accesskit();
    h.run_steps(2);
    assert_eq!(step_of(h.state(), clip, 0, 4).velocity, v5 + 1);

    // Tab "Wahr.": the lane shows and sets probabilities.
    h.get_by_role_and_label(Role::RadioButton, "Wahr.").click();
    h.run_steps(2);
    assert_eq!(h.state().ui().step_seq.lane, StepLane::Probability);
    let bar = h.get_by_label("Wahr. Step 1");
    assert_eq!(bar.accesskit_node().numeric_value(), Some(100.0));
    bar.set_value_accesskit(25.0);
    h.run_steps(2);
    assert_eq!(step_of(h.state(), clip, 0, 0).probability, 25);
    h.get_by_role_and_label(Role::RadioButton, "Ratch.").click();
    h.run_steps(2);
    assert_eq!(h.state().ui().step_seq.lane, StepLane::Ratchet);
    h.get_by_label("Ratch. Step 1");
}

#[test]
fn esc_cancels_a_drag() {
    let mut h = harness(opened());
    let (_, clip) = beat_b(h.state());
    let undo = h.state().doc().undo_label().map(str::to_owned);
    let from = step_pos(&h, "Snare", 4);
    h.event(Event::PointerMoved(from));
    button(&h, from, true, Modifiers::NONE);
    h.step();
    for k in 1..=4 {
        h.event(Event::PointerMoved(from + vec2(0.0, 5.0 * k as f32)));
        h.step();
    }
    assert_eq!(step_of(h.state(), clip, 1, 4).velocity, 90);
    h.key_press(egui::Key::Escape);
    h.step();
    h.event(Event::PointerMoved(from + vec2(0.0, 40.0)));
    h.step();
    button(&h, from + vec2(0.0, 40.0), false, Modifiers::NONE);
    h.run_steps(2);
    assert_eq!(
        step_of(h.state(), clip, 1, 4).velocity,
        110,
        "back to the start"
    );
    assert_eq!(h.state().doc().undo_label().map(str::to_owned), undo);
}

/// Every visible step is a check box named like "Step 5, Kick, an, Velocity 100"; with a low
/// dock only the visible rows have nodes, and the wheel scrolls the rows.
#[test]
fn visible_steps_are_accesskit_check_boxes() {
    let mut h = harness(opened());
    let boxes = step_boxes(&h);
    assert_eq!(boxes, 8 * 16);
    assert_eq!(step_name(&h, "Kick", 4), "Step 5, Kick, an, Velocity 100");
    assert_eq!(step_name(&h, "Kick", 1), "Step 2, Kick, aus");
    h.state_mut().dispatch(ui(UiAction::ResizePanel {
        panel: Panel::Dock,
        size: komponist_tokens::size::dock::MIN,
    }));
    h.run_steps(3);
    let visible = step_boxes(&h);
    assert!((16..8 * 16).contains(&visible), "{visible}");
    assert!(h.query_by_label_contains(&step_label("Perc", 0)).is_none());
    let at = h
        .get_by_label_contains(&step_label("Kick", 8))
        .rect()
        .center();
    h.event(Event::PointerMoved(at));
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -400.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(10);
    assert!(h.query_by_label_contains(&step_label("Perc", 0)).is_some());
}
