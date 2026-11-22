//! Plate reverb after Jon Dattorro, "Effect Design, Part 1: Reverberator and
//! Other Filters" (J. Audio Eng. Soc., 1997).
//!
//! Mono input (the sum of both channels) → pre-delay → band limit → four
//! input diffusers → a "figure eight" tank of two halves, each a modulated
//! allpass, a delay, damping, an allpass and a delay, feeding the other
//! half. Both outputs are sums of taps inside the tank, so the stereo image
//! comes from the tank itself. The delay lengths are Dattorro's (given at
//! 29761 Hz) scaled to the sample rate.
//!
//! All delay lines share one fixed buffer sized for [`MAX_SAMPLE_RATE`]
//! (above it the reverb gets shorter and smaller);
//! nothing is allocated. Parameters change per block, see [`Effects`].
//!
//! [`Effects`]: crate::effects::Effects

use core::f32::consts::TAU;

use crate::effects::{DryWet, Effect, MAX_SAMPLE_RATE, one_pole, sweep};
use crate::smooth::chase;

/// Ranges of [`ReverbParams`].
pub const MIN_TIME: f32 = 0.2;
pub const MAX_TIME: f32 = 20.0;
pub const MAX_PRE_DELAY: f32 = 0.25;

/// Sample rate of Dattorro's delay lengths.
const DATTORRO_RATE: f32 = 29_761.0;

/// Input diffusers: lengths and coefficients.
const INPUT_DIFFUSERS: [(usize, f32); 4] = [(142, 0.75), (107, 0.75), (379, 0.625), (277, 0.625)];
/// The two tank halves: modulated allpass, delay, allpass, delay.
const TANK: [[usize; 4]; 2] = [[672, 4453, 1800, 3720], [908, 4217, 2656, 3163]];
/// Coefficient of the modulated tank allpasses ("decay diffusion 1"); the
/// negative sign is Dattorro's.
const DECAY_DIFFUSION_1: f32 = -0.7;
/// Peak excursion of the modulated allpasses in samples at 29761 Hz, and
/// their rate.
const EXCURSION: f32 = 16.0;
const MOD_HZ: f32 = 0.9;
/// Output taps per side: (tank half, line within it (1..=3), position,
/// sign). Dattorro's table: left from the right half's lines plus the left
/// half's with negative sign, right the other way round.
const TAPS: [[(usize, usize, usize, f32); 7]; 2] = [
    [
        (1, 1, 266, 1.0),
        (1, 1, 2974, 1.0),
        (1, 2, 1913, -1.0),
        (1, 3, 1996, 1.0),
        (0, 1, 1990, -1.0),
        (0, 2, 187, -1.0),
        (0, 3, 1066, -1.0),
    ],
    [
        (0, 1, 353, 1.0),
        (0, 1, 3627, 1.0),
        (0, 2, 1228, -1.0),
        (0, 3, 2673, 1.0),
        (1, 1, 2111, -1.0),
        (1, 2, 335, -1.0),
        (1, 3, 121, -1.0),
    ],
];
/// Scales the summed taps to about the input level.
const OUTPUT_GAIN: f32 = 0.6;
/// Average time between two passes through a decay multiplier, in Dattorro
/// samples: the tank's full loop passes four of them.
const DECAY_INTERVAL: f32 = (672 + 4453 + 1800 + 3720 + 908 + 4217 + 2656 + 3163) as f32 / 4.0;
/// Cutoff of the input band limit.
const BANDWIDTH_HZ: f32 = 12_000.0;
/// Damping 0..1 moves the tank's low-pass cutoff from this ...
const DAMPING_MAX_HZ: f32 = 20_000.0;
/// ... down to this.
const DAMPING_MIN_HZ: f32 = 600.0;
/// Keeps the tank above the denormal range when the input is silent.
const ANTI_DENORMAL: f32 = 1e-20;

/// Length of a Dattorro delay of `n` samples at [`MAX_SAMPLE_RATE`], plus room
/// for the modulation and rounding.
const fn max_len(n: usize) -> usize {
    (n as f32 * (MAX_SAMPLE_RATE / DATTORRO_RATE)) as usize + 2
}

const fn memory_size() -> usize {
    let mut total = max_len((MAX_PRE_DELAY * DATTORRO_RATE) as usize) + 1;
    let mut i = 0;
    while i < INPUT_DIFFUSERS.len() {
        total += max_len(INPUT_DIFFUSERS[i].0);
        i += 1;
    }
    let mut half = 0;
    while half < 2 {
        let mut j = 0;
        while j < 4 {
            total += max_len(TANK[half][j]);
            j += 1;
        }
        total += max_len(EXCURSION as usize) + 2;
        half += 1;
    }
    total
}

/// Samples of all delay lines together.
const MEMORY: usize = memory_size();

/// Patch parameters of the reverb.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ReverbParams {
    /// 0..=1, see [`crate::effects::mix_gains`]; 0 = off.
    pub mix: f32,
    /// Reverb time (−60 dB) in seconds, [`MIN_TIME`]..=[`MAX_TIME`], without
    /// damping.
    pub time: f32,
    /// Seconds before the reverb starts, 0..=[`MAX_PRE_DELAY`].
    pub pre_delay: f32,
    /// 0..=1: how fast high frequencies die away in the tail.
    pub damping: f32,
}

impl ReverbParams {
    pub const DEFAULT: Self = Self {
        mix: 0.0,
        time: 2.0,
        pre_delay: 0.02,
        damping: 0.3,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            mix: crate::clamp_or(self.mix, 0.0, 1.0, d.mix),
            time: crate::clamp_or(self.time, MIN_TIME, MAX_TIME, d.time),
            pre_delay: crate::clamp_or(self.pre_delay, 0.0, MAX_PRE_DELAY, d.pre_delay),
            damping: crate::clamp_or(self.damping, 0.0, 1.0, d.damping),
        }
    }
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A delay line inside the shared buffer.
#[derive(Clone, Copy, Default)]
struct Line {
    start: usize,
    len: usize,
    /// Next write position, 0..len.
    pos: usize,
    /// The line has wrapped since its last clear.
    filled: bool,
}

impl Line {
    fn clear(&mut self) {
        self.pos = 0;
        self.filled = false;
    }

    /// The sample written `delay` (1..=len) samples ago.
    #[inline]
    fn read(&self, memory: &[f32], delay: usize) -> f32 {
        let i = if self.pos >= delay {
            self.pos - delay
        } else {
            if !self.filled {
                return 0.0;
            }
            self.pos + self.len - delay
        };
        memory[self.start + i]
    }

    /// Like `read`, linearly interpolated for a fractional `delay` (1..len-1).
    #[inline]
    fn read_frac(&self, memory: &[f32], delay: f32) -> f32 {
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let a = self.read(memory, whole);
        let b = self.read(memory, whole + 1);
        a + (b - a) * frac
    }

    #[inline]
    fn write(&mut self, memory: &mut [f32], x: f32) {
        memory[self.start + self.pos] = x;
        self.pos += 1;
        if self.pos == self.len {
            self.pos = 0;
            self.filled = true;
        }
    }

    /// Allpass with coefficient `g` around this line (delay = `len`):
    /// returns the output, stores the internal state.
    #[inline]
    fn allpass(&mut self, memory: &mut [f32], x: f32, g: f32) -> f32 {
        let z = self.read(memory, self.len);
        let w = x + g * z;
        self.write(memory, w);
        z - g * w
    }

    /// Allpass with the delay modulated to `delay` (< `len`).
    #[inline]
    fn allpass_mod(&mut self, memory: &mut [f32], x: f32, g: f32, delay: f32) -> f32 {
        let z = self.read_frac(memory, delay);
        let w = x + g * z;
        self.write(memory, w);
        z - g * w
    }
}

/// Per-block values derived from the parameters.
#[derive(Clone, Copy)]
pub(crate) struct ReverbCoefs {
    decay: f32,
    diffusion_2: f32,
    damping: f32,
    pre_delay: usize,
}

/// State of the reverb: delay lines, filters and modulation.
pub(crate) struct Reverb {
    memory: [f32; MEMORY],
    pre_delay: Line,
    diffusers: [Line; 4],
    /// Per half: modulated allpass, delay, allpass, delay.
    tank: [[Line; 4]; 2],
    /// Scaled tap positions, see `TAPS`.
    taps: [[usize; 7]; 2],
    bandwidth: f32,
    band_state: f32,
    damp_state: [f32; 2],
    /// Output of each half, fed into the other one in the next sample.
    feedback: [f32; 2],
    /// Modulation of the tank allpasses: quadrature oscillator (cos, sin),
    /// rotated by `rotation` per sample.
    osc: (f32, f32),
    rotation: (f32, f32),
    /// Centre and peak deviation of the modulated allpasses' delays.
    mod_centre: [f32; 2],
    excursion: f32,
    /// Smoothed parameters, chasing the patch values once per block.
    smoothed: ReverbParams,
    rate: f32,
}

impl Reverb {
    /// Writes a silent reverb for `sample_rate` to `this` without building it
    /// on the stack (it is several hundred KB).
    ///
    /// # Safety
    /// `this` must be valid for writes and suitably aligned.
    pub(crate) unsafe fn write_new(this: *mut Self, sample_rate: f32) {
        let rate = sample_rate.clamp(1.0, MAX_SAMPLE_RATE);
        let scale = rate / DATTORRO_RATE;
        let scaled = |n: usize| ((n as f32 * scale) as usize).max(1);
        let mut next = 0;
        let mut line = |len: usize| {
            let l = Line {
                start: next,
                len,
                ..Line::default()
            };
            next += len;
            l
        };
        let excursion = EXCURSION * scale;
        let pre_delay = line(((MAX_PRE_DELAY * rate) as usize).max(1) + 1);
        let diffusers = INPUT_DIFFUSERS.map(|(n, _)| line(scaled(n)));
        let tank = TANK.map(|half| {
            let mut lines = half.map(|n| Line {
                len: scaled(n),
                ..Line::default()
            });
            // Room for the modulation beyond the centre delay.
            lines[0].len += excursion as usize + 2;
            lines.map(|l| line(l.len))
        });
        debug_assert!(next <= MEMORY);
        let taps = TAPS.map(|side| side.map(|(_, _, n, _)| scaled(n)));
        let step = TAU * MOD_HZ / rate;
        let mod_centre = TANK.map(|half| scaled(half[0]) as f32);
        // SAFETY: guaranteed by the caller; the memory is written in place.
        unsafe {
            core::ptr::addr_of_mut!((*this).memory).write_bytes(0, 1);
            core::ptr::addr_of_mut!((*this).pre_delay).write(pre_delay);
            core::ptr::addr_of_mut!((*this).diffusers).write(diffusers);
            core::ptr::addr_of_mut!((*this).tank).write(tank);
            core::ptr::addr_of_mut!((*this).taps).write(taps);
            core::ptr::addr_of_mut!((*this).bandwidth).write(one_pole(BANDWIDTH_HZ, rate));
            core::ptr::addr_of_mut!((*this).band_state).write(0.0);
            core::ptr::addr_of_mut!((*this).damp_state).write([0.0; 2]);
            core::ptr::addr_of_mut!((*this).feedback).write([0.0; 2]);
            core::ptr::addr_of_mut!((*this).osc).write((1.0, 0.0));
            core::ptr::addr_of_mut!((*this).rotation).write((libm::cosf(step), libm::sinf(step)));
            core::ptr::addr_of_mut!((*this).mod_centre).write(mod_centre);
            core::ptr::addr_of_mut!((*this).excursion).write(excursion);
            core::ptr::addr_of_mut!((*this).smoothed).write(ReverbParams::DEFAULT);
            core::ptr::addr_of_mut!((*this).rate).write(rate);
        }
    }

    pub(crate) fn reset_phase(&mut self) {
        self.osc = (1.0, 0.0);
    }

    fn coefs(&self, params: &ReverbParams) -> ReverbCoefs {
        let interval = DECAY_INTERVAL / DATTORRO_RATE;
        let decay = libm::powf(10.0, -3.0 * interval / params.time.max(MIN_TIME));
        let cutoff = sweep(params.damping, DAMPING_MAX_HZ, DAMPING_MIN_HZ);
        ReverbCoefs {
            decay,
            diffusion_2: (decay + 0.15).clamp(0.25, 0.5),
            damping: one_pole(cutoff, self.rate),
            pre_delay: ((params.pre_delay * self.rate) as usize).clamp(1, self.pre_delay.len - 1),
        }
    }

    /// Adds the reverb of `left` + `right` to them with the coefficients `c`.
    fn render(&mut self, c: &ReverbCoefs, mix: &DryWet, left: &mut [f32], right: &mut [f32]) {
        let memory = &mut self.memory[..];
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dry_gain, wet_gain) = mix.at(i);
            let wet_gain = wet_gain * OUTPUT_GAIN;

            // Input: pre-delay, band limit, diffusion.
            self.pre_delay.write(memory, 0.5 * (*l + *r));
            let x = self.pre_delay.read(memory, c.pre_delay);
            self.band_state = x + (self.band_state - x) * self.bandwidth;
            let mut x = self.band_state;
            for (line, &(_, g)) in self.diffusers.iter_mut().zip(&INPUT_DIFFUSERS) {
                x = line.allpass(memory, x, g);
            }

            // Tank modulation.
            let (cos, sin) = self.osc;
            self.osc = (
                cos * self.rotation.0 - sin * self.rotation.1,
                sin * self.rotation.0 + cos * self.rotation.1,
            );
            let modulation = [sin, cos];

            let feedback = self.feedback;
            for half in 0..2 {
                let [ap1, d1, ap2, d2] = &mut self.tank[half];
                let input = x + feedback[1 - half] + ANTI_DENORMAL;
                let delay = self.mod_centre[half] + self.excursion * modulation[half];
                let a = ap1.allpass_mod(memory, input, DECAY_DIFFUSION_1, delay);
                d1.write(memory, a);
                let delayed = d1.read(memory, d1.len);
                let state = &mut self.damp_state[half];
                *state = delayed + (*state - delayed) * c.damping;
                let b = ap2.allpass(memory, *state * c.decay, c.diffusion_2);
                d2.write(memory, b);
                self.feedback[half] = d2.read(memory, d2.len) * c.decay;
            }

            // Output taps.
            let mut out = [0.0; 2];
            for (side, taps) in TAPS.iter().enumerate() {
                for (k, &(half, line, _, sign)) in taps.iter().enumerate() {
                    out[side] += sign * self.tank[half][line].read(memory, self.taps[side][k]);
                }
            }
            *l = *l * dry_gain + out[0] * wet_gain;
            *r = *r * dry_gain + out[1] * wet_gain;
        }
        // Keep the oscillator on the unit circle.
        let (cos, sin) = self.osc;
        let norm = 1.0 / libm::sqrtf(cos * cos + sin * sin);
        self.osc = (cos * norm, sin * norm);
    }
}

impl Effect for Reverb {
    type Params = ReverbParams;

    /// Silences the tail and jumps to `params`.
    fn clear(&mut self, params: &ReverbParams) {
        self.pre_delay.clear();
        self.diffusers.iter_mut().for_each(Line::clear);
        self.tank.iter_mut().flatten().for_each(Line::clear);
        self.band_state = 0.0;
        self.damp_state = [0.0; 2];
        self.feedback = [0.0; 2];
        self.smoothed = *params;
    }

    fn process(
        &mut self,
        params: &ReverbParams,
        smooth: f32,
        mix: &DryWet,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let p = &mut self.smoothed;
        chase(&mut p.time, params.time, smooth);
        chase(&mut p.damping, params.damping, smooth);
        // A jump, not a glide: a gliding pre-delay would bend the pitch.
        p.pre_delay = params.pre_delay;
        let coefs = self.coefs(&self.smoothed);
        self.render(&coefs, mix, left, right);
    }
}
