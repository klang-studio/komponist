//! Sample-based instruments with the device loader retained across restore and playback.

use std::collections::BTreeMap;
use std::ffi::{CStr, c_void};
use std::sync::{Arc, Mutex};

use komponist_device_core::pinned_clap_state::{MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD};
use komponist_device_core::{Dsp, SAMPLE_RATES};
use komponist_native_clap_sdk::{NativeClapLayout, Panel};
use komponist_sampler::{ManagedLoader, SamplerHandle, links};
use nice_plug::prelude::*;

use super::{Effect, EffectDefinition, STEREO};
use crate::parameter::Parameters;

/// A sampler which supplies its existing DSP and optional extra controls over loaded assets.
pub trait SampledDefinition: EffectDefinition {
    const PADS: usize;

    fn sampler() -> (Box<dyn Dsp>, SamplerHandle, ManagedLoader);

    /// Restore optional device-specific assets, such as the general sampler's slice map.
    fn restore_settings(_handle: &SamplerHandle, _assets: &[u8]) {}
}

struct SampleParameters<T> {
    params: Parameters,
    samples: SamplerHandle,
    assets: Mutex<String>,
    definition: std::marker::PhantomData<fn() -> T>,
}

// SAFETY: The parameter pointers are owned by self.params for the lifetime of the Params Arc.
unsafe impl<T: SampledDefinition> Params for SampleParameters<T> {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        self.params.param_map()
    }

    fn serialize_fields(&self) -> BTreeMap<String, String> {
        let assets = self.assets.lock().unwrap_or_else(|err| err.into_inner());
        let mut fields = self.params.serialize_fields();
        fields.insert(SAMPLE_ASSETS_FIELD.into(), assets.clone());
        fields
    }

    fn deserialize_fields(&self, fields: &BTreeMap<String, String>) {
        let assets = fields.get(SAMPLE_ASSETS_FIELD).map_or("{}", String::as_str);
        self.params.deserialize_fields(fields);
        let mut saved = self.assets.lock().unwrap_or_else(|err| err.into_inner());
        let mut links = links::read(Some(assets.as_bytes()));
        // The legacy JSON reader accepts numeric aliases such as "01". Resolve duplicates
        // before walking the pads so a second entry for one pad cannot hide later pads.
        links.dedup_by_key(|(pad, _)| *pad);
        let mut links = links.into_iter().peekable();
        for pad in 0..T::PADS {
            if links.peek().is_some_and(|item| item.0 == pad) {
                let (_, link) = links.next().expect("checked pad");
                // The CLAP state carries both paths. A generic CLAP host supplies no session
                // directory with this state callback; the loader uses the absolute fallback.
                self.samples.load_link(pad, link, None);
            } else {
                self.samples.clear(pad);
            }
        }
        T::restore_settings(&self.samples, assets.as_bytes());
        saved.clear();
        saved.push_str(assets);
    }
}

/// A standard CLAP instrument using the existing worker-backed sampler DSP.
pub struct Sampled<T> {
    params: Arc<SampleParameters<T>>,
    dsp: Box<dyn Dsp>,
    applied: Vec<Option<f64>>,
    silence: Vec<f32>,
    _loader: ManagedLoader,
}

impl<T: SampledDefinition> Default for Sampled<T> {
    fn default() -> Self {
        let (dsp, samples, loader) = T::sampler();
        let params = Parameters::new(T::SPEC, T::choice);
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(SampleParameters {
                params,
                samples,
                assets: Mutex::new("{}".into()),
                definition: std::marker::PhantomData,
            }),
            dsp,
            silence: Vec::new(),
            _loader: loader,
        }
    }
}

impl<T: SampledDefinition> Sampled<T> {
    fn apply_params(&mut self) {
        for (index, applied) in self.applied.iter_mut().enumerate() {
            if let Some(value) = self.params.params.changed_value(index, applied) {
                self.dsp.set_param(index, value);
            }
        }
    }

    fn note(&mut self, event: NoteEvent<()>) {
        match event {
            NoteEvent::NoteOn {
                key,
                velocity,
                voice_id,
                ..
            } => {
                if let Some(key) = key.number() {
                    self.dsp.note_on_with_id(
                        key,
                        (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8,
                        voice_id.id().map(|id| id as u32),
                    );
                }
            }
            NoteEvent::NoteOff { key, voice_id, .. } => {
                if let Some(key) = key.number() {
                    self.dsp
                        .note_off_with_id(key, voice_id.id().map(|id| id as u32));
                } else {
                    self.dsp
                        .release_note_gates(voice_id.id().map(|id| id as u32));
                }
            }
            NoteEvent::Choke { key, voice_id, .. } => {
                self.dsp
                    .choke_note(key.number(), voice_id.id().map(|id| id as u32));
            }
            NoteEvent::MidiPitchBend { value, .. } => self.dsp.pitch_bend(value * 2.0 - 1.0),
            NoteEvent::MidiCC { cc: 1, value, .. } => self.dsp.mod_wheel(value),
            NoteEvent::MidiCC { cc: 120 | 123, .. } => self.dsp.all_notes_off(),
            NoteEvent::MidiChannelPressure { pressure, .. } => self.dsp.pressure(pressure),
            _ => {}
        }
    }

    fn render(&mut self, output_l: &mut [f32], output_r: &mut [f32]) {
        let silence = &self.silence[..output_l.len()];
        self.dsp.process([silence, silence], [output_l, output_r]);
    }
}

impl<T: SampledDefinition> Plugin for Sampled<T> {
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

    fn validate_state(state: &PluginState) -> bool {
        state.fields.get(SAMPLE_ASSETS_FIELD).is_none_or(|assets| {
            assets.len() <= MAX_SAMPLE_ASSET_BYTES
                && serde_json::from_str::<serde_json::Value>(assets).is_ok_and(|value| {
                    value.is_object()
                        && ["samples", "sample_edit"]
                            .iter()
                            .all(|key| value.get(key).is_none_or(serde_json::Value::is_object))
                })
        })
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
        self.silence.resize(config.max_buffer_size as usize, 0.0);
        self.apply_params();
        self.dsp.activate(config.sample_rate);
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
            return ProcessStatus::Error("expected stereo outputs");
        };
        let frames = left.len();
        let mut rendered = 0;
        while let Some(event) = context.next_event() {
            let at = (event.timing() as usize).clamp(rendered, frames);
            if at > rendered {
                self.render(&mut left[rendered..at], &mut right[rendered..at]);
                rendered = at;
            }
            self.note(event);
        }
        if rendered < frames {
            self.render(&mut left[rendered..], &mut right[rendered..]);
        }
        ProcessStatus::KeepAlive
    }
}

impl<T: SampledDefinition> ClapPlugin for Sampled<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl<T: SampledDefinition> NativeClapLayout for Sampled<T> {
    fn panel() -> Panel {
        Effect::<T>::panel()
    }
}
