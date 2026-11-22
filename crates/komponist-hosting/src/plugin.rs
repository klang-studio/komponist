//! The format-independent face of a hosted plugin (ARCHITECTURE §8.2).
//!
//! CLAP and VST3 both split a plugin instance by thread: main-thread calls (lifecycle, parameters,
//! state, GUI) and audio-thread calls (process). The bridge has the same two threads, so the
//! contract has two halves:
//!
//! - [`HostedPlugin`] stays on the bridge's main thread (not `Send`).
//! - [`HostedProcessor`] is created by [`HostedPlugin::activate`], moves to the bridge's RT thread
//!   (`Send`), and goes back to the main thread for [`HostedPlugin::deactivate`].
//!
//! The halves map directly onto clack-host (`PluginInstance` ↔ `StartedPluginAudioProcessor`) and
//! onto VST3 (`IEditController` + `IComponent` on main, `IAudioProcessor` on audio). An adapter onto
//! truce-rack, whose `Plugin`/`PluginCore` is one `&mut self` object, has to split that object (a
//! shared instance behind a lock the RT half only `try_lock`s, or an upstream change); this is one
//! of the questions of spike WP-15.
//!
//! Per-block data reuses the shared-memory types of `komponist-ipc` ([`WireEvent`],
//! [`WireTransport`]), so the bridge passes what it read from the rings without conversion and the
//! format layers decode with [`WireEvent::decode`]. Audio is always stereo in and out, plus an
//! optional stereo sidechain in (#6); the format layer adapts mono or multi-channel ports
//! (WP-26/WP-32).

use komponist_ipc::{GuiSize, NoteName, ParamInfo, PluginDescriptor, RenderMode};
use komponist_ipc::{WireEvent, WireTransport};
#[cfg(unix)]
use std::os::fd::RawFd;

#[cfg(unix)]
use crate::host::FdEvents;
use crate::host::TimerId;

/// Why a hosting operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostingError {
    /// The bundle could not be opened or has no such plugin.
    Load(String),
    /// The plugin refused activation or start of processing.
    Activate(String),
    /// State save or load failed.
    State(String),
    /// GUI creation, attach or resize failed, or the plugin has no embeddable GUI.
    Gui(String),
    /// The call needs an active (or an inactive) plugin.
    WrongState(&'static str),
    /// The plugin or format does not support this.
    Unsupported(&'static str),
    /// The host loop could not register a timer or fd.
    HostLoop(String),
}

impl core::fmt::Display for HostingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HostingError::Load(m) => write!(f, "load failed: {m}"),
            HostingError::Activate(m) => write!(f, "activation failed: {m}"),
            HostingError::State(m) => write!(f, "state failed: {m}"),
            HostingError::Gui(m) => write!(f, "GUI failed: {m}"),
            HostingError::WrongState(m) => write!(f, "wrong state: {m}"),
            HostingError::Unsupported(m) => write!(f, "unsupported: {m}"),
            HostingError::HostLoop(m) => write!(f, "host loop: {m}"),
        }
    }
}

impl std::error::Error for HostingError {}

/// Audio configuration for [`HostedPlugin::activate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActivateConfig {
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// Smallest block the host will send (1).
    pub min_frames: u32,
    /// Largest block the host will send (≤ `komponist_ipc::shm::MAX_BLOCK`).
    pub max_frames: u32,
}

/// What [`HostedPlugin::gui_open`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GuiInfo {
    /// Initial size: physical pixels (X11) or points (Cocoa).
    pub size: GuiSize,
    /// Whether the GUI accepts host resizes.
    pub resizable: bool,
}

/// Whether this build embeds plugin GUIs: on Linux into an X11 child of the app's frame window
/// (WP-50), on macOS into an `NSView` of the bridge's own frame window (#52; CLAP `cocoa`, VST3
/// `kPlatformTypeNSView`), on Windows into a child `HWND` of the bridge's own frame window (port
/// W6; [`GuiParent::Win32`], CLAP `win32`, VST3 `kPlatformTypeHWND`). Where it is false, no plugin
/// reports a GUI, whatever it offers, and `gui_open` fails.
pub const GUI_EMBEDDING: bool = cfg!(any(target_os = "linux", target_os = "macos", windows));

/// Where [`HostedPlugin::gui_open`] attaches the plugin's GUI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GuiParent {
    /// Linux: the X11 window `xid` (the app frame's child window), at UI `scale` (egui
    /// `pixels_per_point`; CLAP `set_scale`, VST3 `setContentScaleFactor`).
    X11 {
        /// X11 window id.
        xid: u64,
        /// UI scale factor.
        scale: f64,
    },
    /// macOS: an `NSView*` of the bridge's frame window. Cocoa sizes in points and the backing
    /// scale follows the screen, so no scale is passed: CLAP `set_scale` would make JUCE plugins
    /// double their size in points, and VST3 `setContentScaleFactor` is refused (spike #42 Q1).
    Cocoa(*mut std::ffi::c_void),
    /// Windows: an `HWND` of the bridge's frame window (W6), at UI `scale` (the window's DPI scale;
    /// CLAP `set_scale`, VST3 `setContentScaleFactor`). Win32 sizes are physical pixels, and plugins
    /// there expect the host to pass the scale (unlike Cocoa).
    Win32 {
        /// The parent `HWND`.
        hwnd: *mut std::ffi::c_void,
        /// UI scale factor.
        scale: f64,
    },
}

/// Main-thread half of one loaded plugin instance. Lives on the bridge's main thread from
/// [`crate::load`] until drop (drop = deactivate if needed, destroy, unload).
///
/// All methods run on the bridge main thread. They may allocate and block briefly; they must not
/// wait for the RT thread.
pub trait HostedPlugin {
    /// What was loaded.
    fn descriptor(&self) -> &PluginDescriptor;

    /// Whether the plugin offers a GUI this build embeds: on Linux one that embeds into X11 (CLAP
    /// `gui` with `x11` embedded, VST3 `IPlugView` with `kPlatformTypeX11EmbedWindowID`), on macOS
    /// into Cocoa (CLAP `cocoa` embedded, VST3 `kPlatformTypeNSView`). Always false where
    /// [`GUI_EMBEDDING`] is.
    fn has_gui(&self) -> bool;

    /// Activates for `config` and returns the audio-thread half, not yet processing. Fails if
    /// already active. The plugin's latency may change here ([`HostedPlugin::latency`]).
    fn activate(
        &mut self,
        config: ActivateConfig,
    ) -> Result<Box<dyn HostedProcessor>, HostingError>;

    /// Deactivates, consuming the processor the RT thread handed back (after
    /// [`HostedProcessor::stop_processing`]).
    fn deactivate(&mut self, processor: Box<dyn HostedProcessor>);

    /// Whether the plugin is active (a processor exists).
    fn is_active(&self) -> bool;

    /// Sets realtime or offline processing. Only while inactive (`WrongState` otherwise); applies
    /// from the next activation (CLAP `render.set`, VST3 `ProcessSetup::processMode`).
    fn set_render_mode(&mut self, mode: RenderMode) -> Result<(), HostingError>;

    /// The parameter list (may be long; call on load and after `ParamsChanged`).
    fn params(&self) -> Vec<ParamInfo>;

    /// The immutable descriptor read once after plugin initialization. The bridge checks the
    /// parameter bindings again whenever it publishes a parameter inventory.
    fn native_layout(&self) -> Option<(&[u8], &komponist_ipc::native_layout::NativeLayout)> {
        None
    }

    /// Loader and playback snapshot. A tempo requests preparation while audio is detached.
    fn sampler_ui(
        &self,
        _export_tempo: Option<f64>,
    ) -> Option<komponist_ipc::sampler_ui::Snapshot> {
        None
    }

    /// Audition or release a slice without changing the plugin's persisted state.
    fn preview_sampler_slice(&self, _time: Option<f64>) {}

    /// Current plain value of parameter `id` (main-thread view; CLAP `params.get_value`, VST3
    /// controller).
    fn param_value(&self, id: u32) -> Option<f64>;

    /// The plugin's text for `value` of parameter `id` ("80 Hz").
    fn param_text(&self, id: u32, value: f64) -> Option<String>;

    /// A parameter change from the app (generic view). Updates the main-thread side (VST3
    /// controller `setParamNormalized`; nothing for CLAP). The bridge additionally delivers the value
    /// to the audio side: as a `ParamValue` event in the next block while processing, else through
    /// [`HostedPlugin::flush_params`].
    fn set_param(&mut self, id: u32, value: f64);

    /// Syncs a value that the audio processor already received to its editor controller.
    /// Controller-less formats need no separate update.
    fn sync_controller_param(&mut self, _id: u32, _value: f64) {}

    /// Only while not processing: hands `input` events (parameter values) to the plugin and collects
    /// the parameter events it emits into `output` (CLAP `params.flush`; VST3 applies them to the
    /// processor and controller directly). Offsets are ignored.
    fn flush_params(&mut self, input: &[WireEvent], output: &mut Vec<WireEvent>);

    /// Whether the controller has edits waiting for an active processor. Check while audio
    /// processing is paused so that no edit can be in transit from the queue to the component.
    fn has_unapplied_edits(&self) -> bool {
        false
    }

    /// The plugin state as one blob (CLAP `state.save`; VST3 component + controller states with a
    /// small header, format defined by WP-32).
    fn save_state(&mut self) -> Result<Vec<u8>, HostingError>;

    /// Restores a blob from [`HostedPlugin::save_state`] (possibly from an earlier session).
    fn load_state(&mut self, state: &[u8]) -> Result<(), HostingError>;

    /// Latency in frames at the active sample rate (0 when inactive or unknown).
    fn latency(&self) -> u32;

    /// Note names (CLAP note-name; empty if unsupported).
    fn note_names(&self) -> Vec<NoteName>;

    /// Creates the GUI embedded into `parent` and shows it. A [`GuiParent`] of the other OS fails
    /// with `HostingError::Gui`.
    fn gui_open(&mut self, parent: GuiParent) -> Result<GuiInfo, HostingError>;

    /// Asks the GUI to take `size` (physical pixels on X11 and Win32, points on Cocoa); returns the size it
    /// adopted (it may round to its constraints).
    fn gui_set_size(&mut self, size: GuiSize) -> Result<GuiSize, HostingError>;

    /// The open GUI's scale changed (Win32: the frame moved to a monitor with another DPI, port
    /// W6): CLAP `set_scale`, VST3 `setContentScaleFactor`. Returns whether the plugin took it (a
    /// plugin may follow the window's DPI itself). Not for Cocoa, whose GUIs take no scale. The
    /// default does nothing.
    fn gui_set_scale(&mut self, scale: f64) -> bool {
        let _ = scale;
        false
    }

    /// Hides and destroys the GUI. No-op if closed.
    fn gui_close(&mut self);

    /// The plugin asked for a main-thread callback (`HostNotification::MainThreadCallback`); the
    /// bridge calls this soon after on the main thread (CLAP `on_main_thread`).
    fn on_main_thread(&mut self);

    /// A timer registered through [`crate::HostLoop`] fired (CLAP timer-support, VST3 `ITimerHandler`).
    fn on_timer(&mut self, timer: TimerId);

    /// A file descriptor registered through [`crate::HostLoop`] is ready (CLAP posix-fd-support,
    /// VST3 `IEventHandler`). Unix only.
    #[cfg(unix)]
    fn on_fd(&mut self, fd: RawFd, events: FdEvents);
}

/// Audio-thread half of an active plugin instance, created by [`HostedPlugin::activate`].
///
/// Methods run on the bridge RT thread and follow the realtime rules (no allocation, locks, IO or
/// blocking), within what the plugin itself does.
pub trait HostedProcessor: Send {
    /// Starts processing (CLAP `start_processing`, VST3 `setProcessing(true)`).
    fn start_processing(&mut self) -> Result<(), HostingError>;

    /// Stops processing; call before handing the processor back for deactivation.
    fn stop_processing(&mut self);

    /// Processes one block.
    fn process(&mut self, block: &mut ProcessBlock<'_>) -> ProcessStatus;

    /// Begin collection of gain reduction for the next process call.
    fn begin_effect_meter(&mut self, _detector: bool) {}

    /// Return readings after a process call for which effect meters were requested.
    fn end_effect_meter(&mut self) -> Option<komponist_ipc::effect_meter::BlockMeter> {
        None
    }

    /// Routes VST3 controller updates through the host. Called before processing starts.
    fn use_host_controller_sync(&mut self) {}

    /// Last DSP output value per parameter, sorted by ID, when routed through the host.
    /// GUI echo events are separate and still travel through the block's output event list.
    fn controller_updates(&self) -> &[(u32, f64)] {
        &[]
    }

    /// Clears voices and tails (CLAP `reset`; VST3: all-notes-off and a flush at the next block).
    fn reset(&mut self);
}

/// Result of [`HostedProcessor::process`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessStatus {
    /// Output written.
    Continue,
    /// Output written; the plugin is silent until new events arrive (CLAP sleep). The bridge still
    /// calls it every block in v0.1.
    Sleep,
    /// The plugin failed this block; the bridge writes the fallback and reports
    /// `BlockStatus::PluginError`.
    Error,
}

/// One block of audio and events for [`HostedProcessor::process`].
#[derive(Debug)]
pub struct ProcessBlock<'a> {
    /// Frames, 1 ..= the activated `max_frames`.
    pub frames: u32,
    /// Stereo input (effects), `frames` samples each.
    pub input: [&'a [f32]; 2],
    /// The device's sidechain input (#6), `frames` samples each, for the plugin's sidechain port
    /// (a CLAP audio input port besides the main one, a VST3 `kAux` input bus); `None`: the port
    /// gets silence. Ignored by plugins without such a port.
    pub sidechain: Option<[&'a [f32]; 2]>,
    /// Stereo output, `frames` samples each; the plugin writes every frame.
    pub output: [&'a mut [f32]; 2],
    /// Input events sorted by offset (< `frames`).
    pub events_in: &'a [WireEvent],
    /// Sink for the plugin's output events (notes, MIDI, parameter values and gestures).
    pub events_out: EventOut<'a>,
    /// Transport at frame 0.
    pub transport: &'a WireTransport,
    /// Running sample counter (CLAP `steady_time`).
    pub steady_time: u64,
}

/// A fixed-capacity sink for output events; never allocates.
#[derive(Debug)]
pub struct EventOut<'a> {
    slots: &'a mut [WireEvent],
    len: usize,
    dropped: u32,
}

impl<'a> EventOut<'a> {
    /// An empty sink backed by `slots` (capacity = `slots.len()`).
    pub fn new(slots: &'a mut [WireEvent]) -> EventOut<'a> {
        EventOut {
            slots,
            len: 0,
            dropped: 0,
        }
    }

    /// Appends an event; returns `false` (and counts it as dropped) when full.
    pub fn push(&mut self, event: WireEvent) -> bool {
        match self.slots.get_mut(self.len) {
            Some(slot) => {
                *slot = event;
                self.len += 1;
                true
            }
            None => {
                self.dropped += 1;
                false
            }
        }
    }

    /// The events pushed so far.
    pub fn as_slice(&self) -> &[WireEvent] {
        &self.slots[..self.len]
    }

    /// Events dropped because the sink was full.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}
