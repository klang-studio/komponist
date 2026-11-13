//! Plugin GUIs through the bridge: `OpenGui` into an X11 window of the test (standing in for the
//! frame's child window, WP-50), blocks processed meanwhile as the engine would, resize, close,
//! shutdown. Also checks that the bridge's main loop does not spin while a GUI runs (the JUCE
//! write-interest trap, spike WP-15 §1.7).
//!
//! `#[ignore]` and guarded by `KP_GUI_TESTS=1`: run under Xvfb only, never on a desktop.
//!
//! ```text
//! KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!     cargo test -p komponist-bridge --test gui -- --ignored --test-threads=1
//! ```

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use komponist_ipc::{BridgeMsg, ControlMsg, GuiSize};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, EventMask, WindowClass};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Atoms JUCE looks up with `only_if_exists`; on a pristine Xvfb nobody has created them yet and
/// JUCE then sends atom 0 (`BadAtom`, spike WP-15 §1.8). A real desktop has them all.
const ATOMS: &[&str] = &[
    "WM_PROTOCOLS",
    "WM_DELETE_WINDOW",
    "WM_TAKE_FOCUS",
    "UTF8_STRING",
    "CLIPBOARD",
    "TARGETS",
    "_NET_SUPPORTED",
    "_NET_SUPPORTING_WM_CHECK",
    "_NET_ACTIVE_WINDOW",
    "_NET_WORKAREA",
    "_NET_CURRENT_DESKTOP",
    "_NET_FRAME_EXTENTS",
    "_NET_WM_NAME",
    "_NET_WM_ICON",
    "_NET_WM_ICON_NAME",
    "_NET_WM_PID",
    "_NET_WM_PING",
    "_NET_WM_USER_TIME",
    "_NET_WM_STATE",
    "_NET_WM_STATE_ABOVE",
    "_NET_WM_STATE_HIDDEN",
    "_NET_WM_STATE_FULLSCREEN",
    "_NET_WM_STATE_SKIP_TASKBAR",
    "_NET_WM_WINDOW_OPACITY",
    "_NET_WM_WINDOW_TYPE",
    "_NET_WM_WINDOW_TYPE_NORMAL",
    "_NET_WM_WINDOW_TYPE_DIALOG",
    "_NET_WM_WINDOW_TYPE_DND",
    "_NET_WM_WINDOW_TYPE_POPUP_MENU",
    "_NET_WM_WINDOW_TYPE_TOOLTIP",
    "_NET_WM_SYNC_REQUEST",
    "_NET_WM_SYNC_REQUEST_COUNTER",
    "_MOTIF_WM_HINTS",
    "_XEMBED",
    "_XEMBED_INFO",
    "XdndAware",
    "XdndEnter",
    "XdndLeave",
    "XdndPosition",
    "XdndStatus",
    "XdndDrop",
    "XdndFinished",
    "XdndSelection",
    "XdndTypeList",
    "XdndActionList",
    "XdndActionCopy",
    "XdndActionPrivate",
];

/// Whether GUI tests may run here.
fn gui_allowed() -> bool {
    let allowed = std::env::var("KP_GUI_TESTS").is_ok_and(|v| v == "1")
        && std::env::var_os("DISPLAY").is_some();
    if !allowed {
        eprintln!("skipped: GUI tests need KP_GUI_TESTS=1 and an Xvfb DISPLAY");
    }
    allowed
}

/// The frame's stand-in: a top-level window of the test's own X connection.
struct Parent {
    conn: RustConnection,
    window: u32,
}

impl Parent {
    fn new(width: u16, height: u16) -> Parent {
        let (conn, screen) = x11rb::connect(None).expect("X11 connection");
        let cookies: Vec<_> = ATOMS
            .iter()
            .map(|name| conn.intern_atom(false, name.as_bytes()).unwrap())
            .collect();
        for cookie in cookies {
            cookie.reply().unwrap();
        }
        let root = conn.setup().roots[screen].clone();
        let window = conn.generate_id().unwrap();
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root.root,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .background_pixel(root.black_pixel)
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .unwrap();
        conn.map_window(window).unwrap();
        conn.sync().unwrap();
        Parent { conn, window }
    }

    /// Child windows of the parent (the plugin's window once attached).
    fn children(&self) -> Vec<u32> {
        self.conn
            .query_tree(self.window)
            .unwrap()
            .reply()
            .unwrap()
            .children
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.sync();
    }
}

/// CPU time of one thread of process `pid` in seconds.
fn thread_cpu_seconds(pid: u32, tid: u32) -> f64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")).unwrap();
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..]
        .split_whitespace()
        .collect();
    (fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap()) / 100.0
}

/// Processes blocks at roughly the engine's pace for `duration`.
fn run_blocks(bridge: &mut Bridge, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        bridge.block(&[]);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Opens the GUI of `plugin_id` in `bundle` through the bridge, serves it, resizes and closes it.
fn exercise(bundle: &Path, plugin_id: &str, name: &str) {
    // The parent outlives the bridge even when an assertion fails (locals drop in reverse).
    let parent = Parent::new(1600, 1000);
    let mut bridge = Bridge::spawn();
    let info = bridge.load(bundle, plugin_id);
    assert!(info.has_gui, "{name} has no X11 GUI");
    bridge.activate();

    let opened = Instant::now();
    bridge.send(ControlMsg::OpenGui {
        parent_xid: u64::from(parent.window),
        scale: 1.0,
    });
    let (size, resizable) = bridge.expect("GuiOpened", |m| match m {
        BridgeMsg::GuiOpened { size, resizable } => Some((*size, *resizable)),
        _ => None,
    });
    let open_time = opened.elapsed();
    assert!(size.width > 0 && size.height > 0, "{size:?}");

    run_blocks(&mut bridge, Duration::from_millis(1500));
    assert!(
        !parent.children().is_empty(),
        "no plugin window in the parent"
    );

    // The main loop does not spin: CPU of the bridge's main thread over 2 s of GUI and audio.
    let pid = bridge.pid();
    let (cpu_start, wall_start) = (thread_cpu_seconds(pid, pid), Instant::now());
    run_blocks(&mut bridge, Duration::from_secs(2));
    let main_cpu = (thread_cpu_seconds(pid, pid) - cpu_start) / wall_start.elapsed().as_secs_f64();
    let heartbeat = bridge.heartbeat();
    assert!(
        main_cpu < 0.5,
        "bridge main thread at {:.0} % CPU",
        main_cpu * 100.0
    );

    // Resize: a fixed-size GUI keeps its size and says so; a resizable one adopts something.
    let asked = GuiSize {
        width: size.width + 160,
        height: size.height + 120,
    };
    bridge.send(ControlMsg::SetGuiSize(asked));
    let adopted = if resizable {
        bridge.heartbeat();
        bridge.seen.iter().rev().find_map(|m| match m {
            BridgeMsg::GuiSizeRequest(s) => Some(*s),
            _ => None,
        })
    } else {
        Some(bridge.expect("GuiSizeRequest", |m| match m {
            BridgeMsg::GuiSizeRequest(s) => Some(*s),
            _ => None,
        }))
    };
    if !resizable {
        assert_eq!(adopted, Some(size));
    }
    run_blocks(&mut bridge, Duration::from_millis(500));

    bridge.send(ControlMsg::CloseGui);
    bridge.expect("GuiClosed", |m| (*m == BridgeMsg::GuiClosed).then_some(()));
    run_blocks(&mut bridge, Duration::from_millis(300));
    let t0 = Instant::now();
    assert_eq!(bridge.shutdown(), EXIT_OK);
    let shutdown = t0.elapsed();
    let errors: Vec<_> = bridge
        .seen
        .iter()
        .filter(|m| matches!(m, BridgeMsg::Error { .. }))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    eprintln!(
        "GUI {name} through the bridge: opened in {open_time:.0?}, size {}x{}, resizable \
         {resizable}, asked {}x{} -> {adopted:?}, main thread CPU {:.1} %, heartbeat \
         {heartbeat:.1?}, shutdown {shutdown:.0?}, {} log lines",
        size.width,
        size.height,
        asked.width,
        asked.height,
        main_cpu * 100.0,
        bridge
            .seen
            .iter()
            .filter(|m| matches!(m, BridgeMsg::Log { .. }))
            .count(),
    );
    drop(parent);
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_kp_test_synth() {
    if gui_allowed()
        && let Some(bundle) = fixture("kp-test-synth")
    {
        exercise(&bundle, SYNTH, "kp-test-synth");
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_klopfer() {
    if gui_allowed()
        && let Some(bundle) = installed("~/.clap/Klopfer.clap")
    {
        exercise(&bundle, "com.klopfer.drum-sampler", "klopfer");
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_dexed() {
    // Registers its X11 fd with READ | WRITE: the spin trap of spike WP-15 §1.7.
    if gui_allowed()
        && let Some(bundle) = installed("/usr/lib/clap/Dexed.clap")
    {
        exercise(&bundle, "com.digital-suburban.dexed", "dexed");
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_surge_xt() {
    if gui_allowed()
        && let Some(bundle) = installed("/usr/lib/clap/Surge XT.clap")
    {
        exercise(&bundle, "org.surge-synth-team.surge-xt", "surge-xt");
    }
}
