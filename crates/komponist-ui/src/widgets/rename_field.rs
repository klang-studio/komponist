//! The inline rename field (arrangement track header, inspector name, mixer strip name): a text
//! field over the name being renamed, `surface.0` with an accent ring.
//!
//! The rules, shared by all places:
//!
//! - the first frame focuses the field and selects all of its text ([`RenameState::new`]);
//! - Enter or a click elsewhere commits the trimmed name ([`RenameOutcome::Commit`]);
//! - Esc cancels; an empty, blank or unchanged name cancels too (the old name stays), so one
//!   rename is at most one edit and one undo step;
//! - the field has the AccessKit role of a text input and the caller's accessible name.
//!
//! Font, text colour and rect stay the caller's: the field only paints the ground and the ring.
//! The caller sends its own edit on [`RenameOutcome::Commit`] and drops the [`RenameState`] on
//! anything but [`RenameOutcome::Editing`].

use egui::text::CCursorRange;
use egui::{Event, FontId, Frame, Id, Key, Margin, Rect, TextEdit, Ui, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, space};

use crate::px;

/// The text being edited and whether the field still has to take the focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameState {
    text: String,
    /// Focus and select all in the next frame.
    fresh: bool,
}

impl RenameState {
    /// Starts renaming `current` (preselected in the first frame).
    pub fn new(current: &str) -> Self {
        Self {
            text: current.to_owned(),
            fresh: true,
        }
    }

    /// The text as typed so far.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// What a frame of the field leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    /// Still being edited.
    Editing,
    /// Enter or a click elsewhere with a new name: the trimmed name.
    Commit(String),
    /// Esc, or an empty, blank or unchanged name (or, with
    /// [`RenameField::cancel_unfocused`], a field that lost its focus unseen).
    Cancel,
}

/// The field: `RenameField::new(&mut state, id, "Umbenennen", &current, font).show(ui, rect)`.
pub struct RenameField<'a> {
    state: &'a mut RenameState,
    id: Id,
    name: String,
    current: &'a str,
    font: FontId,
    cancel_unfocused: bool,
}

impl<'a> RenameField<'a> {
    /// A field for `state` with the text edit `id`, the accessible `name`, the name before the
    /// rename (`current`) and the `font` of the name it covers. The text is `text.1`.
    pub fn new(
        state: &'a mut RenameState,
        id: Id,
        name: impl Into<String>,
        current: &'a str,
        font: FontId,
    ) -> Self {
        Self {
            state,
            id,
            name: name.into(),
            current,
            font,
            cancel_unfocused: false,
        }
    }

    /// A field that has no focus after its first frame (the view was not shown for a while) is
    /// cancelled instead of staying open.
    #[must_use]
    pub fn cancel_unfocused(mut self) -> Self {
        self.cancel_unfocused = true;
        self
    }

    /// Paints the field over `rect` (the name's rect) and runs one frame of the rules.
    pub fn show(self, ui: &mut Ui, rect: Rect) -> RenameOutcome {
        let state = self.state;
        let field = rect.expand2(vec2(space::SPACE_1, 0.0));
        px::fill(ui.painter(), field, radius::S, color32(color::SURFACE_0));
        px::outline(ui.painter(), field, radius::S, 0.0, color32(color::ACCENT));
        let mut text = state.text.clone();
        let response = ui.put(
            rect,
            TextEdit::singleline(&mut text)
                .id(self.id)
                .frame(Frame::NONE)
                .margin(Margin::ZERO)
                .font(self.font)
                .text_color(color32(color::TEXT_1))
                .desired_width(rect.width()),
        );
        let label = self.name;
        ui.ctx()
            .accesskit_node_builder(response.id, |n| n.set_label(label));
        if state.fresh {
            // Focus after the field exists: the double-click that opened it would otherwise
            // count as a click elsewhere.
            let mut st = TextEdit::load_state(ui.ctx(), response.id).unwrap_or_default();
            st.cursor
                .set_char_range(Some(CCursorRange::select_all_str(&text)));
            TextEdit::store_state(ui.ctx(), response.id, st);
            response.request_focus();
            state.fresh = false;
            state.text = text;
            return RenameOutcome::Editing;
        }
        if response.lost_focus() {
            let name = text.trim();
            if !escape_pressed(ui) && !name.is_empty() && name != self.current {
                RenameOutcome::Commit(name.to_owned())
            } else {
                RenameOutcome::Cancel
            }
        } else if self.cancel_unfocused && !response.has_focus() {
            RenameOutcome::Cancel
        } else {
            state.text = text;
            RenameOutcome::Editing
        }
    }
}

/// Whether Esc went down in this pass. Read from the raw input: the app's keymap consumes Esc
/// (as `ClearSelection`, or `CancelGesture` during a drag) before the views run.
fn escape_pressed(ui: &Ui) -> bool {
    ui.input(|i| {
        i.raw.events.iter().any(|e| {
            matches!(
                e,
                Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                }
            )
        })
    })
}
