//! Portable About dialog, using the existing small-dialog layout (#108).
use egui::{Context, Frame, Margin};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, space, typography};

use super::metrics::dialog as m;
use super::{fit, header, modal, paint_text, styled};

/// Whether the user closed the dialog (close button, Escape or scrim).
pub fn show(ctx: &Context, lang: Lang) -> bool {
    let width = fit(ctx, egui::vec2(m::WIDTH, ctx.content_rect().height())).x;
    let response = modal(
        ctx,
        lang.app_menu_about(),
        color32(color::SURFACE_2),
        |ui| {
            ui.set_width(width);
            let close = header(ui, width, lang.app_menu_about());
            Frame::new()
                .inner_margin(Margin::same(m::PADDING as i8))
                .show(ui, |ui| {
                    ui.set_width((width - 2.0 * m::PADDING).max(space::SPACE_1));
                    for text in [
                        komponist_ui::WORDMARK.to_owned(),
                        lang.about_version(env!("CARGO_PKG_VERSION")),
                        lang.about_license(env!("CARGO_PKG_LICENSE")),
                    ] {
                        let galley = styled(
                            ui,
                            &text,
                            &typography::UI,
                            color32(color::TEXT_1),
                            Some(ui.available_width()),
                        );
                        paint_text(ui, galley, &text);
                        ui.add_space(space::SPACE_2);
                    }
                });
            close
        },
    );
    response
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Dialog, true, lang.app_menu_about()));
    response.inner || response.should_close()
}
