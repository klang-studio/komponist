//! The frame body below the header: the plugin's GUI (an X11 child covers it), the device states
//! of the S05 device card ("Lädt …", "Abgestürzt" with "Neu laden", "… fehlt") and the generic
//! parameter grid (S05 "Parameter": "Plugins ohne GUI oder auf Wunsch: Raster aus Drehreglern mit
//! Namen und Wert.", the prototype's `pw.params`).
//!
//! The body is `surface.0`, the colour the X11 child gets too, so a live resize never flashes
//! another colour (owner test 2026-11-13).

use egui::{Align, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, Vec2, vec2};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, device, radius, size, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::knob::Knob;
use komponist_ui::widgets::value_menu;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px};

use super::metrics as m;
use crate::actions::{Action, PluginAction};
use crate::views::devices::{
    cells::{choice_defaults, draw_cell},
    model::{Card, Param, failure_texts},
};
use crate::views::mixer::gesture_actions;
use crate::views::track_macros::{Control, Pick};
use komponist_model::DeviceId;
use komponist_plugins::FailReason;

/// The parameter grid: WP-44's knob model ([`Param`]: the parameter model's description, value
/// and text) for all parameters, not just a device card's auto-layout.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamGrid {
    /// The knobs.
    pub params: Vec<Param>,
    pub(super) macros: std::collections::BTreeMap<komponist_model::params::ParamId, Control>,
}

impl ParamGrid {
    /// The parameter grid outside a track context.
    pub fn new(params: Vec<Param>) -> Self {
        Self {
            params,
            macros: Default::default(),
        }
    }
}

/// What the body shows.
#[derive(Debug, Clone, PartialEq)]
pub enum BodyModel {
    /// The plugin's own GUI; `native = false` (no X11, tests): the prototype's placeholder.
    Gui {
        /// An X11 child shows the plugin.
        native: bool,
        /// Plugin name for the placeholder.
        name: String,
    },
    /// The device loads.
    Loading,
    /// The bridge died or the plugin failed: „Abgestürzt“, „Lässt sich nicht laden“ with the
    /// reason, or „Hängt“.
    Crashed(FailReason),
    /// The plugin is not installed.
    Missing {
        /// Plugin name.
        name: String,
    },
    /// The generic parameter grid.
    Params(ParamGrid),
    /// The plugin's validated device card in the parameter view.
    Native(Box<Card>),
}

/// What the user did in the body.
#[derive(Debug, Clone, PartialEq)]
pub enum BodyEvent {
    /// "Neu laden".
    Reload,
    /// A drag or wheel burst on parameter `id` starts: one undo step until [`BodyEvent::EndGesture`].
    BeginGesture {
        /// Parameter id.
        id: u32,
    },
    /// The drag or wheel burst ended.
    EndGesture,
    /// An action emitted by a parameter control.
    ControlAction(Action),
    /// A knob moved.
    SetParam {
        /// Parameter id.
        id: u32,
        /// Plain value.
        value: f64,
    },
}

/// Draws the body into `rect`.
pub fn show(ui: &mut Ui, rect: Rect, model: &mut BodyModel) -> Vec<BodyEvent> {
    let t = komponist_ui::lang(ui.ctx());
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_0));
    let mut events = Vec::new();
    let mut body = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt("komponist_frame_body")
            .layout(Layout::top_down(Align::Min)),
    );
    body.set_clip_rect(rect);
    match model {
        BodyModel::Gui { native: true, .. } => {}
        BodyModel::Gui {
            native: false,
            name,
        } => placeholder(&mut body, t, rect, name),
        BodyModel::Loading => {
            message(&mut body, t, rect, Message::Loading);
        }
        BodyModel::Crashed(reason) => {
            if message(&mut body, t, rect, Message::Crashed(reason)) {
                events.push(BodyEvent::Reload);
            }
        }
        BodyModel::Missing { name } => {
            message(&mut body, t, rect, Message::Missing(name));
        }
        BodyModel::Params(grid_model) => grid(&mut body, rect, grid_model, &mut events),
        BodyModel::Native(card) => native_card(&mut body, card, &mut events),
    }
    events
}

fn native_card(ui: &mut Ui, card: &mut Card, events: &mut Vec<BodyEvent>) {
    let page_id = ui.make_persistent_id(("native_clap_page", card.device.0));
    card.page = ui
        .ctx()
        .data(|data| data.get_temp::<usize>(page_id))
        .unwrap_or(0)
        .min(card.layout.pages.len().saturating_sub(1));
    if card.layout.pages.len() > 1 {
        ui.horizontal(|ui| {
            for (index, page) in card.layout.pages.iter().enumerate() {
                if ui
                    .selectable_label(index == card.page, &page.name)
                    .clicked()
                {
                    card.page = index;
                    ui.ctx().data_mut(|data| data.insert_temp(page_id, index));
                }
            }
        });
    }
    let Some(page) = card.current_page() else {
        return;
    };
    let defaults = choice_defaults(&card.layout);
    ScrollArea::both().show(ui, |ui| {
        let (_, body) = ui.allocate_space(vec2(page.width(), device::grid::BODY_H));
        let palette = TrackPalette::new(150);
        let mut actions = Vec::new();
        for (index, cell) in page.cells.iter().enumerate() {
            let [x, y, width, height] = cell.bounds();
            let rect = Rect::from_min_size(body.min + vec2(x, y), vec2(width, height));
            let mut cell_ui =
                ui.new_child(UiBuilder::new().id_salt((card.page, index)).max_rect(rect));
            draw_cell(
                &mut cell_ui,
                &mut actions,
                card,
                &palette,
                cell,
                rect,
                &defaults,
            );
        }
        events.extend(actions.into_iter().map(BodyEvent::ControlAction));
    });
}

enum Message<'a> {
    Loading,
    Crashed(&'a FailReason),
    Missing(&'a str),
}

/// The device card's message (`msgS`) centred in the body; `true` when "Neu laden" was clicked.
fn message(ui: &mut Ui, t: Lang, rect: Rect, msg: Message<'_>) -> bool {
    let (title, title_color, detail) = match msg {
        Message::Loading => (
            t.plugin_frame_loading().to_owned(),
            color::TEXT_1,
            Some(t.plugin_frame_loading_detail().to_owned()),
        ),
        Message::Crashed(reason) => {
            let (title, text) = failure_texts(komponist_ui::lang(ui.ctx()), reason);
            (title.to_owned(), color::STATE_ERROR, Some(text))
        }
        Message::Missing(name) => (t.plugin_frame_missing(name), color::STATE_ERROR, None),
    };
    let crashed = matches!(msg, Message::Crashed(_));
    let detail = detail.as_deref();
    let width = m::MESSAGE_WIDTH.min(rect.width());
    let frame = egui::Frame::new()
        .fill(color32(color::SURFACE_2))
        .corner_radius(komponist_ui::theme::corner_radius(radius::M))
        .inner_margin(egui::Margin::same(m::MESSAGE_PADDING as i8));
    // Measure once, then centre: the box is as high as its content.
    let inner_width = width - 2.0 * m::MESSAGE_PADDING;
    let mut clicked = false;
    let area = Rect::from_center_size(rect.center(), vec2(width, rect.height()));
    let mut column = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .id_salt("komponist_frame_message")
            .layout(Layout::top_down_justified(Align::Center)),
    );
    let height = estimate_height(&column, inner_width, &title, detail, crashed);
    let top = (area.height() - height).max(0.0) / 2.0;
    column.add_space(top);
    let response = frame.show(&mut column, |ui| {
        ui.set_width(inner_width);
        ui.spacing_mut().item_spacing = vec2(0.0, m::MESSAGE_GAP);
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            let style = typography::UI;
            let title_text = egui::RichText::new(&title)
                .font(w::font_at(&style, wm::SEMIBOLD))
                .color(color32(title_color));
            ui.label(title_text);
            if let Some(detail) = detail {
                let text =
                    fonts::rich_text(detail, &typography::SMALL).color(color32(color::TEXT_2));
                ui.add(egui::Label::new(text).wrap());
            }
            if matches!(msg, Message::Loading) {
                progress(ui, inner_width);
            }
            if crashed {
                clicked = ui.add(Button::new(t.plugin_frame_reload())).clicked();
            }
        });
    });
    if crashed {
        // The crashed card's red top edge (`inset 0 2px 0` `state.error`).
        let r = response.response.rect;
        let edge = Rect::from_min_size(r.min, vec2(r.width(), m::CRASH_EDGE));
        px::fill(ui.painter(), edge, 0.0, color32(color::STATE_ERROR));
    }
    clicked
}

/// Height of the message box, for centring.
fn estimate_height(ui: &Ui, width: f32, title: &str, detail: Option<&str>, button: bool) -> f32 {
    let style = typography::UI;
    let mut h = w::galley(
        ui,
        title,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
    )
    .size()
    .y;
    if let Some(detail) = detail {
        let small = typography::SMALL;
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping {
                max_width: width,
                ..Default::default()
            },
            ..fonts::layout_job(detail, &small, color32(color::TEXT_2))
        };
        h += m::MESSAGE_GAP + ui.fonts_mut(|f| f.layout_job(job)).size().y;
    }
    if button {
        h += m::MESSAGE_GAP + size::CONTROL;
    }
    h + 2.0 * m::MESSAGE_PADDING
}

/// The loading bar (`progS`, `progFill`).
fn progress(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, m::PROGRESS_HEIGHT), Sense::hover());
    let painter = ui.painter();
    px::fill(
        painter,
        rect,
        m::PROGRESS_HEIGHT / 2.0,
        color32(color::SURFACE_3),
    );
    let fill = Rect::from_min_size(
        rect.min,
        vec2(rect.width() * m::PROGRESS_FILL, rect.height()),
    );
    px::fill(
        painter,
        fill,
        m::PROGRESS_HEIGHT / 2.0,
        color32(color::TEXT_2),
    );
}

/// The prototype's GUI placeholder (`pw.isGui`): two mono lines in a box.
fn placeholder(ui: &mut Ui, t: Lang, rect: Rect, name: &str) {
    let first = t.plugin_frame_gui_placeholder(name);
    let a = w::styled_galley(ui, &first, &typography::VALUE_M);
    let b = w::styled_galley(
        ui,
        t.plugin_frame_gui_placeholder_detail(),
        &typography::VALUE_S,
    );
    let [pad_x, pad_y] = m::PLACEHOLDER_PADDING;
    let gap = m::PARAM_ITEM_GAP;
    let size = vec2(
        a.size().x.max(b.size().x) + 2.0 * pad_x,
        a.size().y + gap + b.size().y + 2.0 * pad_y,
    );
    let ppp = ui.pixels_per_point();
    let boxed = px::snap_rect(Rect::from_center_size(rect.center(), size), ppp);
    px::fill(ui.painter(), boxed, radius::M, color32(color::SURFACE_1));
    let pos_a = egui::pos2(boxed.center().x - a.size().x / 2.0, boxed.top() + pad_y);
    let pos_b = egui::pos2(
        boxed.center().x - b.size().x / 2.0,
        pos_a.y + a.size().y + gap,
    );
    for (pos, galley, text, colour) in [
        (pos_a, a, first.as_str(), color::TEXT_2),
        (
            pos_b,
            b,
            t.plugin_frame_gui_placeholder_detail(),
            color::TEXT_3,
        ),
    ] {
        let pos = egui::pos2(px::snap(pos.x, ppp), px::snap(pos.y, ppp));
        let r = ui.allocate_rect(Rect::from_min_size(pos, galley.size()), Sense::hover());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
        ui.painter().galley(pos, galley, color32(colour));
    }
}

/// The parameter grid: 4 columns of knob, name and value, scrolling.
fn grid(ui: &mut Ui, rect: Rect, model: &mut ParamGrid, events: &mut Vec<BodyEvent>) {
    let inner_w = (rect.width() - 2.0 * m::PARAM_PADDING).max(size::KNOB);
    let columns = m::PARAM_COLUMNS;
    let col_w =
        ((inner_w - (columns - 1) as f32 * m::PARAM_COLUMN_GAP) / columns as f32).max(size::KNOB);
    ScrollArea::vertical()
        .id_salt("komponist_frame_params")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(m::PARAM_PADDING);
            ui.spacing_mut().item_spacing = vec2(m::PARAM_COLUMN_GAP, m::PARAM_ROW_GAP);
            for chunk in model.params.chunks_mut(columns) {
                ui.horizontal_top(|ui| {
                    ui.add_space(m::PARAM_PADDING - m::PARAM_COLUMN_GAP);
                    for p in chunk {
                        cell(ui, col_w, p, model.macros.get(&p.info.id), events);
                    }
                });
            }
            ui.add_space(m::PARAM_PADDING - m::PARAM_ROW_GAP);
        });
}

fn cell(
    ui: &mut Ui,
    width: f32,
    p: &mut Param,
    control: Option<&Control>,
    events: &mut Vec<BodyEvent>,
) {
    let name_style = typography::SMALL;
    let value_style = typography::VALUE_S;
    let height =
        size::KNOB + 2.0 * m::PARAM_ITEM_GAP + name_style.line_height + value_style.line_height;
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        Layout::top_down(Align::Center),
        |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, m::PARAM_ITEM_GAP);
            // Driven in normalised space, so log and dB parameters turn evenly.
            let current = control
                .and_then(Control::value)
                .map_or_else(|| p.info.to_normalized(p.value), f64::from);
            let mut value = current;
            if let Some(control) = control {
                control.paint_ground(ui, ui.max_rect());
            }
            let shown = p.clone();
            let knob = Knob::new(&mut value, p.name().to_owned())
                .range(0.0..=1.0)
                .default_value(p.info.default_normalized())
                .modulation(control.and_then(Control::modulation))
                .redirect(control.is_some_and(Control::redirects))
                .format(move |n| shown.text_at(shown.info.to_plain(n)));
            let r = ui.add(knob);
            let mut changed = r.changed();
            let mut actions = Vec::new();
            let intercepted = if let Some(control) = control {
                let lang = komponist_ui::lang(ui.ctx());
                let range = control.driver.as_ref().map(|driver| {
                    lang.devices_range(
                        p.text_at(p.info.to_plain(f64::from(driver.range.0))),
                        p.text_at(p.info.to_plain(f64::from(driver.range.1))),
                    )
                });
                control.decorate(ui, &r, ui.max_rect(), range.as_deref());
                let intercepted = control.intercept(&r, changed, &mut actions);
                let ordinary = [
                    komponist_ui::widgets::context_menu::Entry::action(
                        lang.ui_value_to_default(),
                        Pick::Default,
                    ),
                    komponist_ui::widgets::context_menu::Entry::action(
                        lang.ui_value_enter(),
                        Pick::Enter,
                    ),
                ];
                if let Some(pick) = control.menu(lang, &ordinary, &r) {
                    match pick {
                        Pick::Default => {
                            value = p.info.default_normalized();
                            changed = true;
                        }
                        Pick::Enter => komponist_ui::widgets::number_field::open_value_entry(
                            ui.ctx(),
                            r.id,
                            p.text_at(p.info.to_plain(current)),
                        ),
                        other => actions.extend(control.menu_action(other)),
                    }
                }
                intercepted
            } else {
                if value_menu::show(&r, || p.text_at(p.value)) {
                    value = p.info.default_normalized();
                    changed = true;
                }
                false
            };
            events.extend(actions.drain(..).map(BodyEvent::ControlAction));
            if !intercepted {
                let plain = p.info.to_plain(value);
                let moved = changed && plain != p.value;
                // A drag or a burst of wheel notches is one undo step; a click or a reset is one by
                // itself (gesture_actions).
                let id = p.wire;
                gesture_actions(
                    &r,
                    moved,
                    || {
                        Action::Plugin(PluginAction::BeginParamGesture {
                            device: DeviceId(0),
                            id,
                        })
                    },
                    &mut actions,
                    |_| {},
                );
                let (begin, end): (Vec<_>, Vec<_>) = actions
                    .into_iter()
                    .partition(|a| matches!(a, Action::Plugin(_)));
                events.extend(begin.iter().map(|_| BodyEvent::BeginGesture { id }));
                if moved {
                    p.text = p.info.format(plain);
                    p.value = plain;
                    events.push(BodyEvent::SetParam { id, value: plain });
                }
                events.extend(end.iter().map(|_| BodyEvent::EndGesture));
            }
            let name = fonts::rich_text(p.name(), &name_style).color(color32(color::TEXT_2));
            ui.add(egui::Label::new(name).truncate());
            let text = if control.and_then(Control::value).is_some() {
                p.text_at(p.info.to_plain(current))
            } else {
                p.text.clone()
            };
            let value_text = fonts::rich_text(&text, &value_style).color(color32(color::TEXT_3));
            ui.add(egui::Label::new(value_text).truncate());
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};

    fn param(min: f64, max: f64, steps: u32, value: f64, text: &str) -> Param {
        let kind = ParamKind::Float {
            min,
            max,
            scale: Scale::Lin,
            steps,
        };
        let unit = if min == 0.0 && max == 1.0 && steps == 0 {
            Unit::Percent
        } else {
            Unit::None
        };
        Param {
            wire: 1,
            info: ParamInfo::new("p1", "Gain", kind, min).unit(unit),
            value,
            text: text.into(),
        }
    }

    #[test]
    fn the_plugins_text_is_shown_only_for_its_value() {
        let p = param(-60.0, 0.0, 0, -12.0, "-12.0 dB");
        assert_eq!(p.text_at(-12.0), "-12.0 dB");
        assert_eq!(p.text_at(-6.0), "\u{2212}6.00");
        let p = param(0.0, 1.0, 0, 0.5, "");
        assert_eq!(p.text_at(0.25), "25 %");
        assert_eq!(param(0.0, 8.0, 8, 0.0, "").text_at(3.0), "3.00");
    }

    #[test]
    fn a_knob_turns_in_normalised_space_and_reports_plain_values() {
        let mut p = Param {
            wire: 4,
            info: ParamInfo::new(
                "p4",
                "Cutoff",
                ParamKind::scaled(20.0, 20_000.0, Scale::Log),
                1000.0,
            )
            .unit(Unit::Hz),
            value: 20.0,
            text: String::new(),
        };
        p.text = p.info.format(p.value);
        assert_eq!(p.text, "20 Hz");
        assert!((p.info.to_plain(0.5) - 632.455).abs() < 0.01);
        assert_eq!(p.text_at(p.info.to_plain(0.5)), "632 Hz");
    }
}
