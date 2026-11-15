//! EQ: parametric, six bands (`graph::eq`) and an output gain (WP-90).
//!
//! Each band is one RBJ biquad (low cut and high cut 12 dB/oct with `Q` as resonance, low and high
//! shelf, bell). The coefficients come from [`komponist_model::params::eq_curve`], the same code the
//! graph samples for its curve, so the drawn curve is the filter's response. Frequency, gain and Q
//! glide (log domain, 20 ms) and the coefficients are recomputed every [`CONTROL`] samples, which
//! is independent of the block size. Filters are direct form I (`klang_dsp::biquad::Df1`),
//! the form that stays quiet while coefficients move. A band that is switched off or changes type
//! fades out over about 10 ms first, so neither makes a click; a band with 0 dB gain (bell,
//! shelf) is bit-exact, and so is a flat EQ.

use std::sync::Arc;

use komponist_engine::{SampleTap, tap};
use komponist_i18n::Lang;
use komponist_model::params::layout::{
    Cell, DeviceLayout, GraphBinding, GraphConfig, Half, WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

use komponist_model::params::eq_curve::{self, Coefs};

use super::dsp::{CONTROL, Glide, db_to_gain, finite, smooth_coeff};
use super::{Dsp, INITIAL_RATE, Preset, Spec, knob, single_page, template};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "eq",
    name: "EQ",
    features: &["audio-effect", "equalizer", "filter"],
    params,
    layout,
    dsp,
    presets,
    sidechain: false,
};

/// Bands of the EQ.
pub const BANDS: usize = 6;

/// Band types, the order of `graph::eq`'s `band` constants (low cut, low shelf, bell, high shelf,
/// high cut).
pub const TYPES: [&str; 5] = ["LC", "LS", "Gl.", "HS", "HC"];

/// Frequencies the bands start at; the first [`BANDS_ON`] are on.
const FREQS: [f64; BANDS] = [60.0, 180.0, 420.0, 2400.0, 9000.0, 1000.0];
const BANDS_ON: usize = 5;

/// Range of a band's gain and of the output gain, ± dB.
pub const GAIN_RANGE: f64 = 24.0;

/// Parameters per band (`b{n}_on`, `_type`, `_freq`, `_gain`, `_q`), after the global ones.
const PER_BAND: usize = 5;

/// Wire id of the output gain.
pub const OUT: usize = 0;

/// Wire id of the „Analyse“ switch (WP-92): it comes after the bands, so the ids of every parameter
/// that existed before stay what they were.
pub const ANALYSE: usize = 1 + BANDS * PER_BAND;

/// The factory presets (WP-96): a small starter set.
fn presets() -> &'static [Preset] {
    static LIST: [Preset; 6] = [
        Preset::new("Flat", &[]),
        Preset::new("Low Cut 80 Hz", &[("b1_type", 0.0), ("b1_freq", 80.0)]),
        Preset::new(
            "Vocal Presence",
            &[
                ("b1_type", 0.0),
                ("b1_freq", 90.0),
                ("b3_freq", 300.0),
                ("b3_gain", -2.0),
                ("b4_freq", 3500.0),
                ("b4_gain", 3.0),
                ("b4_q", 1.0),
                ("b5_type", 3.0),
                ("b5_freq", 10000.0),
                ("b5_gain", 2.0),
            ],
        ),
        Preset::new(
            "Warm Bass",
            &[
                ("b1_type", 1.0),
                ("b1_freq", 80.0),
                ("b1_gain", 3.0),
                ("b2_freq", 250.0),
                ("b2_gain", -2.5),
                ("b2_q", 1.2),
            ],
        ),
        Preset::new(
            "Treble Sheen",
            &[("b5_type", 3.0), ("b5_freq", 8000.0), ("b5_gain", 3.0)],
        ),
        Preset::new(
            "Telephone",
            &[
                ("b1_type", 0.0),
                ("b1_freq", 400.0),
                ("b5_type", 4.0),
                ("b5_freq", 3400.0),
            ],
        ),
    ];
    &LIST
}

/// The names of the band types in `lang`, in the order of [`TYPES`].
fn type_names(t: Lang) -> [&'static str; 5] {
    [
        t.choice_eq_type_low_cut(),
        t.choice_eq_type_low_shelf(),
        t.choice_eq_type_bell(),
        t.choice_eq_type_high_shelf(),
        t.choice_eq_type_high_cut(),
    ]
}

fn params(t: Lang) -> Vec<ParamInfo> {
    let mut list = vec![
        ParamInfo::new(
            "out",
            t.param_eq_out(),
            ParamKind::scaled(-GAIN_RANGE, GAIN_RANGE, Scale::Lin),
            0.0,
        )
        .unit(Unit::Db),
    ];
    for n in 1..=BANDS {
        let id = |what: &str| ParamId::new(format!("b{n}_{what}"));
        list.push(ParamInfo::new(
            id("on"),
            t.param_eq_band_on(n),
            ParamKind::Bool,
            f64::from(u8::from(n <= BANDS_ON)),
        ));
        list.push(ParamInfo::new(
            id("type"),
            t.param_eq_band_type(n),
            ParamKind::Enum(type_names(t).iter().map(|n| (*n).to_owned()).collect()),
            2.0,
        ));
        list.push(
            ParamInfo::new(
                id("freq"),
                t.param_eq_band_freq(n),
                ParamKind::scaled(20.0, 20_000.0, Scale::Log),
                FREQS[n - 1],
            )
            .unit(Unit::Hz),
        );
        list.push(
            ParamInfo::new(
                id("gain"),
                t.param_eq_band_gain(n),
                ParamKind::scaled(-GAIN_RANGE, GAIN_RANGE, Scale::Lin),
                0.0,
            )
            .unit(Unit::Db),
        );
        list.push(ParamInfo::new(
            id("q"),
            t.param_eq_band_q(n),
            ParamKind::scaled(0.1, 18.0, Scale::Log),
            0.71,
        ));
    }
    // The live spectrum behind the graph (WP-92). On by default, so a project saved before the
    // parameter existed shows it too.
    list.push(ParamInfo::new(
        "analyse",
        t.param_eq_analyse(),
        ParamKind::Bool,
        1.0,
    ));
    list
}

/// Six columns: the graph over two rows, below it the type and on switch of the selected band and
/// its frequency, gain and Q, and the output gain.
fn layout(_: Lang) -> DeviceLayout {
    let mut binding = GraphBinding::new(GraphConfig::Eq { bands: BANDS as u8 }).selecting("band");
    for n in 1..=BANDS {
        for what in ["on", "freq", "gain", "q", "type"] {
            let id = format!("b{n}_{what}");
            binding = binding.bind(id.clone(), id);
        }
    }
    single_page(
        6,
        vec![
            Cell::graph(0, 0, 6, 2, binding),
            Cell::half(
                0,
                2,
                2,
                Half::Top,
                WidgetKind::Segment,
                template("b{band}_type"),
            ),
            super::toggle(0, 2, 1, Half::Bottom, template("b{band}_on")),
            super::toggle(1, 2, 1, Half::Bottom, super::param("analyse")),
            Cell::span(2, 2, 1, 1, WidgetKind::Knob, template("b{band}_freq")),
            Cell::span(3, 2, 1, 1, WidgetKind::Knob, template("b{band}_gain")),
            Cell::span(4, 2, 1, 1, WidgetKind::Knob, template("b{band}_q")),
            knob(5, 2, "out"),
        ],
    )
}

/// Seconds the frequency, gain and Q take to cover 63 % of a change.
const GLIDE: f32 = 0.020;
/// Seconds of the fade of a band that switches on or off or changes its type.
const FADE: f32 = 0.004;

/// Direct form I state for one channel, in `f64`: at 40 Hz and 44.1 kHz the poles sit at radius
/// 0.994, where `f32` coefficients and state are audibly wrong (3 dB off at 40 Hz with an 80 Hz
/// cut), and `f64` costs nothing at six bands.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Df1 {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Df1 {
    #[inline]
    pub(super) fn tick(&mut self, x: f32, c: &Coefs) -> f32 {
        let x = f64::from(x);
        let y =
            c.b[0] * x + c.b[1] * self.x1 + c.b[2] * self.x2 - c.a[0] * self.y1 - c.a[1] * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y as f32
    }

    /// Keeps the recursive state out of the subnormal range.
    pub(super) fn flush_denormals(&mut self) {
        for v in [&mut self.y1, &mut self.y2] {
            if v.abs() < 1e-30 {
                *v = 0.0;
            }
        }
    }
}

/// One band: parameters as set, smoothed values and the filter.
struct Band {
    on: bool,
    /// Type as set, and the one the filter runs.
    want_type: u32,
    typ: u32,
    /// `ln(freq)`, gain in dB, `ln(q)`.
    freq: Glide,
    gain: Glide,
    q: Glide,
    /// How much of the filtered signal is heard: 0 (off) to 1.
    mix: Glide,
    coefs: Coefs,
    state: [Df1; 2],
    dirty: bool,
}

impl Band {
    fn new(n: usize) -> Band {
        Band {
            on: false,
            want_type: eq_curve::BELL,
            typ: eq_curve::BELL,
            freq: Glide::new(FREQS[n].ln() as f32),
            gain: Glide::new(0.0),
            q: Glide::new(0.71f32.ln()),
            mix: Glide::new(0.0),
            coefs: Coefs::IDENTITY,
            state: [Df1::default(); 2],
            dirty: true,
        }
    }

    /// Whether the filter has to run in this interval.
    fn running(&self) -> bool {
        self.on || self.mix.cur > 0.0 || self.mix.next > 0.0
    }

    fn clear(&mut self) {
        self.state = [Df1::default(); 2];
    }

    /// Starts the next control interval: moves the smoothers and updates the coefficients.
    fn control(&mut self, glide: f32, fade_step: f32, rate: f32) {
        // A type change fades the band out, switches, and fades it in again.
        let switching = self.typ != self.want_type;
        if switching && self.mix.next == 0.0 {
            self.typ = self.want_type;
            self.clear();
            self.dirty = true;
            self.freq.snap();
            self.gain.snap();
            self.q.snap();
        }
        let goal = if self.on && self.typ == self.want_type {
            1.0
        } else {
            0.0
        };
        let from = self.mix.next;
        let next = if goal > from {
            (from + fade_step).min(goal)
        } else {
            (from - fade_step).max(goal)
        };
        self.mix.start_to(next);
        if !self.running() {
            self.clear();
            return;
        }
        let was = (self.freq.next, self.gain.next, self.q.next);
        self.freq.start(glide, 1e-5);
        self.gain.start(glide, 1e-4);
        self.q.start(glide, 1e-5);
        if self.dirty || was != (self.freq.next, self.gain.next, self.q.next) {
            self.coefs = self.compute(rate);
            self.dirty = false;
        }
    }

    fn compute(&self, rate: f32) -> Coefs {
        let gain = f64::from(self.gain.next);
        let flat = matches!(
            self.typ,
            eq_curve::BELL | eq_curve::LOW_SHELF | eq_curve::HIGH_SHELF
        ) && gain == 0.0;
        if flat {
            return Coefs::IDENTITY;
        }
        eq_curve::coefs(
            f64::from(self.typ),
            f64::from(self.freq.next).exp(),
            gain,
            f64::from(self.q.next).exp(),
            f64::from(rate),
        )
    }
}

/// Six RBJ bands in series and the output gain.
struct Eq {
    rate: f32,
    bands: [Band; BANDS],
    out: Glide,
    glide: f32,
    fade_step: f32,
    /// Samples until the next control update, and how far into the interval we are.
    countdown: usize,
    /// The output for the live spectrum (WP-92); the audio side copies into it only while
    /// `analyse` is on and the UI looks at it.
    tap: Arc<SampleTap>,
    analyse: bool,
}

fn dsp() -> Box<dyn Dsp> {
    let mut eq = Eq {
        rate: INITIAL_RATE,
        bands: core::array::from_fn(Band::new),
        out: Glide::new(1.0),
        glide: 0.0,
        fade_step: 0.0,
        countdown: 0,
        tap: Arc::new(SampleTap::new(tap::DEFAULT_CAPACITY)),
        analyse: true,
    };
    eq.set_rate(INITIAL_RATE);
    Box::new(eq)
}

impl Eq {
    fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
        self.tap.set_rate(rate);
        self.glide = smooth_coeff(GLIDE, CONTROL, rate);
        self.fade_step = (CONTROL as f32 / (FADE * rate)).min(1.0);
        for b in &mut self.bands {
            b.dirty = true;
        }
    }

    /// Snaps every smoother onto its target (a fresh or reset EQ has nothing to glide from).
    fn snap(&mut self) {
        for b in &mut self.bands {
            b.freq.snap();
            b.gain.snap();
            b.q.snap();
            b.typ = b.want_type;
            b.mix.jump(if b.on { 1.0 } else { 0.0 });
            b.dirty = true;
            b.clear();
        }
        self.out.snap();
        self.countdown = 0;
    }

    /// Starts a control interval.
    fn control(&mut self) {
        for b in &mut self.bands {
            b.control(self.glide, self.fade_step, self.rate);
        }
        self.out.start(self.glide, 1e-6);
    }
}

impl Dsp for Eq {
    fn activate(&mut self, sample_rate: f32) {
        self.set_rate(sample_rate);
        self.snap();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        if index == OUT {
            self.out.target = db_to_gain(value as f32);
            return;
        }
        if index == ANALYSE {
            self.analyse = value >= 0.5;
            return;
        }
        let i = index - 1;
        let Some(band) = self.bands.get_mut(i / PER_BAND) else {
            return;
        };
        match i % PER_BAND {
            0 => band.on = value >= 0.5,
            1 => band.want_type = value.round().clamp(0.0, 4.0) as u32,
            2 => band.freq.target = (value as f32).ln(),
            3 => band.gain.target = value as f32,
            _ => band.q.target = (value as f32).ln(),
        }
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [il, ir] = input;
        let [ol, or] = output;
        let mut n = 0;
        while n < il.len() {
            if self.countdown == 0 {
                self.control();
                self.countdown = CONTROL;
            }
            let run = self.countdown.min(il.len() - n);
            let done = CONTROL - self.countdown;
            let (left, right) = (&mut ol[n..n + run], &mut or[n..n + run]);
            // NaN and infinity never reach the filters, whose state would keep them.
            for (d, s) in left.iter_mut().zip(&il[n..n + run]) {
                *d = finite(*s);
            }
            for (d, s) in right.iter_mut().zip(&ir[n..n + run]) {
                *d = finite(*s);
            }
            for band in &mut self.bands {
                if !band.running() {
                    continue;
                }
                let steady = band.mix.step == 0.0 && band.mix.cur == 1.0;
                for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                    let yl = band.state[0].tick(*l, &band.coefs);
                    let yr = band.state[1].tick(*r, &band.coefs);
                    if steady {
                        *l = yl;
                        *r = yr;
                    } else {
                        // Computed from the interval start, not accumulated: the same value
                        // whatever the block split.
                        let m = band.mix.at(done + i);
                        *l += (yl - *l) * m;
                        *r += (yr - *r) * m;
                    }
                }
                band.state[0].flush_denormals();
                band.state[1].flush_denormals();
            }
            if !(self.out.step == 0.0 && self.out.cur == 1.0) {
                for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                    let g = self.out.at(done + i);
                    *l *= g;
                    *r *= g;
                }
            }
            self.countdown -= run;
            n += run;
        }
        if self.analyse && self.tap.begin(il.len()) {
            self.tap.push_mono(ol, or);
        }
    }

    fn tap(&self) -> Option<Arc<SampleTap>> {
        Some(Arc::clone(&self.tap))
    }

    fn reset(&mut self) {
        self.snap();
    }
}

const _: () = assert!(PER_BAND == 5);
