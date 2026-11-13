//! The metronome (ARCHITECTURE §6.4 steps 3 and 5).
//!
//! One engine-internal voice: a sine with an exponential decay (`klang_dsp::one_pole::t60_coeff`),
//! restarted on every beat. Beat 1 of a bar is accented with a higher pitch, as in the prototype
//! (`ks-engine.js` `click`: 1600 Hz accent, 1100 Hz otherwise). The click is added to the output
//! after the master fader at its own level and is not metered. It sounds during the count-in and,
//! when the program's metronome is on, during playback.

use klang_dsp::one_pole::t60_coeff;
use klang_dsp::phase::increment;
use klang_dsp::sine::sine;
use komponist_model::{TempoMap, Tick};

/// Peak level of a click, linear (about −10.5 dBFS).
pub(crate) const CLICK_LEVEL: f32 = 0.3;

/// Pitch of the accented click on beat 1, in Hz.
pub(crate) const ACCENT_HZ: f32 = 1600.0;

/// Pitch of the other beats, in Hz.
pub(crate) const BEAT_HZ: f32 = 1100.0;

/// Time for a click to fall by 60 dB, in seconds.
const CLICK_T60: f32 = 0.06;

/// Envelope level below which the voice stops (−80 dB re. full scale), so the output between
/// clicks is exactly silent.
const SILENT: f32 = 1e-4;

/// The click voice.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Metronome {
    phase: u32,
    inc: u32,
    env: f32,
    coeff: f32,
}

impl Metronome {
    /// RT. Starts a click (accented: beat 1 of a bar).
    pub(crate) fn trigger(&mut self, accent: bool, sample_rate: f32) {
        let hz = if accent { ACCENT_HZ } else { BEAT_HZ };
        self.phase = 0;
        self.inc = increment(hz, sample_rate);
        self.env = CLICK_LEVEL;
        self.coeff = t60_coeff(CLICK_T60, sample_rate);
    }

    /// RT. Stops the voice at once (sample rate change).
    pub(crate) fn silence(&mut self) {
        self.env = 0.0;
    }

    /// RT. Adds the sounding click to both channels. The first sample of a click is already one
    /// phase step in, so it is not zero and marks the onset.
    pub(crate) fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
            if self.env == 0.0 {
                break;
            }
            self.phase = self.phase.wrapping_add(self.inc);
            let s = sine(self.phase) * self.env;
            *l += s;
            *r += s;
            self.env *= self.coeff;
            if self.env < SILENT {
                self.env = 0.0;
            }
        }
    }

    /// RT. Adds the clicks of a stretch of song samples `start .. start + out_l.len()` to the
    /// output: every beat whose sample lies inside starts a click.
    pub(crate) fn run(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        start: i64,
        map: &TempoMap,
        sample_rate: f32,
    ) {
        let n = out_l.len().min(out_r.len());
        let mut done = 0;
        for_each_beat(
            map,
            f64::from(sample_rate),
            start,
            start + n as i64,
            |offset, accent| {
                self.render(&mut out_l[done..offset], &mut out_r[done..offset]);
                self.trigger(accent, sample_rate);
                done = offset;
            },
        );
        self.render(&mut out_l[done..n], &mut out_r[done..n]);
    }
}

/// Calls `f(offset, accent)` for every beat whose sample (`TempoMap::tick_to_sample`) lies in
/// `start .. end`, in order; `offset` counts from `start`, `accent` marks beat 1 of a bar. Works for
/// negative (count-in) positions.
pub(crate) fn for_each_beat(
    map: &TempoMap,
    sample_rate: f64,
    start: i64,
    end: i64,
    mut f: impl FnMut(usize, bool),
) {
    let per_beat = map.meter.ticks_per_beat();
    let per_bar = map.meter.ticks_per_bar();
    if per_beat <= 0 || end <= start {
        return;
    }
    let first = map.samples_to_ticks(start as f64, sample_rate) / per_beat as f64;
    // One beat early, so rounding to the nearest sample can never skip a beat.
    let mut beat = first.floor() as i64 - 1;
    loop {
        let tick = beat * per_beat;
        let sample = map.tick_to_sample(Tick(tick), sample_rate);
        if sample >= end {
            break;
        }
        if sample >= start {
            f((sample - start) as usize, tick.rem_euclid(per_bar) == 0);
        }
        beat += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{Meter, Tempo};

    fn beats(map: &TempoMap, rate: f64, start: i64, end: i64) -> Vec<(usize, bool)> {
        let mut out = Vec::new();
        for_each_beat(map, rate, start, end, |o, a| out.push((o, a)));
        out
    }

    #[test]
    fn beats_at_the_rounded_sample_of_their_tick() {
        let map = TempoMap::constant(Tempo::new(124.0), Meter::FOUR_FOUR);
        let rate = 44_100.0;
        let all = beats(&map, rate, -200_000, 200_000);
        let expected: Vec<(usize, bool)> = (-10_i64..=10)
            .map(|b| {
                let s = map.tick_to_sample(Tick(b * 960), rate);
                ((s + 200_000) as usize, b.rem_euclid(4) == 0)
            })
            .filter(|&(o, _)| o < 400_000)
            .collect();
        assert_eq!(all, expected);
        // Split anywhere, the beats stay the same.
        for split in [-150_000_i64, -1, 0, 1, 21_338, 21_339, 21_340] {
            let mut joined = beats(&map, rate, -200_000, split);
            joined.extend(
                beats(&map, rate, split, 200_000)
                    .into_iter()
                    .map(|(o, a)| (o + (split + 200_000) as usize, a)),
            );
            assert_eq!(joined, expected, "split at {split}");
        }
    }

    #[test]
    fn beats_in_six_eight() {
        let map = TempoMap::constant(Tempo::new(120.0), Meter { num: 6, den: 8 });
        // An eighth at 120 bpm and 48 kHz is 12 000 samples; bars of six.
        let got = beats(&map, 48_000.0, 0, 84_000);
        let expected: Vec<(usize, bool)> = (0..7).map(|b| (b * 12_000, b % 6 == 0)).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn click_starts_on_its_sample_and_decays_to_silence() {
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        let mut m = Metronome::default();
        let (mut l, mut r) = (vec![0.0; 30_000], vec![0.0; 30_000]);
        // Song samples 100 .. 30 100: the beat at 24 000 (offset 23 900), not accented.
        m.run(&mut l, &mut r, 100, &map, 48_000.0);
        let first = l.iter().position(|&x| x != 0.0).unwrap();
        assert_eq!(first, 23_900);
        let expected = CLICK_LEVEL * sine(increment(BEAT_HZ, 48_000.0));
        assert_eq!(l[first], expected);
        assert_eq!(l, r);
        // Silent again well before the next beat.
        let last = l.iter().rposition(|&x| x != 0.0).unwrap();
        assert!(last - first < 48_000 / 10, "{}", last - first);
        assert!(l[first..].iter().all(|x| x.abs() <= CLICK_LEVEL));
    }

    #[test]
    fn accent_on_beat_one() {
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        let mut m = Metronome::default();
        let (mut l, mut r) = (vec![0.0; 10], vec![0.0; 10]);
        m.run(&mut l, &mut r, 0, &map, 48_000.0);
        assert_eq!(l[0], CLICK_LEVEL * sine(increment(ACCENT_HZ, 48_000.0)));
    }
}
