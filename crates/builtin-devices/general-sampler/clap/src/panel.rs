//! The sampler's bilingual Sample and Modulation pages.

use komponist_bundled_clap_adapter::{DeviceDefinition, Parameters, panel_from_parts};
use komponist_device_core::layout::{knob, param, toggle};
use komponist_i18n::Lang;
use komponist_model::params::ParamId;
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig};
use komponist_model::params::layout::{Half, Page, WidgetKind};
use komponist_native_clap_sdk::display::{Display, Primitive, Tone};
use komponist_native_clap_sdk::{Canvas, GraphLabels, Heading, Labels, PanelV2, TrimUnit};

use super::GeneralSampler;

const COLUMNS: u8 = 14;
const WAVE_COLUMNS: u8 = 10;
const FILTER_ENVELOPE: &str = "filter-envelope";

fn layout(lang: Lang) -> DeviceLayout {
    use Half::{Bottom, Top};

    let number = |col, row, width, half, id| {
        Cell::half(col, row, width, half, WidgetKind::Number, param(id))
    };
    let dropdown =
        |col, row, half, id| Cell::half(col, row, 2, half, WidgetKind::Dropdown, param(id));
    let page = |name: &str, cells| Page {
        name: name.into(),
        cols: COLUMNS,
        cells,
        dividers: vec![WAVE_COLUMNS - 1],
    };
    DeviceLayout {
        pages: vec![
            page(
                lang.devices_sampler_sample(),
                vec![
                    Cell::graph(
                        0,
                        0,
                        WAVE_COLUMNS,
                        2,
                        GraphBinding::new(GraphConfig::Waveform)
                            .bind("start", "start")
                            .bind("end", "end"),
                    ),
                    Cell::half(10, 0, 4, Top, WidgetKind::Segment, param("mode")),
                    dropdown(10, 0, Bottom, "direction"),
                    number(12, 0, 2, Bottom, "root"),
                    toggle(10, 1, 2, Top, param("sync")),
                    number(12, 1, 2, Top, "source_bpm"),
                    knob(10, 2, "coarse"),
                    knob(11, 2, "fine"),
                    knob(12, 2, "gain"),
                    knob(13, 2, "pan"),
                    number(0, 2, 1, Top, "start"),
                    number(1, 2, 1, Top, "end"),
                    number(2, 2, 2, Top, "fade_in"),
                    number(4, 2, 2, Top, "fade_out"),
                ],
            ),
            page(
                lang.devices_sampler_modulation(),
                vec![
                    Cell::graph(
                        0,
                        0,
                        5,
                        2,
                        GraphBinding::new(GraphConfig::Envelope)
                            .bind("attack", "amp_attack")
                            .bind("decay", "amp_decay")
                            .bind("sustain", "amp_sustain")
                            .bind("release", "amp_release"),
                    ),
                    Cell::span(5, 0, 5, 2, WidgetKind::Canvas, CellParam::None),
                    knob(0, 2, "amp_attack"),
                    knob(1, 2, "amp_decay"),
                    knob(2, 2, "amp_sustain"),
                    knob(3, 2, "amp_release"),
                    knob(5, 2, "filter_env"),
                    knob(6, 2, "filter_decay"),
                    knob(7, 2, "resonance"),
                    dropdown(10, 0, Top, "filter"),
                    number(12, 0, 2, Top, "cutoff"),
                    dropdown(10, 0, Bottom, "lfo_shape"),
                    number(12, 0, 2, Bottom, "lfo_rate"),
                    dropdown(10, 1, Bottom, "lfo_target"),
                    number(12, 1, 2, Bottom, "lfo_depth"),
                    dropdown(10, 2, Bottom, "velocity_target"),
                    number(12, 2, 2, Bottom, "velocity_depth"),
                ],
            ),
        ],
    }
}

fn labels<S: Into<String>>(localize: impl Fn(Lang) -> S) -> Labels {
    Lang::ALL
        .into_iter()
        .map(|lang| (lang.code().into(), localize(lang).into()))
        .collect()
}

pub(super) fn panel() -> PanelV2 {
    let spec = GeneralSampler::SPEC;
    let mut panel = panel_from_parts(layout, spec.params_in(Lang::En), spec.params_in(Lang::De));
    panel.device_labels = Some(komponist_native_clap_sdk::PluginLabels {
        names: labels(spec.catalog.name_in),
        descriptions: Default::default(),
    });
    panel.pages[0].key = "sample".into();
    panel.pages[1].key = "modulation".into();

    let waveform = GraphLabels {
        name: labels(Lang::ui_graph_waveform),
        description: labels(|lang| {
            format!(
                "{} · {}",
                lang.ui_graph_sample_start(),
                lang.ui_graph_sample_end()
            )
        }),
        hide_title: true,
        signal_taps: [("playhead".into(), "playhead".into())].into(),
        asset_slot: Some(0),
        waveform_markers: true,
        waveform_toolbar: true,
        waveform_slice_controls: Some(["direction", "fade_in", "fade_out"].map(ParamId::new)),
        audition_note: Some(ParamId::new("root")),
        trim_unit: Some(TrimUnit::SourceFraction),
    };
    let envelope = GraphLabels {
        name: labels(Lang::devices_sampler_amp_envelope),
        description: labels(|lang| {
            [
                lang.param_general_sampler_attack(),
                lang.param_general_sampler_decay(),
                lang.param_general_sampler_sustain(),
                lang.param_general_sampler_release(),
            ]
            .join(" · ")
        }),
        hide_title: false,
        signal_taps: Default::default(),
        asset_slot: None,
        waveform_markers: false,
        waveform_toolbar: false,
        waveform_slice_controls: None,
        audition_note: None,
        trim_unit: None,
    };
    PanelV2 {
        panel,
        graph_labels: [((0, 0), waveform), ((1, 0), envelope)].into(),
        canvases: [(
            (1, 1),
            Canvas {
                key: FILTER_ENVELOPE.into(),
                name: labels(Lang::devices_sampler_filter_envelope),
                description: labels(|lang| {
                    format!(
                        "{} · {}",
                        lang.param_general_sampler_filter_env(),
                        lang.param_general_sampler_filter_decay()
                    )
                }),
                show_title: true,
                horizontal_viewport: false,
            },
        )]
        .into(),
        reduction_taps: Default::default(),
        pad_bank: None,
        headings: [(
            1,
            vec![
                Heading {
                    col: 10,
                    row: 1,
                    cols: 4,
                    labels: labels(Lang::devices_sampler_lfo),
                },
                Heading {
                    col: 10,
                    row: 2,
                    cols: 4,
                    labels: labels(Lang::devices_sampler_velocity),
                },
            ],
        )]
        .into(),
    }
}

pub(super) fn paint(params: &Parameters, display: &mut Display) {
    let depth = params.plain_value_by_id("filter_env").unwrap_or(0.0);
    let decay = params.plain_value_by_id("filter_decay").unwrap_or(200.0) / 1000.0;
    let duration = 1.0_f64.max(decay * 1.5);
    let limit = (depth.abs() * 1.25).max(0.5);

    display.canvas(FILTER_ENVELOPE, |painter| {
        for i in 1..5 {
            let x = i as f32 / 5.0;
            painter.line([x, 0.0], [x, 1.0], Tone::Grid);
        }
        for i in 1..4 {
            let y = i as f32 / 4.0;
            painter.line([0.0, y], [1.0, y], Tone::Grid);
        }
        let points: Vec<_> = (0..=128)
            .map(|i| {
                let x = f64::from(i) / 128.0;
                let value = depth * (-x * duration / decay.max(0.001)).exp();
                [x as f32, (0.5 - 0.5 * value / limit) as f32]
            })
            .collect();
        painter.push(Primitive::Area {
            points: points.clone(),
            baseline: 0.5,
            tone: Tone::Accent,
        });
        painter.polyline(points, Tone::Accent);
    });
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use clap_sys::plugin::clap_plugin;
    use komponist_ipc::device_canvas::{DisplayList, EXTENSION_ID, MAX_BYTES};
    use serde_json::json;

    use super::*;
    use crate::GeneralSamplerPlugin;
    use crate::layout_contract::{self, Instance};
    use crate::sampler_contract::{load_state, save_state};

    #[test]
    fn native_layout_keeps_both_pages_and_the_complete_parameter_inventory() {
        let layout = layout_contract::read_layout_extension::<GeneralSamplerPlugin>(
            komponist_native_clap_sdk::EXTENSION_ID_V2,
        );
        let ids = &layout["author_ids"];
        assert_eq!(
            ids,
            &layout_contract::read_layout::<GeneralSamplerPlugin>()["author_ids"]
        );
        assert_eq!(
            ids.as_object().unwrap().len(),
            GeneralSampler::SPEC.params().len()
        );
        assert_eq!(layout["schema_version"], 2);
        let pages = layout["pages"].as_array().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["key"], "sample");
        assert_eq!(pages[1]["key"], "modulation");
        for page in pages {
            assert_eq!(page["columns"], COLUMNS);
            assert_eq!(page["dividers"], json!([WAVE_COLUMNS - 1]));
        }
        let waveform = &pages[0]["cells"][0]["graph"];
        assert_eq!(waveform["slots"]["start"], ids["start"]);
        assert_eq!(waveform["slots"]["end"], ids["end"]);
        assert_eq!(waveform["trim_unit"], "source_fraction");
        assert_eq!(waveform["asset_slot"], 0);
        assert_eq!(waveform["signal_taps"]["playhead"], "playhead");
        assert_eq!(waveform["waveform_markers"], true);
        assert_eq!(waveform["waveform_toolbar"], true);
        assert_eq!(waveform["audition_note"], ids["root"]);
        let envelope = &pages[1]["cells"][0]["graph"];
        for slot in ["attack", "decay", "sustain", "release"] {
            assert_eq!(envelope["slots"][slot], ids[format!("amp_{slot}")]);
        }
        let canvas = &pages[1]["cells"][1]["canvas"];
        assert_eq!(canvas["key"], FILTER_ENVELOPE);
        assert_eq!(canvas["show_title"], true);
        for lang in Lang::ALL {
            let code = lang.code();
            assert_eq!(pages[0]["labels"][code], lang.devices_sampler_sample());
            assert_eq!(pages[1]["labels"][code], lang.devices_sampler_modulation());
            assert_eq!(waveform["name"][code], lang.ui_graph_waveform());
            assert_eq!(envelope["name"][code], lang.devices_sampler_amp_envelope());
            assert_eq!(canvas["name"][code], lang.devices_sampler_filter_envelope());
        }
    }

    #[repr(C)]
    struct DisplayExtension {
        draw: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32) -> u32,
    }

    fn display(instance: &Instance) -> DisplayList {
        let plugin = instance.0;
        let mut bytes = vec![0; MAX_BYTES];
        // SAFETY: This test owns the initialized CLAP instance and its bounded draw buffer.
        let len = unsafe {
            let table: *const c_void =
                ((*plugin).get_extension.unwrap())(plugin, EXTENSION_ID.as_ptr());
            let table = table.cast::<DisplayExtension>().as_ref().unwrap();
            (table.draw)(plugin, bytes.as_mut_ptr(), MAX_BYTES as u32)
        };
        DisplayList::decode(&bytes[..len as usize]).unwrap()
    }

    #[test]
    fn filter_envelope_canvas_reads_live_parameter_values() {
        let instance = Instance::new::<GeneralSamplerPlugin>();
        let mut state = save_state(&instance);
        let before = display(&instance);
        // The CLAP state stores continuous controls in their normalized ranges.
        state["params"]["filter_env"] = json!({"f32": 0.25});
        state["params"]["filter_decay"] = json!({"f32": 0.0175});
        assert!(load_state(&instance, &state));
        let after = display(&instance);
        assert_ne!(after, before);
        let frame = &after.frames[0];
        assert_eq!(frame.key, FILTER_ENVELOPE);
        let Some(Primitive::Polyline {
            points,
            tone: Tone::Accent,
        }) = frame.primitives.last()
        else {
            panic!("missing filter envelope curve");
        };
        assert!(points.first().unwrap()[1] > 0.5);
        assert!((points.last().unwrap()[1] - 0.5).abs() < 0.01);
    }
}
