//! Which keys the frame takes from a focused plugin (WP-50's policy, `komponist_ipc::frame::keys`)
//! on an `NSEvent` local monitor (spike #42 Q4).
//!
//! The monitor sees every key event of the process before `sendEvent:` hands it to a window, so it
//! decides synchronously on the main thread (no grab thread as on X11):
//!
//! - **Another window of the bridge is above the frame** (a JUCE popup menu, a plugin dialog): every
//!   key goes on unchanged. JUCE keeps the keyboard focus on its editor view while its menu is
//!   open, so without this rule Esc would close the frame instead of the menu (focus block C).
//!   Windows that ignore the mouse (tooltips) do not count.
//! - **The event is for another window**: unchanged.
//! - **The first responder is inside the plugin's container**: the policy with the typing
//!   heuristic; a taken key goes to the app as `Shortcut` and is swallowed, its key-up too.
//! - **Otherwise the header has the focus**: unchanged; egui gets it and the frame forwards it as
//!   `HeaderKey` (the app applies it as on Linux).
//!
//! Mapping: Strg → ⌘ (the policy's `command`), Control is "other". Layout-independent keys by
//! their key code (Space 49, Esc 53, Return 36 / keypad Enter 76, Tab 48, Backspace 51, Delete
//! 117), letters by `charactersIgnoringModifiers` (so ⌘Z is the key labelled Z on QWERTZ too);
//! function and arrow keys (U+F700…) are "other".

use std::cell::Cell;
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use komponist_ipc::BridgeMsg;
use komponist_ipc::frame::keys::{PolicyKey, PolicyMods, decide, kind};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSView, NSWindow,
    NSWindowNumberListOptions,
};

use super::{CURRENT, log};

/// The policy's view of a key: by key code where the layout does not matter, else by the first
/// character of `charactersIgnoringModifiers`.
pub(super) fn policy_key(key_code: u16, chars: &str) -> PolicyKey {
    match key_code {
        49 => PolicyKey::Space,
        53 => PolicyKey::Escape,
        36 | 76 => PolicyKey::Return,
        48 => PolicyKey::Tab,
        51 => PolicyKey::Backspace,
        117 => PolicyKey::Delete,
        _ => match chars.chars().next() {
            // Function keys and arrows come as private-use characters (U+F700…).
            Some(c) if !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c) => {
                PolicyKey::Char(c.to_lowercase().next().unwrap_or(c))
            }
            _ => PolicyKey::Other,
        },
    }
}

/// The policy's modifiers: ⌘ is the shortcut modifier, Control counts as "other".
pub(super) fn policy_mods(flags: NSEventModifierFlags) -> PolicyMods {
    PolicyMods {
        shift: flags.contains(NSEventModifierFlags::Shift),
        command: flags.contains(NSEventModifierFlags::Command),
        alt: flags.contains(NSEventModifierFlags::Option),
        other: flags.contains(NSEventModifierFlags::Control),
    }
}

/// One window of the process, front to back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stacked {
    /// `windowNumber`.
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

/// The process's visible windows, front to back.
fn stacked(mtm: MainThreadMarker) -> Vec<Stacked> {
    let app = NSApplication::sharedApplication(mtm);
    let Some(numbers) = NSWindow::windowNumbersWithOptions(NSWindowNumberListOptions(0), mtm)
    else {
        return Vec::new();
    };
    numbers
        .iter()
        .map(|n| {
            let number = n.integerValue();
            Stacked {
                number,
                ignores_mouse: app
                    .windowWithWindowNumber(number)
                    .is_some_and(|w| w.ignoresMouseEvents()),
            }
        })
        .collect()
}

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Installs the monitor (once per process; it stays for every later frame).
pub(super) fn install() {
    if INSTALLED.replace(true) {
        return;
    }
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a live event for the duration of the call.
        let e = unsafe { event.as_ref() };
        if swallow(e) {
            std::ptr::null_mut()
        } else {
            event.as_ptr()
        }
    });
    let mask = NSEventMask::KeyDown | NSEventMask::KeyUp;
    // SAFETY: the block returns the event or nil, as the monitor requires.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
    // The monitor lives as long as the process.
    std::mem::forget(monitor);
}

/// Whether komponist takes `e` (module docs).
fn swallow(e: &NSEvent) -> bool {
    let Some(frame) = CURRENT.with(|c| c.borrow().upgrade()) else {
        return false;
    };
    let Ok(mut f) = frame.try_borrow_mut() else {
        return false;
    };
    let code = e.keyCode();
    let ty = e.r#type();
    if ty == NSEventType::KeyUp {
        return match f.taken.iter().position(|c| *c == code) {
            Some(i) => {
                f.taken.swap_remove(i);
                true
            }
            None => false,
        };
    }
    if ty != NSEventType::KeyDown || f.closing {
        return false;
    }
    let (Some(window), Some(container)) = (f.window.clone(), f.container.clone()) else {
        return false;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let ours = e
        .window(mtm)
        .is_some_and(|w| Retained::as_ptr(&w) == Retained::as_ptr(&window));
    if !ours {
        return false;
    }
    if window_above(window.windowNumber(), &stacked(mtm)) {
        log(format!(
            "key {code}: another window is above the frame, passed"
        ));
        return false;
    }
    let in_plugin = window.firstResponder().is_some_and(|r| {
        r.downcast_ref::<NSView>()
            .is_some_and(|v| v.isDescendantOf(&container))
    });
    if !in_plugin {
        return false;
    }
    let chars = e
        .charactersIgnoringModifiers()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let key = policy_key(code, &chars);
    let mods = policy_mods(e.modifierFlags());
    let now = Instant::now();
    let verdict = decide(key, mods, f.typing.active(now));
    log(format!(
        "key {key:?} {mods:?} plugin focused, typing {} -> {verdict:?}",
        f.typing.active(now)
    ));
    match verdict {
        Some(chord) => {
            f.taken.push(code);
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

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_ipc::frame::FrameChord;

    #[test]
    fn keys_map_by_code_or_character() {
        assert_eq!(policy_key(49, " "), PolicyKey::Space);
        assert_eq!(policy_key(53, "\u{1b}"), PolicyKey::Escape);
        assert_eq!(policy_key(36, "\r"), PolicyKey::Return);
        assert_eq!(policy_key(76, "\u{3}"), PolicyKey::Return);
        assert_eq!(policy_key(48, "\t"), PolicyKey::Tab);
        assert_eq!(policy_key(51, "\u{7f}"), PolicyKey::Backspace);
        assert_eq!(policy_key(117, "\u{f728}"), PolicyKey::Delete);
        // QWERTZ: the key labelled Z sits where QWERTY has Y (key code 16).
        assert_eq!(policy_key(16, "z"), PolicyKey::Char('z'));
        assert_eq!(policy_key(6, "Z"), PolicyKey::Char('z'));
        assert_eq!(policy_key(1, "s"), PolicyKey::Char('s'));
        assert_eq!(policy_key(41, "ö"), PolicyKey::Char('ö'));
        // Arrows and function keys.
        assert_eq!(policy_key(123, "\u{f702}"), PolicyKey::Other);
        assert_eq!(policy_key(122, "\u{f704}"), PolicyKey::Other);
        assert_eq!(policy_key(0, ""), PolicyKey::Other);
    }

    #[test]
    fn strg_is_command_and_control_is_other() {
        let cmd_shift = policy_mods(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
        assert_eq!(
            cmd_shift,
            PolicyMods {
                shift: true,
                command: true,
                alt: false,
                other: false
            }
        );
        assert_eq!(
            decide(policy_key(6, "Z"), cmd_shift, false),
            Some(FrameChord::command_shift("Z"))
        );
        let ctrl = policy_mods(NSEventModifierFlags::Control);
        assert!(ctrl.other && !ctrl.command);
        // ⌃S is not ⌘S; Caps Lock does not count.
        assert_eq!(decide(policy_key(1, "s"), ctrl, false), None);
        let caps = policy_mods(NSEventModifierFlags::CapsLock);
        assert_eq!(caps, PolicyMods::default());
        assert_eq!(
            decide(policy_key(53, "\u{1b}"), caps, false),
            Some(FrameChord::plain("Escape"))
        );
    }

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
