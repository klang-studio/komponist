//! GUI message routing for WP-50 through the plugin manager: `open_gui` into an X11 window of the
//! test (standing in for the frame's child window), `GuiOpened`, a resize request answered by the
//! plugin, `close_gui` → `GuiClosed` and a state snapshot; GUI edits during processing reach
//! `param_value` through the shared output ring.
//!
//! `#[ignore]` and guarded by `KP_GUI_TESTS=1`: run under Xvfb only, never on a desktop.
//!
//! ```text
//! KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!     cargo test -p komponist-plugins --test gui -- --ignored --test-threads=1
//! ```

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use komponist_ipc::GuiSize;
use komponist_model::{DeviceId, DeviceRole};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManager, PluginManagerEvent};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, CreateWindowAux, EventMask,
    MOTION_NOTIFY_EVENT, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
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

/// The frame's stand-in: a top-level window at the root origin.
struct Parent {
    conn: RustConnection,
    root: u32,
    window: u32,
}

impl Parent {
    fn new() -> Parent {
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
            1600,
            1000,
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
        Parent {
            conn,
            root: root.root,
            window,
        }
    }

    fn children(&self) -> usize {
        self.conn
            .query_tree(self.window)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .len()
    }

    fn pointer(&self, kind: u8, (x, y): (i16, i16)) {
        let detail = if kind == MOTION_NOTIFY_EVENT { 0 } else { 1 };
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .unwrap();
        self.conn.sync().unwrap();
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.sync();
    }
}

/// Runs the rig in realtime blocks for `duration`.
fn serve(rig: &mut Rig, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        rig.process(BLOCK);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Waits (serving the rig) for the first event `pick` accepts after event index `from`.
fn expect<T>(
    rig: &mut Rig,
    from: usize,
    what: &str,
    mut pick: impl FnMut(&PluginManagerEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(found) = rig.events[from..].iter().find_map(&mut pick) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "no {what}; events {:?}",
            rig.events
        );
        serve(rig, Duration::from_millis(10));
    }
}

/// Opens, resizes and closes the GUI of `device` (id 10 in `rig`).
fn route(rig: &mut Rig, parent: &Parent) -> (GuiSize, bool) {
    let device = DeviceId(10);
    assert!(rig.manager.has_gui(device));
    let from = rig.events.len();
    let t0 = Instant::now();
    rig.manager
        .open_gui(device, u64::from(parent.window), 1.0)
        .unwrap();
    let (size, resizable) = expect(rig, from, "GuiOpened", |e| match e {
        PluginManagerEvent::GuiOpened {
            device: d,
            size,
            resizable,
        } if *d == device => Some((*size, *resizable)),
        _ => None,
    });
    let opened = t0.elapsed();
    serve(rig, Duration::from_millis(1000));
    assert!(parent.children() > 0, "no plugin window in the parent");

    // A fixed-size GUI answers a resize with its own size.
    let from = rig.events.len();
    rig.manager.set_gui_size(
        device,
        GuiSize {
            width: size.width + 100,
            height: size.height + 100,
        },
    );
    if !resizable {
        let asked = expect(rig, from, "GuiSizeRequest", |e| match e {
            PluginManagerEvent::GuiSizeRequest { device: d, size } if *d == device => Some(*size),
            _ => None,
        });
        assert_eq!(asked, size);
    }
    serve(rig, Duration::from_millis(300));

    let from = rig.events.len();
    rig.manager.close_gui(device);
    expect(rig, from, "GuiClosed", |e| {
        matches!(e, PluginManagerEvent::GuiClosed { device: d } if *d == device).then_some(())
    });
    expect(rig, from, "StateSnapshot after closing", |e| {
        matches!(e, PluginManagerEvent::StateSnapshot { device: d, .. } if *d == device)
            .then_some(())
    });
    assert_eq!(rig.manager.status(device), DeviceStatus::Ok);
    eprintln!("GUI opened in {opened:.0?}: {size:?}, resizable {resizable}");
    (size, resizable)
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_routing_kp_test_synth() {
    if !gui_allowed() {
        return;
    }
    let dir = scratch("gui-synth");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let parent = Parent::new();
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(10, DeviceStatus::Ok);
    let (_, resizable) = route(&mut rig, &parent);
    assert!(!resizable);
    rig.manager.shutdown();
    drop(parent);
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_edits_of_dexed_reach_the_manager() {
    let bundle = Path::new("/usr/lib/clap/Dexed.clap");
    if !gui_allowed() || !bundle.exists() {
        return;
    }
    let dir = scratch("gui-dexed");
    let mut manager = PluginManager::new(config(&dir));
    manager.start_scan(&PluginDirs {
        clap: vec!["/usr/lib/clap".into()],
        vst3: Vec::new(),
    });
    wait_for(&mut manager, "ScanFinished", |e| {
        matches!(e, PluginManagerEvent::ScanFinished(_)).then_some(())
    });
    let parent = Parent::new();
    let dexed = device(
        10,
        DeviceRole::Instrument,
        clap_plugin("com.digital-suburban.dexed"),
        None,
    );
    let mut rig = Rig::new(manager, project(vec![track(1, vec![dexed])]));
    rig.wait_status(10, DeviceStatus::Ok);
    route(&mut rig, &parent);

    // Open again and drag the Cutoff knob while processing (WP-26/31 coordinates).
    let from = rig.events.len();
    rig.manager
        .open_gui(DeviceId(10), u64::from(parent.window), 1.0)
        .unwrap();
    expect(&mut rig, from, "GuiOpened", |e| {
        matches!(e, PluginManagerEvent::GuiOpened { .. }).then_some(())
    });
    serve(&mut rig, Duration::from_secs(3));
    let before: Vec<(u32, f64)> = rig
        .manager
        .params(DeviceId(10))
        .unwrap()
        .iter()
        .filter_map(|p| Some((p.id, rig.manager.param_value(DeviceId(10), p.id)?.value)))
        .collect();
    let from = rig.events.len();
    let ((x0, y0), (x1, y1)) = ((250i16, 463i16), (250i16, 523i16));
    parent.pointer(MOTION_NOTIFY_EVENT, (x0, y0));
    serve(&mut rig, Duration::from_millis(100));
    parent.pointer(BUTTON_PRESS_EVENT, (x0, y0));
    serve(&mut rig, Duration::from_millis(100));
    for step in 1..=10i16 {
        parent.pointer(
            MOTION_NOTIFY_EVENT,
            (x0 + (x1 - x0) * step / 10, y0 + (y1 - y0) * step / 10),
        );
        serve(&mut rig, Duration::from_millis(30));
    }
    parent.pointer(BUTTON_RELEASE_EVENT, (x1, y1));
    expect(&mut rig, from, "ParamValuesChanged", |e| {
        matches!(e, PluginManagerEvent::ParamValuesChanged { .. }).then_some(())
    });
    let changed: Vec<u32> = before
        .iter()
        .filter(|(id, v)| {
            rig.manager
                .param_value(DeviceId(10), *id)
                .is_some_and(|now| now.value != *v)
        })
        .map(|(id, _)| *id)
        .collect();
    assert!(!changed.is_empty(), "no parameter changed");
    // The edit makes the state dirty: a debounced snapshot follows.
    expect(&mut rig, from, "debounced StateSnapshot", |e| {
        matches!(e, PluginManagerEvent::StateSnapshot { .. }).then_some(())
    });
    eprintln!("Dexed GUI edit changed parameters {changed:?}");
    rig.manager.close_gui(DeviceId(10));
    rig.manager.shutdown();
    drop(parent);
}
