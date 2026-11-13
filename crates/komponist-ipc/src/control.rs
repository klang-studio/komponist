//! The control protocol between the app and a bridge (ARCHITECTURE §8.1), plus the plugin
//! vocabulary both sides share (descriptors, parameters, note names).
//!
//! Transport: a Unix `SOCK_SEQPACKET` socket pair, one per bridge, messages encoded with postcard.
//! The app keeps one end and passes the other to the bridge as an inherited fd (`--instance
//! --control-fd N`). Messages larger than [`MAX_FRAME`] (big plugin states) are split into frames
//! and reassembled by [`Channel`]; callers always see whole messages. The shared-memory fd travels
//! over the same socket as SCM_RIGHTS ancillary data ([`Channel::send_fd`]).
//!
//! Order on a new bridge: bridge sends `Hello`; app sends the memfd, then `Load`, then `Activate`,
//! then `SetState` if it has a state; the bridge answers `Loaded`, `Activated`. Control messages are
//! handled by the bridge's main thread; RT data never goes through the socket.
//!
//! The scan mode (`komponist-bridge --scan <bundle>`) prints a JSON array of [`PluginDescriptor`]
//! to stdout (serde_json, same field names).

use std::marker::PhantomData;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Protocol version; `Hello` carries it and the app refuses other versions.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest frame written to the socket in one `send`; larger messages are fragmented.
pub const MAX_FRAME: usize = 64 * 1024;

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
    /// Lowest plain value.
    pub min: f64,
    /// Highest plain value.
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

/// Size of a plugin GUI in physical pixels (X11 is not DPI-aware; scale is passed separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct GuiSize {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
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
    /// audio side).
    SetParam {
        /// Parameter id.
        id: u32,
        /// Plain value.
        value: f64,
    },
    /// Open the plugin GUI as a child of the X11 window `parent_xid`, at `scale` (egui
    /// `pixels_per_point`). Answer: `GuiOpened` or `Error`.
    OpenGui {
        /// X11 window id of the frame's child window.
        parent_xid: u64,
        /// UI scale factor.
        scale: f64,
    },
    /// The frame window resized the GUI area (answer to `GuiSizeRequest`, or user resize).
    SetGuiSize(GuiSize),
    /// Close the plugin GUI.
    CloseGui,
    /// Switch realtime / offline (export); the bridge deactivates and re-activates if needed.
    SetRenderMode(RenderMode),
    /// Deactivate, unload and exit.
    Shutdown,
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
    /// Parameter values that changed on the bridge's main thread (GUI edits outside processing,
    /// after a state load), coalesced at about 30 Hz.
    ParamValues(Vec<ParamValue>),
    /// Answer to `OpenGui`.
    GuiOpened {
        /// Initial size.
        size: GuiSize,
        /// Whether the user may resize it.
        resizable: bool,
    },
    /// The plugin wants a different GUI size; the app resizes the frame and answers `SetGuiSize`.
    GuiSizeRequest(GuiSize),
    /// The GUI was closed (by the plugin, or as the answer to `CloseGui`).
    GuiClosed,
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
    /// Whether the plugin has its own GUI for X11.
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

/// Creates a connected socket pair (`SOCK_SEQPACKET | SOCK_CLOEXEC`): the app's end and the fd for
/// the bridge (to be inherited by the child; the spawner clears `CLOEXEC` on it).
pub fn socket_pair() -> Result<(AppChannel, OwnedFd), ChannelError> {
    todo!("WP-16")
}

impl<Tx: Serialize, Rx: DeserializeOwned> Channel<Tx, Rx> {
    /// Wraps a connected seqpacket socket.
    pub fn from_fd(fd: OwnedFd) -> Channel<Tx, Rx> {
        Channel {
            fd,
            _msgs: PhantomData,
        }
    }

    /// Sends one message (blocking until the socket accepts all its frames).
    pub fn send(&self, msg: &Tx) -> Result<(), ChannelError> {
        todo!("WP-16")
    }

    /// Receives one whole message, blocking.
    pub fn recv(&self) -> Result<Rx, ChannelError> {
        todo!("WP-16")
    }

    /// Receives one whole message if one is ready, without blocking (for poll loops). A partially
    /// received fragmented message is completed blockingly.
    pub fn try_recv(&self) -> Result<Option<Rx>, ChannelError> {
        todo!("WP-16")
    }

    /// Sends a file descriptor (SCM_RIGHTS) as its own frame.
    pub fn send_fd(&self, fd: BorrowedFd<'_>) -> Result<(), ChannelError> {
        todo!("WP-16")
    }

    /// Receives a file descriptor sent with [`Channel::send_fd`], blocking.
    pub fn recv_fd(&self) -> Result<OwnedFd, ChannelError> {
        todo!("WP-16")
    }
}

impl<Tx, Rx> AsFd for Channel<Tx, Rx> {
    /// The socket, for `poll()`.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
