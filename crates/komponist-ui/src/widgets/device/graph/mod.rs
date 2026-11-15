//! The graph (device handoff §5): one widget for envelope, EQ, filter, compressor curve,
//! waveform, XY field and LFO shape. A [`GraphSpec`] ([`configs`] has the seven constructors)
//! says what is on it; [`Graph`] draws it, handles the handles and reports what changed.
//!
//! **Drawing**, in this order: grid and axis labels (`line.1`, major `line.2`; `type.mono-xs`
//! `text.2`), `Data` (display only, hidden while bypassed), `Region`, `Curve` (1.5 px track colour,
//! 14 % fill to the baseline), `Guide` (dashed), `Marker`, `Handle` (r 5, `surface.0`, 1.5 px
//! `text.1`; selected 2 px accent), then the title. Bypass and disabled: curve and handles in
//! `text.4`, no data.
//!
//! **Operation**: drag a handle like a knob (Shift fine, Ctrl coarse, Alt-click the default),
//! value tooltip at the handle, the wheel over a handle moves its wheel binding, Tab changes the
//! handle and the arrows move it (Shift fine, Ctrl coarse), Entf asks the host to delete it,
//! a double-click on the empty area asks it for a new point or resets the zoom
//! ([`GraphEvent`]). A disabled graph takes no input; a bypassed one stays operable.
//!
//! **AccessKit**: a group named after the graph; every bound parameter of every visible handle is
//! a `Slider` named „Band 2 Frequenz“ with the value as text; they take increment, decrement
//! and set-value.

pub mod configs;
pub mod gallery;
pub mod spec;

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{
    Color32, CursorIcon, Event, EventFilter, Id, Key, Painter, Pos2, Rect, Response, Role, Sense,
    Shape, Stroke, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use komponist_model::params::va_curve;
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, radius, widget};

pub use spec::{
    AreaAction, Axis, AxisKind, Bind, Curve, CurveFn, DataKind, GraphSpec, Grid, Guide, Handle,
    HandleLook, Layer, Marker, Mode, PosFn, Pred, Region, Tick,
};

use crate::drag::{self, speed};
use crate::state::{self, State};
use crate::widgets::metrics::device::graph as m;
use crate::widgets::number_field::{accesskit_requests, expose_value, wheel_steps};
use crate::{focus, fonts, px, tooltip};

/// Share of an axis (or of a slot's range) one keyboard step moves.
const KEY_STEP: f64 = 0.01;
/// Share of a slot's range one wheel notch moves.
const WHEEL_STEP: f64 = 0.02;

/// What the host knows about a slot: its range, default, whether it can be edited and how its
/// value reads (the tooltip and the accessible value).
pub struct SlotInfo<'a> {
    /// Lowest value.
    pub min: f64,
    /// Highest value.
    pub max: f64,
    /// Alt-click.
    pub default: f64,
    /// `false` for a slot that no parameter backs: it takes part in the curve but cannot change.
    pub editable: bool,
    /// The text of a value.
    pub format: Box<dyn Fn(f64) -> String + 'a>,
}

impl<'a> SlotInfo<'a> {
    /// An editable slot in `min..=max`, shown as a plain number.
    pub fn new(min: f64, max: f64, default: f64) -> Self {
        Self {
            min,
            max,
            default,
            editable: true,
            format: Box::new(|v| crate::widgets::number_field::format_number(v, 2)),
        }
    }

    /// The slot of a parameter: its plain range, default and text; with `normalized` its 0–1
    /// position instead (the envelope's segments are drawn from normalised times).
    pub fn from_param(
        info: &komponist_model::params::ParamInfo,
        normalized: bool,
    ) -> SlotInfo<'static> {
        let info = info.clone();
        if normalized {
            let default = info.default_normalized();
            SlotInfo::new(0.0, 1.0, default).format(move |n| info.format(info.to_plain(n)))
        } else {
            let (lo, hi) = info.range();
            let default = info.default;
            SlotInfo::new(lo, hi, default).format(move |v| info.format(v))
        }
    }

    /// With the text function `format`.
    #[must_use]
    pub fn format(mut self, format: impl Fn(f64) -> String + 'a) -> Self {
        self.format = Box::new(format);
        self
    }

    /// Not editable.
    #[must_use]
    pub fn locked(mut self) -> Self {
        self.editable = false;
        self
    }

    fn normalized(&self, v: f64, log: bool) -> f64 {
        let (lo, hi) = (self.min, self.max);
        if hi <= lo {
            return 0.0;
        }
        let v = v.clamp(lo, hi);
        if log && lo > 0.0 {
            (v / lo).ln() / (hi / lo).ln()
        } else {
            (v - lo) / (hi - lo)
        }
    }

    fn denormalized(&self, n: f64, log: bool) -> f64 {
        let n = n.clamp(0.0, 1.0);
        if log && self.min > 0.0 {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * (self.max - self.min)
        }
    }
}

/// How the graph looks regarding the device's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// Track colour, data shown.
    #[default]
    Normal,
    /// Bypassed device: curve and handles `text.4`, no data; still operable.
    Bypass,
    /// Disabled: as bypass, and no input.
    Disabled,
}

/// What the host has to do something about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GraphEvent {
    /// A double-click on the empty area (config: [`AreaAction::NewPoint`]): a new point or band at
    /// these axis values.
    AddPoint {
        /// X in axis units.
        x: f64,
        /// Y in axis units.
        y: f64,
    },
    /// Entf on the focused handle with this index.
    Delete(usize),
    /// A double-click on the empty area (config: [`AreaAction::ResetZoom`]).
    ResetZoom,
}

/// The result of [`Graph::show`].
pub struct GraphOutput {
    /// The area's response.
    pub response: Response,
    /// Slots whose value changed this pass, with the new value (already written to `values`).
    pub changed: Vec<(usize, f64)>,
    /// The selected handle.
    pub selected: Option<usize>,
    /// The slot the selected handle selects as its parameter: the one of the axis moved last, else
    /// its X, Y or wheel binding.
    pub selected_slot: Option<usize>,
    /// Requests the host acts on.
    pub events: Vec<GraphEvent>,
}

/// Which binding of a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Which {
    /// ↔.
    X,
    /// ↕.
    Y,
    /// The mouse wheel.
    Wheel,
}

impl Which {
    const ALL: [Which; 3] = [Which::X, Which::Y, Which::Wheel];

    fn of(self, h: &Handle) -> Option<&Bind> {
        match self {
            Which::X => h.x.as_ref(),
            Which::Y => h.y.as_ref(),
            Which::Wheel => h.wheel.as_ref(),
        }
    }
}

/// How far a handle moves: shares of the axis (`fx`, `fy`: right and up are positive) and of a
/// slot's range for knob-like bindings (`kx`, `ky`, `kw`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Delta {
    /// Along the x axis.
    pub fx: f64,
    /// Along the y axis.
    pub fy: f64,
    /// Knob-like X binding.
    pub kx: f64,
    /// Knob-like Y binding.
    pub ky: f64,
    /// Wheel binding.
    pub kw: f64,
}

impl Delta {
    /// A drag by `acc` px (screen coordinates, accumulated at the speed of the modifiers) on a
    /// plot whose axes are `span` px long.
    pub fn drag(acc: Vec2, span: Vec2) -> Self {
        let share = |px: f32, len: f32| if len > 0.0 { f64::from(px / len) } else { 0.0 };
        Self {
            fx: share(acc.x, span.x),
            fy: share(-acc.y, span.y),
            kx: f64::from(acc.x) / drag::KNOB_FULL_RANGE_PX,
            ky: f64::from(-acc.y) / drag::KNOB_FULL_RANGE_PX,
            kw: 0.0,
        }
    }

    /// An arrow key step: `dx`, `dy` ∈ {−1, 0, 1} at `speed`.
    pub fn key(dx: f64, dy: f64, speed: f64) -> Self {
        let s = KEY_STEP * speed;
        Self {
            fx: dx * s,
            fy: dy * s,
            kx: dx * s,
            ky: dy * s,
            kw: 0.0,
        }
    }

    /// `steps` wheel notches.
    pub fn wheel(steps: f64) -> Self {
        Self {
            kw: steps * WHEEL_STEP,
            ..Self::default()
        }
    }

    fn of(&self, which: Which, mode: Mode) -> f64 {
        match (which, mode) {
            (Which::X, Mode::Axis) => self.fx,
            (Which::Y, Mode::Axis) => self.fy,
            (Which::X, Mode::Knob { .. }) => self.kx,
            (Which::Y, Mode::Knob { .. }) => self.ky,
            (Which::Wheel, _) => self.kw,
        }
    }
}

fn axis_of(spec: &GraphSpec, which: Which) -> Option<&Axis> {
    match which {
        Which::X => Some(&spec.x),
        Which::Y => Some(&spec.y),
        Which::Wheel => None,
    }
}

/// The X axis value of handle `j`, if it is visible and has an X binding.
fn neighbour_x(spec: &GraphSpec, j: usize, values: &[f64]) -> Option<f64> {
    let h = spec.handles.get(j)?;
    if h.visible.as_ref().is_some_and(|p| !p.holds(values)) {
        return None;
    }
    h.x.as_ref().map(|b| b.axis_value(values))
}

/// The new value of the slot behind binding `which` of handle `i` after moving by `delta` from the
/// `start` values; `current` are the values now (the base slots of a chained binding and the
/// neighbours of an order constraint count as they are). `None` if the handle has no such binding,
/// the binding does not apply or the slot cannot be edited.
pub fn moved(
    spec: &GraphSpec,
    infos: &[SlotInfo<'_>],
    start: &[f64],
    current: &[f64],
    i: usize,
    which: Which,
    delta: Delta,
) -> Option<f64> {
    let h = spec.handles.get(i)?;
    let bind = which.of(h)?;
    let info = infos.get(bind.slot)?;
    if !info.editable || !bind.active(current) {
        return None;
    }
    let d = delta.of(which, bind.mode);
    let (knob, log) = match bind.mode {
        Mode::Knob { log } => (true, log),
        Mode::Axis => (false, false),
    };
    let value = match axis_of(spec, which).filter(|_| !knob) {
        Some(axis) => {
            let f0 = axis.fraction(bind.axis_value(start)).clamp(0.0, 1.0);
            let mut v = axis.clamp(axis.value((f0 + d).clamp(0.0, 1.0)));
            if which == Which::X {
                let [before, after] = h.between;
                let lo = before.and_then(|j| neighbour_x(spec, j, current));
                let hi = after.and_then(|j| neighbour_x(spec, j, current));
                if let Some(lo) = lo {
                    v = v.max(lo);
                }
                if let Some(hi) = hi {
                    v = v.min(hi);
                }
            }
            bind.slot_for(v, current)
        }
        None => {
            let n0 = info.normalized(start[bind.slot], log);
            info.denormalized(n0 + d, log)
        }
    };
    Some(value.clamp(info.min.min(info.max), info.max.max(info.min)))
}

/// The place of the plot inside the widget: where the axes start and end in px.
#[derive(Clone, Copy, Debug)]
struct Plot {
    rect: Rect,
    /// x at the axis minimum and maximum.
    xs: (f32, f32),
    /// y at the axis minimum and maximum.
    ys: (f32, f32),
}

impl Plot {
    fn new(spec: &GraphSpec, rect: Rect) -> Self {
        Self {
            rect,
            xs: (rect.left() + spec.x.pad[0], rect.right() - spec.x.pad[1]),
            ys: (rect.bottom() - spec.y.pad[0], rect.top() + spec.y.pad[1]),
        }
    }

    fn x(&self, axis: &Axis, v: f64) -> f32 {
        let f = axis.fraction(v).clamp(-2.0, 3.0) as f32;
        self.xs.0 + f * (self.xs.1 - self.xs.0)
    }

    fn y(&self, axis: &Axis, v: f64) -> f32 {
        let f = axis.fraction(v).clamp(-2.0, 3.0) as f32;
        self.ys.0 + f * (self.ys.1 - self.ys.0)
    }

    fn at(&self, spec: &GraphSpec, x: f64, y: f64) -> Pos2 {
        pos2(self.x(&spec.x, x), self.y(&spec.y, y))
    }

    /// The axis values at a point of the widget.
    fn value_at(&self, spec: &GraphSpec, p: Pos2) -> (f64, f64) {
        let fx = f64::from((p.x - self.xs.0) / (self.xs.1 - self.xs.0));
        let fy = f64::from((p.y - self.ys.0) / (self.ys.1 - self.ys.0));
        (
            spec.x.clamp(spec.x.value(fx.clamp(0.0, 1.0))),
            spec.y.clamp(spec.y.value(fy.clamp(0.0, 1.0))),
        )
    }

    fn span(&self) -> Vec2 {
        vec2((self.xs.1 - self.xs.0).abs(), (self.ys.1 - self.ys.0).abs())
    }
}

/// Where handle `h` is drawn for `values`.
fn handle_value(spec: &GraphSpec, h: &Handle, values: &[f64]) -> (f64, f64) {
    if let Some(at) = &h.at {
        return at.at(values);
    }
    (
        h.x.as_ref().map_or(spec.x.min, |b| b.axis_value(values)),
        h.y.as_ref().map_or(spec.y.min, |b| b.axis_value(values)),
    )
}

fn is_visible(h: &Handle, values: &[f64]) -> bool {
    h.visible.as_ref().is_none_or(|p| p.holds(values))
}

/// The accessible name of binding `which` of `h`: „Band 2 Frequenz“.
pub fn accessible_name(h: &Handle, which: Which) -> String {
    match which.of(h) {
        Some(b) => format!("{} {}", h.name, b.label),
        None => h.name.clone(),
    }
}

/// `color` at `opacity`.
fn with_opacity(c: Color32, opacity: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (opacity * 255.0).round() as u8)
}

/// The graph widget. `values` are the slot values of the host, `infos` describe them (one per
/// slot of the spec).
pub struct Graph<'a> {
    spec: &'a GraphSpec,
    values: &'a mut [f64],
    infos: &'a [SlotInfo<'a>],
    size: Vec2,
    track: Color32,
    look: Look,
    data: &'a [f32],
    playhead: Option<f64>,
    level: Option<f64>,
    selected: Option<Option<usize>>,
    salt: Option<Id>,
}

impl<'a> Graph<'a> {
    /// A graph of `size` px for `spec`.
    pub fn new(
        spec: &'a GraphSpec,
        values: &'a mut [f64],
        infos: &'a [SlotInfo<'a>],
        size: Vec2,
    ) -> Self {
        Self {
            spec,
            values,
            infos,
            size,
            track: color32(color::TEXT_1),
            look: Look::Normal,
            data: &[],
            playhead: None,
            level: None,
            selected: None,
            salt: None,
        }
    }

    /// The track colour (`track.base`) of the curve, the fill, a sample and the XY puck.
    #[must_use]
    pub fn track(mut self, track: Color32) -> Self {
        self.track = track;
        self
    }

    /// Normal, bypassed or disabled.
    #[must_use]
    pub fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// The live data of the `Data` layer: 0–1 values spread evenly over the x axis.
    #[must_use]
    pub fn data(mut self, data: &'a [f32]) -> Self {
        self.data = data;
        self
    }

    /// The playhead's x in axis units.
    #[must_use]
    pub fn playhead(mut self, x: Option<f64>) -> Self {
        self.playhead = x;
        self
    }

    /// The input level of the level point in axis units.
    #[must_use]
    pub fn level(mut self, x: Option<f64>) -> Self {
        self.level = x;
        self
    }

    /// Shows handle `selected` as selected, whatever the user clicked (stills and tests).
    #[must_use]
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = Some(selected);
        self
    }

    /// The id the graph keeps its selection and drag under (default: from its place in the `ui`);
    /// give one that does not change when siblings come and go, e.g. `ui.scope_id().with(…)`.
    #[must_use]
    pub fn id(mut self, id: Id) -> Self {
        self.salt = Some(id);
        self
    }

    /// Draws the graph and handles input.
    pub fn show(self, ui: &mut Ui) -> GraphOutput {
        let Graph {
            spec,
            values,
            infos,
            size,
            track,
            look,
            data,
            playhead,
            level,
            selected: forced_selection,
            salt,
        } = self;
        debug_assert_eq!(values.len(), spec.slots.len());
        debug_assert_eq!(infos.len(), spec.slots.len());
        let disabled = look == Look::Disabled;
        let live = !disabled && !crate::widgets::is_still(ui);
        let sense_area = if live { Sense::click() } else { Sense::hover() };
        let (rect, area) = ui.allocate_exact_size(size, sense_area);
        let id = salt.unwrap_or(area.id);
        area.widget_info(|| WidgetInfo::labeled(Role::Group, !disabled, &spec.name));
        let plot = Plot::new(spec, rect);
        let ctx = ui.ctx().clone();
        let mods = ui.input(|i| i.modifiers);

        let mut changed: Vec<(usize, f64)> = Vec::new();
        let mut events = Vec::new();
        let sel_key = id.with("komponist_graph_selected");
        let last_key = id.with("komponist_graph_last");
        let mut selected: Option<usize> =
            ctx.data(|d| d.get_temp::<Option<usize>>(sel_key)).flatten();
        let mut last: Option<(usize, Which)> = ctx
            .data(|d| d.get_temp::<Option<(usize, Which)>>(last_key))
            .flatten();

        // --- Input, handle by handle (before drawing, so the picture shows the new values).
        let mut tips: Vec<(Rect, String)> = Vec::new();
        let mut focused: Option<usize> = None;
        let mut hits: Vec<(usize, Rect, Id)> = Vec::new();
        let mut hot: Vec<(usize, bool, bool)> = Vec::new();
        for (i, h) in spec.handles.iter().enumerate() {
            if !is_visible(h, values) {
                continue;
            }
            let (vx, vy) = handle_value(spec, h, values);
            let pos = plot.at(spec, vx, vy);
            let hit = hit_rect(h, pos, rect);
            let hid = id.with(("handle", i));
            // The accessible parameters of a handle: the first binding is the handle's own widget
            // (focus, drag); the others sit below it, so the handle stays on top for the pointer.
            let present: Vec<Which> = Which::ALL
                .into_iter()
                .filter(|w| w.of(h).is_some())
                .collect();
            let primary = present.first().copied();
            for &w in present.iter().skip(1) {
                let eid = hid.with(w as u8);
                let r = ui.interact(hit, eid, Sense::hover());
                expose(&r, infos, values, h, w, !disabled);
                accesskit_changes(ui, eid, spec, infos, values, i, w, &mut changed);
            }
            let sense = if live {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            };
            let r = ui.interact(hit, hid, sense);
            match primary {
                Some(w) => {
                    expose(&r, infos, values, h, w, !disabled);
                    accesskit_changes(ui, hid, spec, infos, values, i, w, &mut changed);
                }
                None => r.widget_info(|| WidgetInfo::labeled(Role::Button, !disabled, &h.name)),
            }
            hits.push((i, hit, hid));
            if !live {
                continue;
            }
            hot.push((i, r.hovered() || r.dragged(), r.dragged()));
            if r.has_focus() {
                focused = Some(i);
            }
            if r.clicked() || r.drag_started() {
                r.request_focus();
                selected = Some(i);
            }
            let start_values: Vec<f64> = values.to_vec();
            if r.clicked() && mods.alt {
                for w in [Which::X, Which::Y] {
                    if let Some(b) = w.of(h) {
                        let info = &infos[b.slot];
                        if info.editable && b.active(values) && values[b.slot] != info.default {
                            values[b.slot] = info.default;
                            changed.push((b.slot, info.default));
                        }
                    }
                }
            }
            // Drag.
            let dkey = hid.with("komponist_graph_drag");
            if r.drag_started() {
                drag::grab_pointer(&ctx);
                let moved_px =
                    ctx.input(
                        |i| match (i.pointer.press_origin(), i.pointer.latest_pos()) {
                            (Some(a), Some(b)) => b - a,
                            _ => Vec2::ZERO,
                        },
                    );
                ctx.data_mut(|d| {
                    d.insert_temp(
                        dkey,
                        DragState {
                            acc: Vec2::ZERO,
                            start: start_values.clone(),
                        },
                    )
                });
                drag_step(&ctx, dkey, moved_px, spec, mods);
            } else if r.dragged() {
                drag_step(&ctx, dkey, drag::pointer_delta(&ctx), spec, mods);
            } else if r.drag_stopped() {
                drag::release_pointer(&ctx);
                ctx.data_mut(|d| d.remove::<DragState>(dkey));
            }
            if r.dragged()
                && let Some(state) = ctx.data(|d| d.get_temp::<DragState>(dkey))
            {
                let delta = Delta::drag(effective_acc(state.acc, spec, mods), plot.span());
                let mut now = values.to_vec();
                for w in [Which::X, Which::Y] {
                    if let Some(v) = moved(spec, infos, &state.start, &now, i, w, delta) {
                        let slot = w.of(h).map_or(0, |b| b.slot);
                        if v != now[slot] {
                            now[slot] = v;
                            changed.push((slot, v));
                            last = Some((i, w));
                        }
                    }
                }
                values.copy_from_slice(&now);
                tips.push((
                    Rect::from_center_size(pos, Vec2::splat(2.0 * widget::graph::handle::R)),
                    tip_text(h, infos, values),
                ));
            }
            // Wheel.
            if h.wheel.is_some() {
                let steps = wheel_steps(ui, &r);
                if steps != 0.0 {
                    let cur = values.to_vec();
                    if let Some(v) = moved(
                        spec,
                        infos,
                        &cur,
                        &cur,
                        i,
                        Which::Wheel,
                        Delta::wheel(steps),
                    ) {
                        let slot = h.wheel.as_ref().map_or(0, |b| b.slot);
                        if v != values[slot] {
                            values[slot] = v;
                            changed.push((slot, v));
                            last = Some((i, Which::Wheel));
                            selected = Some(i);
                            tips.push((
                                Rect::from_center_size(
                                    pos,
                                    Vec2::splat(2.0 * widget::graph::handle::R),
                                ),
                                tip_text(h, infos, values),
                            ));
                        }
                    }
                }
            }
            // Keys.
            if r.has_focus() {
                ctx.memory_mut(|mem| {
                    mem.set_focus_lock_filter(
                        hid,
                        EventFilter {
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            ..Default::default()
                        },
                    );
                });
                let (key_delta, delete) = key_input(ui, spec);
                if let Some(delta) = key_delta {
                    let cur = values.to_vec();
                    let mut now = cur.clone();
                    for w in [Which::X, Which::Y] {
                        if let Some(v) = moved(spec, infos, &cur, &now, i, w, delta_for(w, delta)) {
                            let slot = w.of(h).map_or(0, |b| b.slot);
                            if v != now[slot] {
                                now[slot] = v;
                                changed.push((slot, v));
                                last = Some((i, w));
                            }
                        }
                    }
                    values.copy_from_slice(&now);
                }
                if delete {
                    events.push(GraphEvent::Delete(i));
                }
            }
            if r.hovered() || r.dragged() {
                let icon = match h.look {
                    HandleLook::Edge { .. } => CursorIcon::ResizeHorizontal,
                    _ => CursorIcon::Grab,
                };
                if !r.dragged() {
                    ui.set_cursor_icon(icon);
                }
            }
        }
        if live
            && area.double_clicked()
            && !hits.iter().any(|(_, hit, _)| {
                ctx.input(|i| i.pointer.latest_pos().is_some_and(|p| hit.contains(p)))
            })
        {
            match spec.on_double_click {
                AreaAction::NewPoint => {
                    if let Some(p) = area.interact_pointer_pos() {
                        let (x, y) = plot.value_at(spec, p);
                        events.push(GraphEvent::AddPoint { x, y });
                    }
                }
                AreaAction::ResetZoom => events.push(GraphEvent::ResetZoom),
                AreaAction::None => {}
            }
        }
        if let Some(forced) = forced_selection {
            selected = forced;
        }
        ctx.data_mut(|d| {
            d.insert_temp(sel_key, selected);
            d.insert_temp(last_key, last);
        });

        // --- Drawing.
        let painter = ui.painter().with_clip_rect(rect);
        let ppp = ui.pixels_per_point();
        px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_1));
        let curve_col = if look == Look::Normal {
            track
        } else {
            color32(color::TEXT_4)
        };
        paint_grid(&painter, spec, &plot);
        let mut layers: Vec<&Layer> = spec.layers.iter().collect();
        layers.sort_by_key(|l| l.order());
        for layer in layers {
            match layer {
                Layer::Data(kind) if look == Look::Normal => {
                    paint_data(&painter, &plot, spec, *kind, data, track, values)
                }
                Layer::Data(_) => {}
                Layer::Region(r) => paint_region(&painter, &plot, spec, *r, values, curve_col, ppp),
                Layer::Curve(c) => paint_curve(&painter, &plot, spec, c, values, curve_col),
                Layer::Guide(g) => paint_guide(&painter, &plot, spec, *g, values, curve_col, ppp),
                Layer::Marker(mk) => {
                    paint_marker(&painter, &plot, spec, *mk, values, playhead, level, ppp)
                }
            }
        }
        let st_focus = |i: usize| focused == Some(i) && focus::is_visible(&ctx);
        for (i, h) in spec.handles.iter().enumerate() {
            if !is_visible(h, values) {
                continue;
            }
            let (vx, vy) = handle_value(spec, h, values);
            let pos = plot.at(spec, vx, vy);
            let (hovered, pressed) = hot
                .iter()
                .find(|(j, _, _)| *j == i)
                .map_or((false, false), |(_, h, p)| (*h, *p));
            let st = State {
                hovered,
                pressed,
                ..State::REST
            };
            let is_sel = selected == Some(i) && !disabled;
            paint_handle(
                &painter,
                &plot,
                h,
                pos,
                is_sel,
                st,
                st_focus(i),
                look,
                track,
            );
        }
        paint_labels(&painter, spec, &plot, ppp);
        paint_readout(ui, &painter, &plot, spec, values, infos, rect);
        paint_title(ui, &painter, spec, rect);
        for (anchor, text) in tips {
            tooltip::show_value(
                &ctx,
                id.with("tip"),
                anchor,
                tooltip::Placement::Above,
                &text,
            );
        }

        let selected_slot = selected.and_then(|i| {
            let h = spec.handles.get(i)?;
            let last_of = last.filter(|(j, _)| *j == i).and_then(|(_, w)| w.of(h));
            last_of
                .or(h.x.as_ref())
                .or(h.y.as_ref())
                .or(h.wheel.as_ref())
                .map(|b| b.slot)
        });
        // Merge repeated entries of one slot: the last value counts.
        let mut merged: Vec<(usize, f64)> = Vec::new();
        for (slot, v) in changed {
            if let Some(e) = merged.iter_mut().find(|(s, _)| *s == slot) {
                e.1 = v;
            } else {
                merged.push((slot, v));
            }
        }
        GraphOutput {
            response: area,
            changed: merged,
            selected,
            selected_slot,
            events,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Input helpers

#[derive(Clone, Debug)]
struct DragState {
    /// Pointer movement since the drag started in px, at the speed of the modifiers.
    acc: Vec2,
    /// The values when it started.
    start: Vec<f64>,
}

fn effective_acc(acc: Vec2, spec: &GraphSpec, mods: egui::Modifiers) -> Vec2 {
    if spec.shift_locks_axis && mods.shift {
        if acc.x.abs() >= acc.y.abs() {
            vec2(acc.x, 0.0)
        } else {
            vec2(0.0, acc.y)
        }
    } else {
        acc
    }
}

fn drag_step(ctx: &egui::Context, key: Id, delta: Vec2, spec: &GraphSpec, mods: egui::Modifiers) {
    let mut m = mods;
    if spec.shift_locks_axis {
        // Shift locks an axis here instead of making the drag fine.
        m.shift = false;
    }
    ctx.data_mut(|d| {
        if let Some(mut state) = d.get_temp::<DragState>(key) {
            state.acc += delta * speed(m) as f32;
            d.insert_temp(key, state);
        }
    });
}

fn delta_for(which: Which, d: Delta) -> Delta {
    // Keys move the axis of `which` only.
    match which {
        Which::X => Delta {
            fy: 0.0,
            ky: 0.0,
            ..d
        },
        Which::Y => Delta {
            fx: 0.0,
            kx: 0.0,
            ..d
        },
        Which::Wheel => Delta::default(),
    }
}

/// The arrow keys pressed this pass as a [`Delta`] (one key = ±1 %, Shift fine, Ctrl coarse), and
/// whether Entf was pressed on a deletable graph.
fn key_input(ui: &Ui, spec: &GraphSpec) -> (Option<Delta>, bool) {
    ui.input(|i| {
        let (mut dx, mut dy, mut sp) = (0.0, 0.0, 1.0);
        let mut delete = false;
        for e in &i.events {
            if let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = e
            {
                let s = speed(*modifiers);
                match key {
                    Key::ArrowLeft => {
                        dx -= 1.0;
                        sp = s;
                    }
                    Key::ArrowRight => {
                        dx += 1.0;
                        sp = s;
                    }
                    Key::ArrowUp => {
                        dy += 1.0;
                        sp = s;
                    }
                    Key::ArrowDown => {
                        dy -= 1.0;
                        sp = s;
                    }
                    Key::Delete if spec.deletable => delete = true,
                    _ => {}
                }
            }
        }
        (
            (dx != 0.0 || dy != 0.0).then(|| Delta::key(dx, dy, sp)),
            delete,
        )
    })
}

fn hit_rect(h: &Handle, pos: Pos2, rect: Rect) -> Rect {
    match h.look {
        HandleLook::Dot => Rect::from_center_size(
            pos,
            Vec2::splat(2.0 * (widget::graph::handle::R + m::HIT_PAD)),
        ),
        HandleLook::Puck => {
            Rect::from_center_size(pos, Vec2::splat(2.0 * (m::PUCK_R[0] + m::HIT_PAD)))
        }
        HandleLook::Edge { .. } => Rect::from_min_max(
            pos2(pos.x - m::EDGE_HIT, rect.top() + m::EDGE_TOP),
            pos2(pos.x + m::EDGE_HIT, rect.bottom()),
        ),
    }
}

/// The tooltip text of a handle: the values of its X and Y parameters.
fn tip_text(h: &Handle, infos: &[SlotInfo<'_>], values: &[f64]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for w in [Which::X, Which::Y, Which::Wheel] {
        if let Some(b) = w.of(h)
            && b.active(values)
        {
            let text = (infos[b.slot].format)(values[b.slot]);
            if !parts.contains(&text) {
                parts.push(text);
            }
        }
    }
    parts.join(" \u{b7} ")
}

/// Tells AccessKit about binding `which` of `h` behind `r`.
fn expose(
    r: &Response,
    infos: &[SlotInfo<'_>],
    values: &[f64],
    h: &Handle,
    which: Which,
    enabled: bool,
) {
    let Some(b) = which.of(h) else { return };
    let info = &infos[b.slot];
    let name = accessible_name(h, which);
    let v = values[b.slot];
    let text = (info.format)(v);
    let enabled = enabled && info.editable && b.active(values);
    let step = (info.max - info.min) * KEY_STEP;
    expose_value(r, Role::Slider, &name, v, text, info.min..=info.max, step);
    if !enabled {
        r.ctx.accesskit_node_builder(r.id, |n| n.set_disabled());
    }
}

/// AccessKit increment, decrement and set-value requests for binding `which` of handle `i`.
#[allow(clippy::too_many_arguments)]
fn accesskit_changes(
    ui: &Ui,
    id: Id,
    spec: &GraphSpec,
    infos: &[SlotInfo<'_>],
    values: &mut [f64],
    i: usize,
    which: Which,
    changed: &mut Vec<(usize, f64)>,
) {
    let (steps, set) = accesskit_requests(ui, id);
    let Some(b) = spec.handles.get(i).and_then(|h| which.of(h)) else {
        return;
    };
    let cur = values.to_vec();
    let mut new = None;
    if let Some(v) = set {
        let info = &infos[b.slot];
        if info.editable && b.active(&cur) {
            new = Some(v.clamp(info.min.min(info.max), info.max.max(info.min)));
        }
    } else if steps != 0 {
        let d = f64::from(steps) * KEY_STEP;
        let delta = match which {
            Which::X => Delta {
                fx: d,
                kx: d,
                ..Delta::default()
            },
            Which::Y => Delta {
                fy: d,
                ky: d,
                ..Delta::default()
            },
            Which::Wheel => Delta {
                kw: d,
                ..Delta::default()
            },
        };
        new = moved(spec, infos, &cur, &cur, i, which, delta);
    }
    if let Some(v) = new {
        // Order constraints apply to set-value as well.
        let v = if which == Which::X {
            constrain_slot(spec, i, b, v, &cur)
        } else {
            v
        };
        if v != values[b.slot] {
            values[b.slot] = v;
            changed.push((b.slot, v));
        }
    }
}

/// `slot` value `v` of the X binding `b` of handle `i`, held between the neighbours.
fn constrain_slot(spec: &GraphSpec, i: usize, b: &Bind, v: f64, values: &[f64]) -> f64 {
    let Some(h) = spec.handles.get(i) else {
        return v;
    };
    let mut trial = values.to_vec();
    trial[b.slot] = v;
    let mut ax = b.axis_value(&trial);
    if let Some(lo) = h.between[0].and_then(|j| neighbour_x(spec, j, values)) {
        ax = ax.max(lo);
    }
    if let Some(hi) = h.between[1].and_then(|j| neighbour_x(spec, j, values)) {
        ax = ax.min(hi);
    }
    b.slot_for(ax, values)
}

// ---------------------------------------------------------------------------------------------
// Painting

fn text_galley(
    painter: &Painter,
    text: &str,
    style: &TypeStyle,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    painter.layout_job(fonts::layout_job(text, style, color))
}

fn paint_grid(painter: &Painter, spec: &GraphSpec, plot: &Plot) {
    let rect = plot.rect;
    let line = |major: bool| color32(if major { color::LINE_2 } else { color::LINE_1 });
    for t in spec.x.ticks() {
        let x = plot.x(&spec.x, t.value);
        px::vline(painter, x, rect.top()..=rect.bottom(), line(t.major));
    }
    for t in spec.y.ticks() {
        let y = plot.y(&spec.y, t.value);
        px::hline(painter, rect.left()..=rect.right(), y, line(t.major));
    }
}

/// The axis labels, on top of everything but the title (`W_eq` draws them last).
fn paint_labels(painter: &Painter, spec: &GraphSpec, plot: &Plot, ppp: f32) {
    let rect = plot.rect;
    if spec.x.labels {
        for t in spec.x.ticks() {
            if let Some(label) = &t.label {
                let g = text_galley(painter, label, &typography::MONO_XS, color32(color::TEXT_2));
                let x = plot.x(&spec.x, t.value) + m::LABEL_DX;
                let top = rect.bottom() - m::LABEL_BASELINE - g.size().y * 0.8;
                painter.galley(
                    pos2(px::snap(x, ppp), px::snap(top, ppp)),
                    g,
                    color32(color::TEXT_2),
                );
            }
        }
    }
    if spec.y.labels {
        for t in spec.y.ticks() {
            if let Some(label) = &t.label {
                let g = text_galley(painter, label, &typography::MONO_XS, color32(color::TEXT_2));
                let y = plot.y(&spec.y, t.value);
                painter.galley(
                    pos2(rect.left() + m::LABEL_DX, px::snap(y - g.size().y, ppp)),
                    g,
                    color32(color::TEXT_2),
                );
            }
        }
    }
}

/// A mesh filling the strip between the polylines `top` and `bottom` (same length).
fn strip(top: &[Pos2], bottom: &[Pos2], color: Color32) -> Shape {
    let mut mesh = Mesh::default();
    for (t, b) in top.iter().zip(bottom) {
        mesh.vertices.push(Vertex {
            pos: *t,
            uv: WHITE_UV,
            color,
        });
        mesh.vertices.push(Vertex {
            pos: *b,
            uv: WHITE_UV,
            color,
        });
    }
    for i in 0..top.len().saturating_sub(1) as u32 {
        let k = 2 * i;
        mesh.add_triangle(k, k + 1, k + 2);
        mesh.add_triangle(k + 1, k + 3, k + 2);
    }
    Shape::mesh(mesh)
}

fn paint_data(
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    kind: DataKind,
    data: &[f32],
    track: Color32,
    _values: &[f64],
) {
    if data.len() < 2 {
        return;
    }
    let n = data.len() - 1;
    let x_at = |i: usize| plot.xs.0 + (plot.xs.1 - plot.xs.0) * i as f32 / n as f32;
    match kind {
        DataKind::Spectrum => {
            let top: Vec<Pos2> = data
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    pos2(
                        x_at(i),
                        plot.rect.bottom() - v.clamp(0.0, 1.0) * plot.rect.height(),
                    )
                })
                .collect();
            let bottom: Vec<Pos2> = (0..data.len())
                .map(|i| pos2(x_at(i), plot.rect.bottom()))
                .collect();
            painter.add(strip(&top, &bottom, color32(color::SURFACE_3)));
        }
        DataKind::Sample => {
            let mid = plot.y(&spec.y, 0.0);
            let unit = (plot.y(&spec.y, 1.0) - mid).abs();
            let top: Vec<Pos2> = data
                .iter()
                .enumerate()
                .map(|(i, v)| pos2(x_at(i), mid - v.clamp(0.0, 1.0) * unit))
                .collect();
            let bottom: Vec<Pos2> = data
                .iter()
                .enumerate()
                .map(|(i, v)| pos2(x_at(i), mid + v.clamp(0.0, 1.0) * unit))
                .collect();
            painter.add(strip(&top, &bottom, track));
        }
    }
}

fn paint_region(
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    region: Region,
    values: &[f64],
    track: Color32,
    ppp: f32,
) {
    let (start, end, scale) = match region {
        Region::Outside { start, end, scale } => (start, end, scale),
        Region::CutoffMod { cutoff, matrix } => {
            paint_cutoff_mod(painter, plot, spec, values, (cutoff, matrix), track, ppp);
            return;
        }
    };
    let shade = with_opacity(color32(color::SURFACE_1), m::REGION_OPACITY);
    let sx = plot.x(&spec.x, values[start] * scale);
    let ex = plot.x(&spec.x, values[end] * scale);
    let r = plot.rect;
    let left = Rect::from_min_max(r.min, pos2(sx.max(r.left()), r.bottom()));
    let right = Rect::from_min_max(pos2(ex.min(r.right()), r.top()), r.max);
    for part in [left, right] {
        if part.width() > 0.0 {
            painter.rect_filled(px::snap_rect(part, ppp), 0.0, shade);
        }
    }
}

/// The bar of [`Region::CutoffMod`].
fn paint_cutoff_mod(
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    values: &[f64],
    (cutoff, matrix): (usize, usize),
    track: Color32,
    ppp: f32,
) {
    let Some(block) = values.get(matrix..) else {
        return;
    };
    let (lo, hi) = va_curve::cutoff_mod_octaves(block);
    if hi - lo < 1e-3 {
        return;
    }
    let hz = values[cutoff];
    let edge = |octaves: f64| plot.x(&spec.x, (hz * octaves.exp2()).clamp(spec.x.min, spec.x.max));
    let r = plot.rect;
    let bar = m::MOD_BAR;
    let (left, right) = (edge(lo), edge(hi));
    let bottom = r.bottom() - bar.above_bottom;
    let rect = Rect::from_min_max(
        pos2(left, bottom - bar.height),
        pos2(right.max(left + 2.0), bottom),
    );
    painter.rect_filled(
        px::snap_rect(rect, ppp),
        bar.radius,
        with_opacity(track, bar.opacity),
    );
}

fn paint_curve(
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    curve: &Curve,
    values: &[f64],
    col: Color32,
) {
    let width = (plot.xs.1 - plot.xs.0).abs().max(1.0);
    let n = width.ceil() as usize;
    let mut line: Vec<Pos2> = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let x = spec.x.value(i as f64 / n as f64);
        let y = curve.f.at(x, values);
        let y = if y.is_finite() { y } else { spec.y.min };
        line.push(pos2(
            plot.xs.0 + (plot.xs.1 - plot.xs.0) * i as f32 / n as f32,
            plot.y(&spec.y, y),
        ));
    }
    if curve.fill {
        let base = curve.baseline.map_or(plot.ys.0, |b| plot.y(&spec.y, b));
        let bottom: Vec<Pos2> = line.iter().map(|p| pos2(p.x, base)).collect();
        painter.add(strip(
            &line,
            &bottom,
            with_opacity(col, widget::graph::curve::FILL_OPACITY),
        ));
    }
    painter.add(Shape::line(
        line,
        Stroke::new(widget::graph::curve::STROKE, col),
    ));
}

fn dashed(painter: &Painter, a: Pos2, b: Pos2, dash: [f32; 2], color: Color32, ppp: f32) {
    let w = px::line_width(ppp);
    painter.extend(Shape::dashed_line(
        &[a, b],
        Stroke::new(w, color),
        dash[0],
        dash[1],
    ));
}

fn paint_guide(
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    guide: Guide,
    values: &[f64],
    track: Color32,
    ppp: f32,
) {
    let r = plot.rect;
    match guide {
        Guide::Diagonal => {
            let a = plot.at(spec, spec.x.min, spec.y.min);
            let b = plot.at(spec, spec.x.max, spec.y.max);
            dashed(painter, a, b, m::DIAGONAL_DASH, color32(color::LINE_2), ppp);
        }
        Guide::VLine { slot } => {
            let x = px::snap(plot.x(&spec.x, values[slot]), ppp);
            dashed(
                painter,
                pos2(x + 0.5 / ppp, r.top()),
                pos2(x + 0.5 / ppp, r.bottom()),
                m::THRESHOLD_DASH,
                color32(color::TEXT_2),
                ppp,
            );
        }
        Guide::Crosshair { x, y } => {
            let p = plot.at(spec, values[x], values[y]);
            let col = with_opacity(track, m::CROSSHAIR_OPACITY);
            px::vline(painter, p.x, r.top()..=r.bottom(), col);
            px::hline(painter, r.left()..=r.right(), p.y, col);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_marker(
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    marker: Marker,
    values: &[f64],
    playhead: Option<f64>,
    level: Option<f64>,
    ppp: f32,
) {
    let r = plot.rect;
    match marker {
        Marker::Playhead => {
            if let Some(p) = playhead {
                let x = px::snap(plot.x(&spec.x, p), ppp) + 0.5 / ppp;
                dashed(
                    painter,
                    pos2(x, r.top() + m::PLAYHEAD_TOP),
                    pos2(x, r.bottom()),
                    m::PLAYHEAD_DASH,
                    color32(color::TEXT_1),
                    ppp,
                );
            }
        }
        Marker::Level => {
            if let (Some(x), Some(curve)) = (level, spec.first_curve()) {
                let y = curve.f.at(x, values);
                painter.circle_filled(plot.at(spec, x, y), m::POINT_R, color32(color::TEXT_1));
            }
        }
        // Drawn above the handles.
        Marker::Readout { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_handle(
    painter: &Painter,
    plot: &Plot,
    h: &Handle,
    pos: Pos2,
    selected: bool,
    st: State,
    focus_ring: bool,
    look: Look,
    track: Color32,
) {
    let dim = look != Look::Normal;
    let line_col = if dim {
        color32(color::TEXT_4)
    } else {
        color32(color::TEXT_1)
    };
    let accent = color32(color::ACCENT);
    let ppp = painter.pixels_per_point();
    match h.look {
        HandleLook::Dot => {
            let r = widget::graph::handle::R;
            let fill = state::surface_for(0, st);
            let stroke = if selected {
                Stroke::new(m::HANDLE_STROKE_SELECTED, accent)
            } else {
                Stroke::new(m::HANDLE_STROKE, line_col)
            };
            painter.circle(pos, r, fill, stroke);
            if let Some(n) = h.number {
                let g = text_galley(painter, &n.to_string(), &typography::MONO_XS, line_col);
                let at = pos2(
                    pos.x - g.size().x / 2.0,
                    pos.y + m::NUMBER_BASELINE - g.size().y * 0.8,
                );
                painter.galley(at, g, line_col);
            }
            if focus_ring {
                focus::paint_ring(
                    painter,
                    Rect::from_center_size(pos, Vec2::splat(2.0 * r)),
                    r,
                );
            }
        }
        HandleLook::Puck => {
            let fill = if dim { color32(color::TEXT_4) } else { track };
            painter.circle_filled(pos, m::PUCK_R[0], fill);
            if selected {
                painter.circle_stroke(pos, m::PUCK_R[1], Stroke::new(px::line_width(ppp), accent));
            }
            if focus_ring {
                focus::paint_ring(
                    painter,
                    Rect::from_center_size(pos, Vec2::splat(2.0 * m::PUCK_R[1])),
                    m::PUCK_R[1],
                );
            }
        }
        HandleLook::Edge { flag_left } => {
            let col = if selected { accent } else { line_col };
            let r = plot.rect;
            let top = r.top() + m::EDGE_TOP;
            px::vline(painter, pos.x, top..=r.bottom(), col);
            let [fw, fh] = m::EDGE_FLAG;
            let x0 = if flag_left { pos.x - fw + 1.0 } else { pos.x };
            painter.rect_filled(
                px::snap_rect(Rect::from_min_size(pos2(x0, top), vec2(fw, fh)), ppp),
                0.0,
                col,
            );
            if focus_ring {
                focus::paint_ring(
                    painter,
                    Rect::from_min_max(pos2(pos.x - fw, top), pos2(pos.x + fw, r.bottom())),
                    0.0,
                );
            }
        }
    }
}

/// The corner readouts of the XY field.
fn paint_readout(
    ui: &Ui,
    painter: &Painter,
    plot: &Plot,
    spec: &GraphSpec,
    values: &[f64],
    infos: &[SlotInfo<'_>],
    rect: Rect,
) {
    let _ = (ui, plot);
    for layer in &spec.layers {
        let Layer::Marker(Marker::Readout { x, y }) = layer else {
            continue;
        };
        let name_of = |slot: usize| {
            spec.handles
                .iter()
                .flat_map(|h| h.binds())
                .find(|b| b.slot == slot)
                .map_or("", |b| b.label.as_str())
        };
        let [dx, dy] = m::READOUT_INSET;
        for (slot, right_bottom) in [(*y, false), (*x, true)] {
            let name = text_galley(
                painter,
                name_of(slot),
                &typography::DEVICE_LABEL,
                color32(color::TEXT_2),
            );
            let value = text_galley(
                painter,
                &(infos[slot].format)(values[slot]),
                &typography::MONO_XXS,
                color32(color::TEXT_1),
            );
            let w = name.size().x + m::READOUT_GAP + value.size().x;
            let h = name.size().y.max(value.size().y);
            let origin = if right_bottom {
                pos2(rect.right() - dx - w, rect.bottom() - dy - h)
            } else {
                pos2(rect.left() + dx, rect.top() + dy)
            };
            let name_w = name.size().x;
            painter.galley(origin, name, color32(color::TEXT_2));
            painter.galley(
                pos2(origin.x + name_w + m::READOUT_GAP, origin.y),
                value,
                color32(color::TEXT_1),
            );
        }
    }
}

fn paint_title(ui: &Ui, painter: &Painter, spec: &GraphSpec, rect: Rect) {
    if spec.title.is_empty() {
        return;
    }
    let ppp = ui.pixels_per_point();
    let g = text_galley(
        painter,
        &spec.title,
        &typography::GRAPH_TITLE,
        color32(color::TEXT_2),
    );
    let [dx, dy] = m::TITLE_POS;
    painter.galley(
        pos2(
            px::snap(rect.left() + dx, ppp),
            px::snap(rect.top() + dy, ppp),
        ),
        g,
        color32(color::TEXT_2),
    );
}

#[cfg(test)]
mod tests {
    use super::configs::{self, band, env};
    use super::*;

    fn infos(spec: &GraphSpec, ranges: &[(f64, f64, f64)]) -> Vec<SlotInfo<'static>> {
        assert_eq!(spec.slots.len(), ranges.len());
        ranges
            .iter()
            .map(|&(lo, hi, d)| SlotInfo::new(lo, hi, d))
            .collect()
    }

    fn plot(spec: &GraphSpec) -> Plot {
        Plot::new(spec, Rect::from_min_size(pos2(0.0, 0.0), vec2(236.0, 98.0)))
    }

    #[test]
    fn plot_maps_axes_with_padding() {
        let spec = configs::envelope();
        let p = plot(&spec);
        // x 8 … 228, y: 0 at 90, 1 at 18 (W_env: pad 8, top 18, bot h − 8).
        assert_eq!(p.x(&spec.x, 0.0), 8.0);
        assert_eq!(p.x(&spec.x, 1.0), 228.0);
        assert_eq!(p.y(&spec.y, 0.0), 90.0);
        assert_eq!(p.y(&spec.y, 1.0), 18.0);
        let (x, y) = p.value_at(&spec, pos2(118.0, 54.0));
        assert!((x - 0.5).abs() < 1e-6 && (y - 0.5).abs() < 1e-6);
    }

    #[test]
    fn eq_geometry_is_the_generators() {
        let spec = configs::eq(5, 18.0);
        let p = plot(&spec);
        // W_eq: gy(g) = h/2 − g/rng · (h/2 − 10).
        let gy = |g: f64| 98.0 / 2.0 - g / 18.0 * (98.0 / 2.0 - 10.0);
        for g in [-12.0, 0.0, 6.0, 18.0] {
            assert!((f64::from(p.y(&spec.y, g)) - gy(g)).abs() < 1e-4, "{g}");
        }
        let fx = |f: f64| (f.log10() - 20f64.log10()) / (20000f64.log10() - 20f64.log10()) * 236.0;
        for f in [20.0, 100.0, 1000.0, 20000.0] {
            assert!((f64::from(p.x(&spec.x, f)) - fx(f)).abs() < 1e-3, "{f}");
        }
    }

    #[test]
    fn waveform_and_lfo_geometry_are_the_generators() {
        let w = configs::waveform(1.0);
        let p = plot(&w);
        // W_wave: mid = h/2 + 4, amplitude h/2 − 12.
        assert_eq!(p.y(&w.y, 0.0), 98.0 / 2.0 + 4.0);
        assert_eq!(p.y(&w.y, 1.0), 98.0 / 2.0 + 4.0 - (98.0 / 2.0 - 12.0));
        let l = configs::lfo(2);
        let p = plot(&l);
        // W_lfo: X = 8 + t(w − 16), Y = h/2 + 4 − v(h/2 − 16).
        assert_eq!(p.x(&l.x, 0.5), 8.0 + 0.5 * (236.0 - 16.0));
        assert_eq!(p.y(&l.y, 0.5), 98.0 / 2.0 + 4.0 - 0.5 * (98.0 / 2.0 - 16.0));
    }

    #[test]
    fn dragging_an_eq_handle_changes_frequency_and_gain() {
        let spec = configs::eq(1, 18.0);
        let ranges = [
            (0.0, 1.0, 1.0),
            (20.0, 20000.0, 1000.0),
            (-18.0, 18.0, 0.0),
            (0.1, 18.0, 1.0),
            (0.0, 4.0, 2.0),
        ];
        let infos = infos(&spec, &ranges);
        let start = vec![1.0, 1000.0, 0.0, 1.0, band::BELL];
        let p = plot(&spec);
        // Move 20 px right and 10 px up.
        let d = Delta::drag(vec2(20.0, -10.0), p.span());
        let f = moved(&spec, &infos, &start, &start, 0, Which::X, d).unwrap();
        let g = moved(&spec, &infos, &start, &start, 0, Which::Y, d).unwrap();
        let x0 = p.x(&spec.x, 1000.0);
        assert!((p.x(&spec.x, f) - (x0 + 20.0)).abs() < 0.01, "{f}");
        let y0 = p.y(&spec.y, 0.0);
        assert!((p.y(&spec.y, g) - (y0 - 10.0)).abs() < 0.01, "{g}");
        // The wheel is a knob on Q (log).
        let q = moved(
            &spec,
            &infos,
            &start,
            &start,
            0,
            Which::Wheel,
            Delta::wheel(1.0),
        )
        .unwrap();
        assert!(q > 1.0 && q < 1.5, "{q}");
    }

    #[test]
    fn limits_hold_at_the_ends() {
        let spec = configs::eq(1, 18.0);
        let ranges = [
            (0.0, 1.0, 1.0),
            (20.0, 20000.0, 1000.0),
            (-18.0, 18.0, 0.0),
            (0.1, 18.0, 1.0),
            (0.0, 4.0, 2.0),
        ];
        let infos = infos(&spec, &ranges);
        let start = vec![1.0, 1000.0, 0.0, 1.0, band::BELL];
        let d = Delta::drag(vec2(5000.0, -5000.0), plot(&spec).span());
        assert_eq!(
            moved(&spec, &infos, &start, &start, 0, Which::X, d),
            Some(20000.0)
        );
        assert_eq!(
            moved(&spec, &infos, &start, &start, 0, Which::Y, d),
            Some(18.0)
        );
        let d = Delta::drag(vec2(-5000.0, 5000.0), plot(&spec).span());
        assert_eq!(
            moved(&spec, &infos, &start, &start, 0, Which::X, d),
            Some(20.0)
        );
    }

    #[test]
    fn a_cut_band_has_no_gain_and_a_locked_slot_does_not_move() {
        let spec = configs::eq(1, 18.0);
        let ranges = [
            (0.0, 1.0, 1.0),
            (20.0, 20000.0, 1000.0),
            (-18.0, 18.0, 0.0),
            (0.1, 18.0, 1.0),
            (0.0, 4.0, 2.0),
        ];
        let mut infos = infos(&spec, &ranges);
        let cut = vec![1.0, 100.0, 0.0, 1.0, band::LOW_CUT];
        let d = Delta::key(1.0, 1.0, 1.0);
        assert_eq!(moved(&spec, &infos, &cut, &cut, 0, Which::Y, d), None);
        assert!(moved(&spec, &infos, &cut, &cut, 0, Which::X, d).is_some());
        infos[1].editable = false;
        assert_eq!(moved(&spec, &infos, &cut, &cut, 0, Which::X, d), None);
    }

    #[test]
    fn envelope_points_do_not_overtake() {
        let spec = configs::envelope();
        let ranges = [(0.0, 1.0, 0.2); 5];
        let infos = infos(&spec, &ranges);
        let start = vec![0.3, 0.2, 0.5, 0.3, 0.5];
        let span = plot(&spec).span();
        // Dragging the decay point far left stops at the attack point (decay = 0).
        let d = Delta::drag(vec2(-5000.0, 0.0), span);
        assert_eq!(
            moved(&spec, &infos, &start, &start, 1, Which::X, d),
            Some(0.0)
        );
        // Dragging the attack point far right stops at the right end of the plot or of its segment.
        let d = Delta::drag(vec2(5000.0, 0.0), span);
        let a = moved(&spec, &infos, &start, &start, 0, Which::X, d).unwrap();
        assert!(a <= 1.0 && a > 0.3);
        // The decay point also sets the sustain level; the sustain point only that.
        let up = Delta::drag(vec2(0.0, -10.0), span);
        let s = moved(&spec, &infos, &start, &start, 1, Which::Y, up).unwrap();
        assert!(s > 0.5);
        assert!(moved(&spec, &infos, &start, &start, 2, Which::X, up).is_none());
        assert!(moved(&spec, &infos, &start, &start, 2, Which::Y, up).is_some());
    }

    #[test]
    fn lfo_points_stay_between_their_neighbours() {
        let spec = configs::lfo(3);
        let ranges = [
            (0.0, 1.0, 0.0),
            (-1.0, 1.0, 0.0),
            (0.0, 1.0, 0.0),
            (-1.0, 1.0, 0.0),
            (0.0, 1.0, 0.0),
            (-1.0, 1.0, 0.0),
        ];
        let infos = infos(&spec, &ranges);
        let start = vec![0.2, 0.5, 0.5, -0.5, 0.8, 0.0];
        let span = plot(&spec).span();
        let right = Delta::drag(vec2(5000.0, 0.0), span);
        assert_eq!(
            moved(&spec, &infos, &start, &start, 1, Which::X, right),
            Some(0.8)
        );
        let left = Delta::drag(vec2(-5000.0, 0.0), span);
        assert_eq!(
            moved(&spec, &infos, &start, &start, 1, Which::X, left),
            Some(0.2)
        );
        assert_eq!(
            moved(&spec, &infos, &start, &start, 0, Which::X, left),
            Some(0.0)
        );
    }

    #[test]
    fn waveform_start_and_end_keep_their_order() {
        let spec = configs::waveform(1.0);
        let ranges = [(0.0, 1000.0, 0.0), (0.0, 1000.0, 900.0)];
        let infos = infos(&spec, &ranges);
        let start = vec![100.0, 900.0];
        let span = plot(&spec).span();
        let right = Delta::drag(vec2(5000.0, 0.0), span);
        assert_eq!(
            moved(&spec, &infos, &start, &start, 0, Which::X, right),
            Some(900.0)
        );
        let left = Delta::drag(vec2(-5000.0, 0.0), span);
        assert_eq!(
            moved(&spec, &infos, &start, &start, 1, Which::X, left),
            Some(100.0)
        );
    }

    #[test]
    fn transfer_handle_edits_threshold_and_ratio() {
        let spec = configs::transfer();
        let ranges = [(-60.0, 0.0, -18.0), (1.0, 20.0, 4.0), (0.0, 1.0, 1.0)];
        let infos = infos(&spec, &ranges);
        let start = vec![-18.0, 4.0, 1.0];
        let span = plot(&spec).span();
        let d = Delta::drag(vec2(23.6, -20.0), span);
        let thr = moved(&spec, &infos, &start, &start, 0, Which::X, d).unwrap();
        assert!((thr - (-18.0 + 6.0)).abs() < 1e-6, "{thr}");
        let ratio = moved(&spec, &infos, &start, &start, 0, Which::Y, d).unwrap();
        assert!(ratio > 4.0 && ratio < 20.0, "{ratio}");
    }

    #[test]
    fn accessible_names_are_german_and_name_the_parameter() {
        let spec = configs::eq(2, 18.0);
        assert_eq!(
            accessible_name(&spec.handles[1], Which::X),
            "Band 2 Frequenz"
        );
        assert_eq!(accessible_name(&spec.handles[1], Which::Y), "Band 2 Gain");
        assert_eq!(accessible_name(&spec.handles[1], Which::Wheel), "Band 2 Q");
        let _ = env::ATTACK;
    }
}
