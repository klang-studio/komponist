//! The recovery prompt at start (#227; crash-recovery proposal, internal notes, §3, mockups 01 and
//! 02): „Ungespeicherte Änderungen wiederherstellen?“ for a copy another session left behind
//! ([`crate::recovery`]).
//!
//! The small-dialog layout of the unsaved-changes prompt ([`super::new_project::show_unsaved`]):
//! header with title (and „1 von 2“ when several copies wait) and ×, a body on `surface.1` with the
//! text, the facts (when the copy was written; the project file's state and path) and, only when
//! needed, a notice (the settings dialog's device notice on `surface.3`: a neutral dot when the file
//! was saved after the copy, a red one when the file is gone), and the footer with „Verwerfen“ ·
//! „Später“ · „Wiederherstellen“ (destructive left, wait in the middle, primary right). Enter is
//! „Wiederherstellen“, Esc and × are „Später“ (nothing is lost by closing). An unreadable copy
//! names the reason and offers „Verwerfen“ · „Später“, „Später“ primary.
//!
//! AccessKit: the dialog has role `Dialog` with the title as its name and the counter as its
//! description; text, facts and notice are labels in reading order; the buttons carry their labels.

use egui::{Color32, Context, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, typography};
use komponist_ui::px;
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::{self as w};

use super::metrics::{MONO_SMALL, dialog as dm, recovery as m};
use super::{ActionButton, button_row, dot, galley, label_node, modal, styled};

/// Which case the prompt shows (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Variant {
    /// a: a saved project with changes that are not in its file.
    Saved,
    /// b: a project that was never saved.
    Untitled,
    /// c: the project file was saved after the copy.
    FileNewer,
    /// d: the project file is gone.
    FileGone,
    /// e: the copy cannot be read; `reason` is a `project-reason-*` text.
    Unreadable {
        /// Why ("Dateiversion 7 ist neuer als dieses komponist").
        reason: String,
        /// The project was never saved (the file fact says so).
        untitled: bool,
    },
}

/// What the prompt shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The case.
    pub variant: Variant,
    /// The project name ("Night Drive").
    pub name: String,
    /// When the copy was written ("heute 14:32").
    pub copy_time: String,
    /// When the project file was saved ("heute 13:05"), for a project with a file.
    pub file_time: Option<String>,
    /// The project file's path as shown ("~/Musik/klang/Night Drive.komponist").
    pub path: Option<String>,
    /// „1 von 2“: which of how many copies.
    pub counter: Option<(usize, usize)>,
}

/// The answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Move the copy to `discarded/` and continue.
    Discard,
    /// Keep the copy, ask again at the next start; continue.
    Later,
    /// Open the copy.
    Recover,
}

impl Model {
    /// Whether the copy can be recovered (not variant e).
    pub fn recoverable(&self) -> bool {
        !matches!(self.variant, Variant::Unreadable { .. })
    }

    /// The body text.
    pub fn text(&self, t: Lang) -> String {
        match &self.variant {
            Variant::Untitled => t.recovery_text_untitled(&self.name),
            Variant::Unreadable { reason, .. } => t.recovery_text_unreadable(&self.name, reason),
            _ => t.recovery_text_saved(&self.name),
        }
    }

    /// The project file fact ("gespeichert heute 13:05 · ~/Musik/…", "nie gespeichert",
    /// "nicht gefunden · ~/Musik/…").
    pub fn file_fact(&self, t: Lang) -> String {
        let path = self.path.clone().unwrap_or_default();
        match (&self.variant, &self.file_time) {
            (Variant::Untitled | Variant::Unreadable { untitled: true, .. }, _) => {
                t.recovery_file_never().to_owned()
            }
            (Variant::FileGone, _) | (_, None) => t.recovery_file_gone(path),
            (_, Some(time)) => t.recovery_file_saved(time, path),
        }
    }

    /// The notice and whether it is an error (red dot), if the case has one.
    pub fn notice(&self, t: Lang) -> Option<(&'static str, bool)> {
        match self.variant {
            Variant::FileNewer => Some((t.recovery_notice_newer(), false)),
            Variant::FileGone => Some((t.recovery_notice_gone(), true)),
            _ => None,
        }
    }
}

/// Draws the prompt; returns the answer in the frame it is given.
pub fn show(ctx: &Context, model: &Model) -> Option<Answer> {
    let t = komponist_ui::lang(ctx);
    let mut out = None;
    let counter = model.counter.map(|(n, total)| t.recovery_counter(n, total));
    let width = super::fit(ctx, vec2(m::WIDTH, f32::INFINITY)).x;
    let response = modal(ctx, "recovery", color32(color::SURFACE_2), |ui| {
        ui.set_width(width);
        let title = t.recovery_title();
        ui.response()
            .widget_info(|| WidgetInfo::labeled(Role::Dialog, true, title));
        if let Some(counter) = counter.clone() {
            ui.ctx()
                .accesskit_node_builder(ui.response().id, |n| n.set_description(counter));
        }
        if super::header_with(ui, width, title, counter.as_deref()) {
            out = Some(Answer::Later);
        }
        let body = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(egui::Margin::same(dm::PADDING as i8));
        body.show(ui, |ui| {
            let inner = width - 2.0 * dm::PADDING;
            ui.set_width(inner);
            let text = model.text(t);
            let g = galley(
                ui,
                &text,
                w::font_at(&typography::TRACK, dm::TEXT_WEIGHT),
                dm::TEXT_LINE_HEIGHT,
                color32(color::TEXT_2),
                Some(inner),
            );
            super::paint_text(ui, g, &text);
            ui.add_space(m::TEXT_GAP);
            facts(
                ui,
                inner,
                &[
                    (t.recovery_fact_copy(), model.copy_time.clone()),
                    (t.recovery_fact_file(), model.file_fact(t)),
                ],
            );
            if let Some((notice_text, error)) = model.notice(t) {
                ui.add_space(m::TEXT_GAP);
                notice(ui, inner, notice_text, error);
            }
        });
        let footer = egui::Frame::new().inner_margin(egui::Margin::symmetric(
            dm::PADDING as i8,
            dm::FOOTER_PADDING_Y as i8,
        ));
        footer.show(ui, |ui| {
            let later_kind = if model.recoverable() {
                Kind::Secondary
            } else {
                Kind::Primary
            };
            let mut buttons = vec![
                (
                    ActionButton::dialog(t.recovery_discard(), Kind::Secondary, 2),
                    Answer::Discard,
                ),
                (
                    ActionButton::dialog(t.recovery_later(), later_kind, 2),
                    Answer::Later,
                ),
            ];
            if model.recoverable() {
                buttons.push((
                    ActionButton::dialog(t.recovery_recover(), Kind::Primary, 2),
                    Answer::Recover,
                ));
            }
            let answers: Vec<Answer> = buttons.iter().map(|(_, a)| *a).collect();
            let r = button_row(
                ui,
                width - 2.0 * dm::PADDING,
                buttons.into_iter().map(|(b, _)| b).collect(),
            );
            for (response, answer) in r.iter().zip(answers) {
                if response.clicked() {
                    out = Some(answer);
                }
            }
        });
        ui.allocate_space(vec2(width, 0.0));
    });
    let enter = ctx.memory(|mem| mem.focused().is_none())
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if out.is_none() {
        if enter {
            out = Some(if model.recoverable() {
                Answer::Recover
            } else {
                Answer::Later
            });
        } else if response.should_close() {
            out = Some(Answer::Later);
        }
    }
    out
}

/// The facts: labels in `type.small` / `text.3` in a column as wide as the widest, values in Mono
/// / `text.2`, one row each.
fn facts(ui: &mut Ui, width: f32, rows: &[(&str, String)]) {
    let labels: Vec<_> = rows
        .iter()
        .map(|(label, _)| styled(ui, label, &typography::SMALL, color32(color::TEXT_3), None))
        .collect();
    let column = labels.iter().map(|g| g.size().x).fold(0.0, f32::max).ceil() + m::FACT_GAP_X;
    let row_height = typography::SMALL.line_height;
    let ppp = ui.pixels_per_point();
    for (i, ((label, value), label_g)) in rows.iter().zip(labels).enumerate() {
        if i > 0 {
            ui.add_space(m::FACT_GAP_Y);
        }
        let (rect, _) = ui.allocate_exact_size(vec2(width, row_height), Sense::hover());
        let label_pos = w::text_pos(rect, &label_g, rect.left(), ppp);
        let label_rect = Rect::from_min_size(label_pos, label_g.size());
        ui.painter()
            .galley(label_pos, label_g, Color32::PLACEHOLDER);
        label_node(ui, label_rect, label, Role::Label);
        let value_g = super::styled_line(
            ui,
            value,
            &MONO_SMALL,
            color32(color::TEXT_2),
            width - column,
        );
        let value_pos = w::text_pos(rect, &value_g, rect.left() + column, ppp);
        let value_rect = Rect::from_min_size(value_pos, value_g.size());
        ui.painter()
            .galley(value_pos, value_g, Color32::PLACEHOLDER);
        label_node(ui, value_rect, value, Role::Label);
    }
}

/// The notice: `surface.3`, a 6 px dot (`text.3`, or `state.error` for `error`) at the first line,
/// the text in `type.ui` / `text.2` wrapped beside it.
fn notice(ui: &mut Ui, width: f32, text: &str, error: bool) {
    let [pad_x, pad_y] = m::NOTICE_PADDING;
    let text_x = pad_x + m::DOT + m::NOTICE_GAP;
    let g = galley(
        ui,
        text,
        komponist_ui::fonts::font_id(&typography::UI),
        m::NOTICE_LINE_HEIGHT,
        color32(color::TEXT_2),
        Some(width - text_x - pad_x),
    );
    let height = g.size().y + 2.0 * pad_y;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_3));
    let ink = if error {
        color::STATE_ERROR
    } else {
        color::TEXT_3
    };
    dot(
        ui,
        pos2(
            rect.left() + pad_x + m::DOT / 2.0,
            rect.top() + pad_y + m::NOTICE_LINE_HEIGHT / 2.0,
        ),
        m::DOT,
        color32(ink),
    );
    let pos = pos2(rect.left() + text_x, rect.top() + pad_y);
    let text_rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(
        px::snap_rect(text_rect, ui.pixels_per_point()).min,
        g,
        Color32::PLACEHOLDER,
    );
    label_node(ui, text_rect, text, Role::Label);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(variant: Variant) -> Model {
        Model {
            variant,
            name: "Night Drive".into(),
            copy_time: "heute 14:32".into(),
            file_time: Some("heute 13:05".into()),
            path: Some("~/Musik/klang/Night Drive.komponist".into()),
            counter: None,
        }
    }

    #[test]
    fn the_variants_say_what_the_spec_says() {
        let t = Lang::De;
        let a = model(Variant::Saved);
        assert_eq!(
            a.text(t),
            "komponist wurde nicht regulär beendet. Von „Night Drive“ gibt es eine \
             Sicherungskopie mit Änderungen, die noch nicht in der Projektdatei stehen."
        );
        assert_eq!(
            a.file_fact(t),
            "gespeichert heute 13:05 · ~/Musik/klang/Night Drive.komponist"
        );
        assert_eq!(a.notice(t), None);
        assert!(a.recoverable());
        let b = Model {
            name: "Unbenannt".into(),
            file_time: None,
            path: None,
            ..model(Variant::Untitled)
        };
        assert_eq!(
            b.text(t),
            "komponist wurde nicht regulär beendet. Vom ungespeicherten Projekt „Unbenannt“ \
             gibt es eine Sicherungskopie."
        );
        assert_eq!(b.file_fact(t), "nie gespeichert");
        assert_eq!(
            model(Variant::FileNewer).notice(t).map(|(_, e)| e),
            Some(false)
        );
        let d = model(Variant::FileGone);
        assert_eq!(d.notice(t).map(|(_, e)| e), Some(true));
        assert_eq!(
            d.file_fact(t),
            "nicht gefunden · ~/Musik/klang/Night Drive.komponist"
        );
        let e = model(Variant::Unreadable {
            reason: t.project_reason_too_new(7),
            untitled: false,
        });
        assert!(!e.recoverable());
        assert_eq!(
            e.text(t),
            "komponist wurde nicht regulär beendet. Die Sicherungskopie von „Night Drive“ \
             lässt sich nicht lesen: Dateiversion 7 ist neuer als dieses komponist."
        );
        assert_eq!(Lang::En.recovery_counter(1, 2), "1 of 2");
        assert_eq!(t.recovery_counter(1, 2), "1 von 2");
    }
}
