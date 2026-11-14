//! The cells of a device body (WP-83, WP-84): every [`WidgetKind`] is drawn by its widget from
//! `komponist_ui::widgets::device`, bound to the parameter the cell names (a graph to the
//! parameters of its slots, [`GraphBinding`]). Values
//! travel as the parameter model gives them: knobs, mini faders and continuous number fields in
//! normalised 0–1 space (so log and dB scales turn evenly), integer, stepped and enum parameters
//! as plain values. Every change goes out as `PluginAction::SetParam`.
//!
//! A widget that edits no parameter (the pad grid, the gain-reduction meter) keeps its small state
//! (selected pad, held reduction) in egui memory until the device runtime (WP-85, WP-87) feeds it.

use egui::{Id, Rect, Role, Sense, Ui, WidgetInfo};
use komponist_builtin::algorithms::ALGORITHMS;
use komponist_model::DeviceId;
use komponist_model::params::{ParamContext, ParamKind, Scale, resolve};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, radius};
use komponist_ui::widgets::device::choice::{DropdownCell, Segment, SegmentOption, ToggleButton};
use komponist_ui::widgets::device::fm_algorithm::{FmAlgorithm, Topology};
use komponist_ui::widgets::device::graph::configs::{self, band};
use komponist_ui::widgets::device::graph::{
    Axis, AxisKind, Graph, GraphEvent, GraphSpec, Look, SlotInfo,
};
use komponist_ui::widgets::device::knob_cell::KnobCell;
use komponist_ui::widgets::device::meter_gr::{self, GrHold, MeterGr};
use komponist_ui::widgets::device::mini_fader::{Fill, MiniFader};
use komponist_ui::widgets::device::number::Number;
use komponist_ui::widgets::device::pad_grid::{Pad, PadGrid};
use komponist_ui::widgets::dropdown::Item;
use komponist_ui::widgets::number_field::parse_number;
use komponist_ui::{fonts, px};

use super::layout::{BAND, Cell, CellParam, ENV_OP, GraphBinding, GraphConfig, SEL, WidgetKind};
use super::metrics;
use super::model::{Card, Param, names};
use crate::actions::{Action, PluginAction};

/// The memory slot of the context placeholders (`{band}`, `{env_op}`, `{sel}`) of a device.
fn context_id(device: DeviceId) -> Id {
    Id::NULL.with(("komponist_device_context", device.0))
}

/// The placeholders a device's layout resolves with until a view sets them: operator 1, band 1,
/// pad 1.
pub(super) fn default_context() -> ParamContext {
    ParamContext::new()
        .with(ENV_OP, 1)
        .with(BAND, 1)
        .with(SEL, 1)
}

/// The context of `device`'s cells: the defaults with what the views set ([`set_context`]).
pub(super) fn context(ctx: &egui::Context, device: DeviceId) -> ParamContext {
    ctx.data(|d| d.get_temp::<ParamContext>(context_id(device)))
        .unwrap_or_else(default_context)
}

/// Sets placeholder `key` of `device`'s context (the selected EQ band, the operator whose envelope
/// shows) to `value`. Returns whether it changed.
pub(super) fn set_context(
    cu: &egui::Context,
    device: DeviceId,
    key: &str,
    value: impl std::fmt::Display,
) -> bool {
    let mut ctx = context(cu, device);
    let before = ctx.get(key).map(str::to_owned);
    ctx.set(key, &value);
    let changed = before.as_deref() != ctx.get(key);
    if changed {
        cu.data_mut(|d| d.insert_temp(context_id(device), ctx));
    }
    changed
}

/// One cell: the widget of its kind bound to its parameter; the placeholder for a widget the
/// parameter's kind does not fit.
pub(super) fn draw_cell(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    palette: &TrackPalette,
    cell: &Cell,
    rect: Rect,
) {
    let ctx = context(cu.ctx(), card.device);
    let param = match &cell.param {
        CellParam::Id(id) => card.param(id),
        CellParam::Template(t) => resolve(t, &ctx).ok().and_then(|id| card.param(&id)),
        _ => None,
    };
    let drawn = match (cell.widget, param) {
        (WidgetKind::Knob | WidgetKind::KnobLarge, Some(p)) => {
            knob(cu, out, card, cell, p, rect);
            true
        }
        (WidgetKind::Number, Some(p)) => number(cu, out, card, cell, p, rect),
        (WidgetKind::Segment, Some(p)) => segment(cu, out, card, cell, p, rect),
        (WidgetKind::ToggleButton, Some(p)) => toggle(cu, out, card, cell, p, rect),
        (WidgetKind::Dropdown, Some(p)) => dropdown(cu, out, card, cell, p, rect),
        (WidgetKind::MiniFader, Some(p)) => {
            mini_fader(cu, out, card, cell, p, palette, rect);
            true
        }
        (WidgetKind::MeterGr, _) => {
            meter(cu, cell, rect);
            true
        }
        (WidgetKind::PadGrid, _) => {
            pads(cu, cell, palette, rect);
            true
        }
        (WidgetKind::FmAlgorithm, p) => {
            fm(cu, out, card, cell, p, palette, rect);
            true
        }
        (WidgetKind::Graph, _) => graph(cu, out, card, palette, cell, &ctx, rect),
        _ => false,
    };
    if !drawn {
        placeholder(cu, cell, param, rect);
    }
}

fn set(out: &mut Vec<Action>, card: &Card, p: &Param, plain: f64) {
    let plain = p.snap(plain);
    if plain != p.value {
        out.push(Action::Plugin(PluginAction::SetParam {
            device: card.device,
            id: p.wire,
            value: plain,
        }));
    }
}

/// Memory slot of a cell's own state.
fn slot(cu: &Ui, cell: &Cell, what: &str) -> Id {
    cu.scope_id().with((
        "komponist_cell",
        cell.col,
        cell.row,
        cell.half.is_some(),
        what,
    ))
}

/// `text` split into the number and its trailing unit: „420 Hz“ → („420“, „Hz“), „L 14“ stays whole.
fn split_unit(text: &str) -> (&str, &str) {
    match text.rsplit_once(' ') {
        Some((number, unit)) if unit.starts_with(|c: char| !c.is_ascii_digit()) => (number, unit),
        _ => (text, ""),
    }
}

/// A knob, driven in normalised space (so log and dB parameters turn evenly).
fn knob(cu: &mut Ui, out: &mut Vec<Action>, card: &Card, cell: &Cell, p: &Param, rect: Rect) {
    let info = &p.info;
    let mut value = info.to_normalized(p.value);
    let mut w = KnobCell::new(&mut value, p.name().to_owned())
        .default_value(info.default_normalized())
        .bipolar(matches!(
            info.kind,
            ParamKind::Float {
                scale: Scale::Pan,
                ..
            }
        ))
        .format(|n| p.text_at(info.to_plain(n)));
    if cell.widget == WidgetKind::KnobLarge {
        w = w.large();
    }
    let r = cu.put(rect, w);
    if r.changed() {
        set(out, card, p, info.to_plain(value));
    }
}

/// A compact number field: plain values for integer and stepped parameters, normalised for
/// continuous ones.
fn number(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    rect: Rect,
) -> bool {
    let info = &p.info;
    let (min, max) = info.range();
    let steps = info.steps();
    let plain_step = match &info.kind {
        ParamKind::Int { .. } => Some(1.0),
        ParamKind::Float { .. } if steps > 0 => Some((max - min) / f64::from(steps)),
        ParamKind::Float { .. } => None,
        _ => return false,
    };
    let current = p.text_at(p.value);
    let unit = split_unit(&current).1.to_owned();
    let unit_len = if unit.is_empty() { 0 } else { unit.len() + 1 };
    let plain_text = |v: f64| {
        let t = p.text_at(v);
        let end = t.len() - unit_len.min(t.len());
        t[..end].to_owned()
    };
    match plain_step {
        Some(step) => {
            let mut value = p.value;
            let w = Number::new(&mut value, p.name().to_owned())
                .range(min..=max)
                .step(step)
                .default_value(info.default)
                .unit(unit)
                .cols(cell.cols)
                .format(plain_text);
            let r = cu.put(rect, w);
            if r.changed() {
                set(out, card, p, value);
            }
        }
        None => {
            let mut value = info.to_normalized(p.value);
            let w = Number::new(&mut value, p.name().to_owned())
                .default_value(info.default_normalized())
                .unit(unit.clone())
                .cols(cell.cols)
                .format(|n| plain_text(info.to_plain(n)))
                .parse(|t| parse_number(t, &unit).map(|v| info.to_normalized(v)));
            let r = cu.put(rect, w);
            if r.changed() {
                set(out, card, p, info.to_plain(value));
            }
        }
    }
    true
}

fn choices(p: &Param) -> Option<&Vec<String>> {
    match &p.info.kind {
        ParamKind::Enum(names) if !names.is_empty() => Some(names),
        _ => None,
    }
}

fn index_of(p: &Param, n: usize) -> usize {
    (p.value.round().max(0.0) as usize).min(n.saturating_sub(1))
}

fn segment(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    rect: Rect,
) -> bool {
    let Some(names) = choices(p) else {
        return false;
    };
    let options: Vec<SegmentOption<'_>> = names.iter().map(|n| SegmentOption::text(n)).collect();
    let mut selected = index_of(p, names.len());
    let r = cu.put(
        rect,
        Segment::new(p.name(), &options, &mut selected).cols(cell.cols),
    );
    if r.changed() {
        set(out, card, p, selected as f64);
    }
    true
}

fn toggle(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    rect: Rect,
) -> bool {
    if !matches!(p.info.kind, ParamKind::Bool) {
        return false;
    }
    let mut on = p.value >= 0.5;
    let r = cu.put(rect, ToggleButton::new(p.name(), &mut on).cols(cell.cols));
    if r.changed() {
        set(out, card, p, if on { 1.0 } else { 0.0 });
    }
    true
}

fn dropdown(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    rect: Rect,
) -> bool {
    let Some(names) = choices(p) else {
        return false;
    };
    let items: Vec<Item<'_>> = names.iter().map(|n| Item::new(n)).collect();
    let mut selected = index_of(p, names.len());
    let r = cu.put(
        rect,
        DropdownCell::new(p.name(), &items, &mut selected).cols(cell.cols),
    );
    if r.changed() {
        set(out, card, p, selected as f64);
    }
    true
}

fn mini_fader(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    palette: &TrackPalette,
    rect: Rect,
) {
    let info = &p.info;
    let mut value = info.to_normalized(p.value);
    let w = MiniFader::new(&mut value, p.name().to_owned())
        .default_value(info.default_normalized())
        .rows(cell.rows)
        .fill(Fill::Track(color32(palette.base)))
        .format(|n| p.text_at(info.to_plain(n)));
    let r = cu.put(rect, w);
    if r.changed() {
        set(out, card, p, info.to_plain(value));
    }
}

/// The gain-reduction meter: it shows no level until the device runtime delivers them (WP-85);
/// the click resets the held reduction.
fn meter(cu: &mut Ui, cell: &Cell, rect: Rect) {
    let id = slot(cu, cell, "gr_hold");
    let now = cu.input(|i| i.time);
    let mut hold: GrHold = cu.data(|d| d.get_temp(id)).unwrap_or_default();
    let text = meter_gr::readout(hold.max(now));
    let r = cu.put(rect, MeterGr::new([0.0; 3], &text).rows(cell.rows));
    if r.clicked() {
        hold.reset();
    }
    cu.data_mut(|d| d.insert_temp(id, hold));
}

/// The pad grid: one pad per grid cell, numbered; the selected pad is kept here until the sampler
/// (WP-87) owns the pads.
fn pads(cu: &mut Ui, cell: &Cell, palette: &TrackPalette, rect: Rect) {
    let id = slot(cu, cell, "pad");
    let mut selected: usize = cu.data(|d| d.get_temp(id)).unwrap_or(0);
    let count = usize::from(cell.cols) * usize::from(cell.rows);
    let labels: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
    let pads: Vec<Pad<'_>> = labels.iter().map(|l| Pad::new(l, "")).collect();
    let out = cu
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |cu| {
            PadGrid::new(&pads, cell.cols, palette)
                .selected(Some(selected))
                .show(cu)
        })
        .inner;
    if let Some(c) = out.clicked {
        selected = c;
    }
    cu.data_mut(|d| d.insert_temp(id, selected));
}

/// The FM algorithm display of pisynth's algorithm table; the parameter is the algorithm number
/// (1 …). Without a parameter it shows algorithm 1.
fn fm(
    cu: &mut Ui,
    _out: &mut Vec<Action>,
    _card: &Card,
    cell: &Cell,
    p: Option<&Param>,
    palette: &TrackPalette,
    rect: Rect,
) {
    let id = slot(cu, cell, "op");
    let mut selected: usize = cu.data(|d| d.get_temp(id)).unwrap_or(0);
    let number = p.map_or(1, |p| p.value.round().max(1.0) as usize);
    let algo = &ALGORITHMS[(number - 1).min(ALGORITHMS.len() - 1)];
    let topology = Topology {
        modulates: &algo.modulates,
        feedback: Some(algo.feedback),
    };
    let out = cu
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |cu| {
            FmAlgorithm::new(number, topology, palette)
                .cells(cell.cols, cell.rows)
                .selected(Some(selected))
                .show(cu)
        })
        .inner;
    if let Some(c) = out.clicked {
        selected = c;
    }
    cu.data_mut(|d| d.insert_temp(id, selected));
}

/// The value a slot without a parameter holds.
fn fallback(slot: &str) -> f64 {
    match slot {
        "curve" | "knee" => 0.5,
        "type" => 1.0,
        s if s.ends_with("_on") || s.ends_with("_q") => 1.0,
        s if s.ends_with("_type") => band::BELL,
        _ => 0.0,
    }
}

/// The axis of a parameter for the XY field: its range, logarithmic for a log scale.
fn axis_of(p: &Param) -> Axis {
    let (min, max) = p.info.range();
    let log = matches!(
        p.info.kind,
        ParamKind::Float {
            scale: Scale::Log,
            ..
        }
    );
    Axis::new(if log { AxisKind::Log } else { AxisKind::Lin }, min, max)
}

/// The parameter of `card` behind slot `slot` of `binding` under `ctx`.
fn slot_param<'c>(
    card: &'c Card,
    binding: &GraphBinding,
    ctx: &ParamContext,
    slot: &str,
) -> Option<&'c Param> {
    let id = binding.resolved(slot, ctx).ok()??;
    card.param(&id)
}

/// The configuration `binding` asks for; `None` if a parameter it needs is missing (the XY field
/// takes its axes from its two parameters).
fn spec_of<'c>(
    binding: &GraphBinding,
    title: &str,
    lookup: &dyn Fn(&str) -> Option<&'c Param>,
) -> Option<GraphSpec> {
    let spec = match binding.config {
        GraphConfig::Envelope => configs::envelope(),
        GraphConfig::Eq { bands } => {
            // ±12 or ±24 dB, from the range of the gain parameters.
            let range = lookup("b1_gain").map_or(18.0, |p| p.info.range().1.abs());
            configs::eq(usize::from(bands), range)
        }
        GraphConfig::Filter => configs::filter(),
        GraphConfig::Transfer => configs::transfer(),
        // Start and end are in milliseconds; the axis spans the range of the end.
        GraphConfig::Waveform => {
            configs::waveform(lookup("end").map_or(1.0, |p| p.info.range().1 / 1000.0))
        }
        GraphConfig::Xy => {
            let (x, y) = (lookup("x")?, lookup("y")?);
            configs::xy(axis_of(x), axis_of(y), x.name(), y.name())
        }
        GraphConfig::Lfo { points } => configs::lfo(usize::from(points)),
    };
    Some(if title.is_empty() {
        spec
    } else {
        spec.titled(title)
    })
}

/// A graph: the configuration of the cell's binding with the values of the parameters behind its
/// slots (the envelope's in normalised space, so segments stay proportional on log time
/// scales). Changes go out as `SetParam`. A selected handle sets the binding's context
/// placeholder (the EQ band). Double-click and Entf on an EQ add and remove a band
/// (`b{n}_on`); the LFO's point count is the layout's.
fn graph(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    palette: &TrackPalette,
    cell: &Cell,
    ctx: &ParamContext,
    rect: Rect,
) -> bool {
    let CellParam::Graph(binding) = &cell.param else {
        return false;
    };
    let lookup = |slot: &str| slot_param(card, binding, ctx, slot);
    let title = resolve(&binding.title, ctx).map_or(String::new(), |t| t.as_str().to_owned());
    let Some(spec) = spec_of(binding, &title, &lookup) else {
        return false;
    };
    let normalized = binding.config == GraphConfig::Envelope;
    let mut values = Vec::with_capacity(spec.slots.len());
    let mut infos = Vec::with_capacity(spec.slots.len());
    for name in &spec.slots {
        match lookup(name) {
            Some(p) => {
                values.push(if normalized {
                    p.info.to_normalized(p.value)
                } else {
                    p.value
                });
                infos.push(SlotInfo::from_param(&p.info, normalized));
            }
            None => {
                let v = fallback(name);
                values.push(v);
                infos.push(SlotInfo::new(0.0, 1.0, v).locked());
            }
        }
    }
    let look = if card.bypass {
        Look::Bypass
    } else {
        Look::Normal
    };
    let id = slot(cu, cell, "graph");
    let size = rect.size();
    let shown = cu
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |gu| {
            Graph::new(&spec, &mut values, &infos, size)
                .track(color32(palette.base))
                .look(look)
                .id(id)
                .show(gu)
        })
        .inner;
    for (i, v) in &shown.changed {
        if let Some(p) = lookup(&spec.slots[*i]) {
            set(
                out,
                card,
                p,
                if normalized { p.info.to_plain(*v) } else { *v },
            );
        }
    }
    if let (Some(key), Some(handle)) = (binding.select, shown.selected) {
        set_context(cu.ctx(), card.device, key, handle + 1);
    }
    if let GraphConfig::Eq { bands } = binding.config {
        for event in &shown.events {
            eq_event(out, card, &lookup, bands, *event);
        }
    }
    true
}

/// An EQ event: a double-click turns on the first band that is off at the pointer's frequency and
/// gain (as a bell); Entf turns the focused band off.
fn eq_event<'c>(
    out: &mut Vec<Action>,
    card: &Card,
    lookup: &dyn Fn(&str) -> Option<&'c Param>,
    bands: u8,
    event: GraphEvent,
) {
    let band_param = |n: usize, what: &str| -> Option<&'c Param> {
        let name = format!("b{n}_{what}");
        lookup(&name)
    };
    match event {
        GraphEvent::AddPoint { x, y } => {
            let free = (1..=usize::from(bands))
                .find(|&n| band_param(n, "on").is_some_and(|p| p.value < 0.5));
            let Some(n) = free else { return };
            for (what, v) in [("freq", x), ("gain", y), ("type", band::BELL), ("on", 1.0)] {
                if let Some(p) = band_param(n, what) {
                    set(out, card, p, v);
                }
            }
        }
        GraphEvent::Delete(i) => {
            if let Some(p) = band_param(i + 1, "on") {
                set(out, card, p, 0.0);
            }
        }
        GraphEvent::ResetZoom => {}
    }
}

/// The placeholder of a cell without a widget (a widget kind the parameter does not fit): a
/// `surface.1` box with the kind's name and the parameter's.
fn placeholder(cu: &mut Ui, cell: &Cell, param: Option<&Param>, rect: Rect) {
    let ppp = cu.pixels_per_point();
    px::fill(cu.painter(), rect, radius::M, color32(color::SURFACE_1));
    let kind = names::widget(cell.widget);
    let label = match param {
        Some(p) => format!("{kind} {}", p.name()),
        None => kind.to_owned(),
    };
    let style = typography::LABEL;
    let mut job = fonts::layout_job(&label, &style, color32(color::TEXT_3));
    job.wrap.max_width = rect.width() - 2.0 * metrics::CELL_GAP * 2.0;
    job.halign = egui::Align::Center;
    let g = cu.painter().layout_job(job);
    let pos = egui::pos2(
        px::snap(rect.center().x, ppp),
        px::snap(rect.center().y - g.size().y / 2.0, ppp),
    );
    let r = cu.allocate_rect(rect, Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
    cu.painter().galley(pos, g, color32(color::TEXT_3));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::devices::layout::{DeviceLayout, Half, Page};
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;
    use komponist_model::DeviceId;
    use komponist_model::params::{ParamId, ParamInfo};

    fn param(wire: u32, name: &str, kind: ParamKind, default: f64, value: f64) -> Param {
        let info = ParamInfo::new(ParamId::new(format!("p{wire}")), name, kind, default);
        let text = info.format(value);
        Param {
            wire,
            info,
            value,
            text,
        }
    }

    /// A card with one cell of every kind that edits a parameter.
    fn card() -> Card {
        let id = |w: u32| ParamId::new(format!("p{w}"));
        let cells = vec![
            Cell::half(0, 0, 1, Half::Top, WidgetKind::Number, CellParam::Id(id(1))),
            Cell::half(
                1,
                0,
                2,
                Half::Top,
                WidgetKind::Segment,
                CellParam::Id(id(2)),
            ),
            Cell::half(
                0,
                1,
                1,
                Half::Top,
                WidgetKind::ToggleButton,
                CellParam::Id(id(3)),
            ),
            Cell::half(
                1,
                1,
                2,
                Half::Top,
                WidgetKind::Dropdown,
                CellParam::Id(id(4)),
            ),
            Cell::span(3, 0, 1, 2, WidgetKind::MiniFader, CellParam::Id(id(5))),
            Cell::span(0, 2, 2, 1, WidgetKind::KnobLarge, CellParam::Id(id(5))),
        ];
        Card {
            device: DeviceId(7),
            name: "Test".into(),
            tag: "INTERN",
            instrument: false,
            midi_effect: false,
            bypass: false,
            state: super::super::model::CardState::Ok,
            warning: None,
            params: vec![
                param(1, "Stimmen", ParamKind::Int { min: 1, max: 16 }, 8.0, 8.0),
                param(
                    2,
                    "Typ",
                    ParamKind::Enum(vec!["LC".into(), "LS".into(), "Gl.".into()]),
                    0.0,
                    0.0,
                ),
                param(3, "Loop", ParamKind::Bool, 0.0, 0.0),
                param(
                    4,
                    "Kit",
                    ParamKind::Enum(vec!["808".into(), "909".into()]),
                    0.0,
                    0.0,
                ),
                param(5, "Level", ParamKind::lin(0.0, 1.0), 0.5, 0.5),
            ],
            has_gui: false,
            open: false,
            layout: DeviceLayout::single(Page {
                name: String::new(),
                cols: 4,
                cells,
                dividers: Vec::new(),
            }),
            page: 0,
            collapsed: false,
            presets: false,
            preset: None,
            width: 244.0,
            late: None,
        }
    }

    fn harness() -> egui_kittest::Harness<'static, (Card, Vec<Action>)> {
        let mut h = komponist_ui::snapshot::builder(1.0)
            .with_size(egui::vec2(260.0, 240.0))
            .build_ui_state(
                |ui, (card, out): &mut (Card, Vec<Action>)| {
                    let palette = TrackPalette::new(150);
                    let page = card.current_page().unwrap().clone();
                    let origin = ui.min_rect().min;
                    for cell in &page.cells {
                        let rect = crate::views::devices::layout::cell_rect(cell)
                            .translate(origin.to_vec2());
                        draw_cell(ui, out, card, &palette, cell, rect);
                    }
                },
                (card(), Vec::new()),
            );
        h.run();
        h
    }

    fn set_params(h: &egui_kittest::Harness<'_, (Card, Vec<Action>)>) -> Vec<(u32, f64)> {
        h.state()
            .1
            .iter()
            .filter_map(|a| match a {
                Action::Plugin(PluginAction::SetParam { id, value, .. }) => Some((*id, *value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_widget_kind_draws_and_has_a_name() {
        let h = harness();
        h.check_accessibility();
        h.get_by_role_and_label(Role::SpinButton, "Stimmen");
        h.get_by_role_and_label(Role::RadioButton, "Typ: Gl.");
        h.get_by_role_and_label(Role::Button, "Loop");
        h.get_by_role_and_label(Role::ComboBox, "Kit");
        // The mini fader and the large knob both edit "Level".
        assert_eq!(
            h.get_all_by_role_and_label(Role::Slider, "Level").count(),
            2
        );
    }

    #[test]
    fn a_click_sends_the_parameter_to_the_device() {
        let mut h = harness();
        h.get_by_role_and_label(Role::RadioButton, "Typ: Gl.")
            .click();
        h.run();
        assert_eq!(set_params(&h), vec![(2, 2.0)]);
        h.get_by_role_and_label(Role::Button, "Loop").click();
        h.run();
        assert_eq!(set_params(&h).last(), Some(&(3, 1.0)));
    }

    /// A card with an EQ graph of two bands (band 2 off) and an envelope graph.
    fn graph_card() -> Card {
        let mut card = card();
        let mut params = Vec::new();
        let mut eq = GraphBinding::new(GraphConfig::Eq { bands: 2 }).titled("");
        for n in 1..=2u32 {
            let base = 100 + n * 10;
            let on = if n == 1 { 1.0 } else { 0.0 };
            params.push(param(base, "an", ParamKind::Bool, 1.0, on));
            params.push(param(
                base + 1,
                "Frequenz",
                ParamKind::scaled(20.0, 20000.0, Scale::Log),
                1000.0,
                1000.0,
            ));
            params.push(param(
                base + 2,
                "Gain",
                ParamKind::lin(-24.0, 24.0),
                0.0,
                0.0,
            ));
            params.push(param(
                base + 3,
                "Q",
                ParamKind::scaled(0.1, 18.0, Scale::Log),
                1.0,
                1.0,
            ));
            params.push(param(
                base + 4,
                "Typ",
                ParamKind::Enum(vec![
                    "LC".into(),
                    "LS".into(),
                    "Gl.".into(),
                    "HS".into(),
                    "HC".into(),
                ]),
                2.0,
                2.0,
            ));
            for (what, w) in [("on", 0), ("freq", 1), ("gain", 2), ("q", 3), ("type", 4)] {
                eq = eq.bind(format!("b{n}_{what}"), format!("p{}", base + w));
            }
        }
        let mut env = GraphBinding::new(GraphConfig::Envelope).titled("Op {env_op} · Amplitude");
        for (i, (slot, name, max)) in [
            ("attack", "Attack", 10.0),
            ("decay", "Decay", 10.0),
            ("release", "Release", 20.0),
        ]
        .into_iter()
        .enumerate()
        {
            let wire = 200 + i as u32;
            params.push(param(
                wire,
                name,
                ParamKind::scaled(0.001, max, Scale::Log),
                0.1,
                0.1,
            ));
            env = env.bind(slot, format!("p{wire}"));
        }
        params.push(param(210, "Sustain", ParamKind::lin(0.0, 1.0), 0.5, 0.5));
        env = env.bind("sustain", "p210");
        card.params = params;
        card.layout = DeviceLayout::single(Page {
            name: String::new(),
            cols: 7,
            cells: vec![
                Cell::graph(0, 0, 4, 2, eq.selecting("band")),
                Cell::graph(0, 2, 4, 1, env),
            ],
            dividers: Vec::new(),
        });
        card
    }

    fn graph_harness(card: Card) -> egui_kittest::Harness<'static, (Card, Vec<Action>)> {
        graph_harness_sized(card, egui::vec2(460.0, 280.0))
    }

    fn graph_harness_sized(
        card: Card,
        size: egui::Vec2,
    ) -> egui_kittest::Harness<'static, (Card, Vec<Action>)> {
        let mut h = komponist_ui::snapshot::builder(1.0)
            .with_size(size)
            // Two clicks must fall within egui's double-click time.
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                |ui, (card, out): &mut (Card, Vec<Action>)| {
                    let palette = TrackPalette::new(150);
                    let page = card.current_page().unwrap().clone();
                    let origin = ui.min_rect().min;
                    // The device body's ground.
                    px::fill(
                        ui.painter(),
                        Rect::from_min_size(origin, ui.available_size()),
                        0.0,
                        color32(color::SURFACE_2),
                    );
                    for cell in &page.cells {
                        let rect = crate::views::devices::layout::cell_rect(cell)
                            .translate(origin.to_vec2());
                        draw_cell(ui, out, card, &palette, cell, rect);
                    }
                },
                (card, Vec::new()),
            );
        h.run();
        h
    }

    #[test]
    fn a_graph_cell_draws_its_configuration_and_names_every_handle() {
        let h = graph_harness(graph_card());
        h.check_accessibility();
        h.get_by_role_and_label(Role::Group, "Parametrischer EQ");
        h.get_by_role_and_label(Role::Slider, "Band 1 Frequenz");
        h.get_by_role_and_label(Role::Slider, "Band 1 Gain");
        // Band 2 is off: no handle. The envelope resolved `{env_op}` into its title.
        assert!(
            h.query_by_role_and_label(Role::Slider, "Band 2 Frequenz")
                .is_none()
        );
        h.get_by_role_and_label(Role::Group, "Op 1 · Amplitude");
        h.get_by_role_and_label(Role::Slider, "Decay Zeit");
    }

    #[test]
    fn a_graph_sends_plain_values_and_the_selected_band_sets_the_context() {
        let mut h = graph_harness(graph_card());
        h.get_by_role_and_label(Role::Slider, "Band 1 Frequenz")
            .increment_accesskit();
        h.run();
        let freq = h
            .state()
            .1
            .iter()
            .find_map(|a| match a {
                Action::Plugin(PluginAction::SetParam { id: 111, value, .. }) => Some(*value),
                _ => None,
            })
            .expect("frequency of band 1 set");
        assert!(freq > 1000.0 && freq <= 20000.0, "{freq}");
        // The envelope's attack is edited in normalised space and sent as seconds.
        h.state_mut().1.clear();
        h.get_by_role_and_label(Role::Slider, "Attack Zeit")
            .increment_accesskit();
        h.run();
        let attack = h
            .state()
            .1
            .iter()
            .find_map(|a| match a {
                Action::Plugin(PluginAction::SetParam { id: 200, value, .. }) => Some(*value),
                _ => None,
            })
            .expect("attack set");
        assert!(attack > 0.1 && attack < 10.0, "{attack}");
    }

    #[test]
    fn a_double_click_on_the_eq_turns_on_the_next_band() {
        let mut h = graph_harness(graph_card());
        let area = h
            .get_by_role_and_label(Role::Group, "Parametrischer EQ")
            .rect();
        let spot = area.center() + egui::vec2(0.0, 30.0);
        for _ in 0..30 {
            h.step();
        }
        h.event(egui::Event::PointerMoved(spot));
        for _ in 0..2 {
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: spot,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                h.step();
            }
        }
        h.run();
        let set: Vec<(u32, f64)> = h
            .state()
            .1
            .iter()
            .filter_map(|a| match a {
                Action::Plugin(PluginAction::SetParam { id, value, .. }) => Some((*id, *value)),
                _ => None,
            })
            .collect();
        // Band 2 (wires 120 … 124): on, at the clicked frequency and below 0 dB.
        assert!(set.contains(&(120, 1.0)), "{set:?}");
        let freq = set.iter().find(|(id, _)| *id == 121).expect("frequency").1;
        assert!(freq > 20.0 && freq < 20000.0);
        let gain = set.iter().find(|(id, _)| *id == 122).expect("gain").1;
        assert!(gain < 0.0, "{gain}");
    }

    #[test]
    fn a_template_cell_follows_the_context_a_view_sets() {
        let mut card = card();
        card.layout = DeviceLayout::single(Page {
            name: String::new(),
            cols: 4,
            cells: vec![Cell::span(
                0,
                0,
                1,
                1,
                WidgetKind::Knob,
                CellParam::Template("p{sel}".into()),
            )],
            dividers: Vec::new(),
        });
        let mut h = graph_harness(card);
        // The default selection is 1: parameter p1 („Stimmen“).
        h.get_by_role_and_label(Role::Slider, "Stimmen");
        h.state_mut().1.clear();
        assert!(set_context(&h.ctx, DeviceId(7), SEL, 5));
        h.run();
        h.get_by_role_and_label(Role::Slider, "Level");
    }

    /// A card whose body holds one graph of every configuration, each bound to parameters of the
    /// ranges `devices.json` gives them.
    fn all_graphs_card(bypass: bool) -> Card {
        let mut card = card();
        card.bypass = bypass;
        let mut params = Vec::new();
        let mut wire = 300u32;
        let mut add = |name: &str, kind: ParamKind, default: f64, value: f64| {
            wire += 1;
            params.push(param(wire, name, kind, default, value));
            format!("p{wire}")
        };
        let log = |min, max| ParamKind::scaled(min, max, Scale::Log);
        let filter = GraphBinding::new(GraphConfig::Filter)
            .titled("Filter · TP 24")
            .bind("cutoff", add("Cutoff", log(20.0, 20000.0), 900.0, 900.0))
            .bind(
                "resonance",
                add("Resonanz", ParamKind::lin(0.0, 1.0), 0.45, 0.45),
            )
            .bind(
                "type",
                add(
                    "Typ",
                    ParamKind::Enum(vec![
                        "TP 12".into(),
                        "TP 24".into(),
                        "BP".into(),
                        "HP".into(),
                    ]),
                    1.0,
                    1.0,
                ),
            );
        let transfer = GraphBinding::new(GraphConfig::Transfer)
            .bind(
                "threshold",
                add("Threshold", ParamKind::lin(-60.0, 0.0), -18.0, -18.0),
            )
            .bind("ratio", add("Ratio", log(1.0, 20.0), 4.0, 4.0))
            .bind(
                "knee",
                add(
                    "Knie",
                    ParamKind::Enum(vec!["hart".into(), "weich".into()]),
                    1.0,
                    1.0,
                ),
            );
        let waveform = GraphBinding::new(GraphConfig::Waveform)
            .titled("Kick · kick_dry_04.wav")
            .bind("start", add("Start", ParamKind::lin(0.0, 500.0), 0.0, 20.0))
            .bind("end", add("Ende", ParamKind::lin(0.0, 500.0), 450.0, 450.0));
        let xy = GraphBinding::new(GraphConfig::Xy)
            .bind("x", add("Zeit", log(1.0, 2000.0), 375.0, 375.0))
            .bind("y", add("Feedback", ParamKind::lin(0.0, 1.1), 0.42, 0.42));
        let lfo = GraphBinding::new(GraphConfig::Lfo { points: 2 })
            .titled("LFO 1 · Form")
            .bind("x1", add("X1", ParamKind::lin(0.0, 1.0), 0.0, 0.3))
            .bind("y1", add("Y1", ParamKind::lin(-1.0, 1.0), 0.0, 0.8))
            .bind("x2", add("X2", ParamKind::lin(0.0, 1.0), 0.0, 0.7))
            .bind("y2", add("Y2", ParamKind::lin(-1.0, 1.0), 0.0, -0.5));
        card.params = params;
        card.layout = DeviceLayout::single(Page {
            name: String::new(),
            cols: 12,
            cells: vec![
                Cell::graph(0, 0, 4, 1, filter),
                Cell::graph(4, 0, 2, 1, transfer),
                Cell::graph(6, 0, 4, 1, waveform),
                Cell::graph(0, 1, 2, 1, xy),
                Cell::graph(2, 1, 4, 1, lfo),
            ],
            dividers: Vec::new(),
        });
        card
    }

    #[test]
    fn every_graph_configuration_draws_from_its_parameters() {
        let h = graph_harness_sized(all_graphs_card(false), egui::vec2(780.0, 160.0));
        h.check_accessibility();
        for name in [
            "Filter · TP 24",
            "Kennlinie",
            "Kick · kick_dry_04.wav",
            "XY-Feld",
            "LFO 1 · Form",
        ] {
            h.get_by_role_and_label(Role::Group, name);
        }
        h.get_by_role_and_label(Role::Slider, "Filter Cutoff");
        h.get_by_role_and_label(Role::Slider, "Filter Resonanz");
        h.get_by_role_and_label(Role::Slider, "Knie Threshold");
        h.get_by_role_and_label(Role::Slider, "Knie Ratio");
        h.get_by_role_and_label(Role::Slider, "Sample-Start Position");
        h.get_by_role_and_label(Role::Slider, "Sample-Ende Position");
        h.get_by_role_and_label(Role::Slider, "XY-Feld Zeit");
        h.get_by_role_and_label(Role::Slider, "XY-Feld Feedback");
        h.get_by_role_and_label(Role::Slider, "Punkt 2 Phase");
        h.get_by_role_and_label(Role::Slider, "Punkt 2 Wert");
    }

    #[test]
    fn a_dragged_waveform_edge_sends_milliseconds() {
        let mut h = graph_harness_sized(all_graphs_card(false), egui::vec2(780.0, 160.0));
        h.get_by_role_and_label(Role::Slider, "Sample-Ende Position")
            .decrement_accesskit();
        h.run();
        let end = h
            .state()
            .1
            .iter()
            .find_map(|a| match a {
                Action::Plugin(PluginAction::SetParam { id: 308, value, .. }) => Some(*value),
                _ => None,
            })
            .expect("end set");
        assert!(end < 450.0 && end > 20.0, "{end}");
    }

    #[test]
    fn graph_cells_snapshot() {
        let mut results = egui_kittest::SnapshotResults::new();
        for (name, bypass) in [
            ("device-graph-cells-1x", false),
            ("device-graph-cells-bypass-1x", true),
        ] {
            let mut h = graph_harness_sized(all_graphs_card(bypass), egui::vec2(780.0, 170.0));
            h.snapshot_options(name, &egui_kittest::SnapshotOptions::new());
            results.extend_harness(&mut h);
        }
        results.unwrap();
    }
}
