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
//! | Strg Shift S | global | `Project(SaveAs)` |
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
//! | Rücktaste | arrangement, piano roll | `Edit(DeleteSelection)` (prototype `key()`) |
//! | Strg C / Strg D | device chain | `Ui(CopyDevices)` / `Edit(DuplicateDevices)` |
//! | Strg V | device chain | `Edit(PasteDevices)` |
//! | Entf, Rücktaste | device chain | `Edit(DeleteDevices)` |
//! | ← → | device chain | `Ui(SelectAdjacentDevice(∓1))` |
//! | Esc | dialog | `Ui(CloseDialog)` |
//!
//! Computer keyboard MIDI is a separate layer ([`NOTE_KEYS`], [`note_for`]) checked before the
//! table while it is active (a track is armed, or [`crate::ui_state::UiState::keyboard_midi`]
//! is on; ARCHITECTURE §13.2), by physical key position; text fields get keys before both. The
//! app (`App::show`) applies the table: the Esc binding means `Transport(DiscardTake)` while
//! recording, and a widget reached with Tab keeps Space, Enter, arrows and Tab.

use komponist_i18n::Lang;

use crate::actions::{Action, EditAction, ProjectAction, TransportAction, UiAction};
use crate::ui_state::{FocusArea, Panel, SettingsSection, Tool};

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
    /// The label shown in tooltips and context menus, in `t`: German "Strg Shift E", "Leertaste",
    /// "Pos1", "Entf", "Strg ↑" (as written in `HANDOFF.md` §7), English "Ctrl+Shift+E", "Space",
    /// "Home", "Del", "Ctrl+↑". The `key-*` messages hold the names and the separator.
    pub fn label(&self, t: Lang) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(4);
        if self.mods.ctrl {
            parts.push(t.key_ctrl());
        }
        if self.mods.shift {
            parts.push(t.key_shift());
        }
        if self.mods.alt {
            parts.push(t.key_alt());
        }
        parts.push(self.key.label(t));
        parts.join(t.key_separator())
    }

    /// `key` without modifiers.
    pub const fn plain(key: Key) -> KeyChord {
        KeyChord {
            key,
            mods: Modifiers::NONE,
        }
    }

    /// Ctrl + `key`.
    pub const fn ctrl(key: Key) -> KeyChord {
        KeyChord {
            key,
            mods: Modifiers::CTRL,
        }
    }

    /// Ctrl + Shift + `key`.
    pub const fn ctrl_shift(key: Key) -> KeyChord {
        KeyChord {
            key,
            mods: Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            },
        }
    }

    /// Shift + `key`.
    pub const fn shift(key: Key) -> KeyChord {
        KeyChord {
            key,
            mods: Modifiers {
                ctrl: false,
                shift: true,
                alt: false,
            },
        }
    }

    /// Alt + `key`.
    pub const fn alt(key: Key) -> KeyChord {
        KeyChord {
            key,
            mods: Modifiers {
                ctrl: false,
                shift: false,
                alt: true,
            },
        }
    }
}

impl Modifiers {
    /// No modifier.
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
        alt: false,
    };

    /// Ctrl only.
    pub const CTRL: Modifiers = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };
}

impl Key {
    /// The key name in `t`; German as `HANDOFF.md` §7 writes it ("Leertaste", "Pos1", "Entf",
    /// "↑", "−"). Letters, digits, arrows and symbols are the same in every language.
    pub fn label(self, t: Lang) -> &'static str {
        match self {
            Key::A => "A",
            Key::B => "B",
            Key::C => "C",
            Key::D => "D",
            Key::E => "E",
            Key::F => "F",
            Key::G => "G",
            Key::H => "H",
            Key::I => "I",
            Key::J => "J",
            Key::K => "K",
            Key::L => "L",
            Key::M => "M",
            Key::N => "N",
            Key::O => "O",
            Key::P => "P",
            Key::Q => "Q",
            Key::R => "R",
            Key::S => "S",
            Key::T => "T",
            Key::U => "U",
            Key::V => "V",
            Key::W => "W",
            Key::X => "X",
            Key::Y => "Y",
            Key::Z => "Z",
            Key::Num0 => "0",
            Key::Num1 => "1",
            Key::Num2 => "2",
            Key::Num3 => "3",
            Key::Num4 => "4",
            Key::Num5 => "5",
            Key::Num6 => "6",
            Key::Num7 => "7",
            Key::Num8 => "8",
            Key::Num9 => "9",
            Key::Space => t.key_space(),
            Key::Tab => t.key_tab(),
            Key::Enter => t.key_enter(),
            Key::Escape => t.key_escape(),
            Key::Delete => t.key_delete(),
            Key::Backspace => t.key_backspace(),
            Key::Home => t.key_home(),
            Key::End => t.key_end(),
            Key::ArrowUp => "↑",
            Key::ArrowDown => "↓",
            Key::ArrowLeft => "←",
            Key::ArrowRight => "→",
            Key::Plus => "+",
            Key::Minus => "\u{2212}",
            Key::OpenBracket => "[",
            Key::CloseBracket => "]",
            Key::Comma => ",",
        }
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
        use KeyScope::{Area, Canvases, Global};
        let arrangement = Area(FocusArea::Arrangement);
        let piano_roll = Area(FocusArea::PianoRoll);
        let devices = Area(FocusArea::Devices);
        let t = Action::Transport;
        let e = Action::Edit;
        let u = Action::Ui;
        let p = Action::Project;
        let table: Vec<(KeyChord, KeyScope, Action)> = vec![
            // Transport.
            (
                KeyChord::plain(Key::Space),
                Global,
                t(TransportAction::TogglePlay),
            ),
            (
                KeyChord::plain(Key::R),
                Global,
                t(TransportAction::ToggleRecord),
            ),
            (
                KeyChord::plain(Key::L),
                Global,
                t(TransportAction::ToggleLoop),
            ),
            (
                KeyChord::plain(Key::C),
                Global,
                t(TransportAction::ToggleMetronome),
            ),
            (
                KeyChord::plain(Key::Home),
                Global,
                t(TransportAction::GoToStart),
            ),
            // Views and panels.
            (
                KeyChord::plain(Key::Tab),
                Canvases,
                u(UiAction::ToggleMixer),
            ),
            (
                KeyChord::ctrl(Key::Num1),
                Global,
                u(UiAction::TogglePanel(Panel::Browser)),
            ),
            (
                KeyChord::ctrl(Key::Num2),
                Global,
                u(UiAction::TogglePanel(Panel::Inspector)),
            ),
            (
                KeyChord::ctrl(Key::Num3),
                Global,
                u(UiAction::TogglePanel(Panel::Dock)),
            ),
            (
                KeyChord::ctrl(Key::ArrowUp),
                Global,
                u(UiAction::ToggleDockMaximized),
            ),
            (KeyChord::ctrl(Key::Tab), Global, u(UiAction::NextTab)),
            (
                KeyChord::ctrl(Key::F),
                Global,
                u(UiAction::FocusBrowserSearch),
            ),
            (
                KeyChord::ctrl(Key::Comma),
                Global,
                u(UiAction::OpenSettings(SettingsSection::Audio)),
            ),
            (
                KeyChord::plain(Key::Escape),
                Global,
                u(UiAction::ClearSelection),
            ),
            (
                KeyChord::plain(Key::Escape),
                Area(FocusArea::Dialog),
                u(UiAction::CloseDialog),
            ),
            // Project.
            (KeyChord::ctrl(Key::T), Global, e(EditAction::NewMidiTrack)),
            (KeyChord::ctrl(Key::N), Global, p(ProjectAction::New)),
            (KeyChord::ctrl(Key::O), Global, p(ProjectAction::Open)),
            (KeyChord::ctrl(Key::S), Global, p(ProjectAction::Save)),
            (
                KeyChord::ctrl_shift(Key::S),
                Global,
                p(ProjectAction::SaveAs),
            ),
            (
                KeyChord::ctrl_shift(Key::E),
                Global,
                p(ProjectAction::OpenExportDialog),
            ),
            // History.
            (KeyChord::ctrl(Key::Z), Global, e(EditAction::Undo)),
            (KeyChord::ctrl_shift(Key::Z), Global, e(EditAction::Redo)),
            // Canvases: zoom, fit, grid.
            (KeyChord::plain(Key::Plus), Canvases, u(zoom(false, 1))),
            (KeyChord::plain(Key::Minus), Canvases, u(zoom(false, -1))),
            (KeyChord::alt(Key::Plus), Canvases, u(zoom(true, 1))),
            (KeyChord::alt(Key::Minus), Canvases, u(zoom(true, -1))),
            (KeyChord::plain(Key::Z), Canvases, u(UiAction::ZoomToFit)),
            (
                KeyChord::ctrl(Key::OpenBracket),
                Canvases,
                u(UiAction::GridFiner),
            ),
            (
                KeyChord::ctrl(Key::CloseBracket),
                Canvases,
                u(UiAction::GridCoarser),
            ),
            // Arrangement: clips and tracks.
            (
                KeyChord::ctrl(Key::D),
                arrangement,
                e(EditAction::DuplicateSelection),
            ),
            (
                KeyChord::ctrl(Key::E),
                arrangement,
                e(EditAction::SplitAtPlayhead),
            ),
            (
                KeyChord::plain(Key::Num0),
                arrangement,
                e(EditAction::ToggleMuteSelection),
            ),
            (
                KeyChord::plain(Key::M),
                arrangement,
                e(EditAction::ToggleMuteTracks),
            ),
            (
                KeyChord::plain(Key::Delete),
                arrangement,
                e(EditAction::DeleteSelection),
            ),
            (
                KeyChord::plain(Key::Backspace),
                arrangement,
                e(EditAction::DeleteSelection),
            ),
            (KeyChord::ctrl(Key::A), arrangement, u(UiAction::SelectAll)),
            // Device chain: the selected devices.
            (KeyChord::ctrl(Key::C), devices, u(UiAction::CopyDevices)),
            (KeyChord::ctrl(Key::V), devices, e(EditAction::PasteDevices)),
            (
                KeyChord::ctrl(Key::D),
                devices,
                e(EditAction::DuplicateDevices),
            ),
            (
                KeyChord::plain(Key::Delete),
                devices,
                e(EditAction::DeleteDevices),
            ),
            (
                KeyChord::plain(Key::Backspace),
                devices,
                e(EditAction::DeleteDevices),
            ),
            (
                KeyChord::plain(Key::ArrowLeft),
                devices,
                u(UiAction::SelectAdjacentDevice(-1)),
            ),
            (
                KeyChord::plain(Key::ArrowRight),
                devices,
                u(UiAction::SelectAdjacentDevice(1)),
            ),
            // Piano roll: notes and tools.
            (
                KeyChord::plain(Key::Delete),
                piano_roll,
                e(EditAction::DeleteSelection),
            ),
            (
                KeyChord::plain(Key::Backspace),
                piano_roll,
                e(EditAction::DeleteSelection),
            ),
            (KeyChord::ctrl(Key::A), piano_roll, u(UiAction::SelectAll)),
            (
                KeyChord::plain(Key::ArrowUp),
                piano_roll,
                e(EditAction::TransposeSelection(1)),
            ),
            (
                KeyChord::plain(Key::ArrowDown),
                piano_roll,
                e(EditAction::TransposeSelection(-1)),
            ),
            (
                KeyChord::shift(Key::ArrowUp),
                piano_roll,
                e(EditAction::TransposeSelection(12)),
            ),
            (
                KeyChord::shift(Key::ArrowDown),
                piano_roll,
                e(EditAction::TransposeSelection(-12)),
            ),
            (
                KeyChord::plain(Key::ArrowLeft),
                piano_roll,
                e(EditAction::NudgeSelection(-1)),
            ),
            (
                KeyChord::plain(Key::ArrowRight),
                piano_roll,
                e(EditAction::NudgeSelection(1)),
            ),
            (
                KeyChord::plain(Key::Q),
                piano_roll,
                e(EditAction::QuantizeSelection),
            ),
            (
                KeyChord::plain(Key::Num1),
                piano_roll,
                u(UiAction::SetTool(Tool::Pointer)),
            ),
            (
                KeyChord::plain(Key::Num2),
                piano_roll,
                u(UiAction::SetTool(Tool::Pencil)),
            ),
            (
                KeyChord::plain(Key::Num3),
                piano_roll,
                u(UiAction::SetTool(Tool::Eraser)),
            ),
        ];
        let mut keymap = Keymap::new();
        for (chord, scope, action) in table {
            keymap.bind(Binding {
                chord,
                scope,
                action,
            });
        }
        keymap
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
        let find = |scope: KeyScope| {
            self.bindings
                .iter()
                .rev()
                .find(|b| b.chord == chord && b.scope == scope)
                .map(|b| &b.action)
        };
        find(KeyScope::Area(focus))
            .or_else(|| {
                focus
                    .is_canvas()
                    .then(|| find(KeyScope::Canvases))
                    .flatten()
            })
            .or_else(|| {
                (focus != FocusArea::Dialog)
                    .then(|| find(KeyScope::Global))
                    .flatten()
            })
    }

    /// The first chord bound to `action` (the shortcut column of menus and tooltips).
    pub fn chord_for(&self, action: &Action) -> Option<KeyChord> {
        self.bindings
            .iter()
            .find(|b| b.action == *action)
            .map(|b| b.chord)
    }

    /// The label of [`Keymap::chord_for`] in `t` ("Strg 3"), for tooltips and menus.
    pub fn label_for(&self, action: &Action, t: Lang) -> Option<String> {
        self.chord_for(action).map(|c| c.label(t))
    }
}

fn zoom(vertical: bool, steps: i32) -> UiAction {
    UiAction::Zoom { vertical, steps }
}

/// The MIDI note the computer keyboard plays for the physical key `key` ([`NOTE_KEYS`] from
/// [`NOTE_KEYS_BASE`]), if it is a note key.
pub fn note_for(key: Key) -> Option<u8> {
    NOTE_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, offset)| NOTE_KEYS_BASE + offset)
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
        let k = Keymap::default_bindings();
        let tab = KeyChord::plain(Key::Tab);
        let mixer = Action::Ui(UiAction::ToggleMixer);
        for area in [
            FocusArea::Arrangement,
            FocusArea::PianoRoll,
            FocusArea::StepSequencer,
            FocusArea::Mixer,
        ] {
            assert_eq!(k.lookup(tab, area), Some(&mixer), "{area:?}");
        }
        for area in [
            FocusArea::Browser,
            FocusArea::Inspector,
            FocusArea::Devices,
            FocusArea::Transport,
            FocusArea::Dialog,
        ] {
            assert_eq!(k.lookup(tab, area), None, "{area:?}: Tab moves the focus");
        }
    }

    #[test]
    fn labels_follow_the_handoff() {
        let label = |c: KeyChord| c.label(Lang::De);
        assert_eq!(label(KeyChord::plain(Key::Space)), "Leertaste");
        assert_eq!(label(KeyChord::ctrl_shift(Key::E)), "Strg Shift E");
        assert_eq!(label(KeyChord::plain(Key::Home)), "Pos1");
        assert_eq!(label(KeyChord::plain(Key::Delete)), "Entf");
        assert_eq!(label(KeyChord::ctrl(Key::ArrowUp)), "Strg ↑");
        assert_eq!(label(KeyChord::alt(Key::Minus)), "Alt \u{2212}");
        assert_eq!(label(KeyChord::ctrl(Key::Comma)), "Strg ,");
        assert_eq!(label(KeyChord::ctrl(Key::Num3)), "Strg 3");
        assert_eq!(label(KeyChord::shift(Key::ArrowDown)), "Shift ↓");
    }

    #[test]
    fn english_labels_use_plus_and_english_key_names() {
        let label = |c: KeyChord| c.label(Lang::En);
        assert_eq!(label(KeyChord::ctrl_shift(Key::E)), "Ctrl+Shift+E");
        assert_eq!(label(KeyChord::plain(Key::Space)), "Space");
        assert_eq!(label(KeyChord::plain(Key::Delete)), "Del");
        assert_eq!(label(KeyChord::plain(Key::Backspace)), "Backspace");
        assert_eq!(label(KeyChord::plain(Key::Home)), "Home");
        assert_eq!(label(KeyChord::plain(Key::End)), "End");
        assert_eq!(label(KeyChord::ctrl(Key::ArrowUp)), "Ctrl+↑");
        assert_eq!(label(KeyChord::alt(Key::Minus)), "Alt+\u{2212}");
        let k = Keymap::default_bindings();
        let export = Action::Project(ProjectAction::OpenExportDialog);
        assert_eq!(
            k.label_for(&export, Lang::En).as_deref(),
            Some("Ctrl+Shift+E")
        );
    }

    #[test]
    fn area_bindings_win_over_canvases_and_global() {
        let k = Keymap::default_bindings();
        let del = KeyChord::plain(Key::Delete);
        let delete = Action::Edit(EditAction::DeleteSelection);
        assert_eq!(k.lookup(del, FocusArea::Arrangement), Some(&delete));
        assert_eq!(k.lookup(del, FocusArea::PianoRoll), Some(&delete));
        assert_eq!(k.lookup(del, FocusArea::Mixer), None);
        // Esc: global clears the selection, in a dialog it closes the dialog.
        let esc = KeyChord::plain(Key::Escape);
        assert_eq!(
            k.lookup(esc, FocusArea::Arrangement),
            Some(&Action::Ui(UiAction::ClearSelection))
        );
        assert_eq!(
            k.lookup(esc, FocusArea::Dialog),
            Some(&Action::Ui(UiAction::CloseDialog))
        );
        // Globals do not reach into dialogs.
        assert_eq!(
            k.lookup(KeyChord::plain(Key::Space), FocusArea::Dialog),
            None
        );
        // Canvases: zoom only on canvases.
        let plus = KeyChord::plain(Key::Plus);
        assert!(k.lookup(plus, FocusArea::Arrangement).is_some());
        assert!(k.lookup(plus, FocusArea::Inspector).is_none());
        // Exact modifiers.
        assert_eq!(
            k.lookup(KeyChord::shift(Key::Space), FocusArea::Arrangement),
            None
        );
        // Numbers pick tools in the piano roll only.
        assert_eq!(
            k.lookup(KeyChord::plain(Key::Num2), FocusArea::PianoRoll),
            Some(&Action::Ui(UiAction::SetTool(Tool::Pencil)))
        );
        assert_eq!(
            k.lookup(KeyChord::plain(Key::Num2), FocusArea::Arrangement),
            None
        );
    }

    #[test]
    fn later_bindings_win_and_chords_are_found_by_action() {
        let mut k = Keymap::default_bindings();
        let play = Action::Transport(TransportAction::TogglePlay);
        assert_eq!(k.chord_for(&play), Some(KeyChord::plain(Key::Space)));
        assert_eq!(k.label_for(&play, Lang::De).as_deref(), Some("Leertaste"));
        let export = Action::Project(ProjectAction::OpenExportDialog);
        assert_eq!(
            k.label_for(&export, Lang::De).as_deref(),
            Some("Strg Shift E")
        );
        k.bind(Binding {
            chord: KeyChord::plain(Key::Space),
            scope: KeyScope::Global,
            action: Action::Transport(TransportAction::Stop),
        });
        assert_eq!(
            k.lookup(KeyChord::plain(Key::Space), FocusArea::Arrangement),
            Some(&Action::Transport(TransportAction::Stop))
        );
        assert_eq!(k.chord_for(&Action::Ui(UiAction::ToggleKeyboardMidi)), None);
    }

    #[test]
    fn every_handoff_shortcut_is_bound() {
        let k = Keymap::default_bindings();
        // HANDOFF.md §7, in its order.
        let global = [
            (KeyChord::plain(Key::Space), FocusArea::Arrangement),
            (KeyChord::plain(Key::R), FocusArea::Arrangement),
            (KeyChord::plain(Key::L), FocusArea::Arrangement),
            (KeyChord::plain(Key::C), FocusArea::Arrangement),
            (KeyChord::plain(Key::Home), FocusArea::Arrangement),
            (KeyChord::plain(Key::Tab), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::T), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::N), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::O), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::S), FocusArea::Arrangement),
            (KeyChord::ctrl_shift(Key::E), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::Num1), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::Num2), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::Num3), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::F), FocusArea::Arrangement),
            (KeyChord::plain(Key::Plus), FocusArea::Arrangement),
            (KeyChord::plain(Key::Minus), FocusArea::Arrangement),
            (KeyChord::alt(Key::Plus), FocusArea::Arrangement),
            (KeyChord::alt(Key::Minus), FocusArea::Arrangement),
            (KeyChord::plain(Key::Z), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::OpenBracket), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::CloseBracket), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::Z), FocusArea::Arrangement),
            (KeyChord::ctrl_shift(Key::Z), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::Comma), FocusArea::Arrangement),
            (KeyChord::plain(Key::Escape), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::D), FocusArea::Arrangement),
            (KeyChord::ctrl(Key::E), FocusArea::Arrangement),
            (KeyChord::plain(Key::Num0), FocusArea::Arrangement),
            (KeyChord::plain(Key::ArrowUp), FocusArea::PianoRoll),
            (KeyChord::plain(Key::ArrowDown), FocusArea::PianoRoll),
            (KeyChord::shift(Key::ArrowUp), FocusArea::PianoRoll),
            (KeyChord::shift(Key::ArrowDown), FocusArea::PianoRoll),
            (KeyChord::plain(Key::Q), FocusArea::PianoRoll),
        ];
        for (chord, area) in global {
            assert!(
                k.lookup(chord, area).is_some(),
                "{} unbound",
                chord.label(Lang::De)
            );
        }
    }

    #[test]
    fn note_keys_map_to_notes_from_c4() {
        assert_eq!(note_for(Key::A), Some(60));
        assert_eq!(note_for(Key::K), Some(72));
        assert_eq!(note_for(Key::Y), note_for(Key::Z));
        assert_eq!(note_for(Key::R), None);
        assert_eq!(note_for(Key::L), None);
    }
}
