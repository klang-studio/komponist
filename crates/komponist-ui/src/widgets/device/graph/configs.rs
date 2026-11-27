//! The nine configurations of the graph (device handoff §5), constructors that return a
//! [`GraphSpec`]. A device may adapt the result ([`GraphSpec::titled`], its public fields).
//!
//! Slot conventions (what the host puts into [`GraphSpec::slots`], in this order):
//!
//! | Config | Slots | Units |
//! |---|---|---|
//! | [`envelope`] | `attack`, `decay`, `sustain`, `release`, `curve` | **normalised 0–1** (segment widths, level, curvature) |
//! | [`eq`] | per band `b{n}_on`, `b{n}_freq`, `b{n}_gain`, `b{n}_q`, `b{n}_type` | 0/1, Hz, dB, Q, 0 LC · 1 LS · 2 bell · 3 HS · 4 HC |
//! | [`filter`] | `cutoff`, `resonance`, `type` | Hz, 0–1, 0 TP 12 · 1 TP 24 · 2 BP · 3 HP |
//! | [`transfer`] | `threshold`, `ratio`, `knee` | dB, ratio, 0 hard · 1 soft |
//! | [`compressor`] | `threshold`, `ratio`, `knee` | dB, ratio, knee width in dB (0 hard) |
//! | [`gr_history`] | none | display only: `Data` = input level 0–1, `Trace` = reduction in dB (≤ 0) |
//! | [`waveform`] | `start`, `end` | milliseconds |
//! | [`xy`] | `x`, `y` | the axes' own units |
//! | [`lfo`] | `x1`, `y1`, `x2`, `y2`, … | phase 0–1, −1…1 |
//! | [`filter_va`] | `cutoff`, `resonance`, `type`, `mod1_src` … `lfo2_uni` | Hz, 0–1, volt's filter list (0 LP 24 · 1 LP 12 · 2 BP 12 · 3 HP 12 · 4 notch), the matrix |
//! | [`signal`] | `wave_a`, `wave_b`, `pw_a`, `pw_b`, `b_oct`, `b_fine`, `mix`, `sub` | volt's oscillator parameters |
//! | [`lfo_shape`] | `shape` | index of `va_curve::LFO_SHAPES` |

use super::spec::{
    AreaAction, Axis, AxisKind, Bind, Curve, CurveFn, DataKind, GraphSpec, Grid, Guide, Handle,
    HandleLook, Layer, Marker, PosFn, Pred, RangeFn, Region, Stack, StackFn, Tick,
};
use crate::Lang;
use crate::widgets::metrics::device::graph as m;
use komponist_device_dsp::comp_curve;
use komponist_device_dsp::eq_curve::CURVE_RATE;
use komponist_device_dsp::va_curve;
use komponist_device_dsp::wavetable;

/// Slot indices of [`envelope`].
pub mod env {
    /// Attack time (normalised).
    pub const ATTACK: usize = 0;
    /// Decay time (normalised).
    pub const DECAY: usize = 1;
    /// Sustain level.
    pub const SUSTAIN: usize = 2;
    /// Release time (normalised).
    pub const RELEASE: usize = 3;
    /// Curvature (normalised).
    pub const CURVE: usize = 4;
}

/// Slots per EQ band.
pub const EQ_SLOTS: usize = 5;
/// Band types of the EQ (the slot `b{n}_type`).
pub mod band {
    /// Low cut.
    pub const LOW_CUT: f64 = 0.0;
    /// Low shelf.
    pub const LOW_SHELF: f64 = 1.0;
    /// Bell.
    pub const BELL: f64 = 2.0;
    /// High shelf.
    pub const HIGH_SHELF: f64 = 3.0;
    /// High cut.
    pub const HIGH_CUT: f64 = 4.0;
}

fn slots(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn padded(axis: Axis, pad: [f32; 2]) -> Axis {
    axis.padding(pad)
}

// ---------------------------------------------------------------------------------------------
// Envelope

/// `x` positions of the four envelope points (attack end, decay end, sustain end, release end).
pub fn envelope_points(p: &[f64]) -> [f64; 4] {
    let k = m::ENVELOPE_SEGMENT;
    let xa = k * p[env::ATTACK];
    let xd = xa + k * p[env::DECAY];
    let xs = xd + m::ENVELOPE_HOLD;
    let xr = xs + k * p[env::RELEASE];
    [xa, xd, xs, xr]
}

/// The envelope curve: linear attack to 1, decay to the sustain level and release to 0 on
/// `(1 − u)^c` curves (`c` from the `curve` slot: 1 … 5, the middle is about the generator's
/// quadratic Bézier).
pub fn envelope_level(x: f64, p: &[f64]) -> f64 {
    let [xa, xd, xs, xr] = envelope_points(p);
    let s = p[env::SUSTAIN];
    let c = 1.0 + 4.0 * p[env::CURVE];
    let seg = |a: f64, b: f64| {
        if b > a {
            ((x - a) / (b - a)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    };
    if x < 0.0 {
        0.0
    } else if x < xa {
        seg(0.0, xa)
    } else if x < xd {
        s + (1.0 - s) * (1.0 - seg(xa, xd)).powf(c)
    } else if x < xs {
        s
    } else if x < xr {
        s * (1.0 - seg(xs, xr)).powf(c)
    } else {
        0.0
    }
}

/// `graph::envelope`: time on x (segments proportional), 0–1 on y; curve from the segments,
/// playhead; handles A ↔, D ↔↕ (decay and sustain level), S ↕, R ↔, wheel on D and R bends the
/// curve.
pub fn envelope(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::ENVELOPE;
    let mut spec = GraphSpec::new(
        t.ui_graph_envelope(),
        padded(Axis::new(AxisKind::Lin, 0.0, 1.0), px).grid(Grid::Divisions(8)),
        padded(Axis::new(AxisKind::Lin, 0.0, 1.0), py).grid(Grid::None),
    );
    spec.slots = slots(&["attack", "decay", "sustain", "release", "curve"]);
    let k = m::ENVELOPE_SEGMENT;
    spec.layers = vec![
        Layer::Curve(Curve {
            f: CurveFn::new(envelope_level),
            baseline: Some(0.0),
            fill: true,
        }),
        Layer::Marker(Marker::Playhead),
    ];
    let bend = || Bind::knob(env::CURVE, t.ui_graph_curve(), false);
    spec.handles = vec![
        Handle::dot(t.ui_graph_attack())
            .x(Bind::axis(env::ATTACK, t.ui_graph_time()).scaled(k))
            .at(PosFn::new(|p| (envelope_points(p)[0], 1.0))),
        Handle::dot(t.ui_graph_decay())
            .x(Bind::axis(env::DECAY, t.ui_graph_time())
                .scaled(k)
                .after(&[env::ATTACK], 0.0))
            .y(Bind::axis(env::SUSTAIN, t.ui_graph_sustain_level()))
            .wheel(bend()),
        Handle::dot(t.ui_graph_sustain())
            .y(Bind::axis(env::SUSTAIN, t.ui_graph_level()))
            .at(PosFn::new(|p| (envelope_points(p)[2], p[env::SUSTAIN]))),
        Handle::dot(t.ui_graph_release())
            .x(Bind::axis(env::RELEASE, t.ui_graph_time())
                .scaled(k)
                .after(&[env::ATTACK, env::DECAY], m::ENVELOPE_HOLD))
            .wheel(bend())
            .at(PosFn::new(|p| (envelope_points(p)[3], 0.0))),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// EQ

/// The response in dB of band `i` at `hz` (`W_eq` `eq_resp`) at the graph's default sample rate.
pub fn eq_band_response(p: &[f64], i: usize, hz: f64) -> f64 {
    eq_band_response_at(p, i, hz, CURVE_RATE)
}

/// The response in dB of band `i` at `hz` for a device running at `rate`.
pub fn eq_band_response_at(p: &[f64], i: usize, hz: f64, rate: f64) -> f64 {
    let o = i * EQ_SLOTS;
    if p[o] < 0.5 {
        return 0.0;
    }
    let (fc, gain, q, typ) = (p[o + 1], p[o + 2], p[o + 3], p[o + 4]);
    // The real filter's response (WP-90): the device runs the same coefficients.
    komponist_device_dsp::eq_curve::band_response_db_at(typ, fc, gain, q, hz, rate)
}

/// The summed response of `bands` bands at `hz`.
pub fn eq_response(p: &[f64], bands: usize, hz: f64) -> f64 {
    eq_response_at(p, bands, hz, CURVE_RATE)
}

/// The summed response of `bands` bands at `hz` for a device running at `rate`.
pub fn eq_response_at(p: &[f64], bands: usize, hz: f64, rate: f64) -> f64 {
    (0..bands)
        .map(|i| eq_band_response_at(p, i, hz, rate))
        .sum()
}

/// `graph::eq` for up to `bands` bands and `±range_db`: log x 20 Hz–20 kHz, dB on y, spectrum
/// data and the summed curve; per band Freq ↔, Gain ↕, Q on the wheel; double-click adds a band
/// (event), Entf removes the focused one.
pub fn eq(t: Lang, bands: usize, range_db: f64) -> GraphSpec {
    eq_at(t, bands, range_db, CURVE_RATE)
}

/// [`eq`] for a device running at `rate` Hz: the curve is the real filters' response there.
pub fn eq_at(t: Lang, bands: usize, range_db: f64, rate: f64) -> GraphSpec {
    let (px, py) = m::pad::EQ;
    let mut spec = GraphSpec::new(
        "",
        padded(Axis::new(AxisKind::Log, 20.0, 20_000.0), px).labelled(),
        padded(Axis::new(AxisKind::Db, -range_db, range_db), py),
    );
    for n in 1..=bands {
        for what in ["on", "freq", "gain", "q", "type"] {
            spec.slots.push(format!("b{n}_{what}"));
        }
    }
    spec.name = t.ui_graph_eq().to_owned();
    spec.layers = vec![
        Layer::Data(DataKind::Spectrum),
        Layer::Curve(Curve {
            f: CurveFn::new(move |x, p| eq_response_at(p, bands, x, rate)),
            baseline: Some(0.0),
            fill: true,
        }),
    ];
    spec.on_double_click = AreaAction::NewPoint;
    spec.deletable = true;
    for i in 0..bands {
        let o = i * EQ_SLOTS;
        let is_cut = move |p: &[f64]| {
            let t = p[o + 4].round();
            t == band::LOW_CUT || t == band::HIGH_CUT
        };
        spec.handles.push(
            Handle::dot(t.ui_graph_band(i + 1))
                .numbered(i as u32 + 1)
                .x(Bind::axis(o + 1, t.ui_graph_frequency()))
                .y(Bind::axis(o + 2, t.ui_graph_gain()).only_if(Pred::new(move |p| !is_cut(p))))
                .wheel(Bind::knob(o + 3, t.ui_graph_q(), true))
                .at(PosFn::new(move |p| {
                    let y = if is_cut(p) {
                        -3.0
                    } else {
                        eq_response_at(p, bands, p[o + 1], rate)
                    };
                    (p[o + 1], y)
                }))
                .visible_if(Pred::new(move |p| p[o] > 0.5)),
        );
    }
    spec
}

// ---------------------------------------------------------------------------------------------
// Filter

/// The magnitude in dB of the filter at `hz`: `type` 0 low pass 12 dB/oct, 1 low pass 24, 2 band
/// pass, 3 high pass; resonance 0–1 sets Q from 0.5 to 10.
pub fn filter_response(hz: f64, cutoff: f64, resonance: f64, typ: f64) -> f64 {
    let r = hz / cutoff;
    let q = 0.5 + 9.5 * resonance * resonance;
    let stage = |r: f64, q: f64, kind: i32| {
        let den = ((1.0 - r * r).powi(2) + (r / q).powi(2)).sqrt();
        match kind {
            2 => (r / q) / den,
            3 => r * r / den,
            _ => 1.0 / den,
        }
    };
    let mag = match typ.round() as i32 {
        0 => stage(r, q, 0),
        1 => stage(r, q, 0) * stage(r, std::f64::consts::FRAC_1_SQRT_2, 0),
        2 => stage(r, q, 2),
        _ => stage(r, q, 3),
    };
    20.0 * mag.max(1e-9).log10()
}

/// `graph::filter`: log x 20 Hz–20 kHz, dB −36…+12 on y, the curve of the filter; one handle,
/// Cutoff ↔ and Resonanz ↕.
pub fn filter(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::EQ;
    let mut spec = GraphSpec::new(
        t.ui_graph_filter(),
        padded(Axis::new(AxisKind::Log, 20.0, 20_000.0), px).labelled(),
        padded(Axis::new(AxisKind::Db, -36.0, 12.0), py),
    );
    spec.slots = slots(&["cutoff", "resonance", "type"]);
    spec.layers = vec![Layer::Curve(Curve {
        f: CurveFn::new(|x, p| filter_response(x, p[0], p[1], p[2])),
        baseline: Some(0.0),
        fill: true,
    })];
    spec.handles = vec![
        Handle::dot(t.ui_graph_filter())
            .numbered(1)
            .x(Bind::axis(0, t.ui_graph_cutoff()))
            .y(Bind::knob(1, t.ui_graph_resonance(), false))
            .at(PosFn::new(|p| {
                (p[0], filter_response(p[0], p[0], p[1], p[2]))
            })),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// Transfer

/// The output level of a compressor at input `x` dB (`knee` ≥ 0.5: soft, 6 dB wide).
pub fn transfer_level(x: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
    let ratio = ratio.max(1.0);
    let w = if knee >= 0.5 { 6.0 } else { 0.0 };
    let over = x - threshold;
    if 2.0 * over < -w {
        x
    } else if w > 0.0 && 2.0 * over.abs() <= w {
        x + (1.0 / ratio - 1.0) * (over + w / 2.0).powi(2) / (2.0 * w)
    } else {
        threshold + over / ratio
    }
}

/// `graph::transfer`: input dB −60…0 on x, output dB on y; the diagonal and the threshold as
/// guides, the curve, the level point; handle at the knee: Threshold ↔, Ratio ↕.
pub fn transfer(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::TRANSFER;
    let axis = |pad| padded(Axis::new(AxisKind::Db, -60.0, 0.0), pad).grid(Grid::Divisions(4));
    let mut spec = GraphSpec::new(t.ui_graph_transfer(), axis(px), axis(py));
    spec.slots = slots(&["threshold", "ratio", "knee"]);
    spec.layers = vec![
        Layer::Guide(Guide::Diagonal),
        Layer::Guide(Guide::VLine { slot: 0 }),
        Layer::Curve(Curve {
            f: CurveFn::new(|x, p| transfer_level(x, p[0], p[1], p[2])),
            baseline: None,
            fill: false,
        }),
        Layer::Marker(Marker::Level),
    ];
    spec.handles = vec![
        Handle::dot(t.ui_graph_knee())
            .x(Bind::axis(0, t.ui_graph_threshold()))
            .y(Bind::knob(1, t.ui_graph_ratio(), true))
            .at(PosFn::new(|p| (p[0], p[0]))),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// Compressor

/// The compressor's output level at input `x` dB: the curve of the device
/// ([`komponist_device_dsp::comp_curve::output_db`]), with the knee as its width in dB.
pub fn compressor_level(x: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
    comp_curve::output_db(x, threshold, ratio, knee)
}

/// `graph::compressor` (WP-93, `standard-1.svg`): `graph::transfer` for the compressor `presse`.
/// Input dB −60…0 on x, output dB on y, a 4-division grid with labels at −48, −24 and 0, the
/// diagonal and the threshold as guides, the curve with the soft knee of `knee` dB and the level
/// point; to the right of the plot IN, OUT and GR of the live level
/// ([`Marker::Levels`]). Handle at the knee: Threshold ↔, Ratio ↕.
pub fn compressor(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::COMPRESSOR;
    let labels = |step: f64| {
        let mut t = Vec::new();
        let mut v = -60.0 + step;
        while v < 0.0 {
            t.push(if (v + 48.0).abs() < 1e-9 || (v + 24.0).abs() < 1e-9 {
                Tick::major(v, format!("\u{2212}{}", v.abs()))
            } else {
                Tick::minor(v)
            });
            v += step;
        }
        t.push(Tick::major(0.0, "0"));
        t
    };
    let mut x = Axis::new(AxisKind::Db, -60.0, 0.0).padding(px);
    x.grid = Grid::Ticks(labels(12.0));
    x.labels = true;
    let y = Axis::new(AxisKind::Db, -60.0, 0.0)
        .padding(py)
        .grid(Grid::Divisions(5));
    let mut spec = GraphSpec::new(t.ui_graph_transfer(), x, y);
    spec.grid_inside = true;
    spec.slots = slots(&["threshold", "ratio", "knee"]);
    spec.layers = vec![
        Layer::Guide(Guide::Diagonal),
        Layer::Guide(Guide::VLine { slot: 0 }),
        Layer::Curve(Curve {
            f: CurveFn::new(|x, p| compressor_level(x, p[0], p[1], p[2])),
            baseline: None,
            fill: true,
        }),
        Layer::Marker(Marker::Level),
        Layer::Marker(Marker::Levels),
    ];
    spec.handles = vec![
        Handle::dot(t.ui_graph_knee())
            .x(Bind::axis(0, t.ui_graph_threshold()))
            .y(Bind::knob(1, t.ui_graph_ratio(), true))
            .at(PosFn::new(|p| (p[0], p[0]))),
    ];
    spec
}

/// Seconds the reduction history shows.
pub const GR_HISTORY_SECONDS: f64 = 4.0;
/// Bottom of the reduction history's y axis in dB (the top is 0).
pub const GR_HISTORY_DB: f64 = 24.0;

/// `graph::gr_history` (WP-93): time 0…4 s on x, 0 … −24 dB on y (the reduction hangs from the
/// top); the input level as the `Data` layer (`surface.3`), the reduction as the `Trace` layer in
/// the track colour, the playhead at the right edge (now). Display only: no slots, no handles.
pub fn gr_history(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::GR_HISTORY;
    let y = Axis::new(AxisKind::Db, -GR_HISTORY_DB, 0.0)
        .padding(py)
        .grid(Grid::Ticks(vec![
            Tick::major(-6.0, "\u{2212}6"),
            Tick::major(-12.0, "\u{2212}12"),
            Tick::major(-18.0, "\u{2212}18"),
        ]))
        .labelled();
    let mut spec = GraphSpec::new(
        t.ui_graph_gr_history(),
        Axis::new(AxisKind::Time, 0.0, GR_HISTORY_SECONDS)
            .padding(px)
            .grid(Grid::None),
        y,
    );
    spec.title_bottom_right = true;
    spec.layers = vec![
        Layer::Data(DataKind::Spectrum),
        Layer::Trace { baseline: 0.0 },
        Layer::Marker(Marker::Playhead),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// Waveform

/// `graph::waveform` for a sample of `length_s` seconds: time on x, bipolar on y; sample data,
/// the region outside start–end darkened, playhead; S and E as lines, ↔ only. Start and end are
/// in milliseconds.
pub fn waveform(t: Lang, length_s: f64) -> GraphSpec {
    let (px, py) = m::pad::WAVEFORM;
    let mut spec = GraphSpec::new(
        "",
        padded(Axis::new(AxisKind::Time, 0.0, length_s), px).grid(Grid::None),
        padded(Axis::new(AxisKind::Lin, -1.0, 1.0), py).grid(Grid::Ticks(vec![Tick::minor(0.0)])),
    );
    spec.name = t.ui_graph_waveform().to_owned();
    spec.slots = slots(&["start", "end"]);
    spec.layers = vec![
        Layer::Data(DataKind::Sample),
        Layer::Region(Region::Outside {
            start: 0,
            end: 1,
            scale: 0.001,
        }),
        Layer::Marker(Marker::Playhead),
    ];
    spec.handles = vec![
        Handle::dot(t.ui_graph_sample_start())
            .look(HandleLook::Edge { flag_left: false })
            .x(Bind::axis(0, t.ui_graph_position()).scaled(0.001))
            .between(None, Some(1)),
        Handle::dot(t.ui_graph_sample_end())
            .look(HandleLook::Edge { flag_left: true })
            .x(Bind::axis(1, t.ui_graph_position()).scaled(0.001))
            .between(Some(0), None),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// XY

/// `graph::xy`: the two parameters' own axes; the crosshair and one puck, ↔↕, Shift locks the axis
/// that moved less; `x_name` and `y_name` are the parameter names (accessible names, readout).
pub fn xy(t: Lang, x: Axis, y: Axis, x_name: &str, y_name: &str) -> GraphSpec {
    let (px, py) = m::pad::XY;
    let mut spec = GraphSpec::new(
        "",
        padded(x, px).grid(Grid::Divisions(4)),
        padded(y, py).grid(Grid::Divisions(4)),
    );
    spec.name = t.ui_graph_xy().to_owned();
    spec.slots = slots(&["x", "y"]);
    spec.layers = vec![
        Layer::Guide(Guide::Crosshair { x: 0, y: 1 }),
        Layer::Marker(Marker::Readout { x: 0, y: 1 }),
    ];
    spec.handles = vec![
        Handle::dot(t.ui_graph_xy())
            .look(HandleLook::Puck)
            .x(Bind::axis(0, x_name))
            .y(Bind::axis(1, y_name)),
    ];
    spec.shift_locks_axis = true;
    spec
}

// ---------------------------------------------------------------------------------------------
// LFO

/// The LFO shape through `points` points: from (0, 0) through (x_i, y_i) to (1, 0), straight.
pub fn lfo_level(x: f64, p: &[f64]) -> f64 {
    let n = p.len() / 2;
    let (mut px, mut py) = (0.0, 0.0);
    for i in 0..=n {
        let (nx, ny) = if i < n {
            (p[2 * i], p[2 * i + 1])
        } else {
            (1.0, 0.0)
        };
        if x <= nx {
            return if nx > px {
                py + (ny - py) * (x - px) / (nx - px)
            } else {
                ny
            };
        }
        (px, py) = (nx, ny);
    }
    0.0
}

/// `graph::lfo` with `points` points: phase 0–1 on x (grid by quarter), bipolar on y; the curve
/// through the points, playhead; each point ↔↕ between its neighbours; double-click adds a point
/// (event), Entf deletes the focused one.
pub fn lfo(t: Lang, points: usize) -> GraphSpec {
    let (px, py) = m::pad::LFO;
    let mut spec = GraphSpec::new(
        t.ui_graph_lfo_shape(),
        padded(Axis::new(AxisKind::Lin, 0.0, 1.0), px).grid(Grid::Divisions(4)),
        padded(Axis::new(AxisKind::Lin, -1.0, 1.0), py).grid(Grid::Divisions(2)),
    );
    for n in 1..=points {
        spec.slots.push(format!("x{n}"));
        spec.slots.push(format!("y{n}"));
    }
    spec.layers = vec![
        Layer::Curve(Curve {
            f: CurveFn::new(lfo_level),
            baseline: Some(0.0),
            fill: true,
        }),
        Layer::Marker(Marker::Playhead),
    ];
    spec.on_double_click = AreaAction::NewPoint;
    spec.deletable = true;
    spec.handles = (0..points)
        .map(|i| {
            Handle::dot(t.ui_graph_point(i + 1))
                .x(Bind::axis(2 * i, t.ui_graph_phase()))
                .y(Bind::axis(2 * i + 1, t.ui_graph_value()))
                .between(i.checked_sub(1), (i + 1 < points).then_some(i + 1))
        })
        .collect();
    spec
}

// ---------------------------------------------------------------------------------------------
// volt (WP-95)

/// The sample rate the curves of volt's filter assume (the EQ takes its own from the device).
const VA_RATE: f64 = 48_000.0;

/// `graph::filter` for volt: the response of the chosen filter type (the same transfer
/// functions as the DSP, `va_curve::filter_response_db`), one handle (Cutoff ↔, Resonanz ↕) and
/// under the curve the bar the matrix moves the cutoff over.
///
/// Slots: `cutoff` (Hz), `resonance` (0–1), `type` (index of `va_curve::FILTER_TYPES`), then
/// `mod1_src`, `mod1_dst`, `mod1_amt` … `mod4_amt`, `at_cut`, `lfo1_uni`, `lfo2_uni` (what
/// `va_curve::cutoff_mod_octaves` reads, from slot 3).
pub fn filter_va(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::EQ;
    let mut spec = GraphSpec::new(
        t.ui_graph_filter(),
        padded(Axis::new(AxisKind::Log, 20.0, 20_000.0), px).labelled(),
        padded(Axis::new(AxisKind::Db, -36.0, 12.0), py),
    );
    spec.slots = slots(&["cutoff", "resonance", "type"]);
    for n in 1..=4 {
        for what in ["src", "dst", "amt"] {
            spec.slots.push(format!("mod{n}_{what}"));
        }
    }
    spec.slots
        .extend(["at_cut", "lfo1_uni", "lfo2_uni"].map(str::to_owned));
    let response = |hz: f64, p: &[f64]| {
        va_curve::filter_response_db(hz, p[0], p[1], p[2].round() as usize, VA_RATE)
    };
    spec.layers = vec![
        Layer::Region(Region::CutoffMod {
            cutoff: 0,
            matrix: 3,
        }),
        // Filled down to the bottom of the plot, as the mockup draws it.
        Layer::Curve(Curve {
            f: CurveFn::new(move |x, p| response(x, p)),
            baseline: None,
            fill: true,
        }),
    ];
    spec.handles = vec![
        Handle::dot(t.ui_graph_filter())
            .x(Bind::axis(0, t.ui_graph_cutoff()))
            .y(Bind::knob(1, t.ui_graph_resonance(), false))
            .at(PosFn::new(move |p| (p[0], response(p[0], p)))),
    ];
    spec
}

/// The oscillator mix over two periods of oscillator A, display only. Slots: `wave_a`,
/// `wave_b`, `pw_a`, `pw_b`, `b_oct`, `b_fine`, `mix`, `sub` (what `va_curve::signal_at` reads).
pub fn signal(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::WAVEFORM;
    let mut spec = GraphSpec::new(
        t.ui_graph_signal(),
        padded(Axis::new(AxisKind::Lin, 0.0, va_curve::SIGNAL_PERIODS), px)
            .grid(Grid::Divisions(2)),
        padded(Axis::new(AxisKind::Lin, -1.0, 1.0), py).grid(Grid::Ticks(vec![Tick::minor(0.0)])),
    );
    spec.slots = slots(&[
        "wave_a", "wave_b", "pw_a", "pw_b", "b_oct", "b_fine", "mix", "sub",
    ]);
    spec.layers = vec![Layer::Curve(Curve {
        f: CurveFn::new(va_curve::signal_at),
        baseline: Some(0.0),
        fill: true,
    })];
    spec
}

/// One cycle of the LFO shape in slot `shape` (an index of `va_curve::LFO_SHAPES`), display
/// only.
pub fn lfo_shape(t: Lang) -> GraphSpec {
    let (px, py) = m::pad::LFO;
    let mut spec = GraphSpec::new(
        t.ui_graph_lfo_shape(),
        padded(Axis::new(AxisKind::Lin, 0.0, 1.0), px).grid(Grid::Divisions(4)),
        padded(Axis::new(AxisKind::Lin, -1.0, 1.0), py).grid(Grid::Divisions(2)),
    );
    spec.slots = slots(&["shape"]);
    spec.layers = vec![Layer::Curve(Curve {
        f: CurveFn::new(|x, p| va_curve::lfo_shape_at(p[0].round() as usize, x)),
        baseline: Some(0.0),
        fill: true,
    })];
    spec
}

// ---------------------------------------------------------------------------------------------
// surf (#107)

/// surf's wavetable of oscillator `osc` (0 = A, 1 = B) as a stack of frames
/// (`wavetable::preview_at`), the frame at the position on top, a handle on the position axis
/// (↔ moves the position) and the range the matrix moves the position over
/// (`wavetable::position_mod_range`, 100 % = the whole table).
///
/// Slots: `table` (index of `wavetable::TABLES`), `position` (0–1), then `mod1_src`, `mod1_dst`,
/// `mod1_amt` … `mod4_amt`, `lfo1_uni`, `lfo2_uni`.
pub fn wavetable(t: Lang, osc: usize) -> GraphSpec {
    let (px, py) = m::pad::STACK;
    let mut spec = GraphSpec::new(
        t.ui_graph_wavetable(),
        padded(Axis::new(AxisKind::Lin, 0.0, 1.0), px).grid(Grid::None),
        padded(Axis::new(AxisKind::Lin, 0.0, 1.0), py).grid(Grid::None),
    );
    spec.slots = slots(&["table", "position"]);
    for n in 1..=4 {
        for what in ["src", "dst", "amt"] {
            spec.slots.push(format!("mod{n}_{what}"));
        }
    }
    spec.slots
        .extend(["lfo1_uni", "lfo2_uni"].map(str::to_owned));
    let look = m::STACK;
    let stack = Stack {
        f: StackFn::new(|q, phase, p| wavetable::preview_at(p[0].round() as usize, q, phase)),
        position: 1,
        range: RangeFn::new(move |p| wavetable::position_mod_range(osc, &p[2..], 1.0)),
        frames: look.frames,
        cycle_w: look.cycle_w,
        amp: look.amp,
    };
    let at = stack.clone();
    spec.handles = vec![
        Handle::dot(t.ui_graph_wavetable())
            .x(Bind::axis(1, t.ui_graph_position())
                .scaled(1.0 - look.cycle_w)
                .after(&[], look.cycle_w))
            .at(PosFn::new(move |p| at.handle_at(p[1]))),
    ];
    spec.layers = vec![Layer::Stack(stack)];
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn slot_names_and_counts() {
        assert_eq!(envelope(Lang::De).slots.len(), 5);
        assert_eq!(eq(Lang::De, 8, 18.0).slots.len(), 40);
        assert_eq!(eq(Lang::De, 2, 18.0).slots[5], "b2_on");
        assert_eq!(filter(Lang::De).slot("resonance"), Some(1));
        assert_eq!(transfer(Lang::De).slot("knee"), Some(2));
        assert_eq!(waveform(Lang::De, 1.0).slots, ["start", "end"]);
        assert_eq!(lfo(Lang::De, 3).slots.len(), 6);
        let xy = xy(
            Lang::De,
            Axis::new(AxisKind::Lin, 0.0, 1.0),
            Axis::new(AxisKind::Lin, 0.0, 1.0),
            "Zeit",
            "Feedback",
        );
        assert_eq!(xy.slots, ["x", "y"]);
    }

    #[test]
    fn handles_bind_at_most_three_slots_inside_the_slot_list() {
        for spec in [
            envelope(Lang::De),
            eq(Lang::De, 8, 18.0),
            filter(Lang::De),
            transfer(Lang::De),
            waveform(Lang::De, 1.0),
            lfo(Lang::De, 4),
        ] {
            for h in &spec.handles {
                assert!(h.binds().count() <= 3, "{}", h.name);
                for b in h.binds() {
                    assert!(b.slot < spec.slots.len(), "{}", h.name);
                    assert!(b.base.iter().all(|&s| s < spec.slots.len()));
                }
            }
        }
    }

    #[test]
    fn envelope_runs_from_zero_to_one_to_sustain_to_zero() {
        let p = [0.2, 0.4, 0.5, 0.5, 0.5];
        let [xa, xd, xs, xr] = envelope_points(&p);
        assert!(close(envelope_level(0.0, &p), 0.0));
        assert!(close(envelope_level(xa, &p), 1.0));
        assert!(close(envelope_level(xd, &p), 0.5));
        assert!(close(envelope_level(xs - 0.001, &p), 0.5));
        assert!(close(envelope_level(xr, &p), 0.0));
        assert!(xr < 1.0, "{xr}");
        let max = [1.0, 1.0, 1.0, 1.0, 1.0];
        assert!(envelope_points(&max)[3] <= 1.0 + 1e-12);
    }

    #[test]
    fn eq_bell_peaks_at_its_frequency_and_off_bands_do_nothing() {
        let mut p = vec![1.0, 1000.0, 6.0, 1.0, band::BELL];
        assert!(close(eq_response(&p, 1, 1000.0), 6.0));
        assert!(eq_response(&p, 1, 20.0).abs() < 0.01);
        p[0] = 0.0;
        assert_eq!(eq_response(&p, 1, 1000.0), 0.0);
    }

    #[test]
    fn the_eq_curve_is_the_filters_response_at_the_devices_sample_rate() {
        // A high cut at 12 kHz and a bell at 8 kHz: near Nyquist the response depends on the rate.
        let p = [
            1.0,
            12_000.0,
            0.0,
            0.71,
            band::HIGH_CUT, //
            1.0,
            8000.0,
            6.0,
            1.0,
            band::BELL,
        ];
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            let spec = eq_at(Lang::De, 2, 18.0, rate);
            let Some(Layer::Curve(curve)) = spec.layers.get(1) else {
                panic!("the curve layer");
            };
            for hz in [100.0, 8000.0, 12_000.0, 18_000.0] {
                let want = eq_response_at(&p, 2, hz, rate);
                assert!((curve.f.at(hz, &p) - want).abs() < 1e-9, "{rate} {hz}");
            }
            // The bell reaches its gain at its own frequency whatever the rate (cut: ≈ -1 dB there).
            let bell = eq_band_response_at(&p, 1, 8000.0, rate);
            assert!((bell - 6.0).abs() < 1e-6, "{rate}: {bell}");
        }
        // 18 kHz is 4.4 dB lower against 44.1 kHz's Nyquist than against 96 kHz's.
        let (a, b) = (
            eq_response_at(&p, 2, 18_000.0, 44_100.0),
            eq_response_at(&p, 2, 18_000.0, 96_000.0),
        );
        assert!((a - b).abs() > 1.0, "{a} vs {b}");
        assert_eq!(
            eq_response(&p, 2, 5000.0),
            eq_response_at(&p, 2, 5000.0, 48_000.0)
        );
    }

    #[test]
    fn eq_handle_sits_on_the_summed_curve_except_cuts() {
        let spec = eq(Lang::De, 2, 18.0);
        let mut p = vec![0.0; 10];
        p[..5].copy_from_slice(&[1.0, 100.0, 0.0, 1.0, band::LOW_CUT]);
        p[5..].copy_from_slice(&[1.0, 1000.0, 5.0, 1.0, band::BELL]);
        let (_, y0) = spec.handles[0].at.as_ref().unwrap().at(&p);
        assert_eq!(y0, -3.0);
        let (x1, y1) = spec.handles[1].at.as_ref().unwrap().at(&p);
        assert_eq!(x1, 1000.0);
        assert!(close(y1, eq_response(&p, 2, 1000.0)));
        // A cut band has no gain binding.
        assert!(!spec.handles[0].y.as_ref().unwrap().active(&p));
        assert!(spec.handles[1].y.as_ref().unwrap().active(&p));
    }

    #[test]
    fn filter_passes_the_pass_band_and_cuts_the_rest() {
        let lp = |hz| filter_response(hz, 1000.0, 0.0, 1.0);
        assert!(lp(50.0).abs() < 0.5);
        assert!(lp(8000.0) < -36.0);
        // Resonance lifts the peak at the cutoff.
        assert!(filter_response(1000.0, 1000.0, 1.0, 0.0) > 12.0);
        let hp = |hz| filter_response(hz, 1000.0, 0.0, 3.0);
        assert!(hp(8000.0).abs() < 0.5 && hp(50.0) < -30.0);
    }

    #[test]
    fn transfer_follows_the_diagonal_below_and_the_ratio_above() {
        assert_eq!(transfer_level(-40.0, -18.0, 4.0, 0.0), -40.0);
        assert!(close(
            transfer_level(0.0, -18.0, 4.0, 0.0),
            -18.0 + 18.0 / 4.0
        ));
        // A soft knee is continuous at its ends.
        let below = transfer_level(-21.0, -18.0, 4.0, 1.0);
        assert!(close(below, -21.0));
        let above = transfer_level(-15.0, -18.0, 4.0, 1.0);
        assert!(close(above, -18.0 + 3.0 / 4.0));
    }

    #[test]
    fn lfo_is_straight_between_its_points() {
        let p = [0.25, 1.0, 0.75, -1.0];
        assert!(close(lfo_level(0.0, &p), 0.0));
        assert!(close(lfo_level(0.125, &p), 0.5));
        assert!(close(lfo_level(0.5, &p), 0.0));
        assert!(close(lfo_level(1.0, &p), 0.0));
    }

    #[test]
    fn waveform_edges_keep_their_order() {
        let spec = waveform(Lang::De, 1.0);
        assert_eq!(spec.handles[0].between, [None, Some(1)]);
        assert_eq!(spec.handles[1].between, [Some(0), None]);
    }
}
