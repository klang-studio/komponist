//! Project → [`Program`] compiler (ARCHITECTURE §6.3, §6.5).
//!
//! Runs on the UI thread after each document change (`EngineSync` in the app). Incremental: a
//! track whose `Arc<Track>` is `ptr_eq` to the previous program's
//! [`TrackProgram::source`](crate::program::TrackProgram::source) (and whose
//! environment-derived values did not change) reuses the previous `Arc<TrackProgram>`.
//!
//! Sequence per sync (the app's `EngineSync`):
//! 1. `let next = build(&project, previous.as_ref(), &env);`
//! 2. `let changes = slot_changes(previous.as_ref(), &next);`
//! 3. For each `changes.added`: create the processor (builtin in process, external through
//!    `komponist-plugins`), activate it at the engine's rate, send `Command::InsertProcessor`.
//! 4. Send `Command::SetProgram(Box::new(next.clone()))`, keep `next` as `previous`.
//! 5. For each `changes.removed`: send `Command::RemoveProcessor`.
//!
//! Commands apply in order, so a slot is filled before a program uses it and emptied only after
//! the program that used it is gone.
//!
//! What the compiler does (golden-tested by WP-24):
//! - MIDI clips: the active take; clip loops expanded (`loop_length`), notes cut at repetition and
//!   clip ends; clip transpose and velocity applied; muted clips skipped.
//! - Step clips: the pattern repeated from the clip start to the clip end; swing on odd steps;
//!   ratchets expanded into evenly spaced hits (later hits at 75 % velocity,
//!   [`crate::program::PROBABILITY_FOLLOW`]); probability kept per note; muted rows and, with any
//!   row soloed, non-soloed rows skipped.
//! - Devices → [`ChainSlot`](crate::program::ChainSlot)s with stable slots; volume dB → linear
//!   gain; MIDI input names → [`MidiPort`]s; chain latencies → `pdc_delay`.

use komponist_model::{DeviceId, Project};

use crate::program::{MidiPort, Program, SlotId};

/// What the compiler needs from outside the document. Implemented by the app (from
/// `komponist-audio`'s MIDI ports and `komponist-plugins`' latencies); [`NoEnv`] for tests.
pub trait CompileEnv {
    /// The port id of the MIDI input named `device_name`, or `None` if it is not present.
    fn midi_port(&self, device_name: &str) -> Option<MidiPort>;

    /// Latency of `device`'s processor in frames at the engine's sample rate (0 if unknown or
    /// builtin without latency).
    fn device_latency(&self, device: DeviceId) -> u32;
}

/// An environment without MIDI devices and with zero latencies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoEnv;

impl CompileEnv for NoEnv {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, _device: DeviceId) -> u32 {
        0
    }
}

/// Compiles `project` into a program, reusing unchanged track programs and the slot assignment of
/// `previous`. Pure apart from allocation; never fails (invalid data is skipped, e.g. notes outside
/// `0 ..= 127` after transposition). Tracks beyond [`crate::MAX_TRACKS`] are left out.
pub fn build(project: &Project, previous: Option<&Program>, env: &dyn CompileEnv) -> Program {
    todo!("WP-24")
}

/// Devices whose slot appeared or disappeared between two programs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotChanges {
    /// Devices new in `next`, with their slot: create and insert a processor.
    pub added: Vec<(DeviceId, SlotId)>,
    /// Devices gone in `next`, with the slot they had: remove the processor.
    pub removed: Vec<(DeviceId, SlotId)>,
}

/// Compares the slot maps of `previous` (all devices are new when `None`) and `next`.
pub fn slot_changes(previous: Option<&Program>, next: &Program) -> SlotChanges {
    todo!("WP-24")
}
