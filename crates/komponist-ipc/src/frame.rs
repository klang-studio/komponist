//! The plugin frame window's shared vocabulary (S05, ARCHITECTURE §8.4; #52).
//!
//! On Linux the app draws the frame (an eframe viewport with an X11 child for the plugin); on macOS
//! the bridge owns the frame window and draws the same header itself. Both sides therefore share
//! what the header shows ([`HeaderModel`]), what the user did in it ([`HeaderEvent`]), where the
//! frame is ([`FrameRect`]), the keys a frame hands to the app ([`FrameChord`], [`FrameKeyEvent`])
//! and the key policy that decides which keys komponist takes from a focused plugin ([`keys`]).
//! The header's drawing code lives in `komponist-ui` (`plugin_frame::header`).
//!
//! Keys travel by egui's key names (`egui::Key::name`, `egui::Key::from_name`: "S", "Space",
//! "Escape", …), so this crate needs no egui.

use serde::{Deserialize, Serialize};

/// What a plugin frame window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginView {
    /// The plugin's own GUI.
    Gui,
    /// The generic parameter grid ("Ansicht" toggle, plugins without GUI, hexa).
    Params,
}

/// Marks that are notation, not words: they are the same in every language. The header's texts
/// are in the catalog (`plugin-frame.ftl`).
pub mod names {
    /// The preset field without a known preset.
    pub const NO_PRESET: &str = "—";
    /// Marks a changed preset (`pluginView`: `pl.changed?' *':''`).
    pub const CHANGED: &str = " *";
}

/// The colour mark before the name (`pluginView` `pwS`, the device card's `pw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mark {
    /// Running: `state.play`.
    Active,
    /// Bypassed: `surface.4` with a 1 px `text.3` ring.
    Bypassed,
    /// Loading, crashed or missing: `surface.4`.
    Inactive,
}

/// What the frame header shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderModel {
    /// Plugin name ("Surge XT").
    pub name: String,
    /// Track and format ("Pad · CLAP").
    pub sub: String,
    /// The colour mark.
    pub mark: Mark,
    /// Whether the device is bypassed ("Bypass" on).
    pub bypassed: bool,
    /// The presets the field lists; empty: no presets (field and arrows disabled).
    pub presets: Vec<String>,
    /// Index of the current preset in `presets`.
    pub preset: Option<usize>,
    /// The current preset was changed since it was loaded (" *").
    pub preset_changed: bool,
    /// "Speichern" works.
    pub can_save: bool,
    /// The view.
    pub view: PluginView,
    /// The plugin has its own GUI ("Plugin" segment enabled).
    pub has_gui: bool,
    /// "Oben halten" on.
    pub keep_on_top: bool,
    /// The close shortcut shown in the tooltip ("Esc").
    pub close_shortcut: String,
    /// Localized status while the track's parameters can be mapped to a macro.
    #[serde(default)]
    pub mapping_chip: Option<String>,
}

impl HeaderModel {
    /// The field's entries and the selected index: the presets with " *" on a changed current
    /// one, or one disabled "—" entry without a current preset.
    pub fn preset_items(&self) -> (Vec<String>, usize) {
        let mut items = self.presets.clone();
        match self.preset.filter(|i| *i < items.len()) {
            Some(i) => {
                if self.preset_changed {
                    items[i].push_str(names::CHANGED);
                }
                (items, i)
            }
            None => {
                items.push(names::NO_PRESET.to_owned());
                let last = items.len() - 1;
                (items, last)
            }
        }
    }
}

/// What the user did in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HeaderEvent {
    /// "Bypass".
    ToggleBypass,
    /// ‹.
    PresetPrev,
    /// ›.
    PresetNext,
    /// A preset from the field.
    Preset(usize),
    /// The view switch.
    View(PluginView),
    /// "Oben halten" on or off.
    KeepOnTop(bool),
    /// ×.
    Close,
}

/// Where a frame window is: the outer top-left corner and the inner size (header and body), in
/// points, in egui's screen coordinates (origin at the top left of the main screen, as
/// `ViewportBuilder::with_position` takes it).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FrameRect {
    /// Left edge of the window, points.
    pub x: f32,
    /// Top edge of the window, points.
    pub y: f32,
    /// Inner width, points.
    pub width: f32,
    /// Inner height (header and body), points.
    pub height: f32,
}

/// Modifier keys of a [`FrameKeyEvent`], as egui's `Modifiers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct KeyMods {
    /// Alt (⌥).
    pub alt: bool,
    /// Control (Ctrl, ⌃).
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// ⌘ (macOS only).
    pub mac_cmd: bool,
    /// The shortcut modifier: Ctrl on Linux, ⌘ on macOS.
    pub command: bool,
}

/// A shortcut a frame took for komponist: a key by its egui name ("S", "Space", "Escape") and the
/// modifiers that are held (and no others), as komponist's `KeyChord`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameChord {
    /// egui key name (`egui::Key::name`).
    pub key: String,
    /// The shortcut modifier (Strg on Linux, ⌘ on macOS).
    pub command: bool,
    /// Shift.
    pub shift: bool,
    /// Alt (⌥).
    pub alt: bool,
}

impl FrameChord {
    /// `key` without modifiers.
    pub fn plain(key: &str) -> FrameChord {
        FrameChord {
            key: key.to_owned(),
            command: false,
            shift: false,
            alt: false,
        }
    }

    /// `key` with the shortcut modifier.
    pub fn command(key: &str) -> FrameChord {
        FrameChord {
            command: true,
            ..FrameChord::plain(key)
        }
    }

    /// `key` with the shortcut modifier and Shift.
    pub fn command_shift(key: &str) -> FrameChord {
        FrameChord {
            shift: true,
            ..FrameChord::command(key)
        }
    }
}

/// A key event of the frame's own (egui) part, as egui's `Event::Key`: the app applies it as a key
/// of a frame window (global shortcuts, the computer-keyboard MIDI layer, Esc closes the frame).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameKeyEvent {
    /// egui name of the logical key.
    pub key: String,
    /// egui name of the physical key, if known.
    pub physical_key: Option<String>,
    /// Pressed (`true`) or released.
    pub pressed: bool,
    /// An auto-repeat.
    pub repeat: bool,
    /// Modifiers.
    pub mods: KeyMods,
}

/// Which keys komponist takes from a plugin that has the keyboard focus (WP-50), for any
/// platform: Linux maps X keysyms to [`keys::PolicyKey`], macOS `NSEvent` key codes and
/// characters. The policy:
///
/// | Key | komponist takes it |
/// |---|---|
/// | Strg S (⌘S) | always (saving the project includes the plugin's state) |
/// | Leertaste, Esc, Strg Z (⌘Z), Strg Shift Z (⇧⌘Z) | unless the user is typing in the plugin |
/// | everything else | never: the plugin gets it |
///
/// "Typing" is a heuristic ([`keys::Typing`]): a text key passed to the plugin starts or extends a
/// typing session, Enter, Tab or Esc passed to the plugin end it, and it ends
/// [`keys::TYPING_GRACE`] after the last text key.
pub mod keys {
    use std::time::{Duration, Instant};

    use super::FrameChord;

    /// How long a typing session lasts after the last text key.
    pub const TYPING_GRACE: Duration = Duration::from_secs(5);

    /// A key as the policy sees it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum PolicyKey {
        /// A printable character (lower case for letters), from the keyboard layout.
        Char(char),
        /// Space bar.
        Space,
        /// Escape.
        Escape,
        /// Return or keypad Enter.
        Return,
        /// Tab (also Shift Tab).
        Tab,
        /// Backspace.
        Backspace,
        /// Forward delete.
        Delete,
        /// Anything else (arrows, function keys, modifiers).
        Other,
    }

    /// Modifiers of a key press, platform-neutral.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct PolicyMods {
        /// Shift.
        pub shift: bool,
        /// The shortcut modifier: Ctrl ("Strg") on Linux, ⌘ on macOS.
        pub command: bool,
        /// Alt (⌥).
        pub alt: bool,
        /// Any other modifier: Super on Linux, Control on macOS.
        pub other: bool,
    }

    impl PolicyMods {
        fn plain(self) -> bool {
            !self.shift && !self.command && !self.alt && !self.other
        }
    }

    /// What a key is, for the typing heuristic.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum KeyKind {
        /// Prints a character or edits text (Backspace, Delete).
        Text,
        /// Ends a text entry (Enter, Tab, Esc).
        Confirm,
        /// Anything else (arrows, function keys, modifiers).
        Other,
    }

    /// Classifies `key` with `mods`.
    pub fn kind(key: PolicyKey, mods: PolicyMods) -> KeyKind {
        if mods.command || mods.alt || mods.other {
            return KeyKind::Other;
        }
        match key {
            PolicyKey::Return | PolicyKey::Tab | PolicyKey::Escape => KeyKind::Confirm,
            PolicyKey::Char(_) | PolicyKey::Space | PolicyKey::Backspace | PolicyKey::Delete => {
                KeyKind::Text
            }
            PolicyKey::Other => KeyKind::Other,
        }
    }

    /// The policy (module docs) for `key` with `mods` while the plugin has the focus: the chord
    /// komponist takes, or `None` (the plugin gets the key).
    pub fn decide(key: PolicyKey, mods: PolicyMods, typing: bool) -> Option<FrameChord> {
        let command_only = mods.command && !mods.alt && !mods.other;
        match key {
            PolicyKey::Char('s') if command_only && !mods.shift => Some(FrameChord::command("S")),
            _ if typing => None,
            PolicyKey::Space if mods.plain() => Some(FrameChord::plain("Space")),
            PolicyKey::Escape if mods.plain() => Some(FrameChord::plain("Escape")),
            PolicyKey::Char('z') if command_only => Some(if mods.shift {
                FrameChord::command_shift("Z")
            } else {
                FrameChord::command("Z")
            }),
            _ => None,
        }
    }

    /// The typing heuristic of one frame (module docs).
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Typing {
        last_text: Option<Instant>,
    }

    impl Typing {
        /// Whether the user is typing in the plugin at `now`.
        pub fn active(&self, now: Instant) -> bool {
            self.last_text
                .is_some_and(|t| now.duration_since(t) < TYPING_GRACE)
        }

        /// A key of `kind` went to the plugin at `now`.
        pub fn passed(&mut self, kind: KeyKind, now: Instant) {
            match kind {
                KeyKind::Text => self.last_text = Some(now),
                KeyKind::Confirm => self.last_text = None,
                KeyKind::Other => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::keys::*;
    use super::*;

    const CMD: PolicyMods = PolicyMods {
        shift: false,
        command: true,
        alt: false,
        other: false,
    };

    #[test]
    fn the_policy_takes_save_always_and_the_rest_unless_typing() {
        let plain = PolicyMods::default();
        let cmd_shift = PolicyMods { shift: true, ..CMD };
        let s = PolicyKey::Char('s');
        let z = PolicyKey::Char('z');
        for typing in [false, true] {
            assert_eq!(decide(s, CMD, typing), Some(FrameChord::command("S")));
        }
        assert_eq!(
            decide(PolicyKey::Space, plain, false),
            Some(FrameChord::plain("Space"))
        );
        assert_eq!(decide(PolicyKey::Space, plain, true), None);
        assert_eq!(
            decide(PolicyKey::Escape, plain, false),
            Some(FrameChord::plain("Escape"))
        );
        assert_eq!(decide(PolicyKey::Escape, plain, true), None);
        assert_eq!(decide(z, CMD, false), Some(FrameChord::command("Z")));
        assert_eq!(
            decide(z, cmd_shift, false),
            Some(FrameChord::command_shift("Z"))
        );
        assert_eq!(decide(z, CMD, true), None);
        // Letters, Tab, Enter and other modifiers stay with the plugin.
        assert_eq!(decide(PolicyKey::Char('a'), plain, false), None);
        assert_eq!(decide(PolicyKey::Tab, plain, false), None);
        assert_eq!(decide(PolicyKey::Return, plain, false), None);
        assert_eq!(decide(s, PolicyMods { other: true, ..CMD }, false), None);
        assert_eq!(decide(s, PolicyMods { shift: true, ..CMD }, false), None);
        assert_eq!(
            decide(
                PolicyKey::Space,
                PolicyMods {
                    shift: true,
                    ..plain
                },
                false
            ),
            None
        );
    }

    #[test]
    fn typing_starts_with_text_and_ends_with_confirm_or_grace() {
        let t0 = Instant::now();
        let mut typing = Typing::default();
        assert!(!typing.active(t0));
        typing.passed(kind(PolicyKey::Char('p'), PolicyMods::default()), t0);
        assert!(typing.active(t0));
        assert!(!typing.active(t0 + TYPING_GRACE));
        typing.passed(kind(PolicyKey::Return, PolicyMods::default()), t0);
        assert!(!typing.active(t0));
        typing.passed(KeyKind::Text, t0);
        typing.passed(kind(PolicyKey::Other, PolicyMods::default()), t0);
        assert!(typing.active(t0));
        assert_eq!(kind(PolicyKey::Char('a'), CMD), KeyKind::Other);
    }

    #[test]
    fn preset_items_mark_changes_and_the_missing_preset() {
        let mut model = HeaderModel {
            name: "hexa".into(),
            sub: "Bass · Intern".into(),
            mark: Mark::Active,
            bypassed: false,
            presets: vec!["Init".into(), "Pad".into()],
            preset: Some(1),
            preset_changed: true,
            can_save: false,
            view: PluginView::Params,
            has_gui: false,
            keep_on_top: false,
            close_shortcut: "Esc".into(),
            mapping_chip: None,
        };
        assert_eq!(
            model.preset_items(),
            (vec!["Init".into(), "Pad *".into()], 1)
        );
        model.preset = None;
        assert_eq!(
            model.preset_items(),
            (vec!["Init".into(), "Pad".into(), "—".into()], 2)
        );
    }
}
