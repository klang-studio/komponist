//! How the UI thread talks to the engine (ARCHITECTURE §6.3).
//!
//! - **Commands** (UI → engine): an rtrb SPSC queue of [`Command`], drained by the engine at the
//!   start of each block (at most [`crate::MAX_COMMANDS_PER_BLOCK`] per block; the rest waits).
//! - **Events** (engine → UI): an rtrb SPSC queue of [`EngineEvent`], including [`Garbage`] the
//!   engine retires. The UI drains it every frame with [`EngineHandle::poll`]; dropping a polled
//!   event drops its garbage on the UI thread.
//! - **Telemetry** (engine → UI): atomics in [`Telemetry`] and per-track [`TrackMeter`]s, read every
//!   UI frame. CPU load and xruns are measured by `komponist-audio` (`StreamStats`), not here.

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, Ordering};

use komponist_model::{Tick, TrackId};

use crate::processor::Processor;
use crate::program::{Program, SlotId};

/// A request from the UI to the engine. Applied in send order at the start of a block.
pub enum Command {
    /// Start playback from the current position. Ignored while playing. Notes that started
    /// earlier and still sound at the position start at once (note chasing).
    Play,
    /// Stop playback and recording. Sounding notes get note-offs; processors are reset. The
    /// position stays (the second Stop's jump to the loop start or 0 is a `Locate` from the UI).
    Stop,
    /// Move the playhead to `Tick` (≥ 0). While playing: note-offs for sounding notes, playback
    /// continues from there and chases the notes sounding at `Tick`.
    Locate(Tick),
    /// Start recording on all armed tracks. Stopped: locate to `from − count_in_bars × bar`, play,
    /// and start capturing at `from` (state `CountIn` until then, then `EngineEvent::CountInDone`).
    /// Playing: capture starts at once (`CountInDone` is sent immediately) and `from` is ignored.
    Record {
        /// Where capture starts when stopped (the UI uses the bar of the playhead).
        from: Tick,
    },
    /// End capture; playback continues (Esc discards the take in the UI).
    StopRecording,
    /// Install a new program. The old one comes back as `Garbage::Program`. Sounding notes keep
    /// their scheduled note-offs (active-notes table), so a swap never cuts or hangs a note.
    SetProgram(Box<Program>),
    /// Put a processor (already activated at the engine's sample rate) into `slot`. An occupied
    /// slot's processor is replaced and comes back as `Garbage::Processor`.
    InsertProcessor {
        /// Target slot.
        slot: SlotId,
        /// The processor.
        processor: Box<dyn Processor>,
    },
    /// Take the processor out of `slot`; it comes back as `Garbage::Processor`.
    RemoveProcessor {
        /// The slot to empty.
        slot: SlotId,
    },
    /// Deliver `EventKind::Param { id, value }` to the processor in `slot` at offset 0 of the slot's
    /// next processor call (pisynth macros; external plugins get parameters through
    /// `komponist-plugins`). Values wait per slot, coalesced by id, so a bypassed device keeps them.
    SetParam {
        /// Target slot.
        slot: SlotId,
        /// Parameter id.
        id: u32,
        /// Plain value.
        value: f64,
    },
    /// Fast path during a fader gesture: linear gain of a track (ramped), until the next program.
    SetTrackGain {
        /// The track.
        track: TrackId,
        /// Linear gain ≥ 0.
        gain: f32,
    },
    /// Fast path: balance of a track, `-1.0 ..= 1.0`, until the next program.
    SetTrackPan {
        /// The track.
        track: TrackId,
        /// Balance.
        pan: f32,
    },
    /// Fast path: mute of a track, until the next program.
    SetTrackMute {
        /// The track.
        track: TrackId,
        /// Mute.
        mute: bool,
    },
    /// Fast path: solo of a track, until the next program.
    SetTrackSolo {
        /// The track.
        track: TrackId,
        /// Solo.
        solo: bool,
    },
    /// Fast path: master fader, linear gain ≥ 0, until the next program.
    SetMasterGain(f32),
    /// A note from the computer keyboard or a step-row audition, played on `track`'s chain at
    /// offset 0 of the next block (and captured if the track is armed and recording).
    LiveNote {
        /// Target track.
        track: TrackId,
        /// MIDI key.
        key: u8,
        /// MIDI velocity; 0 = note off.
        velocity: u8,
    },
    /// Panic: note-off for every sounding note and `AllNotesOff` to every processor.
    AllNotesOff,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Play => f.write_str("Play"),
            Command::Stop => f.write_str("Stop"),
            Command::Locate(t) => f.debug_tuple("Locate").field(t).finish(),
            Command::Record { from } => f.debug_struct("Record").field("from", from).finish(),
            Command::StopRecording => f.write_str("StopRecording"),
            Command::SetProgram(_) => f.write_str("SetProgram(..)"),
            Command::InsertProcessor { slot, .. } => f
                .debug_struct("InsertProcessor")
                .field("slot", slot)
                .finish_non_exhaustive(),
            Command::RemoveProcessor { slot } => f
                .debug_struct("RemoveProcessor")
                .field("slot", slot)
                .finish(),
            Command::SetParam { slot, id, value } => f
                .debug_struct("SetParam")
                .field("slot", slot)
                .field("id", id)
                .field("value", value)
                .finish(),
            Command::SetTrackGain { track, gain } => f
                .debug_struct("SetTrackGain")
                .field("track", track)
                .field("gain", gain)
                .finish(),
            Command::SetTrackPan { track, pan } => f
                .debug_struct("SetTrackPan")
                .field("track", track)
                .field("pan", pan)
                .finish(),
            Command::SetTrackMute { track, mute } => f
                .debug_struct("SetTrackMute")
                .field("track", track)
                .field("mute", mute)
                .finish(),
            Command::SetTrackSolo { track, solo } => f
                .debug_struct("SetTrackSolo")
                .field("track", track)
                .field("solo", solo)
                .finish(),
            Command::SetMasterGain(g) => f.debug_tuple("SetMasterGain").field(g).finish(),
            Command::LiveNote {
                track,
                key,
                velocity,
            } => f
                .debug_struct("LiveNote")
                .field("track", track)
                .field("key", key)
                .field("velocity", velocity)
                .finish(),
            Command::AllNotesOff => f.write_str("AllNotesOff"),
        }
    }
}

/// A message from the engine to the UI.
#[derive(Debug)]
pub enum EngineEvent {
    /// A captured note start on an armed track while recording. `tick` is the song position of the
    /// input, corrected by the output latency (ARCHITECTURE §6.5), floored.
    RecordedNoteOn {
        /// Armed track that captured it.
        track: TrackId,
        /// Song position.
        tick: Tick,
        /// MIDI key.
        key: u8,
        /// MIDI velocity 1 ..= 127.
        velocity: u8,
    },
    /// A captured note end; pairs with the latest open `RecordedNoteOn` of the same track and key.
    /// Notes still held at `StopRecording` / `Stop` / `Locate` get a note-off at that position, and
    /// notes held across a loop wrap get one at the loop end, so every lap is a self-contained take.
    RecordedNoteOff {
        /// Armed track.
        track: TrackId,
        /// Song position.
        tick: Tick,
        /// MIDI key.
        key: u8,
    },
    /// Playback jumped from the loop end back to the loop start. During loop recording each wrap
    /// starts a new take (S02).
    LoopWrapped {
        /// The loop start the playhead jumped to.
        to: Tick,
    },
    /// The count-in ended and capture started at `at`.
    CountInDone {
        /// First captured tick.
        at: Tick,
    },
    /// A processor returned `ProcessStatus::Failed`; sent once per slot until it is replaced.
    ProcessorFailed {
        /// The failing slot.
        slot: SlotId,
    },
    /// Something the engine no longer needs; drop it on the UI thread.
    Retired(Garbage),
}

/// Objects the engine hands back for dropping on the UI thread (the audio thread never frees).
///
/// When the event queue is full, the engine parks retired objects in a preallocated holding list
/// and sends them in later blocks; it never drops them itself.
pub enum Garbage {
    /// A replaced program.
    Program(Box<Program>),
    /// A removed or replaced processor, with the slot it left.
    Processor(SlotId, Box<dyn Processor>),
}

impl fmt::Debug for Garbage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Garbage::Program(_) => f.write_str("Program(..)"),
            Garbage::Processor(slot, _) => f.debug_tuple("Processor").field(slot).finish(),
        }
    }
}

/// Transport state as seen by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TransportState {
    /// Not running.
    Stopped = 0,
    /// Running before the capture start of a recording ("Vorzähler").
    CountIn = 1,
    /// Playing, not capturing.
    Playing = 2,
    /// Playing and capturing.
    Recording = 3,
}

/// Engine state the UI reads every frame. Written only by the engine (relaxed atomics; values are
/// independent, a frame may see a mix of two blocks).
#[derive(Debug)]
pub struct Telemetry {
    /// Playhead in ticks (floored), negative during the count-in.
    pub(crate) playhead: AtomicI64,
    /// [`TransportState`] as `u8`.
    pub(crate) state: AtomicU8,
    /// Master meter after the master fader (the metronome is not metered).
    pub(crate) master: TrackMeter,
    /// Events dropped because a queue or a per-block event buffer was full (diagnostics).
    pub(crate) dropped_events: AtomicU32,
}

impl Telemetry {
    /// Stopped at tick 0, meters at 0.
    pub fn new() -> Telemetry {
        Telemetry {
            playhead: AtomicI64::new(0),
            state: AtomicU8::new(TransportState::Stopped as u8),
            master: TrackMeter::new(),
            dropped_events: AtomicU32::new(0),
        }
    }

    /// Current playhead.
    pub fn playhead(&self) -> Tick {
        Tick(self.playhead.load(Ordering::Relaxed))
    }

    /// Current transport state.
    pub fn transport(&self) -> TransportState {
        match self.state.load(Ordering::Relaxed) {
            1 => TransportState::CountIn,
            2 => TransportState::Playing,
            3 => TransportState::Recording,
            _ => TransportState::Stopped,
        }
    }

    /// The master meter.
    pub fn master_meter(&self) -> &TrackMeter {
        &self.master
    }

    /// Total number of dropped events since the engine started.
    pub fn dropped_events(&self) -> u32 {
        self.dropped_events.load(Ordering::Relaxed)
    }
}

impl Default for Telemetry {
    fn default() -> Telemetry {
        Telemetry::new()
    }
}

/// Peak levels of one strip since the UI last read them.
///
/// The engine raises the stored peaks every block ([`TrackMeter::record`], RT-safe); the UI takes
/// and resets them every frame ([`TrackMeter::take`]) and feeds them into
/// `klang_dsp::meter::PeakMeter` for ballistics (hold 1.5 s, 20 dB/s, clip latch). Peaks are linear
/// sample magnitudes (1.0 = 0 dBFS), stored as `f32` bits.
#[derive(Debug, Default)]
pub struct TrackMeter {
    peak_l: AtomicU32,
    peak_r: AtomicU32,
}

impl TrackMeter {
    /// Peaks at 0.
    pub const fn new() -> TrackMeter {
        TrackMeter {
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
        }
    }

    /// RT. Raises the stored peaks to at least `l` and `r` (absolute block peaks, ≥ 0, finite).
    /// Uses `fetch_max` on the bit patterns, which orders non-negative floats correctly.
    pub fn record(&self, l: f32, r: f32) {
        self.peak_l.fetch_max(l.to_bits(), Ordering::Relaxed);
        self.peak_r.fetch_max(r.to_bits(), Ordering::Relaxed);
    }

    /// UI. Returns the peaks since the last call and resets them to 0.
    pub fn take(&self) -> (f32, f32) {
        (
            f32::from_bits(self.peak_l.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.peak_r.swap(0, Ordering::Relaxed)),
        )
    }
}

/// The UI's end of the engine queues. Created with the engine by [`crate::Engine::new`]; lives on
/// the UI thread (`Send`, not shared).
pub struct EngineHandle {
    pub(crate) commands: rtrb::Producer<Command>,
    pub(crate) events: rtrb::Consumer<EngineEvent>,
    pub(crate) telemetry: Arc<Telemetry>,
}

impl EngineHandle {
    /// Queues a command. Returns it back if the queue ([`crate::COMMAND_CAPACITY`]) is full; the
    /// caller retries next frame.
    pub fn send(&mut self, command: Command) -> Result<(), Command> {
        self.commands
            .push(command)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// The next event from the engine, if any. Call until `None` every UI frame.
    pub fn poll(&mut self) -> Option<EngineEvent> {
        self.events.pop().ok()
    }

    /// Shared telemetry.
    pub fn telemetry(&self) -> &Arc<Telemetry> {
        &self.telemetry
    }
}

impl fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineHandle").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_meter_keeps_the_maximum_until_taken() {
        let m = TrackMeter::new();
        m.record(0.25, 0.5);
        m.record(0.75, 0.125);
        assert_eq!(m.take(), (0.75, 0.5));
        assert_eq!(m.take(), (0.0, 0.0));
    }

    #[test]
    fn telemetry_starts_stopped_at_zero() {
        let t = Telemetry::new();
        assert_eq!(t.transport(), TransportState::Stopped);
        assert_eq!(t.playhead(), Tick::ZERO);
        assert_eq!(t.master_meter().take(), (0.0, 0.0));
    }
}
