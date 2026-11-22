use komponist_native_clap_sdk::{
    DeviceLayout, NativeClapLayout, Page, PageLabels, Panel, ParamId, layout::knob,
};

use super::NativeGain;

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
        }
    }
}
