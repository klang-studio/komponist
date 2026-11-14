//! Shared driver of the real-window plugin frame tests (`plugin_frame_gui.rs`,
//! `plugin_frame_installed.rs`): the real app with a `PluginHost` and real bridges inside a real
//! eframe event loop, a script of steps run once per frame, and an X11 connection of the test
//! for checks, synthetic input (XTEST) and screenshots (`import -window root`).
//!
//! Runs only with `KP_GUI_TESTS=1` and a `DISPLAY` — under Xvfb, never on a desktop:
//!
//! ```text
//! KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!     cargo test -p komponist-app --test plugin_frame_gui -- --ignored --nocapture
//! ```
//!
//! winit allows one event loop per process, so each test binary runs one script; screenshots go
//! to `target/tmp/plugin-frame-gui/`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_app::Action;
use komponist_app::App;
use komponist_app::actions::{DropTarget, PluginAction};
use komponist_app::windows::plugin_frame::FrameInfo;
use komponist_model::{DeviceId, PluginRef};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as _};
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConfigureWindowAux, ConnectionExt as _,
    KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// How long loading, opening and answers may take on a loaded machine.
pub const PATIENCE: Duration = Duration::from_secs(40);

/// Whether real-window tests may run here.
pub fn gui_allowed() -> bool {
    let allowed = std::env::var("KP_GUI_TESTS").is_ok_and(|v| v == "1")
        && std::env::var_os("DISPLAY").is_some();
    if !allowed {
        eprintln!("skipped: needs KP_GUI_TESTS=1 and an Xvfb DISPLAY");
    }
    allowed
}

/// Where the screenshots go.
pub fn shot_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugin-frame-gui");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ------------------------------------------------------------------------------------------------
// The test's X11 connection
// ------------------------------------------------------------------------------------------------

/// Atoms JUCE looks up with `only_if_exists`; on a pristine Xvfb nobody has created them yet
/// (spike WP-15 §1.8).
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

/// A size in pixels.
pub type Size = (i32, i32);

/// The test's own X11 connection.
pub struct TestX {
    pub conn: RustConnection,
    pub root: Window,
}

impl TestX {
    pub fn connect() -> TestX {
        let (conn, screen) = x11rb::connect(None).expect("X11 connection");
        let cookies: Vec<_> = ATOMS
            .iter()
            .map(|name| conn.intern_atom(false, name.as_bytes()).unwrap())
            .collect();
        for cookie in cookies {
            cookie.reply().unwrap();
        }
        let root = conn.setup().roots[screen].root;
        TestX { conn, root }
    }

    /// `(x, y, width, height)` of `w` relative to the root, `None` if it is gone.
    pub fn geometry(&self, w: Window) -> Option<(i16, i16, u16, u16)> {
        let g = self.conn.get_geometry(w).ok()?.reply().ok()?;
        let t = self
            .conn
            .translate_coordinates(w, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some((t.dst_x, t.dst_y, g.width, g.height))
    }

    /// Size of `w` and its position inside its parent.
    pub fn local(&self, w: Window) -> Option<(i16, i16, u16, u16)> {
        let g = self.conn.get_geometry(w).ok()?.reply().ok()?;
        Some((g.x, g.y, g.width, g.height))
    }

    pub fn children(&self, w: Window) -> Vec<Window> {
        self.conn
            .query_tree(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|t| t.children)
            .unwrap_or_default()
    }

    /// Whether `w` is mapped.
    pub fn mapped(&self, w: Window) -> bool {
        self.conn
            .get_window_attributes(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.map_state != x11rb::protocol::xproto::MapState::UNMAPPED)
    }

    pub fn exists(&self, w: Window) -> bool {
        self.conn
            .get_geometry(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some()
    }

    /// Min and max size of `w`'s `WM_NORMAL_HINTS`.
    pub fn size_hints(&self, w: Window) -> (Option<Size>, Option<Size>) {
        let hints = WmSizeHints::get_normal_hints(&self.conn, w)
            .ok()
            .and_then(|c| c.reply().ok())
            .flatten();
        match hints {
            Some(h) => (h.min_size, h.max_size),
            None => (None, None),
        }
    }

    pub fn focus(&self) -> Window {
        self.conn.get_input_focus().unwrap().reply().unwrap().focus
    }

    /// Resizes top-level window `w` as a window manager (or xdotool windowsize) would.
    pub fn resize(&self, w: Window, width: u32, height: u32) {
        self.conn
            .configure_window(w, &ConfigureWindowAux::new().width(width).height(height))
            .unwrap();
        self.conn.flush().unwrap();
    }

    /// A left click at root position `(x, y)`.
    pub fn click(&self, (x, y): (i16, i16)) {
        let c = &self.conn;
        c.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, self.root, x, y, 0)
            .unwrap();
        c.sync().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        c.xtest_fake_input(BUTTON_PRESS_EVENT, 1, 0, self.root, x, y, 0)
            .unwrap();
        c.sync().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        c.xtest_fake_input(BUTTON_RELEASE_EVENT, 1, 0, self.root, x, y, 0)
            .unwrap();
        c.sync().unwrap();
    }

    /// The keycode of `keysym` (level 0).
    pub fn keycode(&self, keysym: u32) -> u8 {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .unwrap()
            .reply()
            .unwrap();
        let per = usize::from(map.keysyms_per_keycode);
        let index = map
            .keysyms
            .chunks(per)
            .position(|syms| syms.first() == Some(&keysym))
            .expect("keysym on the keyboard");
        min + index as u8
    }

    /// Presses and releases `keysym`.
    pub fn key(&self, keysym: u32) {
        let code = self.keycode(keysym);
        let c = &self.conn;
        c.xtest_fake_input(KEY_PRESS_EVENT, code, 0, self.root, 0, 0, 0)
            .unwrap();
        c.sync().unwrap();
        std::thread::sleep(Duration::from_millis(40));
        c.xtest_fake_input(KEY_RELEASE_EVENT, code, 0, self.root, 0, 0, 0)
            .unwrap();
        c.sync().unwrap();
    }

    /// The process owning window `w` (X-Resource extension).
    pub fn pid_of(&self, w: Window) -> Option<u32> {
        let spec = ClientIdSpec {
            client: w,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        };
        let reply = self.conn.res_query_client_ids(&[spec]).ok()?.reply().ok()?;
        reply.ids.first().and_then(|id| id.value.first().copied())
    }

    /// `import -window root` into the shot folder; returns the path.
    pub fn shot(&self, name: &str) -> PathBuf {
        let path = shot_dir().join(format!("{name}.png"));
        let status = std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(&path)
            .status();
        match status {
            Ok(s) if s.success() => eprintln!("screenshot {}", path.display()),
            other => eprintln!("screenshot {name} failed: {other:?}"),
        }
        path
    }
}

// ------------------------------------------------------------------------------------------------
// Script
// ------------------------------------------------------------------------------------------------

/// What a step reports for this frame.
pub enum Poll {
    /// Not yet; call again next frame.
    Pending,
    /// Done; go to the next step.
    Done,
}

/// What a step works with.
pub struct World<'a> {
    pub app: &'a mut App,
    pub x: &'a TestX,
    /// Since the step started.
    pub elapsed: Duration,
    /// Device events of this frame and all frames since the step started.
    pub events: &'a [PluginManagerEvent],
    /// Devices by the names the script gave them.
    pub devices: &'a mut Vec<(String, DeviceId)>,
    /// Remarks for the report.
    pub notes: &'a mut Vec<String>,
}

impl World<'_> {
    pub fn device(&self, name: &str) -> DeviceId {
        self.devices
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no device {name}"))
            .1
    }

    pub fn frame(&self, name: &str) -> Option<FrameInfo> {
        self.app.plugin_frame(self.device(name))
    }

    pub fn status(&self, name: &str) -> DeviceStatus {
        self.app.engine().device_status(self.device(name))
    }

    pub fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        eprintln!("note: {text}");
        self.notes.push(text);
    }
}

type StepFn = Box<dyn FnMut(&mut World<'_>) -> Result<Poll, String>>;

/// One step: a name, a timeout and the function polled every frame.
pub struct Step {
    pub name: String,
    pub timeout: Duration,
    pub f: StepFn,
}

/// A step polled until it is done.
pub fn step(
    name: impl Into<String>,
    f: impl FnMut(&mut World<'_>) -> Result<Poll, String> + 'static,
) -> Step {
    Step {
        name: name.into(),
        timeout: PATIENCE,
        f: Box::new(f),
    }
}

/// A step that runs once.
pub fn once(name: impl Into<String>, mut f: impl FnMut(&mut World<'_>) + 'static) -> Step {
    step(name, move |w| {
        f(w);
        Ok(Poll::Done)
    })
}

/// A step that waits `time`.
pub fn wait(time: Duration) -> Step {
    step(format!("wait {time:?}"), move |w| {
        Ok(if w.elapsed >= time {
            Poll::Done
        } else {
            Poll::Pending
        })
    })
}

/// A step that waits until `cond` holds.
pub fn until(
    name: impl Into<String>,
    mut cond: impl FnMut(&mut World<'_>) -> bool + 'static,
) -> Step {
    step(name, move |w| {
        Ok(if cond(w) { Poll::Done } else { Poll::Pending })
    })
}

/// How the script ended.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub finished: bool,
    pub failure: Option<String>,
    pub notes: Vec<String>,
}

struct Driver {
    app: App,
    x: TestX,
    steps: Vec<Step>,
    current: usize,
    started: Instant,
    events: Vec<PluginManagerEvent>,
    devices: Vec<(String, DeviceId)>,
    notes: Vec<String>,
    outcome: Arc<Mutex<Outcome>>,
    closing: bool,
}

impl eframe::App for Driver {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.app.show(ui);
        ctx.request_repaint_after(Duration::from_millis(10));
        if self.closing {
            return;
        }
        self.events.extend(self.app.device_events().iter().cloned());
        let Some(step) = self.steps.get_mut(self.current) else {
            self.finish(&ctx, None);
            return;
        };
        let elapsed = self.started.elapsed();
        let mut world = World {
            app: &mut self.app,
            x: &self.x,
            elapsed,
            events: &self.events,
            devices: &mut self.devices,
            notes: &mut self.notes,
        };
        let name = step.name.clone();
        let result = (step.f)(&mut world);
        match result {
            Ok(Poll::Done) => {
                eprintln!("[{:6.2}s] step done: {name}", elapsed.as_secs_f32());
                self.current += 1;
                self.started = Instant::now();
                self.events.clear();
            }
            Ok(Poll::Pending) if elapsed > step.timeout => {
                self.finish(&ctx, Some(format!("step \"{name}\" timed out")));
            }
            Ok(Poll::Pending) => {}
            Err(e) => self.finish(&ctx, Some(format!("step \"{name}\": {e}"))),
        }
    }

    fn on_exit(&mut self) {
        self.app.shutdown();
    }
}

impl Driver {
    fn finish(&mut self, ctx: &egui::Context, failure: Option<String>) {
        if let Some(f) = &failure {
            eprintln!("FAILED: {f}");
            self.x.shot("failure");
        }
        let mut o = self.outcome.lock().unwrap();
        o.finished = failure.is_none();
        o.failure = failure;
        o.notes = std::mem::take(&mut self.notes);
        self.closing = true;
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }
}

/// Runs `steps` on `app` in a real eframe window (X11, any thread) and returns how it ended.
pub fn run(app: App, steps: Vec<Step>) -> Outcome {
    // A stuck script must not hang the test run.
    let watchdog = Duration::from_secs(600);
    std::thread::spawn(move || {
        std::thread::sleep(watchdog);
        eprintln!("watchdog: the GUI script did not end within {watchdog:?}");
        std::process::exit(101);
    });
    let outcome = Arc::new(Mutex::new(Outcome::default()));
    let result = Arc::clone(&outcome);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("komponist · GUI-Test")
            .with_inner_size([1200.0, 700.0])
            .with_position([700.0, 360.0]),
        renderer: eframe::Renderer::Wgpu,
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11 as _;
            builder.with_x11();
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    let x = TestX::connect();
    eframe::run_native(
        "komponist-gui-test",
        options,
        Box::new(move |cc| {
            komponist_ui::install(&cc.egui_ctx);
            Ok(Box::new(Driver {
                app,
                x,
                steps,
                current: 0,
                started: Instant::now(),
                events: Vec::new(),
                devices: Vec::new(),
                notes: Vec::new(),
                outcome: result,
                closing: false,
            }))
        }),
    )
    .expect("eframe run");
    outcome.lock().unwrap().clone()
}

// ------------------------------------------------------------------------------------------------
// Building blocks
// ------------------------------------------------------------------------------------------------

/// Adds the first catalog entry `pick` accepts on a new track and remembers its instrument as
/// `name`; waits for the scan first and fails if the catalog has no such plugin.
pub fn add(name: &str, pick: impl Fn(&PluginRef) -> bool + 'static) -> Step {
    let name = name.to_owned();
    step(format!("add {name}"), move |w| {
        let host = w.app.engine().host();
        if host.is_scanning() || host.last_scan().is_none() {
            return Ok(Poll::Pending);
        }
        let Some(plugin) = host
            .catalog()
            .entries()
            .iter()
            .map(|e| e.plugin.clone())
            .find(|p| pick(p))
        else {
            return Err(format!("{name} is not in the catalog"));
        };
        w.app.dispatch(Action::Plugin(PluginAction::Add {
            plugin: plugin.clone(),
            target: DropTarget::NewTrack { index: None },
        }));
        // An instrument makes a new track, an effect joins the selected one.
        let known: Vec<DeviceId> = w.devices.iter().map(|(_, d)| *d).collect();
        let device = w
            .app
            .project()
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .find(|d| d.plugin == plugin && !known.contains(&d.id))
            .ok_or("the device was not added")?
            .id;
        w.devices.push((name.clone(), device));
        Ok(Poll::Done)
    })
}

/// Waits until device `name` runs.
pub fn loaded(name: &str) -> Step {
    let name = name.to_owned();
    step(format!("{name} loaded"), move |w| match w.status(&name) {
        DeviceStatus::Ok => Ok(Poll::Done),
        DeviceStatus::Loading => Ok(Poll::Pending),
        other => Err(format!("{name}: {other:?}")),
    })
}

/// Opens `name`'s window.
pub fn open(name: &str) -> Step {
    let name = name.to_owned();
    once(format!("open {name}"), move |w| {
        let device = w.device(&name);
        w.app
            .dispatch(Action::Plugin(PluginAction::OpenWindow(device)));
    })
}

/// Closes `name`'s window.
pub fn close(name: &str) -> Step {
    let name = name.to_owned();
    once(format!("close {name}"), move |w| {
        let device = w.device(&name);
        w.app
            .dispatch(Action::Plugin(PluginAction::CloseWindow(device)));
    })
}

/// The frame's expected inner size for its plugin.
pub fn expected_frame(f: &FrameInfo) -> Option<(u32, u32)> {
    let (pw, ph) = f.plugin_size?;
    Some((pw.max(f.min_width_px), ph + f.header_px))
}

/// Whether the X11 frame of `f` has the size its plugin asks for and the child the plugin's size
/// (±1 px from rounding).
pub fn frame_settled(x: &TestX, f: &FrameInfo) -> bool {
    let (Some(window), Some(child), Some(want), Some((pw, ph))) =
        (f.window, f.child, expected_frame(f), f.plugin_size)
    else {
        return false;
    };
    let (Some((_, _, w, h)), Some((_, cy, cw, ch))) = (x.local(window), x.local(child)) else {
        return false;
    };
    let close = |a: u32, b: u32| a.abs_diff(b) <= 1;
    close(u32::from(w), want.0)
        && close(u32::from(h), want.1)
        && close(u32::from(cw), pw)
        && close(u32::from(ch), ph)
        && cy as u32 == f.header_px
}

/// Waits until `name`'s GUI is open in its frame with matching sizes and the plugin's window
/// inside the child.
pub fn gui_shown(name: &str) -> Step {
    let name = name.to_owned();
    step(format!("{name} GUI shown"), move |w| {
        let Some(f) = w.frame(&name) else {
            return Err("no frame".into());
        };
        if !f.gui_open || !frame_settled(w.x, &f) {
            return Ok(Poll::Pending);
        }
        let child = f.child.unwrap_or_default();
        if w.x.children(child).is_empty() {
            return Ok(Poll::Pending);
        }
        let requests: Vec<(u32, u32)> = w
            .events
            .iter()
            .filter_map(|e| match e {
                PluginManagerEvent::GuiSizeRequest { size, .. } => Some((size.width, size.height)),
                _ => None,
            })
            .collect();
        w.note(format!(
            "{name}: GUI {:?} px, resizable {}, frame {:?} px, header {} px; plugin size requests while opening: {requests:?}",
            f.plugin_size,
            f.resizable,
            expected_frame(&f),
            f.header_px
        ));
        Ok(Poll::Done)
    })
}

/// A screenshot after the frames had time to paint.
pub fn shot(name: &str) -> Vec<Step> {
    let name = name.to_owned();
    vec![
        wait(Duration::from_millis(700)),
        once(format!("screenshot {name}"), move |w| {
            w.x.shot(&name);
        }),
    ]
}

/// Checks a fixed-size plugin's frame: not resizable (min = max = its size), and a forced
/// resize (a window manager that ignores the hints) changes neither the plugin nor the child.
pub fn fixed_size_checks(name: &str) -> Vec<Step> {
    let n1 = name.to_owned();
    let n2 = name.to_owned();
    let before: Arc<Mutex<Option<(u32, u32)>>> = Arc::default();
    let b1 = Arc::clone(&before);
    let b2 = Arc::clone(&before);
    vec![
        step(format!("{n1} frame is locked"), move |w| {
            let f = w.frame(&n1).ok_or("no frame")?;
            if f.resizable {
                return Err("expected a fixed-size plugin".into());
            }
            let window = f.window.ok_or("no window")?;
            let want = expected_frame(&f).ok_or("no size")?;
            let (min, max) = w.x.size_hints(window);
            let want = (want.0 as i32, want.1 as i32);
            let near = |s: Option<(i32, i32)>| {
                s.is_some_and(|(a, b)| (a - want.0).abs() <= 1 && (b - want.1).abs() <= 1)
            };
            if !(near(min) && near(max)) {
                return Ok(Poll::Pending);
            }
            *b1.lock().unwrap() = f.plugin_size;
            w.note(format!(
                "{n1}: frame locked at {want:?} (min {min:?}, max {max:?})"
            ));
            let (_, _, fw, fh) = w.x.local(window).ok_or("frame gone")?;
            w.x.resize(window, u32::from(fw) + 150, u32::from(fh) + 90);
            Ok(Poll::Done)
        }),
        wait(Duration::from_millis(1500)),
        once(format!("{n2} keeps its size"), move |w| {
            let f = w.frame(&n2).expect("frame");
            let child = f.child.expect("child");
            let size =
                w.x.local(child)
                    .map(|(_, _, a, b)| (u32::from(a), u32::from(b)));
            let plugin = *b2.lock().unwrap();
            assert_eq!(f.plugin_size, plugin, "{n2}: the plugin size changed");
            assert_eq!(size, plugin, "{n2}: the child was resized");
            let resizes = w
                .events
                .iter()
                .filter(|e| matches!(e, PluginManagerEvent::GuiSizeRequest { .. }))
                .count();
            w.note(format!(
                "{n2}: forced frame resize: plugin and child stay {plugin:?}; {resizes} size requests"
            ));
        }),
    ]
}

/// A user resize of a resizable plugin: the frame grows, the plugin takes (or adjusts) the size,
/// the frame follows.
pub fn user_resize(name: &str, dw: i32, dh: i32) -> Vec<Step> {
    let n = name.to_owned();
    let mut before: Option<Option<(u32, u32)>> = None;
    vec![step(
        format!("{name}: user resize {dw:+} × {dh:+}"),
        move |w| {
            let f = w.frame(&n).ok_or("no frame")?;
            let Some(start) = before else {
                if !f.resizable {
                    return Err(format!("{n} should be resizable"));
                }
                let window = f.window.ok_or("no window")?;
                let (_, _, fw, fh) = w.x.local(window).ok_or("no geometry")?;
                let to = (
                    (i32::from(fw) + dw).max(200) as u32,
                    (i32::from(fh) + dh).max(200) as u32,
                );
                w.note(format!("{n}: frame {fw}×{fh} → {}×{}", to.0, to.1));
                w.x.resize(window, to.0, to.1);
                before = Some(f.plugin_size);
                return Ok(Poll::Pending);
            };
            if w.elapsed < Duration::from_millis(800)
                || f.plugin_size == start
                || !frame_settled(w.x, &f)
            {
                return Ok(Poll::Pending);
            }
            let device = w.device(&n);
            let told: Vec<(u32, u32)> = w
                .events
                .iter()
                .filter_map(|e| match e {
                    PluginManagerEvent::GuiSizeRequest { device: d, size } if *d == device => {
                        Some((size.width, size.height))
                    }
                    _ => None,
                })
                .collect();
            w.note(format!(
                "{n}: plugin {start:?} → {:?}; the plugin's answers to set_size: {told:?}",
                f.plugin_size
            ));
            Ok(Poll::Done)
        },
    )]
}

/// Clicks into the middle of `name`'s plugin area, then checks the shortcut policy with the
/// plugin focused: Space toggles the transport twice.
pub fn space_reaches_komponist(name: &str, click: Option<(i16, i16)>) -> Vec<Step> {
    let n1 = name.to_owned();
    let n2 = name.to_owned();
    let n3 = name.to_owned();
    vec![
        once(format!("{n1}: click into the plugin"), move |w| {
            let f = w.frame(&n1).expect("frame");
            let child = f.child.expect("child");
            let (x, y, cw, ch) = w.x.geometry(child).expect("child geometry");
            let at = click.map_or((x + cw as i16 / 2, y + ch as i16 / 2), |(dx, dy)| {
                (x + dx, y + dy)
            });
            w.x.click(at);
        }),
        wait(Duration::from_millis(500)),
        step(
            format!("{n2}: Space while the plugin has the focus"),
            move |w| {
                let f = w.frame(&n2).ok_or("no frame")?;
                let focus = w.x.focus();
                let (window, child) = (f.window.unwrap_or(0), f.child.unwrap_or(0));
                if focus <= 1 || focus == window {
                    w.note(format!(
                    "{n2}: the plugin did not take the X focus (focus {focus:#x}); key policy not exercised"
                ));
                    return Ok(Poll::Done);
                }
                let inside = focus == child || w.x.children(child).contains(&focus);
                w.note(format!(
                    "{n2}: X focus {focus:#x} (plugin window inside the frame: {inside})"
                ));
                assert_eq!(w.app.transport(), komponist_engine::TransportState::Stopped);
                w.x.key(0x20);
                Ok(Poll::Done)
            },
        ),
        step(format!("{n3}: transport follows Space"), {
            let mut pressed_again = false;
            move |w| {
                use komponist_engine::TransportState::*;
                match (w.app.transport(), pressed_again) {
                    (Stopped, false) if w.elapsed < Duration::from_millis(1500) => {
                        Ok(Poll::Pending)
                    }
                    (Stopped, false) => {
                        // Not exercised (no focus) or not taken back.
                        Ok(Poll::Done)
                    }
                    (Playing, false) => {
                        w.x.key(0x20);
                        pressed_again = true;
                        Ok(Poll::Pending)
                    }
                    (Stopped, true) => {
                        w.note(format!("{n3}: Space played and stopped from the plugin"));
                        Ok(Poll::Done)
                    }
                    _ => Ok(Poll::Pending),
                }
            }
        }),
    ]
}

/// Kills `name`'s bridge (the owner of the plugin's window), waits for the crashed frame, then
/// reloads and waits for the GUI again.
pub fn crash_and_reload(name: &str, shot_name: &str) -> Vec<Step> {
    let n1 = name.to_owned();
    let n2 = name.to_owned();
    let n3 = name.to_owned();
    let mut steps = vec![
        once(format!("{n1}: kill the bridge"), move |w| {
            let f = w.frame(&n1).expect("frame");
            let child = f.child.expect("child");
            let plugin = *w.x.children(child).first().expect("plugin window");
            let pid = w.x.pid_of(plugin).expect("pid of the plugin window");
            w.note(format!("{n1}: SIGKILL bridge {pid}"));
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }),
        step(format!("{n2}: crashed frame"), move |w| {
            if !matches!(w.status(&n2), DeviceStatus::Crashed(_)) {
                return Ok(Poll::Pending);
            }
            let f = w.frame(&n2).ok_or("frame closed after the crash")?;
            if f.child.is_some() {
                return Ok(Poll::Pending);
            }
            let window = f.window.ok_or("no window")?;
            if !w.x.children(window).is_empty() {
                return Err("the frame still has a child window".into());
            }
            Ok(Poll::Done)
        }),
    ];
    steps.extend(shot(&format!("{shot_name}-crashed")));
    steps.push(once(format!("{n3}: Neu laden"), {
        let n = n3.clone();
        move |w| {
            let device = w.device(&n);
            w.app.dispatch(Action::Plugin(PluginAction::Reload(device)));
        }
    }));
    steps.push(loaded(name));
    steps.push(gui_shown(name));
    steps.extend(shot(&format!("{shot_name}-reloaded")));
    steps
}

/// Closes `name`'s frame and checks that the window and child are gone, the bridge got
/// `GuiClosed` and the device keeps running.
pub fn closed(name: &str) -> Vec<Step> {
    let n1 = name.to_owned();
    let remembered: Arc<Mutex<Option<FrameInfo>>> = Arc::default();
    let r1 = Arc::clone(&remembered);
    let r2 = Arc::clone(&remembered);
    vec![
        once(format!("{n1}: remember the frame"), move |w| {
            *r1.lock().unwrap() = w.frame(&n1);
        }),
        close(name),
        step(format!("{name}: frame gone, GUI closed, device runs"), {
            let n = name.to_owned();
            move |w| {
                let f = r2.lock().unwrap().expect("frame before closing");
                if w.frame(&n).is_some() {
                    return Err("frame still listed".into());
                }
                let window_gone = f.window.is_none_or(|win| !w.x.exists(win));
                let child_gone = f.child.is_none_or(|c| !w.x.exists(c));
                let device = w.device(&n);
                let gui_closed = w.events.iter().any(
                    |e| matches!(e, PluginManagerEvent::GuiClosed { device: d } if *d == device),
                );
                if !(window_gone && child_gone && gui_closed) {
                    return Ok(Poll::Pending);
                }
                if w.status(&n) != DeviceStatus::Ok {
                    return Err(format!("{n}: {:?} after closing", w.status(&n)));
                }
                Ok(Poll::Done)
            }
        }),
    ]
}

/// Switches `name`'s frame to the parameter view and back (what the header's "Ansicht" does): the
/// plugin area hides, the frame becomes resizable with the knob grid, then the plugin returns at
/// its size (locked again if it is fixed).
pub fn view_switch(name: &str, shot_name: &str) -> Vec<Step> {
    let n1 = name.to_owned();
    let n2 = name.to_owned();
    let n3 = name.to_owned();
    let n4 = name.to_owned();
    let set_view = |w: &mut World<'_>, name: &str, view: komponist_app::ui_state::PluginView| {
        let device = w.device(name);
        if let Some(win) = w
            .app
            .ui_mut()
            .plugin_windows
            .iter_mut()
            .find(|win| win.device == device)
        {
            win.view = view;
        }
    };
    let mut steps = vec![
        once(format!("{n1}: parameter view"), move |w| {
            set_view(w, &n1, komponist_app::ui_state::PluginView::Params);
        }),
        step(
            format!("{n2}: plugin area hidden, frame resizable"),
            move |w| {
                let f = w.frame(&n2).ok_or("no frame")?;
                let (Some(window), Some(child)) = (f.window, f.child) else {
                    return Err("no window or child".into());
                };
                let (min, max) = w.x.size_hints(window);
                if w.x.mapped(child) || (min.is_some() && min == max) {
                    return Ok(Poll::Pending);
                }
                w.note(format!(
                    "{n2}: parameter view: child hidden, hints min {min:?} max {max:?}"
                ));
                Ok(Poll::Done)
            },
        ),
    ];
    steps.extend(shot(&format!("{shot_name}-params")));
    steps.push(once(format!("{n3}: plugin view"), move |w| {
        set_view(w, &n3, komponist_app::ui_state::PluginView::Gui);
    }));
    steps.push(step(format!("{n4}: plugin back at its size"), move |w| {
        let f = w.frame(&n4).ok_or("no frame")?;
        let child = f.child.ok_or("no child")?;
        if !w.x.mapped(child) || !frame_settled(w.x, &f) {
            return Ok(Poll::Pending);
        }
        if !f.resizable {
            let (min, max) = w.x.size_hints(f.window.ok_or("no window")?);
            if min.is_none() || min != max {
                return Ok(Poll::Pending);
            }
        }
        Ok(Poll::Done)
    }));
    steps.extend(shot(&format!("{shot_name}-back")));
    steps
}
