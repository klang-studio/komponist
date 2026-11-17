//! Approved 28-point audio toolbar: envelopes/source mapping, file actions and zoom.
use super::{
    fields::{self, Field},
    metrics as m,
    source::Source,
};
use crate::actions::ProjectAction;
use crate::views::ViewCtx;
use crate::{Action, EditAction};
use egui::{Align, Layout, Rect, Ui, UiBuilder, vec2};
use komponist_model::{AudioClip, Clip, ClipTimeMode, Project};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, size, space, typography};
use komponist_ui::widgets::{
    button::Button,
    context_menu::{ContextMenu, Entry},
};
use komponist_ui::{fonts, px};

#[derive(Clone, Copy, Debug)]
enum FileAction {
    Replace,
    Show,
    Copy,
}
/// File menu shared with the inspector.
pub fn file_actions(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    clip: &Clip,
    a: &AudioClip,
    source: &Source,
) {
    let t = ctx.lang;
    let path = source.path(a);
    let response = ui.add(Button::icon(Icon::Ellipsis, t.audio_file_actions()).compact());
    if response.clicked() {
        ContextMenu::<FileAction>::open_at(ui.ctx(), response.id, response.rect.left_bottom());
    }
    let entries = [
        Entry::action(t.audio_file_replace(), FileAction::Replace),
        Entry::action(t.audio_file_show(), FileAction::Show),
        Entry::action(t.audio_file_copy_path(), FileAction::Copy),
    ];
    if let Some(action) = ContextMenu::new(t.audio_file_actions(), &entries).show_on(&response) {
        match action {
            FileAction::Replace => ctx.send(Action::Project(ProjectAction::ReplaceAudio(clip.id))),
            FileAction::Show => {
                if let Some(folder) = path.parent() {
                    ctx.send(Action::Project(ProjectAction::ShowInFolder(
                        folder.to_owned(),
                    )));
                }
            }
            FileAction::Copy => ui.ctx().copy_text(path.display().to_string()),
        }
    }
}
/// Missing-source recovery actions remain available when value fields are disabled.
pub fn recovery(ui: &mut Ui, ctx: &mut ViewCtx<'_>, clip: &Clip, a: &AudioClip, source: &Source) {
    if ui
        .add(Button::primary(ctx.lang.audio_file_replace()).width(m::BANNER_REPLACE_W))
        .clicked()
    {
        ctx.send(Action::Project(ProjectAction::ReplaceAudio(clip.id)));
    }
    if ui
        .add(Button::new(ctx.lang.audio_file_copy_path()).width(m::BANNER_COPY_W))
        .clicked()
    {
        ui.ctx().copy_text(source.path(a).display().to_string());
    }
}
/// Re-Pitch switch shared with inspector (button style follows its Loop field).
pub fn repitch(ui: &mut Ui, ctx: &mut ViewCtx<'_>, clip: &Clip, a: &AudioClip) {
    let on = a.time_mode == ClipTimeMode::RePitch;
    if ui
        .add(Button::new(ctx.lang.audio_clip_repitch()).on(on).compact())
        .clicked()
    {
        ctx.send(Action::Edit(EditAction::SetAudioClipProp {
            clip: clip.id,
            prop: komponist_model::edits::AudioClipProp::TimeMode(if on {
                ClipTimeMode::Fixed
            } else {
                ClipTimeMode::RePitch
            }),
        }));
    }
}
/// Ratio/pitch information computed from the actual playback mapping.
pub fn factor(a: &AudioClip, project: &Project, t: komponist_i18n::Lang) -> String {
    let ratio = a.speed(project.tempo);
    let signed = |v: f64| format!("{v:+.2}").replace('-', "−");
    t.audio_factor(
        format!("{}%", signed((ratio - 1.0) * 100.0)),
        signed(12.0 * ratio.log2()),
    )
}
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    clip: &Clip,
    a: &AudioClip,
    source: &Source,
) {
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_2));
    px::hline(
        ui.painter(),
        rect.x_range(),
        rect.bottom() - px::line_width(ui.pixels_per_point()),
        color32(color::SURFACE_0),
    );
    let zoom_w = size::CONTROL_COMPACT * 2.0 + space::SPACE_0_5;
    let values = Rect::from_min_max(
        rect.min + vec2(space::SPACE_2, 0.0),
        rect.max - vec2(zoom_w + space::SPACE_3, 0.0),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(values)
            .id_salt("audio-toolbar-values")
            .accessibility_role(egui::Role::Toolbar)
            .accessibility_label(ctx.lang.audio_waveform_controls())
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.set_clip_rect(values.intersect(ui.clip_rect()));
    crate::views::group_bounds(&row, values);
    egui::ScrollArea::horizontal()
        .scroll_source(egui::scroll_area::ScrollSource {
            mouse_wheel: !komponist_ui::widgets::number_field::wheel_over_control(ui.ctx()),
            ..Default::default()
        })
        .id_salt("audio-toolbar-scroll")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .max_height(size::PANEL_HEADER)
        .show(&mut row, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = space::SPACE_1;
                ui.add_enabled_ui(source.file.is_some(), |ui| {
                    ui.horizontal_centered(|ui| {
                        for (field, width) in [
                            (Field::Gain, m::GAIN_W),
                            (Field::FadeIn, m::FADE_W),
                            (Field::FadeOut, m::FADE_W),
                            (Field::SourceStart, m::SOURCE_W),
                        ] {
                            fields::number(ui, ctx, clip, a, field, width, true);
                        }
                        repitch(ui, ctx, clip, a);
                        fields::number(ui, ctx, clip, a, Field::Tempo, m::TEMPO_W, true);
                        fields::number(ui, ctx, clip, a, Field::Bars, m::BARS_W, true);
                    });
                });
                if a.time_mode == ClipTimeMode::RePitch {
                    let text = factor(a, project, ctx.lang);
                    ui.label(
                        egui::RichText::new(text)
                            .font(fonts::font_id(&typography::MONO_XS))
                            .color(color32(color::TEXT_3)),
                    );
                }
                let filename = a.file.abs.file_name().unwrap_or_default().to_string_lossy();
                let meta = source.metadata(a, ctx.lang);
                let text = format!("{filename} · {meta}");
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&text)
                            .font(fonts::font_id(&typography::MONO_XS))
                            .color(color32(color::TEXT_3)),
                    )
                    .truncate(),
                )
                .on_hover_text(source.path(a).display().to_string());
                file_actions(ui, ctx, clip, a, source);
            });
        });
    let zoom = Rect::from_min_max(
        egui::pos2(rect.right() - zoom_w - space::SPACE_2, rect.top()),
        rect.max - vec2(space::SPACE_2, 0.0),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(zoom)
            .id_salt("audio-zoom")
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = space::SPACE_0_5;
    for (icon, name, factor) in [
        (Icon::Minus, ctx.lang.audio_zoom_out(), 1.0 / m::ZOOM_STEP),
        (Icon::Plus, ctx.lang.audio_zoom_in(), m::ZOOM_STEP),
    ] {
        if row.add(Button::icon(icon, name).compact()).clicked() {
            ctx.ui.waveform.px_per_bar =
                (ctx.ui.waveform.px_per_bar * factor).clamp(m::PPB_MIN, m::PPB_MAX);
        }
    }
}
