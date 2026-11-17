//! Linked audio import and creation of audio tracks (audio proposal §§6–7, §17).

use std::path::{Path, PathBuf};

use komponist_model::edits::{self, NewAudioTrack, NewClip, NewClipContent};
use komponist_model::{AudioFile, AudioInput, ClipId, Tick, TrackId};

use crate::actions::{Action, SelectMode, SelectionChange, UiAction};
use crate::app::App;
use crate::dialogs::file::Purpose;
use crate::dialogs::new_track::{self, default_names};
use crate::ui_state::StatusKind;

/// Metadata of a linked file, without decoding its samples or copying it.
pub fn linked_file(path: &Path, project_dir: Option<&Path>) -> Result<AudioFile, String> {
    let file = komponist_audiofile::FilePool::global()
        .open(path)
        .map_err(|e| e.to_string())?;
    let info = file.info();
    Ok(AudioFile::new(
        path,
        project_dir,
        info.frames,
        info.sample_rate,
        info.source_channels as u16,
    ))
}

/// A browser drag carries source metadata so its arrangement preview never reads the disk.
#[derive(Debug, Clone)]
pub struct AudioDrag {
    /// Source paths, including files whose metadata could not be read.
    pub paths: Vec<PathBuf>,
    /// Linked files in import order.
    pub files: Vec<AudioFile>,
    /// Shown name of the first file.
    pub name: String,
    /// Localized duration/channel metadata.
    pub meta: String,
}

/// Snap audio positions to the arrangement grid when the caller enables snapping.
pub fn snap_tick(tick: Tick, grid: Tick, up: bool) -> Tick {
    let step = grid.0.max(1);
    let value = tick.0.max(0);
    Tick(if up {
        value.saturating_add(step - 1) / step * step
    } else {
        value.saturating_add(step / 2) / step * step
    })
}

impl App {
    pub(crate) fn open_new_audio_track_dialog(&mut self) {
        if self.ui.dialog.is_some() || self.dialogs.overlay.is_some() {
            return;
        }
        let mut state = new_track::State::new(None);
        state.kind = new_track::KIND_AUDIO;
        state.available_input_channels = self.engine.audio_input_channels();
        state.input = new_track::first_unused_input(self.project(), state.available_input_channels);
        self.open_overlay(crate::dialogs::Overlay::NewTrack(state));
    }

    pub(crate) fn create_audio_tracks(&mut self, spec: &new_track::Spec) {
        let names = default_names(self.lang);
        let made = self.apply_with("undo-add-audio-track", |p| {
            let mut p = p.clone();
            let mut ids = Vec::new();
            for i in 1..=spec.count {
                let name = spec.name.as_ref().map(|n| {
                    if spec.count == 1 {
                        n.clone()
                    } else {
                        format!("{n} {i}")
                    }
                });
                let (next, id) = edits::add_audio_track_in(
                    &p,
                    NewAudioTrack {
                        name,
                        input: spec.input,
                        ..Default::default()
                    },
                    &names,
                )?;
                p = next;
                ids.push(id);
            }
            Ok((p, ids))
        });
        if let Some(tracks) = made {
            self.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
                tracks,
                mode: SelectMode::Replace,
            })));
        }
    }

    pub(crate) fn open_audio_import(&mut self) {
        if self.ui.dialog.is_some() || self.dialogs.overlay.is_some() {
            return;
        }
        let target = self
            .project()
            .tracks
            .iter()
            .find(|t| t.is_audio() && self.ui.selection.tracks.contains(&t.id))
            .map(|t| t.id);
        let at = if target.is_some() {
            if self.ui.arrangement.snap {
                snap_tick(
                    self.engine.playhead(),
                    self.ui.arrangement.grid.ticks(self.project().meter),
                    true,
                )
            } else {
                self.engine.playhead().max(Tick::ZERO)
            }
        } else {
            Tick::ZERO
        };
        self.shell.audio_import_target = Some((target, at));
        self.open_file_browser(Purpose::ImportAudio);
    }

    pub(crate) fn audio_import_chosen(&mut self, paths: Vec<PathBuf>) {
        let target = self
            .shell
            .audio_import_target
            .take()
            .unwrap_or((None, Tick::ZERO));
        self.close_overlay();
        if let Some(folder) = paths.first().and_then(|p| p.parent()) {
            self.dialogs.last_dir = Some(folder.to_path_buf());
        }
        self.import_audio_paths(&paths, target.0, target.1);
    }

    /// The only document write for an import: all files form one undo step, with model overlap cuts.
    pub(crate) fn import_audio_paths(
        &mut self,
        paths: &[PathBuf],
        target: Option<TrackId>,
        at: Tick,
    ) {
        if paths.is_empty() {
            return;
        }
        if let Some(track) = target.and_then(|id| self.project().track(id))
            && !track.is_audio()
        {
            let text = if track.is_bus() {
                self.lang.routing_no_clips()
            } else {
                self.lang.status_drop_refused_midi()
            };
            self.set_status(text, StatusKind::Error);
            return;
        }
        let dir = self.project_dir();
        let mut files = Vec::new();
        for path in paths {
            match linked_file(path, dir.as_deref()) {
                Ok(file) => files.push(file),
                Err(why) => {
                    self.set_status(
                        self.lang.status_audio_import_failed(path.display(), why),
                        StatusKind::Error,
                    );
                    return;
                }
            }
        }
        let names = default_names(self.lang);
        let grid = self.ui.arrangement.grid.ticks(self.project().meter);
        let snapping = self.ui.arrangement.snap;
        let at = if target.is_none() && files.len() > 1 {
            Tick::ZERO
        } else if snapping {
            snap_tick(at, grid, false)
        } else {
            at.max(Tick::ZERO)
        };
        let made = self.apply_with("undo-import-audio", |p| {
            let mut p = p.clone();
            let mut next_at = at;
            let mut clips: Vec<ClipId> = Vec::new();
            for file in files {
                let name = file.name();
                let track = if let Some(track) = target {
                    track
                } else {
                    let (next, id) = edits::add_audio_track_in(
                        &p,
                        NewAudioTrack {
                            name: Some(name.clone()),
                            input: AudioInput::None,
                            ..Default::default()
                        },
                        &names,
                    )?;
                    p = next;
                    id
                };
                let length = Tick(
                    p.tempo_map()
                        .seconds_to_ticks(file.frames as f64 / f64::from(file.sample_rate))
                        .floor() as i64,
                )
                .max(Tick(1));
                let (next, clip) = edits::add_clip(
                    &p,
                    track,
                    NewClip {
                        name,
                        start: next_at,
                        length,
                        content: NewClipContent::Audio { file, offset: 0 },
                    },
                )?;
                p = next;
                clips.push(clip);
                if target.is_some() {
                    next_at = if snapping {
                        snap_tick(next_at + length, grid, true)
                    } else {
                        next_at + length
                    };
                }
            }
            Ok((p, clips))
        });
        if let Some(clips) = made {
            let count = clips.len();
            self.dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
                clips,
                mode: SelectMode::Replace,
            })));
            self.set_status(self.lang.status_audio_imported(count), StatusKind::Success);
        }
    }
}
