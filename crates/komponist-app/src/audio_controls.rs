//! Central audio-arm validation and the bus recording entry point; no recording lifecycle.

use komponist_model::edits::{self, NewAudioTrack};
use komponist_model::{AudioInput, TrackId, TrackOutput};

use crate::actions::{Action, SelectMode, SelectionChange, UiAction};
use crate::app::App;
use crate::dialogs::new_track::default_names;
use crate::ui_state::StatusKind;
use crate::views::audio_inputs::hardware_input_available;

impl App {
    /// Checks explicit arm commands and recording startup; MIDI tracks keep their own rules.
    pub(crate) fn audio_arm_allowed(&mut self, id: TrackId) -> bool {
        let Some(track) = self.project().track(id) else {
            return false;
        };
        if !track.is_audio() {
            return true;
        }
        let reason = match track.audio.input {
            AudioInput::None => Some(self.lang.audio_arm_no_input().to_owned()),
            AudioInput::Hardware(input)
                if !hardware_input_available(input, self.engine.audio_input_channels()) =>
            {
                Some(
                    self.lang
                        .audio_arm_unavailable(crate::views::audio_inputs::input_label(
                            self.lang,
                            self.project(),
                            track.audio.input,
                        )),
                )
            }
            AudioInput::Bus(bus)
                if !komponist_model::routing::input_sources(self.project(), id).contains(&bus) =>
            {
                Some(
                    self.lang
                        .audio_arm_unavailable(crate::views::audio_inputs::input_label(
                            self.lang,
                            self.project(),
                            track.audio.input,
                        )),
                )
            }
            _ => None,
        };
        if let Some(reason) = reason {
            self.set_status(reason, StatusKind::Error);
            false
        } else {
            true
        }
    }

    pub(crate) fn record_bus(&mut self, bus: TrackId) {
        let Some(source) = self.project().track(bus).filter(|t| t.is_bus()).cloned() else {
            return;
        };
        let name = self.lang.routing_record_bus_track(&source.name);
        // Inserting before the next group would inherit its membership: create at the end first,
        // explicitly keep Master output, then move the track behind the source group's block.
        let index = if source.folder {
            self.project()
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| {
                    track.id == bus
                        || komponist_model::routing::group_ancestors(self.project(), track.id)
                            .contains(&bus)
                })
                .map(|(index, _)| index + 1)
                .max()
                .unwrap_or(self.project().tracks.len())
        } else {
            self.project()
                .tracks
                .iter()
                .position(|track| track.is_bus() && !track.folder)
                .unwrap_or(self.project().tracks.len())
        };
        let names = default_names(self.lang);
        let made = self.apply_with("undo-record-bus", |project| {
            let (p, id) = edits::add_audio_track_in(
                project,
                NewAudioTrack {
                    name: Some(name.clone()),
                    hue: Some(source.hue),
                    input: AudioInput::Bus(bus),
                    ..Default::default()
                },
                &names,
            )?;
            let p = edits::set_output(&p, &[id], TrackOutput::Master)?;
            let p = edits::set_track_prop(&p, &[id], edits::TrackProp::Arm(true))?;
            let p = edits::move_tracks(&p, &[id], index)?;
            Ok((p, id))
        });
        if let Some(track) = made {
            self.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
                tracks: vec![track],
                mode: SelectMode::Replace,
            })));
            self.set_status(
                self.lang.status_record_bus_created(&name, &source.name),
                StatusKind::Info,
            );
        }
    }
}
