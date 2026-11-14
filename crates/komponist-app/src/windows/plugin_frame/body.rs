//! The frame body below the header: the plugin's GUI (an X11 child covers it), the device states
//! of the S05 device card ("Lädt …", "Abgestürzt" with "Neu laden", "… fehlt") and the generic
//! parameter grid (S05 "Parameter": "Plugins ohne GUI oder auf Wunsch: Raster aus Drehreglern mit
//! Namen und Wert.", the prototype's `pw.params`).
//!
//! The body is `surface.0`, the colour the X11 child gets too, so a live resize never flashes
//! another colour (owner test 2026-11-13).

use egui::{Align, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, Vec2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::knob::Knob;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px};

use super::metrics as m;

/// Texts of the body (S05 `card()`, `Prototyp.dc.html` `pw.guiT`).
pub mod names {
    /// Crashed device (`card()` `msg`).
    pub const CRASHED: &str = "Abgestürzt";
    /// Its explanation (`card()` `msg2`).
    pub const CRASHED_DETAIL: &str = "Das Plugin wurde beendet, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.";
    /// The crashed device's button (`card()` `btnT`).
    pub const RELOAD: &str = "Neu laden";
    /// Loading device (`card()` `msg`).
    pub const LOADING: &str = "Lädt …";
    /// Its explanation (`card()` `msg2`).
    pub const LOADING_DETAIL: &str = "Plugin wird gescannt und instanziiert.";
    /// Missing plugin: "Vital fehlt" (S06 track header).
    pub fn missing(name: &str) -> String {
        format!("{name} fehlt")
    }
    /// GUI placeholder where no X11 child can be embedded (`pw.guiT`).
    pub fn gui_placeholder(name: &str) -> String {
        format!("Plugin-GUI · zeichnet {name} selbst")
    }
    /// Second line of the placeholder ("Rahmen von klang-studio, Inhalt vom Plugin", rule 1).
    pub const GUI_PLACEHOLDER_DETAIL: &str = "Rahmen von komponist, Inhalt vom Plugin";
}

/// One knob of the parameter grid.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamRow {
    /// Parameter id.
    pub id: u32,
    /// Name ("Cutoff").
    pub name: String,
    /// Lowest plain value.
    pub min: f64,
    /// Highest plain value.
    pub max: f64,
    /// Alt-click value.
    pub default: f64,
    /// Steps of a stepped parameter, 0 = continuous.
    pub step_count: u32,
    /// Current plain value.
    pub value: f64,
    /// The plugin's text for `value`, empty if none.
    pub text: String,
}

impl ParamRow {
    /// `v` on the parameter's step grid, inside its range.
    pub fn quantize(&self, v: f64) -> f64 {
        let (lo, hi) = (self.min.min(self.max), self.min.max(self.max));
        let v = v.clamp(lo, hi);
        if self.step_count == 0 || hi <= lo {
            return v;
        }
        let steps = f64::from(self.step_count);
        lo + ((v - lo) / (hi - lo) * steps).round() / steps * (hi - lo)
    }

    /// Text for `v`: the plugin's own while `v` is the value it describes, else a number.
    pub fn text_for(&self, v: f64) -> String {
        if !self.text.is_empty()
            && (v - self.value).abs() <= f64::EPSILON * self.value.abs().max(1.0)
        {
            return self.text.clone();
        }
        if self.step_count > 0 {
            format!("{v:.0}")
        } else {
            format!("{v:.2}")
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
    /// The bridge died or the plugin failed.
    Crashed,
    /// The plugin is not installed.
    Missing {
        /// Plugin name.
        name: String,
    },
    /// The generic parameter grid.
    Params(Vec<ParamRow>),
}

/// What the user did in the body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BodyEvent {
    /// "Neu laden".
    Reload,
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
        } => placeholder(&mut body, rect, name),
        BodyModel::Loading => {
            message(&mut body, rect, Message::Loading);
        }
        BodyModel::Crashed => {
            if message(&mut body, rect, Message::Crashed) {
                events.push(BodyEvent::Reload);
            }
        }
        BodyModel::Missing { name } => {
            message(&mut body, rect, Message::Missing(name));
        }
        BodyModel::Params(rows) => grid(&mut body, rect, rows, &mut events),
    }
    events
}

enum Message<'a> {
    Loading,
    Crashed,
    Missing(&'a str),
}

/// The device card's message (`msgS`) centred in the body; `true` when "Neu laden" was clicked.
fn message(ui: &mut Ui, rect: Rect, msg: Message<'_>) -> bool {
    let (title, title_color, detail) = match msg {
        Message::Loading => (
            names::LOADING.to_owned(),
            color::TEXT_1,
            Some(names::LOADING_DETAIL),
        ),
        Message::Crashed => (
            names::CRASHED.to_owned(),
            color::STATE_ERROR,
            Some(names::CRASHED_DETAIL),
        ),
        Message::Missing(name) => (names::missing(name), color::STATE_ERROR, None),
    };
    let crashed = matches!(msg, Message::Crashed);
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
                clicked = ui.add(Button::new(names::RELOAD)).clicked();
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
fn placeholder(ui: &mut Ui, rect: Rect, name: &str) {
    let first = names::gui_placeholder(name);
    let a = w::styled_galley(ui, &first, &typography::VALUE_M);
    let b = w::styled_galley(ui, names::GUI_PLACEHOLDER_DETAIL, &typography::VALUE_S);
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
        (pos_b, b, names::GUI_PLACEHOLDER_DETAIL, color::TEXT_3),
    ] {
        let pos = egui::pos2(px::snap(pos.x, ppp), px::snap(pos.y, ppp));
        let r = ui.allocate_rect(Rect::from_min_size(pos, galley.size()), Sense::hover());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
        ui.painter().galley(pos, galley, color32(colour));
    }
}

/// The parameter grid: 4 columns of knob, name and value, scrolling.
fn grid(ui: &mut Ui, rect: Rect, rows: &mut [ParamRow], events: &mut Vec<BodyEvent>) {
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
            for chunk in rows.chunks_mut(columns) {
                ui.horizontal_top(|ui| {
                    ui.add_space(m::PARAM_PADDING - m::PARAM_COLUMN_GAP);
                    for row in chunk {
                        cell(ui, col_w, row, events);
                    }
                });
            }
            ui.add_space(m::PARAM_PADDING - m::PARAM_ROW_GAP);
        });
}

fn cell(ui: &mut Ui, width: f32, row: &mut ParamRow, events: &mut Vec<BodyEvent>) {
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
            let mut value = row.value;
            let snapshot = row.clone();
            let knob = Knob::new(&mut value, row.name.clone())
                .range(row.min..=row.max)
                .default_value(row.default)
                .format(move |v| snapshot.text_for(v));
            if ui.add(knob).changed() {
                let value = row.quantize(value);
                if value != row.value {
                    row.value = value;
                    row.text.clear();
                    events.push(BodyEvent::SetParam { id: row.id, value });
                }
            }
            let name = fonts::rich_text(&row.name, &name_style).color(color32(color::TEXT_2));
            ui.add(egui::Label::new(name).truncate());
            let text = row.text_for(row.value);
            let value_text = fonts::rich_text(&text, &value_style).color(color32(color::TEXT_3));
            ui.add(egui::Label::new(value_text).truncate());
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(min: f64, max: f64, steps: u32) -> ParamRow {
        ParamRow {
            id: 1,
            name: "Gain".into(),
            min,
            max,
            default: min,
            step_count: steps,
            value: min,
            text: String::new(),
        }
    }

    #[test]
    fn stepped_parameters_snap_to_their_steps_and_stay_in_range() {
        let r = row(0.0, 32.0, 32);
        assert_eq!(r.quantize(3.4), 3.0);
        assert_eq!(r.quantize(40.0), 32.0);
        let r = row(0.0, 1.0, 4);
        assert_eq!(r.quantize(0.3), 0.25);
        let r = row(-60.0, 0.0, 0);
        assert_eq!(r.quantize(-12.3), -12.3);
        assert_eq!(r.quantize(3.0), 0.0);
    }

    #[test]
    fn the_plugins_text_is_shown_only_for_its_value() {
        let mut r = row(-60.0, 0.0, 0);
        r.value = -12.0;
        r.text = "-12.0 dB".into();
        assert_eq!(r.text_for(-12.0), "-12.0 dB");
        assert_eq!(r.text_for(-6.0), "-6.00");
        r.text.clear();
        assert_eq!(r.text_for(-12.0), "-12.00");
        assert_eq!(row(0.0, 8.0, 8).text_for(3.0), "3");
    }
}
