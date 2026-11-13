//! Shared memory between the app and one bridge process (ARCHITECTURE §8.1).
//!
//! One memfd per plugin instance, fixed size [`SHM_SIZE`], mapped `MAP_SHARED` by both processes.
//! The app creates it ([`ShmRegion::create`]) and passes the fd to the bridge over the control
//! socket (SCM_RIGHTS); the bridge maps it with [`ShmRegion::open`].
//!
//! Layout ([`ShmLayout`], `#[repr(C)]`, all offsets fixed): header with the futex words, the
//! per-block request and result, an input event ring (app → bridge), an output event ring
//! (bridge → app) and stereo audio in/out of [`MAX_BLOCK`] frames.
//!
//! RT handshake per block (app audio thread ↔ bridge RT thread):
//! 1. App ([`BridgeClient::begin_block`]): only if the bridge is idle (`done_seq == request_seq`),
//!    write input audio, push the block's events, write the [`BlockRequest`], then
//!    `request_seq += 1` (release) and futex-wake it.
//! 2. Bridge ([`BridgeServer::wait_request`]): futex-wait on `request_seq`, pop the events, run the
//!    plugin on [`BridgeServer::audio`], push plugin output events, then [`BridgeServer::finish`]:
//!    write the [`BlockResult`], `done_seq = seq` (release), futex-wake.
//! 3. App ([`BridgeClient::wait_done`]): futex-wait on `done_seq` with a deadline (about 60 % of the
//!    block period minus elapsed time), then read the output. On timeout the block is `Late`: the
//!    app uses passthrough (effect) or silence (instrument), and while the bridge is still busy the
//!    next `begin_block` returns `Busy` instead of writing; the stale result is recognised by its
//!    sequence number and ignored.
//!
//! Events of a block the app could not deliver (bridge busy) stay with the `ExternalProcessor`,
//! which delivers pending note-offs with the next block it can send.

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::Instant;

use crate::ring::ShmRing;

/// `b"KPSM"` as little-endian `u32`, the first header field.
pub const SHM_MAGIC: u32 = u32::from_le_bytes(*b"KPSM");

/// Layout version; both sides must agree.
pub const SHM_VERSION: u32 = 1;

/// Frames of audio per block the mapping holds. Must equal `komponist_engine::MAX_BLOCK`
/// (`komponist-plugins` asserts it in a test).
pub const MAX_BLOCK: usize = 2048;

/// Slots of the input event ring (app → bridge). A block carries at most this many events.
pub const INPUT_EVENT_CAPACITY: usize = 1024;

/// Slots of the output event ring (bridge → app). Events beyond it are dropped and counted in
/// [`BlockResult::dropped_events`].
pub const OUTPUT_EVENT_CAPACITY: usize = 1024;

/// Size of the mapping in bytes: the layout rounded up to whole 4 KiB pages.
pub const SHM_SIZE: usize = core::mem::size_of::<ShmLayout>().next_multiple_of(4096);

/// [`BlockRequest::flags`]: reset the plugin (stop, locate, loop jump) before processing.
pub const BLOCK_FLAG_RESET: u32 = 1;
/// [`BlockRequest::flags`]: the block is part of an offline render.
pub const BLOCK_FLAG_OFFLINE: u32 = 2;

/// [`WireTransport::flags`]: transport running.
pub const TRANSPORT_PLAYING: u32 = 1;
/// [`WireTransport::flags`]: recording.
pub const TRANSPORT_RECORDING: u32 = 2;
/// [`WireTransport::flags`]: loop on.
pub const TRANSPORT_LOOPING: u32 = 4;

/// The whole mapping.
#[repr(C)]
pub struct ShmLayout {
    /// Handshake and per-block parameters.
    pub header: ShmHeader,
    /// Events for the next block; producer: app audio thread, consumer: bridge RT thread.
    pub input_events: ShmRing<WireEvent, INPUT_EVENT_CAPACITY>,
    /// Events the plugin emitted (parameter changes and gestures from its GUI); producer: bridge RT
    /// thread, consumer: app UI thread (`BridgeMonitor`, polled by the plugin manager).
    pub output_events: ShmRing<WireEvent, OUTPUT_EVENT_CAPACITY>,
    /// Input audio, `[channel][frame]`; written by the app before a request.
    pub audio_in: UnsafeCell<[[f32; MAX_BLOCK]; 2]>,
    /// Output audio, `[channel][frame]`; written by the bridge before `done_seq`.
    pub audio_out: UnsafeCell<[[f32; MAX_BLOCK]; 2]>,
}

/// Header at offset 0.
#[repr(C)]
pub struct ShmHeader {
    /// [`SHM_MAGIC`].
    pub magic: u32,
    /// [`SHM_VERSION`].
    pub version: u32,
    /// [`SHM_SIZE`] of the creating side.
    pub size: u32,
    /// [`MAX_BLOCK`] of the creating side.
    pub max_block: u32,
    /// Futex word: sequence number of the latest request; written by the app (release).
    pub request_seq: AtomicU32,
    /// Futex word: sequence number of the latest finished request; written by the bridge
    /// (release).
    pub done_seq: AtomicU32,
    /// [`BridgeState`] as `u32`; written by the bridge.
    pub bridge_state: AtomicU32,
    /// Plugin latency in frames; written by the bridge (main thread), read by the app.
    pub latency: AtomicU32,
    /// Parameters of the latest request; written by the app before bumping `request_seq`.
    pub request: UnsafeCell<BlockRequest>,
    /// Result of the latest finished request; written by the bridge before bumping `done_seq`.
    pub result: UnsafeCell<BlockResult>,
}

/// What the app asks for one block.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BlockRequest {
    /// Equals the `request_seq` value this request was published with.
    pub seq: u32,
    /// Frames to process, 1 ..= [`MAX_BLOCK`].
    pub frames: u32,
    /// `BLOCK_FLAG_*` bits.
    pub flags: u32,
    /// Events pushed into the input ring for this block.
    pub event_count: u32,
    /// Running sample counter of the engine (CLAP `steady_time`), reset only by the engine.
    pub steady_time: u64,
    /// Transport at frame 0.
    pub transport: WireTransport,
}

/// What the bridge reports for one block.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockResult {
    /// The `seq` of the request this answers.
    pub seq: u32,
    /// [`BlockStatus`] as `u32`.
    pub status: u32,
    /// Plugin output events dropped because the output ring was full.
    pub dropped_events: u32,
    /// Reserved, 0.
    pub reserved: u32,
}

/// Outcome of one block in the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum BlockStatus {
    /// Output written.
    Ok = 0,
    /// The plugin reported an error; the output holds the fallback.
    PluginError = 1,
    /// The plugin is not active (loading, restarting); the output holds the fallback.
    Inactive = 2,
}

/// Lifecycle state of the bridge, published in [`ShmHeader::bridge_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum BridgeState {
    /// Process started, plugin not loaded yet (zeroed memory reads as this).
    Starting = 0,
    /// Plugin loaded, not activated.
    Loaded = 1,
    /// Activated and processing: requests are served.
    Active = 2,
    /// Fatal error; the bridge is about to exit.
    Failed = 3,
}

impl BridgeState {
    /// Decodes the header value; unknown values read as `Failed`.
    pub fn from_u32(value: u32) -> BridgeState {
        match value {
            0 => BridgeState::Starting,
            1 => BridgeState::Loaded,
            2 => BridgeState::Active,
            _ => BridgeState::Failed,
        }
    }
}

/// Transport at frame 0 of a block, in the units CLAP and VST3 use (quarter-note beats).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WireTransport {
    /// `TRANSPORT_*` bits.
    pub flags: u32,
    /// Meter numerator.
    pub meter_num: u16,
    /// Meter denominator.
    pub meter_den: u16,
    /// Tempo in quarter notes per minute.
    pub tempo_bpm: f64,
    /// Song position in quarter notes (negative during the count-in).
    pub position_beats: f64,
    /// Start of the current bar in quarter notes.
    pub bar_start_beats: f64,
    /// Index of the current bar, 0-based (bar 1 = 0).
    pub bar_number: i32,
    /// Reserved, 0.
    pub reserved: u32,
    /// Loop start in quarter notes.
    pub loop_start_beats: f64,
    /// Loop end in quarter notes.
    pub loop_end_beats: f64,
}

/// One event in a ring: a flat 32-byte POD so it can live in shared memory.
///
/// | `kind` | meaning | fields used |
/// |---|---|---|
/// | 0 | invalid (zeroed memory) | – |
/// | 1 | note on | `data = [channel, key, velocity, 0]`, `note_id` |
/// | 2 | note off | `data = [channel, key, 0, 0]`, `note_id` |
/// | 3 | MIDI channel message | `data = [status, data1, data2, 0]` |
/// | 4 | parameter value | `param_id`, `value` (plain) |
/// | 5 | parameter gesture | `param_id`, `data[0]` = 1 begin / 0 end |
/// | 6 | all notes off | – |
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WireEvent {
    /// Frame offset within the block.
    pub offset: u32,
    /// Event kind, see the table.
    pub kind: u32,
    /// Note id (`u32::MAX` = none).
    pub note_id: u32,
    /// Parameter id.
    pub param_id: u32,
    /// Parameter value, plain range.
    pub value: f64,
    /// Small payload, see the table.
    pub data: [u8; 4],
    /// Reserved, 0.
    pub reserved: u32,
}

/// [`WireEvent`] decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireEventKind {
    /// Note start.
    NoteOn {
        /// MIDI channel 0 ..= 15.
        channel: u8,
        /// MIDI key.
        key: u8,
        /// MIDI velocity 1 ..= 127.
        velocity: u8,
        /// Note id or `u32::MAX`.
        note_id: u32,
    },
    /// Note end.
    NoteOff {
        /// MIDI channel 0 ..= 15.
        channel: u8,
        /// MIDI key.
        key: u8,
        /// Note id or `u32::MAX`.
        note_id: u32,
    },
    /// Other MIDI channel message.
    Midi([u8; 3]),
    /// Parameter value (plain range).
    ParamValue {
        /// Parameter id.
        id: u32,
        /// Value.
        value: f64,
    },
    /// Begin (`true`) or end of a parameter gesture.
    ParamGesture {
        /// Parameter id.
        id: u32,
        /// Begin or end.
        begin: bool,
    },
    /// Release all notes.
    AllNotesOff,
}

impl WireEvent {
    /// Encodes `kind` at frame `offset`.
    pub fn new(offset: u32, kind: WireEventKind) -> WireEvent {
        todo!("WP-16")
    }

    /// Decodes the event; `None` for an unknown or invalid `kind`.
    pub fn decode(&self) -> Option<WireEventKind> {
        todo!("WP-16")
    }
}

/// Why a mapping could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShmError {
    /// A syscall failed (memfd, ftruncate, mmap); the OS error text.
    Io(String),
    /// The header does not start with [`SHM_MAGIC`].
    BadMagic,
    /// Different [`SHM_VERSION`].
    VersionMismatch {
        /// Version in the header.
        found: u32,
    },
    /// Different [`SHM_SIZE`] or [`MAX_BLOCK`]: the two sides were built differently.
    LayoutMismatch,
}

impl core::fmt::Display for ShmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ShmError::Io(m) => write!(f, "shared memory: {m}"),
            ShmError::BadMagic => write!(f, "shared memory: bad magic"),
            ShmError::VersionMismatch { found } => {
                write!(f, "shared memory: version {found}, expected {SHM_VERSION}")
            }
            ShmError::LayoutMismatch => write!(f, "shared memory: layout mismatch"),
        }
    }
}

impl std::error::Error for ShmError {}

/// One mapped memfd holding a [`ShmLayout`]. Unmapped on drop (the fd closes with it).
pub struct ShmRegion {
    fd: OwnedFd,
    ptr: NonNull<ShmLayout>,
}

// SAFETY: the mapping stays valid for the lifetime of the region; all state shared between threads
// and processes is either atomic or behind `UnsafeCell`s whose access follows the sequence protocol
// documented at module level, enforced by the role types `BridgeClient`, `BridgeServer` and
// `BridgeMonitor`.
unsafe impl Send for ShmRegion {}
// SAFETY: see `Send`; `&ShmRegion` only exposes atomics and the role-checked accessors.
unsafe impl Sync for ShmRegion {}

impl ShmRegion {
    /// App side, non-RT. Creates a memfd named `komponist-bridge`, sizes it to [`SHM_SIZE`], maps
    /// it and writes the header constants.
    pub fn create() -> Result<ShmRegion, ShmError> {
        todo!("WP-16")
    }

    /// Bridge side, non-RT. Maps a memfd received from the app and validates magic, version, size
    /// and `max_block`.
    pub fn open(fd: OwnedFd) -> Result<ShmRegion, ShmError> {
        todo!("WP-16")
    }

    /// The memfd, to pass to the bridge.
    pub fn fd(&self) -> BorrowedFd<'_> {
        todo!("WP-16")
    }

    /// The mapped layout.
    pub fn layout(&self) -> &ShmLayout {
        todo!("WP-16")
    }

    /// Bridge lifecycle state (any thread).
    pub fn bridge_state(&self) -> BridgeState {
        BridgeState::from_u32(self.layout().header.bridge_state.load(Ordering::Acquire))
    }

    /// Bridge side (any thread): publishes the lifecycle state.
    pub fn set_bridge_state(&self, state: BridgeState) {
        self.layout()
            .header
            .bridge_state
            .store(state as u32, Ordering::Release);
    }

    /// Plugin latency in frames (any thread).
    pub fn latency(&self) -> u32 {
        self.layout().header.latency.load(Ordering::Acquire)
    }

    /// Bridge side (any thread): publishes the plugin latency in frames.
    pub fn set_latency(&self, frames: u32) {
        self.layout()
            .header
            .latency
            .store(frames, Ordering::Release);
    }
}

impl core::fmt::Debug for ShmRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ShmRegion")
            .field("fd", &self.fd)
            .field("ptr", &self.ptr)
            .finish()
    }
}

/// Parameters of a block for [`BridgeClient::begin_block`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BlockSpec {
    /// Frames, 1 ..= [`MAX_BLOCK`].
    pub frames: u32,
    /// `BLOCK_FLAG_*` bits.
    pub flags: u32,
    /// Running sample counter of the engine.
    pub steady_time: u64,
    /// Transport at frame 0.
    pub transport: WireTransport,
}

/// Why a block did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockError {
    /// The bridge is still working on an earlier (late) request; nothing was written.
    Busy,
    /// The deadline passed before the bridge finished; use the fallback for this block.
    Late,
    /// The bridge is not `Active` (starting, loading, failed); nothing was written.
    NotActive,
    /// More events than [`INPUT_EVENT_CAPACITY`] free slots; nothing was written.
    TooManyEvents,
}

/// The app's RT end of a bridge: producer of requests and input events, reader of output audio.
/// Owned by the `ExternalProcessor` on the audio thread. All methods are RT-safe.
#[derive(Debug)]
pub struct BridgeClient {
    region: Arc<ShmRegion>,
}

impl BridgeClient {
    /// The RT end of `region`. Exactly one `BridgeClient` may exist per region.
    pub fn new(region: Arc<ShmRegion>) -> BridgeClient {
        BridgeClient { region }
    }

    /// Whether the bridge finished the latest request (`done_seq == request_seq`).
    pub fn is_idle(&self) -> bool {
        todo!("WP-16")
    }

    /// Publishes one block (step 1 of the handshake) and wakes the bridge. Returns the sequence
    /// number to wait for. `input` holds `spec.frames` samples per channel; `events` are sorted by
    /// offset.
    pub fn begin_block(
        &mut self,
        spec: &BlockSpec,
        input: [&[f32]; 2],
        events: &[WireEvent],
    ) -> Result<u32, BlockError> {
        todo!("WP-16")
    }

    /// Waits for request `seq` until `deadline` (step 3). On success copies `frames` samples per
    /// channel of output audio into `output` and returns the bridge's result.
    pub fn wait_done(
        &mut self,
        seq: u32,
        deadline: Instant,
        output: [&mut [f32]; 2],
    ) -> Result<BlockResult, BlockError> {
        todo!("WP-16")
    }

    /// The region (for state and latency).
    pub fn region(&self) -> &Arc<ShmRegion> {
        &self.region
    }
}

/// The bridge's RT end: consumer of requests and input events, producer of output audio, output
/// events and results. Owned by the bridge RT thread. All methods except `wait_request` are
/// RT-safe and non-blocking; `wait_request` blocks on the futex.
#[derive(Debug)]
pub struct BridgeServer {
    region: Arc<ShmRegion>,
}

impl BridgeServer {
    /// The RT end of `region`. Exactly one `BridgeServer` may exist per region.
    pub fn new(region: Arc<ShmRegion>) -> BridgeServer {
        BridgeServer { region }
    }

    /// Blocks until a new request is published or `deadline` passes (`None` = forever; a deadline
    /// lets the RT thread check for shutdown). Returns the request.
    pub fn wait_request(&mut self, deadline: Option<Instant>) -> Option<BlockRequest> {
        todo!("WP-16")
    }

    /// The next input event of the current request (call until `None`, before [`Self::audio`]).
    pub fn pop_event(&mut self) -> Option<WireEvent> {
        todo!("WP-16")
    }

    /// Input and output audio of the current request, `frames` samples per channel.
    pub fn audio(&mut self, frames: usize) -> ([&[f32]; 2], [&mut [f32]; 2]) {
        todo!("WP-16")
    }

    /// Queues a plugin output event for the app; `false` if the ring is full (count it in the
    /// result).
    pub fn push_output_event(&mut self, event: WireEvent) -> bool {
        todo!("WP-16")
    }

    /// Completes the current request (step 2): writes `result` (its `seq` is filled in), sets
    /// `done_seq` and wakes the app.
    pub fn finish(&mut self, result: BlockResult) {
        todo!("WP-16")
    }

    /// The region (for state and latency).
    pub fn region(&self) -> &Arc<ShmRegion> {
        &self.region
    }
}

/// The app's UI-thread end: consumer of the plugin's output events. Owned by the plugin manager.
#[derive(Debug)]
pub struct BridgeMonitor {
    region: Arc<ShmRegion>,
}

impl BridgeMonitor {
    /// The output-event consumer of `region`. Exactly one `BridgeMonitor` may exist per region.
    pub fn new(region: Arc<ShmRegion>) -> BridgeMonitor {
        BridgeMonitor { region }
    }

    /// The next plugin output event (parameter changes and gestures from the plugin GUI).
    pub fn pop_output_event(&mut self) -> Option<WireEvent> {
        todo!("WP-16")
    }

    /// The region (for state and latency).
    pub fn region(&self) -> &Arc<ShmRegion> {
        &self.region
    }
}
