//! Master effects on the sum of all voices (stereo in, stereo out), in the
//! order of [`EffectParams::order`].
//!
//! Effects have no modulation slots: per-voice sources (envelopes,
//! velocity, key-synced LFOs) have no single value on the sum. Each effect
//! is off at mix 0 (the EQ: at 0 dB on all bands) and then costs nothing;
//! when its mix has glided to 0 it stops, and its old tail is cleared when
//! it starts again. Each effect implements [`Effect`] and smooths its own
//! continuous parameters; [`Slot`] adds the mix and the start and stop.
//!
//! A change of the order fades the output out over one block and in again
//! over the next (about 1.3 ms at 48 kHz) instead of jumping. A non-finite
//! sample (NaN, infinity) silences the block and restarts every effect, so
//! it cannot stay in a feedback loop.

use crate::BLOCK_SIZE;
use crate::bank::Bank;
use crate::chorus::{Chorus, ChorusParams};
use crate::delay::{Delay, DelayParams};
use crate::distortion::{Bitcrusher, BitcrusherParams, Distortion, DistortionParams};
use crate::eq::{Eq, EqParams};
use crate::reverb::{Reverb, ReverbParams};
use crate::smooth;

/// Highest sample rate the effects' fixed buffers are sized for; above it
/// the longest delays and the reverb get shorter.
pub const MAX_SAMPLE_RATE: f32 = 96_000.0;

/// Smoothing time constant for effect parameter changes.
const SMOOTHING_SECONDS: f32 = 0.02;

/// Number of effect kinds; each appears at most once in the chain.
pub const EFFECTS: usize = 6;

/// The effects, in their default order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum EffectKind {
    #[default]
    Distortion,
    Bitcrusher,
    Eq,
    Chorus,
    Delay,
    Reverb,
}

impl EffectKind {
    pub const ALL: [Self; EFFECTS] = [
        Self::Distortion,
        Self::Bitcrusher,
        Self::Eq,
        Self::Chorus,
        Self::Delay,
        Self::Reverb,
    ];
}

/// The order of the effects: a list, so a patch saved before a new effect
/// existed stays valid (the missing one is inserted at its default place,
/// see [`EffectParams::sanitized`]).
pub type EffectOrder = Bank<EffectKind, EFFECTS>;

/// Dry and wet gains for an effect's `mix` (0..=1): up to 0.5 the effect is
/// added at rising level with the dry signal at full level, above 0.5 the
/// dry signal falls until 1 = effect only. Nothing is turned up (like the
/// balance law).
pub fn mix_gains(mix: f32) -> (f32, f32) {
    let mix = mix.clamp(0.0, 1.0);
    ((2.0 - 2.0 * mix).min(1.0), (2.0 * mix).min(1.0))
}

/// Patch parameters of the master effects.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct EffectParams {
    /// Processing order, each kind exactly once (after sanitizing).
    pub order: EffectOrder,
    pub distortion: DistortionParams,
    pub bitcrusher: BitcrusherParams,
    pub eq: EqParams,
    pub chorus: ChorusParams,
    pub delay: DelayParams,
    pub reverb: ReverbParams,
    /// 0..=1: output level after all effects (1 = unchanged). Effects mixed
    /// above 0.5 add their output to a still loud dry signal; this brings
    /// such a patch back below full scale without changing its sound.
    pub level: f32,
}

impl EffectParams {
    /// Clamps all values into their valid range, see the params' `sanitized`.
    /// The order keeps the first occurrence of each kind; a missing kind
    /// goes before the first kind that follows it in the default order
    /// ([`EffectKind::ALL`]), or to the end.
    pub fn sanitized(self) -> Self {
        let mut kinds = [EffectKind::Distortion; EFFECTS];
        let mut len = 0;
        for &kind in self.order.as_slice() {
            if !kinds[..len].contains(&kind) {
                kinds[len] = kind;
                len += 1;
            }
        }
        for (rank, &missing) in EffectKind::ALL.iter().enumerate() {
            if kinds[..len].contains(&missing) {
                continue;
            }
            let later = &EffectKind::ALL[rank + 1..];
            let at = kinds[..len]
                .iter()
                .position(|k| later.contains(k))
                .unwrap_or(len);
            kinds.copy_within(at..len, at + 1);
            kinds[at] = missing;
            len += 1;
        }
        Self {
            order: EffectOrder::from_slice(&kinds[..len]),
            distortion: self.distortion.sanitized(),
            bitcrusher: self.bitcrusher.sanitized(),
            eq: self.eq.sanitized(),
            chorus: self.chorus.sanitized(),
            delay: self.delay.sanitized(),
            reverb: self.reverb.sanitized(),
            level: crate::clamp_or(self.level, 0.0, 1.0, 1.0),
        }
    }
}

impl Default for EffectParams {
    fn default() -> Self {
        Self {
            order: EffectOrder::from_slice(&EffectKind::ALL),
            distortion: DistortionParams::DEFAULT,
            bitcrusher: BitcrusherParams::DEFAULT,
            eq: EqParams::DEFAULT,
            chorus: ChorusParams::DEFAULT,
            delay: DelayParams::DEFAULT,
            reverb: ReverbParams::DEFAULT,
            level: 1.0,
        }
    }
}

/// Coefficient c of a one-pole low-pass y += (x − y)·(1 − c) at `hz`.
pub(crate) use klang_dsp::one_pole::cutoff_coeff as one_pole;

/// A frequency swept exponentially from `from` (x = 0) to `to` (x = 1), for
/// damping and tone controls.
pub(crate) fn sweep(x: f32, from: f32, to: f32) -> f32 {
    from * libm::powf(to / from, x.clamp(0.0, 1.0))
}

/// Dry and wet gains ramped linearly across a block.
pub(crate) struct DryWet {
    dry: (f32, f32),
    wet: (f32, f32),
    inv_n: f32,
}

impl DryWet {
    /// Dry and wet gain at sample `i` of the block.
    #[inline]
    pub(crate) fn at(&self, i: usize) -> (f32, f32) {
        let t = i as f32 * self.inv_n;
        (
            self.dry.0 + (self.dry.1 - self.dry.0) * t,
            self.wet.0 + (self.wet.1 - self.wet.0) * t,
        )
    }
}

/// A master effect with a dry/wet mix.
pub(crate) trait Effect {
    type Params;

    /// Silences the effect and jumps to `params`, for a (re)start.
    fn clear(&mut self, params: &Self::Params);

    /// Processes one block of both channels in place, mixed by `mix`;
    /// continuous parameters chase `params` by `smooth`.
    fn process(
        &mut self,
        params: &Self::Params,
        smooth: f32,
        mix: &DryWet,
        left: &mut [f32],
        right: &mut [f32],
    );
}

/// An effect with its smoothed mix and on/off state.
struct Slot<E> {
    effect: E,
    /// Smoothed mix, chasing the patch value.
    mix: f32,
    /// The effect ran in the previous block.
    running: bool,
}

impl<E: Effect> Slot<E> {
    /// Writes a stopped slot to `this`, with the effect written by `effect`.
    ///
    /// # Safety
    /// As for the effects' `write_new`: `this` must be valid for writes and
    /// aligned, and `effect` must initialize the effect fully.
    unsafe fn write_new(this: *mut Self, effect: impl FnOnce(*mut E)) {
        use core::ptr::addr_of_mut;
        // SAFETY: guaranteed by the caller; every field is written.
        unsafe {
            effect(addr_of_mut!((*this).effect));
            addr_of_mut!((*this).mix).write(0.0);
            addr_of_mut!((*this).running).write(false);
        }
    }

    /// Runs the effect for one block at the smoothed `mix` (towards
    /// `target`), clearing it when it starts; nothing while off.
    fn process(
        &mut self,
        params: &E::Params,
        target: f32,
        smooth: f32,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let start = self.mix;
        smooth::chase(&mut self.mix, target, smooth);
        if start == 0.0 && self.mix == 0.0 {
            self.running = false;
            return;
        }
        if !self.running {
            self.effect.clear(params);
            self.running = true;
        }
        let (dry0, wet0) = mix_gains(start);
        let (dry1, wet1) = mix_gains(self.mix);
        let mix = DryWet {
            dry: (dry0, dry1),
            wet: (wet0, wet1),
            inv_n: 1.0 / left.len().max(1) as f32,
        };
        self.effect.process(params, smooth, &mix, left, right);
    }

    /// Stops the effect at once; it restarts from silence.
    fn stop(&mut self) {
        self.mix = 0.0;
        self.running = false;
    }
}

/// State of the master effects. About 2 MB (the delay lines); see
/// [`Effects::write_new`] for building it in place.
pub struct Effects {
    distortion: Slot<Distortion>,
    bitcrusher: Slot<Bitcrusher>,
    eq: Eq,
    chorus: Slot<Chorus>,
    delay: Slot<Delay>,
    reverb: Slot<Reverb>,
    /// The order in use; `None` until the first block.
    order: Option<EffectOrder>,
    /// The order changed in the previous block, which faded out: fade in.
    fade_in: bool,
    /// Smoothed output level; `None` until the first block.
    level: Option<f32>,
    sample_rate: f32,
}

impl Effects {
    /// Writes silent effects for `sample_rate` to `this` without building
    /// them on the stack.
    ///
    /// # Safety
    /// `this` must be valid for writes and suitably aligned.
    pub unsafe fn write_new(this: *mut Self, sample_rate: f32) {
        use core::ptr::addr_of_mut;
        // SAFETY: guaranteed by the caller; every field is written.
        unsafe {
            Slot::write_new(addr_of_mut!((*this).distortion), |e| {
                e.write(Distortion::new(sample_rate))
            });
            Slot::write_new(addr_of_mut!((*this).bitcrusher), |e| {
                e.write(Bitcrusher::new(sample_rate))
            });
            addr_of_mut!((*this).eq).write(Eq::new(sample_rate));
            Slot::write_new(addr_of_mut!((*this).chorus), |e| {
                Chorus::write_new(e, sample_rate)
            });
            Slot::write_new(addr_of_mut!((*this).delay), |e| {
                Delay::write_new(e, sample_rate)
            });
            Slot::write_new(addr_of_mut!((*this).reverb), |e| {
                Reverb::write_new(e, sample_rate)
            });
            addr_of_mut!((*this).order).write(None);
            addr_of_mut!((*this).fade_in).write(false);
            addr_of_mut!((*this).level).write(None);
            addr_of_mut!((*this).sample_rate).write(sample_rate);
        }
    }

    /// Resets playback state. Each delay line starts from silence on its next use.
    pub(crate) fn reset(&mut self) {
        self.stop();
        self.order = None;
        self.fade_in = false;
        self.chorus.effect.reset_phase();
        self.reverb.effect.reset_phase();
    }

    /// Applies the effects to both channels in place (unclamped). `params`
    /// must be sanitized (each kind once in the order).
    pub fn process(&mut self, params: &EffectParams, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        for (l, r) in left[..frames]
            .chunks_mut(BLOCK_SIZE)
            .zip(right[..frames].chunks_mut(BLOCK_SIZE))
        {
            self.process_block(params, l, r);
        }
    }

    fn process_block(&mut self, params: &EffectParams, l: &mut [f32], r: &mut [f32]) {
        let smooth = smooth::amount(l.len() as f32 / self.sample_rate, SMOOTHING_SECONDS);
        let order = *self.order.get_or_insert(params.order);
        for &kind in order.as_slice() {
            match kind {
                EffectKind::Distortion => {
                    self.distortion
                        .process(&params.distortion, params.distortion.mix, smooth, l, r)
                }
                EffectKind::Bitcrusher => {
                    self.bitcrusher
                        .process(&params.bitcrusher, params.bitcrusher.mix, smooth, l, r)
                }
                EffectKind::Eq => self.eq.process(&params.eq, smooth, l, r),
                EffectKind::Chorus => {
                    self.chorus
                        .process(&params.chorus, params.chorus.mix, smooth, l, r)
                }
                EffectKind::Delay => {
                    self.delay
                        .process(&params.delay, params.delay.mix, smooth, l, r)
                }
                EffectKind::Reverb => {
                    self.reverb
                        .process(&params.reverb, params.reverb.mix, smooth, l, r)
                }
            }
        }
        if self.fade_in {
            self.fade_in = false;
            ramp(l, r, 0.0, 1.0);
        }
        let level = self.level.get_or_insert(params.level);
        let from = *level;
        smooth::chase(level, params.level, smooth);
        if from != 1.0 || *level != 1.0 {
            ramp(l, r, from, *level);
        }
        if params.order != order {
            // Out with the old order; the next block comes in with the new.
            ramp(l, r, 1.0, 0.0);
            self.order = Some(params.order);
            self.fade_in = true;
        }
        if !l.iter().chain(r.iter()).all(|x| x.is_finite()) {
            l.fill(0.0);
            r.fill(0.0);
            self.stop();
        }
    }

    /// Stops every effect; each restarts from silence.
    fn stop(&mut self) {
        self.level = None;
        self.distortion.stop();
        self.bitcrusher.stop();
        self.eq.stop();
        self.chorus.stop();
        self.delay.stop();
        self.reverb.stop();
    }
}

/// Scales both channels by a gain ramped linearly from `from` to `to`
/// across the block.
fn ramp(left: &mut [f32], right: &mut [f32], from: f32, to: f32) {
    let step = (to - from) / left.len().max(1) as f32;
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let gain = from + step * (i + 1) as f32;
        *l *= gain;
        *r *= gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reverb::MAX_TIME;
    use core::f32::consts::TAU;

    const SR: f32 = 48_000.0;

    fn effects(sample_rate: f32) -> Box<Effects> {
        let mut effects = Box::<Effects>::new_uninit();
        // SAFETY: a valid place, fully initialized by `write_new`.
        unsafe {
            Effects::write_new(effects.as_mut_ptr(), sample_rate);
            effects.assume_init()
        }
    }

    fn reverb(mix: f32, time: f32, damping: f32) -> EffectParams {
        EffectParams {
            reverb: ReverbParams {
                mix,
                time,
                pre_delay: 0.0,
                damping,
            },
            ..Default::default()
        }
    }

    /// Runs `input` (mono, both channels) through the effects; returns both
    /// output channels.
    fn run(effects: &mut Effects, params: &EffectParams, input: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (input.to_vec(), input.to_vec());
        for (l, r) in l.chunks_mut(100).zip(r.chunks_mut(100)) {
            effects.process(params, l, r);
        }
        (l, r)
    }

    /// Energy in dB of 10 ms windows.
    fn envelope_db(x: &[f32]) -> Vec<f32> {
        x.chunks(480)
            .map(|w| 10.0 * (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32 + 1e-30).log10())
            .collect()
    }

    #[test]
    fn off_passes_the_signal_unchanged() {
        let mut fx = effects(SR);
        let input: Vec<f32> = (0..5000).map(|i| (i as f32 * 0.01).sin()).collect();
        let (l, r) = run(&mut fx, &EffectParams::default(), &input);
        assert_eq!(l, input);
        assert_eq!(r, input);
    }

    #[test]
    fn reverb_time_matches_the_setting() {
        for time in [0.5, 2.0, 6.0] {
            let mut fx = effects(SR);
            let params = reverb(1.0, time, 0.0);
            // Let the mix glide in on silence, then an impulse.
            run(&mut fx, &params, &[0.0; 4800]);
            let mut input = vec![0.0; (SR * time * 1.5) as usize];
            input[0] = 1.0;
            let (l, r) = run(&mut fx, &params, &input);
            let db: Vec<f32> = envelope_db(&l)
                .iter()
                .zip(envelope_db(&r))
                .map(|(a, b)| a.max(b))
                .collect();
            // Decay rate between -10 dB and -40 dB below the peak, as RT60.
            let peak = db.iter().cloned().fold(f32::MIN, f32::max);
            let at = |level: f32| db.iter().rposition(|&d| d > peak + level).unwrap() as f32 * 0.01;
            let rt60 = (at(-40.0) - at(-10.0)) * 2.0;
            assert!(
                (rt60 / time - 1.0).abs() < 0.3,
                "set {time} s, measured {rt60} s"
            );
        }
    }

    #[test]
    fn output_is_stereo_and_bounded() {
        let mut fx = effects(SR);
        let params = reverb(1.0, MAX_TIME, 0.0);
        // Loud noise for a long time at the longest time.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..(SR as usize * 10))
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect();
        let (l, r) = run(&mut fx, &params, &noise);
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 8.0));
        let dot: f32 = l.iter().zip(&r).map(|(a, b)| a * b).sum();
        let norm =
            (l.iter().map(|a| a * a).sum::<f32>() * r.iter().map(|b| b * b).sum::<f32>()).sqrt();
        assert!(
            (dot / norm).abs() < 0.5,
            "channels correlate by {}",
            dot / norm
        );
    }

    #[test]
    fn restarting_clears_the_old_tail() {
        let mut fx = effects(SR);
        let on = reverb(1.0, MAX_TIME, 0.0);
        let mut input = vec![0.0; 4800];
        input[2400] = 1.0;
        run(&mut fx, &on, &input);
        // Switch off: the mix glides to 0 (about 0.25 s), then the reverb stops.
        run(&mut fx, &EffectParams::default(), &[0.0; 24_000]);
        let (l, r) = run(&mut fx, &on, &[0.0; 9600]);
        assert!(
            l.iter().chain(&r).all(|x| x.abs() < 1e-12),
            "old tail audible"
        );
    }

    #[test]
    fn dry_level_is_kept_up_to_half_mix() {
        assert_eq!(mix_gains(0.0), (1.0, 0.0));
        assert_eq!(mix_gains(0.5), (1.0, 1.0));
        assert_eq!(mix_gains(1.0), (0.0, 1.0));
        assert_eq!(mix_gains(0.25), (1.0, 0.5));
    }

    fn delay(mix: f32, time: f32, feedback: f32, ping_pong: bool) -> EffectParams {
        EffectParams {
            delay: DelayParams {
                mix,
                time,
                feedback,
                damping: 0.0,
                ping_pong,
            },
            ..Default::default()
        }
    }

    /// Positions and levels of the peaks above `threshold` (one per echo).
    fn echoes(x: &[f32], threshold: f32) -> Vec<(usize, f32)> {
        let mut peaks: Vec<(usize, f32)> = Vec::new();
        for (i, &v) in x.iter().enumerate() {
            if v.abs() < threshold {
                continue;
            }
            match peaks.last_mut() {
                Some((at, level)) if i - *at < 100 => {
                    if v.abs() > level.abs() {
                        (*at, *level) = (i, v);
                    }
                }
                _ => peaks.push((i, v)),
            }
        }
        peaks
    }

    /// An impulse after the mix has glided in; returns both channels.
    fn impulse_response(params: &EffectParams, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut fx = effects(SR);
        run(&mut fx, params, &[0.0; 24_000]);
        let mut input = vec![0.0; frames];
        input[0] = 1.0;
        run(&mut fx, params, &input)
    }

    #[test]
    fn delay_echoes_at_the_set_time_and_feedback() {
        let (l, r) = impulse_response(&delay(1.0, 0.1, 0.5, false), 24_000);
        for channel in [&l, &r] {
            let peaks = echoes(channel, 0.01);
            // The dry impulse is gone at mix 1; echoes every 4800 samples.
            assert_eq!(
                peaks.iter().map(|p| p.0).take(3).collect::<Vec<_>>(),
                vec![4800, 9600, 14_400]
            );
            assert!((peaks[0].1 - 1.0).abs() < 0.02, "first echo {}", peaks[0].1);
            assert!(
                (peaks[1].1 - 0.5).abs() < 0.02,
                "second echo {}",
                peaks[1].1
            );
        }
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let (l, r) = impulse_response(&delay(1.0, 0.05, 0.8, true), 12_000);
        let (left, right) = (echoes(&l, 0.01), echoes(&r, 0.01));
        assert_eq!(
            left.iter().map(|p| p.0).collect::<Vec<_>>(),
            vec![2400, 7200]
        );
        assert_eq!(
            right.iter().map(|p| p.0).collect::<Vec<_>>(),
            vec![4800, 9600]
        );
    }

    #[test]
    fn delay_is_bounded_and_glides_without_jumps() {
        let mut fx = effects(SR);
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin()).collect();
        let (l, _) = run(
            &mut fx,
            &delay(0.5, 0.3, crate::delay::MAX_FEEDBACK, false),
            &input,
        );
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < 25.0));
        // Changing the time bends the echoes instead of jumping.
        let (l2, _) = run(
            &mut fx,
            &delay(0.5, 0.05, crate::delay::MAX_FEEDBACK, false),
            &input[..4800],
        );
        let max_step = l2
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        let max_level = l2.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(
            max_step < 0.2 * max_level,
            "step {max_step} at level {max_level}"
        );
    }

    #[test]
    fn order_decides_what_feeds_what() {
        let both = |order: &[EffectKind]| EffectParams {
            order: EffectOrder::from_slice(order),
            delay: DelayParams {
                mix: 0.5,
                time: 0.05,
                feedback: 0.3,
                ..DelayParams::DEFAULT
            },
            reverb: ReverbParams {
                mix: 0.5,
                ..ReverbParams::DEFAULT
            },
            ..Default::default()
        };
        let (a, _) = impulse_response(&both(&[EffectKind::Delay, EffectKind::Reverb]), 9600);
        let (b, _) = impulse_response(&both(&[EffectKind::Reverb, EffectKind::Delay]), 9600);
        assert!(
            a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-3),
            "order makes no difference"
        );
    }

    #[test]
    fn order_is_sanitized_to_each_kind_once() {
        let order = |kinds: &[EffectKind]| {
            EffectParams {
                order: EffectOrder::from_slice(kinds),
                ..Default::default()
            }
            .sanitized()
            .order
        };
        use EffectKind::{Bitcrusher, Chorus, Delay, Distortion, Eq, Reverb};
        assert_eq!(order(&[]).as_slice(), &EffectKind::ALL);
        assert_eq!(
            order(&[Reverb]).as_slice(),
            &[Distortion, Bitcrusher, Eq, Chorus, Delay, Reverb]
        );
        assert_eq!(
            order(&[Reverb, Reverb]).as_slice(),
            &[Distortion, Bitcrusher, Eq, Chorus, Delay, Reverb]
        );
        // A patch from before distortion, bitcrusher and EQ: they go first,
        // the rest keeps its order.
        assert_eq!(
            order(&[Reverb, Chorus, Delay]).as_slice(),
            &[Distortion, Bitcrusher, Eq, Reverb, Chorus, Delay]
        );
        // A patch from before the chorus too (delay after reverb).
        assert_eq!(
            order(&[Reverb, Delay]).as_slice(),
            &[Distortion, Bitcrusher, Eq, Chorus, Reverb, Delay]
        );
        assert_eq!(
            order(&[Delay, Distortion]).as_slice(),
            &[Bitcrusher, Eq, Chorus, Delay, Distortion, Reverb]
        );
        // A patch from before the EQ, with a custom order: the EQ goes
        // before the first kind that follows it by default.
        assert_eq!(
            order(&[Distortion, Chorus, Bitcrusher, Reverb, Delay]).as_slice(),
            &[Distortion, Eq, Chorus, Bitcrusher, Reverb, Delay]
        );
        assert_eq!(
            order(&[Distortion, Bitcrusher, Chorus, Delay, Reverb]).as_slice(),
            &[Distortion, Bitcrusher, Eq, Chorus, Delay, Reverb]
        );
    }

    fn chorus(mix: f32, depth: f32, delay: f32) -> EffectParams {
        EffectParams {
            chorus: ChorusParams {
                mix,
                rate: 0.6,
                depth,
                delay,
            },
            ..Default::default()
        }
    }

    #[test]
    fn chorus_without_depth_is_a_plain_delay() {
        let (l, r) = impulse_response(&chorus(1.0, 0.0, 0.012), 2000);
        for channel in [&l, &r] {
            let peaks = echoes(channel, 0.01);
            assert_eq!(peaks.len(), 1);
            assert_eq!(peaks[0].0, 576);
            // The three taps coincide: √3 (see `chorus`).
            assert!(
                (peaks[0].1 - 3f32.sqrt()).abs() < 1e-3,
                "level {}",
                peaks[0].1
            );
        }
    }

    #[test]
    fn chorus_widens_a_mono_signal_at_about_its_level() {
        let mut fx = effects(SR);
        let params = chorus(1.0, 1.0, 0.012);
        let input: Vec<f32> = (0..96_000)
            .map(|i| (TAU * 220.0 * i as f32 / SR).sin() * 0.5)
            .collect();
        let (l, r) = run(&mut fx, &params, &input);
        let (l, r, input) = (&l[48_000..], &r[48_000..], &input[48_000..]);
        let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
        for channel in [l, r] {
            let ratio = rms(channel) / rms(input);
            assert!((0.6..1.4).contains(&ratio), "level ratio {ratio}");
            assert!(channel.iter().all(|x| x.is_finite() && x.abs() < 1.5));
        }
        assert!(
            l.iter().zip(r).any(|(a, b)| (a - b).abs() > 0.05),
            "left and right are the same"
        );
    }

    #[test]
    fn chorus_delay_glides_without_jumps() {
        let mut fx = effects(SR);
        let input: Vec<f32> = (0..24_000)
            .map(|i| (TAU * 220.0 * i as f32 / SR).sin() * 0.5)
            .collect();
        run(&mut fx, &chorus(1.0, 0.5, 0.005), &input);
        let (l, _) = run(&mut fx, &chorus(1.0, 0.5, 0.03), &input[..4800]);
        let max_step = l
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        // A 220 Hz sine at 0.5 moves at most 0.0145 per sample; a jump would be far more.
        assert!(max_step < 0.03, "step {max_step}");
    }

    fn max_step(x: &[f32]) -> f32 {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    /// Largest step of `before`'s last sample followed by `after`.
    fn step_across(before: &[f32], after: &[f32]) -> f32 {
        let joined: Vec<f32> = before.last().into_iter().chain(after).copied().collect();
        max_step(&joined)
    }

    #[test]
    fn chorus_depth_glides_without_jumps() {
        let mut fx = effects(SR);
        let input = sine_at(220.0, 0.5, 24_000);
        let mut params = chorus(1.0, 0.0, 0.012);
        let (before, _) = run(&mut fx, &params, &input);
        params.chorus.depth = 1.0;
        let (after, _) = run(&mut fx, &params, &input[..4800]);
        let step = step_across(&before, &after);
        assert!(step < 0.03, "step {step}");
    }

    #[test]
    fn changing_the_order_fades_instead_of_jumping() {
        use crate::distortion::DistortionParams;
        let mut fx = effects(SR);
        let input = sine_at(220.0, 0.5, 9600);
        let mut params = reverb(0.5, 2.0, 0.3);
        params.distortion = DistortionParams {
            mix: 1.0,
            drive: 12.0,
            ..DistortionParams::DEFAULT
        };
        run(&mut fx, &params, &input);
        let mut swapped = params;
        swapped.order = EffectOrder::from_slice(&[
            EffectKind::Reverb,
            EffectKind::Bitcrusher,
            EffectKind::Eq,
            EffectKind::Chorus,
            EffectKind::Delay,
            EffectKind::Distortion,
        ]);
        let (l, _) = run(&mut fx, &swapped, &input[..4800]);
        // Distorted sine with a reverb tail; without the fade the switch
        // steps by about the signal's size.
        let natural = max_step(&run(&mut effects(SR), &swapped, &input).0[4800..]);
        assert!(
            max_step(&l) < 1.5 * natural,
            "{} vs {natural}",
            max_step(&l)
        );
        assert!(
            l[..2 * BLOCK_SIZE].iter().any(|&x| x.abs() < 0.05),
            "no dip"
        );
    }

    #[test]
    fn a_nan_does_not_stay_in_the_tail() {
        let mut fx = effects(SR);
        let params = delay_params(0.5, 0.2, 0.9);
        let mut input = sine_at(220.0, 0.5, 4800);
        input[100] = f32::NAN;
        let (l, r) = run(&mut fx, &params, &input);
        assert!(l.iter().chain(&r).all(|x| x.is_finite()));
        let (l, _) = run(&mut fx, &params, &sine_at(220.0, 0.5, 48_000));
        assert!(l.iter().all(|x| x.is_finite()) && l.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn delay_feedback_glides() {
        let mut fx = effects(SR);
        // A constant: the echoes add up to 0.5 / (1 - 0.9) = 5.
        let input = vec![0.5; 96_000];
        run(&mut fx, &delay_params(1.0, 0.25, 0.9), &input);
        let (l, _) = run(&mut fx, &delay_params(1.0, 0.25, 0.0), &input[..24_000]);
        // A quarter second later the echoes stop feeding back: they fall
        // with the feedback's glide instead of dropping to 0.5 at once.
        let step = max_step(&l[11_000..13_000]);
        assert!(step < 0.3, "step {step}");
    }

    fn delay_params(mix: f32, time: f32, feedback: f32) -> EffectParams {
        EffectParams {
            delay: DelayParams {
                mix,
                time,
                feedback,
                ..DelayParams::DEFAULT
            },
            ..Default::default()
        }
    }

    #[test]
    fn level_scales_the_output_and_glides() {
        let mut fx = effects(SR);
        let input = sine_at(220.0, 0.5, 9600);
        let params = EffectParams {
            level: 0.5,
            ..Default::default()
        };
        let (l, _) = run(&mut fx, &params, &input);
        assert!(
            l.iter()
                .zip(&input)
                .all(|(y, x)| (y - 0.5 * x).abs() < 1e-6)
        );
        let (after, _) = run(&mut fx, &EffectParams::default(), &input);
        let step = step_across(&l, &after);
        assert!(step < 0.03, "step {step}");
        assert_eq!(
            EffectParams {
                level: 2.0,
                ..Default::default()
            }
            .sanitized()
            .level,
            1.0
        );
    }

    fn sine_at(hz: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (TAU * hz * i as f32 / SR).sin() * amplitude)
            .collect()
    }

    /// Amplitude of the `hz` component of `x` (correlation with a sine and
    /// a cosine).
    fn amplitude(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let w = (TAU * hz * i as f32 / SR) as f64;
            re += v as f64 * w.cos();
            im += v as f64 * w.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    #[test]
    fn distortion_adds_overtones_at_about_the_same_level() {
        use crate::distortion::{DistortionKind, DistortionParams};
        for kind in [
            DistortionKind::Soft,
            DistortionKind::Hard,
            DistortionKind::Fold,
        ] {
            let mut fx = effects(SR);
            let params = EffectParams {
                distortion: DistortionParams {
                    mix: 1.0,
                    drive: 30.0,
                    kind,
                    tone: 0.0,
                },
                ..Default::default()
            };
            run(&mut fx, &params, &[0.0; 24_000]);
            let (l, _) = run(&mut fx, &params, &sine_at(200.0, 0.5, 48_000));
            let peak = l.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!((0.3..0.75).contains(&peak), "{kind:?}: peak {peak}");
            // Odd harmonics appear (symmetric shapers).
            let third = amplitude(&l, 600.0) / amplitude(&l, 200.0).max(1e-6);
            assert!(third > 0.05, "{kind:?}: third harmonic {third}");
        }
    }

    #[test]
    fn bitcrusher_quantizes_and_holds() {
        use crate::distortion::BitcrusherParams;
        let mut fx = effects(SR);
        let params = EffectParams {
            bitcrusher: BitcrusherParams {
                mix: 1.0,
                bits: 2.0,
                rate: 1000.0,
            },
            ..Default::default()
        };
        run(&mut fx, &params, &[0.0; 24_000]);
        let (l, _) = run(&mut fx, &params, &sine_at(110.0, 0.9, 4800));
        // 2 bits: steps of 0.5.
        assert!(
            l.iter().all(|x| (x * 2.0 - (x * 2.0).round()).abs() < 1e-6),
            "not quantized"
        );
        // 1 kHz out of 48 kHz: each value is held for about 48 samples.
        let changes = l.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(changes <= 4800 / 48 + 1, "{changes} changes");
        assert!(changes > 10, "nothing moves");
    }

    #[test]
    fn fits_every_sample_rate() {
        for sr in [22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let mut fx = effects(sr);
            let mut input = vec![0.0; 1000];
            input[0] = 1.0;
            let (l, _) = run(&mut fx, &reverb(0.5, 1.0, 0.5), &input);
            assert!(l.iter().all(|x| x.is_finite()));
        }
    }

    fn eq(params: EqParams) -> EffectParams {
        EffectParams {
            eq: params,
            ..Default::default()
        }
    }

    /// Gain in dB of the EQ at `hz`, after the gains have glided in.
    fn eq_gain_db(params: EqParams, hz: f32) -> f32 {
        let mut fx = effects(SR);
        let params = eq(params);
        run(&mut fx, &params, &sine_at(hz, 0.25, 9600));
        let (l, r) = run(&mut fx, &params, &sine_at(hz, 0.25, 48_000));
        // Whole periods are not needed: 48 000 samples cover many of them.
        let (gl, gr) = (amplitude(&l, hz), amplitude(&r, hz));
        assert!((gl - gr).abs() < 1e-4, "channels differ at {hz} Hz");
        20.0 * (gl / 0.25).log10()
    }

    #[test]
    fn eq_bands_boost_and_cut_where_set() {
        let low = EqParams {
            low_gain: 12.0,
            ..EqParams::DEFAULT
        };
        let high = EqParams {
            high_gain: -12.0,
            ..EqParams::DEFAULT
        };
        let mid = EqParams {
            mid_gain: 9.0,
            ..EqParams::DEFAULT
        };
        for (params, hz, expected, tolerance) in [
            (low, 40.0, 12.0, 1.0),
            (low, 5000.0, 0.0, 1.0),
            (high, 15_000.0, -12.0, 1.0),
            (high, 100.0, 0.0, 1.0),
            (mid, 1000.0, 9.0, 1.0),
            (mid, 100.0, 0.0, 1.5),
            (mid, 10_000.0, 0.0, 1.5),
        ] {
            let db = eq_gain_db(params, hz);
            assert!(
                (db - expected).abs() < tolerance,
                "{params:?} at {hz} Hz: {db} dB, expected {expected}"
            );
        }
    }

    #[test]
    fn eq_gain_changes_glide_without_jumps() {
        let mut fx = effects(SR);
        let input = sine_at(100.0, 0.5, 4800);
        run(&mut fx, &EffectParams::default(), &input);
        let mut out = Vec::new();
        // Gains up, down, a frequency sweep, back to flat.
        for (gains, freq) in [(15.0, 30.0), (-15.0, 30.0), (-15.0, 1000.0), (0.0, 1000.0)] {
            let params = eq(EqParams {
                low_gain: gains,
                low_freq: freq,
                mid_gain: gains,
                mid_freq: freq,
                high_gain: gains,
                ..EqParams::DEFAULT
            });
            out.extend(run(&mut fx, &params, &input).0);
        }
        // A 100 Hz sine at 0.5, boosted by up to 15 dB (×5.6), moves at most
        // about 0.037 per sample; a jump would be far more.
        let max_step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.06, "step {max_step}");
    }

    #[test]
    fn eq_stays_finite_at_extreme_settings() {
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect();
        for sr in [22_050.0, 48_000.0, 192_000.0] {
            for (gain, q) in [(15.0, crate::eq::MAX_MID_Q), (-15.0, crate::eq::MIN_MID_Q)] {
                let mut fx = effects(sr);
                let params = eq(EqParams {
                    low_gain: gain,
                    low_freq: crate::eq::MAX_LOW_FREQ,
                    mid_gain: gain,
                    mid_freq: crate::eq::MAX_MID_FREQ,
                    mid_q: q,
                    high_gain: gain,
                    high_freq: crate::eq::MAX_HIGH_FREQ,
                });
                let (l, r) = run(&mut fx, &params, &noise);
                assert!(
                    l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 100.0),
                    "{sr} Hz, {gain} dB"
                );
            }
        }
    }

    #[test]
    fn eq_turns_off_and_restarts_clean() {
        let mut fx = effects(SR);
        run(
            &mut fx,
            &eq(EqParams {
                low_gain: 15.0,
                ..EqParams::DEFAULT
            }),
            &sine_at(50.0, 0.5, 4800),
        );
        // Back to flat: once the gains have settled the signal passes unchanged.
        run(&mut fx, &EffectParams::default(), &[0.0; 24_000]);
        let input = sine_at(440.0, 0.5, 4800);
        let (l, r) = run(&mut fx, &EffectParams::default(), &input);
        assert_eq!(l, input);
        assert_eq!(r, input);
        // Restarting on silence: no old state comes out.
        let (l, _) = run(
            &mut fx,
            &eq(EqParams {
                low_gain: 15.0,
                ..EqParams::DEFAULT
            }),
            &[0.0; 4800],
        );
        assert!(l.iter().all(|&x| x == 0.0), "old state audible");
    }

    #[test]
    fn eq_params_are_sanitized() {
        let wild = EqParams {
            low_gain: 40.0,
            low_freq: 5.0,
            mid_gain: f32::NAN,
            mid_freq: 1e6,
            mid_q: 0.0,
            high_gain: -40.0,
            high_freq: f32::INFINITY,
        };
        let s = EffectParams {
            eq: wild,
            ..Default::default()
        }
        .sanitized()
        .eq;
        assert_eq!(
            s,
            EqParams {
                low_gain: 15.0,
                low_freq: 30.0,
                mid_gain: 0.0,
                mid_freq: 10_000.0,
                mid_q: 0.3,
                high_gain: -15.0,
                high_freq: EqParams::DEFAULT.high_freq,
            }
        );
        assert_eq!(EqParams::DEFAULT.sanitized(), EqParams::DEFAULT);
    }
}
