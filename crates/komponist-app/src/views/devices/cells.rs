//! The cells of a device body (WP-83, WP-84): every [`WidgetKind`] is drawn by its widget from
//! `komponist_ui::widgets::device`, bound to the parameter the cell names (a graph to the
//! parameters of its slots, [`GraphBinding`]). Values
//! travel as the parameter model gives them: knobs, mini faders and continuous number fields in
//! normalised 0–1 space (so log and dB scales turn evenly), integer, stepped and enum parameters
//! as plain values. Every change goes out as `PluginAction::SetParam`, an undo step per drag (or
//! wheel burst) and per click, key step, text entry or reset ([`touch`]).
//!
//! A widget that edits no parameter (the pad grid, the gain-reduction meter) keeps its small state
//! (selected pad, held reduction) in egui memory until the device runtime (WP-85, WP-87) feeds it.

use egui::{Id, Rect, Role, Sense, Ui, WidgetInfo};
use komponist_builtin::algorithms::ALGORITHMS;
use komponist_builtin::internal::knurr;
use komponist_i18n::Lang;
use komponist_model::DeviceId;
use komponist_model::params::eq_curve::CURVE_RATE;
use komponist_model::params::{ParamContext, ParamKind, Scale, resolve};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, device, radius};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::device::KnobCellOptions;
use komponist_ui::widgets::device::choice::{
    DropdownCell, Glyph, Segment, SegmentOption, ToggleButton, Wave,
};
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
use komponist_ui::widgets::number_field::{open_value_entry, parse_number};
use komponist_ui::{fonts, px};

use super::layout::{
    BAND, Cell, CellParam, ENV_OP, ENV_VIEW, GraphBinding, GraphConfig, LFO_VIEW, SEL, WidgetKind,
};
use super::metrics;
use super::model::{Card, Param, names};
use crate::actions::{Action, EditAction, PluginAction, UiAction};
use crate::views::mixer::{Touch, gesture_actions, gesture_touch};
use komponist_builtin::sampler::PadStatus;
use komponist_model::format::note_name;

/// The memory slot of the context placeholders (`{band}`, `{env_op}`, `{sel}`) of a device.
fn context_id(device: DeviceId) -> Id {
    Id::NULL.with(("komponist_device_context", device.0))
}

/// The placeholders a device's layout resolves with until a view sets them: operator 1, band 1,
/// pad 1.
pub(super) fn default_context() -> ParamContext {
    let mut ctx = ParamContext::new()
        .with(ENV_OP, 1)
        .with(BAND, 1)
        .with(SEL, 1);
    // knurr's switches start on their first entry (Amp envelope, LFO 1).
    choose(&mut ctx, ENV_VIEW, knurr::ENVELOPES, 0);
    choose(&mut ctx, LFO_VIEW, knurr::LFOS, 0);
    ctx
}

/// Sets placeholder `key` to the value of entry `index` of `options` and `{key}_label` to its
/// label ([`CellParam::Choice`]).
fn choose(ctx: &mut ParamContext, key: &str, options: &[(&str, &str)], index: usize) {
    if let Some((value, label)) = options.get(index) {
        ctx.set(key, value);
        ctx.set(&format!("{key}_label"), label);
    }
}

/// The context of `device`'s cells: the defaults with what the views set ([`set_context`]).
pub(super) fn context(ctx: &egui::Context, device: DeviceId) -> ParamContext {
    ctx.data(|d| d.get_temp::<ParamContext>(context_id(device)))
        .unwrap_or_else(default_context)
}

/// The pad (0-based) the sampler `device` has selected (`{sel}`).
pub(crate) fn selected_pad(ctx: &egui::Context, device: DeviceId) -> usize {
    selected_entry(&context(ctx, device), SEL, 255).saturating_sub(1)
}

/// Selects pad `pad` (0-based) of the sampler `device`.
pub(crate) fn select_pad(ctx: &egui::Context, device: DeviceId, pad: usize) {
    set_context(ctx, device, SEL, pad + 1);
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
    if let CellParam::Select { key, count } = &cell.param {
        match cell.widget {
            WidgetKind::Segment => select_segment(cu, card, cell, key, *count, &ctx, rect),
            WidgetKind::ColumnSelect => column_select(cu, card, cell, key, *count, &ctx, rect),
            _ => placeholder(cu, cell, None, rect),
        }
        return;
    }
    if let CellParam::Choice {
        key,
        label,
        options,
    } = &cell.param
    {
        match cell.widget {
            WidgetKind::Segment => {
                choice_segment(cu, card, cell, (key, label), options, &ctx, rect)
            }
            _ => placeholder(cu, cell, None, rect),
        }
        return;
    }
    let param = match &cell.param {
        CellParam::Id(id) => card.param(id),
        CellParam::Template(t) => resolve(t, &ctx).ok().and_then(|id| card.param(&id)),
        _ => None,
    };
    // The rate knob of a synced LFO shows its divider instead.
    let param = param.map(|p| synced_divider(card, p));
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
            meter(cu, card, cell, rect);
            true
        }
        (WidgetKind::PadGrid, _) => {
            pads(cu, out, card, cell, palette, rect);
            true
        }
        (WidgetKind::FmAlgorithm, p) => {
            fm(cu, card, cell, p, palette, rect);
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

/// What the right-click menu of a parameter offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamMenu {
    /// Back to the default value.
    Default,
    /// The text entry.
    Enter,
}

/// A parameter's control was touched: the inspector and the status line follow (`TouchParam`).
/// `r` is the control's response; `entry` is the text its entry opens with (`None`: a choice, a
/// switch: no "Wert eingeben"); `double_click_resets` is true for knobs and faders (the menu's
/// shortcut column says "Doppelklick"), false for the others ("Alt-Klick"). `change` is the plain
/// value the control just gave, if it changed.
///
/// The change is one undo step: a drag or a burst of wheel notches is bracketed into a gesture
/// (`BeginParamGesture` … `EndGesture`, like the mixer's faders); a click, a key step, a text entry
/// and the menu's reset are a step each.
///
/// The right-click menu: "Auf Standard" (the reset) and, for a control with text entry, "Wert
/// eingeben". Nothing else yet (automation, MIDI learn and macros are prepared, not built).
#[allow(clippy::too_many_arguments)]
fn touch(
    cu: &Ui,
    out: &mut Vec<Action>,
    card: &Card,
    p: &Param,
    r: &egui::Response,
    entry: Option<String>,
    double_click_resets: bool,
    change: Option<f64>,
) {
    gesture_actions(
        r,
        change.is_some(),
        || begin_param(card, p.wire),
        out,
        |out| {
            if let Some(plain) = change {
                set(out, card, p, plain);
            }
        },
    );
    if r.is_pointer_button_down_on() || r.drag_stopped() || r.clicked() {
        out.push(touch_action(card, p, r.dragged()));
    }
    let mut enter = Entry::action(names::ENTER_VALUE, ParamMenu::Enter);
    if entry.is_none() {
        enter = enter.disabled();
    }
    let entries = [
        Entry::action(names::TO_DEFAULT, ParamMenu::Default).shortcut(if double_click_resets {
            names::TO_DEFAULT_SHORTCUT
        } else {
            names::TO_DEFAULT_ALT_SHORTCUT
        }),
        enter,
    ];
    match ContextMenu::new(names::PARAMETER, &entries).show_on(r) {
        Some(ParamMenu::Default) => {
            set(out, card, p, p.info.default);
            out.push(touch_action(card, p, false));
        }
        Some(ParamMenu::Enter) => {
            if let Some(text) = entry {
                open_value_entry(cu.ctx(), r.id, text);
            }
            out.push(touch_action(card, p, false));
        }
        None => {}
    }
}

/// Opens the undo gesture of parameter `id` of the card's device.
fn begin_param(card: &Card, id: u32) -> Action {
    Action::Plugin(PluginAction::BeginParamGesture {
        device: card.device,
        id,
    })
}

fn touch_action(card: &Card, p: &Param, dragging: bool) -> Action {
    Action::Ui(UiAction::TouchParam {
        device: card.device,
        param: p.info.id.clone(),
        dragging,
    })
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
    // A knob whose value is not used while a switch is on (makeup under auto-makeup).
    let unused = cell
        .disabled_by
        .as_ref()
        .and_then(|id| card.param(id))
        .is_some_and(|flag| flag.value >= 0.5);
    if unused {
        w = w.options(KnobCellOptions {
            disabled: true,
            ..KnobCellOptions::default()
        });
    }
    let r = cu.put(rect, w);
    touch(
        cu,
        out,
        card,
        p,
        &r,
        Some(p.text_at(p.value)),
        true,
        r.changed().then(|| info.to_plain(value)),
    );
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
            touch(
                cu,
                out,
                card,
                p,
                &r,
                Some(plain_text(p.value)),
                false,
                r.changed().then_some(value),
            );
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
            touch(
                cu,
                out,
                card,
                p,
                &r,
                Some(plain_text(p.value)),
                false,
                r.changed().then(|| info.to_plain(value)),
            );
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
    // The four oscillator waves are drawn as their icons (their names stay the accessible ones).
    let options: Vec<SegmentOption<'_>> = names
        .iter()
        .map(|n| match wave_of(n) {
            Some(wave) => SegmentOption::glyph(n, Glyph::Wave(wave)),
            None => SegmentOption::text(n),
        })
        .collect();
    let mut selected = index_of(p, names.len());
    let r = cu.put(
        rect,
        Segment::new(p.name(), &options, &mut selected).cols(cell.cols),
    );
    touch(
        cu,
        out,
        card,
        p,
        &r,
        None,
        false,
        r.changed().then_some(selected as f64),
    );
    true
}

/// The icon of an oscillator wave's German name.
fn wave_of(name: &str) -> Option<Wave> {
    match name {
        "Sinus" => Some(Wave::Sine),
        "Dreieck" => Some(Wave::Triangle),
        "Säge" => Some(Wave::Saw),
        "Rechteck" => Some(Wave::Square),
        _ => None,
    }
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
    touch(
        cu,
        out,
        card,
        p,
        &r,
        None,
        false,
        r.changed().then_some(if on { 1.0 } else { 0.0 }),
    );
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
    touch(
        cu,
        out,
        card,
        p,
        &r,
        None,
        false,
        r.changed().then_some(selected as f64),
    );
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
        .fill(if carrier(card, cell) {
            Fill::Track(color32(palette.base))
        } else {
            Fill::Neutral
        })
        .format(|n| p.text_at(info.to_plain(n)));
    let r = cu.put(rect, w);
    touch(
        cu,
        out,
        card,
        p,
        &r,
        Some(p.text_at(p.value)),
        true,
        r.changed().then(|| info.to_plain(value)),
    );
}

/// Whether the mini fader of `cell` fills as a carrier (the track colour): the Bool parameter the
/// cell names says so; without one (or while it is unknown) it does.
fn carrier(card: &Card, cell: &Cell) -> bool {
    cell.carrier
        .as_ref()
        .and_then(|id| card.param(id))
        .is_none_or(|p| p.value >= 0.5)
}

/// The selected entry (1-based) of context placeholder `key`, 1 without one.
fn selected_entry(ctx: &ParamContext, key: &str, count: u8) -> usize {
    ctx.get(key)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, usize::from(count.max(1)))
}

/// A switch of 1 … `count` bound to context placeholder `key` (the operator whose envelope shows).
fn select_segment(
    cu: &mut Ui,
    card: &Card,
    cell: &Cell,
    key: &str,
    count: u8,
    ctx: &ParamContext,
    rect: Rect,
) {
    let labels: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
    let options: Vec<SegmentOption<'_>> = labels.iter().map(|n| SegmentOption::text(n)).collect();
    let mut selected = selected_entry(ctx, key, count) - 1;
    let r = cu.put(
        rect,
        Segment::new(names::OPERATOR, &options, &mut selected).cols(cell.cols),
    );
    if r.changed() && set_context(cu.ctx(), card.device, key, selected + 1) {
        cu.ctx().request_repaint();
    }
}

/// A switch with named entries bound to context placeholder `key` ([`CellParam::Choice`]): the
/// selected entry's value is `{key}`, its label `{key}_label`.
fn choice_segment(
    cu: &mut Ui,
    card: &Card,
    cell: &Cell,
    (key, label): (&str, &str),
    options: &[(&str, &str)],
    ctx: &ParamContext,
    rect: Rect,
) {
    let entries: Vec<SegmentOption<'_>> = options
        .iter()
        .map(|(_, name)| SegmentOption::text(name))
        .collect();
    let mut selected = ctx
        .get(key)
        .and_then(|v| options.iter().position(|(value, _)| *value == v))
        .unwrap_or(0);
    let r = cu.put(
        rect,
        Segment::new(label, &entries, &mut selected).cols(cell.cols),
    );
    if r.changed() {
        let changed = set_context(cu.ctx(), card.device, key, options[selected].0)
            | set_context(
                cu.ctx(),
                card.device,
                &format!("{key}_label"),
                options[selected].1,
            );
        if changed {
            cu.ctx().request_repaint();
        }
    }
}

/// The parameter a cell for `p` shows: for the rate of an LFO whose `Sync` is on (`lfoN_rate` with
/// `lfoN_sync`), the divider `lfoN_div`.
fn synced_divider<'c>(card: &'c Card, p: &'c Param) -> &'c Param {
    let Some(prefix) = p.info.id.as_str().strip_suffix("_rate") else {
        return p;
    };
    let on = card
        .param(&komponist_model::params::ParamId::new(format!(
            "{prefix}_sync"
        )))
        .is_some_and(|sync| sync.value >= 0.5);
    if !on {
        return p;
    }
    card.param(&komponist_model::params::ParamId::new(format!(
        "{prefix}_div"
    )))
    .unwrap_or(p)
}

/// The columns of a [`WidgetKind::ColumnSelect`] overlay: a press anywhere in a column (also on
/// the knob or fader that covers it) selects it in context placeholder `key`; the selected column
/// has the accent outline. One AccessKit button per column.
fn column_select(
    cu: &mut Ui,
    card: &Card,
    cell: &Cell,
    key: &str,
    count: u8,
    ctx: &ParamContext,
    rect: Rect,
) {
    let selected = selected_entry(ctx, key, count);
    let step = device::grid::CELL[0] + device::grid::GAP;
    let pressed = cu.input(|i| {
        i.pointer
            .primary_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    for n in 1..=usize::from(count) {
        let left = rect.left() + (n - 1) as f32 * step;
        let column = Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(left + device::grid::CELL[0], rect.bottom()),
        );
        let name = names::operator(n);
        let r = cu.interact(
            column,
            slot(cu, cell, "column").with(n),
            komponist_ui::widgets::sense(cu, Sense::click()),
        );
        r.widget_info(|| WidgetInfo::selected(Role::Button, true, n == selected, &name));
        komponist_ui::widgets::accesskit(&r, |node| node.set_selected(n == selected));
        let hit = r.clicked() || pressed.is_some_and(|p| column.contains(p));
        if hit && set_context(cu.ctx(), card.device, key, n) {
            cu.ctx().request_repaint();
        }
        if n == selected {
            komponist_ui::state::paint_select_ring(cu.painter(), column, radius::M);
        }
    }
}

/// The gain-reduction meter: In, GR and Out from the device's level feed (`card.levels`, WP-89;
/// empty without one); the read-out is the largest reduction of the last 1.5 s. The click resets
/// the held reduction.
fn meter(cu: &mut Ui, card: &Card, cell: &Cell, rect: Rect) {
    let id = slot(cu, cell, "gr_hold");
    let now = cu.input(|i| i.time);
    let mut hold: GrHold = cu.data(|d| d.get_temp(id)).unwrap_or_default();
    if let Some(levels) = card.levels {
        hold.push(now, levels.reduction_db);
    }
    let text = meter_gr::readout(hold.max(now));
    let bars = card.levels.map_or([0.0; 3], |l| l.bars);
    let r = cu.put(rect, MeterGr::new(bars, &text).rows(cell.rows));
    if r.clicked() {
        hold.reset();
    }
    cu.data_mut(|d| d.insert_temp(id, hold));
}

/// What a pad's context menu offers.
#[derive(Clone, Copy)]
enum PadMenu {
    /// "Sample laden …" (or "Suchen …" for a missing one): the file browser.
    Pick,
    /// "Sample entfernen".
    Clear,
}

/// The pad grid: one pad per grid cell. The sampler's pads (WP-87) show the sample's name and the
/// note (or "fehlt" / "Lädt …"), light up while their voice plays, and are the selection `{sel}`
/// of the device's context (a click selects the pad and plays it, a sample file dropped on a pad
/// loads it, the right-click menu loads, searches and removes). Any other device with a pad grid
/// numbers its pads and keeps the selection in its context too.
fn pads(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    palette: &TrackPalette,
    rect: Rect,
) {
    let count = usize::from(cell.cols) * usize::from(cell.rows);
    let ctx = context(cu.ctx(), card.device);
    let selected = selected_entry(&ctx, SEL, count.min(255) as u8) - 1;
    let sampler = card.sampler.as_ref();
    let views = |i: usize| sampler.and_then(|s| s.pads.get(i));
    let names: Vec<String> = (0..count)
        .map(|i| match views(i).filter(|v| !v.name.trim().is_empty()) {
            Some(v) => v.name.clone(),
            None if sampler.is_some() => names::pad(i + 1),
            None => (i + 1).to_string(),
        })
        .collect();
    let notes: Vec<String> = (0..count)
        .map(|i| match (sampler, views(i).map(|v| &v.status)) {
            (None, _) => String::new(),
            (_, Some(PadStatus::Missing)) => names::PAD_MISSING.to_owned(),
            (_, Some(PadStatus::Loading)) => names::PAD_LOADING.to_owned(),
            (_, Some(PadStatus::Failed(_))) => names::PAD_FAILED.to_owned(),
            _ => note_name(komponist_builtin::sampler::note_of_pad(i)),
        })
        .collect();
    let pad_list: Vec<Pad<'_>> = names
        .iter()
        .zip(&notes)
        .map(|(n, note)| Pad::new(n, note))
        .collect();
    let playing: Vec<usize> = sampler.map_or_else(Vec::new, |s| {
        s.playheads
            .iter()
            .enumerate()
            .filter_map(|(i, h)| h.map(|_| i))
            .collect()
    });
    let mut dropped: Vec<(usize, std::path::PathBuf)> = Vec::new();
    let shown = cu
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |cu| {
            let mut grid = PadGrid::new(&pad_list, cell.cols, palette)
                .selected(Some(selected))
                .playing(&playing);
            if sampler.is_some() {
                grid = grid.on_drop(|pad, file| dropped.push((pad, file.path().to_path_buf())));
            }
            grid.show(cu)
        })
        .inner;
    if let Some(c) = shown.clicked {
        if set_context(cu.ctx(), card.device, SEL, c + 1) {
            cu.ctx().request_repaint();
        }
        if sampler.is_some() {
            out.push(Action::Plugin(PluginAction::AuditionPad {
                device: card.device,
                pad: c,
            }));
        }
    }
    let Some(sampler) = sampler else { return };
    for (pad, path) in dropped {
        set_context(cu.ctx(), card.device, SEL, pad + 1);
        out.push(Action::Plugin(PluginAction::LoadSample {
            device: card.device,
            pad,
            path,
        }));
    }
    for (i, response) in shown.pads.iter().enumerate() {
        let status = sampler.pads.get(i).map(|v| &v.status);
        let pick = match status {
            Some(PadStatus::Missing) => names::SEARCH,
            _ => names::LOAD_SAMPLE,
        };
        let mut clear = Entry::action(names::REMOVE_SAMPLE, PadMenu::Clear);
        if matches!(status, None | Some(PadStatus::Empty)) {
            clear = clear.disabled();
        }
        let entries = [Entry::action(pick, PadMenu::Pick), clear];
        match ContextMenu::new(&names::pad_menu(&names[i]), &entries).show_on(response) {
            Some(PadMenu::Pick) => out.push(Action::Plugin(PluginAction::PickSample {
                device: card.device,
                pad: i,
            })),
            Some(PadMenu::Clear) => out.push(Action::Plugin(PluginAction::ClearSample {
                device: card.device,
                pad: i,
            })),
            None => {}
        }
    }
}

/// The FM algorithm display of hexa's algorithm table; the parameter is the algorithm number
/// (1 …). Without a parameter it shows algorithm 1.
fn fm(
    cu: &mut Ui,
    card: &Card,
    cell: &Cell,
    p: Option<&Param>,
    palette: &TrackPalette,
    rect: Rect,
) {
    let ctx = context(cu.ctx(), card.device);
    let selected = selected_entry(&ctx, ENV_OP, ALGORITHMS[0].modulates.len() as u8) - 1;
    // Algorithm 0 is free routing: the display draws the routing the patch really has.
    let value = p.map_or(1, |p| p.value.round().max(0.0) as usize);
    let free = value == 0 && card.routing.is_some();
    let number = value.max(1);
    let algo = &ALGORITHMS[(number - 1).min(ALGORITHMS.len() - 1)];
    let routing = card.routing.filter(|_| free);
    let topology = match &routing {
        Some(r) => Topology {
            modulates: &r.modulates,
            outputs: Some(r.outputs),
            feedback: r.feedback,
        },
        None => Topology {
            modulates: &algo.modulates,
            outputs: None,
            feedback: Some(algo.feedback),
        },
    };
    let mut display = FmAlgorithm::new(number, topology, palette);
    if free {
        display = display.titled(names::FM_FREE);
    }
    let out = cu
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |cu| {
            display
                .cells(cell.cols, cell.rows)
                .selected(Some(selected))
                .show(cu)
        })
        .inner;
    if let Some(c) = out.clicked
        && set_context(cu.ctx(), card.device, ENV_OP, c + 1)
    {
        cu.ctx().request_repaint();
    }
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

/// The status of the pad `sample` is about.
fn v_status(
    sample: Option<(&komponist_builtin::sampler::PadView, Option<f32>)>,
) -> Option<&PadStatus> {
    sample.map(|(v, _)| &v.status)
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
    t: Lang,
    binding: &GraphBinding,
    title: &str,
    rate: Option<f32>,
    lookup: &dyn Fn(&str) -> Option<&'c Param>,
    sample_length: Option<f64>,
) -> Option<GraphSpec> {
    let spec = match binding.config {
        GraphConfig::Envelope => configs::envelope(t),
        GraphConfig::Eq { bands } => {
            // ±12 or ±24 dB, from the range of the gain parameters.
            let range = lookup("b1_gain").map_or(18.0, |p| p.info.range().1.abs());
            // The curve is the filters' response at the device's sample rate (WP-92).
            configs::eq_at(
                t,
                usize::from(bands),
                range,
                rate.map_or(CURVE_RATE, f64::from),
            )
        }
        GraphConfig::Filter => configs::filter(t),
        GraphConfig::Transfer => configs::transfer(t),
        GraphConfig::Compressor => configs::compressor(t),
        GraphConfig::GrHistory => configs::gr_history(t),
        // Start and end are in milliseconds; the axis spans the sample's real length when the
        // device knows it (the sampler), else the range of the end.
        GraphConfig::Waveform => configs::waveform(
            t,
            sample_length
                .filter(|l| *l > 0.0)
                .unwrap_or_else(|| lookup("end").map_or(1.0, |p| p.info.range().1 / 1000.0)),
        ),
        GraphConfig::Xy => {
            let (x, y) = (lookup("x")?, lookup("y")?);
            configs::xy(t, axis_of(x), axis_of(y), x.name(), y.name())
        }
        GraphConfig::Lfo { points } => configs::lfo(t, usize::from(points)),
        GraphConfig::FilterVa => configs::filter_va(t),
        GraphConfig::Signal => configs::signal(t),
        GraphConfig::LfoShape => configs::lfo_shape(t),
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
    let mut title = resolve(&binding.title, ctx).map_or(String::new(), |t| t.as_str().to_owned());
    // knurr: the filter names its type, the LFO its number and shape.
    let text_of = |slot: &str| lookup(slot).map(|p| p.text_at(p.value));
    match binding.config {
        GraphConfig::FilterVa => {
            if let Some(kind) = text_of("type") {
                title = format!("{} · {kind}", names::FILTER);
            }
        }
        GraphConfig::LfoShape => {
            if let (Some(shape), Some(n)) = (text_of("shape"), ctx.get(LFO_VIEW)) {
                title = format!("{} {n} · {shape}", names::LFO);
            }
        }
        _ => {}
    }
    // The sampler's waveform: the selected pad's real sample (length, peaks, playhead).
    let pad = selected_entry(ctx, SEL, 255) - 1;
    let sample = (binding.config == GraphConfig::Waveform)
        .then_some(card.sampler.as_ref())
        .flatten()
        .and_then(|s| Some((s.pads.get(pad)?, s.playheads.get(pad).copied().flatten())));
    if let Some((view, _)) = sample {
        title = names::waveform_title(pad + 1, &view.name);
    }
    let length = sample
        .map(|(v, _)| v.length_s)
        .filter(|l| *l > 0.0 && matches!(v_status(sample), Some(PadStatus::Ready)));
    let rate = card.analysis.as_ref().map(|a| a.rate).filter(|r| *r > 0.0);
    let Some(spec) = spec_of(
        komponist_ui::lang(cu.ctx()),
        binding,
        &title,
        rate,
        &lookup,
        length,
    ) else {
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
    // An end beyond the sample (the maximum means "to the end") shows at the sample's end, a start
    // never behind it.
    if let Some(len) = length {
        let max_ms = len * 1000.0;
        for (i, name) in spec.slots.iter().enumerate() {
            if name == "end" || name == "start" {
                values[i] = values[i].min(max_ms);
            }
        }
    }
    // The live spectrum; empty while hidden, bypassed or switched off.
    let spectrum: &[f32] = card.analysis.as_ref().map_or(&[], |a| &a.spectrum);
    // The sampler's waveform shows the sample's peaks, the EQ its live spectrum.
    let peaks: &[f32] = sample.map_or(spectrum, |(v, _)| &v.peaks);
    let mut playhead = sample
        .and_then(|(_, head)| head)
        .zip(length)
        .map(|(h, len)| f64::from(h) * len);
    // The compressor (WP-93): its live level on the transfer curve, and the history of the input
    // level and the reduction, with „now“ at the right edge.
    let dynamics = card.dynamics.as_ref();
    let (mut peaks, mut trace, mut level): (&[f32], &[f32], Option<f64>) = (peaks, &[], None);
    match binding.config {
        GraphConfig::Compressor => {
            level = dynamics
                .and_then(|d| d.level_db)
                .map(|db| f64::from(db).clamp(spec.x.min, spec.x.max));
        }
        GraphConfig::GrHistory => {
            if let Some(d) = dynamics {
                peaks = &d.input;
                trace = &d.reduction;
                playhead = Some(configs::GR_HISTORY_SECONDS);
            }
        }
        _ => {}
    }
    // presse with an external sidechain source: the detector listens to that source.
    let detector_note = (binding.config == GraphConfig::Compressor && card.sidechain.is_some())
        .then_some((names::DETECTOR, names::DETECTOR_EXTERNAL));
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
                .data(peaks)
                .trace(trace)
                .level(level)
                .levels_note(detector_note)
                .playhead(playhead)
                .id(id)
                .show(gu)
        })
        .inner;
    // One undo step per drag of a handle (or burst of wheel notches); the first parameter the
    // handle edits names it.
    let first = shown.selected_slot.or(shown.changed.first().map(|c| c.0));
    let first_wire = first
        .and_then(|i| spec.slots.get(i))
        .and_then(|s| lookup(s))
        .map_or(u32::MAX, |p| p.wire);
    let touch = Touch {
        drag_started: shown.touch.drag_started,
        dragged: shown.touch.dragging,
        drag_stopped: shown.touch.drag_stopped,
        wheel: shown.touch.wheel,
        hovered: shown.response.hovered(),
    };
    gesture_touch(
        cu.ctx(),
        shown.response.id,
        touch,
        !shown.changed.is_empty(),
        || begin_param(card, first_wire),
        out,
        |out| {
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
        },
    );
    // Touching a handle selects the parameter it edits (the axis moved last).
    let touched = shown.response.is_pointer_button_down_on()
        || shown.response.drag_stopped()
        || !shown.changed.is_empty();
    if touched
        && let Some(slot) = shown.selected_slot.or(shown.changed.first().map(|c| c.0))
        && let Some(p) = spec.slots.get(slot).and_then(|s| lookup(s))
    {
        out.push(touch_action(card, p, shown.response.dragged()));
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
            // Four parameters, one undo step.
            out.push(Action::Edit(EditAction::BeginGesture {
                label: names::UNDO_BAND_ADD.to_owned(),
            }));
            for (what, v) in [("freq", x), ("gain", y), ("type", band::BELL), ("on", 1.0)] {
                if let Some(p) = band_param(n, what) {
                    set(out, card, p, v);
                }
            }
            out.push(Action::Edit(EditAction::EndGesture));
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
    use egui_kittest::kittest::NodeT as _;
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
            sampler: None,
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
            internal_presets: None,
            preset_changed: false,
            has_sidechain: false,
            sidechain: None,
            width: 244.0,
            late: None,
            levels: None,
            analysis: None,
            dynamics: None,
            routing: None,
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
    fn dragging_a_graph_handle_is_bracketed_into_one_gesture() {
        let mut h = graph_harness(graph_card());
        let handle = h
            .get_by_role_and_label(Role::Slider, "Band 1 Frequenz")
            .rect()
            .center();
        h.hover_at(handle);
        h.step();
        for pressed in [true, false] {
            if !pressed {
                for i in 1..=6 {
                    h.hover_at(handle + egui::vec2(6.0 * i as f32, 0.0));
                    h.step();
                }
            }
            h.event(egui::Event::PointerButton {
                pos: if pressed {
                    handle
                } else {
                    handle + egui::vec2(36.0, 0.0)
                },
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
        }
        h.run_steps(3);
        let actions = &h.state().1;
        let begins = actions
            .iter()
            .filter(|a| matches!(a, Action::Plugin(PluginAction::BeginParamGesture { .. })))
            .count();
        let ends = actions
            .iter()
            .filter(|a| matches!(a, Action::Edit(EditAction::EndGesture)))
            .count();
        let sets = actions
            .iter()
            .filter(|a| matches!(a, Action::Plugin(PluginAction::SetParam { .. })))
            .count();
        assert!(sets >= 2, "the handle moved: {actions:?}");
        assert_eq!((begins, ends), (1, 1), "{actions:?}");
        let first = actions
            .iter()
            .position(|a| matches!(a, Action::Plugin(PluginAction::BeginParamGesture { .. })))
            .unwrap();
        let first_set = actions
            .iter()
            .position(|a| matches!(a, Action::Plugin(PluginAction::SetParam { .. })))
            .unwrap();
        assert!(
            first < first_set,
            "the gesture opens before the first value"
        );
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
    /// A spectrum like music: a sawtooth at 220 Hz at -14 dB over a quiet noise floor, analyzed the
    /// way the app does (tap, window, FFT, ballistics) at `rate`.
    fn test_spectrum(rate: f32) -> std::sync::Arc<[f32]> {
        let tap = komponist_engine::SampleTap::new(16_384);
        tap.set_rate(rate);
        let mut analyzer = crate::spectrum::SpectrumAnalyzer::new();
        analyzer.update(0.0, &tap);
        let mut seed = 12_345_u32;
        let signal: Vec<f32> = (0..8192)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f32 / (1 << 24) as f32 - 0.5;
                let phase = (220.0 * i as f32 / rate).fract();
                0.2 * (2.0 * phase - 1.0) + 0.004 * noise
            })
            .collect();
        tap.push(&signal);
        analyzer.update(0.05, &tap);
        std::sync::Arc::clone(analyzer.values())
    }

    fn with_spectrum(rate: f32, bypass: bool) -> Card {
        let mut card = graph_card();
        card.bypass = bypass;
        card.analysis = Some(crate::spectrum::Analysis {
            rate,
            spectrum: test_spectrum(rate),
        });
        card
    }

    #[test]
    fn the_eq_graph_shows_the_live_spectrum_behind_its_curve() {
        let mut results = egui_kittest::SnapshotResults::new();
        let mut h = graph_harness(with_spectrum(48_000.0, false));
        h.snapshot_options(
            "device-eq-spectrum-1x",
            &egui_kittest::SnapshotOptions::new(),
        );
        results.extend_harness(&mut h);
        results.unwrap();
    }

    #[test]
    fn bypass_and_an_empty_spectrum_show_no_data() {
        let image = |card: Card| graph_harness(card).render().unwrap();
        let mut none = graph_card();
        none.analysis = Some(crate::spectrum::Analysis {
            rate: 48_000.0,
            spectrum: std::sync::Arc::default(),
        });
        let plain = image(none);
        assert!(
            image(with_spectrum(48_000.0, false)) != plain,
            "the spectrum is drawn"
        );
        let mut bypassed = graph_card();
        bypassed.bypass = true;
        let bypassed_plain = image(bypassed);
        assert!(
            image(with_spectrum(48_000.0, true)) == bypassed_plain,
            "bypass hides the data"
        );
    }
    // --- The compressor card (WP-93) ----------------------------------------------------------

    /// The card of `presse` with its real layout and the parameters at `set` (by id) over the
    /// defaults, and the live data `dynamics`.
    fn presse_card(set: &[(&str, f64)], dynamics: Option<crate::gain_trace::Dynamics>) -> Card {
        let kind = komponist_builtin::Internal::Presse;
        let mut card = graph_card();
        card.params = kind
            .params()
            .iter()
            .enumerate()
            .map(|(i, info)| {
                let value = set
                    .iter()
                    .find(|(k, _)| *k == info.id.as_str())
                    .map_or(info.default, |(_, v)| *v);
                Param {
                    wire: i as u32,
                    text: info.format(value),
                    info: info.clone(),
                    value,
                }
            })
            .collect();
        card.layout = kind.layout().clone();
        card.dynamics = dynamics;
        card
    }

    /// Four seconds of a loud passage in the middle: input level bumps and a reduction that dips.
    fn some_dynamics(level_db: Option<f32>) -> crate::gain_trace::Dynamics {
        let n = crate::gain_trace::POINTS;
        let input: Vec<f32> = (0..n)
            .map(|i| 0.05 + 0.3 * ((i as f32 * 0.37).sin().abs()))
            .collect();
        let reduction: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / n as f32;
                -(12.0 * (t * 6.0).sin().powi(2) * t).min(18.0)
            })
            .collect();
        crate::gain_trace::Dynamics {
            level_db,
            input: input.into(),
            reduction: reduction.into(),
        }
    }

    fn presse_harness(card: Card) -> egui_kittest::Harness<'static, (Card, Vec<Action>)> {
        // The device body: 8 columns, 3 rows (`device_width(8)` × `frame::BODY_H`).
        graph_harness_sized(card, egui::vec2(492.0, 212.0))
    }

    #[test]
    fn the_compressor_card_has_both_graphs_and_a_makeup_that_auto_makeup_dims() {
        let h = presse_harness(presse_card(&[], Some(some_dynamics(Some(-9.0)))));
        h.check_accessibility();
        h.get_by_role_and_label(Role::Group, "Kennlinie");
        h.get_by_role_and_label(Role::Group, "Reduktion \u{b7} 4 s");
        for name in [
            "Threshold",
            "Ratio",
            "Knie",
            "Attack",
            "Release",
            "Makeup",
            "Mix",
            "Link",
            "SC-HP",
        ] {
            h.get_by_role_and_label(Role::Slider, name);
        }
        for name in ["Lookahead", "Auto-Rel.", "Auto-Mkp.", "SC h\u{f6}ren"] {
            h.get_by_role_and_label(Role::Button, name);
        }
        assert!(
            !h.get_by_role_and_label(Role::Slider, "Makeup")
                .accesskit_node()
                .is_disabled(),
            "manual makeup is live"
        );
        let auto = presse_harness(presse_card(&[("auto_makeup", 1.0)], None));
        assert!(
            auto.get_by_role_and_label(Role::Slider, "Makeup")
                .accesskit_node()
                .is_disabled(),
            "makeup is dimmed under auto-makeup"
        );
        assert!(
            !auto
                .get_by_role_and_label(Role::Slider, "Threshold")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn live_data_changes_both_compressor_graphs_and_a_bypassed_card_shows_none() {
        let image = |card: Card| presse_harness(card).render().unwrap();
        let idle = image(presse_card(&[], None));
        let playing = image(presse_card(&[], Some(some_dynamics(Some(-9.0)))));
        assert!(
            playing != idle,
            "level point, readouts and history are drawn"
        );
        let no_level = image(presse_card(&[], Some(some_dynamics(None))));
        assert!(no_level != playing, "the level point is the live part");
        // The level changes the readouts and the point, so a different level is a different picture.
        let louder = image(presse_card(&[], Some(some_dynamics(Some(-3.0)))));
        assert!(louder != playing);
    }

    #[test]
    fn the_compressor_card_snapshot() {
        let mut results = egui_kittest::SnapshotResults::new();
        let mut h = presse_harness(presse_card(
            &[("detector", 1.0), ("auto_makeup", 1.0), ("sc_hpf", 80.0)],
            Some(some_dynamics(Some(-9.0))),
        ));
        h.snapshot_options("device-presse-1x", &egui_kittest::SnapshotOptions::new());
        results.extend_harness(&mut h);
        results.unwrap();
    }
}
