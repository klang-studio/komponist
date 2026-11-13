//! Switch (`sw`, `switches` of the design system): 28 × 16 pill, 12 px knob inset 2 px.
//!
//! | | track | knob |
//! |---|---|---|
//! | off | surface.3 | text.2, left |
//! | off, hover | surface.4 | text.1 |
//! | on | accent | accent.ink, right |
//! | disabled | surface.2 | text.4 |
//!
//! Click, Enter or Space (when focused) flips it; the response is `changed()` then. Focus ring
//! after keyboard input, following the pill.

use egui::{Color32, Response, Role, Sense, Ui, Vec2, Widget, WidgetInfo, pos2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, size, space};

use crate::state::{self, State};
use crate::{focus, px};

/// Colours of a switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchLook {
    /// The pill.
    pub track: Color32,
    /// The knob.
    pub knob: Color32,
    /// Draw the focus ring.
    pub focus_ring: bool,
}

/// The [`SwitchLook`] of a switch that is `on`, in `state`.
pub fn look(on: bool, state: State) -> SwitchLook {
    let (track, knob) = if state.disabled {
        (color::SURFACE_2, color::TEXT_4)
    } else if on {
        (color::ACCENT, color::ACCENT_INK)
    } else if state.hovered || state.pressed {
        (color::SURFACE_4, color::TEXT_1)
    } else {
        (color::SURFACE_3, color::TEXT_2)
    };
    SwitchLook {
        track: color32(track),
        knob: color32(knob),
        focus_ring: state.focused && !state.disabled,
    }
}

/// Inset of the knob in the pill.
const INSET: f32 = space::SPACE_0_5;

/// A switch bound to a `bool`; `name` is its accessible name ("Normalisieren").
#[must_use = "add it with `ui.add(…)`"]
pub struct Switch<'a> {
    on: &'a mut bool,
    name: &'a str,
}

impl<'a> Switch<'a> {
    /// A switch showing and flipping `on`.
    pub fn new(on: &'a mut bool, name: &'a str) -> Self {
        Self { on, name }
    }
}

impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::from(size::SWITCH), super::sense(ui, Sense::click()));
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let st = State::of(ui, &response);
        let enabled = ui.is_enabled() && !st.disabled;
        let (on, name) = (*self.on, self.name);
        response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, on, name));

        if ui.is_rect_visible(rect) {
            let l = look(on, st);
            let painter = ui.painter();
            let pill = rect.height() / 2.0;
            px::fill(painter, rect, pill, l.track);
            let d = rect.height() - 2.0 * INSET;
            let x = if on {
                rect.right() - INSET - d / 2.0
            } else {
                rect.left() + INSET + d / 2.0
            };
            painter.circle_filled(pos2(x, rect.center().y), d / 2.0, l.knob);
            if l.focus_ring {
                focus::paint_ring(painter, rect, pill);
            }
        }
        response
    }
}

/// The switch row of the "Toggle" card: "Schalter aus", hover, an, deaktiviert.
pub fn gallery_row(ui: &mut Ui) {
    let samples = [
        ("Schalter aus", false, State::REST),
        ("hover", false, State::HOVERED),
        ("an", true, State::REST),
        ("deaktiviert", false, State::DISABLED),
    ];
    super::sample_row(ui, space::SPACE_4 + space::SPACE_1, |ui| {
        for (caption, on, st) in samples {
            super::sample(ui, caption, |ui| {
                let mut on = on;
                let name = format!("Schalter · {caption}");
                state::force(ui, st, |ui| ui.add(Switch::new(&mut on, &name)));
            });
        }
        let id = egui::Id::unique("komponist_gallery_switch_live");
        let mut on: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
        super::sample(ui, "live", |ui| {
            ui.add(Switch::new(&mut on, "Schalter live"))
        });
        ui.data_mut(|d| d.insert_temp(id, on));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_follow_sw() {
        let off = look(false, State::REST);
        assert_eq!(
            (off.track, off.knob),
            (color32(color::SURFACE_3), color32(color::TEXT_2))
        );
        let hover = look(false, State::HOVERED);
        assert_eq!(
            (hover.track, hover.knob),
            (color32(color::SURFACE_4), color32(color::TEXT_1))
        );
        let on = look(true, State::HOVERED);
        assert_eq!(
            (on.track, on.knob),
            (color32(color::ACCENT), color32(color::ACCENT_INK))
        );
        let dis = look(true, State::DISABLED);
        assert_eq!(
            (dis.track, dis.knob),
            (color32(color::SURFACE_2), color32(color::TEXT_4))
        );
        assert!(look(false, State::FOCUSED).focus_ring);
        assert_eq!(size::SWITCH[1] - 2.0 * INSET, 12.0, "12 px knob");
    }
}
