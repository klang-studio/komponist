use komponist_native_clap_sdk::{
    Canvas, Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, GraphLabels, Labels,
    NativeClapLayout, Page, PageLabels, Panel, PanelV2, ParamId, PluginLabels, WidgetKind,
    display::{Display, DisplaySource, Tone},
    layout::knob,
};

use super::{GainParams, NativeGain};

fn labels(en: &str, de: &str) -> Labels {
    [("en".into(), en.into()), ("de".into(), de.into())].into()
}

impl NativeClapLayout for NativeGain {
    fn panel() -> Panel {
        Panel {
            default_locale: "en".into(),
            locales: vec!["en".into(), "de".into()],
            grid: DeviceLayout::single(Page {
                name: "Main".into(),
                cols: 4,
                cells: vec![knob(0, 0, "gain"), knob(1, 0, "mix")],
                dividers: Vec::new(),
            }),
            pages: vec![PageLabels {
                key: "main".into(),
                labels: [
                    ("en".into(), "Main".into()),
                    ("de".into(), "Hauptseite".into()),
                ]
                .into(),
            }],
            parameter_labels: [
                (
                    ParamId::new("gain"),
                    [("en".into(), "Gain".into()), ("de".into(), "Pegel".into())].into(),
                ),
                (
                    ParamId::new("mix"),
                    [
                        ("en".into(), "Mix".into()),
                        ("de".into(), "Mischung".into()),
                    ]
                    .into(),
                ),
            ]
            .into(),
            choice_labels: Default::default(),
            device_labels: Some(PluginLabels {
                names: labels("Native Gain Example", "Pegelbeispiel"),
                descriptions: labels("Stereo gain and mix.", "Stereo-Pegel und Mischung."),
            }),
        }
    }

    fn panel_v2() -> Option<PanelV2> {
        let mut panel = Self::panel();
        panel.grid.pages.push(Page {
            name: "Display".into(),
            cols: 8,
            cells: vec![
                Cell::graph(
                    0,
                    0,
                    4,
                    3,
                    GraphBinding::new(GraphConfig::Xy)
                        .bind("x", "mix")
                        .bind("y", "gain"),
                ),
                Cell::span(4, 0, 4, 3, WidgetKind::Canvas, CellParam::None),
            ],
            dividers: Vec::new(),
        });
        panel.pages.push(PageLabels {
            key: "display".into(),
            labels: labels("Display", "Anzeige"),
        });
        Some(PanelV2 {
            panel,
            graph_labels: [(
                (1, 0),
                GraphLabels {
                    name: labels("Gain and mix", "Pegel und Mischung"),
                    description: labels(
                        "Edit both parameters with one handle.",
                        "Beide Parameter mit einem Griff bearbeiten.",
                    ),
                    hide_title: false,
                },
            )]
            .into(),
            canvases: [(
                (1, 1),
                Canvas {
                    key: "response".into(),
                    horizontal_viewport: true,
                    show_title: false,
                    name: labels("Gain response", "Pegelverlauf"),
                    description: labels(
                        "Output gain from zero to full mix.",
                        "Ausgangspegel von null bis zur vollen Mischung.",
                    ),
                },
            )]
            .into(),
            headings: Default::default(),
        })
    }
}

impl DisplaySource for GainParams {
    fn paint(&self, display: &mut Display) {
        display.canvas("response", |painter| {
            let gain = (self.gain.value() + 36.0) / 48.0;
            let mix = self.mix.value();
            painter.line([0.0, 0.25], [1.0, 0.25], Tone::Grid);
            painter.line([0.0, 0.25], [1.0, 1.0 - gain], Tone::Accent);
            painter.line([mix, 0.0], [mix, 1.0], Tone::Muted);
        });
    }
}
