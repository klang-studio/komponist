//! The control protocol between the app and a bridge (ARCHITECTURE §8.1), plus the plugin
//! vocabulary both sides share (descriptors, parameters, note names).
//!
//! Transport: a Unix `SOCK_STREAM` socket pair, one per bridge, messages encoded with postcard.
//! The app keeps one end and passes the other to the bridge as an inherited fd (`--instance
//! --control-fd N`). Messages larger than [`MAX_FRAME`] (big plugin states) are split into frames
//! and reassembled by [`Channel`]; callers always see whole messages. The shared-memory fd travels
//! over the same socket as SCM_RIGHTS ancillary data ([`Channel::send_fd`]).
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
//! - `komponist-bridge --scan <bundle>`: prints a JSON array of [`PluginDescriptor`] (serde_json,
//!   same field names) and a newline to stdout; plugin output during the scan goes to stderr.
//!   Exit codes: 0 success (the array may be empty), 1 the bundle could not be scanned (reason on
//!   stderr), 2 bad command line, 3 the format has no hosting (unused since CLAP and VST3 are both hosted). A crashing
//!   bundle kills the scan process with its signal.

use std::io::{IoSlice, IoSliceMut};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
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
/// frame's last rect, `RaiseFrame` and `FrameWindow`).
pub const PROTOCOL_VERSION: u32 = 4;

/// Largest frame written to the socket in one `send`; larger messages are fragmented.
pub const MAX_FRAME: usize = 64 * 1024;

// Frame format: an 8-byte header — tag (1 byte), the payload length of this frame (u24,
// little-endian), the length of the whole encoded message (u32, little-endian, the same in every
// fragment) — then up to `MAX_FRAME - FRAME_HEADER` payload bytes. A message is the payloads of
// its `TAG_MORE` frames followed by one `TAG_LAST` frame; a `TAG_FD` frame carries one fd and no
// payload.

/// Bytes in front of every frame.
const FRAME_HEADER: usize = 8;
/// Payload bytes per frame.
const MAX_PAYLOAD: usize = MAX_FRAME - FRAME_HEADER;
/// The last (or only) frame of a message.
const TAG_LAST: u8 = 0;
/// A fragment; more follow.
const TAG_MORE: u8 = 1;
/// A file descriptor as SCM_RIGHTS, no payload.
const TAG_FD: u8 = 2;
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
    /// Restore a state blob (from `State` or the project file).
    SetState {
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
}

/// Bridge → app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BridgeMsg {
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
    },
    /// Answer to `ListParams`, or unsolicited after the plugin rescanned its parameters.
    Params(Vec<ParamInfo>),
    /// Parameter values with the plugin's text, coalesced to at most one message per 33 ms: values
    /// that changed on the bridge's main thread (GUI edits outside processing, all values after a
    /// state load or a parameter rescan) and the echo of every `SetParam` value (so the generic
    /// view gets the plugin's text, "−12.0 dB"). GUI edits during processing arrive as output
    /// events in the shared memory instead.
    ParamValues(Vec<ParamValue>),
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
    fd: OwnedFd,
    _msgs: PhantomData<fn(Tx) -> Rx>,
}

/// The app's end.
pub type AppChannel = Channel<ControlMsg, BridgeMsg>;

/// The bridge's end.
pub type BridgeChannel = Channel<BridgeMsg, ControlMsg>;

/// Creates a connected socket pair (`SOCK_STREAM`, close-on-exec): the app's end and the fd for
/// the bridge (to be inherited by the child; the spawner clears `CLOEXEC` on it).
pub fn socket_pair() -> Result<(AppChannel, OwnedFd), ChannelError> {
    let (app, bridge) = sys::stream_pair().map_err(io_error)?;
    Ok((Channel::from_fd(app), bridge))
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
        // No SOCK_CLOEXEC on macOS. The window until `fcntl` is short; bridges are spawned from
        // the thread that creates their socket pair.
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!(
    "komponist-ipc: no control-channel backend (control.rs) for this OS yet (Windows: W2, byte-stream pipe + DuplicateHandle)"
);

fn io_error(e: Errno) -> ChannelError {
    ChannelError::Io(e.to_string())
}

fn codec_error(message: impl Into<String>) -> ChannelError {
    ChannelError::Codec(message.into())
}

/// The fds that arrived with the bytes of one frame.
#[derive(Default)]
struct Incoming {
    /// The first fd (more are closed).
    fd: Option<OwnedFd>,
    /// The kernel dropped fds for lack of control space (more than one fd in a frame).
    truncated: bool,
}

/// One received frame (payload in the caller's buffer after the header).
struct Frame {
    tag: u8,
    /// Length of the whole message, from the header.
    total: usize,
    /// Payload bytes in this frame.
    len: usize,
    /// The first fd that came with the frame (more are closed).
    fd: Option<OwnedFd>,
}

/// Blocks until `fd` is ready for `flags` (used when someone made the socket non-blocking).
fn wait_ready(fd: BorrowedFd<'_>, flags: PollFlags) -> Result<(), ChannelError> {
    let mut fds = [PollFd::from_borrowed_fd(fd, flags)];
    match poll(&mut fds, None) {
        Ok(_) | Err(Errno::INTR) => Ok(()),
        Err(e) => Err(io_error(e)),
    }
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
    pub fn from_fd(fd: OwnedFd) -> Channel<Tx, Rx> {
        Channel {
            fd,
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
            self.send_frame(tag, total, &bytes[sent..sent + len], None)?;
            sent += len;
            if tag == TAG_LAST {
                return Ok(());
            }
        }
    }

    /// Receives one whole message, blocking.
    pub fn recv(&self) -> Result<Rx, ChannelError> {
        loop {
            if let Some(msg) = self.recv_message(false)? {
                return Ok(msg);
            }
        }
    }

    /// Receives one whole message if one is ready, without blocking (for poll loops). A partially
    /// received fragmented message is completed blockingly.
    pub fn try_recv(&self) -> Result<Option<Rx>, ChannelError> {
        self.recv_message(true)
    }

    /// Sends a file descriptor (SCM_RIGHTS) as its own frame.
    pub fn send_fd(&self, fd: BorrowedFd<'_>) -> Result<(), ChannelError> {
        self.send_frame(TAG_FD, 0, &[], Some(fd))
    }

    /// Receives a file descriptor sent with [`Channel::send_fd`], blocking.
    ///
    /// A message frame in its place is consumed and reported as `Codec`.
    pub fn recv_fd(&self) -> Result<OwnedFd, ChannelError> {
        let mut buf = vec![0u8; MAX_FRAME];
        let frame = loop {
            if let Some(frame) = self.recv_frame(&mut buf, false)? {
                break frame;
            }
        };
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

    /// Fills `buf` exactly from the stream, collecting the fds that come with the bytes.
    /// `Ok(false)` only with `dont_wait` when not a single byte is queued; once a byte has
    /// arrived, the rest is awaited blockingly. End of stream is `Closed`.
    fn recv_exact(
        &self,
        buf: &mut [u8],
        mut dont_wait: bool,
        incoming: &mut Incoming,
    ) -> Result<bool, ChannelError> {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut got = 0;
        while got < buf.len() {
            let flags = if dont_wait {
                sys::RECV_FLAGS | RecvFlags::DONTWAIT
            } else {
                sys::RECV_FLAGS
            };
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let mut iov = [IoSliceMut::new(&mut buf[got..])];
            let msg = match recvmsg(&self.fd, &mut iov, &mut control, flags) {
                Ok(msg) => msg,
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) if dont_wait => return Ok(false),
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
            got += msg.bytes;
            dont_wait = false;
        }
        Ok(true)
    }

    /// Reads one frame into `buf` (at least `MAX_FRAME` bytes; the payload lands after
    /// `FRAME_HEADER`). `None` only with `dont_wait` when nothing is queued.
    fn recv_frame(&self, buf: &mut [u8], dont_wait: bool) -> Result<Option<Frame>, ChannelError> {
        let mut header = [0u8; FRAME_HEADER];
        let mut incoming = Incoming::default();
        if !self.recv_exact(&mut header, dont_wait, &mut incoming)? {
            return Ok(None);
        }
        let len = u32::from_le_bytes([header[1], header[2], header[3], 0]) as usize;
        let total = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        if len > MAX_PAYLOAD {
            // Read past the oversized payload, so the stream stays in step, then refuse it.
            let mut left = len;
            while left > 0 {
                let n = left.min(MAX_PAYLOAD);
                self.recv_exact(
                    &mut buf[FRAME_HEADER..FRAME_HEADER + n],
                    false,
                    &mut incoming,
                )?;
                left -= n;
            }
            return Err(codec_error(format!("frame larger than {MAX_FRAME} bytes")));
        }
        self.recv_exact(
            &mut buf[FRAME_HEADER..FRAME_HEADER + len],
            false,
            &mut incoming,
        )?;
        if incoming.truncated {
            return Err(codec_error("too many file descriptors in one frame"));
        }
        Ok(Some(Frame {
            tag: header[0],
            total,
            len,
            fd: incoming.fd,
        }))
    }

    /// Receives one message; the first frame without blocking if `dont_wait`.
    fn recv_message(&self, dont_wait: bool) -> Result<Option<Rx>, ChannelError> {
        let mut buf = vec![0u8; MAX_FRAME];
        let Some(first) = self.recv_frame(&mut buf, dont_wait)? else {
            return Ok(None);
        };
        let total = first.total;
        match first.tag {
            TAG_LAST if first.len == total => {
                decode(&buf[FRAME_HEADER..FRAME_HEADER + first.len]).map(Some)
            }
            TAG_LAST => Err(codec_error(format!(
                "frame of {} bytes announces a message of {total}",
                first.len
            ))),
            TAG_MORE if total > MAX_MESSAGE => Err(codec_error(format!(
                "announced message of {total} bytes exceeds {MAX_MESSAGE}"
            ))),
            TAG_MORE if first.len >= total => {
                Err(codec_error("fragment as long as the whole message"))
            }
            TAG_MORE => {
                // Grow with the data rather than trusting the announced length up front.
                let mut message = Vec::with_capacity(total.min(16 * MAX_FRAME));
                message.extend_from_slice(&buf[FRAME_HEADER..FRAME_HEADER + first.len]);
                loop {
                    let Some(frame) = self.recv_frame(&mut buf, false)? else {
                        continue;
                    };
                    if frame.tag != TAG_MORE && frame.tag != TAG_LAST {
                        return Err(codec_error("unexpected frame inside a fragmented message"));
                    }
                    if frame.total != total || message.len() + frame.len > total {
                        return Err(codec_error("fragment lengths do not add up"));
                    }
                    message.extend_from_slice(&buf[FRAME_HEADER..FRAME_HEADER + frame.len]);
                    if frame.tag == TAG_LAST {
                        if message.len() != total {
                            return Err(codec_error("fragmented message ends early"));
                        }
                        return decode(&message).map(Some);
                    }
                }
            }
            TAG_FD => Err(codec_error(
                "expected a message, got a file descriptor frame",
            )),
            tag => Err(codec_error(format!("unknown frame tag {tag}"))),
        }
    }
}

impl<Tx, Rx> AsFd for Channel<Tx, Rx> {
    /// The socket, for `poll()`.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{KeyMods, Mark, PluginView};
    use crate::shm::ShmRegion;

    fn pair() -> (AppChannel, BridgeChannel) {
        let (app, fd) = socket_pair().unwrap();
        (app, BridgeChannel::from_fd(fd))
    }

    /// Writes raw bytes (bypassing the encoder), all of them.
    fn raw_send(fd: BorrowedFd<'_>, bytes: &[u8]) {
        let mut sent = 0;
        while sent < bytes.len() {
            sent += rustix::net::send(fd, &bytes[sent..], sys::SEND_FLAGS).unwrap();
        }
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
        }
    }

    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            format: PluginFormat::Clap,
            id: "org.example.synth".into(),
            name: "Synth".into(),
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
            ControlMsg::SetState {
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
            })),
            BridgeMsg::Activated { latency: 64 },
            BridgeMsg::State {
                request: 7,
                state: vec![9; 100],
            },
            BridgeMsg::Params(vec![param]),
            BridgeMsg::ParamValues(vec![value]),
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
        for msg in &messages {
            bridge.send(msg).unwrap();
            assert_eq!(&app.recv().unwrap(), msg);
        }
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
                    };
                    bridge.send(&msg).unwrap();
                }
            });
            for state in &states {
                match app.recv().unwrap() {
                    BridgeMsg::State {
                        request,
                        state: got,
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
        // A fragmented message whose first frame is queued is completed.
        let big = ControlMsg::SetState {
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

    #[test]
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
    fn fds_stay_with_their_frames_between_messages() {
        let (app, bridge) = pair();
        let a = ShmRegion::create().unwrap();
        let b = ShmRegion::create().unwrap();
        a.set_latency(1);
        b.set_latency(2);
        let big = ControlMsg::SetState {
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
            assert_eq!(bridge.recv().unwrap(), big);
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
    fn sockets_are_close_on_exec() {
        let (app, bridge) = socket_pair().unwrap();
        for fd in [app.as_fd(), bridge.as_fd()] {
            let flags = rustix::io::fcntl_getfd(fd).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        }
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
        assert_eq!(bridge.recv_fd().unwrap_err(), ChannelError::Closed);
    }

    #[test]
    fn malformed_frames_are_codec_errors() {
        let (app, bridge) = pair();
        let fd = app.as_fd();
        let is_codec =
            |r: Result<ControlMsg, ChannelError>| matches!(r, Err(ChannelError::Codec(_)));

        // Garbage payload, trailing bytes, wrong length, unknown tag, short frame.
        raw_send(fd, &frame(TAG_LAST, 3, &[0xff, 0xff, 0xff]));
        assert!(is_codec(bridge.recv()));
        let mut ok = postcard::to_stdvec(&ControlMsg::ListParams).unwrap();
        ok.push(0);
        raw_send(fd, &frame(TAG_LAST, ok.len() as u32, &ok));
        assert!(is_codec(bridge.recv()));
        raw_send(fd, &frame(TAG_LAST, 99, &ok[..1]));
        assert!(is_codec(bridge.recv()));
        raw_send(fd, &frame(9, 0, &[]));
        assert!(is_codec(bridge.recv()));
        // A frame beyond MAX_FRAME is read past and refused.
        raw_send(fd, &frame(TAG_LAST, 0, &vec![0; MAX_FRAME]));
        assert!(is_codec(bridge.recv()));
        let lie = frame_announcing(TAG_LAST, MAX_PAYLOAD as u32 + 1, 3, &[0; MAX_PAYLOAD + 1]);
        raw_send(fd, &lie);
        assert!(is_codec(bridge.recv()));
        // Fragments whose lengths do not add up, and a fragment announcing too much.
        raw_send(fd, &frame(TAG_MORE, 10, &[0; 4]));
        raw_send(fd, &frame(TAG_LAST, 11, &[0; 6]));
        assert!(is_codec(bridge.recv()));
        raw_send(fd, &frame(TAG_MORE, u32::MAX, &[0; 4]));
        assert!(is_codec(bridge.recv()));
        // An fd where a message is expected, and a message where an fd is expected.
        app.send_fd(fd).unwrap();
        assert!(is_codec(bridge.recv()));
        app.send(&ControlMsg::Shutdown).unwrap();
        assert!(matches!(bridge.recv_fd(), Err(ChannelError::Codec(_))));
        // The channel is still in sync afterwards.
        app.send(&ControlMsg::Deactivate).unwrap();
        assert_eq!(bridge.recv().unwrap(), ControlMsg::Deactivate);
    }

    #[test]
    fn a_frame_split_across_writes_is_reassembled() {
        let (app, bridge) = pair();
        let bytes = postcard::to_stdvec(&ControlMsg::SetParam { id: 9, value: 0.25 }).unwrap();
        let whole = frame(TAG_LAST, bytes.len() as u32, &bytes);
        // Cut inside the header, then inside the payload; the rest arrives a little later.
        for cut in [3, FRAME_HEADER + 1] {
            assert_eq!(bridge.try_recv().unwrap(), None);
            raw_send(app.as_fd(), &whole[..cut]);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    raw_send(app.as_fd(), &whole[cut..]);
                });
                // A started frame is completed blockingly, also by `try_recv`.
                assert_eq!(
                    bridge.try_recv().unwrap(),
                    Some(ControlMsg::SetParam { id: 9, value: 0.25 })
                );
            });
        }
    }
}
