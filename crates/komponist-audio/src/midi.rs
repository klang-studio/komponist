//! MIDI inputs through midir (ALSA sequencer; JACK where enabled) with per-device enable, activity
//! flags and timestamps (ARCHITECTURE §7, S07 "MIDI-Eingänge").
//!
//! midir callbacks run on their own threads. They stamp each message with `Instant::now()` and push
//! it into a lock-free queue that the audio callback drains through [`MidiReceiver`] (via
//! [`crate::MidiIn`]). Only channel messages (note on/off, CC, pitch bend, aftertouch, program
//! change) are forwarded; SysEx and system real-time messages are dropped.

use std::time::Instant;

/// Capacity of the queue between the MIDI threads and the audio callback, in messages. Messages
/// beyond it are dropped.
pub const MIDI_QUEUE_CAPACITY: usize = 4096;

/// Id of a MIDI input port, stable per device name for the lifetime of the hub (a re-plugged device
/// gets its old id back). The app maps it 1:1 onto `komponist_engine::MidiPort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MidiPortId(pub u16);

/// A MIDI message with its arrival time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiStamped {
    /// `Instant::now()` when the message reached komponist.
    pub time: Instant,
    /// The port it came from.
    pub port: MidiPortId,
    /// Channel message bytes (status, data1, data2); unused bytes are 0.
    pub data: [u8; 3],
}

/// A MIDI input as listed in the settings (S07).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MidiPortInfo {
    /// Stable id.
    pub id: MidiPortId,
    /// Device name as reported by the backend ("Arturia KeyStep 37").
    pub name: String,
    /// Whether messages from it are forwarded (switch in S07).
    pub enabled: bool,
    /// Whether the device is currently present.
    pub connected: bool,
}

/// Why a MIDI operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    /// The MIDI backend could not be opened or failed.
    Backend(String),
    /// No port with this name.
    UnknownPort(String),
}

impl core::fmt::Display for MidiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MidiError::Backend(m) => write!(f, "MIDI backend error: {m}"),
            MidiError::UnknownPort(n) => write!(f, "unknown MIDI port {n}"),
        }
    }
}

impl std::error::Error for MidiError {}

/// The audio callback's end of the MIDI queue. Moves into the stream together with the engine.
#[derive(Debug)]
pub struct MidiReceiver {
    _private: (),
}

/// Owner of the MIDI input connections. UI thread.
#[derive(Debug)]
pub struct MidiInputHub {
    _private: (),
}

impl MidiInputHub {
    /// Opens the MIDI backend (no ports connected yet) and returns the hub and the receiver for the
    /// audio callback. Call [`MidiInputHub::refresh`] afterwards.
    pub fn new() -> Result<(MidiInputHub, MidiReceiver), MidiError> {
        todo!("WP-13")
    }

    /// Re-enumerates the input ports, connects enabled ones that appeared and marks vanished ones
    /// as not connected. New ports start enabled, except names starting with "Midi Through".
    pub fn refresh(&mut self) -> Result<(), MidiError> {
        todo!("WP-13")
    }

    /// All ports seen since the hub was created, in enumeration order.
    pub fn ports(&self) -> Vec<MidiPortInfo> {
        todo!("WP-13")
    }

    /// The id of the port named `name`, if seen.
    pub fn port_id(&self, name: &str) -> Option<MidiPortId> {
        todo!("WP-13")
    }

    /// Enables or disables forwarding from the port named `name` (connects or disconnects it).
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), MidiError> {
        todo!("WP-13")
    }

    /// Whether a message arrived on `port` since the last call (the activity dot in S07 and the
    /// status bar). Resets the flag.
    pub fn take_activity(&self, port: MidiPortId) -> bool {
        todo!("WP-13")
    }
}
