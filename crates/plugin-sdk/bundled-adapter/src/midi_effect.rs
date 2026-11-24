//! Routes CLAP note events through the bundled devices' existing pre-instrument stages.

use std::ffi::{CStr, c_void};
use std::sync::Arc;

use komponist_device_core::SAMPLE_RATES;
use komponist_engine::processor::{EventBuffer, EventSink};
use komponist_engine::{
    EventKind, EventProcessor, MAX_EVENTS_PER_BLOCK, NOTE_ID_NONE, ProcessContext as EngineContext,
    TransportInfo,
};
use komponist_model::{Meter, PPQ, Tick, TickRange};
use komponist_native_clap_sdk::{NativeClapLayout, Panel};
use nice_plug::midi::{Channel, Key, MidiResult, VoiceID};
use nice_plug::prelude::*;

use super::{MidiDefinition, panel_for, parameter};

/// Bounded event-only CLAP plugin backed by a device spec's existing MIDI stage.
pub struct MidiEffect<T> {
    params: Arc<parameter::Parameters>,
    applied: Vec<Option<f64>>,
    stage: Box<dyn EventProcessor + Send>,
    input: EventBuffer,
    output: EventBuffer,
    // The bundled stages hold one input note per channel and key.
    input_notes: [[Option<VoiceID>; 128]; 16],
    active_channels: u16,
    recover_channels: u16,
    offline: bool,
    definition: std::marker::PhantomData<T>,
}

impl<T: MidiDefinition> Default for MidiEffect<T> {
    fn default() -> Self {
        let params = parameter::Parameters::new(T::SPEC, T::choice);
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(params),
            stage: T::event_stage(),
            input: EventBuffer::with_insertion_order(MAX_EVENTS_PER_BLOCK),
            // A stage can output an off followed by an on for every repeated input key.
            output: EventBuffer::with_insertion_order(MAX_EVENTS_PER_BLOCK * 2),
            input_notes: [[None; 128]; 16],
            active_channels: 0,
            recover_channels: 0,
            offline: false,
            definition: std::marker::PhantomData,
        }
    }
}

impl<T: MidiDefinition> MidiEffect<T> {
    fn apply_params(&mut self) {
        for (index, applied) in self.applied.iter_mut().enumerate() {
            if let Some(value) = self.params.changed_value(index, applied) {
                self.stage.set_param(index as u32, value);
            }
        }
    }

    fn reconcile(&mut self) {
        self.stage.reset();
        self.input_notes.fill([None; 128]);
        self.recover_channels |= std::mem::take(&mut self.active_channels);
    }
}

impl<T: MidiDefinition> Plugin for MidiEffect<T> {
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

    fn filter_state(state: &mut PluginState) {
        T::migrate_state(state);
    }

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
        while let Some(event) = context.next_event() {
            push_input(&mut input, &mut self.input_notes, &event);
        }
        if input.overflowed() || self.recover_channels != 0 {
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
        if output.overflowed() {
            self.reconcile();
            return if self.offline {
                ProcessStatus::Error("MIDI effect generated too many events")
            } else {
                ProcessStatus::KeepAlive
            };
        }
        let mut overflow = false;
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

impl<T: MidiDefinition> ClapPlugin for MidiEffect<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        let preset = crate::presets::load_extension::<T>(id);
        if !preset.is_null() {
            return preset;
        }
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl<T: MidiDefinition> NativeClapLayout for MidiEffect<T> {
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

fn push_input(
    input: &mut EventSink<'_>,
    held: &mut [[Option<VoiceID>; 128]; 16],
    note: &NoteEvent<()>,
) {
    let timing = note.timing();
    let note_id = |voice: VoiceID| {
        voice
            .id()
            .and_then(|id| u32::try_from(id).ok())
            .unwrap_or(NOTE_ID_NONE)
    };
    match *note {
        NoteEvent::NoteOn {
            channel: Channel::Number(channel),
            key: Key::Number(key),
            voice_id,
            velocity,
            ..
        } if channel < 16 && key < 128 => {
            held[usize::from(channel)][usize::from(key)] = Some(voice_id);
            let velocity = (velocity * 127.0).round().clamp(1.0, 127.0) as u8;
            input.push(
                timing,
                EventKind::NoteOn {
                    channel,
                    key,
                    note_id: note_id(voice_id),
                    velocity,
                },
            );
        }
        NoteEvent::Choke {
            channel: Channel::Wildcard,
            key: Key::Wildcard,
            voice_id: VoiceID::Wildcard,
            ..
        } => {
            held.fill([None; 128]);
            input.push(timing, EventKind::AllNotesOff);
        }
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
            if let (Some(channel), Some(key)) = (channel.number(), key.number()) {
                if channel < 16 && key < 128 {
                    let slot = &mut held[usize::from(channel)][usize::from(key)];
                    if slot.is_some_and(|id| voice_id.is_wildcard() || id == voice_id) {
                        *slot = None;
                        input.push(
                            timing,
                            EventKind::NoteOff {
                                channel,
                                key,
                                note_id: note_id(voice_id),
                            },
                        );
                    }
                }
            } else {
                let range = |number: Option<u8>, limit| {
                    number.map_or(0..limit, |number| {
                        let index = usize::from(number).min(limit);
                        index..(index + 1).min(limit)
                    })
                };
                let channels = range(channel.number(), 16);
                let keys = range(key.number(), 128);
                for (ch, channel_notes) in held
                    .iter_mut()
                    .enumerate()
                    .take(channels.end)
                    .skip(channels.start)
                {
                    for (k, slot) in channel_notes
                        .iter_mut()
                        .enumerate()
                        .take(keys.end)
                        .skip(keys.start)
                    {
                        if let Some(id) = *slot
                            && (voice_id.is_wildcard() || voice_id == id)
                        {
                            *slot = None;
                            if !input.push(
                                timing,
                                EventKind::NoteOff {
                                    channel: ch as u8,
                                    key: k as u8,
                                    note_id: note_id(id),
                                },
                            ) {
                                return;
                            }
                        }
                    }
                }
            }
        }
        _ => {
            if let Some(MidiResult::Basic(bytes)) = note.as_midi() {
                let [status, control, _] = bytes;
                if status & 0xf0 == 0xb0 && matches!(control, 120 | 123) {
                    held[usize::from(status & 0x0f)].fill(None);
                }
                input.push(timing, EventKind::Midi(bytes));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn note_on(channel: u8, key: u8, id: i32) -> NoteEvent<()> {
        NoteEvent::NoteOn {
            timing: 4,
            channel: Channel::Number(channel),
            key: Key::Number(key),
            voice_id: VoiceID::ID(id),
            velocity: 1.0,
        }
    }

    #[test]
    fn wildcard_release_keeps_same_frame_order_and_channel_scope() {
        let mut buffer = EventBuffer::with_insertion_order(4);
        let mut held = [[None; 128]; 16];
        let mut input = buffer.sink(32);
        push_input(&mut input, &mut held, &note_on(2, 61, 91));
        push_input(&mut input, &mut held, &note_on(3, 61, 92));
        push_input(
            &mut input,
            &mut held,
            &NoteEvent::NoteOff {
                timing: 4,
                channel: Channel::Number(2),
                key: Key::Wildcard,
                voice_id: VoiceID::ID(91),
                velocity: 0.0,
            },
        );

        let events = buffer.events();
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0].kind,
            EventKind::NoteOn {
                channel: 2,
                note_id: 91,
                ..
            }
        ));
        assert!(matches!(
            events[1].kind,
            EventKind::NoteOn {
                channel: 3,
                note_id: 92,
                ..
            }
        ));
        assert!(matches!(
            events[2].kind,
            EventKind::NoteOff {
                channel: 2,
                key: 61,
                note_id: 91
            }
        ));
        assert!(events.iter().all(|event| event.offset == 4));
        assert_eq!(held[2][61], None);
        assert_eq!(held[3][61], Some(VoiceID::ID(92)));
    }

    #[test]
    fn output_is_overflowed_only_if_a_push_is_rejected() {
        let mut buffer = EventBuffer::with_insertion_order(2);
        let mut output = buffer.sink(32);
        assert!(output.push(4, EventKind::Midi([0xb0, 1, 100])));
        assert!(output.push(4, EventKind::Midi([0xb0, 1, 101])));
        assert_eq!(output.room(), 0);
        assert!(!output.overflowed());
        assert!(!output.push(4, EventKind::Midi([0xb0, 1, 102])));
        assert!(output.overflowed());
    }
}
