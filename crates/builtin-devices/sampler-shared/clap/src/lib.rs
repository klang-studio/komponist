//! Sample-based instruments with the device loader retained across restore and playback.

// Missing SAFETY comments: 1. Remove this allow when the crate is clean (#241 follow-up).
#![allow(clippy::undocumented_unsafe_blocks)]

mod assets;

use std::collections::BTreeMap;
use std::ffi::{CStr, c_void};
use std::sync::{Arc, Mutex};

use clap_sys::plugin::clap_plugin;
use komponist_bundled_clap_adapter::{DeviceDefinition, Parameters, panel_for, to_device_note};
use komponist_device_core::pinned_clap_state::{MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD};
use komponist_device_core::{Dsp, SAMPLE_RATES, dispatch_note};
use komponist_ipc::sampler_ui::{self, EXTENSION_ID, Pad, Snapshot, Status, Sync as WireSync};
use komponist_native_clap_sdk::{NativeClapLayout, Panel};
use komponist_sampler_shared::{ManagedLoader, PadStatus, SamplerHandle, SyncStatus, links};
use nice_plug::prelude::*;

const STEREO: NonZeroU32 = NonZeroU32::new(2).unwrap();

/// A sampler which supplies its existing DSP and optional extra controls over loaded assets.
pub trait SampledDefinition: DeviceDefinition {
    const PADS: usize;

    fn sampler() -> (Box<dyn Dsp>, SamplerHandle, ManagedLoader);

    /// Restore optional device-specific assets, such as the general sampler's slice map.
    fn restore_settings(_handle: &SamplerHandle, _assets: &[u8]) {}
}

struct SampleParameters<T> {
    params: Parameters,
    samples: SamplerHandle,
    assets: Mutex<String>,
    host_bindings: Mutex<BTreeMap<usize, Option<links::SampleLink>>>,
    offline: Mutex<Option<komponist_ipc::device_assets::OfflineRequest>>,
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
        let canonical_links = |assets: &str| {
            let mut links = links::read(Some(assets.as_bytes()));
            links.dedup_by_key(|(pad, _)| *pad);
            links
        };
        // The JSON reader accepts numeric aliases (for example "01"). Resolve duplicates
        // before walking the pads so a second entry for one pad cannot hide later pads.
        let old = canonical_links(&saved);
        let new = canonical_links(assets);
        let old_revisions = links::revisions(&saved);
        let new_revisions = links::revisions(assets);
        let bindings = self
            .host_bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for pad in 0..T::PADS {
            if bindings.contains_key(&pad) {
                continue;
            }
            let link_at = |links: &[(usize, links::SampleLink)]| {
                links.iter().position(|(index, _)| *index == pad)
            };
            let previous = link_at(&old).map(|i| &old[i].1);
            let link = link_at(&new).map(|i| &new[i].1);
            if previous == link && old_revisions[pad] == new_revisions[pad] {
                continue;
            }
            if let Some(link) = link {
                // The host has already resolved its relative links in the state copy sent to
                // the plugin. Standalone CLAP hosts can use the absolute fallback as well.
                self.samples.load_link(pad, link.clone(), None);
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
            applied: params.unapplied_values(),
            params: Arc::new(SampleParameters {
                params,
                samples,
                assets: Mutex::new("{}".into()),
                host_bindings: Mutex::new(BTreeMap::new()),
                offline: Mutex::new(None),
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
        if let Some(note) = to_device_note(event) {
            dispatch_note(self.dsp.as_mut(), note);
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

#[repr(C)]
struct SamplerUi {
    snapshot: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, f64) -> u32,
    preview: unsafe extern "C" fn(*const clap_plugin, f64),
}

impl<T: SampledDefinition> Sampled<T> {
    const SAMPLER_UI: SamplerUi = SamplerUi {
        snapshot: Self::read_sampler_ui,
        preview: Self::preview_slice,
    };

    unsafe extern "C" fn read_sampler_ui(
        plugin: *const clap_plugin,
        bytes: *mut u8,
        capacity: u32,
        export_tempo: f64,
    ) -> u32 {
        if bytes.is_null() || capacity == 0 {
            return 0;
        }
        // SAFETY: The caller supplies a writable buffer and the live CLAP instance.
        let out = unsafe {
            std::slice::from_raw_parts_mut(bytes, (capacity as usize).min(sampler_ui::MAX_BYTES))
        };
        unsafe {
            nice_plug::wrapper::clap::Wrapper::<Self>::with_custom_extension_data::<
                SampleParameters<T>,
                _,
            >(plugin, |params| {
                let handle = &params.samples;
                if export_tempo.is_finite() && export_tempo > 0.0 {
                    let plain = |id| params.params.plain_value_by_id(id);
                    if let Some((source_bpm, sync)) = plain("source_bpm").zip(plain("sync")) {
                        handle.prepare_general_sync(source_bpm, export_tempo, sync >= 0.5);
                    }
                }
                let pads = handle
                    .views()
                    .into_iter()
                    .take(T::PADS)
                    .enumerate()
                    .map(|(pad, mut view)| {
                        view.name
                            .truncate(view.name.floor_char_boundary(sampler_ui::MAX_TEXT_BYTES));
                        Pad {
                            name: view.name,
                            status: match view.status {
                                PadStatus::Empty => Status::Empty,
                                PadStatus::Loading => Status::Loading,
                                PadStatus::Ready => Status::Ready,
                                PadStatus::Missing => Status::Missing,
                                PadStatus::Failed(mut error) => {
                                    error.truncate(
                                        error.floor_char_boundary(sampler_ui::MAX_TEXT_BYTES),
                                    );
                                    Status::Failed(error)
                                }
                            },
                            length: view.length_s,
                            peaks: view
                                .peaks
                                .iter()
                                .take(sampler_ui::MAX_PEAKS)
                                .map(|peak| peak.clamp(0.0, 1.0))
                                .collect(),
                            playhead: handle.playhead(pad).map(|head| head.clamp(0.0, 1.0)),
                        }
                    })
                    .collect();
                let (count, hit_pad) = handle.last_hit();
                Snapshot {
                    pads,
                    last_hit: (count, hit_pad as u8),
                    transients: handle
                        .transients()
                        .into_iter()
                        .take(sampler_ui::MAX_TRANSIENTS)
                        .collect(),
                    sync: handle.sync_status().map(|status| match status {
                        SyncStatus::Ready => WireSync::Ready,
                        SyncStatus::Preparing => WireSync::Preparing,
                        SyncStatus::Unavailable => WireSync::Unavailable,
                    }),
                    source_rate: handle.waveform_peaks().map(|data| data.sample_rate()),
                    idle: handle.is_idle(),
                }
                .encode(out)
                .map_or(0, |len| len as u32)
            })
            .unwrap_or(0)
        }
    }

    unsafe extern "C" fn preview_slice(plugin: *const clap_plugin, time: f64) {
        // SAFETY: CLAP host invokes the callback on a live plugin on its main thread.
        unsafe {
            nice_plug::wrapper::clap::Wrapper::<Self>::with_custom_extension_data::<
                SampleParameters<T>,
                _,
            >(plugin, |params| {
                let handle = &params.samples;
                if time.is_finite() {
                    if time >= 0.0 {
                        handle.preview_slice(time);
                    } else {
                        handle.release_slice_preview();
                    }
                }
            });
        }
    }
}

impl<T: SampledDefinition> ClapPlugin for Sampled<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DISTINGUISH_MIDI_NOTE_OFF: bool = true;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    fn clap_custom_extension_data(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.params.clone())
    }

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        if id == EXTENSION_ID {
            std::ptr::from_ref(&Self::SAMPLER_UI).cast()
        } else if id == komponist_ipc::device_assets::EXTENSION_ID {
            komponist_native_clap_sdk::assets::extension::<Self, SampleParameters<T>>(id)
        } else {
            komponist_native_clap_sdk::extension::<Self>(id)
        }
    }
}

impl<T: SampledDefinition> NativeClapLayout for Sampled<T> {
    fn panel() -> Panel {
        panel_for::<T>()
    }
}
