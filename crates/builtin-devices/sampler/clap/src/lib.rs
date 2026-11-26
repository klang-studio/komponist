//! The eight-pad drum sampler packaged as a native CLAP instrument.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, panel_from_parts};
use komponist_device_core::{Dsp, Spec};
use komponist_i18n::Lang;
use komponist_model::params::layout::{CellParam, DeviceLayout, SEL, WidgetKind};
use komponist_model::params::{ParamContext, ParamId, ParamInfo, resolve};
use komponist_native_clap_sdk::{GraphLabels, Pad, PadBank, Panel};
use komponist_sampler::{PADS, PadField};
use komponist_sampler_clap::{AssetEditIntent, Sampled, SampledDefinition};
use komponist_sampler_shared::{ManagedLoader, SamplerHandle};
use nice_plug::prelude::*;

struct DrumSampler;

#[derive(Clone, Copy, Enum, PartialEq)]
enum Mode {
    #[id = "one-shot"]
    #[name = "1×"]
    OneShot,
    #[id = "gate"]
    Gate,
}

impl DeviceDefinition for DrumSampler {
    const SPEC: &'static Spec = &komponist_sampler::SPEC;
    const NAME: &'static str = komponist_sampler::NAME;
    const ID: &'static str = "org.klang-studio.komponist.sampler";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Drum,
        ClapFeature::Sampler,
        ClapFeature::Stereo,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        info.id
            .as_str()
            .ends_with("_mode")
            .then(|| Binding::choice::<Mode>(info))
    }
}

impl SampledDefinition for DrumSampler {
    const PADS: usize = PADS;

    fn trim_for_asset_edit(slot: usize, intent: AssetEditIntent) -> Option<[usize; 2]> {
        (intent != AssetEditIntent::Clear)
            .then(|| [PadField::Start, PadField::End].map(|field| field.wire(slot)))
    }

    fn panel() -> Panel {
        let mut panel = panel_from_parts(
            pad_layout,
            Self::SPEC.params_in(Lang::En),
            Self::SPEC.params_in(Lang::De),
        );
        panel.device_labels = Some(komponist_bundled_clap_adapter::device_labels(
            &Self::SPEC.catalog,
        ));
        let graph_labels = panel
            .grid
            .pages
            .iter()
            .enumerate()
            .map(|(pad, page)| {
                let index = page
                    .cells
                    .iter()
                    .position(|cell| cell.widget == WidgetKind::Graph)
                    .expect("a waveform on each pad page");
                (
                    (pad, index),
                    GraphLabels {
                        name: panel.pages[pad].labels.clone(),
                        description: [Lang::En, Lang::De]
                            .into_iter()
                            .map(|lang| (lang.code().into(), lang.ui_graph_waveform().into()))
                            .collect(),
                        hide_title: false,
                        signal_taps: Default::default(),
                        asset_slot: Some(pad as u32),
                        waveform_markers: false,
                        waveform_toolbar: false,
                        waveform_slice_controls: None,
                        audition_note: None,
                        trim_unit: None,
                    },
                )
            })
            .collect();
        Panel {
            parameter_display: Default::default(),
            full_band_choice_labels: Default::default(),
            graph_labels,
            canvases: Default::default(),
            reduction_taps: Default::default(),
            headings: Default::default(),
            pad_bank: Some(PadBank {
                pads: (0..PADS)
                    .map(|pad| Pad {
                        note: komponist_sampler::note_of_pad(pad),
                        asset_slot: pad as u32,
                    })
                    .collect(),
                activity_tap: Some("pads".into()),
                follow_param: Some(ParamId::new("follow")),
            }),
            ..panel
        }
    }

    fn sampler() -> (Box<dyn Dsp>, SamplerHandle, ManagedLoader) {
        komponist_sampler::dsp_with_managed_loader()
    }
}

fn pad_layout(lang: Lang) -> DeviceLayout {
    let template = &DrumSampler::SPEC.layout_in(lang).pages[0];
    let pages = (1..=PADS)
        .map(|number| {
            let mut page = template.clone();
            let mut context = ParamContext::new();
            context.set(SEL, number);
            page.name = lang.devices_pad(number);
            for cell in &mut page.cells {
                if let Some(id) = cell.resolved(&context).expect("fixed pad binding") {
                    cell.param = CellParam::Id(id);
                }
                if let CellParam::Graph(binding) = &mut cell.param {
                    for (_, parameter) in &mut binding.bind {
                        *parameter = resolve(parameter, &context)
                            .expect("fixed pad waveform binding")
                            .as_str()
                            .to_owned();
                    }
                }
            }
            page
        })
        .collect();
    DeviceLayout { pages }
}

type DrumSamplerPlugin = Sampled<DrumSampler>;

nice_export_clap!(DrumSamplerPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[path = "../../../sampler-shared/clap_contract.rs"]
mod sampler_contract;

#[cfg(test)]
#[test]
fn all_pads_have_stable_bilingual_bindings_and_restore_sample_links() {
    layout_contract::assert_localized_bindings::<DrumSamplerPlugin>(
        DrumSampler::SPEC.params().len(),
    );
    sampler_contract::sample_asset_roundtrip::<DrumSamplerPlugin>(
        komponist_sampler::FIRST_NOTE,
        "pad1_mode",
        false,
    );
}

#[cfg(test)]
#[test]
fn authored_pad_bank_preserves_every_pad_editor() {
    let layout = layout_contract::read_layout::<DrumSamplerPlugin>();
    let pages = layout["pages"].as_array().unwrap();
    let pads = layout["pad_bank"]["pads"].as_array().unwrap();
    let author_ids = layout["author_ids"].as_object().unwrap();
    assert_eq!(pages.len(), PADS);
    assert_eq!(pads.len(), PADS);
    assert_eq!(author_ids.len(), DrumSampler::SPEC.params().len());
    assert_eq!(layout["pad_bank"]["follow_param"], author_ids["follow"]);
    assert_eq!(layout["pad_bank"]["activity_tap"], "pads");
    for (index, (page, pad)) in pages.iter().zip(pads).enumerate() {
        let number = index + 1;
        assert_eq!(page["columns"], 9);
        assert_eq!(page["dividers"], serde_json::json!([3, 7]));
        assert_eq!(pad["note"], komponist_sampler::note_of_pad(index));
        assert_eq!(pad["asset_slot"], index);
        let cells = page["cells"].as_array().unwrap();
        assert_eq!(cells[0]["widget"], "pad_grid");
        let waveform = cells.iter().find(|cell| cell["widget"] == "graph").unwrap();
        let waveform = &waveform["graph"];
        assert_eq!(waveform["asset_slot"], index);
        assert_eq!(waveform["description"]["de"], "Wellenform");
        assert_eq!(waveform["description"]["en"], "Waveform");
        for trim in ["start", "end"] {
            assert_eq!(
                waveform["slots"][trim],
                author_ids[&format!("pad{number}_{trim}")]
            );
        }
        let fader = cells
            .iter()
            .find(|cell| cell["widget"] == "mini_fader")
            .unwrap();
        assert_eq!(fader["param_id"], author_ids[&format!("pad{number}_level")]);
    }
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<DrumSampler>();
}
