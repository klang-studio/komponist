//! Activation between the app and a bridge's own frame window (macOS, #52; spike #42 Q3).
//!
//! macOS 14 activates cooperatively: an app comes to the front only when the active app yields to
//! it. To bring a frame forward the app yields to the bridge's process (by pid: in the app bundle
//! the bridge has the app's bundle id) and sends `ActivateFrame`; the bridge then activates and
//! orders its window front. When a frame closes the bridge yields back, and the app activates
//! itself if the bridge was the active app. Elsewhere only `ActivateFrame` is sent (tests).

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
    #[cfg(not(target_os = "macos"))]
    let _ = bridge;
}

fn yield_to(pid: u32) {
    #[cfg(target_os = "macos")]
    macos::yield_to(pid);
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSRunningApplication, NSWorkspace};

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
