//! Hall: reverb with size, decay, pre-delay, damping, diffusion, width and modulation, three room
//! types, freeze and "Nur Hall" (WP-90).
//!
//! Algorithm: pre-delay, two chains of four allpass filters (diffusion) and an 8-line feedback delay
//! network. The lines have mutually prime lengths (27 to 62 ms for `size` 28 m in the "Halle");
//! their outputs go through a one-pole low-pass (damping) and the gain that gives every line the
//! same decay time (`g = 10^(−3·len / (decay·rate))`), then through an orthonormal 8×8 Hadamard
//! matrix back into the lines, so the loop loses energy only through that gain. The lines are read
//! with linear interpolation at a slowly moving position (eight sine LFOs of 0.17 to 0.71 Hz,
//! up to ±8 samples at 48 kHz) so the tail does not ring. The stereo output sums the damped line
//! outputs with two orthogonal sign patterns.
//!
//! - **size** scales all lengths (`size / 28 m`) times the room's scale; **room** picks a preset of
//!   that scale, a damping factor and the modulation depth (Zimmer: small, bright, calm; Halle;
//!   Kirche: large, darker, livelier). The decay time stays what `decay` says whatever the size.
//! - **freeze** fades the input out, the gain to 1 and the damping off: the tail holds.
//! - **Nur Hall** mutes the dry signal and plays the wet signal at full level (for a send).
//!
//! Parameters glide (control rate [`CONTROL`] samples, linear ramps in between); size and room
//! changes bend the tail like a moving wall instead of clicking. Recursive state is flushed
//! below 1e-20 against denormals.

use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, WidgetKind};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

use super::dsp::{CONTROL, Glide, OnePole, Ring, finite, flush, pole, smooth_coeff};
use super::{Dsp, INITIAL_RATE, Spec, knob, knob_large, single_page, toggle};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "hall",
    name: "Hall",
    features: &["audio-effect", "reverb"],
    params,
    layout,
    dsp,
};

fn params() -> Vec<ParamInfo> {
    let percent = |id: &str, name: &str, default: f64| {
        ParamInfo::new(id, name, ParamKind::lin(0.0, 1.0), default).unit(Unit::Percent)
    };
    vec![
        ParamInfo::new(
            "size",
            "Größe",
            ParamKind::scaled(1.0, 60.0, Scale::Log),
            28.0,
        )
        .unit(Unit::Meters),
        ParamInfo::new(
            "decay",
            "Decay",
            ParamKind::scaled(0.1, 20.0, Scale::Log),
            2.4,
        )
        .unit(Unit::S),
        ParamInfo::new("predelay", "Pre-Delay", ParamKind::lin(0.0, 250.0), 18.0).unit(Unit::Ms),
        ParamInfo::new(
            "damping",
            "Dämpfung",
            ParamKind::scaled(500.0, 20_000.0, Scale::Log),
            5200.0,
        )
        .unit(Unit::Hz),
        percent("diffusion", "Diffusion", 0.7),
        percent("width", "Breite", 0.9),
        percent("modulation", "Modul.", 0.25),
        percent("mix", "Mix", 0.22),
        ParamInfo::new(
            "room",
            "Raum",
            ParamKind::Enum(vec!["Zimmer".into(), "Halle".into(), "Kirche".into()]),
            1.0,
        ),
        ParamInfo::new("freeze", "Einfrieren", ParamKind::Bool, 0.0),
        ParamInfo::new("wet_only", "Nur Hall", ParamKind::Bool, 0.0),
    ]
}

/// Four columns of knobs, no graph.
fn layout() -> DeviceLayout {
    let id = |key: &str| CellParam::Id(ParamId::new(key));
    single_page(
        4,
        vec![
            knob_large(0, 0, "size"),
            knob(2, 0, "decay"),
            knob(3, 0, "predelay"),
            knob(0, 1, "damping"),
            knob(1, 1, "diffusion"),
            knob(2, 1, "width"),
            knob(3, 1, "modulation"),
            knob_large(0, 2, "mix"),
            Cell::half(2, 2, 2, Half::Top, WidgetKind::Segment, id("room")),
            toggle(2, 2, 1, Half::Bottom, id("freeze")),
            toggle(3, 2, 1, Half::Bottom, id("wet_only")),
        ],
    )
}

/// Wire ids.
const SIZE: usize = 0;
const DECAY: usize = 1;
const PREDELAY: usize = 2;
const DAMPING: usize = 3;
const DIFFUSION: usize = 4;
const WIDTH: usize = 5;
const MODULATION: usize = 6;
const MIX: usize = 7;
const ROOM: usize = 8;
const FREEZE: usize = 9;
const WET_ONLY: usize = 10;

/// Lines of the network.
const LINES: usize = 8;
/// Line lengths in samples at 48 kHz for size 28 m and room scale 1 (mutually prime).
const LENGTHS: [f32; LINES] = [
    1327.0, 1523.0, 1759.0, 1993.0, 2251.0, 2473.0, 2711.0, 2953.0,
];
/// LFO rates of the lines in Hz.
const LFO_HZ: [f32; LINES] = [0.17, 0.23, 0.31, 0.37, 0.43, 0.53, 0.61, 0.71];
/// Allpass lengths at 48 kHz of the left and right diffusion chains.
const ALLPASS: [[usize; 4]; 2] = [[142, 107, 379, 277], [151, 113, 401, 293]];
/// Maximum allpass gain at diffusion 100 %.
const ALLPASS_GAIN: f32 = 0.7;
/// Sign patterns of the left and right output.
const OUT_L: [f32; LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const OUT_R: [f32; LINES] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];
/// Level of the wet signal (white noise in, wet and dry about equal at 2.4 s decay).
const WET_GAIN: f32 = 0.35;
/// Input level into the lines.
const INJECT: f32 = 0.5;
/// Modulation depth at 100 %, in samples at 48 kHz.
const MOD_DEPTH: f32 = 8.0;
/// Smallest and largest length scale.
const SCALE_MIN: f32 = 0.08;
const SCALE_MAX: f32 = 4.0;
/// Gain of a frozen loop (a hair below 1: it can only decay).
const FROZEN: f32 = 0.999_995;
/// Longest pre-delay (the parameter's range) in milliseconds.
const PREDELAY_MAX_MS: f32 = 250.0;

/// Room presets: length scale, factor on the damping frequency, modulation factor.
const ROOMS: [(f32, f32, f32); 3] = [(0.45, 1.3, 0.5), (1.0, 1.0, 1.0), (1.7, 0.8, 1.3)];

/// Schroeder allpass with a fixed length.
#[derive(Clone, Debug, Default)]
struct Allpass {
    buf: Vec<f32>,
    at: usize,
}

impl Allpass {
    fn new(len: usize) -> Allpass {
        Allpass {
            buf: vec![0.0; len.max(2)],
            at: 0,
        }
    }

    #[inline]
    fn tick(&mut self, x: f32, g: f32) -> f32 {
        let d = self.buf[self.at];
        let w = flush(x + g * d);
        self.buf[self.at] = w;
        self.at += 1;
        if self.at == self.buf.len() {
            self.at = 0;
        }
        d - g * w
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// In-place orthonormal Hadamard transform of 8 values.
#[inline]
fn hadamard(v: &mut [f32; LINES]) {
    for step in [1, 2, 4] {
        let mut i = 0;
        while i < LINES {
            for j in i..i + step {
                let (a, b) = (v[j], v[j + step]);
                v[j] = a + b;
                v[j + step] = a - b;
            }
            i += 2 * step;
        }
    }
    let k = 1.0 / (LINES as f32).sqrt();
    for x in v.iter_mut() {
        *x *= k;
    }
}

struct Hall {
    rate: f32,
    /// Parameters as set.
    size_m: f32,
    room: usize,
    mix: f32,
    wet_only: bool,
    // Smoothed parameters (glides applied per control interval).
    scale: Glide,
    decay: Glide,
    predelay: Glide,
    damping: Glide,
    diffusion: Glide,
    width: Glide,
    modulation: Glide,
    wet: Glide,
    dry: Glide,
    freeze: Glide,
    slow_coeff: f32,
    glide: f32,
    pre: [Ring; 2],
    allpass: [[Allpass; 4]; 2],
    lines: [Ring; LINES],
    lp: [OnePole; LINES],
    /// Phases of the LFOs in turns.
    phase: [f32; LINES],
    /// Line read positions (samples) and gains per control interval, as ramps.
    pos_cur: [f32; LINES],
    pos_step: [f32; LINES],
    gain: [f32; LINES],
    damp_pole: f32,
    ap_gain: f32,
    inject: Glide,
    countdown: usize,
}

fn dsp() -> Box<dyn Dsp> {
    let mut h = Hall {
        rate: INITIAL_RATE,
        size_m: 28.0,
        room: 1,
        mix: 0.22,
        wet_only: false,
        scale: Glide::new(1.0),
        decay: Glide::new(2.4),
        predelay: Glide::new(18.0),
        damping: Glide::new(5200.0),
        diffusion: Glide::new(0.7),
        width: Glide::new(0.9),
        modulation: Glide::new(0.25),
        wet: Glide::new(0.22),
        dry: Glide::new(0.78),
        freeze: Glide::new(0.0),
        slow_coeff: 0.0,
        glide: 0.0,
        pre: [Ring::default(), Ring::default()],
        allpass: Default::default(),
        lines: Default::default(),
        lp: [OnePole::default(); LINES],
        phase: core::array::from_fn(|i| i as f32 / LINES as f32),
        pos_cur: [0.0; LINES],
        pos_step: [0.0; LINES],
        gain: [0.0; LINES],
        damp_pole: 0.0,
        ap_gain: 0.0,
        inject: Glide::new(1.0),
        countdown: 0,
    };
    h.allocate();
    Box::new(h)
}

impl Hall {
    /// Buffers for the sample rate (allocates).
    fn allocate(&mut self) {
        let k = self.rate / 48_000.0;
        let longest = LENGTHS[LINES - 1] * SCALE_MAX * k + MOD_DEPTH * k + 8.0;
        for line in &mut self.lines {
            *line = Ring::with_len(longest as usize);
        }
        let pre = (PREDELAY_MAX_MS * 0.001 * self.rate) as usize + 4;
        self.pre = [Ring::with_len(pre), Ring::with_len(pre)];
        for (chain, lens) in self.allpass.iter_mut().zip(ALLPASS) {
            for (ap, len) in chain.iter_mut().zip(lens) {
                *ap = Allpass::new((len as f32 * k) as usize);
            }
        }
        self.slow_coeff = smooth_coeff(0.15, CONTROL, self.rate);
        self.glide = smooth_coeff(0.030, CONTROL, self.rate);
        self.retarget_scale();
        self.snap();
    }

    fn retarget_scale(&mut self) {
        let room = ROOMS[self.room.min(ROOMS.len() - 1)];
        self.scale.target = (self.size_m / 28.0 * room.0).clamp(SCALE_MIN, SCALE_MAX);
    }

    fn mix_set(&mut self, mix: Option<f32>, wet_only: Option<bool>) {
        if let Some(m) = mix {
            self.mix = m;
        }
        if let Some(w) = wet_only {
            self.wet_only = w;
        }
        let (wet, dry) = if self.wet_only {
            (1.0, 0.0)
        } else {
            (self.mix, 1.0 - self.mix)
        };
        self.wet.target = wet;
        self.dry.target = dry;
    }

    fn snap(&mut self) {
        for g in [
            &mut self.scale,
            &mut self.decay,
            &mut self.predelay,
            &mut self.damping,
            &mut self.diffusion,
            &mut self.width,
            &mut self.modulation,
            &mut self.wet,
            &mut self.dry,
            &mut self.freeze,
            &mut self.inject,
        ] {
            g.snap();
        }
        self.countdown = 0;
        // The first interval starts from the positions of the snapped values.
        self.begin_positions();
    }

    /// Sets the read positions to what the current (snapped) scale and LFOs give.
    fn begin_positions(&mut self) {
        let k = self.rate / 48_000.0;
        let sc = self.scale.next;
        for i in 0..LINES {
            self.pos_cur[i] = (LENGTHS[i] * sc * k).max(2.0);
            self.pos_step[i] = 0.0;
        }
    }

    /// Starts a control interval: glides the parameters, recomputes gains, poles and positions.
    fn control(&mut self) {
        let (slow, fast) = (self.slow_coeff, self.glide);
        self.scale.start(slow, 1e-4);
        self.decay.start(fast, 1e-4);
        self.predelay.start(fast, 1e-3);
        self.damping.start(fast, 0.5);
        self.diffusion.start(fast, 1e-4);
        self.width.start(fast, 1e-4);
        self.modulation.start(fast, 1e-4);
        self.wet.start(fast, 1e-5);
        self.dry.start(fast, 1e-5);
        self.freeze.start(fast, 1e-4);
        self.inject.start(fast, 1e-4);
        let k = self.rate / 48_000.0;
        let room = ROOMS[self.room.min(ROOMS.len() - 1)];
        let fz = self.freeze.next;
        let decay = self.decay.next.max(0.05);
        let depth = self.modulation.next * MOD_DEPTH * k * room.2;
        let sc = self.scale.next;
        let interval = CONTROL as f32 / self.rate;
        for i in 0..LINES {
            let len = (LENGTHS[i] * sc * k).max(2.0);
            let g = 10f32.powf(-3.0 * len / (decay * self.rate));
            self.gain[i] = g + (FROZEN - g) * fz;
            self.phase[i] = (self.phase[i] + LFO_HZ[i] * interval).fract();
            let lfo = (self.phase[i] * core::f32::consts::TAU).sin();
            let target = (len + depth * lfo).max(2.0);
            let from = self.pos_cur[i] + self.pos_step[i] * CONTROL as f32;
            // Continue from where the last interval ended.
            self.pos_cur[i] = if from.is_finite() { from } else { target };
            self.pos_step[i] = (target - self.pos_cur[i]) / CONTROL as f32;
        }
        let hz = self.damping.next * room.1;
        self.damp_pole = pole(hz.clamp(200.0, 20_000.0), self.rate) * (1.0 - fz);
        self.ap_gain = self.diffusion.next * ALLPASS_GAIN;
    }
}

impl Dsp for Hall {
    fn activate(&mut self, sample_rate: f32) {
        self.rate = sample_rate;
        self.allocate();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        let v = value as f32;
        match index {
            SIZE => {
                self.size_m = v;
                self.retarget_scale();
            }
            ROOM => {
                self.room = value.round().max(0.0) as usize;
                self.retarget_scale();
            }
            DECAY => self.decay.target = v,
            PREDELAY => self.predelay.target = v,
            DAMPING => self.damping.target = v,
            DIFFUSION => self.diffusion.target = v,
            WIDTH => self.width.target = v,
            MODULATION => self.modulation.target = v,
            MIX => self.mix_set(Some(v), None),
            FREEZE => self.freeze.target = if value >= 0.5 { 1.0 } else { 0.0 },
            WET_ONLY => self.mix_set(None, Some(value >= 0.5)),
            _ => {}
        }
        self.inject.target = 1.0 - self.freeze.target;
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [il, ir] = input;
        let [ol, or] = output;
        for n in 0..il.len() {
            if self.countdown == 0 {
                self.control();
                self.countdown = CONTROL;
            }
            let done = CONTROL - self.countdown;
            self.countdown -= 1;
            let (xl, xr) = (finite(il[n]), finite(ir[n]));
            // Pre-delay (reads before the write: a delay of d samples is d).
            let pd = (self.predelay.at(done) * 0.001 * self.rate).max(1.0);
            let (pl, pr) = (self.pre[0].read(pd), self.pre[1].read(pd));
            self.pre[0].push(xl);
            self.pre[1].push(xr);
            // Diffusion.
            let g = self.ap_gain;
            let (mut dl, mut dr) = (pl, pr);
            for a in &mut self.allpass[0] {
                dl = a.tick(dl, g);
            }
            for a in &mut self.allpass[1] {
                dr = a.tick(dr, g);
            }
            let inject = INJECT * self.inject.at(done);
            // The network.
            let mut v = [0.0f32; LINES];
            for i in 0..LINES {
                let pos = self.pos_cur[i] + self.pos_step[i] * done as f32;
                let out = self.lines[i].read(pos);
                v[i] = self.lp[i].lp(out, self.damp_pole) * self.gain[i];
            }
            let (mut wl, mut wr) = (0.0, 0.0);
            for i in 0..LINES {
                wl += v[i] * OUT_L[i];
                wr += v[i] * OUT_R[i];
            }
            hadamard(&mut v);
            for i in 0..LINES {
                let input = if i % 2 == 0 { dl } else { dr };
                self.lines[i].push(flush(v[i] + input * inject));
            }
            let (wl, wr) = (wl * WET_GAIN, wr * WET_GAIN);
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * self.width.at(done);
            let (wet, dry) = (self.wet.at(done), self.dry.at(done));
            ol[n] = xl * dry + (mid + side) * wet;
            or[n] = xr * dry + (mid - side) * wet;
        }
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        for p in &mut self.pre {
            p.clear();
        }
        for a in self.allpass.iter_mut().flatten() {
            a.clear();
        }
        for f in &mut self.lp {
            f.reset();
        }
        self.snap();
    }
}
