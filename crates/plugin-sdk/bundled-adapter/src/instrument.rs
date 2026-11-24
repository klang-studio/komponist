//! Spec instrument adapter using the same DSP and ordered event slices as the engine.

use std::ffi::{CStr, c_void};
use std::marker::PhantomData;
use std::sync::Arc;

use komponist_device_core::{DeviceNote, Dsp, SAMPLE_RATES, dispatch_note, pitch_bend_bytes};
use komponist_native_clap_sdk::{NativeClapLayout, Panel};
use nice_plug::midi::{Channel, Key, VoiceID};
use nice_plug::prelude::*;

use crate::{AudioDefinition, STEREO, panel_for, parameter};

/// A stereo instrument backed by an existing spec DSP.
pub struct Instrument<T> {
    params: Arc<parameter::Parameters>,
    dsp: Box<dyn Dsp>,
    applied: Vec<Option<f64>>,
    silence: Vec<f32>,
    // Volt retriggers voices by key. Track the corresponding CLAP address for each key.
    notes: [Option<(Channel, VoiceID)>; 128],
    definition: PhantomData<T>,
}

impl<T: AudioDefinition> Default for Instrument<T> {
    fn default() -> Self {
        let params = parameter::Parameters::new(T::SPEC, T::choice);
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(params),
            dsp: T::dsp(),
            silence: Vec::new(),
            notes: [None; 128],
            definition: PhantomData,
        }
    }
}

impl<T: AudioDefinition> Instrument<T> {
    fn apply_params(&mut self) {
        for (index, applied) in self.applied.iter_mut().enumerate() {
            if let Some(value) = self.params.changed_value(index, applied) {
                self.dsp.set_param(index, value);
            }
        }
    }
}

impl<T: AudioDefinition> Plugin for Instrument<T> {
    const NAME: &'static str = T::NAME;
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = T::VERSION;
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_output_channels: Some(STEREO),
        ..AudioIOLayout::const_default()
    }];

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
        context: &mut impl ActivateContext<Self>,
    ) -> bool {
        if !SAMPLE_RATES.contains(&config.sample_rate) {
            return false;
        }
        self.silence.resize(config.max_buffer_size as usize, 0.0);
        self.apply_params();
        self.dsp.activate(config.sample_rate);
        context.set_latency_samples(self.dsp.latency());
        true
    }

    fn reset(&mut self) {
        self.apply_params();
        self.notes.fill(None);
        self.dsp.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        self.apply_params();
        if let Some(tempo) = context.transport().tempo {
            self.dsp.set_tempo(tempo);
        }
        let [left, right] = buffer.as_slice() else {
            return ProcessStatus::Error("expected stereo main outputs");
        };
        let silence = &self.silence[..left.len()];
        let mut done = 0;
        while let Some(event) = context.next_event() {
            let at = (event.timing() as usize).clamp(done, left.len());
            if at > done {
                self.dsp.process(
                    [&silence[done..at]; 2],
                    [&mut left[done..at], &mut right[done..at]],
                );
                done = at;
            }
            apply_note(&mut *self.dsp, &mut self.notes, event);
        }
        if done < left.len() {
            self.dsp.process(
                [&silence[done..]; 2],
                [&mut left[done..], &mut right[done..]],
            );
        }
        ProcessStatus::KeepAlive
    }
}

pub(crate) fn apply_note(
    dsp: &mut dyn Dsp,
    notes: &mut [Option<(Channel, VoiceID)>; 128],
    event: NoteEvent<()>,
) {
    match event {
        NoteEvent::NoteOn {
            channel,
            key,
            voice_id,
            ..
        } => {
            if let Some(key) = key.number().filter(|key| *key < 128) {
                notes[usize::from(key)] = Some((channel, voice_id));
                if let Some(note) = to_device_note(event) {
                    dispatch_note(dsp, note);
                }
            }
        }
        NoteEvent::Choke {
            channel: Channel::Wildcard,
            key: Key::Wildcard,
            voice_id: VoiceID::Wildcard,
            ..
        } => {
            notes.fill(None);
            dispatch_note(dsp, DeviceNote::AllNotesOff);
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
            let choke = matches!(event, NoteEvent::Choke { .. });
            let mut release = |key, address: &mut Option<(Channel, VoiceID)>| {
                if address.is_some_and(|(note_ch, note_id)| {
                    (channel.is_wildcard() || channel == note_ch)
                        && (voice_id.is_wildcard() || voice_id == note_id)
                }) {
                    let note_id = voice_id.id().map(|id| id as u32);
                    if choke {
                        dispatch_note(
                            dsp,
                            DeviceNote::Choke {
                                key: Some(key),
                                note_id,
                                channel: channel.number(),
                            },
                        );
                        *address = None;
                    } else {
                        // Keep the address during release so a subsequent choke can still target it.
                        dispatch_note(
                            dsp,
                            DeviceNote::Off {
                                key,
                                note_id,
                                channel: channel.number(),
                            },
                        );
                    }
                }
            };
            if let Some(key) = key.number() {
                if let Some(address) = notes.get_mut(usize::from(key)) {
                    release(key, address);
                }
            } else {
                for (key, address) in notes.iter_mut().enumerate() {
                    release(key as u8, address);
                }
            }
        }
        NoteEvent::MidiCC { cc: 120 | 123, .. } => {
            // The key table follows the device: all notes end, whatever the channel.
            notes.fill(None);
            if let Some(note) = to_device_note(event) {
                dispatch_note(dsp, note);
            }
        }
        _ => {
            if let Some(note) = to_device_note(event) {
                dispatch_note(dsp, note);
            }
        }
    }
}

/// Translates a CLAP note or MIDI event to the host-independent [`DeviceNote`]; `None` for events
/// a device ignores. Wildcards stay wildcards: one definition for every bundled CLAP (#223).
pub fn to_device_note(event: NoteEvent<()>) -> Option<DeviceNote> {
    let midi_value = |value: f32| (value.clamp(0.0, 1.0) * 127.0).round() as u8;
    Some(match event {
        NoteEvent::NoteOn {
            channel,
            key,
            voice_id,
            velocity,
            ..
        } => DeviceNote::On {
            key: key.number().filter(|key| *key < 128)?,
            velocity: (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8,
            note_id: voice_id.id().map(|id| id as u32),
            channel: channel.number(),
        },
        NoteEvent::NoteOff {
            channel,
            key,
            voice_id,
            ..
        } => DeviceNote::ReleaseGates {
            key: key.number(),
            note_id: voice_id.id().map(|id| id as u32),
            channel: channel.number(),
        },
        NoteEvent::MidiNoteOff { key, channel, .. } => DeviceNote::Off {
            key,
            note_id: None,
            channel: Some(channel),
        },
        NoteEvent::Choke {
            channel: Channel::Wildcard,
            key: Key::Wildcard,
            voice_id: VoiceID::Wildcard,
            ..
        } => DeviceNote::AllNotesOff,
        NoteEvent::Choke {
            channel,
            key,
            voice_id,
            ..
        } => DeviceNote::Choke {
            key: key.number(),
            note_id: voice_id.id().map(|id| id as u32),
            channel: channel.number(),
        },
        NoteEvent::MidiPitchBend { channel, value, .. } => DeviceNote::Midi(pitch_bend_bytes(
            channel,
            (value.clamp(0.0, 1.0) * 16383.0).round() as u16,
        )),
        NoteEvent::MidiCC {
            channel, cc, value, ..
        } => DeviceNote::Midi([0xB0 | (channel & 0x0F), cc & 0x7F, midi_value(value)]),
        NoteEvent::MidiChannelPressure {
            channel, pressure, ..
        } => DeviceNote::Midi([0xD0 | (channel & 0x0F), midi_value(pressure), 0]),
        _ => return None,
    })
}

impl<T: AudioDefinition> ClapPlugin for Instrument<T> {
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

impl<T: AudioDefinition> NativeClapLayout for Instrument<T> {
    fn panel() -> Panel {
        panel_for::<T>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct NoteProbe(Vec<(u8, bool)>);

    impl Dsp for NoteProbe {
        fn activate(&mut self, _: f32) {}
        fn set_param(&mut self, _: usize, _: f64) {}
        fn process(&mut self, _: [&[f32]; 2], _: [&mut [f32]; 2]) {}
        fn reset(&mut self) {}
        fn note_off(&mut self, key: u8) {
            self.0.push((key, false));
        }
        fn choke_note(&mut self, key: Option<u8>, _: Option<u32>, _: Option<u8>) {
            self.0.push((key.unwrap(), true));
        }
        fn all_notes_off(&mut self) {
            // 255 stands for the global release.
            self.0.push((255, false));
        }
    }

    #[test]
    fn channel_mode_messages_end_all_notes_and_forget_the_keys() {
        for cc in [120, 123] {
            let mut dsp = NoteProbe::default();
            let mut held = [None; 128];
            for (channel, key) in [(2, 60), (3, 67)] {
                apply_note(
                    &mut dsp,
                    &mut held,
                    NoteEvent::NoteOn {
                        timing: 0,
                        channel: Channel::Number(channel),
                        key: Key::Number(key),
                        voice_id: VoiceID::ID(i32::from(key)),
                        velocity: 1.0,
                    },
                );
            }
            apply_note(
                &mut dsp,
                &mut held,
                NoteEvent::MidiCC {
                    timing: 5,
                    channel: 2,
                    cc,
                    value: 0.0,
                },
            );
            assert_eq!(dsp.0, [(255, false)]);
            assert!(held.iter().all(Option::is_none));
        }
    }

    #[test]
    fn wildcard_note_off_and_keyed_choke_dispatch_only_the_matched_notes() {
        let mut dsp = NoteProbe::default();
        let mut held = [None; 128];
        for (key, id) in [(60, 91), (67, 92)] {
            apply_note(
                &mut dsp,
                &mut held,
                NoteEvent::NoteOn {
                    timing: 0,
                    channel: Channel::Number(2),
                    key: Key::Number(key),
                    voice_id: VoiceID::ID(id),
                    velocity: 1.0,
                },
            );
        }
        apply_note(
            &mut dsp,
            &mut held,
            NoteEvent::NoteOff {
                timing: 4,
                channel: Channel::Number(2),
                key: Key::Wildcard,
                voice_id: VoiceID::ID(91),
                velocity: 0.0,
            },
        );
        apply_note(
            &mut dsp,
            &mut held,
            NoteEvent::Choke {
                timing: 5,
                channel: Channel::Number(2),
                key: Key::Number(67),
                voice_id: VoiceID::ID(92),
            },
        );
        assert_eq!(dsp.0, [(60, false), (67, true)]);
    }
}
