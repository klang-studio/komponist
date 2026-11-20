//! Shared audio placeholder and parameter handling for MIDI effects.

use komponist_engine::{Event, EventProcessor, EventSink, ProcessContext};
use komponist_model::params::ParamInfo;

use super::Dsp;

/// The audio side of a MIDI effect: copies input to output. Never called by the engine.
struct PassThrough;

/// A [`Spec::dsp`](super::Spec::dsp) for MIDI effects.
pub fn passthrough() -> Box<dyn Dsp> {
    Box::new(PassThrough)
}

impl Dsp for PassThrough {
    fn activate(&mut self, _sample_rate: f32) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [ol, or] = output;
        ol.copy_from_slice(input[0]);
        or.copy_from_slice(input[1]);
    }

    fn reset(&mut self) {}
}

/// A MIDI effect's stage with the device's parameter table: values are snapped to their range, wire
/// ids outside the table are ignored (as for the audio effects).
pub(super) struct Stage {
    params: &'static [ParamInfo],
    stage: Box<dyn EventProcessor + Send>,
}

impl Stage {
    pub(super) fn new(
        params: &'static [ParamInfo],
        stage: Box<dyn EventProcessor + Send>,
    ) -> Stage {
        Stage { params, stage }
    }
}

impl EventProcessor for Stage {
    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(info) = self.params.get(id as usize) {
            self.stage.set_param(id, info.snap(value));
        }
    }

    fn process_events(&mut self, ctx: &ProcessContext, input: &[Event], out: &mut EventSink<'_>) {
        self.stage.process_events(ctx, input, out);
    }

    fn reset(&mut self) {
        self.stage.reset();
    }
}

/// Names of the twelve pitch classes as the piano roll writes them (`komponist_model::note_name`).
pub const PITCH_CLASSES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
