//! EQ: parametric, six bands (`graph::eq`). Placeholder DSP: the audio passes through; the output
//! gain applies (bit-exact at 0 dB). The bands only drive the curve in the graph until WP-90 adds
//! the biquads.

use komponist_model::params::layout::{
    Cell, DeviceLayout, GraphBinding, GraphConfig, Half, WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

use super::{Dsp, Spec, db_to_gain, knob, single_page, template};

/// The device.
pub(super) static SPEC: Spec = Spec {
    id: "eq",
    name: "EQ",
    features: &["audio-effect", "equalizer", "filter"],
    params,
    layout,
    dsp,
};

/// Bands of the EQ.
pub const BANDS: usize = 6;

/// Band types, the order of `graph::eq`'s `band` constants (low cut, low shelf, bell, high shelf,
/// high cut).
pub const TYPES: [&str; 5] = ["LC", "LS", "Gl.", "HS", "HC"];

/// Frequencies the bands start at; the first [`BANDS_ON`] are on.
const FREQS: [f64; BANDS] = [60.0, 180.0, 420.0, 2400.0, 9000.0, 1000.0];
const BANDS_ON: usize = 5;

/// Range of a band's gain and of the output gain, ± dB.
pub const GAIN_RANGE: f64 = 24.0;

/// Parameters per band (`b{n}_on`, `_type`, `_freq`, `_gain`, `_q`), after the global ones.
const PER_BAND: usize = 5;

/// Wire id of the output gain.
pub const OUT: usize = 0;

fn params() -> Vec<ParamInfo> {
    let mut list = vec![
        ParamInfo::new(
            "out",
            "Ausgang",
            ParamKind::scaled(-GAIN_RANGE, GAIN_RANGE, Scale::Lin),
            0.0,
        )
        .unit(Unit::Db),
    ];
    for n in 1..=BANDS {
        let id = |what: &str| ParamId::new(format!("b{n}_{what}"));
        list.push(ParamInfo::new(
            id("on"),
            format!("Band {n} an"),
            ParamKind::Bool,
            f64::from(u8::from(n <= BANDS_ON)),
        ));
        list.push(ParamInfo::new(
            id("type"),
            format!("Typ {n}"),
            ParamKind::Enum(TYPES.iter().map(|t| (*t).to_owned()).collect()),
            2.0,
        ));
        list.push(
            ParamInfo::new(
                id("freq"),
                format!("Frequenz {n}"),
                ParamKind::scaled(20.0, 20_000.0, Scale::Log),
                FREQS[n - 1],
            )
            .unit(Unit::Hz),
        );
        list.push(
            ParamInfo::new(
                id("gain"),
                format!("Gain {n}"),
                ParamKind::scaled(-GAIN_RANGE, GAIN_RANGE, Scale::Lin),
                0.0,
            )
            .unit(Unit::Db),
        );
        list.push(ParamInfo::new(
            id("q"),
            format!("Q {n}"),
            ParamKind::scaled(0.1, 18.0, Scale::Log),
            0.71,
        ));
    }
    list
}

/// Six columns: the graph over two rows, below it the type and on switch of the selected band and
/// its frequency, gain and Q, and the output gain.
fn layout() -> DeviceLayout {
    let mut binding = GraphBinding::new(GraphConfig::Eq { bands: BANDS as u8 }).selecting("band");
    for n in 1..=BANDS {
        for what in ["on", "freq", "gain", "q", "type"] {
            let id = format!("b{n}_{what}");
            binding = binding.bind(id.clone(), id);
        }
    }
    single_page(
        6,
        vec![
            Cell::graph(0, 0, 6, 2, binding),
            Cell::half(
                0,
                2,
                2,
                Half::Top,
                WidgetKind::Segment,
                template("b{band}_type"),
            ),
            super::toggle(0, 2, 2, Half::Bottom, template("b{band}_on")),
            Cell::span(2, 2, 1, 1, WidgetKind::Knob, template("b{band}_freq")),
            Cell::span(3, 2, 1, 1, WidgetKind::Knob, template("b{band}_gain")),
            Cell::span(4, 2, 1, 1, WidgetKind::Knob, template("b{band}_q")),
            knob(5, 2, "out"),
        ],
    )
}

/// Pass-through with the output gain.
struct Eq {
    gain: f32,
}

fn dsp() -> Box<dyn Dsp> {
    Box::new(Eq { gain: 1.0 })
}

impl Dsp for Eq {
    fn activate(&mut self, _sample_rate: f32) {}

    fn set_param(&mut self, index: usize, value: f64) {
        if index == OUT {
            self.gain = db_to_gain(value);
        }
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        for (i, o) in input.into_iter().zip(output) {
            if self.gain == 1.0 {
                o.copy_from_slice(i);
            } else {
                for (s, d) in i.iter().zip(o.iter_mut()) {
                    *d = s * self.gain;
                }
            }
        }
    }

    fn reset(&mut self) {}
}

const _: () = assert!(PER_BAND == 5);
