//! Limiter: input gain and a ceiling, a normal device (also on the master chain). Placeholder DSP:
//! a hard clip at the ceiling after the input gain (no lookahead, no release; WP-90 brings the
//! real limiter). Below the ceiling the audio passes bit-exact at 0 dB input gain.
//!
//! `release` and `lookahead` are carried for the real DSP and do nothing yet.

use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, WidgetKind};
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};

use super::{Dsp, Spec, db_to_gain, knob, knob_large, single_page, toggle};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "limiter",
    name: "Limiter",
    features: &["audio-effect", "limiter", "dynamics"],
    params,
    layout,
    dsp,
};

/// Wire ids.
const GAIN: usize = 0;
const CEILING: usize = 1;

fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new("gain", "Eingang", ParamKind::lin(0.0, 24.0), 0.0).unit(Unit::Db),
        ParamInfo::new("ceiling", "Ceiling", ParamKind::lin(-12.0, 0.0), -0.3).unit(Unit::Db),
        ParamInfo::new(
            "release",
            "Release",
            ParamKind::scaled(10.0, 1000.0, Scale::Log),
            100.0,
        )
        .unit(Unit::Ms),
        ParamInfo::new("lookahead", "Lookahead", ParamKind::Bool, 1.0),
    ]
}

/// Four columns: input and ceiling as large knobs, release, lookahead and the gain-reduction
/// meter on the right.
fn layout() -> DeviceLayout {
    single_page(
        4,
        vec![
            knob_large(0, 0, "gain"),
            knob(2, 0, "release"),
            knob_large(0, 1, "ceiling"),
            toggle(2, 1, 1, Half::Top, CellParam::Id("lookahead".into())),
            Cell::span(3, 0, 1, 3, WidgetKind::MeterGr, CellParam::None),
        ],
    )
}

/// Hard clip at the ceiling.
struct Limiter {
    gain: f32,
    ceiling: f32,
}

fn dsp() -> Box<dyn Dsp> {
    Box::new(Limiter {
        gain: 1.0,
        ceiling: db_to_gain(-0.3),
    })
}

impl Dsp for Limiter {
    fn activate(&mut self, _sample_rate: f32) {}

    fn set_param(&mut self, index: usize, value: f64) {
        match index {
            GAIN => self.gain = db_to_gain(value),
            CEILING => self.ceiling = db_to_gain(value),
            _ => {}
        }
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let (gain, ceiling) = (self.gain, self.ceiling);
        for (i, o) in input.into_iter().zip(output) {
            for (s, d) in i.iter().zip(o.iter_mut()) {
                *d = (s * gain).clamp(-ceiling, ceiling);
            }
        }
    }

    fn reset(&mut self) {}
}
