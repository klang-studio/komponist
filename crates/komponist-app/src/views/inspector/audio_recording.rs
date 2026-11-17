//! Read-only live capture summary; unfinished WAVs are never opened by the file pool.

use super::{Meta, field_grid, field_key, frame, read_only_field};
use crate::views::ViewCtx;
use egui::{Rangef, Ui};
use komponist_model::format::{fmt_bar_pos, fmt_length};
use komponist_model::{ClipContent, Project, TrackId};
use komponist_tokens::{color, typography};

pub(super) fn show(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
    track: TrackId,
) {
    let Some(recording) = ctx.audio_recording else {
        return;
    };
    let Some(clip) = recording.clip(track).cloned() else {
        return;
    };
    let Some(progress) = recording.progress(track) else {
        return;
    };
    let Some(source) = project.track(track) else {
        return;
    };
    let ClipContent::Audio(audio) = &clip.content else {
        return;
    };
    let t = ctx.lang;
    let input = crate::views::audio_inputs::input_label(t, project, source.audio.input);
    let takes = progress.lap.saturating_add(1).to_string();
    frame(
        ui,
        x,
        y,
        t.inspector_section_clip(),
        Meta::Record(t.inspector_clip_recording()),
        |ui, col| {
            super::name_row(
                ui,
                col,
                clip.name.strip_prefix("● ").unwrap_or(&clip.name),
                color::STATE_RECORD,
                false,
                false,
            );
            let cells = field_grid(col, 4);
            let values = [
                (
                    t.inspector_clip_start(),
                    fmt_bar_pos(clip.start, project.meter),
                ),
                (
                    t.inspector_clip_length(),
                    fmt_length(clip.length, project.meter),
                ),
                (t.new_track_input(), input),
                (t.inspector_clip_takes(), takes),
            ];
            for ((key, field), (name, value)) in cells.into_iter().zip(values) {
                field_key(ui, key, name);
                read_only_field(ui, field, name, &value, "");
            }
        },
    );
    let path = progress.path.display().to_string();
    let provisional = ctx
        .provisional_recordings
        .filter(|folder| progress.path.starts_with(folder));
    let state = if provisional.is_some() {
        t.audio_file_provisional()
    } else {
        t.audio_file_in_project()
    };
    frame(ui, x, y, t.audio_file(), Meta::Text(state), |ui, col| {
        let rect = col.take(typography::MONO_XS.line_height * 2.0);
        let g = crate::views::elided_galley(
            ui,
            &path,
            komponist_ui::fonts::font_id(&typography::MONO_XS),
            typography::MONO_XS.line_height,
            rect.width(),
        );
        ui.painter().galley(
            rect.min,
            g,
            komponist_tokens::egui_support::color32(color::TEXT_3),
        );
        let response = ui.interact(
            rect,
            ui.scope_id().with("live-audio-file"),
            egui::Sense::hover(),
        );
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::Role::Label, true, t.audio_file_path());
            info.current_text_value = Some(path.clone());
            info
        });
        komponist_ui::widgets::accesskit(&response, |node| node.set_label(t.audio_file_path()));
        let detail = provisional.map_or_else(
            || path.clone(),
            |folder| {
                format!(
                    "{}\n{}",
                    path,
                    t.audio_file_provisional_note(folder.display())
                )
            },
        );
        response.on_hover_text(&detail);
        let rect = col.take(typography::SMALL.line_height);
        let seconds =
            (progress.frames as f64 / f64::from(audio.file.sample_rate.max(1))).round() as u64;
        let duration = format!("{:02}:{:02}", seconds / 60, seconds % 60);
        let metadata = t.audio_file_metadata(
            crate::project_io::fmt_bpm(f64::from(audio.file.sample_rate) / 1000.0),
            if progress.channels > 1 {
                t.audio_channel_stereo()
            } else {
                t.audio_channel_mono()
            },
            duration,
        );
        super::label(ui, rect, &metadata, &typography::SMALL, None, color::TEXT_3);
    });
}
