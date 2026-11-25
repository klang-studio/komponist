//! Hexa CLAP instrument with native SDK bindings and complete preset patch state.

mod panel;
mod state;

use std::ffi::{CStr, c_void};
use std::sync::{Arc, OnceLock};

use komponist_bundled_clap_adapter::presets::{
    FactoryPreset, FactoryPresets, embedded_load_extension,
};
use komponist_bundled_clap_adapter::{Parameters, panel_from_parts};
use komponist_engine::{Event, EventKind, NOTE_ID_NONE, Processor};
use komponist_hexa::device::{self, Field, WIRE_COUNT};
use komponist_hexa::{HexaProcessor, default_patch};
use komponist_i18n::Lang;
use komponist_model::params::layout::{Cell, DeviceLayout, Half, Page, WidgetKind};
use komponist_model::params::{ParamInfo, ParamKind};
use komponist_native_clap_sdk::{NativeClapLayout, Panel, PanelV2};
use nice_plug::prelude::*;

use crate::state::{HexaParams, Snapshot};

const STEREO: NonZeroU32 = NonZeroU32::new(2).unwrap();
const EMPTY_EVENT: Event = Event {
    offset: 0,
    kind: EventKind::AllNotesOff,
};

/// Convert a project-era patch or a factory preset to the current CLAP state stream.

fn infos(lang: Lang) -> &'static [ParamInfo] {
    static INFOS: [OnceLock<Vec<ParamInfo>>; Lang::ALL.len()] =
        [const { OnceLock::new() }; Lang::ALL.len()];
    INFOS[lang as usize].get_or_init(|| {
        let patch = default_patch();
        (0..WIRE_COUNT)
            .map(|wire| {
                let field = Field::from_wire(wire as u32).expect("valid Hexa wire ID");
                let mut info = field.info_in(lang);
                info.default = info.snap(field.get(&patch));
                info
            })
            .collect()
    })
}

struct HexaPlugin {
    params: Arc<HexaParams>,
    dsp: HexaProcessor,
    applied: [Option<f64>; WIRE_COUNT],
}

impl Default for HexaPlugin {
    fn default() -> Self {
        let params = HexaParams::new(Parameters::from_infos(infos(Lang::En), |_| None));
        let snapshot = params.snapshot();
        Self {
            params: Arc::new(params),
            dsp: HexaProcessor::new(snapshot.patch),
            applied: snapshot.applied.map(Some),
        }
    }
}

impl HexaPlugin {
    fn apply_parameters(&mut self) {
        if let Some(restored) = self.params.to_audio.pop() {
            self.dsp.replace_patch(restored.patch);
            self.applied = restored.applied.map(Some);
        }

        let mut updates = [EMPTY_EVENT; WIRE_COUNT];
        let mut count = 0;
        for (wire, value) in self.applied.iter_mut().enumerate() {
            if let Some(value) = self.params.bindings.changed_value(wire, value) {
                updates[count].kind = EventKind::Param {
                    id: wire as u32,
                    value,
                };
                count += 1;
            }
        }
        if count != 0 {
            self.dsp
                .render_events(&updates[..count], [&mut [], &mut []]);
            self.params.from_audio.force_push(Snapshot {
                patch: *self.dsp.patch(),
                applied: self
                    .applied
                    .map(|value| value.expect("initialized Hexa binding")),
            });
        }
    }
}

impl Plugin for HexaPlugin {
    const NAME: &'static str = "Hexa";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
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
        self.apply_parameters();
        if self
            .dsp
            .activate(config.sample_rate, config.max_buffer_size as usize)
            .is_err()
        {
            return false;
        }
        context.set_latency_samples(self.dsp.latency());
        true
    }

    fn reset(&mut self) {
        self.apply_parameters();
        self.dsp.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _auxiliary: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        self.apply_parameters();
        let [left, right] = buffer.as_slice() else {
            return ProcessStatus::Error("expected stereo main output");
        };
        let mut done = 0;
        while let Some(event) = context.next_event() {
            let at = (event.timing() as usize).clamp(done, left.len());
            if let Some(kind) = engine_event(event) {
                if at > done {
                    self.dsp
                        .render_events(&[], [&mut left[done..at], &mut right[done..at]]);
                    done = at;
                }
                self.dsp
                    .render_events(&[Event { offset: 0, kind }], [&mut [], &mut []]);
            }
        }
        self.dsp
            .render_events(&[], [&mut left[done..], &mut right[done..]]);
        ProcessStatus::KeepAlive
    }
}

fn engine_event(event: NoteEvent<()>) -> Option<EventKind> {
    Some(match event {
        NoteEvent::NoteOn {
            key,
            velocity,
            channel,
            voice_id,
            ..
        } => EventKind::NoteOn {
            key: key.number()?,
            velocity: (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8,
            channel: channel.number().unwrap_or(0),
            note_id: voice_id.id().map_or(NOTE_ID_NONE, |id| id as u32),
        },
        NoteEvent::NoteOff {
            key,
            channel,
            voice_id,
            ..
        }
        | NoteEvent::Choke {
            key,
            channel,
            voice_id,
            ..
        } => {
            if let Some(key) = key.number() {
                EventKind::NoteOff {
                    key,
                    channel: channel.number().unwrap_or(0),
                    note_id: voice_id.id().map_or(NOTE_ID_NONE, |id| id as u32),
                }
            } else {
                EventKind::AllNotesOff
            }
        }
        NoteEvent::MidiCC {
            channel, cc, value, ..
        } => EventKind::Midi([0xb0 | channel, cc, (value * 127.0).round() as u8]),
        NoteEvent::MidiPitchBend { channel, value, .. } => {
            let value = (value * 16383.0).round() as u16;
            EventKind::Midi([0xe0 | channel, (value & 127) as u8, (value >> 7) as u8])
        }
        NoteEvent::MidiChannelPressure {
            channel, pressure, ..
        } => EventKind::Midi([0xd0 | channel, (pressure * 127.0).round() as u8, 0]),
        _ => return None,
    })
}

fn layout(lang: Lang) -> DeviceLayout {
    let page = |label: String, params: &[ParamInfo]| {
        let cells = params
            .iter()
            .enumerate()
            .map(|(index, param)| {
                let col = (index % 12) as u8;
                let row = (index / 12) as u8;
                match param.kind {
                    ParamKind::Bool => Cell::half(
                        col,
                        row,
                        1,
                        Half::Top,
                        WidgetKind::ToggleButton,
                        komponist_model::params::layout::CellParam::Id(param.id.clone()),
                    ),
                    _ => Cell::knob(col, row, param.id.clone()),
                }
            })
            .collect();
        Page {
            name: label,
            cols: 12,
            cells,
            dividers: Vec::new(),
        }
    };
    let params = infos(lang);
    let globals = params[..device::FIRST_WIRE as usize]
        .iter()
        .chain(&params[device::GLOBAL_BASE as usize..])
        .cloned()
        .collect::<Vec<_>>();
    let mut pages = vec![page(lang.param_hexa_global_page().into(), &globals)];
    for (pair, fields) in params[device::FIRST_WIRE as usize..device::GLOBAL_BASE as usize]
        .chunks(2 * device::OpField::COUNT)
        .enumerate()
    {
        let first = 2 * pair + 1;
        pages.push(page(
            lang.param_hexa_operator_page(first, first + 1),
            fields,
        ));
    }
    DeviceLayout { pages }
}

impl ClapPlugin for HexaPlugin {
    const CLAP_ID: &'static str = "org.klang-studio.komponist.hexa";
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];

    fn clap_custom_extension_data(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.params.clone())
    }

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        let preset = embedded_load_extension::<Self>(id);
        if !preset.is_null() {
            return preset;
        }
        let display = komponist_native_clap_sdk::display::extension::<Self, HexaParams>(id);
        if !display.is_null() {
            return display;
        }
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl NativeClapLayout for HexaPlugin {
    fn panel() -> Panel {
        let mut panel = panel_from_parts(layout, infos(Lang::En), infos(Lang::De));
        panel.device_labels = Some(komponist_bundled_clap_adapter::device_labels(
            &komponist_hexa::CATALOG,
        ));
        panel
    }

    fn panel_v2() -> Option<PanelV2> {
        Some(panel::panel())
    }
}

impl FactoryPresets for HexaPlugin {
    fn metadata() -> impl Iterator<Item = FactoryPreset> {
        komponist_hexa::presets().map(|preset| FactoryPreset {
            name: preset.name,
            key: preset.index.to_string().into(),
            category: Some(preset.category),
        })
    }

    fn state(key: &str) -> Option<Vec<u8>> {
        let index = key.parse::<usize>().ok()?;
        let patch = komponist_hexa::preset_patch(index)?;
        Some(komponist_hexa::state::encode_clap_patch(
            patch,
            Self::VERSION,
        ))
    }
}

komponist_bundled_clap_adapter::export_with_presets!(HexaPlugin, HexaPlugin);

#[cfg(test)]
mod tests;
