//! WP-21 value controls and meters, rendered and driven with `egui_kittest`.
//!
//! - Snapshots per state at 1× and 2× (`tests/snapshots/number-field-*`, `knob-*`, `fader-*`,
//!   `meter-*`) and of the three gallery cards.
//! - Interaction: drag math (1 px = 1 step, Shift fine, Ctrl coarse, knob 200 px, fader absolute: the
//!   thumb follows the pointer without a jump), Alt-click default, double-click text entry (Enter applies,
//!   Esc discards), arrows, wheel, AccessKit actions, the value tooltip, the meter's clip reset.

use egui::{
    Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Ui,
    Vec2, pos2, vec2,
};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_ui::State;
use komponist_ui::snapshot;
use komponist_ui::widgets::fader::{self, DbValue, Fader, FaderSize};
use komponist_ui::widgets::knob::{self, Knob};
use komponist_ui::widgets::meter::{self, Meter, MeterState, Style};
use komponist_ui::widgets::number_field::{self, NumberField, WHEEL_GESTURE_IDLE};

/// One frame per event, short enough that two clicks make a double-click.
const STEP: f32 = 1.0 / 60.0;

// ---------------------------------------------------------------------------------------------
// Snapshots

#[test]
fn number_field_states() {
    let mut shots = snapshot::Snapshots::new();
    for (_, st) in State::CONTROL_STATES {
        shots.add_state("number-field", st, None, |ui| {
            let mut bpm = 131.5;
            ui.add(tempo(&mut bpm));
        });
    }
    shots.add("number-field-editing", None, |ui| {
        number_field::still(ui, "Tempo", State::REST, "124.00", "BPM", true, 0.0);
    });
    shots.add("number-field-db", None, |ui| {
        let mut db = -6.0;
        ui.add(number_field::db_field(&mut db, "Lautstärke"));
    });
    shots.finish();
}

#[test]
fn knob_states() {
    let mut shots = snapshot::Snapshots::new();
    for (_, st) in State::CONTROL_STATES {
        shots.add_state("knob", st, None, |ui| {
            let mut v = 0.35;
            ui.add(Knob::new(&mut v, "Cutoff"));
        });
    }
    shots.add("knob-bipolar", None, |ui| {
        ui.horizontal(|ui| {
            for pan in [-0.28_f32, 0.0, 0.6] {
                let mut p = pan;
                ui.add(Knob::pan(&mut p, format!("Pan {pan}")));
            }
        });
    });
    shots.add("knob-small", None, |ui| {
        let mut v = 0.7;
        ui.add(Knob::new(&mut v, "Res").small());
    });
    shots.finish();
}

#[test]
fn fader_states() {
    let mut shots = snapshot::Snapshots::new();
    for (_, st) in State::CONTROL_STATES {
        shots.add_state("fader", st, None, |ui| {
            let mut db = -3.2;
            ui.add(Fader::new(&mut db, "Lautstärke").scale(true));
        });
    }
    for (_, st) in [State::CONTROL_STATES[0], State::CONTROL_STATES[2]] {
        shots.add_state("fader-compact", st, None, |ui| {
            let mut db = 0.0;
            ui.add(
                Fader::new(&mut db, "Lautstärke")
                    .compact()
                    .track_length(80.0),
            );
        });
    }
    shots.add("fader-value", None, |ui| {
        ui.horizontal(|ui| {
            for v in [0.0_f32, -3.2, 4.5, -70.0] {
                let mut db = v;
                ui.add(DbValue::new(&mut db, format!("Wert {v}")));
            }
        });
    });
    shots.finish();
}

#[test]
fn meter_states() {
    let mut shots = snapshot::Snapshots::new();
    for (label, levels, held) in meter::SAMPLES {
        shots.add(&format!("meter-{label}"), None, |ui| {
            meter::still(ui, label, &MeterState::frozen(levels, held));
        });
    }
    shots.add("meter-header-clip", None, |ui| {
        let state = MeterState::frozen([0.8, -2.0], [0.8, -1.5]);
        let (rect, _) = ui.allocate_exact_size(Style::HEADER.size(40.0), egui::Sense::hover());
        meter::paint(ui.painter(), rect, &state, &Style::HEADER);
    });
    shots.add("meter-horizontal", None, |ui| {
        let state = MeterState::frozen([-3.0, -9.0], [-2.1, -8.0]);
        let style = Style {
            orientation: meter::Orientation::Horizontal,
            ..Style::MIXER
        };
        let (rect, _) = ui.allocate_exact_size(style.size(124.0), egui::Sense::hover());
        meter::paint(ui.painter(), rect, &state, &style);
    });
    shots.finish();
}

#[test]
fn gallery_cards() {
    let mut shots = snapshot::Snapshots::new();
    shots.add(
        "number-field-gallery",
        Some(vec2(720.0, 150.0)),
        number_field::gallery,
    );
    shots.add("knob-gallery", Some(vec2(720.0, 220.0)), knob::gallery);
    shots.add(
        "text-field-gallery",
        Some(vec2(860.0, 90.0)),
        komponist_ui::widgets::text_field::gallery,
    );
    shots.add("fader-gallery", Some(vec2(720.0, 640.0)), fader::gallery);
    shots.finish();
}

// ---------------------------------------------------------------------------------------------
// Interaction helpers

fn tempo(bpm: &mut f64) -> NumberField<'_> {
    NumberField::new(bpm, "Tempo")
        .unit("BPM")
        .range(20.0..=300.0)
        .step(1.0)
        .decimals(2)
        .default_value(120.0)
}

/// A harness at 1× around `app`, one frame per event.
fn harness<S>(state: S, app: impl FnMut(&mut Ui, &mut S) + 'static) -> Harness<'static, S> {
    let mut app = app;
    let mut h = snapshot::builder::<S>(1.0)
        .with_size(vec2(360.0, 320.0))
        .with_step_dt(STEP)
        .build_ui_state(move |ui, s| snapshot::padded(ui, |ui| app(ui, s)), state);
    h.run();
    h
}

/// Lets half a second pass, so the next click does not make a double-click with the last one.
fn pause<S>(h: &mut Harness<'_, S>) {
    for _ in 0..30 {
        h.step();
    }
}

/// Presses at `from`, moves by `by` in `steps` events with `modifiers` held, releases.
fn drag<S>(h: &mut Harness<'_, S>, from: Pos2, by: Vec2, steps: usize, modifiers: Modifiers) {
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

/// Presses at `at` and moves by `by` in `steps` events, without releasing.
fn hold_drag<S>(h: &mut Harness<'_, S>, at: Pos2, by: Vec2, steps: usize) {
    pause(h);
    h.event(Event::PointerMoved(at));
    h.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    for i in 1..=steps {
        h.event(Event::PointerMoved(at + by * (i as f32 / steps as f32)));
    }
    h.run();
}

fn release<S>(h: &mut Harness<'_, S>, at: Pos2) {
    h.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
}

fn click<S>(h: &mut Harness<'_, S>, at: Pos2, modifiers: Modifiers) {
    pause(h);
    h.event(Event::PointerMoved(at));
    h.event(Event::ModifiersChanged(modifiers));
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        });
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

fn double_click<S>(h: &mut Harness<'_, S>, at: Pos2) {
    pause(h);
    h.event(Event::PointerMoved(at));
    for pressed in [true, false, true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run();
}

fn type_and<S>(h: &mut Harness<'_, S>, text: &str, key: Key) {
    h.event(Event::Text(text.to_owned()));
    h.key_press(key);
    h.run();
}

fn wheel<S>(h: &mut Harness<'_, S>, at: Pos2, notches: f32, modifiers: Modifiers) {
    h.event(Event::PointerMoved(at));
    h.event(Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, notches),
        phase: TouchPhase::Move,
        source: MouseWheelSource::Wheel,
        modifiers,
    });
    // egui keeps smoothing the wheel for a few frames.
    h.run_steps(20);
}

fn center<S>(h: &Harness<'_, S>, label: &str) -> Pos2 {
    h.get_by_label(label).rect().center()
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

// ---------------------------------------------------------------------------------------------
// Number field

#[derive(Default)]
struct Num {
    bpm: f64,
    changes: u32,
}

fn number_harness(bpm: f64) -> Harness<'static, Num> {
    harness(Num { bpm, changes: 0 }, |ui, s| {
        if ui.add(tempo(&mut s.bpm)).changed() {
            s.changes += 1;
        }
    })
}

#[test]
fn number_field_drag_is_one_px_per_step() {
    let mut h = number_harness(124.0);
    let c = center(&h, "Tempo");
    drag(&mut h, c, vec2(0.0, -10.0), 10, Modifiers::NONE);
    assert!(near(h.state().bpm, 134.0), "{}", h.state().bpm);
    assert!(h.state().changes > 0);
    // Down, past the end of the field: the value keeps following.
    drag(&mut h, c, vec2(0.0, 40.0), 20, Modifiers::NONE);
    assert!(near(h.state().bpm, 94.0), "{}", h.state().bpm);
    // Shift: 1 px = 0.1 step; Ctrl: 1 px = 10 steps.
    drag(&mut h, c, vec2(0.0, -15.0), 15, Modifiers::SHIFT);
    assert!(near(h.state().bpm, 95.5), "{}", h.state().bpm);
    drag(&mut h, c, vec2(0.0, -8.0), 8, Modifiers::CTRL);
    assert!(near(h.state().bpm, 175.5), "{}", h.state().bpm);
    // Clamped at the range.
    drag(&mut h, c, vec2(0.0, -40.0), 10, Modifiers::CTRL);
    assert!(near(h.state().bpm, 300.0));
}

#[test]
fn number_field_alt_click_sets_the_default() {
    let mut h = number_harness(131.5);
    let c = center(&h, "Tempo");
    click(&mut h, c, Modifiers::NONE);
    assert!(near(h.state().bpm, 131.5), "a plain click changes nothing");
    click(&mut h, c, Modifiers::ALT);
    assert!(near(h.state().bpm, 120.0));
}

#[test]
fn number_field_text_entry() {
    let mut h = number_harness(124.0);
    let c = center(&h, "Tempo");
    double_click(&mut h, c);
    // The entry is open, focused, with the value preselected.
    let entry = h.get_by_label("Tempo Eingabe");
    assert!(entry.is_focused());
    assert_eq!(entry.value().as_deref(), Some("124.00"));
    h.snapshot(snapshot::file_stem("number-field-entry-live", 1.0));
    // Typing replaces the selection; Enter applies (comma as decimal point, unit allowed).
    type_and(&mut h, "131,5 BPM", Key::Enter);
    assert!(near(h.state().bpm, 131.5), "{}", h.state().bpm);
    assert!(h.query_by_label("Tempo Eingabe").is_none(), "entry closed");
    assert!(
        h.get_by_label("Tempo").is_focused(),
        "focus back on the field"
    );

    // Esc discards.
    double_click(&mut h, c);
    type_and(&mut h, "200", Key::Escape);
    assert!(near(h.state().bpm, 131.5));
    assert!(h.query_by_label("Tempo Eingabe").is_none());

    // Unparsable text keeps the value; out of range clamps.
    double_click(&mut h, c);
    type_and(&mut h, "schnell", Key::Enter);
    assert!(near(h.state().bpm, 131.5));
    double_click(&mut h, c);
    type_and(&mut h, "1000", Key::Enter);
    assert!(near(h.state().bpm, 300.0));
}

#[test]
fn number_field_keys_wheel_and_accesskit() {
    let mut h = number_harness(124.0);
    let c = center(&h, "Tempo");
    wheel(&mut h, c, 1.0, Modifiers::NONE);
    assert!(near(h.state().bpm, 125.0), "{}", h.state().bpm);
    wheel(&mut h, c, -1.0, Modifiers::SHIFT);
    assert!(near(h.state().bpm, 124.9), "{}", h.state().bpm);

    h.get_by_label("Tempo").focus();
    h.run();
    assert!(h.get_by_label("Tempo").is_focused());
    h.key_press(Key::ArrowUp);
    h.run();
    h.key_press(Key::ArrowUp);
    h.run();
    assert!(near(h.state().bpm, 126.9), "{}", h.state().bpm);
    assert!(
        h.get_by_label("Tempo").is_focused(),
        "arrows keep the focus"
    );
    h.key_press_modifiers(Modifiers::CTRL, Key::ArrowDown);
    h.run();
    assert!(near(h.state().bpm, 116.9), "{}", h.state().bpm);

    // Enter opens the text entry for keyboard users.
    h.key_press(Key::Enter);
    h.run();
    assert!(h.get_by_label("Tempo Eingabe").is_focused());
    type_and(&mut h, "100", Key::Enter);
    assert!(near(h.state().bpm, 100.0));

    let node = h.get_by_label("Tempo");
    assert_eq!(node.value().as_deref(), Some("100.00 BPM"));
    node.increment_accesskit();
    h.run();
    assert!(near(h.state().bpm, 101.0));
    h.get_by_label("Tempo").set_value_accesskit(140.0);
    h.run();
    assert!(near(h.state().bpm, 140.0));
}

// ---------------------------------------------------------------------------------------------
// Knob

#[derive(Default)]
struct Knobs {
    cutoff: f64,
    pan: f32,
}

fn knob_harness(cutoff: f64, pan: f32) -> Harness<'static, Knobs> {
    harness(Knobs { cutoff, pan }, |ui, s| {
        ui.add_space(30.0);
        ui.horizontal(|ui| {
            ui.add(
                Knob::new(&mut s.cutoff, "Cutoff")
                    .default_value(0.5)
                    .format(|v| format!("{:.0} %", v * 100.0)),
            );
            ui.add(Knob::pan(&mut s.pan, "Pan"));
        });
    })
}

#[test]
fn knob_drag_is_200_px_for_the_range() {
    let mut h = knob_harness(0.0, 0.0);
    let c = center(&h, "Cutoff");
    drag(&mut h, c, vec2(0.0, -100.0), 20, Modifiers::NONE);
    assert!(
        (h.state().cutoff - 0.5).abs() < 1e-6,
        "{}",
        h.state().cutoff
    );
    drag(&mut h, c, vec2(0.0, -100.0), 20, Modifiers::SHIFT);
    assert!(
        (h.state().cutoff - 0.55).abs() < 1e-6,
        "{}",
        h.state().cutoff
    );
    drag(&mut h, c, vec2(0.0, 8.0), 8, Modifiers::CTRL);
    assert!(
        (h.state().cutoff - 0.15).abs() < 1e-6,
        "{}",
        h.state().cutoff
    );
    // Pan: −1 … 1 over 200 px; dragging down goes left.
    let p = center(&h, "Pan");
    drag(&mut h, p, vec2(0.0, 24.0), 12, Modifiers::NONE);
    assert!((h.state().pan + 0.24).abs() < 1e-5, "{}", h.state().pan);
    assert_eq!(h.get_by_label("Pan").value().as_deref(), Some("L 12"));
}

#[test]
fn a_bypassed_knob_takes_no_input_and_reads_as_disabled() {
    let mut h = harness(
        Knobs {
            cutoff: 0.25,
            pan: 0.0,
        },
        |ui, s| {
            ui.add(Knob::new(&mut s.cutoff, "Cutoff").bypassed(true));
        },
    );
    let node = h.get_by_label("Cutoff");
    assert_eq!(node.accesskit_node().role(), egui::accesskit::Role::Slider);
    assert!(node.accesskit_node().is_disabled());
    assert_eq!(node.accesskit_node().numeric_value(), Some(0.25));
    let c = center(&h, "Cutoff");
    drag(&mut h, c, vec2(0.0, -100.0), 20, Modifiers::NONE);
    drag(&mut h, c, vec2(0.0, 0.0), 1, Modifiers::ALT);
    assert_eq!(h.state().cutoff, 0.25);
}

#[test]
fn knob_shows_the_value_tooltip_while_dragging() {
    let mut h = knob_harness(0.25, 0.0);
    let c = center(&h, "Cutoff");
    hold_drag(&mut h, c, vec2(0.0, -20.0), 10);
    let tip = h.get_by_label("35 %");
    assert!(tip.rect().bottom() < c.y, "the tooltip is above the knob");
    h.snapshot(snapshot::file_stem("knob-drag-live", 1.0));
    release(&mut h, c + vec2(0.0, -20.0));
    assert!(h.query_by_label("35 %").is_none(), "gone after the drag");
}

/// The wheel shows the same value tooltip as a drag while it turns, and hides it one
/// `WHEEL_GESTURE_IDLE` after the last notch.
#[test]
fn knob_shows_the_value_tooltip_while_the_wheel_turns() {
    let mut h = knob_harness(0.5, 0.0);
    let c = center(&h, "Cutoff");
    assert!(h.query_by_label("51 %").is_none());
    wheel(&mut h, c, 1.0, Modifiers::NONE);
    let tip = h.get_by_label("51 %");
    assert!(tip.rect().bottom() < c.y, "the tooltip is above the knob");
    h.snapshot(snapshot::file_stem("knob-wheel-live", 1.0));
    // A second notch within the idle time keeps it, with the new value.
    wheel(&mut h, c, 1.0, Modifiers::NONE);
    assert!(h.query_by_label("52 %").is_some());
    // The burst ends: gone after the idle time, the pointer still over the knob.
    for _ in 0..((WHEEL_GESTURE_IDLE / f64::from(STEP)) as usize + 5) {
        h.step();
    }
    assert!(h.query_by_label("52 %").is_none(), "gone after the burst");
    assert!(near(h.state().cutoff, 0.52));
}

#[test]
fn fader_shows_the_value_tooltip_while_the_wheel_turns() {
    let mut h = fader_harness(0.0);
    let thumb = thumb_center(&h, 0.0);
    let count = |h: &Harness<'_, Faders>| h.query_all_by_label("+1.0 dB").count();
    wheel(&mut h, thumb, 1.0, Modifiers::NONE);
    assert_eq!(count(&h), 1, "the tooltip");
    for _ in 0..((WHEEL_GESTURE_IDLE / f64::from(STEP)) as usize + 5) {
        h.step();
    }
    assert_eq!(count(&h), 0, "tooltip gone after the burst");
}

#[test]
fn knob_alt_click_and_text_entry() {
    let mut h = knob_harness(0.9, 0.6);
    let c = center(&h, "Cutoff");
    click(&mut h, c, Modifiers::ALT);
    assert!((h.state().cutoff - 0.5).abs() < 1e-9);
    let p = center(&h, "Pan");
    click(&mut h, p, Modifiers::ALT);
    assert_eq!(h.state().pan, 0.0, "Pan: Alt-click = centre");
    // Enter on the focused knob: text entry in the tooltip's place.
    h.get_by_label("Pan").focus();
    h.run();
    h.key_press(Key::Enter);
    h.run();
    let entry = h.get_by_label("Pan Eingabe");
    assert!(entry.rect().bottom() < p.y);
    assert_eq!(entry.value().as_deref(), Some("C"));
    type_and(&mut h, "L 12", Key::Enter);
    assert!((h.state().pan + 0.24).abs() < 1e-6);
}

/// Owner decision 2026-11-15: a double-click resets, and neither starts text entry nor a drag.
#[test]
fn knob_double_click_resets_and_does_not_open_text_entry() {
    let mut h = knob_harness(0.9, 0.6);
    let c = center(&h, "Cutoff");
    double_click(&mut h, c);
    assert!(
        (h.state().cutoff - 0.5).abs() < 1e-9,
        "{}",
        h.state().cutoff
    );
    assert!(h.query_by_label("Cutoff Eingabe").is_none());
    let p = center(&h, "Pan");
    double_click(&mut h, p);
    assert_eq!(h.state().pan, 0.0, "Pan: double-click = centre");
    assert!(h.query_by_label("Pan Eingabe").is_none());
}

#[test]
fn knob_wheel_steps_one_percent_shift_fine_ctrl_coarse_and_only_when_hovered() {
    let mut h = knob_harness(0.5, 0.0);
    let c = center(&h, "Cutoff");
    wheel(&mut h, c, 1.0, Modifiers::NONE);
    assert!(near(h.state().cutoff, 0.51), "{}", h.state().cutoff);
    wheel(&mut h, c, -2.0, Modifiers::NONE);
    assert!(near(h.state().cutoff, 0.50), "{}", h.state().cutoff);
    wheel(&mut h, c, 1.0, Modifiers::SHIFT);
    assert!(near(h.state().cutoff, 0.501), "{}", h.state().cutoff);
    wheel(&mut h, c, -1.0, Modifiers::CTRL);
    assert!(near(h.state().cutoff, 0.401), "{}", h.state().cutoff);
    // Not hovered: nothing happens.
    wheel(&mut h, pos2(350.0, 310.0), 3.0, Modifiers::NONE);
    assert!(near(h.state().cutoff, 0.401), "{}", h.state().cutoff);
    // A touchpad: pixel deltas accumulate to whole lines.
    let line = h.ctx.options(|o| o.input_options.line_scroll_speed);
    h.event(Event::PointerMoved(c));
    for _ in 0..2 {
        h.event(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, line / 2.0),
            phase: TouchPhase::Move,
            source: MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(20);
    }
    assert!(near(h.state().cutoff, 0.411), "{}", h.state().cutoff);
}

// ---------------------------------------------------------------------------------------------
// Fader

#[derive(Default)]
struct Faders {
    db: f32,
    changes: u32,
}

fn fader_harness(db: f32) -> Harness<'static, Faders> {
    harness(Faders { db, changes: 0 }, |ui, s| {
        ui.add_space(20.0);
        if ui
            .add(Fader::new(&mut s.db, "Lautstärke").scale(true))
            .changed()
        {
            s.changes += 1;
        }
        ui.add(DbValue::new(&mut s.db, "Lautstärke, Wert"));
    })
}

/// The centre of the thumb of the fader at `db`.
fn thumb_center(h: &Harness<'_, Faders>, db: f32) -> Pos2 {
    let rect = h.get_by_label("Lautstärke").rect();
    let layout = fader::Layout::new(rect, FaderSize::Normal, true);
    layout.thumb_rect(db).center()
}

#[test]
fn fader_drag_is_relative_with_the_taper() {
    let mut h = fader_harness(0.0);
    // 0 dB sits at 14 % of the 200 px track; 32 px down is 30 %: −6 dB.
    let t = thumb_center(&h, 0.0);
    drag(&mut h, t, vec2(0.0, 32.0), 16, Modifiers::NONE);
    assert_eq!(h.state().db, -6.0);
    // Shift: 30 px are 3 px of position.
    let t = thumb_center(&h, -6.0);
    drag(&mut h, t, vec2(0.0, -30.0), 15, Modifiers::SHIFT);
    assert_eq!(h.state().db, -5.4);
    // To the bottom: −∞, stored as −70.
    drag(&mut h, t, vec2(0.0, 400.0), 20, Modifiers::NONE);
    assert_eq!(h.state().db, -70.0);
    assert_eq!(
        h.get_by_label("Lautstärke").value().as_deref(),
        Some("−∞ dB")
    );
}

/// A fader harness with a track of `length` points at `ppp`.
fn sized_fader(db: f32, length: f32, ppp: f32) -> Harness<'static, Faders> {
    let mut h = snapshot::builder::<Faders>(ppp)
        .with_size(vec2(360.0, 420.0))
        .with_step_dt(STEP)
        .build_ui_state(
            move |ui, s| {
                snapshot::padded(ui, |ui| {
                    ui.add(
                        Fader::new(&mut s.db, "Lautstärke")
                            .track_length(length)
                            .scale(true),
                    );
                })
            },
            Faders { db, changes: 0 },
        );
    h.run();
    h
}

/// The y of the thumb centre of a fader at `db` on its track.
fn thumb_y(h: &Harness<'_, Faders>) -> f32 {
    let rect = h.get_by_label("Lautstärke").rect();
    let layout = fader::Layout::new(rect, FaderSize::Normal, true);
    layout.thumb_rect(h.state().db).center().y
}

/// Owner decision 2026-11-15: the thumb sticks to the pointer; N points of pointer movement are
/// N points of thumb movement, at any track length and scale factor, and the grab offset stays.
#[test]
fn fader_thumb_follows_the_pointer_one_to_one() {
    for ppp in [1.0, 2.0] {
        for length in [80.0, 200.0, 320.0] {
            let mut h = sized_fader(-20.0, length, ppp);
            let y0 = thumb_y(&h);
            // Grab 5 points below the thumb centre, then drag 30 points up.
            let grab = pos2(thumb_center(&h, -20.0).x, y0 + 5.0);
            drag(&mut h, grab, vec2(0.0, -30.0), 15, Modifiers::NONE);
            let moved = y0 - thumb_y(&h);
            // The dB value is rounded to 0.1 dB on the way, so allow a small step.
            assert!(
                (moved - 30.0).abs() < 1.5,
                "ppp {ppp}, length {length}: thumb moved {moved}"
            );
            // The grab offset stays: the thumb is still 5 points above the release position.
            assert!(((grab.y - 30.0) - (thumb_y(&h) + 5.0)).abs() < 1.5);
        }
    }
}

#[test]
fn fader_clamps_at_the_ends_and_answers_when_the_pointer_is_back() {
    let mut h = sized_fader(-20.0, 200.0, 1.0);
    let t = thumb_center(&h, -20.0);
    // Far above the top: +6 dB.
    drag(&mut h, t, vec2(0.0, -600.0), 20, Modifiers::NONE);
    assert_eq!(h.state().db, 6.0);
    // Down again, but only 20 points past the clamped end: value stays high (absolute semantics:
    // the pointer began 600 points away).
    pause(&mut h);
    let top = thumb_center(&h, 6.0);
    hold_drag(&mut h, top, vec2(0.0, -300.0), 10);
    assert_eq!(h.state().db, 6.0);
    h.event(Event::PointerMoved(top + vec2(0.0, -280.0)));
    h.run();
    assert_eq!(h.state().db, 6.0, "still beyond the track end");
    h.event(Event::PointerMoved(top + vec2(0.0, -300.0 + 340.0)));
    h.run();
    assert!(h.state().db < 6.0, "back over the track the thumb answers");
    release(&mut h, top);
}

#[test]
fn fader_shift_drag_is_fine_and_releasing_shift_does_not_jump() {
    let mut h = sized_fader(-20.0, 200.0, 1.0);
    let t = thumb_center(&h, -20.0);
    let y0 = thumb_y(&h);
    pause(&mut h);
    h.event(Event::PointerMoved(t));
    h.event(Event::PointerButton {
        pos: t,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.event(Event::PointerMoved(t + vec2(0.0, -10.0)));
    h.event(Event::ModifiersChanged(Modifiers::SHIFT));
    for i in 1..=10 {
        h.event(Event::PointerMoved(t + vec2(0.0, -10.0 - 3.0 * i as f32)));
    }
    h.run();
    // 10 points normal + 30 points at × 0.1 = 13 points.
    assert!(
        (y0 - thumb_y(&h) - 13.0).abs() < 1.5,
        "{}",
        y0 - thumb_y(&h)
    );
    // Shift released: no jump back under the pointer (which is 27 points further up).
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.event(Event::PointerMoved(t + vec2(0.0, -40.0)));
    h.run();
    assert!(
        (y0 - thumb_y(&h) - 13.0).abs() < 1.5,
        "{}",
        y0 - thumb_y(&h)
    );
    h.event(Event::PointerMoved(t + vec2(0.0, -50.0)));
    h.run();
    assert!((y0 - thumb_y(&h) - 23.0).abs() < 1.5);
    release(&mut h, t + vec2(0.0, -50.0));
}

#[test]
fn fader_does_not_jump_on_a_track_click() {
    let mut h = fader_harness(-6.0);
    let rect = h.get_by_label("Lautstärke").rect();
    let layout = fader::Layout::new(rect, FaderSize::Normal, true);
    let low = pos2(layout.track.center().x, layout.y(0.95));
    click(&mut h, low, Modifiers::NONE);
    assert_eq!(
        h.state().db,
        -6.0,
        "a click on the track does not move the fader"
    );
    assert_eq!(h.state().changes, 0);
    // A drag that starts on the track moves relative from the thumb: 30 px up from −6 dB (30 %)
    // is 15 %.
    drag(&mut h, low, vec2(0.0, -30.0), 15, Modifiers::NONE);
    assert_eq!(h.state().db, -0.4);
}

#[test]
fn fader_value_line_and_tooltip_while_dragging() {
    let mut h = fader_harness(0.0);
    let t = thumb_center(&h, 0.0);
    hold_drag(&mut h, t, vec2(0.0, 16.0), 8);
    let tip = h.get_by_label("−3.0 dB");
    assert!(tip.rect().bottom() < t.y + 16.0);
    h.snapshot(snapshot::file_stem("fader-drag-live", 1.0));
    release(&mut h, t + vec2(0.0, 16.0));
    assert_eq!(h.state().db, -3.0);
}

#[test]
fn fader_alt_click_keys_and_text_entry() {
    let mut h = fader_harness(-12.0);
    let t = thumb_center(&h, -12.0);
    click(&mut h, t, Modifiers::ALT);
    assert_eq!(h.state().db, 0.0, "Alt-click: 0.0 dB");

    // Enter on the focused fader: dB entry above the thumb.
    h.get_by_label("Lautstärke").focus();
    h.run();
    h.key_press(Key::Enter);
    h.run();
    let entry = h.get_by_label("Lautstärke Eingabe");
    assert_eq!(entry.value().as_deref(), Some("0.0"));
    type_and(&mut h, "−12,5", Key::Enter);
    assert_eq!(h.state().db, -12.5);

    // Double-click on the value under the fader.
    let v = center(&h, "Lautstärke, Wert");
    double_click(&mut h, v);
    assert!(h.get_by_label("Lautstärke, Wert Eingabe").is_focused());
    type_and(&mut h, "-inf", Key::Enter);
    assert_eq!(h.state().db, -70.0);
    double_click(&mut h, v);
    type_and(&mut h, "+3", Key::Escape);
    assert_eq!(h.state().db, -70.0, "Esc discards");

    // Arrows on the focused fader: 1 dB, Shift 0.1 dB.
    h.get_by_label("Lautstärke").focus();
    h.run();
    h.key_press(Key::ArrowUp);
    h.run();
    assert_eq!(h.state().db, -69.0);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowUp);
    h.run();
    assert_eq!(h.state().db, -68.9);
}

#[test]
fn fader_double_click_resets_and_does_not_open_text_entry() {
    let mut h = fader_harness(-12.0);
    let t = thumb_center(&h, -12.0);
    double_click(&mut h, t);
    assert_eq!(h.state().db, 0.0);
    assert_eq!(h.state().changes, 1, "one change");
    assert!(h.query_by_label("Lautstärke Eingabe").is_none());
}

#[test]
fn fader_wheel_steps_one_db_shift_fine_ctrl_coarse() {
    let mut h = fader_harness(-12.0);
    let t = thumb_center(&h, -12.0);
    wheel(&mut h, t, 1.0, Modifiers::NONE);
    assert_eq!(h.state().db, -11.0);
    wheel(&mut h, t, -1.0, Modifiers::SHIFT);
    assert!((h.state().db + 11.1).abs() < 1e-4, "{}", h.state().db);
    let t = thumb_center(&h, h.state().db);
    wheel(&mut h, t, -1.0, Modifiers::CTRL);
    assert!((h.state().db + 21.1).abs() < 1e-3, "{}", h.state().db);
    // Over the value readout (not a fader) the wheel does nothing here.
    let v = center(&h, "Lautstärke, Wert");
    let before = h.state().db;
    wheel(&mut h, v, 1.0, Modifiers::NONE);
    assert_eq!(h.state().db, before);
}

// ---------------------------------------------------------------------------------------------
// Meter

struct Meters {
    state: MeterState,
    peaks: [f32; 2],
    clicks: u32,
}

#[test]
fn meter_latches_clip_until_clicked() {
    let mut h = harness(
        Meters {
            state: MeterState::new(),
            peaks: [1.2, 0.5],
            clicks: 0,
        },
        |ui, s| {
            if ui
                .add(Meter::new(&mut s.state, s.peaks, "Pegel Master"))
                .clicked()
            {
                s.clicks += 1;
            }
        },
    );
    assert!(h.state().state.clipped());
    h.state_mut().peaks = [0.0, 0.0];
    h.run();
    let node = h.get_by_label("Pegel Master");
    assert_eq!(node.value().as_deref(), Some("+1.6 dB, Clip"));
    assert!(h.state().state.clipped(), "the latch holds without signal");
    h.snapshot(snapshot::file_stem("meter-live-clip", 1.0));
    let c = center(&h, "Pegel Master");
    click(&mut h, c, Modifiers::NONE);
    assert_eq!(h.state().clicks, 1);
    assert!(!h.state().state.clipped(), "a click resets the latch");
    assert_eq!(
        h.get_by_label("Pegel Master").value().as_deref(),
        Some("−∞ dB")
    );
}

#[test]
fn meter_falls_after_the_hold() {
    // Each frame is 1/60 s in this harness; the widget measures time itself.
    let mut h = harness(
        Meters {
            state: MeterState::new(),
            peaks: [0.5, 0.5],
            clicks: 0,
        },
        |ui, s| {
            ui.add(Meter::new(&mut s.state, s.peaks, "Pegel Lead"));
        },
    );
    h.state_mut().peaks = [0.0, 0.0];
    let held = h.state().state.held_db(0);
    // 1 s of frames: the level fell, the held peak still holds.
    for _ in 0..60 {
        h.step();
    }
    let s = &h.state().state;
    assert!(s.level_db(0) < held - 15.0, "{}", s.level_db(0));
    assert_eq!(s.held_db(0), held);
    // 1.5 s more: the held peak falls at 20 dB/s.
    for _ in 0..90 {
        h.step();
    }
    assert!(h.state().state.held_db(0) < held - 15.0);
}

#[test]
fn a_text_field_is_a_named_text_input_that_edits() {
    use komponist_ui::widgets::text_field::TextField;
    let mut h = harness(String::new(), |ui, s| {
        ui.add(TextField::new(s, "Filter").hint("Filter").width(160.0));
    });
    let node = h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Filter");
    node.focus();
    h.run();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Filter")
        .type_text("Abc");
    h.run();
    assert_eq!(h.state(), "Abc");
}
