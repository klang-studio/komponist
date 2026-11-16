//! Activation between the app and a bridge's own frame window (macOS, #52; spike #42 Q3).
//!
//! macOS 14 activates cooperatively: an app comes to the front only when the active app yields to
//! it. To bring a frame forward the app yields to the bridge's process (by pid: in the app bundle
//! the bridge has the app's bundle id) and sends `ActivateFrame`; the bridge then activates and
//! orders its window front. When a frame closes the bridge yields back, and the app activates
//! itself if the bridge was the active app. Elsewhere only `ActivateFrame` is sent (tests).
//!
//! Bridge frames belong to other processes, so the Dock icon, ⌘-Tab or a click on the main window
//! bring only the app's own windows forward. When the app becomes active
//! (`NSApplicationDidBecomeActiveNotification`, [`became_active`]) every open bridge frame gets
//! `RaiseFrame`, back to front ([`front_to_back`]), and orders itself front without taking the
//! activation or the keyboard.
//!
//! A frame picked in a menu (#95, `App::focus_plugin_window`) gets the same yield +
//! `ActivateFrame`; from the Dock menu the app may not be active, and an inactive app cannot yield,
//! so it asks for its own activation first ([`activate`]) and yields once it is active.

use komponist_model::DeviceId;

use crate::device_host::DeviceHost;

/// Brings `device`'s bridge frame to the front.
pub(super) fn bring_to_front(host: &mut dyn DeviceHost, device: DeviceId) {
    if let Some(pid) = host.bridge_pid(device) {
        yield_to(pid);
    }
    host.activate_frame(device);
}

/// After the bridge `bridge` closed its frame (and yielded back): the app becomes active if the
/// bridge was the active app.
pub(super) fn take_back(bridge: Option<u32>) {
    #[cfg(target_os = "macos")]
    if let Some(pid) = bridge {
        macos::activate_if_front(pid);
    }
    #[cfg(any(x11_embed, windows))]
    let _ = bridge;
}

/// Whether the app became active since the last call (macOS; the observer is installed on the
/// first call and wakes `ctx`). Always `false` elsewhere.
pub(super) fn became_active(ctx: &egui::Context) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::watch(ctx);
        macos::BECAME_ACTIVE.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    #[cfg(any(x11_embed, windows))]
    {
        let _ = ctx;
        false
    }
}

/// The window numbers of all applications' windows, front to back (macOS; empty elsewhere).
pub(super) fn front_to_back() -> Vec<i64> {
    #[cfg(target_os = "macos")]
    return macos::front_to_back();
    #[cfg(any(x11_embed, windows))]
    Vec::new()
}

/// Whether the app is the active application (macOS; always `true` elsewhere).
pub(super) fn is_active() -> bool {
    #[cfg(target_os = "macos")]
    return macos::is_active();
    #[cfg(any(x11_embed, windows))]
    true
}

/// Asks macOS to make the app active (a pick in the Dock menu while another app is active, #95);
/// [`became_active`] reports when it is. No-op elsewhere.
pub(super) fn activate() {
    #[cfg(target_os = "macos")]
    macos::activate();
}

fn yield_to(pid: u32) {
    #[cfg(target_os = "macos")]
    macos::yield_to(pid);
    #[cfg(any(x11_embed, windows))]
    let _ = pid;
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationDidBecomeActiveNotification, NSRunningApplication, NSWindow,
        NSWindowNumberListOptions, NSWorkspace,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter};

    /// Set by the observer, taken by [`super::became_active`].
    pub(super) static BECAME_ACTIVE: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static WATCHING: Cell<bool> = const { Cell::new(false) };
    }

    /// Installs the observer of `NSApplicationDidBecomeActiveNotification` once; it marks the
    /// activation and wakes `ctx`, whose next pass raises the frames.
    pub(super) fn watch(ctx: &egui::Context) {
        if WATCHING.replace(true) {
            return;
        }
        let ctx = ctx.clone();
        let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
            BECAME_ACTIVE.store(true, Ordering::Release);
            ctx.request_repaint();
        });
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: a valid static notification name; the block lives as long as the observer,
        // which stays for the life of the process.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidBecomeActiveNotification),
                None,
                None,
                &block,
            )
        };
        std::mem::forget(observer);
    }

    /// All applications' window numbers, front to back.
    pub(super) fn front_to_back() -> Vec<i64> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Vec::new();
        };
        NSWindow::windowNumbersWithOptions(NSWindowNumberListOptions::AllApplications, mtm)
            .map(|numbers| numbers.iter().map(|n| n.integerValue() as i64).collect())
            .unwrap_or_default()
    }

    /// The app's own activation goes to process `pid`.
    pub(super) fn yield_to(pid: u32) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            NSApplication::sharedApplication(mtm).yieldActivationToApplication(&app);
        }
    }

    /// Whether the app is active (`true` off the main thread: nothing to do there).
    pub(super) fn is_active() -> bool {
        MainThreadMarker::new().is_none_or(|mtm| NSApplication::sharedApplication(mtm).isActive())
    }

    /// Asks for the app's activation.
    pub(super) fn activate() {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).activate();
        }
    }

    /// Activates the app if process `pid` is the frontmost application.
    pub(super) fn activate_if_front(pid: u32) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let front = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|a| a.processIdentifier());
        if front.is_some_and(|f| i64::from(f) == i64::from(pid)) {
            NSApplication::sharedApplication(mtm).activate();
        }
    }
}

// Windows (W4b): everything above is a no-op there, because frames never open while
// `komponist_bridge::GUI_EMBEDDING` is false. This is W6's seam: `yield_to` gets
// `AllowSetForegroundWindow(pid)` and `bring_to_front` a `SetForegroundWindow` on the frame;
// `take_back`, `became_active`, `front_to_back` and `is_active` keep their defaults unless W6
// needs them. Linux and Windows share the `not(macos)` arms (Linux: `x11_embed`).
