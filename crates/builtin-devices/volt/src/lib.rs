//! **volt** (WP-95): a virtual-analog synthesizer, an internal instrument.
//!
//! Variant B „standard“ of the VA-synth proposal (internal notes) (owner decision 2026-11-15): two
//! oscillators with a sub and noise, a high-pass, one multimode filter, three envelopes, two
//! tempo-syncable LFOs, a four-slot matrix with fixed paths for velocity, mod wheel and pressure,
//! unison with detune and spread, analog drift, glide, poly, mono and legato. The name is
//! [`NAME`], the only place it is written.
//!
//! # Signal flow
//!
//! ```text
//! OSC A ─┐
//!        ├─► Mix ─► + Sub + Noise ─► HP (6 dB) ─► Drive ─► Filter ─► VCA ─► Volume ─► Out
//! OSC B ─┘  (unison: 1 … 7 pairs per key, summed in front of the filter)
//! ```
//!
//! - **Oscillators** ([`osc`]): PolyBLEP saw and pulse, PolyBLAMP triangle, `klang_dsp`'s sine;
//!   phases are `u32`. With a unison above 1 the phases start random, else at 0. The sub is a
//!   sine one octave under oscillator A.
//! - **Filter** ([`filter`]): LP 24 is a zero-delay ladder (four TPT one-poles, the feedback
//!   solved in closed form, `tanh`-like clip at the input, resonance up to self-oscillation, part
//!   of the lost passband made up); LP 12, BP 12, HP 12 and notch are `klang_dsp`'s Simper SVF.
//!   With `Spread > 0` and `Unison > 1` the filter runs twice (left, right), else once. The curve
//!   in the graph comes from the same formulas (`komponist_model::params::va_curve`).
//! - **Control rate:** every [`CONTROL`](komponist_device_core::dsp::CONTROL) = 32 samples the smoothed
//!   parameters, envelopes, LFOs, matrix and glide advance and every voice recomputes its
//!   increments, filter coefficients and gain (the gain ramps linearly across the interval). The
//!   counter runs across calls, so the output does not depend on how the host splits blocks.
//! - **Envelopes** ([`env`]): linear attack, exponential decay and release whose time is the time
//!   to fall 40 dB; a new gate starts from the current level.
//! - **LFOs** ([`lfo`]): sine, triangle, saw, square, random (sample and hold) and smooth
//!   random; `Sync` takes the tempo and a divider (4 bars … 1/32, 4/4), `Key` gives every voice
//!   its own LFO that restarts on the key, `Fade` fades it in, `Uni` makes it unipolar (0 … 1).
//! - **Matrix:** nine sources, twelve destinations, units in `va_curve` (100 % = 24 st pitch,
//!   5 oct cutoff, 2 oct LFO rate, …). Fixed paths: velocity → amplitude (`velocity`) and filter
//!   envelope amount (`vel_filter`), mod wheel → vibrato by LFO 1 (`wheel_vib`), pressure →
//!   cutoff (`at_cut`), pitch bend → pitch (`bend` semitones).
//! - **Voices:** 16 in a fixed pool; `voices × unison` is capped at [`engine::MAX_GROUPS`]
//!   oscillator groups. A new note takes a free voice, else the quietest released one, else the
//!   oldest held one (which keeps its phases and level: no click). Mono and legato use voice 0
//!   with a note stack of 8 (last key wins, the previous key returns on release; mono restarts
//!   the envelopes, legato lets them run). Glide takes the same time for every interval, also
//!   only while a key is held ("Glide Legato").
//! - **Analog:** per voice a random walk on pitch (± 6 ct) and cutoff (± 0.08 oct), a static
//!   tolerance per oscillator (± 3 ct) and on the envelope times (± 3 %), scaled by `Analog`. All
//!   randomness is seeded from the note counter, so a project renders the same every time.
//!
//! The instrument gets pitch bend, the mod wheel (CC 1) and channel pressure from
//! [`Dsp::pitch_bend`], [`Dsp::mod_wheel`] and [`Dsp::pressure`], which the processor fills from
//! `EventKind::Midi`.

use komponist_i18n::Lang;
use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, ENV_VIEW, GraphBinding, GraphConfig, Half, LFO_VIEW, Page,
    WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo};

use komponist_device_core::layout::{knob, knob_large, template};
use komponist_device_core::{CatalogSpec, Dsp, Spec, SpecCache};

pub mod engine;
pub mod env;
pub mod filter;
pub mod lfo;
pub mod osc;
pub mod params;
pub mod presets;
pub mod voice;

pub use engine::{Engine, MAX_GROUPS, VOICES, Volt};
pub use osc::{Analog, Oscillators};
pub use params::id;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;

/// The device's name in the browser and on the card. The only place it is written.
pub const NAME: &str = "Volt";

/// The stored id of the device in project files; only [`NAME`] is shown to users.
pub const ID: &str = "volt";

/// Catalog, parameters, layout, presets and DSP for Volt.
pub static SPEC: Spec = Spec {
    catalog: CatalogSpec {
        id: ID,
        name: NAME,
        name_in: Lang::device_name_volt,
        description_in: Some(Lang::device_description_volt),
        role: komponist_model::DeviceRole::Instrument,
        features: &["instrument", "synthesizer", "analog"],
        vendor: "komponist",
        sidechain: false,
    },
    params,
    layout,
    presets: presets::presets,
    cache: SpecCache::new(),
};

fn params(t: Lang) -> Vec<ParamInfo> {
    params::params(t)
}

pub fn dsp() -> Box<dyn Dsp> {
    Box::new(Volt::new())
}

fn key(id: &str) -> ParamId {
    ParamId::new(id)
}

fn half(col: u8, row: u8, cols: u8, half: Half, widget: WidgetKind, id: &str) -> Cell {
    Cell::half(col, row, cols, half, widget, CellParam::Id(key(id)))
}

fn number(col: u8, row: u8, id: &str) -> Cell {
    half(col, row, 1, Half::Bottom, WidgetKind::Number, id)
}

/// The graph of the filter: slots and the parameters behind them (surf shares it).
pub fn filter_graph(col: u8) -> Cell {
    let mut binding = GraphBinding::new(GraphConfig::FilterVa)
        .bind("cutoff", "cutoff")
        .bind("resonance", "resonance")
        .bind("type", "filter_type");
    for n in 1..=4 {
        for what in ["src", "dst", "amt"] {
            let id = format!("mod{n}_{what}");
            binding = binding.bind(id.clone(), id);
        }
    }
    binding = binding
        .bind("at_cut", "at_cut")
        .bind("lfo1_uni", "lfo1_uni")
        .bind("lfo2_uni", "lfo2_uni");
    Cell::graph_half(col, 0, 4, 1, binding)
}

fn signal_graph(col: u8) -> Cell {
    let mut binding = GraphBinding::new(GraphConfig::Signal);
    for (slot, id) in [
        ("wave_a", "osc_a_wave"),
        ("wave_b", "osc_b_wave"),
        ("pw_a", "osc_a_pw"),
        ("pw_b", "osc_b_pw"),
        ("b_oct", "osc_b_oct"),
        ("b_fine", "osc_b_fine"),
        ("mix", "osc_mix"),
        ("sub", "sub_level"),
    ] {
        binding = binding.bind(slot, id);
    }
    Cell::graph_half(col, 0, 3, 1, binding)
}

/// Page 1, "Klang" ("Sound"): oscillators, filter, output (10 columns, `LAYOUTS.md`).
fn page_sound(t: Lang) -> Page {
    let cells = vec![
        half(0, 0, 2, Half::Top, WidgetKind::Segment, "osc_a_wave"),
        half(0, 0, 2, Half::Bottom, WidgetKind::Segment, "osc_b_wave"),
        knob(0, 1, "osc_b_oct"),
        knob(0, 2, "osc_mix"),
        knob(1, 1, "osc_b_fine"),
        knob(1, 2, "sub_level"),
        knob(2, 0, "osc_a_pw"),
        knob(2, 1, "osc_b_pw"),
        knob(2, 2, "noise_level"),
        filter_graph(3),
        half(3, 1, 2, Half::Bottom, WidgetKind::Dropdown, "filter_type"),
        knob_large(3, 2, "cutoff"),
        number(5, 1, "drive"),
        knob(5, 2, "resonance"),
        number(6, 1, "key_amount"),
        knob(6, 2, "env_amount"),
        signal_graph(7),
        number(7, 1, "hp_freq"),
        knob_large(7, 2, "volume"),
        number(8, 1, "bend"),
        number(9, 1, "octave"),
        knob(9, 2, "analog"),
    ];
    Page {
        name: t.page_volt_sound().into(),
        cols: 10,
        cells,
        dividers: vec![2, 6],
    }
}

/// The envelopes the left switch of the second page chooses between: context value and label
/// (the names are the same in every language).
pub const ENVELOPES: &[(&str, &str)] = &[("amp", "Amp"), ("filt", "Filter"), ("mod", "Mod")];
/// The LFOs the right switch chooses between.
pub const LFOS: &[(&str, &str)] = &[("1", "LFO 1"), ("2", "LFO 2")];

/// Page 2, "Hüllkurven" ("Envelopes"): the chosen envelope and the chosen LFO (8 columns); surf
/// shows the same page.
pub fn page_envelopes(t: Lang) -> Page {
    let envelope = GraphBinding::new(GraphConfig::Envelope)
        .titled(t.graph_title_volt_envelope(format!("{{{ENV_VIEW}_label}}")))
        .bind("attack", format!("{{{ENV_VIEW}}}_a"))
        .bind("decay", format!("{{{ENV_VIEW}}}_d"))
        .bind("sustain", format!("{{{ENV_VIEW}}}_s"))
        .bind("release", format!("{{{ENV_VIEW}}}_r"));
    let lfo =
        GraphBinding::new(GraphConfig::LfoShape).bind("shape", format!("lfo{{{LFO_VIEW}}}_shape"));
    let env = |col: u8, what: &str| {
        Cell::span(
            col,
            2,
            1,
            1,
            WidgetKind::Knob,
            template(&format!("{{{ENV_VIEW}}}_{what}")),
        )
    };
    let lfo_param = |what: &str| template(&format!("lfo{{{LFO_VIEW}}}_{what}"));
    let cells = vec![
        Cell::graph_half(0, 0, 4, 1, envelope),
        Cell::half(
            0,
            1,
            3,
            Half::Bottom,
            WidgetKind::Segment,
            CellParam::Choice {
                key: ENV_VIEW,
                label: t.label_volt_envelope(),
                options: ENVELOPES.to_vec(),
            },
        ),
        env(0, "a"),
        env(1, "d"),
        env(2, "s"),
        env(3, "r"),
        Cell::graph_half(4, 0, 4, 1, lfo),
        Cell::half(
            4,
            1,
            2,
            Half::Bottom,
            WidgetKind::Segment,
            CellParam::Choice {
                key: LFO_VIEW,
                label: t.label_volt_lfo(),
                options: LFOS.to_vec(),
            },
        ),
        Cell::half(4, 2, 2, Half::Top, WidgetKind::Dropdown, lfo_param("shape")),
        Cell::half(
            4,
            2,
            1,
            Half::Bottom,
            WidgetKind::ToggleButton,
            lfo_param("uni"),
        ),
        Cell::half(
            6,
            1,
            1,
            Half::Bottom,
            WidgetKind::ToggleButton,
            lfo_param("sync"),
        ),
        Cell::span(6, 2, 1, 1, WidgetKind::Knob, lfo_param("rate")),
        Cell::half(
            7,
            1,
            1,
            Half::Bottom,
            WidgetKind::ToggleButton,
            lfo_param("key"),
        ),
        Cell::span(7, 2, 1, 1, WidgetKind::Knob, lfo_param("fade")),
    ];
    Page {
        name: t.page_volt_envelopes().into(),
        cols: 8,
        cells,
        dividers: vec![3],
    }
}

/// Page 3, "Matrix": four slots, the fixed paths, the voices (8 columns); surf shows the same page.
pub fn page_matrix(t: Lang) -> Page {
    let slot = |n: usize| {
        let (row, h) = [
            (0, Half::Top),
            (0, Half::Bottom),
            (1, Half::Top),
            (1, Half::Bottom),
        ][n - 1];
        [
            half(0, row, 2, h, WidgetKind::Dropdown, &format!("mod{n}_src")),
            half(2, row, 2, h, WidgetKind::Dropdown, &format!("mod{n}_dst")),
            half(4, row, 1, h, WidgetKind::Number, &format!("mod{n}_amt")),
        ]
    };
    let mut cells: Vec<Cell> = (1..=4).flat_map(slot).collect();
    cells.extend([
        knob(0, 2, "vel_filter"),
        knob(1, 2, "wheel_vib"),
        knob(2, 2, "at_cut"),
        half(5, 0, 2, Half::Top, WidgetKind::Dropdown, "mode"),
        half(
            5,
            0,
            2,
            Half::Bottom,
            WidgetKind::ToggleButton,
            "glide_legato",
        ),
        knob(5, 1, "unison"),
        knob(5, 2, "glide"),
        knob(6, 1, "detune"),
        knob(6, 2, "analog"),
        half(7, 0, 1, Half::Top, WidgetKind::Number, "voices"),
        knob(7, 1, "spread"),
        knob(7, 2, "velocity"),
    ]);
    Page {
        name: t.page_volt_matrix().into(),
        cols: 8,
        cells,
        dividers: vec![4],
    }
}

fn layout(t: Lang) -> DeviceLayout {
    DeviceLayout {
        pages: vec![page_sound(t), page_envelopes(t), page_matrix(t)],
    }
}
