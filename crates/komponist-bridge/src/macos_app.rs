//! The bridge as a macOS process (#100): no Dock icon, no App Nap.
//!
//! **Dock icon.** In `komponist.app` the bridge is the bundle's own executable (`komponist --bridge`),
//! so when it first talks to AppKit (its plugin frame, or a plugin that sets up `NSApplication`),
//! LaunchServices checks it in with the bundle's `Info.plist`. As a regular app it was "Foreground"
//! with a Dock tile until winit switched it to the accessory policy in
//! `applicationDidFinishLaunching:`; the Dock then showed a second komponist icon that bounced until
//! it gave up (owner test of #95/#96, 2026-11-16; `lsappinfo` while a frame opened: "" →
//! "Foreground" → "UIElement"). Setting the policy at the bridge's start does not help: creating
//! `NSApplication` checks the process in as "Foreground" before any policy can be set (measured).
//! So the bundle declares `LSUIElement` (`assets/macos/Info.plist.in`): every bridge is checked in
//! as "UIElement" from the start, and the app makes itself regular when it has finished launching
//! (komponist-ui `RunOptions::native_options`). Nothing here; the frame's winit loop keeps its
//! accessory policy, and #52's yield + `ActivateFrame` works as before.
//!
//! **App Nap.** [`setup`] holds an `NSProcessInfo` activity for the life of an instance bridge
//! (latency critical, user initiated, idle system sleep allowed): an application without a visible
//! window could otherwise be put into App Nap, which delays its main thread's timers (the plugin's
//! own, the parameter coalescing). The audio thread is a real-time thread either way. The activity
//! is Foundation only: it does not check the process in with LaunchServices.

use objc2_foundation::{NSActivityOptions, NSProcessInfo, ns_string};

/// Keeps an instance bridge out of App Nap until the process ends.
pub(crate) fn setup() {
    let options = NSActivityOptions::LatencyCritical
        | NSActivityOptions::UserInitiatedAllowingIdleSystemSleep;
    let activity = NSProcessInfo::processInfo()
        .beginActivityWithOptions_reason(options, ns_string!("komponist plugin bridge"));
    std::mem::forget(activity);
}
