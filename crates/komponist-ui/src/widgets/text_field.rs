//! The single-line text field (S09 recent-projects filter, S06/S08 file name and folder, the
//! file browser's location): `size.control` high, `space.2` padding, text in `type.ui` or
//! `value.m` (paths), hint in `text.3`, `surface.2` ground; while it has the keyboard focus its
//! contour is the accent ring (`inset 0 0 0 1px accent`, as the browser search in S05).
//!
//! AccessKit: role `TextInput` with the caller's German name.

use egui::{Align2, Id, Margin, Response, RichText, Role, Stroke, Ui, Widget, WidgetInfo, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, line, size, space};

use crate::fonts;

/// A single-line text field: `ui.add(TextField::new(&mut text, "Filter").hint("Filter"))`.
///
/// The response is the text edit's: `changed()`, `lost_focus()` with Enter, `request_focus()`.
pub struct TextField<'a> {
    text: &'a mut String,
    name: String,
    hint: Option<String>,
    style: TypeStyle,
    id: Option<Id>,
    width: Option<f32>,
}

impl<'a> TextField<'a> {
    /// A field for `text` with the German accessible `name`, filling the available width.
    pub fn new(text: &'a mut String, name: impl Into<String>) -> Self {
        Self {
            text,
            name: name.into(),
            hint: None,
            style: typography::UI,
            id: None,
            width: None,
        }
    }

    /// The hint shown while the field is empty (`text.3`).
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The text style (default `type.ui`; paths use `value.m`).
    #[must_use]
    pub fn style(mut self, style: TypeStyle) -> Self {
        self.style = style;
        self
    }

    /// A stable id, so the caller can focus the field.
    #[must_use]
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// A fixed width in points instead of the available width.
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
}

impl Widget for TextField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let mut edit = egui::TextEdit::singleline(self.text)
            .font(fonts::font_id(&self.style))
            .desired_width(width)
            .min_size(vec2(width, size::CONTROL))
            .align(Align2::LEFT_CENTER)
            .margin(Margin::symmetric(space::SPACE_2 as i8, 0));
        if let Some(id) = self.id {
            edit = edit.id(id);
        }
        if let Some(hint) = &self.hint {
            edit = edit.hint_text(
                RichText::new(hint)
                    .font(fonts::font_id(&self.style))
                    .color(color32(color::TEXT_3)),
            );
        }
        let response = ui
            .scope(|ui| {
                // egui draws the focused contour with `selection.stroke`.
                ui.visuals_mut().selection.stroke =
                    Stroke::new(line::WIDTH, color32(color::ACCENT));
                ui.add(edit)
            })
            .inner;
        let enabled = response.enabled();
        response.widget_info(|| WidgetInfo::labeled(Role::TextInput, enabled, &self.name));
        response
    }
}

/// The "Textfeld" samples of the gallery: leer (hint), gefüllt, Fokus (accent ring), Pfad.
pub fn gallery(ui: &mut Ui) {
    let id = Id::unique("komponist_gallery_text_field");
    let mut empty = String::new();
    let mut filled = String::from("Nachtfahrt");
    let mut focused = String::from("Nacht");
    let mut path = String::from("/home/kim/Projekte");
    super::sample_row(ui, space::SPACE_4, |ui| {
        super::sample(ui, "leer", |ui| {
            ui.add(
                TextField::new(&mut empty, "Filter · leer")
                    .hint("Filter")
                    .width(160.0),
            )
        });
        super::sample(ui, "gefüllt", |ui| {
            ui.add(TextField::new(&mut filled, "Filter · gefüllt").width(160.0))
        });
        super::sample(ui, "fokus", |ui| {
            let r = ui.add(
                TextField::new(&mut focused, "Filter · fokus")
                    .id(id)
                    .width(160.0),
            );
            if !ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false)) {
                r.request_focus();
                ui.data_mut(|d| d.insert_temp(id, true));
            }
            r
        });
        super::sample(ui, "pfad", |ui| {
            ui.add(
                TextField::new(&mut path, "Ordner")
                    .style(typography::VALUE_M)
                    .width(200.0),
            )
        });
    });
}
