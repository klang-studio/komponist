//! The bundle-owned operator, envelope, and global pages.

use std::collections::BTreeMap;

use komponist_bundled_clap_adapter::{device_labels, panel_from_parts};
use komponist_hexa::{CATALOG, device};
use komponist_i18n::Lang;
use komponist_model::params::layout::{CellParam, ENV_OP, WidgetKind};
use komponist_native_clap_sdk::{
    Canvas, FmCarrier, FmDiagram, GraphLabels, Labels, Panel, ParamId,
};

use super::infos;
use super::state::FM_ROUTING_KEY;

fn labels(en: &str, de: &str) -> Labels {
    [("en".into(), en.into()), ("de".into(), de.into())].into()
}

pub(super) fn panel() -> Panel {
    let mut panel = panel_from_parts(device::clap_editor_layout, infos(Lang::En), infos(Lang::De));
    panel.device_labels = Some(device_labels(&CATALOG));
    panel.parameter_labels.insert(
        ParamId::new(ENV_OP),
        labels(Lang::En.devices_operator(), Lang::De.devices_operator()),
    );
    let mut graph_labels = BTreeMap::new();
    let mut canvases = BTreeMap::new();
    for (page, grid) in panel.grid.pages.iter_mut().enumerate() {
        let carriers: Vec<_> = grid
            .cells
            .iter_mut()
            .filter_map(|cell| {
                cell.carrier.take()?;
                Some(FmCarrier {
                    col: cell.col,
                    row: cell.row,
                    operator: cell.col,
                })
            })
            .collect();
        for (index, cell) in grid.cells.iter_mut().enumerate() {
            match cell.widget {
                WidgetKind::FmAlgorithm => {
                    cell.widget = WidgetKind::Canvas;
                    cell.param = CellParam::None;
                    canvases.insert(
                        (page, index),
                        Canvas {
                            key: FM_ROUTING_KEY.into(),
                            name: labels(Lang::En.devices_fm_free(), Lang::De.devices_fm_free()),
                            description: labels(
                                "FM routing. Select an operator to show its envelope.",
                                "FM-Routing. Wähle einen Operator, um seine Hüllkurve anzuzeigen.",
                            ),
                            show_title: false,
                            horizontal_viewport: false,
                            fm_topology: Some(FmDiagram {
                                selector: ENV_OP.into(),
                                carriers: carriers.clone(),
                            }),
                        },
                    );
                }
                WidgetKind::Graph => {
                    graph_labels.insert(
                        (page, index),
                        GraphLabels {
                            name: labels(
                                &Lang::En.graph_title_hexa_amp("{selection}"),
                                &Lang::De.graph_title_hexa_amp("{selection}"),
                            ),
                            description: labels(
                                "Amplitude envelope of the selected operator.",
                                "Amplitudenhüllkurve des gewählten Operators.",
                            ),
                            hide_title: false,
                            signal_taps: Default::default(),
                            asset_slot: None,
                            waveform_markers: false,
                            waveform_toolbar: false,
                            waveform_slice_controls: None,
                            audition_note: None,
                            trim_unit: None,
                        },
                    );
                }
                _ => {}
            }
        }
    }
    Panel {
        graph_labels,
        canvases,
        reduction_taps: Default::default(),
        headings: Default::default(),
        pad_bank: None,
        full_band_choice_labels: Default::default(),
        ..panel
    }
}
