//! Decimation by 2 for oversampled rendering.
//!
//! The operators can run at twice the output rate, so harmonics of strong FM
//! and of saw/square waves between the output's Nyquist frequency and the
//! doubled one do not alias. A half-band FIR low-pass then removes
//! everything above the output band before every second sample is kept.
//!
//! Half-band filters have every other coefficient zero (except the centre,
//! 0.5), so each output sample costs one multiply per non-zero pair. The
//! filter is linear phase with a delay of [`DELAY`] output samples.

/// Half the filter length minus one half: taps run from -`HALF` to `HALF`.
/// Odd, so the outermost taps are non-zero.
const HALF: usize = 35;
const TAPS: usize = 2 * HALF + 1;
/// Non-zero coefficient pairs (odd offsets 1, 3, …, `HALF` from the centre).
const PAIRS: usize = HALF.div_ceil(2);

/// Group delay in output samples, rounded down: the filter's true delay is
/// `HALF / 2 + 0.5`, half a sample more for impulses on odd input samples.
pub const DELAY: usize = HALF / 2;

/// Longest input per call: two samples per output sample of a block.
const MAX_INPUT: usize = 2 * crate::BLOCK_SIZE;

/// Kaiser window shape: about 90 dB stopband attenuation.
const KAISER_BETA: f64 = 9.0;

/// Stateful half-band decimator by 2.
#[derive(Clone)]
pub(crate) struct HalfBand {
    /// Coefficients at offsets ±1, ±3, …; the centre tap is 0.5.
    pairs: [f32; PAIRS],
    /// The last `TAPS - 1` input samples of the previous call, oldest first.
    history: [f32; TAPS - 1],
}

impl HalfBand {
    pub(crate) fn new() -> Self {
        Self {
            pairs: coefficients(),
            history: [0.0; TAPS - 1],
        }
    }

    /// Clears the history (e.g. after switching oversampling on).
    pub(crate) fn reset(&mut self) {
        self.history = [0.0; TAPS - 1];
    }

    /// Filters `input` and keeps every second sample: `output.len()` must be
    /// `input.len() / 2`, and `input.len()` at most `2 * BLOCK_SIZE`.
    pub(crate) fn decimate(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), 2 * output.len());
        debug_assert!(input.len() <= MAX_INPUT);
        let mut buf = [0.0f32; MAX_INPUT + TAPS - 1];
        let len = TAPS - 1 + input.len();
        buf[..TAPS - 1].copy_from_slice(&self.history);
        buf[TAPS - 1..len].copy_from_slice(input);

        for (m, out) in output.iter_mut().enumerate() {
            // Centre of the filter for output m, which lines up with input 2m.
            let c = 2 * m + 1 + HALF;
            let mut y = 0.5 * buf[c];
            for (p, &h) in self.pairs.iter().enumerate() {
                let k = 2 * p + 1;
                y += h * (buf[c - k] + buf[c + k]);
            }
            *out = y;
        }
        self.history.copy_from_slice(&buf[len - (TAPS - 1)..len]);
    }
}

/// Kaiser-windowed sinc with cutoff at a quarter of the input rate. The
/// pairs are scaled for unity gain at DC; the centre stays exactly 0.5.
fn coefficients() -> [f32; PAIRS] {
    let i0_beta = bessel_i0(KAISER_BETA);
    let mut pairs = [0.0f64; PAIRS];
    for (p, pair) in pairs.iter_mut().enumerate() {
        let k = (2 * p + 1) as f64;
        let x = k / HALF as f64;
        let window = bessel_i0(KAISER_BETA * libm::sqrt(1.0 - x * x)) / i0_beta;
        // sin(πk/2) / (πk) for odd k: ±1 / (πk).
        let sign = if p % 2 == 0 { 1.0 } else { -1.0 };
        *pair = sign / (core::f64::consts::PI * k) * window;
    }
    // DC gain 0.5 + 2 Σ pairs = 1.
    let scale = 0.25 / pairs.iter().sum::<f64>();
    pairs.map(|h| (h * scale) as f32)
}

/// Modified Bessel function of the first kind, order 0 (power series).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..50 {
        term *= (half / k as f64) * (half / k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Magnitude response at `f` (fraction of the input rate) in dB.
    fn response_db(pairs: &[f32; PAIRS], f: f64) -> f64 {
        let w = core::f64::consts::TAU * f;
        let mut h = 0.5;
        for (p, &c) in pairs.iter().enumerate() {
            h += 2.0 * c as f64 * libm::cos(w * (2 * p + 1) as f64);
        }
        20.0 * libm::log10(h.abs().max(1e-12))
    }

    #[test]
    fn passband_flat_stopband_deep() {
        let pairs = coefficients();
        // At 96 kHz input: flat to 20 kHz, ≥ 85 dB down from 28 kHz on
        // (which would fold back below 20 kHz).
        for i in 0..=200 {
            let f = 20_000.0 / 96_000.0 * i as f64 / 200.0;
            let db = response_db(&pairs, f);
            assert!(db.abs() < 0.01, "{:.0} Hz: {db:.4} dB", f * 96_000.0);
        }
        for i in 0..=200 {
            let f = (28_000.0 + 20_000.0 * i as f64 / 200.0) / 96_000.0;
            let db = response_db(&pairs, f);
            assert!(db < -85.0, "{:.0} Hz: {db:.1} dB", f * 96_000.0);
        }
    }

    #[test]
    fn streaming_matches_one_shot_and_delays_by_delay() {
        // An impulse comes out as the filter, centred DELAY samples later.
        let mut input = [0.0f32; 64];
        input[0] = 1.0;
        let mut whole = [0.0f32; 32];
        HalfBand::new().decimate(&input, &mut whole);
        let mut split = [0.0f32; 32];
        let mut filter = HalfBand::new();
        filter.decimate(&input[..20], &mut split[..10]);
        filter.decimate(&input[20..], &mut split[10..]);
        assert_eq!(whole, split);
        let peak = (0..32)
            .max_by(|&a, &b| whole[a].abs().total_cmp(&whole[b].abs()))
            .unwrap();
        assert_eq!(peak, DELAY);
        // DC passes with unity gain.
        let mut filter = HalfBand::new();
        let mut out = [0.0f32; 32];
        for _ in 0..4 {
            filter.decimate(&[1.0; 64], &mut out);
        }
        assert!(out.iter().all(|&y| (y - 1.0).abs() < 1e-5), "{out:?}");
    }
}
