//! Win32 around the frame (Windows, port W6): the plugin's container `HWND`, sizes in physical
//! pixels with the window's DPI scale, activation, "Oben halten" only while komponist is active,
//! the frame's rect in egui's coordinates, the key hook and off-screen mode. The frame logic both
//! OSes share is in `mod.rs`; this module is its Windows half (`appkit.rs` is the macOS one).
//!
//! - **Container:** a child window of winit's window below the header (`WS_CHILD`,
//!   `WS_CLIPCHILDREN`, background `surface.0`), the plugin's parent (CLAP `set_parent` win32,
//!   VST3 `attached` with `kPlatformTypeHWND`). The plugin's windows are children of it, in the
//!   bridge's process and thread, so a hung plugin blocks only its bridge. winit's window clips
//!   its children, so its swapchain never paints over the plugin.
//! - **Sizes:** physical pixels; the frame's scale is `GetDpiForWindow / 96` (winit makes the
//!   process per-monitor-v2 DPI aware, and `komponist.exe`'s manifest says so too). The window's
//!   size is set with `SetWindowPos` for the outer rect `AdjustWindowRectExForDpi` gives for the
//!   wanted client rect.
//! - **Activation:** Windows' foreground lock replaces macOS' cooperative activation: the app calls
//!   `AllowSetForegroundWindow(bridge pid)` before `ActivateFrame`, and the bridge then calls
//!   `SetForegroundWindow`. When a frame closes, the bridge allows the app in turn.
//!   `RaiseFrame` puts the frame on top of the z-order without activating it
//!   (`SetWindowPos(HWND_TOP, SWP_NOACTIVATE)`).
//! - **"Oben halten":** `HWND_TOPMOST` while komponist (the app or one of its bridges) is the
//!   foreground process, watched with a `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` hook; when
//!   another application comes to the front, the frame leaves the topmost band and goes right
//!   below that application's window (`HWND_NOTOPMOST` alone would keep it above it).
//! - **Keys:** a `WH_GETMESSAGE` hook on the bridge's main thread sees every key message any
//!   message loop (eframe's, a modal one) takes from the queue, before `TranslateMessage` and
//!   `DispatchMessage`, so a key for the plugin's windows is decided before the plugin sees it
//!   (`keys` module docs); a taken one becomes `WM_NULL`. It is installed after the plugin's GUI
//!   opened: plugins hook the thread too (baseview, under the test plugins' egui GUIs, takes key
//!   messages for its windows in its own `WH_GETMESSAGE` hook, so winit's message hook never sees
//!   them; JUCE has such a hook as well), and hooks run newest first. Mapping: Strg (Ctrl) is the
//!   policy's `command` as on Linux, the Windows key is "other", AltGr (Ctrl + Alt) is no
//!   shortcut; Space, Esc, Return (and keypad Enter), Tab, Backspace and Delete by their
//!   virtual-key code, letters by the layout's character (`MapVirtualKeyW`). "Another window
//!   above the frame": a visible top-level window of this process above the frame in the z-order;
//!   tooltips (`WS_EX_TRANSPARENT`, `tooltips_class32`) do not count.
//! - **Off-screen mode:** the frame opens at −20000 points without a taskbar button, and neither
//!   `ActivateFrame` nor the yield activate anything.

use std::cell::Cell;
use std::ffi::c_void;

use egui::{Rect, ViewportCommand, vec2};
use komponist_hosting::GuiParent;
use komponist_ipc::frame::FrameRect;
use komponist_ipc::frame::keys::{PolicyKey, PolicyMods};
use komponist_tokens::color;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{
    CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW, VK_BACK, VK_CONTROL, VK_DELETE, VK_ESCAPE,
    VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
    EVENT_SYSTEM_FOREGROUND, GW_CHILD, GW_HWNDNEXT, GWL_EXSTYLE, GWL_STYLE, GetClassNameW,
    GetClientRect, GetForegroundWindow, GetTopWindow, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HC_ACTION, HHOOK, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, IsChild,
    IsIconic, IsWindowVisible, MSG, PM_REMOVE, RegisterClassExW, SW_RESTORE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
    SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx, WH_GETMESSAGE, WINEVENT_OUTOFCONTEXT,
    WM_KEYDOWN, WM_KEYUP, WM_NULL, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_VISIBLE,
};

use super::keys::{self, Stacked, window_above};
use super::{CURRENT, apply_level, is_komponist, log, offscreen};

/// A window handle of this thread (`Clone` only, as the macOS handles: the shared code clones it
/// out of a borrow).
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Hwnd(pub(super) HWND);

/// The frame's window (winit's).
pub(super) type Window = Hwnd;

/// The plugin's container window.
pub(super) type Container = Hwnd;

/// Windows' 100 % DPI.
const BASE_DPI: f64 = 96.0;

/// Most top-level windows looked at in one z-order walk (a guard, never reached in practice).
const MAX_WINDOWS: usize = 4096;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// winit's window of eframe's creation context, its DPI scale, and a container child window for
/// the plugin below the header, `body` points large.
pub(super) fn attach(
    cc: &eframe::CreationContext<'_>,
    body: (f64, f64),
) -> Result<(Window, Container, f64), String> {
    let window = match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get() as HWND,
        _ => return Err("no Win32 window".into()),
    };
    let scale = scale(&Hwnd(window));
    let header = (f64::from(super::metrics::HEADER) * scale).round();
    let class = wide("komponist-plugin-container");
    // SAFETY: the module handle of this executable.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(DefWindowProcW),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        // The body below the plugin: `surface.0`, as egui paints it.
        hbrBackground: background(),
        // SAFETY: all-zero is valid for the rest of this plain C struct.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a valid class description; a second registration (the next frame) fails harmlessly.
    unsafe { RegisterClassExW(&wc) };
    // SAFETY: a child of winit's live window, of a registered class.
    let container = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            header as i32,
            (body.0 * scale).round() as i32,
            (body.1 * scale).round() as i32,
            window,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if container.is_null() {
        return Err(format!(
            "container window: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok((Hwnd(window), Hwnd(container), scale))
}

/// The container's background brush (`surface.0`), created once.
fn background() -> HBRUSH {
    thread_local! {
        static BRUSH: Cell<HBRUSH> = const { Cell::new(std::ptr::null_mut()) };
    }
    BRUSH.with(|b| {
        if b.get().is_null() {
            let [r, g, bl, _] = color::SURFACE_0;
            // SAFETY: a plain GDI object, kept for the life of the process.
            b.set(unsafe {
                CreateSolidBrush(u32::from(r) | (u32::from(g) << 8) | (u32::from(bl) << 16))
            });
        }
        b.get()
    })
}

/// Where the plugin attaches: the container at the window's scale (`GuiParent::Win32`).
pub(super) fn parent(container: &Container, scale: f64) -> GuiParent {
    GuiParent::Win32 {
        hwnd: container.0.cast::<c_void>(),
        scale,
    }
}

/// Physical pixels per point: the window's DPI / 96.
pub(super) fn scale(window: &Window) -> f64 {
    // SAFETY: a plain query; 0 for a window that is gone.
    let dpi = unsafe { GetDpiForWindow(window.0) };
    if dpi == 0 {
        1.0
    } else {
        f64::from(dpi) / BASE_DPI
    }
}

/// The window's client size, physical pixels.
fn client(window: HWND) -> (i32, i32) {
    let mut rect: RECT = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a valid out pointer; the rect stays zero for a window that is gone.
    unsafe { GetClientRect(window, &mut rect) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// The window's client size, physical pixels: the outer size for it from
/// `AdjustWindowRectExForDpi`.
pub(super) fn set_content_size(window: &Window, size: (f64, f64)) {
    let window = window.0;
    // SAFETY: plain queries of a live window.
    let (style, ex_style, dpi) = unsafe {
        (
            GetWindowLongPtrW(window, GWL_STYLE) as u32,
            GetWindowLongPtrW(window, GWL_EXSTYLE) as u32,
            GetDpiForWindow(window),
        )
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: size.0.ceil() as i32,
        bottom: size.1.ceil() as i32,
    };
    // SAFETY: a valid rect; the window's own styles and DPI.
    unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, ex_style, dpi) };
    // SAFETY: resizes a live window; winit hears of it through WM_SIZE.
    unsafe {
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// The window's smallest client size, physical pixels: winit's minimum inner size (winit answers
/// `WM_GETMINMAXINFO` itself), in points.
pub(super) fn set_min_content_size(
    _window: &Window,
    ctx: Option<&egui::Context>,
    scale: f64,
    size: (f64, f64),
) {
    if let Some(ctx) = ctx {
        ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(
            (size.0 / scale) as f32,
            (size.1 / scale) as f32,
        )));
    }
}

/// The window's client size, physical pixels (egui's rect is in points; the client rect is
/// exact).
pub(super) fn content_size(window: &Window, _inner: Rect) -> (f64, f64) {
    let (w, h) = client(window.0);
    (f64::from(w), f64::from(h))
}

/// Places the container below the header, centred when the window is wider than the plugin.
pub(super) fn layout(window: &Window, container: &Container, body: (f64, f64), header: f64) {
    let (width, _) = client(window.0);
    let x = ((f64::from(width) - body.0) / 2.0).max(0.0).floor();
    // SAFETY: moves a live child window.
    unsafe {
        SetWindowPos(
            container.0,
            std::ptr::null_mut(),
            x as i32,
            header as i32,
            body.0.round() as i32,
            body.1.round() as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// Removes the container (after the plugin's GUI closed; windows the plugin left in it go too).
pub(super) fn remove_container(container: Container) {
    // SAFETY: the container this thread created.
    unsafe { DestroyWindow(container.0) };
}

/// The window's id for `FrameWindow`: its `HWND` (valid in every process).
pub(super) fn window_id(window: &Window) -> i64 {
    window.0 as i64
}

/// The log line about an opened frame.
pub(super) fn describe(window: &Window, container: &Container) -> String {
    // SAFETY: a plain query.
    let plugin = unsafe { GetWindow(container.0, GW_CHILD) };
    format!(
        "window {:#x}, client {:?} px, plugin window {}",
        window.0 as usize,
        client(window.0),
        if plugin.is_null() {
            "none".to_owned()
        } else {
            class_name(plugin)
        }
    )
}

/// The window class's name of `window`.
fn class_name(window: HWND) -> String {
    let mut name = [0u16; 64];
    // SAFETY: the buffer's length is passed.
    let len = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len.max(0) as usize])
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

/// The app: the bridge's parent process.
pub(super) fn app_pid() -> u32 {
    // SAFETY: a plain query.
    parent_pid(unsafe { GetCurrentProcessId() })
}

/// `ActivateFrame`: the app allowed this process to take the foreground
/// (`AllowSetForegroundWindow`); the frame comes forward with it (not in off-screen mode).
pub(super) fn activate(window: &Window) {
    if offscreen() {
        return;
    }
    // SAFETY: plain calls on a live window of this thread.
    unsafe {
        if IsIconic(window.0) != 0 {
            ShowWindow(window.0, SW_RESTORE);
        } else {
            ShowWindow(window.0, SW_SHOW);
        }
        SetForegroundWindow(window.0);
    }
}

/// `RaiseFrame`: a visible frame goes on top of the z-order without activating; whether it was
/// visible.
pub(super) fn raise(window: &Window) -> bool {
    // SAFETY: plain calls on a live window of this thread.
    unsafe {
        if IsWindowVisible(window.0) == 0 {
            return false;
        }
        SetWindowPos(
            window.0,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    true
}

/// Allows process `pid` (the app, when a frame closes) to take the foreground. Only the foreground
/// process may allow it; otherwise this does nothing.
pub(super) fn yield_to(pid: u32) {
    if offscreen() {
        return;
    }
    // SAFETY: a plain call.
    unsafe { AllowSetForegroundWindow(pid) };
}

/// The process of the foreground window (0 without one).
fn foreground_pid() -> (HWND, u32) {
    // SAFETY: plain queries.
    unsafe {
        let window = GetForegroundWindow();
        let mut pid = 0;
        if !window.is_null() {
            GetWindowThreadProcessId(window, &mut pid);
        }
        (window, pid)
    }
}

/// Whether the foreground process is komponist: the app `app_pid`, this bridge, or another bridge
/// of the app.
pub(super) fn komponist_active(app_pid: u32) -> bool {
    let (_, pid) = foreground_pid();
    pid != 0 && is_komponist(pid, app_pid, std::process::id(), parent_pid)
}

/// "Oben halten": topmost while komponist is active (module docs); when another application is
/// in front, the frame leaves the topmost band below that application's window.
pub(super) fn set_level(window: &Window, keep_on_top: bool, komponist_active: bool) {
    let window = window.0;
    let floating = keep_on_top && komponist_active;
    // SAFETY: plain calls on a live window of this thread.
    unsafe {
        let topmost = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0;
        if floating == topmost {
            return;
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        if floating {
            SetWindowPos(window, HWND_TOPMOST, 0, 0, 0, 0, flags);
            return;
        }
        SetWindowPos(window, HWND_NOTOPMOST, 0, 0, 0, 0, flags);
        let (front, _) = foreground_pid();
        if !komponist_active && !front.is_null() && front != window {
            SetWindowPos(window, front, 0, 0, 0, 0, flags);
        }
    }
}

/// The frame's rect in egui's screen coordinates: the window's top-left corner and its client
/// size, physical pixels divided by the window's scale.
pub(super) fn frame_rect(window: &Window) -> FrameRect {
    let scale = scale(window) as f32;
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a valid out pointer.
    unsafe { GetWindowRect(window.0, &mut outer) };
    let (width, height) = client(window.0);
    FrameRect {
        x: outer.left as f32 / scale,
        y: outer.top as f32 / scale,
        width: width as f32 / scale,
        height: height as f32 / scale,
    }
}

/// The viewport's platform parts: off-screen frames get no taskbar button.
pub(super) fn viewport(builder: egui::ViewportBuilder, offscreen: bool) -> egui::ViewportBuilder {
    if offscreen {
        builder.with_taskbar(false)
    } else {
        builder
    }
}

/// eframe's event loop needs nothing of its own on Windows (the key hook is a thread hook,
/// [`hook_keys`]).
pub(super) fn event_loop_hook() -> Option<eframe::EventLoopBuilderHook> {
    None
}

/// Off-screen mode needs nothing beyond the position, the taskbar and the skipped activation.
pub(super) fn install_offscreen_mode() {}

thread_local! {
    static KEY_HOOK: Cell<HHOOK> = const { Cell::new(std::ptr::null_mut()) };
}

/// Installs the key hook (module docs, "Keys") once the plugin's GUI is open: thread hooks run
/// newest first, so it comes before a `WH_GETMESSAGE` hook the plugin installed while opening
/// (baseview and JUCE take key messages for their windows that way, before any message loop
/// sees them).
pub(super) fn hook_keys() {
    unhook_keys();
    // SAFETY: a hook on this thread only; its procedure lives as long as the process.
    let hook = unsafe {
        SetWindowsHookExW(
            WH_GETMESSAGE,
            Some(get_message),
            std::ptr::null_mut(),
            GetCurrentThreadId(),
        )
    };
    if hook.is_null() {
        log(format!("key hook: {}", std::io::Error::last_os_error()));
    }
    KEY_HOOK.with(|h| h.set(hook));
}

/// Removes the key hook (the frame's window is gone).
pub(super) fn unhook_keys() {
    let hook = KEY_HOOK.with(|h| h.replace(std::ptr::null_mut()));
    if !hook.is_null() {
        // SAFETY: the hook `hook_keys` installed on this thread.
        unsafe { UnhookWindowsHookEx(hook) };
    }
}

/// The key hook: a key message komponist takes becomes `WM_NULL` before anyone else sees it.
unsafe extern "system" fn get_message(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam == PM_REMOVE as usize && lparam != 0 {
        // SAFETY: for `HC_ACTION` the system passes the message being retrieved, which a hook
        // may change.
        let msg = unsafe { &mut *(lparam as *mut MSG) };
        if swallow(msg) {
            msg.message = WM_NULL;
            msg.hwnd = std::ptr::null_mut();
            msg.wParam = 0;
            msg.lParam = 0;
        }
    }
    // SAFETY: the next hook gets the (possibly changed) message.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

thread_local! {
    static WATCHING: Cell<HWINEVENTHOOK> = const { Cell::new(std::ptr::null_mut()) };
}

/// The foreground hook of "Oben halten", once per process (module docs).
pub(super) fn install_hooks() {
    WATCHING.with(|w| {
        if !w.get().is_null() {
            return;
        }
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
        w.set(hook);
    });
}

/// Another window came to the foreground: "Oben halten" follows.
unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if let Some(frame) = CURRENT.with(|c| c.borrow().upgrade()) {
        apply_level(&frame);
    }
}

// ------------------------------------------------------------------------------------------------
// Keys (module docs; the policy is in `keys`)
// ------------------------------------------------------------------------------------------------

/// The policy's view of virtual key `vk`: by code where the layout does not matter, else by the
/// layout's unshifted character `ch` (`MapVirtualKeyW(MAPVK_VK_TO_CHAR)`; `None` for keys without
/// one and dead keys).
pub(super) fn policy_key(vk: u16, ch: Option<char>) -> PolicyKey {
    match vk {
        VK_SPACE => PolicyKey::Space,
        VK_ESCAPE => PolicyKey::Escape,
        // Keypad Enter is VK_RETURN too (an extended key).
        VK_RETURN => PolicyKey::Return,
        VK_TAB => PolicyKey::Tab,
        VK_BACK => PolicyKey::Backspace,
        VK_DELETE => PolicyKey::Delete,
        _ => match ch {
            Some(c) if !c.is_control() => PolicyKey::Char(c.to_lowercase().next().unwrap_or(c)),
            _ => PolicyKey::Other,
        },
    }
}

/// The layout's unshifted character of `vk` (`None` for none or a dead key).
fn layout_char(vk: u16) -> Option<char> {
    // SAFETY: a plain query of the thread's keyboard layout.
    let mapped = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_CHAR) };
    // The top bit marks a dead key.
    if mapped == 0 || mapped & 0x8000_0000 != 0 {
        return None;
    }
    char::from_u32(mapped & 0xFFFF)
}

/// Which modifiers are down: Strg (Ctrl) is the shortcut modifier, the Windows key is "other".
pub(super) fn policy_mods(shift: bool, ctrl: bool, alt: bool, win: bool) -> PolicyMods {
    PolicyMods {
        shift,
        command: ctrl,
        alt,
        other: win,
    }
}

/// The modifiers at the time of the message being handled (`GetKeyState` follows the queue).
fn current_mods() -> PolicyMods {
    // SAFETY: plain queries of this thread's key state.
    let down = |vk: u16| unsafe { GetKeyState(i32::from(vk)) } < 0;
    policy_mods(
        down(VK_SHIFT),
        down(VK_CONTROL),
        down(VK_MENU),
        down(VK_LWIN) || down(VK_RWIN),
    )
}

/// This process's visible top-level windows from the top of the z-order down to `frame`.
fn stacked(frame: HWND) -> Vec<Stacked> {
    let mut out = Vec::new();
    // SAFETY: plain queries; the walk ends at the bottom of the z-order.
    unsafe {
        let own = GetCurrentProcessId();
        let mut window = GetTopWindow(std::ptr::null_mut());
        for _ in 0..MAX_WINDOWS {
            if window.is_null() {
                break;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            if pid == own && IsWindowVisible(window) != 0 {
                let ex_style = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
                out.push(Stacked {
                    number: window as isize,
                    ignores_mouse: ex_style & WS_EX_TRANSPARENT != 0
                        || class_name(window) == "tooltips_class32",
                });
            }
            if window == frame {
                break;
            }
            window = GetWindow(window, GW_HWNDNEXT);
        }
    }
    out
}

/// Whether komponist takes the key message `msg` (`keys` module docs); other messages pass.
fn swallow(msg: &MSG) -> bool {
    let down = match msg.message {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return false,
    };
    let Some(frame) = CURRENT.with(|c| c.borrow().upgrade()) else {
        return false;
    };
    let Ok(mut f) = frame.try_borrow_mut() else {
        return false;
    };
    let vk = (msg.wParam & 0xFFFF) as u16;
    if !down {
        return keys::key_up(&mut f, u32::from(vk));
    }
    if f.closing {
        return false;
    }
    let Some(native) = f.native.clone() else {
        return false;
    };
    let (Some(Hwnd(container)), Hwnd(window)) = (native.container, native.window) else {
        return false;
    };
    // SAFETY: plain queries.
    let in_plugin = msg.hwnd == container || unsafe { IsChild(container, msg.hwnd) } != 0;
    if !in_plugin {
        return false;
    }
    if window_above(window as isize, &stacked(window)) {
        log(format!(
            "key {vk:#x}: another window is above the frame, passed"
        ));
        return false;
    }
    let key = policy_key(vk, layout_char(vk));
    keys::plugin_key_down(&mut f, u32::from(vk), key, current_mods())
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_ipc::frame::FrameChord;
    use komponist_ipc::frame::keys::decide;

    #[test]
    fn keys_map_by_virtual_key_or_the_layouts_character() {
        assert_eq!(policy_key(VK_SPACE, Some(' ')), PolicyKey::Space);
        assert_eq!(policy_key(VK_ESCAPE, Some('\u{1b}')), PolicyKey::Escape);
        assert_eq!(policy_key(VK_RETURN, Some('\r')), PolicyKey::Return);
        assert_eq!(policy_key(VK_TAB, Some('\t')), PolicyKey::Tab);
        assert_eq!(policy_key(VK_BACK, Some('\u{8}')), PolicyKey::Backspace);
        assert_eq!(policy_key(VK_DELETE, None), PolicyKey::Delete);
        // Letters by the layout: QWERTZ maps the key labelled Z to VK 'Z' ('Z' upper case).
        assert_eq!(policy_key(0x5A, Some('Z')), PolicyKey::Char('z'));
        assert_eq!(policy_key(0x53, Some('S')), PolicyKey::Char('s'));
        // German Ö (VK_OEM_3 on QWERTZ).
        assert_eq!(policy_key(0xC0, Some('Ö')), PolicyKey::Char('ö'));
        // Arrows, function keys, dead keys: no character.
        assert_eq!(policy_key(0x25, None), PolicyKey::Other);
        assert_eq!(policy_key(0x70, None), PolicyKey::Other);
    }

    #[test]
    fn strg_is_command_altgr_and_the_windows_key_are_not() {
        let strg_shift = policy_mods(true, true, false, false);
        assert_eq!(
            strg_shift,
            PolicyMods {
                shift: true,
                command: true,
                alt: false,
                other: false
            }
        );
        assert_eq!(
            decide(policy_key(0x5A, Some('Z')), strg_shift, false),
            Some(FrameChord::command_shift("Z"))
        );
        let strg = policy_mods(false, true, false, false);
        assert_eq!(
            decide(policy_key(0x53, Some('S')), strg, false),
            Some(FrameChord::command("S"))
        );
        // AltGr is Ctrl + Alt: AltGr Q types "@" on QWERTZ and is no shortcut.
        let altgr = policy_mods(false, true, true, false);
        assert_eq!(decide(policy_key(0x51, Some('Q')), altgr, false), None);
        assert_eq!(decide(policy_key(0x53, Some('S')), altgr, false), None);
        // The Windows key is "other": Win S is no Strg S.
        let win = policy_mods(false, false, false, true);
        assert!(win.other && !win.command);
        assert_eq!(decide(policy_key(0x53, Some('S')), win, false), None);
        // Plain Esc and Space while not typing.
        let none = policy_mods(false, false, false, false);
        assert_eq!(
            decide(policy_key(VK_ESCAPE, None), none, false),
            Some(FrameChord::plain("Escape"))
        );
        assert_eq!(
            decide(policy_key(VK_SPACE, Some(' ')), none, false),
            Some(FrameChord::plain("Space"))
        );
        // Typing in the plugin: Space goes to it, Strg S still to komponist.
        assert_eq!(decide(policy_key(VK_SPACE, Some(' ')), none, true), None);
        assert_eq!(
            decide(policy_key(0x53, Some('S')), strg, true),
            Some(FrameChord::command("S"))
        );
    }
}
