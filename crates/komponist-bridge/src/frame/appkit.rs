//! AppKit around the frame: activation (spike #42 Q3), "Oben halten" only while komponist is
//! active, the frame's rect in egui's coordinates, and off-screen mode.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;

use block2::RcBlock;
use komponist_ipc::frame::FrameRect;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSFloatingWindowLevel, NSNormalWindowLevel, NSRunningApplication, NSScreen,
    NSWindow, NSWorkspace, NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::NSNotification;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit on the main thread")
}

/// `ActivateFrame`: after the app's `yieldActivationToApplication`, the cooperative `activate`
/// succeeds; `orderFront` alone never activates (spike #42, focus block A).
pub(super) fn activate(window: &NSWindow) {
    NSApplication::sharedApplication(mtm()).activate();
    window.makeKeyAndOrderFront(None);
}

/// Hands the activation to process `pid` (the app, when a frame closes), so it can activate
/// itself under cooperative activation. Always by pid: in the app bundle the bridge has the app's
/// bundle id.
pub(super) fn yield_to(pid: i32) {
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        NSApplication::sharedApplication(mtm()).yieldActivationToApplication(&app);
    }
}

/// The parent of process `pid` (0 if unknown).
fn parent_pid(pid: i32) -> i32 {
    // SAFETY: a plain query into a zeroed struct of the size passed.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let got = libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size);
        if got == size { info.pbi_ppid as i32 } else { 0 }
    }
}

/// Whether the frontmost application is komponist: the app `app_pid`, this bridge, or another
/// bridge of the app.
pub(super) fn komponist_active(app_pid: i32) -> bool {
    let Some(front) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return false;
    };
    let pid = front.processIdentifier();
    is_komponist(pid, app_pid, std::process::id() as i32, parent_pid)
}

/// [`komponist_active`] for frontmost process `pid`, with `parent` to look up a parent pid.
pub(super) fn is_komponist(
    pid: i32,
    app_pid: i32,
    own_pid: i32,
    parent: impl Fn(i32) -> i32,
) -> bool {
    pid == app_pid || pid == own_pid || (app_pid > 1 && parent(pid) == app_pid)
}

/// "Oben halten": the floating level while komponist is active, the normal level otherwise, so
/// the frame never floats above other apps (spike #42 risk 4).
pub(super) fn set_floating(window: &NSWindow, floating: bool) {
    let level = if floating {
        NSFloatingWindowLevel
    } else {
        NSNormalWindowLevel
    };
    if window.level() != level {
        window.setLevel(level);
    }
}

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Re-applies "Oben halten" of the open frame whenever another application becomes active
/// (`NSWorkspaceDidActivateApplicationNotification`). Installed once per process.
pub(super) fn watch_activation() {
    if WATCHING.replace(true) {
        return;
    }
    let block = RcBlock::new(|_note: NonNull<NSNotification>| {
        if let Some(frame) = super::CURRENT.with(|c| c.borrow().upgrade())
            && let Ok(f) = frame.try_borrow()
        {
            f.apply_level();
        }
    });
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: the notification name is a valid static; the block lives as long as the observer.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidActivateApplicationNotification),
            None,
            None,
            &block,
        )
    };
    // The observer stays for the life of the process.
    std::mem::forget(observer);
}

/// The frame's rect in egui's screen coordinates (module docs of `komponist_ipc::frame`): the
/// window's top-left corner from the top of the main screen, and its content size.
pub(super) fn frame_rect(window: &NSWindow) -> FrameRect {
    let frame = window.frame();
    let content = window.contentRectForFrameRect(frame);
    let screens = NSScreen::screens(mtm());
    let top = screens.firstObject().map_or(0.0, |s| s.frame().size.height);
    FrameRect {
        x: frame.origin.x as f32,
        y: (top - (frame.origin.y + frame.size.height)) as f32,
        width: content.size.width as f32,
        height: content.size.height as f32,
    }
}

// ------------------------------------------------------------------------------------------------
// Off-screen mode (`KOMPONIST_FRAME_OFFSCREEN=1`)
// ------------------------------------------------------------------------------------------------

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn class_getInstanceMethod(cls: *mut c_void, sel: *mut c_void) -> *mut c_void;
    fn method_getTypeEncoding(m: *mut c_void) -> *const c_char;
    fn class_replaceMethod(
        cls: *mut c_void,
        sel: *mut c_void,
        imp: *const c_void,
        types: *const c_char,
    ) -> *const c_void;
}

extern "C" fn no_activate(_this: *mut c_void, _sel: *mut c_void) {}
extern "C" fn no_activate_flag(_this: *mut c_void, _sel: *mut c_void, _flag: bool) {}
extern "C" fn no_activate_options(_this: *mut c_void, _sel: *mut c_void, _options: usize) -> bool {
    false
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

extern "C" fn unconstrained(
    _this: *mut c_void,
    _sel: *mut c_void,
    rect: Rect,
    _screen: *mut c_void,
) -> Rect {
    rect
}

/// Replaces the implementation of `selector` in `class` (off-screen mode only).
///
/// # Safety
/// `imp` must have the method's signature.
unsafe fn replace(class: &CStr, selector: &CStr, imp: *const c_void) {
    // SAFETY: Objective-C runtime calls with valid C strings; the caller vouches for `imp`.
    unsafe {
        let cls = objc_getClass(class.as_ptr());
        if cls.is_null() {
            return;
        }
        let sel = sel_registerName(selector.as_ptr());
        let method = class_getInstanceMethod(cls, sel);
        if method.is_null() {
            return;
        }
        class_replaceMethod(cls, sel, imp, method_getTypeEncoding(method));
    }
}

/// Off-screen mode: this process can never activate itself (also when a plugin asks) and windows
/// may sit outside every screen. Installed once per process.
pub(super) fn install_offscreen_mode() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: each replacement has the replaced method's signature.
        unsafe {
            replace(c"NSApplication", c"activate", no_activate as *const c_void);
            replace(
                c"NSApplication",
                c"activateIgnoringOtherApps:",
                no_activate_flag as *const c_void,
            );
            replace(
                c"NSRunningApplication",
                c"activateWithOptions:",
                no_activate_options as *const c_void,
            );
            replace(
                c"NSWindow",
                c"constrainFrameRect:toScreen:",
                unconstrained as *const c_void,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn komponist_is_the_app_this_bridge_or_a_sibling_bridge() {
        let parent = |pid: i32| if pid == 300 { 100 } else { 1 };
        assert!(is_komponist(100, 100, 200, parent));
        assert!(is_komponist(200, 100, 200, parent));
        assert!(is_komponist(300, 100, 200, parent));
        assert!(!is_komponist(400, 100, 200, parent));
        // Without a real parent (launchd), no stranger counts as a sibling.
        assert!(!is_komponist(400, 1, 200, |_| 1));
    }
}
