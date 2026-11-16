//! The frame header (S05 `s.ovPlugin`, `ks-view.js` `pluginView`): bypass mark, plugin name,
//! track · format, preset ‹ field › and "Speichern", then on the right the view switch
//! (Plugin / Parameter), "Bypass", "Oben halten" and the close button.
//!
//! The controls are the design system's (`Button` 24 high, `Dropdown`, `Segmented`); S05 draws
//! them 22 high in a 36 px header, the frame uses the 28 px header of ARCHITECTURE §8.4.
//!
//! The header only reports what the user did ([`HeaderEvent`]); the frame turns that into
//! actions. It needs no app state, so tests render it directly. The same code draws the header of
//! the app's frame viewport (Linux) and of the bridge's own frame window (macOS, #52); the model
//! and the events are plain data in `komponist_ipc::frame`, so they cross the process boundary.

use egui::{Align, Layout, Rect, Sense, Ui, UiBuilder, Vec2, pos2, vec2};
use komponist_i18n::Lang;
pub use komponist_ipc::frame::{HeaderEvent, HeaderModel, Mark, PluginView, names};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, typography};

use super::metrics as m;
use crate::widgets::button::Button;
use crate::widgets::dropdown::{Dropdown, Item};
use crate::widgets::tabs::Segmented;
use crate::widgets::tooltip::Tip;
use crate::{fonts, px};

/// Surface level of the header: the design system's panel header (`surface.2`), so its
/// controls (`surface.3`) stand out. S05 paints the frame `surface.3` with controls one level
/// up (`#303236`), which the design system's button does not have (report).
pub const GROUND: usize = 2;

fn metrics_ground() -> komponist_tokens::Srgba {
    color::SURFACE[GROUND]
}

/// Result of [`show`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeaderOutput {
    /// What the user did.
    pub events: Vec<HeaderEvent>,
    /// Width the header needs for all its items, in points.
    pub natural_width: f32,
}

/// Draws the header into `rect`.
pub fn show(ui: &mut Ui, rect: Rect, model: &HeaderModel) -> HeaderOutput {
    let t = crate::lang(ui.ctx());
    let mut out = HeaderOutput::default();
    px::fill(ui.painter(), rect, 0.0, color32(metrics_ground()));
    let inner = Rect::from_min_max(
        rect.min + vec2(m::HEADER_PAD_LEFT, 0.0),
        rect.max - vec2(m::HEADER_PAD_RIGHT, 0.0),
    );

    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .id_salt("komponist_frame_header_left")
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.spacing_mut().item_spacing = vec2(m::HEADER_GAP, 0.0);
    left_items(&mut left, t, model, &mut out.events);
    let left_width = left.min_rect().width();

    // The right group in reading order, ending at the right edge: its width is measured in one
    // pass and used in the next (a right-to-left layout would flip the segments' order).
    // Per viewport: every frame window has its own header, egui's memory is shared.
    let width_id = egui::Id::unique(("komponist_frame_header_right", ui.ctx().viewport_id()));
    let known: f32 = ui.data(|d| d.get_temp(width_id)).unwrap_or(0.0);
    let x = (inner.right() - known).max(inner.left());
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(x, inner.top()), inner.max))
            .id_salt("komponist_frame_header_right")
            .layout(Layout::left_to_right(Align::Center)),
    );
    right.spacing_mut().item_spacing = vec2(m::HEADER_GAP, 0.0);
    right_items(&mut right, t, model, &mut out.events);
    let right_width = right.min_rect().width();
    if (right_width - known).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(width_id, right_width));
        ui.ctx().request_discard("plugin frame header width");
    }

    out.natural_width =
        m::HEADER_PAD_LEFT + left_width + m::HEADER_GAP + right_width + m::HEADER_PAD_RIGHT;
    out
}

fn left_items(ui: &mut Ui, t: Lang, model: &HeaderModel, events: &mut Vec<HeaderEvent>) {
    mark(ui, model.mark);
    let name = fonts::rich_text(&model.name, &typography::TRACK).color(color32(color::TEXT_1));
    ui.label(name);
    let sub = fonts::rich_text(&model.sub, &typography::UI).color(color32(color::TEXT_3));
    ui.label(sub);
    separator(ui);

    let has_presets = !model.presets.is_empty();
    ui.add_enabled_ui(has_presets, |ui| {
        if ui
            .add(Button::icon(
                Icon::ChevronLeft,
                t.plugin_frame_preset_prev(),
            ))
            .clicked()
        {
            events.push(HeaderEvent::PresetPrev);
        }
        let (labels, current) = model.preset_items();
        let items: Vec<Item<'_>> = labels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let item = Item::new(l);
                if model.preset.is_none() && i == labels.len() - 1 {
                    item.disabled()
                } else {
                    item
                }
            })
            .collect();
        let mut selected = current;
        let field = ui.add(
            Dropdown::new(t.plugin_frame_preset(), &mut selected, &items).width(m::PRESET_WIDTH),
        );
        if field.changed() && selected != current && selected < model.presets.len() {
            events.push(HeaderEvent::Preset(selected));
        }
        if ui
            .add(Button::icon(
                Icon::ChevronRight,
                t.plugin_frame_preset_next(),
            ))
            .clicked()
        {
            events.push(HeaderEvent::PresetNext);
        }
    });
    // No preset API for CLAP/VST3 and no user presets for hexa yet (report).
    ui.add_enabled(model.can_save, Button::new(t.plugin_frame_save()));
}

fn right_items(ui: &mut Ui, t: Lang, model: &HeaderModel, events: &mut Vec<HeaderEvent>) {
    let items = [
        if model.has_gui {
            Item::new(t.plugin_frame_view_plugin())
        } else {
            Item::new(t.plugin_frame_view_plugin()).disabled()
        },
        Item::new(t.plugin_frame_view_params()),
    ];
    let current = match model.view {
        PluginView::Gui => 0,
        PluginView::Params => 1,
    };
    let mut selected = current;
    if ui
        .add(Segmented::new(t.plugin_frame_view(), &mut selected, &items))
        .changed()
        && selected != current
    {
        events.push(HeaderEvent::View(if selected == 0 {
            PluginView::Gui
        } else {
            PluginView::Params
        }));
    }
    if ui
        .add(Button::new(t.plugin_frame_bypass()).on(model.bypassed))
        .clicked()
    {
        events.push(HeaderEvent::ToggleBypass);
    }
    let keep = Button::new(t.plugin_frame_keep_on_top())
        .frameless(GROUND)
        .on(model.keep_on_top);
    if ui.add(keep).clicked() {
        events.push(HeaderEvent::KeepOnTop(!model.keep_on_top));
    }
    let close = Button::icon(Icon::X, t.dialog_close())
        .frameless(GROUND)
        .tip(Tip::new(t.dialog_close()).shortcut(&model.close_shortcut));
    if ui.add(close).clicked() {
        events.push(HeaderEvent::Close);
    }
}

/// The 12 × 12 mark: a status, not a control (the "Bypass" button switches).
fn mark(ui: &mut Ui, mark: Mark) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(m::MARK), Sense::hover());
    let rect = px::snap_rect(rect, ui.pixels_per_point());
    let painter = ui.painter();
    match mark {
        Mark::Active => px::fill(painter, rect, radius::S, color32(color::STATE_PLAY)),
        Mark::Bypassed => {
            px::fill(painter, rect, radius::S, color32(color::SURFACE_4));
            px::inner_border(painter, rect, color32(color::TEXT_3));
        }
        Mark::Inactive => px::fill(painter, rect, radius::S, color32(color::SURFACE_4)),
    }
}

/// `width:1px; height:16px; background:#3b3d42; margin:0 4px`.
fn separator(ui: &mut Ui) {
    ui.add_space(m::SEPARATOR_MARGIN);
    let width = px::line_width(ui.pixels_per_point());
    let (rect, _) = ui.allocate_exact_size(vec2(width, m::SEPARATOR_HEIGHT), Sense::hover());
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_5));
    ui.add_space(m::SEPARATOR_MARGIN);
}
