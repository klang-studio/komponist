//! Low-frequency oscillators, evaluated at control rate (once per block).
//!
//! Each LFO runs per voice. With `key_sync` its phase restarts on every
//! note-on; without, a voice joins a free-running phase shared by all voices.
//! A patch has a list of up to [`MAX_LFOS`] LFOs ([`LfoBank`]); parameters
//! use them as modulation sources (see [`crate::modulation`]). LFOs that no
//! parameter uses are not computed.
//!
//! Phase, wrap detection and shapes come from [`klang_synth::CyclicLfo`];
//! Hexa keeps the timing (its truncating `increment()`, one step per block),
//! the one random generator shared by all LFOs and the order of its draws,
//! delay, fade-in and amount.
//!
//! # Random draws
//!
//! All LFOs of all voices draw from one generator ([`Lfos::seed`]), in the
//! order in which they start and wrap. Every shape but Smooth Random draws one
//! value when an LFO starts and one on every wrap (the held value of
//! [`LfoWaveform::SampleHold`]; the periodic shapes draw it too and ignore
//! it, so adding Smooth Random left their sequence alone). Smooth Random draws two
//! values when it starts, the held value first and then the next one, and one
//! next value on every wrap, so it glides on from where the last cycle ended.
//! An LFO whose shape changes to Smooth Random while it runs glides towards
//! the next value it had (0 if it never had one) until its next wrap.

use klang_dsp::rng::Rng;
use klang_synth::{CyclicLfo, Shape};

use crate::modulation::{LfoModulation, ModTarget, Offsets};
use crate::{MAX_VOICES, clamp_or};

/// Largest number of LFOs per patch; the engine reserves state for all.
pub const MAX_LFOS: usize = 8;

/// Slowest and fastest accepted rate in Hz.
pub const MIN_LFO_RATE: f32 = 0.01;
pub const MAX_LFO_RATE: f32 = 50.0;

/// Longest accepted fade-in time in seconds.
pub const MAX_FADE_IN: f32 = 10.0;

/// Longest accepted delay in seconds.
pub const MAX_LFO_DELAY: f32 = 10.0;

/// Largest rate modulation depth in octaves.
pub const MAX_RATE_DEPTH: f32 = 8.0;

/// Numbers in [`LfoPositions`]: three per LFO.
pub const LFO_POSITIONS: usize = 3 * MAX_LFOS;

/// Where every LFO of the newest sounding voice is, three numbers per LFO:
/// the phase 0..1 (-1 for an unused or missing LFO), the level 0..1 (amount
/// times delay and fade-in) and the output -1..1 (shape times level).
pub type LfoPositions = [f32; LFO_POSITIONS];

/// LFO shapes; all span -1..=1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum LfoWaveform {
    #[default]
    Sine,
    Triangle,
    SawUp,
    SawDown,
    Square,
    /// A new random value at the start of every period.
    SampleHold,
    /// From one random value to the next over every period, without steps
    /// (smoothstep); two draws at the start, one per wrap.
    SmoothRandom,
}

impl LfoWaveform {
    /// The [`klang_synth`] shape that evaluates this waveform.
    pub(crate) const fn shape(self) -> Shape {
        match self {
            Self::Sine => Shape::Sine,
            Self::Triangle => Shape::Triangle,
            Self::SawUp => Shape::SawUp,
            Self::SawDown => Shape::SawDown,
            Self::Square => Shape::Square,
            Self::SampleHold => Shape::SampleHold,
            Self::SmoothRandom => Shape::SmoothRandom,
        }
    }

    /// Whether the shape follows random values (they differ between engines
    /// with their own seeds).
    pub const fn is_random(self) -> bool {
        matches!(self, Self::SampleHold | Self::SmoothRandom)
    }
}

/// Patch parameters of one LFO.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct LfoParams {
    pub waveform: LfoWaveform,
    /// Frequency in Hz.
    pub rate: f32,
    /// Time in seconds after the delay over which the LFO fades in
    /// linearly (e.g. delayed vibrato); 0 = full depth right away.
    pub fade_in: f32,
    /// Seconds after note-on during which the LFO is silent (its phase runs
    /// on); then the fade-in starts. Patches from before have none.
    pub delay: f32,
    /// Start phase 0..1 (fraction of a period): where a key-synced LFO starts
    /// at note-on, or how far a free-running one is ahead of the shared phase.
    pub phase: f32,
    /// Restart the phase on every note-on instead of running freely.
    pub key_sync: bool,
    /// Output level 0..=1, scaling the LFO's effect on everything it modulates.
    pub amount: f32,
}

impl LfoParams {
    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            rate: clamp_or(self.rate, MIN_LFO_RATE, MAX_LFO_RATE, d.rate),
            fade_in: clamp_or(self.fade_in, 0.0, MAX_FADE_IN, d.fade_in),
            delay: clamp_or(self.delay, 0.0, MAX_LFO_DELAY, d.delay),
            phase: clamp_or(self.phase - libm::floorf(self.phase), 0.0, 1.0, d.phase),
            amount: clamp_or(self.amount, 0.0, 1.0, d.amount),
            ..self
        }
    }
}

impl LfoParams {
    pub const DEFAULT: Self = Self {
        waveform: LfoWaveform::Sine,
        rate: 5.0,
        fade_in: 0.0,
        delay: 0.0,
        phase: 0.0,
        key_sync: true,
        amount: 1.0,
    };
}

impl Default for LfoParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The LFOs of a patch.
pub type LfoBank = crate::bank::Bank<LfoParams, MAX_LFOS>;

/// Start of the random sequence of the LFOs, also in place of seed 0.
const SEED: u32 = 0x2545_f491;

/// Runtime state of all LFOs across all voices.
pub(crate) struct Lfos {
    /// Free-running phase per LFO, joined by voices without key sync.
    global: [u32; MAX_LFOS],
    /// Phase and random values per LFO and voice.
    lfo: [[CyclicLfo; MAX_VOICES]; MAX_LFOS],
    /// Output at the start of the next block per LFO and voice.
    value: [[f32; MAX_VOICES]; MAX_LFOS],
    /// The level (amount × fade) behind `value`, for displays.
    level: [[f32; MAX_VOICES]; MAX_LFOS],
    /// Seconds since note-on per voice, for the fade-in.
    age: [f32; MAX_VOICES],
    /// Random values of the random shapes, see the module documentation.
    rng: Rng,
}

impl Lfos {
    pub(crate) fn new() -> Self {
        Self {
            global: [0; MAX_LFOS],
            lfo: [[CyclicLfo::new(); MAX_VOICES]; MAX_LFOS],
            value: [[0.0; MAX_VOICES]; MAX_LFOS],
            level: [[0.0; MAX_VOICES]; MAX_LFOS],
            age: [0.0; MAX_VOICES],
            rng: Rng::new(SEED),
        }
    }

    /// Restarts the random sequence of the LFOs from `seed` (0 is replaced,
    /// xorshift would stay at 0).
    pub(crate) fn seed(&mut self, seed: u32) {
        self.rng = Rng::new(if seed == 0 { SEED } else { seed });
    }

    /// Starts the LFOs of `voice` for a new note. All are started, so an LFO
    /// that gets used while the note sounds has a sensible phase.
    pub(crate) fn note_on(&mut self, voice: usize, params: &[LfoParams]) {
        self.age[voice] = 0.0;
        for (l, p) in params.iter().enumerate() {
            self.start(l, voice, p);
        }
    }

    /// Starts LFO `l` of `voice`: at note-on, or when the LFO is added while
    /// the voice sounds (then its fade-in counts from the note-on).
    pub(crate) fn start(&mut self, l: usize, voice: usize, p: &LfoParams) {
        let offset = crate::phase::from_unit(p.phase);
        let lfo = &mut self.lfo[l][voice];
        lfo.set_phase(if p.key_sync {
            offset
        } else {
            self.global[l].wrapping_add(offset)
        });
        // Draw order: see the module documentation.
        let held = self.rng.bipolar();
        if p.waveform == LfoWaveform::SmoothRandom {
            lfo.set_random(held, self.rng.bipolar());
        } else {
            lfo.set_held(held);
        }
        self.set_output(l, voice, p, p.amount, p.fade_in);
    }

    /// Phase, level and output of LFO `l` of `voice`, see [`LfoPositions`].
    pub(crate) fn position(&self, l: usize, voice: usize) -> [f32; 3] {
        let phase = crate::phase::to_unit(self.lfo[l][voice].phase());
        [phase, self.level[l][voice], self.value[l][voice]]
    }

    /// Advances the free-running phases by one block; call once per block.
    pub(crate) fn advance_global(&mut self, params: &[LfoParams], block_seconds: f32) {
        for (l, p) in params.iter().enumerate() {
            self.global[l] = self.global[l].wrapping_add(increment(p.rate, block_seconds));
        }
    }

    /// Advances the LFOs in `used` (bit mask) of a sounding `voice` by one
    /// block, with their own modulation (rate, amount and fade-in offsets
    /// from every source but LFOs, where `modulated` says) at the start and
    /// the end of the block.
    /// Returns each LFO's output at the start and at the end of the block; 0
    /// for unused ones.
    pub(crate) fn advance_voice(
        &mut self,
        voice: usize,
        params: &[LfoParams],
        used: u8,
        modulated: LfoModulation,
        (o0, o1): (&Offsets, &Offsets),
        block_seconds: f32,
    ) -> ([f32; MAX_LFOS], [f32; MAX_LFOS]) {
        self.age[voice] += block_seconds;
        let mut start = [0.0; MAX_LFOS];
        let mut end = [0.0; MAX_LFOS];
        for (l, p) in params.iter().enumerate() {
            if used & (1 << l) == 0 {
                continue;
            }
            start[l] = self.value[l][voice];
            let bit = 1 << l;
            let octaves = if modulated.rates & bit != 0 {
                o0.get(ModTarget::LfoRate(l))
            } else {
                0.0
            };
            let rate = if octaves != 0.0 {
                p.rate * libm::exp2f(octaves)
            } else {
                p.rate
            };
            let lfo = &mut self.lfo[l][voice];
            if lfo.advance(increment(rate, block_seconds)) {
                let value = self.rng.bipolar();
                if p.waveform == LfoWaveform::SmoothRandom {
                    lfo.push_random(value);
                } else {
                    lfo.set_held(value);
                }
            }
            let amount = if modulated.amounts & bit != 0 {
                (p.amount + o1.get(ModTarget::LfoAmount(l))).clamp(0.0, 1.0)
            } else {
                p.amount
            };
            let fade_in = if modulated.fades & bit != 0 {
                crate::modulation::time_offset(
                    p.fade_in,
                    o1.get(ModTarget::LfoFade(l)),
                    MAX_FADE_IN,
                )
            } else {
                p.fade_in
            };
            self.set_output(l, voice, p, amount, fade_in);
            end[l] = self.value[l][voice];
        }
        (start, end)
    }

    fn set_output(&mut self, l: usize, voice: usize, p: &LfoParams, amount: f32, fade_in: f32) {
        let age = self.age[voice] - p.delay;
        let fade = if age < 0.0 {
            0.0
        } else if fade_in > 0.0 {
            (age / fade_in).min(1.0)
        } else {
            1.0
        };
        let level = fade * amount;
        self.level[l][voice] = level;
        self.value[l][voice] = self.lfo[l][voice].value(p.waveform.shape()) * level;
    }
}

/// Phase increment for `rate` Hz over `seconds`, one period = 2^32.
fn increment(rate: f32, seconds: f32) -> u32 {
    (rate as f64 * seconds as f64 * crate::phase::PHASE_PERIOD) as u64 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: f32 = 32.0 / 48_000.0;
    const NO_ENV: (&Offsets, &Offsets) = (&Offsets::ZERO, &Offsets::ZERO);
    const ALL: LfoModulation = LfoModulation {
        rates: 0xff,
        amounts: 0xff,
        fades: 0xff,
    };

    fn lfo_params(waveform: LfoWaveform, rate: f32) -> LfoParams {
        LfoParams {
            waveform,
            rate,
            ..Default::default()
        }
    }

    fn at(waveform: LfoWaveform, t: f64) -> f32 {
        let mut lfo = CyclicLfo::new();
        lfo.set_phase((t * crate::phase::PHASE_PERIOD) as u64 as u32);
        lfo.set_held(0.3);
        lfo.value(waveform.shape())
    }

    /// Phase of LFO `l` of `voice`.
    fn phase(lfos: &Lfos, l: usize, voice: usize) -> u32 {
        lfos.lfo[l][voice].phase()
    }

    #[test]
    fn shapes_span_full_range_and_start_like_a_sine() {
        use LfoWaveform::*;
        for (w, expected) in [
            (Sine, [0.0, 1.0, 0.0, -1.0]),
            (Triangle, [0.0, 1.0, 0.0, -1.0]),
            (SawUp, [-1.0, -0.5, 0.0, 0.5]),
            (SawDown, [1.0, 0.5, 0.0, -0.5]),
            (Square, [1.0, 1.0, -1.0, -1.0]),
            (SampleHold, [0.3, 0.3, 0.3, 0.3]),
        ] {
            for (i, &e) in expected.iter().enumerate() {
                let v = at(w, i as f64 * 0.25);
                assert!(
                    (v - e).abs() < 1e-3,
                    "{w:?} at {}: {v}, expected {e}",
                    i as f64 * 0.25
                );
            }
        }
    }

    #[test]
    fn rate_sets_the_period() {
        let params = [LfoParams {
            waveform: LfoWaveform::SawUp,
            rate: 2.0,
            ..Default::default()
        }; 3];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        // Half a second = one period at 2 Hz: back near the start value.
        let blocks = (0.5 / BLOCK) as usize;
        let mut last = (0.0, 0.0);
        for _ in 0..blocks {
            let (s, e) = lfos.advance_voice(0, &params, 0b111, ALL, NO_ENV, BLOCK);
            last = (s[0], e[0]);
        }
        assert!(last.1 < -0.98 || last.0 > 0.98, "{last:?}");
    }

    #[test]
    fn key_sync_restarts_free_running_follows_global() {
        let mut params = [LfoParams::default(); 3];
        params[1].key_sync = false;
        let mut lfos = Lfos::new();
        for _ in 0..100 {
            lfos.advance_global(&params, BLOCK);
        }
        lfos.note_on(3, &params);
        assert_eq!(phase(&lfos, 0, 3), 0);
        assert_eq!(phase(&lfos, 1, 3), lfos.global[1]);
        assert_ne!(lfos.global[1], 0);
    }

    #[test]
    fn fade_in_ramps_depth() {
        let params = [LfoParams {
            waveform: LfoWaveform::Square,
            fade_in: 1.0,
            ..Default::default()
        }; 3];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        assert_eq!(lfos.value[0][0], 0.0);
        let (_, end) = lfos.advance_voice(0, &params, 0b111, ALL, NO_ENV, 0.25);
        assert!((end[0].abs() - 0.25).abs() < 1e-6, "{}", end[0]);
        lfos.advance_voice(0, &params, 0b111, ALL, NO_ENV, 1.0);
        assert_eq!(lfos.value[0][0].abs(), 1.0);
    }

    #[test]
    fn sample_and_hold_changes_once_per_period() {
        let params = [LfoParams {
            waveform: LfoWaveform::SampleHold,
            rate: 1.0,
            ..Default::default()
        }; 3];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        let first = lfos.value[0][0];
        let (_, end) = lfos.advance_voice(0, &params, 0b111, ALL, NO_ENV, 0.4);
        assert_eq!(end[0], first);
        let (_, end) = lfos.advance_voice(0, &params, 0b111, ALL, NO_ENV, 0.7);
        assert_ne!(end[0], first);
        assert!((-1.0..1.0).contains(&end[0]));
    }

    /// The values a fresh [`Lfos`] draws, in order.
    fn draws(n: usize) -> Vec<f32> {
        let mut rng = Rng::new(SEED);
        (0..n).map(|_| rng.bipolar()).collect()
    }

    #[test]
    fn smooth_random_draws_two_at_the_start_and_one_per_wrap() {
        let params = [
            lfo_params(LfoWaveform::SmoothRandom, 1.0),
            lfo_params(LfoWaveform::Sine, 1.0),
        ];
        let r = draws(6);
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        // Smooth Random: held, then next; the sine after it: one value.
        let random = &lfos.lfo[0][0];
        assert_eq!((random.held(), random.next_random()), (r[0], r[1]));
        assert_eq!(lfos.lfo[1][0].held(), r[2]);
        assert_eq!(lfos.value[0][0], r[0], "starts at the held value");

        // Past one period: both wrap, in LFO order, one draw each.
        lfos.advance_voice(0, &params, 0b11, ALL, NO_ENV, 0.6);
        lfos.advance_voice(0, &params, 0b11, ALL, NO_ENV, 0.6);
        let random = &lfos.lfo[0][0];
        assert_eq!((random.held(), random.next_random()), (r[1], r[3]));
        assert_eq!(lfos.lfo[1][0].held(), r[4]);
        assert_eq!(lfos.rng.bipolar(), r[5]);
    }

    #[test]
    fn other_shapes_draw_once_at_the_start_and_per_wrap() {
        use LfoWaveform::*;
        for waveform in [Sine, Triangle, SawUp, SawDown, Square, SampleHold] {
            let params = [lfo_params(waveform, 1.0)];
            let r = draws(3);
            let mut lfos = Lfos::new();
            lfos.note_on(0, &params);
            assert_eq!(lfos.lfo[0][0].held(), r[0], "{waveform:?}");
            lfos.advance_voice(0, &params, 1, ALL, NO_ENV, 0.6);
            assert_eq!(lfos.lfo[0][0].held(), r[0], "{waveform:?}");
            lfos.advance_voice(0, &params, 1, ALL, NO_ENV, 0.6);
            assert_eq!(lfos.lfo[0][0].held(), r[1], "{waveform:?}");
            assert_eq!(lfos.rng.bipolar(), r[2], "{waveform:?}");
        }
    }

    #[test]
    fn smooth_random_glides_across_wraps_within_range() {
        let params = [lfo_params(LfoWaveform::SmoothRandom, 3.0)];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        let mut last = lfos.value[0][0];
        let (mut wraps, mut low, mut high) = (0, f32::MAX, f32::MIN);
        // A little over two seconds at 3 Hz: six wraps.
        for _ in 0..(2.1 / BLOCK) as usize {
            let before = phase(&lfos, 0, 0);
            let (start, end) = lfos.advance_voice(0, &params, 1, ALL, NO_ENV, BLOCK);
            if phase(&lfos, 0, 0) < before {
                wraps += 1;
            }
            assert_eq!(start[0], last);
            // Largest step: 1.5 · |next − held| · 3 Hz · BLOCK ≤ 0.02; none at a wrap.
            assert!((end[0] - last).abs() < 0.02, "step {last} → {}", end[0]);
            assert!((-1.0..=1.0).contains(&end[0]));
            (low, high) = (low.min(end[0]), high.max(end[0]));
            last = end[0];
        }
        assert_eq!(wraps, 6);
        assert!(high - low > 0.2, "barely moves: {low}..{high}");
    }

    #[test]
    fn unused_lfos_are_not_advanced() {
        let params = [LfoParams::default(); 2];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        let (_, end) = lfos.advance_voice(0, &params, 0b10, ALL, NO_ENV, BLOCK);
        assert_eq!(end[0], 0.0);
        assert_eq!(phase(&lfos, 0, 0), 0);
        assert_ne!(phase(&lfos, 1, 0), 0);
    }

    /// Offsets with `target` moved by `value`.
    fn offset(target: ModTarget, value: f32) -> Offsets<'static> {
        use crate::modulation::{ModRoute, ModSource, SourceValues};
        let route = ModRoute {
            source: ModSource::Velocity,
            target,
            depth: value,
            via: ModSource::None,
        };
        Offsets::new(
            &[route],
            &SourceValues {
                velocity: 1.0,
                ..SourceValues::ZERO
            },
        )
    }

    #[test]
    fn offsets_scale_amount_and_rate() {
        let params = [LfoParams {
            waveform: LfoWaveform::Square,
            amount: 0.0,
            ..Default::default()
        }];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        let half = offset(ModTarget::LfoAmount(0), 0.5);
        let (_, end) = lfos.advance_voice(0, &params, 1, ALL, (&half, &half), BLOCK);
        assert_eq!(end[0], 0.5);

        // One octave faster: twice the phase step.
        let params = [LfoParams::default()];
        let octave = offset(ModTarget::LfoRate(0), 1.0);
        lfos.note_on(1, &params);
        lfos.note_on(2, &params);
        lfos.advance_voice(1, &params, 1, ALL, NO_ENV, BLOCK);
        lfos.advance_voice(2, &params, 1, ALL, (&octave, &octave), BLOCK);
        assert!(phase(&lfos, 0, 2).abs_diff(2 * phase(&lfos, 0, 1)) <= 2);
    }

    #[test]
    fn sanitized_clamps() {
        let p = LfoParams {
            phase: 1.25,
            delay: 99.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!((p.phase, p.delay), (0.25, MAX_LFO_DELAY));
        let p = LfoParams {
            rate: 1000.0,
            fade_in: f32::NAN,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(p.rate, MAX_LFO_RATE);
        assert_eq!(p.fade_in, LfoParams::default().fade_in);
    }

    #[test]
    fn delay_silences_then_fades_and_phase_offsets_the_start() {
        let params = [LfoParams {
            waveform: LfoWaveform::Square,
            delay: 0.5,
            fade_in: 0.5,
            ..Default::default()
        }];
        let mut lfos = Lfos::new();
        lfos.note_on(0, &params);
        let (_, end) = lfos.advance_voice(0, &params, 1, ALL, NO_ENV, 0.4);
        assert_eq!(end[0], 0.0, "still delayed");
        let (_, end) = lfos.advance_voice(0, &params, 1, ALL, NO_ENV, 0.35);
        assert!(
            (end[0].abs() - 0.5).abs() < 1e-3,
            "half faded in: {}",
            end[0]
        );

        // A quarter period ahead: a sine starts at its peak.
        let params = [LfoParams {
            phase: 0.25,
            ..Default::default()
        }];
        lfos.note_on(1, &params);
        assert!(
            (lfos.value[0][1] - 1.0).abs() < 1e-3,
            "{}",
            lfos.value[0][1]
        );
    }
}
