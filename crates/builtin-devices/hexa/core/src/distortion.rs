//! Distortion (waveshaper) and bitcrusher, two master effects.
//!
//! Both work sample by sample on each channel and keep little state. The
//! distortion runs at the output rate without oversampling, so strong drive
//! on high notes aliases; its tone low-pass softens that.

use core::f32::consts::FRAC_PI_2;

use klang_dsp::crush::{Quantizer, SampleHold};

use crate::effects::{DryWet, Effect, one_pole, sweep};
use crate::filter::soft_clip;
use crate::smooth::chase;

/// Largest distortion drive in dB.
pub const MAX_DRIVE_DB: f32 = 40.0;

/// Bitcrusher ranges: bits and the held sample rate in Hz.
pub const MIN_BITS: f32 = 1.0;
pub const MAX_BITS: f32 = 16.0;
pub const MIN_CRUSH_RATE: f32 = 200.0;
pub const MAX_CRUSH_RATE: f32 = 48_000.0;

/// Tone 0..1 moves the distortion's low-pass from this ...
const TONE_MAX_HZ: f32 = 20_000.0;
/// ... down to this.
const TONE_MIN_HZ: f32 = 800.0;
/// The distortion keeps the peak of a signal at this level unchanged, so
/// turning the drive up does not mainly make it louder.
const REFERENCE: f32 = 0.5;

/// How the distortion bends the signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DistortionKind {
    /// Gentle saturation (tanh-like), like an overdriven tube or tape.
    #[default]
    Soft,
    /// Hard clipping at ±1: harsh, buzzy.
    Hard,
    /// Wavefolder: beyond ±1 the wave folds back, adding bright, synthetic
    /// overtones that change with the drive.
    Fold,
}

/// Patch parameters of the distortion.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct DistortionParams {
    /// 0..=1, see [`crate::effects::mix_gains`]; 0 = off.
    pub mix: f32,
    /// Gain into the shaper in dB, 0..=[`MAX_DRIVE_DB`].
    pub drive: f32,
    pub kind: DistortionKind,
    /// 0..=1: low-pass after the shaper, 0 = open.
    pub tone: f32,
}

impl DistortionParams {
    pub const DEFAULT: Self = Self {
        mix: 0.0,
        drive: 18.0,
        kind: DistortionKind::Soft,
        tone: 0.3,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            mix: crate::clamp_or(self.mix, 0.0, 1.0, d.mix),
            drive: crate::clamp_or(self.drive, 0.0, MAX_DRIVE_DB, d.drive),
            tone: crate::clamp_or(self.tone, 0.0, 1.0, d.tone),
            ..self
        }
    }
}

impl Default for DistortionParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Patch parameters of the bitcrusher.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct BitcrusherParams {
    /// 0..=1, see [`crate::effects::mix_gains`]; 0 = off.
    pub mix: f32,
    /// Resolution, [`MIN_BITS`]..=[`MAX_BITS`]; fractions give in-between
    /// step sizes.
    pub bits: f32,
    /// The signal is held at this rate in Hz ([`MIN_CRUSH_RATE`]..=
    /// [`MAX_CRUSH_RATE`]; at or above the output rate nothing is held).
    pub rate: f32,
}

impl BitcrusherParams {
    pub const DEFAULT: Self = Self {
        mix: 0.0,
        bits: 8.0,
        rate: 12_000.0,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            mix: crate::clamp_or(self.mix, 0.0, 1.0, d.mix),
            bits: crate::clamp_or(self.bits, MIN_BITS, MAX_BITS, d.bits),
            rate: crate::clamp_or(self.rate, MIN_CRUSH_RATE, MAX_CRUSH_RATE, d.rate),
        }
    }
}

impl Default for BitcrusherParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The shaper curve; odd, and 0 at 0, for all kinds.
#[inline]
fn shape(kind: DistortionKind, x: f32) -> f32 {
    match kind {
        DistortionKind::Soft => soft_clip(x),
        DistortionKind::Hard => x.clamp(-1.0, 1.0),
        // Sine folder: saturates smoothly up to ±1, beyond that the wave
        // folds back again and again.
        DistortionKind::Fold => libm::sinf(x * FRAC_PI_2),
    }
}

/// The largest output of `shape` for inputs up to `amplitude`: the shapes
/// rise monotonically, except the folder, which reaches 1 and folds back.
fn peak(kind: DistortionKind, amplitude: f32) -> f32 {
    match kind {
        DistortionKind::Fold if amplitude >= 1.0 => 1.0,
        _ => shape(kind, amplitude).abs(),
    }
}

/// State of the distortion.
pub(crate) struct Distortion {
    /// Tone low-pass state per channel.
    tone_state: [f32; 2],
    /// Smoothed drive and tone, chasing the patch values once per block.
    drive: f32,
    tone: f32,
    rate: f32,
}

impl Distortion {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let d = DistortionParams::DEFAULT;
        Self {
            tone_state: [0.0; 2],
            drive: d.drive,
            tone: d.tone,
            rate: sample_rate.max(1.0),
        }
    }
}

impl Effect for Distortion {
    type Params = DistortionParams;

    fn clear(&mut self, params: &DistortionParams) {
        self.tone_state = [0.0; 2];
        self.drive = params.drive;
        self.tone = params.tone;
    }

    /// Distorts `left` and `right` in place.
    fn process(
        &mut self,
        params: &DistortionParams,
        smooth: f32,
        mix: &DryWet,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        chase(&mut self.drive, params.drive, smooth);
        chase(&mut self.tone, params.tone, smooth);
        let gain = klang_dsp::units::db_to_gain(self.drive.clamp(0.0, MAX_DRIVE_DB));
        let kind = params.kind;
        // Keeps a peak of REFERENCE where it was.
        let makeup = REFERENCE / peak(kind, REFERENCE * gain).max(1e-3);
        let tone = if self.tone > 0.0 {
            one_pole(sweep(self.tone, TONE_MAX_HZ, TONE_MIN_HZ), self.rate)
        } else {
            0.0
        };
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dry_gain, wet_gain) = mix.at(i);
            for (x, state) in [l, r].into_iter().zip(self.tone_state.iter_mut()) {
                let y = shape(kind, *x * gain) * makeup;
                *state = y + (*state - y) * tone;
                *x = *x * dry_gain + *state * wet_gain;
            }
        }
    }
}

/// State of the bitcrusher.
pub(crate) struct Bitcrusher {
    /// The held, quantized samples.
    hold: SampleHold<2>,
    /// Smoothed bit depth, chasing the patch value once per block.
    bits: f32,
    rate: f32,
}

impl Bitcrusher {
    pub(crate) fn new(sample_rate: f32) -> Self {
        Self {
            hold: SampleHold::default(),
            bits: BitcrusherParams::DEFAULT.bits,
            rate: sample_rate.max(1.0),
        }
    }
}

impl Effect for Bitcrusher {
    type Params = BitcrusherParams;

    fn clear(&mut self, params: &BitcrusherParams) {
        self.hold.reset();
        self.bits = params.bits;
    }

    /// Crushes `left` and `right` in place.
    fn process(
        &mut self,
        params: &BitcrusherParams,
        smooth: f32,
        mix: &DryWet,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        chase(&mut self.bits, params.bits, smooth);
        let quantizer = Quantizer::new(self.bits.clamp(MIN_BITS, MAX_BITS));
        // At most one new sample per sample: rates above the sample rate
        // hold nothing.
        let advance = (params.rate / self.rate).min(1.0);
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dry_gain, wet_gain) = mix.at(i);
            let x = [*l, *r];
            let held = self.hold.tick(advance, || x.map(|x| quantizer.apply(x)));
            *l = *l * dry_gain + held[0] * wet_gain;
            *r = *r * dry_gain + held[1] * wet_gain;
        }
    }
}
