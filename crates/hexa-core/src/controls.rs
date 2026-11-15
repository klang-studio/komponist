//! The MIDI controllers that act as modulation sources: mod wheel, pitch
//! wheel and channel pressure. Their 7- and 14-bit steps are smoothed at
//! control rate (like patch parameters), so a moving wheel does not step
//! audibly.

use crate::midi::{DATA_MAX, MidiMessage};
use crate::smooth::chase;

/// The mod wheel's controller number.
pub const CC_MOD_WHEEL: u8 = 1;
/// "Reset All Controllers": wheels and pressure back to rest.
pub const CC_RESET_CONTROLLERS: u8 = 121;

/// Controller values at one point in time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ControlValues {
    /// 0..=1.
    pub(crate) mod_wheel: f32,
    /// -1..=1.
    pub(crate) pitch_bend: f32,
    /// Channel pressure, 0..=1.
    pub(crate) pressure: f32,
}

/// The controllers' last received values and their smoothed state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Controls {
    target: ControlValues,
    value: ControlValues,
}

impl Controls {
    pub(crate) const fn new() -> Self {
        let rest = ControlValues {
            mod_wheel: 0.0,
            pitch_bend: 0.0,
            pressure: 0.0,
        };
        Self {
            target: rest,
            value: rest,
        }
    }

    /// Takes a controller message (any channel); other messages are ignored.
    pub(crate) fn handle(&mut self, msg: &MidiMessage) {
        let unit = |x: u8| x.min(DATA_MAX) as f32 / DATA_MAX as f32;
        match *msg {
            MidiMessage::ControlChange {
                controller: CC_MOD_WHEEL,
                value,
                ..
            } => self.target.mod_wheel = unit(value),
            MidiMessage::ControlChange {
                controller: CC_RESET_CONTROLLERS,
                ..
            } => {
                self.target = ControlValues::default();
            }
            MidiMessage::PitchBend { value, .. } => {
                self.target.pitch_bend = (value as f32 / 8192.0).clamp(-1.0, 1.0)
            }
            MidiMessage::ChannelPressure { pressure, .. } => self.target.pressure = unit(pressure),
            _ => {}
        }
    }

    /// Moves the values a step towards the received ones (once per block);
    /// returns them before and after.
    pub(crate) fn advance(&mut self, amount: f32) -> (ControlValues, ControlValues) {
        let before = self.value;
        chase(&mut self.value.mod_wheel, self.target.mod_wheel, amount);
        chase(&mut self.value.pitch_bend, self.target.pitch_bend, amount);
        chase(&mut self.value.pressure, self.target.pressure, amount);
        (before, self.value)
    }
}

/// Pressure of a single key (polyphonic aftertouch), 0..=1.
pub(crate) fn key_pressure(pressure: u8) -> f32 {
    pressure.min(DATA_MAX) as f32 / DATA_MAX as f32
}

/// A random value -1..1 for the `count`th note-on of `voice`: a hash, so it
/// does not depend on which engine renders the voice (see [`crate::Synth`]).
pub(crate) fn note_random(voice: usize, count: u32) -> f32 {
    let mut x = (voice as u32).wrapping_mul(0x9e37_79b9) ^ count.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 23) as f32 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controllers_are_read_and_smoothed() {
        let mut c = Controls::new();
        c.handle(&MidiMessage::ControlChange {
            channel: 3,
            controller: CC_MOD_WHEEL,
            value: 127,
        });
        c.handle(&MidiMessage::PitchBend {
            channel: 0,
            value: -8192,
        });
        c.handle(&MidiMessage::ChannelPressure {
            channel: 0,
            pressure: 64,
        });
        let (before, after) = c.advance(0.5);
        assert_eq!(before, ControlValues::default());
        assert_eq!((after.mod_wheel, after.pitch_bend), (0.5, -0.5));
        for _ in 0..100 {
            c.advance(0.5);
        }
        let (_, rest) = c.advance(0.5);
        assert_eq!(rest.mod_wheel, 1.0);
        assert_eq!(rest.pitch_bend, -1.0);
        assert!((rest.pressure - 64.0 / 127.0).abs() < 1e-6);
        c.handle(&MidiMessage::ControlChange {
            channel: 0,
            controller: CC_RESET_CONTROLLERS,
            value: 0,
        });
        for _ in 0..100 {
            c.advance(0.5);
        }
        assert_eq!(c.advance(0.5).1, ControlValues::default());
    }

    #[test]
    fn note_randoms_spread_over_the_range() {
        let values: Vec<f32> = (0..1000)
            .map(|i| note_random(i % 16, i as u32 / 16))
            .collect();
        assert!(values.iter().all(|x| (-1.0..1.0).contains(x)));
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        assert!(mean.abs() < 0.1, "{mean}");
        assert!(values.iter().any(|&x| x > 0.9) && values.iter().any(|&x| x < -0.9));
        assert_ne!(note_random(0, 1), note_random(0, 2));
        assert_ne!(note_random(0, 1), note_random(1, 1));
    }
}
