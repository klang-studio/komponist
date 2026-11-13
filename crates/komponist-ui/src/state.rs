//! Widget states and the colours they imply ("Zustandsregeln" of the design system).
//!
//! | State | Rule |
//! |---|---|
//! | Hover | surface +1 step (surface.3 → surface.4), text text.2 → text.1, no border |
//! | Pressed | surface +2 steps (surface.5) |
//! | Focus (keyboard) | 1 px accent ring at 1 px distance, only after keyboard input |
//! | Selection | timeline objects: 1 px accent ring, colour stays; rows: `accent.surface` |
//! | Disabled | surface.2 and text.4, no hover |
//!
//! A widget computes its [`State`] with [`State::of`], then its colours with the helpers below
//! ([`surface_for`], [`text_for`], [`control_look`], [`row_fill`]) and paints the rings with
//! [`crate::focus::paint_ring`] and [`paint_select_ring`]. Every widget (WP-20/21/22) has one
//! function `(state) → look`, tested by snapshots.
//!
//! [`force`] makes everything inside report a given state. The gallery uses it to show a
//! component in all states side by side, snapshot tests to render a state without simulating
//! input.

use egui::{Color32, Id, Painter, Rect, Response, Ui};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Srgba, color, select};

use crate::{focus, px};

/// The interaction state of a widget. Flags combine (a focused widget can be hovered); the
/// colour helpers resolve precedence: disabled, then pressed, then hovered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct State {
    /// The pointer is over the widget.
    pub hovered: bool,
    /// A pointer button is held down on the widget (or it is being dragged).
    pub pressed: bool,
    /// The widget has keyboard focus and the focus ring is visible (focus-visible).
    pub focused: bool,
    /// The widget or object is selected.
    pub selected: bool,
    /// The widget cannot be used.
    pub disabled: bool,
}

impl State {
    /// Nothing going on.
    pub const REST: Self = Self {
        hovered: false,
        pressed: false,
        focused: false,
        selected: false,
        disabled: false,
    };
    /// Pointer over the widget.
    pub const HOVERED: Self = Self {
        hovered: true,
        ..Self::REST
    };
    /// Pointer button down on the widget (hovered as well).
    pub const PRESSED: Self = Self {
        hovered: true,
        pressed: true,
        ..Self::REST
    };
    /// Keyboard focus, ring visible.
    pub const FOCUSED: Self = Self {
        focused: true,
        ..Self::REST
    };
    /// Selected.
    pub const SELECTED: Self = Self {
        selected: true,
        ..Self::REST
    };
    /// Disabled.
    pub const DISABLED: Self = Self {
        disabled: true,
        ..Self::REST
    };

    /// The states the gallery shows for a control, with the labels of the design system
    /// (`buttonRows`): standard, hover, gedrückt, fokus, deaktiviert.
    pub const CONTROL_STATES: [(&'static str, Self); 5] = [
        ("standard", Self::REST),
        ("hover", Self::HOVERED),
        ("gedrückt", Self::PRESSED),
        ("fokus", Self::FOCUSED),
        ("deaktiviert", Self::DISABLED),
    ];

    /// The state of the widget behind `response`, from egui's interaction: hover, pointer down
    /// or drag, focus with visible ring ([`focus::ring_visible`]), enabled. `selected` is the
    /// caller's to set ([`Self::with_selected`]).
    pub fn from_response(response: &Response) -> Self {
        let disabled = !response.enabled();
        Self {
            hovered: !disabled && response.hovered(),
            pressed: !disabled && (response.is_pointer_button_down_on() || response.dragged()),
            focused: !disabled && focus::ring_visible(response),
            selected: false,
            disabled,
        }
    }

    /// Like [`Self::from_response`], but a state forced with [`force`] wins (gallery, tests).
    /// This is what widgets call.
    pub fn of(ui: &Ui, response: &Response) -> Self {
        forced(ui).unwrap_or_else(|| Self::from_response(response))
    }

    /// `self` with `selected` set.
    #[must_use]
    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// `self` with `disabled` set.
    #[must_use]
    pub fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The step the surface rises: 0 at rest, 1 on hover, 2 when pressed, none when disabled.
    pub fn surface_steps(&self) -> usize {
        if self.disabled {
            0
        } else if self.pressed {
            2
        } else if self.hovered {
            1
        } else {
            0
        }
    }

    /// A short name for file names and test output: `rest`, `hovered`, `pressed`, `focused`,
    /// `selected`, `disabled`, joined with `-` when combined.
    pub fn slug(&self) -> String {
        let parts: Vec<&str> = [
            (self.hovered && !self.pressed, "hovered"),
            (self.pressed, "pressed"),
            (self.focused, "focused"),
            (self.selected, "selected"),
            (self.disabled, "disabled"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        if parts.is_empty() {
            "rest".to_owned()
        } else {
            parts.join("-")
        }
    }
}

/// Surface `level` (0–5) of the surface scale, clamped to surface.5.
pub fn surface(level: usize) -> Color32 {
    color32(color::SURFACE[level.min(color::SURFACE.len() - 1)])
}

/// The fill of a control whose resting surface is `base` (e.g. 3 for buttons): +1 on hover, +2
/// pressed (clamped at surface.5), surface.2 when disabled.
pub fn surface_for(base: usize, state: State) -> Color32 {
    if state.disabled {
        color32(color::SURFACE_2)
    } else {
        surface(base + state.surface_steps())
    }
}

/// The text colour of a control whose resting text is `base`: text.2 becomes text.1 on hover
/// and while pressed; disabled is text.4; other colours stay.
pub fn text_for(base: Srgba, state: State) -> Color32 {
    if state.disabled {
        color32(color::TEXT_4)
    } else if (state.hovered || state.pressed) && base == color::TEXT_2 {
        color32(color::TEXT_1)
    } else {
        color32(base)
    }
}

/// Colours and rings of a plain control in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Background surface.
    pub fill: Color32,
    /// Text and icon colour.
    pub text: Color32,
    /// Draw the focus ring ([`focus::paint_ring`]).
    pub focus_ring: bool,
}

/// The [`Look`] of a control resting on surface `base` with text `base_text`.
pub fn control_look(base: usize, base_text: Srgba, state: State) -> Look {
    Look {
        fill: surface_for(base, state),
        text: text_for(base_text, state),
        focus_ring: state.focused && !state.disabled,
    }
}

/// The fill of a row (track header, list entry) resting on surface `base`: `accent.surface`
/// when selected, otherwise as [`surface_for`].
pub fn row_fill(base: usize, state: State) -> Color32 {
    if state.selected && !state.disabled {
        color32(color::ACCENT_SURFACE)
    } else {
        surface_for(base, state)
    }
}

/// Paints the selection ring of a timeline object: `select.ring`, 1 px accent directly at the
/// object (outside its edge), following the corner radius `radius`. The object keeps its colour.
pub fn paint_select_ring(painter: &Painter, rect: Rect, radius: f32) {
    px::outline(painter, rect, radius, 0.0, color32(select::RING.color));
}

fn forced_id() -> Id {
    Id::unique("komponist_ui::state::forced")
}

/// Runs `add_contents` so that every widget inside reports `state` from [`State::of`]. Nested
/// calls stack.
pub fn force<R>(ui: &mut Ui, state: State, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let ctx = ui.ctx().clone();
    let id = forced_id();
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<State>>(id).push(state));
    let result = add_contents(ui);
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<State>>(id).pop();
    });
    result
}

/// The state forced by the innermost [`force`] around the current code, if any.
pub fn forced(ui: &Ui) -> Option<State> {
    ui.ctx().data(|d| {
        d.get_temp::<Vec<State>>(forced_id())
            .and_then(|stack| stack.last().copied())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_follow_the_rules() {
        assert_eq!(surface_for(3, State::REST), color32(color::SURFACE_3));
        assert_eq!(surface_for(3, State::HOVERED), color32(color::SURFACE_4));
        assert_eq!(surface_for(3, State::PRESSED), color32(color::SURFACE_5));
        assert_eq!(surface_for(4, State::PRESSED), color32(color::SURFACE_5));
        assert_eq!(surface_for(3, State::DISABLED), color32(color::SURFACE_2));
        let disabled_hover = State {
            hovered: true,
            ..State::DISABLED
        };
        assert_eq!(surface_for(3, disabled_hover), color32(color::SURFACE_2));
        assert_eq!(surface(9), color32(color::SURFACE_5));
    }

    #[test]
    fn text_follows_the_rules() {
        assert_eq!(text_for(color::TEXT_2, State::REST), color32(color::TEXT_2));
        assert_eq!(
            text_for(color::TEXT_2, State::HOVERED),
            color32(color::TEXT_1)
        );
        assert_eq!(
            text_for(color::TEXT_2, State::PRESSED),
            color32(color::TEXT_1)
        );
        assert_eq!(
            text_for(color::TEXT_3, State::HOVERED),
            color32(color::TEXT_3)
        );
        assert_eq!(
            text_for(color::TEXT_1, State::DISABLED),
            color32(color::TEXT_4)
        );
    }

    #[test]
    fn looks_and_rows() {
        let l = control_look(3, color::TEXT_1, State::FOCUSED);
        assert!(l.focus_ring);
        assert_eq!(l.fill, color32(color::SURFACE_3));
        assert!(!control_look(3, color::TEXT_1, State::REST).focus_ring);
        assert!(
            !control_look(
                3,
                color::TEXT_1,
                State {
                    focused: true,
                    disabled: true,
                    ..State::REST
                }
            )
            .focus_ring
        );
        assert_eq!(row_fill(2, State::SELECTED), color32(color::ACCENT_SURFACE));
        assert_eq!(
            row_fill(2, State::SELECTED.with_disabled(true)),
            color32(color::SURFACE_2)
        );
        assert_eq!(row_fill(2, State::HOVERED), color32(color::SURFACE_3));
    }

    #[test]
    fn slugs() {
        assert_eq!(State::REST.slug(), "rest");
        assert_eq!(State::PRESSED.slug(), "pressed");
        assert_eq!(
            State::HOVERED.with_selected(true).slug(),
            "hovered-selected"
        );
        let names: Vec<String> = State::CONTROL_STATES
            .iter()
            .map(|(_, s)| s.slug())
            .collect();
        assert_eq!(names, ["rest", "hovered", "pressed", "focused", "disabled"]);
    }

    #[test]
    fn forced_states_stack() {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert_eq!(forced(ui), None);
            force(ui, State::HOVERED, |ui| {
                assert_eq!(forced(ui), Some(State::HOVERED));
                force(ui, State::DISABLED, |ui| {
                    let r = ui.label("x");
                    assert_eq!(State::of(ui, &r), State::DISABLED);
                });
                assert_eq!(forced(ui), Some(State::HOVERED));
            });
            assert_eq!(forced(ui), None);
            let r = ui.label("x");
            assert_eq!(State::of(ui, &r), State::REST);
        });
        let mut output = output;
        output.textures_delta.clear();
    }
}
