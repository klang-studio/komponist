//! FM operator: an oscillator (sine, triangle, saw, square or noise) with
//! its own pitch, output level, envelope, key scaling and routing. The
//! routing of all operators together forms the algorithm: an operator can
//! be heard (carrier), modulate other operators (modulator), both, and
//! modulate itself (feedback).

use crate::MAX_VOICES;
use crate::envelope::{Envelope, EnvelopeParams};
use crate::smooth::chase;
use crate::waveform::Waveform;

/// Number of operators per voice.
pub const OPERATORS: usize = 6;

const _: () = assert!(OPERATORS <= 8, "routing masks are u8");

/// Largest accepted frequency ratio.
pub const MAX_RATIO: f32 = 32.0;

/// Largest transposition in [`PitchMode::Semitone`], up and down.
pub const MAX_SEMITONES: f32 = 36.0;

/// Highest fixed frequency in [`PitchMode::Frequency`], in Hz.
pub const MAX_FREQUENCY: f32 = 10_000.0;

/// How an operator's frequency is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PitchMode {
    /// `ratio` × the note frequency.
    #[default]
    Ratio,
    /// The note transposed by `semitones` (fractions are cents).
    Semitone,
    /// Fixed at `frequency` Hz, independent of the note.
    Frequency,
}

/// Phase deviation in radians that a modulator at full level (1.0) applies
/// to the operator it modulates.
pub const MAX_MODULATION: f32 = 16.0;

/// Phase deviation in radians of an operator modulating itself at full
/// feedback (1.0) and full level.
pub const MAX_FEEDBACK: f32 = core::f32::consts::PI;

/// How far HTE sync speeds up the carrier within each modulator cycle at
/// full level, in octaves.
pub const MAX_SYNC_OCTAVES: f32 = 3.0;

/// What an operator is: an oscillator, or a 2-pole filter (opsix style) on
/// the operators routed into it. As a filter, its pitch (ratio, semitones or
/// frequency, with its pitch modulation) is the cutoff, so it follows the keys like an
/// oscillator would; level and envelope set its output, `waveform` and
/// `feedback` do not matter. Sources in [`ModulationMode::Phase`] are its
/// audio input (each times its depth), sources in
/// [`ModulationMode::Frequency`] move the cutoff at audio rate (filter FM,
/// scaled like linear FM), [`ModulationMode::Ring`] works as usual on its
/// output; pulse width and sync have nothing to act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum OperatorKind {
    #[default]
    Oscillator,
    LowPass,
    HighPass,
    /// Unity gain at the cutoff.
    BandPass,
}

/// Gain into an operator's wavefolder at a fold of 1.0: the wave folds back
/// at ±1, so a sine crosses zero up to about twice this often per cycle.
pub const MAX_FOLD: f32 = 8.0;

/// How an operator affects the operators it modulates (it does not change
/// the operator's own sound).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ModulationMode {
    /// The output shifts the carrier's phase (DX7 style); level 1.0 =
    /// [`MAX_MODULATION`] radians.
    #[default]
    Phase,
    /// Linear (through-zero) FM: the output shifts the carrier's frequency,
    /// by [`MAX_MODULATION`] × the modulator frequency at level 1.0, so the
    /// index matches phase mode.
    Frequency,
    /// Pulse width: within each modulator cycle, the first half of the
    /// carrier's waveform is squeezed into a shorter time and the second half
    /// stretched; level 1.0 squeezes it away completely. The modulator's
    /// waveform does not matter, its ratio sets how often this happens.
    PulseWidth,
    /// Hard sync: each modulator cycle restarts the carrier's cycle, and the
    /// level speeds the carrier up by up to [`MAX_SYNC_OCTAVES`] in between
    /// (the classic sync sweep). The modulator's waveform does not matter.
    Sync,
    /// Ring modulation: the carrier's output is multiplied by
    /// `1 − g + output`, `g` being the modulator's level (with envelope and
    /// depth): at 0 the carrier is unchanged, at 1 it is the pure product
    /// (sum and difference frequencies, the carrier's own gone), in between
    /// amplitude modulation. Several ring modulators multiply.
    Ring,
}

/// Patch parameters of one operator.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OperatorParams {
    /// Patches saved before waveforms existed default to sine.
    #[cfg_attr(feature = "serde", serde(default))]
    pub waveform: Waveform,
    /// How this operator modulates others. Patches from before default to phase.
    #[cfg_attr(feature = "serde", serde(default))]
    pub mode: ModulationMode,
    /// Which of `ratio`, `semitones` and `frequency` sets the pitch. All three
    /// are kept, so switching back restores the previous value.
    #[cfg_attr(feature = "serde", serde(default))]
    pub pitch_mode: PitchMode,
    /// Frequency as a multiple of the note frequency, 0..=[`MAX_RATIO`]
    /// (0 stops the oscillator: a constant).
    pub ratio: f32,
    /// Transposition of the note in semitones, ±[`MAX_SEMITONES`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub semitones: f32,
    /// Fixed frequency in Hz, 0..=[`MAX_FREQUENCY`].
    #[cfg_attr(feature = "serde", serde(default = "default_frequency"))]
    pub frequency: f32,
    /// Output level 0..=1. As a carrier this is the amplitude, as a modulator
    /// the modulation depth (1.0 = [`MAX_MODULATION`]).
    pub level: f32,
    /// Self-modulation 0..=1 (1.0 = [`MAX_FEEDBACK`]).
    pub feedback: f32,
    /// Whether this operator is heard.
    pub output: bool,
    /// Operators this one modulates, by index. The own index is ignored; use
    /// `feedback` for self-modulation.
    pub modulates: [bool; OPERATORS],
    /// Per target in `modulates`: the share of this operator's output that
    /// modulates it, 0..=1 (1 = the full level, as without depths). In every
    /// modulation mode; it scales the depth, the level stays the operator's.
    #[cfg_attr(feature = "serde", serde(default = "full_depths"))]
    pub depths: [f32; OPERATORS],
    pub envelope: EnvelopeParams,
    /// Balance of a heard operator: -1 = left only, 0 = both at full level,
    /// 1 = right only (the other side is turned down, nothing turned up).
    #[cfg_attr(feature = "serde", serde(default))]
    pub pan: f32,
    /// Level and envelope speed by the played note (DX7 keyboard scaling).
    #[cfg_attr(feature = "serde", serde(default))]
    pub key_scaling: KeyScaling,
    /// Wavefolder on the operator's own waveform, before its level, 0..=1:
    /// up to 1/[`MAX_FOLD`] it fades from the plain wave to a gently
    /// saturated one, above that the gain rises to [`MAX_FOLD`] and the wave
    /// folds back at ±1 again and again (bright, odd harmonics for a sine).
    /// As a modulator it changes the modulating signal. Patches from before
    /// default to 0, off.
    #[cfg_attr(feature = "serde", serde(default))]
    pub fold: f32,
    /// Oscillator or filter, see [`OperatorKind`]. Patches from before are
    /// oscillators.
    #[cfg_attr(feature = "serde", serde(default))]
    pub kind: OperatorKind,
    /// Resonance of a filter operator, 0..=1 (as the voice filter's).
    #[cfg_attr(feature = "serde", serde(default))]
    pub resonance: f32,
}

/// Largest level scaling in dB per octave, both directions.
pub const MAX_KEY_SCALING: f32 = 24.0;

/// Note at which rate scaling leaves the envelope times unchanged (C4).
pub const RATE_SCALING_CENTER: f32 = 60.0;

/// Keyboard scaling of an operator, after the DX7: its level follows the
/// distance of the played note from a break point, separately below and
/// above it, and its envelope runs faster for higher notes. It acts on the
/// operator itself, so on a modulator it scales the modulation depth. Level
/// scaling never raises the level above 1.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct KeyScaling {
    /// MIDI note where the level is unchanged.
    pub break_point: f32,
    /// dB per octave going down from the break point (positive = louder
    /// towards the bass), ±[`MAX_KEY_SCALING`].
    pub left: f32,
    /// dB per octave going up from the break point (negative = quieter
    /// towards the treble), ±[`MAX_KEY_SCALING`].
    pub right: f32,
    /// 0..=1: at 1 the envelope runs twice as fast per octave above
    /// [`RATE_SCALING_CENTER`] and half as fast per octave below.
    pub rate: f32,
}

impl KeyScaling {
    pub const NONE: Self = Self {
        break_point: 60.0,
        left: 0.0,
        right: 0.0,
        rate: 0.0,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to [`Self::NONE`].
    pub fn sanitized(self) -> Self {
        let d = Self::NONE;
        Self {
            break_point: crate::clamp_or(self.break_point, 0.0, 127.0, d.break_point),
            left: crate::clamp_or(self.left, -MAX_KEY_SCALING, MAX_KEY_SCALING, d.left),
            right: crate::clamp_or(self.right, -MAX_KEY_SCALING, MAX_KEY_SCALING, d.right),
            rate: crate::clamp_or(self.rate, 0.0, 1.0, d.rate),
        }
    }

    /// Level factor for `note` (not yet limited to a level of 1).
    pub fn gain(&self, note: f32) -> f32 {
        if self.left == 0.0 && self.right == 0.0 {
            return 1.0;
        }
        let octaves = (note - self.break_point) * (1.0 / 12.0);
        let db = if octaves < 0.0 {
            -octaves * self.left
        } else {
            octaves * self.right
        };
        // 20·log10(2) dB per doubling.
        libm::exp2f(db * (1.0 / 6.020_6))
    }

    /// Envelope speed factor for `note`.
    pub fn speed(&self, note: f32) -> f32 {
        if self.rate == 0.0 {
            return 1.0;
        }
        libm::exp2f(self.rate * (note - RATE_SCALING_CENTER) * (1.0 / 12.0))
    }
}

impl Default for KeyScaling {
    fn default() -> Self {
        Self::NONE
    }
}

#[cfg(feature = "serde")]
fn full_depths() -> [f32; OPERATORS] {
    [1.0; OPERATORS]
}

#[cfg(feature = "serde")]
fn default_frequency() -> f32 {
    OperatorParams::default().frequency
}

impl OperatorParams {
    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            ratio: crate::clamp_or(self.ratio, 0.0, MAX_RATIO, d.ratio),
            semitones: crate::clamp_or(self.semitones, -MAX_SEMITONES, MAX_SEMITONES, d.semitones),
            frequency: crate::clamp_or(self.frequency, 0.0, MAX_FREQUENCY, d.frequency),
            level: crate::clamp_or(self.level, 0.0, 1.0, d.level),
            feedback: crate::clamp_or(self.feedback, 0.0, 1.0, d.feedback),
            depths: self.depths.map(|x| crate::clamp_or(x, 0.0, 1.0, 1.0)),
            envelope: self.envelope.sanitized(),
            pan: crate::clamp_or(self.pan, -1.0, 1.0, d.pan),
            key_scaling: self.key_scaling.sanitized(),
            fold: crate::clamp_or(self.fold, 0.0, 1.0, d.fold),
            resonance: crate::clamp_or(self.resonance, 0.0, 1.0, d.resonance),
            ..self
        }
    }
}

impl Default for OperatorParams {
    fn default() -> Self {
        Self {
            waveform: Waveform::Sine,
            mode: ModulationMode::Phase,
            pitch_mode: PitchMode::Ratio,
            ratio: 1.0,
            semitones: 0.0,
            frequency: 440.0,
            level: 1.0,
            feedback: 0.0,
            output: false,
            modulates: [false; OPERATORS],
            depths: [1.0; OPERATORS],
            envelope: EnvelopeParams::default(),
            pan: 0.0,
            key_scaling: KeyScaling::NONE,
            fold: 0.0,
            kind: OperatorKind::Oscillator,
            resonance: 0.0,
        }
    }
}

/// Runtime state of one operator across all voices, one lane per voice.
pub(crate) struct Operator {
    pub(crate) phase: [u32; MAX_VOICES],
    pub(crate) envelope: [Envelope; MAX_VOICES],
    /// Smoothed ratio, semitones and frequency, chasing the patch values.
    pub(crate) ratio: f32,
    pub(crate) semitones: f32,
    pub(crate) frequency: f32,
    /// Smoothed level, chasing the patch value.
    pub(crate) level: f32,
    /// Smoothed balance, chasing the patch value.
    pub(crate) pan: f32,
    /// Smoothed feedback, chasing the patch value.
    pub(crate) feedback: f32,
    /// Smoothed fold and resonance, chasing the patch values.
    pub(crate) fold: f32,
    pub(crate) resonance: f32,
    /// Smoothed modulation depths per target, chasing the patch values.
    pub(crate) depths: [f32; OPERATORS],
    /// Last two output samples per voice, for modulation and feedback.
    pub(crate) out1: [f32; MAX_VOICES],
    pub(crate) out2: [f32; MAX_VOICES],
    /// Noise generator per voice (`Waveform::Noise`).
    pub(crate) noise: [u32; MAX_VOICES],
    /// Filter state per voice (the SVF's two integrators), for filter operators.
    pub(crate) filter: [[f32; 2]; MAX_VOICES],
}

impl Operator {
    /// Operator `index` of the patch, with `params`.
    pub(crate) fn new(index: usize, params: &OperatorParams) -> Self {
        Self {
            phase: [0; MAX_VOICES],
            envelope: [Envelope::new(); MAX_VOICES],
            ratio: params.ratio,
            semitones: params.semitones,
            frequency: params.frequency,
            level: params.level,
            pan: params.pan,
            feedback: params.feedback,
            fold: params.fold,
            resonance: params.resonance,
            depths: params.depths,
            out1: [0.0; MAX_VOICES],
            out2: [0.0; MAX_VOICES],
            noise: core::array::from_fn(|v| crate::waveform::noise_seed(index, v)),
            filter: [[0.0; 2]; MAX_VOICES],
        }
    }

    /// Silences a voice and restarts it at `phase`, for a newly allocated voice.
    pub(crate) fn reset_voice(&mut self, voice: usize, phase: u32) {
        self.phase[voice] = phase;
        self.envelope[voice].reset();
        self.out1[voice] = 0.0;
        self.out2[voice] = 0.0;
        self.filter[voice] = [0.0; 2];
    }

    /// Moves pitch, level, balance, feedback, fold, resonance and depths a
    /// step towards `params` (once per block) and returns the level, the
    /// balance and the fold before and after.
    pub(crate) fn smooth(&mut self, params: &OperatorParams, amount: f32) -> [(f32, f32); 3] {
        let level_before = self.level;
        let pan_before = self.pan;
        let fold_before = self.fold;
        chase(&mut self.fold, params.fold, amount);
        chase(&mut self.resonance, params.resonance, amount);
        chase(&mut self.pan, params.pan, amount);
        chase(&mut self.ratio, params.ratio, amount);
        chase(&mut self.semitones, params.semitones, amount);
        chase(&mut self.frequency, params.frequency, amount);
        chase(&mut self.level, params.level, amount);
        chase(&mut self.feedback, params.feedback, amount);
        for (depth, &target) in self.depths.iter_mut().zip(&params.depths) {
            chase(depth, target, amount);
        }
        [
            (level_before, self.level),
            (pan_before, self.pan),
            (fold_before, self.fold),
        ]
    }
}
