//! Limiter: input gain, a ceiling, release and lookahead; a normal device (also on the master
//! chain) (WP-90).
//!
//! A stereo-linked peak limiter with a hard guarantee: no sample leaves above the ceiling.
//! With **lookahead** the audio is delayed by [`LOOKAHEAD_MS`] and the gain is the sliding minimum
//! of the required gain over that window, smoothed by a moving average of the same length (a
//! linear ramp that is down at the peak). Both stages are at or below the required gain at every
//! sample, so the delayed signal never exceeds the ceiling; the processor reports the delay as
//! its latency ([`Dsp::latency`], so the engine's delay compensation lines the tracks up).
//! Without lookahead the latency is 0 and the gain drops instantly (hard attack). `release` is
//! the time constant of the gain's recovery. A final clamp at the ceiling only acts on rounding.
//! The input gain glides (10 ms). Sample peak only: no true-peak (inter-sample) detection.
//!
//! The deepest gain reduction is available through [`Dsp::take_gain_reduction_db`] for the GR meter.
//!
//! Switching `lookahead` while playing changes the latency: the audio around the switch is cut
//! (the delay line is cleared), and the host has to read [`Dsp::latency`] again for delay
//! compensation.

use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, WidgetKind};
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};

use super::dsp::{CONTROL, Glide, Ring, SlidingMin, db_to_gain, finite, smooth_coeff};
use super::{Dsp, INITIAL_RATE, Spec, knob, knob_large, single_page, toggle};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "limiter",
    name: "Limiter",
    features: &["audio-effect", "limiter", "dynamics"],
    params,
    layout,
    dsp,
};

/// Lookahead (and latency) in milliseconds when `lookahead` is on.
pub const LOOKAHEAD_MS: f64 = 2.0;

/// Wire ids.
const GAIN: usize = 0;
const CEILING: usize = 1;
const RELEASE: usize = 2;
const LOOKAHEAD: usize = 3;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new("gain", "Eingang", ParamKind::lin(0.0, 24.0), 0.0).unit(Unit::Db),
        ParamInfo::new("ceiling", "Ceiling", ParamKind::lin(-12.0, 0.0), -0.3).unit(Unit::Db),
        ParamInfo::new(
            "release",
            "Release",
            ParamKind::scaled(10.0, 1000.0, Scale::Log),
            100.0,
        )
        .unit(Unit::Ms),
        ParamInfo::new("lookahead", "Lookahead", ParamKind::Bool, 1.0),
    ]
}

/// Four columns: input and ceiling as large knobs, release, the lookahead switch below and the
/// gain-reduction meter on the right.
fn layout() -> DeviceLayout {
    single_page(
        4,
        vec![
            knob_large(0, 0, "gain"),
            knob(2, 0, "release"),
            knob_large(0, 1, "ceiling"),
            toggle(0, 2, 2, Half::Top, CellParam::Id("lookahead".into())),
            Cell::span(3, 0, 1, 3, WidgetKind::MeterGr, CellParam::None),
        ],
    )
}

/// Lookahead in frames at `rate`.
fn lookahead_frames(rate: f32) -> usize {
    (f64::from(rate) * LOOKAHEAD_MS / 1000.0).round() as usize
}

struct Limiter {
    rate: f32,
    gain: Glide,
    glide: f32,
    ceiling: f32,
    release_ms: f32,
    release: f32,
    lookahead: bool,
    /// Frames the audio is delayed by now (0 without lookahead).
    delay: usize,
    lines: [Ring; 2],
    /// Sliding minimum of the required gain and the moving average over its output.
    min: SlidingMin,
    avg: Ring,
    avg_sum: f64,
    /// The released gain, applied to the (delayed) audio.
    gr: f32,
    /// Smallest applied gain since the last take.
    deepest: f32,
    countdown: usize,
}

fn dsp() -> Box<dyn Dsp> {
    let mut l = Limiter {
        rate: INITIAL_RATE,
        gain: Glide::new(1.0),
        glide: 0.0,
        ceiling: db_to_gain(-0.3),
        release_ms: 100.0,
        release: 0.0,
        lookahead: true,
        delay: 0,
        lines: [Ring::default(), Ring::default()],
        min: SlidingMin::default(),
        avg: Ring::default(),
        avg_sum: 0.0,
        gr: 1.0,
        deepest: 1.0,
        countdown: 0,
    };
    l.allocate();
    Box::new(l)
}

impl Limiter {
    /// Sizes the buffers for the sample rate (allocates).
    fn allocate(&mut self) {
        let max = lookahead_frames(self.rate).max(1);
        self.lines = [Ring::with_len(max + 2), Ring::with_len(max + 2)];
        self.avg = Ring::with_len(max + 2);
        self.min = SlidingMin::with_capacity(max + 1);
        self.glide = smooth_coeff(0.010, CONTROL, self.rate);
        self.update_release();
        self.gain.snap();
        self.apply_lookahead();
    }

    fn update_release(&mut self) {
        // Per-sample one-pole pole for a time constant of `release_ms`.
        self.release = smooth_coeff(self.release_ms * 0.001, 1, self.rate);
    }

    /// Sets the delay from the `lookahead` switch and clears all history.
    fn apply_lookahead(&mut self) {
        self.delay = if self.lookahead {
            lookahead_frames(self.rate)
        } else {
            0
        };
        for line in &mut self.lines {
            line.clear();
        }
        self.avg.clear();
        self.avg_sum = 0.0;
        // Window of `delay + 1` values: the sample, and the `delay` before it.
        self.min.set_window(self.delay + 1);
        // The average over the window starts at "no reduction".
        for _ in 0..=self.delay {
            self.avg.push(1.0);
        }
        self.avg_sum = (self.delay + 1) as f64;
        self.gr = 1.0;
        self.deepest = 1.0;
        self.countdown = 0;
    }
}

impl Dsp for Limiter {
    fn activate(&mut self, sample_rate: f32) {
        self.rate = sample_rate;
        self.allocate();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        match index {
            GAIN => self.gain.target = db_to_gain(value as f32),
            CEILING => self.ceiling = db_to_gain(value as f32),
            RELEASE => {
                self.release_ms = value as f32;
                self.update_release();
            }
            LOOKAHEAD => {
                let on = value >= 0.5;
                if on != self.lookahead {
                    self.lookahead = on;
                    self.apply_lookahead();
                }
            }
            _ => {}
        }
    }

    fn latency(&self) -> u32 {
        self.delay as u32
    }

    fn take_gain_reduction_db(&mut self) -> f32 {
        let db = 20.0 * self.deepest.max(1e-6).log10();
        self.deepest = 1.0;
        db.min(0.0)
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [il, ir] = input;
        let [ol, or] = output;
        let ceiling = self.ceiling;
        let delay = self.delay;
        let window = (delay + 1) as f64;
        for n in 0..il.len() {
            if self.countdown == 0 {
                self.gain.start(self.glide, 1e-5);
                self.countdown = CONTROL;
            }
            let done = CONTROL - self.countdown;
            self.countdown -= 1;
            let g_in = self.gain.at(done);
            let xl = finite(il[n]) * g_in;
            let xr = finite(ir[n]) * g_in;
            let peak = xl.abs().max(xr.abs());
            let required = if peak > ceiling { ceiling / peak } else { 1.0 };
            // Attack: sliding minimum, then the moving average of the minimum.
            let minimum = self.min.push(required);
            let (yl, yr, smooth) = if delay == 0 {
                (xl, xr, minimum)
            } else {
                self.lines[0].push(xl);
                self.lines[1].push(xr);
                // The audio leaves `delay` frames after it came in. The average of the last
                // `delay + 1` minima, the newest included, only holds minima whose window
                // (`delay + 1` frames) contains that sample, so it is at or below the gain the
                // sample needs.
                let old = self.avg.tap(delay + 1);
                self.avg.push(minimum);
                self.avg_sum += f64::from(minimum) - f64::from(old);
                let avg = (self.avg_sum / window) as f32;
                (
                    self.lines[0].tap(delay + 1),
                    self.lines[1].tap(delay + 1),
                    avg.min(1.0),
                )
            };
            // Release: down at once, up with the time constant.
            self.gr = if smooth < self.gr {
                smooth
            } else {
                smooth + (self.gr - smooth) * self.release
            };
            // `gr` can lag the average above it by rounding only; never above the required gain.
            let g = self.gr.min(smooth);
            self.deepest = self.deepest.min(g);
            ol[n] = (yl * g).clamp(-ceiling, ceiling);
            or[n] = (yr * g).clamp(-ceiling, ceiling);
        }
    }

    fn reset(&mut self) {
        self.gain.snap();
        self.apply_lookahead();
    }
}
