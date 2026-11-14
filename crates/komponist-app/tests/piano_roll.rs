//! The piano roll (WP-41) through `egui_kittest`: one test per row of `ix.note`
//! (`Designsystem.dc.html`), the toolbar, the keyboard, the velocity lane, the AccessKit nodes
//! of the visible notes, snapshots next to the S01 dock and of an empty clip, and the frame time
//! with 10 000 notes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::{FocusArea, Tool};
use komponist_app::views::piano_roll::geometry::Geometry;
use komponist_app::views::piano_roll::names;
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_model::edits::{NewClip, NewClipContent, NewNote};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipContent, ClipId, Grid, Note, NoteId, Project, Tick};
use komponist_tokens::size;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const W: f32 = 1920.0;
const H: f32 = 1080.0;
const BAR: i64 = 3840;

/// The node with `role` and `label` in the dock: the arrangement above has its own ruler
/// ("Lineal"), "Snap" and zoom buttons, so of several matches the piano roll's is the lowest.
fn dock_node<'h>(h: &'h Harness<'_, App>, role: Role, label: &'h str) -> egui_kittest::Node<'h> {
    h.get_all_by_role_and_label(role, label)
        .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("node")
}

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
    h.run_steps(3);
    h
}

/// The toolbar's field `label`: the inspector (WP-43) shows the clip's fields under the same
/// names, so take the one left of it.
fn toolbar_field<'h>(h: &'h Harness<'_, App>, label: &'h str) -> egui_kittest::Node<'h> {
    let inspector = W - h.state().ui().layout.inspector_width;
    h.query_all_by_role_and_label(Role::SpinButton, label)
        .find(|n| n.rect().right() <= inspector)
        .unwrap_or_else(|| panic!("no toolbar field {label}"))
}

/// Dock height of the interaction tests: 30 key rows, so all of Lead A (G3 … C5) and some room
/// above are visible.
const TEST_DOCK: f32 = 500.0;

/// The demo with Lead A (bars 9–10) open in the piano roll, the dock [`TEST_DOCK`] high.
fn lead_a() -> (App, ClipId) {
    let mut app = App::headless(demo_project());
    app.ui_mut().layout.dock_height = TEST_DOCK;
    let clip = app.project().tracks[2].clips[0].id;
    app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    (app, clip)
}

fn notes_of(app: &App, clip: ClipId) -> Vec<Note> {
    let (_, c) = app.project().find_clip(clip).unwrap();
    match &c.content {
        ClipContent::Midi(m) => m.active().notes.to_vec(),
        ClipContent::Step(_) => unreachable!(),
    }
}

/// The piano roll's geometry in the harness window (the dock body at the default layout).
fn geometry(h: &Harness<'_, App>, clip: ClipId) -> Geometry {
    let app = h.state();
    let (_, c) = app.project().find_clip(clip).unwrap();
    // The dock body: below the tab strip, between browser and inspector, above the status bar.
    let name = format!("{} · Piano Roll", c.name);
    let top = h.get_by_role_and_label(Role::Tab, &name).rect().bottom();
    let l = app.ui().layout;
    let left = l.browser_width + 1.0;
    let right = W - l.inspector_width - 1.0;
    let bottom = H - size::STATUSBAR - 1.0;
    let body = Rect::from_min_max(pos2(left, top), pos2(right, bottom));
    Geometry::new(body, &app.ui().piano_roll, c.start, app.project().meter)
}

fn note_center(g: &Geometry, n: &Note) -> Pos2 {
    g.note_rect(n.start, n.length, n.pitch).center()
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, modifiers: Modifiers, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
}

fn click(h: &mut Harness<'_, App>, pos: Pos2, modifiers: Modifiers) {
    h.hover_at(pos);
    h.step();
    press(h, pos, modifiers, true);
    press(h, pos, modifiers, false);
    h.run_steps(2);
}

fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        press(h, pos, Modifiers::NONE, true);
        press(h, pos, Modifiers::NONE, false);
        h.step();
    }
    h.run_steps(2);
}

/// Press at `from` with `modifiers` held, move to `to` in steps, release.
fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2, modifiers: Modifiers) {
    drag_with(h, from, to, modifiers, modifiers);
}

/// Press at `from` with `press` held, then hold `held` while moving to `to`, release.
fn drag_with(
    h: &mut Harness<'_, App>,
    from: Pos2,
    to: Pos2,
    press_mods: Modifiers,
    held: Modifiers,
) {
    let modifiers = held;
    h.hover_at(from);
    h.event(Event::ModifiersChanged(press_mods));
    h.step();
    press(h, from, press_mods, true);
    h.step();
    h.event(Event::ModifiersChanged(held));
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        h.hover_at(from + (to - from) * t);
        h.step();
    }
    press(h, to, modifiers, false);
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
}

fn select(app: &mut App, notes: Vec<NoteId>) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Notes {
        notes,
        mode: SelectMode::Replace,
    })));
}

fn undo(h: &mut Harness<'_, App>) {
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
}

// ------------------------------------------------------------------------------------------------
// ix.note

/// "Stift: Klick / Ziehen — Note setzen, Länge = Raster / Länge aufziehen."
#[test]
fn pencil_click_sets_a_grid_note_and_drag_pulls_its_length() {
    let (mut app, clip) = lead_a();
    app.dispatch(Action::Ui(UiAction::SetTool(Tool::Pencil)));
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let before = notes_of(h.state(), clip).len();
    // An empty spot: key 75 (D#5) at the start of bar 10 + a bit.
    let at = pos2(g.x_of_local(Tick(BAR + 300)), g.row_top(75) + 4.0);
    click(&mut h, at, Modifiers::NONE);
    let notes = notes_of(h.state(), clip);
    assert_eq!(notes.len(), before + 1);
    let new = notes.iter().find(|n| n.pitch == 75).expect("a note on D#5");
    assert_eq!((new.start, new.length), (Tick(BAR + 240), Tick(240)));
    assert_eq!(new.velocity, 100);
    assert_eq!(h.state().ui().selection.notes, [new.id].into());
    assert_eq!(h.state().doc().undo_label(), Some("Noten hinzufügen"));
    // Drag from an empty spot: the length follows in grid steps.
    let from = pos2(g.x_of_local(Tick(0)) + 2.0, g.row_top(76) + 4.0);
    let to = pos2(g.x_of_local(Tick(4 * 240)) + 3.0, from.y);
    drag(&mut h, from, to, Modifiers::NONE);
    let n = notes_of(h.state(), clip)
        .into_iter()
        .find(|n| n.pitch == 76)
        .expect("a note on E5");
    assert_eq!((n.start, n.length), (Tick(0), Tick(4 * 240)));
    // One undo step each.
    undo(&mut h);
    assert!(notes_of(h.state(), clip).iter().all(|n| n.pitch != 76));
}

/// "Zeiger: Doppelklick — Note setzen oder löschen."
#[test]
fn pointer_double_click_sets_and_deletes() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    assert_eq!(h.state().ui().piano_roll.tool, Tool::Pointer);
    let g = geometry(&h, clip);
    let at = pos2(g.x_of_local(Tick(BAR + 2 * 240 + 10)), g.row_top(75) + 4.0);
    double_click(&mut h, at);
    let notes = notes_of(h.state(), clip);
    let new = *notes.iter().find(|n| n.pitch == 75).expect("set");
    assert_eq!((new.start, new.length), (Tick(BAR + 480), Tick(240)));
    // Past egui's double-click window, so the next two clicks are not a triple click.
    h.run_steps(30);
    double_click(&mut h, note_center(&g, &new));
    assert!(
        notes_of(h.state(), clip).iter().all(|n| n.id != new.id),
        "deleted"
    );
}

/// "Ziehen — Zeit und Tonhöhe. Shift: ohne Snap. Alt: Kopie."
#[test]
fn dragging_moves_in_time_and_pitch() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let n = notes_of(h.state(), clip)[2]; // G4, 0 … 2 steps
    let from = note_center(&g, &n);
    let step = g.axis.width(Tick(240));
    // Two steps right (+ a bit, snapped away), two keys up.
    let to = from + vec2(2.0 * step + 4.0, -2.0 * g.key_height);
    drag(&mut h, from, to, Modifiers::NONE);
    let moved = *notes_of(h.state(), clip)
        .iter()
        .find(|q| q.id == n.id)
        .unwrap();
    assert_eq!(moved.start, n.start + Tick(480));
    assert_eq!(moved.pitch, n.pitch + 2);
    assert_eq!(h.state().doc().undo_label(), Some("Noten verschieben"));
    undo(&mut h);
    assert_eq!(notes_of(h.state(), clip)[2], n, "one undo step");

    // Shift held while dragging: without snap (Shift on the press would toggle the selection).
    let from = note_center(&g, &n);
    drag_with(
        &mut h,
        from,
        from + vec2(step * 0.5, 0.0),
        Modifiers::NONE,
        Modifiers::SHIFT,
    );
    let moved = *notes_of(h.state(), clip)
        .iter()
        .find(|q| q.id == n.id)
        .unwrap();
    let off = (moved.start - n.start).0;
    assert!(
        (100..=140).contains(&off),
        "free move by half a step: {off}"
    );

    // Alt: a copy, the original stays.
    undo(&mut h);
    let count = notes_of(h.state(), clip).len();
    let from = note_center(&g, &n);
    drag(&mut h, from, from + vec2(4.0 * step, 0.0), Modifiers::ALT);
    let notes = notes_of(h.state(), clip);
    assert_eq!(notes.len(), count + 1);
    assert!(notes.contains(&n), "original unchanged");
    let copy = notes
        .iter()
        .find(|q| q.pitch == n.pitch && q.start == n.start + Tick(960))
        .expect("copy one beat later");
    assert_eq!(h.state().ui().selection.notes, [copy.id].into());
    assert_eq!(h.state().doc().undo_label(), Some("Noten kopieren"));
}

#[test]
fn esc_during_a_drag_restores_the_notes() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let n = notes_of(h.state(), clip)[2];
    let label = h.state().doc().undo_label().map(str::to_owned);
    let from = note_center(&g, &n);
    let step = g.axis.width(Tick(240));
    h.hover_at(from);
    h.step();
    press(&mut h, from, Modifiers::NONE, true);
    h.step();
    for i in 1..=4 {
        h.hover_at(from + vec2(i as f32 * step, 0.0));
        h.step();
    }
    let moved = notes_of(h.state(), clip)
        .into_iter()
        .find(|q| q.id == n.id)
        .unwrap();
    assert_eq!(moved.start, n.start + Tick(4 * 240), "moving");
    h.key_press(egui::Key::Escape);
    h.step();
    // Moving on after Esc changes nothing.
    h.hover_at(from + vec2(6.0 * step, 0.0));
    h.step();
    press(&mut h, from + vec2(6.0 * step, 0.0), Modifiers::NONE, false);
    h.run_steps(2);
    assert!(notes_of(h.state(), clip).contains(&n), "back at the start");
    assert_eq!(h.state().doc().undo_label().map(str::to_owned), label);
}

/// "Kante ziehen — Länge ändern, gilt für alle ausgewählten Noten."
#[test]
fn edge_drag_resizes_all_selected() {
    let (mut app, clip) = lead_a();
    let notes = notes_of(&app, clip);
    let (a, b) = (notes[0], notes[1]); // C4 and E4, 8 steps each
    select(&mut app, vec![a.id, b.id]);
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let r = g.note_rect(a.start, a.length, a.pitch);
    let from = pos2(r.right() - 2.0, r.center().y);
    let step = g.axis.width(Tick(240));
    drag(&mut h, from, from + vec2(-2.0 * step, 0.0), Modifiers::NONE);
    let after = notes_of(h.state(), clip);
    let get = |id| after.iter().find(|n| n.id == id).unwrap();
    assert_eq!(get(a.id).length, a.length - Tick(480));
    assert_eq!(get(b.id).length, b.length - Tick(480));
    assert_eq!(h.state().doc().undo_label(), Some("Noten-Länge"));
}

/// "Strg-Ziehen ↕ auf Note — Velocity ändern, Wert im Tooltip."
#[test]
fn ctrl_drag_changes_velocity() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let n = notes_of(h.state(), clip)[0]; // velocity 70
    let from = note_center(&g, &n);
    drag(&mut h, from, from + vec2(0.0, -15.0), Modifiers::COMMAND);
    let after = *notes_of(h.state(), clip)
        .iter()
        .find(|q| q.id == n.id)
        .unwrap();
    assert_eq!(after.velocity, n.velocity + 10);
    assert_eq!((after.start, after.pitch), (n.start, n.pitch));
    assert_eq!(h.state().doc().undo_label(), Some("Velocity"));
}

/// "↑ ↓ / Shift ↑ ↓ — Halbton / Oktave." and "← → / Q — Ein Rasterschritt / quantisieren."
#[test]
fn arrows_transpose_and_nudge_q_quantizes() {
    let (mut app, clip) = lead_a();
    let n = notes_of(&app, clip)[3]; // A4 at 2 steps
    select(&mut app, vec![n.id]);
    let mut h = harness(app);
    let get = |h: &Harness<'_, App>| {
        *notes_of(h.state(), clip)
            .iter()
            .find(|q| q.id == n.id)
            .unwrap()
    };
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(get(&h).pitch, n.pitch + 1);
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(get(&h).pitch, n.pitch + 1 - 12);
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(get(&h).start, n.start + Tick(240));
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(get(&h).start, n.start);
    // Q: off-grid notes snap to the grid.
    let g = geometry(&h, clip);
    let from = note_center(&g, &get(&h));
    drag_with(
        &mut h,
        from,
        from + vec2(g.axis.width(Tick(100)), 0.0),
        Modifiers::NONE,
        Modifiers::SHIFT,
    );
    assert_eq!(get(&h).start, n.start + Tick(100));
    h.key_press(egui::Key::Q);
    h.run_steps(2);
    assert_eq!(get(&h).start, n.start);
    // The toolbar button does the same.
    h.get_by_role_and_label(Role::Button, names::QUANTIZE)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().doc().undo_label(), Some("Quantisieren"));
}

/// "Strg A / Entf — Alle wählen / löschen."
#[test]
fn ctrl_a_selects_all_and_delete_deletes() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let total = notes_of(h.state(), clip).len();
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
    h.run_steps(2);
    assert_eq!(h.state().ui().selection.notes.len(), total);
    h.get_by_role_and_label(Role::Label, &format!("{total} Noten"));
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(notes_of(h.state(), clip).is_empty());
    // The empty clip shows its hint.
    h.get_by_role_and_label(Role::Label, names::EMPTY_HINT);
}

// ------------------------------------------------------------------------------------------------
// Selection, tools, keyboard, lane

#[test]
fn click_shift_click_and_box_select() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let notes = notes_of(h.state(), clip);
    click(&mut h, note_center(&g, &notes[2]), Modifiers::NONE);
    assert_eq!(h.state().ui().selection.notes, [notes[2].id].into());
    click(&mut h, note_center(&g, &notes[3]), Modifiers::SHIFT);
    assert_eq!(
        h.state().ui().selection.notes,
        [notes[2].id, notes[3].id].into()
    );
    click(&mut h, note_center(&g, &notes[2]), Modifiers::SHIFT);
    assert_eq!(h.state().ui().selection.notes, [notes[3].id].into());
    // A box from an empty spot on G#4 down to C4 over the first 220 ticks takes C4, E4, G4
    // (A4 starts at 480).
    let a = pos2(g.x_of_local(Tick(220)), g.row_top(68) + 6.0);
    let b = pos2(g.x_of_local(Tick(0)) + 1.0, g.row_top(60) + 10.0);
    drag(&mut h, a, b, Modifiers::NONE);
    let want: std::collections::BTreeSet<NoteId> = [notes[0].id, notes[1].id, notes[2].id].into();
    assert_eq!(h.state().ui().selection.notes, want);
    // A click on empty space clears.
    click(
        &mut h,
        pos2(g.x_of_local(Tick(BAR + 3000)), g.row_top(75) + 3.0),
        Modifiers::NONE,
    );
    assert!(h.state().ui().selection.notes.is_empty());
}

#[test]
fn eraser_deletes_by_click_and_drag() {
    let (mut app, clip) = lead_a();
    app.dispatch(Action::Ui(UiAction::SetTool(Tool::Eraser)));
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let notes = notes_of(h.state(), clip);
    click(&mut h, note_center(&g, &notes[2]), Modifiers::NONE);
    assert_eq!(notes_of(h.state(), clip).len(), notes.len() - 1);
    // Drag across the C4 and E4 rows at the clip start.
    let from = pos2(g.x_of_local(Tick(100)), g.row_top(64) + 5.0);
    let to = pos2(g.x_of_local(Tick(100)), g.row_top(60) + 5.0);
    drag(&mut h, from, to, Modifiers::NONE);
    let left = notes_of(h.state(), clip);
    assert!(
        left.iter()
            .all(|n| n.id != notes[0].id && n.id != notes[1].id)
    );
    undo(&mut h);
    assert_eq!(notes_of(h.state(), clip).len(), notes.len() - 1, "one step");
}

#[test]
fn toolbar_switches_tools_grid_and_snap() {
    let (app, _) = lead_a();
    let mut h = harness(app);
    h.get_by_role_and_label(Role::RadioButton, names::PENCIL)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().ui().piano_roll.tool, Tool::Pencil);
    h.key_press(egui::Key::Num3);
    h.run_steps(2);
    assert_eq!(h.state().ui().piano_roll.tool, Tool::Eraser);
    dock_node(&h, Role::Button, names::SNAP).click();
    h.run_steps(2);
    assert!(!h.state().ui().piano_roll.snap);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(2);
    assert_eq!(h.state().ui().piano_roll.grid, Grid::Eighth);
    dock_node(&h, Role::Button, names::ZOOM_IN).click();
    h.run_steps(2);
    assert!(h.state().ui().piano_roll.px_per_bar > 320.0);
    h.key_press_modifiers(Modifiers::ALT, egui::Key::Plus);
    h.run_steps(2);
    assert_eq!(h.state().ui().piano_roll.key_height, 15.0);
}

#[test]
fn toolbar_fields_change_the_selection() {
    let (mut app, clip) = lead_a();
    let n = notes_of(&app, clip)[2]; // G4, velocity 100, 2 steps
    select(&mut app, vec![n.id]);
    let mut h = harness(app);
    let field = toolbar_field(&h, names::VELOCITY_FIELD);
    assert_eq!(field.value().as_deref(), Some("100"));
    toolbar_field(&h, names::LENGTH_FIELD);
    let note = toolbar_field(&h, names::NOTE_FIELD);
    assert_eq!(note.value().as_deref(), Some("G4"));
    note.increment_accesskit();
    h.run_steps(2);
    let get = |h: &Harness<'_, App>| {
        *notes_of(h.state(), clip)
            .iter()
            .find(|q| q.id == n.id)
            .unwrap()
    };
    assert_eq!(get(&h).pitch, n.pitch + 1);
    toolbar_field(&h, names::LENGTH_FIELD).increment_accesskit();
    h.run_steps(2);
    assert_eq!(get(&h).length, n.length + Tick(240));
    // Drag ↑ on Vel: 10 px = +10, one undo step.
    let r = toolbar_field(&h, names::VELOCITY_FIELD).rect();
    drag(
        &mut h,
        r.center(),
        r.center() + vec2(0.0, -10.0),
        Modifiers::NONE,
    );
    assert_eq!(get(&h).velocity, 110);
    assert_eq!(h.state().doc().undo_label(), Some("Velocity"));
}

#[test]
fn keyboard_click_auditions_on_the_clip_track() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let track = h.state().project().find_clip(clip).unwrap().0.id;
    h.state_mut().clear_dispatched();
    let key = h.get_by_role_and_label(Role::Button, "Taste C4");
    let at = key.rect().center();
    h.hover_at(at);
    h.step();
    press(&mut h, at, Modifiers::NONE, true);
    h.run_steps(2);
    let on = Action::Transport(TransportAction::LiveNote {
        track: Some(track),
        key: 60,
        velocity: 100,
    });
    assert!(h.state().dispatched().any(|a| *a == on));
    press(&mut h, at, Modifiers::NONE, false);
    h.run_steps(2);
    let off = Action::Transport(TransportAction::LiveNote {
        track: Some(track),
        key: 60,
        velocity: 0,
    });
    assert!(h.state().dispatched().any(|a| *a == off));
    assert_eq!(h.state().ui().focus, FocusArea::PianoRoll);
}

#[test]
fn velocity_lane_sets_the_nearest_start() {
    let (app, clip) = lead_a();
    let mut h = harness(app);
    let g = geometry(&h, clip);
    let lane = g.lane.unwrap();
    let notes = notes_of(h.state(), clip);
    // Step 4: B4 alone (velocity 112). Press at the stem's base + 26 px → 64.
    let b4 = notes.iter().find(|n| n.start == Tick(4 * 240)).unwrap();
    let at = pos2(g.x_of_local(b4.start) + 2.0, lane.bottom() - 4.0 - 26.0);
    click(&mut h, at, Modifiers::NONE);
    let after = notes_of(h.state(), clip);
    assert_eq!(after.iter().find(|n| n.id == b4.id).unwrap().velocity, 64);
    assert_eq!(h.state().doc().undo_label(), Some("Velocity"));
}

#[test]
fn notes_past_the_clip_end_lengthen_it() {
    let (mut app, clip) = lead_a();
    app.dispatch(Action::Ui(UiAction::SetTool(Tool::Pencil)));
    let mut h = harness(app);
    let g = geometry(&h, clip);
    assert_eq!(
        h.state().project().find_clip(clip).unwrap().1.length,
        Tick(2 * BAR)
    );
    let at = pos2(g.x_of_local(Tick(2 * BAR + 300)), g.row_top(72) + 4.0);
    click(&mut h, at, Modifiers::NONE);
    assert_eq!(
        h.state().project().find_clip(clip).unwrap().1.length,
        Tick(3 * BAR)
    );
    undo(&mut h);
    assert_eq!(
        h.state().project().find_clip(clip).unwrap().1.length,
        Tick(2 * BAR),
        "note and clip length are one undo step"
    );
}

// ------------------------------------------------------------------------------------------------
// Empty clip, playback, accessibility

/// S10 "Leerer Clip — Piano Roll zeigt Raster und Klaviatur, Stift ist vorausgewählt."
fn empty_clip_app() -> (App, ClipId) {
    let mut app = App::headless(demo_project());
    let track = app.project().tracks[2].id;
    app.dispatch(Action::Edit(EditAction::AddClip {
        track,
        clip: NewClip {
            start: Tick(16 * BAR),
            length: Tick(BAR),
            name: "MIDI-Clip".to_owned(),
            content: NewClipContent::Midi {
                notes: Vec::new(),
                take_name: String::new(),
            },
        },
    }));
    let clip = app.ui().editor.clip.expect("opened");
    (app, clip)
}

#[test]
fn an_empty_clip_opens_with_the_pencil() {
    let (app, clip) = empty_clip_app();
    assert_eq!(app.ui().piano_roll.tool, Tool::Pencil);
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Label, names::EMPTY_HINT);
    h.get_by_role_and_label(Role::Button, "Taste C4");
    h.get_by_role_and_label(Role::Label, "0 Noten");
    let g = geometry(&h, clip);
    click(
        &mut h,
        pos2(g.x_of_local(Tick(10)), g.row_top(64) + 3.0),
        Modifiers::NONE,
    );
    assert_eq!(notes_of(h.state(), clip).len(), 1);
    h.get_by_role_and_label(Role::GridCell, "Note E4, 17.1.000");
}

#[test]
fn visible_notes_have_accesskit_nodes() {
    let (mut app, clip) = lead_a();
    let n = notes_of(&app, clip)[3];
    select(&mut app, vec![n.id]);
    let h = harness(app);
    // Lead A starts at bar 9: C4 at 9.1.000; A4 two steps later.
    h.get_by_role_and_label(Role::GridCell, "Note C4, 9.1.000");
    let a4 = h.get_by_role_and_label(Role::GridCell, "Note A4, 9.1.120");
    assert_eq!(a4.accesskit_node().is_selected(), Some(true));
    h.get_by_role_and_label(Role::Grid, names::NOTES);
    h.get_by_role_and_label(Role::Group, names::KEYBOARD);
    h.get_by_role_and_label(Role::Group, names::VELOCITY_LANE);
    dock_node(&h, Role::Slider, "Lineal");
    // Only visible notes: none below the lowest visible key.
    let g = geometry(&h, clip);
    let hidden = notes_of(h.state(), clip)
        .iter()
        .filter(|n| n.pitch < g.bottom_key() || n.pitch > g.top_key)
        .count();
    let cells = h
        .root()
        .children_recursive()
        .filter(|c| {
            // Notes only: the arrangement's clips are grid cells too.
            let n = c.accesskit_node();
            n.role() == Role::GridCell && n.label().is_some_and(|l| l.starts_with("Note "))
        })
        .count();
    assert_eq!(cells, notes_of(h.state(), clip).len() - hidden);
}

#[test]
fn notes_and_keys_light_up_while_playing() {
    // Pure part in `edit::tests::playhead_inside_clips_and_loops`; here through the engine.
    let (mut app, clip) = lead_a();
    app.dispatch(Action::Transport(TransportAction::Locate(Tick(8 * BAR))));
    let mut h = harness(app);
    h.key_press(egui::Key::Space);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && h.state().engine().playhead() < Tick(8 * BAR + 100) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
    let g = geometry(&h, clip);
    let image = h.render().expect("render");
    let ppp = 1.0;
    // C4 (60) sounds from the clip start for 8 steps: its key shows `key-playing`.
    let key = Rect::from_min_size(
        pos2(g.keys.left() + 2.0, g.row_top(60) + 2.0),
        vec2(4.0, 4.0),
    );
    let px = image.get_pixel((key.center().x * ppp) as u32, (key.center().y * ppp) as u32);
    let playing =
        komponist_tokens::TrackPalette::new(h.state().project().tracks[2].hue.0).key_playing;
    assert_eq!([px[0], px[1], px[2]], [playing[0], playing[1], playing[2]]);
    h.key_press(egui::Key::Space);
    h.run_steps(2);
}

// ------------------------------------------------------------------------------------------------
// Snapshots

/// The S01 state of the dock: Lead A, notes 7 and 8 selected (`pianoRoll({sel:[6,7]})`),
/// playhead at 10.2.120, stopped.
fn s01() -> App {
    let (mut app, clip) = lead_a();
    app.ui_mut().layout.dock_height = size::dock::DEFAULT;
    let notes = notes_of(&app, clip);
    select(&mut app, vec![notes[6].id, notes[7].id]);
    app.dispatch(Action::Transport(TransportAction::Locate(Tick(36_000))));
    app
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_app::panels::transport::names::CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshots_of_the_piano_roll() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let mut h = harness_at(s01(), ppp);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && h.state().engine().playhead() != Tick(36_000) {
            h.step();
        }
        // The screen's key range (`pianoRoll`: `TOP=74`); opening the clip fitted it to the top.
        h.state_mut().ui_mut().piano_roll.top_key = 74;
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot(format!("piano-roll-s01-{ppp}x"));
        results.extend_harness(&mut h);
    }
    let (app, _) = empty_clip_app();
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    mask_cpu(&mut h);
    h.snapshot("piano-roll-empty-1x");
    results.extend_harness(&mut h);

    // Zoom levels (`notes`: "mit Name ab h 12, w 28", "zoom · Taste 4 · Auswahl =
    // Akzentfläche"): key height 24 shows names, key height 4 fills the selection with accent.
    for (name, key_height, px_per_bar) in [("keys-24", 24.0, 640.0), ("keys-4", 4.0, 160.0)] {
        let mut app = s01();
        app.ui_mut().layout.dock_height = TEST_DOCK;
        app.ui_mut().piano_roll.key_height = key_height;
        app.ui_mut().piano_roll.px_per_bar = px_per_bar;
        let mut h = harness_at(app, 1.0);
        h.run_steps(3);
        let top = if key_height > 12.0 { 74 } else { 90 };
        h.state_mut().ui_mut().piano_roll.top_key = top;
        h.run_steps(2);
        mask_cpu(&mut h);
        h.snapshot(format!("piano-roll-{name}-1x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// Performance

/// 10 000 notes in one clip (ARCHITECTURE §9.2: UI frame ≤ 8 ms). Debug builds are slow, so
/// the hard limit is only checked with optimisations; the time is printed either way.
#[test]
fn ten_thousand_notes_frame_time() {
    let p: Project = demo_project();
    let track = p.tracks[2].id;
    let notes: Vec<NewNote> = (0..10_000)
        .map(|i| NewNote {
            start: Tick(i as i64 * 60),
            length: Tick(120 + (i as i64 % 7) * 60),
            pitch: 48 + (i * 7 % 36) as u8,
            velocity: 30 + (i * 13 % 97) as u8,
        })
        .collect();
    let (p, clip) = komponist_model::edits::add_clip(
        &p,
        track,
        NewClip {
            start: Tick(32 * BAR),
            length: Tick(10_000 * 60 + BAR),
            name: "Dicht".to_owned(),
            content: NewClipContent::Midi {
                notes,
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let mut app = App::headless(p);
    app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    // Zoomed out so that thousands are visible: 1/16 bar per 24 px.
    app.ui_mut().piano_roll.px_per_bar = 24.0;
    app.ui_mut().piano_roll.key_height = 4.0;
    app.dispatch(Action::Ui(UiAction::SelectAll));
    let mut h = harness(app);
    let mut times = Vec::new();
    for i in 0..20 {
        h.hover_at(pos2(600.0 + i as f32, 900.0));
        let t = Instant::now();
        h.step();
        times.push(t.elapsed());
    }
    times.sort();
    let median = times[times.len() / 2];
    eprintln!("piano roll, 10 000 notes, median frame (whole app, CPU side): {median:?}");
    if !cfg!(debug_assertions) {
        assert!(median < Duration::from_millis(8), "{median:?}");
    }
}
