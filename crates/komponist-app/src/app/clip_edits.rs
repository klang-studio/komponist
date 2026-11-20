//! Clip and note edits that also update the editor or its selection.

use komponist_model::edits::{self, AudioClipProp, NewClip, NewNote, VelocityChange};
use komponist_model::{ClipContent, ClipId, NoteId, Tick, TrackId};

use super::App;
use crate::actions::{SelectMode, UiAction};

impl App {
    /// The clip open in the editor and whether it is a MIDI clip; `None` while recording (the
    /// editor shows the take being recorded, not the open clip).
    pub(super) fn open_clip_notes(&self) -> Option<(ClipId, bool)> {
        if self.recording.is_some() || self.audio_recording.is_active() {
            return None;
        }
        let id = self.ui.editor.clip?;
        let (_, clip) = self.project().find_clip(id)?;
        Some((id, matches!(clip.content, ClipContent::Midi(_))))
    }

    pub(super) fn selected_notes(&self) -> Vec<NoteId> {
        self.ui.selection.notes.iter().copied().collect()
    }

    pub(super) fn duplicate_clips(&mut self) {
        let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
        if clips.is_empty() {
            return;
        }
        if let Some(copies) = self.apply_with("undo-duplicate-clips", |p| {
            edits::duplicate_clips(p, &clips)
        }) {
            self.select_clips(copies, SelectMode::Replace);
        }
    }

    pub(super) fn split_clips_at_playhead(&mut self) {
        let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
        if clips.is_empty() {
            return;
        }
        let at = self.engine.playhead();
        self.apply_with("undo-split-clips", |p| edits::split_clips(p, &clips, at));
    }

    pub(super) fn toggle_mute_clips(&mut self) {
        let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
        let p = self.project();
        let any_unmuted = clips
            .iter()
            .filter_map(|&c| p.find_clip(c))
            .any(|(_, c)| !c.mute);
        if clips.is_empty() {
            return;
        }
        self.apply("undo-mute-clips", |p| {
            edits::set_clips_muted(p, &clips, any_unmuted)
        });
    }

    pub(super) fn add_clip(&mut self, track: TrackId, clip: NewClip) {
        if let Some(id) = self.apply_with("undo-add-clip", |p| edits::add_clip(p, track, clip)) {
            self.select_clips(vec![id], SelectMode::Replace);
            self.ui_action(UiAction::OpenClip(id));
        }
    }

    pub(super) fn remove_clips(&mut self, clips: Vec<ClipId>) {
        if self.apply("undo-delete-clips", |p| edits::remove_clips(p, &clips)) {
            self.prune_selection();
        }
    }

    pub(super) fn copy_clips(&mut self, clips: Vec<ClipId>, delta: Tick, track_delta: i32) {
        if let Some(copies) = self.apply_delta("undo-copy-clips", |p| {
            edits::copy_clips(p, &clips, delta, track_delta)
        }) {
            self.select_clips(copies, SelectMode::Replace);
        }
    }

    pub(super) fn set_audio_clip_prop(&mut self, clip: ClipId, prop: AudioClipProp) {
        let label = match prop {
            AudioClipProp::GainDb(_) => "undo-clip-gain",
            AudioClipProp::SourceOffset(_) => "undo-clip-source-start",
            AudioClipProp::FadeIn(_) | AudioClipProp::FadeOut(_) => "undo-clip-fade",
            AudioClipProp::TimeMode(_) => "undo-clip-audio-mode",
            AudioClipProp::OriginalTempo(_) => "undo-clip-original-tempo",
        };
        self.apply_delta(label, |p| {
            edits::set_audio_clip_prop(p, clip, prop).map(|p| (p, ()))
        });
    }

    pub(super) fn quantize_selected_notes(&mut self) {
        let Some((clip, true)) = self.open_clip_notes() else {
            return;
        };
        let notes = self.selected_notes();
        let grid = self.ui.piano_roll.grid.ticks(self.project().meter);
        self.apply("undo-quantize", |p| {
            let sel = (!notes.is_empty()).then_some(notes.as_slice());
            edits::quantize_notes(p, clip, sel, grid)
        });
    }

    pub(super) fn transpose_selected_notes(&mut self, semitones: i8) {
        let Some((clip, true)) = self.open_clip_notes() else {
            return;
        };
        let notes = self.selected_notes();
        if notes.is_empty() {
            return;
        }
        self.apply("undo-transpose-notes", |p| {
            edits::move_notes(p, clip, &notes, Tick::ZERO, i32::from(semitones))
        });
    }

    pub(super) fn nudge_selected_notes(&mut self, steps: i32) {
        let Some((clip, true)) = self.open_clip_notes() else {
            return;
        };
        let notes = self.selected_notes();
        if notes.is_empty() {
            return;
        }
        let grid = self.ui.piano_roll.grid.ticks(self.project().meter);
        let delta = Tick(grid.0 * i64::from(steps));
        self.apply("undo-move-notes", |p| {
            edits::move_notes(p, clip, &notes, delta, 0)
        });
    }

    pub(super) fn add_notes(&mut self, clip: ClipId, notes: Vec<NewNote>) {
        if let Some(ids) = self.apply_with("undo-add-notes", |p| edits::add_notes(p, clip, &notes))
        {
            self.ui.selection.notes = ids.into_iter().collect();
        }
    }

    pub(super) fn remove_notes(&mut self, clip: ClipId, notes: Vec<NoteId>) {
        if self.apply("undo-delete-notes", |p| {
            edits::remove_notes(p, clip, &notes)
        }) {
            for n in &notes {
                self.ui.selection.notes.remove(n);
            }
        }
    }

    pub(super) fn copy_notes(
        &mut self,
        clip: ClipId,
        notes: Vec<NoteId>,
        delta: Tick,
        pitch_delta: i32,
    ) {
        if let Some(ids) = self.apply_delta("undo-copy-notes", |p| {
            edits::copy_notes(p, clip, &notes, delta, pitch_delta)
        }) {
            self.ui.selection.notes = ids.into_iter().collect();
        }
    }

    pub(super) fn set_notes_velocity(
        &mut self,
        clip: ClipId,
        notes: Vec<NoteId>,
        change: VelocityChange,
    ) {
        let label = "undo-velocity";
        match change {
            VelocityChange::Offset(_) => {
                self.apply_delta(label, |p| {
                    edits::set_notes_velocity(p, clip, &notes, change).map(|p| (p, ()))
                });
            }
            VelocityChange::Set(_) => {
                self.apply(label, |p| {
                    edits::set_notes_velocity(p, clip, &notes, change)
                });
            }
        }
    }
}
