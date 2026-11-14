//! The cells of a device body (WP-83): every [`WidgetKind`] except the graph (WP-84) is drawn by
//! its widget from `komponist_ui::widgets::device`, bound to the parameter the cell names. Values
//! travel as the parameter model gives them: knobs, mini faders and continuous number fields in
//! normalised 0–1 space (so log and dB scales turn evenly), integer, stepped and enum parameters
//! as plain values. Every change goes out as `PluginAction::SetParam`.
//!
//! A widget that edits no parameter (the pad grid, the gain-reduction meter) keeps its small state
//! (selected pad, held reduction) in egui memory until the device runtime (WP-85, WP-87) feeds it.

use egui::{Id, Rect, Role, Sense, Ui, WidgetInfo};
use komponist_builtin::algorithms::ALGORITHMS;
use komponist_model::params::{ParamKind, Scale};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, radius};
use komponist_ui::widgets::device::choice::{DropdownCell, Segment, SegmentOption, ToggleButton};
use komponist_ui::widgets::device::fm_algorithm::{FmAlgorithm, Topology};
use komponist_ui::widgets::device::knob_cell::KnobCell;
use komponist_ui::widgets::device::meter_gr::{self, GrHold, MeterGr};
use komponist_ui::widgets::device::mini_fader::{Fill, MiniFader};
use komponist_ui::widgets::device::number::Number;
use komponist_ui::widgets::device::pad_grid::{Pad, PadGrid};
use komponist_ui::widgets::dropdown::Item;
use komponist_ui::widgets::number_field::parse_number;
use komponist_ui::{fonts, px};

use super::layout::{Cell, CellParam, WidgetKind};
use super::metrics;
use super::model::{Card, Param, names};
use crate::actions::{Action, PluginAction};

/// One cell: the widget of its kind bound to its parameter; the placeholder for the graph and for a
/// widget the parameter's kind does not fit.
pub(super) fn draw_cell(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    palette: &TrackPalette,
    cell: &Cell,
    rect: Rect,
) {
    let param = match &cell.param {
        CellParam::Id(id) => card.param(id),
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

/// The placeholder of a cell without a widget (the graph, WP-84, or a widget kind the parameter
/// does not fit): a `surface.1` box with the kind's name and the parameter's.
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
                        let rect = cell.rect().translate(origin.to_vec2());
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
}
