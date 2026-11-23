//! The pointer gestures of the automation curve editor in the whole app: move, Alt-copy, bend,
//! Ctrl-draw, Shift-fine and Esc. Each gesture is one undo step; Esc leaves no step behind.

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_app::{Action, App, EditAction};
use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
use komponist_model::edits::{self, NewAudioTrack};
use komponist_model::{Project, Tick, TrackId};

const BAR: i64 = 3840;

/// A track with a shown volume lane and the points at bars 2, 4 and 6 (low, high, low).
fn lane_app() -> (App, TrackId) {
    let (project, track) =
        edits::add_audio_track(&Project::new("Curve"), NewAudioTrack::default()).unwrap();
    let owner = AutomationOwner::Track(track);
    let (project, lane) = edits::automation::add_lane(
        &project,
        owner,
        AutomationTarget::Volume,
        komponist_i18n::de::PARAM_VOLUME.to_owned(),
    )
    .unwrap();
    let project = edits::automation::set_shown(&project, owner, true).unwrap();
    let points = Points::new(vec![
        Point::linear(Tick(BAR), 0.2),
        Point::linear(Tick(3 * BAR), 0.8),
        Point::linear(Tick(5 * BAR), 0.2),
    ]);
    let project = edits::automation::set_lane_points(&project, owner, lane, points).unwrap();
    (App::headless(project), track)
}

fn harness() -> (Harness<'static, App>, TrackId) {
    let (app, track) = lane_app();
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    (h, track)
}

fn points(h: &Harness<'_, App>, track: TrackId) -> Vec<Point> {
    h.state().project().track(track).unwrap().automation.lanes[0]
        .points
        .as_slice()
        .to_vec()
}

fn undo_label(h: &Harness<'_, App>) -> Option<String> {
    h.state().doc().undo_label().map(str::to_owned)
}

/// The centres of the point nodes, left to right.
fn point_positions(h: &Harness<'_, App>) -> Vec<Pos2> {
    let mut rects: Vec<Rect> = h
        .get_all_by_role(Role::Slider)
        .filter(|node| {
            node.accesskit_node()
                .label()
                .is_some_and(|label| label.starts_with("Punkt "))
        })
        .map(|node| node.rect())
        .collect();
    rects.sort_by(|a, b| a.left().total_cmp(&b.left()));
    rects.iter().map(Rect::center).collect()
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
}

/// Presses at `from`, moves in steps to `to` with `modifiers` held and leaves the button down.
fn drag_start(h: &mut Harness<'_, App>, from: Pos2, to: Pos2, modifiers: Modifiers) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(from));
    h.step();
    press(h, from, true, modifiers);
    h.step();
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
}

fn release(h: &mut Harness<'_, App>, at: Pos2, modifiers: Modifiers) {
    press(h, at, false, modifiers);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2, modifiers: Modifiers) {
    drag_start(h, from, to, modifiers);
    release(h, to, modifiers);
}

fn undo(h: &mut Harness<'_, App>) {
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
}

/// The hollow diamond between the first two points: the bend handle of the first segment.
fn first_handle(h: &Harness<'_, App>) -> Pos2 {
    let p = point_positions(h);
    pos2((p[0].x + p[1].x) / 2.0, (p[0].y + p[1].y) / 2.0)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn dragging_a_point_moves_it_in_one_undo_step() {
    let (mut h, track) = harness();
    let original = points(&h, track);
    let b = point_positions(&h)[1];
    // One bar right on the grid, a quarter of the lane down.
    drag(&mut h, b, b + vec2(72.0, 10.0), Modifiers::NONE);
    let moved = points(&h, track);
    assert_eq!(moved.len(), 3);
    assert_eq!(moved[1].tick, Tick(4 * BAR));
    assert!(near(moved[1].value, 0.55), "{}", moved[1].value);
    assert_eq!((moved[0], moved[2]), (original[0], original[2]));
    assert_eq!(
        undo_label(&h).as_deref(),
        Some("undo-automation-move-points")
    );
    undo(&mut h);
    assert_eq!(points(&h, track), original);
    assert_eq!(undo_label(&h), None);
}

#[test]
fn alt_drag_copies_the_point_and_the_original_stays() {
    let (mut h, track) = harness();
    let original = points(&h, track);
    let b = point_positions(&h)[1];
    drag(&mut h, b, b + vec2(72.0, 0.0), Modifiers::ALT);
    let copied = points(&h, track);
    assert_eq!(copied.len(), 4);
    assert_eq!(copied[1], original[1]);
    assert_eq!(copied[2].tick, Tick(4 * BAR));
    assert_eq!(copied[2].value, original[1].value);
    assert_eq!(
        undo_label(&h).as_deref(),
        Some("undo-automation-copy-points")
    );
    undo(&mut h);
    assert_eq!(points(&h, track), original);
}

#[test]
fn dragging_a_segment_handle_bends_it() {
    let (mut h, track) = harness();
    let original = points(&h, track);
    let handle = first_handle(&h);
    // Half of the lane's half height up: tension 0.5.
    drag(&mut h, handle, handle - vec2(0.0, 10.0), Modifiers::NONE);
    let bent = points(&h, track);
    let Shape::Curve { tension } = bent[0].shape else {
        panic!("{:?}", bent[0].shape)
    };
    assert!(near(tension, 0.5), "{tension}");
    assert_eq!((bent[1], bent[2]), (original[1], original[2]));
    assert_eq!(undo_label(&h).as_deref(), Some("undo-automation-bend"));
    undo(&mut h);
    assert_eq!(points(&h, track), original);
}

#[test]
fn ctrl_drag_draws_a_curve_over_the_points_it_crosses() {
    let (mut h, track) = harness();
    let original = points(&h, track);
    let from = pos2(580.0, 175.0);
    drag(&mut h, from, pos2(780.0, 165.0), Modifiers::COMMAND);
    let drawn = points(&h, track);
    // The drawn stroke replaces the middle point and keeps the outer ones.
    assert_eq!(drawn.first(), original.first());
    assert_eq!(drawn.last(), original.last());
    assert!(!drawn.contains(&original[1]));
    assert!(drawn.len() > 2);
    assert_eq!(undo_label(&h).as_deref(), Some("undo-automation-draw"));
    undo(&mut h);
    assert_eq!(points(&h, track), original);
}

#[test]
fn shift_makes_the_vertical_drag_ten_times_finer_and_skips_the_grid() {
    let (mut h, track) = harness();
    let b = point_positions(&h)[1];
    drag(&mut h, b, b + vec2(5.0, 10.0), Modifiers::SHIFT);
    let fine = points(&h, track)[1];
    // 10 px of 40 px is 0.25 of the range, a tenth of it with Shift.
    assert!(near(fine.value, 0.8 - 0.025), "{}", fine.value);
    // 5 px are about 267 ticks: off the grid, not rounded to it.
    assert_ne!(fine.tick, Tick(3 * BAR));
    assert_ne!(fine.tick.0 % (BAR / 4), 0);
    undo(&mut h);
    let b = point_positions(&h)[1];
    drag(&mut h, b, b + vec2(0.0, 10.0), Modifiers::NONE);
    assert!(near(points(&h, track)[1].value, 0.55));
}

#[test]
fn esc_cancels_a_move_and_a_bend_without_an_undo_step() {
    let (mut h, track) = harness();
    let original = points(&h, track);
    let b = point_positions(&h)[1];
    drag_start(&mut h, b, b + vec2(72.0, 10.0), Modifiers::NONE);
    assert_ne!(points(&h, track), original);
    h.key_press(Key::Escape);
    h.run_steps(2);
    // Moving on after Esc changes nothing.
    h.event(Event::PointerMoved(b + vec2(100.0, 0.0)));
    h.run_steps(2);
    release(&mut h, b + vec2(100.0, 0.0), Modifiers::NONE);
    assert_eq!(points(&h, track), original);
    assert_eq!(undo_label(&h), None);

    let handle = first_handle(&h);
    drag_start(&mut h, handle, handle - vec2(0.0, 10.0), Modifiers::NONE);
    assert_ne!(points(&h, track), original);
    h.key_press(Key::Escape);
    h.run_steps(2);
    release(&mut h, handle - vec2(0.0, 10.0), Modifiers::NONE);
    assert_eq!(points(&h, track), original);
    assert_eq!(undo_label(&h), None);
}

#[test]
fn esc_with_a_modifier_cancels_a_move_and_closes_its_gesture() {
    let (mut h, track) = harness();
    let original = points(&h, track);
    let b = point_positions(&h)[1];
    drag_start(&mut h, b, b + vec2(72.0, 10.0), Modifiers::NONE);
    assert_ne!(points(&h, track), original);
    // The keymap has no binding for ⌘/Ctrl+Esc: the editor itself must cancel the gesture.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Escape);
    h.run_steps(2);
    release(&mut h, b + vec2(72.0, 10.0), Modifiers::NONE);
    assert_eq!(points(&h, track), original);
    assert!(!h.state().doc().in_gesture());
    assert_eq!(undo_label(&h), None);
}
