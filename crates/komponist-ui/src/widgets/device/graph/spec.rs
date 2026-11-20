//! What a graph configuration is made of (device handoff §5): two axes, layers and handles.
//!
//! A [`GraphSpec`] says nothing about pixels or parameters ids. Its **slots** are numbered
//! values the host owns (`GraphSpec::slots` names them: `"attack"`, `"b1_freq"`); the host maps
//! slots to parameters, hands the widget the slot values and [`super::SlotInfo`]s, and gets the
//! changed values back. Curves are functions `f(x, slots) -> y` the host's closure samples; handles
//! bind up to three slots (X, Y, mouse wheel) with limits and order constraints.

use std::fmt;
use std::sync::Arc;

/// `f(x, slots) -> y` in axis units.
#[derive(Clone)]
pub struct CurveFn(Arc<CurveClosure>);

type CurveClosure = dyn Fn(f64, &[f64]) -> f64 + Send + Sync;

impl CurveFn {
    /// A curve function.
    pub fn new(f: impl Fn(f64, &[f64]) -> f64 + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// `y` at `x` for `slots`.
    pub fn at(&self, x: f64, slots: &[f64]) -> f64 {
        (self.0)(x, slots)
    }
}

impl fmt::Debug for CurveFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CurveFn(..)")
    }
}

/// A condition on the slots (a band is on, a band has a gain).
#[derive(Clone)]
pub struct Pred(Arc<PredClosure>);

type PredClosure = dyn Fn(&[f64]) -> bool + Send + Sync;

impl Pred {
    /// A condition.
    pub fn new(f: impl Fn(&[f64]) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Whether it holds for `slots`.
    pub fn holds(&self, slots: &[f64]) -> bool {
        (self.0)(slots)
    }
}

impl fmt::Debug for Pred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pred(..)")
    }
}

/// Where a handle is drawn, in axis units: `(x, y)` for `slots`.
#[derive(Clone)]
pub struct PosFn(Arc<PosClosure>);

type PosClosure = dyn Fn(&[f64]) -> (f64, f64) + Send + Sync;

impl PosFn {
    /// A position function.
    pub fn new(f: impl Fn(&[f64]) -> (f64, f64) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// The position for `slots`.
    pub fn at(&self, slots: &[f64]) -> (f64, f64) {
        (self.0)(slots)
    }
}

impl fmt::Debug for PosFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PosFn(..)")
    }
}

// ---------------------------------------------------------------------------------------------
// Axes

/// How an axis maps values onto its length and which grid and labels it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisKind {
    /// Linear.
    Lin,
    /// Logarithmic (needs `min > 0`, else linear); the grid follows the decades.
    Log,
    /// Decibels: linear in dB, the grid every 12 dB around 0 dB.
    Db,
    /// Seconds: linear, labels as `ms` / `s`.
    Time,
}

/// One grid line.
#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    /// Where, in axis units.
    pub value: f64,
    /// `line.2` instead of `line.1`.
    pub major: bool,
    /// The label (x axis: at the bottom, y axis: at the left), `type.mono-xs` `text.2`.
    pub label: Option<String>,
}

impl Tick {
    /// A minor line.
    pub fn minor(value: f64) -> Self {
        Self {
            value,
            major: false,
            label: None,
        }
    }

    /// A major line with a label.
    pub fn major(value: f64, label: impl Into<String>) -> Self {
        Self {
            value,
            major: true,
            label: Some(label.into()),
        }
    }

    /// A major line without label.
    pub fn line(value: f64) -> Self {
        Self {
            value,
            major: true,
            label: None,
        }
    }
}

/// The grid of an axis.
#[derive(Clone, Debug, PartialEq)]
pub enum Grid {
    /// Per [`AxisKind`]: decades, 12 dB steps, or four divisions.
    Auto,
    /// `n` equal divisions (`n − 1` interior lines, all `line.1`).
    Divisions(u32),
    /// These lines.
    Ticks(Vec<Tick>),
    /// No lines.
    None,
}

/// An axis: kind, range, padding and grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    /// The kind.
    pub kind: AxisKind,
    /// Value at the low end (left / bottom).
    pub min: f64,
    /// Value at the high end (right / top).
    pub max: f64,
    /// Padding in px between the widget's edge and the low and high end.
    pub pad: [f32; 2],
    /// The grid.
    pub grid: Grid,
    /// Draw the labels of the major ticks that have one (automatic ones on a log axis).
    pub labels: bool,
}

impl Axis {
    /// An axis of `kind` from `min` to `max`, no padding, automatic grid, no labels.
    pub fn new(kind: AxisKind, min: f64, max: f64) -> Self {
        Self {
            kind,
            min,
            max,
            pad: [0.0, 0.0],
            grid: Grid::Auto,
            labels: false,
        }
    }

    /// Padding at the low and high end.
    #[must_use]
    pub fn padding(mut self, pad: [f32; 2]) -> Self {
        self.pad = pad;
        self
    }

    /// The grid.
    #[must_use]
    pub fn grid(mut self, grid: Grid) -> Self {
        self.grid = grid;
        self
    }

    /// Draws the labels.
    #[must_use]
    pub fn labelled(mut self) -> Self {
        self.labels = true;
        self
    }

    fn is_log(&self) -> bool {
        self.kind == AxisKind::Log && self.min > 0.0 && self.max > self.min
    }

    /// `v` held inside the range.
    pub fn clamp(&self, v: f64) -> f64 {
        let (lo, hi) = if self.min <= self.max {
            (self.min, self.max)
        } else {
            (self.max, self.min)
        };
        if v.is_nan() { lo } else { v.clamp(lo, hi) }
    }

    /// The position of `v` along the axis: 0 at `min`, 1 at `max` (not clamped).
    pub fn fraction(&self, v: f64) -> f64 {
        if self.max == self.min {
            return 0.0;
        }
        if self.is_log() {
            if v <= 0.0 {
                return f64::NEG_INFINITY;
            }
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        }
    }

    /// The value at `fraction`.
    pub fn value(&self, fraction: f64) -> f64 {
        if self.is_log() {
            self.min * (self.max / self.min).powf(fraction)
        } else {
            self.min + fraction * (self.max - self.min)
        }
    }

    /// The grid lines of the axis.
    pub fn ticks(&self) -> Vec<Tick> {
        let inside = |v: f64| {
            let f = self.fraction(v);
            f > 1e-6 && f < 1.0 - 1e-6
        };
        match &self.grid {
            Grid::None => Vec::new(),
            Grid::Ticks(t) => t.clone(),
            Grid::Divisions(n) => (1..*n)
                .map(|i| Tick::minor(self.value(f64::from(i) / f64::from(*n))))
                .collect(),
            Grid::Auto => match self.kind {
                AxisKind::Log if self.is_log() => {
                    let k0 = self.min.log10().floor() as i32;
                    let k1 = self.max.log10().ceil() as i32;
                    let mut out = Vec::new();
                    for k in k0..=k1 {
                        let decade = 10f64.powi(k);
                        for m in [1.0, 2.0, 5.0] {
                            let v = m * decade;
                            if !inside(v) {
                                continue;
                            }
                            out.push(if m == 1.0 {
                                Tick::major(v, hz_label(v))
                            } else {
                                Tick::minor(v)
                            });
                        }
                    }
                    out
                }
                AxisKind::Db => {
                    let (lo, hi) = (self.min.min(self.max), self.max.max(self.min));
                    let mut out = Vec::new();
                    let first = (lo / 12.0).ceil() as i32;
                    let last = (hi / 12.0).floor() as i32;
                    for i in first..=last {
                        let v = f64::from(i) * 12.0;
                        if inside(v) {
                            out.push(if v == 0.0 {
                                Tick::line(v)
                            } else {
                                Tick::minor(v)
                            });
                        }
                    }
                    out
                }
                _ => (1..4)
                    .map(|i| {
                        let v = self.value(f64::from(i) / 4.0);
                        if self.kind == AxisKind::Time {
                            Tick {
                                value: v,
                                major: false,
                                label: Some(time_label(v)),
                            }
                        } else {
                            Tick::minor(v)
                        }
                    })
                    .collect(),
            },
        }
    }
}

/// `100`, `1k`, `10k`: the label of a frequency tick.
pub fn hz_label(hz: f64) -> String {
    if hz >= 1000.0 {
        format!("{}k", trim(hz / 1000.0))
    } else {
        trim(hz)
    }
}

/// `250 ms`, `1.5 s`: the label of a time tick (seconds in).
pub fn time_label(seconds: f64) -> String {
    if seconds < 1.0 {
        format!("{} ms", trim(seconds * 1000.0))
    } else {
        format!("{} s", trim(seconds))
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

// ---------------------------------------------------------------------------------------------
// Bindings and handles

/// How a binding turns movement into a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The handle moves along the axis; the axis value is `k · (slot + Σ base) + gap`.
    Axis,
    /// Like a knob: 200 px (a keyboard step: 1 %) cover the slot's range, linear or `log`.
    Knob {
        /// Logarithmic over the slot's range (needs a positive minimum).
        log: bool,
    },
}

/// One parameter a handle edits: an axis (X or Y) or the mouse wheel.
#[derive(Clone, Debug)]
pub struct Bind {
    /// The slot it writes.
    pub slot: usize,
    /// German word for what it is, in the accessible name: „Frequenz“, „Gain“, „Q“.
    pub label: String,
    /// How it moves.
    pub mode: Mode,
    /// Axis units per slot unit (`Mode::Axis`): 1 if the slot is in axis units.
    pub scale: f64,
    /// Slots whose values add to this one before the scale (segments that follow each other).
    pub base: Vec<usize>,
    /// Axis units added after the scale (a fixed hold).
    pub gap: f64,
    /// The binding only works while this holds (a low cut has no gain).
    pub when: Option<Pred>,
}

impl Bind {
    /// A binding along the axis; the slot is in axis units.
    pub fn axis(slot: usize, label: impl Into<String>) -> Self {
        Self {
            slot,
            label: label.into(),
            mode: Mode::Axis,
            scale: 1.0,
            base: Vec::new(),
            gap: 0.0,
            when: None,
        }
    }

    /// A knob-like binding.
    pub fn knob(slot: usize, label: impl Into<String>, log: bool) -> Self {
        Self {
            mode: Mode::Knob { log },
            ..Self::axis(slot, label)
        }
    }

    /// Axis units per slot unit.
    #[must_use]
    pub fn scaled(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// The value follows `base` (in sequence) plus a fixed `gap`: `k · (Σ base + slot) + gap`.
    #[must_use]
    pub fn after(mut self, base: &[usize], gap: f64) -> Self {
        self.base = base.to_vec();
        self.gap = gap;
        self
    }

    /// Only while `when` holds.
    #[must_use]
    pub fn only_if(mut self, when: Pred) -> Self {
        self.when = Some(when);
        self
    }

    /// The axis value for `slots`.
    pub fn axis_value(&self, slots: &[f64]) -> f64 {
        let sum: f64 = self.base.iter().map(|&b| slots[b]).sum::<f64>() + slots[self.slot];
        self.scale * sum + self.gap
    }

    /// The slot value that gives axis value `v`, other slots as in `slots`.
    pub fn slot_for(&self, v: f64, slots: &[f64]) -> f64 {
        let base: f64 = self.base.iter().map(|&b| slots[b]).sum();
        (v - self.gap) / self.scale - base
    }

    /// Whether it applies for `slots`.
    pub fn active(&self, slots: &[f64]) -> bool {
        self.when.as_ref().is_none_or(|p| p.holds(slots))
    }
}

/// What a handle looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleLook {
    /// A circle, r 5, optionally numbered.
    Dot,
    /// A vertical line over the whole height with a flag, for waveform start and end.
    Edge {
        /// The flag hangs left of the line (the end, `true`) or right of it (the start, `false`).
        flag_left: bool,
    },
    /// A vertical line with a triangular marker head.
    Marker(MarkerHead),
    /// The XY puck: a filled circle in the track colour.
    Puck,
}

/// Direction of a triangular waveform marker head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerHead {
    Right,
    Left,
    Down,
}

/// A handle: up to three bindings and the way it is drawn.
#[derive(Clone, Debug)]
pub struct Handle {
    /// German name, the first part of the accessible name: „Band 2“.
    pub name: String,
    /// The number in the circle.
    pub number: Option<u32>,
    /// Its look.
    pub look: HandleLook,
    /// ↔.
    pub x: Option<Bind>,
    /// ↕.
    pub y: Option<Bind>,
    /// The mouse wheel.
    pub wheel: Option<Bind>,
    /// Where it is drawn, if not at its bindings' axis values.
    pub at: Option<PosFn>,
    /// Shown only while this holds (a band that is on).
    pub visible: Option<Pred>,
    /// The indices of the handles whose X it must not pass: `[before, after]`.
    pub between: [Option<usize>; 2],
}

impl Handle {
    /// A dot named `name`.
    pub fn dot(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            number: None,
            look: HandleLook::Dot,
            x: None,
            y: None,
            wheel: None,
            at: None,
            visible: None,
            between: [None, None],
        }
    }

    /// With look `look`.
    #[must_use]
    pub fn look(mut self, look: HandleLook) -> Self {
        self.look = look;
        self
    }

    /// Numbered.
    #[must_use]
    pub fn numbered(mut self, number: u32) -> Self {
        self.number = Some(number);
        self
    }

    /// ↔ binding.
    #[must_use]
    pub fn x(mut self, bind: Bind) -> Self {
        self.x = Some(bind);
        self
    }

    /// ↕ binding.
    #[must_use]
    pub fn y(mut self, bind: Bind) -> Self {
        self.y = Some(bind);
        self
    }

    /// Mouse wheel binding.
    #[must_use]
    pub fn wheel(mut self, bind: Bind) -> Self {
        self.wheel = Some(bind);
        self
    }

    /// Position function.
    #[must_use]
    pub fn at(mut self, at: PosFn) -> Self {
        self.at = Some(at);
        self
    }

    /// Visibility condition.
    #[must_use]
    pub fn visible_if(mut self, p: Pred) -> Self {
        self.visible = Some(p);
        self
    }

    /// Order constraint: ↔ stays between the handles `before` and `after`.
    #[must_use]
    pub fn between(mut self, before: Option<usize>, after: Option<usize>) -> Self {
        self.between = [before, after];
        self
    }

    /// The slots it binds, in the order X, Y, wheel.
    pub fn binds(&self) -> impl Iterator<Item = &Bind> {
        [&self.x, &self.y, &self.wheel].into_iter().flatten()
    }
}

// ---------------------------------------------------------------------------------------------
// Layers

/// What the live data is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKind {
    /// Levels 0–1 from the bottom, filled with `surface.3`.
    Spectrum,
    /// Amplitudes 0–1 around the zero line of the y axis, filled with the track colour.
    Sample,
}

/// A region.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Region {
    /// Everything left of the slot `start` and right of the slot `end` (axis values via `scale`)
    /// darkened (`surface.1` at 75 %).
    Outside {
        /// Slot of the start.
        start: usize,
        /// Slot of the end.
        end: usize,
        /// Axis units per slot unit.
        scale: f64,
    },
    /// A full-height track-coloured highlight between two positions.
    Inside {
        /// Start and end slots.
        bounds: [usize; 2],
        /// Axis units per slot unit, one scale per boundary.
        scales: [f64; 2],
    },
    /// Fade ramps with full-height markers and shaded fade regions.
    Fades {
        /// Start and end slots in visual order.
        bounds: [usize; 2],
        /// Axis units per slot unit, one scale per boundary.
        scales: [f64; 2],
        /// Duration slots in visual order (fade-out first for reverse playback).
        durations: [usize; 2],
    },
    /// A thin bar under a log frequency axis from `cutoff · 2^lo` to `cutoff · 2^hi`: the
    /// range the modulation matrix moves the cutoff over (volt). `cutoff` is the slot of the
    /// cutoff in Hz, `matrix` the first of the 15 slots `va_curve::cutoff_mod_octaves` reads.
    /// Nothing is drawn while the range is empty.
    CutoffMod {
        /// Slot of the cutoff.
        cutoff: usize,
        /// First slot of the matrix block.
        matrix: usize,
    },
}

/// A curve: `f(x, slots) -> y`, sampled per pixel, 1.5 px in the track colour, filled 14 % down
/// to the baseline.
#[derive(Clone, Debug)]
pub struct Curve {
    /// The function.
    pub f: CurveFn,
    /// The y value the fill runs to (`None` = the bottom of the axis).
    pub baseline: Option<f64>,
    /// Fill the area between the curve and the baseline.
    pub fill: bool,
}

/// A guide line (dashed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guide {
    /// The diagonal from the corner (min, min) to (max, max), `line.2`.
    Diagonal,
    /// A vertical line at the value of a slot (in axis units), `text.2`.
    VLine {
        /// The slot.
        slot: usize,
    },
    /// A crosshair at the values of two slots (solid, track colour at 50 %).
    Crosshair {
        /// X slot.
        x: usize,
        /// Y slot.
        y: usize,
    },
}

/// A marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// The playhead (`Graph::playhead`): 1 px `text.1`, dashed, from y 14 down.
    Playhead,
    /// A solid playhead covering the full height of the graph.
    FullHeightPlayhead,
    /// The level point (`Graph::level`): r 3 `text.1` on the first curve at the level's x.
    Level,
    /// IN, OUT and GR of the live level point as three lines of name and value in the strip right
    /// of the plot (the compressor: the strip is the x axis' high padding). IN is the level
    /// ([`super::Graph::level`]), OUT the first curve at that level, GR their difference.
    Levels,
    /// The readout of two slots in the corners: y name and value top left, x name and value
    /// bottom right (the XY field).
    Readout {
        /// X slot.
        x: usize,
        /// Y slot.
        y: usize,
    },
}

/// One layer. Drawn in this order: grid, data, region, curve, guide, marker, handle.
#[derive(Clone, Debug)]
pub enum Layer {
    /// Live data, display only.
    Data(DataKind),
    /// A region.
    Region(Region),
    /// A curve.
    Curve(Curve),
    /// A guide.
    Guide(Guide),
    /// A marker.
    Marker(Marker),
    /// A history line (display only): the values of [`super::Graph::trace`] are y values spread
    /// evenly over the x axis, drawn like a curve (1.5 px, filled 14 % down to `baseline`).
    Trace {
        /// The y value the fill runs to.
        baseline: f64,
    },
}

impl Layer {
    /// The position in the drawing order (handles are 6).
    pub fn order(&self) -> u8 {
        match self {
            Layer::Data(_) => 1,
            Layer::Region(_) => 2,
            Layer::Curve(_) | Layer::Trace { .. } => 3,
            Layer::Guide(_) => 4,
            Layer::Marker(_) => 5,
        }
    }
}

/// What a double-click on the empty area does (reported as a [`super::GraphEvent`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaAction {
    /// Nothing.
    None,
    /// A new point or band at the pointer.
    NewPoint,
    /// Reset the zoom.
    ResetZoom,
}

/// A graph configuration.
#[derive(Clone, Debug)]
pub struct GraphSpec {
    /// The title in the top left corner (empty: none).
    pub title: String,
    /// The accessible name of the group: the title, or what the configuration is.
    pub name: String,
    /// The x axis.
    pub x: Axis,
    /// The y axis.
    pub y: Axis,
    /// The layers (any order; drawn by [`Layer::order`]).
    pub layers: Vec<Layer>,
    /// The handles, in Tab order.
    pub handles: Vec<Handle>,
    /// The names of the slots, which the host binds to parameters.
    pub slots: Vec<String>,
    /// The double-click on the area.
    pub on_double_click: AreaAction,
    /// Shift locks the axis that moved less instead of making the drag fine (the XY field).
    pub shift_locks_axis: bool,
    /// Entf deletes the focused handle (reported as an event).
    pub deletable: bool,
    /// The grid lines stop at the plot area (the axis paddings) instead of running to the edges
    /// of the widget, for a padded plot beside readouts (the compressor).
    pub grid_inside: bool,
    /// The title sits in the bottom right corner instead of the top left, where a curve hanging
    /// from the top would cover it (the reduction history).
    pub title_bottom_right: bool,
}

impl GraphSpec {
    /// An empty configuration.
    pub fn new(title: impl Into<String>, x: Axis, y: Axis) -> Self {
        let title = title.into();
        Self {
            name: title.clone(),
            title,
            x,
            y,
            layers: Vec::new(),
            handles: Vec::new(),
            slots: Vec::new(),
            on_double_click: AreaAction::None,
            shift_locks_axis: false,
            deletable: false,
            grid_inside: false,
            title_bottom_right: false,
        }
    }

    /// The index of the slot `name`.
    pub fn slot(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s == name)
    }

    /// The first curve.
    pub fn first_curve(&self) -> Option<&Curve> {
        self.layers.iter().find_map(|l| match l {
            Layer::Curve(c) => Some(c),
            _ => None,
        })
    }

    /// The same configuration under another title (devices may adapt a configuration).
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self.name = self.title.clone();
        self
    }

    /// With the accessible name `name`.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_axis_maps_decades_evenly() {
        let a = Axis::new(AxisKind::Log, 20.0, 20000.0);
        assert!((a.fraction(20.0)).abs() < 1e-12);
        assert!((a.fraction(20000.0) - 1.0).abs() < 1e-12);
        assert!((a.fraction(632.455_532) - 0.5).abs() < 1e-6);
        assert!((a.value(0.5) - 632.455_532).abs() < 1e-3);
    }

    #[test]
    fn log_axis_ticks_are_decades_with_minor_2_and_5() {
        let a = Axis::new(AxisKind::Log, 20.0, 20000.0);
        let t = a.ticks();
        let majors: Vec<_> = t.iter().filter(|t| t.major).map(|t| t.value).collect();
        let minors: Vec<_> = t.iter().filter(|t| !t.major).map(|t| t.value).collect();
        assert_eq!(majors, [100.0, 1000.0, 10000.0]);
        assert_eq!(minors, [50.0, 200.0, 500.0, 2000.0, 5000.0]);
        assert_eq!(t[1].label.as_deref(), Some("100"));
        assert_eq!(hz_label(1000.0), "1k");
        assert_eq!(hz_label(10000.0), "10k");
    }

    #[test]
    fn db_axis_has_a_major_zero_line_and_no_edges() {
        let t = Axis::new(AxisKind::Db, -18.0, 18.0).ticks();
        let values: Vec<_> = t.iter().map(|t| t.value).collect();
        assert_eq!(values, [-12.0, 0.0, 12.0]);
        assert!(t[1].major && !t[0].major);
    }

    #[test]
    fn divisions_leave_out_the_edges() {
        let t = Axis::new(AxisKind::Lin, 0.0, 1.0)
            .grid(Grid::Divisions(8))
            .ticks();
        assert_eq!(t.len(), 7);
    }

    #[test]
    fn time_labels() {
        assert_eq!(time_label(0.25), "250 ms");
        assert_eq!(time_label(1.5), "1.5 s");
    }

    #[test]
    fn relative_bindings_chain_segments() {
        let b = Bind::axis(1, "Zeit").scaled(0.5).after(&[0], 0.1);
        let slots = [0.2, 0.4];
        assert!((b.axis_value(&slots) - (0.5 * 0.6 + 0.1)).abs() < 1e-12);
        assert!((b.slot_for(0.5, &slots) - (0.4 / 0.5 - 0.2)).abs() < 1e-12);
    }
}
