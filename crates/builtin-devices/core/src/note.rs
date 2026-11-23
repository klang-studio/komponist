//! The one translation from note and MIDI events to [`Dsp`] calls, shared by every host of a
//! built-in device: the in-app [`crate::InternalProcessor`], the bundled CLAP adapter's
//! instrument and the sampler CLAP (#223). A host maps its own events to [`DeviceNote`] and calls
//! [`dispatch_note`]; it never calls the note methods of [`Dsp`] itself.

use crate::Dsp;

/// A note or channel message for an instrument, independent of the host's event format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceNote {
    /// A note starts: `key` 0 ..= 127, `velocity` 1 ..= 127.
    On {
        /// MIDI key.
        key: u8,
        /// MIDI velocity.
        velocity: u8,
        /// Host-assigned id of the note, if any.
        note_id: Option<u32>,
        /// MIDI channel, if the host has one.
        channel: Option<u8>,
    },
    /// The note with this key (and id and channel, if given) ends: [`Dsp::note_off_with_id`].
    Off {
        /// MIDI key.
        key: u8,
        /// Host-assigned id of the note, if any.
        note_id: Option<u32>,
        /// MIDI channel, if the host has one.
        channel: Option<u8>,
    },
    /// CLAP note-off with an address that may be partly wildcard: [`Dsp::release_note_gates`].
    ReleaseGates {
        /// Key, `None` for any.
        key: Option<u8>,
        /// Note id, `None` for any.
        note_id: Option<u32>,
        /// Channel, `None` for any.
        channel: Option<u8>,
    },
    /// CLAP choke at an address that may be partly wildcard: [`Dsp::choke_note`].
    Choke {
        /// Key, `None` for any.
        key: Option<u8>,
        /// Note id, `None` for any.
        note_id: Option<u32>,
        /// Channel, `None` for any.
        channel: Option<u8>,
    },
    /// Every sounding note ends now: [`Dsp::all_notes_off`].
    AllNotesOff,
    /// A channel message as raw MIDI bytes. See [`dispatch_note`] for what is handled.
    Midi([u8; 3]),
}

/// The 14-bit pitch bend value `0 ..= 16383` as the status and data bytes of a MIDI message on
/// `channel`.
pub fn pitch_bend_bytes(channel: u8, value: u16) -> [u8; 3] {
    [
        0xE0 | (channel & 0x0F),
        (value & 0x7F) as u8,
        ((value >> 7) & 0x7F) as u8,
    ]
}

/// Hands a note or channel message to an instrument. This is the single definition of what a
/// built-in device does with it, whatever the host:
///
/// - Notes go to [`Dsp::note_on_with_id`], [`Dsp::note_off_with_id`], [`Dsp::release_note_gates`]
///   and [`Dsp::choke_note`] as they are.
/// - Raw MIDI: pitch bend (14 bit, −1 … +1), the mod wheel (CC 1), channel pressure, and
///   **All Sound Off (CC 120) and All Notes Off (CC 123), which both call
///   [`Dsp::all_notes_off`]**: the device decides how it ends the notes (a release or a fast
///   fade), the host never chokes per key or per channel. The channel does not matter. Other
///   controllers and polyphonic aftertouch are ignored (WP-95).
///
/// RT-safe: no allocation, no locks.
pub fn dispatch_note(dsp: &mut dyn Dsp, note: DeviceNote) {
    match note {
        DeviceNote::On {
            key,
            velocity,
            note_id,
            channel,
        } => dsp.note_on_with_id(key, velocity, note_id, channel),
        DeviceNote::Off {
            key,
            note_id,
            channel,
        } => dsp.note_off_with_id(key, note_id, channel),
        DeviceNote::ReleaseGates {
            key,
            note_id,
            channel,
        } => dsp.release_note_gates(key, note_id, channel),
        DeviceNote::Choke {
            key,
            note_id,
            channel,
        } => dsp.choke_note(key, note_id, channel),
        DeviceNote::AllNotesOff => dsp.all_notes_off(),
        DeviceNote::Midi([status, d1, d2]) => match status & 0xF0 {
            0xE0 => {
                let value = (i32::from(d2 & 0x7F) << 7 | i32::from(d1 & 0x7F)) - 8192;
                dsp.pitch_bend((value as f32 / 8192.0).clamp(-1.0, 1.0));
            }
            0xB0 => match d1 {
                1 => dsp.mod_wheel(f32::from(d2 & 0x7F) / 127.0),
                120 | 123 => dsp.all_notes_off(),
                _ => {}
            },
            0xD0 => dsp.pressure(f32::from(d1 & 0x7F) / 127.0),
            _ => {}
        },
    }
}
