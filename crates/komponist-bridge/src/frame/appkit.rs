//! AppKit around the frame (macOS, #52): the plugin's container `NSView`, sizes in points,
//! activation (spike #42 Q3), "Oben halten" only while komponist is active, the frame's rect in
//! egui's coordinates, the key monitor (spike #42 Q4) and off-screen mode. The frame logic both
//! OSes share is in `mod.rs`; this module is its macOS half (`win32.rs` is the Windows one).

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;

use block2::RcBlock;
use egui::Rect;
use komponist_hosting::GuiParent;
use komponist_plugin_api::frame::FrameRect;
use komponist_plugin_api::frame::keys::{PolicyKey, PolicyMods};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSFloatingWindowLevel,
    NSNormalWindowLevel, NSRunningApplication, NSScreen, NSView, NSWindow,
    NSWindowNumberListOptions, NSWorkspace, NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::keys::{self, Stacked, window_above};
use super::{CURRENT, apply_level, is_komponist, log};

/// The frame's window.
pub(super) type Window = Retained<NSWindow>;

/// The plugin's container view.
pub(super) type Container = Retained<NSView>;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("AppKit on the main thread")
}

/// winit's window of eframe's creation context and a container `NSView` for the plugin below the
/// header, a subview of winit's view (never replace winit's content view: winit casts it). Plugins
/// size in points, so the scale is 1.
pub(super) fn attach(
    cc: &eframe::CreationContext<'_>,
    body: (f64, f64),
) -> Result<(Window, Container, f64), String> {
    let view = match cc.window_handle().map(|h| h.as_raw()) {
        // SAFETY: winit's view of this window, alive while the window is.
        Ok(RawWindowHandle::AppKit(h)) => unsafe { h.ns_view.cast::<NSView>().as_ref() },
        _ => return Err("no AppKit window".into()),
    };
    let Some(window) = view.window() else {
        return Err("the view has no window".into());
    };
    let container = NSView::initWithFrame(
        NSView::alloc(mtm()),
        NSRect::new(
            NSPoint::new(0.0, f64::from(super::metrics::HEADER)),
            NSSize::new(body.0, body.1),
        ),
    );
    view.addSubview(&container);
    Ok((window, container, 1.0))
}

/// Where the plugin attaches: the container view (no scale on Cocoa, `GuiParent::Cocoa`).
pub(super) fn parent(container: &Container, _scale: f64) -> GuiParent {
    GuiParent::Cocoa(Retained::as_ptr(container).cast_mut().cast::<c_void>())
}

/// Points per point.
pub(super) fn scale(_window: &Window) -> f64 {
    1.0
}

/// The window's content size, points.
pub(super) fn set_content_size(window: &Window, size: (f64, f64)) {
    window.setContentSize(NSSize::new(size.0, size.1));
}

/// The window's smallest content size, points.
pub(super) fn set_min_content_size(
    window: &Window,
    _ctx: Option<&egui::Context>,
    _scale: f64,
    size: (f64, f64),
) {
    window.setContentMinSize(NSSize::new(size.0, size.1));
}

/// The content size egui saw, points (`inner` is egui's inner rect).
pub(super) fn content_size(_window: &Window, inner: Rect) -> (f64, f64) {
    (f64::from(inner.width()), f64::from(inner.height()))
}

/// Places the container below the header, centred when the window is wider than the plugin.
pub(super) fn layout(window: &Window, container: &Container, body: (f64, f64), header: f64) {
    let Some(content) = window.contentView() else {
        return;
    };
    let size = content.frame().size;
    let x = ((size.width - body.0) / 2.0).max(0.0).floor();
    // winit's view is flipped: y grows downwards from the header's top.
    let y = if content.isFlipped() {
        header
    } else {
        size.height - header - body.1
    };
    container.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(body.0, body.1)));
}

/// Removes the container (after the plugin's GUI closed).
pub(super) fn remove_container(container: Container) {
    container.removeFromSuperview();
}

/// The window number (the window server's id, the same in every process; `FrameWindow`).
pub(super) fn window_id(window: &Window) -> i64 {
    window.windowNumber() as i64
}

/// The log line about an opened frame.
pub(super) fn describe(window: &Window, container: &Container) -> String {
    format!(
        "backing scale {}, plugin view {}",
        window.backingScaleFactor(),
        container
            .subviews()
            .firstObject()
            .map_or("none".into(), |v| v
                .class()
                .name()
                .to_string_lossy()
                .into_owned())
    )
}

/// The app: the bridge's parent process.
pub(super) fn app_pid() -> u32 {
    // SAFETY: plain syscall.
    unsafe { libc::getppid() as u32 }
}

/// `ActivateFrame`: after the app's `yieldActivationToApplication`, the cooperative `activate`
/// succeeds; `orderFront` alone never activates (spike #42, focus block A).
pub(super) fn activate(window: &Window) {
    NSApplication::sharedApplication(mtm()).activate();
    window.makeKeyAndOrderFront(None);
}

/// `RaiseFrame`: a visible frame comes to the front of its level without activating the bridge or
/// becoming key; whether it was visible.
pub(super) fn raise(window: &Window) -> bool {
    if !window.isVisible() {
        return false;
    }
    window.orderFrontRegardless();
    true
}

/// Hands the activation to process `pid` (the app, when a frame closes), so it can activate
/// itself under cooperative activation. Always by pid: in the app bundle the bridge has the app's
/// bundle id.
pub(super) fn yield_to(pid: u32) {
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32) {
        NSApplication::sharedApplication(mtm()).yieldActivationToApplication(&app);
    }
}

/// The parent of process `pid` (0 if unknown).
fn parent_pid(pid: u32) -> u32 {
    // SAFETY: a plain query into a zeroed struct of the size passed.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let got = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        );
        if got == size { info.pbi_ppid } else { 0 }
    }
}

/// Whether the frontmost application is komponist: the app `app_pid`, this bridge, or another
/// bridge of the app.
pub(super) fn komponist_active(app_pid: u32) -> bool {
    let Some(front) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return false;
    };
    let pid = front.processIdentifier() as u32;
    is_komponist(pid, app_pid, std::process::id(), parent_pid)
}

/// "Oben halten": the floating level while komponist is active, the normal level otherwise, so
/// the frame never floats above other apps (spike #42 risk 4).
pub(super) fn set_level(window: &Window, keep_on_top: bool, komponist_active: bool) {
    let level = if keep_on_top && komponist_active {
        NSFloatingWindowLevel
    } else {
        NSNormalWindowLevel
    };
    if window.level() != level {
        window.setLevel(level);
    }
}

/// The frame's rect in egui's screen coordinates (module docs of `komponist_plugin_api::frame`): the
/// window's top-left corner from the top of the main screen, and its content size.
pub(super) fn frame_rect(window: &Window) -> FrameRect {
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

/// The viewport's platform parts (none on macOS).
pub(super) fn viewport(builder: egui::ViewportBuilder, _offscreen: bool) -> egui::ViewportBuilder {
    builder
}

/// eframe's event loop hook: no Dock icon, no menu bar of its own, no activation by winit
/// (spike #42 Q3). It runs once: run-and-return reuses the event loop.
pub(super) fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    Some(Box::new(|builder| {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false)
            .with_activate_ignoring_other_apps(false);
    }))
}

/// The key monitor and the activation observer, once per process.
pub(super) fn install_hooks() {
    install_key_monitor();
    watch_activation();
}

/// The key monitor stays for the life of the process (no hook to renew per frame on macOS).
pub(super) fn hook_keys() {}

/// See [`hook_keys`].
pub(super) fn unhook_keys() {}

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static KEYS_INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Re-applies "Oben halten" of the open frame whenever another application becomes active
/// (`NSWorkspaceDidActivateApplicationNotification`). Installed once per process.
fn watch_activation() {
    if WATCHING.replace(true) {
        return;
    }
    let block = RcBlock::new(|_note: NonNull<NSNotification>| {
        if let Some(frame) = CURRENT.with(|c| c.borrow().upgrade()) {
            apply_level(&frame);
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

// ------------------------------------------------------------------------------------------------
// Keys: an `NSEvent` local monitor (`keys` module docs)
// ------------------------------------------------------------------------------------------------
//
// The monitor sees every key event of the process before `sendEvent:` hands it to a window.
// Mapping: Strg → ⌘ (the policy's `command`), Control is "other". Layout-independent keys by their
// key code (Space 49, Esc 53, Return 36 / keypad Enter 76, Tab 48, Backspace 51, Delete 117),
// letters by `charactersIgnoringModifiers` (so ⌘Z is the key labelled Z on QWERTZ too); function
// and arrow keys (U+F700…) are "other".

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

/// Installs the monitor (once per process; it stays for every later frame).
fn install_key_monitor() {
    if KEYS_INSTALLED.replace(true) {
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

/// Whether komponist takes `e` (`keys` module docs).
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
        return keys::key_up(&mut f, u32::from(code));
    }
    if ty != NSEventType::KeyDown || f.closing {
        return false;
    }
    let Some(native) = f.native.clone() else {
        return false;
    };
    let (window, Some(container)) = (native.window, native.container) else {
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
    keys::plugin_key_down(&mut f, u32::from(code), key, mods)
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
struct CgRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

extern "C" fn unconstrained(
    _this: *mut c_void,
    _sel: *mut c_void,
    rect: CgRect,
    _screen: *mut c_void,
) -> CgRect {
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
    use komponist_plugin_api::frame::FrameChord;
    use komponist_plugin_api::frame::keys::decide;

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
}
