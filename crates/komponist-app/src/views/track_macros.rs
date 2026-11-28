//! The track macro card and the controls shared by the devices, mixer and inspector.

use egui::{Align, Color32, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_i18n::Lang;
use komponist_model::automation::{AutomationMode, AutomationOwner, AutomationTarget};
use komponist_model::macros::{MACRO_COUNT, TrackMacro};
use komponist_model::{Track, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, device, radius, size, space, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry, Kind};
use komponist_ui::widgets::knob::{Knob, ModArc};
use komponist_ui::widgets::number_field::{open_value_entry, parse_number, wheel_turned};
use komponist_ui::{fonts, px, state, widgets};

use crate::actions::Action as AppAction;
use crate::views::{ViewCtx, elided_galley, group_bounds};

/// A macro interaction sent to the app's dispatcher.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Action {
    Select {
        track: TrackId,
        index: u8,
    },
    ToggleMapping {
        track: TrackId,
    },
    ExitMapping,
    ToggleCollapsed {
        track: TrackId,
    },
    BeginValue {
        track: TrackId,
        index: u8,
    },
    SetValue {
        track: TrackId,
        index: u8,
        value: f32,
    },
    Rename {
        track: TrackId,
        index: u8,
        name: String,
    },
    Map {
        track: TrackId,
        index: u8,
        target: AutomationTarget,
    },
    Remove {
        track: TrackId,
        index: u8,
        target: AutomationTarget,
    },
    Clear {
        track: TrackId,
        index: u8,
    },
    Range {
        track: TrackId,
        index: u8,
        target: AutomationTarget,
        min: f32,
        max: f32,
    },
    Invert {
        track: TrackId,
        index: u8,
        target: AutomationTarget,
        invert: bool,
    },
    Bend {
        track: TrackId,
        index: u8,
        target: AutomationTarget,
        bend: f32,
    },
    ShowDriver {
        track: TrackId,
        target: AutomationTarget,
    },
    Reveal {
        track: TrackId,
        target: AutomationTarget,
    },
}

/// Width of the expanded device card (the proposal's four-column card).
const CARD_WIDTH: f32 =
    device::grid::PADDING * 2.0 + 4.0 * device::grid::CELL[0] + 3.0 * device::grid::GAP;
/// Badge square and insets from Track Macros proposal §8.
const BADGE: f32 = 12.0;
const BADGE_INSET: f32 = 1.0;
const ERROR_DOT: f32 = 6.0;

/// Name shown for an unnamed slot.
pub(super) fn name(lang: Lang, slot: &TrackMacro) -> &str {
    if slot.name.is_empty() {
        lang.macros_unused()
    } else {
        &slot.name
    }
}

/// Localized reference suitable for embedding in a sentence.
pub(crate) fn reference(lang: Lang, index: u8, slot: &TrackMacro) -> String {
    let n = u16::from(index) + 1;
    if slot.name.is_empty() {
        lang.macros_slot(n)
    } else {
        lang.macros_ref(n, &slot.name)
    }
}

/// Track macro value visible to the UI, including an active automation override.
pub(crate) fn value(ctx: &ViewCtx<'_>, track: &Track, index: u8) -> f32 {
    effective_value(ctx.project, ctx.engine, track, index)
}

pub(crate) fn effective_value(
    project: &komponist_model::Project,
    engine: &komponist_session::engine_sync::EngineSync,
    track: &Track,
    index: u8,
) -> f32 {
    let base = track.macros.0[usize::from(index)].value;
    engine
        .pending_macro_value(track.id, index, base)
        .or_else(|| {
            engine
                .telemetry()
                .macro_values(track.id)
                .map(|values| values[usize::from(index)])
        })
        .or_else(|| {
            crate::views::automation::automation_control_value(
                project,
                engine,
                AutomationOwner::Track(track.id),
                &AutomationTarget::Macro { index },
            )
        })
        .unwrap_or(base)
}

/// Color, range and driver for a mapped control.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Driver {
    pub(crate) index: u8,
    pub(crate) reference: String,
    pub(crate) range: (f32, f32),
    pub(crate) value: f32,
    pub(crate) highlighted: bool,
}

/// An automatable control on a track, with its current mapping state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Control {
    pub(crate) track: TrackId,
    pub(crate) target: AutomationTarget,
    pub(crate) ink: Color32,
    pub(crate) driver: Option<Driver>,
    mapping: Option<(u8, String)>,
    slots: [String; MACRO_COUNT],
    revealed: bool,
}

/// Host control menu option. Ordinary value options are applied by the calling widget.
#[derive(Clone, Copy)]
pub(crate) enum Pick {
    Default,
    Enter,
    Automation,
    Map(u8),
    Show,
    Remove,
}

impl Control {
    pub(crate) fn new(
        ctx: &ViewCtx<'_>,
        owner: AutomationOwner,
        target: AutomationTarget,
    ) -> Option<Self> {
        let AutomationOwner::Track(track) = owner else {
            return None;
        };
        let track = ctx.project.track(track)?;
        Some(Self::for_track(
            ctx.lang,
            &ctx.ui.track_macros,
            track,
            target,
            |index| value(ctx, track, index),
        ))
    }

    pub(crate) fn for_track(
        lang: Lang,
        view: &crate::ui_state::TrackMacrosView,
        track: &Track,
        target: AutomationTarget,
        value_at: impl Fn(u8) -> f32,
    ) -> Self {
        let driver = track.macros.driver(&target).and_then(|index| {
            let slot = &track.macros.0[usize::from(index)];
            let mapping = slot.mappings.iter().find(|m| m.target == target)?;
            Some(Driver {
                index,
                reference: reference(lang, index, slot),
                range: (mapping.min, mapping.max),
                value: mapping.value_at(value_at(index)),
                highlighted: view
                    .hovered
                    .is_some_and(|(id, slot, _)| id == track.id && slot == index),
            })
        });
        let selected = view.selected(track.id);
        let mapping = (view.mapping == Some(track.id)).then(|| {
            (
                selected,
                reference(lang, selected, &track.macros.0[usize::from(selected)]),
            )
        });
        Self {
            track: track.id,
            revealed: view
                .revealed
                .as_ref()
                .is_some_and(|(track_id, key)| *track_id == track.id && *key == target),
            target,
            ink: color32(TrackPalette::new(track.hue.0).base),
            driver,
            mapping,
            slots: std::array::from_fn(|i| {
                lang.macros_entry(i + 1, name(lang, &track.macros.0[i]))
            }),
        }
    }

    pub(crate) fn modulation(&self) -> Option<ModArc> {
        self.driver.as_ref().map(|driver| ModArc {
            low: driver.range.0 - driver.value,
            high: driver.range.1 - driver.value,
            color: self.ink,
        })
    }

    pub(crate) fn redirects(&self) -> bool {
        self.mapping.is_some() || self.driver.is_some()
    }

    pub(crate) fn candidate(&self) -> bool {
        self.mapping.is_some()
    }

    pub(crate) fn paint_ground(&self, ui: &Ui, rect: Rect) {
        if let Some((index, _)) = &self.mapping {
            px::fill(
                ui.painter(),
                rect,
                radius::S,
                color32(color::ACCENT_SURFACE),
            );
            if self
                .driver
                .as_ref()
                .is_some_and(|driver| driver.index == *index)
            {
                px::outline(ui.painter(), rect, radius::S, 0.0, color32(color::ACCENT));
            }
        }
    }

    pub(crate) fn decorate(&self, ui: &Ui, r: &Response, rect: Rect, range: Option<&str>) {
        let lang = komponist_ui::lang(ui.ctx());
        if let Some(driver) = &self.driver {
            badge(
                ui,
                rect,
                driver.index,
                self.ink,
                color32(color::SURFACE_0),
                false,
            );
            let tip = lang.macros_controlled_tip(&driver.reference);
            let tooltip = range.map_or_else(|| tip.clone(), |range| format!("{tip}\n{range}"));
            r.clone().on_hover_text(tooltip);
            widgets::accesskit(r, |node| {
                node.set_read_only();
                node.set_description(tip.as_str());
            });
        } else if let Some((_, title)) = &self.mapping {
            let description = lang.macros_a11y_candidate(title);
            widgets::accesskit(r, |node| node.set_description(description.as_str()));
        }
        if self.revealed || self.driver.as_ref().is_some_and(|d| d.highlighted) {
            px::outline(ui.painter(), rect, radius::S, 0.0, self.ink);
        }
    }

    /// Intercept value gestures before opening ordinary parameter history.
    pub(crate) fn intercept(&self, r: &Response, changed: bool, out: &mut Vec<AppAction>) -> bool {
        if let Some((index, _)) = &self.mapping {
            // A pointer clicking a continuous value does not map it. The widget's first
            // change (wheel, arrows, switch click) or drag start is the mapping gesture.
            if (changed || r.drag_started() || wheel_turned(r))
                && self.driver.as_ref().is_none_or(|d| d.index != *index)
            {
                out.push(AppAction::Macro(self.map(*index)));
            }
            return true;
        }
        if self.driver.is_some() {
            if changed || r.drag_started() || wheel_turned(r) {
                out.push(AppAction::Macro(Action::ShowDriver {
                    track: self.track,
                    target: self.target.clone(),
                }));
            }
            return true;
        }
        false
    }

    pub(crate) fn menu<'a>(
        &'a self,
        lang: Lang,
        ordinary: &[Entry<'a, Pick>],
        r: &Response,
    ) -> Option<Pick> {
        let slots = self.slots();
        if self.driver.is_some() {
            let entries = [
                Entry::action(lang.macros_show(), Pick::Show),
                Entry::action(lang.macros_remove_menu(), Pick::Remove),
                Entry::submenu(lang.macros_map_to(), &slots),
            ];
            return ContextMenu::new(lang.devices_parameter(), &entries).show_on(r);
        }
        let mut entries: Vec<_> = ordinary
            .iter()
            .copied()
            .filter(|entry| {
                self.mapping.is_none() || !matches!(entry.kind, Kind::Action(Pick::Enter))
            })
            .collect();
        entries.push(Entry::submenu(lang.macros_map_to(), &slots));
        ContextMenu::new(lang.devices_parameter(), &entries).show_on(r)
    }

    pub(crate) fn slots_as<T: Copy>(
        &self,
        variant: impl Fn(Pick) -> T,
    ) -> [Entry<'_, T>; MACRO_COUNT] {
        std::array::from_fn(|index| {
            Entry::action(&self.slots[index], variant(Pick::Map(index as u8))).checked(
                self.driver
                    .as_ref()
                    .is_some_and(|d| usize::from(d.index) == index),
            )
        })
    }

    pub(crate) fn value(&self) -> Option<f32> {
        self.driver.as_ref().map(|driver| driver.value)
    }

    pub(crate) fn value_menu(
        &self,
        lang: Lang,
        response: &Response,
        text: impl FnOnce() -> String,
        default: impl FnOnce() -> AppAction,
    ) -> Option<AppAction> {
        let ordinary = [
            Entry::action(lang.ui_value_to_default(), Pick::Default)
                .shortcut(lang.ui_value_to_default_shortcut()),
            Entry::action(lang.ui_value_enter(), Pick::Enter).shortcut(lang.key_enter()),
        ];
        let pick = self.menu(lang, &ordinary, response)?;
        match pick {
            Pick::Default => Some(default()),
            Pick::Enter => {
                open_value_entry(&response.ctx, response.id, text());
                None
            }
            pick => self.menu_action(pick),
        }
    }

    fn slots(&self) -> [Entry<'_, Pick>; MACRO_COUNT] {
        self.slots_as(|pick| pick)
    }

    fn map(&self, index: u8) -> Action {
        Action::Map {
            track: self.track,
            index,
            target: self.target.clone(),
        }
    }

    pub(crate) fn menu_action(&self, pick: Pick) -> Option<AppAction> {
        let action = match pick {
            Pick::Map(index) => self.map(index),
            Pick::Show => Action::ShowDriver {
                track: self.track,
                target: self.target.clone(),
            },
            Pick::Remove => Action::Remove {
                track: self.track,
                index: self.driver.as_ref()?.index,
                target: self.target.clone(),
            },
            _ => return None,
        };
        Some(AppAction::Macro(action))
    }
}

/// The common response policy for track faders and pan knobs.
pub(super) fn channel_control(
    ui: &Ui,
    ctx: &mut ViewCtx<'_>,
    control: Option<&Control>,
    response: &Response,
    undo: &str,
    change: impl FnOnce() -> AppAction,
) -> bool {
    if let Some(control) = control {
        control.decorate(ui, response, response.rect, None);
        let mut out = Vec::new();
        let intercepted = control.intercept(response, response.changed(), &mut out);
        for action in out {
            ctx.send(action);
        }
        if intercepted {
            return false;
        }
    }
    super::mixer::send_gesture(ctx, response, undo, change);
    true
}

fn badge(ui: &Ui, rect: Rect, index: u8, fill: Color32, digits: Color32, missing: bool) {
    let r = Rect::from_min_size(
        pos2(rect.right() - BADGE - BADGE_INSET, rect.top() + BADGE_INSET),
        vec2(BADGE, BADGE),
    );
    px::fill(ui.painter(), r, radius::S, fill);
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        u16::from(index) + 1,
        fonts::font_id(&typography::MONO_XXS),
        digits,
    );
    if missing {
        ui.painter()
            .circle_filled(r.right_top(), ERROR_DOT / 2.0, color32(color::STATE_ERROR));
    }
}

pub(super) fn text(ui: &Ui, rect: Rect, text: &str, style: &typography::TypeStyle, color: Color32) {
    let galley = elided_galley(
        ui,
        text,
        fonts::font_id(style),
        style.line_height,
        rect.width(),
    );
    let at = widgets::text_pos(rect, &galley, rect.left(), ui.pixels_per_point());
    ui.painter().galley(at, galley, color);
}

pub(super) fn map_button(ui: &mut Ui, ctx: &mut ViewCtx<'_>, track: TrackId) {
    let lang = ctx.lang;
    let r = ui.add(Button::new(lang.macros_map()).on(ctx.ui.track_macros.mapping == Some(track)));
    r.clone().on_hover_text(lang.macros_map_tip());
    if r.clicked() {
        ctx.send(AppAction::Macro(Action::ToggleMapping { track }));
    }
}

pub(super) fn percent(lang: Lang, value: f64) -> String {
    super::automation::percentage(lang, value as f32)
}

pub(super) fn parse_percent(text: &str) -> Option<f64> {
    parse_number(text, "%").map(|value| value / 100.0)
}

/// Width occupied by the macro card before the scrolling chain.
pub(super) fn card_width(ctx: &ViewCtx<'_>, track: TrackId) -> f32 {
    if ctx.ui.track_macros.collapsed.contains(&track) {
        device::frame::HEAD_H
    } else {
        CARD_WIDTH
    }
}

/// Draw the fixed-width card at the head of the device dock.
pub(super) fn card(ui: &mut Ui, ctx: &mut ViewCtx<'_>, body: Rect, track: &Track, scroll: f32) {
    let is_collapsed = ctx.ui.track_macros.collapsed.contains(&track.id);
    let width = card_width(ctx, track.id);
    let pad = device::chain::PADDING;
    let r = Rect::from_min_size(
        pos2(body.left() + pad, body.top() + pad - scroll),
        vec2(width, device::frame::TOTAL_H),
    );
    let lang = ctx.lang;
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(r)
            .id_salt(("track-macros-card", track.id))
            .accessibility_role(Role::Group)
            .accessibility_label(lang.macros_title()),
    );
    ui.set_clip_rect(ui.clip_rect().intersect(body));
    group_bounds(&ui, r);
    px::fill(ui.painter(), r, radius::M, color32(color::SURFACE_2));
    if is_collapsed {
        let hit = ui.interact(r, ui.scope_id().with("expand"), Sense::click());
        hit.widget_info(|| {
            WidgetInfo::labeled(
                Role::Button,
                true,
                lang.devices_expand_name(lang.macros_title()),
            )
        });
        hit.clone().on_hover_text(lang.macros_title());
        let label = widgets::styled_galley(&ui, lang.macros_title(), &typography::UI);
        let position = pos2(
            r.center().x + label.size().y / 2.0,
            r.top() + space::SPACE_2,
        );
        let mut shape = egui::epaint::TextShape::new(position, label, color32(color::TEXT_1));
        shape.angle = std::f32::consts::FRAC_PI_2;
        ui.painter().add(shape);
        if hit.clicked() {
            ctx.send(AppAction::Macro(Action::ToggleCollapsed {
                track: track.id,
            }));
        }
        return;
    }
    let head = Rect::from_min_size(r.min, vec2(r.width(), device::frame::HEAD_H));
    px::fill(ui.painter(), head, radius::M, color32(color::SURFACE_3));
    let mark = Rect::from_center_size(
        pos2(head.left() + space::SPACE_1, head.center().y),
        vec2(3.0, 14.0),
    );
    px::fill(
        ui.painter(),
        mark,
        0.0,
        color32(TrackPalette::new(track.hue.0).base),
    );
    let buttons = head.shrink2(vec2(space::SPACE_1, 0.0));
    let mut controls = ui.new_child(
        UiBuilder::new()
            .max_rect(buttons)
            .layout(egui::Layout::right_to_left(Align::Center)),
    );
    let collapse = controls.add(Button::icon(
        komponist_tokens::Icon::ChevronDown,
        lang.devices_collapse(),
    ));
    if collapse.clicked() {
        ctx.send(AppAction::Macro(Action::ToggleCollapsed {
            track: track.id,
        }));
    }
    map_button(&mut controls, ctx, track.id);
    let right = controls.cursor().right();
    let label = Rect::from_x_y_ranges(mark.right() + space::SPACE_1..=right, head.y_range());
    text(
        &ui,
        label,
        lang.macros_title(),
        &typography::UI,
        color32(color::TEXT_1),
    );
    let start = pos2(
        r.left() + device::grid::PADDING,
        head.bottom() + device::grid::PADDING,
    );
    let step = vec2(device::grid::CELL[0], device::grid::CELL[1])
        + vec2(device::grid::GAP, device::grid::GAP);
    for index in 0..MACRO_COUNT {
        let min = start + vec2((index % 4) as f32 * step.x, (index / 4) as f32 * step.y);
        let cell = Rect::from_min_size(min, vec2(device::grid::CELL[0], device::grid::CELL[1]));
        dial(&mut ui, ctx, track, index as u8, cell, false);
    }
    let selected = &track.macros.0[usize::from(ctx.ui.track_macros.selected(track.id))];
    let (title, detail) = if ctx.ui.track_macros.mapping == Some(track.id) {
        (
            lang.macros_map_hint().to_owned(),
            lang.macros_map_hint_esc().to_owned(),
        )
    } else {
        (
            lang.macros_targets(selected.mappings.len()),
            selected
                .mappings
                .iter()
                .map(|m| m.label.as_str())
                .collect::<Vec<_>>()
                .join(" · "),
        )
    };
    let row = Rect::from_min_max(
        start + vec2(0.0, 2.0 * step.y),
        r.max - vec2(device::grid::PADDING, device::grid::PADDING),
    );
    let style = typography::SMALL;
    let first = Rect::from_min_size(row.min, vec2(row.width(), style.line_height));
    text(&ui, first, &title, &style, color32(color::TEXT_1));
    text(
        &ui,
        first.translate(vec2(0.0, style.line_height)),
        &detail,
        &style,
        color32(color::TEXT_3),
    );
}

/// One of the eight slots, drawn in the card and the inspector.
pub(super) fn dial(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    track: &Track,
    index: u8,
    cell: Rect,
    compact: bool,
) {
    let lang = ctx.lang;
    let slot = &track.macros.0[usize::from(index)];
    let name = name(lang, slot);
    let selected = ctx.ui.track_macros.selected(track.id) == index;
    let mapping = ctx.ui.track_macros.mapping == Some(track.id);
    if selected && mapping {
        px::fill(
            ui.painter(),
            cell,
            radius::S,
            color32(color::ACCENT_SURFACE),
        );
        px::outline(ui.painter(), cell, radius::S, 0.0, color32(color::ACCENT));
    } else if selected {
        state::paint_select_ring(ui.painter(), cell, radius::S);
    }
    let hue = color32(TrackPalette::new(track.hue.0).base);
    let ink = if slot.mappings.is_empty() {
        color32(color::TEXT_3)
    } else {
        hue
    };
    let d = if compact {
        size::KNOB_SMALL
    } else {
        size::KNOB
    };
    let p = pos2(cell.center().x, cell.top() + space::SPACE_0_5 + d / 2.0);
    let knob = Rect::from_center_size(p, vec2(d, d));
    let mut current = value(ctx, track, index);
    let label = lang.macros_a11y(index + 1, name);
    let mut widget = Knob::new(&mut current, label)
        .format(|value| percent(lang, value))
        .parse(parse_percent)
        .arc_color(ink);
    if compact {
        widget = widget.small();
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(knob)
            .id_salt(("macro-knob", index)),
    );
    let response = child.add(widget);
    let catalog = super::automation::Catalog::new(ctx, AutomationOwner::Track(track.id));
    let missing = slot
        .mappings
        .iter()
        .filter(|m| {
            catalog
                .as_ref()
                .is_some_and(|catalog| matches!(catalog.resolve(&m.target), Err(Some(_))))
        })
        .count();
    let automation = track.automation.lane(&AutomationTarget::Macro { index });
    let mut description = if missing == 0 {
        lang.macros_a11y_mappings(slot.mappings.len())
    } else {
        format!(
            "{} · {}",
            lang.macros_a11y_mappings(slot.mappings.len()),
            lang.macros_targets_missing(missing)
        )
    };
    if let Some(lane) = automation {
        let label = super::automation::mode_names(lang)
            .into_iter()
            .find_map(|(mode, label)| (mode == lane.mode).then_some(label))
            .expect("automation mode");
        description = format!("{description} · {label}");
        if lane.mode != AutomationMode::Off {
            let writing = ctx.automation_capture.is_some_and(|capture| {
                capture.is_writing(AutomationOwner::Track(track.id), lane.id)
            });
            ui.painter().circle_filled(
                knob.left_top(),
                komponist_tokens::widget::knob::automation_dot::SIZE / 2.0,
                if writing {
                    color32(color::STATE_RECORD)
                } else {
                    hue
                },
            );
        }
    }
    widgets::accesskit(&response, |node| {
        node.set_selected(selected);
        node.set_description(description.as_str());
    });
    badge(
        ui,
        cell,
        index,
        color32(if selected {
            color::ACCENT
        } else {
            color::SURFACE_4
        }),
        color32(if selected {
            color::ACCENT_INK
        } else {
            color::TEXT_2
        }),
        missing > 0,
    );
    if missing > 0 {
        let badge_rect = Rect::from_min_size(
            pos2(cell.right() - BADGE - BADGE_INSET, cell.top() + BADGE_INSET),
            vec2(BADGE, BADGE),
        );
        let badge = ui.interact(
            badge_rect,
            ui.scope_id().with(("macro-missing", index)),
            Sense::hover(),
        );
        badge.on_hover_text(format!(
            "{} · {}",
            reference(lang, index, slot),
            lang.macros_targets_missing(missing)
        ));
    }
    let name_rect = Rect::from_min_size(
        pos2(cell.left(), knob.bottom()),
        vec2(cell.width(), typography::LABEL.line_height),
    );
    let name_resp = ui.interact(
        name_rect,
        ui.scope_id().with(("macro-name", index)),
        Sense::click(),
    );
    name_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    let rename_id = name_resp.id.with("edit");
    let rename = ui
        .ctx()
        .data(|d| d.get_temp::<widgets::rename_field::RenameState>(rename_id));
    if let Some(mut rename) = rename {
        use widgets::rename_field::{RenameField, RenameOutcome};
        match RenameField::new(
            &mut rename,
            rename_id,
            lang.macros_name(),
            &slot.name,
            fonts::font_id(&typography::LABEL),
        )
        .cancel_unfocused()
        .show(ui, name_rect)
        {
            RenameOutcome::Editing => {
                ui.ctx().data_mut(|d| d.insert_temp(rename_id, rename));
            }
            RenameOutcome::Commit(name) => {
                ctx.send(AppAction::Macro(Action::Rename {
                    track: track.id,
                    index,
                    name,
                }));
                ui.ctx()
                    .data_mut(|d| d.remove::<widgets::rename_field::RenameState>(rename_id));
            }
            RenameOutcome::Cancel => ui
                .ctx()
                .data_mut(|d| d.remove::<widgets::rename_field::RenameState>(rename_id)),
        }
    } else {
        centered_text(
            ui,
            name_rect,
            name,
            &typography::LABEL,
            color32(color::TEXT_2),
        );
    }
    let amount = name_rect.translate(vec2(0.0, typography::LABEL.line_height));
    centered_text(
        ui,
        amount,
        &percent(lang, f64::from(current)),
        &typography::VALUE_S,
        ink,
    );
    if response.clicked() || response.drag_started() || response.changed() || name_resp.clicked() {
        ctx.send(AppAction::Macro(Action::Select {
            track: track.id,
            index,
        }));
    }
    if response.hovered() || response.dragged() || name_resp.hovered() {
        ctx.ui.track_macros.hovered = Some((track.id, index, compact));
    } else if ctx.ui.track_macros.hovered == Some((track.id, index, compact)) {
        ctx.ui.track_macros.hovered = None;
    }
    let mut actions = Vec::new();
    super::mixer::gesture_actions(
        &response,
        response.changed(),
        || {
            AppAction::Macro(Action::BeginValue {
                track: track.id,
                index,
            })
        },
        &mut actions,
        |actions| {
            actions.push(AppAction::Macro(Action::SetValue {
                track: track.id,
                index,
                value: current,
            }))
        },
    );
    for action in actions {
        ctx.send(action);
    }
    #[derive(Clone, Copy)]
    enum MacroPick {
        Default,
        Enter,
        Rename,
        Clear,
        Automation,
    }
    let entries = [
        Entry::action(lang.devices_to_default(), MacroPick::Default),
        Entry::action(lang.devices_enter_value(), MacroPick::Enter),
        Entry::action(lang.automation_show_menu(), MacroPick::Automation),
        Entry::separator(),
        Entry::action(lang.macros_rename(), MacroPick::Rename),
        if slot.mappings.is_empty() {
            Entry::action(lang.macros_remove_all(), MacroPick::Clear).disabled()
        } else {
            Entry::action(lang.macros_remove_all(), MacroPick::Clear)
        },
    ];
    let pick = ContextMenu::new(lang.macros_title(), &entries).show_on(&response);
    if name_resp.double_clicked() || matches!(pick, Some(MacroPick::Rename)) {
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                rename_id,
                widgets::rename_field::RenameState::new(&slot.name),
            )
        });
    }
    match pick {
        Some(MacroPick::Default) => ctx.send(AppAction::Macro(Action::SetValue {
            track: track.id,
            index,
            value: 0.0,
        })),
        Some(MacroPick::Automation) => ctx.send(AppAction::Automation(
            super::automation::Action::ShowTarget {
                owner: AutomationOwner::Track(track.id),
                replace: None,
                target: AutomationTarget::Macro { index },
                label: super::automation::macro_label(lang, index, slot),
            },
        )),
        Some(MacroPick::Enter) => {
            open_value_entry(ui.ctx(), response.id, percent(lang, f64::from(current)))
        }
        Some(MacroPick::Clear) => ctx.send(AppAction::Macro(Action::Clear {
            track: track.id,
            index,
        })),
        _ => {}
    }
}

fn centered_text(ui: &Ui, rect: Rect, text: &str, style: &typography::TypeStyle, color: Color32) {
    let galley = elided_galley(
        ui,
        text,
        fonts::font_id(style),
        style.line_height,
        rect.width(),
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}
