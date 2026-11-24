//! Shared memory between the app and one bridge process (ARCHITECTURE §8.1).
//!
//! One shared-memory file per plugin instance, fixed size [`SHM_SIZE`], mapped `MAP_SHARED` by both
//! processes. The app creates it ([`ShmRegion::create`]) and passes the fd to the bridge over the
//! control socket (SCM_RIGHTS); the bridge maps it with [`ShmRegion::open`]. Per OS:
//!
//! - Linux: a memfd, its size sealed (`F_SEAL_SHRINK | F_SEAL_GROW`), mapped with `MAP_POPULATE`.
//! - macOS: a POSIX shm object (`shm_open` with a fresh random name, unlinked at once, so only the
//!   fd refers to it). Its size can be set once only (a second `ftruncate` fails), which gives the
//!   guarantee the seals give on Linux. The mapping is prefaulted by `mlock` (or by touching every
//!   page if locking is refused). `fstat` reports the size rounded up to the 16 KiB page.
//! - Windows: a pagefile-backed section (`CreateFileMappingW` on `INVALID_HANDLE_VALUE`, no name,
//!   not inheritable) of exactly `SHM_SIZE` bytes; a section's size can never change, so no seal is
//!   needed. Mapped with `MapViewOfFile`, prefaulted by `VirtualLock` (best effort: the default
//!   minimum working set lets a process lock about one region) or else by touching every page.
//!   The bridge maps the whole section and checks its size with `VirtualQuery`. Handles cannot
//!   travel in-band, so the region also owns the two wake events of the handshake ([`signal`],
//!   "Windows"): the app duplicates the section and both events into the bridge
//!   (`Channel::send_region`), the bridge opens them as `ShmHandles`. Each region also has a
//!   deadline timer per waiting role, created in each process. In the handshake below, "futex-wake"
//!   is `SetEvent` there and "futex-wait" the event wait with its timer (the app's wait spins
//!   briefly and ends at its deadline within µs; the bridge's does neither).
//!
//! Layout ([`ShmLayout`], `#[repr(C)]`, all offsets fixed): header with the futex words, the
//! per-block request and result, an input event ring (app → bridge), an output event ring
//! (bridge → app), stereo audio in/out of [`MAX_BLOCK`] frames and, since version 2 (#6), the
//! stereo sidechain input of the same size. A block carries a sidechain only when its request has
//! [`BLOCK_FLAG_SIDECHAIN`]; without it the bridge applies the plugin format's disconnected-input
//! behavior.
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
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(windows)]
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::sync::Arc;
use std::time::Instant;

#[cfg(unix)]
use rustix::fs::fstat;
#[cfg(unix)]
use rustix::mm::{MapFlags, ProtFlags, mlock, mmap, munmap};

use crate::ring::ShmRing;
use crate::signal::{self, WaitOutcome};

/// `b"KPSM"` as little-endian `u32`, the first header field.
pub const SHM_MAGIC: u32 = u32::from_le_bytes(*b"KPSM");

/// Layout version; both sides must agree. 2 (#6): [`ShmLayout::audio_sidechain`] and
/// [`BLOCK_FLAG_SIDECHAIN`]. 3 (#45): [`BlockRequest::deadline_ticks`].
/// 4 (#191): optional bounded effect-meter data from the same completed block.
/// 5 (#289): optional named signal streams from the same completed block.
pub const SHM_VERSION: u32 = 5;

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
/// [`BlockRequest::flags`]: [`ShmLayout::audio_sidechain`] holds this block's sidechain input
/// (set by [`BridgeClient::begin_block`] when it is given one; #6). Without it the source is
/// disconnected.
pub const BLOCK_FLAG_SIDECHAIN: u32 = 4;
/// The client requests gain reduction from the plugin's effect-meter extension.
pub const BLOCK_FLAG_EFFECT_METER: u32 = 8;
/// In addition to gain reduction, request detector history for this process call.
pub const BLOCK_FLAG_EFFECT_DETECTOR: u32 = 16;
/// `BlockResult.reserved` uses this bit when the bridge wrote effect-meter data.
const RESULT_EFFECT_METER: u32 = 1;
const RESULT_SIGNAL_TAPS: u32 = 2;
const TAP_SHIFT: u32 = 8;
const TAP_MASK: u32 = (1 << crate::signal_taps::MAX_TAPS) - 1;

/// Encode a signal tap selection into the block request flags.
pub fn signal_tap_flags(mask: u32) -> u32 {
    (mask & TAP_MASK) << TAP_SHIFT
}

/// [`WireTransport::flags`]: transport running.
pub const TRANSPORT_PLAYING: u32 = 1;
/// [`WireTransport::flags`]: recording.
pub const TRANSPORT_RECORDING: u32 = 2;
/// [`WireTransport::flags`]: loop on.
pub const TRANSPORT_LOOPING: u32 = 4;

/// [`WireEvent::kind`] values (see the table at [`WireEvent`]).
const KIND_NOTE_ON: u32 = 1;
const KIND_NOTE_OFF: u32 = 2;
const KIND_MIDI: u32 = 3;
const KIND_PARAM_VALUE: u32 = 4;
const KIND_PARAM_GESTURE: u32 = 5;
const KIND_ALL_NOTES_OFF: u32 = 6;

/// [`WireEvent::note_id`] for "no note id".
const NO_NOTE_ID: u32 = u32::MAX;

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
    /// Sidechain input audio, `[channel][frame]` (#6); written by the app before a request with
    /// [`BLOCK_FLAG_SIDECHAIN`], untouched otherwise.
    pub audio_sidechain: UnsafeCell<[[f32; MAX_BLOCK]; 2]>,
    /// Output audio, `[channel][frame]`; written by the bridge before `done_seq`.
    pub audio_out: UnsafeCell<[[f32; MAX_BLOCK]; 2]>,
    /// Optional named streams, read only after the matching completion.
    signal_taps: UnsafeCell<crate::signal_taps::BlockTaps>,
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
    /// Optional meter readings, written with the matching `result` before the bridge signals.
    pub meter: UnsafeCell<crate::effect_meter::BlockMeter>,
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
    /// When the app stops waiting for this block ([`BlockSpec::deadline`]) as an absolute
    /// `mach_absolute_time` (macOS): the bridge RT thread passes it to its audio work interval, so
    /// the CPU frequency controller knows the real deadline (#45). 0 = none (offline render, and
    /// always on Linux, where nothing reads it).
    pub deadline_ticks: u64,
    /// Transport at frame 0.
    pub transport: WireTransport,
}

impl BlockRequest {
    /// The selected streams for this callback.
    pub fn signal_taps(&self) -> u32 {
        (self.flags >> TAP_SHIFT) & TAP_MASK
    }
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
    /// Result flags (zero unless the bridge returned effect-meter data).
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
    /// Originating block sequence for bridge output events. Zero for inputs.
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
        let mut event = WireEvent {
            offset,
            note_id: NO_NOTE_ID,
            ..WireEvent::default()
        };
        match kind {
            WireEventKind::NoteOn {
                channel,
                key,
                velocity,
                note_id,
            } => {
                event.kind = KIND_NOTE_ON;
                event.data = [channel, key, velocity, 0];
                event.note_id = note_id;
            }
            WireEventKind::NoteOff {
                channel,
                key,
                note_id,
            } => {
                event.kind = KIND_NOTE_OFF;
                event.data = [channel, key, 0, 0];
                event.note_id = note_id;
            }
            WireEventKind::Midi([status, data1, data2]) => {
                event.kind = KIND_MIDI;
                event.data = [status, data1, data2, 0];
            }
            WireEventKind::ParamValue { id, value } => {
                event.kind = KIND_PARAM_VALUE;
                event.param_id = id;
                event.value = value;
            }
            WireEventKind::ParamGesture { id, begin } => {
                event.kind = KIND_PARAM_GESTURE;
                event.param_id = id;
                event.data[0] = u8::from(begin);
            }
            WireEventKind::AllNotesOff => event.kind = KIND_ALL_NOTES_OFF,
        }
        event
    }

    /// Decodes the event; `None` for an unknown or invalid `kind`.
    pub fn decode(&self) -> Option<WireEventKind> {
        let [d0, d1, d2, _] = self.data;
        Some(match self.kind {
            KIND_NOTE_ON => WireEventKind::NoteOn {
                channel: d0,
                key: d1,
                velocity: d2,
                note_id: self.note_id,
            },
            KIND_NOTE_OFF => WireEventKind::NoteOff {
                channel: d0,
                key: d1,
                note_id: self.note_id,
            },
            KIND_MIDI => WireEventKind::Midi([d0, d1, d2]),
            KIND_PARAM_VALUE => WireEventKind::ParamValue {
                id: self.param_id,
                value: self.value,
            },
            KIND_PARAM_GESTURE => WireEventKind::ParamGesture {
                id: self.param_id,
                begin: d0 != 0,
            },
            KIND_ALL_NOTES_OFF => WireEventKind::AllNotesOff,
            _ => return None,
        })
    }
}

/// Why a mapping could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShmError {
    /// A syscall failed (memfd / shm_open, ftruncate, mmap); the OS error text.
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

#[cfg(unix)]
impl From<rustix::io::Errno> for ShmError {
    fn from(e: rustix::io::Errno) -> ShmError {
        ShmError::Io(e.to_string())
    }
}

#[cfg(windows)]
impl From<std::io::Error> for ShmError {
    fn from(e: std::io::Error) -> ShmError {
        ShmError::Io(e.to_string())
    }
}

/// One of the two handshake words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
    /// `request_seq`: written by the app, awaited by the bridge.
    Request,
    /// `done_seq`: written by the bridge, awaited by the app.
    Done,
}

/// Windows: the handles the bridge needs to open a region, received from the app
/// (`Channel::recv_region`; the app sends them with `Channel::send_region`).
#[cfg(windows)]
#[derive(Debug)]
pub struct ShmHandles {
    /// The pagefile-backed section.
    pub mapping: OwnedHandle,
    /// The wake event of `request_seq`.
    pub request: OwnedHandle,
    /// The wake event of `done_seq`.
    pub done: OwnedHandle,
}

/// Role bits in [`ShmRegion::roles`]: each RT role may exist once per region.
const ROLE_CLIENT: u8 = 1;
const ROLE_SERVER: u8 = 2;
const ROLE_MONITOR: u8 = 4;

/// One mapped shared-memory file holding a [`ShmLayout`]. Unmapped on drop (the fd closes with it).
pub struct ShmRegion {
    #[cfg(unix)]
    fd: OwnedFd,
    /// The section, the wake events and this process' deadline timers.
    #[cfg(windows)]
    os: sys::Objects,
    ptr: NonNull<ShmLayout>,
    /// `ROLE_*` bits of the live role objects in this process; keeps the unsafe single-producer /
    /// single-consumer contracts of the rings out of reach of safe code.
    roles: AtomicU8,
}

// SAFETY: the mapping stays valid for the lifetime of the region; all state shared between threads
// and processes is either atomic or behind `UnsafeCell`s whose access follows the sequence protocol
// documented at module level, enforced by the role types `BridgeClient`, `BridgeServer` and
// `BridgeMonitor`.
unsafe impl Send for ShmRegion {}
// SAFETY: see `Send`; `&ShmRegion` only exposes atomics and the role-checked accessors.
unsafe impl Sync for ShmRegion {}

impl ShmRegion {
    /// App side, non-RT. Creates the shared-memory file (module docs), sizes it to [`SHM_SIZE`]
    /// for good, maps it and writes the header constants.
    #[cfg(unix)]
    pub fn create() -> Result<ShmRegion, ShmError> {
        let fd = sys::create(SHM_SIZE)?;
        let ptr = map(&fd)?;
        // SAFETY: fresh, zero-filled mapping of `SHM_SIZE` bytes that nobody else sees yet.
        unsafe { write_header(ptr) };
        Ok(ShmRegion {
            fd,
            ptr,
            roles: AtomicU8::new(0),
        })
    }

    /// App side, non-RT. Creates the section of [`SHM_SIZE`] bytes and the two wake events
    /// (module docs), maps the section and writes the header constants.
    #[cfg(windows)]
    pub fn create() -> Result<ShmRegion, ShmError> {
        let os = sys::Objects::new(
            sys::create(SHM_SIZE)?,
            signal::WakeEvent::new()?,
            signal::WakeEvent::new()?,
        )?;
        let ptr = sys::map(&os.mapping)?.0;
        // SAFETY: fresh, zero-filled mapping of `SHM_SIZE` bytes that nobody else sees yet.
        unsafe { write_header(ptr) };
        sys::prefault(ptr);
        Ok(ShmRegion {
            os,
            ptr,
            roles: AtomicU8::new(0),
        })
    }

    /// Bridge side, non-RT. Maps the section received from the app with its wake events and
    /// validates magic, version, size and `max_block`.
    #[cfg(windows)]
    pub fn open(handles: ShmHandles) -> Result<ShmRegion, ShmError> {
        let os = sys::Objects::new(
            handles.mapping,
            signal::WakeEvent::from_handle(handles.request),
            signal::WakeEvent::from_handle(handles.done),
        )?;
        // The whole section, whatever its size: `MapViewOfFile` refuses what is not a section.
        let (ptr, size) = sys::map(&os.mapping)?;
        // From here on `Drop` unmaps the view whatever the checks find.
        let region = ShmRegion {
            os,
            ptr,
            roles: AtomicU8::new(0),
        };
        // SAFETY: the view is at least one page long, so the four header words are inside it.
        // Volatile: the other process may write the mapping (not these words, by protocol).
        let word = |i: usize| unsafe { ptr.as_ptr().cast::<u32>().add(i).read_volatile() };
        if word(0) != SHM_MAGIC {
            return Err(ShmError::BadMagic);
        }
        if word(1) != SHM_VERSION {
            return Err(ShmError::VersionMismatch { found: word(1) });
        }
        if word(2) as usize != SHM_SIZE || word(3) as usize != MAX_BLOCK || size != SHM_SIZE {
            return Err(ShmError::LayoutMismatch);
        }
        sys::prefault(ptr);
        Ok(region)
    }

    /// Bridge side, non-RT. Maps a shared-memory file received from the app and validates magic,
    /// version, size and `max_block`.
    #[cfg(unix)]
    pub fn open(fd: OwnedFd) -> Result<ShmRegion, ShmError> {
        // Validate before mapping: the four header words, then the file size through `fstat` (a
        // smaller file would turn accesses past its end into SIGBUS).
        let Some(words) = sys::read_header(&fd)? else {
            return Err(ShmError::BadMagic);
        };
        let word = |i: usize| u32::from_ne_bytes(words[i * 4..i * 4 + 4].try_into().unwrap());
        if word(0) != SHM_MAGIC {
            return Err(ShmError::BadMagic);
        }
        if word(1) != SHM_VERSION {
            return Err(ShmError::VersionMismatch { found: word(1) });
        }
        let file_size = fstat(&fd)?.st_size;
        if word(2) as usize != SHM_SIZE
            || word(3) as usize != MAX_BLOCK
            || !sys::size_matches(file_size, SHM_SIZE)
        {
            return Err(ShmError::LayoutMismatch);
        }
        let ptr = map(&fd)?;
        Ok(ShmRegion {
            fd,
            ptr,
            roles: AtomicU8::new(0),
        })
    }

    /// The shared-memory file, to pass to the bridge.
    #[cfg(unix)]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Windows: the handles to pass to the bridge, in the order of [`ShmHandles`]: section,
    /// request event, done event (`Channel::send_region` sends them).
    #[cfg(windows)]
    pub fn handles(&self) -> [BorrowedHandle<'_>; 3] {
        [
            self.os.mapping.as_handle(),
            self.os.request.as_handle(),
            self.os.done.as_handle(),
        ]
    }

    /// The handshake word `word`.
    fn word(&self, word: Word) -> &AtomicU32 {
        let header = &self.layout().header;
        match word {
            Word::Request => &header.request_seq,
            Word::Done => &header.done_seq,
        }
    }

    /// RT: wakes the waiter of `word` (in the other process). Store the word first (release).
    fn wake(&self, word: Word) {
        #[cfg(unix)]
        signal::wake(self.word(word));
        #[cfg(windows)]
        self.os.event(word).wake();
    }

    /// RT: waits while `word == current`, until `deadline` (`None` = none). The app waits for
    /// `Done`, the bridge for `Request`; on Windows each with its own timer and mode.
    fn wait_while(&self, word: Word, current: u32, deadline: Option<Instant>) -> WaitOutcome {
        #[cfg(unix)]
        {
            signal::wait_while(self.word(word), current, deadline)
        }
        #[cfg(windows)]
        {
            let (timer, mode) = match word {
                Word::Done => (&self.os.client_timer, signal::Mode::Exact),
                Word::Request => (&self.os.server_timer, signal::Mode::Relaxed),
            };
            signal::wait_while_event(
                self.word(word),
                current,
                deadline,
                self.os.event(word),
                timer,
                mode,
            )
        }
    }

    /// The mapped layout.
    pub fn layout(&self) -> &ShmLayout {
        // SAFETY: the mapping is valid, aligned and `SHM_SIZE >= size_of::<ShmLayout>()` bytes
        // long for the lifetime of `self`. Through `&ShmLayout` safe code reaches only atomics and
        // the header constants (never written after `create`); everything else sits behind
        // `UnsafeCell`s and unsafe ring methods.
        unsafe { self.ptr.as_ref() }
    }

    /// Marks `role` as taken; panics if it already is (contract: one object per role).
    fn claim(&self, role: u8, name: &str) {
        let before = self.roles.fetch_or(role, Ordering::AcqRel);
        assert!(before & role == 0, "a second {name} for one ShmRegion");
    }

    /// Frees `role` again (drop of the role object).
    fn release(&self, role: u8) {
        self.roles.fetch_and(!role, Ordering::AcqRel);
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

impl Drop for ShmRegion {
    fn drop(&mut self) {
        // SAFETY: `ptr` is the start of our own `SHM_SIZE` mapping; no reference into it outlives
        // `self` (role objects hold an `Arc<ShmRegion>`).
        #[cfg(unix)]
        unsafe {
            let _ = munmap(self.ptr.as_ptr().cast(), SHM_SIZE);
        }
        // SAFETY: as above (Windows: the view of the whole section).
        #[cfg(windows)]
        unsafe {
            sys::unmap(self.ptr);
        }
    }
}

/// Writes the header constants into a fresh mapping.
///
/// # Safety
///
/// `ptr` is a new mapping of at least `SHM_SIZE` bytes that nobody else sees yet; the constants
/// are written through raw pointers before any reference to the layout exists.
unsafe fn write_header(ptr: NonNull<ShmLayout>) {
    // SAFETY: per the caller.
    unsafe {
        let header = &raw mut (*ptr.as_ptr()).header;
        (&raw mut (*header).magic).write(SHM_MAGIC);
        (&raw mut (*header).version).write(SHM_VERSION);
        (&raw mut (*header).size).write(SHM_SIZE as u32);
        (&raw mut (*header).max_block).write(MAX_BLOCK as u32);
    }
}

/// Maps `SHM_SIZE` bytes of `fd` shared, prefaulted and (best effort) locked, so the RT paths
/// never take a page fault.
#[cfg(unix)]
fn map(fd: &OwnedFd) -> Result<NonNull<ShmLayout>, ShmError> {
    // SAFETY: a new mapping at an address the kernel picks; nothing aliases it yet. The caller
    // made sure the file is at least `SHM_SIZE` bytes long.
    let ptr = unsafe {
        mmap(
            core::ptr::null_mut(),
            SHM_SIZE,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED | sys::PREFAULT,
            fd,
            0,
        )?
    };
    // SAFETY: locks our own fresh mapping (which also faults its pages in). Failure
    // (RLIMIT_MEMLOCK) only means the pages may be swapped out under memory pressure.
    let locked = unsafe { mlock(ptr, SHM_SIZE) }.is_ok();
    if !locked && sys::PREFAULT.is_empty() {
        // No MAP_POPULATE on this OS: fault every page in by reading one byte of it.
        for offset in (0..SHM_SIZE).step_by(4096) {
            // SAFETY: inside our own mapping of `SHM_SIZE` bytes, which the file covers.
            unsafe { core::ptr::read_volatile(ptr.cast::<u8>().add(offset)) };
        }
    }
    NonNull::new(ptr.cast()).ok_or_else(|| ShmError::Io("mmap returned null".into()))
}

#[cfg(target_os = "linux")]
mod sys {
    use std::os::fd::OwnedFd;

    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
    use rustix::mm::MapFlags;

    use super::ShmError;

    /// The mapping is prefaulted by the kernel.
    pub(super) const PREFAULT: MapFlags = MapFlags::POPULATE;

    /// A memfd named `komponist-bridge` of `size` bytes, its size sealed.
    pub(super) fn create(size: usize) -> Result<OwnedFd, ShmError> {
        let fd = memfd_create(
            "komponist-bridge",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?;
        ftruncate(&fd, size as u64)?;
        // Fix the size for good: a bridge that shrank the file would make the app's next access
        // to the missing pages fault with SIGBUS.
        fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)?;
        Ok(fd)
    }

    /// The first 16 bytes through `pread`; `None` if the file is shorter.
    pub(super) fn read_header(fd: &OwnedFd) -> Result<Option<[u8; 16]>, ShmError> {
        let mut words = [0u8; 16];
        let read = rustix::io::pread(fd, &mut words, 0)?;
        Ok((read == words.len()).then_some(words))
    }

    /// The sealed memfd has exactly the size `create` gave it.
    pub(super) fn size_matches(file_size: i64, size: usize) -> bool {
        file_size == size as i64
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::os::fd::OwnedFd;
    use std::time::{SystemTime, UNIX_EPOCH};

    use rustix::fs::{FileType, Mode, fstat, ftruncate};
    use rustix::io::Errno;
    use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
    use rustix::shm;

    use super::ShmError;

    /// No `MAP_POPULATE`: `map` prefaults with `mlock` or by touching the pages.
    pub(super) const PREFAULT: MapFlags = MapFlags::empty();

    /// Longest shm name including the leading slash (`PSHMNAMLEN`).
    const NAME_MAX: usize = 31;

    /// A fresh, unlinked POSIX shm object of `size` bytes (rounded up to the page by the kernel).
    ///
    /// The name is never seen by anybody: `/kp.<pid>.<counter>.<clock>` in hex (at most 27 chars),
    /// created with `O_EXCL` and mode 0600 and unlinked right away, so only the fd refers to the
    /// object. A name taken by someone else (`EEXIST`) just moves on to the next one.
    pub(super) fn create(size: usize) -> Result<OwnedFd, ShmError> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let pid = std::process::id();
        for _ in 0..64 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let name = format!("/kp.{pid:x}.{n:x}.{clock:x}");
            debug_assert!(name.len() <= NAME_MAX, "{name}");
            let flags = shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR;
            let fd = match shm::open(name.as_str(), flags, Mode::RUSR | Mode::WUSR) {
                Ok(fd) => fd,
                Err(Errno::EXIST | Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            };
            shm::unlink(name.as_str())?;
            // The only size this object will ever have: a second `ftruncate` fails with EINVAL.
            ftruncate(&fd, size as u64)?;
            return Ok(fd);
        }
        Err(Errno::EXIST.into())
    }

    /// The first 16 bytes through a short read-only mapping (POSIX shm objects support neither
    /// `read` nor `pread`); `None` if the object is shorter. Sockets, pipes and devices are refused
    /// as `Io` (a shm object's `st_mode` carries no file type at all).
    pub(super) fn read_header(fd: &OwnedFd) -> Result<Option<[u8; 16]>, ShmError> {
        let mut words = [0u8; 16];
        let st = fstat(fd)?;
        match FileType::from_raw_mode(st.st_mode) {
            FileType::Socket
            | FileType::Fifo
            | FileType::Directory
            | FileType::CharacterDevice
            | FileType::BlockDevice
            | FileType::Symlink => return Err(ShmError::Io("not a shared-memory file".into())),
            FileType::RegularFile | FileType::Unknown => {}
        }
        if st.st_size < words.len() as i64 {
            return Ok(None);
        }
        // SAFETY: a new view at an address the kernel picks, read only within the object's size
        // (checked above; the size of a shm object cannot change) and unmapped right away.
        unsafe {
            let ptr = mmap(
                core::ptr::null_mut(),
                words.len(),
                ProtFlags::READ,
                MapFlags::SHARED,
                fd,
                0,
            )?;
            core::ptr::copy_nonoverlapping(ptr.cast::<u8>(), words.as_mut_ptr(), words.len());
            let _ = munmap(ptr, words.len());
        }
        Ok(Some(words))
    }

    /// The kernel rounds the object's size up to the page (16 KiB on Apple Silicon); larger than
    /// `size` is fine, since the size can never change again.
    pub(super) fn size_matches(file_size: i64, size: usize) -> bool {
        let page = rustix::param::page_size() as i64;
        file_size >= size as i64 && file_size < size as i64 + page
    }
}

#[cfg(windows)]
mod sys {
    use core::ffi::c_void;
    use core::ptr::NonNull;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION,
        MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile, VirtualLock,
        VirtualQuery,
    };

    use super::{SHM_SIZE, ShmError, ShmLayout, Word};
    use crate::signal::{DeadlineTimer, WakeEvent};

    /// The kernel objects of one region in this process.
    #[derive(Debug)]
    pub(super) struct Objects {
        /// The section.
        pub(super) mapping: OwnedHandle,
        /// Wakes the bridge (`request_seq`).
        pub(super) request: WakeEvent,
        /// Wakes the app (`done_seq`).
        pub(super) done: WakeEvent,
        /// The `BridgeClient`'s deadline timer (it waits for `done_seq`).
        pub(super) client_timer: DeadlineTimer,
        /// The `BridgeServer`'s deadline timer (it waits for `request_seq`).
        pub(super) server_timer: DeadlineTimer,
    }

    impl Objects {
        /// The shared objects plus this process' two timers.
        pub(super) fn new(
            mapping: OwnedHandle,
            request: WakeEvent,
            done: WakeEvent,
        ) -> Result<Objects, ShmError> {
            Ok(Objects {
                mapping,
                request,
                done,
                client_timer: DeadlineTimer::new()?,
                server_timer: DeadlineTimer::new()?,
            })
        }

        /// The wake event of `word`.
        pub(super) fn event(&self, word: Word) -> &WakeEvent {
            match word {
                Word::Request => &self.request,
                Word::Done => &self.done,
            }
        }
    }

    /// A pagefile-backed, unnamed, non-inheritable section of exactly `size` bytes.
    pub(super) fn create(size: usize) -> Result<OwnedHandle, ShmError> {
        let size = size as u64;
        // SAFETY: no file (the pagefile backs it), no security attributes, no name.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                core::ptr::null(),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                core::ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: a fresh handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// Maps the whole section read-write; the view and its size in bytes (whole pages). Fails for
    /// a handle that is not a section.
    pub(super) fn map(mapping: &OwnedHandle) -> Result<(NonNull<ShmLayout>, usize), ShmError> {
        // SAFETY: a new view at an address the kernel picks; nothing aliases it yet.
        let view = unsafe {
            MapViewOfFile(
                mapping.as_raw_handle(),
                FILE_MAP_READ | FILE_MAP_WRITE,
                0,
                0,
                0,
            )
        };
        let Some(ptr) = NonNull::new(view.Value.cast::<ShmLayout>()) else {
            return Err(io::Error::last_os_error().into());
        };
        // SAFETY: a plain C struct for which all zeroes are valid.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { core::mem::zeroed() };
        // SAFETY: queries our own view into a properly sized buffer.
        let written = unsafe {
            VirtualQuery(
                view.Value,
                &mut info,
                core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if written == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: our own view, unused so far.
            unsafe { unmap(ptr) };
            return Err(error.into());
        }
        Ok((ptr, info.RegionSize))
    }

    /// Locks the view's `SHM_SIZE` bytes into memory (which faults them in), or, if the working
    /// set quota refuses it, faults every page in by reading one byte of it, so the RT paths
    /// start without page faults. Unlocked pages may still be trimmed under memory pressure.
    pub(super) fn prefault(ptr: NonNull<ShmLayout>) {
        let base = ptr.as_ptr().cast::<c_void>();
        // SAFETY: our own view of at least `SHM_SIZE` bytes; locking changes no contents.
        if unsafe { VirtualLock(base, SHM_SIZE) } != 0 {
            return;
        }
        for offset in (0..SHM_SIZE).step_by(4096) {
            // SAFETY: inside our own view of at least `SHM_SIZE` bytes.
            unsafe { core::ptr::read_volatile(ptr.as_ptr().cast::<u8>().add(offset)) };
        }
    }

    /// Unmaps a view from [`map`] (which also ends a `VirtualLock`).
    ///
    /// # Safety
    ///
    /// `ptr` is a live view from [`map`] that nothing refers to any more.
    pub(super) unsafe fn unmap(ptr: NonNull<ShmLayout>) {
        let view = MEMORY_MAPPED_VIEW_ADDRESS {
            Value: ptr.as_ptr().cast(),
        };
        // SAFETY: per the caller.
        unsafe { UnmapViewOfFile(view) };
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("komponist-ipc: no shared-memory backend (shm.rs) for this OS");

impl core::fmt::Debug for ShmRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut s = f.debug_struct("ShmRegion");
        #[cfg(unix)]
        s.field("fd", &self.fd);
        #[cfg(windows)]
        s.field("os", &self.os);
        s.field("ptr", &self.ptr).finish()
    }
}

/// The audio of the open request, from [`BridgeServer::audio_with_sidechain`]: input, sidechain
/// (`None` without one), output.
pub type BlockAudio<'a> = ([&'a [f32]; 2], Option<[&'a [f32]; 2]>, [&'a mut [f32]; 2]);

/// Copies `frames` samples of each channel of `src` into the stereo buffer `dst`.
///
/// # Safety
///
/// Nobody else may access `dst` during the call (the handshake's ownership rules), and
/// `frames <= MAX_BLOCK` with every `src` channel at least `frames` long.
unsafe fn write_stereo(dst: &UnsafeCell<[[f32; MAX_BLOCK]; 2]>, src: [&[f32]; 2], frames: usize) {
    let dst = dst.get().cast::<[f32; MAX_BLOCK]>();
    for (channel, samples) in src.iter().enumerate() {
        // SAFETY: per the caller; `channel < 2`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                samples.as_ptr(),
                dst.add(channel).cast::<f32>(),
                frames,
            );
        }
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
    /// The deadline the app will wait for this block until ([`BridgeClient::wait_done`]), `None`
    /// for an offline render. Goes to the bridge as [`BlockRequest::deadline_ticks`].
    pub deadline: Option<Instant>,
    /// Transport at frame 0.
    pub transport: WireTransport,
}

/// RT. [`BlockRequest::deadline_ticks`] for `deadline`: the absolute `mach_absolute_time` on
/// macOS, 0 without a deadline and on Linux and Windows (no reader there; saves the clock reads).
fn deadline_ticks(deadline: Option<Instant>) -> u64 {
    #[cfg(target_os = "macos")]
    {
        deadline.map_or(0, komponist_rt::mach_time::deadline)
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        let _ = deadline;
        0
    }
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
    /// Sequence number of the latest request (this client is the only writer of `request_seq`,
    /// so a local copy cannot be confused by the bridge).
    seq: u32,
    /// Frames of the latest request this client published; 0 = none yet.
    frames: u32,
}

impl BridgeClient {
    /// The RT end of `region`. Exactly one `BridgeClient` may exist per region (a second one
    /// panics while the first is alive).
    pub fn new(region: Arc<ShmRegion>) -> BridgeClient {
        region.claim(ROLE_CLIENT, "BridgeClient");
        let seq = region.layout().header.request_seq.load(Ordering::Acquire);
        BridgeClient {
            region,
            seq,
            frames: 0,
        }
    }

    /// Whether the bridge finished the latest request (`done_seq == request_seq`).
    pub fn is_idle(&self) -> bool {
        self.region.layout().header.done_seq.load(Ordering::Acquire) == self.seq
    }

    /// Publishes one block (step 1 of the handshake) and wakes the bridge. Returns the sequence
    /// number to wait for. `input` holds `spec.frames` samples per channel; `sidechain`, if given,
    /// too (it sets [`BLOCK_FLAG_SIDECHAIN`]; `None` clears it, so the bridge treats the source as
    /// disconnected); `events` are sorted by offset.
    ///
    /// # Panics
    ///
    /// If `spec.frames` is not in 1 ..= [`MAX_BLOCK`] or an input or sidechain channel is shorter.
    pub fn begin_block(
        &mut self,
        spec: &BlockSpec,
        input: [&[f32]; 2],
        sidechain: Option<[&[f32]; 2]>,
        events: &[WireEvent],
    ) -> Result<u32, BlockError> {
        let frames = spec.frames as usize;
        assert!(
            (1..=MAX_BLOCK).contains(&frames)
                && input.iter().all(|ch| ch.len() >= frames)
                && sidechain.is_none_or(|sc| sc.iter().all(|ch| ch.len() >= frames)),
            "begin_block: frames out of range or input too short"
        );
        if self.region.bridge_state() != BridgeState::Active {
            return Err(BlockError::NotActive);
        }
        if !self.is_idle() {
            return Err(BlockError::Busy);
        }
        let layout = self.region.layout();
        let free = INPUT_EVENT_CAPACITY.saturating_sub(layout.input_events.len());
        if events.len() > free {
            return Err(BlockError::TooManyEvents);
        }
        // SAFETY: the bridge is idle; it reads `audio_in` and `audio_sidechain` only between a
        // request and its `done_seq`, so nobody else accesses the buffers now.
        // `frames <= MAX_BLOCK`.
        unsafe { write_stereo(&layout.audio_in, input, frames) };
        let mut flags = spec.flags & !BLOCK_FLAG_SIDECHAIN;
        if let Some(sidechain) = sidechain {
            // SAFETY: as above.
            unsafe { write_stereo(&layout.audio_sidechain, sidechain, frames) };
            flags |= BLOCK_FLAG_SIDECHAIN;
        }
        let mut pushed = 0u32;
        for event in events {
            // SAFETY: the `BridgeClient` role (one per region) is the only input-ring producer.
            // Cannot fail after the capacity check unless the bridge corrupted the ring.
            if unsafe { layout.input_events.push(*event) }.is_ok() {
                pushed += 1;
            }
        }
        let seq = self.seq.wrapping_add(1);
        let request = BlockRequest {
            seq,
            frames: spec.frames,
            flags,
            event_count: pushed,
            steady_time: spec.steady_time,
            deadline_ticks: deadline_ticks(spec.deadline),
            transport: spec.transport,
        };
        let header = &layout.header;
        // SAFETY: as for `audio_in`: the bridge reads `request` only after seeing the new
        // `request_seq`, which is published below with release ordering.
        unsafe { header.request.get().write(request) };
        header.request_seq.store(seq, Ordering::Release);
        self.region.wake(Word::Request);
        self.seq = seq;
        self.frames = spec.frames;
        Ok(seq)
    }

    /// Waits for request `seq` until `deadline` (step 3). On success copies `frames` samples per
    /// channel of output audio into `output` and returns the bridge's result.
    ///
    /// `Late` also for a `seq` that is not the latest request (a stale one), and for a result that
    /// does not carry `seq`.
    ///
    /// # Panics
    ///
    /// If an output channel is shorter than the request's frames.
    pub fn wait_done(
        &mut self,
        seq: u32,
        deadline: Instant,
        output: [&mut [f32]; 2],
    ) -> Result<BlockResult, BlockError> {
        if seq != self.seq || self.frames == 0 {
            return Err(BlockError::Late);
        }
        let frames = self.frames as usize;
        assert!(
            output.iter().all(|ch| ch.len() >= frames),
            "wait_done: output too short"
        );
        let result = self.wait_done_events(seq, deadline)?;
        let layout = self.region.layout();
        let audio_out = layout.audio_out.get().cast::<[f32; MAX_BLOCK]>();
        for (channel, samples) in output.into_iter().enumerate() {
            // SAFETY: wait_done_events acquired this request's completion. The bridge
            // touches its output only after the next request; only this client can publish it.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    audio_out.add(channel).cast::<f32>(),
                    samples.as_mut_ptr(),
                    frames,
                );
            }
        }
        Ok(result)
    }

    /// Waits for the same completion as [`Self::wait_done`] without copying output audio.
    /// MIDI effects use the corresponding request sequence to consume their output events.
    pub fn wait_done_events(
        &mut self,
        seq: u32,
        deadline: Instant,
    ) -> Result<BlockResult, BlockError> {
        if seq != self.seq || self.frames == 0 {
            return Err(BlockError::Late);
        }
        let layout = self.region.layout();
        let header = &layout.header;
        loop {
            let done = header.done_seq.load(Ordering::Acquire);
            if done == seq {
                break;
            }
            if self.region.wait_while(Word::Done, done, Some(deadline)) == WaitOutcome::TimedOut {
                return Err(BlockError::Late);
            }
        }
        // SAFETY: `done_seq == seq` (acquired): the bridge wrote `result` and `audio_out` for this
        // request before publishing it and touches neither again until the next request, which
        // only this client can publish.
        let result = unsafe { header.result.get().read() };
        if result.seq != seq {
            return Err(BlockError::Late);
        }
        Ok(result)
    }

    /// Optional meter readings for this client's completed, unretired request.
    pub fn effect_meter(&self, seq: u32) -> Option<crate::effect_meter::BlockMeter> {
        let header = &self.region.layout().header;
        if self.frames == 0 || seq != self.seq || header.done_seq.load(Ordering::Acquire) != seq {
            return None;
        }
        // SAFETY: The matching request is done. Only this client can publish the next request,
        // and the bridge writes these fields only while processing a new request.
        let result = unsafe { header.result.get().read() };
        if result.seq != seq || result.reserved & RESULT_EFFECT_METER == 0 {
            return None;
        }
        // SAFETY: In the matching request the bridge initialized the meter before completing it.
        let meter = unsafe { header.meter.get().read() };
        meter.validated().then_some(meter)
    }

    /// Named streams of the matching completed block, before the next request.
    pub fn signal_taps(&self, seq: u32) -> Option<&crate::signal_taps::BlockTaps> {
        let layout = self.region.layout();
        let header = &layout.header;
        if self.frames == 0 || seq != self.seq || header.done_seq.load(Ordering::Acquire) != seq {
            return None;
        }
        // SAFETY: Only this client can start the next request, which requires a mutable borrow.
        let result = unsafe { header.result.get().read() };
        if result.seq != seq || result.reserved & RESULT_SIGNAL_TAPS == 0 {
            return None;
        }
        // SAFETY: The bridge completed the matching block before publishing the result.
        Some(unsafe { &*layout.signal_taps.get() })
    }

    /// The region (for state and latency).
    pub fn region(&self) -> &Arc<ShmRegion> {
        &self.region
    }
}

impl Drop for BridgeClient {
    fn drop(&mut self) {
        self.region.release(ROLE_CLIENT);
    }
}

/// The bridge's RT end: consumer of requests and input events, producer of output audio, output
/// events and results. Owned by the bridge RT thread. All methods except `wait_request` are
/// RT-safe and non-blocking; `wait_request` blocks on the futex.
#[derive(Debug)]
pub struct BridgeServer {
    region: Arc<ShmRegion>,
    /// Sequence number of the latest request taken (or, before the first one, the last finished
    /// request: a request published before the server existed is still served).
    seq: u32,
    /// Input events of the current request not popped yet.
    remaining_events: u32,
    /// A request was taken and not finished yet.
    busy: bool,
    /// The current request carries a sidechain ([`BLOCK_FLAG_SIDECHAIN`]).
    sidechain: bool,
}

impl BridgeServer {
    /// The RT end of `region`. Exactly one `BridgeServer` may exist per region (a second one
    /// panics while the first is alive).
    pub fn new(region: Arc<ShmRegion>) -> BridgeServer {
        region.claim(ROLE_SERVER, "BridgeServer");
        let seq = region.layout().header.done_seq.load(Ordering::Acquire);
        BridgeServer {
            region,
            seq,
            remaining_events: 0,
            busy: false,
            sidechain: false,
        }
    }

    /// Blocks until a new request is published or `deadline` passes (`None` = forever; a deadline
    /// lets the RT thread check for shutdown). Returns the request.
    ///
    /// `frames` of the returned request is clamped to [`MAX_BLOCK`]. Complete every request with
    /// [`Self::finish`]: until then the app's `begin_block` returns `Busy`, so no new request
    /// arrives.
    pub fn wait_request(&mut self, deadline: Option<Instant>) -> Option<BlockRequest> {
        let header = &self.region.layout().header;
        loop {
            let seq = header.request_seq.load(Ordering::Acquire);
            if seq != self.seq {
                // SAFETY: the app wrote `request` before publishing `seq` (acquired above) and
                // does not write it again until this request is done.
                let request = unsafe { header.request.get().read() };
                self.seq = seq;
                self.remaining_events = request.event_count.min(INPUT_EVENT_CAPACITY as u32);
                self.busy = true;
                self.sidechain = request.flags & BLOCK_FLAG_SIDECHAIN != 0;
                return Some(BlockRequest {
                    seq,
                    frames: request.frames.min(MAX_BLOCK as u32),
                    ..request
                });
            }
            if self.region.wait_while(Word::Request, seq, deadline) == WaitOutcome::TimedOut {
                return None;
            }
        }
    }

    /// The next input event of the current request (call until `None`, before [`Self::audio`]).
    pub fn pop_event(&mut self) -> Option<WireEvent> {
        if self.remaining_events == 0 {
            return None;
        }
        // SAFETY: the `BridgeServer` role (one per region) is the only input-ring consumer.
        match unsafe { self.region.layout().input_events.pop() } {
            Some(event) => {
                self.remaining_events -= 1;
                Some(event)
            }
            None => {
                self.remaining_events = 0;
                None
            }
        }
    }

    /// Input and output audio of the current request, `frames` samples per channel.
    ///
    /// Empty slices while no request is open (before [`Self::wait_request`] or after
    /// [`Self::finish`]): the app may be writing the buffers then.
    ///
    /// # Panics
    ///
    /// If `frames > MAX_BLOCK`.
    pub fn audio(&mut self, frames: usize) -> ([&[f32]; 2], [&mut [f32]; 2]) {
        let (input, _, output) = self.audio_with_sidechain(frames);
        (input, output)
    }

    /// [`Self::audio`] plus the sidechain input of the current request: `Some` with `frames`
    /// samples per channel if the request carries one ([`BLOCK_FLAG_SIDECHAIN`]), else `None`
    /// (the plugin's sidechain port gets silence).
    ///
    /// # Panics
    ///
    /// If `frames > MAX_BLOCK`.
    pub fn audio_with_sidechain(&mut self, frames: usize) -> BlockAudio<'_> {
        assert!(frames <= MAX_BLOCK, "audio: frames > MAX_BLOCK");
        let frames = if self.busy { frames } else { 0 };
        let layout = self.region.layout();
        // SAFETY: while a request is open the app touches none of the buffers (it writes
        // `audio_in` and `audio_sidechain` only when idle and reads `audio_out` only after
        // `done_seq`); the borrows are tied to `&mut self`, and `BridgeServer` exists once per
        // region.
        let (input, sidechain, output) = unsafe {
            (
                &*layout.audio_in.get(),
                &*layout.audio_sidechain.get(),
                &mut *layout.audio_out.get(),
            )
        };
        let [out_l, out_r] = output;
        (
            [&input[0][..frames], &input[1][..frames]],
            (self.busy && self.sidechain)
                .then(|| [&sidechain[0][..frames], &sidechain[1][..frames]]),
            [&mut out_l[..frames], &mut out_r[..frames]],
        )
    }

    /// Queues a plugin output event for the app; `false` if the ring is full (count it in the
    /// result).
    pub fn push_output_event(&mut self, event: WireEvent) -> bool {
        // SAFETY: the `BridgeServer` role (one per region) is the only output-ring producer.
        unsafe { self.region.layout().output_events.push(event) }.is_ok()
    }

    /// Completes the current request (step 2): writes `result` (its `seq` is filled in), sets
    /// `done_seq` and wakes the app.
    ///
    /// Input events of the request that were not popped are discarded, so the next request starts
    /// on its own events. Without an open request this does nothing.
    pub fn finish(&mut self, result: BlockResult) {
        self.finish_with_meter(result, None);
    }

    /// Completes the current request with its optional effect-meter readings.
    pub fn finish_with_meter(
        &mut self,
        result: BlockResult,
        meter: Option<crate::effect_meter::BlockMeter>,
    ) {
        self.finish_with_telemetry(result, meter, false);
    }

    /// Write streams directly into the reusable mapping for an open request.
    pub fn signal_taps_mut(&mut self) -> Option<&mut crate::signal_taps::BlockTaps> {
        if !self.busy {
            return None;
        }
        // SAFETY: This server is the only writer. The client cannot read until completion.
        let payload = unsafe { &mut *self.region.layout().signal_taps.get() };
        payload.clear();
        Some(payload)
    }

    /// Complete a request, publishing only the telemetry supplied for this block.
    pub fn finish_with_telemetry(
        &mut self,
        mut result: BlockResult,
        meter: Option<crate::effect_meter::BlockMeter>,
        wrote_signal_taps: bool,
    ) {
        if !self.busy {
            return;
        }
        while self.pop_event().is_some() {}
        let header = &self.region.layout().header;
        result.reserved &= !(RESULT_EFFECT_METER | RESULT_SIGNAL_TAPS);
        if wrote_signal_taps {
            result.reserved |= RESULT_SIGNAL_TAPS;
        }
        if let Some(meter) = meter {
            result.reserved |= RESULT_EFFECT_METER;
            // SAFETY: The client can read this meter only after the completion below.
            unsafe { header.meter.get().write(meter) };
        }
        // SAFETY: the app reads `result` only after `done_seq == seq`, published below with
        // release ordering.
        unsafe {
            header.result.get().write(BlockResult {
                seq: self.seq,
                ..result
            });
        }
        header.done_seq.store(self.seq, Ordering::Release);
        self.region.wake(Word::Done);
        self.busy = false;
    }

    /// The region (for state and latency).
    pub fn region(&self) -> &Arc<ShmRegion> {
        &self.region
    }
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        self.region.release(ROLE_SERVER);
    }
}

/// The app's single consumer of the plugin's output events. Owned by the external processor.
#[derive(Debug)]
pub struct BridgeMonitor {
    region: Arc<ShmRegion>,
}

impl BridgeMonitor {
    /// The output-event consumer of `region`. Exactly one `BridgeMonitor` may exist per region.
    ///
    /// A second one panics while the first is alive.
    pub fn new(region: Arc<ShmRegion>) -> BridgeMonitor {
        region.claim(ROLE_MONITOR, "BridgeMonitor");
        BridgeMonitor { region }
    }

    /// The next plugin output event (MIDI or a parameter value/gesture), tagged by block sequence.
    pub fn pop_output_event(&mut self) -> Option<WireEvent> {
        // SAFETY: the `BridgeMonitor` role (one per region) is the only output-ring consumer.
        unsafe { self.region.layout().output_events.pop() }
    }

    /// The region (for state and latency).
    pub fn region(&self) -> &Arc<ShmRegion> {
        &self.region
    }
}

impl Drop for BridgeMonitor {
    fn drop(&mut self) {
        self.region.release(ROLE_MONITOR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_sizes() {
        assert_eq!(core::mem::size_of::<WireEvent>(), 32);
        assert_eq!(core::mem::size_of::<WireTransport>(), 56);
        assert_eq!(SHM_SIZE % 4096, 0);
        assert!(SHM_SIZE >= core::mem::size_of::<ShmLayout>());
        // The futex words must be naturally aligned for FUTEX_WAIT.
        assert_eq!(core::mem::offset_of!(ShmHeader, request_seq) % 4, 0);
        assert_eq!(core::mem::offset_of!(ShmHeader, done_seq) % 4, 0);
    }

    #[test]
    fn bridge_state_decoding() {
        for s in [
            BridgeState::Starting,
            BridgeState::Loaded,
            BridgeState::Active,
            BridgeState::Failed,
        ] {
            assert_eq!(BridgeState::from_u32(s as u32), s);
        }
        assert_eq!(BridgeState::from_u32(99), BridgeState::Failed);
    }

    #[test]
    fn magic_is_kpsm() {
        assert_eq!(&SHM_MAGIC.to_le_bytes(), b"KPSM");
    }

    use assert_no_alloc::assert_no_alloc;
    use std::time::Duration;

    #[cfg(unix)]
    fn dup(region: &ShmRegion) -> OwnedFd {
        region.fd().try_clone_to_owned().unwrap()
    }

    /// Windows: copies of the region's handles, as the bridge receives them.
    #[cfg(windows)]
    fn dup(region: &ShmRegion) -> ShmHandles {
        let [mapping, request, done] = region.handles().map(|h| h.try_clone_to_owned().unwrap());
        ShmHandles {
            mapping,
            request,
            done,
        }
    }

    /// Windows: a section of `len` bytes whose first words are `words`, with two fresh events.
    #[cfg(windows)]
    fn raw_section(len: usize, words: &[u32]) -> ShmHandles {
        let mapping = sys::create(len).unwrap();
        let (ptr, size) = sys::map(&mapping).unwrap();
        assert!(size >= words.len() * 4);
        // SAFETY: our own fresh view, long enough (checked above), unmapped right after.
        unsafe {
            let base = ptr.as_ptr().cast::<u32>();
            for (i, word) in words.iter().enumerate() {
                base.add(i).write(*word);
            }
            sys::unmap(ptr);
        }
        let event = || {
            signal::WakeEvent::new()
                .unwrap()
                .as_handle()
                .try_clone_to_owned()
        };
        ShmHandles {
            mapping,
            request: event().unwrap(),
            done: event().unwrap(),
        }
    }

    /// A memfd of `len` bytes whose first words are `words`.
    #[cfg(target_os = "linux")]
    fn raw_memfd(len: u64, words: &[u32]) -> OwnedFd {
        use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
        let fd = memfd_create("komponist-test", MemfdFlags::CLOEXEC).unwrap();
        ftruncate(&fd, len).unwrap();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
        rustix::io::pwrite(&fd, &bytes, 0).unwrap();
        fd
    }

    /// A POSIX shm object of `len` bytes whose first words are `words` (written through a
    /// mapping: shm objects support no `pwrite`).
    #[cfg(target_os = "macos")]
    fn raw_memfd(len: u64, words: &[u32]) -> OwnedFd {
        use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
        let fd = sys::create(len as usize).unwrap();
        if !words.is_empty() {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
            // SAFETY: a fresh mapping of the first bytes of the object, which is at least that
            // long; unmapped right after the copy.
            unsafe {
                let ptr = mmap(
                    core::ptr::null_mut(),
                    bytes.len(),
                    ProtFlags::READ | ProtFlags::WRITE,
                    MapFlags::SHARED,
                    &fd,
                    0,
                )
                .unwrap();
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
                munmap(ptr, bytes.len()).unwrap();
            }
        }
        fd
    }

    #[test]
    fn create_writes_the_header_and_open_maps_the_same_memory() {
        let region = ShmRegion::create().unwrap();
        let h = &region.layout().header;
        assert_eq!(
            (h.magic, h.version, h.size, h.max_block),
            (SHM_MAGIC, SHM_VERSION, SHM_SIZE as u32, MAX_BLOCK as u32)
        );
        assert_eq!(region.bridge_state(), BridgeState::Starting);
        let other = ShmRegion::open(dup(&region)).unwrap();
        other.set_latency(64);
        other.set_bridge_state(BridgeState::Active);
        assert_eq!(region.latency(), 64);
        assert_eq!(region.bridge_state(), BridgeState::Active);
        #[cfg(unix)]
        assert!(sys::size_matches(
            fstat(region.fd()).unwrap().st_size,
            SHM_SIZE
        ));
        #[cfg(windows)]
        assert_eq!(
            sys::map(&region.os.mapping).map(|(p, size)| {
                // SAFETY: the view just mapped, not used.
                unsafe { sys::unmap(p) };
                size
            }),
            Ok(SHM_SIZE)
        );
    }

    /// Sealed on Linux, set once for good on macOS: neither side can change the size. (A Windows
    /// section has no way to change its size at all.)
    #[test]
    #[cfg(unix)]
    fn the_size_cannot_change() {
        use rustix::fs::ftruncate;
        let region = ShmRegion::create().unwrap();
        let size = fstat(region.fd()).unwrap().st_size;
        assert!(ftruncate(region.fd(), 4096).is_err());
        assert!(ftruncate(region.fd(), 2 * SHM_SIZE as u64).is_err());
        assert!(ftruncate(region.fd(), 0).is_err());
        assert_eq!(fstat(region.fd()).unwrap().st_size, size);
        // The mapping is intact.
        region.set_latency(7);
        assert_eq!(ShmRegion::open(dup(&region)).unwrap().latency(), 7);
    }

    #[test]
    fn regions_are_distinct_and_close_on_exec() {
        let a = ShmRegion::create().unwrap();
        let b = ShmRegion::create().unwrap();
        a.set_latency(1);
        b.set_latency(2);
        assert_eq!((a.latency(), b.latency()), (1, 2));
        #[cfg(unix)]
        for region in [&a, &b] {
            let flags = rustix::io::fcntl_getfd(region.fd()).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        }
        // Windows: no handle of a region is inheritable (a spawned child gets none of them).
        #[cfg(windows)]
        for handle in a.handles().into_iter().chain(b.handles()) {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
            let mut flags = 0;
            // SAFETY: a valid handle and an out pointer.
            let ok = unsafe { GetHandleInformation(handle.as_raw_handle(), &mut flags) };
            assert_ne!(ok, 0);
            assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        }
    }

    #[test]
    #[cfg(windows)]
    fn open_validates_the_section() {
        let (s, m) = (SHM_SIZE as u32, MAX_BLOCK as u32);
        let open = |len, words: &[u32]| ShmRegion::open(raw_section(len, words)).map(|_| ());
        assert_eq!(open(SHM_SIZE, &[]), Err(ShmError::BadMagic));
        assert_eq!(
            open(SHM_SIZE, &[SHM_MAGIC, 7, s, m]),
            Err(ShmError::VersionMismatch { found: 7 })
        );
        for words in [
            [SHM_MAGIC, SHM_VERSION, s + 4096, m],
            [SHM_MAGIC, SHM_VERSION, s, 512],
        ] {
            assert_eq!(open(SHM_SIZE, &words), Err(ShmError::LayoutMismatch));
        }
        // Right header, section too small or too large.
        let words = [SHM_MAGIC, SHM_VERSION, s, m];
        assert_eq!(open(4096, &words), Err(ShmError::LayoutMismatch));
        assert_eq!(open(SHM_SIZE + 4096, &words), Err(ShmError::LayoutMismatch));
        assert_eq!(open(SHM_SIZE, &words), Ok(()));
        // Not a section at all (an event).
        let mut handles = raw_section(SHM_SIZE, &words);
        handles.mapping = signal::WakeEvent::new()
            .unwrap()
            .as_handle()
            .try_clone_to_owned()
            .unwrap();
        assert!(matches!(ShmRegion::open(handles), Err(ShmError::Io(_))));
    }

    #[test]
    #[cfg(unix)]
    fn open_validates_the_header() {
        let size = SHM_SIZE as u64;
        let (s, m) = (SHM_SIZE as u32, MAX_BLOCK as u32);
        assert_eq!(
            ShmRegion::open(raw_memfd(size, &[])).unwrap_err(),
            ShmError::BadMagic
        );
        assert_eq!(
            ShmRegion::open(raw_memfd(0, &[])).unwrap_err(),
            ShmError::BadMagic
        );
        assert_eq!(
            ShmRegion::open(raw_memfd(size, &[SHM_MAGIC, 7, s, m])).unwrap_err(),
            ShmError::VersionMismatch { found: 7 }
        );
        for words in [
            [SHM_MAGIC, SHM_VERSION, s + 4096, m],
            [SHM_MAGIC, SHM_VERSION, s, 512],
        ] {
            assert_eq!(
                ShmRegion::open(raw_memfd(size, &words)).unwrap_err(),
                ShmError::LayoutMismatch
            );
        }
        // Right header, file too small: mapping it would fault.
        let words = [SHM_MAGIC, SHM_VERSION, s, m];
        assert_eq!(
            ShmRegion::open(raw_memfd(4096, &words)).unwrap_err(),
            ShmError::LayoutMismatch
        );
        assert!(ShmRegion::open(raw_memfd(size, &words)).is_ok());
        // Not a file at all.
        let (a, _b) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::empty(),
            None,
        )
        .unwrap();
        assert!(matches!(ShmRegion::open(a).unwrap_err(), ShmError::Io(_)));
    }

    #[test]
    fn wire_events_round_trip() {
        let kinds = [
            WireEventKind::NoteOn {
                channel: 15,
                key: 127,
                velocity: 1,
                note_id: 42,
            },
            WireEventKind::NoteOff {
                channel: 0,
                key: 60,
                note_id: u32::MAX,
            },
            WireEventKind::Midi([0xB0, 64, 127]),
            WireEventKind::ParamValue {
                id: 0xDEAD_BEEF,
                value: -1.5e300,
            },
            WireEventKind::ParamGesture { id: 3, begin: true },
            WireEventKind::ParamGesture {
                id: 3,
                begin: false,
            },
            WireEventKind::AllNotesOff,
        ];
        for (i, kind) in kinds.into_iter().enumerate() {
            let event = WireEvent::new(i as u32 * 10, kind);
            assert_eq!(event.offset, i as u32 * 10);
            assert_eq!(event.reserved, 0);
            assert_eq!(event.decode(), Some(kind), "{event:?}");
        }
        let midi = WireEvent::new(0, WireEventKind::Midi([0x90, 1, 2]));
        assert_eq!(
            (midi.kind, midi.data, midi.note_id),
            (3, [0x90, 1, 2, 0], u32::MAX)
        );
        assert_eq!(WireEvent::default().decode(), None);
        let unknown = WireEvent {
            kind: 7,
            ..WireEvent::default()
        };
        assert_eq!(unknown.decode(), None);
    }

    /// A region with the bridge marked active, and its three role objects.
    fn active() -> (BridgeClient, BridgeServer, BridgeMonitor) {
        let region = Arc::new(ShmRegion::create().unwrap());
        region.set_bridge_state(BridgeState::Active);
        (
            BridgeClient::new(Arc::clone(&region)),
            BridgeServer::new(Arc::clone(&region)),
            BridgeMonitor::new(region),
        )
    }

    fn spec(frames: u32) -> BlockSpec {
        BlockSpec {
            frames,
            ..BlockSpec::default()
        }
    }

    fn param(offset: u32, id: u32, value: f64) -> WireEvent {
        WireEvent::new(offset, WireEventKind::ParamValue { id, value })
    }

    /// The block's deadline reaches the bridge in Mach ticks on macOS (for its work interval,
    /// #45); without a deadline, and on Linux, the field is 0.
    #[test]
    fn the_deadline_travels_with_the_request() {
        let (mut client, mut server, _monitor) = active();
        let input = [[0.0f32; 16]; 2];
        let deadline = Instant::now() + Duration::from_millis(3);
        for given in [Some(deadline), None] {
            let spec = BlockSpec {
                deadline: given,
                ..spec(16)
            };
            let seq =
                assert_no_alloc(|| client.begin_block(&spec, [&input[0], &input[1]], None, &[]))
                    .unwrap();
            let request = server.wait_request(Some(Instant::now())).unwrap();
            #[cfg(target_os = "macos")]
            match given {
                Some(_) => {
                    // Converted again here, a little later: equal up to rounding.
                    let ticks = komponist_rt::mach_time::deadline(deadline);
                    let slack = komponist_rt::mach_time::ticks(Duration::from_micros(10));
                    assert!(
                        request.deadline_ticks.abs_diff(ticks) <= slack,
                        "{} vs {ticks}",
                        request.deadline_ticks
                    );
                }
                None => assert_eq!(request.deadline_ticks, 0),
            }
            #[cfg(any(target_os = "linux", windows))]
            assert_eq!(request.deadline_ticks, 0);
            server.finish(BlockResult::default());
            let mut out = [[0f32; 16]; 2];
            let [l, r] = &mut out;
            client
                .wait_done(seq, Instant::now() + Duration::from_secs(1), [l, r])
                .unwrap();
        }
    }

    #[test]
    fn meter_data_belongs_only_to_its_completed_block() {
        use crate::effect_meter::BlockMeter;

        let (mut client, mut server, _) = active();
        let samples = [0.0; 8];
        let input = [&samples[..]; 2];
        let spec = BlockSpec {
            flags: BLOCK_FLAG_EFFECT_METER,
            ..spec(8)
        };
        let seq = client.begin_block(&spec, input, None, &[]).unwrap();
        assert_eq!(server.wait_request(Some(Instant::now())).unwrap().seq, seq);
        assert_eq!(client.effect_meter(seq), None);
        let meter = BlockMeter {
            reduction_db: 7.0,
            ..BlockMeter::EMPTY
        };
        assert_no_alloc(|| server.finish_with_meter(BlockResult::default(), Some(meter)));
        assert_eq!(assert_no_alloc(|| client.effect_meter(seq)), Some(meter));

        let next = client.begin_block(&spec, input, None, &[]).unwrap();
        assert_eq!(server.wait_request(Some(Instant::now())).unwrap().seq, next);
        assert_eq!(client.effect_meter(seq), None);
        assert_eq!(client.effect_meter(next), None);
        server.finish(BlockResult::default());
        assert_eq!(client.effect_meter(next), None);
    }

    #[test]
    fn signal_taps_are_only_visible_in_the_matching_completion() {
        let (mut client, mut server, _) = active();
        let samples = [0.0; 8];
        let input = [&samples[..]; 2];
        let requested = BlockSpec {
            flags: signal_tap_flags(0b0100),
            ..spec(8)
        };
        let seq = client.begin_block(&requested, input, None, &[]).unwrap();
        let request = server.wait_request(Some(Instant::now())).unwrap();
        assert_eq!(request.signal_taps(), 0b0100);
        assert!(client.signal_taps(seq).is_none());
        assert_no_alloc(|| {
            server.signal_taps_mut().unwrap().fill(2, |out| {
                out[..2].copy_from_slice(&[-2.0, -3.0]);
                2
            });
            server.finish_with_telemetry(BlockResult::default(), None, true);
            let taps = client.signal_taps(seq).unwrap();
            assert_eq!(taps.samples(2), [-2.0, -3.0]);
            assert!(taps.samples(0).is_empty());
        });
        let next = client.begin_block(&spec(8), input, None, &[]).unwrap();
        assert_eq!(
            server
                .wait_request(Some(Instant::now()))
                .unwrap()
                .signal_taps(),
            0
        );
        server.finish(BlockResult::default());
        assert!(client.signal_taps(seq).is_none());
        assert!(client.signal_taps(next).is_none());
    }

    #[test]
    fn one_block_in_process() {
        let (mut client, mut server, mut monitor) = active();
        let input = [[0.25f32; 64], [-0.5f32; 64]];
        let events = [param(0, 1, 0.5), param(7, 2, 0.75)];
        let spec = BlockSpec {
            frames: 64,
            flags: BLOCK_FLAG_RESET,
            steady_time: 1234,
            deadline: None,
            transport: WireTransport {
                flags: TRANSPORT_PLAYING,
                tempo_bpm: 120.0,
                ..WireTransport::default()
            },
        };
        assert!(client.is_idle());
        let seq =
            assert_no_alloc(|| client.begin_block(&spec, [&input[0], &input[1]], None, &events))
                .unwrap();
        assert_eq!(seq, 1);
        assert!(!client.is_idle());

        let request = assert_no_alloc(|| server.wait_request(Some(Instant::now()))).unwrap();
        assert_eq!(
            (
                request.seq,
                request.frames,
                request.flags,
                request.event_count
            ),
            (1, 64, BLOCK_FLAG_RESET, 2)
        );
        assert_eq!(request.steady_time, 1234);
        assert_eq!(request.transport, spec.transport);
        assert_eq!(server.pop_event(), Some(events[0]));
        assert_eq!(server.pop_event(), Some(events[1]));
        assert_eq!(server.pop_event(), None);
        assert_no_alloc(|| {
            let (inp, out) = server.audio(64);
            for c in 0..2 {
                for (o, i) in out[c].iter_mut().zip(inp[c]) {
                    *o = i * 2.0;
                }
            }
        });
        assert!(server.push_output_event(param(0, 9, 1.0)));
        assert_no_alloc(|| {
            server.finish(BlockResult {
                seq: 999,
                status: BlockStatus::Ok as u32,
                dropped_events: 0,
                reserved: 0,
            })
        });
        // Nothing open any more.
        assert_eq!(server.audio(64).0[0].len(), 0);

        let mut out = [[0f32; 64]; 2];
        let [l, r] = &mut out;
        let result = assert_no_alloc(|| client.wait_done(seq, Instant::now(), [l, r])).unwrap();
        assert_eq!(result.seq, 1);
        assert_eq!(result.status, BlockStatus::Ok as u32);
        assert_eq!(out, [[0.5f32; 64], [-1.0f32; 64]]);
        assert!(client.is_idle());
        assert_eq!(
            assert_no_alloc(|| monitor.pop_output_event()),
            Some(param(0, 9, 1.0))
        );
        assert_eq!(monitor.pop_output_event(), None);
        // Asking again for the same block returns the same result; a stale seq is late.
        let [l, r] = &mut out;
        assert!(client.wait_done(seq, Instant::now(), [l, r]).is_ok());
        let [l, r] = &mut out;
        assert_eq!(
            client.wait_done(seq.wrapping_sub(1), Instant::now(), [l, r]),
            Err(BlockError::Late)
        );
    }

    #[test]
    fn a_block_carries_its_sidechain_and_the_next_one_without_none() {
        let (mut client, mut server, _monitor) = active();
        let input = [[0.25f32; 32], [-0.5f32; 32]];
        let sidechain = [[0.75f32; 32], [-1.0f32; 32]];
        // A caller's stray flag does not count: only a given sidechain sets it.
        let stray = BlockSpec {
            frames: 32,
            flags: BLOCK_FLAG_SIDECHAIN | BLOCK_FLAG_RESET,
            ..BlockSpec::default()
        };
        let mut out = [[0f32; 32]; 2];
        for (spec, with) in [(spec(32), true), (stray, false), (spec(16), true)] {
            let sc = with.then_some([&sidechain[0][..], &sidechain[1][..]]);
            let seq =
                assert_no_alloc(|| client.begin_block(&spec, [&input[0], &input[1]], sc, &[]))
                    .unwrap();
            let request = server.wait_request(None).unwrap();
            assert_eq!(request.flags & BLOCK_FLAG_SIDECHAIN != 0, with);
            assert_eq!(
                request.flags & BLOCK_FLAG_RESET,
                spec.flags & BLOCK_FLAG_RESET
            );
            let frames = request.frames as usize;
            assert_no_alloc(|| {
                let (inp, sc, out) = server.audio_with_sidechain(frames);
                assert_eq!(inp[1], &input[1][..frames]);
                match sc {
                    Some(sc) => {
                        assert!(with);
                        assert_eq!(sc[0], &sidechain[0][..frames]);
                        assert_eq!(sc[1], &sidechain[1][..frames]);
                    }
                    None => assert!(!with),
                }
                for (o, s) in out.into_iter().zip(sc.unwrap_or(inp)) {
                    o.copy_from_slice(s);
                }
            });
            server.finish(BlockResult::default());
            // Nothing open any more: no sidechain either.
            assert!(server.audio_with_sidechain(frames).1.is_none());
            let [l, r] = &mut out;
            client.wait_done(seq, Instant::now(), [l, r]).unwrap();
            let expected = if with { sidechain[0][0] } else { input[0][0] };
            assert!(out[0][..frames].iter().all(|&s| s == expected));
        }
    }

    #[test]
    fn not_active_busy_and_too_many_events() {
        let (mut client, mut server, _monitor) = active();
        let input = [0f32; MAX_BLOCK];
        let both = [&input[..], &input[..]];
        client.region().set_bridge_state(BridgeState::Loaded);
        assert_eq!(
            client.begin_block(&spec(16), both, None, &[]),
            Err(BlockError::NotActive)
        );
        client.region().set_bridge_state(BridgeState::Active);

        let many = [param(0, 0, 0.0); INPUT_EVENT_CAPACITY + 1];
        assert_eq!(
            client.begin_block(&spec(16), both, None, &many),
            Err(BlockError::TooManyEvents)
        );
        let seq = client
            .begin_block(
                &spec(MAX_BLOCK as u32),
                both,
                None,
                &many[..INPUT_EVENT_CAPACITY],
            )
            .unwrap();
        assert_eq!(seq, 1, "refused blocks publish nothing");
        assert_eq!(
            client.begin_block(&spec(16), both, None, &[]),
            Err(BlockError::Busy)
        );

        // The server pops only part of the events; `finish` drops the rest.
        let request = server.wait_request(None).unwrap();
        assert_eq!(request.event_count, INPUT_EVENT_CAPACITY as u32);
        assert_eq!(request.frames, MAX_BLOCK as u32);
        assert!(server.pop_event().is_some());
        server.finish(BlockResult::default());
        assert!(client.region().layout().input_events.is_empty());

        let mut out = [[0f32; MAX_BLOCK]; 2];
        let [l, r] = &mut out;
        assert!(client.wait_done(seq, Instant::now(), [l, r]).is_ok());
        let seq = client
            .begin_block(&spec(16), both, None, &[param(3, 3, 3.0)])
            .unwrap();
        assert_eq!(seq, 2);
        let request = server.wait_request(None).unwrap();
        assert_eq!(request.event_count, 1);
        assert_eq!(server.pop_event(), Some(param(3, 3, 3.0)));
        assert_eq!(server.pop_event(), None);
    }

    #[test]
    fn late_block_then_resync() {
        let (mut client, mut server, _monitor) = active();
        let ones = [1f32; 32];
        let twos = [2f32; 32];
        let mut out = [[0f32; 32]; 2];

        let late = client
            .begin_block(&spec(32), [&ones, &ones], None, &[])
            .unwrap();
        let t0 = Instant::now();
        let [l, r] = &mut out;
        let outcome =
            assert_no_alloc(|| client.wait_done(late, t0 + Duration::from_millis(5), [l, r]));
        assert_eq!(outcome, Err(BlockError::Late));
        assert!(t0.elapsed() >= Duration::from_millis(5));
        assert_eq!(
            client.begin_block(&spec(32), [&twos, &twos], None, &[]),
            Err(BlockError::Busy)
        );

        // The bridge finishes the late block after all; its output is never read.
        let request = server.wait_request(None).unwrap();
        assert_eq!(request.seq, late);
        let (_, out_bridge) = server.audio(32);
        for ch in out_bridge {
            ch.fill(-1.0);
        }
        server.finish(BlockResult::default());

        let next = client
            .begin_block(&spec(32), [&twos, &twos], None, &[])
            .unwrap();
        assert_eq!(next, late + 1);
        let [l, r] = &mut out;
        // The stale result of `late` does not satisfy the wait for `next`.
        assert_eq!(
            client.wait_done(next, Instant::now() + Duration::from_millis(2), [l, r]),
            Err(BlockError::Late)
        );
        let request = server.wait_request(None).unwrap();
        assert_eq!(request.seq, next);
        let (inp, out_bridge) = server.audio(32);
        for c in 0..2 {
            out_bridge[c].copy_from_slice(inp[c]);
        }
        server.finish(BlockResult::default());
        let [l, r] = &mut out;
        let result = client.wait_done(next, Instant::now(), [l, r]).unwrap();
        assert_eq!(result.seq, next);
        assert_eq!(out, [[2f32; 32]; 2]);
    }

    #[test]
    fn server_serves_a_request_published_before_it_existed() {
        let region = Arc::new(ShmRegion::create().unwrap());
        region.set_bridge_state(BridgeState::Active);
        let mut client = BridgeClient::new(Arc::clone(&region));
        let input = [0f32; 8];
        let seq = client
            .begin_block(&spec(8), [&input, &input], None, &[])
            .unwrap();
        let mut server = BridgeServer::new(region);
        assert_eq!(
            server.wait_request(Some(Instant::now())).map(|r| r.seq),
            Some(seq)
        );
        server.finish(BlockResult::default());
        assert!(client.is_idle());
        // Nothing new: the wait times out.
        assert_eq!(server.wait_request(Some(Instant::now())), None);
    }

    #[test]
    fn threads_round_trip_without_allocation() {
        const BLOCKS: u32 = 2_000;
        let (mut client, mut server, mut monitor) = active();
        let bridge = std::thread::spawn(move || {
            assert_no_alloc(|| {
                let mut served = 0;
                while served < BLOCKS {
                    let Some(request) =
                        server.wait_request(Some(Instant::now() + Duration::from_secs(10)))
                    else {
                        break;
                    };
                    let mut sum = 0.0;
                    while let Some(e) = server.pop_event() {
                        sum += e.value;
                    }
                    let (inp, out) = server.audio(request.frames as usize);
                    for c in 0..2 {
                        for (o, i) in out[c].iter_mut().zip(inp[c]) {
                            *o = -i;
                        }
                    }
                    server.push_output_event(param(0, request.seq, sum));
                    server.finish(BlockResult::default());
                    served += 1;
                }
                served
            })
        });
        let mut input = [[0f32; 256]; 2];
        let mut out = [[0f32; 256]; 2];
        let mut bad = 0;
        let mut echoed = 0;
        for block in 0..BLOCKS {
            for (c, ch) in input.iter_mut().enumerate() {
                for (i, s) in ch.iter_mut().enumerate() {
                    *s = (block * 1000 + i as u32 * 2 + c as u32) as f32;
                }
            }
            let frames = 1 + block % 256;
            let events = [param(0, 0, block as f64), param(1, 0, 0.5)];
            let [l, r] = &mut out;
            let result = assert_no_alloc(|| {
                let seq =
                    client.begin_block(&spec(frames), [&input[0], &input[1]], None, &events)?;
                client.wait_done(seq, Instant::now() + Duration::from_secs(10), [l, r])
            });
            bad += usize::from(result.map(|r| r.seq) != Ok(block + 1));
            for c in 0..2 {
                let n = frames as usize;
                bad += out[c][..n]
                    .iter()
                    .zip(&input[c][..n])
                    .filter(|(o, i)| **o != -**i)
                    .count();
            }
            while let Some(e) = assert_no_alloc(|| monitor.pop_output_event()) {
                bad += usize::from(e.param_id != echoed + 1 || e.value != echoed as f64 + 0.5);
                echoed += 1;
            }
        }
        assert_eq!(bridge.join().unwrap(), BLOCKS);
        assert_eq!(bad, 0);
        assert_eq!(echoed, BLOCKS);
    }

    #[test]
    #[should_panic(expected = "a second BridgeClient")]
    fn a_second_client_panics() {
        let region = Arc::new(ShmRegion::create().unwrap());
        let _first = BridgeClient::new(Arc::clone(&region));
        let _second = BridgeClient::new(region);
    }

    #[test]
    fn roles_are_released_on_drop() {
        let region = Arc::new(ShmRegion::create().unwrap());
        drop(BridgeServer::new(Arc::clone(&region)));
        drop(BridgeMonitor::new(Arc::clone(&region)));
        let _server = BridgeServer::new(Arc::clone(&region));
        let _monitor = BridgeMonitor::new(region);
    }
}
