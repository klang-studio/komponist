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
use komponist_hexa::algorithms::ALGORITHMS;
use komponist_i18n::Lang;
use komponist_ipc::device_assets::{AssetKind, AssetStatus};
use komponist_model::DeviceId;
use komponist_model::params::eq_curve::CURVE_RATE;
use komponist_model::params::{ParamContext, ParamKind, Scale, Unit, resolve};
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
    AreaAction, Axis, AxisKind, Bind, Graph, GraphEvent, GraphSpec, Handle, HandleLook, Look,
    MarkerHead, Pred, SlotInfo,
};
use komponist_ui::widgets::device::knob_cell::KnobCell;
use komponist_ui::widgets::device::meter_gr::{self, GrHold, MeterGr};
use komponist_ui::widgets::device::mini_fader::{Fill, MiniFader};
use komponist_ui::widgets::device::number::Number;
use komponist_ui::widgets::device::pad_grid::{Pad, PadGrid};
use komponist_ui::widgets::dropdown::Item;
use komponist_ui::widgets::number_field::{open_value_entry, parse_number};
use komponist_ui::{fonts, px};
use komponist_volt as volt;

use super::asset_waveform;
use super::layout::{
    BAND, Cell, CellParam, ENV_OP, ENV_VIEW, GraphBinding, GraphConfig, LFO_VIEW, OSC_VIEW, SEL,
    WidgetKind,
};
use super::metrics;
use super::model::{self, Card, Param};
use super::waveform_slices;
use crate::actions::{Action, EditAction, PluginAction, UiAction};
use crate::platform::Platform;
use crate::views::mixer::{Touch, gesture_actions, gesture_touch};
use crate::views::track_macros::Pick as ParamMenu;
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
    // volt's switches start on their first entry (Amp envelope, LFO 1), surf's on oscillator A.
    choose(&mut ctx, ENV_VIEW, volt::ENVELOPES, 0);
    choose(&mut ctx, LFO_VIEW, volt::LFOS, 0);
    choose(&mut ctx, OSC_VIEW, komponist_surf::OSCILLATORS, 0);
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
pub(crate) fn draw_cell(
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
    if param.is_some_and(|p| waveform_slices::hides_control(card, p.wire)) {
        return;
    }
    let localized = param.and_then(|param| {
        let layout = card.native_layout.as_ref()?;
        let labels = card.native_cell(cell)?.labels.as_ref()?;
        let label = layout.label(labels, komponist_ui::lang(cu.ctx()).code())?;
        let mut localized = param.clone();
        localized.info.name = label.to_owned();
        Some(localized)
    });
    let param = localized.as_ref().or(param);
    // The rate knob of a synced LFO shows its divider instead.
    let param = param.map(|p| synced_divider(card, p));
    if let Some(control) = param.and_then(|p| card.macros.get(&p.info.id)) {
        control.paint_ground(cu, rect);
    }
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
        (WidgetKind::PadGrid, _) => card.pad_bank().is_some_and(|bank| {
            native_pads(cu, out, card, cell, palette, rect, bank);
            true
        }),
        (WidgetKind::FmAlgorithm, p) => {
            fm(cu, card, cell, p, palette, rect);
            true
        }
        (WidgetKind::Graph, _) => graph(cu, out, card, palette, cell, &ctx, rect),
        (WidgetKind::Canvas, _) => super::canvas::draw(cu, card, cell, palette, rect),
        _ => false,
    };
    if !drawn {
        placeholder(cu, cell, param, rect);
    }
}

pub(super) fn set(out: &mut Vec<Action>, card: &Card, p: &Param, plain: f64) {
    if card
        .macros
        .get(&p.info.id)
        .is_some_and(|control| control.driver.is_some())
    {
        return;
    }
    let plain = p.snap(plain);
    if plain != p.value {
        out.push(Action::Plugin(PluginAction::SetParam {
            device: card.device,
            id: p.wire,
            value: card.wire_value(p, plain),
        }));
    }
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
/// eingeben" and "Automation zeigen" for automatable parameters.
#[allow(clippy::too_many_arguments)]
pub(super) fn touch(
    cu: &Ui,
    out: &mut Vec<Action>,
    card: &Card,
    p: &Param,
    r: &egui::Response,
    entry: Option<String>,
    double_click_resets: bool,
    change: Option<f64>,
) {
    let t = komponist_ui::lang(cu.ctx());
    let control = card.macros.get(&p.info.id);
    if let Some(control) = control {
        let range = control.driver.as_ref().map(|driver| {
            t.devices_range(
                p.text_at(p.value_at_normalized(card.bundled_editor, f64::from(driver.range.0))),
                p.text_at(p.value_at_normalized(card.bundled_editor, f64::from(driver.range.1))),
            )
        });
        control.decorate(cu, r, r.rect, range.as_deref());
    }
    let intercepted = control.is_some_and(|control| control.intercept(r, change.is_some(), out));
    if !intercepted {
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
    }
    if r.is_pointer_button_down_on() || r.drag_stopped() || r.clicked() {
        out.push(touch_action(card, p, r.dragged()));
    }
    let mut enter = Entry::action(t.devices_enter_value(), ParamMenu::Enter);
    if entry.is_none() {
        enter = enter.disabled();
    }
    let mut entries = vec![
        Entry::action(t.devices_to_default(), ParamMenu::Default).shortcut(
            if double_click_resets {
                t.devices_to_default_shortcut()
            } else {
                Platform::current().pick(
                    t.devices_to_default_alt_shortcut(),
                    t.devices_to_default_alt_shortcut_mac(),
                )
            },
        ),
        enter,
    ];
    let owner = card.automation_owner.filter(|_| p.info.automatable);
    if owner.is_some() {
        entries.push(Entry::action(
            t.automation_show_menu(),
            ParamMenu::Automation,
        ));
    }
    let pick = if let Some(control) = control {
        control.menu(t, &entries, r)
    } else {
        ContextMenu::new(t.devices_parameter(), &entries).show_on(r)
    };
    if let Some(action) = pick.and_then(|pick| control?.menu_action(pick)) {
        out.push(action);
        return;
    }
    match pick {
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
        Some(ParamMenu::Automation) => {
            if let Some(owner) = owner {
                out.push(Action::Automation(
                    crate::views::automation::Action::ShowTarget {
                        owner,
                        replace: None,
                        target: komponist_model::automation::AutomationTarget::Param {
                            device: card.device,
                            param: p.project_id(card.bundled_editor),
                        },
                        label: format!("{} · {}", card.name, p.name()),
                    },
                ));
            }
        }
        _ => {}
    }
}

/// Opens the undo gesture of parameter `id` of the card's device.
pub(super) fn begin_param(card: &Card, id: u32) -> Action {
    Action::Plugin(PluginAction::BeginParamGesture {
        device: card.device,
        id,
    })
}

pub(super) fn touch_action(card: &Card, p: &Param, dragging: bool) -> Action {
    Action::Ui(UiAction::TouchParam {
        device: card.device,
        param: p.project_id(card.bundled_editor),
        dragging,
    })
}

/// Memory slot of a cell's own state.
fn slot(cu: &Ui, cell: &Cell, what: &str) -> Id {
    cu.scope_id().with((
        "komponist_cell",
        cell.col,
        cell.row,
        match cell.half {
            None => 0,
            Some(super::layout::Half::Top) => 1,
            Some(super::layout::Half::Bottom) => 2,
        },
        what,
    ))
}

/// `text` split into the number and its trailing unit: „420 Hz“ → („420“, „Hz“), „L 14“ stays whole.
fn split_unit(text: &str, native: bool) -> (&str, &str) {
    if native && let Some(number) = text.strip_suffix('%') {
        return (number.trim_end(), "%");
    }
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
        .redirect(
            card.macros
                .get(&p.info.id)
                .is_some_and(crate::views::track_macros::Control::redirects),
        )
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
    w = w.options(KnobCellOptions {
        disabled: unused,
        automation_dot: card.automation.get(&p.info.id).copied(),
        modulation: card
            .macros
            .get(&p.info.id)
            .and_then(|control| control.modulation()),
        ..KnobCellOptions::default()
    });
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
    if let Some(scale) = asset_waveform::trim_milliseconds(card, p.wire) {
        let display = |ms| komponist_ui::widgets::number_field::format_number(ms, 0);
        let mut time = p.value * scale;
        let response = cu.put(
            rect,
            Number::new(&mut time, p.name())
                .range(0.0..=scale)
                .default_value(p.info.default * scale)
                .unit("ms")
                .cols(cell.cols)
                .format(display),
        );
        touch(
            cu,
            out,
            card,
            p,
            &response,
            Some(display(p.value * scale)),
            false,
            response.changed().then_some(time / scale),
        );
        return true;
    }
    let info = &p.info;
    let (min, max) = info.range();
    let steps = info.steps();
    let plain_step = match &info.kind {
        ParamKind::Int { .. } => Some(1.0),
        ParamKind::Bool if card.native_layout.is_some() => Some(1.0),
        ParamKind::Float { .. } if steps > 0 => Some((max - min) / f64::from(steps)),
        ParamKind::Float { .. } => None,
        _ => return false,
    };
    let native = card.native_layout.is_some();
    let current = p.text_at(p.value);
    let unit = split_unit(&current, native).1.to_owned();
    let plain_text = |v: f64| {
        let text = p.text_at(v);
        let (number, suffix) = split_unit(&text, native);
        if !unit.is_empty() && suffix == unit {
            number.to_owned()
        } else {
            text
        }
    };
    match plain_step {
        Some(step) => {
            let mut value = p.value;
            let w = Number::new(&mut value, p.name().to_owned())
                .range(min..=max)
                .step(step)
                .default_value(info.default)
                .unit(unit.clone())
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
            let unit_scale = if native && unit == "%" && info.unit == Unit::Percent {
                100.0
            } else {
                1.0
            };
            let w = Number::new(&mut value, p.name().to_owned())
                .default_value(info.default_normalized())
                .unit(unit.clone())
                .cols(cell.cols)
                .format(|n| plain_text(info.to_plain(n)))
                .parse(|t| parse_number(t, &unit).map(|v| info.to_normalized(v / unit_scale)));
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

fn choices<'a>(
    card: &Card,
    cell: &Cell,
    p: &'a Param,
    lang: Lang,
) -> Option<(std::borrow::Cow<'a, [String]>, f64)> {
    use std::borrow::Cow;
    if let Some(layout) = &card.native_layout
        && let Some(choices) = &card.native_cell(cell)?.choices
    {
        let names = choices
            .iter()
            .map(|choice| {
                layout
                    .label(&choice.labels, lang.code())
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        return Some((Cow::Owned(names), f64::from(choices.first()?.value)));
    }
    p.info
        .switch_choices()
        .filter(|names| !names.is_empty())
        .map(|names| (Cow::Borrowed(names), 0.0))
}

fn index_of(value: f64, first: f64, n: usize) -> usize {
    ((value - first).round().max(0.0) as usize).min(n.saturating_sub(1))
}

fn segment(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    rect: Rect,
) -> bool {
    let Some((names, first)) = choices(card, cell, p, komponist_ui::lang(cu.ctx())) else {
        return false;
    };
    // The four oscillator waves are drawn as their icons (their names stay the accessible ones).
    let options: Vec<SegmentOption<'_>> = names
        .iter()
        .enumerate()
        .map(|(i, n)| match wave_of(p, i) {
            Some(wave) => SegmentOption::glyph(n, Glyph::Wave(wave)),
            None => SegmentOption::text(n),
        })
        .collect();
    let mut selected = index_of(p.value, first, names.len());
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
        r.changed().then_some(first + selected as f64),
    );
    true
}

/// The icon of choice `index` of an oscillator wave or LFO shape parameter: the first four choices
/// are sine, triangle, saw and square in every language (`va_curve::WAVES`); anything else is text.
fn wave_of(p: &Param, index: usize) -> Option<Wave> {
    const WAVE_PARAMS: [&str; 4] = ["osc_a_wave", "osc_b_wave", "lfo1_shape", "lfo2_shape"];
    if !WAVE_PARAMS.contains(&p.info.id.as_str()) {
        return None;
    }
    [Wave::Sine, Wave::Triangle, Wave::Saw, Wave::Square]
        .get(index)
        .copied()
}

fn toggle(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    p: &Param,
    rect: Rect,
) -> bool {
    if card.native_layout.is_none() && !matches!(p.info.kind, ParamKind::Bool) {
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
    let Some((names, first)) = choices(card, cell, p, komponist_ui::lang(cu.ctx())) else {
        return false;
    };
    let items: Vec<Item<'_>> = names.iter().map(|n| Item::new(n)).collect();
    let mut selected = index_of(p.value, first, names.len());
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
        r.changed().then_some(first + selected as f64),
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
    let Some(id) = &cell.carrier else {
        return true;
    };
    // Changing Hexa's algorithm edits the patch routing as a group. The separate output
    // CLAP bindings can still report their last individual edits.
    if card.bundled_editor
        && let Some(routing) = card.routing
        && let Some(op) = id
            .as_str()
            .strip_prefix("op")
            .and_then(|id| id.strip_suffix("_output"))
            .and_then(|number| number.parse::<usize>().ok())
            .and_then(|number| number.checked_sub(1))
            .filter(|&op| op < routing.modulates.len())
        && let Some(algorithm) = card
            .params
            .iter()
            .find(|p| p.info.id.as_str() == "algorithm")
    {
        let number = algorithm.value.round() as usize;
        return if number == 0 {
            routing.outputs & (1 << op) != 0
        } else {
            komponist_hexa::device::is_carrier(number, op)
        };
    }
    card.param(id).is_none_or(|p| p.value >= 0.5)
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
    let t = komponist_ui::lang(cu.ctx());
    let labels: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
    let options: Vec<SegmentOption<'_>> = labels.iter().map(|n| SegmentOption::text(n)).collect();
    let mut selected = selected_entry(ctx, key, count) - 1;
    let r = cu.put(
        rect,
        Segment::new(t.devices_operator(), &options, &mut selected).cols(cell.cols),
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
    let t = komponist_ui::lang(cu.ctx());
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
        let name = t.devices_operator_n(n);
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
    let id = slot(cu, cell, "gr_hold").with(card.page);
    let now = cu.input(|i| i.time);
    let mut hold: GrHold = cu.data(|d| d.get_temp(id)).unwrap_or_default();
    let levels = card
        .native_meter_levels
        .get(&cell.col)
        .copied()
        .or(card.levels);
    if let Some(levels) = levels {
        hold.push(now, levels.reduction_db);
    }
    let text = meter_gr::readout(hold.max(now));
    let bars = levels.map_or([0.0; 3], |l| l.bars);
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

fn native_pads(
    cu: &mut Ui,
    out: &mut Vec<Action>,
    card: &Card,
    cell: &Cell,
    palette: &TrackPalette,
    rect: Rect,
    bank: &komponist_ipc::native_layout::PadBank,
) {
    let lang = komponist_ui::lang(cu.ctx());
    let slot = |id| {
        card.asset_slots
            .iter()
            .find(|(slot, _)| slot.id == id && slot.kind == AssetKind::AudioSample)
    };
    let names: Vec<_> = bank
        .pads
        .iter()
        .enumerate()
        .map(|(index, pad)| {
            card.asset_names
                .get(&pad.asset_slot)
                .cloned()
                .unwrap_or_else(|| card.layout.pages[index].name.clone())
        })
        .collect();
    let notes: Vec<_> = bank
        .pads
        .iter()
        .map(
            |pad| match slot(pad.asset_slot).map(|(asset, _)| &asset.status) {
                Some(AssetStatus::Missing) => lang.devices_pad_missing().into(),
                Some(AssetStatus::Loading) => lang.devices_pad_loading().into(),
                Some(AssetStatus::Failed(_)) => lang.devices_pad_failed().into(),
                Some(AssetStatus::Empty) | None
                    if card.asset_names.contains_key(&pad.asset_slot) =>
                {
                    lang.devices_pad_loading().into()
                }
                _ => note_name(pad.note),
            },
        )
        .collect();
    let pads: Vec<_> = names
        .iter()
        .zip(&notes)
        .map(|(name, note)| Pad::new(name, note))
        .collect();
    let playing: Vec<_> = card
        .pad_activity
        .iter()
        .flat_map(|activity| {
            activity
                .playheads
                .iter()
                .enumerate()
                .filter_map(|(index, head)| head.map(|_| index))
        })
        .collect();
    let shown = cu
        .scope_builder(egui::UiBuilder::new().max_rect(rect), |cu| {
            PadGrid::new(&pads, cell.cols, palette)
                .selected(Some(card.page))
                .playing(&playing)
                .show(cu)
        })
        .inner;
    if let Some(index) = shown.clicked {
        select_pad(cu.ctx(), card.device, index);
        out.push(Action::Plugin(PluginAction::AuditionNote {
            device: card.device,
            key: bank.pads[index].note,
        }));
        cu.ctx().request_repaint();
    }
    if card.macro_mapping {
        return;
    }
    for (index, response) in shown.pads.iter().enumerate() {
        let id = bank.pads[index].asset_slot;
        let Some((asset, linked)) = slot(id) else {
            continue;
        };
        if let Some(path) = asset_waveform::dropped_file(cu, card, response.rect, id) {
            select_pad(cu.ctx(), card.device, index);
            out.push(Action::Plugin(PluginAction::SetPluginAsset {
                device: card.device,
                slot: id,
                path: Some(path),
            }));
            cu.ctx().request_repaint();
        }
        let pick = if asset.status == AssetStatus::Missing {
            lang.devices_search()
        } else {
            lang.devices_load_sample()
        };
        let clear = Entry::action(lang.devices_remove_sample(), PadMenu::Clear);
        let entries = [
            Entry::action(pick, PadMenu::Pick),
            if *linked { clear } else { clear.disabled() },
        ];
        match ContextMenu::new(&lang.devices_pad_menu(&names[index]), &entries).show_on(response) {
            Some(PadMenu::Pick) => out.push(Action::Plugin(PluginAction::PickPluginAsset {
                device: card.device,
                slot: id,
            })),
            Some(PadMenu::Clear) => out.push(Action::Plugin(PluginAction::SetPluginAsset {
                device: card.device,
                slot: id,
                path: None,
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
    let t = komponist_ui::lang(cu.ctx());
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
        display = display.titled(t.devices_fm_free());
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
    osc: usize,
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
        // asset provides it, else the range of the end.
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
        GraphConfig::Wavetable => configs::wavetable(t, osc),
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
    let t = komponist_ui::lang(cu.ctx());
    let CellParam::Graph(binding) = &cell.param else {
        return false;
    };
    let lookup = |slot: &str| slot_param(card, binding, ctx, slot);
    let mut title = resolve(&binding.title, ctx).map_or(String::new(), |t| t.as_str().to_owned());
    // volt: the filter names its type, the LFO its number and shape.
    let text_of = |slot: &str| lookup(slot).map(|p| p.text_at(p.value));
    match binding.config {
        GraphConfig::FilterVa => {
            if let Some(kind) = text_of("type") {
                title = format!("{} · {kind}", t.devices_filter());
            }
        }
        GraphConfig::LfoShape => {
            if let (Some(shape), Some(n)) = (text_of("shape"), ctx.get(LFO_VIEW)) {
                title = format!("{} {n} · {shape}", t.devices_lfo());
            }
        }
        // surf: „Osz A · Vokale“.
        GraphConfig::Wavetable => {
            if let (Some(table), Some(osc)) =
                (text_of("table"), ctx.get(&format!("{OSC_VIEW}_label")))
            {
                title = format!("{} {osc} · {table}", t.devices_osc());
            }
        }
        _ => {}
    }
    let native_graph = card.native_cell(cell).and_then(|cell| cell.graph.as_ref());
    let asset_slot = native_graph.and_then(|graph| graph.asset_slot);
    let fractional_trim = native_graph.is_some_and(|graph| {
        graph.trim_unit == Some(komponist_ipc::native_layout::TrimUnit::SourceFraction)
    });
    let asset = asset_slot.and_then(|id| card.asset_waveforms.get(&id));
    let length = asset.map(|source| source.duration);
    let signals = card.graph_signals.get(&(cell.row, cell.col));
    let analysis = signals.map_or(card.analysis.as_ref(), |signals| signals.analysis.as_ref());
    let rate = analysis.map(|a| a.rate).filter(|r| *r > 0.0);
    // surf: the range on the stack follows the shown oscillator's position.
    let osc = usize::from(ctx.get(OSC_VIEW) == Some("b"));
    let (title, description) = card.native_layout.as_ref().zip(native_graph).map_or(
        (title.as_str(), None),
        |(layout, graph)| {
            (
                layout.label(&graph.name, t.code()).unwrap_or(&title),
                layout.label(&graph.description, t.code()),
            )
        },
    );
    let pad_title = card.pad_for_asset(asset_slot).map(|pad| {
        let name = asset_slot
            .and_then(|slot| card.asset_names.get(&slot))
            .map_or_else(|| t.devices_no_sample(), String::as_str);
        format!("{} · {name}", card.layout.pages[pad].name)
    });
    let title = pad_title.as_deref().unwrap_or(title);
    let Some(mut spec) = spec_of(
        komponist_ui::lang(cu.ctx()),
        binding,
        osc,
        title,
        rate,
        &lookup,
        length.or(fractional_trim.then_some(1.0)),
    ) else {
        return false;
    };
    if native_graph.is_some_and(|graph| graph.hide_title) {
        spec.title.clear();
    }
    let has_waveform_toolbar = native_graph.is_some_and(|graph| graph.waveform_toolbar);
    if has_waveform_toolbar {
        asset_waveform::style(&mut spec);
    }
    let full_rect = rect;
    let rect = native_graph.map_or(rect, |native| {
        asset_waveform::toolbar(
            cu,
            out,
            card,
            cell,
            rect,
            native,
            signals.and_then(|signals| signals.playhead),
        )
    });
    let viewport =
        asset_slot.and_then(|slot| asset_waveform::view(cu, card, cell, rect, spec.x.pad, slot));
    let full_end = spec.x.max;
    // The waveform draws trim in milliseconds even when the plugin stores source fractions.
    let trim_scale = fractional_trim.then_some(full_end * 1000.0);
    if let Some(view) = &viewport {
        spec.x.min = view.start;
        spec.x.max = view.end;
        spec.on_double_click = AreaAction::ResetZoom;
        for (slot, handle) in spec.handles.iter_mut().enumerate() {
            let visible_ms = view.start * 1000.0..=view.end * 1000.0;
            handle.visible = Some(Pred::new(move |values| visible_ms.contains(&values[slot])));
        }
    }
    if has_waveform_toolbar {
        asset_waveform::time_ticks(&mut spec);
    }
    let normalized = binding.config == GraphConfig::Envelope;
    let mut values = Vec::with_capacity(spec.slots.len());
    let mut infos = Vec::with_capacity(spec.slots.len());
    for name in &spec.slots {
        match lookup(name) {
            Some(p) => {
                values.push(if normalized {
                    p.info.to_normalized(p.value)
                } else {
                    p.value * trim_scale.unwrap_or(1.0)
                });
                let info = trim_scale.map_or_else(
                    || SlotInfo::from_param(&p.info, normalized),
                    |scale| {
                        SlotInfo::new(0.0, scale, p.info.default * scale).format(|ms| {
                            komponist_ui::widgets::number_field::format_number(ms, 0) + " ms"
                        })
                    },
                );
                let driven = card
                    .macros
                    .get(&p.info.id)
                    .is_some_and(|control| control.driver.is_some());
                infos.push(
                    if driven || card.macro_mapping || fractional_trim && length.is_none() {
                        info.locked()
                    } else {
                        info
                    },
                );
            }
            None => {
                let v = if asset_slot.is_some() && name == "end" {
                    full_end * 1000.0
                } else {
                    fallback(name)
                };
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
    let graph_id = slot(cu, cell, "graph");
    let marker_drag_id = graph_id.with("marker-drag");
    let snapshot = signals.and_then(|signals| signals.markers.as_ref());
    let slice_controls = native_graph.filter(|graph| graph.waveform_slice_controls.is_some());
    if has_waveform_toolbar && let Some(status) = snapshot.and_then(|snapshot| snapshot.preparation)
    {
        asset_waveform::preparation(cu, card, cell, full_rect, status);
    }
    let markers = asset
        .and(snapshot)
        .filter(|snapshot| slice_controls.is_none() || snapshot.slice_options.is_some());
    let trim_position = |name: &str, fallback| {
        spec.slot(name)
            .map_or(fallback, |index| values[index] / 1000.0)
    };
    let trim = trim_position("start", 0.0)..trim_position("end", full_end);
    let mut slices = slice_controls.zip(snapshot).and_then(|(graph, snapshot)| {
        let note = graph.audition_note?;
        let root = card.params.iter().find(|param| param.wire == note)?.value;
        waveform_slices::SliceView::new(
            cu,
            graph_id.with("slices"),
            snapshot,
            trim.clone(),
            root as u8,
        )
    });
    let editable_markers = markers.filter(|markers| markers.editable && !card.macro_mapping);
    let first_marker_slot = spec.slots.len();
    let first_marker_handle = spec.handles.len();
    if let Some(markers) = markers {
        let rate = asset
            .and_then(|asset| asset.peaks.as_ref())
            .map_or(48_000, |peaks| peaks.sample_rate());
        let gap = 1.0 / f64::from(rate);
        let preview = editable_markers.and_then(|markers| {
            cu.data(|data| data.get_temp::<asset_waveform::MarkerDrag>(marker_drag_id))
                .filter(|drag| drag.revision == markers.revision)
        });
        if editable_markers.is_some()
            && (slices.is_some() || cu.input(|input| input.modifiers.shift))
        {
            spec.on_double_click = AreaAction::NewPoint;
        }
        spec.deletable = editable_markers.is_some();
        for (index, &seconds) in markers.positions.iter().enumerate() {
            let slot = values.len();
            let visible = spec.x.min..=spec.x.max;
            let position = preview
                .filter(|drag| usize::from(drag.index) == index)
                .map_or(seconds, |drag| drag.seconds);
            let min = index
                .checked_sub(1)
                .map_or(gap, |i| markers.positions[i] + gap);
            let max = markers
                .positions
                .get(index + 1)
                .map_or(full_end, |time| time - gap);
            values.push(position);
            let info = SlotInfo::new(min.min(seconds), max.max(seconds), seconds).format(|time| {
                komponist_ui::widgets::number_field::format_number(time * 1000.0, 0) + " ms"
            });
            infos.push(if editable_markers.is_some() {
                info
            } else {
                info.locked()
            });
            spec.slots.push(format!("marker-{index}"));
            let name = slices.as_ref().map_or_else(
                || t.devices_sampler_slice(index + 1),
                |view| view.marker_name(t, index),
            );
            spec.handles.push(
                Handle::dot(name)
                    .look(HandleLook::Marker(MarkerHead::Down))
                    .x(Bind::axis(slot, t.ui_graph_position()))
                    .visible_if(Pred::new(move |values| visible.contains(&values[slot]))),
            );
        }
        if let Some(slices) = &slices {
            slices.add_layers(&mut spec, &mut values, &mut infos, first_marker_slot);
        }
    }
    if slice_controls.is_some()
        && slices.is_none()
        && asset.is_some()
        && let Some(defaults) = snapshot.and_then(|snapshot| snapshot.default_options)
    {
        waveform_slices::playback_fades(&mut spec, &mut values, &mut infos, defaults);
    }
    // The live spectrum; empty while hidden, bypassed or switched off.
    let spectrum: &[f32] = analysis.map_or(&[], |a| &a.spectrum);
    // An asset waveform uses the linked file's peaks, independently of the device's live signal.
    let asset_data = asset
        .zip(viewport.as_ref())
        .map_or_else(Vec::new, |(source, view)| {
            source.amplitudes(view, rect.width().ceil() as usize)
        });
    let peaks: &[f32] = if asset_slot.is_some() {
        &asset_data
    } else {
        spectrum
    };
    let mut playhead = signals
        .and_then(|signals| signals.playhead)
        .zip(length)
        .map(|(h, len)| f64::from(h) * len);
    // The compressor (WP-93): its live level on the transfer curve, and the history of the input
    // level and the reduction, with „now“ at the right edge.
    let dynamics = signals.map_or(card.dynamics.as_ref(), |signals| signals.dynamics.as_ref());
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
        .then_some((t.devices_detector(), t.devices_detector_external()));
    let look = if card.bypass || card.macro_mapping {
        Look::Bypass
    } else {
        Look::Normal
    };
    let id = markers.map_or(graph_id, |markers| graph_id.with(markers.revision));
    if let Some(slices) = &slices {
        slices.select_handle(cu, id, first_marker_handle);
    }
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
                .description(description)
                .playhead(playhead)
                .id(id)
                .show(gu)
        })
        .inner;
    if let Some(slot) = asset_slot
        && shown.events.contains(&GraphEvent::ResetZoom)
    {
        asset_waveform::reset(cu, card, cell, slot);
    }
    let request = editable_markers.and_then(|markers| {
        asset_waveform::marker_request(
            cu,
            marker_drag_id,
            markers,
            first_marker_slot,
            first_marker_handle,
            &shown,
        )
    });
    if let Some(slot) = asset_slot {
        let request = if let Some(slices) = &mut slices {
            slices.observe(cu, &shown, first_marker_slot);
            slices.show(
                cu,
                card,
                cell,
                full_rect,
                slot,
                viewport.as_ref(),
                playhead,
                request,
            )
        } else {
            request
        };
        if let Some(request) = request {
            out.push(Action::Plugin(PluginAction::EditWaveformMarkers {
                device: card.device,
                slot,
                request,
            }));
        }
    }
    if card.macro_mapping {
        return true;
    }
    if let Some((slot, note)) = asset_slot.zip(native_graph.and_then(|graph| graph.audition_note))
        && asset.is_some()
        && !card.bypass
        && asset_waveform::preview_enabled(cu, card, cell, slot)
        && cu.input(|input| {
            !input.modifiers.shift && input.pointer.button_pressed(egui::PointerButton::Primary)
        })
        && (shown.response.is_pointer_button_down_on()
            || shown.response.clicked_by(egui::PointerButton::Primary))
        && let Some(param) = card.params.iter().find(|param| param.wire == note)
        && param.value.is_finite()
    {
        let preview = if native_graph.is_some_and(|graph| graph.waveform_markers) {
            snapshot.and_then(|markers| {
                if markers.editable || markers.slice_options.is_some() {
                    let position = |name: &str, fallback| {
                        spec.slots
                            .iter()
                            .position(|slot| slot == name)
                            .map_or(fallback, |index| values[index] / 1000.0)
                    };
                    let trim = position("start", 0.0)..position("end", full_end);
                    shown.pointer_x.filter(|time| trim.contains(time)).map(Some)
                } else {
                    Some(None)
                }
            })
        } else {
            Some(None)
        };
        if let Some(slice_time) = preview {
            out.push(Action::Plugin(PluginAction::PreviewWaveform {
                device: card.device,
                slot,
                key: param.value.round().clamp(0.0, 127.0) as u8,
                slice_time,
            }));
        }
    }
    if let Some(slot) = asset_slot
        && let Some(path) = asset_waveform::dropped_file(cu, card, full_rect, slot)
    {
        out.push(Action::Plugin(PluginAction::SetPluginAsset {
            device: card.device,
            slot,
            path: Some(path),
        }));
    }
    let changed_params = shown.changed.iter().any(|(i, _)| *i < first_marker_slot);
    // One undo step per drag of a handle (or burst of wheel notches); the first parameter the
    // handle edits names it.
    let first = shown.selected_slot.or(shown.changed.first().map(|c| c.0));
    let first_wire = first
        .and_then(|i| spec.slots.get(i))
        .and_then(|s| lookup(s))
        .map_or(u32::MAX, |p| p.wire);
    let parameter_touch = first_wire != u32::MAX || markers.is_none();
    let touch = Touch {
        drag_started: parameter_touch && shown.touch.drag_started,
        dragged: parameter_touch && shown.touch.dragging,
        drag_stopped: parameter_touch && shown.touch.drag_stopped,
        wheel: parameter_touch && shown.touch.wheel,
        hovered: shown.response.hovered(),
    };
    gesture_touch(
        cu.ctx(),
        shown.response.id,
        touch,
        changed_params,
        || begin_param(card, first_wire),
        out,
        |out| {
            for (i, v) in &shown.changed {
                if let Some(p) = lookup(&spec.slots[*i]) {
                    set(
                        out,
                        card,
                        p,
                        if normalized {
                            p.info.to_plain(*v)
                        } else {
                            *v / trim_scale.unwrap_or(1.0)
                        },
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
                label: "undo-add-band".to_owned(),
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
    let t = komponist_ui::lang(cu.ctx());
    let ppp = cu.pixels_per_point();
    px::fill(cu.painter(), rect, radius::M, color32(color::SURFACE_1));
    let kind = model::widget_name(t, cell.widget);
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

    #[test]
    fn bundled_hexa_carriers_follow_the_graph_after_an_algorithm_edit() {
        let mut card = card();
        card.bundled_editor = true;
        let patch = komponist_hexa::preset_patch(1).unwrap();
        card.routing = Some(komponist_hexa::algorithms::routing(&patch));
        let info = komponist_hexa::Macro::Algorithm.param_info();
        card.params.push(Param {
            wire: 110,
            info,
            value: 7.0,
            text: String::new(),
        });
        // E-Piano initially has operator 5 as a carrier. Fixed algorithm 7 uses 1 and 3.
        let cell = Cell::knob(0, 0, ParamId::new("op5_level")).carrier(ParamId::new("op5_output"));
        assert!(!carrier(&card, &cell));
        card.params.last_mut().unwrap().value = 0.0;
        assert!(carrier(&card, &cell));
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
            native_layout: None,
            display: None,
            asset_slots: Vec::new(),
            asset_waveforms: Default::default(),
            asset_names: Default::default(),
            pad_activity: None,
            bundled_editor: false,
            device: DeviceId(7),
            automation_owner: None,
            automation: Default::default(),
            macros: Default::default(),
            macro_mapping: false,
            macro_mappings: 0,
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
            user_presets: false,
            preset: None,
            preset_name: None,
            preset_changed: false,
            has_sidechain: false,
            sidechain: None,
            width: 244.0,
            late: None,
            levels: None,
            native_meter_levels: Default::default(),
            analysis: None,
            dynamics: None,
            graph_signals: Default::default(),
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
    fn authored_graph_and_canvas_use_the_layout_locale() {
        use komponist_ipc::device_canvas::DisplayList;
        use komponist_ipc::native_layout::NativeLayout;
        use std::sync::Arc;

        let layout = Arc::new(
            NativeLayout::parse(include_bytes!(
                "../../../../../docs/clap-device-layout/example-v2.json"
            ))
            .unwrap(),
        );
        let display = Arc::new(
            DisplayList::decode(
                br#"{
            "frames":[{"key":"scope","primitives":[
                {"kind":"line","from":[0,1],"to":[1,0],"tone":"accent"}
            ]}]
        }"#,
            )
            .unwrap(),
        );
        for (lang, graph, canvas) in [
            (Lang::De, "Hüllkurve", "Signalanzeige"),
            (Lang::En, "Envelope", "Signal display"),
        ] {
            let mut card = card();
            card.layout = model::native_card_layout(&layout, lang);
            card.native_layout = Some(Arc::clone(&layout));
            card.display = Some(Arc::clone(&display));
            let mut h = komponist_ui::snapshot::builder(1.0)
                .with_size(egui::vec2(760.0, 240.0))
                .build_ui_state(
                    move |ui, (card, out): &mut (Card, Vec<Action>)| {
                        komponist_ui::set_lang(ui.ctx(), lang);
                        let palette = TrackPalette::new(150);
                        let page = card.current_page().unwrap();
                        for cell in &page.cells {
                            let rect = crate::views::devices::layout::cell_rect(cell)
                                .translate(ui.min_rect().min.to_vec2());
                            draw_cell(ui, out, card, &palette, cell, rect);
                        }
                    },
                    (card, Vec::new()),
                );
            h.run();
            h.get_by_role_and_label(Role::Group, graph);
            let painted_text = |label: &str| {
                let label = label.to_uppercase();
                h.output().shapes.iter().any(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == label)
                })
            };
            assert!(painted_text("Equalizer"));
            assert!(!painted_text(graph));
            h.state_mut().0.page = 1;
            h.run();
            h.get_by_role_and_label(Role::Group, canvas);
            assert!(set_params(&h).is_empty());
        }
    }

    #[test]
    fn a_native_pad_bank_selects_and_auditions_its_authored_notes() {
        use komponist_ipc::device_assets::Slot;
        use komponist_ipc::native_layout::{CAP_PAD_BANK, NativeLayout};
        use serde_json::json;
        use std::path::{Path, PathBuf};
        use std::sync::Arc;

        #[derive(Debug)]
        struct Dropped(PathBuf);

        impl egui::DroppedFile for Dropped {
            fn path(&self) -> &Path {
                &self.0
            }

            fn bytes(&self) -> Result<Vec<u8>, String> {
                panic!("dropping a pad file should pass only its path")
            }
        }

        let page = |index: u8, names, slot| {
            json!({"key": format!("pad-{index}"), "labels": names, "columns": 6, "cells": [
                {"widget": "pad_grid", "col": 0, "row": 0, "cols": 2, "rows": 1},
                {"widget": "graph", "col": 2, "row": 0, "cols": 4, "rows": 1, "graph": {
                    "kind": "waveform", "name": names, "asset_slot": slot,
                    "description": {"de": "Wellenform", "en": "Waveform"}
                }}
            ]})
        };
        let document = json!({
            "schema_version": 2, "default_locale": "en", "locales": ["en", "de"],
            "required_capabilities": [CAP_PAD_BANK],
            "pad_bank": {"pads": [{"note": 36, "asset_slot": 7}, {"note": 43, "asset_slot": 12}]},
            "pages": [
                page(1, json!({"de": "Links", "en": "Left"}), 7),
                page(2, json!({"de": "Rechts", "en": "Right"}), 12),
            ]
        });
        let layout =
            Arc::new(NativeLayout::parse(&serde_json::to_vec(&document).unwrap()).unwrap());
        for (lang, first, second, empty) in [
            (Lang::De, "Links", "Rechts", "kein Sample"),
            (Lang::En, "Left", "Right", "no sample"),
        ] {
            let mut card = card();
            card.layout = model::native_card_layout(&layout, lang);
            card.native_layout = Some(Arc::clone(&layout));
            card.asset_names.insert(7, "Kick".into());
            card.asset_slots.push((
                Slot {
                    id: 12,
                    role: "sample".into(),
                    label: second.into(),
                    kind: AssetKind::AudioSample,
                    status: AssetStatus::Empty,
                },
                false,
            ));
            let device = card.device;
            let mut h = komponist_ui::snapshot::builder(1.0)
                .with_size(egui::vec2(440.0, 240.0))
                .build_ui_state(
                    move |ui, (card, out): &mut (Card, Vec<Action>)| {
                        komponist_ui::set_lang(ui.ctx(), lang);
                        card.page = selected_pad(ui.ctx(), card.device);
                        let palette = TrackPalette::new(150);
                        let origin = ui.min_rect().min;
                        for cell in &card.current_page().unwrap().cells {
                            let rect = crate::views::devices::layout::cell_rect(cell)
                                .translate(origin.to_vec2());
                            draw_cell(ui, out, card, &palette, cell, rect);
                        }
                    },
                    (card, Vec::new()),
                );
            h.run();
            h.get_by_role_and_label(Role::Button, "Pad Kick");
            h.get_by_role_and_label(Role::Group, &format!("{first} · Kick"));
            let button = format!("Pad {second}");
            h.get_by_role_and_label(Role::Button, &button).click();
            h.run();
            assert_eq!(h.state().0.page, 1);
            h.get_by_role_and_label(Role::Group, &format!("{second} · {empty}"));
            assert!(matches!(
                h.state().1.as_slice(),
                [Action::Plugin(PluginAction::AuditionNote { device: id, key: 43 })] if *id == device
            ));

            let target = h
                .get_by_role_and_label(Role::Button, &button)
                .rect()
                .center();
            h.state_mut().1.clear();
            h.hover_at(target);
            h.step();
            let path = PathBuf::from("Snare.wav");
            h.input_mut()
                .dropped_files
                .push(Arc::new(Dropped(path.clone())));
            h.run();
            assert!(matches!(
                h.state().1.as_slice(),
                [Action::Plugin(PluginAction::SetPluginAsset { device: id, slot: 12, path: Some(file) })]
                    if *id == device && file == &path
            ));
        }
    }

    #[test]
    fn authored_gain_reduction_meters_use_independent_accessible_holds() {
        use crate::meters::DeviceLevels;
        use komponist_ipc::native_layout::NativeLayout;
        use std::sync::Arc;

        let layout = Arc::new(
            NativeLayout::parse(
                br#"{
            "schema_version":2,"default_locale":"en","locales":["en"],
            "pages":[{"key":"main","labels":{"en":"Main"},"columns":2,"cells":[
                {"widget":"meter_gr","row":0,"col":0,"rows":3,"cols":1,"reduction_tap":"first"},
                {"widget":"meter_gr","row":0,"col":1,"rows":3,"cols":1,"reduction_tap":"second"}
            ]}]
        }"#,
            )
            .unwrap(),
        );
        for lang in [Lang::De, Lang::En] {
            let mut card = card();
            card.layout = model::native_card_layout(&layout, lang);
            card.native_layout = Some(Arc::clone(&layout));
            card.native_meter_levels = [
                (
                    0,
                    DeviceLevels {
                        bars: [0.0, 0.25, 0.0],
                        reduction_db: 6.0,
                    },
                ),
                (
                    1,
                    DeviceLevels {
                        bars: [0.0, 0.50, 0.0],
                        reduction_db: 12.0,
                    },
                ),
            ]
            .into();
            let mut h = komponist_ui::snapshot::builder(1.0)
                .with_size(egui::vec2(260.0, 240.0))
                .build_ui_state(
                    move |ui, (card, out): &mut (Card, Vec<Action>)| {
                        komponist_ui::set_lang(ui.ctx(), lang);
                        let palette = TrackPalette::new(150);
                        for cell in &card.current_page().unwrap().cells {
                            let rect = crate::views::devices::layout::cell_rect(cell)
                                .translate(ui.min_rect().min.to_vec2());
                            draw_cell(ui, out, card, &palette, cell, rect);
                        }
                    },
                    (card, Vec::new()),
                );
            h.run();
            let first = lang.ui_meter_gr_value("−6.0");
            let second = lang.ui_meter_gr_value("−12.0");
            {
                let mut nodes = h.get_all_by_role_and_label(Role::Meter, lang.ui_meter_gr_name());
                let first_node = nodes.next().unwrap();
                let second_node = nodes.next().unwrap();
                assert!(nodes.next().is_none());
                assert_eq!(first_node.accesskit_node().value(), Some(first));
                assert_eq!(second_node.accesskit_node().value(), Some(second.clone()));
                assert_eq!(
                    first_node.accesskit_node().description().as_deref(),
                    Some(lang.ui_meter_gr_hint())
                );
                first_node.click_accesskit();
            }
            h.state_mut().0.native_meter_levels.clear();
            h.run();
            h.run();
            let mut nodes = h.get_all_by_role_and_label(Role::Meter, lang.ui_meter_gr_name());
            assert_eq!(
                nodes.next().unwrap().accesskit_node().value(),
                Some(lang.ui_meter_gr_value("0.0"))
            );
            assert_eq!(nodes.next().unwrap().accesskit_node().value(), Some(second));
            assert!(h.state().1.is_empty());
        }
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

    #[test]
    fn number_entry_uses_the_displayed_units() {
        let layout = komponist_ipc::native_layout::NativeLayout::parse(
            br#"{
            "schema_version":1,"default_locale":"en","locales":["en"],
            "pages":[{"key":"main","labels":{"en":"Main"},"columns":4,"cells":[
                {"widget":"number","row":0,"col":0,"rows":1,"cols":1,"half":"top","param_id":1}
            ]}]
        }"#,
        )
        .unwrap();
        let layout = std::sync::Arc::new(layout);
        for (display, input, expected) in [
            ("50 %", "25", 0.25),
            ("50%", "75%", 0.75),
            ("0.500", "0.25", 0.25),
        ] {
            let mut h = harness();
            let card = &mut h.state_mut().0;
            card.native_layout = Some(layout.clone());
            let p = &mut card.params[0];
            p.info.kind = ParamKind::lin(0.0, 1.0);
            p.info.unit = Unit::Percent;
            p.value = 0.5;
            p.text = display.into();
            h.run();
            h.get_by_role_and_label(Role::SpinButton, "Stimmen").click();
            h.run();
            h.key_press(egui::Key::Enter);
            h.run();
            h.event(egui::Event::Text(input.into()));
            h.run();
            h.key_press(egui::Key::Enter);
            h.run();
            assert_eq!(set_params(&h), vec![(1, expected)], "{display}");
        }
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
        localized_graph_harness(card, size, Lang::De)
    }

    fn localized_graph_harness(
        card: Card,
        size: egui::Vec2,
        lang: Lang,
    ) -> egui_kittest::Harness<'static, (Card, Vec<Action>)> {
        let mut h = komponist_ui::snapshot::builder(1.0)
            .with_size(size)
            // Two clicks must fall within egui's double-click time.
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                move |ui, (card, out): &mut (Card, Vec<Action>)| {
                    komponist_ui::set_lang(ui.ctx(), lang);
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
    fn an_authored_xy_panel_keeps_physical_axes_and_normalized_actions() {
        use komponist_ipc::control::ParamInfo as WireInfo;
        use komponist_ipc::native_layout::NativeLayout;
        use komponist_model::params::ValueTexts;
        use serde_json::json;
        use std::sync::Arc;

        let label =
            |get: fn(Lang) -> &'static str| json!({"de": get(Lang::De), "en": get(Lang::En)});
        let native = Arc::new(NativeLayout::parse(&serde_json::to_vec(&json!({
            "schema_version": 2, "default_locale": "en", "locales": ["en", "de"],
            "required_capabilities": ["parameter-display"],
            "parameter_display": {
                "42": {"min": 1, "max": 2000, "default": 375, "scale": "logarithmic", "unit": "milliseconds"},
                "43": {"min": 0, "max": 0.95, "default": 0.42, "scale": "linear", "unit": "percent"}
            },
            "pages": [{"key": "main", "labels": {"en": "Main"}, "columns": 4, "cells": [
                {"widget": "graph", "col": 0, "row": 0, "cols": 2, "rows": 2, "graph": {
                    "kind": "xy", "hide_title": true, "name": label(Lang::ui_graph_xy),
                    "description": label(Lang::param_feedback), "slots": {"x": 42, "y": 43}
                }},
                {"widget": "knob", "col": 2, "row": 0, "cols": 1, "rows": 1,
                 "param_id": 42, "labels": label(Lang::param_time)},
                {"widget": "knob", "col": 3, "row": 0, "cols": 1, "rows": 1,
                 "param_id": 43, "labels": label(Lang::param_feedback)}
            ]}]
        })).unwrap()).unwrap());

        for lang in Lang::ALL {
            let mut legacy = card();
            legacy.bundled_editor = true;
            legacy.params = [
                (
                    42,
                    lang.param_time(),
                    ParamKind::scaled(1.0, 2000.0, Scale::Log),
                    375.0,
                    Unit::Ms,
                ),
                (
                    43,
                    lang.param_feedback(),
                    ParamKind::lin(0.0, 0.95),
                    0.42,
                    Unit::Percent,
                ),
            ]
            .into_iter()
            .map(|(wire, name, kind, value, unit)| {
                let mut p = param(wire, name, kind, value, value);
                p.info = p.info.unit(unit).texts(ValueTexts {
                    on: lang.value_on(),
                    off: lang.value_off(),
                    percent: lang.value_percent(),
                });
                p.text = p.info.format(value);
                p
            })
            .collect();
            legacy.layout = DeviceLayout::single(Page {
                name: String::new(),
                cols: 4,
                cells: vec![
                    Cell::graph(
                        0,
                        0,
                        2,
                        2,
                        GraphBinding::new(GraphConfig::Xy)
                            .bind("x", "p42")
                            .bind("y", "p43"),
                    ),
                    Cell::knob(2, 0, ParamId::new("p42")),
                    Cell::knob(3, 0, ParamId::new("p43")),
                ],
                dividers: Vec::new(),
            });
            let mut authored = legacy.clone();
            authored.bundled_editor = false;
            authored.layout = model::native_card_layout(&native, lang);
            authored.native_layout = Some(Arc::clone(&native));
            let wires: Vec<_> = legacy
                .params
                .iter()
                .map(|p| WireInfo {
                    id: p.wire,
                    name: "Wire label".into(),
                    module: String::new(),
                    min: 0.0,
                    max: 1.0,
                    default: f64::from(p.info.default_normalized() as f32),
                    step_count: 0,
                    flags: komponist_ipc::control::param_flags::AUTOMATABLE,
                })
                .collect();
            assert!(native.validate_params(&wires).is_ok());
            authored.params = wires
                .iter()
                .map(|wire| model::native_param(lang, &native, wire, None))
                .collect();
            let time = &authored.params[0];
            assert_eq!(time.project_id(false), ParamId::plugin(42));
            assert!((time.value_at_normalized(false, 0.5) - 2000.0_f64.sqrt()).abs() < 1e-9);
            assert_eq!(time.text_at(time.value), "375 ms");
            assert_eq!(
                authored.params[1].text,
                format!("42{}", lang.value_percent())
            );

            let test = |card| localized_graph_harness(card, egui::vec2(310.0, 240.0), lang);
            let mut old = test(legacy);
            let mut new = test(authored);
            let legacy_image = old.render().unwrap();
            let native_image = new.render().unwrap();
            assert!(
                legacy_image == native_image,
                "{} XY rendering differs",
                lang.code()
            );
            let axis = format!("{} {}", lang.ui_graph_xy(), lang.param_time());
            for harness in [&mut old, &mut new] {
                harness.get_by_role_and_label(Role::Group, lang.ui_graph_xy());
                let slider = harness.get_by_role_and_label(Role::Slider, &axis);
                assert_eq!(slider.value().as_deref(), Some("375 ms"));
                slider.increment_accesskit();
                harness.run();
            }
            let legacy_actions = set_params(&old);
            assert!(!legacy_actions.is_empty());
            assert_eq!(legacy_actions, set_params(&new));
        }
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

    fn authored_asset_waveform_card(fractional: bool) -> Card {
        use komponist_ipc::device_assets::{AssetKind, AssetStatus, Slot};
        use komponist_ipc::native_layout::{CAP_SOURCE_FRACTION_TRIM, NativeLayout};
        use std::sync::Arc;

        let mut layout: serde_json::Value = serde_json::from_slice(
            br#"{
            "schema_version":2,"default_locale":"de","locales":["de","en"],
            "pages":[{"key":"sample","labels":{"de":"Sample"},"columns":4,"cells":[
              {"widget":"graph","row":0,"col":0,"rows":2,"cols":4,"graph":{
                "kind":"waveform","name":{"de":"Sample-Quelle","en":"Sample source"},
                "description":{"de":"Verkn\u00fcpfte Audiodatei.","en":"Linked audio file."},
                "asset_slot":5,"slots":{"end":42}
              }}
            ]}]
        }"#,
        )
        .unwrap();
        if fractional {
            layout["pages"][0]["cells"][0]["graph"]["trim_unit"] = "source_fraction".into();
            layout["required_capabilities"] = serde_json::json!([CAP_SOURCE_FRACTION_TRIM]);
        }
        let layout = Arc::new(NativeLayout::parse(&serde_json::to_vec(&layout).unwrap()).unwrap());
        let mut card = card();
        card.layout = model::native_card_layout(&layout, Lang::De);
        card.native_layout = Some(layout);
        let max = if fractional { 1.0 } else { 500.0 };
        let mut end = param(42, "Ende", ParamKind::lin(0.0, max), max, max);
        end.info.id = ParamId::plugin(42);
        card.params = vec![end];
        card.asset_slots = vec![(
            Slot {
                id: 5,
                role: "sample".into(),
                label: "Sample".into(),
                kind: AssetKind::AudioSample,
                status: AssetStatus::Ready,
            },
            true,
        )];
        card
    }

    #[test]
    fn authored_slice_controls_name_the_notes_and_request_transient_detection() {
        use komponist_ipc::native_layout::{CAP_SOURCE_FRACTION_TRIM, NativeLayout};
        use komponist_ipc::waveform_markers::{Edit, Request, SliceOptions, Snapshot};
        use serde_json::json;
        use std::sync::Arc;

        let native = json!({
            "schema_version": 2, "default_locale":"de", "locales":["de"],
            "required_capabilities": [CAP_SOURCE_FRACTION_TRIM],
            "pages": [{"key":"sample", "labels":{"de":"Sample"}, "columns": 14, "cells": [
                {"widget":"graph", "row":0, "col":0, "rows":2, "cols":10, "graph":{
                    "kind":"waveform", "name":{"de":"Wellenform"}, "description":{"de":"Sample"},
                    "asset_slot":5, "slots":{"start":41, "end":42}, "audition_note":43,
                    "trim_unit":"source_fraction", "waveform_toolbar":true, "waveform_markers":true,
                    "waveform_slice_controls":[44,45,46],
                }},
                {"widget":"number","row":2,"col":0,"rows":1,"cols":1,"half":"top","param_id":41},
                {"widget":"number","row":2,"col":2,"rows":1,"cols":2,"half":"top","param_id":45},
                {"widget":"number","row":2,"col":4,"rows":1,"cols":2,"half":"top","param_id":46},
                {"widget":"toggle_button","row":0,"col":10,"rows":1,"cols":2,"half":"bottom","param_id":44},
            ]}]
        });
        let layout = Arc::new(NativeLayout::parse(&serde_json::to_vec(&native).unwrap()).unwrap());
        let mut card = authored_asset_waveform_card(true);
        card.layout = model::native_card_layout(&layout, Lang::De);
        card.native_layout = Some(layout);
        for (id, name, value) in [
            (41, "Anfang", 0.0_f64),
            (43, "Grundton", 60.0),
            (44, "Sampler-Richtung", 0.0),
            (45, "Sampler-Fade 1", 0.0),
            (46, "Sampler-Fade 2", 0.0),
        ] {
            let mut p = param(id, name, ParamKind::lin(0.0, value.max(1.0)), 0.0, value);
            p.info.id = ParamId::plugin(id);
            card.params.push(p);
        }
        card.asset_waveforms.insert(
            5,
            asset_waveform::Waveform {
                duration: 1.0,
                file: komponist_audiofile::FileId {
                    path: "source.wav".into(),
                    len: 0,
                    mtime_ns: None,
                },
                peaks: None,
            },
        );
        card.graph_signals.insert(
            (0, 0),
            model::GraphSignals {
                markers: Some(Snapshot {
                    revision: 7,
                    editable: true,
                    positions: vec![0.25, 0.5],
                    slice_options: Some(vec![SliceOptions::default(); 3]),
                    default_options: Some(SliceOptions {
                        reverse: Some(false),
                        fade_in_ms: Some(25.0),
                        fade_out_ms: Some(125.0),
                        ..Default::default()
                    }),
                    preparation: None,
                }),
                ..Default::default()
            },
        );

        let mut harness = graph_harness_sized(card, egui::vec2(900.0, 240.0));
        let lang = Lang::De;
        let marker = format!(
            "{} {}",
            komponist_model::format::note_name(61),
            lang.ui_graph_position()
        );
        harness.get_by_role_and_label(Role::Slider, marker.as_str());
        harness.get_by_role_and_label(Role::Button, lang.devices_sampler_fit());
        harness
            .get_by_role_and_label(Role::Button, lang.devices_sampler_detect())
            .click();
        harness.run();
        let requests = harness
            .state()
            .1
            .iter()
            .filter_map(|action| match action {
                Action::Plugin(PluginAction::EditWaveformMarkers { slot, request, .. }) => {
                    Some((*slot, *request))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            requests,
            [(
                5,
                Request {
                    revision: 7,
                    operation: Edit::DetectSlices
                }
            )]
        );
    }

    #[test]
    fn pressing_an_authored_waveform_uses_its_midi_note_binding() {
        let mut card = authored_asset_waveform_card(false);
        let layout = std::sync::Arc::get_mut(card.native_layout.as_mut().unwrap()).unwrap();
        layout.pages[0].cells[0]
            .graph
            .as_mut()
            .unwrap()
            .audition_note = Some(43);
        card.params.push(param(
            43,
            "Grundton",
            ParamKind::Int { min: 0, max: 127 },
            60.0,
            64.0,
        ));
        card.asset_waveforms.insert(
            5,
            asset_waveform::Waveform {
                duration: 0.25,
                file: komponist_audiofile::FileId {
                    path: "source.wav".into(),
                    len: 0,
                    mtime_ns: None,
                },
                peaks: None,
            },
        );
        let mut h = graph_harness(card);
        let pos = h
            .get_by_role_and_label(Role::Group, "Sample-Quelle")
            .rect()
            .center();
        h.hover_at(pos);
        h.step();
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert!(h.state().1.iter().any(|action| matches!(
            action,
            Action::Plugin(PluginAction::PreviewWaveform {
                slot: 5,
                key: 64,
                slice_time: None,
                ..
            })
        )));
    }

    #[test]
    fn an_authored_waveform_names_only_visible_markers() {
        let mut card = authored_asset_waveform_card(false);
        card.asset_waveforms.insert(
            5,
            asset_waveform::Waveform {
                duration: 0.25,
                file: komponist_audiofile::FileId {
                    path: "source.wav".into(),
                    len: 0,
                    mtime_ns: None,
                },
                peaks: None,
            },
        );
        card.graph_signals.insert(
            (0, 0),
            model::GraphSignals {
                markers: Some(komponist_ipc::waveform_markers::Snapshot {
                    revision: 4,
                    editable: true,
                    positions: vec![0.125, 0.5],
                    slice_options: None,
                    default_options: None,
                    preparation: None,
                }),
                ..Default::default()
            },
        );
        let mut h = graph_harness(card);
        h.get_by_role_and_label(Role::Slider, "Schnitt 1 Position")
            .decrement_accesskit();
        h.run();
        assert!(
            h.query_by_role_and_label(Role::Slider, "Schnitt 2 Position")
                .is_none()
        );
        assert!(h.state().1.iter().any(|action| matches!(
            action,
            Action::Plugin(PluginAction::EditWaveformMarkers {
                slot: 5,
                request: komponist_ipc::waveform_markers::Request {
                    revision: 4,
                    operation: komponist_ipc::waveform_markers::Edit::Move { index: 0, seconds },
                },
                ..
            }) if *seconds > 0.0 && *seconds < 0.125
        )));
    }

    #[test]
    fn asset_waveform_reads_its_link_and_edits_the_plugin_range() {
        use super::super::viewport::Viewport;
        use komponist_ipc::device_assets::AssetKind;
        use komponist_model::plugin_assets::AssetLink;
        use komponist_model::{Device, DeviceRole, PluginFormat, PluginRef};
        use std::path::Path;
        use std::time::{Duration, Instant};

        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("source.wav");
        let mut wav = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..2_000 {
            wav.write_sample(0_i16).unwrap();
            wav.write_sample(if i < 1_000 { 24_000_i16 } else { 0 })
                .unwrap();
        }
        wav.finalize().unwrap();
        let mut card = authored_asset_waveform_card(false);
        card.asset_slots[0].0.kind = AssetKind::File;
        let mut device = Device {
            id: card.device,
            role: DeviceRole::Instrument,
            plugin: PluginRef {
                format: PluginFormat::Clap,
                id: "example.test.waveform".into(),
                name: "Example waveform".into(),
                vendor: String::new(),
                version: String::new(),
                module: Box::default(),
            },
            bypass: false,
            // Simulate a moved project with a stale absolute path.
            asset_links: [(
                5,
                AssetLink {
                    abs: folder.path().join("old").join("source.wav"),
                    rel: Path::new("source.wav").to_owned(),
                    display_name: None,
                },
            )]
            .into(),
            state: None,
            parameter_bases: Default::default(),
            sidechain: None,
        };
        let egui = egui::Context::default();
        let mut cache = super::super::asset_waveform::Cache::default();
        let dir = Some(folder.path());
        assert!(cache.for_card(&egui, &card, &device, dir).is_empty());
        card.asset_slots[0].0.kind = AssetKind::AudioSample;
        let deadline = Instant::now() + Duration::from_secs(5);
        card.asset_waveforms = loop {
            let sources = cache.for_card(&egui, &card, &device, dir);
            if sources.get(&5).is_some_and(|source| source.peaks.is_some()) {
                break sources;
            }
            assert!(Instant::now() < deadline, "waveform peak worker timed out");
            std::thread::sleep(Duration::from_millis(5));
        };
        let source = &card.asset_waveforms[&5];
        assert_eq!(source.duration, 0.25);
        let amplitudes = source.amplitudes(&Viewport::fit(0.25), 100);
        assert!(amplitudes[..40].iter().all(|&peak| peak > 0.7));
        assert!(amplitudes[60..].iter().all(|&peak| peak == 0.0));
        let silent = Viewport {
            start: 0.15,
            end: 0.25,
        };
        assert!(
            source
                .amplitudes(&silent, 100)
                .iter()
                .all(|&peak| peak == 0.0)
        );

        let mut h = graph_harness(card);
        h.get_by_role_and_label(Role::Group, "Sample-Quelle");
        h.get_by_role_and_label(Role::Slider, "Sample-Ende Position")
            .decrement_accesskit();
        h.run();
        let (_, end) = set_params(&h)
            .into_iter()
            .find(|(id, _)| *id == 42)
            .unwrap();
        assert!((0.0..250.0).contains(&end), "{end}");

        device.asset_links.clear();
        assert!(cache.for_card(&egui, &h.state().0, &device, dir).is_empty());
    }

    #[test]
    fn fractional_waveform_trim_sends_the_same_fraction_for_different_durations() {
        for duration in [0.25, 2.5] {
            let mut card = authored_asset_waveform_card(true);
            card.asset_waveforms.insert(
                5,
                asset_waveform::Waveform {
                    duration,
                    file: komponist_audiofile::FileId {
                        path: "source.wav".into(),
                        len: 0,
                        mtime_ns: None,
                    },
                    peaks: None,
                },
            );
            let mut h = graph_harness(card);
            h.get_by_role_and_label(Role::Slider, "Sample-Ende Position")
                .decrement_accesskit();
            h.run();
            let (_, end) = set_params(&h)
                .into_iter()
                .find(|(id, _)| *id == 42)
                .unwrap();
            assert!((end - 0.99).abs() < 1e-6, "{duration}s: {end}");
        }
    }

    #[test]
    fn dropping_a_file_on_an_asset_waveform_targets_only_its_audio_slot() {
        use std::path::{Path, PathBuf};
        use std::sync::Arc;

        #[derive(Debug)]
        struct Dropped(PathBuf);
        impl egui::DroppedFile for Dropped {
            fn path(&self) -> &Path {
                &self.0
            }

            fn bytes(&self) -> Result<Vec<u8>, String> {
                panic!("dropping a waveform file should pass only its path")
            }
        }

        let card = authored_asset_waveform_card(false);
        let device = card.device;
        let mut h = graph_harness(card);
        let path = PathBuf::from("source.wav");
        let target = h.get_by_role_and_label(Role::Group, "Sample-Quelle").rect();
        h.hover_at(target.max + egui::vec2(20.0, 20.0));
        h.step();
        h.input_mut()
            .dropped_files
            .push(Arc::new(Dropped(path.clone())));
        h.step();
        assert!(h.state().1.is_empty());

        h.hover_at(target.center());
        h.step();
        h.input_mut()
            .dropped_files
            .push(Arc::new(Dropped(path.clone())));
        h.run();
        assert!(matches!(
            h.state().1.as_slice(),
            [Action::Plugin(PluginAction::SetPluginAsset {
                device: id, slot: 5, path: Some(file)
            })] if *id == device && file == &path
        ));
    }

    #[test]
    fn asset_waveform_zoom_pan_and_fit_leave_the_parameter_alone() {
        let mut card = authored_asset_waveform_card(false);
        card.asset_waveforms.insert(
            5,
            asset_waveform::Waveform {
                duration: 0.25,
                file: komponist_audiofile::FileId {
                    path: "source.wav".into(),
                    len: 0,
                    mtime_ns: None,
                },
                peaks: None,
            },
        );
        let mut h = graph_harness(card);
        let plot = h.get_by_role_and_label(Role::Group, "Sample-Quelle").rect();
        let end_label = "Sample-Ende Position";
        h.get_by_role_and_label(Role::Slider, end_label);
        h.hover_at(plot.center());
        h.event(egui::Event::Zoom(2.0));
        h.run_steps(3);
        assert!(h.query_by_role_and_label(Role::Slider, end_label).is_none());

        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-1_000.0, 0.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        h.run_steps(30);
        h.get_by_role_and_label(Role::Slider, end_label);
        h.event(egui::Event::Zoom(2.0));
        h.run_steps(3);
        assert!(h.query_by_role_and_label(Role::Slider, end_label).is_none());

        for _ in 0..2 {
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: plot.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                h.step();
            }
        }
        h.run_steps(3);
        h.get_by_role_and_label(Role::Slider, end_label);
        assert!(set_params(&h).is_empty());
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

    #[test]
    fn waves_are_icons_by_choice_index_in_every_language() {
        use komponist_builtin::Internal;
        use komponist_i18n::Lang;
        for lang in Lang::ALL {
            let info = |key: &str| {
                Internal::Volt
                    .params_in(lang)
                    .iter()
                    .find(|i| i.id.as_str() == key)
                    .unwrap_or_else(|| panic!("volt has {key}"))
                    .clone()
            };
            let param = |key: &str| Param {
                wire: 0,
                text: String::new(),
                info: info(key),
                value: 0.0,
            };
            for key in ["osc_a_wave", "osc_b_wave", "lfo1_shape", "lfo2_shape"] {
                let p = param(key);
                let waves: Vec<bool> = (0..6).map(|i| wave_of(&p, i).is_some()).collect();
                assert_eq!(waves[..4], [true; 4], "{key} in {lang:?}");
                assert!(!waves[4], "choice 5 is not a wave");
            }
            assert!(wave_of(&param("filter_type"), 0).is_none());
        }
    }
}
