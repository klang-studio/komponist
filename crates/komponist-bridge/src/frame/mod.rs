//! The plugin frame window the bridge owns on macOS (#52; spike #42, ARCHITECTURE §8.4).
//!
//! The app cannot embed another process's `NSView`, so on macOS the bridge opens the frame itself:
//! one eframe window whose top [`HEADER`] points are the header (drawn with the same code as the
//! app's Linux frame, `komponist_ui::plugin_frame::header`, from the model the app sends) and whose
//! body is a plain container `NSView`, a subview of winit's view, that the plugin attaches to
//! (CLAP `set_parent` cocoa, VST3 `attached` with `kPlatformTypeNSView`; no scale on either). The
//! app keeps the frame model and every decision: it gets `HeaderAction`, `Shortcut`, `HeaderKey`,
//! `FrameMoved`, `CloseRequested` and answers with `HeaderState` and `CloseGui`.
//!
//! # Run loop
//!
//! `OpenGui` records a [`Request`] and stops `CFRunLoopRun`; the outer loop of
//! `Bridge::run` then calls [`run`], which runs `eframe::run_native` with run-and-return (one winit
//! event loop, reused for every open) until the window closes. All bridge sources stay in the
//! common modes, so control messages, plugin timers and notifications keep being served inside
//! `[NSApp run]` and during live resize. Nothing the protocol needs is driven from egui's `ui()`
//! (eframe skips it for occluded windows, spike #42 Q5): the close button and moves are seen in
//! `logic()`, which runs for hidden windows too, and debounced work (the plugin's size after a
//! drag, `FrameMoved`) is due on the bridge's deadline timer ([`deadline`], [`tick`]).
//!
//! # Sizes
//!
//! Plugins size in points (CLAP `cocoa` `get_size`, VST3 `getSize`); the window follows the
//! screen's backing scale by itself. A plugin's resize request sets the window's content size
//! **first**, then the container (the other order lets the container grow too much, spike #42 Q1).
//! A user drag of a resizable plugin's frame stretches the container at once and asks the plugin
//! for the new size [`DRAG_SETTLE`] after the drag rests (CLAP `adjust_size` + `set_size`, VST3
//! `checkSizeConstraint` + `onSize`); the window then snaps to what the plugin took. A fixed-size
//! plugin's frame is not resizable. The frame is at least as wide as the header needs; a narrower
//! plugin is centred.
//!
//! # Close
//!
//! The traffic light sends `CloseRequested` (the app answers `CloseGui`, after its state
//! snapshot request). `CloseGui`: the bridge yields its activation to the app, reads the frame's
//! rect, closes the plugin's GUI (`gui_close`: CLAP `destroy`, VST3 `removed`), removes the
//! container, answers `GuiClosed{rect}` and only then closes the window, so the plugin never sees
//! its parent go first.
//!
//! # Off-screen mode
//!
//! `KOMPONIST_FRAME_OFFSCREEN=1` (tests and agents on the owner's Mac): frames open at
//! x/y = −20000, AppKit may not pull them back on screen, and the process can never activate
//! itself, also when a plugin asks ([`appkit::install_offscreen_mode`]).

mod appkit;
mod keys;

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use egui::{Event, Rect, ViewportCommand, vec2};
use komponist_hosting::{GuiParent, GuiSize};
use komponist_i18n::Lang;
use komponist_ipc::frame::{FrameKeyEvent, FrameRect, HeaderModel, KeyMods, keys::Typing};
use komponist_ipc::{BridgeChannel, BridgeMsg, ErrorContext};
use komponist_tokens::color;
use komponist_tokens::egui_support::color32;
use komponist_ui::plugin_frame::{header, metrics, window_title};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::Bridge;

/// Height of the header, points.
const HEADER: f64 = metrics::HEADER as f64;

/// How long a user's drag of a resizable frame rests before the plugin is asked for the size.
pub(super) const DRAG_SETTLE: Duration = Duration::from_millis(100);

/// How long a frame rests after a move or resize before `FrameMoved` reports it.
const MOVE_SETTLE: Duration = Duration::from_millis(300);

/// The plugin area before the plugin reports its size: S05 "820 × 524" (as the app's frame).
const DEFAULT_BODY: (f64, f64) = (820.0, 524.0);

/// Set to `1`: frames open off-screen and nothing activates (module docs).
pub(super) const OFFSCREEN_ENV: &str = "KOMPONIST_FRAME_OFFSCREEN";

/// Where off-screen frames open (left of and above every screen).
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

/// The open frame, shared by the bridge, eframe's app and the key monitor (through [`CURRENT`]).
pub(super) struct Frame {
    header: HeaderModel,
    lang: Lang,
    /// Where the app wants it to open.
    initial: Option<FrameRect>,
    channel: Rc<BridgeChannel>,
    /// The app (the bridge's parent), to yield the activation back to.
    app_pid: i32,
    ctx: Option<egui::Context>,
    window: Option<Retained<NSWindow>>,
    container: Option<Retained<NSView>>,
    /// The plugin's GUI is open in the container.
    gui_open: bool,
    /// The plugin's size, points.
    body: (f64, f64),
    resizable: bool,
    /// Width the header needs, points (0 before the first draw).
    header_width: f64,
    /// The content size the bridge set last, points.
    content: (f64, f64),
    /// Closing: the window may go.
    closing: bool,
    /// The plugin is asked for `body` then (after a user drag).
    resize_due: Option<Instant>,
    /// `FrameMoved` is sent then.
    moved_due: Option<Instant>,
    /// The viewport's rects seen last by `logic()`.
    seen: Option<(Option<Rect>, Option<Rect>)>,
    /// The typing heuristic of the key policy.
    typing: Typing,
    /// Key codes whose key-down komponist took: their key-up is swallowed too.
    taken: Vec<u16>,
}

thread_local! {
    /// The open frame, for the key monitor and the activation observer (main thread).
    static CURRENT: RefCell<Weak<RefCell<Frame>>> = const { RefCell::new(Weak::new()) };
}

impl Frame {
    fn send(&self, msg: &BridgeMsg) {
        // A broken channel ends the bridge through its own next receive.
        let _ = self.channel.send(msg);
    }

    /// The frame's rect now (`None` before the window exists).
    fn rect(&self) -> Option<FrameRect> {
        let window = self.window.as_ref()?;
        Some(appkit::frame_rect(window))
    }

    /// The content size the frame needs: the plugin's size below the header, at least as wide as
    /// the header.
    fn wanted_content(&self) -> (f64, f64) {
        (self.body.0.max(self.header_width), self.body.1 + HEADER)
    }

    /// Places the container below the header, centred when the window is wider than the plugin.
    fn layout(&self) {
        let (Some(window), Some(container)) = (&self.window, &self.container) else {
            return;
        };
        let Some(content) = window.contentView() else {
            return;
        };
        let size = content.frame().size;
        let x = ((size.width - self.body.0) / 2.0).max(0.0).floor();
        // winit's view is flipped: y grows downwards from the header's top.
        let y = if content.isFlipped() {
            HEADER
        } else {
            size.height - HEADER - self.body.1
        };
        container.setFrame(NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(self.body.0, self.body.1),
        ));
    }

    /// The earliest debounced work.
    fn due(&self) -> Option<Instant> {
        match (self.resize_due, self.moved_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Applies "Oben halten" for whether komponist (the app or one of its bridges) is active.
    fn apply_level(&self) {
        if let Some(window) = &self.window {
            appkit::set_floating(
                window,
                self.header.keep_on_top && appkit::komponist_active(self.app_pid),
            );
        }
    }
}

/// Window first, then the container (module docs, "Sizes"). No borrow of `frame` is held while
/// AppKit resizes the window: winit may hand the resize to eframe at once, whose `logic()`
/// borrows the frame.
fn fit(frame: &RefCell<Frame>) {
    let (window, content, min) = {
        let mut f = frame.borrow_mut();
        let Some(window) = f.window.clone() else {
            return;
        };
        let (w, h) = f.wanted_content();
        let changed = (w - f.content.0).abs() > 0.5 || (h - f.content.1).abs() > 0.5;
        if changed {
            f.content = (w, h);
        }
        let min = f.resizable.then(|| {
            NSSize::new(
                f.header_width.max(1.0),
                HEADER + f64::from(metrics::MIN_BODY_HEIGHT),
            )
        });
        log(format!(
            "fit: content {w}×{h} pt, plugin {}×{} pt",
            f.body.0, f.body.1
        ));
        (window, changed.then(|| NSSize::new(w, h)), min)
    };
    if let Some(size) = content {
        window.setContentSize(size);
    }
    if let Some(min) = min {
        window.setContentMinSize(min);
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
    let mut f = frame.borrow_mut();
    let title = window_title(&header);
    let retitle = window_title(&f.header) != title;
    f.header = header;
    f.lang = lang;
    f.apply_level();
    if let Some(ctx) = &f.ctx {
        if retitle {
            ctx.send_viewport_cmd(ViewportCommand::Title(title));
        }
        ctx.request_repaint();
    }
}

/// `ActivateFrame`: the app yielded its activation to this process; take it and come forward.
pub(super) fn activate(bridge: &mut Bridge) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    let window = frame.borrow().window.clone();
    if let Some(window) = window {
        appkit::activate(&window);
    }
}

/// `SetGuiSize` for the own frame: the body takes `size` (points) as after a user drag.
pub(super) fn set_body_size(bridge: &mut Bridge, size: GuiSize) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
    let resizable = frame.borrow().resizable;
    if !resizable {
        frame.borrow().send(&BridgeMsg::GuiSizeRequest(GuiSize {
            width: frame.borrow().body.0.round() as u32,
            height: frame.borrow().body.1.round() as u32,
        }));
        return;
    }
    frame.borrow_mut().body = (f64::from(size.width), f64::from(size.height));
    fit(&frame);
    ask_plugin(bridge, &frame);
}

/// The plugin asked for `size` (points): window first, then the container.
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
    let (rect, was_open, ctx) = {
        let mut f = frame.borrow_mut();
        if f.closing {
            return;
        }
        f.closing = true;
        f.resize_due = None;
        f.moved_due = None;
        appkit::yield_to(f.app_pid);
        (f.rect(), std::mem::take(&mut f.gui_open), f.ctx.clone())
    };
    if was_open && let Some(plugin) = bridge.plugin.as_mut() {
        plugin.gui_close();
    }
    let container = frame.borrow_mut().container.take();
    if let Some(container) = container {
        container.removeFromSuperview();
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

/// Debounced work that is due: the plugin's size after a drag, `FrameMoved`.
pub(super) fn tick(bridge: &mut Bridge) {
    let Some(frame) = bridge.frame.open.clone() else {
        return;
    };
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
        appkit::install_offscreen_mode();
    }
    // SAFETY: plain syscall.
    let app_pid = unsafe { libc::getppid() };
    let frame = Rc::new(RefCell::new(Frame {
        header: request.header,
        lang: request.lang,
        initial: request.rect,
        channel,
        app_pid,
        ctx: None,
        window: None,
        container: None,
        gui_open: false,
        body: request.rect.map_or(DEFAULT_BODY, |r| {
            (f64::from(r.width), f64::from(r.height) - HEADER)
        }),
        resizable: true,
        header_width: 0.0,
        content: (0.0, 0.0),
        closing: false,
        resize_due: None,
        moved_due: None,
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
    {
        let mut f = frame.borrow_mut();
        f.ctx = None;
        f.container = None;
        f.window = None;
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
        viewport,
        renderer: eframe::Renderer::Wgpu,
        run_and_return: true,
        centered: position.is_none(),
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            // No Dock icon, no menu bar of its own, no activation by winit (spike #42 Q3). The
            // hook runs once: run-and-return reuses the event loop.
            builder
                .with_activation_policy(ActivationPolicy::Accessory)
                .with_default_menu(false)
                .with_activate_ignoring_other_apps(false);
        })),
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
    // egui draws in winit's Metal layer, below the plugin's view: a tooltip below a header control
    // would be hidden behind the plugin (owner test, #52). Header tooltips go beside their control.
    komponist_ui::tooltip::set_hover_side(&ctx, komponist_ui::tooltip::HoverSide::Beside);
    let view = match cc.window_handle().map(|h| h.as_raw()) {
        // SAFETY: winit's view of this window, alive while the window is.
        Ok(RawWindowHandle::AppKit(h)) => unsafe { h.ns_view.cast::<NSView>().as_ref() },
        _ => {
            fail(bridge, frame, &ctx, "no AppKit window".into());
            return;
        }
    };
    let Some(window) = view.window() else {
        fail(bridge, frame, &ctx, "the view has no window".into());
        return;
    };
    let mtm = MainThreadMarker::new().expect("the bridge's frame runs on the main thread");
    // Never replace winit's content view (winit casts it); the container is a subview of it.
    let body = frame.borrow().body;
    let container = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, HEADER), NSSize::new(body.0, body.1)),
    );
    view.addSubview(&container);
    {
        let mut f = frame.borrow_mut();
        f.ctx = Some(ctx.clone());
        f.window = Some(window.clone());
        f.container = Some(container.clone());
    }
    keys::install();
    appkit::watch_activation();
    let parent = Retained::as_ptr(&container).cast_mut().cast::<c_void>();
    let opened = match bridge.upgrade() {
        Some(bridge) => {
            let mut b = bridge.borrow_mut();
            let opened = match b.plugin.as_mut() {
                Some(plugin) => plugin
                    .gui_open(GuiParent::Cocoa(parent))
                    .map_err(|e| e.to_string()),
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
            fit(frame);
            let f = frame.borrow();
            f.apply_level();
            f.send(&BridgeMsg::GuiOpened {
                size: info.size,
                resizable: info.resizable,
            });
            log(format!(
                "opened: plugin {}×{} pt, resizable {}, backing scale {}, plugin view {}",
                info.size.width,
                info.size.height,
                info.resizable,
                window.backingScaleFactor(),
                container
                    .subviews()
                    .firstObject()
                    .map_or("none".into(), |v| v
                        .class()
                        .name()
                        .to_string_lossy()
                        .into_owned())
            ));
            ctx.send_viewport_cmd(ViewportCommand::Resizable(info.resizable));
        }
        Err(message) => fail(bridge, frame, &ctx, message),
    }
    if let Some(bridge) = bridge.upgrade()
        && let Ok(mut b) = bridge.try_borrow_mut()
    {
        b.drain_ready();
    }
}

/// The GUI did not open: `Error` for the app (which shows the parameters), and the window goes.
fn fail(
    bridge: &Weak<RefCell<Bridge>>,
    frame: &Rc<RefCell<Frame>>,
    ctx: &egui::Context,
    message: String,
) {
    {
        let mut f = frame.borrow_mut();
        f.closing = true;
        f.send(&BridgeMsg::Error {
            context: ErrorContext::Gui,
            message,
            fatal: false,
        });
        if let Some(container) = f.container.take() {
            container.removeFromSuperview();
        }
    }
    let _ = bridge;
    ctx.request_repaint();
}

/// eframe's side of the frame.
struct FrameApp {
    frame: Rc<RefCell<Frame>>,
    bridge: Weak<RefCell<Bridge>>,
}

impl FrameApp {
    /// Lets the bridge handle what queued meanwhile and re-arm its timers (if it is not busy:
    /// the busy caller drains itself).
    fn drain(&self) {
        if let Some(bridge) = self.bridge.upgrade()
            && let Ok(mut b) = bridge.try_borrow_mut()
        {
            b.drain_ready();
        }
    }
}

impl eframe::App for FrameApp {
    /// Runs for hidden and occluded windows too: the close button, moves and user resizes.
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
                // The traffic light: the app decides (and answers `CloseGui`).
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                f.send(&BridgeMsg::CloseRequested);
            }
            let rects = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
            if f.seen.is_some_and(|seen| seen != rects) && !f.closing {
                f.moved_due = Some(Instant::now() + MOVE_SETTLE);
                schedule = true;
            }
            f.seen = Some(rects);
            if let Some(inner) = rects.1
                && f.gui_open
                && f.resizable
                && !f.closing
            {
                let (w, h) = (f64::from(inner.width()), f64::from(inner.height()));
                if (w - f.content.0).abs() > 0.5 || (h - f.content.1).abs() > 0.5 {
                    // A user drag: the container stretches now, the plugin follows when it rests.
                    f.content = (w, h);
                    f.body = (w, (h - HEADER).max(1.0));
                    f.layout();
                    f.resize_due = Some(Instant::now() + DRAG_SETTLE);
                    schedule = true;
                }
            }
        }
        if schedule {
            self.drain();
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
