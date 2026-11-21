//! Spec instrument adapter using the same DSP and ordered event slices as the engine.

use std::ffi::{CStr, c_void};
use std::marker::PhantomData;
use std::sync::Arc;

use komponist_device_core::{Dsp, SAMPLE_RATES};
use komponist_native_clap_sdk::{NativeClapLayout, Panel};
use nice_plug::prelude::*;

use crate::{DeviceDefinition, STEREO, panel_for, parameter};

/// A stereo instrument backed by an existing spec DSP.
pub struct Instrument<T> {
    params: Arc<parameter::Parameters>,
    dsp: Box<dyn Dsp>,
    applied: Vec<Option<f64>>,
    silence: Vec<f32>,
    definition: PhantomData<T>,
}

impl<T: DeviceDefinition> Default for Instrument<T> {
    fn default() -> Self {
        let params = parameter::Parameters::new(T::SPEC, T::choice);
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(params),
            dsp: (T::SPEC.dsp)(),
            silence: Vec::new(),
            definition: PhantomData,
        }
    }
}

impl<T: DeviceDefinition> Instrument<T> {
    fn apply_params(&mut self) {
        for (index, applied) in self.applied.iter_mut().enumerate() {
            if let Some(value) = self.params.changed_value(index, applied) {
                self.dsp.set_param(index, value);
            }
        }
    }
}

impl<T: DeviceDefinition> Plugin for Instrument<T> {
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
            apply_note(&mut *self.dsp, event);
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

fn apply_note(dsp: &mut dyn Dsp, event: NoteEvent<()>) {
    match event {
        NoteEvent::NoteOn {
            key,
            voice_id,
            velocity,
            ..
        } => {
            if let Some(key) = key.number() {
                let velocity = (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
                dsp.note_on_with_id(key, velocity, voice_id.id().map(|id| id as u32));
            }
        }
        NoteEvent::NoteOff { key, voice_id, .. } | NoteEvent::Choke { key, voice_id, .. } => {
            if let Some(key) = key.number() {
                dsp.note_off_with_id(key, voice_id.id().map(|id| id as u32));
            } else {
                dsp.all_notes_off();
            }
        }
        NoteEvent::MidiCC { cc: 1, value, .. } => dsp.mod_wheel(value),
        NoteEvent::MidiCC { cc: 120 | 123, .. } => dsp.all_notes_off(),
        NoteEvent::MidiChannelPressure { pressure, .. } => dsp.pressure(pressure),
        NoteEvent::MidiPitchBend { value, .. } => {
            let bend = (value * 16383.0).round();
            dsp.pitch_bend((bend - 8192.0) / 8192.0);
        }
        _ => {}
    }
}

impl<T: DeviceDefinition> ClapPlugin for Instrument<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl<T: DeviceDefinition> NativeClapLayout for Instrument<T> {
    fn panel() -> Panel {
        panel_for::<T>()
    }
}
