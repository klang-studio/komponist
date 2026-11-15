//! WP-84 graph: AccessKit nodes, drag / keyboard / Alt-click / double-click / Entf on the real
//! widget, bypass and disabled behaviour. The pictures are the gallery cards of
//! `device_widgets::gallery_cards`.

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_ui::Lang;
use komponist_ui::snapshot;
use komponist_ui::widgets::device::graph::configs::{self, band};
use komponist_ui::widgets::device::graph::{Graph, GraphEvent, GraphSpec, Look, SlotInfo};

struct World {
    values: Vec<f64>,
    events: Vec<GraphEvent>,
    selected: Option<usize>,
    selected_slot: Option<usize>,
    changed: Vec<(usize, f64)>,
}

fn eq_values() -> Vec<f64> {
    // Two bands: a bell at 1 kHz and a bell at 4 kHz.
    vec![
        1.0,
        1000.0,
        0.0,
        1.0,
        band::BELL, //
        1.0,
        4000.0,
        3.0,
        1.0,
        band::BELL,
    ]
}

fn eq_infos() -> Vec<SlotInfo<'static>> {
    let mut v = Vec::new();
    for _ in 0..2 {
        v.push(SlotInfo::new(0.0, 1.0, 1.0));
        v.push(SlotInfo::new(20.0, 20000.0, 1000.0).format(|f| format!("{f:.0} Hz")));
        v.push(SlotInfo::new(-24.0, 24.0, 0.0).format(|g| format!("{g:+.1} dB")));
        v.push(SlotInfo::new(0.1, 18.0, 0.71));
        v.push(SlotInfo::new(0.0, 4.0, 2.0));
    }
    v
}

fn world(values: Vec<f64>) -> World {
    World {
        values,
        events: Vec::new(),
        selected: None,
        selected_slot: None,
        changed: Vec::new(),
    }
}

fn harness(
    spec: GraphSpec,
    infos: impl Fn() -> Vec<SlotInfo<'static>> + 'static,
    look: Look,
    values: Vec<f64>,
) -> Harness<'static, World> {
    let mut h = snapshot::builder::<World>(1.0)
        .with_size(vec2(300.0, 160.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            move |ui, w| {
                snapshot::padded(ui, |ui| {
                    let infos = infos();
                    let out = Graph::new(&spec, &mut w.values, &infos, vec2(236.0, 112.0))
                        .look(look)
                        .playhead(Some(0.3))
                        .show(ui);
                    w.events.extend(out.events);
                    w.selected = out.selected;
                    w.selected_slot = out.selected_slot;
                    w.changed.extend(out.changed);
                });
            },
            world(values),
        );
    h.run();
    h
}

fn eq_harness(look: Look) -> Harness<'static, World> {
    harness(configs::eq(Lang::De, 2, 18.0), eq_infos, look, eq_values())
}

fn pause(h: &mut Harness<'_, World>) {
    for _ in 0..30 {
        h.step();
    }
}

fn drag(h: &mut Harness<'_, World>, from: Pos2, by: Vec2, steps: usize, modifiers: Modifiers) {
    pause(h);
    h.event(Event::PointerMoved(from));
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers,
    });
    for i in 1..=steps {
        h.event(Event::PointerMoved(from + by * (i as f32 / steps as f32)));
    }
    h.event(Event::PointerButton {
        pos: from + by,
        button: PointerButton::Primary,
        pressed: false,
        modifiers,
    });
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

fn click(h: &mut Harness<'_, World>, at: Pos2, modifiers: Modifiers, times: usize) {
    pause(h);
    h.event(Event::PointerMoved(at));
    h.event(Event::ModifiersChanged(modifiers));
    for _ in 0..times {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers,
            });
            h.step();
        }
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

fn center(h: &Harness<'_, World>, role: Role, name: &str) -> Pos2 {
    h.get_by_role_and_label(role, name).rect().center()
}

#[test]
fn the_graph_is_a_group_and_every_parameter_of_a_handle_a_slider() {
    let h = eq_harness(Look::Normal);
    h.check_accessibility();
    h.get_by_role_and_label(Role::Group, "Parametrischer EQ");
    for name in [
        "Band 1 Frequenz",
        "Band 1 Gain",
        "Band 1 Q",
        "Band 2 Frequenz",
        "Band 2 Gain",
        "Band 2 Q",
    ] {
        h.get_by_role_and_label(Role::Slider, name);
    }
    let f = h.get_by_role_and_label(Role::Slider, "Band 2 Frequenz");
    assert_eq!(f.accesskit_node().numeric_value(), Some(4000.0));
    assert_eq!(f.value().as_deref(), Some("4000 Hz"));
    let g = h.get_by_role_and_label(Role::Slider, "Band 2 Gain");
    assert_eq!(g.value().as_deref(), Some("+3.0 dB"));
}

#[test]
fn a_band_that_is_off_has_no_handle() {
    let mut values = eq_values();
    values[5] = 0.0;
    let h = harness(
        configs::eq(Lang::De, 2, 18.0),
        eq_infos,
        Look::Normal,
        values,
    );
    h.get_by_role_and_label(Role::Slider, "Band 1 Frequenz");
    assert!(
        h.query_by_role_and_label(Role::Slider, "Band 2 Frequenz")
            .is_none()
    );
}

#[test]
fn dragging_a_handle_moves_frequency_and_gain_and_selects_it() {
    let mut h = eq_harness(Look::Normal);
    let c = center(&h, Role::Slider, "Band 1 Frequenz");
    drag(&mut h, c, vec2(30.0, -12.0), 10, Modifiers::NONE);
    let w = h.state();
    assert!(w.values[1] > 1000.0, "{}", w.values[1]);
    assert!(w.values[2] > 0.0, "{}", w.values[2]);
    assert_eq!(w.values[6], 4000.0, "the other band stays");
    assert_eq!(w.selected, Some(0));
    // The parameter of the axis moved last: the gain.
    assert_eq!(w.selected_slot, Some(2));
}

#[test]
fn shift_is_fine_and_ctrl_is_coarse() {
    let by = vec2(20.0, 0.0);
    let moved = |m: Modifiers| {
        let mut h = eq_harness(Look::Normal);
        let c = center(&h, Role::Slider, "Band 1 Frequenz");
        drag(&mut h, c, by, 10, m);
        (h.state().values[1] / 1000.0).ln()
    };
    let normal = moved(Modifiers::NONE);
    let fine = moved(Modifiers::SHIFT);
    let coarse = moved(Modifiers::CTRL);
    assert!(normal > 0.0);
    assert!(
        (fine - normal * 0.1).abs() < normal * 0.03,
        "{fine} {normal}"
    );
    assert!(coarse > normal * 5.0, "{coarse} {normal}");
}

#[test]
fn alt_click_sets_the_defaults() {
    let mut h = eq_harness(Look::Normal);
    let c = center(&h, Role::Slider, "Band 2 Frequenz");
    click(&mut h, c, Modifiers::ALT, 1);
    let w = h.state();
    assert_eq!(w.values[6], 1000.0);
    assert_eq!(w.values[7], 0.0);
}

#[test]
fn the_wheel_over_a_handle_changes_q() {
    let mut h = eq_harness(Look::Normal);
    let c = center(&h, Role::Slider, "Band 1 Frequenz");
    pause(&mut h);
    h.event(Event::PointerMoved(c));
    h.run();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: vec2(0.0, 1.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    // egui keeps smoothing the wheel for a few frames.
    h.run_steps(20);
    assert!(h.state().values[3] > 1.0, "{}", h.state().values[3]);
}

#[test]
fn tab_changes_the_handle_and_arrows_move_it() {
    let mut h = eq_harness(Look::Normal);
    h.get_by_role_and_label(Role::Slider, "Band 1 Frequenz")
        .focus();
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.run();
    let f1 = h.state().values[1];
    assert!(f1 > 1000.0, "{f1}");
    h.key_press(egui::Key::ArrowUp);
    h.run();
    assert!(h.state().values[2] > 0.0);
    // Tab: the next handle takes the focus, so the arrows now move band 2.
    h.key_press(egui::Key::Tab);
    h.run();
    h.key_press_modifiers(Modifiers::NONE, egui::Key::ArrowLeft);
    h.run();
    assert!(h.state().values[6] < 4000.0, "{}", h.state().values[6]);
    assert_eq!(h.state().values[1], f1, "band 1 stays");
    // Shift is fine: one step moves less than without.
    let before = h.state().values[6];
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::ArrowLeft);
    h.run();
    let fine = before - h.state().values[6];
    h.key_press(egui::Key::ArrowLeft);
    h.run();
    let normal = (before - fine) - h.state().values[6];
    assert!(fine > 0.0 && fine < normal * 0.3, "{fine} {normal}");
}

#[test]
fn double_click_on_the_area_asks_for_a_band_and_entf_for_a_delete() {
    let mut h = eq_harness(Look::Normal);
    // An empty spot well away from both handles.
    let band = center(&h, Role::Slider, "Band 1 Frequenz");
    let spot = band + vec2(-40.0, 30.0);
    click(&mut h, spot, Modifiers::NONE, 2);
    let events = &h.state().events;
    let Some(GraphEvent::AddPoint { x, y }) = events.first().copied() else {
        panic!("{events:?}");
    };
    assert!(x > 20.0 && x < 1000.0, "{x}");
    assert!(y < 0.0, "below the middle: {y}");
    h.get_by_role_and_label(Role::Slider, "Band 2 Frequenz")
        .focus();
    h.run();
    h.key_press(egui::Key::Delete);
    h.run();
    assert!(h.state().events.contains(&GraphEvent::Delete(1)));
}

#[test]
fn accesskit_can_step_and_set_a_parameter() {
    let mut h = eq_harness(Look::Normal);
    h.get_by_role_and_label(Role::Slider, "Band 2 Frequenz")
        .increment_accesskit();
    h.run();
    assert!(h.state().values[6] > 4000.0);
    h.get_by_role_and_label(Role::Slider, "Band 2 Gain")
        .set_value_accesskit(-6.0);
    h.run();
    assert_eq!(h.state().values[7], -6.0);
    h.get_by_role_and_label(Role::Slider, "Band 1 Q")
        .decrement_accesskit();
    h.run();
    assert!(h.state().values[3] < 1.0);
}

#[test]
fn bypass_stays_operable_and_disabled_does_not() {
    let mut h = eq_harness(Look::Bypass);
    let c = center(&h, Role::Slider, "Band 1 Frequenz");
    drag(&mut h, c, vec2(30.0, 0.0), 10, Modifiers::NONE);
    assert!(h.state().values[1] > 1000.0);

    let mut h = eq_harness(Look::Disabled);
    let c = center(&h, Role::Slider, "Band 1 Frequenz");
    drag(&mut h, c, vec2(30.0, 0.0), 10, Modifiers::NONE);
    assert_eq!(h.state().values[1], 1000.0);
    h.get_by_role_and_label(Role::Slider, "Band 1 Frequenz")
        .accesskit_node()
        .is_disabled()
        .then_some(())
        .expect("disabled in the AccessKit tree");
}

fn envelope_harness() -> Harness<'static, World> {
    harness(
        configs::envelope(Lang::De),
        || {
            vec![
                SlotInfo::new(0.0, 1.0, 0.0),
                SlotInfo::new(0.0, 1.0, 0.0),
                SlotInfo::new(0.0, 1.0, 0.5),
                SlotInfo::new(0.0, 1.0, 0.0),
                SlotInfo::new(0.0, 1.0, 0.5),
            ]
        },
        Look::Normal,
        vec![0.3, 0.3, 0.5, 0.3, 0.5],
    )
}

#[test]
fn envelope_points_do_not_overtake_each_other() {
    let mut h = envelope_harness();
    // The decay point dragged far left stops at the attack point.
    let c = center(&h, Role::Slider, "Decay Zeit");
    drag(&mut h, c, vec2(-150.0, 0.0), 15, Modifiers::NONE);
    assert_eq!(h.state().values[1], 0.0);
    assert!((h.state().values[0] - 0.3).abs() < 1e-9, "the attack stays");
    // The sustain point only moves ↕.
    let before = h.state().values.clone();
    let s = center(&h, Role::Slider, "Sustain Pegel");
    drag(&mut h, s, vec2(40.0, -10.0), 10, Modifiers::NONE);
    assert!(h.state().values[2] > before[2]);
    assert_eq!(h.state().values[0], before[0]);
    assert_eq!(h.state().values[1], before[1]);
}

#[test]
fn xy_shift_locks_the_axis_that_moved_less() {
    use komponist_ui::widgets::device::graph::{Axis, AxisKind};
    let ax = || Axis::new(AxisKind::Lin, 0.0, 1.0);
    let mut h = harness(
        configs::xy(Lang::De, ax(), ax(), "Zeit", "Feedback"),
        || vec![SlotInfo::new(0.0, 1.0, 0.5), SlotInfo::new(0.0, 1.0, 0.5)],
        Look::Normal,
        vec![0.5, 0.5],
    );
    let c = center(&h, Role::Slider, "XY-Feld Zeit");
    drag(&mut h, c, vec2(40.0, -10.0), 10, Modifiers::SHIFT);
    let w = h.state();
    assert!(w.values[0] > 0.5, "x moved the most");
    assert_eq!(w.values[1], 0.5, "y is locked");
    h.get_by_role_and_label(Role::Slider, "XY-Feld Feedback");
}
