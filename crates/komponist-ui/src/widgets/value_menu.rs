//! The right-click menu of a knob or fader outside the device cards: „Auf Standard“ and „Wert
//! eingeben“ (owner decision 2026-11-15: a double-click resets, so text entry has this menu and
//! Enter). The device cards build the same menu themselves, because their entries also report
//! the touched parameter.
//!
//! ```ignore
//! let r = ui.add(Fader::new(&mut db, "Lautstärke"));
//! if value_menu::show(&r, || fmt_db(db)) {
//!     db = 0.0; // "Auf Standard": the caller sets its default and applies it as one edit
//! }
//! ```

use egui::Response;

use super::context_menu::{ContextMenu, Entry};
use super::number_field::open_value_entry;

/// Accessible name of the menu.
pub const MENU: &str = "Wert";
/// Entry: back to the default value.
pub const TO_DEFAULT: &str = "Auf Standard";
/// Its shortcut column.
pub const TO_DEFAULT_SHORTCUT: &str = "Doppelklick";
/// Entry: open the text entry.
pub const ENTER_VALUE: &str = "Wert eingeben";
/// Its shortcut column.
pub const ENTER_VALUE_SHORTCUT: &str = "Enter";

/// What the menu chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Default,
    Enter,
}

/// Shows the menu on a right-click on `target` (a knob or fader). "Wert eingeben" opens the
/// control's text entry with `entry_text` selected; "Auf Standard" returns `true`: the caller sets
/// the default value and applies it (one undo step, like an Alt-click).
pub fn show(target: &Response, entry_text: impl FnOnce() -> String) -> bool {
    let entries = [
        Entry::action(TO_DEFAULT, Choice::Default).shortcut(TO_DEFAULT_SHORTCUT),
        Entry::action(ENTER_VALUE, Choice::Enter).shortcut(ENTER_VALUE_SHORTCUT),
    ];
    match ContextMenu::new(MENU, &entries).show_on(target) {
        Some(Choice::Default) => true,
        Some(Choice::Enter) => {
            open_value_entry(&target.ctx, target.id, entry_text());
            false
        }
        None => false,
    }
}
