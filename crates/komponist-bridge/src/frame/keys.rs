//! Which keys the frame takes from a focused plugin (WP-50's policy, `komponist_plugin_api::frame::keys`):
//! the decision both OS layers share. Each OS inspects key events before the plugin gets them and
//! asks here: macOS with an `NSEvent` local monitor (spike #42 Q4, `appkit.rs`), Windows with a
//! `WH_GETMESSAGE` hook on the main thread (`win32.rs`). Both decide synchronously on the main thread
//! (no grab thread as on X11):
//!
//! - **Another window of the bridge is above the frame** (a JUCE popup menu, a plugin dialog): every
//!   key goes on unchanged. JUCE keeps the keyboard focus on its editor view while its menu is
//!   open, so without this rule Esc would close the frame instead of the menu (focus block C).
//!   Windows that ignore the mouse (tooltips) do not count ([`window_above`]).
//! - **The event is for another window**: unchanged.
//! - **The keyboard focus is inside the plugin's container**: the policy with the typing
//!   heuristic; a taken key goes to the app as `Shortcut` and is swallowed, its key-up too
//!   ([`plugin_key_down`], [`key_up`]).
//! - **Otherwise the header has the focus**: unchanged; egui gets it and the frame forwards it as
//!   `HeaderKey` (the app applies it as on Linux).
//!
//! Each OS maps its keys to the policy's: Strg is the shortcut modifier (⌘ on macOS, Ctrl on
//! Windows as on Linux), layout-independent keys by their key code, letters by the layout's
//! character (so Strg Z is the key labelled Z on QWERTZ too).

use std::time::Instant;

use komponist_ipc::BridgeMsg;
use komponist_plugin_api::frame::keys::{PolicyKey, PolicyMods, decide, kind};

use super::{Frame, log};

/// One window of the process, front to back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stacked {
    /// The window's id (`windowNumber` on macOS, the `HWND` on Windows).
    pub(super) number: isize,
    /// It ignores the mouse (a tooltip).
    pub(super) ignores_mouse: bool,
}

/// The Esc rule (module docs): whether a window of the bridge other than `frame` lies above it.
/// `front_to_back` lists the process's visible windows in z-order.
pub(super) fn window_above(frame: isize, front_to_back: &[Stacked]) -> bool {
    front_to_back
        .iter()
        .take_while(|w| w.number != frame)
        .any(|w| !w.ignores_mouse)
        && front_to_back.iter().any(|w| w.number == frame)
}

/// A key-down for the focused plugin, key code `code` (the OS's): whether komponist takes it.
/// A taken key goes to the app as `Shortcut`, and its key-up is swallowed too ([`key_up`]).
pub(super) fn plugin_key_down(f: &mut Frame, code: u32, key: PolicyKey, mods: PolicyMods) -> bool {
    let now = Instant::now();
    let verdict = decide(key, mods, f.typing.active(now));
    log(format!(
        "key {key:?} {mods:?} plugin focused, typing {} -> {verdict:?}",
        f.typing.active(now)
    ));
    match verdict {
        Some(chord) => {
            // Once per key: auto-repeat sends more key-downs than key-ups, and a key-up left in
            // the list would swallow a later key-up the plugin needs.
            if !f.taken.contains(&code) {
                f.taken.push(code);
            }
            f.send(&BridgeMsg::Shortcut {
                chord,
                from_plugin: true,
            });
            true
        }
        None => {
            f.typing.passed(kind(key, mods), now);
            false
        }
    }
}

/// A key-up: swallowed if komponist took its key-down.
pub(super) fn key_up(f: &mut Frame, code: u32) -> bool {
    match f.taken.iter().position(|c| *c == code) {
        Some(i) => {
            f.taken.swap_remove(i);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(number: isize, ignores_mouse: bool) -> Stacked {
        Stacked {
            number,
            ignores_mouse,
        }
    }

    #[test]
    fn the_esc_rule_passes_keys_while_a_plugin_window_is_above_the_frame() {
        // Only the frame.
        assert!(!window_above(7, &[w(7, false)]));
        // A JUCE popup menu (or a dialog) in front of the frame.
        assert!(window_above(7, &[w(9, false), w(7, false)]));
        // A tooltip in front does not count.
        assert!(!window_above(7, &[w(9, true), w(7, false)]));
        assert!(window_above(7, &[w(9, true), w(8, false), w(7, false)]));
        // A plugin window behind the frame does not count.
        assert!(!window_above(7, &[w(7, false), w(9, false)]));
        // The frame not listed (hidden): nothing is above it.
        assert!(!window_above(7, &[w(9, false)]));
        assert!(!window_above(7, &[]));
    }
}
