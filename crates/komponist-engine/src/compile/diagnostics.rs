//! What [`build_with_diagnostics`](super::build_with_diagnostics) reports next to the program:
//! each track or device of the project that the program leaves out, and why (#92).
//!
//! A diagnostic names its object by position *and* id, so a duplicate id (whose second
//! occurrence is the one left out) still points at one object. The list is in project order:
//! tracks by index, each track's devices by chain index right after the track, the master's
//! devices last. It depends only on the project and the slot map of the previous program, so the
//! same inputs give the same list.

use komponist_model::{DeviceId, DeviceRole, TrackId};

/// One track or device the program leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diagnostic {
    /// What is left out.
    pub subject: Subject,
    /// Why.
    pub reason: Reason,
}

/// The object a [`Diagnostic`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// A track: `index` in `Project::tracks`.
    Track {
        /// The track's id.
        track: TrackId,
        /// Its index in `Project::tracks`.
        index: usize,
    },
    /// A device: `index` in its chain's device list.
    Device {
        /// The chain the device is in.
        chain: Chain,
        /// The device's id.
        device: DeviceId,
        /// Its index in the chain's device list.
        index: usize,
        /// Its role.
        role: DeviceRole,
    },
}

/// A device chain of the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    /// The chain of the track at `index` in `Project::tracks`.
    Track {
        /// The track's id.
        track: TrackId,
        /// Its index in `Project::tracks`.
        index: usize,
    },
    /// The master inserts.
    Master,
}

/// Why an object is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The track comes after the first [`MAX_TRACKS`](crate::MAX_TRACKS) tracks.
    TrackLimit,
    /// Another track or device with the same id comes earlier in the project (the master comes
    /// last).
    DuplicateId,
    /// A MIDI effect at the head of a track chain after the first
    /// [`MAX_STAGES`](crate::MAX_STAGES).
    MidiEffectLimit,
    /// A MIDI effect that is not at the head of the chain, or an instrument that is not right
    /// behind the MIDI effects.
    Misplaced,
    /// An instrument or MIDI effect on a chain that runs audio effects only: an audio track, a
    /// bus or the master.
    WrongChain,
    /// No free processor slot below [`MAX_SLOTS`](crate::MAX_SLOTS) is left.
    SlotLimit,
}

impl Reason {
    /// Whether the reason is a limit of the engine (more tracks, MIDI effects or devices than it
    /// holds) rather than invalid project data.
    pub fn is_limit(self) -> bool {
        matches!(
            self,
            Reason::TrackLimit | Reason::MidiEffectLimit | Reason::SlotLimit
        )
    }
}

impl Diagnostic {
    /// Sort key for project order: the track index (master last), then the track itself before
    /// its devices, then the chain index.
    pub(super) fn order(&self) -> (usize, usize) {
        match self.subject {
            Subject::Track { index, .. } => (index, 0),
            Subject::Device { chain, index, .. } => {
                let track = match chain {
                    Chain::Track { index, .. } => index,
                    Chain::Master => usize::MAX,
                };
                (track, index + 1)
            }
        }
    }
}
