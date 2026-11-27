//! Owns the recording attempt across count-in, capture and deferred audio finalization.

pub mod audio_record;
pub(crate) mod automation_capture;

use komponist_engine::EngineEvent;
use komponist_engine::automation::EngineTarget;
use komponist_i18n::Lang;
use komponist_model::automation::{AutomationOwner, LaneId};
use komponist_model::{Project, Tick};

use crate::engine_sync::DraftOverlay;
use crate::record::{Commit, Recording};
use audio_record::AudioRecording;
use automation_capture::AutomationCapture;

#[derive(Default)]
pub(crate) struct RecordingController {
    midi: Option<Recording>,
    audio: audio_record::State,
    automation: AutomationCapture,
    controls: Vec<(AutomationOwner, EngineTarget)>,
    automation_stop_pending: bool,
    record_from: Option<Tick>,
    // Ignore a count-in event from a recording attempt that has since ended.
    take_expected: bool,
}

impl RecordingController {
    pub(crate) fn midi(&self) -> Option<&Recording> {
        self.midi.as_ref()
    }

    pub(crate) fn audio(&self) -> Option<&AudioRecording> {
        self.audio.running()
    }

    pub(crate) fn automation(&self) -> &AutomationCapture {
        &self.automation
    }

    pub(crate) fn draft(&self) -> DraftOverlay<'_> {
        DraftOverlay::new(self.audio.pending_automation(), &self.automation)
    }

    pub(crate) fn references_lane(&self, owner: AutomationOwner, lane: LaneId) -> bool {
        self.automation.references_lane(owner, lane)
            || self.audio.references_automation_lane(owner, lane)
    }

    pub(crate) fn capturing_notes_or_audio(&self) -> bool {
        self.midi.is_some() || self.audio.is_active()
    }

    pub(crate) fn active(&self) -> bool {
        self.capturing_notes_or_audio() || self.automation.active()
    }

    pub(crate) fn has_material(&self, recording: bool) -> bool {
        self.midi.as_ref().is_some_and(Recording::has_material)
            || self.audio.has_material()
            || self.automation.has_points()
            || (self.audio.is_active() && recording)
    }

    pub(crate) fn expect_take(&mut self, from: Tick) {
        self.record_from = Some(from);
        self.take_expected = true;
    }

    pub(crate) fn expecting_take(&self) -> bool {
        self.take_expected
    }

    pub(crate) fn take_expected(&mut self) -> bool {
        std::mem::take(&mut self.take_expected)
    }

    pub(crate) fn start_midi(&mut self, project: &Project, at: Tick, lang: Lang) {
        let midi = Recording::new(project, at, lang);
        self.midi = midi.first().is_some().then_some(midi);
    }

    pub(crate) fn handle_midi(&mut self, event: &EngineEvent, project: &Project) {
        if let Some(midi) = &mut self.midi {
            midi.handle(event, project);
        }
    }

    pub(crate) fn interrupt(
        &mut self,
        at: Tick,
        first_incomplete_lap: u32,
        triggering_track: Option<komponist_model::TrackId>,
        project: &Project,
        lang: Lang,
    ) -> bool {
        if self.midi.is_none()
            && triggering_track.is_some()
            && let Some(audio) = self.audio.running()
        {
            // Audio recording can gain its first MIDI input after capture starts.
            let start = if first_incomplete_lap == 0 {
                audio.start
            } else {
                project.loop_range.start
            };
            self.midi = Some(Recording::new(project, start, lang));
        }
        if let Some(midi) = &mut self.midi {
            midi.interrupt(at, first_incomplete_lap, triggering_track, project);
        }
        self.capturing_notes_or_audio()
    }

    pub(crate) fn update_midi(&mut self, playhead: Tick) {
        if let Some(midi) = &mut self.midi {
            midi.update(playhead);
        }
    }

    pub(crate) fn finish_midi(&mut self, project: &Project, at: Tick) -> Option<Commit> {
        self.midi.take().and_then(|midi| midi.commit(project, at))
    }

    pub(crate) fn audio_active(&self) -> bool {
        self.audio.is_active()
    }

    pub(crate) fn finish_with_audio(
        &mut self,
        midi: Option<Commit>,
        automation: AutomationCapture,
    ) {
        self.audio.defer_midi(midi);
        self.audio.defer_automation(automation);
        self.audio.stop();
    }

    pub(crate) fn wait_for_automation_stop(&mut self) -> bool {
        if self.automation.active() {
            self.automation_stop_pending = true;
            return true;
        }
        false
    }

    pub(crate) fn cancel_take(&mut self) -> Option<Tick> {
        self.take_expected = false;
        self.midi = None;
        self.audio.cancel();
        self.record_from
    }

    pub(crate) fn reset_take(&mut self) {
        self.take_expected = false;
        self.midi = None;
        self.audio.reset();
    }

    pub(crate) fn shutdown(&mut self) {
        self.audio.shutdown(audio_record::TAKE_FINISH_TIMEOUT);
    }
}
