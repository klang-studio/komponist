//! Portable About dialog, using the existing small-dialog layout (#108).
use egui::{Context, Frame, Margin};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, space, typography};

use super::metrics::{about, dialog as m};
use super::{fit, header, modal, paint_text, styled};

/// The dark-background variant used in the README.
const LOGO: &[u8] = include_bytes!("../../../../assets/logo/06-spurmarke/logo.svg");

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
                    let logo_width = ui.available_width().min(about::LOGO_MAX_WIDTH);
                    ui.vertical_centered(|ui| {
                        ui.add(
                            egui::Image::from_bytes("bytes://komponist-logo.svg", LOGO)
                                .fit_to_exact_size(egui::vec2(
                                    logo_width,
                                    logo_width / about::LOGO_ASPECT,
                                ))
                                .alt_text(komponist_ui::WORDMARK),
                        );
                    });
                    ui.add_space(space::SPACE_3);
                    for text in [
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
