//! Modulation: LFO, sources, the per-pad matrix and how matrix targets are scaled.

use klang_dsp::{phase, wave};

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
}

/// A bipolar LFO on a `u32` phase (one period = 2^32, see [`klang_dsp::phase`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Lfo {
    phase: u32,
    held: f32,
}

impl Lfo {
    pub fn reset(&mut self, phase: f32, rng: &mut Rng) {
        self.phase = phase::from_unit(phase);
        self.held = rng.bipolar();
    }

    /// The value at the current phase without advancing.
    #[inline]
    pub fn value(&self, shape: LfoShape) -> f32 {
        match shape {
            LfoShape::Sine => wave::sine(self.phase),
            LfoShape::Triangle => wave::triangle(self.phase),
            LfoShape::SawUp => wave::saw_up(self.phase),
            LfoShape::SawDown => wave::saw_down(self.phase),
            LfoShape::Square => wave::square(self.phase),
            LfoShape::SampleHold => self.held,
        }
    }

    /// Returns the current value and advances by one sample.
    #[inline]
    pub fn next(&mut self, shape: LfoShape, freq_hz: f32, sample_rate: f32, rng: &mut Rng) -> f32 {
        let out = self.value(shape);
        let old = self.phase;
        self.phase = old.wrapping_add(phase::increment(freq_hz, sample_rate));
        if self.phase < old {
            self.held = rng.bipolar();
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
            lfo.reset(0.0, &mut rng);
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
        lfo.reset(0.0, &mut rng);
        let values: Vec<f32> = (0..20)
            .map(|_| lfo.next(LfoShape::SampleHold, 100.0, 1000.0, &mut rng))
            .collect();
        assert!(values[..10].iter().all(|v| *v == values[0]));
        assert_ne!(values[0], values[10]);
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
