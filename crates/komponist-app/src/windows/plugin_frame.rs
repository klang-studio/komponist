//! Plugin frame windows (S05 "Öffnen": "Fremd-GUI im eigenen Fenster, Rahmen von komponist:
//! Bypass, Presets, Ansicht, Schließen"; ARCHITECTURE §8.4; spike WP-15).
//!
//! Each entry of [`UiState::plugin_windows`](crate::ui_state::UiState::plugin_windows) gets a
//! **frame**: an eframe deferred viewport with the 28 px [`header`] and a [`body`] below it. In
//! the plugin view the body holds an **X11 child window** that the bridge's plugin GUI attaches
//! to; in the parameter view it is the generic knob grid (all parameters, with WP-44's knob model;
//! legacy device parameters come from their state in the document).
//!
//! # Life of a frame (per root pass, [`App::update_plugin_frames`])
//!
//! 1. The viewport opens with a unique title token; the frame finds its X11 window by that title
//!    ([`x11::X11::find_by_title`], 36–70 ms in the spike) and then takes its real title
//!    ("Surge XT · Pad · komponist"). Its keys are grabbed ([`keys`]).
//! 2. Once the device runs (`DeviceStatus::Ok`) and the view is "Plugin", the frame creates the
//!    child below the header (background `surface.0`) and calls `PluginWindowHost::open_gui` with it
//!    and the frame's `pixels_per_point` as scale.
//! 3. `GuiOpened`, `GuiSizeRequest`: the frame follows the plugin ([`sizing`]); a fixed-size
//!    plugin gets a frame the user cannot resize. A user resize of a resizable plugin reaches it
//!    through `PluginWindowHost::set_gui_size`.
//! 4. The bridge dies or stalls (`Crashed`), or "Neu laden" restarts it (`Loading`): the child is
//!    destroyed and the body shows the device card's state ("Abgestürzt", "Neu laden"); when the
//!    device runs again, a new child opens the GUI again.
//! 5. Closing (×, Esc inside the frame, the window manager's close button,
//!    `PluginAction::CloseWindow`, another project): the GUI is closed in the bridge (a state
//!    snapshot follows) and the child is parked outside the frame until the bridge answers
//!    `GuiClosed`, so the frame window disappears at once without destroying the plugin's window
//!    under it. Esc in the main window leaves the frames open. Opening another project closes
//!    every frame before `EngineSync::reset` releases the bridges.
//!
//! # macOS and Windows: the bridge owns the frame (#52, port W6)
//!
//! An app cannot embed another process's `NSView` (macOS), and a parent/child window pair across
//! processes would tie the app's input to the plugin's (Windows), so there the plugin view of a
//! frame is a window of the bridge: `PluginWindowHost::open_gui` with `GuiTarget::OwnFrame` (the header model, the
//! language, where the frame was). The bridge draws the same header (`komponist_ui::plugin_frame`)
//! and reports what happens in it; the app keeps every decision, as on Linux:
//!
//! - `FrameHeaderAction` → the header's events (bypass, presets, view, "Oben halten", ×);
//!   `HeaderState` carries the new header back whenever the model changes.
//! - `FrameShortcut` (a key the bridge took from the focused plugin) and `FrameKey` (keys while
//!   its header has the focus) → the same chords as the X11 grab and the frame's egui keys.
//! - `FrameMoved`, `GuiClosed{rect}` → the position the frame reopens at (remembered per plugin).
//! - `FrameCloseRequested` (the window's close button) → `CloseWindow`.
//!
//! While the bridge shows the frame the app shows no viewport for it. The parameter view and the
//! device states (loading, crashed, missing) are the app's own viewport as on Linux, at the
//! bridge frame's last position; switching "Ansicht" swaps the windows. Activation follows
//! macOS's cooperative rules (spike #42 Q3) and Windows' foreground lock ([`activation`]): the app
//! yields to the bridge's pid (Windows: `AllowSetForegroundWindow`) and sends `ActivateFrame`
//! when the frame opens or is opened again; when the bridge closes a frame it yields back, and the
//! app activates itself if the bridge was the active app.
//!
//! Without native viewports (`egui_kittest`, `Context::embed_viewports`) frames run without X11 and
//! are not drawn, so other views' snapshots stay as they are; frame tests turn on
//! [`App::show_embedded_plugin_frames`] to get them as embedded egui windows, where the plugin view
//! shows the prototype's placeholder. Tests never open a window on a desktop.
//!
//! # Keys and focus (X11)
//!
//! - The frame's own window has the focus (click on the header, Alt-Tab back): every global
//!   shortcut of the main window works (Leertaste, R, L, C, Pos1, Strg S, Strg Z, …), computer
//!   keyboard MIDI plays while it is active, and Esc closes the frame.
//! - The plugin has the focus (a click into it; JUCE plugins take it when they open): the plugin
//!   gets its keys, except Strg S always and Leertaste, Esc, Strg Z / Strg Shift Z while the user
//!   is not typing in it ([`keys`] has the policy and the heuristic).
//! - Focus returns to the main window only through the window manager (click, Alt-Tab); komponist
//!   never moves the X focus itself.
//!
//! `KOMPONIST_LOG_KEYS=1` logs key events of the main window and the frames, the grab decisions
//! and X focus changes to stderr.

mod activation;
pub mod body;
pub mod header;
pub mod keys;
pub mod metrics;
pub mod sizing;
#[cfg(x11_embed)]
mod x11;
#[cfg(not(x11_embed))]
#[path = "plugin_frame/x11_unavailable.rs"]
mod x11;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::{
    Context, Event, Rect, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
    WindowLevel, vec2,
};
use komponist_i18n::Lang;
use komponist_ipc::GuiTarget;
use komponist_model::{ChannelRef, DeviceId};
use komponist_plugin_api::clap_presets::ClapPresetSource;
use komponist_plugin_api::frame::{FrameKeyEvent, FrameRect};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};
use komponist_tokens::color;

use self::body::{BodyEvent, BodyModel, ParamGrid};
use self::header::{HeaderEvent, HeaderModel, Mark};
use self::sizing::{Chrome, Effect, Px, Sizing};
use self::x11::X11;
use crate::actions::{Action, EditAction, PluginAction, UiAction};
use crate::app::App;
use crate::keymap::{self, Key, KeyChord};
use crate::settings::FrameMemory;
use crate::ui_state::{FocusArea, PluginView, PluginWindow, StatusKind};
use crate::views::devices::model::all_plugin_params;
use komponist_session::device_host::PluginWindowHost;

/// How long a parked child waits for the bridge's `GuiClosed` before it is destroyed anyway.
const PARK_TIMEOUT: Duration = Duration::from_secs(3);

/// How often the root repaints while a frame waits for X11 (its window, a size).
const X11_POLL: Duration = Duration::from_millis(16);

/// Prefix of the title token that finds a frame's X11 window.
const TOKEN_PREFIX: &str = "komponist-plugin-frame";

/// What a frame shows and holds, for tests and diagnostics ([`App::plugin_frame`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    /// The frame viewport's X11 window, once found.
    pub window: Option<u32>,
    /// The X11 child the plugin GUI lives in.
    pub child: Option<u32>,
    /// The plugin's GUI is open (`GuiOpened` arrived).
    pub gui_open: bool,
    /// The plugin's size in physical pixels, once known.
    pub plugin_size: Option<(u32, u32)>,
    /// The plugin may be resized.
    pub resizable: bool,
    /// Header height in physical pixels (0 before the first draw).
    pub header_px: u32,
    /// Width the header needs in physical pixels (0 before the first draw).
    pub min_width_px: u32,
    /// "Oben halten" is on.
    pub keep_on_top: bool,
}

/// Owns the app and bridge frame lifecycle, focus requests and the shared X11 connection.
#[derive(Debug, Default)]
pub struct PluginFrames {
    frames: Vec<Frame>,
    parked: Vec<Parked>,
    x11: X11State,
    counter: u64,
    /// Draw frames as embedded egui windows where there are no native viewports (frame tests).
    embedded: bool,
    /// Tests: the bridge owns the plugin frames (as on macOS and Windows) also without native viewports.
    force_bridge_frames: bool,
    /// This pass: the bridge owns the plugin frames (macOS and Windows with native viewports, or forced).
    bridge_frames: bool,
    /// Last X focus logged (`KOMPONIST_LOG_KEYS`).
    logged_focus: Option<u32>,
    /// A frame to bring to the front with the focus in the next root pass (menus, `OpenWindow`).
    focus: Option<FocusRequest>,
}

/// A pending [`App::focus_plugin_window`].
#[derive(Debug, Clone, Copy)]
struct FocusRequest {
    device: DeviceId,
    /// The app asked to become active first (macOS, the app was not active) at this time.
    claimed: Option<Instant>,
}

/// How long a focus request waits for the app's own activation before it goes ahead anyway.
const CLAIM_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
enum X11State {
    #[default]
    Untried,
    Failed,
    Ready(X11),
}

/// A child window waiting for `GuiClosed` after its frame closed.
#[derive(Debug)]
struct Parked {
    /// `None` once another project opened: its device ids start again at 1, so a `GuiClosed`
    /// of the new project must not match; the timeout destroys it.
    device: Option<DeviceId>,
    child: u32,
    since: Instant,
}

/// Where the plugin's GUI stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gui {
    /// Not opened (no child, device not running, parameter view from the start).
    Idle,
    /// `open_gui` sent.
    Opening,
    /// `GuiOpened` arrived.
    Open,
}

/// One frame window.
#[derive(Debug)]
struct Frame {
    device: DeviceId,
    viewport: ViewportId,
    token: String,
    shared: Arc<Mutex<Shared>>,
    /// The frame viewport's X11 window.
    xid: Option<u32>,
    /// Our child window inside it (the plugin area).
    child: Option<u32>,
    child_mapped: bool,
    gui: Gui,
    sizing: Sizing,
    /// Last frame inner size asked for or seen, physical px (centres a narrow plugin).
    frame_px: Option<Px>,
    /// Header width last used for the frame's minimum, physical px.
    min_width: u32,
    keep_on_top: bool,
    /// Where the frame opens (outer position, points), if remembered.
    pos: Option<(i32, i32)>,
    /// Where the bridge's own frame was last (macOS, Windows).
    remote_rect: Option<FrameRect>,
    /// The window number of the bridge's own frame (macOS; Windows: its `HWND`).
    remote_window: Option<i64>,
    /// The view the window was last sized for.
    sized_for: Option<PluginView>,
    /// The user picked the view (the frame no longer switches it to "Plugin" by itself).
    user_view: bool,
    /// The plugin showed a GUI once (keeps "Plugin" enabled while it is crashed).
    had_gui: bool,
    /// Initial inner size in points.
    initial: Vec2,
    /// The parameter grid needs rebuilding.
    params_dirty: bool,
    /// The preset the device last loaded, as the header shows it.
    preset: Option<ClapPresetSource>,
    preset_changed: bool,
    requested_preset: Option<ClapPresetSource>,
    queued_preset: Option<ClapPresetSource>,
}

/// Shared between the root pass and the frame viewport's callback.
#[derive(Debug)]
struct Shared {
    header: HeaderModel,
    body: BodyModel,
    out: Vec<Output>,
    seen: Seen,
}

/// What the frame's callback saw in its last pass.
#[derive(Debug, Default, Clone, Copy)]
struct Seen {
    ppp: f32,
    inner: Option<Vec2>,
    header_width: f32,
    /// The frame's outer position in points (a native frame only).
    outer: Option<(i32, i32)>,
}

/// What happened in a frame window.
#[derive(Debug, Clone)]
enum Output {
    Header(HeaderEvent),
    Body(BodyEvent),
    Keys(Vec<Event>),
    CloseRequested,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PluginFrames {
    /// Whether any frame is open.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn frame_mut(&mut self, device: DeviceId) -> Option<&mut Frame> {
        self.frames.iter_mut().find(|f| f.device == device)
    }

    fn x11(&self) -> Option<&X11> {
        match &self.x11 {
            X11State::Ready(x) => Some(x),
            _ => None,
        }
    }

    /// Whether the bridge shows `device`'s frame right now (its `OpenGui{OwnFrame}` is out or
    /// answered), so the app shows no viewport for it.
    fn in_bridge(&self, device: DeviceId) -> bool {
        self.bridge_frames
            && self
                .frames
                .iter()
                .any(|f| f.device == device && f.gui != Gui::Idle)
    }

    /// The app became active (macOS: `NSApplicationDidBecomeActiveNotification`): every frame the
    /// bridges show comes to the front, back to front so their order stays, without taking the
    /// activation from the app (module docs).
    fn raise_bridge_frames(&self, host: &mut dyn PluginWindowHost) {
        if !self.bridge_frames {
            return;
        }
        let order = activation::front_to_back();
        let mut open: Vec<(usize, DeviceId)> = self
            .frames
            .iter()
            .filter(|f| f.gui == Gui::Open)
            .map(|f| {
                let at = f
                    .remote_window
                    .and_then(|n| order.iter().position(|o| *o == n))
                    .unwrap_or(usize::MAX);
                (at, f.device)
            })
            .collect();
        // Back first: each raise lands on top of the ones before.
        open.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (_, device) in open {
            host.raise_frame(device);
        }
    }

    /// Carries out a pending [`App::focus_plugin_window`] (module docs).
    fn apply_focus(
        &mut self,
        windows: &[PluginWindow],
        host: &mut dyn PluginWindowHost,
        ctx: &Context,
        native: bool,
        now: Instant,
    ) {
        let Some(mut request) = self.focus.take() else {
            return;
        };
        let Some(frame) = self.frames.iter().find(|f| f.device == request.device) else {
            // Not drawn yet: try again in the next pass.
            if windows.iter().any(|w| w.device == request.device) {
                self.focus = Some(request);
            }
            return;
        };
        if self.bridge_frames && frame.gui != Gui::Idle {
            if frame.gui == Gui::Opening {
                // `GuiOpened` brings it to the front.
                return;
            }
            if native && !activation::is_active() {
                match request.claimed {
                    None => {
                        activation::activate();
                        request.claimed = Some(now);
                        self.focus = Some(request);
                        ctx.request_repaint_after(CLAIM_TIMEOUT);
                        return;
                    }
                    Some(at) if now.duration_since(at) < CLAIM_TIMEOUT => {
                        self.focus = Some(request);
                        ctx.request_repaint_after(CLAIM_TIMEOUT - now.duration_since(at));
                        return;
                    }
                    Some(_) => {}
                }
            }
            activation::bring_to_front(host, request.device);
        } else if native {
            ctx.send_viewport_cmd_to(frame.viewport, ViewportCommand::Focus);
        }
    }

    /// The frame of `device`'s plugin window, if one is open.
    fn info(&self, device: DeviceId) -> Option<FrameInfo> {
        let f = self.frames.iter().find(|f| f.device == device)?;
        let seen = lock(&f.shared).seen;
        Some(FrameInfo {
            window: f.xid,
            child: f.child,
            gui_open: f.gui == Gui::Open,
            plugin_size: f.sizing.plugin().map(|p| (p.w, p.h)),
            resizable: f.sizing.resizable(),
            header_px: (metrics::HEADER * seen.ppp).round() as u32,
            min_width_px: (seen.header_width * seen.ppp).ceil() as u32,
            keep_on_top: f.keep_on_top,
        })
    }

    fn request_focus(&mut self, windows: &[PluginWindow], device: DeviceId) {
        if windows.iter().any(|window| window.device == device) {
            self.focus = Some(FocusRequest {
                device,
                claimed: None,
            });
        }
    }

    fn remove(&mut self, device: DeviceId) -> Option<Frame> {
        let index = self
            .frames
            .iter()
            .position(|frame| frame.device == device)?;
        Some(self.frames.remove(index))
    }

    fn retirement_position(&self, frame: &Frame) -> Option<(i32, i32)> {
        if self.bridge_frames && frame.gui != Gui::Idle {
            frame.remote_rect.map(rect_pos)
        } else {
            lock(&frame.shared).seen.outer
        }
    }

    fn retire(&mut self, host: &mut dyn PluginWindowHost, frame: Frame) {
        let device = frame.device;
        let open = matches!(frame.gui, Gui::Opening | Gui::Open);
        if open {
            // A state snapshot follows.
            host.close_gui(device);
        }
        if let Some(x11) = self.x11() {
            if let Some(xid) = frame.xid {
                x11.release_keys(xid);
            }
            if let Some(child) = frame.child {
                if open {
                    x11.park(child);
                    self.parked.push(Parked {
                        device: Some(device),
                        child,
                        since: Instant::now(),
                    });
                } else {
                    x11.destroy(child);
                }
            }
        }
    }

    fn unlink_parked(&mut self) {
        for parked in &mut self.parked {
            parked.device = None;
        }
    }

    fn shutdown(&mut self) {
        let frames = std::mem::take(&mut self.frames);
        if let Some(x11) = self.x11() {
            for frame in &frames {
                if let Some(child) = frame.child {
                    x11.park(child);
                }
            }
        }
    }

    /// Destroys parked children whose `GuiClosed` never came.
    fn expire_parked(&mut self, now: Instant) {
        let (old, keep): (Vec<Parked>, Vec<Parked>) = std::mem::take(&mut self.parked)
            .into_iter()
            .partition(|p| now.duration_since(p.since) >= PARK_TIMEOUT);
        self.parked = keep;
        if let Some(x11) = self.x11() {
            for p in old {
                x11.destroy(p.child);
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// App hooks
// ------------------------------------------------------------------------------------------------

impl App {
    /// Without native viewports (`egui_kittest`): draw the frames as embedded egui windows over the
    /// main window (off by default, so other views' snapshots do not show them).
    pub fn show_embedded_plugin_frames(&mut self, on: bool) {
        self.frames.embedded = on;
    }

    /// Tests: the bridge owns the plugin view of a frame (`GuiTarget::OwnFrame`, as on macOS and Windows)
    /// also without native viewports and on Linux.
    pub fn use_bridge_frames(&mut self, on: bool) {
        self.frames.force_bridge_frames = on;
    }

    /// Whether the bridge shows this device's frame instead of an app viewport.
    pub fn plugin_frame_in_bridge(&self, device: DeviceId) -> bool {
        self.frames.in_bridge(device)
    }

    /// Brings the bridge-owned plugin windows forward in their current stacking order.
    pub fn raise_bridge_frames(&mut self) {
        self.frames.raise_bridge_frames(self.engine.host_mut());
    }

    /// `OpenWindow` for a window that is open already: it comes to the front.
    pub(crate) fn raise_plugin_window(&mut self, device: DeviceId) {
        self.focus_plugin_window(device);
    }

    /// `PluginAction::FocusWindow` (the Window, Dock or "⋯" menu, #95): `device`'s window comes to
    /// the front with the keyboard focus in the next root pass ([`Self::update_plugin_frames`]).
    /// A frame the bridge shows gets the yield + `ActivateFrame` handshake (macOS; if the app is not
    /// active, as from the Dock menu, it activates itself first and waits for that); an app-owned
    /// frame gets `ViewportCommand::Focus`. A window that is not open is ignored.
    pub(crate) fn focus_plugin_window(&mut self, device: DeviceId) {
        self.frames.request_focus(&self.ui.plugin_windows, device);
    }

    /// The frame of `device`'s plugin window, if one is open.
    pub fn plugin_frame(&self, device: DeviceId) -> Option<FrameInfo> {
        self.frames.info(device)
    }

    /// Opens the plugin window of the first device (tracks in order, then the master) whose
    /// plugin is called `name`; `false` if there is none (`komponist --open-plugin`).
    pub fn open_plugin_named(&mut self, name: &str) -> bool {
        let project = Arc::clone(self.project());
        let device = project
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .chain(project.master.devices.iter())
            .find(|d| {
                d.plugin.name == name
                    || crate::views::catalog_plugin_name(
                        self.lang,
                        self.engine.host().catalog(),
                        &d.plugin,
                    ) == name
            })
            .map(|d| d.id);
        match device {
            Some(device) => {
                self.dispatch(Action::Plugin(PluginAction::OpenWindow(device)));
                true
            }
            None => false,
        }
    }

    /// `UiAction::SetPluginView`: the window shows `view`; the user's pick, so the frame no longer
    /// switches it by itself.
    pub(crate) fn set_plugin_view(&mut self, device: DeviceId, view: PluginView) {
        if let Some(w) = self
            .ui
            .plugin_windows
            .iter_mut()
            .find(|w| w.device == device)
        {
            w.view = view;
        }
        if let Some(f) = self.frames.frame_mut(device) {
            f.user_view = true;
            f.params_dirty = true;
        }
    }

    /// `UiAction::SetPluginKeepOnTop`: "Oben halten" of the window, remembered for the plugin.
    pub(crate) fn set_keep_on_top(&mut self, device: DeviceId, on: bool) {
        if let Some(w) = self
            .ui
            .plugin_windows
            .iter_mut()
            .find(|w| w.device == device)
        {
            w.keep_on_top = on;
        }
        self.remember_frame(device, |m| m.keep_on_top = on);
    }

    /// Updates what is remembered for `device`'s plugin and saves the settings when it changed.
    fn remember_frame(&mut self, device: DeviceId, change: impl FnOnce(&mut FrameMemory)) {
        let Some(plugin) = self
            .project()
            .find_device(device)
            .map(|(_, _, d)| d.plugin.clone())
        else {
            return;
        };
        if self.settings.update_frame_memory(&plugin, change) {
            self.save_settings();
        }
    }

    /// `PluginAction::CloseWindow`: closes the frame of `device` (module docs, step 5).
    pub(crate) fn close_plugin_window(&mut self, device: DeviceId) {
        let Some(index) = self
            .ui
            .plugin_windows
            .iter()
            .position(|w| w.device == device)
        else {
            return;
        };
        let window = self.ui.plugin_windows.remove(index);
        let Some(frame) = self.frames.remove(device) else {
            // No frame drawn yet (headless): close what may be open.
            if window.view == PluginView::Gui {
                self.engine.host_mut().close_gui(device);
            }
            return;
        };
        self.retire_frame(frame);
    }

    /// Closes the GUI of a frame that goes away and parks or destroys its child.
    fn retire_frame(&mut self, frame: Frame) {
        let device = frame.device;
        if let Some(pos) = self.frames.retirement_position(&frame) {
            self.remember_frame(device, |m| m.pos = Some(pos));
        }
        self.frames.retire(self.engine.host_mut(), frame);
    }

    /// Another project opens (`App::open_document`, before `EngineSync::reset` releases the
    /// bridges): every frame closes as with ×, and the parked plugin areas no longer belong to a
    /// device of the new project.
    pub(crate) fn close_all_plugin_frames(&mut self) {
        let devices: Vec<DeviceId> = self.ui.plugin_windows.iter().map(|w| w.device).collect();
        for device in devices {
            self.close_plugin_window(device);
        }
        for frame in std::mem::take(&mut self.frames.frames) {
            self.retire_frame(frame);
        }
        self.frames.unlink_parked();
    }

    /// App exit: moves every plugin area out of its frame, so the frame windows can go while the
    /// bridges close their GUIs (`ProcessorHost::shutdown` follows).
    pub(crate) fn shutdown_plugin_frames(&mut self) {
        self.frames.shutdown();
    }

    /// Once per root pass, after the queue was dispatched (module docs).
    pub(crate) fn update_plugin_frames(&mut self, ctx: &Context) {
        let native = !ctx.embed_viewports();
        let remote = (native && cfg!(bridge_frame)) || self.frames.force_bridge_frames;
        self.frames.bridge_frames = remote;
        if remote && native && activation::became_active(ctx) {
            self.raise_bridge_frames();
        }
        self.log_focus();
        // Keys the key thread took back from plugins.
        let grabbed = self
            .frames
            .x11()
            .map(|x| x.keys().poll())
            .unwrap_or_default();
        for g in grabbed {
            self.grabbed_key(g.device, g.chord);
        }
        // What the user did in the frames.
        let outputs: Vec<(DeviceId, Vec<Output>)> = self
            .frames
            .frames
            .iter()
            .map(|f| (f.device, std::mem::take(&mut lock(&f.shared).out)))
            .collect();
        for (device, outs) in outputs {
            for out in outs {
                self.frame_output(device, out);
            }
        }
        self.flush_queue();
        // Windows of devices that are gone close.
        let gone: Vec<DeviceId> = self
            .ui
            .plugin_windows
            .iter()
            .map(|w| w.device)
            .filter(|d| self.project().find_device(*d).is_none())
            .collect();
        for device in gone {
            self.close_plugin_window(device);
        }
        self.sync_frames(native);
        if native
            && !remote
            && !self.frames.is_empty()
            && matches!(self.frames.x11, X11State::Untried)
        {
            self.frames.x11 = match X11::connect(ctx) {
                Ok(x) => X11State::Ready(x),
                Err(e) => {
                    eprintln!("komponist: no X11 connection for plugin windows: {e}");
                    X11State::Failed
                }
            };
        }
        let events: Vec<PluginManagerEvent> = self.device_events().to_vec();
        for event in &events {
            self.frame_event(event, ctx);
        }
        self.flush_frame_presets();
        let now = Instant::now();
        // After the raise on activation, so the picked frame ends up in front.
        self.frames.apply_focus(
            &self.ui.plugin_windows,
            self.engine.host_mut(),
            ctx,
            native,
            now,
        );
        let mut busy = false;
        for i in 0..self.frames.frames.len() {
            if remote {
                busy |= self.step_remote(i);
            } else if native {
                busy |= self.step_native(i, ctx, now);
            }
            self.update_models(i, ctx, native);
        }
        self.frames.expire_parked(now);
        if native || self.frames.embedded {
            for frame in &self.frames.frames {
                // The bridge shows this one (macOS, Windows).
                if remote && frame.gui != Gui::Idle {
                    continue;
                }
                show_viewport(ctx, frame, native, remote);
            }
        }
        if busy {
            ctx.request_repaint_after(X11_POLL);
        }
    }

    /// Creates frames for new windows; frames of closed windows went in `close_plugin_window`.
    fn sync_frames(&mut self, native: bool) {
        let windows: Vec<PluginWindow> = self.ui.plugin_windows.clone();
        // Windows that went without `CloseWindow` (a new project).
        let (keep, gone): (Vec<Frame>, Vec<Frame>) = std::mem::take(&mut self.frames.frames)
            .into_iter()
            .partition(|f| windows.iter().any(|w| w.device == f.device));
        self.frames.frames = keep;
        for frame in gone {
            self.retire_frame(frame);
        }
        for window in windows {
            if self.frames.frames.iter().any(|f| f.device == window.device) {
                continue;
            }
            self.frames.counter += 1;
            let token = format!(
                "{TOKEN_PREFIX}-{}-{}",
                std::process::id(),
                self.frames.counter
            );
            let initial = initial_size(&window);
            let header = self.header_model(window.device, window.view, false, false, None, false);
            let body = BodyModel::Loading;
            let frame = Frame {
                device: window.device,
                viewport: ViewportId::from_hash_of(&token),
                token,
                shared: Arc::new(Mutex::new(Shared {
                    header,
                    body,
                    out: Vec::new(),
                    seen: Seen::default(),
                })),
                xid: None,
                child: None,
                child_mapped: false,
                gui: Gui::Idle,
                sizing: Sizing::new(),
                frame_px: None,
                min_width: 0,
                keep_on_top: window.keep_on_top,
                pos: window.pos,
                remote_rect: None,
                remote_window: None,
                sized_for: if native { None } else { Some(window.view) },
                user_view: false,
                had_gui: false,
                initial,
                params_dirty: true,
                preset: None,
                preset_changed: false,
                requested_preset: None,
                queued_preset: None,
            };
            self.frames.frames.push(frame);
        }
    }

    /// One thing the user did in frame `device`.
    fn frame_output(&mut self, device: DeviceId, out: Output) {
        match out {
            Output::Header(e) => match e {
                HeaderEvent::ToggleBypass => {
                    if let Some((_, _, d)) = self.project().find_device(device) {
                        let bypass = !d.bypass;
                        self.queue
                            .push(Action::Edit(EditAction::SetDeviceBypass { device, bypass }));
                    }
                }
                HeaderEvent::PresetPrev | HeaderEvent::PresetNext => {
                    let Some((_, _, target)) = self.project().find_device(device) else {
                        return;
                    };
                    let entries = self.engine.host().clap_presets(target);
                    let n = entries.len();
                    if n == 0 {
                        return;
                    }
                    let current = self
                        .frames
                        .frames
                        .iter()
                        .find(|f| f.device == device)
                        .and_then(|f| {
                            [
                                f.queued_preset.as_ref(),
                                f.requested_preset.as_ref(),
                                f.preset.as_ref(),
                            ]
                            .into_iter()
                            .flatten()
                            .find_map(|source| {
                                entries.iter().position(|entry| &entry.source == source)
                            })
                        });
                    let next = match (e, current) {
                        (HeaderEvent::PresetNext, Some(i)) => (i + 1) % n,
                        (HeaderEvent::PresetNext, None) => 0,
                        (_, Some(i)) => (i + n - 1) % n,
                        (_, None) => n - 1,
                    };
                    self.apply_preset(device, next);
                }
                HeaderEvent::Preset(i) => self.apply_preset(device, i),
                HeaderEvent::SavePreset => self
                    .queue
                    .push(Action::Plugin(PluginAction::SaveUserPreset(device))),
                HeaderEvent::View(view) => {
                    self.queue
                        .push(Action::Ui(UiAction::SetPluginView { device, view }));
                }
                HeaderEvent::KeepOnTop(on) => {
                    self.queue
                        .push(Action::Ui(UiAction::SetPluginKeepOnTop { device, on }));
                }
                HeaderEvent::Close => {
                    self.queue
                        .push(Action::Plugin(PluginAction::CloseWindow(device)));
                }
            },
            Output::Body(BodyEvent::Reload) => {
                self.queue
                    .push(Action::Plugin(PluginAction::Reload(device)));
            }
            Output::Body(BodyEvent::BeginGesture { id }) => {
                self.queue
                    .push(Action::Plugin(PluginAction::BeginParamGesture {
                        device,
                        id,
                    }));
            }
            Output::Body(BodyEvent::EndGesture) => {
                self.queue.push(Action::Edit(EditAction::EndGesture));
            }
            Output::Body(BodyEvent::SetParam { id, value }) => {
                self.queue
                    .push(Action::Plugin(PluginAction::SetParam { device, id, value }));
            }
            Output::Body(BodyEvent::ControlAction(action)) => self.queue.push(action),
            Output::Keys(events) => {
                for event in events {
                    self.frame_key(device, &event);
                }
            }
            Output::CloseRequested => {
                self.queue
                    .push(Action::Plugin(PluginAction::CloseWindow(device)));
            }
        }
    }

    fn apply_preset(&mut self, device: DeviceId, preset: usize) {
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        if let Some(entry) = self.engine.host().clap_presets(target).get(preset)
            && let Some(frame) = self.frames.frame_mut(device)
        {
            frame.queued_preset = Some(entry.source.clone());
        }
    }

    fn flush_frame_presets(&mut self) {
        for i in 0..self.frames.frames.len() {
            let frame = &mut self.frames.frames[i];
            if frame.requested_preset.is_some() {
                continue;
            }
            let Some(source) = frame.queued_preset.take() else {
                continue;
            };
            let device = frame.device;
            let Some((_, _, target)) = self.project().find_device(device) else {
                continue;
            };
            if !self
                .engine
                .host()
                .clap_presets(target)
                .iter()
                .any(|entry| entry.source == source)
            {
                continue;
            }
            self.frames.frames[i].requested_preset = Some(source.clone());
            self.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
                device,
                source,
            }));
        }
    }

    pub(crate) fn frame_preset_finished(&mut self, device: DeviceId, applied: bool) {
        if let Some(frame) = self.frames.frame_mut(device)
            && frame.requested_preset.take().is_some()
            && applied
        {
            frame.params_dirty = true;
        }
    }

    pub(crate) fn cancel_frame_preset_requests(&mut self, device: DeviceId) {
        if let Some(frame) = self.frames.frame_mut(device)
            && frame.requested_preset.take().is_some()
        {
            frame.queued_preset = None;
        }
    }

    /// A key the key thread took back from the plugin in frame `device` ([`keys`]).
    fn grabbed_key(&mut self, device: DeviceId, chord: KeyChord) {
        keys::log(format!(
            "frame {} (grab): {}",
            device.0,
            chord.label(Lang::En)
        ));
        self.frame_chord(device, chord, false);
    }

    /// A key the bridge's own frame took from its plugin (macOS, Windows; the X11 grab's counterpart).
    fn frame_shortcut(
        &mut self,
        device: DeviceId,
        chord: &komponist_plugin_api::frame::FrameChord,
    ) {
        let Some(chord) = keys::chord_from_frame(chord) else {
            return;
        };
        keys::log(format!(
            "frame {} (bridge): {}",
            device.0,
            chord.label(Lang::En)
        ));
        self.frame_chord(device, chord, false);
    }

    /// A key event of the bridge's own frame while its header has the focus (macOS, Windows): applied as a
    /// key of the frame's own window.
    fn frame_bridge_key(&mut self, device: DeviceId, key: &FrameKeyEvent) {
        let Some(logical) = egui::Key::from_name(&key.key) else {
            return;
        };
        let event = Event::Key {
            key: logical,
            physical_key: key.physical_key.as_deref().and_then(egui::Key::from_name),
            pressed: key.pressed,
            repeat: key.repeat,
            modifiers: egui::Modifiers {
                alt: key.mods.alt,
                ctrl: key.mods.ctrl,
                shift: key.mods.shift,
                mac_cmd: key.mods.mac_cmd,
                command: key.mods.command,
            },
        };
        self.frame_key(device, &event);
    }

    /// A key event of frame `device`'s own window (the frame, not the plugin, has the focus).
    fn frame_key(&mut self, device: DeviceId, event: &Event) {
        let Event::Key {
            key,
            physical_key,
            pressed,
            repeat,
            modifiers,
        } = *event
        else {
            return;
        };
        keys::log(format!(
            "frame {} (egui): {key:?} pressed={pressed} repeat={repeat} {modifiers:?}",
            device.0
        ));
        let ctrl = modifiers.ctrl || modifiers.command;
        let physical = crate::app::key_from_egui(physical_key.unwrap_or(key));
        if let Some(k) = physical
            && let Some(note) = keymap::note_for(k)
            && !ctrl
            && !modifiers.alt
            && (self.keyboard_midi_active() || !pressed)
        {
            self.keyboard_note(k, note, pressed, repeat);
            return;
        }
        if !pressed {
            return;
        }
        let Some(k) = crate::app::key_from_egui(key) else {
            return;
        };
        let shift = modifiers.shift && !matches!(k, Key::Plus | Key::Minus);
        let chord = KeyChord {
            key: k,
            mods: keymap::Modifiers {
                command: ctrl,
                shift,
                alt: modifiers.alt,
            },
        };
        self.frame_chord(device, chord, repeat);
    }

    /// Applies `chord` from frame `device`: Esc first ends mapping mode, or closes the frame.
    /// Other shortcuts follow the main window (none while a dialog is open there).
    fn frame_chord(&mut self, device: DeviceId, chord: KeyChord, repeat: bool) {
        if chord == KeyChord::plain(Key::Escape) {
            if !repeat {
                let action = if self.ui.track_macros.mapping.is_some() {
                    Action::Macro(crate::views::track_macros::Action::ExitMapping)
                } else {
                    Action::Plugin(PluginAction::CloseWindow(device))
                };
                self.queue.push(action);
            }
            return;
        }
        if self.ui.dialog.is_some() || chord.key == Key::Tab {
            return;
        }
        // The transport bar has no bindings of its own: this finds the global ones.
        let Some(action) = self.keymap.lookup(chord, FocusArea::Transport).cloned() else {
            return;
        };
        if repeat && !crate::app::repeatable(&action) {
            return;
        }
        self.queue.push(action);
    }

    /// Whether `device` has a frame.
    fn has_frame(&self, device: DeviceId) -> bool {
        self.frames.frames.iter().any(|f| f.device == device)
    }

    /// The parts of a device host event that concern the frames.
    fn frame_event(&mut self, event: &PluginManagerEvent, ctx: &Context) {
        let now = Instant::now();
        match *event {
            PluginManagerEvent::GuiOpened {
                device,
                size,
                resizable,
            } => {
                let remote = self.frames.bridge_frames;
                let Some(f) = self.frames.frame_mut(device) else {
                    return;
                };
                if f.gui != Gui::Opening {
                    return;
                }
                f.gui = Gui::Open;
                f.had_gui = true;
                if remote {
                    // The bridge's frame is up: to the front (macOS cooperative activation, the
                    // Windows foreground lock).
                    activation::bring_to_front(self.engine.host_mut(), device);
                    return;
                }
                let chrome = chrome(f);
                let effects = f.sizing.opened(size, resizable, chrome, now);
                if f.sized_for == Some(PluginView::Gui) {
                    self.apply_effects(device, &effects, ctx);
                }
            }
            PluginManagerEvent::GuiSizeRequest { device, size } => {
                if self.frames.bridge_frames {
                    // The bridge's frame follows its plugin itself.
                    return;
                }
                let Some(f) = self.frames.frame_mut(device) else {
                    return;
                };
                if f.gui != Gui::Open {
                    return;
                }
                let chrome = chrome(f);
                let effects = f.sizing.requested(size, chrome, now);
                if f.sized_for == Some(PluginView::Gui) {
                    self.apply_effects(device, &effects, ctx);
                }
            }
            PluginManagerEvent::GuiClosed { device, rect } => {
                if let Some(rect) = rect {
                    // The bridge's frame closed (×, Esc, "Parameter", the plugin itself): it
                    // yielded back, so the app takes the focus if the bridge had it.
                    activation::take_back(self.engine.host().bridge_pid(device));
                    self.remember_frame(device, |m| m.pos = Some(rect_pos(rect)));
                    if let Some(f) = self.frames.frame_mut(device) {
                        f.remote_rect = Some(rect);
                        f.pos = Some(rect_pos(rect));
                    }
                }
                if let Some(at) = self
                    .frames
                    .parked
                    .iter()
                    .position(|p| p.device == Some(device))
                {
                    let parked = self.frames.parked.remove(at);
                    if let Some(x11) = self.frames.x11() {
                        x11.destroy(parked.child);
                    }
                    return;
                }
                let Some(i) = self.frames.frames.iter().position(|f| f.device == device) else {
                    return;
                };
                match self.frames.frames[i].gui {
                    Gui::Opening => {
                        // The GUI did not open: the frame shows the parameters instead.
                        let f = &mut self.frames.frames[i];
                        f.gui = Gui::Idle;
                        f.user_view = true;
                        if let (Some(child), X11State::Ready(x11)) =
                            (f.child.take(), &self.frames.x11)
                        {
                            x11.destroy(child);
                        }
                        if let Some(w) = self
                            .ui
                            .plugin_windows
                            .iter_mut()
                            .find(|w| w.device == device)
                        {
                            w.view = PluginView::Params;
                        }
                        let name = self.plugin_name(device);
                        self.set_status(self.lang.plugin_frame_gui_failed(name), StatusKind::Error);
                    }
                    Gui::Open => {
                        // The plugin closed its GUI itself: the frame closes too.
                        let f = &mut self.frames.frames[i];
                        f.gui = Gui::Idle;
                        if let (Some(child), X11State::Ready(x11)) =
                            (f.child.take(), &self.frames.x11)
                        {
                            x11.destroy(child);
                        }
                        self.queue
                            .push(Action::Plugin(PluginAction::CloseWindow(device)));
                    }
                    Gui::Idle => {}
                }
            }
            PluginManagerEvent::ParamsChanged { device }
            | PluginManagerEvent::ParamValuesChanged { device } => {
                if let Some(f) = self.frames.frame_mut(device) {
                    f.params_dirty = true;
                }
            }
            PluginManagerEvent::StatusChanged { device, .. } => {
                if let Some(f) = self.frames.frame_mut(device) {
                    f.params_dirty = true;
                }
            }
            PluginManagerEvent::FrameHeaderAction { device, event } if self.has_frame(device) => {
                self.frame_output(device, Output::Header(event));
            }
            PluginManagerEvent::FrameShortcut { device, ref chord } if self.has_frame(device) => {
                self.frame_shortcut(device, chord);
            }
            PluginManagerEvent::FrameKey { device, ref key } if self.has_frame(device) => {
                self.frame_bridge_key(device, key);
            }
            PluginManagerEvent::FrameWindow { device, number } => {
                if let Some(f) = self.frames.frame_mut(device) {
                    f.remote_window = Some(number);
                }
            }
            PluginManagerEvent::FrameMoved { device, rect } => {
                if let Some(f) = self.frames.frame_mut(device) {
                    f.remote_rect = Some(rect);
                    f.pos = Some(rect_pos(rect));
                }
            }
            PluginManagerEvent::FrameCloseRequested { device } if self.has_frame(device) => {
                self.queue
                    .push(Action::Plugin(PluginAction::CloseWindow(device)));
            }
            _ => {}
        }
    }

    /// Frame `i` where the bridge owns the plugin view (macOS, Windows; module docs): opens and closes the
    /// bridge's frame with the view and the device's state; `true` while it waits for the bridge.
    fn step_remote(&mut self, i: usize) -> bool {
        let device = self.frames.frames[i].device;
        let Some(window) = self
            .ui
            .plugin_windows
            .iter()
            .find(|w| w.device == device)
            .copied()
        else {
            return false;
        };
        let host = self.engine.host();
        let running = host.status(device) == DeviceStatus::Ok;
        let has_gui = host.has_gui(device);
        let frame = &mut self.frames.frames[i];
        frame.had_gui |= has_gui;
        if !running && frame.gui != Gui::Idle {
            // A crash or reload took the bridge's window with it: the app's frame shows the state.
            frame.gui = Gui::Idle;
        }
        let mut view = window.view;
        if running
            && !frame.user_view
            && has_gui
            && view == PluginView::Params
            && frame.gui == Gui::Idle
        {
            view = PluginView::Gui;
        }
        if running && view == PluginView::Gui && !has_gui {
            view = PluginView::Params;
        }
        if view != PluginView::Gui && frame.gui != Gui::Idle {
            // "Parameter": the app's frame takes over at the bridge frame's place.
            frame.gui = Gui::Idle;
            self.engine.host_mut().close_gui(device);
        }
        let frame = &self.frames.frames[i];
        if running && view == PluginView::Gui && frame.gui == Gui::Idle {
            let rect = frame.remote_rect.or_else(|| {
                frame.pos.map(|(x, y)| {
                    let [w, h] = metrics::GUI_AREA;
                    FrameRect {
                        x: x as f32,
                        y: y as f32,
                        width: w,
                        height: h + metrics::HEADER,
                    }
                })
            });
            let header = self.header_model(
                device,
                PluginView::Gui,
                window.keep_on_top,
                frame.had_gui || has_gui,
                frame.preset.as_ref(),
                frame.preset_changed,
            );
            let target = GuiTarget::OwnFrame {
                header: header.clone(),
                lang: self.lang.code().to_owned(),
                rect,
            };
            match self.engine.host_mut().open_gui(device, target) {
                Ok(()) => {
                    let frame = &mut self.frames.frames[i];
                    frame.gui = Gui::Opening;
                    // What the bridge shows now; `update_models` sends changes from here.
                    lock(&frame.shared).header = header;
                }
                Err(e) => {
                    eprintln!("komponist: device {}: plugin GUI not opened: {e}", device.0);
                    self.frames.frames[i].user_view = true;
                    view = PluginView::Params;
                }
            }
        }
        if view != window.view
            && let Some(w) = self
                .ui
                .plugin_windows
                .iter_mut()
                .find(|w| w.device == device)
        {
            w.view = view;
        }
        let frame = &mut self.frames.frames[i];
        frame.sized_for = Some(view);
        // The app's viewport opens anew whenever the bridge's frame goes: at the view's size.
        frame.initial = initial_size(&PluginWindow { view, ..window });
        frame.gui == Gui::Opening
    }

    /// X11 work of frame `i`; `true` while it waits for something (the root keeps polling).
    fn step_native(&mut self, i: usize, ctx: &Context, now: Instant) -> bool {
        let device = self.frames.frames[i].device;
        let Some(window) = self
            .ui
            .plugin_windows
            .iter()
            .find(|w| w.device == device)
            .copied()
        else {
            return false;
        };
        let host = self.engine.host();
        let status = host.status(device);
        let has_gui = host.has_gui(device);
        let running = status == DeviceStatus::Ok;
        let frames = &mut self.frames;
        let X11State::Ready(x11) = &frames.x11 else {
            return false;
        };
        let frame = &mut frames.frames[i];
        let seen = lock(&frame.shared).seen;
        if frame.xid.is_none() {
            match x11.find_by_title(&frame.token) {
                Some(xid) => {
                    keys::log(format!("frame {}: window {xid:#x}", device.0));
                    frame.xid = Some(xid);
                    x11.grab_keys(xid, device);
                    ctx.request_repaint_of(frame.viewport);
                }
                None => return true,
            }
        }
        if seen.ppp <= 0.0 {
            return true;
        }
        frame.had_gui |= has_gui;
        // A crash or reload takes the plugin area away.
        if !running && (frame.gui != Gui::Idle || frame.child.is_some()) {
            if let Some(child) = frame.child.take() {
                x11.destroy(child);
            }
            frame.gui = Gui::Idle;
            frame.child_mapped = false;
            frame.sizing.reset_frame();
        }
        let mut view = window.view;
        if running
            && !frame.user_view
            && has_gui
            && view == PluginView::Params
            && frame.gui == Gui::Idle
        {
            // Opened while loading: now it can show its GUI.
            view = PluginView::Gui;
        }
        if running && view == PluginView::Gui && !has_gui {
            view = PluginView::Params;
        }
        let chrome = Chrome {
            header: (metrics::HEADER * seen.ppp).round() as u32,
            min_width: (seen.header_width * seen.ppp).ceil() as u32,
        };
        let mut effects = Vec::new();
        if running && view == PluginView::Gui && frame.child.is_none() && frame.gui == Gui::Idle {
            let size = frame.sizing.plugin().unwrap_or_else(|| {
                let [w, h] = metrics::GUI_AREA;
                Px::new((w * seen.ppp).round() as u32, (h * seen.ppp).round() as u32)
            });
            let pos = child_pos(frame.frame_px, size, chrome);
            let xid = frame.xid.unwrap_or_default();
            let opened = x11
                .create_child(xid, pos, size, color::SURFACE_0, true)
                .and_then(|child| {
                    frame.child = Some(child);
                    frame.child_mapped = true;
                    frame.gui = Gui::Opening;
                    keys::log(format!("frame {}: child {child:#x}", device.0));
                    let scale = f64::from(seen.ppp);
                    self.engine
                        .host_mut()
                        .open_gui(
                            device,
                            GuiTarget::X11Child {
                                xid: u64::from(child),
                                scale,
                            },
                        )
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = opened {
                eprintln!("komponist: device {}: plugin GUI not opened: {e}", device.0);
                if let Some(child) = frame.child.take() {
                    x11.destroy(child);
                }
                frame.gui = Gui::Idle;
                frame.user_view = true;
                view = PluginView::Params;
            }
        }
        if view != window.view
            && let Some(w) = self
                .ui
                .plugin_windows
                .iter_mut()
                .find(|w| w.device == device)
        {
            w.view = view;
        }
        let show_gui = running && view == PluginView::Gui;
        if let Some(child) = frame.child
            && frame.child_mapped != show_gui
        {
            x11.set_mapped(child, show_gui);
            frame.child_mapped = show_gui;
        }
        // The window's size follows the view.
        if frame.sized_for != Some(view) {
            frame.sized_for = Some(view);
            frame.sizing.reset_frame();
            frame.min_width = chrome.min_width;
            if view == PluginView::Gui && frame.sizing.plugin().is_some() {
                effects.extend(frame.sizing.refit(chrome, now));
            } else {
                let [w, h] = if view == PluginView::Gui {
                    metrics::GUI_AREA
                } else {
                    metrics::PARAMS_AREA
                };
                let area = Px::new((w * seen.ppp).round() as u32, (h * seen.ppp).round() as u32);
                effects.push(Effect::Frame {
                    inner: Sizing::frame_for(area, chrome),
                    locked: false,
                });
            }
        } else if chrome.min_width != frame.min_width {
            // The header got wider or narrower (first draw, scale).
            frame.min_width = chrome.min_width;
            if view == PluginView::Gui && frame.gui == Gui::Open {
                effects.extend(frame.sizing.refit(chrome, now));
            }
        }
        if view == PluginView::Gui && frame.gui == Gui::Open {
            if let Some(inner) = seen.inner {
                let px = Px::new(
                    (inner.x * seen.ppp).round() as u32,
                    (inner.y * seen.ppp).round() as u32,
                );
                if !frame.sizing.busy() {
                    frame.frame_px = Some(px);
                }
                effects.extend(frame.sizing.observed(px, chrome, now));
            }
            effects.extend(frame.sizing.tick(now));
        }
        let busy = frame.sizing.busy() || frame.gui == Gui::Opening;
        if !effects.is_empty() {
            self.apply_effects(device, &effects, ctx);
        }
        busy
    }

    /// Carries out size effects of frame `device`.
    fn apply_effects(&mut self, device: DeviceId, effects: &[Effect], ctx: &Context) {
        let Some(i) = self.frames.frames.iter().position(|f| f.device == device) else {
            return;
        };
        let ppp = lock(&self.frames.frames[i].shared).seen.ppp;
        if ppp <= 0.0 {
            return;
        }
        let header_px = (metrics::HEADER * ppp).round() as u32;
        let min_width = self.frames.frames[i].min_width;
        let chrome = Chrome {
            header: header_px,
            min_width,
        };
        // Frame first, so the child is centred in the new width.
        let mut ordered: Vec<Effect> = effects
            .iter()
            .filter(|e| matches!(e, Effect::Frame { .. }))
            .copied()
            .collect();
        ordered.extend(
            effects
                .iter()
                .filter(|e| !matches!(e, Effect::Frame { .. })),
        );
        for effect in ordered {
            let frame = &mut self.frames.frames[i];
            match effect {
                Effect::Frame { inner, locked } => {
                    frame.frame_px = Some(inner);
                    resize_viewport(ctx, frame.viewport, inner, locked, ppp, chrome);
                }
                Effect::Child(size) => {
                    if let (Some(child), X11State::Ready(x11)) = (frame.child, &self.frames.x11) {
                        x11.configure(child, child_pos(frame.frame_px, size, chrome), size);
                    }
                    if let Some(w) = self
                        .ui
                        .plugin_windows
                        .iter_mut()
                        .find(|w| w.device == device)
                    {
                        w.width = size.w;
                        w.height = size.h;
                    }
                }
                Effect::Tell(size) => self.engine.host_mut().set_gui_size(device, size),
            }
        }
    }

    /// Writes header and body of frame `i` for its callback; wakes the frame when they changed.
    fn update_models(&mut self, i: usize, ctx: &Context, native: bool) {
        let device = self.frames.frames[i].device;
        let Some(window) = self
            .ui
            .plugin_windows
            .iter()
            .find(|w| w.device == device)
            .copied()
        else {
            return;
        };
        self.update_preset_state(i);
        self.frames.frames[i].keep_on_top = window.keep_on_top;
        let f = &self.frames.frames[i];
        let header = self.header_model(
            device,
            window.view,
            f.keep_on_top,
            f.had_gui,
            f.preset.as_ref(),
            f.preset_changed,
        );
        let status = self.engine.host().status(device);
        let name = self.plugin_name(device);
        let body = match status {
            DeviceStatus::Loading => Some(BodyModel::Loading),
            DeviceStatus::Crashed(reason) => Some(BodyModel::Crashed(reason)),
            DeviceStatus::Missing => Some(BodyModel::Missing { name }),
            DeviceStatus::Ok => match window.view {
                PluginView::Gui => Some(BodyModel::Gui { native, name }),
                PluginView::Params if self.engine.host().native_layout(device).is_some() => {
                    self.project().find_device(device).map(|(_, _, device)| {
                        let mut card = crate::views::devices::model::card(
                            self.lang,
                            device,
                            self.engine.host(),
                            &self.ui.plugin_windows,
                        );
                        card.macros = self.param_grid(i).macros;
                        BodyModel::Native(Box::new(card))
                    })
                }
                PluginView::Params => {
                    let f = &self.frames.frames[i];
                    let (shown, was_redirecting) = match &lock(&f.shared).body {
                        BodyModel::Params(grid) => (
                            true,
                            grid.macros
                                .values()
                                .any(crate::views::track_macros::Control::redirects),
                        ),
                        _ => (false, false),
                    };
                    let macro_relevant = self
                        .project()
                        .find_device(device)
                        .and_then(|(owner, _, _)| match owner {
                            ChannelRef::Track(id) => self.project().track(id),
                            ChannelRef::Master => None,
                        })
                        .is_some_and(|track| {
                            self.ui.track_macros.mapping == Some(track.id)
                                || !track.macros.is_default()
                        });
                    (f.params_dirty || !shown || was_redirecting || macro_relevant)
                        .then(|| BodyModel::Params(self.param_grid(i)))
                }
            },
        };
        let remote = self.frames.bridge_frames;
        let f = &mut self.frames.frames[i];
        f.params_dirty = false;
        let mut s = lock(&f.shared);
        let mut changed = false;
        let mut to_bridge = None;
        if s.header != header {
            if remote && f.gui != Gui::Idle {
                to_bridge = Some(header.clone());
            }
            s.header = header;
            changed = true;
        }
        if let Some(body) = body
            && s.body != body
        {
            s.body = body;
            changed = true;
        }
        drop(s);
        if changed {
            ctx.request_repaint_of(f.viewport);
        }
        if let Some(header) = to_bridge {
            let lang = self.lang.code();
            self.engine
                .host_mut()
                .set_frame_header(device, header, lang);
        }
    }

    /// The preset the device shows and whether it was modified since
    /// ([`komponist_session::device_host::selected_preset`]).
    fn update_preset_state(&mut self, i: usize) {
        let device = self.frames.frames[i].device;
        let Some((_, _, d)) = self.project().find_device(device) else {
            return;
        };
        let entries = self.engine.host().clap_presets(d);
        let selected = komponist_session::device_host::selected_preset(d, entries);
        let preset = selected.map(|(index, _)| entries[index].source.clone());
        let changed = selected.is_some_and(|(_, modified)| modified);
        let f = &mut self.frames.frames[i];
        if f.preset == preset && f.preset_changed == changed {
            return;
        }
        f.params_dirty = true;
        f.preset = preset;
        f.preset_changed = changed;
    }

    /// The header of `device`'s frame.
    fn header_model(
        &self,
        device: DeviceId,
        view: PluginView,
        keep_on_top: bool,
        had_gui: bool,
        preset: Option<&ClapPresetSource>,
        preset_changed: bool,
    ) -> HeaderModel {
        let project = self.project();
        let found = project.find_device(device);
        let host = self.engine.host();
        let status = host.status(device);
        let (name, sub, bypassed) = match found {
            Some((channel, _, d)) => {
                let track = match channel {
                    ChannelRef::Track(t) => project.track(t).map_or("", |t| t.name.as_str()),
                    ChannelRef::Master => self.lang.dialog_master(),
                };
                (
                    crate::views::catalog_plugin_name(
                        self.lang,
                        self.engine.host().catalog(),
                        &d.plugin,
                    ),
                    format!(
                        "{track} · {}",
                        crate::dialogs::missing_plugins::format_label(self.lang, d.plugin.format)
                    ),
                    d.bypass,
                )
            }
            None => (String::new(), String::new(), false),
        };
        let mark = match (&status, bypassed) {
            (DeviceStatus::Ok, false) => Mark::Active,
            (DeviceStatus::Ok, true) => Mark::Bypassed,
            _ => Mark::Inactive,
        };
        let entries = found.map_or(&[][..], |(_, _, device)| host.clap_presets(device));
        let presets = entries
            .iter()
            .map(|entry| entry.display_name(self.lang.code()).to_owned())
            .collect();
        let preset =
            preset.and_then(|source| entries.iter().position(|entry| &entry.source == source));
        let mapping_chip = found.and_then(|(owner, _, _)| {
            let ChannelRef::Track(id) = owner else {
                return None;
            };
            if self.ui.track_macros.mapping != Some(id) {
                return None;
            }
            let track = project.track(id)?;
            let index = self.ui.track_macros.selected(id);
            let source = crate::views::track_macros::reference(
                self.lang,
                index,
                &track.macros.0[usize::from(index)],
            );
            Some(self.lang.macros_map_chip(source))
        });
        HeaderModel {
            name,
            sub,
            mark,
            bypassed,
            presets,
            preset,
            preset_changed,
            can_save: host.supports_user_presets(device),
            view,
            has_gui: host.has_gui(device) || had_gui,
            keep_on_top,
            close_shortcut: KeyChord::plain(Key::Escape).label(self.lang),
            mapping_chip,
        }
    }

    /// The knobs of frame `i`: the hosted plugin's visible parameters or the standalone
    /// parameters read from a legacy device's saved state.
    fn param_grid(&self, i: usize) -> ParamGrid {
        let device = self.frames.frames[i].device;
        let host = self.engine.host();
        let owner = self.project().find_device(device);
        let params = if let Some(infos) = host.params(device) {
            all_plugin_params(self.lang, infos, |id| host.param_value(device, id))
        } else {
            Vec::new()
        };
        let mut grid = ParamGrid::new(params);
        if let Some((ChannelRef::Track(id), _, _)) = owner
            && let Some(track) = self.project().track(id)
        {
            let view = &self.ui().track_macros;
            let effective = self.engine.telemetry().macro_values(id);
            for param in grid.params.iter().filter(|param| param.info.automatable) {
                let target = komponist_model::automation::AutomationTarget::Param {
                    device,
                    param: param.info.id.clone(),
                };
                let value_at = |index: u8| {
                    let index = usize::from(index);
                    effective.map_or(track.macros.0[index].value, |values| values[index])
                };
                let control = crate::views::track_macros::Control::for_track(
                    self.lang, view, track, target, value_at,
                );
                grid.macros.insert(param.info.id.clone(), control);
            }
        }
        grid
    }

    /// The plugin name of `device` ("Surge XT").
    fn plugin_name(&self, device: DeviceId) -> String {
        self.project()
            .find_device(device)
            .map_or_else(String::new, |(_, _, d)| {
                crate::views::catalog_plugin_name(
                    self.lang,
                    self.engine.host().catalog(),
                    &d.plugin,
                )
            })
    }

    /// `KOMPONIST_LOG_KEYS`: X focus changes while frames are open.
    fn log_focus(&mut self) {
        if !keys::log_enabled() {
            return;
        }
        let Some(focus) = self.frames.x11().and_then(X11::input_focus) else {
            return;
        };
        if self.frames.logged_focus != Some(focus) {
            self.frames.logged_focus = Some(focus);
            let frames: Vec<String> = self
                .frames
                .frames
                .iter()
                .map(|f| format!("device {} = {:?}/{:?}", f.device.0, f.xid, f.child))
                .collect();
            keys::log(format!(
                "X input focus → {focus:#x} (frames: {})",
                frames.join(", ")
            ));
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------

/// The outer position of `rect`, as the settings remember it.
fn rect_pos(rect: FrameRect) -> (i32, i32) {
    (rect.x.round() as i32, rect.y.round() as i32)
}

/// The frame's measures from what its callback saw.
fn chrome(f: &Frame) -> Chrome {
    let seen = lock(&f.shared).seen;
    Chrome {
        header: (metrics::HEADER * seen.ppp).round() as u32,
        min_width: f.min_width,
    }
}

/// Where the child goes: below the header, centred when the frame is wider than the plugin.
fn child_pos(frame: Option<Px>, child: Px, chrome: Chrome) -> (i16, i16) {
    let width = frame.map_or(child.w, |f| f.w);
    let x = width.saturating_sub(child.w) / 2;
    (
        i16::try_from(x).unwrap_or(i16::MAX),
        i16::try_from(chrome.header).unwrap_or(i16::MAX),
    )
}

/// The inner size a frame opens with, in points.
fn initial_size(window: &PluginWindow) -> Vec2 {
    let [w, h] = match window.view {
        PluginView::Gui => metrics::GUI_AREA,
        PluginView::Params => metrics::PARAMS_AREA,
    };
    vec2(w, h + metrics::HEADER)
}

/// Sends the viewport commands for an inner size of `inner` px; `locked`: not resizable, min =
/// max = `inner`.
fn resize_viewport(
    ctx: &Context,
    id: ViewportId,
    inner: Px,
    locked: bool,
    ppp: f32,
    chrome: Chrome,
) {
    let size = vec2(inner.w as f32 / ppp, inner.h as f32 / ppp);
    if locked {
        ctx.send_viewport_cmd_to(id, ViewportCommand::Resizable(false));
        // Order the hints so min never exceeds max on the way.
        ctx.send_viewport_cmd_to(id, ViewportCommand::MaxInnerSize(Vec2::INFINITY));
        ctx.send_viewport_cmd_to(id, ViewportCommand::MinInnerSize(size));
        ctx.send_viewport_cmd_to(id, ViewportCommand::MaxInnerSize(size));
    } else {
        let min = vec2(
            chrome.min_width as f32 / ppp,
            metrics::HEADER + metrics::MIN_BODY_HEIGHT,
        );
        ctx.send_viewport_cmd_to(id, ViewportCommand::MaxInnerSize(Vec2::INFINITY));
        ctx.send_viewport_cmd_to(id, ViewportCommand::MinInnerSize(min));
        ctx.send_viewport_cmd_to(id, ViewportCommand::Resizable(true));
    }
    ctx.send_viewport_cmd_to(id, ViewportCommand::InnerSize(size));
    keys::log(format!(
        "frame viewport {id:?}: inner {}×{} px ({size:?} pt, locked {locked})",
        inner.w, inner.h
    ));
}

/// Registers frame `frame`'s viewport for this pass. Its title is the token until X11 found the
/// window (Linux); where the bridges own the plugin frames (macOS, Windows) there is no lookup, and the
/// app's own frame (parameter view, device states) carries its real title from the start, as the
/// Window and Dock menus expect (#95).
fn show_viewport(ctx: &Context, frame: &Frame, native: bool, remote: bool) {
    let title = {
        let s = lock(&frame.shared);
        if native && !remote && frame.xid.is_none() {
            frame.token.clone()
        } else {
            komponist_ui::plugin_frame::window_title(&s.header)
        }
    };
    let level = if frame.keep_on_top {
        WindowLevel::AlwaysOnTop
    } else {
        WindowLevel::Normal
    };
    let mut builder = ViewportBuilder::default()
        .with_title(title)
        .with_app_id(crate::APP_ID)
        .with_icon(komponist_ui::app_icon())
        .with_inner_size(frame.initial)
        .with_window_level(level);
    if let Some((x, y)) = frame.pos {
        builder = builder.with_position(egui::pos2(x as f32, y as f32));
    }
    let shared = Arc::clone(&frame.shared);
    let initial = frame.initial;
    ctx.show_viewport_deferred(frame.viewport, builder, move |ui, class| {
        frame_ui(ui, class, &shared, initial);
    });
}

/// The frame viewport's callback.
fn frame_ui(ui: &mut egui::Ui, class: ViewportClass, shared: &Mutex<Shared>, initial: Vec2) {
    let ctx = ui.ctx().clone();
    let embedded = class == ViewportClass::EmbeddedWindow;
    let mut s = lock(shared);
    let mut wake_root = false;
    s.seen.ppp = ctx.pixels_per_point();
    if !embedded {
        s.seen.outer = ctx.input(|i| {
            i.viewport()
                .outer_rect
                .map(|r| (r.min.x.round() as i32, r.min.y.round() as i32))
        });
    }
    let inner = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
    if inner != s.seen.inner {
        s.seen.inner = inner;
        wake_root = true;
    }
    if !embedded && ctx.input(|i| i.viewport().close_requested()) {
        s.out.push(Output::CloseRequested);
        wake_root = true;
    }
    // Keys for the app, unless a knob's text entry has them.
    if !embedded && !ctx.egui_wants_keyboard_input() {
        let widget_keys =
            komponist_ui::focus::is_visible(&ctx) && ctx.memory(|m| m.focused().is_some());
        let keys: Vec<Event> = ctx.input(|i| {
            i.events
                .iter()
                .filter(|e| match e {
                    Event::Key { key, .. } => {
                        !(widget_keys
                            && matches!(
                                key,
                                egui::Key::Space
                                    | egui::Key::Enter
                                    | egui::Key::Tab
                                    | egui::Key::ArrowUp
                                    | egui::Key::ArrowDown
                                    | egui::Key::ArrowLeft
                                    | egui::Key::ArrowRight
                            ))
                    }
                    _ => false,
                })
                .cloned()
                .collect()
        });
        if !keys.is_empty() {
            s.out.push(Output::Keys(keys));
            wake_root = true;
        }
    }
    let rect = if embedded {
        // As wide as the header needs, like a native frame's minimum.
        let size = vec2(initial.x.max(s.seen.header_width), initial.y);
        ui.set_min_size(size);
        Rect::from_min_size(ui.max_rect().min, size)
    } else {
        ui.max_rect()
    };
    let header_rect = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::HEADER));
    let body_rect = Rect::from_min_max(egui::pos2(rect.left(), header_rect.bottom()), rect.max);
    let model = s.header.clone();
    let out = header::show(ui, header_rect, &model);
    if (out.natural_width - s.seen.header_width).abs() > 0.5 {
        s.seen.header_width = out.natural_width;
        wake_root = true;
    }
    for e in out.events {
        s.out.push(Output::Header(e));
        wake_root = true;
    }
    let body_events = body::show(ui, body_rect, &mut s.body);
    for e in body_events {
        s.out.push(Output::Body(e));
        wake_root = true;
    }
    if embedded {
        ui.allocate_rect(rect, egui::Sense::hover());
    }
    drop(s);
    if wake_root {
        ctx.request_repaint_of(ViewportId::ROOT);
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;

    #[derive(Default)]
    struct WindowHost {
        activated: Option<DeviceId>,
    }

    impl PluginWindowHost for WindowHost {
        fn activate_frame(&mut self, device: DeviceId) {
            self.activated = Some(device);
        }
    }

    #[test]
    fn activation_needs_only_the_window_capability() {
        let device = DeviceId(1);
        let mut host = WindowHost::default();
        activation::bring_to_front(&mut host, device);
        assert_eq!(host.activated, Some(device));
    }

    #[test]
    fn focus_waits_for_the_frame_and_is_dropped_when_the_window_closes() {
        let device = DeviceId(1);
        let window = PluginWindow::new(device, PluginView::Gui, None);
        let mut frames = PluginFrames::default();
        let mut host = WindowHost::default();
        let ctx = Context::default();
        let now = Instant::now();

        frames.request_focus(&[], device);
        assert!(frames.focus.is_none());

        frames.request_focus(&[window], device);
        frames.apply_focus(&[window], &mut host, &ctx, false, now);
        assert_eq!(frames.focus.map(|request| request.device), Some(device));

        frames.apply_focus(&[], &mut host, &ctx, false, now);
        assert!(frames.focus.is_none());
    }
}
