//! Keyboard shortcuts: (key, modifiers, focus area) → [`Action`] (ARCHITECTURE §9.3, R8).
//!
//! The default table (WP-30) is filled from `HANDOFF.md` §7 and the design system's `shortcuts` /
//! `ix` lists:
//!
//! | Keys | Scope | Action |
//! |---|---|---|
//! | Leertaste | global | `Transport(TogglePlay)` |
//! | R / L / C | global | `Transport(ToggleRecord / ToggleLoop / ToggleMetronome)` |
//! | Pos1 | global | `Transport(GoToStart)` |
//! | Tab | canvases | `Ui(ToggleMixer)` (§13.1; elsewhere Tab moves widget focus) |
//! | Strg T | global | `Edit(NewMidiTrack)` |
//! | Strg N / O / S | global | `Project(New / Open / Save)` |
//! | Strg Shift E | global | `Project(OpenExportDialog)` |
//! | Strg 1 / 2 / 3 | global | `Ui(TogglePanel(Browser / Inspector / Dock))` |
//! | Strg ↑ | global | `Ui(ToggleDockMaximized)` |
//! | Strg Tab | global | `Ui(NextTab)` |
//! | Strg F | global | `Ui(FocusBrowserSearch)` |
//! | + / − | canvases | `Ui(Zoom { vertical: false, steps: ±1 })` |
//! | Alt + / Alt − | canvases | `Ui(Zoom { vertical: true, steps: ±1 })` |
//! | Z | canvases | `Ui(ZoomToFit)` (only while computer keyboard MIDI is off, §13.2) |
//! | Strg [ / Strg ] | canvases | `Ui(GridFiner / GridCoarser)` |
//! | Strg Z / Strg Shift Z | global | `Edit(Undo / Redo)` |
//! | Strg , | global | `Ui(OpenSettings(Audio))` |
//! | Esc | global | `Ui(ClearSelection)` (recording: `Transport(DiscardTake)`) |
//! | Strg D / Strg E / 0 | arrangement | `Edit(DuplicateSelection / SplitAtPlayhead / ToggleMuteSelection)` |
//! | M | arrangement | `Edit(ToggleMuteTracks)` |
//! | Entf | arrangement, piano roll | `Edit(DeleteSelection)` |
//! | ↑ ↓ / Shift ↑ ↓ | piano roll | `Edit(TransposeSelection(±1 / ±12))` |
//! | ← → | piano roll | `Edit(NudgeSelection(±1))` |
//! | Q | piano roll | `Edit(QuantizeSelection)` |
//! | Strg A | piano roll, arrangement | `Ui(SelectAll)` |
//! | 1 / 2 / 3 | piano roll | `Ui(SetTool(Pointer / Pencil / Eraser))` |
//!
//! Computer keyboard MIDI is a separate layer ([`NOTE_KEYS`]) checked before the table while
//! [`crate::ui_state::UiState::keyboard_midi`] is on; text fields get keys before both.

use crate::actions::Action;
use crate::ui_state::FocusArea;

/// Keys the keymap can bind (logical keys; letters by their US-layout name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    Space,
    Tab,
    Enter,
    Escape,
    Delete,
    Backspace,
    Home,
    End,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    /// `+` (also `=` and numpad `+`).
    Plus,
    /// `-` (also numpad `-`).
    Minus,
    /// `[`.
    OpenBracket,
    /// `]`.
    CloseBracket,
    /// `,`.
    Comma,
}

/// Modifier state of a chord. Ctrl means Ctrl (Linux); there is no Cmd mapping in v0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Modifiers {
    /// Ctrl ("Strg").
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt.
    pub alt: bool,
}

/// A key with exact modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    /// The key.
    pub key: Key,
    /// Modifiers that must be held (and no others).
    pub mods: Modifiers,
}

impl KeyChord {
    /// The German label shown in tooltips and context menus ("Strg Shift E", "Leertaste", "Pos1",
    /// "Entf", "Strg ↑"), as written in `HANDOFF.md` §7.
    pub fn label(&self) -> String {
        todo!("WP-30")
    }
}

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    /// Everywhere except dialogs and text fields.
    Global,
    /// Only while this area has focus; wins over a global binding of the same chord.
    Area(FocusArea),
    /// While any canvas has focus ([`FocusArea::is_canvas`]).
    Canvases,
}

/// One shortcut.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// The chord.
    pub chord: KeyChord,
    /// Where it applies.
    pub scope: KeyScope,
    /// What it does.
    pub action: Action,
}

/// The shortcut table.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    /// An empty keymap.
    pub fn new() -> Keymap {
        Keymap::default()
    }

    /// The default table (see the module docs).
    pub fn default_bindings() -> Keymap {
        todo!("WP-30")
    }

    /// Adds a binding (later bindings win over earlier ones with the same chord and scope).
    pub fn bind(&mut self, binding: Binding) {
        self.bindings.push(binding);
    }

    /// All bindings in insertion order.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The action for `chord` with `focus`: an `Area` binding for that area first, then
    /// `Canvases` (if the area is a canvas), then `Global`. Global bindings do not apply while a
    /// dialog has focus.
    pub fn lookup(&self, chord: KeyChord, focus: FocusArea) -> Option<&Action> {
        todo!("WP-30")
    }

    /// The first chord bound to `action` (the shortcut column of menus and tooltips).
    pub fn chord_for(&self, action: &Action) -> Option<KeyChord> {
        todo!("WP-30")
    }
}

/// MIDI note of the [`NOTE_KEYS`] offset 0: C4.
pub const NOTE_KEYS_BASE: u8 = 60;

/// Computer keyboard MIDI (prototype `NOTEKEYS`): physical key positions (US names) and semitone
/// offsets from [`NOTE_KEYS_BASE`]. Y and Z both play G# so the layer works on German (QWERTZ) and
/// US layouts.
pub const NOTE_KEYS: [(Key, u8); 14] = [
    (Key::A, 0),
    (Key::W, 1),
    (Key::S, 2),
    (Key::E, 3),
    (Key::D, 4),
    (Key::F, 5),
    (Key::T, 6),
    (Key::G, 7),
    (Key::Y, 8),
    (Key::Z, 8),
    (Key::H, 9),
    (Key::U, 10),
    (Key::J, 11),
    (Key::K, 12),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_keys_span_one_octave() {
        let offsets: Vec<u8> = NOTE_KEYS.iter().map(|(_, o)| *o).collect();
        assert_eq!(offsets.first(), Some(&0));
        assert_eq!(offsets.last(), Some(&12));
        // Y and Z are the same physical position on QWERTZ / QWERTY.
        assert!(NOTE_KEYS.contains(&(Key::Y, 8)) && NOTE_KEYS.contains(&(Key::Z, 8)));
    }

    #[test]
    fn tab_toggles_the_mixer_only_from_canvases() {
        assert!(FocusArea::Arrangement.is_canvas());
        assert!(FocusArea::Mixer.is_canvas());
        assert!(!FocusArea::Inspector.is_canvas());
        assert!(!FocusArea::Dialog.is_canvas());
    }
}
