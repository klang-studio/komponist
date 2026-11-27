//! Activation between the app and a bridge's own frame window (macOS, #52; spike #42 Q3; Windows,
//! port W6).
//!
//! macOS 14 activates cooperatively: an app comes to the front only when the active app yields to
//! it. To bring a frame forward the app yields to the bridge's process (by pid: in the app bundle
//! the bridge has the app's bundle id) and sends `ActivateFrame`; the bridge then activates and
//! orders its window front. When a frame closes the bridge yields back, and the app activates
//! itself if the bridge was the active app. Elsewhere only `ActivateFrame` is sent (tests).
//!
//! Windows' foreground lock plays the same role: the app allows the bridge's process to take the
//! foreground (`AllowSetForegroundWindow(pid)`) before `ActivateFrame`, and the bridge calls
//! `SetForegroundWindow`; when a frame closes, the bridge allows the app, and the app brings its
//! own front window forward if the bridge still had the foreground.
//!
//! Bridge frames belong to other processes, so the Dock icon, ⌘-Tab or a click on the main window
//! (Windows: Alt-Tab, the taskbar) bring only the app's own windows forward. When the app becomes
//! active (`NSApplicationDidBecomeActiveNotification`; Windows: a foreground hook sees the app's
//! window come to the front from a process that is not komponist, [`became_active`]) every open
//! bridge frame gets `RaiseFrame`, back to front ([`front_to_back`]), and comes to the front
//! without taking the activation or the keyboard. (Windows: switching from a frame to the main
//! window does not raise the frames, so the window clicked stays in front of them.)
//!
//! A frame picked in a menu (#95, `App::focus_plugin_window`) gets the same yield +
//! `ActivateFrame`; from the Dock menu the app may not be active, and an inactive app cannot yield,
//! so it asks for its own activation first ([`activate`]) and yields once it is active.

use komponist_model::DeviceId;

use crate::device_host::PluginWindowHost;

/// Brings `device`'s bridge frame to the front.
pub(super) fn bring_to_front(host: &mut dyn PluginWindowHost, device: DeviceId) {
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
    #[cfg(windows)]
    if let Some(pid) = bridge {
        win32::activate_if_front(pid);
    }
    #[cfg(x11_embed)]
    let _ = bridge;
}

/// Whether the app became active since the last call (macOS, Windows; the observer is installed
/// on the first call and wakes `ctx`). Always `false` elsewhere.
pub(super) fn became_active(ctx: &egui::Context) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::watch(ctx);
        macos::BECAME_ACTIVE.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    #[cfg(windows)]
    {
        win32::watch(ctx);
        win32::BECAME_ACTIVE.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
    #[cfg(x11_embed)]
    {
        let _ = ctx;
        false
    }
}

/// The window numbers of all applications' windows, front to back (macOS; Windows: the top-level
/// `HWND`s, as the bridges report them in `FrameWindow`; empty elsewhere).
pub(super) fn front_to_back() -> Vec<i64> {
    #[cfg(target_os = "macos")]
    return macos::front_to_back();
    #[cfg(windows)]
    return win32::front_to_back();
    #[cfg(x11_embed)]
    Vec::new()
}

/// Whether the app is the active application (macOS; always `true` elsewhere: on Windows the
/// menus that pick a frame are the app's own, so it is active when they are used).
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
    #[cfg(windows)]
    win32::yield_to(pid);
    #[cfg(x11_embed)]
    let _ = pid;
}

#[cfg(not(any(target_os = "macos", windows, x11_embed)))]
compile_error!(
    "komponist-app: no frame activation backend (plugin_frame/activation.rs) for this OS"
);

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

#[cfg(windows)]
mod win32 {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{CloseHandle, HWND, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, EVENT_SYSTEM_FOREGROUND, GW_HWNDNEXT, GetForegroundWindow,
        GetTopWindow, GetWindow, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
        WINEVENT_OUTOFCONTEXT,
    };

    /// Most top-level windows looked at in one z-order walk (a guard).
    const MAX_WINDOWS: usize = 4096;

    /// Set by the foreground hook, taken by [`super::became_active`].
    pub(super) static BECAME_ACTIVE: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static HOOK: Cell<HWINEVENTHOOK> = const { Cell::new(std::ptr::null_mut()) };
        static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
        /// The foreground was komponist (the app or one of its bridges) at the last change.
        static WAS_KOMPONIST: Cell<bool> = const { Cell::new(false) };
    }

    /// The process of `window` (0 for none).
    fn pid_of(window: HWND) -> u32 {
        let mut pid = 0;
        if !window.is_null() {
            // SAFETY: a plain query.
            unsafe { GetWindowThreadProcessId(window, &mut pid) };
        }
        pid
    }

    /// The parent of process `pid` (0 if unknown), from a ToolHelp snapshot.
    fn parent_pid(pid: u32) -> u32 {
        // SAFETY: a snapshot of the process list, closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return 0;
        }
        // SAFETY: all-zero is valid for this plain C struct; its size is set.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut parent = 0;
        // SAFETY: a valid snapshot and entry.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32ProcessID == pid {
                parent = entry.th32ParentProcessID;
                break;
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        // SAFETY: the snapshot opened above.
        unsafe { CloseHandle(snapshot) };
        parent
    }

    /// Installs the foreground hook once; it marks the app's activation from outside komponist
    /// and wakes `ctx`, whose next pass raises the frames.
    pub(super) fn watch(ctx: &egui::Context) {
        if !HOOK.with(Cell::get).is_null() {
            return;
        }
        CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        // SAFETY: plain queries.
        let own = unsafe { GetCurrentProcessId() };
        WAS_KOMPONIST.with(|w| w.set(pid_of(unsafe { GetForegroundWindow() }) == own));
        // SAFETY: an out-of-context hook; its callback runs on this thread's message loop.
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                std::ptr::null_mut(),
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        HOOK.with(|h| h.set(hook));
    }

    /// Another window came to the foreground: the app became active if it is the app's and the
    /// one before was not komponist's.
    unsafe extern "system" fn foreground_changed(
        _hook: HWINEVENTHOOK,
        _event: u32,
        window: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        // SAFETY: a plain query.
        let own = unsafe { GetCurrentProcessId() };
        let pid = pid_of(window);
        let app = pid == own;
        let komponist = app || (pid != 0 && parent_pid(pid) == own);
        let was = WAS_KOMPONIST.with(|w| w.replace(komponist));
        if app && !was {
            BECAME_ACTIVE.store(true, Ordering::Release);
            CTX.with(|c| {
                if let Some(ctx) = c.borrow().as_ref() {
                    ctx.request_repaint();
                }
            });
        }
    }

    /// All top-level windows, front to back (`HWND`s as `FrameWindow` carries them).
    pub(super) fn front_to_back() -> Vec<i64> {
        let mut out = Vec::new();
        // SAFETY: plain queries; the walk ends at the bottom of the z-order.
        unsafe {
            let mut window = GetTopWindow(std::ptr::null_mut());
            for _ in 0..MAX_WINDOWS {
                if window.is_null() {
                    break;
                }
                out.push(window as i64);
                window = GetWindow(window, GW_HWNDNEXT);
            }
        }
        out
    }

    /// Allows process `pid` (a bridge) to take the foreground with its frame.
    pub(super) fn yield_to(pid: u32) {
        // SAFETY: a plain call; it does nothing unless this process may set the foreground.
        unsafe { AllowSetForegroundWindow(pid) };
    }

    /// The app's front window comes forward if process `pid` (a bridge whose frame closed and
    /// allowed the app) still has the foreground, or nobody has.
    pub(super) fn activate_if_front(pid: u32) {
        // SAFETY: plain queries and calls.
        unsafe {
            let front = pid_of(GetForegroundWindow());
            if front != 0 && front != pid {
                return;
            }
            let own = GetCurrentProcessId();
            let mut window = GetTopWindow(std::ptr::null_mut());
            for _ in 0..MAX_WINDOWS {
                if window.is_null() {
                    return;
                }
                if pid_of(window) == own && IsWindowVisible(window) != 0 {
                    SetForegroundWindow(window);
                    return;
                }
                window = GetWindow(window, GW_HWNDNEXT);
            }
        }
    }
}
