//! Routes CLAP note events through the bundled devices' existing pre-instrument stages.

use std::ffi::{CStr, c_void};
use std::sync::Arc;

use komponist_device_core::SAMPLE_RATES;
use komponist_engine::processor::EventBuffer;
use komponist_engine::{
    EventKind, EventProcessor, MAX_EVENTS_PER_BLOCK, NOTE_ID_NONE, ProcessContext as EngineContext,
    TransportInfo,
};
use komponist_model::{Meter, PPQ, Tick, TickRange};
use komponist_native_clap_sdk::{NativeClapLayout, Panel};
use nice_plug::midi::{Channel, Key, MidiResult, VoiceID};
use nice_plug::prelude::*;

use super::{DeviceDefinition, panel_for, parameter};

/// Bounded event-only CLAP plugin backed by a device spec's existing MIDI stage.
pub struct MidiEffect<T> {
    params: Arc<parameter::Parameters>,
    applied: Vec<Option<f64>>,
    stage: Box<dyn EventProcessor + Send>,
    input: EventBuffer,
    output: EventBuffer,
    active_channels: u16,
    recover_channels: u16,
    offline: bool,
    definition: std::marker::PhantomData<T>,
}

impl<T: DeviceDefinition> Default for MidiEffect<T> {
    fn default() -> Self {
        let params = parameter::Parameters::new(T::SPEC, T::choice);
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(params),
            stage: (T::SPEC
                .event_stage
                .expect("MIDI device spec needs an event stage"))(),
            input: EventBuffer::new(MAX_EVENTS_PER_BLOCK),
            // A stage can output an off followed by an on for every repeated input key.
            output: EventBuffer::new(MAX_EVENTS_PER_BLOCK * 2),
            active_channels: 0,
            recover_channels: 0,
            offline: false,
            definition: std::marker::PhantomData,
        }
    }
}

impl<T: DeviceDefinition> MidiEffect<T> {
    fn apply_params(&mut self) {
        for (index, applied) in self.applied.iter_mut().enumerate() {
            if let Some(value) = self.params.changed_value(index, applied) {
                self.stage.set_param(index as u32, value);
            }
        }
    }

    fn reconcile(&mut self) {
        self.stage.reset();
        self.recover_channels |= std::mem::take(&mut self.active_channels);
    }
}

impl<T: DeviceDefinition> Plugin for MidiEffect<T> {
    const NAME: &'static str = T::NAME;
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = T::VERSION;
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[];

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn activate(
        &mut self,
        _layout: &AudioIOLayout,
        config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        if !SAMPLE_RATES.contains(&config.sample_rate) {
            return false;
        }
        self.offline = config.process_mode == ProcessMode::Offline;
        self.apply_params();
        true
    }

    fn reset(&mut self) {
        self.reconcile();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let frames = buffer.samples() as u32;
        if frames == 0 {
            return ProcessStatus::KeepAlive;
        }
        self.apply_params();
        release_channels(context, 0, &mut self.recover_channels);
        let engine = engine_context(context.transport(), frames, self.offline);
        let mut input = self.input.sink(frames);
        let mut overflow = false;
        while let Some(event) = context.next_event() {
            if let Some(kind) = to_engine_event(&event) {
                overflow |= !input.push(event.timing(), kind);
            }
        }
        if overflow || self.recover_channels != 0 {
            self.reconcile();
            return if self.offline {
                ProcessStatus::Error("MIDI effect input or recovery overflowed")
            } else {
                ProcessStatus::KeepAlive
            };
        }

        let mut output = self.output.sink(frames);
        self.stage
            .process_events(&engine, self.input.events(), &mut output);
        if output.room() == 0 {
            self.reconcile();
            return if self.offline {
                ProcessStatus::Error("MIDI effect generated too many events")
            } else {
                ProcessStatus::KeepAlive
            };
        }
        for event in self.output.events() {
            if event.kind == EventKind::AllNotesOff {
                self.recover_channels |= std::mem::take(&mut self.active_channels);
                release_channels(context, event.offset, &mut self.recover_channels);
                if self.recover_channels != 0 {
                    overflow = true;
                    break;
                }
            } else if let Some(note) = from_engine_event(event.offset, event.kind) {
                if context.try_send_event(note).is_err() {
                    overflow = true;
                    break;
                }
                if let EventKind::NoteOn { channel, .. } = event.kind {
                    self.active_channels |= 1 << channel;
                }
            }
        }
        if overflow {
            self.reconcile();
            if self.offline {
                return ProcessStatus::Error("MIDI effect output was rejected");
            }
        }
        // Arpeggiation can schedule note-ons and note-offs without fresh input.
        ProcessStatus::KeepAlive
    }
}

impl<T: DeviceDefinition> ClapPlugin for MidiEffect<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl<T: DeviceDefinition> NativeClapLayout for MidiEffect<T> {
    fn panel() -> Panel {
        panel_for::<T>()
    }
}

fn release_channels<P: Plugin<SysExMessage = ()>>(
    context: &mut impl ProcessContext<P>,
    timing: u32,
    channels: &mut u16,
) {
    for channel in 0u8..16 {
        let bit = 1 << channel;
        if *channels & bit != 0 {
            let message = NoteEvent::MidiCC {
                timing,
                channel,
                cc: 123,
                value: 0.0,
            };
            if context.try_send_event(message).is_err() {
                break;
            }
            *channels &= !bit;
        }
    }
}

fn engine_context(transport: &Transport, frames: u32, offline: bool) -> EngineContext {
    let beats = transport.pos_beats().filter(|beats| beats.is_finite());
    let tempo_bpm = transport
        .tempo
        .filter(|tempo| tempo.is_finite() && *tempo > 0.0);
    let loop_range = transport
        .loop_range_beats()
        .filter(|(start, end)| start.is_finite() && end.is_finite() && start < end)
        .map(|(start, end)| {
            TickRange::new(
                Tick((start * PPQ as f64).round() as i64),
                Tick((end * PPQ as f64).round() as i64),
            )
        });

    let meter = match (transport.time_sig_numerator, transport.time_sig_denominator) {
        (Some(num), Some(den))
            if (1..=u8::MAX as i32).contains(&num) && (1..=u8::MAX as i32).contains(&den) =>
        {
            Meter {
                num: num as u8,
                den: den as u8,
            }
        }
        _ => Meter::FOUR_FOUR,
    };
    EngineContext {
        frames,
        sample_rate: transport.sample_rate,
        offline,
        deadline: None,
        transport: TransportInfo {
            playing: transport.playing && beats.is_some() && tempo_bpm.is_some(),
            recording: transport.recording,
            looping: loop_range.is_some(),
            tempo_bpm: tempo_bpm.unwrap_or(120.0),
            meter,
            position_ticks: beats.unwrap_or(0.0) * PPQ as f64,
            loop_range: loop_range.unwrap_or(TickRange::new(Tick(0), Tick(0))),
        },
    }
}

fn to_engine_event(note: &NoteEvent<()>) -> Option<EventKind> {
    let address = |channel: Channel, key: Key, voice: VoiceID| {
        let channel = channel.number().filter(|channel| *channel < 16)?;
        let key = key.number().filter(|key| *key < 128)?;
        let note_id = voice
            .id()
            .and_then(|id| u32::try_from(id).ok())
            .unwrap_or(NOTE_ID_NONE);
        Some((channel, key, note_id))
    };
    match *note {
        NoteEvent::NoteOn {
            channel,
            key,
            voice_id,
            velocity,
            ..
        } => {
            let (channel, key, note_id) = address(channel, key, voice_id)?;
            let velocity = (velocity * 127.0).round().clamp(1.0, 127.0) as u8;
            Some(EventKind::NoteOn {
                channel,
                key,
                note_id,
                velocity,
            })
        }
        NoteEvent::Choke {
            channel: Channel::Wildcard,
            key: Key::Wildcard,
            voice_id: VoiceID::Wildcard,
            ..
        } => Some(EventKind::AllNotesOff),
        NoteEvent::NoteOff {
            channel,
            key,
            voice_id,
            ..
        }
        | NoteEvent::Choke {
            channel,
            key,
            voice_id,
            ..
        } => {
            let (channel, key, note_id) = address(channel, key, voice_id)?;
            Some(EventKind::NoteOff {
                channel,
                key,
                note_id,
            })
        }
        _ => {
            if let Some(MidiResult::Basic(bytes)) = note.as_midi() {
                Some(EventKind::Midi(bytes))
            } else {
                None
            }
        }
    }
}

fn from_engine_event(timing: u32, event: EventKind) -> Option<NoteEvent<()>> {
    let voice = |id| i32::try_from(id).map_or(VoiceID::Wildcard, VoiceID::ID);
    match event {
        EventKind::NoteOn {
            channel,
            key,
            note_id,
            velocity,
        } => Some(NoteEvent::NoteOn {
            timing,
            voice_id: voice(note_id),
            channel: Channel::Number(channel),
            key: Key::Number(key),
            velocity: f32::from(velocity) / 127.0,
        }),
        EventKind::NoteOff {
            channel,
            key,
            note_id,
        } => Some(NoteEvent::NoteOff {
            timing,
            voice_id: voice(note_id),
            channel: Channel::Number(channel),
            key: Key::Number(key),
            velocity: 0.0,
        }),
        EventKind::Midi(bytes) => NoteEvent::from_midi(timing, &bytes).ok(),
        EventKind::AllNotesOff | EventKind::Param { .. } => None,
    }
}
