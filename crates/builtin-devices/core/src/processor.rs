//! Audio, MIDI, parameter and meter plumbing for in-process devices.

use std::sync::Arc;

use komponist_engine::{
    DeviceMeter, Event, EventKind, EventProcessor, NOTE_ID_NONE, ProcessContext, ProcessStatus,
    Processor, ProcessorError, SampleTap, StereoIo,
};
use komponist_model::params::ParamInfo;

use crate::{Spec, midi};

/// Sample rates accepted by the built-in audio processors (Hz).
pub const SAMPLE_RATES: core::ops::RangeInclusive<f32> = 8_000.0..=384_000.0;

/// Sample rate a processor is built for before its first [`Processor::activate`].
pub const INITIAL_RATE: f32 = 48_000.0;

/// The DSP of an internal device. Created and activated on the UI thread, then used by one thread
/// at a time on the audio side.
pub trait Dsp: Send {
    /// Non-RT, may allocate (delay lines). Prepares for `sample_rate`; keeps the parameters.
    fn activate(&mut self, sample_rate: f32);

    /// RT. Parameter `index` (the wire id) now has plain value `value`, already snapped.
    fn set_param(&mut self, index: usize, value: f64);

    /// RT. Processes `input` into `output` (same length, never aliasing).
    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]);

    /// RT. Like [`Dsp::process`] with an external sidechain signal (WP-104), aligned with `input`
    /// and of the same length. Only called on devices whose [`crate::CatalogSpec::sidechain`] is set; the default
    /// ignores the sidechain.
    fn process_sidechain(
        &mut self,
        input: [&[f32]; 2],
        sidechain: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) {
        let _ = sidechain;
        self.process(input, output);
    }

    /// RT. Clears tails and filter state without allocating.
    fn reset(&mut self);

    /// RT-safe, cached. Latency in frames.
    fn latency(&self) -> u32 {
        0
    }

    /// Non-RT. The reduction in dB at which the device's gain-reduction bar is full.
    fn gr_range_db(&self) -> f32 {
        DeviceMeter::DEFAULT_GR_RANGE_DB
    }

    /// RT. The tempo of the transport in quarter notes per minute, delivered at the start of every
    /// call (constant within one). Tempo-synced effects follow it; the default ignores it.
    fn set_tempo(&mut self, _bpm: f64) {}

    /// Non-RT. The tap of the device's output samples for a live display (WP-92), if it has one.
    /// The DSP copies into it only while the UI looks ([`komponist_engine::SampleTap::begin`]).
    fn tap(&self) -> Option<Arc<SampleTap>> {
        None
    }

    /// RT. The largest gain reduction in dB (≥ 0) since the last call, then resets it. Only
    /// dynamics devices report one; the meter feed ([`InternalProcessor::meter`])
    /// calls it once per rendered piece.
    fn take_reduction_db(&mut self) -> f32 {
        0.0
    }

    /// Whether the device is an instrument: it ignores its input, overwrites the output and gets
    /// the note events ([`Dsp::note_on`] …) at their frame (the sampler, WP-87).
    fn is_instrument(&self) -> bool {
        false
    }

    /// RT, instruments only. A note starts: `key` 0 ..= 127, `velocity` 1 ..= 127.
    fn note_on(&mut self, key: u8, velocity: u8) {
        let _ = (key, velocity);
    }

    /// RT, instruments only. A note starts, optionally paired with an engine-assigned id.
    fn note_on_with_id(
        &mut self,
        key: u8,
        velocity: u8,
        _note_id: Option<u32>,
        _channel: Option<u8>,
    ) {
        self.note_on(key, velocity);
    }

    /// RT, instruments only. A note ends.
    fn note_off(&mut self, key: u8) {
        let _ = key;
    }

    /// RT, instruments only. A note ends, with its id when the engine paired it.
    fn note_off_with_id(&mut self, key: u8, _note_id: Option<u32>, _channel: Option<u8>) {
        self.note_off(key);
    }

    /// RT, instruments only. Release gates at a CLAP note address. Missing fields match any
    /// value. One-shot samples continue playing.
    fn release_note_gates(&mut self, key: Option<u8>, note_id: Option<u32>, channel: Option<u8>) {
        if let Some(key) = key {
            self.note_off_with_id(key, note_id, channel);
        }
    }

    /// RT, instruments only. Stop voices matching the supplied key, note ID, and channel.
    /// An entirely wildcard address requests a global choke.
    fn choke_note(&mut self, key: Option<u8>, note_id: Option<u32>, channel: Option<u8>) {
        if let Some(key) = key {
            self.note_off_with_id(key, note_id, channel);
        } else if note_id.is_none() && channel.is_none() {
            self.all_notes_off();
        }
    }

    /// RT, instruments only. Every sounding note ends now.
    fn all_notes_off(&mut self) {}

    /// RT, instruments only. The pitch wheel moved: −1 (all the way down) … +1 (all the way up),
    /// 0 at rest. The processor turns the 14-bit MIDI value into this (WP-95).
    fn pitch_bend(&mut self, value: f32) {
        let _ = value;
    }

    /// RT, instruments only. The mod wheel (CC 1) moved: 0 … 1 (WP-95).
    fn mod_wheel(&mut self, value: f32) {
        let _ = value;
    }

    /// RT, instruments only. Channel pressure (aftertouch): 0 … 1 (WP-95).
    fn pressure(&mut self, value: f32) {
        let _ = value;
    }
}

/// Hands a channel message to an instrument: pitch bend (14 bit, −1 … +1), the mod wheel (CC 1),
/// channel pressure and "all sound/notes off" (CC 120, 123). Other controllers and polyphonic
/// aftertouch are ignored; the channel does not matter (WP-95).
fn midi_to_dsp(dsp: &mut dyn Dsp, [status, d1, d2]: [u8; 3]) {
    match status & 0xF0 {
        0xE0 => {
            let value = (i32::from(d2 & 0x7F) << 7 | i32::from(d1 & 0x7F)) - 8192;
            dsp.pitch_bend((value as f32 / 8192.0).clamp(-1.0, 1.0));
        }
        0xB0 => match d1 {
            1 => dsp.mod_wheel(f32::from(d2 & 0x7F) / 127.0),
            120 | 123 => dsp.all_notes_off(),
            _ => {}
        },
        0xD0 => dsp.pressure(f32::from(d1 & 0x7F) / 127.0),
        _ => {}
    }
}

/// An internal device as an engine [`Processor`]: an audio effect, or a MIDI effect whose event
/// stage the engine runs in front of the instrument ([`Processor::event_processor`]).
///
/// `EventKind::Param { id, value }` sets the parameter with wire id `id` (unknown ids are ignored)
/// at its frame: the block is processed in pieces between the events. Note and MIDI events are
/// ignored, except for an instrument: it gets them, plus pitch bend, mod wheel and channel pressure
/// (`EventKind::Midi`, WP-95). [`Processor::latency`] is the DSP's.
pub struct InternalProcessor {
    params: &'static [ParamInfo],
    dsp: Box<dyn Dsp>,
    /// The event stage of a MIDI effect ([`Processor::event_processor`]).
    stage: Option<midi::Stage>,
    sample_rate: f32,
    /// Input peak, gain reduction and output peak for the UI (WP-89).
    meter: Arc<DeviceMeter>,
}

impl InternalProcessor {
    /// Creates an audio processor. Call `Processor::activate` before handing it to the engine.
    pub fn from_dsp(spec: &'static Spec, values: &[f64], dsp: Box<dyn Dsp>) -> Self {
        Self::with_dsp_and_stage(spec, values, dsp, None)
    }

    /// Creates a MIDI-only processor with a passthrough audio path.
    pub fn from_event_stage(
        spec: &'static Spec,
        values: &[f64],
        stage: Box<dyn EventProcessor + Send>,
    ) -> Self {
        Self::with_dsp_and_stage(spec, values, midi::passthrough(), Some(stage))
    }

    fn with_dsp_and_stage(
        spec: &'static Spec,
        values: &[f64],
        mut dsp: Box<dyn Dsp>,
        stage: Option<Box<dyn EventProcessor + Send>>,
    ) -> Self {
        let params = spec.params();
        for (i, p) in params.iter().enumerate() {
            dsp.set_param(i, p.snap(values.get(i).copied().unwrap_or(p.default)));
        }
        let mut stage = stage.map(|stage| midi::Stage::new(params, stage));
        if let Some(stage) = &mut stage {
            for (i, p) in params.iter().enumerate() {
                stage.set_param(i as u32, values.get(i).copied().unwrap_or(p.default));
            }
        }
        let meter = Arc::new(DeviceMeter::new());
        if let Some(tap) = dsp.tap() {
            meter.set_tap(tap);
        }
        Self {
            params,
            dsp,
            stage,
            sample_rate: INITIAL_RATE,
            meter,
        }
    }

    /// The level meter the processor feeds every block; the UI takes the values every frame
    /// ([`DeviceMeter::take`]).
    pub fn meter(&self) -> Arc<DeviceMeter> {
        Arc::clone(&self.meter)
    }

    /// The sample rate the processor was last activated at (48 kHz before that).
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }
}

impl Processor for InternalProcessor {
    fn activate(&mut self, sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        if !SAMPLE_RATES.contains(&sample_rate) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        self.sample_rate = sample_rate;
        self.dsp.activate(sample_rate);
        self.meter.set_latency(self.dsp.latency());
        self.meter.set_gr_range_db(self.dsp.gr_range_db());
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        self.run(ctx, events, io, None)
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        self.run(ctx, events, io, Some(sidechain))
    }

    fn latency(&self) -> u32 {
        self.dsp.latency()
    }

    fn reset(&mut self) {
        self.dsp.reset();
        if let Some(stage) = &mut self.stage {
            stage.reset();
        }
    }

    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor> {
        self.stage.as_mut().map(|s| s as &mut dyn EventProcessor)
    }
}

impl InternalProcessor {
    /// `process` and `process_sidechain`: the block in pieces between the parameter events.
    fn run(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sc: Option<[&[f32]; 2]>,
    ) -> ProcessStatus {
        self.dsp.set_tempo(ctx.transport.tempo_bpm);
        let frames = io.frames();
        let mut done = 0;
        let instrument = self.dsp.is_instrument();
        for event in events {
            // Parameters for every device; notes only reach an instrument.
            if !matches!(event.kind, EventKind::Param { .. })
                && !(instrument
                    && matches!(
                        event.kind,
                        EventKind::NoteOn { .. }
                            | EventKind::NoteOff { .. }
                            | EventKind::AllNotesOff
                            | EventKind::Midi(_)
                    ))
            {
                continue;
            }
            let at = (event.offset as usize).clamp(done, frames);
            if at > done {
                self.render(io, sc, done, at);
                done = at;
            }
            match event.kind {
                EventKind::Param { id, value } => {
                    if let Some(info) = self.params.get(id as usize) {
                        self.dsp.set_param(id as usize, info.snap(value));
                        // A parameter can change the latency (limiter lookahead); the UI thread
                        // reads it from the meter and recompiles the delay compensation.
                        self.meter.set_latency(self.dsp.latency());
                    }
                }
                EventKind::NoteOn {
                    key,
                    velocity,
                    note_id,
                    channel,
                } => self.dsp.note_on_with_id(
                    key,
                    velocity,
                    (note_id != NOTE_ID_NONE).then_some(note_id),
                    Some(channel),
                ),
                EventKind::NoteOff {
                    key,
                    note_id,
                    channel,
                } => self.dsp.note_off_with_id(
                    key,
                    (note_id != NOTE_ID_NONE).then_some(note_id),
                    Some(channel),
                ),
                EventKind::AllNotesOff => self.dsp.all_notes_off(),
                EventKind::Midi(bytes) => midi_to_dsp(self.dsp.as_mut(), bytes),
            }
        }
        if done < frames {
            self.render(io, sc, done, frames);
        }
        ProcessStatus::Continue
    }

    fn render(&mut self, io: &mut StereoIo<'_>, sc: Option<[&[f32]; 2]>, from: usize, to: usize) {
        let [il, ir] = io.input;
        let [ol, or] = &mut io.output;
        let input = [&il[from..to], &ir[from..to]];
        let output = [&mut ol[from..to], &mut or[from..to]];
        match sc {
            Some([sl, sr]) if sl.len() >= to && sr.len() >= to => {
                self.dsp
                    .process_sidechain(input, [&sl[from..to], &sr[from..to]], output);
            }
            _ => self.dsp.process(input, output),
        }
        let peak = |a: &[f32], b: &[f32]| a.iter().chain(b).fold(0.0_f32, |m, s| m.max(s.abs()));
        self.meter.record(
            peak(&il[from..to], &ir[from..to]),
            self.dsp.take_reduction_db(),
            peak(&ol[from..to], &or[from..to]),
        );
    }
}
