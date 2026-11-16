//! The plugin frame window the bridge owns on macOS (#52; spike #42) and Windows (port W6)
//! (ARCHITECTURE §8.4).
//!
//! The app cannot embed another process's `NSView` (macOS), and a parent/child window pair across
//! processes would tie the app's input queue to a plugin's (Windows: a hung plugin would freeze the
//! app), so the bridge opens the frame itself: one eframe window whose top [`metrics::HEADER`]
//! points are the header (drawn with the same code as the app's Linux frame,
//! `komponist_ui::plugin_frame::header`, from the model the app sends) and whose body is a plain
//! container the plugin attaches to: an `NSView`, a subview of winit's view (CLAP `set_parent`
//! cocoa, VST3 `attached` with `kPlatformTypeNSView`; no scale on either), or a child `HWND` of
//! winit's window (CLAP win32, VST3 `kPlatformTypeHWND`, with the window's DPI scale). The app
//! keeps the frame model and every decision: it gets `HeaderAction`, `Shortcut`, `HeaderKey`,
//! `FrameMoved`, `CloseRequested` and answers with `HeaderState` and `CloseGui`.
//!
//! This module is the frame logic both OSes share; `appkit.rs` (macOS) and `win32.rs` (Windows)
//! are the OS halves ([`os`]): container, sizes, activation, "Oben halten", key hook.
//!
//! # Run loop
//!
//! `OpenGui` records a [`Request`] and stops the bridge's outer loop (`CFRunLoopRun` on macOS, the
//! message pump on Windows); the outer loop of `Bridge::run` then calls [`run`], which runs
//! `eframe::run_native` with run-and-return (one winit event loop, reused for every open) until
//! the window closes. Control messages, plugin timers and notifications keep being served inside
//! eframe's event loop and during live resize: on macOS all bridge sources stay in the common
//! modes, on Windows the wake relay posts to a window of the main thread that winit's loop (and
//! the move and size loop, and a plugin's modal dialog) dispatches (`win_loop`). Nothing the
//! protocol needs is driven from egui's `ui()` (eframe skips it for occluded windows, spike #42
//! Q5): the close button, moves and new scales are seen in `logic()`, which runs for hidden
//! windows too, and debounced work (the plugin's size after a drag, `FrameMoved`) is due on the
//! bridge's deadline timer ([`deadline`], [`tick`]).
//!
//! # Sizes
//!
//! The frame works in the plugin's units: points on macOS (CLAP `cocoa` `get_size`, VST3
//! `getSize`; the window follows the screen's backing scale by itself), physical pixels on Windows
//! (CLAP `win32`, VST3 on Windows), where the plugin gets the window's scale (CLAP `set_scale`,
//! VST3 `setContentScaleFactor`) and the header is [`metrics::HEADER`] × scale pixels high. The
//! protocol stays in points: `GuiOpened`, `SetGuiSize`, `GuiSizeRequest` and [`FrameRect`] are
//! divided by the scale. A plugin's resize request sets the window's content size **first**, then
//! the container (the other order lets the container grow too much, spike #42 Q1). A user drag of
//! a resizable plugin's frame stretches the container at once and asks the plugin for the new size
//! [`DRAG_SETTLE`] after the drag rests (CLAP `adjust_size` + `set_size`, VST3
//! `checkSizeConstraint` + `onSize`); the window then snaps to what the plugin took. A fixed-size
//! plugin's frame is not resizable. The frame is at least as wide as the header needs; a narrower
//! plugin is centred. Windows: when the frame moves to a monitor with another DPI, winit keeps its
//! size in points; the plugin gets the new scale and is asked for its size at that scale (a
//! fixed-size plugin reports what it took).
//!
//! # Close
//!
//! The window's close button sends `CloseRequested` (the app answers `CloseGui`, after its state
//! snapshot request). `CloseGui`: the bridge yields its activation to the app, reads the frame's
//! rect, closes the plugin's GUI (`gui_close`: CLAP `destroy`, VST3 `removed`), removes the
//! container, answers `GuiClosed{rect}` and only then closes the window, so the plugin never sees
//! its parent go first.
//!
//! # Off-screen mode
//!
//! `KOMPONIST_FRAME_OFFSCREEN=1` (tests and agents on the owner's machine): frames open at
//! x/y = −20000 and the process never activates itself. macOS: AppKit may not pull them back on
//! screen, and activation is blocked also when a plugin asks ([`os::install_offscreen_mode`]).
//! Windows: no taskbar button, and `ActivateFrame` and the yield are skipped.
//!
//! # Cost
//!
//! egui draws the whole window, header and body, so each open frame costs a wgpu surface and
//! eframe's renderer in its bridge: +40–60 MiB per visible frame on macOS (spike #42), measured on
//! Windows in ARCHITECTURE §8.4.

#[cfg(target_os = "macos")]
mod appkit;
mod keys;
#[cfg(windows)]
mod win32;

#[cfg(target_os = "macos")]
use appkit as os;
#[cfg(windows)]
use win32 as os;

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use egui::{Event, Rect, ViewportCommand, vec2};
use komponist_hosting::GuiSize;
use komponist_i18n::Lang;
use komponist_ipc::frame::{FrameKeyEvent, FrameRect, HeaderModel, KeyMods, keys::Typing};
use komponist_ipc::{BridgeChannel, BridgeMsg, ErrorContext};
use komponist_tokens::color;
use komponist_tokens::egui_support::color32;
use komponist_ui::plugin_frame::{header, metrics, window_title};

use super::Bridge;

/// How long a user's drag of a resizable frame rests before the plugin is asked for the size.
pub(super) const DRAG_SETTLE: Duration = Duration::from_millis(100);

/// How long a frame rests after a move or resize before `FrameMoved` reports it.
const MOVE_SETTLE: Duration = Duration::from_millis(300);

/// The plugin area before the plugin reports its size, points: S05 "820 × 524" (as the app's
/// frame).
const DEFAULT_BODY: (f64, f64) = (820.0, 524.0);

/// Set to `1`: frames open off-screen and nothing activates (module docs).
pub(super) const OFFSCREEN_ENV: &str = "KOMPONIST_FRAME_OFFSCREEN";

/// Where off-screen frames open, points (left of and above every screen).
const OFFSCREEN_POS: f32 = -20_000.0;

/// Whether [`OFFSCREEN_ENV`] is set.
pub(super) fn offscreen() -> bool {
    std::env::var_os(OFFSCREEN_ENV).is_some_and(|v| v == "1")
}

/// Whether `KOMPONIST_LOG_KEYS=1` is set (key decisions and frame events to stderr).
fn log_enabled() -> bool {
    std::env::var_os("KOMPONIST_LOG_KEYS").is_some_and(|v| v == "1")
}

fn log(line: impl AsRef<str>) {
    if log_enabled() {
        eprintln!("komponist-bridge frame: {}", line.as_ref());
    }
}

/// Whether the active process `pid` is komponist: the app `app_pid`, this bridge `own_pid`, or
/// another bridge of the app (`parent` looks up a parent pid; 0 if unknown).
pub(super) fn is_komponist(
    pid: u32,
    app_pid: u32,
    own_pid: u32,
    parent: impl Fn(u32) -> u32,
) -> bool {
    pid == app_pid || pid == own_pid || (app_pid > 1 && parent(pid) == app_pid)
}

/// An `OpenGui{OwnFrame}` waiting for the outer loop.
#[derive(Debug)]
pub(super) struct Request {
    header: HeaderModel,
    lang: Lang,
    rect: Option<FrameRect>,
}

/// The bridge's frame: one requested or open at a time.
#[derive(Default)]
pub(super) struct Slot {
    /// `OpenGui` arrived; the outer loop opens it.
    pub(super) request: Option<Request>,
    /// The open frame.
    pub(super) open: Option<Rc<RefCell<Frame>>>,
}

impl Slot {
    /// A frame is requested, or open and not closing (a closing frame's window may still be
    /// there while the next one is requested; the outer loop opens it once the window is gone).
    pub(super) fn busy(&self) -> bool {
        self.request.is_some() || self.open.as_ref().is_some_and(|f| !f.borrow().closing)
    }
}

/// The frame's native window and the plugin's container in it (the OS's handles, cheap to clone:
/// no borrow of the frame is held while the OS calls back, see [`fit`]).
#[derive(Clone)]
struct Native {
    window: os::Window,
    /// Removed when the plugin's GUI closed.
    container: Option<os::Container>,
}

/// The open frame, shared by the bridge, eframe's app and the key hook (through [`CURRENT`]).
pub(super) struct Frame {
    header: HeaderModel,
    lang: Lang,
    /// Where the app wants it to open.
    initial: Option<FrameRect>,
    channel: std::sync::Arc<BridgeChannel>,
    /// The app (the bridge's parent), to yield the activation back to.
    app_pid: u32,
    ctx: Option<egui::Context>,
    native: Option<Native>,
    /// The plugin's GUI is open in the container.
    gui_open: bool,
    /// The plugin's size, in its units (module docs, "Sizes").
    body: (f64, f64),
    resizable: bool,
    /// Width the header needs, points (0 before the first draw).
    header_width: f64,
    /// Plugin units per point: 1 on macOS, the window's DPI scale on Windows.
    scale: f64,
    /// The content size the bridge set last, plugin units.
    content: (f64, f64),
    /// Closing: the window may go.
    closing: bool,
    /// The plugin is asked for `body` then (after a user drag).
    resize_due: Option<Instant>,
    /// `FrameMoved` is sent then.
    moved_due: Option<Instant>,
    /// The scale changed from this one (another monitor's DPI): the plugin gets the new one.
    rescale_from: Option<f64>,
    /// The viewport's rects seen last by `logic()`.
    seen: Option<(Option<Rect>, Option<Rect>)>,
    /// The typing heuristic of the key policy.
    typing: Typing,
    /// Key codes whose key-down komponist took: their key-up is swallowed too.
    taken: Vec<u32>,
}

thread_local! {
    /// The open frame, for the key hook and the activation observer (main thread).
    static CURRENT: RefCell<Weak<RefCell<Frame>>> = const { RefCell::new(Weak::new()) };
}

impl Frame {
    fn send(&self, msg: &BridgeMsg) {
        // A broken channel ends the bridge through its own next receive.
        let _ = self.channel.send(msg);
    }

    /// The frame's rect now (`None` before the window exists).
    fn rect(&self) -> Option<FrameRect> {
        let native = self.native.as_ref()?;
        Some(os::frame_rect(&native.window))
    }

    /// The header's height in plugin units.
    fn header_units(&self) -> f64 {
        (f64::from(metrics::HEADER) * self.scale).round()
    }

    /// `size` in plugin units as points (the protocol's unit).
    fn points(&self, size: (f64, f64)) -> GuiSize {
        GuiSize {
            width: (size.0 / self.scale).round() as u32,
            height: (size.1 / self.scale).round() as u32,
        }
    }

    /// The content size the frame needs: the plugin's size below the header, at least as wide as
    /// the header.
    fn wanted_content(&self) -> (f64, f64) {
        (
            self.body.0.max(self.header_width * self.scale),
            self.body.1 + self.header_units(),
        )
    }

    /// Places the container below the header, centred when the window is wider than the plugin.
    fn layout(&self) {
        let Some(Native {
            window,
            container: Some(container),
        }) = &self.native
        else {
            return;
        };
        os::layout(window, container, self.body, self.header_units());
    }

    /// The earliest debounced work.
    fn due(&self) -> Option<Instant> {
        let rescale = self.rescale_from.map(|_| Instant::now());
        [self.resize_due, self.moved_due, rescale]
            .into_iter()
            .flatten()
            .min()
    }
}

/// Applies "Oben halten" for whether komponist (the app or one of its bridges) is active. No
/// borrow of the frame is held while the OS changes the window's level.
fn apply_level(frame: &RefCell<Frame>) {
    let (window, keep_on_top, app_pid) = {
        let Ok(f) = frame.try_borrow() else {
            return;
        };
        let Some(native) = &f.native else {
            return;
        };
        (native.window.clone(), f.header.keep_on_top, f.app_pid)
    };
    os::set_level(&window, keep_on_top, os::komponist_active(app_pid));
}

/// Window first, then the container (module docs, "Sizes"). No borrow of `frame` is held while
/// the OS resizes the window: winit may hand the resize to eframe at once, whose `logic()`
/// borrows the frame.
fn fit(frame: &RefCell<Frame>) {
    let (window, content, min, ctx, scale) = {
        let mut f = frame.borrow_mut();
        let Some(native) = &f.native else {
            return;
        };
        let window = native.window.clone();
        let (w, h) = f.wanted_content();
        let changed = (w - f.content.0).abs() > 0.5 || (h - f.content.1).abs() > 0.5;
        if changed {
            f.content = (w, h);
        }
        let min = f.resizable.then(|| {
            (
                (f.header_width * f.scale).max(1.0),
                f.header_units() + f64::from(metrics::MIN_BODY_HEIGHT) * f.scale,
            )
        });
        log(format!(
            "fit: content {w}×{h}, plugin {}×{} (scale {})",
            f.body.0, f.body.1, f.scale
        ));
        (
            window,
            changed.then_some((w, h)),
            min,
            f.ctx.clone(),
            f.scale,
        )
    };
    if let Some(size) = content {
        os::set_content_size(&window, size);
    }
    if let Some(min) = min {
        os::set_min_content_size(&window, ctx.as_ref(), scale, min);
    }
    frame.borrow().layout();
}

// ------------------------------------------------------------------------------------------------
// The bridge's side
// ------------------------------------------------------------------------------------------------

/// `OpenGui{OwnFrame}`: records the frame for the outer loop (which `Bridge::handle` wakes).
pub(super) fn request(
    bridge: &mut Bridge,
    header: HeaderModel,
    lang: &str,
    rect: Option<FrameRect>,
) -> Result<(), String> {
    if bridge.frame.busy() {
        return Err("the GUI is already open".into());
    }
    bridge.frame.request = Some(Request {
        header,
        lang: Lang::from_code(lang).unwrap_or_default(),
        rect,
    });
    bridge.event_loop.stop();
    Ok(())
}

/// `HeaderState`: the header shows `header` in `lang`.
pub(super) fn set_header(bridge: &mut Bridge, header: HeaderModel, lang: &str) {
    let lang = Lang::from_code(lang).unwrap_or_default();
    if let Some(request) = bridge.frame.request.as_mut() {
        request.header = header;
        request.lang = lang;
        return;
    }
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    {
        let mut f = frame.borrow_mut();
        let title = window_title(&header);
        let retitle = window_title(&f.header) != title;
        f.header = header;
        f.lang = lang;
        if let Some(ctx) = &f.ctx {
            if retitle {
                ctx.send_viewport_cmd(ViewportCommand::Title(title));
            }
            ctx.request_repaint();
        }
    }
    apply_level(&frame);
}

/// `ActivateFrame`: the app yielded its activation to this process; take it and come forward.
pub(super) fn activate(bridge: &mut Bridge) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    let window = frame.borrow().native.as_ref().map(|n| n.window.clone());
    if let Some(window) = window {
        os::activate(&window);
    }
}

/// `RaiseFrame`: komponist became active; the frame comes to the front of its level, the bridge
/// stays inactive and the frame does not take the keyboard (the app's main window keeps it).
pub(super) fn raise(bridge: &mut Bridge) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    let window = {
        let f = frame.borrow();
        if f.closing || !f.gui_open {
            return;
        }
        f.native.as_ref().map(|n| n.window.clone())
    };
    if let Some(window) = window
        && os::raise(&window)
    {
        log("raised");
    }
}

/// `SetGuiSize` for the own frame: the body takes `size` (points) as after a user drag.
pub(super) fn set_body_size(bridge: &mut Bridge, size: GuiSize) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    let (resizable, kept) = {
        let f = frame.borrow();
        (f.resizable, f.points(f.body))
    };
    if !resizable {
        frame.borrow().send(&BridgeMsg::GuiSizeRequest(kept));
        return;
    }
    {
        let mut f = frame.borrow_mut();
        f.body = (
            f64::from(size.width) * f.scale,
            f64::from(size.height) * f.scale,
        );
    }
    fit(&frame);
    ask_plugin(bridge, &frame);
}

/// The plugin asked for `size` (its units): window first, then the container.
pub(super) fn plugin_resized(bridge: &mut Bridge, size: GuiSize) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    {
        let mut f = frame.borrow_mut();
        f.body = (f64::from(size.width), f64::from(size.height));
        f.resize_due = None;
        f.moved_due = Some(Instant::now() + MOVE_SETTLE);
    }
    fit(&frame);
}

/// Whether the plugin's GUI lives in the bridge's own frame.
pub(super) fn owns_gui(bridge: &Bridge) -> bool {
    bridge.frame.busy()
}

/// `CloseGui`, the plugin closed its GUI, or shutdown: closes the plugin's GUI, answers
/// `GuiClosed{rect}` (unless `quiet`) and then the window (module docs, "Close").
pub(super) fn close(bridge: &mut Bridge, quiet: bool) {
    if bridge.frame.request.take().is_some() {
        if !quiet {
            bridge.send(BridgeMsg::GuiClosed { rect: None });
        }
        return;
    }
    let Some(frame) = bridge.frame.open.clone() else {
        if !quiet {
            bridge.send(BridgeMsg::GuiClosed { rect: None });
        }
        return;
    };
    let (rect, was_open, ctx, app_pid) = {
        let mut f = frame.borrow_mut();
        if f.closing {
            return;
        }
        f.closing = true;
        f.resize_due = None;
        f.moved_due = None;
        f.rescale_from = None;
        (
            f.rect(),
            std::mem::take(&mut f.gui_open),
            f.ctx.clone(),
            f.app_pid,
        )
    };
    os::yield_to(app_pid);
    if was_open && let Some(plugin) = bridge.plugin.as_mut() {
        plugin.gui_close();
    }
    let container = frame
        .borrow_mut()
        .native
        .as_mut()
        .and_then(|n| n.container.take());
    if let Some(container) = container {
        os::remove_container(container);
    }
    if !quiet {
        bridge.send(BridgeMsg::GuiClosed { rect });
    }
    log(format!("closed at {rect:?}"));
    // `logic()` sends the window's close until eframe ends (it takes two passes).
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}

/// The frame's next debounced work, for the bridge's deadline timer.
pub(super) fn deadline(bridge: &Bridge) -> Option<Instant> {
    bridge.frame.open.as_ref().and_then(|f| f.borrow().due())
}

/// Debounced work that is due: a new scale, the plugin's size after a drag, `FrameMoved`.
pub(super) fn tick(bridge: &mut Bridge) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    let rescale = frame.borrow_mut().rescale_from.take();
    if let Some(from) = rescale {
        rescale_plugin(bridge, &frame, from);
    }
    let now = Instant::now();
    let resize = {
        let mut f = frame.borrow_mut();
        let due = f.resize_due.is_some_and(|d| d <= now);
        if due {
            f.resize_due = None;
        }
        due
    };
    if resize {
        ask_plugin(bridge, &frame);
    }
    let mut f = frame.borrow_mut();
    if f.moved_due.is_some_and(|d| d <= now) {
        f.moved_due = None;
        if let Some(rect) = f.rect() {
            f.send(&BridgeMsg::FrameMoved(rect));
        }
    }
}

/// Asks the plugin to take the frame's body size and follows what it took.
fn ask_plugin(bridge: &mut Bridge, frame: &Rc<RefCell<Frame>>) {
    let (body, open) = {
        let f = frame.borrow();
        (f.body, f.gui_open)
    };
    if !open {
        return;
    }
    let Some(plugin) = bridge.plugin.as_mut() else {
        return;
    };
    let wanted = GuiSize {
        width: body.0.round().max(1.0) as u32,
        height: body.1.round().max(1.0) as u32,
    };
    let taken = plugin.gui_set_size(wanted);
    log(format!("user size {wanted:?} -> plugin {taken:?}"));
    {
        let mut f = frame.borrow_mut();
        if let Ok(taken) = taken {
            f.body = (f64::from(taken.width), f64::from(taken.height));
        }
        f.moved_due = Some(Instant::now() + MOVE_SETTLE);
    }
    fit(frame);
}

/// The frame moved to a monitor with another scale (Windows; `logic()` set the new one): the
/// plugin gets it and is asked for its size at that scale; a fixed-size plugin reports its own.
fn rescale_plugin(bridge: &mut Bridge, frame: &Rc<RefCell<Frame>>, from: f64) {
    let (body, open, scale, resizable) = {
        let f = frame.borrow();
        (f.body, f.gui_open, f.scale, f.resizable)
    };
    if !open {
        return;
    }
    let Some(plugin) = bridge.plugin.as_mut() else {
        return;
    };
    let took = plugin.gui_set_scale(scale);
    let ratio = scale / from;
    let wanted = if resizable {
        GuiSize {
            width: (body.0 * ratio).round().max(1.0) as u32,
            height: (body.1 * ratio).round().max(1.0) as u32,
        }
    } else {
        // A fixed-size plugin keeps its size: `gui_set_size` reports what it has now.
        GuiSize {
            width: body.0.round().max(1.0) as u32,
            height: body.1.round().max(1.0) as u32,
        }
    };
    let taken = plugin.gui_set_size(wanted);
    log(format!(
        "scale {from} -> {scale} (plugin took it: {took}): {wanted:?} -> {taken:?}"
    ));
    {
        let mut f = frame.borrow_mut();
        f.body = match taken {
            Ok(taken) => (f64::from(taken.width), f64::from(taken.height)),
            Err(_) => (f64::from(wanted.width), f64::from(wanted.height)),
        };
        f.moved_due = Some(Instant::now() + MOVE_SETTLE);
    }
    fit(frame);
}

// ------------------------------------------------------------------------------------------------
// The window
// ------------------------------------------------------------------------------------------------

/// Runs the frame of `request` until its window closes (module docs, "Run loop").
pub(super) fn run(bridge: &Rc<RefCell<Bridge>>, request: Request) {
    let (channel, quiet) = {
        let b = bridge.borrow();
        (b.channel.clone(), offscreen())
    };
    if quiet {
        os::install_offscreen_mode();
    }
    let frame = Rc::new(RefCell::new(Frame {
        header: request.header,
        lang: request.lang,
        initial: request.rect,
        channel,
        app_pid: os::app_pid(),
        ctx: None,
        native: None,
        gui_open: false,
        // Points until the window's scale is known (`create`).
        body: request.rect.map_or(DEFAULT_BODY, |r| {
            (
                f64::from(r.width),
                f64::from(r.height) - f64::from(metrics::HEADER),
            )
        }),
        resizable: true,
        header_width: 0.0,
        scale: 1.0,
        content: (0.0, 0.0),
        closing: false,
        resize_due: None,
        moved_due: None,
        rescale_from: None,
        seen: None,
        typing: Typing::default(),
        taken: Vec::new(),
    }));
    bridge.borrow_mut().frame.open = Some(frame.clone());
    CURRENT.with(|c| *c.borrow_mut() = Rc::downgrade(&frame));
    let options = native_options(&frame.borrow(), quiet);
    let weak = Rc::downgrade(bridge);
    let app_frame = frame.clone();
    let result = eframe::run_native(
        "komponist-bridge",
        options,
        Box::new(move |cc| {
            create(cc, &weak, &app_frame);
            Ok(Box::new(FrameApp {
                frame: app_frame,
                bridge: weak,
            }))
        }),
    );
    if let Err(e) = result {
        eprintln!("komponist-bridge: plugin frame: {e}");
    }
    log("window closed");
    // The window is gone (or never came): nothing of the plugin may stay attached to it.
    let mut b = bridge.borrow_mut();
    let still_open = std::mem::take(&mut frame.borrow_mut().gui_open);
    if still_open {
        if let Some(plugin) = b.plugin.as_mut() {
            plugin.gui_close();
        }
        let rect = frame.borrow().rect();
        b.send(BridgeMsg::GuiClosed { rect });
    }
    b.frame.open = None;
    CURRENT.with(|c| *c.borrow_mut() = Weak::new());
    os::unhook_keys();
    {
        let mut f = frame.borrow_mut();
        f.ctx = None;
        f.native = None;
    }
    b.drain_ready();
}

fn native_options(frame: &Frame, quiet: bool) -> eframe::NativeOptions {
    let size = frame.initial.map_or(
        vec2(
            DEFAULT_BODY.0 as f32,
            DEFAULT_BODY.1 as f32 + metrics::HEADER,
        ),
        |r| vec2(r.width, r.height),
    );
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(window_title(&frame.header))
        .with_inner_size(size)
        // The app activates the frame (`ActivateFrame` after its yield), never winit.
        .with_active(false);
    let position = if quiet {
        Some(egui::pos2(OFFSCREEN_POS, OFFSCREEN_POS))
    } else {
        frame.initial.map(|r| egui::pos2(r.x, r.y))
    };
    if let Some(position) = position {
        viewport = viewport.with_position(position);
    }
    eframe::NativeOptions {
        viewport: os::viewport(viewport, quiet),
        renderer: eframe::Renderer::Wgpu,
        run_and_return: true,
        centered: position.is_none(),
        event_loop_builder: os::event_loop_hook(),
        ..Default::default()
    }
}

/// eframe's app creator: the container below the header and the plugin in it.
fn create(
    cc: &eframe::CreationContext<'_>,
    bridge: &Weak<RefCell<Bridge>>,
    frame: &Rc<RefCell<Frame>>,
) {
    let ctx = cc.egui_ctx.clone();
    komponist_ui::install(&ctx);
    komponist_ui::set_lang(&ctx, frame.borrow().lang);
    // egui draws below the plugin's view (winit's Metal layer on macOS, the parent window's
    // swapchain below a child `HWND` on Windows): a tooltip below a header control would be hidden
    // behind the plugin (owner test, #52). Header tooltips go beside their control.
    komponist_ui::tooltip::set_hover_side(&ctx, komponist_ui::tooltip::HoverSide::Beside);
    let body_points = frame.borrow().body;
    let (window, container, scale) = match os::attach(cc, body_points) {
        Ok(attached) => attached,
        Err(message) => {
            fail(frame, &ctx, message);
            return;
        }
    };
    {
        let mut f = frame.borrow_mut();
        f.ctx = Some(ctx.clone());
        f.scale = scale;
        f.body = (body_points.0 * scale, body_points.1 * scale);
        f.native = Some(Native {
            window: window.clone(),
            container: Some(container.clone()),
        });
    }
    os::install_hooks();
    let parent = os::parent(&container, scale);
    let opened = match bridge.upgrade() {
        Some(bridge) => {
            let mut b = bridge.borrow_mut();
            let opened = match b.plugin.as_mut() {
                Some(plugin) => plugin.gui_open(parent).map_err(|e| e.to_string()),
                None => Err("no plugin loaded".into()),
            };
            drop(b);
            opened
        }
        None => Err("the bridge is gone".into()),
    };
    match opened {
        Ok(info) => {
            {
                let mut f = frame.borrow_mut();
                f.gui_open = true;
                f.body = (f64::from(info.size.width), f64::from(info.size.height));
                f.resizable = info.resizable;
            }
            // After the plugin's own hooks (Windows: thread hooks run newest first).
            os::hook_keys();
            fit(frame);
            apply_level(frame);
            let f = frame.borrow();
            f.send(&BridgeMsg::GuiOpened {
                size: f.points(f.body),
                resizable: info.resizable,
            });
            f.send(&BridgeMsg::FrameWindow(os::window_id(&window)));
            log(format!(
                "opened: plugin {}×{}, resizable {}, scale {scale}, {}",
                info.size.width,
                info.size.height,
                info.resizable,
                os::describe(&window, &container),
            ));
            ctx.send_viewport_cmd(ViewportCommand::Resizable(info.resizable));
            // Windows keeps the maximize button of a window that cannot be resized.
            #[cfg(windows)]
            ctx.send_viewport_cmd(ViewportCommand::EnableButtons {
                close: true,
                minimized: true,
                maximize: info.resizable,
            });
        }
        Err(message) => fail(frame, &ctx, message),
    }
    if let Some(bridge) = bridge.upgrade()
        && let Ok(mut b) = bridge.try_borrow_mut()
    {
        b.reschedule();
    }
}

/// The GUI did not open: `Error` for the app (which shows the parameters), and the window goes.
fn fail(frame: &Rc<RefCell<Frame>>, ctx: &egui::Context, message: String) {
    let container = {
        let mut f = frame.borrow_mut();
        f.closing = true;
        f.send(&BridgeMsg::Error {
            context: ErrorContext::Gui,
            message,
            fatal: false,
        });
        f.native.as_mut().and_then(|n| n.container.take())
    };
    if let Some(container) = container {
        os::remove_container(container);
    }
    ctx.request_repaint();
}

/// eframe's side of the frame.
struct FrameApp {
    frame: Rc<RefCell<Frame>>,
    bridge: Weak<RefCell<Bridge>>,
}

impl FrameApp {
    /// Lets the bridge re-arm its timers for new debounced work (if it is not busy: the busy
    /// caller re-arms itself).
    fn reschedule(&self) {
        if let Some(bridge) = self.bridge.upgrade()
            && let Ok(mut b) = bridge.try_borrow_mut()
        {
            b.reschedule();
        }
    }
}

impl eframe::App for FrameApp {
    /// Runs for hidden and occluded windows too: the close button, moves, user resizes and new
    /// scales.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut schedule = false;
        {
            let mut f = self.frame.borrow_mut();
            if f.closing {
                // Closing (`close`, `fail`): the window goes. A `Close` command takes effect in
                // the next pass, so keep asking for passes until eframe returns.
                ctx.send_viewport_cmd(ViewportCommand::Close);
                ctx.request_repaint();
                return;
            }
            if ctx.input(|i| i.viewport().close_requested()) {
                // The window's close button: the app decides (and answers `CloseGui`).
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                f.send(&BridgeMsg::CloseRequested);
            }
            let rects = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
            if f.seen.is_some_and(|seen| seen != rects) && !f.closing {
                f.moved_due = Some(Instant::now() + MOVE_SETTLE);
                schedule = true;
            }
            f.seen = Some(rects);
            let native = f.native.clone();
            let scale = native.as_ref().map(|n| os::scale(&n.window));
            if let Some(scale) = scale
                && f.gui_open
                && (scale - f.scale).abs() > 1e-3
            {
                // Another monitor's DPI (Windows): the plugin follows from the bridge's tick.
                log(format!("scale {} -> {scale}", f.scale));
                f.rescale_from = Some(f.rescale_from.unwrap_or(f.scale));
                f.scale = scale;
                f.content = (0.0, 0.0);
                f.resize_due = None;
                schedule = true;
            } else if let (Some(native), Some(inner)) = (native, rects.1)
                && f.gui_open
                && f.resizable
                && f.rescale_from.is_none()
            {
                let (w, h) = os::content_size(&native.window, inner);
                if (w - f.content.0).abs() > 0.5 || (h - f.content.1).abs() > 0.5 {
                    // A user drag: the container stretches now, the plugin follows when it rests.
                    f.content = (w, h);
                    f.body = (w, (h - f.header_units()).max(1.0));
                    f.layout();
                    f.resize_due = Some(Instant::now() + DRAG_SETTLE);
                    schedule = true;
                }
            }
        }
        if schedule {
            self.reschedule();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (model, lang) = {
            let f = self.frame.borrow();
            (f.header.clone(), f.lang)
        };
        komponist_ui::set_lang(&ctx, lang);
        // The root node carries the language for screen readers (BCP 47), as in the app.
        ctx.accesskit_node_builder(egui::accesskit_root_id(), |n| n.set_language(lang.code()));
        let rect = ui.max_rect();
        let header_rect = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::HEADER));
        let body_rect = Rect::from_min_max(egui::pos2(rect.left(), header_rect.bottom()), rect.max);
        // The body under the plugin: `surface.0`, as the app's frame (no flash on live resize).
        ui.painter()
            .rect_filled(body_rect, 0.0, color32(color::SURFACE_0));
        let out = header::show(ui, header_rect, &model);
        let keys = header_keys(&ctx);
        let refit = {
            let mut f = self.frame.borrow_mut();
            let wider = (f64::from(out.natural_width) - f.header_width).abs() > 0.5;
            if wider {
                f.header_width = f64::from(out.natural_width);
            }
            wider && !f.closing
        };
        if refit {
            fit(&self.frame);
        }
        let f = self.frame.borrow();
        for event in out.events {
            log(format!("header: {event:?}"));
            f.send(&BridgeMsg::HeaderAction(event));
        }
        for key in keys {
            log(format!("header key: {key:?}"));
            f.send(&BridgeMsg::HeaderKey(key));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(color32(color::SURFACE_0)).to_array()
    }
}

/// Key events of the header for the app (the header has the keyboard focus), unless a widget's
/// text entry has them; the same filter as the app's frame (`frame_ui`).
fn header_keys(ctx: &egui::Context) -> Vec<FrameKeyEvent> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    let widget_keys = komponist_ui::focus::is_visible(ctx) && ctx.memory(|m| m.focused().is_some());
    ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                Event::Key {
                    key,
                    physical_key,
                    pressed,
                    repeat,
                    modifiers,
                } => {
                    let widget_key = matches!(
                        key,
                        egui::Key::Space
                            | egui::Key::Enter
                            | egui::Key::Tab
                            | egui::Key::ArrowUp
                            | egui::Key::ArrowDown
                            | egui::Key::ArrowLeft
                            | egui::Key::ArrowRight
                    );
                    (!(widget_keys && widget_key)).then(|| FrameKeyEvent {
                        key: key.name().to_owned(),
                        physical_key: physical_key.map(|k| k.name().to_owned()),
                        pressed: *pressed,
                        repeat: *repeat,
                        mods: KeyMods {
                            alt: modifiers.alt,
                            ctrl: modifiers.ctrl,
                            shift: modifiers.shift,
                            mac_cmd: modifiers.mac_cmd,
                            command: modifiers.command,
                        },
                    })
                }
                _ => None,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn komponist_is_the_app_this_bridge_or_a_sibling_bridge() {
        let parent = |pid: u32| if pid == 300 { 100 } else { 1 };
        assert!(is_komponist(100, 100, 200, parent));
        assert!(is_komponist(200, 100, 200, parent));
        assert!(is_komponist(300, 100, 200, parent));
        assert!(!is_komponist(400, 100, 200, parent));
        // Without a real parent (launchd), no stranger counts as a sibling.
        assert!(!is_komponist(400, 1, 200, |_| 1));
    }
}
