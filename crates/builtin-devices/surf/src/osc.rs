//! surf's oscillator: the shared engine of volt ([`komponist_volt::Engine`]) with a wavetable
//! reader instead of the analog waveforms.
//!
//! Per control interval [`Wavetable::prepare`] picks, from the table choice, the position after
//! the matrix and the pitch, the two neighbouring frames and the two band-limited levels
//! ([`Mip`]); per sample [`Wavetable::sample`] reads the four slices linearly at the phase and
//! blends them: between the frames by the position, between the levels by the pitch.
//!
//! Nothing steps at the borders of the intervals: while the frames and levels stay the same the
//! two weights glide from the last interval's values to the new ones across the interval; when
//! they change (the position crosses a frame, the pitch a level, the table is switched) the last
//! interval's slices fade out across the interval while the new ones fade in. Everything is
//! references into the shared [`Bank`](crate::bank::Bank): no allocation, no lock.

use komponist_device_dsp::wavetable::{FRAMES, TABLES};
use komponist_volt::Oscillators;
use komponist_volt::params::id;

use crate::bank::{self, Mip, level_bits};

/// The slices a silent oscillator reads (before its first interval).
static SILENCE: [f32; 257] = [0.0; 257];

/// The four slices an interval reads: `[level][frame]`, and what they are.
#[derive(Clone, Copy, Debug)]
struct Slices {
    /// The lower and the upper frame of the brighter and the darker level.
    at: [[&'static [f32]; 2]; 2],
    /// Right shift that turns a phase into an index, per level.
    shift: [u32; 2],
    /// Table, lower frame, brighter and darker level: equal keys read the same slices.
    key: [u16; 4],
}

impl Slices {
    const SILENT: Slices = Slices {
        at: [[&SILENCE; 2]; 2],
        shift: [24; 2],
        key: [u16::MAX; 4],
    };

    /// The value at `phase` with the frame weight `fw` and the level weight `lw`.
    #[inline]
    fn read(&self, phase: u32, fw: f32, lw: f32) -> f32 {
        let at = |l: usize| {
            let [lo, hi] = self.at[l];
            let a = read(lo, self.shift[l], phase);
            a + (read(hi, self.shift[l], phase) - a) * fw
        };
        let bright = at(0);
        if lw <= 0.0 {
            return bright;
        }
        bright + (at(1) - bright) * lw
    }
}

/// The oscillator of one unison voice for a control interval.
#[derive(Clone, Copy, Debug)]
pub struct TableOsc {
    now: Slices,
    /// Weight of the upper frame at the start and at the end of the interval.
    frame_w: [f32; 2],
    /// Weight of the darker level at the start and at the end of the interval.
    level_w: [f32; 2],
    /// The last interval's slices and their weights, fading out (different slices only).
    fade: Option<(Slices, f32, f32)>,
}

impl Default for TableOsc {
    fn default() -> Self {
        TableOsc {
            now: Slices::SILENT,
            frame_w: [0.0; 2],
            level_w: [0.0; 2],
            fade: None,
        }
    }
}

/// A linear read of `t` (one guard sample at the end) at `phase`.
#[inline]
fn read(t: &[f32], shift: u32, phase: u32) -> f32 {
    let i = (phase >> shift) as usize;
    // The bits under the index as a fraction (24 bits are plenty for f32).
    let frac = ((phase << (32 - shift)) >> 8) as f32 * (1.0 / 16_777_216.0);
    let (a, b) = (t[i], t[i + 1]);
    a + (b - a) * frac
}

/// surf's oscillators.
pub struct Wavetable;

impl Oscillators for Wavetable {
    type Osc = TableOsc;
    /// The position, 0 … 1.
    const SHAPE_RANGE: (f32, f32) = (0.0, 1.0);
    /// 100 % in the matrix sweeps the whole table.
    const SHAPE_MOD: f32 = 1.0;

    fn prepare(prev: &TableOsc, wave: usize, inc: u32, shape: f32, _rate: f32) -> TableOsc {
        let Some(bank) = bank::built() else {
            return TableOsc::default();
        };
        let index = wave.min(TABLES.len() - 1);
        let table = bank.table(index);
        let fp = shape.clamp(0.0, 1.0) * (FRAMES - 1) as f32;
        let f0 = (fp as usize).min(FRAMES - 2);
        let mip = Mip::for_increment(inc);
        let level = |l: usize| [table.frame(f0, l), table.frame(f0 + 1, l)];
        let now = Slices {
            at: [level(mip.a), level(mip.b)],
            shift: [32 - level_bits(mip.a), 32 - level_bits(mip.b)],
            key: [index as u16, f0 as u16, mip.a as u16, mip.b as u16],
        };
        let (fw, lw) = (fp - f0 as f32, mip.w);
        if prev.now.key == now.key {
            TableOsc {
                now,
                frame_w: [prev.frame_w[1], fw],
                level_w: [prev.level_w[1], lw],
                fade: None,
            }
        } else {
            TableOsc {
                now,
                frame_w: [fw; 2],
                level_w: [lw; 2],
                fade: Some((prev.now, prev.frame_w[1], prev.level_w[1])),
            }
        }
    }

    #[inline]
    fn sample(osc: &TableOsc, _wave: usize, phase: u32, t: f32) -> f32 {
        let fw = osc.frame_w[0] + (osc.frame_w[1] - osc.frame_w[0]) * t;
        let lw = osc.level_w[0] + (osc.level_w[1] - osc.level_w[0]) * t;
        let v = osc.now.read(phase, fw, lw);
        match &osc.fade {
            Some((old, ofw, olw)) => {
                let o = old.read(phase, *ofw, *olw);
                o + (v - o) * t
            }
            None => v,
        }
    }

    fn defaults() -> Vec<f64> {
        crate::params::params(komponist_i18n::Lang::De)
            .iter()
            .map(|p| p.default)
            .collect()
    }

    fn tune_a(values: &[f64]) -> (f32, f32) {
        let at = |i: usize| values.get(i).copied().unwrap_or(0.0) as f32;
        (at(crate::params::OSC_A_OCT), at(crate::params::OSC_A_FINE))
    }
}

/// Wire ids of volt's table that surf reads differently: the waveform is the table, the pulse
/// width is the position.
pub mod ids {
    use super::id;
    /// Table of oscillator A (volt: `osc_a_wave`).
    pub const TABLE_A: usize = id::OSC_A_WAVE;
    /// Table of oscillator B.
    pub const TABLE_B: usize = id::OSC_B_WAVE;
    /// Position of oscillator A (volt: `osc_a_pw`).
    pub const POS_A: usize = id::OSC_A_PW;
    /// Position of oscillator B.
    pub const POS_B: usize = id::OSC_B_PW;
}
