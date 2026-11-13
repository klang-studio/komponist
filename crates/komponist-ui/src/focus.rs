//! Keyboard focus: the "focus-visible" rule and the focus ring.
//!
//! "Fokus (Tastatur): 1 px Akzent-Kontur mit 1 px Abstand. […] Fokus ist nur bei
//! Tastaturbedienung als Ring sichtbar, das Panel-Signal immer." The ring is therefore drawn only
//! while the user works with the keyboard, like CSS `:focus-visible`: a key press turns it on, a
//! mouse button press turns it off. [`install`] adds a small egui plugin that watches the input of
//! every pass; [`is_visible`] answers the question for the current pass, and [`ring_visible`]
//! for one widget.

use egui::{Context, Event, Id, Painter, Rect, Response, Ui};
use komponist_tokens::egui_support::color32;
use komponist_tokens::focus;

use crate::px;

fn data_id() -> Id {
    Id::unique("komponist_ui::focus::visible")
}

/// The egui plugin behind [`install`]: tracks whether the last input was keyboard input.
#[derive(Debug, Default)]
pub struct FocusVisible {
    visible: bool,
}

impl FocusVisible {
    /// Applies the events of one pass: a key press shows the ring, a pointer press hides it.
    /// Returns the new value.
    pub fn observe(&mut self, events: &[Event]) -> bool {
        for event in events {
            match event {
                Event::Key { pressed: true, .. } => self.visible = true,
                Event::PointerButton { pressed: true, .. } => self.visible = false,
                _ => {}
            }
        }
        self.visible
    }
}

impl egui::Plugin for FocusVisible {
    fn debug_name(&self) -> &'static str {
        "komponist_ui::focus::FocusVisible"
    }

    fn on_begin_pass(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let visible = ctx.input(|i| self.observe(&i.events));
        ctx.data_mut(|d| d.insert_temp(data_id(), visible));
    }
}

/// Registers the focus-visible tracking on `ctx` (once; further calls do nothing).
pub fn install(ctx: &Context) {
    ctx.add_plugin(FocusVisible::default());
}

/// `true` while keyboard focus is shown as a ring (last input was a key press).
pub fn is_visible(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(data_id())).unwrap_or(false)
}

/// Forces the focus-visible flag for the current pass (gallery and tests). The next input event
/// that changes it wins again.
pub fn set_visible(ctx: &Context, visible: bool) {
    ctx.with_plugin(|p: &mut FocusVisible| p.visible = visible);
    ctx.data_mut(|d| d.insert_temp(data_id(), visible));
}

/// `true` if `response`'s widget has keyboard focus and the ring is to be drawn.
pub fn ring_visible(response: &Response) -> bool {
    response.has_focus() && is_visible(&response.ctx)
}

/// Paints the focus ring around `rect`: 1 px `focus.outline` colour at `focus.offset` (1 px)
/// distance, following the object's corner radius `radius`.
pub fn paint_ring(painter: &Painter, rect: Rect, radius: f32) {
    px::outline(
        painter,
        rect,
        radius,
        focus::OFFSET,
        color32(focus::OUTLINE_COLOR),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers, PointerButton, pos2};

    fn key(pressed: bool) -> Event {
        Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn click(pressed: bool) -> Event {
        Event::PointerButton {
            pos: pos2(1.0, 1.0),
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn keyboard_shows_pointer_hides() {
        let mut f = FocusVisible::default();
        assert!(!f.observe(&[]));
        assert!(f.observe(&[key(true)]));
        assert!(f.observe(&[key(false), click(false)]));
        assert!(!f.observe(&[click(true)]));
        assert!(f.observe(&[click(true), key(true)]));
        assert!(!f.observe(&[key(true), click(true)]));
    }

    #[test]
    fn tracked_per_pass() {
        let ctx = Context::default();
        install(&ctx);
        let run = |events: Vec<Event>| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |_| {}).textures_delta.clear();
            is_visible(&ctx)
        };
        assert!(!run(vec![]));
        assert!(run(vec![key(true)]));
        assert!(run(vec![]));
        assert!(!run(vec![click(true)]));
        set_visible(&ctx, true);
        assert!(run(vec![]));
    }
}
