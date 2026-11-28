//! App routing of automation events, gestures and recording lifecycle.

use komponist_engine::automation::{AutomationBoundary, EngineTarget};
use komponist_engine::{Command, EngineEvent, TransportState};
use komponist_model::automation::{AutomationMode, AutomationOwner, AutomationTarget, LaneId};
use komponist_model::edits::automation as edits;
use komponist_model::params::ParamId;
use komponist_model::{DeviceId, Tick};
use komponist_session::recording::automation::AutomationCapture;

use crate::app::App;

impl App {
    /// Closes a live write at the mode change while keeping it in the current undoable pass.
    pub(crate) fn close_writes_for_mode_change(
        &mut self,
        owner: AutomationOwner,
        lane: Option<LaneId>,
        mode: AutomationMode,
    ) {
        self.drain_events();
        let tick = self.session.engine().playhead();
        let project = self.project().clone();
        self.recording
            .automation
            .close_writes_for_mode_change(&project, tick, owner, lane, mode);
    }

    pub(crate) fn capture_native_automation(
        &mut self,
        device: DeviceId,
        id: u32,
        owner: AutomationOwner,
        target: EngineTarget,
        tick: Tick,
        learned: bool,
    ) {
        if learned {
            // Successful mappings already removed this lane from the compiled program.
            // A rejected mapping can still produce samples and loop restarts.
            if self
                .session
                .engine()
                .program()
                .is_some_and(|program| program.automation_model_target(owner, target).is_some())
            {
                self.recording.learned_automation.insert((device, id));
            }
        } else {
            self.recording.learned_automation.remove(&(device, id));
            self.capture_automation(&EngineEvent::AutomationTouch {
                owner,
                target,
                begin: true,
                tick,
            });
        }
    }

    pub(crate) fn capture_automation(&mut self, event: &EngineEvent) {
        let (owner, target, tick) = match *event {
            EngineEvent::AutomationTouch {
                owner,
                target,
                tick,
                ..
            }
            | EngineEvent::AutomationValue {
                owner,
                target,
                tick,
                ..
            } => (owner, target, tick),
            EngineEvent::AutomationBoundary { tick, kind } => {
                let project = self.project().clone();
                self.recording.automation.end_segments(&project, tick);
                if kind != AutomationBoundary::LoopWrap {
                    self.recording.learned_automation.clear();
                }
                if kind == AutomationBoundary::Stop
                    && std::mem::take(&mut self.recording.automation_stop_pending)
                {
                    self.finish_take();
                } else if matches!(kind, AutomationBoundary::Stop | AutomationBoundary::Locate)
                    && !self.recording.capturing_notes_or_audio()
                {
                    self.commit_automation_capture();
                }
                return;
            }
            _ => return,
        };
        let Some(model) = self
            .session
            .engine()
            .program()
            .and_then(|p| p.automation_model_target(owner, target))
            .cloned()
        else {
            return;
        };
        if let AutomationTarget::Param { device, param } = &model
            && let Some(id) = param.plugin_id()
        {
            let key = (*device, id);
            if matches!(event, EngineEvent::AutomationTouch { begin: false, .. }) {
                self.recording.learned_automation.remove(&key);
            } else if self.recording.learned_automation.contains(&key) {
                // Ignore samples and automatic loop restarts for this intercepted write.
                return;
            }
        }
        let project = self.project().clone();
        match *event {
            EngineEvent::AutomationTouch { begin: true, .. } => {
                let stepped = self.automation_stepped(target);
                let base = self.automation_base(owner, &model);
                self.recording
                    .automation
                    .open(&project, owner, model, stepped, base);
            }
            EngineEvent::AutomationTouch { begin: false, .. } => {
                self.recording
                    .automation
                    .release(&project, owner, &model, tick);
            }
            EngineEvent::AutomationValue { value, .. } => {
                self.recording
                    .automation
                    .value(&project, owner, &model, tick, value);
            }
            _ => {}
        }
    }

    fn automation_stepped(&self, target: EngineTarget) -> bool {
        let EngineTarget::Slot {
            device, wire_id, ..
        } = target
        else {
            return false;
        };
        self.session
            .engine()
            .host()
            .params(device)
            .and_then(|params| params.iter().find(|p| p.id == wire_id))
            .is_some_and(|p| p.step_count != 0)
    }

    fn automation_base(&self, owner: AutomationOwner, target: &AutomationTarget) -> f32 {
        self.session
            .engine()
            .program()
            .and_then(|p| p.automation_base(owner, target))
            .unwrap_or_default()
    }

    /// Ends every running write at `tick` as a stop boundary does, without committing the pass
    /// (an engine rebuilt after a panic sends no boundary for the old transport, #210).
    pub(crate) fn end_automation_writes_at(&mut self, tick: Tick) {
        let project = self.project().clone();
        self.recording.automation.end_segments(&project, tick);
        self.recording.learned_automation.clear();
        self.recording.automation_stop_pending = false;
    }

    pub(crate) fn take_automation_capture(&mut self) -> AutomationCapture {
        let project = self.project().clone();
        self.recording
            .automation
            .end_segments(&project, self.session.engine().playhead());
        self.recording.learned_automation.clear();
        std::mem::take(&mut self.recording.automation)
    }

    pub(crate) fn commit_automation_capture(&mut self) {
        let pass = self.take_automation_capture();
        if pass.has_points() {
            self.end_gesture();
            self.apply_with("undo-automation-record", |project| {
                pass.apply(project).map(|p| (p, ()))
            });
        }
    }

    pub(crate) fn clear_automation_capture(&mut self) {
        self.recording.automation = AutomationCapture::default();
        self.recording.learned_automation.clear();
        self.recording.controls.clear();
        self.recording.automation_stop_pending = false;
        self.session.engine_mut().discard_pending_macro_overrides();
    }

    pub(crate) fn device_automation_target(
        &self,
        device: DeviceId,
        id: u32,
    ) -> Option<(AutomationOwner, AutomationTarget)> {
        let (owner, _, _) = self.project().find_device(device)?;
        let param = ParamId::plugin(id);
        Some((owner.into(), AutomationTarget::Param { device, param }))
    }

    /// Returns true when the automation engine owns this control's temporary edit.
    pub(crate) fn manual_automation(
        &mut self,
        owner: AutomationOwner,
        target: &AutomationTarget,
        command: Command,
    ) -> bool {
        let Some(lane) = edits::lanes(self.project(), owner)
            .ok()
            .and_then(|a| a.lane(target))
        else {
            return false;
        };
        if lane.mode == AutomationMode::Off {
            return false;
        }
        let playing = matches!(
            self.transport(),
            TransportState::Playing | TransportState::Recording
        );
        let Some(program) = self.session.engine().program() else {
            return false;
        };
        if program.automation_mode(owner, target) != Some(lane.mode) {
            return false;
        }
        if !program.automation_reads(owner, target)
            && !(playing && matches!(lane.mode, AutomationMode::Touch | AutomationMode::Latch))
        {
            return false;
        }
        let Some(target) = program.automation_target(owner, target) else {
            return false;
        };
        let temporary = lane.mode == AutomationMode::Read || !playing;
        if temporary && let Some(hint) = self.lang.by_id("automation-override-hint") {
            self.set_status(hint, crate::ui_state::StatusKind::Info);
        }
        if let Command::SetParam { slot, id, value } = command
            && let Some(device) = self
                .session
                .engine()
                .program()
                .and_then(|p| p.slots.device(slot))
        {
            self.session
                .engine_mut()
                .host_mut()
                .preview_param(device, id, value);
        }
        let gesture = self.session.document().in_gesture();
        if !self.recording.controls.contains(&(owner, target)) {
            self.session.engine_mut().send(Command::Touch {
                owner,
                target,
                begin: true,
            });
            if gesture {
                self.recording.controls.push((owner, target));
            }
        }
        let command = match command {
            Command::SetMacro {
                track,
                index,
                value,
                ..
            } => self.session.engine_mut().macro_value_command(
                track,
                index,
                komponist_session::engine_sync::MacroValueEdit::AutomationOverride(
                    value.clamp(0.0, 1.0),
                ),
            ),
            command => command,
        };
        self.session.engine_mut().send(command);
        if !gesture {
            self.session.engine_mut().send(Command::Touch {
                owner,
                target,
                begin: false,
            });
        }
        true
    }

    pub(crate) fn release_automation_controls(&mut self) {
        for (owner, target) in self.recording.controls.drain(..) {
            self.session.engine_mut().send(Command::Touch {
                owner,
                target,
                begin: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::automation::{self, Point, Points};
    use komponist_model::{Project, fixtures};

    #[test]
    fn changing_mode_during_latch_closes_the_write_at_that_tick() {
        use crate::actions::{Action, EditAction, TransportAction};
        use crate::views::automation::Action as Automation;
        use komponist_model::edits::ChannelProp;

        for bulk in [false, true] {
            let project = Project::new("Capture");
            let owner = AutomationOwner::Master;
            let target = AutomationTarget::Pan;
            let (project, lane) =
                edits::add_lane(&project, owner, target.clone(), "Pan".into()).unwrap();
            let project =
                edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();
            let mut app = App::headless(project);
            app.session.engine_mut().hold_engine().unwrap();
            let run = |app: &mut App, frames| {
                assert!(app.session.engine_mut().run_held(frames, &[], 0));
                app.step();
            };
            app.dispatch(Action::Transport(TransportAction::TogglePlay));
            run(&mut app, 1024);
            app.dispatch(Action::Edit(EditAction::SetMasterProp(ChannelProp::Pan(
                0.5,
            ))));
            run(&mut app, 1024);
            assert!(app.recording.automation.is_writing(owner, lane));

            let end = app.session.engine().playhead();
            let action = if bulk {
                Automation::AllModes(owner, AutomationMode::Off)
            } else {
                Automation::Mode(owner, lane, AutomationMode::Off)
            };
            app.dispatch(Action::Automation(action));
            assert!(!app.recording.automation.is_writing(owner, lane));
            app.step();
            run(&mut app, 2048);
            assert!(app.session.engine().playhead() > end + Tick(1));
            app.dispatch(Action::Transport(TransportAction::Stop));
            run(&mut app, 256);

            let lane = app.project().master_automation.lane(&target).unwrap();
            assert_eq!(lane.mode, AutomationMode::Off);
            assert_eq!(
                automation::value_at(lane.points.as_slice(), end),
                Some(0.75)
            );
            assert_eq!(
                automation::value_at(lane.points.as_slice(), end + Tick(2)),
                Some(0.5)
            );
            assert_eq!(
                app.session.document().undo_label(),
                Some("undo-automation-record")
            );
        }
    }

    #[test]
    fn master_fader_latch_commits_at_engine_stop_without_changing_the_base() {
        use crate::actions::{Action, EditAction, TransportAction};
        use komponist_model::edits::ChannelProp;

        let p = Project::new("Capture");
        let owner = AutomationOwner::Master;
        let target = AutomationTarget::Volume;
        let (p, id) = edits::add_lane(&p, owner, target.clone(), "Volume".into()).unwrap();
        let p = edits::set_lane_mode(&p, owner, id, AutomationMode::Latch).unwrap();
        let master_gain = p.master.volume_db;
        let mut app = App::headless(p.clone());
        app.session.engine_mut().hold_engine().unwrap();
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
        assert!(app.session.engine_mut().run_held(1024, &[], 0));
        app.step();
        app.dispatch(Action::Edit(EditAction::BeginGesture {
            label: "Volume".into(),
        }));
        app.dispatch(Action::Edit(EditAction::SetMasterProp(
            ChannelProp::VolumeDb(-16.0),
        )));
        assert!(app.session.engine_mut().run_held(1024, &[], 0));
        app.step();
        app.dispatch(Action::Edit(EditAction::EndGesture));
        assert!(app.session.engine_mut().run_held(1024, &[], 0));
        app.step();
        assert!(app.recording.automation.is_writing(owner, id));
        assert_eq!(app.project().master.volume_db, master_gain);
        assert!(app.session.document().undo_label().is_none());
        app.dispatch(Action::Transport(TransportAction::Stop));
        assert!(app.session.engine_mut().run_held(256, &[], 0));
        app.step();
        let lane = app.project().master_automation.lane(&target).unwrap();
        assert!(lane.points.as_slice().iter().any(|point| {
            (point.value - (1.0 - komponist_model::params::db_to_p(-16.0))).abs() < 1e-5
        }));
        assert_eq!(
            app.session.document().undo_label(),
            Some("undo-automation-record")
        );
        assert!(app.session.document_mut().undo());
        assert_eq!(app.project().as_ref(), &p);
        assert!(app.session.document().undo_label().is_none());
    }

    /// #210 review: a latch pass running when the engine panics ends at the crash playhead and
    /// is committed in the rebuild's drain (no take runs), as one undo step.
    #[test]
    fn a_write_pass_is_committed_at_the_crash_when_the_engine_is_rebuilt() {
        use crate::actions::{Action, EditAction, TransportAction};
        use komponist_model::edits::ChannelProp;

        let p = Project::new("Capture");
        let owner = AutomationOwner::Master;
        let target = AutomationTarget::Volume;
        let (p, id) = edits::add_lane(&p, owner, target.clone(), "Volume".into()).unwrap();
        let p = edits::set_lane_mode(&p, owner, id, AutomationMode::Latch).unwrap();
        let mut app = App::headless(p);
        // A hexa track whose instrument can panic; disarmed, so playing records no take.
        app.dispatch(Action::Edit(EditAction::NewMidiTrack));
        let track = app.project().tracks[0].id;
        app.dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track],
            prop: komponist_model::edits::TrackProp::Arm(false),
        }));
        app.step();
        let instrument = app.project().tracks[0].channel.devices[0].id;
        // Single-threaded, so the panic is on this thread and outside `assert_no_alloc`.
        assert_eq!(
            app.session
                .engine_mut()
                .set_workers(Some(0))
                .unwrap()
                .unwrap()
                .threads,
            0
        );
        app.session.engine_mut().hold_engine().unwrap();
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
        assert!(app.session.engine_mut().run_held(1024, &[], 0));
        app.step();
        app.dispatch(Action::Edit(EditAction::BeginGesture {
            label: "Volume".into(),
        }));
        app.dispatch(Action::Edit(EditAction::SetMasterProp(
            ChannelProp::VolumeDb(-16.0),
        )));
        assert!(app.session.engine_mut().run_held(1024, &[], 0));
        app.step();
        app.dispatch(Action::Edit(EditAction::EndGesture));
        assert!(app.session.engine_mut().run_held(1024, &[], 0));
        app.step();
        assert!(app.recording.automation.is_writing(owner, id));
        assert!(app.recording.midi.is_none() && !app.recording.audio.is_active());
        let at = app.session.engine().playhead();
        assert!(at > Tick::ZERO);
        let slot = app
            .session
            .engine()
            .program()
            .unwrap()
            .slots
            .slot(instrument)
            .unwrap();
        app.session.engine_mut().send(Command::InsertProcessor {
            slot,
            processor: Box::new(komponist_engine::testing::TestPanic),
        });
        let engine = app.session.engine_mut();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.run_held(256, &[], 0);
        }));
        assert!(caught.is_err(), "the processor ran");
        app.session.engine_mut().release_engine();
        app.drain_events();

        assert!(!app.recording.automation.active(), "the pass ended");
        assert_eq!(
            app.session.document().undo_label(),
            Some("undo-automation-record")
        );
        let lane = app.project().master_automation.lane(&target).unwrap();
        let written = 1.0 - komponist_model::params::db_to_p(-16.0);
        let points = lane.points.as_slice();
        // The written value holds up to the crash; the latch returns one tick later, as at a stop.
        assert!(
            points
                .iter()
                .any(|p| p.tick == at && (p.value - written).abs() < 1e-5),
            "{points:?}"
        );
        assert_eq!(points.iter().map(|p| p.tick).max(), Some(Tick(at.0 + 1)));
        assert_eq!(
            app.ui.status.as_ref().map(|s| s.text.as_str()),
            Some(app.lang.status_engine_rebuilt())
        );
    }

    #[test]
    fn playback_rebases_active_and_deferred_drafts_in_one_sync() {
        use komponist_model::edits::{self as project_edits, ChannelProp, ProjectProp};

        let owner = AutomationOwner::Master;
        let target = AutomationTarget::Pan;
        let (project, lane) = edits::add_lane(
            &Project::new("Capture"),
            owner,
            target.clone(),
            "Pan".into(),
        )
        .unwrap();
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();
        let pass = |value| {
            let mut capture = AutomationCapture::default();
            capture.open(&project, owner, target.clone(), false, 0.5);
            capture.value(&project, owner, &target, Tick(10), value);
            capture.end_segments(&project, Tick(30));
            capture
        };
        let mut app = App::headless(project.clone());
        app.sync_project();
        let compiled_value = |app: &App| {
            let program = app.session.engine().program().unwrap();
            let lane = program
                .master
                .automation
                .iter()
                .find(|lane| lane.model == target)
                .unwrap();
            automation::value_at(&lane.points, Tick(20))
        };

        app.recording.audio.defer_automation(pass(0.25));
        app.recording.automation = pass(0.75);
        app.sync_project();
        assert_eq!(compiled_value(&app), Some(0.75));
        let sent = app.session.engine().programs_sent();
        app.sync_project();
        assert_eq!(app.session.engine().programs_sent(), sent);

        // Replacing one segment with another must publish even at the same document revision.
        let before = app.session.document().revision();
        app.recording.automation = pass(0.6);
        app.sync_project();
        assert_eq!(app.session.document().revision(), before);
        assert_eq!(compiled_value(&app), Some(0.6));
        assert_eq!(app.session.engine().programs_sent(), sent + 1);

        // Changing the document and the draft together compiles one project.
        app.session
            .document_mut()
            .apply("Metronome", |p| {
                project_edits::set_project_prop(p, ProjectProp::Metronome(!p.metronome))
            })
            .unwrap();
        app.recording.automation = pass(0.65);
        app.sync_project();
        assert_ne!(
            app.session.engine().program().unwrap().metronome,
            project.metronome
        );
        assert_eq!(compiled_value(&app), Some(0.65));
        assert_eq!(app.session.engine().programs_sent(), sent + 2);

        // During a fast-path gesture, unchanged drafts can wait. Changes in the draft
        // still compile, even when the command covered the whole document edit.
        app.session.document_mut().begin_gesture("Master gain");
        let before = app.session.document().revision();
        app.session
            .document_mut()
            .apply("Master gain", |p| {
                project_edits::set_master_prop(p, ChannelProp::VolumeDb(-6.0))
            })
            .unwrap();
        let after = app.session.document().revision();
        app.session
            .engine_mut()
            .fast_path(Command::SetMasterGain(0.5), before, after);
        app.sync_project();
        assert_eq!(app.session.engine().programs_sent(), sent + 2);
        app.recording.automation = AutomationCapture::default();
        app.sync_project();
        assert_eq!(compiled_value(&app), Some(0.25));
        assert_eq!(app.session.engine().programs_sent(), sent + 3);
        app.session.document_mut().end_gesture();

        app.session.engine_mut().invalidate_project();
        app.sync_project();
        assert_eq!(compiled_value(&app), Some(0.25));
        assert_eq!(app.session.engine().programs_sent(), sent + 4);

        // Cancelling the writer drops the last draft without changing the document or undo.
        let before = app.session.document().revision();
        app.discard_take();
        assert_eq!(app.session.document().revision(), before);
        assert_eq!(compiled_value(&app), None);
        assert_eq!(app.session.engine().programs_sent(), sent + 5);
        assert!(
            app.project()
                .master_automation
                .lane(&target)
                .unwrap()
                .points
                .is_empty()
        );
        assert_eq!(app.session.document().undo_label(), Some("Master gain"));
        app.shutdown();
    }

    #[test]
    fn looped_clip_write_can_be_discarded_and_then_undone_in_one_step() {
        use crate::actions::{Action, EditAction, TransportAction};
        use komponist_model::edits::{ChannelProp, TrackProp};

        let mut project = fixtures::demo_project();
        project.loop_on = false;
        // This test exercises channel pan and clip automation; use the demo's clip without
        // opening unrelated plugins or applying their independent startup preset notifications.
        project.tracks.retain(|track| track.name == "Lead");
        let track = std::sync::Arc::make_mut(&mut project.tracks[0]);
        track.channel.devices.clear();
        let (track, clip, start) = (track.id, track.clips[0].id, track.clips[0].start);
        let owner = AutomationOwner::Track(track);
        let target = AutomationTarget::Pan;
        let period = Tick(480);
        let project =
            komponist_model::edits::set_clip_loop(&project, clip, Some(period), Tick(1440))
                .unwrap();
        let project =
            edits::add_clip_automation(&project, clip, target.clone(), "Pan".into()).unwrap();
        let project = edits::set_clip_points(
            &project,
            clip,
            &target,
            Points::new(vec![Point::linear(Tick::ZERO, 0.5)]),
        )
        .unwrap();
        let lane = edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .id;
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();

        let mut app = App::headless(project.clone());
        app.session.engine_mut().hold_engine().unwrap();
        let advance = |app: &mut App, frames| {
            assert!(app.session.engine_mut().run_held(frames, &[], 0));
            app.step();
        };
        let transport = |app: &mut App, action| app.dispatch(Action::Transport(action));
        let write = |app: &mut App, pan| {
            transport(app, TransportAction::Locate(start + Tick(32)));
            advance(app, 64);
            if app.transport() == TransportState::Stopped {
                transport(app, TransportAction::TogglePlay);
                advance(app, 64);
            }
            app.dispatch(Action::Edit(EditAction::BeginGesture {
                label: "Pan".into(),
            }));
            app.dispatch(Action::Edit(EditAction::SetTrackProp {
                tracks: vec![track],
                prop: TrackProp::Channel(ChannelProp::Pan(pan)),
            }));
            advance(app, 1024);
            app.dispatch(Action::Edit(EditAction::EndGesture));
            advance(app, 1024);
            assert!(app.recording.automation.is_writing(owner, lane));
        };
        write(&mut app, -0.6);
        assert!(app.recording.automation.has_points());
        // Locate has reached the engine but the app has not drained its boundary yet.
        transport(&mut app, TransportAction::Locate(start + Tick(100)));
        assert!(app.session.engine_mut().run_held(64, &[], 0));
        transport(&mut app, TransportAction::DiscardTake);
        advance(&mut app, 64);
        assert!(!app.recording.automation.active());
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.session.document().undo_label().is_none());

        // A published preview can also be owned by the audio writer while it drains.
        write(&mut app, -0.6);
        let playhead = app.session.engine().playhead();
        app.recording.automation.end_segments(&project, playhead);
        app.sync_project();
        let deferred = app.take_automation_capture();
        app.recording.audio.defer_automation(deferred);
        let published_program = app.session.engine().programs_sent();
        app.sync_project();
        assert_eq!(app.session.engine().programs_sent(), published_program);
        transport(&mut app, TransportAction::DiscardTake);
        assert!(app.session.engine().programs_sent() > published_program);
        assert!(app.recording.audio.pending_automation().is_none());
        advance(&mut app, 64);
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.session.document().undo_label().is_none());

        write(&mut app, 0.6);
        transport(&mut app, TransportAction::Stop);
        advance(&mut app, 256);
        let track_after = app.project().track(track).unwrap();
        assert!(
            track_after
                .automation
                .lane(&target)
                .unwrap()
                .points
                .is_empty()
        );
        let tick = start + Tick(56);
        let first = automation::effective_value_at(track_after, &target, tick).unwrap();
        assert!((first - 0.8).abs() < 1e-6, "captured pan {first}");
        assert_eq!(
            automation::effective_value_at(track_after, &target, tick + period),
            Some(first)
        );
        assert_eq!(
            app.session.document().undo_label(),
            Some("undo-automation-record")
        );
        app.dispatch(Action::Edit(EditAction::Undo));
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.session.document().undo_label().is_none());
    }
}
