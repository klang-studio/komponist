//! The control protocol between the app and a bridge (ARCHITECTURE §8.1), plus the plugin
//! vocabulary both sides share (descriptors, parameters, note names).
//!
//! Transport: a Unix `SOCK_STREAM` socket pair, one per bridge, messages encoded with postcard.
//! The app keeps one end and passes the other to the bridge as an inherited fd (`--instance
//! --control-fd N`). Messages larger than [`MAX_FRAME`] (big plugin states) are split into frames
//! and reassembled by [`Channel`]; callers always see whole messages. The shared-memory fd travels
//! over the same socket as SCM_RIGHTS ancillary data (`Channel::send_fd`). What has arrived of the
//! next frame and message is kept in the channel, so [`Channel::try_recv`] takes whatever is
//! queued and never waits for the rest: a peer that stops in the middle of a message (frozen, or
//! plugin code writing to the socket) cannot hold up the app's UI thread and its watchdog (#216).
//!
//! Every frame header carries the frame's own payload length, and the receiver reads each frame
//! exactly (header, then payload), never past its end. So frame boundaries hold on a byte stream,
//! and an fd, which the kernel attaches to the first byte of its frame, always arrives with that
//! frame. The same transport runs on Linux and macOS (#44; macOS has no `SOCK_SEQPACKET` on
//! `AF_UNIX`). OS details: close-on-exec at creation (Linux) or by `fcntl` (macOS); no SIGPIPE
//! through `MSG_NOSIGNAL` (Linux) or `SO_NOSIGPIPE` (macOS); received fds close-on-exec through
//! `MSG_CMSG_CLOEXEC` (Linux) or `fcntl` right after `recvmsg` (macOS); macOS also raises the
//! socket buffers from their 8 KiB default.
//!
//! Windows: the transport is a duplex byte-mode named pipe (one instance, a fresh random name
//! under `\\.\pipe\`, remote clients rejected, both ends opened right away by [`socket_pair`], so
//! nobody else can connect), both ends overlapped, so one thread may block in `recv` while
//! another sends (synchronous handles would serialise the two). Neither end is inheritable; the
//! bridge's end reaches the child as its stdin (see "Command line"). The framing is the same, with
//! one difference: handles cannot travel in-band, so a `TAG_FD` frame carries an 8-byte payload,
//! the handle's value **in the receiving process**: the sender duplicates the handle into the
//! peer first (`DuplicateHandle`, through the peer's process handle, which only the app has), see
//! `Channel::send_handle` / `Channel::send_region`. Only the bridge receives handles. Both ends
//! of a channel always run on the same OS, so [`PROTOCOL_VERSION`] stays the same.
//!
//! Order on a new bridge: bridge sends `Hello`; app sends the memfd, then `Load`, then `SetState`
//! if it has a state, then `Activate` (so the plugin starts processing with its state); the bridge
//! answers `Loaded`, then `RtPriority` and `Activated`. `SetState` has no answer; a failure comes as
//! `Error` with context `State`. Control messages are handled by the bridge's main thread; RT data
//! never goes through the socket.
//!
//! Watchdog: the app sends `Ping` periodically and expects the matching `Pong`. The bridge main
//! thread answers between two plugin calls, so a `Pong` also waits while the main thread is inside
//! a long plugin call (`Load`, `OpenGui`, a state load of a big sample library): the app pauses or
//! stretches the watchdog while such a request is outstanding. The bridge's RT thread is not
//! involved: a plugin that is slow in `process` does not delay the `Pong`.
//!
//! Command line of the bridge:
//!
//! - `komponist-bridge --instance --control-fd <fd> [--rt-priority <1..=99>]`: one plugin instance
//!   on the inherited control socket `fd`. `--rt-priority` sets the `SCHED_FIFO` priority of the
//!   RT thread (default 80, below PipeWire's data loops at 83/88).
//! - Windows: the bridge's end of the pipe is the child's stdin (`Stdio::from(OwnedHandle)`: std
//!   makes only that copy inheritable, under its spawn lock), so no handle value goes on the
//!   command line; the flag that replaces `--control-fd` is W5's to choose.
//! - `komponist-bridge --scan <bundle>`: prints a JSON array of [`PluginDescriptor`] (serde_json,
//!   same field names) and a newline to stdout; plugin output during the scan goes to stderr.
//!   Exit codes: 0 success (the array may be empty), 1 the bundle could not be scanned (reason on
//!   stderr), 2 bad command line, 3 the format has no hosting (unused since CLAP and VST3 are both hosted). A crashing
//!   bundle kills the scan process with its signal.

#[cfg(unix)]
use std::io::{IoSlice, IoSliceMut};
use std::marker::PhantomData;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(windows)]
use std::os::windows::io::{AsHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, Timespec, poll};
#[cfg(unix)]
use rustix::io::Errno;
#[cfg(unix)]
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, recvmsg, sendmsg,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::frame::{FrameChord, FrameKeyEvent, FrameRect, HeaderEvent, HeaderModel};

/// Protocol version; `Hello` carries it and the app refuses other versions. 2: `Ping`/`Pong`,
/// `RtPriority`. 3 (#6): [`PluginDescriptor::sidechain`] (shared memory version 2 carries the
/// sidechain audio). 4 (#52): `OpenGui` takes a [`GuiTarget`] (an X11 child on Linux, the
/// bridge's own frame window on macOS) and the frame messages (`HeaderState`, `ActivateFrame`,
/// `HeaderAction`, `Shortcut`, `HeaderKey`, `FrameMoved`, `CloseRequested`, `GuiClosed` with the
/// frame's last rect, `RaiseFrame` and `FrameWindow`). 5 (#67): the engine consumes parameter
/// edits and a state restore replies with its authoritative parameter values. 8: checked
/// snapshots distinguish delivered, quantized edits and return the verified parameter values.
/// 9 (#75): the shared native frame header carries the track macro mapping chip.
/// 11 adds bounded sampler front-panel data and the slice audition command.
/// 13 distinguishes plugin-originated parameter flushes from value telemetry.
/// 14 (#215): a checked snapshot carries the caller's remaining time.
/// 15 adds optional device catalog labels to scanned and loaded descriptors.
/// 18 distinguishes unsupported file slots from a temporarily missing snapshot.
pub const PROTOCOL_VERSION: u32 = 18;

/// Largest frame written to the socket in one `send`; larger messages are fragmented.
pub const MAX_FRAME: usize = 64 * 1024;

// Frame format: an 8-byte header — tag (1 byte), the payload length of this frame (u24,
// little-endian), the length of the whole encoded message (u32, little-endian, the same in every
// fragment) — then up to `MAX_FRAME - FRAME_HEADER` payload bytes. A message is the payloads of
// its `TAG_MORE` frames followed by one `TAG_LAST` frame; a `TAG_FD` frame carries one fd and no
// payload (Windows: no fd, but the 8-byte little-endian handle value as payload and total).

/// Bytes in front of every frame.
const FRAME_HEADER: usize = 8;
/// Payload bytes per frame.
const MAX_PAYLOAD: usize = MAX_FRAME - FRAME_HEADER;
/// The last (or only) frame of a message.
const TAG_LAST: u8 = 0;
/// A fragment; more follow.
const TAG_MORE: u8 = 1;
/// A file descriptor as SCM_RIGHTS, no payload (Windows: a handle value as payload).
const TAG_FD: u8 = 2;
/// Windows: payload bytes of a `TAG_FD` frame (the handle value as `u64`, little-endian).
#[cfg(windows)]
const HANDLE_PAYLOAD: usize = 8;
/// Largest encoded message either side accepts (a sanity limit against a corrupt peer; plugin
/// states of sample-based instruments reach tens of MiB).
const MAX_MESSAGE: usize = 1 << 30;

// ------------------------------------------------------------------------------------------------
// Shared plugin vocabulary
// ------------------------------------------------------------------------------------------------

/// Plugin format of a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginFormat {
    /// `.clap` bundle.
    Clap,
    /// `.vst3` bundle.
    Vst3,
}

/// Broad kind of a plugin, from CLAP features / VST3 subcategories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    /// Makes sound from notes (CLAP `instrument`, VST3 `Instrument`).
    Instrument,
    /// Processes audio (CLAP `audio-effect`, VST3 `Fx`).
    Effect,
    /// Rewrites note events in front of an instrument (komponist's own MIDI effects, WP-88; the
    /// scan never reports it: foreign note effects stay [`PluginKind::Other`]).
    MidiEffect,
    /// Anything else (note effects, analyzers); not offered in the browser in v0.1.
    Other,
}

/// One plugin found in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// Format of the bundle.
    pub format: PluginFormat,
    /// CLAP plugin id, or the VST3 class id as 32 upper-case hex digits.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional labels from a validated native layout. The descriptor name remains the fallback.
    #[serde(default)]
    pub localized: Option<crate::native_layout::LocalizedPlugin>,
    /// Vendor.
    pub vendor: String,
    /// Version string as reported.
    pub version: String,
    /// Instrument, effect or other.
    pub kind: PluginKind,
    /// CLAP features or VST3 subcategories (split at `|`), lower-case, in plugin order. The
    /// browser categories ("EQ und Filter", …) are derived from them by `komponist-plugins`.
    pub features: Vec<String>,
    /// Absolute path of the bundle (`X.clap` file, or the `X.vst3` directory).
    pub bundle: PathBuf,
    /// Whether the plugin has a sidechain input (#6): a CLAP audio input port besides the main
    /// one, a VST3 audio input bus of type `kAux`. The bridge feeds the device's sidechain into it
    /// (ARCHITECTURE §6.8.2); the device head shows the „SC“ chip. Absent in scan output from
    /// before #6 (read as `false`).
    #[serde(default)]
    pub sidechain: bool,
}

/// Parameter flag bits ([`ParamInfo::flags`]).
pub mod param_flags {
    /// Integer steps (`step_count` > 0).
    pub const STEPPED: u32 = 1;
    /// The plugin's bypass parameter.
    pub const BYPASS: u32 = 2;
    /// Not shown in generic views.
    pub const HIDDEN: u32 = 4;
    /// Read-only (meters, outputs).
    pub const READ_ONLY: u32 = 8;
    /// Automatable.
    pub const AUTOMATABLE: u32 = 16;
    /// Enumerated values: show [`super::ParamValue::text`].
    pub const ENUM: u32 = 32;
    /// The plugin reports the parameter as a macro: the auto-layout shows these first. No host
    /// sets it yet (neither CLAP nor VST3 has the concept; reserved for plugins that tell).
    pub const MACRO: u32 = 64;
}

/// A plugin parameter as the generic parameter view needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    /// Stable parameter id (CLAP `clap_id`, VST3 `ParamID`).
    pub id: u32,
    /// Display name ("Cutoff").
    pub name: String,
    /// Group path ("Filter/Env"), empty if none.
    pub module: String,
    /// Lowest plain value. VST3 parameters are exposed normalized (0..1, stepped = `step_count`
    /// steps of 1/n), because only the controller can convert to plain values; use the plugin's
    /// value text for display.
    pub min: f64,
    /// Highest plain value (1.0 for VST3, see `min`).
    pub max: f64,
    /// Default plain value (Alt-click).
    pub default: f64,
    /// Number of steps for stepped parameters (0 = continuous).
    pub step_count: u32,
    /// [`param_flags`] bits.
    pub flags: u32,
}

/// A parameter value with the plugin's own text for it ("80 Hz", "−1.5 dB").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamValue {
    /// Parameter id.
    pub id: u32,
    /// Plain value.
    pub value: f64,
    /// Text from the plugin (CLAP `value_to_text`, VST3 `getParamStringByValue`); empty if none.
    pub text: String,
}

/// A failed state restore, with any parameter changes the plugin made before returning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRestoreError {
    /// Why the state could not be restored.
    pub message: String,
    /// Changed parameter values, empty when the plugin state was untouched.
    pub changed_values: Vec<ParamValue>,
}

/// A note name the instrument provides (CLAP note-name), used for step sequencer rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoteName {
    /// MIDI key.
    pub key: u8,
    /// MIDI channel it applies to, `None` = all.
    pub channel: Option<u8>,
    /// Name ("Kick").
    pub name: String,
}

/// Realtime or offline processing (CLAP render extension, VST3 process mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// Live playback.
    #[default]
    Realtime,
    /// Export: no realtime pacing, highest quality.
    Offline,
}

/// Size of a plugin GUI: physical pixels on Linux (X11 is not DPI-aware; scale is passed
/// separately), points on macOS (CLAP `cocoa` and VST3 `NSView` size in points).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct GuiSize {
    /// Width in physical pixels (Linux) or points (macOS).
    pub width: u32,
    /// Height in physical pixels (Linux) or points (macOS).
    pub height: u32,
}

/// Where a plugin GUI opens (`ControlMsg::OpenGui`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GuiTarget {
    /// Linux (WP-50): embedded as a child of the X11 window `xid` (the app's frame draws the
    /// header), at UI `scale` (egui `pixels_per_point`).
    X11Child {
        /// X11 window id of the frame's child window.
        xid: u64,
        /// UI scale factor.
        scale: f64,
    },
    /// macOS (#52): the bridge opens a frame window of its own, draws the header from `header` in
    /// the language `lang` (a `Lang` code, "de" or "en") and attaches the plugin's `NSView` below
    /// it. `rect` is where the frame was last (its position is reused; `None`: the system
    /// places it). "Oben halten" is `header.keep_on_top`. The bridge answers `GuiOpened` (size in
    /// points) and from then on reports the frame: `HeaderAction`, `Shortcut`, `HeaderKey`,
    /// `FrameMoved`, `CloseRequested`, and `GuiClosed` with the last rect.
    OwnFrame {
        /// What the header shows.
        header: HeaderModel,
        /// UI language code.
        lang: String,
        /// Where the frame opens.
        rect: Option<FrameRect>,
    },
}

/// Severity of a log line from a bridge or plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Debugging detail.
    Debug,
    /// Information.
    Info,
    /// Something odd but recoverable.
    Warning,
    /// A failure.
    Error,
}

// ------------------------------------------------------------------------------------------------
// Messages
// ------------------------------------------------------------------------------------------------

/// App → bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlMsg {
    /// Query the current sampler UI snapshot from a running plugin.
    GetSamplerUi(u32, Option<f64>),
    /// Start a held slice audition at the source position, or release it with `None`.
    PreviewSamplerSlice(Option<f64>),
    /// Query the live file-slot inventory and offline preparation capability.
    GetDeviceAssets(u32),
    /// Apply a bounded atomic file-slot update on the main thread.
    SetDeviceAssets(u32, Vec<u8>),
    /// Start, poll or clear a detached offline preparation request.
    PrepareDeviceOffline(u32, Option<crate::device_assets::OfflineRequest>),
    /// Load plugin `plugin_id` from `bundle`. Answer: `Loaded` or `Error`.
    Load {
        /// Bundle format.
        format: PluginFormat,
        /// Bundle path.
        bundle: PathBuf,
        /// Plugin id within the bundle.
        plugin_id: String,
    },
    /// Activate at `sample_rate` Hz for blocks up to `max_block` frames and start processing
    /// (bridge state `Active`). Answer: `Activated` or `Error`.
    Activate {
        /// Sample rate in Hz.
        sample_rate: f64,
        /// Largest block in frames (≤ `shm::MAX_BLOCK`).
        max_block: u32,
    },
    /// Stop processing and deactivate (bridge state `Loaded`).
    Deactivate,
    /// Ask for a state snapshot. Answer: `State` with the same `request`.
    GetState {
        /// Echoed in the answer.
        request: u32,
    },
    /// Snapshot only when the listed parameters are back at their persistent values or a
    /// persistent edit has been delivered and quantized by the plugin.
    GetStateChecked {
        /// Echoed in the answer.
        request: u32,
        /// Expected persistent values and whether a delivered edit may be quantized.
        expected: Vec<(u32, f64, bool)>,
        /// Milliseconds the app can still wait for the answer (0: no deadline). It decides
        /// whether a snapshot that keeps moving may still run alongside audio.
        budget_ms: u32,
    },
    /// Restore a state blob (from `State` or the project file). Answer: `StateApplied`.
    SetState {
        /// Echoed in the answer.
        request: u32,
        /// Opaque state.
        state: Vec<u8>,
    },
    /// Ask for the parameter list. Answer: `Params`.
    ListParams,
    /// Set a parameter from the generic view (main thread side; the bridge also feeds it to the
    /// audio side: an event at offset 0 of the next block while active, `flush` while inactive).
    /// The value comes back in `ParamValues` with the plugin's text for it.
    SetParam {
        /// Parameter id.
        id: u32,
        /// Plain value.
        value: f64,
    },
    /// Open the plugin GUI in `GuiTarget`. Answer: `GuiOpened` or `Error`.
    OpenGui(GuiTarget),
    /// The frame window resized the GUI area (answer to `GuiSizeRequest`, or user resize). For a
    /// bridge-owned frame (macOS): resize the body to this size in points as a user resize would
    /// (the plugin may adjust it; the frame follows and reports `FrameMoved`).
    SetGuiSize(GuiSize),
    /// Close the plugin GUI. A bridge-owned frame closes with it, after the bridge yielded the
    /// activation back to the app.
    CloseGui,
    /// What the header of the bridge-owned frame shows, in language `lang` (sent when it changes;
    /// macOS).
    HeaderState {
        /// The header.
        header: HeaderModel,
        /// UI language code.
        lang: String,
    },
    /// Bring the bridge-owned frame to the front and make it key (macOS). The app yields its
    /// activation to the bridge's pid right before (`NSApplication::yieldActivationToApplication`),
    /// so the bridge's `activate` succeeds under cooperative activation (spike #42 Q3).
    ActivateFrame,
    /// komponist became the active application (Dock icon, ⌘-Tab, a click on the main window):
    /// order the bridge-owned frame to the front of its level without activating the bridge or
    /// making the frame key (macOS). The app sends it to its frames back to front, so their order
    /// stays.
    RaiseFrame,
    /// Switch realtime / offline (export); the bridge deactivates and re-activates if needed.
    SetRenderMode(RenderMode),
    /// Deactivate, unload and exit.
    Shutdown,
    /// Watchdog heartbeat; answered with `Pong` carrying the same `seq` (see the module docs).
    Ping {
        /// Echoed in the answer.
        seq: u32,
    },
    /// Query a display list on the bridge's CLAP main thread.
    GetDeviceDisplay(u32),
}

/// Bridge → app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BridgeMsg {
    /// Bounded sampler UI data encoded with the public extension's current schema.
    SamplerUi(u32, Option<Vec<u8>>),
    /// File-slot inventory and capabilities. A supported inventory can be temporarily unavailable.
    DeviceAssets {
        request: u32,
        snapshot: Option<Vec<u8>>,
        file_slots: bool,
        offline: bool,
    },
    /// Whether the plugin accepted the file-slot update.
    DeviceAssetsApplied(u32, bool),
    /// Preparation result. Absent when the extension is unavailable.
    DeviceOfflineStatus(u32, Option<crate::device_assets::OfflineStatus>),
    /// First message after start.
    Hello {
        /// [`PROTOCOL_VERSION`] of the bridge.
        protocol: u32,
        /// Process id of the bridge.
        pid: u32,
    },
    /// Answer to `Load` (boxed: by far the largest message).
    Loaded(Box<LoadedInfo>),
    /// Answer to `Activate`.
    Activated {
        /// Latency in frames after activation.
        latency: u32,
    },
    /// Answer to `GetState`.
    State {
        /// The request number from `GetState`.
        request: u32,
        /// Opaque state blob.
        state: Vec<u8>,
        /// Confirmed plugin values for a checked request, empty for an ordinary request.
        verified: Vec<(u32, f64)>,
    },
    /// The plugin still has a temporary value or has not processed a pending edit.
    StateDeferred {
        /// The original snapshot request.
        request: u32,
    },
    /// Answer to `ListParams`, or unsolicited after the plugin rescanned its parameters.
    Params {
        /// The current parameter inventory.
        params: Vec<ParamInfo>,
        /// Bounded native-layout descriptor revalidated against this inventory.
        native_layout: Option<Vec<u8>>,
    },
    /// Parameter values with the plugin's text, coalesced to at most one message per 33 ms: values
    /// that changed on the bridge's main thread, including automation playback, parameter
    /// rescans and the echo of `SetParam`. GUI edits during processing also arrive as output
    /// events in shared memory.
    ParamValues(Vec<ParamValue>),
    /// Parameter edits emitted by a plugin-requested flush with no host input while
    /// inactive. Edits emitted during processing use the shared output ring.
    PluginParamEdits(Vec<(u32, f64)>),
    /// Answer to `SetState`. Successful values represent the newly restored base state,
    /// independently of any controller updates from automation playback.
    StateApplied {
        /// The restore request.
        request: u32,
        /// Restored parameter values with display text, or the error and partial changes.
        result: Result<Vec<ParamValue>, StateRestoreError>,
    },
    /// Answer to `OpenGui`.
    GuiOpened {
        /// Initial size.
        size: GuiSize,
        /// Whether the user may resize it.
        resizable: bool,
    },
    /// The plugin wants a different GUI size; the app resizes the frame and answers `SetGuiSize`
    /// (Linux; a bridge-owned frame follows its plugin itself).
    GuiSizeRequest(GuiSize),
    /// The GUI was closed (by the plugin, or as the answer to `CloseGui`). `rect`: where a
    /// bridge-owned frame was when it closed (`None` on Linux).
    GuiClosed {
        /// The frame's last rect.
        rect: Option<FrameRect>,
    },
    /// The user clicked in the header of the bridge-owned frame; the app decides (it owns the
    /// state and answers with `HeaderState`).
    HeaderAction(HeaderEvent),
    /// The frame took a key from the focused plugin for komponist (`frame::keys` policy):
    /// `from_plugin` is always `true` for now (keys of the header come as `HeaderKey`).
    Shortcut {
        /// The chord the app applies as on Linux (Esc closes the frame, the rest are global
        /// shortcuts).
        chord: FrameChord,
        /// Taken from the plugin's view (not the header).
        from_plugin: bool,
    },
    /// A key event while the header of the bridge-owned frame has the keyboard focus: the app
    /// applies it as a key of a frame window (global shortcuts, computer-keyboard MIDI, Esc).
    HeaderKey(FrameKeyEvent),
    /// The bridge-owned frame moved or changed its size (debounced).
    FrameMoved(FrameRect),
    /// The window's close button of the bridge-owned frame: the app closes it like × (`CloseGui`).
    CloseRequested,
    /// The bridge-owned frame's window number (the window server's id, the same in every
    /// process), sent after `GuiOpened`: the app finds the frames' order in the window list with
    /// it before it raises them (`RaiseFrame`).
    FrameWindow(i64),
    /// The plugin's latency changed (frames).
    LatencyChanged(u32),
    /// The plugin asked for a restart; the bridge handles it and reports `Activated`.
    RestartRequested,
    /// The plugin state changed (CLAP `mark_dirty`, VST3 edits): snapshot it soon (debounced).
    StateDirty,
    /// The note names changed.
    NoteNames(Vec<NoteName>),
    /// A log line from the bridge or the plugin.
    Log {
        /// Severity.
        level: LogLevel,
        /// Text.
        message: String,
    },
    /// A request failed; the bridge keeps running unless the error is fatal.
    Error {
        /// What failed.
        context: ErrorContext,
        /// Description.
        message: String,
        /// The bridge exits after sending this.
        fatal: bool,
    },
    /// Answer to `Ping`.
    Pong {
        /// The `seq` of the `Ping`.
        seq: u32,
    },
    /// How the bridge's RT thread is scheduled; sent on every activation (also restarts and render
    /// mode changes), right before `Activated`. Without realtime priority the plugin may miss its
    /// deadlines under load (spike WP-15: up to 3 ms): the app shows a warning in the status bar.
    /// The bridge also logs the outcome as a `Log` line starting with "RT priority:".
    RtPriority {
        /// Whether the RT thread runs at `SCHED_FIFO`.
        granted: bool,
        /// The requested `SCHED_FIFO` priority (`--rt-priority`).
        priority: u32,
        /// Why it was refused (the OS error); empty when granted.
        detail: String,
    },
    /// Bounded display list encoded with the public display schema.
    DeviceDisplay(u32, Option<Vec<u8>>),
}

/// Payload of [`BridgeMsg::Loaded`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedInfo {
    /// The loaded plugin.
    pub descriptor: PluginDescriptor,
    /// Parameter list.
    pub params: Vec<ParamInfo>,
    /// Current parameter values.
    pub values: Vec<ParamValue>,
    /// Latency in frames (also in the shared header).
    pub latency: u32,
    /// Note names (empty if the plugin has none).
    pub note_names: Vec<NoteName>,
    /// Whether the plugin has its own GUI this build embeds (X11 on Linux, Cocoa on macOS).
    pub has_gui: bool,
    /// Bounded native CLAP layout descriptor, when available.
    pub native_layout: Option<Vec<u8>>,
}

/// The request a [`BridgeMsg::Error`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorContext {
    /// `Load`.
    Load,
    /// `Activate` / `Deactivate` / restart.
    Activate,
    /// `GetState` / `SetState`.
    State,
    /// Parameters.
    Params,
    /// GUI.
    Gui,
    /// Anything else (protocol, shared memory).
    Other,
}

// ------------------------------------------------------------------------------------------------
// Channel
// ------------------------------------------------------------------------------------------------

/// Why a channel operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// The other side closed the socket (or died).
    Closed,
    /// A socket syscall failed; the OS error text.
    Io(String),
    /// A message could not be encoded or decoded.
    Codec(String),
}

impl core::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ChannelError::Closed => write!(f, "control channel closed"),
            ChannelError::Io(m) => write!(f, "control channel: {m}"),
            ChannelError::Codec(m) => write!(f, "control message codec: {m}"),
        }
    }
}

impl std::error::Error for ChannelError {}

/// One end of a control socket that sends `Tx` and receives `Rx`. Non-RT (main threads).
#[derive(Debug)]
pub struct Channel<Tx, Rx> {
    #[cfg(unix)]
    fd: OwnedFd,
    /// Windows: an overlapped end of the duplex named pipe.
    #[cfg(windows)]
    pipe: OwnedHandle,
    /// What has arrived of the next frame and message (one receiver at a time).
    rx: Mutex<Receiver>,
    /// The base of `received`.
    epoch: Instant,
    /// When bytes last arrived: nanoseconds since `epoch` plus one; 0 before the first.
    received: AtomicU64,
    _msgs: PhantomData<fn(Tx) -> Rx>,
}

/// The app's end.
pub type AppChannel = Channel<ControlMsg, BridgeMsg>;

/// The bridge's end.
pub type BridgeChannel = Channel<BridgeMsg, ControlMsg>;

/// Creates a connected socket pair (`SOCK_STREAM`, close-on-exec): the app's end and the fd for
/// the bridge (to be inherited by the child; the spawner clears `CLOEXEC` on it).
#[cfg(unix)]
pub fn socket_pair() -> Result<(AppChannel, OwnedFd), ChannelError> {
    let (app, bridge) = sys::stream_pair().map_err(io_error)?;
    Ok((Channel::from_fd(app), bridge))
}

/// Windows: creates a connected duplex pipe (module docs): the app's end and the bridge's end,
/// neither inheritable. Give the bridge's end to the child as its stdin
/// (`Command::stdin(Stdio::from(handle))`) and drop the `Command` after spawning, or a dead
/// bridge would never close the pipe.
#[cfg(windows)]
pub fn socket_pair() -> Result<(AppChannel, OwnedHandle), ChannelError> {
    let (app, bridge) = sys::stream_pair().map_err(io_error)?;
    Ok((Channel::from_handle(app), bridge))
}

#[cfg(target_os = "linux")]
mod sys {
    use std::os::fd::OwnedFd;

    use rustix::io::Errno;
    use rustix::net::{AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType, socketpair};

    /// A dead peer must give EPIPE, not kill us with SIGPIPE.
    pub(super) const SEND_FLAGS: SendFlags = SendFlags::NOSIGNAL;
    /// Received fds are close-on-exec atomically.
    pub(super) const RECV_FLAGS: RecvFlags = RecvFlags::CMSG_CLOEXEC;

    pub(super) fn stream_pair() -> Result<(OwnedFd, OwnedFd), Errno> {
        socketpair(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::CLOEXEC,
            None,
        )
    }

    /// Already close-on-exec (`MSG_CMSG_CLOEXEC`).
    pub(super) fn received_fd(fd: OwnedFd) -> OwnedFd {
        fd
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::os::fd::OwnedFd;

    use rustix::io::{Errno, FdFlags, fcntl_setfd};
    use rustix::net::sockopt::{
        set_socket_nosigpipe, set_socket_recv_buffer_size, set_socket_send_buffer_size,
    };
    use rustix::net::{AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType, socketpair};

    use super::MAX_FRAME;

    /// No `MSG_NOSIGNAL` in rustix on macOS: both sockets have `SO_NOSIGPIPE` instead.
    pub(super) const SEND_FLAGS: SendFlags = SendFlags::empty();
    /// No `MSG_CMSG_CLOEXEC` on macOS: see [`received_fd`].
    pub(super) const RECV_FLAGS: RecvFlags = RecvFlags::empty();
    /// Socket buffer size per direction: four full frames (the default for Unix stream sockets is
    /// 8 KiB, which would split every large frame into many context switches).
    const BUFFER: usize = 4 * MAX_FRAME;

    pub(super) fn stream_pair() -> Result<(OwnedFd, OwnedFd), Errno> {
        // No SOCK_CLOEXEC on macOS: until `fcntl`, a child spawned by another thread (a scan
        // worker) would inherit both ends and keep the bridge's socket open. Children are spawned
        // under the read side of the spawn lock (`process::spawn_tied`), this takes the write side.
        let _creating = crate::process::SPAWN_LOCK
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (a, b) = socketpair(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::empty(),
            None,
        )?;
        for fd in [&a, &b] {
            fcntl_setfd(fd, FdFlags::CLOEXEC)?;
            set_socket_nosigpipe(fd, true)?;
            set_socket_send_buffer_size(fd, BUFFER)?;
            set_socket_recv_buffer_size(fd, BUFFER)?;
        }
        Ok((a, b))
    }

    /// Marks a received fd close-on-exec. Between `recvmsg` and this `fcntl` a fork + exec in
    /// another thread of the receiver could inherit the fd: accepted, since the bridge (the side
    /// that receives the shared memory) spawns no children itself; only a plugin that forks in
    /// that moment could (owner, #44). A failure leaves the fd usable, so it is ignored.
    pub(super) fn received_fd(fd: OwnedFd) -> OwnedFd {
        let _ = fcntl_setfd(&fd, FdFlags::CLOEXEC);
        fd
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::Foundation::{
        DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED,
        ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
    };
    use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess};

    use super::{ChannelError, MAX_FRAME};

    /// Pipe buffer quota per direction: four full frames, as on macOS.
    const BUFFER: u32 = 4 * MAX_FRAME as u32;

    /// `handle` from a create call that reports failure as `INVALID_HANDLE_VALUE`.
    fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: a fresh handle that nothing else owns.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    /// The two ends of a fresh duplex byte pipe, both overlapped and not inheritable.
    pub(super) fn stream_pair() -> io::Result<(OwnedHandle, OwnedHandle)> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let pid = std::process::id();
        let mut error = io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32);
        for _ in 0..16 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            let name: Vec<u16> = format!(r"\\.\pipe\komponist-{pid:x}-{n:x}-{clock:x}")
                .encode_utf16()
                .chain([0])
                .collect();
            // SAFETY: a NUL-terminated name; default security (no attributes: not inheritable).
            // One instance only, and FIRST_PIPE_INSTANCE fails if the name exists already.
            let server = owned(unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    BUFFER,
                    BUFFER,
                    0,
                    core::ptr::null(),
                )
            });
            let server = match server {
                Ok(server) => server,
                Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                    error = e;
                    continue;
                }
                Err(e) => return Err(e),
            };
            // SAFETY: as above; the client end may not impersonate beyond identification.
            let client = owned(unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    core::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    core::ptr::null_mut(),
                )
            });
            let client = match client {
                Ok(client) => client,
                // Someone else took the only instance first: never use that pipe.
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    error = e;
                    continue;
                }
                Err(e) => return Err(e),
            };
            // The client is connected already; this only confirms it (ERROR_PIPE_CONNECTED).
            overlapped(&server, |ov| {
                // SAFETY: a valid server end and an OVERLAPPED that outlives the operation.
                unsafe { ConnectNamedPipe(server.as_raw_handle(), ov) }
            })
            .or_else(|e| match e.raw_os_error() {
                Some(code) if code == ERROR_PIPE_CONNECTED as i32 => Ok(0),
                _ => Err(e),
            })?;
            return Ok((server, client));
        }
        Err(error)
    }

    /// Runs one overlapped operation on `pipe` to completion and returns its byte count. `start`
    /// issues it with the given `OVERLAPPED` (which stays valid and pinned until it completes,
    /// also on error: the wait below never returns before).
    fn overlapped(
        pipe: &OwnedHandle,
        start: impl FnOnce(*mut OVERLAPPED) -> windows_sys::core::BOOL,
    ) -> io::Result<u32> {
        // One manual-reset event per operation: a sender and a receiver on two threads must not
        // share one, and this is not an RT path.
        // SAFETY: no attributes, no name.
        let event = owned(unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) })?;
        // SAFETY: a plain C struct for which all zeroes are valid.
        let mut ov: OVERLAPPED = unsafe { core::mem::zeroed() };
        ov.hEvent = event.as_raw_handle();
        if start(&mut ov) == 0 {
            // SAFETY: plain FFI call.
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
        let mut bytes = 0u32;
        // SAFETY: the operation started on `pipe` with `ov`; waits for its completion.
        if unsafe { GetOverlappedResult(pipe.as_raw_handle(), &ov, &mut bytes, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(bytes)
    }

    /// `Closed` for the errors of a pipe whose other end is gone.
    fn pipe_error(e: io::Error) -> ChannelError {
        match e.raw_os_error().map(|c| c as u32) {
            Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED) => {
                ChannelError::Closed
            }
            _ => super::io_error(e),
        }
    }

    /// Writes all of `bytes`.
    pub(super) fn write_all(pipe: &OwnedHandle, mut bytes: &[u8]) -> Result<(), ChannelError> {
        while !bytes.is_empty() {
            let len = bytes.len().min(u32::MAX as usize) as u32;
            let written = overlapped(pipe, |ov| {
                // SAFETY: `bytes` outlives the operation (`overlapped` waits for it).
                unsafe {
                    WriteFile(
                        pipe.as_raw_handle(),
                        bytes.as_ptr(),
                        len,
                        core::ptr::null_mut(),
                        ov,
                    )
                }
            })
            .map_err(pipe_error)?;
            bytes = &bytes[written as usize..];
        }
        Ok(())
    }

    /// Reads at least one byte into `buf` (blocking); 0 only for an empty write of the peer.
    pub(super) fn read(pipe: &OwnedHandle, buf: &mut [u8]) -> Result<usize, ChannelError> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let read = overlapped(pipe, |ov| {
            // SAFETY: `buf` outlives the operation (`overlapped` waits for it).
            unsafe {
                ReadFile(
                    pipe.as_raw_handle(),
                    buf.as_mut_ptr(),
                    len,
                    core::ptr::null_mut(),
                    ov,
                )
            }
        })
        .map_err(pipe_error)?;
        Ok(read as usize)
    }

    /// Bytes queued for reading, without blocking. `Closed` once the other end is gone and
    /// nothing is left.
    pub(super) fn available(pipe: &OwnedHandle) -> Result<u32, ChannelError> {
        let mut available = 0u32;
        // SAFETY: a valid pipe handle; only the count is asked for.
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
                &mut available,
                core::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(pipe_error(io::Error::last_os_error()));
        }
        Ok(available)
    }

    /// Duplicates `handle` into `process` (same access, not inheritable); its value there.
    pub(super) fn duplicate_into(
        handle: BorrowedHandle<'_>,
        process: BorrowedHandle<'_>,
    ) -> io::Result<HANDLE> {
        let mut target: HANDLE = core::ptr::null_mut();
        // SAFETY: valid handles; `target` receives a handle value of the other process.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle.as_raw_handle(),
                process.as_raw_handle(),
                &mut target,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(target)
    }

    /// Closes `value`, a handle that [`duplicate_into`] put into `process` (when its message could
    /// not be sent).
    pub(super) fn close_in(process: BorrowedHandle<'_>, value: HANDLE) {
        // SAFETY: closes the source handle in `process`; no target, nothing is duplicated.
        unsafe {
            DuplicateHandle(
                process.as_raw_handle(),
                value,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                0,
                0,
                DUPLICATE_CLOSE_SOURCE,
            );
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("komponist-ipc: no control-channel backend (control.rs) for this OS");

#[cfg(unix)]
fn io_error(e: Errno) -> ChannelError {
    ChannelError::Io(e.to_string())
}

#[cfg(windows)]
fn io_error(e: std::io::Error) -> ChannelError {
    ChannelError::Io(e.to_string())
}

fn codec_error(message: impl Into<String>) -> ChannelError {
    ChannelError::Codec(message.into())
}

/// The fds that arrived with the bytes of one frame.
#[cfg(unix)]
#[derive(Debug, Default)]
struct Incoming {
    /// The first fd (more are closed).
    fd: Option<OwnedFd>,
    /// The kernel dropped fds for lack of control space (more than one fd in a frame).
    truncated: bool,
}

/// Windows: nothing arrives besides the bytes.
#[cfg(windows)]
#[derive(Debug, Default)]
struct Incoming {}

/// One received frame (its payload is in [`Receiver::payload`] until the next read).
struct Frame {
    tag: u8,
    /// Length of the whole message, from the header.
    total: usize,
    /// Payload bytes in this frame.
    len: usize,
    /// The first fd that came with the frame (more are closed).
    #[cfg(unix)]
    fd: Option<OwnedFd>,
}

/// The receive side of a channel end: what has arrived of the current frame and of a fragmented
/// message, kept across calls so that [`Channel::try_recv`] can return in the middle of either
/// and go on where it stopped (#216). Blocking receives run through the same state.
struct Receiver {
    /// The current frame, header then payload: `MAX_FRAME` bytes, allocated once per channel.
    buf: Box<[u8]>,
    /// Bytes of the current frame received so far.
    filled: usize,
    /// Payload bytes of a refused oversized frame still to be read past (stream sync).
    discard: usize,
    /// The fds that came with the current frame.
    incoming: Incoming,
    /// The payloads received so far of a fragmented message.
    message: Vec<u8>,
    /// The announced length of the fragmented message in `message`; `None` between messages.
    total: Option<usize>,
}

impl core::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Receiver")
            .field("filled", &self.filled)
            .field("discard", &self.discard)
            .field("incoming", &self.incoming)
            .field("message", &self.message.len())
            .field("total", &self.total)
            .finish()
    }
}

impl Receiver {
    fn new() -> Receiver {
        Receiver {
            buf: vec![0; MAX_FRAME].into_boxed_slice(),
            filled: 0,
            discard: 0,
            incoming: Incoming::default(),
            message: Vec::new(),
            total: None,
        }
    }

    /// The payload of the frame [`Channel::next_frame`] returned last.
    fn payload(&self, frame: &Frame) -> &[u8] {
        &self.buf[FRAME_HEADER..FRAME_HEADER + frame.len]
    }

    /// Whether part of a frame or message has arrived (or an oversized frame is being read past).
    fn in_progress(&self) -> bool {
        self.filled > 0 || self.discard > 0 || self.total.is_some()
    }

    /// Forgets the fragmented message (complete or abandoned); a big buffer is given back.
    fn end_message(&mut self) {
        self.total = None;
        if self.message.capacity() > 16 * MAX_FRAME {
            self.message = Vec::new();
        } else {
            self.message.clear();
        }
    }
}

/// Blocks until `fd` is ready for `flags` (used by the blocking calls when someone made the
/// socket non-blocking; never by [`Channel::try_recv`]).
#[cfg(unix)]
fn wait_ready(fd: BorrowedFd<'_>, flags: PollFlags) -> Result<(), ChannelError> {
    let mut fds = [PollFd::from_borrowed_fd(fd, flags)];
    match poll(&mut fds, None) {
        Ok(_) | Err(Errno::INTR) => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}

/// The payload length a frame header announces (u24, little-endian, after the tag).
fn payload_len(header: &[u8]) -> usize {
    u32::from_le_bytes([header[1], header[2], header[3], 0]) as usize
}

/// Decodes exactly one message from `bytes` (trailing bytes are an error).
fn decode<Rx: DeserializeOwned>(bytes: &[u8]) -> Result<Rx, ChannelError> {
    let (msg, rest) = postcard::take_from_bytes(bytes).map_err(|e| codec_error(e.to_string()))?;
    if !rest.is_empty() {
        return Err(codec_error(format!(
            "{} trailing bytes after the message",
            rest.len()
        )));
    }
    Ok(msg)
}

impl<Tx: Serialize, Rx: DeserializeOwned> Channel<Tx, Rx> {
    /// Wraps a connected stream socket (one end of [`socket_pair`]).
    #[cfg(unix)]
    pub fn from_fd(fd: OwnedFd) -> Channel<Tx, Rx> {
        Channel {
            fd,
            rx: Mutex::new(Receiver::new()),
            epoch: Instant::now(),
            received: AtomicU64::new(0),
            _msgs: PhantomData,
        }
    }

    /// Windows: wraps one end of the pipe from [`socket_pair`] (in the bridge: its stdin handle,
    /// `std::io::stdin().as_handle().try_clone_to_owned()`). It must be an overlapped pipe handle.
    #[cfg(windows)]
    pub fn from_handle(pipe: OwnedHandle) -> Channel<Tx, Rx> {
        Channel {
            pipe,
            rx: Mutex::new(Receiver::new()),
            epoch: Instant::now(),
            received: AtomicU64::new(0),
            _msgs: PhantomData,
        }
    }

    /// Sends one message (blocking until the socket accepts all its frames).
    ///
    /// The fragments of a large message go out back to back, so one channel end must not send
    /// from two threads at once. Messages above 1 GiB encoded are refused (`Codec`).
    pub fn send(&self, msg: &Tx) -> Result<(), ChannelError> {
        let bytes = postcard::to_stdvec(msg).map_err(|e| codec_error(e.to_string()))?;
        if bytes.len() > MAX_MESSAGE {
            return Err(codec_error(format!(
                "message of {} bytes exceeds {MAX_MESSAGE}",
                bytes.len()
            )));
        }
        let total = bytes.len();
        let mut sent = 0;
        loop {
            let len = (total - sent).min(MAX_PAYLOAD);
            let tag = if sent + len == total {
                TAG_LAST
            } else {
                TAG_MORE
            };
            #[cfg(unix)]
            self.send_frame(tag, total, &bytes[sent..sent + len], None)?;
            #[cfg(windows)]
            self.send_frame(tag, total, &bytes[sent..sent + len])?;
            sent += len;
            if tag == TAG_LAST {
                return Ok(());
            }
        }
    }

    /// Receives one whole message, blocking.
    pub fn recv(&self) -> Result<Rx, ChannelError> {
        let mut rx = self.receiver();
        loop {
            if let Some(msg) = self.next_message(&mut rx, true, None)? {
                return Ok(msg);
            }
        }
    }

    /// Receives one whole message if it has arrived, never blocking (for poll loops on the UI
    /// thread, #216). A partial frame or the first fragments of a large message are read and
    /// kept in the channel; a later call goes on from there, so a peer that stops in the middle
    /// of a message holds up nobody. `None` also while another thread is inside a blocking
    /// receive on this end.
    pub fn try_recv(&self) -> Result<Option<Rx>, ChannelError> {
        let mut rx = match self.rx.try_lock() {
            Ok(rx) => rx,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(None),
        };
        self.next_message(&mut rx, false, None)
    }

    /// Like [`Channel::try_recv`], but a message that has begun to arrive is read on as long as
    /// its bytes keep coming, until `deadline` at the latest: no read starts after it, also while
    /// the peer keeps the socket readable (what has arrived stays in the channel for the next
    /// call; decoding a message that is complete is not bounded by it). With nothing queued it
    /// returns `None` at once, as `try_recv` does. For the app's poll loop: a bridge that streams a
    /// large state is read at full speed (one socket buffer per poll would hold the bridge's
    /// main thread in `send` for seconds, past its watchdog), while one that stalls in the
    /// middle of a message costs the caller at most until `deadline`.
    pub fn try_recv_until(&self, deadline: Instant) -> Result<Option<Rx>, ChannelError> {
        let mut rx = match self.rx.try_lock() {
            Ok(rx) => rx,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(None),
        };
        loop {
            if let Some(msg) = self.next_message(&mut rx, false, Some(deadline))? {
                return Ok(Some(msg));
            }
            let now = Instant::now();
            if !rx.in_progress() || now >= deadline {
                return Ok(None);
            }
            self.wait_readable(deadline - now)?;
        }
    }

    /// When bytes last arrived on this end (by any receive call); `None` before the first. A
    /// poll loop that reads a large message over many calls uses it as the peer's liveness:
    /// a reply queued behind that message cannot be read before it, but its bytes keep coming.
    pub fn last_received(&self) -> Option<Instant> {
        match self.received.load(Ordering::Relaxed) {
            0 => None,
            n => Some(self.epoch + Duration::from_nanos(n - 1)),
        }
    }

    /// Whether part of a frame or message has arrived and waits for the rest (or another thread
    /// is inside a receive on this end).
    pub fn is_receiving(&self) -> bool {
        match self.rx.try_lock() {
            Ok(rx) => rx.in_progress(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().in_progress(),
            Err(TryLockError::WouldBlock) => true,
        }
    }

    /// Records that bytes arrived now.
    fn note_received(&self) {
        let nanos = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX - 1);
        self.received.store(nanos + 1, Ordering::Relaxed);
    }

    /// The receive state, for a blocking receive.
    fn receiver(&self) -> MutexGuard<'_, Receiver> {
        self.rx.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits up to `timeout` for bytes to read (or the end of the stream).
    #[cfg(unix)]
    fn wait_readable(&self, timeout: Duration) -> Result<(), ChannelError> {
        let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(Errno::INTR) => Ok(()),
            Err(e) => Err(io_error(e)),
        }
    }

    /// Windows: a pipe cannot be waited on for "readable"; look again shortly.
    #[cfg(windows)]
    fn wait_readable(&self, timeout: Duration) -> Result<(), ChannelError> {
        std::thread::sleep(timeout.min(Duration::from_millis(1)));
        Ok(())
    }

    /// Windows: duplicates `handle` into `peer` (the bridge's process handle, e.g. its
    /// `ProcessWatcher`, which has `PROCESS_DUP_HANDLE`) and sends the new value as its own frame.
    /// If the frame cannot be sent, the duplicate in `peer` is closed again.
    #[cfg(windows)]
    pub fn send_handle(
        &self,
        handle: BorrowedHandle<'_>,
        peer: BorrowedHandle<'_>,
    ) -> Result<(), ChannelError> {
        let value = sys::duplicate_into(handle, peer).map_err(io_error)?;
        let payload = (value as usize as u64).to_le_bytes();
        let sent = self.send_frame(TAG_FD, HANDLE_PAYLOAD, &payload);
        if sent.is_err() {
            sys::close_in(peer, value);
        }
        sent
    }

    /// Windows: receives a handle sent with [`Channel::send_handle`], blocking.
    ///
    /// A message frame in its place is consumed and reported as `Codec`.
    ///
    /// # Safety
    ///
    /// The peer is trusted: the value it sends must be a handle it duplicated into this process
    /// for this message ([`Channel::send_handle`]) and that nothing else here owns; a lying peer
    /// would make the returned `OwnedHandle` close a handle that belongs to someone else. Only the
    /// bridge receives handles, from the app that started it.
    #[cfg(windows)]
    pub unsafe fn recv_handle(&self) -> Result<OwnedHandle, ChannelError> {
        let mut rx = self.receiver();
        let frame = self.next_frame_blocking(&mut rx)?;
        match (frame.tag, frame.len) {
            (TAG_FD, HANDLE_PAYLOAD) => {
                let payload = rx.payload(&frame);
                let value = u64::from_le_bytes(payload.try_into().unwrap()) as usize as RawHandle;
                if value.is_null() || value as isize == -1 {
                    return Err(codec_error("handle frame without a valid handle"));
                }
                // SAFETY: per the caller, a handle the peer put into this process for us.
                Ok(unsafe { OwnedHandle::from_raw_handle(value) })
            }
            (TAG_FD, _) => Err(codec_error("handle frame without a handle")),
            _ => Err(codec_error("expected a handle, got a message frame")),
        }
    }

    /// Windows: sends the handles of `region` (section, request event, done event) to the bridge
    /// `peer`, as three handle frames (see [`Channel::send_handle`]). The counterpart of
    /// `send_fd(region.fd())` on Unix.
    #[cfg(windows)]
    pub fn send_region(
        &self,
        region: &crate::shm::ShmRegion,
        peer: BorrowedHandle<'_>,
    ) -> Result<(), ChannelError> {
        for handle in region.handles() {
            self.send_handle(handle, peer)?;
        }
        Ok(())
    }

    /// Windows: receives the handles sent with [`Channel::send_region`], for
    /// [`ShmRegion::open`](crate::shm::ShmRegion::open).
    ///
    /// # Safety
    ///
    /// As for [`Channel::recv_handle`].
    #[cfg(windows)]
    pub unsafe fn recv_region(&self) -> Result<crate::shm::ShmHandles, ChannelError> {
        // SAFETY: per the caller.
        unsafe {
            Ok(crate::shm::ShmHandles {
                mapping: self.recv_handle()?,
                request: self.recv_handle()?,
                done: self.recv_handle()?,
            })
        }
    }

    /// Windows: writes one frame, header then payload (one sender at a time, so the two writes
    /// stay together on the stream).
    #[cfg(windows)]
    fn send_frame(&self, tag: u8, total: usize, payload: &[u8]) -> Result<(), ChannelError> {
        debug_assert!(payload.len() <= MAX_PAYLOAD);
        let mut header = [0u8; FRAME_HEADER];
        header[0] = tag;
        header[1..4].copy_from_slice(&(payload.len() as u32).to_le_bytes()[..3]);
        header[4..].copy_from_slice(&(total as u32).to_le_bytes());
        sys::write_all(&self.pipe, &header)?;
        sys::write_all(&self.pipe, payload)
    }

    /// Windows: reads up to `buf.len()` bytes from the pipe (never more, so no read crosses the
    /// end of a frame). With `wait` it blocks for the first byte; without, it reads only what is
    /// queued and returns 0 when nothing is (the caller holds the receive lock, so nobody takes
    /// the queued bytes in between). A pipe whose other end is gone is `Closed` (after what it
    /// still holds was read).
    #[cfg(windows)]
    fn read_some(
        &self,
        buf: &mut [u8],
        wait: bool,
        _incoming: &mut Incoming,
    ) -> Result<usize, ChannelError> {
        let len = if wait {
            buf.len()
        } else {
            (sys::available(&self.pipe)? as usize).min(buf.len())
        };
        if len == 0 {
            return Ok(0);
        }
        sys::read(&self.pipe, &mut buf[..len])
    }

    /// Sends a file descriptor (SCM_RIGHTS) as its own frame.
    #[cfg(unix)]
    pub fn send_fd(&self, fd: BorrowedFd<'_>) -> Result<(), ChannelError> {
        self.send_frame(TAG_FD, 0, &[], Some(fd))
    }

    /// Receives a file descriptor sent with [`Channel::send_fd`], blocking.
    ///
    /// A message frame in its place is consumed and reported as `Codec`.
    #[cfg(unix)]
    pub fn recv_fd(&self) -> Result<OwnedFd, ChannelError> {
        let frame = self.next_frame_blocking(&mut self.receiver())?;
        match (frame.tag, frame.fd) {
            (TAG_FD, Some(fd)) => Ok(fd),
            (TAG_FD, None) => Err(codec_error("file descriptor frame without a descriptor")),
            _ => Err(codec_error(
                "expected a file descriptor, got a message frame",
            )),
        }
    }

    /// Writes one frame: header, `payload` and optionally `fd` as SCM_RIGHTS (with the first
    /// byte). A stream socket may take the frame in several writes; the rest follows at once.
    #[cfg(unix)]
    fn send_frame(
        &self,
        tag: u8,
        total: usize,
        payload: &[u8],
        fd: Option<BorrowedFd<'_>>,
    ) -> Result<(), ChannelError> {
        debug_assert!(payload.len() <= MAX_PAYLOAD);
        let mut header = [0u8; FRAME_HEADER];
        header[0] = tag;
        header[1..4].copy_from_slice(&(payload.len() as u32).to_le_bytes()[..3]);
        header[4..].copy_from_slice(&(total as u32).to_le_bytes());
        let fds = fd.map(|fd| [fd]);
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if let Some(fds) = &fds {
            control.push(SendAncillaryMessage::ScmRights(fds));
        }
        let len = FRAME_HEADER + payload.len();
        let mut sent = 0;
        while sent < len {
            let iov = if sent < FRAME_HEADER {
                [IoSlice::new(&header[sent..]), IoSlice::new(payload)]
            } else {
                [
                    IoSlice::new(&payload[sent - FRAME_HEADER..]),
                    IoSlice::new(&[]),
                ]
            };
            // The fd goes with the first write only.
            let result = if sent == 0 {
                sendmsg(&self.fd, &iov, &mut control, sys::SEND_FLAGS)
            } else {
                sendmsg(
                    &self.fd,
                    &iov,
                    &mut SendAncillaryBuffer::default(),
                    sys::SEND_FLAGS,
                )
            };
            match result {
                Ok(0) => {
                    return Err(ChannelError::Io(format!(
                        "short send: {sent} of {len} bytes"
                    )));
                }
                Ok(n) => sent += n,
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => wait_ready(self.fd.as_fd(), PollFlags::OUT)?,
                Err(Errno::PIPE | Errno::CONNRESET) => return Err(ChannelError::Closed),
                Err(e) => return Err(io_error(e)),
            }
        }
        Ok(())
    }

    /// Reads up to `buf.len()` bytes from the stream (never more, so no read crosses the end of
    /// a frame and every fd stays with its frame), collecting the fds that come with them. With
    /// `wait` it blocks for the first byte; without, it returns 0 when nothing is queued. End of
    /// stream is `Closed`.
    #[cfg(unix)]
    fn read_some(
        &self,
        buf: &mut [u8],
        wait: bool,
        incoming: &mut Incoming,
    ) -> Result<usize, ChannelError> {
        let flags = if wait {
            sys::RECV_FLAGS
        } else {
            sys::RECV_FLAGS | RecvFlags::DONTWAIT
        };
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        loop {
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let mut iov = [IoSliceMut::new(buf)];
            let msg = match recvmsg(&self.fd, &mut iov, &mut control, flags) {
                Ok(msg) => msg,
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) if !wait => return Ok(0),
                Err(Errno::AGAIN) => {
                    wait_ready(self.fd.as_fd(), PollFlags::IN)?;
                    continue;
                }
                Err(Errno::CONNRESET) => return Err(ChannelError::Closed),
                Err(e) => return Err(io_error(e)),
            };
            // Take ownership of every received fd first, so none leaks whatever happens next.
            for message in control.drain() {
                if let RecvAncillaryMessage::ScmRights(fds) = message {
                    for received in fds {
                        if incoming.fd.is_none() {
                            incoming.fd = Some(sys::received_fd(received));
                        }
                    }
                }
            }
            if msg.flags.contains(ReturnFlags::CTRUNC) {
                incoming.truncated = true;
            }
            // Every frame has a header, so an empty read is the end of the stream (also in the
            // middle of a frame: the peer died while writing it).
            if msg.bytes == 0 {
                return Err(ChannelError::Closed);
            }
            return Ok(msg.bytes);
        }
    }

    /// Reads on with the current frame. `None` when it is not complete yet (only without `wait`;
    /// on Windows also after an empty write of the peer): what arrived is kept in `rx`.
    ///
    /// An oversized frame is refused (`Codec`) once its header is in; its payload is read past
    /// before the next frame, so the stream stays in step. No read starts after `deadline`
    /// (`None` then, too).
    fn next_frame(
        &self,
        rx: &mut Receiver,
        wait: bool,
        deadline: Option<Instant>,
    ) -> Result<Option<Frame>, ChannelError> {
        let expired = || deadline.is_some_and(|deadline| Instant::now() >= deadline);
        loop {
            if rx.discard > 0 {
                if expired() {
                    return Ok(None);
                }
                let n = rx.discard.min(MAX_FRAME);
                let got = self.read_some(&mut rx.buf[..n], wait, &mut rx.incoming)?;
                if got == 0 {
                    return Ok(None);
                }
                self.note_received();
                rx.discard -= got;
                if rx.discard == 0 {
                    rx.incoming = Incoming::default();
                }
                continue;
            }
            let end = if rx.filled < FRAME_HEADER {
                FRAME_HEADER
            } else {
                FRAME_HEADER + payload_len(&rx.buf)
            };
            if rx.filled == end {
                let frame = Frame {
                    tag: rx.buf[0],
                    total: u32::from_le_bytes(rx.buf[4..8].try_into().unwrap()) as usize,
                    len: end - FRAME_HEADER,
                    #[cfg(unix)]
                    fd: rx.incoming.fd.take(),
                };
                rx.filled = 0;
                #[cfg(unix)]
                if std::mem::take(&mut rx.incoming.truncated) {
                    return Err(codec_error("too many file descriptors in one frame"));
                }
                return Ok(Some(frame));
            }
            if expired() {
                return Ok(None);
            }
            let got = self.read_some(&mut rx.buf[rx.filled..end], wait, &mut rx.incoming)?;
            if got == 0 {
                return Ok(None);
            }
            self.note_received();
            rx.filled += got;
            if rx.filled == FRAME_HEADER {
                let len = payload_len(&rx.buf);
                if len > MAX_PAYLOAD {
                    rx.filled = 0;
                    rx.discard = len;
                    rx.incoming = Incoming::default();
                    return Err(codec_error(format!("frame larger than {MAX_FRAME} bytes")));
                }
            }
        }
    }

    /// The next whole frame, blocking.
    fn next_frame_blocking(&self, rx: &mut Receiver) -> Result<Frame, ChannelError> {
        loop {
            if let Some(frame) = self.next_frame(rx, true, None)? {
                return Ok(frame);
            }
        }
    }

    /// Reads on until a whole message is in. `None` when it is not yet (only without `wait`, or
    /// once `deadline` has passed; on Windows also after an empty write): the fragments so far
    /// are kept in `rx`. A broken message, also one cut by a refused frame, is abandoned with a
    /// `Codec` error; the stream stays in step.
    fn next_message(
        &self,
        rx: &mut Receiver,
        wait: bool,
        deadline: Option<Instant>,
    ) -> Result<Option<Rx>, ChannelError> {
        loop {
            let frame = match self.next_frame(rx, wait, deadline) {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                Err(e) => {
                    // A refused frame (oversized, too many fds) ends the message it was part of:
                    // the next one must not be measured against it. `discard` stays, so the
                    // refused payload is still read past.
                    if matches!(e, ChannelError::Codec(_)) {
                        rx.end_message();
                    }
                    return Err(e);
                }
            };
            let Some(total) = rx.total else {
                // The first (or only) frame of a message.
                let total = frame.total;
                match frame.tag {
                    TAG_LAST if frame.len == total => return decode(rx.payload(&frame)).map(Some),
                    TAG_LAST => {
                        return Err(codec_error(format!(
                            "frame of {} bytes announces a message of {total}",
                            frame.len
                        )));
                    }
                    TAG_MORE if total > MAX_MESSAGE => {
                        return Err(codec_error(format!(
                            "announced message of {total} bytes exceeds {MAX_MESSAGE}"
                        )));
                    }
                    TAG_MORE if frame.len >= total => {
                        return Err(codec_error("fragment as long as the whole message"));
                    }
                    TAG_MORE => {
                        // Grow with the data rather than trusting the announced length up front.
                        rx.message.reserve(total.min(16 * MAX_FRAME));
                        let Receiver { buf, message, .. } = &mut *rx;
                        message.extend_from_slice(&buf[FRAME_HEADER..FRAME_HEADER + frame.len]);
                        rx.total = Some(total);
                        continue;
                    }
                    TAG_FD => {
                        return Err(codec_error(
                            "expected a message, got a file descriptor frame",
                        ));
                    }
                    tag => return Err(codec_error(format!("unknown frame tag {tag}"))),
                }
            };
            if frame.tag != TAG_MORE && frame.tag != TAG_LAST {
                rx.end_message();
                return Err(codec_error("unexpected frame inside a fragmented message"));
            }
            if frame.total != total || rx.message.len() + frame.len > total {
                rx.end_message();
                return Err(codec_error("fragment lengths do not add up"));
            }
            let Receiver { buf, message, .. } = &mut *rx;
            message.extend_from_slice(&buf[FRAME_HEADER..FRAME_HEADER + frame.len]);
            if frame.tag == TAG_LAST {
                let msg = if rx.message.len() == total {
                    decode(&rx.message)
                } else {
                    Err(codec_error("fragmented message ends early"))
                };
                rx.end_message();
                return msg.map(Some);
            }
        }
    }
}

#[cfg(unix)]
impl<Tx, Rx> AsFd for Channel<Tx, Rx> {
    /// The socket, for `poll()`.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(windows)]
impl<Tx, Rx> AsHandle for Channel<Tx, Rx> {
    /// The pipe end (overlapped). A pipe handle is not a waitable "readable" object: a bridge
    /// main loop that waits for messages needs its own overlapped read or a reader thread.
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.pipe.as_handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{KeyMods, Mark, PluginView};
    use crate::shm::ShmRegion;

    #[cfg(unix)]
    fn pair() -> (AppChannel, BridgeChannel) {
        let (app, fd) = socket_pair().unwrap();
        (app, BridgeChannel::from_fd(fd))
    }

    #[cfg(windows)]
    fn pair() -> (AppChannel, BridgeChannel) {
        let (app, handle) = socket_pair().unwrap();
        (app, BridgeChannel::from_handle(handle))
    }

    /// Writes raw bytes (bypassing the encoder), all of them.
    #[cfg(unix)]
    fn raw_send(channel: &AppChannel, bytes: &[u8]) {
        let mut sent = 0;
        while sent < bytes.len() {
            sent += rustix::net::send(channel.as_fd(), &bytes[sent..], sys::SEND_FLAGS).unwrap();
        }
    }

    /// Writes raw bytes (bypassing the encoder), all of them.
    #[cfg(windows)]
    fn raw_send(channel: &AppChannel, bytes: &[u8]) {
        sys::write_all(&channel.pipe, bytes).unwrap();
    }

    /// Windows: this process, as the "peer" of handles sent within one test process.
    #[cfg(windows)]
    fn this_process() -> BorrowedHandle<'static> {
        // SAFETY: the pseudo handle of the current process (-1) is valid for the whole run.
        unsafe {
            BorrowedHandle::borrow_raw(windows_sys::Win32::System::Threading::GetCurrentProcess())
        }
    }

    /// Windows: whether `handle` is inheritable.
    #[cfg(windows)]
    fn inheritable(handle: BorrowedHandle<'_>) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
        let mut flags = 0;
        // SAFETY: a valid handle and an out pointer.
        let ok = unsafe { GetHandleInformation(handle.as_raw_handle(), &mut flags) };
        assert_ne!(ok, 0);
        flags & HANDLE_FLAG_INHERIT != 0
    }

    /// A frame whose header announces `payload.len()` bytes.
    fn frame(tag: u8, total: u32, payload: &[u8]) -> Vec<u8> {
        frame_announcing(tag, payload.len() as u32, total, payload)
    }

    /// A frame whose header announces `len` payload bytes, whatever follows.
    fn frame_announcing(tag: u8, len: u32, total: u32, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![tag];
        f.extend_from_slice(&len.to_le_bytes()[..3]);
        f.extend_from_slice(&total.to_le_bytes());
        f.extend_from_slice(payload);
        f
    }

    fn header() -> HeaderModel {
        HeaderModel {
            name: "Surge XT".into(),
            sub: "Pad · CLAP".into(),
            mark: Mark::Bypassed,
            bypassed: true,
            presets: vec!["Init".into()],
            preset: Some(0),
            preset_changed: true,
            can_save: false,
            view: PluginView::Gui,
            has_gui: true,
            keep_on_top: true,
            close_shortcut: "Esc".into(),
            mapping_chip: Some("Map · Track macro 1".into()),
        }
    }

    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::Clap,
            id: "org.example.synth".into(),
            name: "Synth".into(),
            localized: Some(crate::native_layout::LocalizedPlugin {
                default_locale: "en".into(),
                names: [
                    ("en".into(), "Synth".into()),
                    ("de".into(), "Synthesizer".into()),
                ]
                .into(),
                descriptions: [("en".into(), "A test instrument".into())].into(),
            }),
            vendor: "Example".into(),
            version: "1.0".into(),
            kind: PluginKind::Instrument,
            features: vec!["instrument".into(), "synthesizer".into()],
            bundle: "/usr/lib/clap/Synth.clap".into(),
            sidechain: false,
        }
    }

    #[test]
    fn every_control_message_round_trips() {
        let (app, bridge) = pair();
        let messages = vec![
            ControlMsg::Load {
                format: PluginFormat::Vst3,
                bundle: "/x/Ü.vst3".into(),
                plugin_id: "0123456789ABCDEF0123456789ABCDEF".into(),
            },
            ControlMsg::Activate {
                sample_rate: 48_000.0,
                max_block: 2048,
            },
            ControlMsg::Deactivate,
            ControlMsg::GetState { request: 7 },
            ControlMsg::GetStateChecked {
                request: 3,
                expected: vec![(1, 0.4, true)],
                budget_ms: 1500,
            },
            ControlMsg::SetState {
                request: 9,
                state: vec![1, 2, 3],
            },
            ControlMsg::ListParams,
            ControlMsg::SetParam { id: 3, value: -0.5 },
            ControlMsg::OpenGui(GuiTarget::X11Child {
                xid: 0x0400_0007,
                scale: 1.5,
            }),
            ControlMsg::OpenGui(GuiTarget::OwnFrame {
                header: header(),
                lang: "de".into(),
                rect: Some(FrameRect {
                    x: -1200.0,
                    y: 40.5,
                    width: 1141.0,
                    height: 739.0,
                }),
            }),
            ControlMsg::OpenGui(GuiTarget::OwnFrame {
                header: header(),
                lang: "en".into(),
                rect: None,
            }),
            ControlMsg::HeaderState {
                header: header(),
                lang: "en".into(),
            },
            ControlMsg::ActivateFrame,
            ControlMsg::RaiseFrame,
            ControlMsg::SetGuiSize(GuiSize {
                width: 800,
                height: 600,
            }),
            ControlMsg::CloseGui,
            ControlMsg::SetRenderMode(RenderMode::Offline),
            ControlMsg::Shutdown,
            ControlMsg::Ping { seq: u32::MAX },
        ];
        for msg in &messages {
            app.send(msg).unwrap();
            assert_eq!(&bridge.recv().unwrap(), msg);
        }
    }

    #[test]
    fn every_bridge_message_round_trips() {
        let (app, bridge) = pair();
        let param = ParamInfo {
            id: 1,
            name: "Cutoff".into(),
            module: "Filter".into(),
            min: 20.0,
            max: 20_000.0,
            default: 1_000.0,
            step_count: 0,
            flags: param_flags::AUTOMATABLE,
        };
        let value = ParamValue {
            id: 1,
            value: 80.0,
            text: "80 Hz".into(),
        };
        let note = NoteName {
            key: 36,
            channel: None,
            name: "Kick".into(),
        };
        let messages = vec![
            BridgeMsg::DeviceAssets {
                request: 9,
                snapshot: None,
                file_slots: true,
                offline: false,
            },
            BridgeMsg::Hello {
                protocol: PROTOCOL_VERSION,
                pid: 4242,
            },
            BridgeMsg::Loaded(Box::new(LoadedInfo {
                descriptor: descriptor(),
                params: vec![param.clone()],
                values: vec![value.clone()],
                latency: 64,
                note_names: vec![note.clone()],
                has_gui: true,
                native_layout: Some(b"{}".to_vec()),
            })),
            BridgeMsg::Activated { latency: 64 },
            BridgeMsg::State {
                request: 7,
                state: vec![9; 100],
                verified: vec![(1, 0.4)],
            },
            BridgeMsg::StateDeferred { request: 11 },
            BridgeMsg::Params {
                params: vec![param.clone()],
                native_layout: None,
            },
            BridgeMsg::Params {
                params: vec![param],
                native_layout: Some(
                    (0..crate::native_layout::MAX_BYTES)
                        .map(|i| (i * 7 + i / 251) as u8)
                        .collect(),
                ),
            },
            BridgeMsg::ParamValues(vec![value.clone()]),
            BridgeMsg::PluginParamEdits(vec![(value.id, value.value)]),
            BridgeMsg::StateApplied {
                request: 8,
                result: Ok(vec![value.clone()]),
            },
            BridgeMsg::StateApplied {
                request: 9,
                result: Err(StateRestoreError {
                    message: "bad state".into(),
                    changed_values: vec![value],
                }),
            },
            BridgeMsg::GuiOpened {
                size: GuiSize {
                    width: 640,
                    height: 480,
                },
                resizable: false,
            },
            BridgeMsg::GuiSizeRequest(GuiSize {
                width: 1,
                height: 2,
            }),
            BridgeMsg::GuiClosed { rect: None },
            BridgeMsg::GuiClosed {
                rect: Some(FrameRect {
                    x: 20.0,
                    y: 60.0,
                    width: 866.0,
                    height: 702.0,
                }),
            },
            BridgeMsg::HeaderAction(HeaderEvent::ToggleBypass),
            BridgeMsg::HeaderAction(HeaderEvent::Preset(3)),
            BridgeMsg::HeaderAction(HeaderEvent::View(PluginView::Params)),
            BridgeMsg::HeaderAction(HeaderEvent::KeepOnTop(true)),
            BridgeMsg::Shortcut {
                chord: FrameChord::command_shift("Z"),
                from_plugin: true,
            },
            BridgeMsg::HeaderKey(FrameKeyEvent {
                key: "A".into(),
                physical_key: Some("Q".into()),
                pressed: true,
                repeat: false,
                mods: KeyMods {
                    shift: true,
                    ..KeyMods::default()
                },
            }),
            BridgeMsg::FrameMoved(FrameRect {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
            }),
            BridgeMsg::CloseRequested,
            BridgeMsg::FrameWindow(4711),
            BridgeMsg::LatencyChanged(128),
            BridgeMsg::RestartRequested,
            BridgeMsg::StateDirty,
            BridgeMsg::NoteNames(vec![note]),
            BridgeMsg::Log {
                level: LogLevel::Warning,
                message: "„Lädt“ − slow".into(),
            },
            BridgeMsg::Error {
                context: ErrorContext::Gui,
                message: "no X11".into(),
                fatal: false,
            },
            BridgeMsg::Pong { seq: 17 },
            BridgeMsg::RtPriority {
                granted: false,
                priority: 80,
                detail: "Operation not permitted (os error 1)".into(),
            },
        ];
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for msg in &messages {
                    bridge.send(msg).unwrap();
                }
            });
            for msg in &messages {
                assert_eq!(&app.recv().unwrap(), msg);
            }
        });
    }

    #[test]
    fn large_messages_are_fragmented_and_reassembled() {
        let (app, bridge) = pair();
        // Sizes around the fragment boundary and one far beyond the socket buffer.
        let sizes = [
            0,
            MAX_PAYLOAD - 4,
            MAX_PAYLOAD - 3,
            MAX_PAYLOAD,
            MAX_PAYLOAD + 1,
            3 * MAX_PAYLOAD,
            4 * 1024 * 1024 + 3,
        ];
        let states: Vec<Vec<u8>> = sizes
            .iter()
            .map(|&n| (0..n).map(|i| (i * 7 + i / 251) as u8).collect())
            .collect();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for state in &states {
                    let msg = BridgeMsg::State {
                        request: state.len() as u32,
                        state: state.clone(),
                        verified: Vec::new(),
                    };
                    bridge.send(&msg).unwrap();
                }
            });
            for state in &states {
                match app.recv().unwrap() {
                    BridgeMsg::State {
                        request,
                        state: got,
                        ..
                    } => {
                        assert_eq!(request as usize, state.len());
                        assert!(got == *state, "state of {} bytes differs", state.len());
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        });
    }

    #[test]
    fn try_recv_does_not_block() {
        let (app, bridge) = pair();
        assert_eq!(bridge.try_recv().unwrap(), None);
        app.send(&ControlMsg::ListParams).unwrap();
        assert_eq!(bridge.try_recv().unwrap(), Some(ControlMsg::ListParams));
        assert_eq!(bridge.try_recv().unwrap(), None);
        // A fragmented message arrives once all its fragments are in.
        let big = ControlMsg::SetState {
            request: 9,
            state: vec![5; 3 * MAX_FRAME],
        };
        std::thread::scope(|scope| {
            scope.spawn(|| app.send(&big).unwrap());
            loop {
                if let Some(msg) = bridge.try_recv().unwrap() {
                    assert_eq!(msg, big);
                    break;
                }
                std::thread::yield_now();
            }
        });
    }

    /// Windows: the region's three handles arrive as new handles of the receiving process (here
    /// the same one), not inheritable, and map the same memory.
    #[test]
    #[cfg(windows)]
    fn a_region_travels_over_the_pipe() {
        let (app, bridge) = pair();
        let region = ShmRegion::create().unwrap();
        app.send_region(&region, this_process()).unwrap();
        app.send(&ControlMsg::ListParams).unwrap();
        // SAFETY: the peer is `app` above, which duplicated the handles into this process.
        let handles = unsafe { bridge.recv_region() }.unwrap();
        assert_eq!(bridge.recv().unwrap(), ControlMsg::ListParams);
        for handle in [&handles.mapping, &handles.request, &handles.done] {
            assert!(!inheritable(handle.as_handle()));
        }
        let mapped = ShmRegion::open(handles).unwrap();
        mapped.set_latency(321);
        assert_eq!(region.latency(), 321);
    }

    #[test]
    #[cfg(windows)]
    fn handles_stay_with_their_frames_between_messages() {
        let (app, bridge) = pair();
        let a = ShmRegion::create().unwrap();
        let b = ShmRegion::create().unwrap();
        a.set_latency(1);
        b.set_latency(2);
        let big = ControlMsg::SetState {
            request: 9,
            state: (0..3 * MAX_FRAME).map(|i| i as u8).collect(),
        };
        // Queued back to back before anything is read.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                app.send(&ControlMsg::ListParams).unwrap();
                app.send_region(&a, this_process()).unwrap();
                app.send(&big).unwrap();
                app.send_region(&b, this_process()).unwrap();
                app.send(&ControlMsg::Deactivate).unwrap();
            });
            // SAFETY: the peer is `app` above, which sends both regions.
            let open = || ShmRegion::open(unsafe { bridge.recv_region() }.unwrap()).unwrap();
            assert_eq!(bridge.recv().unwrap(), ControlMsg::ListParams);
            assert_eq!(open().latency(), 1);
            // Read piecewise while the sender is still writing it (#216).
            assert_eq!(poll_until_message(&bridge), big);
            assert_eq!(open().latency(), 2);
            assert_eq!(bridge.recv().unwrap(), ControlMsg::Deactivate);
        });
    }

    /// Windows: a message sent right before the peer closes is still received; then `Closed`.
    #[test]
    #[cfg(windows)]
    fn messages_before_close_are_received() {
        let (app, bridge) = pair();
        bridge.send(&BridgeMsg::StateDirty).unwrap();
        bridge.send(&BridgeMsg::Pong { seq: 3 }).unwrap();
        drop(bridge);
        assert_eq!(app.try_recv().unwrap(), Some(BridgeMsg::StateDirty));
        assert_eq!(app.recv().unwrap(), BridgeMsg::Pong { seq: 3 });
        assert_eq!(app.try_recv().unwrap_err(), ChannelError::Closed);
    }

    /// Windows: one thread blocked in `recv` does not hold up a `send` on the same end.
    #[test]
    #[cfg(windows)]
    fn send_while_another_thread_receives() {
        let (app, bridge) = pair();
        std::thread::scope(|scope| {
            let receiver = scope.spawn(|| app.recv().unwrap());
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.send(&ControlMsg::Ping { seq: 1 }).unwrap();
            assert_eq!(bridge.recv().unwrap(), ControlMsg::Ping { seq: 1 });
            bridge.send(&BridgeMsg::Pong { seq: 1 }).unwrap();
            assert_eq!(receiver.join().unwrap(), BridgeMsg::Pong { seq: 1 });
        });
    }

    #[test]
    #[cfg(unix)]
    fn a_memfd_travels_over_the_socket() {
        let (app, bridge) = pair();
        let region = ShmRegion::create().unwrap();
        app.send_fd(region.fd()).unwrap();
        app.send(&ControlMsg::ListParams).unwrap();
        let fd = bridge.recv_fd().unwrap();
        assert_eq!(bridge.recv().unwrap(), ControlMsg::ListParams);
        let flags = rustix::io::fcntl_getfd(&fd).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        let mapped = ShmRegion::open(fd).unwrap();
        mapped.set_latency(321);
        assert_eq!(region.latency(), 321);
    }

    #[test]
    #[cfg(unix)]
    fn fds_stay_with_their_frames_between_messages() {
        let (app, bridge) = pair();
        let a = ShmRegion::create().unwrap();
        let b = ShmRegion::create().unwrap();
        a.set_latency(1);
        b.set_latency(2);
        let big = ControlMsg::SetState {
            request: 9,
            state: (0..3 * MAX_FRAME).map(|i| i as u8).collect(),
        };
        // Queued back to back before anything is read: the stream holds them in one piece.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                app.send(&ControlMsg::ListParams).unwrap();
                app.send_fd(a.fd()).unwrap();
                app.send(&big).unwrap();
                app.send_fd(b.fd()).unwrap();
                app.send(&ControlMsg::Deactivate).unwrap();
            });
            assert_eq!(bridge.recv().unwrap(), ControlMsg::ListParams);
            assert_eq!(
                ShmRegion::open(bridge.recv_fd().unwrap())
                    .unwrap()
                    .latency(),
                1
            );
            // Read piecewise while the sender is still writing it (#216).
            assert_eq!(poll_until_message(&bridge), big);
            assert_eq!(
                ShmRegion::open(bridge.recv_fd().unwrap())
                    .unwrap()
                    .latency(),
                2
            );
            assert_eq!(bridge.recv().unwrap(), ControlMsg::Deactivate);
        });
    }

    #[test]
    #[cfg(unix)]
    fn sockets_are_close_on_exec() {
        let (app, bridge) = socket_pair().unwrap();
        for fd in [app.as_fd(), bridge.as_fd()] {
            let flags = rustix::io::fcntl_getfd(fd).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        }
    }

    #[test]
    #[cfg(windows)]
    fn pipe_ends_are_not_inheritable() {
        let (app, bridge) = socket_pair().unwrap();
        assert!(!inheritable(app.as_handle()));
        assert!(!inheritable(bridge.as_handle()));
    }

    #[test]
    fn closed_peer_is_reported() {
        let (app, bridge) = pair();
        drop(bridge);
        assert_eq!(app.recv().unwrap_err(), ChannelError::Closed);
        assert_eq!(app.try_recv().unwrap_err(), ChannelError::Closed);
        assert_eq!(
            app.send(&ControlMsg::Shutdown).unwrap_err(),
            ChannelError::Closed
        );
        let (app, bridge) = pair();
        drop(app);
        #[cfg(unix)]
        assert_eq!(bridge.recv_fd().unwrap_err(), ChannelError::Closed);
        #[cfg(windows)]
        {
            // SAFETY: nothing arrives (the peer is gone).
            let received = unsafe { bridge.recv_handle() };
            assert_eq!(received.unwrap_err(), ChannelError::Closed);
        }
    }

    #[test]
    fn malformed_frames_are_codec_errors() {
        let (app, bridge) = pair();
        let is_codec =
            |r: Result<ControlMsg, ChannelError>| matches!(r, Err(ChannelError::Codec(_)));

        // Garbage payload, trailing bytes, wrong length, unknown tag, short frame.
        raw_send(&app, &frame(TAG_LAST, 3, &[0xff, 0xff, 0xff]));
        assert!(is_codec(bridge.recv()));
        let mut ok = postcard::to_stdvec(&ControlMsg::ListParams).unwrap();
        ok.push(0);
        raw_send(&app, &frame(TAG_LAST, ok.len() as u32, &ok));
        assert!(is_codec(bridge.recv()));
        raw_send(&app, &frame(TAG_LAST, 99, &ok[..1]));
        assert!(is_codec(bridge.recv()));
        raw_send(&app, &frame(9, 0, &[]));
        assert!(is_codec(bridge.recv()));
        // A frame beyond MAX_FRAME is read past and refused.
        raw_send(&app, &frame(TAG_LAST, 0, &vec![0; MAX_FRAME]));
        assert!(is_codec(bridge.recv()));
        let lie = frame_announcing(TAG_LAST, MAX_PAYLOAD as u32 + 1, 3, &[0; MAX_PAYLOAD + 1]);
        raw_send(&app, &lie);
        assert!(is_codec(bridge.recv()));
        // Fragments whose lengths do not add up, and a fragment announcing too much.
        raw_send(&app, &frame(TAG_MORE, 10, &[0; 4]));
        raw_send(&app, &frame(TAG_LAST, 11, &[0; 6]));
        assert!(is_codec(bridge.recv()));
        raw_send(&app, &frame(TAG_MORE, u32::MAX, &[0; 4]));
        assert!(is_codec(bridge.recv()));
        // An fd where a message is expected, and a message where an fd is expected.
        #[cfg(unix)]
        {
            app.send_fd(app.as_fd()).unwrap();
            assert!(is_codec(bridge.recv()));
            app.send(&ControlMsg::Shutdown).unwrap();
            assert!(matches!(bridge.recv_fd(), Err(ChannelError::Codec(_))));
        }
        #[cfg(windows)]
        {
            // The duplicate stays open in this process: nobody takes it from a wrong frame.
            app.send_handle(app.as_handle(), this_process()).unwrap();
            assert!(is_codec(bridge.recv()));
            app.send(&ControlMsg::Shutdown).unwrap();
            // SAFETY: a message arrives, no handle is taken.
            let wrong = unsafe { bridge.recv_handle() };
            assert!(matches!(wrong, Err(ChannelError::Codec(_))));
            // A handle frame with the wrong payload length.
            raw_send(&app, &frame(TAG_FD, 4, &[1, 0, 0, 0]));
            // SAFETY: as above.
            let short = unsafe { bridge.recv_handle() };
            assert!(matches!(short, Err(ChannelError::Codec(_))));
        }
        // The channel is still in sync afterwards.
        app.send(&ControlMsg::Deactivate).unwrap();
        assert_eq!(bridge.recv().unwrap(), ControlMsg::Deactivate);
    }

    /// Calls `try_recv` until a message is in.
    fn poll_until_message(bridge: &BridgeChannel) -> ControlMsg {
        loop {
            if let Some(msg) = bridge.try_recv().unwrap() {
                return msg;
            }
            std::thread::yield_now();
        }
    }

    /// How long a test peer stalls in the middle of a message before it writes the rest.
    const STALL: Duration = Duration::from_millis(500);

    /// The frames `Channel::send` writes for `msg`.
    fn frames_of(msg: &ControlMsg) -> Vec<u8> {
        let bytes = postcard::to_stdvec(msg).unwrap();
        let chunks: Vec<&[u8]> = bytes.chunks(MAX_PAYLOAD).collect();
        let mut wire = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let tag = if i + 1 == chunks.len() {
                TAG_LAST
            } else {
                TAG_MORE
            };
            wire.extend(frame(tag, bytes.len() as u32, chunk));
        }
        wire
    }

    /// The peer has written part of a message and stalls: `try_recv` returns `None` at once
    /// (twice, the second time with the partial data already taken), and `rest` arrives only
    /// after [`STALL`]. A `try_recv` that waited for the rest would fail the time check.
    fn stall_then_send(app: &AppChannel, bridge: &BridgeChannel, rest: &[u8]) {
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                std::thread::sleep(STALL);
                raw_send(app, rest);
            });
            let t0 = Instant::now();
            assert_eq!(bridge.try_recv().unwrap(), None);
            assert_eq!(bridge.try_recv().unwrap(), None);
            let waited = t0.elapsed();
            assert!(
                waited < STALL / 2,
                "try_recv waited {waited:?} for a stalled peer"
            );
            writer.join().unwrap();
        });
    }

    /// Runs `write` on a second thread while `receive` reads on this one, and hands `bridge`
    /// back. `bridge` is moved in, so a failing assertion in `receive` drops it while unwinding,
    /// before the scope joins the writer: a writer blocked on the full socket then fails instead
    /// of waiting for ever, and the test fails instead of hanging.
    fn receive_while<'a>(
        bridge: BridgeChannel,
        write: impl FnOnce() + Send + 'a,
        receive: impl FnOnce(&BridgeChannel),
    ) -> BridgeChannel {
        std::thread::scope(move |scope| {
            scope.spawn(write);
            receive(&bridge);
            bridge
        })
    }

    /// #216: a peer that stops in the middle of a frame holds up no `try_recv`; the bytes so far
    /// are kept and the message arrives once the rest is written.
    #[test]
    fn try_recv_never_waits_for_the_rest_of_a_frame() {
        let (app, bridge) = pair();
        let msg = ControlMsg::SetParam { id: 9, value: 0.25 };
        let whole = frames_of(&msg);
        // Cut inside the header, after it, inside the payload and before the last byte.
        for cut in [1, 3, FRAME_HEADER, FRAME_HEADER + 1, whole.len() - 1] {
            raw_send(&app, &whole[..cut]);
            stall_then_send(&app, &bridge, &whole[cut..]);
            assert_eq!(
                bridge.try_recv().unwrap(),
                Some(msg.clone()),
                "cut at {cut}"
            );
            assert_eq!(bridge.try_recv().unwrap(), None);
        }
        // One byte at a time: complete only with the last one.
        for (i, byte) in whole.iter().enumerate() {
            raw_send(&app, &[*byte]);
            let got = bridge.try_recv().unwrap();
            assert_eq!(got.is_some(), i + 1 == whole.len(), "after byte {i}");
        }
    }

    /// #216: the fragments of a large message are collected across many `try_recv` calls, also
    /// when the peer stalls right after a whole fragment (where the old code blocked).
    #[test]
    fn a_fragmented_message_reassembles_across_try_recv_calls() {
        let (app, bridge) = pair();
        let big = ControlMsg::SetState {
            request: 7,
            state: (0..3 * MAX_FRAME + 123)
                .map(|i| (i * 7 + i / 251) as u8)
                .collect(),
        };
        let wire = frames_of(&big);
        assert!(wire.len() > 3 * MAX_FRAME);
        // The first fragment whole, then a stall; then part of the next header after a stall.
        raw_send(&app, &wire[..MAX_FRAME]);
        stall_then_send(&app, &bridge, &wire[MAX_FRAME..MAX_FRAME + 5]);
        // The rest in pieces that cut headers and payloads at odd places.
        let pieces: Vec<&[u8]> = wire[MAX_FRAME + 5..].chunks(9_999).collect();
        for (i, piece) in pieces.iter().enumerate() {
            raw_send(&app, piece);
            let got = bridge.try_recv().unwrap();
            if i + 1 == pieces.len() {
                assert_eq!(got, Some(big.clone()));
            } else {
                assert_eq!(got, None, "after piece {i}");
            }
        }
        assert_eq!(bridge.try_recv().unwrap(), None);
        // The blocking receive goes on from a fragment `try_recv` left half read.
        raw_send(&app, &wire[..MAX_FRAME + 3]);
        assert_eq!(bridge.try_recv().unwrap(), None);
        receive_while(
            bridge,
            || raw_send(&app, &wire[MAX_FRAME + 3..]),
            |bridge| assert_eq!(bridge.recv().unwrap(), big),
        );
    }

    /// `try_recv_until` reads a message that has begun to arrive at the pace of its sender, in
    /// one call; with nothing queued it returns at once; a stalled peer costs it its deadline.
    #[test]
    fn try_recv_until_reads_on_while_bytes_keep_coming() {
        let (app, bridge) = pair();
        let far = || Instant::now() + Duration::from_secs(30);
        let t0 = Instant::now();
        assert_eq!(bridge.try_recv_until(far()).unwrap(), None);
        assert!(t0.elapsed() < STALL / 2, "waited with nothing queued");
        // Far beyond the socket buffer: the sender has to wait for the reader again and again.
        let big = ControlMsg::SetState {
            request: 3,
            state: (0..4 * 1024 * 1024 + 5)
                .map(|i| (i * 3 + i / 509) as u8)
                .collect(),
        };
        let wire = frames_of(&big);
        raw_send(&app, &wire[..MAX_FRAME + 1]);
        let bridge = receive_while(
            bridge,
            || raw_send(&app, &wire[MAX_FRAME + 1..]),
            |bridge| assert_eq!(bridge.try_recv_until(far()).unwrap(), Some(big.clone())),
        );
        // A peer that stalls in the middle of a frame: `None` once the deadline has passed.
        raw_send(&app, &wire[..FRAME_HEADER + 2]);
        let budget = Duration::from_millis(30);
        let t0 = Instant::now();
        assert_eq!(bridge.try_recv_until(t0 + budget).unwrap(), None);
        let waited = t0.elapsed();
        assert!(waited >= budget && waited < STALL, "waited {waited:?}");
        // It goes on from there once the rest arrives.
        receive_while(
            bridge,
            || raw_send(&app, &wire[FRAME_HEADER + 2..]),
            |bridge| assert_eq!(bridge.try_recv_until(far()).unwrap(), Some(big)),
        );
    }

    /// `try_recv_until` starts no read after its deadline, also while more is queued and the
    /// peer keeps writing: what arrived stays in the channel, and later calls finish the message.
    #[test]
    fn try_recv_until_keeps_its_deadline_while_bytes_keep_coming() {
        let (app, bridge) = pair();
        let big = ControlMsg::SetState {
            request: 5,
            state: (0..8 * 1024 * 1024)
                .map(|i| (i * 5 + i / 307) as u8)
                .collect(),
        };
        let wire = frames_of(&big);
        // Begun: the first fragment is in.
        raw_send(&app, &wire[..MAX_FRAME]);
        assert_eq!(bridge.try_recv().unwrap(), None);
        let taken = || bridge.rx.lock().unwrap().message.len();
        assert_eq!(taken(), MAX_PAYLOAD);
        // The deadline has passed: the queued next fragment stays unread.
        raw_send(&app, &wire[MAX_FRAME..2 * MAX_FRAME]);
        assert_eq!(bridge.try_recv_until(Instant::now()).unwrap(), None);
        assert_eq!(taken(), MAX_PAYLOAD);
        // A sender that writes the rest as fast as it can: no call outlasts its budget by more
        // than one read (plus scheduling slack); together they complete the message.
        let budget = Duration::from_millis(5);
        receive_while(
            bridge,
            || raw_send(&app, &wire[2 * MAX_FRAME..]),
            |bridge| {
                let mut longest = Duration::ZERO;
                let msg = loop {
                    let t0 = Instant::now();
                    let got = bridge.try_recv_until(t0 + budget).unwrap();
                    if let Some(msg) = got {
                        // Decoding the whole message is not bounded by the deadline.
                        break msg;
                    }
                    longest = longest.max(t0.elapsed());
                };
                assert!(msg == big, "the message differs");
                assert!(
                    longest < budget + Duration::from_millis(100),
                    "a call took {longest:?} with a budget of {budget:?}"
                );
            },
        );
    }

    /// `last_received` moves with every read, also in the middle of a message, and
    /// `is_receiving` says whether a message waits for the rest.
    #[test]
    fn progress_is_reported_in_the_middle_of_a_message() {
        let (app, bridge) = pair();
        assert_eq!(bridge.last_received(), None);
        assert!(!bridge.is_receiving());
        let wire = frames_of(&ControlMsg::SetState {
            request: 1,
            state: vec![3; 2 * MAX_FRAME],
        });
        raw_send(&app, &wire[..MAX_FRAME + 4]);
        let t0 = Instant::now();
        assert_eq!(bridge.try_recv().unwrap(), None);
        let first = bridge.last_received().expect("bytes arrived");
        assert!(first >= t0);
        assert!(bridge.is_receiving());
        std::thread::sleep(Duration::from_millis(5));
        raw_send(&app, &wire[MAX_FRAME + 4..MAX_FRAME + 9]);
        assert_eq!(bridge.try_recv().unwrap(), None);
        assert!(bridge.last_received().unwrap() > first);
        assert!(bridge.is_receiving());
        raw_send(&app, &wire[MAX_FRAME + 9..]);
        assert!(bridge.try_recv().unwrap().is_some());
        assert!(!bridge.is_receiving());
    }

    /// A frame refused in the middle of a fragmented message ends that message, so the next
    /// message is not measured against it and arrives intact.
    #[test]
    fn a_refused_frame_ends_the_fragmented_message_it_cut() {
        let (app, bridge) = pair();
        raw_send(&app, &frame(TAG_MORE, 10, &[0; 4]));
        assert_eq!(bridge.try_recv().unwrap(), None);
        let lie = frame_announcing(TAG_LAST, MAX_PAYLOAD as u32 + 1, 3, &[0; MAX_PAYLOAD + 1]);
        raw_send(&app, &lie);
        assert!(matches!(bridge.try_recv(), Err(ChannelError::Codec(_))));
        app.send(&ControlMsg::Ping { seq: 4 }).unwrap();
        assert_eq!(
            bridge.try_recv().unwrap(),
            Some(ControlMsg::Ping { seq: 4 })
        );
        // The same with a blocking receive.
        raw_send(&app, &frame(TAG_MORE, 10, &[0; 4]));
        raw_send(&app, &lie);
        assert!(matches!(bridge.recv(), Err(ChannelError::Codec(_))));
        app.send(&ControlMsg::Ping { seq: 5 }).unwrap();
        assert_eq!(bridge.recv().unwrap(), ControlMsg::Ping { seq: 5 });
    }

    /// An oversized frame is refused as soon as its header is in, and its payload is read past
    /// piecewise; the next message arrives intact.
    #[test]
    fn an_oversized_frame_is_read_past_without_blocking() {
        let (app, bridge) = pair();
        let lie = frame_announcing(TAG_LAST, MAX_PAYLOAD as u32 + 1, 3, &[0; MAX_PAYLOAD + 1]);
        raw_send(&app, &lie[..FRAME_HEADER + 10]);
        assert!(matches!(bridge.try_recv(), Err(ChannelError::Codec(_))));
        stall_then_send(&app, &bridge, &lie[FRAME_HEADER + 10..]);
        app.send(&ControlMsg::Deactivate).unwrap();
        assert_eq!(bridge.try_recv().unwrap(), Some(ControlMsg::Deactivate));
    }

    /// `try_recv` does not queue behind a thread blocked in `recv` on the same end.
    #[test]
    fn try_recv_does_not_wait_for_a_blocked_recv() {
        let (app, bridge) = pair();
        std::thread::scope(|scope| {
            let receiver = scope.spawn(|| bridge.recv().unwrap());
            std::thread::sleep(Duration::from_millis(20));
            assert_eq!(bridge.try_recv().unwrap(), None);
            app.send(&ControlMsg::ListParams).unwrap();
            assert_eq!(receiver.join().unwrap(), ControlMsg::ListParams);
        });
    }

    /// An fd still arrives with its own frame when the message after it is read piecewise.
    #[test]
    #[cfg(unix)]
    fn an_fd_arrives_with_its_frame_before_a_partial_message() {
        let (app, bridge) = pair();
        let region = ShmRegion::create().unwrap();
        region.set_latency(5);
        let whole = frames_of(&ControlMsg::ListParams);
        app.send_fd(region.fd()).unwrap();
        raw_send(&app, &whole[..3]);
        let fd = bridge.recv_fd().unwrap();
        assert_eq!(ShmRegion::open(fd).unwrap().latency(), 5);
        stall_then_send(&app, &bridge, &whole[3..]);
        assert_eq!(bridge.try_recv().unwrap(), Some(ControlMsg::ListParams));
    }
}
