//! Routes document edit actions through the feature handlers and shared edit application.

use std::sync::Arc;

use komponist_model::{Document, edits};

use super::App;
use crate::actions::EditAction;
use crate::ui_state::FocusArea;

impl App {
    pub(super) fn edit_action(&mut self, action: EditAction) {
        match action {
            // History and selection.
            EditAction::Undo => self.edit_history(Document::undo),
            EditAction::Redo => self.edit_history(Document::redo),
            EditAction::BeginGesture { label } => {
                self.end_gesture();
                self.doc.begin_gesture(label);
            }
            EditAction::EndGesture => self.end_gesture(),
            EditAction::CancelGesture => self.cancel_gesture(),
            EditAction::DeleteSelection => self.delete_selection(),

            // Clips and takes.
            EditAction::DuplicateSelection => self.duplicate_clips(),
            EditAction::SplitAtPlayhead => self.split_clips_at_playhead(),
            EditAction::ToggleMuteSelection => self.toggle_mute_clips(),
            EditAction::AddClip { track, clip } => self.add_clip(track, clip),
            EditAction::RemoveClips(clips) => self.remove_clips(clips),
            EditAction::MoveClips {
                clips,
                delta,
                track_delta,
            } => {
                self.apply_delta("undo-move-clips", |p| {
                    edits::move_clips(p, &clips, delta, track_delta).map(|p| (p, ()))
                });
            }
            EditAction::CopyClips {
                clips,
                delta,
                track_delta,
            } => self.copy_clips(clips, delta, track_delta),
            EditAction::SetClipLength { clip, length } => {
                self.apply_delta("undo-clip-length", |p| {
                    edits::set_clip_length(p, clip, length).map(|p| (p, ()))
                });
            }
            EditAction::TrimAudioClipStart { clip, start } => {
                self.apply_delta("undo-clip-length", |p| {
                    edits::trim_audio_clip_start(p, clip, start).map(|p| (p, ()))
                });
            }
            EditAction::SetAudioClipFile { clip, file } => {
                self.apply("undo-clip-replace-file", |p| {
                    edits::set_audio_clip_file(p, clip, file)
                });
            }
            EditAction::SetAudioClipProp { clip, prop } => self.set_audio_clip_prop(clip, prop),
            EditAction::SetAudioClipWarp { clip, enabled } => {
                self.apply_delta("undo-clip-audio-mode", |p| {
                    edits::set_audio_clip_warp(p, clip, enabled).map(|p| (p, ()))
                });
            }
            EditAction::SetClipLoop {
                clip,
                loop_length,
                length,
            } => {
                self.apply("undo-clip-loop", |p| {
                    edits::set_clip_loop(p, clip, loop_length, length)
                });
            }
            EditAction::SetClipsMuted { clips, muted } => {
                self.apply("undo-mute-clips", |p| {
                    edits::set_clips_muted(p, &clips, muted)
                });
            }
            EditAction::SetClipProp { clip, prop } => {
                self.apply("undo-change-clip", |p| edits::set_clip_prop(p, clip, prop));
            }
            EditAction::SelectTake { clip, take } => {
                self.apply("undo-select-take", |p| edits::select_take(p, clip, take));
            }
            EditAction::SelectAudioTake { clip, take } => {
                self.apply_with("undo-select-audio-take", |p| {
                    edits::select_audio_take(p, clip, take).map(|p| (p, ()))
                });
            }
            EditAction::RenameAudioTake { clip, take, name } => {
                self.apply_with("undo-rename-audio-take", |p| {
                    edits::rename_audio_take(p, clip, take, name).map(|p| (p, ()))
                });
            }
            EditAction::RemoveAudioTake { clip, take } => {
                self.apply_with("undo-remove-audio-take", |p| {
                    edits::remove_audio_take(p, clip, take).map(|p| (p, ()))
                });
            }

            // Notes and patterns.
            EditAction::QuantizeSelection => self.quantize_selected_notes(),
            EditAction::TransposeSelection(semitones) => self.transpose_selected_notes(semitones),
            EditAction::NudgeSelection(steps) => self.nudge_selected_notes(steps),
            EditAction::AddNotes { clip, notes } => self.add_notes(clip, notes),
            EditAction::RemoveNotes { clip, notes } => self.remove_notes(clip, notes),
            EditAction::MoveNotes {
                clip,
                notes,
                delta,
                pitch_delta,
            } => {
                self.apply_delta("undo-move-notes", |p| {
                    edits::move_notes(p, clip, &notes, delta, pitch_delta).map(|p| (p, ()))
                });
            }
            EditAction::CopyNotes {
                clip,
                notes,
                delta,
                pitch_delta,
            } => self.copy_notes(clip, notes, delta, pitch_delta),
            EditAction::ResizeNotes { clip, notes, delta } => {
                self.apply_delta("undo-note-length", |p| {
                    edits::resize_notes(p, clip, &notes, delta).map(|p| (p, ()))
                });
            }
            EditAction::SetNotesVelocity {
                clip,
                notes,
                change,
            } => self.set_notes_velocity(clip, notes, change),
            EditAction::SetStep {
                clip,
                row,
                step,
                prop,
            } => {
                self.apply("undo-step", |p| edits::set_step(p, clip, row, step, prop));
            }
            EditAction::SetStepClipProp { clip, prop } => {
                self.apply("undo-change-pattern", |p| {
                    edits::set_step_clip_prop(p, clip, prop)
                });
            }
            EditAction::SetStepRow { clip, row, prop } => {
                self.apply("undo-change-row", |p| {
                    edits::set_step_row(p, clip, row, prop)
                });
            }
            EditAction::StepClipToMidi(clip) => {
                self.apply("undo-as-midi-clip", |p| edits::step_clip_to_midi(p, clip));
            }

            // Project, tracks and routing.
            EditAction::SetProjectProp(prop) => {
                self.apply("undo-change-project", |p| edits::set_project_prop(p, prop));
            }
            EditAction::SetMasterProp(prop) => self.set_master_prop(prop),
            EditAction::NewMidiTrack => self.new_midi_track(None, self.default_hexa()),
            EditAction::NewAudioTrack => self.open_new_audio_track_dialog(),
            EditAction::AddTrack(spec) => self.add_track(spec),
            EditAction::RemoveTracks(tracks) => self.remove_tracks(tracks),
            EditAction::FlattenMidiTrack(track) => self.start_flatten(track),
            EditAction::MoveTracks { tracks, to_index } => {
                self.apply("undo-move-tracks", |p| {
                    edits::move_tracks(p, &tracks, to_index)
                });
            }
            EditAction::SetTrackProp { tracks, prop } => self.set_track_prop(tracks, prop),
            EditAction::SetExclusive { track, flag } => self.set_exclusive(track, flag),
            EditAction::ToggleMuteTracks => self.toggle_mute_tracks(),
            EditAction::RecordBus(bus) => self.record_bus(bus),
            EditAction::GroupSelection => self.group_selection(),
            EditAction::GroupTracks(tracks) => self.group_tracks(tracks),
            EditAction::SendToNewBus(senders) => self.send_to_new_bus(senders),
            EditAction::AddBus(spec) => self.add_bus(spec),
            EditAction::SetOutput { tracks, output } => {
                self.apply("undo-set-output", |p| edits::set_output(p, &tracks, output));
            }
            EditAction::AddSend { track, target } => {
                self.apply_with("undo-add-send", |p| edits::add_send(p, track, target));
            }
            EditAction::SetSend { send, prop } => self.set_send(send, prop),
            EditAction::RemoveSend(send) => {
                self.apply("undo-remove-send", |p| edits::remove_send(p, send));
            }

            // Devices.
            EditAction::RemoveDevice(device) => self.remove_device(device),
            EditAction::MoveDevice { device, to_index } => {
                self.apply("undo-move-device", |p| {
                    edits::move_device(p, device, to_index)
                });
            }
            EditAction::SetDeviceBypass { device, bypass } => {
                self.apply("undo-bypass", |p| {
                    edits::set_device_bypass(p, device, bypass)
                });
            }
            EditAction::SetSidechain { device, sidechain } => {
                self.apply("undo-set-sidechain", |p| {
                    edits::set_sidechain(p, device, sidechain)
                });
            }
            EditAction::CopyDevice { device, to, index } => self.copy_device(device, to, index),
            EditAction::DuplicateDevices => self.duplicate_devices(),
            EditAction::PasteDevices => self.paste_devices(),
            EditAction::DeleteDevices => self.delete_devices(),
        }
    }

    fn edit_history(&mut self, apply: impl FnOnce(&mut Document) -> bool) {
        // Finish a gesture before stepping history, then propagate the restored device states.
        self.cancel_plugin_asset_edits();
        self.end_gesture();
        let before = Arc::clone(self.doc.project());
        apply(&mut self.doc);
        self.follow_device_states(&before);
    }

    /// Entf: notes in the editor, else clips, else tracks.
    fn delete_selection(&mut self) {
        let editor = matches!(
            self.ui.focus,
            FocusArea::PianoRoll | FocusArea::StepSequencer
        );
        if editor
            && let Some((clip, true)) = self.open_clip_notes()
            && !self.ui.selection.notes.is_empty()
        {
            let notes = self.selected_notes();
            if self.apply("undo-delete-notes", |p| {
                edits::remove_notes(p, clip, &notes)
            }) {
                self.ui.selection.notes.clear();
            }
            return;
        }
        if editor {
            return;
        }
        if !self.ui.selection.clips.is_empty() {
            let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
            self.remove_clips(clips);
        } else if !self.ui.selection.tracks.is_empty() {
            let tracks: Vec<_> = self.ui.selection.tracks.iter().copied().collect();
            self.remove_tracks(tracks);
        }
    }
}
