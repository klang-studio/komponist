//! The bridge's own plugin frame window on Windows (port W6): `OpenGui{OwnFrame}` opens an eframe
//! window with the header and a container child window the plugin attaches to, `CloseGui` closes
//! both, blocks keep being processed meanwhile, the frame can open again in the same bridge
//! (run-and-return), the key hook applies the WP-50 policy, and a bridge that dies takes its frame
//! with it.
//!
//! Every bridge runs with `KOMPONIST_FRAME_OFFSCREEN=1`: frames open at x/y = −20000 points without
//! a taskbar button and the bridge never activates, so nothing shows on the desktop or takes the
//! focus. The installed-plugin tests are `#[ignore]` (large third-party plugins):
//! `cargo test -p komponist-bridge --test it frame_win:: -- --ignored --test-threads=1`.
//!
//! kp-test-synth draws with OpenGL (nice-plug-egui on baseview), which creates a 3.2 core context
//! through `wglCreateContextAttribsARB`. A machine with only Windows' software renderer ("GDI
//! Generic", OpenGL 1.1: the GitHub Windows runner, no GPU) does not offer it, so the plugin
//! cannot open its GUI ("set_parent: Failed to set plugin window parent"); there the open test
//! checks that the bridge reports the failure without harm ([`gui_without_opengl`]), as
//! `frame_mac` does on the macOS runner (#94).

use crate::common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use komponist_ipc::frame::{FrameChord, FrameRect, HeaderModel, Mark, PluginView};
use komponist_ipc::{BlockStatus, BridgeMsg, ControlMsg, ErrorContext, GuiSize, GuiTarget};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_CHILD, GetClientRect, GetWindow, GetWindowTextW, IsWindow, PostMessageW, WM_KEYDOWN,
    WM_KEYUP,
};

/// Height of the frame header, points (`komponist_ui::plugin_frame::metrics::HEADER`).
const HEADER: f32 = 28.0;

/// Where off-screen frames open, points.
const OFFSCREEN: f32 = -20_000.0;

// Even with offscreen windows, concurrent software DX12 devices contend on CI's WARP driver.
static FRAME_STARTUP: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn offscreen_bridge() -> Bridge {
    Bridge::spawn_with(SpawnOptions {
        env: vec![("KOMPONIST_FRAME_OFFSCREEN".into(), "1".into())],
        ..SpawnOptions::default()
    })
}

fn header(name: &str) -> HeaderModel {
    HeaderModel {
        name: name.into(),
        sub: "Pad · CLAP".into(),
        mark: Mark::Active,
        bypassed: false,
        presets: Vec::new(),
        preset: None,
        preset_changed: false,
        can_save: false,
        view: PluginView::Gui,
        has_gui: true,
        keep_on_top: false,
        close_shortcut: "Esc".into(),
        mapping_chip: None,
    }
}

fn own_frame(name: &str, rect: Option<FrameRect>) -> ControlMsg {
    ControlMsg::OpenGui(GuiTarget::OwnFrame {
        header: header(name),
        lang: "de".into(),
        rect,
    })
}

/// `OpenGui{OwnFrame}` → `GuiOpened` (the plugin's size in points, resizable) and `FrameWindow`
/// (the frame's `HWND`).
fn open_at(bridge: &mut Bridge, name: &str, rect: Option<FrameRect>) -> (GuiSize, bool, HWND) {
    bridge.send(own_frame(name, rect));
    let (size, resizable) = bridge.expect("GuiOpened", |m| match m {
        BridgeMsg::GuiOpened { size, resizable } => Some((*size, *resizable)),
        _ => None,
    });
    let window = bridge.expect("FrameWindow", |m| match m {
        BridgeMsg::FrameWindow(n) => Some(*n),
        _ => None,
    });
    (size, resizable, window as HWND)
}

fn open(bridge: &mut Bridge, name: &str) -> (GuiSize, bool, HWND) {
    open_at(bridge, name, None)
}

/// `CloseGui` → `GuiClosed` with the frame's last rect.
fn close(bridge: &mut Bridge) -> FrameRect {
    bridge.send(ControlMsg::CloseGui);
    bridge.expect("GuiClosed", |m| match m {
        BridgeMsg::GuiClosed { rect } => Some(rect.expect("the own frame reports its rect")),
        _ => None,
    })
}

/// A few blocks, all processed in time, while the frame is open.
fn process(bridge: &mut Bridge) {
    for i in 0..8 {
        let events = if i == 0 {
            vec![note_on(0, 60)]
        } else {
            Vec::new()
        };
        assert_eq!(
            bridge.block(&events).status,
            BlockStatus::Ok as u32,
            "block {i}"
        );
    }
}

fn is_window(window: HWND) -> bool {
    // SAFETY: a plain query; any value is allowed.
    unsafe { IsWindow(window) != 0 }
}

/// Waits until `window` is gone (the frame closes after `GuiClosed`).
fn wait_gone(window: HWND) {
    let start = Instant::now();
    while is_window(window) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the frame window stays"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn title(window: HWND) -> String {
    let mut text = [0u16; 256];
    // SAFETY: the buffer's length is passed; works across processes for top-level windows.
    let len = unsafe { GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32) };
    String::from_utf16_lossy(&text[..len.max(0) as usize])
}

/// The client size of `window` in physical pixels: the test thread is made per-monitor DPI aware
/// first (the test binary has no manifest, and an unaware thread sees another process's windows
/// in scaled sizes).
fn client(window: HWND) -> (i32, i32) {
    // SAFETY: a setting of this thread only.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a valid out pointer.
    unsafe { GetClientRect(window, &mut rect) };
    (rect.right, rect.bottom)
}

/// The frame's container (winit's window has no other child) and the plugin's window in it.
fn container_and_plugin(frame: HWND) -> (HWND, HWND) {
    // SAFETY: plain queries.
    let container = unsafe { GetWindow(frame, GW_CHILD) };
    assert!(!container.is_null(), "no container in the frame");
    // SAFETY: as above.
    let plugin = unsafe { GetWindow(container, GW_CHILD) };
    assert!(
        !plugin.is_null(),
        "the plugin has no window in the container"
    );
    (container, plugin)
}

/// The synth's frame settles at the frame's DPI: the editor's fixed 240 × 80 logical px are
/// physical pixels × scale in the container (a CLAP editor may first report its unscaled size and
/// ask for the scaled one right after), with the header above it and the window at least as wide.
fn settled_synth(window: HWND) {
    // SAFETY: a plain query.
    let scale = f64::from(unsafe { GetDpiForWindow(window) }) / 96.0;
    let want = (
        (240.0 * scale).round() as i32,
        (80.0 * scale).round() as i32,
    );
    let start = Instant::now();
    loop {
        let (container, plugin) = container_and_plugin(window);
        let (sizes, frame) = ((client(container), client(plugin)), client(window));
        let header = (f64::from(HEADER) * scale).round() as i32;
        if sizes == (want, want) && frame.1 == want.1 + header && frame.0 >= want.0 {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "scale {scale}: container, plugin {sizes:?}, frame {frame:?}, want {want:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether this machine's OpenGL driver can make the context kp-test-synth's GUI asks for: a
/// throw-away context on a hidden window must offer `wglCreateContextAttribsARB`, which baseview
/// uses for its 3.2 core context. Windows' software renderer (GDI Generic, OpenGL 1.1) does not.
fn opengl_available() -> bool {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
    use windows_sys::Win32::Graphics::OpenGL::{
        ChoosePixelFormat, GL_RENDERER, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW, PFD_MAIN_PLANE,
        PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR, SetPixelFormat, glGetString,
        wglCreateContext, wglDeleteContext, wglGetProcAddress, wglMakeCurrent,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow};

    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: a hidden window of a system class, its DC and a legacy context, each released or
    // destroyed before returning; the pixel format descriptor and the name are live for the calls.
    unsafe {
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if window.is_null() {
            eprintln!("no window for the OpenGL probe");
            return false;
        }
        let dc = GetDC(window);
        let pfd = PIXELFORMATDESCRIPTOR {
            nSize: size_of::<PIXELFORMATDESCRIPTOR>() as u16,
            nVersion: 1,
            dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
            iPixelType: PFD_TYPE_RGBA,
            cColorBits: 32,
            cAlphaBits: 8,
            cDepthBits: 24,
            cStencilBits: 8,
            iLayerType: PFD_MAIN_PLANE as u8,
            ..std::mem::zeroed()
        };
        let mut modern = false;
        let format = ChoosePixelFormat(dc, &pfd);
        if format != 0 && SetPixelFormat(dc, format, &pfd) != 0 {
            let context = wglCreateContext(dc);
            if !context.is_null() {
                if wglMakeCurrent(dc, context) != 0 {
                    modern =
                        wglGetProcAddress(c"wglCreateContextAttribsARB".as_ptr().cast()).is_some();
                    let renderer = glGetString(GL_RENDERER);
                    if !renderer.is_null() {
                        let name = std::ffi::CStr::from_ptr(renderer.cast());
                        eprintln!("OpenGL renderer: {}", name.to_string_lossy());
                    }
                    wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut());
                }
                wglDeleteContext(context);
            }
        }
        ReleaseDC(window, dc);
        DestroyWindow(window);
        if !modern {
            eprintln!("no OpenGL driver with wglCreateContextAttribsARB");
        }
        modern
    }
}

/// Without a modern OpenGL driver: opening the frame is refused with a GUI error (not fatal), and
/// the bridge keeps processing and shuts down cleanly. What a user on such a machine sees for an
/// OpenGL plugin.
fn gui_without_opengl(bundle: &Path, id: &str) {
    eprintln!("skipping the frame checks: kp-test-synth's GUI needs OpenGL 3.2 (module docs)");
    let mut bridge = offscreen_bridge();
    bridge.load(bundle, id);
    bridge.activate();
    bridge.send(own_frame("kp-test-synth", None));
    let error = bridge.expect_error(ErrorContext::Gui);
    assert!(
        error.contains("GUI failed") || error.starts_with("plugin frame initialization failed:"),
        "{error}"
    );
    process(&mut bridge);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

fn synth_frame(bundle: &Path, id: &str) {
    let _startup = FRAME_STARTUP
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !opengl_available() {
        return gui_without_opengl(bundle, id);
    }
    let mut bridge = offscreen_bridge();
    let info = bridge.load(bundle, id);
    assert!(info.has_gui, "kp-test-synth has a Win32 GUI");
    bridge.activate();

    let t0 = Instant::now();
    let (size, resizable, window) = open(&mut bridge, "kp-test-synth");
    eprintln!("first open: {:?}, {size:?}", t0.elapsed());
    assert!(!resizable);
    assert!(is_window(window));
    assert_eq!(title(window), "kp-test-synth · Pad · komponist");
    settled_synth(window);

    process(&mut bridge);
    bridge.sync();
    // A second open while one is open is refused.
    bridge.send(own_frame("kp-test-synth", None));
    assert!(
        bridge
            .expect_error(ErrorContext::Gui)
            .contains("already open")
    );
    // A fixed-size plugin keeps its size: the frame answers with it.
    bridge.send(ControlMsg::SetGuiSize(GuiSize {
        width: 500,
        height: 400,
    }));
    let kept = bridge.expect("GuiSizeRequest", |m| match m {
        BridgeMsg::GuiSizeRequest(s) => Some(*s),
        _ => None,
    });
    assert_eq!((kept.width, kept.height), (240, 80));
    // Header changes and the activation request are taken while open (off-screen: no activation).
    let mut changed = header("kp-test-synth");
    changed.bypassed = true;
    changed.mark = Mark::Bypassed;
    changed.keep_on_top = true;
    changed.sub = "Bass · CLAP".into();
    bridge.send(ControlMsg::HeaderState {
        header: changed,
        lang: "en".into(),
    });
    bridge.send(ControlMsg::ActivateFrame);
    // komponist became active: the frame comes forward (without activating).
    bridge.send(ControlMsg::RaiseFrame);
    bridge.sync();
    // The new title arrives with the next pass of the frame.
    let start = Instant::now();
    while title(window) != "kp-test-synth · Bass · komponist" {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{}",
            title(window)
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let rect = close(&mut bridge);
    assert_eq!((rect.x, rect.y), (OFFSCREEN, OFFSCREEN), "{rect:?}");
    // The plugin below the header; the frame at least as wide as the plugin (the header needs
    // more than 240 points, so the plugin is centred in a wider frame).
    assert!((rect.height - (80.0 + HEADER)).abs() < 1.0, "{rect:?}");
    assert!(rect.width >= 240.0, "{rect:?}");
    wait_gone(window);

    // Again in the same bridge at the same rect: eframe's event loop is reused. (The header is
    // the first one again, so the width follows it.)
    let t1 = Instant::now();
    let (_, _, window) = open_at(&mut bridge, "kp-test-synth", Some(rect));
    eprintln!("reopen: {:?}", t1.elapsed());
    settled_synth(window);
    process(&mut bridge);
    let reopened = close(&mut bridge);
    assert_eq!(
        (reopened.x, reopened.y, reopened.height),
        (rect.x, rect.y, rect.height)
    );
    wait_gone(window);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn the_test_synth_opens_in_an_own_frame_clap() {
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    synth_frame(&bundle, SYNTH);
}

#[test]
fn the_test_synth_opens_in_an_own_frame_vst3() {
    let Some(bundle) = fixture_in("vst3", "kp-test-synth") else {
        return;
    };
    synth_frame(&bundle, SYNTH_VST3);
}

#[test]
fn shutdown_closes_an_open_frame() {
    let _startup = FRAME_STARTUP
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    if !opengl_available() {
        // This check needs an open window. The synth open test covers the headless refusal.
        return;
    }
    let mut bridge = offscreen_bridge();
    bridge.load(&bundle, SYNTH);
    bridge.activate();
    let (_, _, window) = open(&mut bridge, "kp-test-synth");
    assert_eq!(bridge.shutdown(), EXIT_OK);
    assert!(!is_window(window));
}

/// The bridge dies with its frame open (a plugin crash, or the watchdog's kill): the frame goes
/// with the process, and the app sees the exit.
#[test]
fn a_dying_bridge_takes_its_frame_with_it() {
    let _startup = FRAME_STARTUP
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    if !opengl_available() {
        // This check needs an open window. The synth open test covers the headless refusal.
        return;
    }
    let mut bridge = offscreen_bridge();
    bridge.load(&bundle, SYNTH);
    bridge.activate();
    let (_, _, window) = open(&mut bridge, "kp-test-synth");
    assert!(is_window(window));
    bridge.watcher.kill().unwrap();
    let exit = bridge.wait_exit();
    assert_ne!(exit, EXIT_OK);
    assert!(!is_window(window), "the frame outlived its bridge");
}

/// A key for the plugin's window passes the bridge's key hook (in eframe's message loop) first:
/// Esc and Space go to the app as `Shortcut` while the user is not typing, letters go to the
/// plugin and start a typing session, in which Space goes to the plugin too.
#[test]
fn the_key_hook_takes_shortcuts_from_the_focused_plugin() {
    let _startup = FRAME_STARTUP
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(bundle) = fixture("kp-test-synth") else {
        return;
    };
    if !opengl_available() {
        // This check needs an open window. The synth open test covers the headless refusal.
        return;
    }
    let mut bridge = offscreen_bridge();
    bridge.load(&bundle, SYNTH);
    let (_, _, window) = open(&mut bridge, "kp-test-synth");
    let (_, plugin) = container_and_plugin(window);
    let key = |vk: u16| {
        // SAFETY: posting key messages to the plugin's window in the bridge's queue.
        unsafe {
            PostMessageW(plugin, WM_KEYDOWN, usize::from(vk), 1);
            PostMessageW(plugin, WM_KEYUP, usize::from(vk), 0xC000_0001_u32 as isize);
        }
    };
    let shortcuts = |bridge: &mut Bridge| -> Vec<FrameChord> {
        bridge.sync();
        let taken = bridge
            .seen
            .iter()
            .filter_map(|m| match m {
                BridgeMsg::Shortcut { chord, from_plugin } => {
                    assert!(from_plugin);
                    Some(chord.clone())
                }
                _ => None,
            })
            .collect();
        bridge
            .seen
            .retain(|m| !matches!(m, BridgeMsg::Shortcut { .. }));
        taken
    };
    // Esc (VK_ESCAPE) and Space (VK_SPACE) while not typing.
    key(0x1B);
    let start = Instant::now();
    let mut taken = Vec::new();
    while taken.is_empty() && start.elapsed() < Duration::from_secs(5) {
        taken = shortcuts(&mut bridge);
    }
    assert_eq!(taken, [FrameChord::plain("Escape")]);
    key(0x20);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(shortcuts(&mut bridge), [FrameChord::plain("Space")]);
    // A letter (VK 'A') goes to the plugin and starts typing; Space then goes to the plugin too.
    key(0x41);
    key(0x20);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(shortcuts(&mut bridge), []);
    // Keys for the header (winit's window itself) never become `Shortcut`.
    // SAFETY: as above.
    unsafe { PostMessageW(window, WM_KEYDOWN, 0x1B, 1) };
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(shortcuts(&mut bridge), []);
    close(&mut bridge);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
fn x11_targets_and_plugins_without_gui_are_refused() {
    let Some(bundle) = fixture("kp-test-fx") else {
        return;
    };
    let mut bridge = offscreen_bridge();
    let info = bridge.load(&bundle, FX);
    assert!(!info.has_gui);
    bridge.send(own_frame("kp-test-fx", None));
    bridge.expect_error(ErrorContext::Gui);
    bridge.send(ControlMsg::OpenGui(GuiTarget::X11Child {
        xid: 1,
        scale: 1.0,
    }));
    bridge.expect_error(ErrorContext::Gui);
    // Raising without a frame does nothing.
    bridge.send(ControlMsg::RaiseFrame);
    bridge.sync();
    // Closing what never opened still answers.
    bridge.send(ControlMsg::CloseGui);
    bridge.expect("GuiClosed", |m| {
        matches!(m, BridgeMsg::GuiClosed { .. }).then_some(())
    });
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

// ------------------------------------------------------------------------------------------------
// Installed plugins
// ------------------------------------------------------------------------------------------------

/// Opens `path`'s frame twice; for a resizable plugin also a user resize to 900 × 600 points, which
/// the plugin may adjust: the frame then reports the adjusted body.
fn installed_frame(path: &str, plugin_id: &str) {
    let Some(bundle) = installed(path) else {
        return;
    };
    let mut bridge = offscreen_bridge();
    let info = bridge.load(&bundle, plugin_id);
    assert!(info.has_gui, "{path}: no Win32 GUI");
    bridge.activate();
    let t0 = Instant::now();
    let (size, resizable, window) = open(&mut bridge, &info.descriptor.name);
    eprintln!(
        "{}: {}×{} pt, resizable {resizable}, opened in {:?}",
        info.descriptor.name,
        size.width,
        size.height,
        t0.elapsed()
    );
    assert!(size.width > 100 && size.height > 100, "{size:?}");
    container_and_plugin(window);
    process(&mut bridge);
    if resizable {
        bridge.send(ControlMsg::SetGuiSize(GuiSize {
            width: 900,
            height: 600,
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        let moved = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match bridge.recv_timeout(left) {
                Some(BridgeMsg::FrameMoved(rect)) => break rect,
                Some(BridgeMsg::Error { message, .. }) => panic!("{message}"),
                Some(other) => bridge.seen.push(other),
                None => panic!("no FrameMoved after the resize"),
            }
        };
        eprintln!("{}: resized to {moved:?}", info.descriptor.name);
        let body = (moved.width, moved.height - HEADER);
        // Surge snaps to its zoom steps or keeps its aspect ratio: near 900 × 600.
        assert!((body.0 - 900.0).abs() <= 100.0, "{moved:?}");
        assert!((body.1 - 600.0).abs() <= 100.0, "{moved:?}");
    }
    let rect = close(&mut bridge);
    assert_eq!((rect.x, rect.y), (OFFSCREEN, OFFSCREEN), "{rect:?}");
    wait_gone(window);
    let (again, _, _) = open(&mut bridge, &info.descriptor.name);
    assert!(again.width > 100, "{again:?}");
    close(&mut bridge);
    assert_eq!(bridge.shutdown(), EXIT_OK);
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn surge_xt_frame() {
    installed_frame(
        r"C:\Program Files\Common Files\CLAP\Surge Synth Team\Surge XT.clap",
        "org.surge-synth-team.surge-xt",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn surge_xt_vst3_frame() {
    installed_frame(
        r"C:\Program Files\Common Files\VST3\Surge Synth Team\Surge XT.vst3",
        "ABCDEF019182FAEB566D624153675854",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn dexed_frame() {
    installed_frame(
        r"C:\Program Files\Common Files\CLAP\Dexed.clap",
        "com.digital-suburban.dexed",
    );
}

#[test]
#[ignore = "installed plugin; run with --ignored"]
fn dexed_vst3_frame() {
    installed_frame(
        r"C:\Program Files\Common Files\VST3\Dexed.vst3",
        "ABCDEF019182FAEB4447534244657864",
    );
}
