//! Plugin frame windows (S05 "Öffnen": "Fremd-GUI im eigenen Fenster, Rahmen von komponist:
//! Bypass, Presets, Ansicht, Schließen"; ARCHITECTURE §8.4; spike WP-15).
//!
//! Each entry of [`UiState::plugin_windows`](crate::ui_state::UiState::plugin_windows) gets a
//! **frame**: an eframe deferred viewport with the 28 px [`header`] and a [`body`] below it. In
//! the plugin view the body holds an **X11 child window** that the bridge's plugin GUI attaches
//! to; in the parameter view it is the generic knob grid (all parameters, with WP-44's knob model;
//! pisynth's macros come from its state in the document).
//!
//! # Life of a frame (per root pass, [`App::update_plugin_frames`])
//!
//! 1. The viewport opens with a unique title token; the frame finds its X11 window by that title
//!    ([`x11::X11::find_by_title`], 36–70 ms in the spike) and then takes its real title
//!    ("Surge XT · Pad · komponist"). Its keys are grabbed ([`keys`]).
//! 2. Once the device runs (`DeviceStatus::Ok`) and the view is "Plugin", the frame creates the
//!    child below the header (background `surface.0`) and calls `DeviceHost::open_gui` with it
//!    and the frame's `pixels_per_point` as scale.
//! 3. `GuiOpened`, `GuiSizeRequest`: the frame follows the plugin ([`sizing`]); a fixed-size
//!    plugin gets a frame the user cannot resize. A user resize of a resizable plugin reaches it
//!    through `DeviceHost::set_gui_size`.
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

pub mod body;
pub mod header;
pub mod keys;
pub mod metrics;
pub mod sizing;
mod x11;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::{
    Context, Event, Rect, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
    WindowLevel, vec2,
};
use komponist_model::project::BUILTIN_PISYNTH;
use komponist_model::{ChannelRef, Device, DeviceId, PluginFormat};
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
use crate::views::devices::model::{all_plugin_params, internal_params, pisynth_params};

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

/// The open frames and the X11 connection they share.
#[derive(Debug, Default)]
pub struct PluginFrames {
    frames: Vec<Frame>,
    parked: Vec<Parked>,
    x11: X11State,
    counter: u64,
    /// Draw frames as embedded egui windows where there are no native viewports (frame tests).
    embedded: bool,
    /// Last X focus logged (`KOMPONIST_LOG_KEYS`).
    logged_focus: Option<u32>,
}

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
    /// pisynth: the state the preset match belongs to (`None`: not seen yet).
    state_seen: Option<Option<Arc<[u8]>>>,
    /// pisynth: the preset matching the state, or the last one chosen here.
    preset: Option<usize>,
    preset_changed: bool,
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

    /// The frame of `device`'s plugin window, if one is open.
    pub fn plugin_frame(&self, device: DeviceId) -> Option<FrameInfo> {
        let f = self.frames.frames.iter().find(|f| f.device == device)?;
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

    /// Opens the plugin window of the first device (tracks in order, then the master) whose
    /// plugin is called `name`; `false` if there is none (`komponist --open-plugin`).
    pub fn open_plugin_named(&mut self, name: &str) -> bool {
        let project = Arc::clone(self.project());
        let device = project
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .chain(project.master.devices.iter())
            .find(|d| d.plugin.name == name)
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
        let Some(at) = self.frames.frames.iter().position(|f| f.device == device) else {
            // No frame drawn yet (headless): close what may be open.
            if window.view == PluginView::Gui {
                self.engine.host_mut().close_gui(device);
            }
            return;
        };
        let frame = self.frames.frames.remove(at);
        self.retire_frame(frame);
    }

    /// Closes the GUI of a frame that goes away and parks or destroys its child.
    fn retire_frame(&mut self, frame: Frame) {
        let device = frame.device;
        if let Some(pos) = lock(&frame.shared).seen.outer {
            self.remember_frame(device, |m| m.pos = Some(pos));
        }
        let open = matches!(frame.gui, Gui::Opening | Gui::Open);
        if open {
            // A state snapshot follows.
            self.engine.host_mut().close_gui(device);
        }
        if let Some(x11) = self.frames.x11() {
            if let Some(xid) = frame.xid {
                x11.release_keys(xid);
            }
            if let Some(child) = frame.child {
                if open {
                    x11.park(child);
                    self.frames.parked.push(Parked {
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
        for parked in &mut self.frames.parked {
            parked.device = None;
        }
    }

    /// App exit: moves every plugin area out of its frame, so the frame windows can go while the
    /// bridges close their GUIs (`DeviceHost::shutdown` follows).
    pub(crate) fn shutdown_plugin_frames(&mut self) {
        let frames = std::mem::take(&mut self.frames.frames);
        if let Some(x11) = self.frames.x11() {
            for frame in &frames {
                if let Some(child) = frame.child {
                    x11.park(child);
                }
            }
        }
    }

    /// Once per root pass, after the queue was dispatched (module docs).
    pub(crate) fn update_plugin_frames(&mut self, ctx: &Context) {
        let native = !ctx.embed_viewports();
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
        if native && !self.frames.is_empty() && matches!(self.frames.x11, X11State::Untried) {
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
        let now = Instant::now();
        let mut busy = false;
        for i in 0..self.frames.frames.len() {
            if native {
                busy |= self.step_native(i, ctx, now);
            }
            self.update_models(i, ctx, native);
        }
        self.expire_parked(now);
        if native || self.frames.embedded {
            for frame in &self.frames.frames {
                show_viewport(ctx, frame, native);
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
                sized_for: if native { None } else { Some(window.view) },
                user_view: false,
                had_gui: false,
                initial,
                params_dirty: true,
                state_seen: None,
                preset: None,
                preset_changed: false,
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
                    let n = komponist_builtin::PRESET_COUNT;
                    let current = self.frames.frame_mut(device).and_then(|f| f.preset);
                    let next = match (e, current) {
                        (HeaderEvent::PresetNext, Some(i)) => (i + 1) % n,
                        (HeaderEvent::PresetNext, None) => 0,
                        (_, Some(i)) => (i + n - 1) % n,
                        (_, None) => n - 1,
                    };
                    self.apply_preset(device, next);
                }
                HeaderEvent::Preset(i) => self.apply_preset(device, i),
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
            Output::Body(BodyEvent::SetParam { id, value }) => {
                self.queue
                    .push(Action::Plugin(PluginAction::SetParam { device, id, value }));
            }
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
        let Some((_, _, d)) = self.project().find_device(device) else {
            return;
        };
        if !is_pisynth(d) || preset >= komponist_builtin::PRESET_COUNT {
            return;
        }
        if let Some(f) = self.frames.frame_mut(device) {
            f.preset = Some(preset);
            f.preset_changed = false;
            f.params_dirty = true;
        }
        self.queue
            .push(Action::Plugin(PluginAction::ApplyPisynthPreset {
                device,
                preset,
            }));
    }

    /// A key the key thread took back from the plugin in frame `device` ([`keys`]).
    fn grabbed_key(&mut self, device: DeviceId, chord: KeyChord) {
        keys::log(format!("frame {} (grab): {}", device.0, chord.label()));
        self.frame_chord(device, chord, false);
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
                ctrl,
                shift,
                alt: modifiers.alt,
            },
        };
        self.frame_chord(device, chord, repeat);
    }

    /// Applies `chord` from frame `device`: Esc closes the frame, everything else is a global
    /// shortcut of the main window (none while a dialog is open there).
    fn frame_chord(&mut self, device: DeviceId, chord: KeyChord, repeat: bool) {
        if chord == KeyChord::plain(Key::Escape) {
            if !repeat {
                self.queue
                    .push(Action::Plugin(PluginAction::CloseWindow(device)));
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

    /// The parts of a device host event that concern the frames.
    fn frame_event(&mut self, event: &PluginManagerEvent, ctx: &Context) {
        let now = Instant::now();
        match *event {
            PluginManagerEvent::GuiOpened {
                device,
                size,
                resizable,
            } => {
                let Some(f) = self.frames.frame_mut(device) else {
                    return;
                };
                if f.gui != Gui::Opening {
                    return;
                }
                f.gui = Gui::Open;
                f.had_gui = true;
                let chrome = chrome(f);
                let effects = f.sizing.opened(size, resizable, chrome, now);
                if f.sized_for == Some(PluginView::Gui) {
                    self.apply_effects(device, &effects, ctx);
                }
            }
            PluginManagerEvent::GuiSizeRequest { device, size } => {
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
            PluginManagerEvent::GuiClosed { device } => {
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
                        self.set_status(
                            format!("{name}: Plugin-GUI lässt sich nicht öffnen"),
                            StatusKind::Error,
                        );
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
            _ => {}
        }
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
                        .open_gui(device, u64::from(child), scale)
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

    /// Destroys parked children whose `GuiClosed` never came.
    fn expire_parked(&mut self, now: Instant) {
        let (old, keep): (Vec<Parked>, Vec<Parked>) = std::mem::take(&mut self.frames.parked)
            .into_iter()
            .partition(|p| now.duration_since(p.since) >= PARK_TIMEOUT);
        self.frames.parked = keep;
        if let Some(x11) = self.frames.x11() {
            for p in old {
                x11.destroy(p.child);
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
        self.update_pisynth_state(i);
        self.frames.frames[i].keep_on_top = window.keep_on_top;
        let f = &self.frames.frames[i];
        let header = self.header_model(
            device,
            window.view,
            f.keep_on_top,
            f.had_gui,
            f.preset,
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
                PluginView::Params => {
                    let f = &self.frames.frames[i];
                    let shown = matches!(lock(&f.shared).body, BodyModel::Params(_));
                    (f.params_dirty || !shown).then(|| BodyModel::Params(self.param_grid(i)))
                }
            },
        };
        let f = &mut self.frames.frames[i];
        f.params_dirty = false;
        let mut s = lock(&f.shared);
        let mut changed = false;
        if s.header != header {
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
    }

    /// pisynth: which factory preset its state is (" *" once a knob changed it).
    fn update_pisynth_state(&mut self, i: usize) {
        let device = self.frames.frames[i].device;
        let Some((_, _, d)) = self.project().find_device(device) else {
            return;
        };
        if !is_pisynth(d) {
            return;
        }
        let state = d.state.clone();
        let f = &mut self.frames.frames[i];
        let same = match &f.state_seen {
            Some(Some(seen)) => state.as_ref().is_some_and(|s| Arc::ptr_eq(seen, s)),
            Some(None) => state.is_none(),
            None => false,
        };
        if same {
            return;
        }
        f.state_seen = Some(state.clone());
        f.params_dirty = true;
        let patch = komponist_builtin::decode_state(state.as_deref())
            .unwrap_or_else(|_| komponist_builtin::default_patch());
        let encoded = komponist_builtin::encode_state(&patch);
        match factory_states().iter().position(|p| *p == encoded) {
            Some(p) => {
                f.preset = Some(p);
                f.preset_changed = false;
            }
            None => f.preset_changed = f.preset.is_some(),
        }
    }

    /// The header of `device`'s frame.
    fn header_model(
        &self,
        device: DeviceId,
        view: PluginView,
        keep_on_top: bool,
        had_gui: bool,
        preset: Option<usize>,
        preset_changed: bool,
    ) -> HeaderModel {
        let project = self.project();
        let found = project.find_device(device);
        let host = self.engine.host();
        let status = host.status(device);
        let (name, sub, bypassed, pisynth) = match found {
            Some((channel, _, d)) => {
                let track = match channel {
                    ChannelRef::Track(t) => project.track(t).map_or("", |t| t.name.as_str()),
                    ChannelRef::Master => "Master",
                };
                (
                    d.plugin.name.clone(),
                    format!("{track} · {}", format_label(d.plugin.format)),
                    d.bypass,
                    is_pisynth(d),
                )
            }
            None => (String::new(), String::new(), false, false),
        };
        let mark = match (&status, bypassed) {
            (DeviceStatus::Ok, false) => Mark::Active,
            (DeviceStatus::Ok, true) => Mark::Bypassed,
            _ => Mark::Inactive,
        };
        let presets = if pisynth {
            komponist_builtin::presets()
                .map(|p| p.name.to_owned())
                .collect()
        } else {
            Vec::new()
        };
        HeaderModel {
            name,
            sub,
            mark,
            bypassed,
            presets,
            preset: if pisynth { preset } else { None },
            preset_changed,
            can_save: false,
            view,
            has_gui: host.has_gui(device) || had_gui,
            keep_on_top,
            close_shortcut: KeyChord::plain(Key::Escape).label(),
        }
    }

    /// The knobs of frame `i`: all visible parameters of a bridged plugin (WP-44's filter and
    /// formatting, without the card's auto-layout limit), or pisynth's macros from its state in the
    /// document (WP-44 writes knob changes there).
    fn param_grid(&self, i: usize) -> ParamGrid {
        let device = self.frames.frames[i].device;
        let host = self.engine.host();
        if let Some(infos) = host.params(device) {
            return ParamGrid {
                params: all_plugin_params(infos, |id| host.param_value(device, id)),
            };
        }
        let params = match self.project().find_device(device) {
            Some((_, _, d)) if is_pisynth(d) => pisynth_params(d),
            Some((_, _, d)) => crate::device_host::internal_kind(d)
                .map(|kind| internal_params(kind, d))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        ParamGrid { params }
    }

    /// The plugin name of `device` ("Surge XT").
    fn plugin_name(&self, device: DeviceId) -> String {
        self.project()
            .find_device(device)
            .map_or_else(String::new, |(_, _, d)| d.plugin.name.clone())
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

/// pisynth's factory presets as device states, encoded once (preset match per state change).
pub(crate) fn factory_states() -> &'static [Vec<u8>] {
    static STATES: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    STATES.get_or_init(|| {
        (0..komponist_builtin::PRESET_COUNT)
            .map(|p| {
                komponist_builtin::preset_patch(p)
                    .map(|patch| komponist_builtin::encode_state(&patch))
                    .unwrap_or_default()
            })
            .collect()
    })
}

fn is_pisynth(device: &Device) -> bool {
    device.plugin.format == PluginFormat::Builtin && device.plugin.id == BUILTIN_PISYNTH
}

/// "CLAP", "VST3", "INTERN" (the prototype's format labels).
fn format_label(format: PluginFormat) -> &'static str {
    match format {
        PluginFormat::Builtin => "INTERN",
        PluginFormat::Clap => "CLAP",
        PluginFormat::Vst3 => "VST3",
    }
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

/// Registers frame `frame`'s viewport for this pass.
fn show_viewport(ctx: &Context, frame: &Frame, native: bool) {
    let title = {
        let s = lock(&frame.shared);
        if native && frame.xid.is_none() {
            frame.token.clone()
        } else {
            // "Surge XT · Pad · komponist": the header's sub line is "track · format".
            let track = s
                .header
                .sub
                .rsplit_once(" · ")
                .map_or(s.header.sub.as_str(), |(track, _)| track);
            format!(
                "{} · {track} · {}",
                s.header.name,
                crate::panels::transport::WORDMARK
            )
        }
    };
    let level = if frame.keep_on_top {
        WindowLevel::AlwaysOnTop
    } else {
        WindowLevel::Normal
    };
    let mut builder = ViewportBuilder::default()
        .with_title(title)
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
