//! Browser and desktop file drops, with grid placement, overlap preview and refusals.

use std::path::PathBuf;

use egui::{CursorIcon, Id, Rect, Stroke, Ui, pos2};
use komponist_model::{Project, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, space};
use komponist_ui::widgets::metrics::timeline as m;
use komponist_ui::{px, tooltip};

use super::{Arrangement, geometry::Geometry, metrics};
use crate::actions::{Action, ProjectAction, UiAction};
use crate::audio_import::{AudioDrag, snap_tick};
use crate::ui_state::StatusKind;
use crate::views::ViewCtx;

/// Native drags have paths only; read their preview metadata off the UI thread.
#[derive(Debug, Default)]
pub(super) struct HoverFile {
    path: Option<PathBuf>,
    file: Option<komponist_model::AudioFile>,
    pending: Option<std::sync::mpsc::Receiver<Option<komponist_model::AudioFile>>>,
}

impl HoverFile {
    fn get(
        &mut self,
        path: &std::path::Path,
        ctx: &egui::Context,
    ) -> Option<&komponist_model::AudioFile> {
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.file = None;
            let (tx, rx) = std::sync::mpsc::channel();
            let path = path.to_path_buf();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::audio_import::linked_file(&path, None).ok());
                ctx.request_repaint();
            });
            self.pending = Some(rx);
        }
        if let Some(file) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.file = file;
            self.pending = None;
        }
        self.file.as_ref()
    }
}

impl Arrangement {
    pub(super) fn drop_audio(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
    ) {
        let browser = egui::DragAndDrop::payload::<AudioDrag>(ui.ctx());
        let paths: Vec<PathBuf> = browser
            .as_ref()
            .map(|d| d.paths.clone())
            .unwrap_or_else(|| {
                ui.input(|i| {
                    let files = if i.raw.dropped_files.is_empty() {
                        i.raw
                            .hovered_files
                            .iter()
                            .filter_map(|f| f.path.clone())
                            .collect::<Vec<_>>()
                    } else {
                        i.raw
                            .dropped_files
                            .iter()
                            .map(|f| f.path().to_path_buf())
                            .collect()
                    };
                    files
                        .into_iter()
                        .filter(|p| crate::dialogs::file::is_audio_file(p))
                        .collect()
                })
            });
        if paths.is_empty() {
            return;
        }
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        if !g.tracks.contains(pos) {
            return;
        }
        let track = g.row_at(pos.y).and_then(|i| project.tracks.get(i));
        let target = track.map(|t| t.id);
        let released = ui.input(|i| i.pointer.any_released() || !i.raw.dropped_files.is_empty());
        if let Some(track) = track
            && !track.is_audio()
        {
            let reason = if track.is_bus() {
                ctx.lang.routing_no_clips()
            } else {
                ctx.lang.status_drop_refused_midi()
            };
            ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
            if let Some(row) = g.row_at(pos.y) {
                let rect = g.lane(row).intersect(ui.clip_rect());
                let painter = ui.painter().with_clip_rect(rect);
                let mut x = rect.left() - rect.height();
                while x < rect.right() {
                    painter.line_segment(
                        [pos2(x, rect.bottom()), pos2(x + rect.height(), rect.top())],
                        Stroke::new(
                            metrics::AUDIO_DROP_HATCH_WIDTH,
                            color32(color::STATE_ERROR_SURFACE),
                        ),
                    );
                    x += space::SPACE_2;
                }
                tooltip::show_value(
                    ui.ctx(),
                    Id::unique("audio_drop_refused"),
                    rect,
                    tooltip::Placement::Above,
                    reason,
                );
            }
            if released {
                egui::DragAndDrop::clear_payload(ui.ctx());
                ui.input_mut(|i| i.raw.dropped_files.clear());
                ctx.send(Action::Ui(UiAction::ShowStatus {
                    text: reason.to_owned(),
                    kind: StatusKind::Error,
                }));
            }
            return;
        }
        let raw = if pos.x < g.lane_x.min {
            Tick::ZERO
        } else {
            Tick(g.tick_at(pos.x).round().max(0.0) as i64)
        };
        let at = if ctx.ui.arrangement.snap {
            snap_tick(raw, ctx.ui.arrangement.grid.ticks(project.meter), false)
        } else {
            raw
        };
        let at = if target.is_none() && paths.len() > 1 {
            Tick::ZERO
        } else {
            at
        };
        let native = if browser.is_none() {
            self.audio_hover.get(&paths[0], ui.ctx())
        } else {
            None
        };
        let length = browser
            .as_ref()
            .and_then(|d| d.files.first())
            .or(native)
            .map(|file| {
                Tick(
                    project
                        .tempo_map()
                        .seconds_to_ticks(file.duration_secs())
                        .floor() as i64,
                )
            })
            .unwrap_or(Tick(project.meter.ticks_per_bar()));
        let row = g.row_at(pos.y);
        let y = row.map(|i| g.rows[i].y).unwrap_or_else(|| {
            let top = g.rows_end.max(g.tracks.top());
            egui::Rangef::new(
                top,
                (top + komponist_tokens::size::track::DEFAULT).min(g.tracks.bottom()),
            )
        });
        let rect =
            Rect::from_x_y_ranges(g.axis.x(at)..=g.axis.x(at + length), y).intersect(g.lanes());
        let painter = ui
            .painter()
            .with_clip_rect(g.lanes().intersect(ui.clip_rect()));
        px::fill(&painter, rect, 0.0, color32(color::ACCENT_SURFACE));
        px::dashed_border(
            &painter,
            rect,
            color32(color::ACCENT),
            metrics::NEW_TRACK_DASH,
        );
        let label = browser.as_ref().map_or_else(
            || {
                paths[0]
                    .file_name()
                    .unwrap_or(paths[0].as_os_str())
                    .to_string_lossy()
                    .into_owned()
            },
            |drag| drag.name.clone(),
        );
        let style = komponist_tokens::typography::UI;
        let galley = crate::views::elided_galley(
            ui,
            &label,
            komponist_ui::fonts::font_id(&style),
            style.line_height,
            (rect.width() - 2.0 * m::CLIP_HEAD_PAD_X).max(0.0),
        );
        painter.with_clip_rect(rect).galley(
            rect.min + egui::vec2(m::CLIP_HEAD_PAD_X, m::CLIP_INSET_Y),
            galley,
            color32(color::TEXT_1),
        );
        let overlap = track.is_some_and(|track| {
            track
                .clips
                .iter()
                .any(|clip| clip.start < at + length && clip.end() > at)
        });
        if let Some(track) = track {
            for clip in &track.clips {
                let start = clip.start.max(at);
                let end = clip.end().min(at + length);
                if end > start {
                    let cut = Rect::from_x_y_ranges(g.axis.x(start)..=g.axis.x(end), y)
                        .intersect(g.lanes());
                    px::fill(
                        &painter,
                        cut,
                        0.0,
                        color32(color::ACCENT).gamma_multiply(m::CUT_SHADE_ALPHA),
                    );
                    px::vline(&painter, cut.left(), cut.y_range(), color32(color::ACCENT));
                }
            }
        } else {
            px::inner_border(
                ui.painter(),
                Rect::from_x_y_ranges(
                    g.header_x,
                    egui::Rangef::new(
                        y.min,
                        (y.min + metrics::NEW_TRACK_MIN_HEIGHT).min(g.tracks.bottom()),
                    ),
                ),
                color32(color::ACCENT),
            );
        }
        if overlap {
            tooltip::show_value(
                ui.ctx(),
                Id::unique("audio_drop_overlap"),
                rect,
                tooltip::Placement::Above,
                ctx.lang.audio_clip_cuts_below(),
            );
        }
        if released {
            egui::DragAndDrop::clear_payload(ui.ctx());
            ui.input_mut(|i| i.raw.dropped_files.clear());
            ctx.send(Action::Project(ProjectAction::ImportAudioPaths {
                paths,
                target,
                at,
            }));
        }
    }
}
