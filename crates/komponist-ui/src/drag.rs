//! Drag math and pointer handling for value controls (binding, HANDOFF §5 "Drag-Mathe").
//!
//! - Number field: 1 px = 1 step ([`ValueDrag::number`]).
//! - Knob: 200 px = the full range ([`ValueDrag::knob`]).
//! - Fader: relative from the thumb over the track length ([`ValueDrag::span`] on the fader's
//!   position; the dB taper is the fader's business).
//! - Shift = × 0.1 (fine), Ctrl = × 10 (coarse) ([`speed`]); both together cancel out.
//! - Number fields: Alt-click = default value, double-click = text entry ([`ClickAction::of`]).
//! - Knobs and faders (owner decision 2026-11-15): double-click and Alt-click = default value
//!   ([`ClickAction::of_control`]); text entry is Enter on the focused control or the right-click
//!   menu.
//! - ↑ ↓ and the mouse wheel: one step, with Shift fine ([`step_delta`]).
//!
//! Dragging up increases the value. While dragging, the pointer is hidden and confined to the
//! window ([`grab_pointer`] / [`release_pointer`]) and raw mouse motion drives the value
//! ([`pointer_delta`]), so the value keeps following when the cursor would hit the screen edge.
//!
//! The numbers in this module are interaction rules of the design system (`ix.number`,
//! `ix.knob`, HANDOFF §5), not visual tokens.

use egui::{Context, CursorGrab, Event, Modifiers, Response, Vec2, ViewportCommand};

/// Number field: pixels per step.
pub const NUMBER_PX_PER_STEP: f64 = 1.0;
/// Knob: pixels for the full range.
pub const KNOB_FULL_RANGE_PX: f64 = 200.0;
/// Shift: fine factor.
pub const FINE: f64 = 0.1;
/// Ctrl: coarse factor.
pub const COARSE: f64 = 10.0;

/// The speed factor of the held modifiers: Shift × 0.1, Ctrl × 10, both × 1.
pub fn speed(modifiers: Modifiers) -> f64 {
    let mut f = 1.0;
    if modifiers.shift {
        f *= FINE;
    }
    if modifiers.ctrl || modifiers.command {
        f *= COARSE;
    }
    f
}

/// One drag gesture on a value: accumulates pointer movement into a value.
///
/// The accumulated value is unquantised and clamped, so turning back after hitting a limit
/// responds at once, and fine movements add up (10 px with Shift = 1 step). The result is
/// quantised to whole steps *relative to the start value*, truncating towards it, so a gesture
/// never jumps by a step before the pointer has moved a full step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueDrag {
    start: f64,
    raw: f64,
    min: f64,
    max: f64,
    per_px: f64,
    step: Option<f64>,
}

impl ValueDrag {
    /// A drag where `px_for_range` pixels move the value from `min` to `max`, unquantised.
    pub fn span(value: f64, min: f64, max: f64, px_for_range: f64) -> Self {
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        let start = value.clamp(min, max);
        Self {
            start,
            raw: start,
            min,
            max,
            per_px: (max - min) / px_for_range,
            step: None,
        }
    }

    /// Number field: 1 px = 1 `step`, quantised to whole steps.
    pub fn number(value: f64, min: f64, max: f64, step: f64) -> Self {
        Self {
            per_px: step / NUMBER_PX_PER_STEP,
            ..Self::span(value, min, max, 1.0)
        }
        .with_step(step)
    }

    /// Knob: 200 px = the full range `min..=max`.
    pub fn knob(value: f64, min: f64, max: f64) -> Self {
        Self::span(value, min, max, KNOB_FULL_RANGE_PX)
    }

    /// Quantises the result to whole `step`s from the start value.
    #[must_use]
    pub fn with_step(mut self, step: f64) -> Self {
        self.step = (step > 0.0).then_some(step);
        self
    }

    /// Value change per pixel at normal speed.
    pub fn per_px(&self) -> f64 {
        self.per_px
    }

    /// Adds `up_px` pixels of upward movement (negative = down) at the speed of `modifiers`;
    /// returns the new value.
    pub fn drag_px(&mut self, up_px: f64, modifiers: Modifiers) -> f64 {
        self.raw = (self.raw + up_px * self.per_px * speed(modifiers)).clamp(self.min, self.max);
        self.value()
    }

    /// Adds a pointer movement in points (screen coordinates, y down); only the vertical part
    /// counts. Returns the new value.
    pub fn drag(&mut self, delta: Vec2, modifiers: Modifiers) -> f64 {
        self.drag_px(-f64::from(delta.y), modifiers)
    }

    /// The current value: the accumulated value, quantised and clamped.
    pub fn value(&self) -> f64 {
        match self.step {
            None => self.raw,
            Some(step) => {
                let steps = (self.raw - self.start) / step;
                // Guard against 0.1 * 10 = 0.9999… losing a whole step.
                let steps = (steps + steps.signum() * 1e-9).trunc();
                (self.start + steps * step).clamp(self.min, self.max)
            }
        }
    }

    /// The value the gesture started from.
    pub fn start(&self) -> f64 {
        self.start
    }
}

/// Value change for one keyboard or wheel step (`steps` = +1 up, −1 down) of size `step`;
/// Shift makes it fine, Ctrl coarse.
pub fn step_delta(steps: f64, step: f64, modifiers: Modifiers) -> f64 {
    steps * step * speed(modifiers)
}

/// What a click on a value control does besides starting a drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickAction {
    /// Nothing special.
    None,
    /// Alt-click (and, on knobs and faders, double-click): set the default value.
    Reset,
    /// Double-click: open text entry with the value preselected.
    TextEntry,
}

impl ClickAction {
    /// The action for a click (`clicked`, `double_clicked`) with `modifiers` held.
    pub fn from_click(clicked: bool, double_clicked: bool, modifiers: Modifiers) -> Self {
        if double_clicked {
            Self::TextEntry
        } else if clicked && modifiers.alt {
            Self::Reset
        } else {
            Self::None
        }
    }

    /// The action for `response` in this pass, for number fields and value readouts: a
    /// double-click opens text entry.
    pub fn of(response: &Response) -> Self {
        let modifiers = response.ctx.input(|i| i.modifiers);
        Self::from_click(response.clicked(), response.double_clicked(), modifiers)
    }

    /// The action for a knob or fader: a double-click or an Alt-click sets the default value
    /// (never text entry; that is Enter or the context menu).
    pub fn of_control(response: &Response) -> Self {
        let modifiers = response.ctx.input(|i| i.modifiers);
        Self::from_control_click(response.clicked(), response.double_clicked(), modifiers)
    }

    /// [`ClickAction::of_control`] for the click state given.
    pub fn from_control_click(clicked: bool, double_clicked: bool, modifiers: Modifiers) -> Self {
        if double_clicked || (clicked && modifiers.alt) {
            Self::Reset
        } else {
            Self::None
        }
    }
}

/// Hides the pointer and confines it to the window for a drag gesture. X11 cannot lock the
/// pointer in place, so it is confined; [`pointer_delta`] keeps delivering movement at the edges.
pub fn grab_pointer(ctx: &Context) {
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::Confined));
    ctx.send_viewport_cmd(ViewportCommand::CursorVisible(false));
}

/// Undoes [`grab_pointer`].
pub fn release_pointer(ctx: &Context) {
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::None));
    ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
}

/// The pointer movement of this pass in points: the raw mouse motion if the backend reports it
/// (eframe does, from `DeviceEvent::MouseMotion`), else the pointer delta (e.g. in kittest).
pub fn pointer_delta(ctx: &Context) -> Vec2 {
    ctx.input(|i| raw_delta(&i.events, i.pixels_per_point).unwrap_or_else(|| i.pointer.delta()))
}

/// The sum of the raw mouse motion in `events`, converted from physical pixels to points; `None`
/// if there is none.
pub fn raw_delta(events: &[Event], pixels_per_point: f32) -> Option<Vec2> {
    let mut sum = None;
    for event in events {
        if let Event::MouseMoved(d) = event {
            *sum.get_or_insert(Vec2::ZERO) += *d;
        }
    }
    sum.map(|d| d / pixels_per_point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    const NONE: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers::SHIFT;
    const CTRL: Modifiers = Modifiers::CTRL;

    #[test]
    fn number_field_one_px_one_step() {
        let mut d = ValueDrag::number(64.0, 0.0, 127.0, 1.0);
        assert_eq!(d.drag_px(1.0, NONE), 65.0);
        assert_eq!(d.drag_px(10.0, NONE), 75.0);
        assert_eq!(d.drag_px(-25.0, NONE), 50.0);
        // Screen coordinates: moving the pointer up (negative y) increases.
        assert_eq!(d.drag(vec2(30.0, -3.0), NONE), 53.0);
        let mut d = ValueDrag::number(120.0, 20.0, 300.0, 0.5);
        assert_eq!(d.drag_px(7.0, NONE), 123.5);
    }

    #[test]
    fn shift_is_fine_ctrl_is_coarse() {
        let mut d = ValueDrag::number(0.0, -100.0, 100.0, 1.0);
        for _ in 0..9 {
            assert_eq!(d.drag_px(1.0, SHIFT), 0.0);
        }
        // Ten fine pixels make one step, despite 0.1 not being exact in binary.
        assert_eq!(d.drag_px(1.0, SHIFT), 1.0);
        assert_eq!(d.drag_px(1.0, CTRL), 11.0);
        assert_eq!(d.drag_px(-2.0, CTRL), -9.0);
        assert_eq!(d.drag_px(1.0, SHIFT | CTRL), -8.0);
        assert_eq!(speed(NONE), 1.0);
        assert_eq!(speed(SHIFT), 0.1);
        assert_eq!(speed(CTRL), 10.0);
    }

    #[test]
    fn knob_two_hundred_px_full_range() {
        let mut d = ValueDrag::knob(0.0, 0.0, 1.0);
        assert!((d.drag_px(100.0, NONE) - 0.5).abs() < 1e-12);
        assert!((d.drag_px(100.0, NONE) - 1.0).abs() < 1e-12);
        let mut pan = ValueDrag::knob(0.0, -1.0, 1.0);
        assert!((pan.drag_px(-100.0, NONE) + 1.0).abs() < 1e-12);
        let mut fine = ValueDrag::knob(0.0, 0.0, 1.0);
        assert!((fine.drag_px(200.0, SHIFT) - 0.1).abs() < 1e-12);
        let mut coarse = ValueDrag::knob(0.0, 0.0, 1.0);
        assert!((coarse.drag_px(20.0, CTRL) - 1.0).abs() < 1e-12);
        assert_eq!(ValueDrag::knob(0.0, 0.0, 200.0).per_px(), 1.0);
    }

    #[test]
    fn clamps_without_dead_zone() {
        let mut d = ValueDrag::number(125.0, 0.0, 127.0, 1.0);
        assert_eq!(d.drag_px(50.0, NONE), 127.0);
        // Turning back responds at once instead of first unwinding the 48 px overshoot.
        assert_eq!(d.drag_px(-1.0, NONE), 126.0);
        let mut k = ValueDrag::knob(0.9, 0.0, 1.0);
        k.drag_px(400.0, NONE);
        assert!((k.drag_px(-20.0, NONE) - 0.9).abs() < 1e-12);
        assert_eq!(ValueDrag::span(5.0, 0.0, 1.0, 100.0).start(), 1.0);
        assert_eq!(ValueDrag::span(0.5, 1.0, 0.0, 100.0).value(), 0.5);
    }

    #[test]
    fn fader_track_relative() {
        // A 120 px track: dragging 60 px from the thumb moves half the position range.
        let mut f = ValueDrag::span(0.14, 0.0, 1.0, 120.0);
        assert!((f.drag_px(-60.0, NONE) - 0.0).abs() < 1e-12);
        let mut f = ValueDrag::span(0.30, 0.0, 1.0, 120.0);
        assert!((f.drag_px(12.0, NONE) - 0.40).abs() < 1e-12);
    }

    #[test]
    fn clicks() {
        use ClickAction::*;
        assert_eq!(ClickAction::from_click(true, false, Modifiers::ALT), Reset);
        assert_eq!(ClickAction::from_click(true, false, NONE), None);
        assert_eq!(ClickAction::from_click(true, true, NONE), TextEntry);
        assert_eq!(ClickAction::from_click(false, false, Modifiers::ALT), None);
    }

    #[test]
    fn keyboard_steps() {
        assert_eq!(step_delta(1.0, 1.0, NONE), 1.0);
        assert_eq!(step_delta(-1.0, 0.5, NONE), -0.5);
        assert!((step_delta(1.0, 1.0, SHIFT) - 0.1).abs() < 1e-12);
        assert_eq!(step_delta(1.0, 1.0, CTRL), 10.0);
    }

    #[test]
    fn raw_motion_wins_over_pointer_delta() {
        let events = [
            Event::PointerMoved(egui::pos2(5.0, 5.0)),
            Event::MouseMoved(vec2(2.0, -4.0)),
            Event::MouseMoved(vec2(2.0, -6.0)),
        ];
        assert_eq!(raw_delta(&events, 2.0), Some(vec2(2.0, -5.0)));
        assert_eq!(raw_delta(&events[..1], 1.0), Option::None);
    }
}
