//! The synth's voices: one sine per held key, decaying exponentially.

use std::f32::consts::TAU;

/// The most voices that sound at once. A new note beyond that steals the oldest voice.
pub const MAX_VOICES: usize = 16;
/// The time a released voice takes to fall by 60 dB.
pub const RELEASE_SECONDS: f32 = 0.01;
/// A voice below this amplitude (−80 dB) is finished.
const SILENT: f32 = 1e-4;

#[derive(Clone, Copy)]
struct Voice {
    key: u8,
    /// Phase in turns, in `[0, 1)`.
    phase: f32,
    /// Phase increment per sample, in turns.
    step: f32,
    amplitude: f32,
    released: bool,
    /// Order of the note-ons, to find the oldest voice.
    age: u64,
}

/// A fixed set of sine voices. Allocates nothing after construction.
pub struct Voices {
    voices: [Option<Voice>; MAX_VOICES],
    sample_rate: f32,
    /// Per-sample amplitude factor of held voices.
    decay_factor: f32,
    /// Per-sample amplitude factor of released voices.
    release_factor: f32,
    notes_played: u64,
}

impl Voices {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            voices: [None; MAX_VOICES],
            sample_rate,
            decay_factor: 1.0,
            release_factor: t60_factor(RELEASE_SECONDS, sample_rate),
            notes_played: 0,
        }
    }

    /// Sets the time a held voice takes to fall by 60 dB. Applies to all held voices.
    pub fn set_decay(&mut self, seconds: f32) {
        self.decay_factor = t60_factor(seconds, self.sample_rate);
    }

    /// Starts a voice for `key` at `velocity` (0 to 1). A key that sounds already restarts.
    pub fn note_on(&mut self, key: u8, velocity: f32) {
        let slot = self
            .voices
            .iter()
            .position(|v| v.is_some_and(|v| v.key == key))
            .or_else(|| self.voices.iter().position(Option::is_none))
            .unwrap_or_else(|| self.oldest());
        self.notes_played += 1;
        self.voices[slot] = Some(Voice {
            key,
            phase: 0.0,
            step: key_to_freq(key) / self.sample_rate,
            amplitude: velocity.clamp(0.0, 1.0),
            released: false,
            age: self.notes_played,
        });
    }

    /// Releases the voice of `key`: it fades out within [`RELEASE_SECONDS`].
    pub fn note_off(&mut self, key: u8) {
        for voice in self.voices.iter_mut().flatten() {
            if voice.key == key {
                voice.released = true;
            }
        }
    }

    /// Releases all voices.
    pub fn release_all(&mut self) {
        for voice in self.voices.iter_mut().flatten() {
            voice.released = true;
        }
    }

    /// Silences all voices at once.
    pub fn reset(&mut self) {
        self.voices = [None; MAX_VOICES];
    }

    /// The number of sounding voices.
    pub fn active(&self) -> usize {
        self.voices.iter().flatten().count()
    }

    /// Renders the next sample: the sum of all voices.
    pub fn next_sample(&mut self) -> f32 {
        let mut sum = 0.0;
        for slot in &mut self.voices {
            let Some(voice) = slot else { continue };
            sum += (voice.phase * TAU).sin() * voice.amplitude;
            voice.phase = (voice.phase + voice.step).fract();
            voice.amplitude *= if voice.released {
                self.release_factor
            } else {
                self.decay_factor
            };
            if voice.amplitude < SILENT {
                *slot = None;
            }
        }
        sum
    }

    fn oldest(&self) -> usize {
        (0..MAX_VOICES)
            .min_by_key(|&i| self.voices[i].map_or(0, |v| v.age))
            .unwrap_or(0)
    }
}

/// The frequency of a MIDI key in Hz, A4 (69) = 440 Hz.
pub fn key_to_freq(key: u8) -> f32 {
    440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0)
}

/// The per-sample factor that lowers an amplitude by 60 dB in `seconds`.
fn t60_factor(seconds: f32, sample_rate: f32) -> f32 {
    0.001f32.powf(1.0 / (seconds.max(1e-3) * sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn render(voices: &mut Voices, samples: usize) -> Vec<f32> {
        (0..samples).map(|_| voices.next_sample()).collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn a4_is_440_hz() {
        assert_eq!(key_to_freq(69), 440.0);
        assert!((key_to_freq(81) - 880.0).abs() < 1e-3);
    }

    #[test]
    fn note_renders_a_sine_of_its_key() {
        let mut voices = Voices::new(RATE);
        voices.set_decay(100.0);
        voices.note_on(69, 1.0);
        let out = render(&mut voices, 4800);
        // 440 Hz over 0.1 s: 44 periods, so 88 sign changes (±1 for the start at zero).
        let crossings = out
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((87..=89).contains(&crossings), "{crossings} zero crossings");
        assert!(peak(&out) > 0.99);
    }

    #[test]
    fn held_note_decays_by_60_db_in_decay_time() {
        let mut voices = Voices::new(RATE);
        voices.set_decay(0.5);
        voices.note_on(69, 1.0);
        render(&mut voices, 24_000 - 480);
        let last = render(&mut voices, 480);
        assert!((0.0008..0.0013).contains(&peak(&last)), "{}", peak(&last));
    }

    #[test]
    fn released_note_ends_quickly() {
        let mut voices = Voices::new(RATE);
        voices.set_decay(10.0);
        voices.note_on(60, 1.0);
        render(&mut voices, 100);
        voices.note_off(60);
        // −80 dB takes 4/3 of the 60 dB release time.
        render(&mut voices, (RELEASE_SECONDS * RATE * 1.4) as usize);
        assert_eq!(voices.active(), 0);
    }

    #[test]
    fn same_key_reuses_its_voice_and_overflow_steals_the_oldest() {
        let mut voices = Voices::new(RATE);
        voices.set_decay(10.0);
        voices.note_on(60, 1.0);
        voices.note_on(60, 1.0);
        assert_eq!(voices.active(), 1);

        for key in 0..MAX_VOICES as u8 {
            voices.note_on(70 + key, 1.0);
        }
        assert_eq!(voices.active(), MAX_VOICES);
        // Key 60 was the oldest and got stolen.
        assert!(voices.voices.iter().flatten().all(|v| v.key != 60));
    }

    #[test]
    fn reset_silences_everything() {
        let mut voices = Voices::new(RATE);
        voices.note_on(60, 1.0);
        voices.reset();
        assert_eq!(voices.active(), 0);
        assert_eq!(voices.next_sample(), 0.0);
    }
}
