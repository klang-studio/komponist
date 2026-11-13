//! Embedded GUIs under Xvfb: open into an x11rb parent window, serve timers and fds through the
//! [`PollLoop`] for a while, check that the plugin's window is a child of the parent and shows
//! something, resize, close.
//!
//! `#[ignore]` and guarded by `KP_GUI_TESTS=1`: run only under Xvfb (see `super`), never on a
//! desktop. `KP_GUI_SHOTS=<dir>` additionally saves a screenshot per plugin with ImageMagick's
//! `import`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use komponist_ipc::GuiSize;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, CreateWindowAux, EventMask,
    ImageFormat, MOTION_NOTIFY_EVENT, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::installed::{DEXED, KLOPFER, SHORTCIRCUIT_XT, SURGE_XT, SURGE_XT_FX};
use super::*;
use crate::host::FdEvents;
use crate::host_loop::notification_bits as bits;
use crate::plugin::HostedPlugin;
use komponist_ipc::WireEventKind;

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
pub(crate) fn gui_allowed() -> bool {
    let allowed = std::env::var("KP_GUI_TESTS").is_ok_and(|v| v == "1")
        && std::env::var_os("DISPLAY").is_some();
    if !allowed {
        eprintln!("skipped: GUI tests need KP_GUI_TESTS=1 and an Xvfb DISPLAY");
    }
    allowed
}

/// The frame's stand-in: a top-level window of our own X connection, kept alive for the test.
pub(crate) struct Parent {
    conn: RustConnection,
    root: u32,
    pub(crate) window: u32,
}

impl Parent {
    pub(crate) fn new(width: u16, height: u16) -> Parent {
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
        Parent {
            conn,
            root: root.root,
            window,
        }
    }

    /// Fakes a pointer event (XTEST) at window coordinates; the parent sits at the root origin.
    pub(crate) fn pointer(&self, kind: u8, (x, y): (i16, i16)) {
        let detail = if kind == MOTION_NOTIFY_EVENT { 0 } else { 1 };
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .unwrap();
        self.conn.sync().unwrap();
    }

    /// Child windows of the parent (the plugin's window once attached).
    pub(crate) fn children(&self) -> Vec<u32> {
        self.conn
            .query_tree(self.window)
            .unwrap()
            .reply()
            .unwrap()
            .children
    }

    /// Distinct pixel values in the top-left `size` of the parent (children included).
    pub(crate) fn distinct_colors(&self, size: GuiSize) -> usize {
        let (w, h) = (size.width.min(1600) as u16, size.height.min(1000) as u16);
        let image = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.window, 0, 0, w, h, !0)
            .unwrap()
            .reply()
            .unwrap();
        image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0]))
            .collect::<HashSet<_>>()
            .len()
    }

    pub(crate) fn screenshot(&self, name: &str) {
        let Some(dir) = std::env::var_os("KP_GUI_SHOTS") else {
            return;
        };
        let file = Path::new(&dir).join(format!("{name}.png"));
        let status = std::process::Command::new("import")
            .args(["-window", &self.window.to_string()])
            .arg(&file)
            .status();
        eprintln!("screenshot {}: {status:?}", file.display());
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.sync();
    }
}

/// CPU time of this process (all threads) in seconds.
pub(crate) fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    // Fields after the parenthesised command: state is field 3, utime 14, stime 15.
    let rest = &stat[stat.rfind(')').unwrap() + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks: f64 = fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap();
    ticks / 100.0
}

/// What [`exercise_gui`] expects of a plugin's GUI.
struct Expect {
    /// The GUI draws into its window (false only for the known-blank fixture, see
    /// [`gui_kp_test_synth`]).
    drawn: bool,
    /// A mouse drag (from, to) in GUI coordinates over a control, to check that GUI edits reach
    /// the host while the plugin is inactive (`request_flush` → `flush_params`).
    drag: Option<((i16, i16), (i16, i16))>,
}

/// Drags the pointer while serving the loop; returns the parameter events the edit flushed.
pub(crate) fn drag(
    parent: &Parent,
    host_loop: &PollLoop,
    plugin: &mut dyn HostedPlugin,
    (from, to): ((i16, i16), (i16, i16)),
) -> Vec<WireEvent> {
    let serve = |plugin: &mut dyn HostedPlugin, ms| {
        host_loop.run_for(plugin, Duration::from_millis(ms));
    };
    host_loop.take_observed();
    parent.pointer(MOTION_NOTIFY_EVENT, from);
    serve(plugin, 100);
    parent.pointer(BUTTON_PRESS_EVENT, from);
    serve(plugin, 100);
    for step in 1..=10i16 {
        let at = (
            from.0 + (to.0 - from.0) * step / 10,
            from.1 + (to.1 - from.1) * step / 10,
        );
        parent.pointer(MOTION_NOTIFY_EVENT, at);
        serve(plugin, 30);
    }
    parent.pointer(BUTTON_RELEASE_EVENT, to);
    serve(plugin, 300);
    host_loop.take_observed().flushed
}

/// Opens the GUI of the first plugin in `bundle`, serves it, resizes and closes it.
fn exercise_gui(bundle: &Path, name: &str, expect: Expect) {
    // The parent outlives the plugin even when an assertion fails (locals drop in reverse).
    let parent = Parent::new(1600, 1000);
    let mut rig = load_first(bundle);
    let host_loop = rig.host_loop.clone();
    let plugin = rig.plugin.as_mut();
    assert!(plugin.has_gui());

    let opened = Instant::now();
    let info = plugin.gui_open(u64::from(parent.window), 1.0).unwrap();
    let open_time = opened.elapsed();
    assert!(info.size.width > 0 && info.size.height > 0, "{info:?}");
    assert!(matches!(
        plugin.gui_open(u64::from(parent.window), 1.0),
        Err(crate::HostingError::WrongState(_))
    ));

    // Serve timers and fds while the GUI comes up, and measure what the loop costs.
    host_loop.run_for(plugin, Duration::from_millis(1500));
    let cpu_start = cpu_seconds();
    let stats_start = host_loop.stats();
    let measure = Duration::from_secs(2);
    host_loop.run_for(plugin, measure);
    let cpu = (cpu_seconds() - cpu_start) / measure.as_secs_f64();
    let stats = host_loop.stats();
    let ticks = stats.ticks - stats_start.ticks;
    let write_calls = stats.write_calls - stats_start.write_calls;
    let (timers, fds) = host_loop.registrations();
    let observed = host_loop.take_observed();

    assert!(
        !parent.children().is_empty(),
        "no plugin window in the parent"
    );
    let colors = parent.distinct_colors(info.size);
    parent.screenshot(&format!("{name}-open"));

    // No spinning: timers (typically 20 ms) and rate-limited WRITE reports bound the ticks.
    assert!(ticks < 1500, "{ticks} loop ticks in 2 s");
    let fds_with_write = (0..1024)
        .filter(|&fd| {
            host_loop
                .fd_interest(fd)
                .is_some_and(|i| i.contains(FdEvents::WRITE))
        })
        .count() as u64;
    assert!(
        write_calls <= fds_with_write * 45,
        "{write_calls} WRITE callbacks in 2 s for {fds_with_write} fds"
    );

    // A GUI edit while inactive arrives through request_flush → flush_params.
    let mut edit = None;
    if let Some(path) = expect.drag {
        let flushed = drag(&parent, &host_loop, plugin, path);
        let kinds: Vec<_> = flushed.iter().filter_map(WireEvent::decode).collect();
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, WireEventKind::ParamGesture { begin: true, .. })),
            "no gesture begin in {kinds:?}"
        );
        let (id, value) = kinds
            .iter()
            .rev()
            .find_map(|k| match *k {
                WireEventKind::ParamValue { id, value } => Some((id, value)),
                _ => None,
            })
            .expect("no parameter value from the GUI edit");
        assert!(plugin.params().iter().any(|p| p.id == id));
        let now = plugin.param_value(id).unwrap();
        assert!(
            (now - value).abs() < 1e-6,
            "param {id}: {now} after edit {value}"
        );
        edit = Some((kinds.len(), id, value));
    }

    // Resize: a resizable GUI adopts (or snaps) a new size, a fixed one keeps its size.
    let errors_before = host_loop.notify().log_counts()[3];
    let asked = GuiSize {
        width: info.size.width + 160,
        height: info.size.height + 120,
    };
    let adopted = plugin.gui_set_size(asked).unwrap();
    host_loop.run_for(plugin, Duration::from_millis(500));
    if info.resizable {
        assert!(adopted.width > 0 && adopted.height > 0);
        parent.screenshot(&format!("{name}-resized"));
    } else {
        assert_eq!(adopted, info.size);
    }
    let resize_errors = host_loop.notify().log_counts()[3] - errors_before;
    let resize_observed = host_loop.take_observed();

    plugin.gui_close();
    plugin.gui_close();
    host_loop.run_for(plugin, Duration::from_millis(200));

    eprintln!(
        "GUI {name}: opened in {open_time:?}, size {}x{}, resizable {}, asked {}x{} -> {}x{}, \
         {colors} colours, {timers} timers, {fds} fds ({fds_with_write} with WRITE), \
         {ticks} ticks / {} timer calls / {} fd calls / {write_calls} WRITE calls in 2 s, \
         CPU {:.0} %, resize requests {:?} then {:?}, error logs after resize {resize_errors}, \
         GUI edit (events, id, value) {edit:?}",
        info.size.width,
        info.size.height,
        info.resizable,
        asked.width,
        asked.height,
        adopted.width,
        adopted.height,
        stats.timer_calls - stats_start.timer_calls,
        stats.fd_calls - stats_start.fd_calls,
        cpu * 100.0,
        observed
            .has(bits::GUI_RESIZE_REQUESTED)
            .then_some(observed.gui_size),
        resize_observed
            .has(bits::GUI_RESIZE_REQUESTED)
            .then_some(resize_observed.gui_size),
    );
    if expect.drawn {
        assert!(colors > 1, "the GUI area is blank");
    }

    drop(rig);
    assert_eq!(host_loop.registrations(), (0, 0), "leftover timers or fds");
    drop(parent);
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_kp_test_synth() {
    if !gui_allowed() {
        return;
    }
    if let Some(bundle) = fixture("kp-test-synth") {
        // Known: the fixture's window stays black under Xvfb. It uses crates.io baseview, which
        // lacks the X11 Present/MapNotify fix of the klang-studio fork that Klopfer is built with
        // (spike WP-15 §1.8). Attach, size, close and cleanup are still checked.
        exercise_gui(
            &bundle,
            "kp-test-synth",
            Expect {
                drawn: false,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_klopfer() {
    if gui_allowed()
        && let Some(bundle) = installed(KLOPFER)
    {
        exercise_gui(
            &bundle,
            "klopfer",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_surge_xt() {
    if gui_allowed()
        && let Some(bundle) = installed(SURGE_XT)
    {
        exercise_gui(
            &bundle,
            "surge-xt",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_surge_xt_effects() {
    if gui_allowed()
        && let Some(bundle) = installed(SURGE_XT_FX)
    {
        exercise_gui(
            &bundle,
            "surge-xt-effects",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_dexed() {
    if gui_allowed()
        && let Some(bundle) = installed(DEXED)
    {
        exercise_gui(
            &bundle,
            "dexed",
            Expect {
                drawn: true,
                // The Cutoff knob (default: fully open), dragged down.
                drag: Some(((250, 463), (250, 523))),
            },
        );
    }
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_shortcircuit_xt() {
    if gui_allowed()
        && let Some(bundle) = installed(SHORTCIRCUIT_XT)
    {
        exercise_gui(
            &bundle,
            "shortcircuit-xt",
            Expect {
                drawn: true,
                drag: None,
            },
        );
    }
}

/// The bridge's real situation: the RT thread processes while the main thread serves the GUI.
/// GUI edits then arrive as output events of `process`, and the loop must not call
/// `flush_params` on the active plugin.
#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1"]
fn gui_dexed_edit_while_processing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    if !gui_allowed() {
        return;
    }
    let Some(bundle) = installed(DEXED) else {
        return;
    };
    let parent = Parent::new(1600, 1000);
    let mut rig = load_first(&bundle);
    let host_loop = rig.host_loop.clone();
    let plugin = rig.plugin.as_mut();
    let processor = plugin.activate(CONFIG).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let rt = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let mut processor = processor;
            processor.start_processing().unwrap();
            let mut blocks = Blocks::new();
            let mut events = Vec::new();
            while !stop.load(Ordering::Acquire) {
                blocks.run(processor.as_mut(), &[]);
                events.extend_from_slice(&blocks.events_out[..blocks.out_count]);
                std::thread::sleep(Duration::from_millis(5));
            }
            processor.stop_processing();
            (processor, events)
        }
    });

    plugin.gui_open(u64::from(parent.window), 1.0).unwrap();
    host_loop.run_for(plugin, Duration::from_millis(1000));
    // The Cutoff knob again, dragged down.
    let flushed = drag(&parent, &host_loop, plugin, ((250, 463), (250, 523)));
    host_loop.run_for(plugin, Duration::from_millis(200));
    stop.store(true, Ordering::Release);
    let (processor, events) = rt.join().unwrap();
    plugin.gui_close();
    plugin.deactivate(processor);

    assert!(flushed.is_empty(), "flushed while active: {flushed:?}");
    let kinds: Vec<_> = events.iter().filter_map(WireEvent::decode).collect();
    let values: Vec<_> = kinds
        .iter()
        .filter_map(|k| match *k {
            WireEventKind::ParamValue { id, value } => Some((id, value)),
            _ => None,
        })
        .collect();
    assert!(
        !values.is_empty(),
        "no parameter values from process: {kinds:?}"
    );
    let (id, value) = *values.last().unwrap();
    assert!((plugin.param_value(id).unwrap() - value).abs() < 1e-6);
    eprintln!(
        "GUI dexed while processing: {} output events, {} values, last {id} = {value}",
        kinds.len(),
        values.len()
    );
    drop(rig);
    drop(parent);
}
