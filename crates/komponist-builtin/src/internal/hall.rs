//! Hall: reverb with room, decay, pre-delay, damping, diffusion, width and modulation. Placeholder
//! DSP: the audio passes through unchanged (WP-90 brings the plate reverb).

use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, WidgetKind};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

use super::{Dsp, Spec, knob, knob_large, single_page, toggle};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "hall",
    name: "Hall",
    features: &["audio-effect", "reverb"],
    params,
    layout,
    dsp,
};

fn params() -> Vec<ParamInfo> {
    let percent = |id: &str, name: &str, default: f64| {
        ParamInfo::new(id, name, ParamKind::lin(0.0, 1.0), default).unit(Unit::Percent)
    };
    vec![
        ParamInfo::new(
            "size",
            "Größe",
            ParamKind::scaled(1.0, 60.0, Scale::Log),
            28.0,
        )
        .unit(Unit::Meters),
        ParamInfo::new(
            "decay",
            "Decay",
            ParamKind::scaled(0.1, 20.0, Scale::Log),
            2.4,
        )
        .unit(Unit::S),
        ParamInfo::new("predelay", "Pre-Delay", ParamKind::lin(0.0, 250.0), 18.0).unit(Unit::Ms),
        ParamInfo::new(
            "damping",
            "Dämpfung",
            ParamKind::scaled(500.0, 20_000.0, Scale::Log),
            5200.0,
        )
        .unit(Unit::Hz),
        percent("diffusion", "Diffusion", 0.7),
        percent("width", "Breite", 0.9),
        percent("modulation", "Modul.", 0.25),
        percent("mix", "Mix", 0.22),
        ParamInfo::new(
            "room",
            "Raum",
            ParamKind::Enum(vec!["Zimmer".into(), "Halle".into(), "Kirche".into()]),
            1.0,
        ),
        ParamInfo::new("freeze", "Einfrieren", ParamKind::Bool, 0.0),
        ParamInfo::new("wet_only", "Nur Hall", ParamKind::Bool, 0.0),
    ]
}

/// Four columns of knobs, no graph.
fn layout() -> DeviceLayout {
    let id = |key: &str| CellParam::Id(ParamId::new(key));
    single_page(
        4,
        vec![
            knob_large(0, 0, "size"),
            knob(2, 0, "decay"),
            knob(3, 0, "predelay"),
            knob(0, 1, "damping"),
            knob(1, 1, "diffusion"),
            knob(2, 1, "width"),
            knob(3, 1, "modulation"),
            knob_large(0, 2, "mix"),
            Cell::half(2, 2, 2, Half::Top, WidgetKind::Segment, id("room")),
            toggle(2, 2, 1, Half::Bottom, id("freeze")),
            toggle(3, 2, 1, Half::Bottom, id("wet_only")),
        ],
    )
}

/// Pass-through.
struct Hall;

fn dsp() -> Box<dyn Dsp> {
    Box::new(Hall)
}

impl Dsp for Hall {
    fn activate(&mut self, _sample_rate: f32) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        for (i, o) in input.into_iter().zip(output) {
            o.copy_from_slice(i);
        }
    }

    fn reset(&mut self) {}
}
