//! Audio clip and linked-file inspector sections (approved audio proposal §4).
use super::{Col, Meta, field_grid, field_key, frame, label, place, switch_field};
use crate::views::{
    ViewCtx,
    audio_editor::{
        fields::{self, Field},
        source::Source,
        toolbar,
    },
};
use egui::{Rangef, Ui};
use komponist_model::{AudioClip, Clip, ClipContent, ClipId, Project};
use komponist_tokens::{color, size, typography};

pub(super) fn fields(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    col: &mut Col,
    clip: &Clip,
    a: &AudioClip,
) {
    let cells = field_grid(col, 9);
    super::clip::position_fields(ui, ctx, project, &cells, clip);
    for (index, kind) in [
        (2, Field::SourceStart),
        (3, Field::Gain),
        (4, Field::FadeIn),
        (5, Field::FadeOut),
        (7, Field::Tempo),
        (8, Field::Bars),
    ] {
        let (key, field) = cells[index];
        field_key(ui, key, kind.label(ctx.lang));
        place(ui, field, kind, |ui| {
            fields::number(ui, ctx, clip, a, kind, field.width(), false)
        });
    }
    let (key, field) = cells[6];
    field_key(ui, key, ctx.lang.audio_clip_repitch());
    let on = a.time_mode == komponist_model::ClipTimeMode::RePitch;
    if switch_field(
        ui,
        ctx.lang,
        field,
        ctx.lang.audio_clip_repitch(),
        on,
        false,
    ) {
        ctx.send(crate::Action::Edit(crate::EditAction::SetAudioClipProp {
            clip: clip.id,
            prop: komponist_model::edits::AudioClipProp::TimeMode(if on {
                komponist_model::ClipTimeMode::Fixed
            } else {
                komponist_model::ClipTimeMode::RePitch
            }),
        }));
    }
    if on {
        let r = col.take(typography::MONO_XS.line_height);
        label(
            ui,
            r,
            &toolbar::factor(a, project, ctx.lang),
            &typography::MONO_XS,
            None,
            color::TEXT_3,
        );
    }
}
pub(super) fn file_section(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
    id: ClipId,
    source: &mut Source,
) {
    let Some((_, clip)) = project.find_clip(id) else {
        return;
    };
    let ClipContent::Audio(a) = &clip.content else {
        return;
    };
    source.refresh(a, ctx, ui.ctx());
    let path = source.path(a);
    let state = if source.file.is_none() {
        ctx.lang.audio_clip_missing_short()
    } else if ctx.project_dir.is_some_and(|dir| path.starts_with(dir)) {
        ctx.lang.audio_file_in_project()
    } else {
        ctx.lang.audio_file_external()
    };
    frame(
        ui,
        x,
        y,
        ctx.lang.audio_file(),
        Meta::Text(state),
        |ui, col| {
            let r = col.take(typography::MONO_XS.line_height * 2.0);
            let text = path.display().to_string();
            let response = ui.put(
                r,
                egui::Label::new(
                    egui::RichText::new(&text)
                        .font(komponist_ui::fonts::font_id(&typography::MONO_XS))
                        .color(komponist_tokens::egui_support::color32(color::TEXT_3)),
                )
                .wrap(),
            );
            response.on_hover_text(&text);
            let r = col.take(typography::SMALL.line_height * 2.0);
            label(
                ui,
                r,
                &source.metadata(a, ctx.lang),
                &typography::SMALL,
                None,
                color::TEXT_3,
            );
            let r = col.take(size::CONTROL);
            place(ui, r, "audio-file-actions", |ui| {
                ui.horizontal(|ui| {
                    toolbar::file_actions(ui, ctx, clip, a, source);
                    if ui
                        .add(
                            komponist_ui::widgets::button::Button::new(
                                ctx.lang.audio_file_replace(),
                            )
                            .small(),
                        )
                        .clicked()
                    {
                        ctx.send(crate::Action::Project(
                            crate::actions::ProjectAction::ReplaceAudio(id),
                        ));
                    }
                });
            });
        },
    );
}
