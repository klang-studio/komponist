//! Delay: a stereo echo with time and feedback on the XY field, ping-pong and mix. Placeholder
//! DSP: a plain delay line per channel with linear interpolation, a smoothed time and the
//! feedback limited below 1 (no filters, no tempo sync, no modulation; WP-90 brings pisynth's
//! tested delay). At 0 % mix the audio passes bit-exact.

use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

use super::{Dsp, INITIAL_RATE, Spec, knob, knob_large, single_page, toggle};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "delay",
    name: "Delay",
    features: &["audio-effect", "delay", "echo"],
    params,
    layout,
    dsp,
};

/// Longest delay time in milliseconds.
pub const MAX_TIME_MS: f64 = 2000.0;

/// Highest feedback; below 1 so the echo always dies out.
pub const MAX_FEEDBACK: f64 = 0.95;

/// Wire ids.
const TIME: usize = 0;
const FEEDBACK: usize = 1;
const PING_PONG: usize = 2;
const MIX: usize = 3;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(
            "time",
            "Zeit",
            ParamKind::scaled(1.0, MAX_TIME_MS, Scale::Log),
            375.0,
        )
        .unit(Unit::Ms),
        ParamInfo::new(
            "feedback",
            "Feedback",
            ParamKind::lin(0.0, MAX_FEEDBACK),
            0.42,
        )
        .unit(Unit::Percent),
        ParamInfo::new("ping_pong", "Ping-Pong", ParamKind::Bool, 1.0),
        ParamInfo::new("mix", "Mix", ParamKind::lin(0.0, 1.0), 0.3).unit(Unit::Percent),
    ]
}

/// Four columns: the XY field (time × feedback) with the ping-pong switch below, the two values as
/// knobs and the mix.
fn layout() -> DeviceLayout {
    single_page(
        4,
        vec![
            Cell::graph(
                0,
                0,
                2,
                2,
                GraphBinding::new(GraphConfig::Xy)
                    .bind("x", "time")
                    .bind("y", "feedback"),
            ),
            toggle(0, 2, 2, Half::Top, CellParam::Id(ParamId::new("ping_pong"))),
            knob(2, 0, "time"),
            knob(3, 0, "feedback"),
            knob_large(2, 1, "mix"),
        ],
    )
}

/// Seconds the time moves by one second (smoothing); about 30 ms to settle.
const SMOOTH: f32 = 0.002;

struct Delay {
    sample_rate: f32,
    /// One ring per channel, `len` samples each (allocated by `activate`).
    lines: [Vec<f32>; 2],
    write: usize,
    /// Delay in samples now and where it goes.
    time: f32,
    target: f32,
    feedback: f32,
    ping_pong: bool,
    mix: f32,
}

fn dsp() -> Box<dyn Dsp> {
    let mut d = Delay {
        sample_rate: INITIAL_RATE,
        lines: [Vec::new(), Vec::new()],
        write: 0,
        time: 0.0,
        target: 0.0,
        feedback: 0.0,
        ping_pong: true,
        mix: 0.3,
    };
    d.allocate();
    Box::new(d)
}

impl Delay {
    fn allocate(&mut self) {
        let len = (f64::from(self.sample_rate) * MAX_TIME_MS / 1000.0) as usize + 4;
        for line in &mut self.lines {
            line.clear();
            line.resize(len, 0.0);
        }
        self.write = 0;
    }

    fn samples(&self, ms: f64) -> f32 {
        (ms * f64::from(self.sample_rate) / 1000.0) as f32
    }

    /// The line `c` read `delay` samples before the write position, interpolated.
    fn read(&self, c: usize, delay: f32) -> f32 {
        let line = &self.lines[c];
        let len = line.len();
        let whole = delay.floor();
        let frac = delay - whole;
        let a = (self.write + len - whole as usize % len) % len;
        let b = (a + len - 1) % len;
        line[a] + (line[b] - line[a]) * frac
    }
}

impl Dsp for Delay {
    fn activate(&mut self, sample_rate: f32) {
        let ms = f64::from(self.target) * 1000.0 / f64::from(self.sample_rate);
        self.sample_rate = sample_rate;
        self.allocate();
        self.target = self.samples(ms);
        self.time = self.target;
    }

    fn set_param(&mut self, index: usize, value: f64) {
        match index {
            TIME => {
                self.target = self.samples(value).max(1.0);
                if self.time == 0.0 {
                    self.time = self.target;
                }
            }
            FEEDBACK => self.feedback = value as f32,
            PING_PONG => self.ping_pong = value >= 0.5,
            MIX => self.mix = value as f32,
            _ => {}
        }
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [il, ir] = input;
        let [ol, or] = output;
        let len = self.lines[0].len();
        let max = (len - 3) as f32;
        let dry = 1.0 - self.mix;
        for n in 0..il.len() {
            self.time += (self.target - self.time) * SMOOTH;
            let delay = self.time.clamp(1.0, max);
            let (wl, wr) = (self.read(0, delay), self.read(1, delay));
            let (fl, fr) = if self.ping_pong { (wr, wl) } else { (wl, wr) };
            self.lines[0][self.write] = il[n] + fl * self.feedback;
            self.lines[1][self.write] = ir[n] + fr * self.feedback;
            self.write = (self.write + 1) % len;
            ol[n] = il[n] * dry + wl * self.mix;
            or[n] = ir[n] * dry + wr * self.mix;
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.fill(0.0);
        }
        self.time = self.target;
    }
}
