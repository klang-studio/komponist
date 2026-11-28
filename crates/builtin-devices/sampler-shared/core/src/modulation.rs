//! Modulation: LFO, sources, the per-pad matrix and how matrix targets are scaled.
//!
//! Phase, wrap detection and shapes of the LFO come from [`klang_synth::CyclicLfo`]; sampler-core
//! keeps the timing (one step per sample, the value read before the step, the rounded
//! [`phase::increment`]), the voice RNG it shares with the noise source and the order of its
//! draws, and the free-running `f32` phase per pad (in [`crate::engine`]).
//!
//! # Random draws
//!
//! The LFO draws from the voice RNG (in `Voice`: after the per-note random value at the
//! note start; per sample before the noise source). Every shape but Smooth Random draws one value
//! in [`Lfo::reset`] and one on every wrap in [`Lfo::next`] (the held value of
//! [`LfoShape::SampleHold`]; the periodic shapes draw it too and ignore it, so adding Smooth Random
//! left their sequence alone). [`LfoShape::SmoothRandom`] draws two values in `reset`, the held
//! value first and then the next one, and one next value on every wrap, so it glides on from where
//! the last cycle ended. A voice whose shape changes to Smooth Random while it sounds glides towards
//! the next value it had (0 if it never had one) until its next wrap.

use klang_dsp::phase;
use klang_synth::{CyclicLfo, Shape};

pub const NUM_SLOTS: usize = 8;

/// Full-scale ranges of the matrix targets at amount 1.0 and a source value of 1.0
pub const PITCH_RANGE_SEMIS: f32 = 48.0;
pub const CUTOFF_RANGE_OCT: f32 = 8.0;
pub const LFO_RATE_RANGE_OCT: f32 = 4.0;
pub const TIME_RANGE_OCT: f32 = 4.0;
pub const BITS_RANGE: f32 = 22.0;
pub const DOWNSAMPLE_RANGE_OCT: f32 = 5.0;

pub use klang_dsp::rng::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LfoShape {
    #[default]
    Sine,
    Triangle,
    SawUp,
    SawDown,
    Square,
    SampleHold,
    /// From one random value to the next over every cycle, without steps (smoothstep).
    SmoothRandom,
}

impl LfoShape {
    /// The [`klang_synth`] shape that evaluates this one.
    #[inline]
    const fn shape(self) -> Shape {
        match self {
            LfoShape::Sine => Shape::Sine,
            LfoShape::Triangle => Shape::Triangle,
            LfoShape::SawUp => Shape::SawUp,
            LfoShape::SawDown => Shape::SawDown,
            LfoShape::Square => Shape::Square,
            LfoShape::SampleHold => Shape::SampleHold,
            LfoShape::SmoothRandom => Shape::SmoothRandom,
        }
    }
}

/// A bipolar LFO on a `u32` phase (one period = 2^32, see [`klang_dsp::phase`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Lfo {
    lfo: CyclicLfo,
}

impl Lfo {
    /// Moves to `phase` (in periods) and draws the random values of `shape` from `rng`: one, or
    /// for Smooth Random two (held, then next); see the module documentation.
    pub fn reset(&mut self, shape: LfoShape, phase: f32, rng: &mut Rng) {
        self.lfo.set_phase(phase::from_unit(phase));
        let held = rng.bipolar();
        if shape == LfoShape::SmoothRandom {
            self.lfo.set_random(held, rng.bipolar());
        } else {
            self.lfo.set_held(held);
        }
    }

    /// The value at the current phase without advancing.
    #[inline]
    pub fn value(&self, shape: LfoShape) -> f32 {
        self.lfo.value(shape.shape())
    }

    /// Returns the current value and advances by one sample; a new cycle draws one random value
    /// from `rng` (the held value, or for Smooth Random the next one).
    #[inline]
    pub fn next(&mut self, shape: LfoShape, freq_hz: f32, sample_rate: f32, rng: &mut Rng) -> f32 {
        let out = self.value(shape);
        if self.lfo.advance(phase::increment(freq_hz, sample_rate)) {
            let value = rng.bipolar();
            if shape == LfoShape::SmoothRandom {
                self.lfo.push_random(value);
            } else {
                self.lfo.set_held(value);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModSource {
    #[default]
    Off,
    Lfo,
    ModEnv1,
    ModEnv2,
    AmpEnv,
    Velocity,
    /// A random value per note
    Random,
    /// White noise, changes every sample
    Noise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModTarget {
    #[default]
    Off,
    Pitch,
    Cutoff,
    Resonance,
    Drive,
    Pan,
    Volume,
    /// Evaluated once at note on
    SampleStart,
    SampleEnd,
    LfoRate,
    Bits,
    Downsample,
    /// Evaluated once at note on
    AmpAttack,
    /// Evaluated once at note on
    AmpDecay,
}

impl ModTarget {
    pub fn is_note_on_only(self) -> bool {
        matches!(
            self,
            ModTarget::SampleStart | ModTarget::AmpAttack | ModTarget::AmpDecay
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModSlot {
    pub source: ModSource,
    pub target: ModTarget,
    /// `-1..1`
    pub amount: f32,
}

impl ModSlot {
    pub fn is_active(&self) -> bool {
        self.source != ModSource::Off && self.target != ModTarget::Off && self.amount != 0.0
    }
}

/// Current source values for one voice. Envelopes, velocity and random are unipolar, LFO and noise
/// bipolar.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sources {
    pub lfo: f32,
    pub env1: f32,
    pub env2: f32,
    pub amp: f32,
    pub velocity: f32,
    pub random: f32,
    pub noise: f32,
}

impl Sources {
    #[inline]
    fn get(&self, source: ModSource) -> f32 {
        match source {
            ModSource::Off => 0.0,
            ModSource::Lfo => self.lfo,
            ModSource::ModEnv1 => self.env1,
            ModSource::ModEnv2 => self.env2,
            ModSource::AmpEnv => self.amp,
            ModSource::Velocity => self.velocity,
            ModSource::Random => self.random,
            ModSource::Noise => self.noise,
        }
    }
}

/// Summed `source * amount` per target, in normalized units (`-1..1` per slot).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModValues {
    pub pitch: f32,
    pub cutoff: f32,
    pub resonance: f32,
    pub drive: f32,
    pub pan: f32,
    pub volume: f32,
    pub start: f32,
    pub end: f32,
    pub lfo_rate: f32,
    pub bits: f32,
    pub downsample: f32,
    pub amp_attack: f32,
    pub amp_decay: f32,
}

impl ModValues {
    #[inline]
    pub fn evaluate(slots: &[ModSlot], sources: &Sources) -> Self {
        let mut m = Self::default();
        for slot in slots {
            let v = sources.get(slot.source) * slot.amount;
            let field = match slot.target {
                ModTarget::Off => continue,
                ModTarget::Pitch => &mut m.pitch,
                ModTarget::Cutoff => &mut m.cutoff,
                ModTarget::Resonance => &mut m.resonance,
                ModTarget::Drive => &mut m.drive,
                ModTarget::Pan => &mut m.pan,
                ModTarget::Volume => &mut m.volume,
                ModTarget::SampleStart => &mut m.start,
                ModTarget::SampleEnd => &mut m.end,
                ModTarget::LfoRate => &mut m.lfo_rate,
                ModTarget::Bits => &mut m.bits,
                ModTarget::Downsample => &mut m.downsample,
                ModTarget::AmpAttack => &mut m.amp_attack,
                ModTarget::AmpDecay => &mut m.amp_decay,
            };
            *field += v;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfo_shapes_cover_full_range() {
        let mut rng = Rng::default();
        for shape in [
            LfoShape::Sine,
            LfoShape::Triangle,
            LfoShape::SawUp,
            LfoShape::SawDown,
            LfoShape::Square,
        ] {
            let mut lfo = Lfo::default();
            lfo.reset(shape, 0.0, &mut rng);
            let values: Vec<f32> = (0..1000)
                .map(|_| lfo.next(shape, 1.0, 1000.0, &mut rng))
                .collect();
            let (lo, hi) = values
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            assert!(lo < -0.95 && hi > 0.95, "{shape:?}: {lo}..{hi}");
        }
    }

    #[test]
    fn sample_hold_changes_once_per_cycle() {
        let mut rng = Rng::new(42);
        let mut lfo = Lfo::default();
        lfo.reset(LfoShape::SampleHold, 0.0, &mut rng);
        let values: Vec<f32> = (0..20)
            .map(|_| lfo.next(LfoShape::SampleHold, 100.0, 1000.0, &mut rng))
            .collect();
        assert!(values[..10].iter().all(|v| *v == values[0]));
        assert_ne!(values[0], values[10]);
    }

    #[test]
    fn smooth_random_draws_two_at_reset_and_one_per_wrap() {
        let mut draws = Rng::new(7);
        let r: Vec<f32> = (0..5).map(|_| draws.bipolar()).collect();
        let mut rng = Rng::new(7);
        let mut lfo = Lfo::default();
        lfo.reset(LfoShape::SmoothRandom, 0.0, &mut rng);
        assert_eq!((lfo.lfo.held(), lfo.lfo.next_random()), (r[0], r[1]));
        assert_eq!(lfo.value(LfoShape::SmoothRandom), r[0]);
        // 100 Hz at 1 kHz: ten samples per cycle, the tenth wraps.
        for _ in 0..9 {
            lfo.next(LfoShape::SmoothRandom, 100.0, 1000.0, &mut rng);
        }
        assert_eq!(lfo.lfo.held(), r[0]);
        lfo.next(LfoShape::SmoothRandom, 100.0, 1000.0, &mut rng);
        assert_eq!((lfo.lfo.held(), lfo.lfo.next_random()), (r[1], r[2]));
        assert_eq!(rng.bipolar(), r[3]);
    }

    #[test]
    fn other_shapes_draw_once_at_reset_and_per_wrap() {
        use LfoShape::*;
        for shape in [Sine, Triangle, SawUp, SawDown, Square, SampleHold] {
            let mut draws = Rng::new(7);
            let r: Vec<f32> = (0..3).map(|_| draws.bipolar()).collect();
            let mut rng = Rng::new(7);
            let mut lfo = Lfo::default();
            lfo.reset(shape, 0.0, &mut rng);
            assert_eq!(lfo.lfo.held(), r[0], "{shape:?}");
            for _ in 0..10 {
                lfo.next(shape, 100.0, 1000.0, &mut rng);
            }
            assert_eq!(lfo.lfo.held(), r[1], "{shape:?}");
            assert_eq!(rng.bipolar(), r[2], "{shape:?}");
        }
    }

    #[test]
    fn smooth_random_glides_across_wraps_within_range() {
        let mut rng = Rng::new(42);
        let mut lfo = Lfo::default();
        lfo.reset(LfoShape::SmoothRandom, 0.0, &mut rng);
        let mut last = lfo.value(LfoShape::SmoothRandom);
        let (mut low, mut high) = (f32::MAX, f32::MIN);
        // Ten cycles of 1000 samples.
        for i in 0..10_000 {
            let out = lfo.next(LfoShape::SmoothRandom, 1.0, 1000.0, &mut rng);
            assert_eq!(out, last, "the value before the step, sample {i}");
            last = lfo.value(LfoShape::SmoothRandom);
            // Largest step mid-cycle: 1.5 · |next − held| / 1000 ≤ 3e-3; none at a wrap.
            assert!((last - out).abs() < 3.1e-3, "step {out} → {last} at {i}");
            assert!((-1.0..=1.0).contains(&last));
            (low, high) = (low.min(last), high.max(last));
        }
        assert!(high - low > 0.5, "barely moves: {low}..{high}");
    }

    #[test]
    fn matrix_sums_slots_per_target() {
        let slots = [
            ModSlot {
                source: ModSource::Velocity,
                target: ModTarget::Cutoff,
                amount: 0.5,
            },
            ModSlot {
                source: ModSource::Lfo,
                target: ModTarget::Cutoff,
                amount: -1.0,
            },
            ModSlot {
                source: ModSource::Velocity,
                target: ModTarget::Off,
                amount: 1.0,
            },
        ];
        let sources = Sources {
            velocity: 1.0,
            lfo: 0.25,
            ..Sources::default()
        };
        let m = ModValues::evaluate(&slots, &sources);
        assert!((m.cutoff - 0.25).abs() < 1e-6);
        assert_eq!(m.pitch, 0.0);
    }
}
