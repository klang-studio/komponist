//! Audio takes: selection, inline rename, deletion, duration and measured mini waveforms.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_audiofile::{FilePool, PeakState, PoolFile};
use komponist_model::{AudioClip, AudioFile, Clip, ClipId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::timeline::{WaveMesh, WaveSpec};
use komponist_ui::widgets::rename_field::{RenameField, RenameOutcome, RenameState};
use komponist_ui::widgets::{self as w};
use komponist_ui::{fonts, icons, px};

use super::Col;
use crate::actions::{Action, EditAction};
use crate::views::ViewCtx;

/// Two lines and a miniature waveform (audio proposal mockup 10).
const ROW: f32 = 44.0;
const WAVE_HEIGHT: f32 = 18.0;

#[derive(Debug, Default)]
pub(super) struct State {
    files: HashMap<AudioFile, Arc<PoolFile>>,
    clip: Option<ClipId>,
    rename: Option<(ClipId, usize, RenameState)>,
    palettes: HashMap<u16, komponist_tokens::TrackPalette>,
}

pub(super) fn show(
    state: &mut State,
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    col: &mut Col,
    clip: &Clip,
    audio: &AudioClip,
) {
    if audio.takes.len() < 2 {
        return;
    }
    if state.clip != Some(clip.id) {
        state.files.clear();
        state.rename = None;
        state.clip = Some(clip.id);
    }
    state
        .files
        .retain(|file, _| audio.takes.iter().any(|take| &take.file == file));
    let t = ctx.lang;
    let key = col.take(typography::SMALL.line_height);
    super::field_key(ui, key, t.inspector_clip_takes());
    for (i, take) in audio.takes.iter().enumerate() {
        let rect = col.take(ROW);
        let active = audio.active_take == i;
        let response = ui.interact(
            rect,
            ui.scope_id().with(("audio_take", clip.id.0, i)),
            Sense::click(),
        );
        let name = if take.name.is_empty() {
            t.inspector_take(i + 1)
        } else {
            take.name.clone()
        };
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, active, &name));
        px::fill(
            ui.painter(),
            rect,
            radius::S,
            color32(if active {
                color::ACCENT_SURFACE
            } else {
                color::SURFACE_2
            }),
        );
        if response.clicked() {
            response.request_focus();
        }
        if response.has_focus()
            && state.rename.is_none()
            && ui.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
            })
        {
            ctx.send(Action::Edit(EditAction::RemoveAudioTake {
                clip: clip.id,
                take: i,
            }));
        }
        if response.has_focus() {
            komponist_ui::focus::paint_ring(ui.painter(), rect, radius::S);
        }
        if response.double_clicked() {
            state.rename = Some((clip.id, i, RenameState::new(&name)));
        } else if response.clicked() && !active {
            ctx.send(Action::Edit(EditAction::SelectAudioTake {
                clip: clip.id,
                take: i,
            }));
        }
        let dot = pos2(rect.left() + space::SPACE_2, rect.top() + space::SPACE_2);
        ui.painter().circle_stroke(
            dot,
            space::SPACE_1,
            egui::Stroke::new(
                px::line_width(ui.pixels_per_point()),
                color32(color::TEXT_3),
            ),
        );
        if active {
            ui.painter()
                .circle_filled(dot, space::SPACE_0_5, color32(color::ACCENT));
        }
        let remove = Rect::from_min_size(
            pos2(
                rect.right() - size::ICON - space::SPACE_1,
                rect.top() + space::SPACE_1,
            ),
            vec2(size::ICON, size::ICON),
        );
        let duration = take.file.duration_secs();
        let seconds = duration.round() as u64;
        let duration = format!("{:02}:{:02}", seconds / 60, seconds % 60);
        w::accesskit(&response, |node| node.set_description(&duration));
        let g = w::styled_galley(ui, &duration, &typography::VALUE_S);
        let duration_x = remove.left() - space::SPACE_1 - g.size().x;
        ui.painter().galley(
            pos2(duration_x, rect.top() + space::SPACE_1),
            g,
            color32(color::TEXT_3),
        );
        let name_rect = Rect::from_min_max(
            pos2(rect.left() + space::SPACE_4, rect.top() + space::SPACE_1),
            pos2(
                duration_x - space::SPACE_1,
                rect.top() + space::SPACE_1 + typography::UI.line_height,
            ),
        );
        let g = crate::views::elided_galley(
            ui,
            &name,
            fonts::font_id(&typography::UI),
            typography::UI.line_height,
            name_rect.width(),
        );
        ui.painter()
            .galley(name_rect.min, g, color32(color::TEXT_1));
        let delete = ui.interact(remove, response.id.with("remove"), Sense::click());
        icons::paint_in(ui, remove, Icon::X, color32(color::TEXT_3));
        let delete_name = t.audio_take_remove(&name);
        delete.widget_info(|| WidgetInfo::labeled(Role::Button, true, &delete_name));
        komponist_ui::widgets::tooltip::Tip::new(&delete_name).attach(&delete);
        if delete.clicked() {
            ctx.send(Action::Edit(EditAction::RemoveAudioTake {
                clip: clip.id,
                take: i,
            }));
        }
        let wave = Rect::from_min_size(
            pos2(
                name_rect.left(),
                rect.bottom() - WAVE_HEIGHT - space::SPACE_1,
            ),
            vec2(
                rect.right() - space::SPACE_2 - name_rect.left(),
                WAVE_HEIGHT,
            ),
        );
        let entry = state.files.entry(take.file.clone());
        let pool = match entry {
            std::collections::hash_map::Entry::Occupied(entry) => Some(entry.into_mut()),
            std::collections::hash_map::Entry::Vacant(entry) => take
                .file
                .resolve(ctx.project_dir)
                .and_then(|path| FilePool::global().open(&path).ok())
                .map(|file| entry.insert(file)),
        };
        if let Some(file) = pool {
            let peaks = match file.peaks_state() {
                PeakState::Ready(peaks) => Some(peaks),
                PeakState::Idle => {
                    let repaint = ui.ctx().clone();
                    let _ = file.request_peaks(move || repaint.request_repaint());
                    None
                }
                _ => None,
            };
            if let Some(peaks) = peaks {
                let spec = WaveSpec {
                    width: wave.width(),
                    px_per_tick: wave.width() / clip.length.0.max(1) as f32,
                    body_height: wave.height(),
                    offset: take.offset,
                    frames: take.file.frames.saturating_sub(take.offset),
                    gain: 1.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                };
                let mesh = WaveMesh::from_peaks(&peaks, &spec);
                let hue = ctx
                    .project
                    .find_clip(clip.id)
                    .map_or(0, |(track, _)| track.hue.0);
                let palette = state
                    .palettes
                    .entry(hue)
                    .or_insert_with(|| komponist_tokens::TrackPalette::new(hue));
                mesh.paint(
                    ui.painter(),
                    wave,
                    color32(palette.wave_peak),
                    color32(palette.wave_rms),
                    color32(color::METER_CLIP),
                );
            }
        }
        if let Some((id, index, rename)) = &mut state.rename
            && *id == clip.id
            && *index == i
        {
            match RenameField::new(
                rename,
                response.id.with("rename"),
                t.audio_take_rename(),
                &name,
                fonts::font_id(&typography::UI),
            )
            .show(ui, name_rect)
            {
                RenameOutcome::Commit(name) => {
                    ctx.send(Action::Edit(EditAction::RenameAudioTake {
                        clip: clip.id,
                        take: i,
                        name,
                    }));
                    state.rename = None;
                }
                RenameOutcome::Cancel => state.rename = None,
                RenameOutcome::Editing => {}
            }
        }
    }
}
