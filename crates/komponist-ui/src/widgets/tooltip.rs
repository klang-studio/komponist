//! Hover tooltip with shortcut: "Quantisieren  Q", and the two-line form with a hint
//! ("Solo  S" / "Alt-Klick: nur diese Spur solo").
//!
//! `h 22 · px 8 · fläche-5 · radius.m · shadow.float` ([`crate::tooltip::frame`]): text in
//! `type.small` (`text.1`), the shortcut in Mono 10 `text.2`, 10 px after the text on the same
//! baseline; the hint below in `type.small` (`text.2`), wrapped at `size.menu-width`. The
//! two-line form has `6 px` vertical padding and `2 px` between its lines.
//!
//! [`Tip::attach`] shows the tooltip after 600 ms of hover, 6 px below the element (timing and
//! gap from [`crate::tooltip`]); [`Tip::show_inline`] draws it in place (gallery).

use egui::text::{LayoutJob, LayoutSection, TextFormat};
use egui::{Frame, Margin, Response, Ui};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, size, space};

use super::metrics;
use crate::{fonts, tooltip as timing};

/// The text of a hover tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tip<'a> {
    /// What the element does ("Quantisieren").
    pub text: &'a str,
    /// Its shortcut, if any ("Q", "Strg D").
    pub shortcut: Option<&'a str>,
    /// A second line, if any ("Alt-Klick: nur diese Spur solo").
    pub detail: Option<&'a str>,
}

impl<'a> Tip<'a> {
    /// A tooltip saying `text`.
    pub const fn new(text: &'a str) -> Self {
        Self {
            text,
            shortcut: None,
            detail: None,
        }
    }

    /// Adds the shortcut shown after the text.
    #[must_use]
    pub const fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// Adds a second line.
    #[must_use]
    pub const fn detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The whole tooltip as one string ("Quantisieren · Q · …"), for the AccessKit description.
    pub fn description(&self) -> String {
        let mut s = self.text.to_owned();
        for part in [self.shortcut, self.detail].into_iter().flatten() {
            s.push_str(" · ");
            s.push_str(part);
        }
        s
    }

    /// The first line: text and shortcut on one baseline.
    fn first_line(&self) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.append(
            self.text,
            0.0,
            fonts::text_format(&typography::SMALL, color32(color::TEXT_1)),
        );
        if let Some(sc) = self.shortcut {
            let start = job.text.len();
            job.text.push_str(sc);
            job.sections.push(LayoutSection {
                leading_space: metrics::SHORTCUT_GAP,
                byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(job.text.len()),
                format: TextFormat {
                    font_id: fonts::font_id(&typography::VALUE_S),
                    line_height: Some(typography::VALUE_S.line_height),
                    color: color32(color::TEXT_2),
                    ..Default::default()
                },
            });
        }
        job
    }

    /// The frame: [`crate::tooltip::frame`], with `6 px` vertical padding for the two-line form.
    pub fn frame(&self) -> Frame {
        let frame = timing::frame();
        if self.detail.is_some() {
            frame.inner_margin(Margin::symmetric(
                space::SPACE_2 as i8,
                space::SPACE_1_5 as i8,
            ))
        } else {
            frame
        }
    }

    /// The contents (no frame).
    pub fn content(&self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = space::SPACE_0_5;
        let first = ui.painter().layout_job(self.first_line());
        ui.label(first);
        if let Some(detail) = self.detail {
            let width = size::MENU_WIDTH - 2.0 * space::SPACE_2;
            let mut job = fonts::layout_job(detail, &typography::SMALL, color32(color::TEXT_2));
            job.wrap.max_width = width;
            let galley = ui.painter().layout_job(job);
            ui.label(galley);
        }
    }

    /// The outer height of the tooltip, frame and 1 px edge included.
    pub fn height(&self, ui: &Ui) -> f32 {
        let first = ui.painter().layout_job(self.first_line()).size().y;
        let detail = self.detail.map_or(0.0, |d| {
            let mut job = fonts::layout_job(d, &typography::SMALL, color32(color::TEXT_2));
            job.wrap.max_width = size::MENU_WIDTH - 2.0 * space::SPACE_2;
            space::SPACE_0_5 + ui.painter().layout_job(job).size().y
        });
        let frame = self.frame();
        first + detail + frame.inner_margin.sum().y + 2.0 * frame.stroke.width
    }

    /// Draws the tooltip in place (not floating), e.g. in the gallery.
    pub fn show_inline(&self, ui: &mut Ui) -> Response {
        self.frame()
            .show(ui, |ui| {
                ui.set_max_width(size::MENU_WIDTH);
                self.content(ui);
            })
            .response
    }

    /// Shows this tooltip while `response` is hovered (after [`crate::tooltip::DELAY`],
    /// [`crate::tooltip::GAP`] below it, or beside it where [`crate::tooltip::hover_side`] says
    /// so) and makes it the element's AccessKit description.
    pub fn attach(&self, response: &Response) {
        let description = self.description();
        super::accesskit(response, |node| node.set_description(description));
        let mut tooltip = egui::Tooltip::for_enabled(response);
        tooltip.popup = tooltip
            .popup
            .frame(self.frame())
            .gap(timing::GAP)
            .width(size::MENU_WIDTH);
        if timing::hover_side(&response.ctx) == timing::HoverSide::Beside {
            tooltip.popup = tooltip
                .popup
                .align(egui::RectAlign::LEFT)
                .align_alternatives(&[egui::RectAlign::RIGHT]);
        }
        tooltip.show(|ui| self.content(ui));
    }
}

/// The "Tooltip" card of the gallery: with shortcut, two lines, the inverted value tooltip, and a
/// live button whose tooltip appears after 600 ms.
pub fn gallery(ui: &mut Ui) {
    let short = Tip::new("Quantisieren").shortcut("Q");
    let two = Tip::new("Solo")
        .shortcut("S")
        .detail("Alt-Klick: nur diese Spur solo");
    // Bottom-aligned like the design's row (`align-items: flex-end`).
    let heights = [short.height(ui), two.height(ui), size::CONTROL_COMPACT];
    let max = heights.iter().copied().fold(0.0, f32::max);
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_6;
        super::sample(ui, "mit Shortcut", |ui| {
            ui.add_space(max - heights[0]);
            short.show_inline(ui);
        });
        super::sample(ui, "zweizeilig", |ui| {
            ui.add_space(max - heights[1]);
            two.show_inline(ui);
        });
        super::sample(ui, "Wert beim Ziehen · invertiert", |ui| {
            ui.add_space(max - heights[2]);
            timing::value_frame().show(ui, |ui| {
                ui.label(
                    fonts::rich_text("−3.2 dB", &timing::VALUE_TEXT)
                        .color(color32(color::SURFACE_0)),
                );
            });
        });
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_3;
        ui.add(super::button::Button::new("Duplizieren").shortcut("Strg D"));
        ui.add(super::button::Button::icon(
            komponist_tokens::Icon::Plus,
            "Hinzufügen",
        ));
        super::caption_text(ui, "live · Hover 600 ms");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_joins_the_parts() {
        let t = Tip::new("Solo")
            .shortcut("S")
            .detail("Alt-Klick: nur diese Spur solo");
        assert_eq!(t.description(), "Solo · S · Alt-Klick: nur diese Spur solo");
        assert_eq!(Tip::new("Mute").description(), "Mute");
    }

    #[test]
    fn frames() {
        let one = Tip::new("Quantisieren").shortcut("Q").frame();
        assert_eq!(f32::from(one.inner_margin.top) * 2.0 + 14.0, size::TOOLTIP);
        let two = Tip::new("Solo").detail("x").frame();
        assert_eq!(f32::from(two.inner_margin.top), space::SPACE_1_5);
    }
}
