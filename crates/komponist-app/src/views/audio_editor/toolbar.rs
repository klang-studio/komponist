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
/// Shared exclusive mode choice; only the model stores the selected mode.
pub fn mode(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    clip: &Clip,
    a: &AudioClip,
    width: f32,
    keyed: bool,
) {
    use komponist_ui::widgets::dropdown::{Dropdown, Item};
    let t = ctx.lang;
    let items = [
        Item::new(t.audio_clip_fixed()),
        Item::new(t.audio_clip_repitch()),
        Item::new(t.audio_clip_warp()),
    ];
    let mut selected = if a.warp_enabled() {
        2
    } else if a.time_mode == ClipTimeMode::RePitch {
        1
    } else {
        0
    };
    let mut widget = Dropdown::new(t.audio_clip_mode(), &mut selected, &items)
        .width(width)
        .compact();
    if keyed {
        widget = widget.label(t.audio_clip_mode());
    }
    let response = ui.add(widget).on_hover_text(t.audio_clip_mode_help());
    if response.clicked() {
        response.request_focus();
    }
    komponist_ui::widgets::accesskit(&response, |node| {
        node.set_description(t.audio_clip_mode_help())
    });
    if response.changed() {
        let action = match selected {
            2 => EditAction::SetAudioClipWarp {
                clip: clip.id,
                enabled: true,
            },
            n => EditAction::SetAudioClipProp {
                clip: clip.id,
                prop: komponist_model::edits::AudioClipProp::TimeMode(if n == 0 {
                    ClipTimeMode::Fixed
                } else {
                    ClipTimeMode::RePitch
                }),
            },
        };
        ctx.send(Action::Edit(action));
    }
}
pub fn duration_ratio(a: &AudioClip, project: &Project) -> f64 {
    if a.warp_enabled() {
        a.original_tempo.bpm / project.tempo.bpm
    } else {
        1.0 / a.speed(project.tempo)
    }
}
/// Read-only duration factor with an explicit output/source accessible description.
pub fn duration(
    ui: &mut Ui,
    a: &AudioClip,
    project: &Project,
    t: komponist_i18n::Lang,
    keyed: bool,
) {
    let ratio = format!("{:.2}", duration_ratio(a, project));
    let name = t.audio_clip_duration_help(&ratio);
    let response = ui.label(
        egui::RichText::new(if keyed {
            t.audio_clip_duration_value(&ratio)
        } else {
            format!("{ratio}×")
        })
        .font(fonts::font_id(&typography::VALUE_S)),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &name));
    komponist_ui::widgets::accesskit(&response, |node| node.set_label(name));
    response.on_hover_text(t.audio_clip_duration_help(&ratio));
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
/// Two compact rows from approved variant A; sub-640 docks may wrap the second group.
pub fn height(width: f32) -> f32 {
    size::PANEL_HEADER * if width < m::MODE_ROW_MIN { 3.0 } else { 2.0 }
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
    let first = Rect::from_min_size(rect.min, vec2(rect.width(), size::PANEL_HEADER));
    // Compact icon buttons are 20 high but retain CONTROL (24) width.
    let controls_w = size::CONTROL * 3.0 + space::SPACE_0_5 * 2.0;
    let values = Rect::from_min_max(
        first.min + vec2(space::SPACE_2, 0.0),
        first.max - vec2(controls_w + space::SPACE_3, 0.0),
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
                    for (field, width) in [
                        (Field::Gain, m::GAIN_W),
                        (Field::FadeIn, m::FADE_W),
                        (Field::FadeOut, m::FADE_W),
                        (Field::SourceStart, m::SOURCE_W),
                    ] {
                        fields::number(ui, ctx, clip, a, field, width, true);
                    }
                });
                if rect.width() >= m::FILE_ROW_MIN {
                    let filename = a.file.abs.file_name().unwrap_or_default().to_string_lossy();
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "{filename} · {}",
                                source.metadata(a, ctx.lang)
                            ))
                            .font(fonts::font_id(&typography::MONO_XS))
                            .color(color32(color::TEXT_3)),
                        )
                        .truncate(),
                    )
                    .on_hover_text(source.path(a).display().to_string());
                }
            });
        });
    let tools = Rect::from_min_max(
        egui::pos2(first.right() - controls_w - space::SPACE_2, first.top()),
        first.max - vec2(space::SPACE_2, 0.0),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(tools)
            .id_salt("audio-zoom")
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = space::SPACE_0_5;
    file_actions(&mut row, ctx, clip, a, source);
    for (icon, name, factor) in [
        (Icon::Minus, ctx.lang.audio_zoom_out(), 1.0 / m::ZOOM_STEP),
        (Icon::Plus, ctx.lang.audio_zoom_in(), m::ZOOM_STEP),
    ] {
        if row.add(Button::icon(icon, name).compact()).clicked() {
            ctx.ui.waveform.px_per_bar =
                (ctx.ui.waveform.px_per_bar * factor).clamp(m::PPB_MIN, m::PPB_MAX);
        }
    }
    let second = Rect::from_min_max(
        egui::pos2(rect.left() + space::SPACE_2, first.bottom()),
        rect.max - vec2(space::SPACE_2, 0.0),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(second)
            .id_salt("audio-mapping-controls")
            .accessibility_role(egui::Role::Toolbar)
            .accessibility_label(ctx.lang.audio_clip_mode())
            .layout(Layout::left_to_right(Align::Center).with_main_wrap(true)),
    );
    row.spacing_mut().item_spacing = vec2(space::SPACE_1, space::SPACE_1);
    row.set_clip_rect(second.intersect(ui.clip_rect()));
    row.add_enabled_ui(source.file.is_some(), |ui| {
        mode(ui, ctx, clip, a, m::MODE_W, true);
        fields::number(ui, ctx, clip, a, Field::Tempo, m::TEMPO_W, true);
        fields::number(ui, ctx, clip, a, Field::Bars, m::BARS_W, true);
    });
    duration(&mut row, a, project, ctx.lang, true);
    if rect.width() >= m::FILE_ROW_MIN {
        let text = if a.warp_enabled() {
            ctx.lang.audio_clip_preserve_pitch().to_owned()
        } else if a.time_mode == ClipTimeMode::RePitch {
            factor(a, project, ctx.lang)
        } else {
            ctx.lang.audio_clip_original_speed().to_owned()
        };
        row.label(
            egui::RichText::new(text)
                .font(fonts::font_id(&typography::MONO_XS))
                .color(color32(color::TEXT_3)),
        );
    }
}
