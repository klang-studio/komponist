//! The seven configurations of the graph (device handoff §5), constructors that return a
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
//! | [`waveform`] | `start`, `end` | milliseconds |
//! | [`xy`] | `x`, `y` | the axes' own units |
//! | [`lfo`] | `x1`, `y1`, `x2`, `y2`, … | phase 0–1, −1…1 |

use super::spec::{
    AreaAction, Axis, AxisKind, Bind, Curve, CurveFn, DataKind, GraphSpec, Grid, Guide, Handle,
    HandleLook, Layer, Marker, PosFn, Pred, Region, Tick,
};
use crate::widgets::metrics::device::graph as m;
use komponist_model::params::eq_curve::CURVE_RATE;

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
pub fn envelope() -> GraphSpec {
    let (px, py) = m::pad::ENVELOPE;
    let mut spec = GraphSpec::new(
        "Hüllkurve",
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
    let bend = || Bind::knob(env::CURVE, "Krümmung", false);
    spec.handles = vec![
        Handle::dot("Attack")
            .x(Bind::axis(env::ATTACK, "Zeit").scaled(k))
            .at(PosFn::new(|p| (envelope_points(p)[0], 1.0))),
        Handle::dot("Decay")
            .x(Bind::axis(env::DECAY, "Zeit")
                .scaled(k)
                .after(&[env::ATTACK], 0.0))
            .y(Bind::axis(env::SUSTAIN, "Sustain-Pegel"))
            .wheel(bend()),
        Handle::dot("Sustain")
            .y(Bind::axis(env::SUSTAIN, "Pegel"))
            .at(PosFn::new(|p| (envelope_points(p)[2], p[env::SUSTAIN]))),
        Handle::dot("Release")
            .x(Bind::axis(env::RELEASE, "Zeit")
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
    komponist_model::params::eq_curve::band_response_db_at(typ, fc, gain, q, hz, rate)
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
pub fn eq(bands: usize, range_db: f64) -> GraphSpec {
    eq_at(bands, range_db, CURVE_RATE)
}

/// [`eq`] for a device running at `rate` Hz: the curve is the real filters' response there.
pub fn eq_at(bands: usize, range_db: f64, rate: f64) -> GraphSpec {
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
    spec.name = "Parametrischer EQ".to_owned();
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
            Handle::dot(format!("Band {}", i + 1))
                .numbered(i as u32 + 1)
                .x(Bind::axis(o + 1, "Frequenz"))
                .y(Bind::axis(o + 2, "Gain").only_if(Pred::new(move |p| !is_cut(p))))
                .wheel(Bind::knob(o + 3, "Q", true))
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
pub fn filter() -> GraphSpec {
    let (px, py) = m::pad::EQ;
    let mut spec = GraphSpec::new(
        "Filter",
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
        Handle::dot("Filter")
            .numbered(1)
            .x(Bind::axis(0, "Cutoff"))
            .y(Bind::knob(1, "Resonanz", false))
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
pub fn transfer() -> GraphSpec {
    let (px, py) = m::pad::TRANSFER;
    let axis = |pad| padded(Axis::new(AxisKind::Db, -60.0, 0.0), pad).grid(Grid::Divisions(4));
    let mut spec = GraphSpec::new("Kennlinie", axis(px), axis(py));
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
        Handle::dot("Knie")
            .x(Bind::axis(0, "Threshold"))
            .y(Bind::knob(1, "Ratio", true))
            .at(PosFn::new(|p| (p[0], p[0]))),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// Waveform

/// `graph::waveform` for a sample of `length_s` seconds: time on x, bipolar on y; sample data,
/// the region outside start–end darkened, playhead; S and E as lines, ↔ only. Start and end are
/// in milliseconds.
pub fn waveform(length_s: f64) -> GraphSpec {
    let (px, py) = m::pad::WAVEFORM;
    let mut spec = GraphSpec::new(
        "",
        padded(Axis::new(AxisKind::Time, 0.0, length_s), px).grid(Grid::None),
        padded(Axis::new(AxisKind::Lin, -1.0, 1.0), py).grid(Grid::Ticks(vec![Tick::minor(0.0)])),
    );
    spec.name = "Wellenform".to_owned();
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
        Handle::dot("Sample-Start")
            .look(HandleLook::Edge { flag_left: false })
            .x(Bind::axis(0, "Position").scaled(0.001))
            .between(None, Some(1)),
        Handle::dot("Sample-Ende")
            .look(HandleLook::Edge { flag_left: true })
            .x(Bind::axis(1, "Position").scaled(0.001))
            .between(Some(0), None),
    ];
    spec
}

// ---------------------------------------------------------------------------------------------
// XY

/// `graph::xy`: the two parameters' own axes; the crosshair and one puck, ↔↕, Shift locks the axis
/// that moved less; `x_name` and `y_name` are the parameter names (accessible names, readout).
pub fn xy(x: Axis, y: Axis, x_name: &str, y_name: &str) -> GraphSpec {
    let (px, py) = m::pad::XY;
    let mut spec = GraphSpec::new(
        "",
        padded(x, px).grid(Grid::Divisions(4)),
        padded(y, py).grid(Grid::Divisions(4)),
    );
    spec.name = "XY-Feld".to_owned();
    spec.slots = slots(&["x", "y"]);
    spec.layers = vec![
        Layer::Guide(Guide::Crosshair { x: 0, y: 1 }),
        Layer::Marker(Marker::Readout { x: 0, y: 1 }),
    ];
    spec.handles = vec![
        Handle::dot("XY-Feld")
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
pub fn lfo(points: usize) -> GraphSpec {
    let (px, py) = m::pad::LFO;
    let mut spec = GraphSpec::new(
        "LFO · Form",
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
            Handle::dot(format!("Punkt {}", i + 1))
                .x(Bind::axis(2 * i, "Phase"))
                .y(Bind::axis(2 * i + 1, "Wert"))
                .between(i.checked_sub(1), (i + 1 < points).then_some(i + 1))
        })
        .collect();
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
        assert_eq!(envelope().slots.len(), 5);
        assert_eq!(eq(8, 18.0).slots.len(), 40);
        assert_eq!(eq(2, 18.0).slots[5], "b2_on");
        assert_eq!(filter().slot("resonance"), Some(1));
        assert_eq!(transfer().slot("knee"), Some(2));
        assert_eq!(waveform(1.0).slots, ["start", "end"]);
        assert_eq!(lfo(3).slots.len(), 6);
        let xy = xy(
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
            envelope(),
            eq(8, 18.0),
            filter(),
            transfer(),
            waveform(1.0),
            lfo(4),
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
    fn eq_handle_sits_on_the_summed_curve_except_cuts() {
        let spec = eq(2, 18.0);
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
        let spec = waveform(1.0);
        assert_eq!(spec.handles[0].between, [None, Some(1)]);
        assert_eq!(spec.handles[1].between, [Some(0), None]);
    }
}
