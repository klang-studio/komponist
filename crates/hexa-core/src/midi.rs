//! MIDI 1.0 channel messages.
//!
//! A message starts with a *status byte* (top bit set) followed by one or two
//! *data bytes* (top bit clear, so 7-bit values 0..=127):
//!
//! ```text
//! status byte   1kkk cccc   kkk = message kind, cccc = channel 0..=15
//! data byte     0vvv vvvv   vvvvvvv = value 0..=127
//! ```
//!
//! Example: `90 3C 64` is Note On, channel 0, note 60 (middle C), velocity 100.

/// Set in status bytes, clear in data bytes.
pub const STATUS_BIT: u8 = 0x80;
/// Lower nibble of a status byte: the channel.
pub const CHANNEL_MASK: u8 = 0x0f;
/// Largest value of a 7-bit data byte.
pub const DATA_MAX: u8 = 0x7f;
/// Control change number of the sustain (damper) pedal; values >= 64 mean down.
pub const CC_SUSTAIN: u8 = 64;
/// Control change numbers of the channel mode messages handled: All Sound
/// Off silences at once, All Notes Off releases (and so do 124..=127, the
/// omni and mono/poly mode changes, by the MIDI specification).
pub const CC_ALL_SOUND_OFF: u8 = 120;
pub const CC_ALL_NOTES_OFF: u8 = 123;
/// Control values from this up mean a switch (pedal) is down.
pub const SWITCH_ON: u8 = 64;
/// Pitch bend value with the wheel at rest (14-bit range 0..=16383).
pub const PITCH_BEND_CENTER: u16 = 0x2000;

/// A decoded channel message. `channel` is 0..=15 (shown as 1..=16 on most
/// devices); all other fields are 0..=127 unless noted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    /// Key released. `velocity` is the release speed; most keyboards send 0 or 64.
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    /// Key pressed with `velocity` 1..=127.
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    /// Aftertouch of a single held key.
    PolyPressure {
        channel: u8,
        note: u8,
        pressure: u8,
    },
    /// Knob, pedal or switch, e.g. controller 64 = sustain pedal.
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    /// Aftertouch of the whole channel.
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    /// Pitch wheel, -8192..=8191 with 0 at rest.
    PitchBend {
        channel: u8,
        value: i16,
    },
}

impl MidiMessage {
    /// Decodes one complete message. Returns `None` for system messages,
    /// running status (missing status byte), truncated messages and invalid
    /// data bytes.
    ///
    /// Note On with velocity 0 is decoded as Note Off: devices use it as a
    /// shorter way to release keys.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let (&status, data) = bytes.split_first()?;
        if status & STATUS_BIT == 0 {
            return None;
        }
        let channel = status & CHANNEL_MASK;
        let data_byte = |i: usize| data.get(i).copied().filter(|&b| b & STATUS_BIT == 0);

        // The upper nibble of the status byte selects the message kind.
        let message = match status >> 4 {
            0x8 => MidiMessage::NoteOff {
                channel,
                note: data_byte(0)?,
                velocity: data_byte(1)?,
            },
            0x9 => match (data_byte(0)?, data_byte(1)?) {
                (note, 0) => MidiMessage::NoteOff {
                    channel,
                    note,
                    velocity: 0,
                },
                (note, velocity) => MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                },
            },
            0xa => MidiMessage::PolyPressure {
                channel,
                note: data_byte(0)?,
                pressure: data_byte(1)?,
            },
            0xb => MidiMessage::ControlChange {
                channel,
                controller: data_byte(0)?,
                value: data_byte(1)?,
            },
            0xc => MidiMessage::ProgramChange {
                channel,
                program: data_byte(0)?,
            },
            0xd => MidiMessage::ChannelPressure {
                channel,
                pressure: data_byte(0)?,
            },
            0xe => {
                // Two 7-bit halves, least significant first.
                let raw = data_byte(0)? as u16 | (data_byte(1)? as u16) << 7;
                MidiMessage::PitchBend {
                    channel,
                    value: raw as i16 - PITCH_BEND_CENTER as i16,
                }
            }
            // 0xf: system messages (SysEx, clock, …), not channel specific.
            _ => return None,
        };
        Some(message)
    }

    /// Encodes the message; returns the bytes and how many of them are used
    /// (2 or 3). Out-of-range fields are masked to their bit width.
    pub fn to_bytes(self) -> ([u8; 3], usize) {
        let status = |kind: u8, channel: u8| kind << 4 | channel & CHANNEL_MASK;
        let three =
            |kind, channel, a: u8, b: u8| ([status(kind, channel), a & DATA_MAX, b & DATA_MAX], 3);
        let two = |kind, channel, a: u8| ([status(kind, channel), a & DATA_MAX, 0], 2);
        match self {
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => three(0x8, channel, note, velocity),
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => three(0x9, channel, note, velocity),
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => three(0xa, channel, note, pressure),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => three(0xb, channel, controller, value),
            MidiMessage::ProgramChange { channel, program } => two(0xc, channel, program),
            MidiMessage::ChannelPressure { channel, pressure } => two(0xd, channel, pressure),
            MidiMessage::PitchBend { channel, value } => {
                let raw = (value.clamp(-8192, 8191) + PITCH_BEND_CENTER as i16) as u16;
                three(0xe, channel, raw as u8, (raw >> 7) as u8)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes() {
        assert_eq!(
            MidiMessage::parse(&[0x90, 60, 100]),
            Some(MidiMessage::NoteOn {
                channel: 0,
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(
            MidiMessage::parse(&[0x83, 60, 64]),
            Some(MidiMessage::NoteOff {
                channel: 3,
                note: 60,
                velocity: 64
            })
        );
        assert_eq!(
            MidiMessage::parse(&[0x9f, 60, 0]),
            Some(MidiMessage::NoteOff {
                channel: 15,
                note: 60,
                velocity: 0
            })
        );
    }

    #[test]
    fn other_channel_messages() {
        assert_eq!(
            MidiMessage::parse(&[0xb0, 64, 127]),
            Some(MidiMessage::ControlChange {
                channel: 0,
                controller: 64,
                value: 127
            })
        );
        assert_eq!(
            MidiMessage::parse(&[0xc1, 5]),
            Some(MidiMessage::ProgramChange {
                channel: 1,
                program: 5
            })
        );
        assert_eq!(
            MidiMessage::parse(&[0xd2, 90]),
            Some(MidiMessage::ChannelPressure {
                channel: 2,
                pressure: 90
            })
        );
        assert_eq!(
            MidiMessage::parse(&[0xa0, 60, 30]),
            Some(MidiMessage::PolyPressure {
                channel: 0,
                note: 60,
                pressure: 30
            })
        );
    }

    #[test]
    fn pitch_bend_range() {
        let bend = |lsb, msb| match MidiMessage::parse(&[0xe0, lsb, msb]) {
            Some(MidiMessage::PitchBend { value, .. }) => value,
            other => panic!("{other:?}"),
        };
        assert_eq!(bend(0x00, 0x40), 0);
        assert_eq!(bend(0x00, 0x00), -8192);
        assert_eq!(bend(0x7f, 0x7f), 8191);
    }

    #[test]
    fn to_bytes_round_trips() {
        let messages = [
            MidiMessage::NoteOff {
                channel: 3,
                note: 60,
                velocity: 64,
            },
            MidiMessage::NoteOn {
                channel: 15,
                note: 127,
                velocity: 1,
            },
            MidiMessage::PolyPressure {
                channel: 0,
                note: 60,
                pressure: 30,
            },
            MidiMessage::ControlChange {
                channel: 1,
                controller: 64,
                value: 127,
            },
            MidiMessage::ProgramChange {
                channel: 2,
                program: 5,
            },
            MidiMessage::ChannelPressure {
                channel: 4,
                pressure: 90,
            },
            MidiMessage::PitchBend {
                channel: 5,
                value: -8192,
            },
            MidiMessage::PitchBend {
                channel: 5,
                value: 8191,
            },
        ];
        for msg in messages {
            let (bytes, len) = msg.to_bytes();
            assert_eq!(MidiMessage::parse(&bytes[..len]), Some(msg));
        }
    }

    #[test]
    fn rejects_invalid_input() {
        assert_eq!(MidiMessage::parse(&[]), None);
        // Running status: data bytes without a status byte.
        assert_eq!(MidiMessage::parse(&[60, 100]), None);
        // Truncated.
        assert_eq!(MidiMessage::parse(&[0x90, 60]), None);
        // Data byte with the status bit set.
        assert_eq!(MidiMessage::parse(&[0x90, 0xc8, 100]), None);
        // System messages (here: timing clock).
        assert_eq!(MidiMessage::parse(&[0xf8]), None);
    }
}
