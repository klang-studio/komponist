//! The sampler (WP-87): komponist's own drum sampler, an internal instrument.
//!
//! Eight pads (4 × 2) on the notes 36–43 ([`FIRST_NOTE`]). Each pad plays one sample (WAV, mono or
//! stereo, any rate) with start, end, tune, decay, drive, pan, level, mute, solo, a choke group and
//! a mode (one shot or gate); the kit has a tune and "Pad folgt Eingang". The device name is
//! [`NAME`] (one place, here); it is an [`Internal`](crate::Internal) like the effects, with
//! role instrument.
//!
//! # Engine: `klopfer_dsp` by path
//!
//! The voices are klopfer's (`../klopfer/dsp`, read-only, GPL-3.0-or-later like komponist): its
//! `Engine` has fixed-size voice and pad arrays, takes samples as `Arc<Sample>` slots it only reads
//! and allocates nothing in `note_on` / `render_frame`, which is what an RT instrument needs. The
//! pad's start/end, tune, drive, pan, gain, amp envelope and choke group are its `PadSettings`;
//! the interpolation is Hermite and resamples any sample rate. What the sampler does not use
//! (filter, LFO, modulation slots, layers, slicing, stretch) stays off. Copying it would only fork
//! it, so it is a path dependency like `synth-core`.
//!
//! # Parameters
//!
//! Wire ids: 0 `kit_tune`, 1 `follow`, then 11 per pad from 2 (`2 + 11 · pad + field`,
//! [`PadField`]): `pad{n}_start`, `_end` (milliseconds into the sample; `_end` at its maximum
//! means "to the end"), `_mode` (1× / Gate), `_tune`, `_decay` (seconds, at its maximum "Aus": the
//! sample plays out), `_drive`, `_pan`, `_level`, `_mute`, `_solo`, `_choke` (0 = none).
//!
//! # Samples
//!
//! Not parameters: links in the device state ([`links`]), loaded on a worker thread
//! ([`load`]) and handed to the audio thread lock-free. The UI side is the [`SamplerHandle`] of
//! the processor ([`crate::InternalProcessor::sampler`]).

pub mod links;
pub mod load;
pub mod testkit;

mod dsp;

use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half, Page, WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

pub use links::{Collector, SampleLink};
pub use load::{PadStatus, PadView, SamplerHandle};

use crate::internal::{Dsp, Spec};

/// The device's name in the browser and on the card. The only place it is written.
pub const NAME: &str = "Sampler";

/// Pads of the sampler.
pub const PADS: usize = 8;

/// The note of the first pad (C1, the General MIDI kick).
pub const FIRST_NOTE: u8 = 36;

/// The pad (0-based) a note plays, if any.
pub fn pad_of_note(note: u8) -> Option<usize> {
    note.checked_sub(FIRST_NOTE)
        .map(usize::from)
        .filter(|p| *p < PADS)
}

/// The note of pad `pad` (0-based).
pub fn note_of_pad(pad: usize) -> u8 {
    FIRST_NOTE + pad as u8
}

/// The longest start or end, in milliseconds; an end at this value plays to the sample's end.
pub const MAX_MS: f64 = 10_000.0;

/// The longest decay in seconds; a decay at this value is "Aus" (the sample plays out).
pub const MAX_DECAY_S: f64 = 10.0;

/// The most choke groups.
pub const CHOKE_GROUPS: i32 = 8;

/// The device.
pub(crate) static SPEC: Spec = Spec {
    id: "sampler",
    name: NAME,
    features: &["instrument", "drum", "sampler"],
    params,
    layout,
    dsp,
};

/// Wire id of the kit tune.
pub const KIT_TUNE: usize = 0;
/// Wire id of "Pad folgt Eingang".
pub const FOLLOW: usize = 1;
/// Wire id of the first pad parameter.
pub const PAD_BASE: usize = 2;

/// A per-pad parameter, in wire order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadField {
    /// Where playback starts (ms).
    Start = 0,
    /// Where it ends (ms; the maximum is "to the end").
    End,
    /// 1× or Gate.
    Mode,
    /// Pitch in semitones.
    Tune,
    /// Fade time in seconds ("Aus" at the maximum).
    Decay,
    /// Saturation.
    Drive,
    /// Pan.
    Pan,
    /// Level in dB.
    Level,
    /// Mute.
    Mute,
    /// Solo.
    Solo,
    /// Choke group.
    Choke,
}

impl PadField {
    /// Parameters per pad.
    pub const COUNT: usize = 11;

    /// All of them in wire order.
    pub const ALL: [PadField; PadField::COUNT] = [
        PadField::Start,
        PadField::End,
        PadField::Mode,
        PadField::Tune,
        PadField::Decay,
        PadField::Drive,
        PadField::Pan,
        PadField::Level,
        PadField::Mute,
        PadField::Solo,
        PadField::Choke,
    ];

    /// The key after `pad{n}_`.
    pub fn key(self) -> &'static str {
        match self {
            PadField::Start => "start",
            PadField::End => "end",
            PadField::Mode => "mode",
            PadField::Tune => "tune",
            PadField::Decay => "decay",
            PadField::Drive => "drive",
            PadField::Pan => "pan",
            PadField::Level => "level",
            PadField::Mute => "mute",
            PadField::Solo => "solo",
            PadField::Choke => "choke",
        }
    }

    /// The wire id of this field of pad `pad` (0-based).
    pub fn wire(self, pad: usize) -> usize {
        PAD_BASE + pad * PadField::COUNT + self as usize
    }

    /// The pad (0-based) and field a wire id belongs to; `None` for the global ones.
    pub fn of_wire(wire: usize) -> Option<(usize, PadField)> {
        let i = wire.checked_sub(PAD_BASE)?;
        let pad = i / PadField::COUNT;
        (pad < PADS).then(|| (pad, PadField::ALL[i % PadField::COUNT]))
    }
}

/// "375 ms" below a second, "1.20 s" above.
fn time_text(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{} ms", ms.round())
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

fn end_text(v: f64) -> String {
    if v >= MAX_MS {
        "Ende".to_owned()
    } else {
        time_text(v)
    }
}

fn decay_text(v: f64) -> String {
    if v >= MAX_DECAY_S {
        "Aus".to_owned()
    } else {
        time_text(v * 1000.0)
    }
}

fn params() -> Vec<ParamInfo> {
    let mut list = vec![
        ParamInfo::new("kit_tune", "Kit-Tune", ParamKind::lin(-24.0, 24.0), 0.0)
            .unit(Unit::Semitones),
        ParamInfo::new("follow", "Pad folgt Eingang", ParamKind::Bool, 1.0),
    ];
    for pad in 1..=PADS {
        let key = |f: PadField| format!("pad{pad}_{}", f.key());
        for field in PadField::ALL {
            let id = ParamId::new(key(field));
            list.push(match field {
                PadField::Start => {
                    ParamInfo::new(id, format!("Start {pad}"), ParamKind::lin(0.0, MAX_MS), 0.0)
                        .unit(Unit::Ms)
                }
                PadField::End => ParamInfo::new(
                    id,
                    format!("Ende {pad}"),
                    ParamKind::lin(0.0, MAX_MS),
                    MAX_MS,
                )
                .unit(Unit::Ms)
                .formatted(end_text),
                PadField::Mode => ParamInfo::new(
                    id,
                    format!("Modus {pad}"),
                    ParamKind::Enum(vec!["1×".into(), "Gate".into()]),
                    0.0,
                ),
                PadField::Tune => {
                    ParamInfo::new(id, format!("Tune {pad}"), ParamKind::lin(-24.0, 24.0), 0.0)
                        .unit(Unit::Semitones)
                }
                PadField::Decay => ParamInfo::new(
                    id,
                    format!("Decay {pad}"),
                    ParamKind::scaled(0.01, MAX_DECAY_S, Scale::Log),
                    MAX_DECAY_S,
                )
                .unit(Unit::S)
                .formatted(decay_text),
                PadField::Drive => {
                    ParamInfo::new(id, format!("Drive {pad}"), ParamKind::lin(0.0, 1.0), 0.0)
                        .unit(Unit::Percent)
                }
                PadField::Pan => ParamInfo::new(
                    id,
                    format!("Pan {pad}"),
                    ParamKind::scaled(-1.0, 1.0, Scale::Pan),
                    0.0,
                ),
                PadField::Level => ParamInfo::new(
                    id,
                    format!("Pegel {pad}"),
                    ParamKind::scaled(-70.0, 6.0, Scale::Db),
                    0.0,
                )
                .unit(Unit::Db),
                PadField::Mute => ParamInfo::new(id, format!("Mute {pad}"), ParamKind::Bool, 0.0),
                PadField::Solo => ParamInfo::new(id, format!("Solo {pad}"), ParamKind::Bool, 0.0),
                PadField::Choke => ParamInfo::new(
                    id,
                    format!("Choke {pad}"),
                    ParamKind::Int {
                        min: 0,
                        max: CHOKE_GROUPS,
                    },
                    0.0,
                ),
            });
        }
    }
    list
}

/// Nine columns as in the device handoff: the pads and the kit on the left, the selected pad's
/// waveform and knobs in the middle, its level fader on the right. Every per-pad cell is a
/// template on `{sel}`, the pad the pad grid (or the step sequencer's row) selected.
fn layout() -> DeviceLayout {
    use komponist_model::params::layout::SEL;
    let pad = |field: &str| CellParam::Template(format!("pad{{{SEL}}}_{field}"));
    let id = |key: &str| CellParam::Id(ParamId::new(key));
    let knob = |col: u8, field: &str| Cell::span(col, 2, 1, 1, WidgetKind::Knob, pad(field));
    let cells = vec![
        Cell::span(0, 0, 4, 2, WidgetKind::PadGrid, CellParam::None),
        Cell::half(0, 2, 2, Half::Top, WidgetKind::ToggleButton, id("follow")),
        Cell::half(0, 2, 1, Half::Bottom, WidgetKind::ToggleButton, pad("mute")),
        Cell::half(1, 2, 1, Half::Bottom, WidgetKind::ToggleButton, pad("solo")),
        Cell::half(2, 2, 1, Half::Top, WidgetKind::Number, pad("choke")),
        Cell::span(3, 2, 1, 1, WidgetKind::Knob, id("kit_tune")),
        Cell::graph(
            4,
            0,
            4,
            1,
            GraphBinding::new(GraphConfig::Waveform)
                .titled(format!("Pad {{{SEL}}}"))
                .bind("start", format!("pad{{{SEL}}}_start"))
                .bind("end", format!("pad{{{SEL}}}_end")),
        ),
        Cell::half(4, 1, 2, Half::Top, WidgetKind::Segment, pad("mode")),
        Cell::half(6, 1, 1, Half::Top, WidgetKind::Number, pad("start")),
        Cell::half(7, 1, 1, Half::Top, WidgetKind::Number, pad("end")),
        knob(4, "tune"),
        knob(5, "decay"),
        knob(6, "drive"),
        knob(7, "pan"),
        Cell::span(8, 0, 1, 3, WidgetKind::MiniFader, pad("level")),
    ];
    DeviceLayout::single(Page {
        name: String::new(),
        cols: 9,
        cells,
        dividers: vec![3, 7],
    })
}

fn dsp() -> Box<dyn Dsp> {
    Box::new(dsp::SamplerDsp::new())
}

#[cfg(test)]
mod tests;
