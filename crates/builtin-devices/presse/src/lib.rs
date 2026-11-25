//! Compressor „presse“ (WP-93): a stereo feed-forward compressor with a log-domain gain computer,
//! soft knee, attack and release smoothing, auto-release, Peak or RMS detection, stereo link,
//! 3 ms lookahead, a sidechain high-pass with „SC hören“, makeup (manual or automatic) and a
//! dry/wet mix for parallel compression.
//!
//! **Signal flow** (per sample): the input feeds the detector through the sidechain high-pass
//! (2nd order, 12 dB/oct; 20 Hz = off). The detector is the absolute value (Peak) or the square
//! root of a one-pole mean of the square, τ = 10 ms (RMS); both are turned into dB with
//! [`fast_log2`] and floored at −120 dB. With **link** the level of each channel is blended
//! towards the louder one (`lerp(x_c, max(x_L, x_R), link)`, in dB); at 100 % one gain computer
//! serves both channels. The gain computer is the quadratic soft knee of `comp_curve`
//! ([`komponist_model::params::comp_curve`], the same formulas the graph draws). Its reduction
//! (dB, ≤ 0) goes through a branching one-pole smoother in the dB domain: deeper → the attack
//! coefficient, shallower → the release coefficient. Times are time constants (63 %).
//! **Auto-release** (program dependent): the smoother's release becomes 0.3 · `release`, and a
//! slow follower of its output dives with τ = `release` and recovers with τ = 3 · `release`; the
//! reduction applied is the deeper of the two. A short peak hardly moves the follower, so the gain
//! recovers fast; a long loud passage takes it down and the gain recovers slowly. (The proposal
//! averaged two parallel smoothers with the same attack, which recovers the same way after every
//! peak, so this is a deviation.) The factors are starting values for listening tests.
//!
//! The smoothed reduction plus the makeup (dB) becomes a linear gain by [`fast_exp2`] and is
//! applied to the (with lookahead delayed) audio. **Lookahead** delays audio and dry signal by
//! [`LOOKAHEAD_MS`] while the detector sees the undelayed signal, so the gain is already going
//! down when the loud part arrives; the delay is the processor's latency ([`Dsp::latency`], the
//! engine's delay compensation lines the tracks up). Switching it clears the delay line (the
//! audio is cut once) and changes the latency; the host reads it again (WP-91). **Mix** is
//! `dry·(1 − mix) + wet·mix`; 0 passes the (delayed) input bit-exactly. **SC hören** outputs the
//! filtered detector input (not delayed) in place of the audio, makeup and mix bypassed.
//! **Auto-makeup** adds the reduction the static curve has at `threshold / 2` (a heuristic).
//!
//! Threshold, ratio, knee, makeup and mix glide (20 ms) at the control rate [`CONTROL`], which
//! counts across calls, so the output does not depend on how the host splits blocks.
//!
//! **Telemetry.** The deepest reduction goes to the GR meter ([`Dsp::take_reduction_db`], range
//! 24 dB). For the live level point and the 4 s reduction history the DSP offers a
//! [`SampleTap`]: while the UI looks, it appends a pair `[level in dB, reduction in dB (≥ 0)]`
//! every [`pair_frames`] frames (about 1500 pairs per second). The level is the louder of the
//! two detector levels after the link, the reduction the deepest of the pair's interval.

use std::sync::Arc;

use komponist_engine::SampleTap;
use komponist_i18n::Lang;
use komponist_model::params::comp_curve::{self, RATIO_MAX};
use komponist_model::params::eq_curve::{self, Coefs};
use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half, Page, WidgetKind,
};
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};

use komponist_device_core::dsp::{
    CONTROL, DB_PER_OCTAVE, Df1, Glide, Ring, fast_exp2, fast_log2, finite, flush, smooth_coeff,
};
use komponist_device_core::layout::{knob, knob_large, param, toggle};
use komponist_device_core::{Dsp, INITIAL_RATE, Preset, Spec};

/// The device. The name is the working name of the proposal; this is the one place it is written.
pub static SPEC: Spec = Spec {
    catalog: komponist_device_core::CatalogSpec {
        id: "presse",
        name: "Presse",
        name_in: Lang::device_name_presse,
        description_in: Some(Lang::device_description_presse),
        role: komponist_model::DeviceRole::Effect,
        features: &["audio-effect", "compressor", "dynamics"],
        vendor: "komponist",
        sidechain: true,
    },
    params,
    layout,
    presets,
    cache: komponist_device_core::SpecCache::new(),
};

/// Lookahead (and latency) in milliseconds when `lookahead` is on.
pub const LOOKAHEAD_MS: f64 = 3.0;

/// Seconds of reduction history the graph shows.
pub const HISTORY_SECONDS: f32 = 4.0;

/// Capacity of the telemetry tap in samples: room for two samples per pair, 4 s of pairs with the
/// reader's half-ring limit to spare.
const TAP_CAPACITY: usize = 32_768;

/// Wire ids (the order of [`params`]).
const THRESHOLD: usize = 0;
const RATIO: usize = 1;
const KNEE: usize = 2;
const ATTACK: usize = 3;
const RELEASE: usize = 4;
const MAKEUP: usize = 5;
const MIX: usize = 6;
const DETECTOR: usize = 7;
const LINK: usize = 8;
const LOOKAHEAD: usize = 9;
const SC_HPF: usize = 10;
const SC_LISTEN: usize = 11;
const AUTO_RELEASE: usize = 12;
const AUTO_MAKEUP: usize = 13;

/// The sidechain high-pass at or below this frequency is off.
const SC_OFF_HZ: f64 = 20.0;

/// Detector time constant of RMS, seconds.
const RMS_SECONDS: f32 = 0.010;

/// Level floor in dB: silence and denormals never reach the control path.
const FLOOR_DB: f32 = -120.0;

/// Auto-release factors of the base release time.
const AUTO_FAST: f32 = 0.3;
const AUTO_SLOW: f32 = 3.0;

fn params(t: Lang) -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(
            "threshold",
            t.param_threshold(),
            ParamKind::lin(-60.0, 0.0),
            -18.0,
        )
        .unit(Unit::Db),
        ParamInfo::new(
            "ratio",
            t.param_ratio(),
            ParamKind::scaled(1.0, RATIO_MAX, Scale::Log),
            4.0,
        )
        .unit(Unit::Ratio),
        ParamInfo::new(
            "knee",
            t.param_presse_knee(),
            ParamKind::lin(0.0, 24.0),
            6.0,
        )
        .unit(Unit::DbWidth),
        ParamInfo::new(
            "attack",
            t.param_attack(),
            ParamKind::scaled(0.1, 200.0, Scale::Log),
            10.0,
        )
        .unit(Unit::Ms),
        ParamInfo::new(
            "release",
            t.param_release(),
            ParamKind::scaled(5.0, 2000.0, Scale::Log),
            120.0,
        )
        .unit(Unit::Ms),
        ParamInfo::new("makeup", t.param_makeup(), ParamKind::lin(-12.0, 24.0), 0.0).unit(Unit::Db),
        ParamInfo::new("mix", t.param_mix(), ParamKind::lin(0.0, 1.0), 1.0).unit(Unit::Percent),
        ParamInfo::new(
            "detector",
            t.param_presse_detector(),
            ParamKind::Enum(vec!["Peak".into(), "RMS".into()]),
            1.0,
        ),
        ParamInfo::new("link", t.param_link(), ParamKind::lin(0.0, 1.0), 1.0).unit(Unit::Percent),
        ParamInfo::new("lookahead", t.param_lookahead(), ParamKind::Bool, 0.0),
        ParamInfo::new(
            "sc_hpf",
            t.param_presse_sc_hpf(),
            ParamKind::scaled(SC_OFF_HZ, 500.0, Scale::Log),
            SC_OFF_HZ,
        )
        .unit(Unit::Hz),
        ParamInfo::new(
            "sc_listen",
            t.param_presse_sc_listen(),
            ParamKind::Bool,
            0.0,
        ),
        ParamInfo::new(
            "auto_release",
            t.param_presse_auto_release(),
            ParamKind::Bool,
            1.0,
        ),
        ParamInfo::new(
            "auto_makeup",
            t.param_presse_auto_makeup(),
            ParamKind::Bool,
            0.0,
        ),
    ]
}

/// Eight columns, one page, after the compressor proposal, `standard-1` mockup (internal notes): the transfer graph
/// (3 × 2) and below it knee, link, detector and lookahead; threshold, ratio, makeup, attack and
/// release; the reduction history, sidechain high-pass, mix, auto switches and „SC hören“; the
/// gain-reduction meter on the right.
fn layout(_: Lang) -> DeviceLayout {
    let transfer = GraphBinding::new(GraphConfig::Compressor)
        .bind("threshold", "threshold")
        .bind("ratio", "ratio")
        .bind("knee", "knee");
    let history = GraphBinding::new(GraphConfig::GrHistory);
    let makeup = Cell::knob(4, 1, "makeup".into()).disabled_by("auto_makeup".into());
    DeviceLayout::single(Page {
        name: String::new(),
        cols: 8,
        cells: vec![
            Cell::graph(0, 0, 3, 2, transfer),
            knob(0, 2, "knee"),
            knob(1, 2, "link"),
            Cell::half(2, 2, 1, Half::Top, WidgetKind::Segment, param("detector")),
            toggle(2, 2, 1, Half::Bottom, param("lookahead")),
            knob_large(3, 0, "threshold"),
            knob(3, 1, "ratio"),
            makeup,
            knob(3, 2, "attack"),
            knob(4, 2, "release"),
            Cell::graph(5, 0, 2, 1, history),
            knob(5, 1, "sc_hpf"),
            knob(6, 1, "mix"),
            toggle(5, 2, 1, Half::Top, param("auto_release")),
            toggle(5, 2, 1, Half::Bottom, param("auto_makeup")),
            toggle(6, 2, 1, Half::Top, param("sc_listen")),
            Cell::span(7, 0, 1, 3, WidgetKind::MeterGr, CellParam::None),
        ],
        dividers: vec![2, 4, 6],
    })
}

/// Frames between two telemetry pairs at `rate`: [`CONTROL`] at 44.1 and 48 kHz, a multiple of it
/// at higher rates, so the pairs per second stay about the same (≈ 1500).
pub fn pair_frames(rate: f32) -> usize {
    CONTROL * (rate / 48_000.0).round().max(1.0) as usize
}

/// Lookahead in frames at `rate`.
fn lookahead_frames(rate: f32) -> usize {
    (f64::from(rate) * LOOKAHEAD_MS / 1000.0).round() as usize
}

/// The factory presets of the proposal (WP-93, a menu since WP-96).
fn presets() -> &'static [Preset] {
    static LIST: [Preset; 11] = [
        Preset::new(
            "Soft Vocal",
            &[
                ("ratio", 3.0),
                ("attack", 15.0),
                ("release", 150.0),
                ("knee", 8.0),
                ("detector", 1.0),
                ("auto_release", 1.0),
                ("auto_makeup", 1.0),
            ],
        ),
        Preset::new(
            "Forward Vocal",
            &[
                ("ratio", 4.0),
                ("attack", 5.0),
                ("release", 80.0),
                ("knee", 4.0),
                ("detector", 0.0),
                ("auto_release", 0.0),
                ("mix", 1.0),
            ],
        ),
        Preset::new(
            "Tight Drums",
            &[
                ("ratio", 4.0),
                ("attack", 30.0),
                ("release", 100.0),
                ("detector", 0.0),
                ("auto_release", 0.0),
                ("auto_makeup", 1.0),
            ],
        ),
        Preset::new(
            "Parallel Drums",
            &[
                ("ratio", 10.0),
                ("attack", 2.0),
                ("release", 60.0),
                ("threshold", -30.0),
                ("mix", 0.4),
                ("auto_release", 0.0),
            ],
        ),
        Preset::new(
            "Bus Glue",
            &[
                ("ratio", 2.0),
                ("attack", 30.0),
                ("auto_release", 1.0),
                ("knee", 12.0),
                ("detector", 1.0),
                ("sc_hpf", 80.0),
                ("mix", 1.0),
            ],
        ),
        Preset::new(
            "Steady Bass",
            &[
                ("ratio", 4.0),
                ("attack", 20.0),
                ("release", 200.0),
                ("detector", 1.0),
                ("sc_hpf", 60.0),
                ("knee", 6.0),
                ("auto_release", 0.0),
            ],
        ),
        Preset::new(
            "Gentle Guitar & Keys",
            &[
                ("ratio", 2.5),
                ("attack", 20.0),
                ("release", 250.0),
                ("knee", 10.0),
                ("detector", 1.0),
                ("auto_release", 0.0),
            ],
        ),
        Preset::new(
            "Hard Limit",
            &[
                ("ratio", 20.0),
                ("attack", 0.1),
                ("release", 50.0),
                ("knee", 0.0),
                ("threshold", -6.0),
                ("lookahead", 1.0),
                ("auto_release", 0.0),
            ],
        ),
        Preset::new(
            "Pumping",
            &[
                ("ratio", 8.0),
                ("attack", 1.0),
                ("release", 40.0),
                ("threshold", -30.0),
                ("detector", 0.0),
                ("mix", 0.8),
                ("auto_release", 0.0),
            ],
        ),
        // For a sidechain source (WP-104): the kick pushes a pad or bass down.
        Preset::new(
            "Ducking",
            &[
                ("ratio", 6.0),
                ("attack", 2.0),
                ("release", 150.0),
                ("threshold", -24.0),
                ("knee", 4.0),
                ("detector", 0.0),
                ("auto_release", 0.0),
            ],
        ),
        Preset::new("Neutral", &[("ratio", 1.0)]),
    ];
    &LIST
}

/// One smoothed reduction in dB (≤ 0): the branching smoother `fast` and, for auto-release, a
/// slow follower of it.
#[derive(Clone, Copy, Default)]
struct Reduction {
    fast: f32,
    slow: f32,
}

/// The coefficients of the reduction smoothers.
#[derive(Clone, Copy, Default)]
struct Times {
    attack: f32,
    /// The release of the first smoother: the base time, or 0.3 × it with auto-release.
    release_fast: f32,
    /// The follower: it dives with τ = `release` and recovers with τ = 3 × `release`.
    dive_slow: f32,
    release_slow: f32,
}

impl Reduction {
    /// One step towards `target` (dB ≤ 0); returns the reduction to apply. With `auto` = 1 it is
    /// the deeper of `fast` and `slow`: a short peak barely moves the slow follower, so the
    /// reduction recovers with the fast release; a long loud passage takes the follower down and
    /// the recovery is slow. `auto` = 0 is `fast` alone (the plain release).
    #[inline]
    fn step(&mut self, target: f32, t: &Times, auto: f32) -> f32 {
        let release = if target < self.fast {
            t.attack
        } else {
            t.release_fast
        };
        self.fast = flush(target + (self.fast - target) * release);
        let follow = if self.fast < self.slow {
            t.dive_slow
        } else {
            t.release_slow
        };
        self.slow = flush(self.fast + (self.slow - self.fast) * follow);
        self.fast + auto * (self.slow - self.fast).min(0.0)
    }
}

/// The gain computer on `f32`: the reduction in dB (≤ 0) at level `x` dB. `slope` is `1/R − 1`.
#[inline]
fn reduction_target(x: f32, threshold: f32, slope: f32, knee: f32) -> f32 {
    let over = x - threshold;
    if 2.0 * over < -knee {
        0.0
    } else if 2.0 * over >= knee {
        slope * over
    } else {
        let u = over + knee * 0.5;
        slope * u * u / (2.0 * knee)
    }
}

/// Length of the stack buffer telemetry pairs are collected in before they go into the tap.
const BATCH: usize = 64;

struct Presse {
    rate: f32,
    // Parameters as set.
    threshold: Glide,
    slope: Glide,
    knee: Glide,
    makeup: Glide,
    mix: Glide,
    auto_amount: Glide,
    glide: f32,
    makeup_db: f32,
    auto_makeup: bool,
    attack_ms: f32,
    release_ms: f32,
    times: Times,
    rms: bool,
    link: f32,
    lookahead: bool,
    hp_hz: f32,
    hp_on: bool,
    hp_coefs: Coefs,
    listen: bool,
    // State.
    hp: [Df1; 2],
    mean_square: [f32; 2],
    rms_coeff: f32,
    reduction: [Reduction; 2],
    /// Frames the audio is delayed by now (0 without lookahead).
    delay: usize,
    lines: [Ring; 2],
    countdown: usize,
    // Values of the current control interval.
    t: f32,
    s: f32,
    w: f32,
    // Telemetry.
    deepest: f32,
    tap: Arc<SampleTap>,
    pair_every: usize,
    pair_ticks: usize,
    pair_level: f32,
    pair_reduction: f32,
}

pub fn dsp() -> Box<dyn Dsp> {
    let mut p = Presse {
        rate: INITIAL_RATE,
        threshold: Glide::new(-18.0),
        slope: Glide::new(0.25 - 1.0),
        knee: Glide::new(6.0),
        makeup: Glide::new(0.0),
        mix: Glide::new(1.0),
        auto_amount: Glide::new(1.0),
        glide: 0.0,
        makeup_db: 0.0,
        auto_makeup: false,
        attack_ms: 10.0,
        release_ms: 120.0,
        times: Times::default(),
        rms: true,
        link: 1.0,
        lookahead: false,
        hp_hz: SC_OFF_HZ as f32,
        hp_on: false,
        hp_coefs: Coefs::IDENTITY,
        listen: false,
        hp: [Df1::default(); 2],
        mean_square: [0.0; 2],
        rms_coeff: 0.0,
        reduction: [Reduction::default(); 2],
        delay: 0,
        lines: [Ring::default(), Ring::default()],
        countdown: 0,
        t: -18.0,
        s: -0.75,
        w: 6.0,
        deepest: 0.0,
        tap: Arc::new(SampleTap::new(TAP_CAPACITY)),
        pair_every: 1,
        pair_ticks: 0,
        pair_level: FLOOR_DB,
        pair_reduction: 0.0,
    };
    p.allocate();
    Box::new(p)
}

impl Presse {
    /// Sizes the buffers and recomputes everything that depends on the sample rate (allocates).
    fn allocate(&mut self) {
        let max = lookahead_frames(self.rate).max(1);
        self.lines = [Ring::with_len(max + 2), Ring::with_len(max + 2)];
        self.glide = smooth_coeff(0.020, CONTROL, self.rate);
        self.rms_coeff = smooth_coeff(RMS_SECONDS, 1, self.rate);
        self.pair_every = pair_frames(self.rate) / CONTROL;
        self.tap.set_rate(self.rate);
        self.update_times();
        self.update_filter();
        self.apply_lookahead();
        self.snap();
    }

    fn snap(&mut self) {
        for g in [
            &mut self.threshold,
            &mut self.slope,
            &mut self.knee,
            &mut self.makeup,
            &mut self.mix,
            &mut self.auto_amount,
        ] {
            g.snap();
        }
        self.refresh_interval();
    }

    fn update_times(&mut self) {
        let rate = self.rate;
        let coeff = |ms: f32| smooth_coeff(ms * 0.001, 1, rate);
        let fast = if self.auto_amount.target >= 0.5 {
            AUTO_FAST
        } else {
            1.0
        };
        self.times = Times {
            attack: coeff(self.attack_ms),
            release_fast: coeff(self.release_ms * fast),
            dive_slow: coeff(self.release_ms),
            release_slow: coeff(self.release_ms * AUTO_SLOW),
        };
    }

    fn update_filter(&mut self) {
        let on = f64::from(self.hp_hz) > SC_OFF_HZ + 0.5;
        if on && !self.hp_on {
            self.hp = [Df1::default(); 2];
        }
        self.hp_on = on;
        self.hp_coefs = eq_curve::coefs(
            f64::from(eq_curve::LOW_CUT),
            f64::from(self.hp_hz),
            0.0,
            core::f64::consts::FRAC_1_SQRT_2,
            f64::from(self.rate),
        );
    }

    /// The makeup the knob or auto-makeup asks for, in dB.
    fn update_makeup(&mut self) {
        self.makeup.target = if self.auto_makeup {
            comp_curve::auto_makeup_db(
                f64::from(self.threshold.target),
                f64::from(1.0 / (1.0 + self.slope.target)),
                f64::from(self.knee.target),
            ) as f32
        } else {
            self.makeup_db
        };
    }

    fn apply_lookahead(&mut self) {
        self.delay = if self.lookahead {
            lookahead_frames(self.rate)
        } else {
            0
        };
        for line in &mut self.lines {
            line.clear();
        }
    }

    /// Starts a control interval: the glides take a step and the interval's values are read.
    fn start_interval(&mut self) {
        for g in [
            &mut self.threshold,
            &mut self.slope,
            &mut self.knee,
            &mut self.makeup,
            &mut self.mix,
            &mut self.auto_amount,
        ] {
            g.start(self.glide, 1e-4);
        }
        self.refresh_interval();
        for h in &mut self.hp {
            h.flush_denormals();
        }
    }

    fn refresh_interval(&mut self) {
        self.t = self.threshold.next;
        self.s = self.slope.next;
        self.w = self.knee.next;
    }

    /// The level in dB of detector signal `s` on channel `c`.
    #[inline]
    fn level(&mut self, c: usize, s: f32) -> f32 {
        let s = s.clamp(-1e6, 1e6);
        if self.rms {
            let sq = s * s;
            let m = flush(sq + (self.mean_square[c] - sq) * self.rms_coeff);
            self.mean_square[c] = m;
            (0.5 * DB_PER_OCTAVE * fast_log2(m.max(1e-12))).max(FLOOR_DB)
        } else {
            (DB_PER_OCTAVE * fast_log2(s.abs().max(1e-6))).max(FLOOR_DB)
        }
    }
}

impl Dsp for Presse {
    fn activate(&mut self, sample_rate: f32) {
        self.rate = sample_rate;
        self.allocate();
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        match index {
            THRESHOLD => {
                self.threshold.target = value as f32;
                self.update_makeup();
            }
            RATIO => {
                self.slope.target = (1.0 / value.clamp(1.0, RATIO_MAX) - 1.0) as f32;
                self.update_makeup();
            }
            KNEE => {
                self.knee.target = value as f32;
                self.update_makeup();
            }
            ATTACK => {
                self.attack_ms = value as f32;
                self.update_times();
            }
            RELEASE => {
                self.release_ms = value as f32;
                self.update_times();
            }
            MAKEUP => {
                self.makeup_db = value as f32;
                self.update_makeup();
            }
            MIX => self.mix.target = value as f32,
            DETECTOR => self.rms = value >= 0.5,
            LINK => self.link = value as f32,
            LOOKAHEAD => {
                let on = value >= 0.5;
                if on != self.lookahead {
                    self.lookahead = on;
                    self.apply_lookahead();
                }
            }
            SC_HPF => {
                self.hp_hz = value as f32;
                self.update_filter();
            }
            SC_LISTEN => self.listen = value >= 0.5,
            AUTO_RELEASE => {
                self.auto_amount.target = if value >= 0.5 { 1.0 } else { 0.0 };
                self.update_times();
            }
            AUTO_MAKEUP => {
                self.auto_makeup = value >= 0.5;
                self.update_makeup();
            }
            _ => {}
        }
    }

    fn latency(&self) -> u32 {
        self.delay as u32
    }

    fn gr_range_db(&self) -> f32 {
        24.0
    }

    fn tap(&self) -> Option<Arc<SampleTap>> {
        Some(Arc::clone(&self.tap))
    }

    fn take_reduction_db(&mut self) -> f32 {
        let db = self.deepest;
        self.deepest = 0.0;
        db
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        self.run(input, None, output);
    }

    fn process_sidechain(
        &mut self,
        input: [&[f32]; 2],
        sidechain: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) {
        self.run(input, Some(sidechain), output);
    }

    fn reset(&mut self) {
        self.hp = [Df1::default(); 2];
        self.mean_square = [0.0; 2];
        self.reduction = [Reduction::default(); 2];
        self.deepest = 0.0;
        self.pair_ticks = 0;
        self.pair_level = FLOOR_DB;
        self.pair_reduction = 0.0;
        self.countdown = 0;
        self.snap();
        self.apply_lookahead();
    }
}

impl Presse {
    /// The block: `sidechain`, when given, feeds the detector in place of `input` (the audio path
    /// and the lookahead stay on `input`).
    fn run(&mut self, input: [&[f32]; 2], sidechain: Option<[&[f32]; 2]>, output: [&mut [f32]; 2]) {
        let [il, ir] = input;
        let [ol, or] = output;
        let tapping = self.tap.begin(il.len());
        let mut batch = [0.0f32; BATCH];
        let mut filled = 0;
        let inv_octave = 1.0 / DB_PER_OCTAVE;
        for n in 0..il.len() {
            if self.countdown == 0 {
                self.start_interval();
                self.countdown = CONTROL;
                // A telemetry pair per `pair_every` intervals.
                self.pair_ticks += 1;
                if self.pair_ticks >= self.pair_every {
                    if tapping {
                        batch[filled] = self.pair_level;
                        batch[filled + 1] = self.pair_reduction;
                        filled += 2;
                        if filled == BATCH {
                            self.tap.push(&batch);
                            filled = 0;
                        }
                    }
                    self.pair_ticks = 0;
                    self.pair_level = FLOOR_DB;
                    self.pair_reduction = 0.0;
                }
            }
            let done = CONTROL - self.countdown;
            self.countdown -= 1;
            let xl = finite(il[n]);
            let xr = finite(ir[n]);

            // Detector side: sidechain high-pass, level in dB, link. The detector hears the
            // external sidechain when there is one.
            let (detl, detr) = match sidechain {
                Some([a, b]) => (finite(a[n]), finite(b[n])),
                None => (xl, xr),
            };
            let (sl, sr) = if self.hp_on {
                (
                    self.hp[0].tick(detl, &self.hp_coefs),
                    self.hp[1].tick(detr, &self.hp_coefs),
                )
            } else {
                (detl, detr)
            };
            let lvl_l = self.level(0, sl);
            let lvl_r = self.level(1, sr);
            let loudest = lvl_l.max(lvl_r);
            let linked = self.link >= 1.0;
            let (eff_l, eff_r) = if linked {
                (loudest, loudest)
            } else {
                (
                    lvl_l + (loudest - lvl_l) * self.link,
                    lvl_r + (loudest - lvl_r) * self.link,
                )
            };

            // Gain computer and smoothing.
            let auto = self.auto_amount.at(done);
            let times = self.times;
            let gr_l = self.reduction[0].step(
                reduction_target(eff_l, self.t, self.s, self.w),
                &times,
                auto,
            );
            let gr_r = if linked {
                self.reduction[1] = self.reduction[0];
                gr_l
            } else {
                self.reduction[1].step(
                    reduction_target(eff_r, self.t, self.s, self.w),
                    &times,
                    auto,
                )
            };
            let deepest = -gr_l.min(gr_r);
            self.deepest = self.deepest.max(deepest);
            self.pair_level = self.pair_level.max(loudest);
            self.pair_reduction = self.pair_reduction.max(deepest);

            // The audio, with lookahead delayed.
            let (dl, dr) = if self.delay == 0 {
                (xl, xr)
            } else {
                self.lines[0].push(xl);
                self.lines[1].push(xr);
                (
                    self.lines[0].tap(self.delay + 1),
                    self.lines[1].tap(self.delay + 1),
                )
            };
            if self.listen {
                ol[n] = sl;
                or[n] = sr;
                continue;
            }
            let makeup = self.makeup.at(done);
            let wet_l = dl * fast_exp2((gr_l + makeup) * inv_octave);
            let wet_r = dr * fast_exp2((gr_r + makeup) * inv_octave);
            let mix = self.mix.at(done);
            if mix >= 1.0 {
                ol[n] = wet_l;
                or[n] = wet_r;
            } else if mix <= 0.0 {
                ol[n] = dl;
                or[n] = dr;
            } else {
                ol[n] = dl + (wet_l - dl) * mix;
                or[n] = dr + (wet_r - dr) * mix;
            }
        }
        if filled > 0 {
            self.tap.push(&batch[..filled]);
        }
    }
}

#[cfg(test)]
mod tests;
