//! Track, mixer and routing edits and their realtime updates.

use komponist_engine::Command;
use komponist_model::TrackId;
use komponist_model::automation::{AutomationOwner, AutomationTarget};
use komponist_model::edits::{self, ChannelProp, NewTrack, TrackProp};
use komponist_model::project::VOLUME_DB_MIN;

use super::App;
use crate::actions::SelectMode;
use crate::dialogs::new_track::default_names;
use crate::ui_state::StatusKind;

impl App {
    pub(super) fn toggle_mute_tracks(&mut self) {
        let tracks: Vec<_> = self.ui.selection.tracks.iter().copied().collect();
        let p = self.project();
        let any_unmuted = tracks
            .iter()
            .filter_map(|&t| p.track(t))
            .any(|t| !t.channel.mute);
        if tracks.is_empty() {
            return;
        }
        self.apply("undo-mute-tracks", |p| {
            edits::set_track_prop(
                p,
                &tracks,
                TrackProp::Channel(ChannelProp::Mute(any_unmuted)),
            )
        });
    }

    pub(super) fn set_master_prop(&mut self, prop: ChannelProp) {
        let automation = match prop {
            ChannelProp::VolumeDb(db) => Some((
                AutomationTarget::Volume,
                Command::SetMasterGain(fader_gain(db)),
            )),
            ChannelProp::Pan(pan) => Some((AutomationTarget::Pan, Command::SetMasterPan(pan))),
            _ => None,
        };
        if let Some((target, command)) = automation
            && self.manual_automation(AutomationOwner::Master, &target, command)
        {
            return;
        }
        let before = self.session.document().revision();
        if self.apply("undo-change-master", |p| edits::set_master_prop(p, prop))
            && self.session.document().in_gesture()
            && let ChannelProp::VolumeDb(db) = prop
        {
            let after = self.session.document().revision();
            self.session.engine_mut().fast_path(
                Command::SetMasterGain(fader_gain(db)),
                before,
                after,
            );
        }
    }

    pub(super) fn add_track(&mut self, spec: NewTrack) {
        let names = default_names(self.lang);
        if let Some(id) =
            self.apply_with("undo-add-track", |p| edits::add_track_in(p, spec, &names))
        {
            self.select_tracks(vec![id], SelectMode::Replace);
        }
    }

    pub(super) fn remove_tracks(&mut self, tracks: Vec<TrackId>) {
        if self.apply("undo-delete-tracks", |p| edits::remove_tracks(p, &tracks)) {
            self.prune_selection();
        }
    }

    pub(super) fn set_exclusive(&mut self, track: TrackId, flag: edits::ExclusiveFlag) {
        if flag == edits::ExclusiveFlag::Arm && !self.audio_arm_allowed(track) {
            return;
        }
        self.apply("undo-exclusive", |p| edits::set_exclusive(p, track, flag));
    }

    pub(super) fn group_selection(&mut self) {
        let tracks = self.selected_track_order();
        self.group_tracks(tracks);
    }

    pub(super) fn send_to_new_bus(&mut self, senders: Vec<TrackId>) {
        let names = default_names(self.lang);
        if let Some(id) = self.apply_with("undo-send-to-new-bus", |p| {
            edits::add_bus_in(p, edits::NewBus::effect(senders), &names)
        }) {
            self.select_tracks(vec![id], SelectMode::Replace);
            self.request_rename(id);
        }
    }

    pub(super) fn add_bus(&mut self, spec: edits::NewBus) {
        let names = default_names(self.lang);
        if let Some(id) = self.apply_with("undo-add-bus", |p| edits::add_bus_in(p, spec, &names)) {
            self.select_tracks(vec![id], SelectMode::Replace);
        }
    }

    pub(super) fn set_track_prop(&mut self, mut tracks: Vec<TrackId>, prop: TrackProp) {
        if let TrackProp::Channel(channel) = &prop {
            tracks.retain(|&track| {
                let automation = match *channel {
                    ChannelProp::VolumeDb(db) => Some((
                        AutomationTarget::Volume,
                        Command::SetTrackGain {
                            track,
                            gain: fader_gain(db),
                        },
                    )),
                    ChannelProp::Pan(pan) => {
                        Some((AutomationTarget::Pan, Command::SetTrackPan { track, pan }))
                    }
                    _ => None,
                };
                !automation.is_some_and(|(target, command)| {
                    let owner = AutomationOwner::Track(track);
                    self.handle_macro_target_edit(owner, &target)
                        || self.manual_automation(owner, &target, command)
                })
            });
            if tracks.is_empty() {
                return;
            }
        }
        if matches!(&prop, TrackProp::AudioInput(_)) && self.audio_recording().is_some() {
            self.set_status(self.lang.audio_input_change_recording(), StatusKind::Error);
            return;
        }
        if prop == TrackProp::Arm(true)
            && tracks.iter().any(|&track| !self.audio_arm_allowed(track))
        {
            return;
        }
        let label = match prop {
            TrackProp::AudioInput(_) => "undo-audio-input",
            TrackProp::MonitorMode(_) => "undo-audio-monitor",
            _ => "undo-change-track",
        };
        let before = self.session.document().revision();
        let fast = match &prop {
            TrackProp::Channel(c) => Some(*c),
            _ => None,
        };
        if !self.apply(label, |p| edits::set_track_prop(p, &tracks, prop)) {
            return;
        }
        let (Some(channel), true) = (fast, self.session.document().in_gesture()) else {
            return;
        };
        let after = self.session.document().revision();
        // Mute and solo also go out as the gate table: with buses, solo follows the signal path.
        let gates = matches!(channel, ChannelProp::Mute(_) | ChannelProp::Solo(_))
            .then(|| {
                self.session
                    .engine()
                    .gates(self.session.document().project())
            })
            .flatten();
        for track in tracks {
            let command = match channel {
                ChannelProp::VolumeDb(db) => Command::SetTrackGain {
                    track,
                    gain: fader_gain(db),
                },
                ChannelProp::Pan(pan) => Command::SetTrackPan { track, pan },
                ChannelProp::Mute(mute) => Command::SetTrackMute { track, mute },
                ChannelProp::Solo(solo) => Command::SetTrackSolo { track, solo },
            };
            self.session.engine_mut().fast_path(command, before, after);
        }
        if let Some(command) = gates {
            self.session.engine_mut().fast_path(command, before, after);
        }
    }

    /// Strg G: `tracks` into a new group (one undo step); the group is selected and its name goes
    /// into the edit mode of the arrangement (WP-100 §6).
    pub(super) fn group_tracks(&mut self, tracks: Vec<TrackId>) {
        if tracks.is_empty() {
            return;
        }
        let names = default_names(self.lang);
        if let Some(id) = self.apply_with("undo-group-tracks", |p| {
            edits::add_bus_in(p, edits::NewBus::group(tracks), &names)
        }) {
            self.select_tracks(vec![id], SelectMode::Replace);
            self.request_rename(id);
        }
    }

    /// Opens the name field of a bus just made, but only if the arrangement is visible at this
    /// moment (owner, 2026-11-15); otherwise no request is kept and the bus has its automatic
    /// name, so a later switch to the arrangement does not open a stale rename.
    fn request_rename(&mut self, id: TrackId) {
        let l = &self.ui.layout;
        let arrangement_hidden = l.dock_open && l.dock_maximized;
        self.ui.arrangement.rename_request = (!arrangement_hidden).then_some(id);
    }

    /// A send change; a level change inside a gesture reaches the engine on the fast path
    /// (`Command::SetSendGain`, ramped by the send's edge state), so dragging a send level does not
    /// recompile the program per frame.
    pub(super) fn set_send(&mut self, send: komponist_model::SendId, prop: edits::SendProp) {
        if let edits::SendProp::LevelDb(db) = prop
            && let Some((track, _)) = self.project().find_send(send)
            && {
                let owner = AutomationOwner::Track(track.id);
                let target = AutomationTarget::Send { send };
                self.handle_macro_target_edit(owner, &target)
                    || self.manual_automation(
                        owner,
                        &target,
                        Command::SetSendGain {
                            send,
                            gain: fader_gain(db),
                        },
                    )
            }
        {
            return;
        }
        let before = self.session.document().revision();
        if !self.apply("undo-change-send", |p| edits::set_send(p, send, prop)) {
            return;
        }
        if let (edits::SendProp::LevelDb(_), true) = (prop, self.session.document().in_gesture()) {
            let after = self.session.document().revision();
            let db = self
                .project()
                .find_send(send)
                .map_or(0.0, |(t, i)| t.sends[i].level_db);
            self.session.engine_mut().fast_path(
                Command::SetSendGain {
                    send,
                    gain: fader_gain(db),
                },
                before,
                after,
            );
        }
    }
}

/// The fader's linear gain for `db` (`VOLUME_DB_MIN` = −∞ → 0), as the compiler computes it.
fn fader_gain(db: f32) -> f32 {
    if db <= VOLUME_DB_MIN {
        0.0
    } else {
        klang_dsp::units::db_to_gain(db)
    }
}
