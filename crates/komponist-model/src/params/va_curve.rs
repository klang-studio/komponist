//! The maths of the virtual-analog synth „volt“ (WP-95) that its DSP **and** its graphs share, so
//! the filter curve, the signal display and the modulation bar cannot drift away from the sound.
//!
//! The DSP (`komponist_builtin::internal::volt`) takes its filter damping, level compensation,
//! matrix units and source/destination lists from here; the graph configurations
//! (`komponist_ui::widgets::device::graph::configs`) draw the response, the oscillator mix, the LFO
//! shape and the range the matrix moves the cutoff over, from the same functions.
//!
//! Everything is `f64` and allocation-free; frequencies in Hz, levels linear, dB where named.

use std::f64::consts::{PI, TAU};

// ------------------------------------------------------------------------------------------------
// Matrix: sources, destinations, units

/// The matrix sources, in the order of the `modN_src` list (index 0 is "Aus").
pub mod src {
    /// No source: the slot does nothing.
    pub const OFF: usize = 0;
    /// LFO 1, bipolar (unipolar with its `Uni` switch).
    pub const LFO1: usize = 1;
    /// LFO 2.
    pub const LFO2: usize = 2;
    /// The third envelope, 0 … 1.
    pub const ENV3: usize = 3;
    /// Key velocity, 0 … 1.
    pub const VELOCITY: usize = 4;
    /// Key position, −1 (key 0) … +1 (key 120), 0 at middle C.
    pub const KEY: usize = 5;
    /// Mod wheel (CC 1), 0 … 1.
    pub const WHEEL: usize = 6;
    /// Channel pressure, 0 … 1.
    pub const PRESSURE: usize = 7;
    /// One random value per note, −1 … +1 (reproducible: seeded from the note counter).
    pub const RANDOM: usize = 8;
    /// The names the dropdown shows.
    pub const NAMES: [&str; 9] = [
        "Aus", "LFO 1", "LFO 2", "Hk. 3", "Velocity", "Key", "Modrad", "Druck", "Zufall",
    ];

    /// Whether the source swings both ways (`lfo_uni`: that LFO's `Uni` switch).
    pub fn is_bipolar(source: usize, lfo_uni: [bool; 2]) -> bool {
        match source {
            LFO1 => !lfo_uni[0],
            LFO2 => !lfo_uni[1],
            KEY | RANDOM => true,
            _ => false,
        }
    }
}

/// The matrix destinations, in the order of the `modN_dst` list (index 0 is "Aus").
pub mod dst {
    /// No destination.
    pub const OFF: usize = 0;
    /// Pitch of both oscillators.
    pub const PITCH: usize = 1;
    /// Pitch of oscillator B only.
    pub const PITCH_B: usize = 2;
    /// Pulse width of oscillator A.
    pub const PW_A: usize = 3;
    /// Pulse width of oscillator B.
    pub const PW_B: usize = 4;
    /// The balance A/B.
    pub const MIX: usize = 5;
    /// Filter cutoff.
    pub const CUTOFF: usize = 6;
    /// Filter resonance.
    pub const RESONANCE: usize = 7;
    /// Filter drive.
    pub const DRIVE: usize = 8;
    /// Level.
    pub const VOLUME: usize = 9;
    /// Unison detune.
    pub const DETUNE: usize = 10;
    /// Rate of LFO 1.
    pub const LFO1_RATE: usize = 11;
    /// Rate of LFO 2.
    pub const LFO2_RATE: usize = 12;
    /// How many there are (the length of the list and of a per-voice accumulator).
    pub const COUNT: usize = 13;
    /// The names the dropdown shows.
    pub const NAMES: [&str; COUNT] = [
        "Aus",
        "Tonhöhe",
        "Osz B Tonhöhe",
        "Breite A",
        "Breite B",
        "Mix A/B",
        "Cutoff",
        "Resonanz",
        "Drive",
        "Lautstärke",
        "Detune",
        "LFO 1 Rate",
        "LFO 2 Rate",
    ];
}

/// A slot with the amount 100 % moves pitch by this many semitones (two octaves).
pub const MOD_PITCH_ST: f64 = 24.0;
/// … the cutoff by this many octaves.
pub const MOD_CUTOFF_OCT: f64 = 5.0;
/// … a pulse width by this much (of the whole 0 … 1 range).
pub const MOD_PW: f64 = 0.45;
/// … the detune by this many cents.
pub const MOD_DETUNE_CT: f64 = 100.0;
/// … an LFO rate by this many octaves.
pub const MOD_RATE_OCT: f64 = 2.0;
/// The filter envelope with the amount 100 % moves the cutoff by this many octaves.
pub const ENV_CUTOFF_OCT: f64 = 6.0;

// ------------------------------------------------------------------------------------------------
// Filter

/// The filter types in the order of the `filter_type` list.
pub const FILTER_TYPES: [&str; 5] = ["LP 24", "LP 12", "BP 12", "HP 12", "Notch"];
/// `filter_type`: the 4-pole ladder.
pub const LP24: usize = 0;
/// `filter_type`: low-pass of the state-variable filter.
pub const LP12: usize = 1;
/// `filter_type`: band-pass (unity gain at the centre).
pub const BP12: usize = 2;
/// `filter_type`: high-pass.
pub const HP12: usize = 3;
/// `filter_type`: notch.
pub const NOTCH: usize = 4;

/// Feedback gain of the ladder for `resonance` 0 … 1 (it rings by itself around 4).
pub fn ladder_k(resonance: f64) -> f64 {
    4.2 * resonance.clamp(0.0, 1.0)
}

/// The input gain that makes up part of the passband the feedback takes away.
pub fn ladder_comp(k: f64) -> f64 {
    1.0 + 0.5 * k
}

/// Damping `k = 1 / Q` of the state-variable filter for `resonance` 0 … 1: 2 (Q ½) at 0, the
/// Butterworth value near 0.3, and almost undamped at 1.
pub fn svf_k(resonance: f64) -> f64 {
    (2.0 * (1.0 - resonance.clamp(0.0, 1.0))).max(0.02)
}

/// `tan(π f / rate)` with `f` kept within 10 Hz and 0.45 × `rate`, as `klang_dsp::svf::warp`.
pub fn warp(hz: f64, rate: f64) -> f64 {
    (PI * hz.clamp(10.0, 0.45 * rate) / rate).tan()
}

#[derive(Clone, Copy)]
struct C(f64, f64);

impl C {
    fn mul(self, o: C) -> C {
        C(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }
    fn div(self, o: C) -> C {
        let d = o.0 * o.0 + o.1 * o.1;
        C(
            (self.0 * o.0 + self.1 * o.1) / d,
            (self.1 * o.0 - self.0 * o.1) / d,
        )
    }
    fn add(self, o: C) -> C {
        C(self.0 + o.0, self.1 + o.1)
    }
    fn abs(self) -> f64 {
        self.0.hypot(self.1)
    }
}

/// The magnitude in dB of the filter of type `typ` (a `filter_type` index) at `hz`, for the given
/// `cutoff` (Hz) and `resonance` (0 … 1) at `rate`. The same transfer functions as the DSP
/// (linear part: the ladder's `tanh` and the drive are left out).
pub fn filter_response_db(hz: f64, cutoff: f64, resonance: f64, typ: usize, rate: f64) -> f64 {
    let r = warp(hz, rate) / warp(cutoff, rate);
    let jr = C(0.0, r);
    let mag = if typ == LP24 {
        let k = ladder_k(resonance);
        let h1 = C(1.0, 0.0).div(C(1.0, 0.0).add(jr));
        let h4 = h1.mul(h1).mul(h1).mul(h1);
        let den = C(1.0, 0.0).add(C(k * h4.0, k * h4.1));
        ladder_comp(k) * h4.div(den).abs()
    } else {
        let k = svf_k(resonance);
        let den = C(1.0 - r * r, k * r).abs();
        match typ {
            BP12 => k * r / den,
            HP12 => r * r / den,
            NOTCH => (1.0 - r * r).abs() / den,
            _ => 1.0 / den,
        }
    };
    20.0 * mag.max(1e-9).log10()
}

/// How far the matrix and the pressure path can move the cutoff, in octaves, as `(lowest,
/// highest)`: summed over every slot that targets the cutoff, each at the extremes of its source
/// (a bipolar source both ways, a unipolar one from 0).
///
/// `slots` is `[mod1_src, mod1_dst, mod1_amt, … mod4_amt, at_cut, lfo1_uni, lfo2_uni]` (15 values;
/// fewer count as missing, i.e. no movement).
pub fn cutoff_mod_octaves(slots: &[f64]) -> (f64, f64) {
    let at = |i: usize| slots.get(i).copied().unwrap_or(0.0);
    let uni = [at(13) >= 0.5, at(14) >= 0.5];
    let (mut lo, mut hi) = (0.0, 0.0);
    let mut add = |amount: f64, bipolar: bool| {
        let span = amount * MOD_CUTOFF_OCT;
        if bipolar {
            lo -= span.abs();
            hi += span.abs();
        } else {
            lo += span.min(0.0);
            hi += span.max(0.0);
        }
    };
    for slot in 0..4 {
        let (source, dest, amount) = (
            at(3 * slot).round() as usize,
            at(3 * slot + 1).round() as usize,
            at(3 * slot + 2),
        );
        if source != src::OFF && dest == dst::CUTOFF {
            add(amount, src::is_bipolar(source, uni));
        }
    }
    add(at(12), false);
    (lo, hi)
}

// ------------------------------------------------------------------------------------------------
// Oscillators

/// The oscillator waveforms in the order of `osc_a_wave` / `osc_b_wave`.
pub const WAVES: [&str; 4] = ["Sinus", "Dreieck", "Säge", "Rechteck"];
/// `osc_*_wave`: sine.
pub const SINE: usize = 0;
/// `osc_*_wave`: triangle.
pub const TRIANGLE: usize = 1;
/// `osc_*_wave`: rising saw.
pub const SAW: usize = 2;
/// `osc_*_wave`: pulse.
pub const PULSE: usize = 3;

/// The sub oscillator's level at 100 % (a sine one octave under oscillator A).
pub const SUB_GAIN: f64 = 0.7;

/// Equal-power gains `(a, b)` for the balance `mix` 0 (only A) … 1 (only B).
pub fn mix_gains(mix: f64) -> (f64, f64) {
    let angle = mix.clamp(0.0, 1.0) * PI / 2.0;
    (angle.cos(), angle.sin())
}

/// One sample of an oscillator waveform at `phase` (0 … 1), not band-limited (the display).
pub fn wave_naive(wave: usize, phase: f64, pulse_width: f64) -> f64 {
    let t = phase - phase.floor();
    match wave {
        SINE => (TAU * t).sin(),
        TRIANGLE => 4.0 * (t - 0.5).abs() - 1.0,
        SAW => 2.0 * t - 1.0,
        _ => {
            if t < pulse_width {
                1.0
            } else {
                -1.0
            }
        }
    }
}

/// The periods of oscillator A the signal display spans.
pub const SIGNAL_PERIODS: f64 = 2.0;

/// The mix of both oscillators and the sub at `x` periods of oscillator A, scaled to fit −1 … 1.
/// `p` is `[osc_a_wave, osc_b_wave, osc_a_pw, osc_b_pw, osc_b_oct, osc_b_fine, osc_mix,
/// sub_level]`.
pub fn signal_at(x: f64, p: &[f64]) -> f64 {
    let at = |i: usize| p.get(i).copied().unwrap_or(0.0);
    let (ga, gb) = mix_gains(at(6));
    let ratio = ((at(4) * 1200.0 + at(5)) / 1200.0).exp2();
    let a = wave_naive(at(0).round() as usize, x, at(2));
    let b = wave_naive(at(1).round() as usize, x * ratio, at(3));
    let sub = (TAU * x * 0.5).sin();
    let total = ga + gb + SUB_GAIN * at(7);
    (ga * a + gb * b + SUB_GAIN * at(7) * sub) / total.max(1e-9)
}

// ------------------------------------------------------------------------------------------------
// LFO

/// The LFO shapes in the order of `lfoN_shape`.
pub const LFO_SHAPES: [&str; 6] = ["Sinus", "Dreieck", "Säge", "Rechteck", "Zufall", "Glatt"];
/// `lfoN_shape`: sample and hold.
pub const LFO_RANDOM: usize = 4;
/// `lfoN_shape`: smoothed random.
pub const LFO_SMOOTH: usize = 5;

/// The beats (quarter notes) one cycle of an LFO lasts for each `lfoN_div` entry.
pub const LFO_DIV_BEATS: [f64; 8] = [16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125];
/// The names of the `lfoN_div` entries.
pub const LFO_DIVS: [&str; 8] = [
    "4 Takte", "2 Takte", "1 Takt", "1/2", "1/4", "1/8", "1/16", "1/32",
];

/// The values the display uses for the two random shapes: a fixed, readable sequence.
const LFO_RANDOM_VALUES: [f64; 5] = [0.6, -0.5, 0.9, -0.8, 0.3];

/// One cycle of LFO shape `shape` at `phase` 0 … 1, −1 … 1 (the display; the DSP uses the same
/// formulas on its own phase).
pub fn lfo_shape_at(shape: usize, phase: f64) -> f64 {
    let t = phase - phase.floor();
    match shape {
        0 => (TAU * t).sin(),
        1 => 1.0 - 4.0 * ((t + 0.25).fract() - 0.5).abs(),
        2 => 2.0 * t - 1.0,
        3 => {
            if t < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        LFO_RANDOM => LFO_RANDOM_VALUES[((t * 5.0) as usize).min(4)],
        _ => {
            let n = LFO_RANDOM_VALUES.len();
            let x = t * n as f64;
            let i = (x as usize).min(n - 1);
            let f = x - i as f64;
            let (a, b) = (LFO_RANDOM_VALUES[i], LFO_RANDOM_VALUES[(i + 1) % n]);
            let s = f * f * (3.0 - 2.0 * f);
            a + (b - a) * s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    #[test]
    fn the_lowpass_is_flat_below_and_falls_above_the_cutoff() {
        for typ in [LP24, LP12] {
            let low = filter_response_db(50.0, 1000.0, 0.0, typ, RATE);
            let high = filter_response_db(8000.0, 1000.0, 0.0, typ, RATE);
            assert!(low.abs() < 0.5, "{typ}: {low}");
            assert!(high < -30.0, "{typ}: {high}");
        }
        // 24 dB/oct falls twice as fast as 12.
        let l24 = filter_response_db(4000.0, 1000.0, 0.0, LP24, RATE);
        let l12 = filter_response_db(4000.0, 1000.0, 0.0, LP12, RATE);
        assert!(l24 < l12 - 10.0, "{l24} {l12}");
    }

    #[test]
    fn resonance_peaks_at_the_cutoff() {
        let flat = filter_response_db(1000.0, 1000.0, 0.0, LP12, RATE);
        let peak = filter_response_db(1000.0, 1000.0, 0.9, LP12, RATE);
        assert!(peak > flat + 10.0, "{flat} {peak}");
        let ladder = filter_response_db(1000.0, 1000.0, 0.9, LP24, RATE);
        let ladder_flat = filter_response_db(1000.0, 1000.0, 0.0, LP24, RATE);
        assert!(ladder > ladder_flat + 3.0, "{ladder_flat} {ladder}");
    }

    #[test]
    fn the_other_types_have_their_shapes() {
        assert!(filter_response_db(100.0, 1000.0, 0.3, HP12, RATE) < -30.0);
        assert!(filter_response_db(10_000.0, 1000.0, 0.3, HP12, RATE).abs() < 1.0);
        assert!(filter_response_db(1000.0, 1000.0, 0.3, NOTCH, RATE) < -40.0);
        assert!(filter_response_db(1000.0, 1000.0, 0.3, BP12, RATE).abs() < 0.2);
        assert!(filter_response_db(100.0, 1000.0, 0.3, BP12, RATE) < -15.0);
    }

    #[test]
    fn the_cutoff_range_adds_up_the_slots_that_target_it() {
        // LFO 1 (bipolar) → cutoff 20 %, velocity → cutoff 40 %, pressure −50 %.
        let slots = [
            1.0, 6.0, 0.2, 4.0, 6.0, 0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.5, 0.0, 0.0,
        ];
        let (lo, hi) = cutoff_mod_octaves(&slots);
        assert!((lo - (-1.0 + 0.0 - 2.5)).abs() < 1e-9, "{lo}");
        assert!((hi - (1.0 + 2.0)).abs() < 1e-9, "{hi}");
        // A unipolar LFO only goes one way; a slot to another target does not count.
        let mut uni = slots;
        uni[13] = 1.0;
        assert!((cutoff_mod_octaves(&uni).0 - (0.0 - 2.5)).abs() < 1e-9);
        let mut other = slots;
        other[1] = 7.0;
        assert!((cutoff_mod_octaves(&other).1 - 2.0).abs() < 1e-9);
        assert_eq!(cutoff_mod_octaves(&[]), (0.0, 0.0));
    }

    #[test]
    fn the_signal_fits_the_display_and_follows_the_mix() {
        // [a wave, b wave, a pw, b pw, b oct, b fine, mix, sub]
        let saw_only = [2.0, 3.0, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0];
        assert!((signal_at(0.25, &saw_only) + 0.5).abs() < 1e-9);
        assert!((signal_at(0.75, &saw_only) - 0.5).abs() < 1e-9);
        for x in 0..200 {
            let y = signal_at(
                f64::from(x) * 0.01,
                &[2.0, 3.0, 0.5, 0.5, 0.0, 7.0, 0.5, 1.0],
            );
            assert!(y.abs() <= 1.0 + 1e-9, "{y}");
        }
    }

    #[test]
    fn the_lfo_shapes_start_where_the_dsp_starts() {
        assert_eq!(lfo_shape_at(0, 0.0), 0.0);
        assert!((lfo_shape_at(0, 0.25) - 1.0).abs() < 1e-12);
        assert!((lfo_shape_at(1, 0.25) - 1.0).abs() < 1e-12);
        assert_eq!(lfo_shape_at(2, 0.0), -1.0);
        assert_eq!(lfo_shape_at(3, 0.1), 1.0);
        assert_eq!(lfo_shape_at(3, 0.6), -1.0);
        for shape in 0..6 {
            for i in 0..100 {
                let y = lfo_shape_at(shape, f64::from(i) / 100.0);
                assert!((-1.0..=1.0).contains(&y), "{shape} {y}");
            }
        }
    }
}
