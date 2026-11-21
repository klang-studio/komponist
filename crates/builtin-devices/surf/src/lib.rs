//! **surf** (#107): a wavetable synthesizer, an internal instrument.
//!
//! Proposal `107-wavetable` (internal notes), owner decision 2026-11-21: layout B (one 3D graph with
//! an A/B switch, volt's width), volt's voice shared. surf is volt's engine
//! ([`komponist_volt::Engine`]) with two wavetable oscillators ([`osc::Wavetable`]) instead of the
//! analog ones; the sub, noise, high-pass, filter, three envelopes, two LFOs, the matrix, unison,
//! glide, the voice modes and the analog drift are volt's, as are pages 2 and 3. The name is
//! [`NAME`], the only place it is written.
//!
//! # Signal flow
//!
//! ```text
//! Table A @ Position A ─┐
//!                       ├─► Mix ─► + Sub + Noise ─► HP ─► Drive ─► Filter ─► VCA ─► Volume ─► Out
//! Table B @ Position B ─┘  (unison: 1 … 7 pairs per key)
//! ```
//!
//! - **Tables** ([`bank`]): 16 original factory tables of 32 frames, generated from the recipes in
//!   `komponist_model::params::wavetable` when the first surf is created, ten band-limited levels
//!   each. No sample files, no import (a later issue).
//! - **Oscillators** ([`osc`]): per control interval (32 samples) the frames around the position
//!   and the two levels for the pitch; per sample a linear read of four slices, blended. The
//!   position is smoothed like every continuous parameter and is a matrix destination (Position
//!   A/B, where volt has its pulse widths; 100 % sweeps the whole table).
//! - **Tuning:** A and B each have an octave and a fine tune against the played pitch.

use komponist_i18n::Lang;
use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half, OSC_VIEW, Page, WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo};

use komponist_device_core::layout::{knob, knob_large, template};
use komponist_device_core::{CatalogSpec, Dsp, Spec, SpecCache};

pub mod bank;
pub mod osc;
pub mod params;
pub mod presets;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;

/// surf: volt's engine with the wavetable oscillators.
pub type Surf = komponist_volt::Engine<osc::Wavetable>;

/// The device's name in the browser and on the card. The only place it is written.
pub const NAME: &str = "Surf";

/// The stored id of the device in project files.
pub const ID: &str = "surf";

/// The oscillators the switch on the first page chooses between: context value and label (the
/// same in every language).
pub const OSCILLATORS: &[(&str, &str)] = &[("a", "A"), ("b", "B")];

/// Catalog, parameters, layout, presets and DSP for surf.
pub static SPEC: Spec = Spec {
    catalog: CatalogSpec {
        id: ID,
        name: NAME,
        name_in: Lang::device_name_surf,
        role: komponist_model::DeviceRole::Instrument,
        features: &["instrument", "synthesizer", "wavetable"],
        vendor: "komponist",
        sidechain: false,
    },
    params,
    layout,
    dsp,
    presets: presets::presets,
    event_stage: None,
    preset_aliases: &[],
    migrate_params: None,
    cache: SpecCache::new(),
};

fn params(t: Lang) -> Vec<ParamInfo> {
    params::params(t)
}

fn dsp() -> Box<dyn Dsp> {
    // The tables are built here, off the audio thread, once per process.
    bank::bank();
    Box::new(Surf::new())
}

fn half(col: u8, row: u8, cols: u8, half: Half, widget: WidgetKind, id: &str) -> Cell {
    Cell::half(
        col,
        row,
        cols,
        half,
        widget,
        CellParam::Id(ParamId::new(id)),
    )
}

fn number(col: u8, row: u8, id: &str) -> Cell {
    half(col, row, 1, Half::Bottom, WidgetKind::Number, id)
}

/// `osc_{osc_view}_<what>`: the parameter of the oscillator the switch shows.
fn osc_param(what: &str) -> CellParam {
    template(&format!("osc_{{{OSC_VIEW}}}_{what}"))
}

/// The wavetable graph of the oscillator the switch shows.
fn wavetable_graph() -> Cell {
    let mut binding = GraphBinding::new(GraphConfig::Wavetable)
        .bind("table", format!("osc_{{{OSC_VIEW}}}_table"))
        .bind("position", format!("osc_{{{OSC_VIEW}}}_pos"));
    for n in 1..=4 {
        for what in ["src", "dst", "amt"] {
            let id = format!("mod{n}_{what}");
            binding = binding.bind(id.clone(), id);
        }
    }
    binding = binding
        .bind("lfo1_uni", "lfo1_uni")
        .bind("lfo2_uni", "lfo2_uni");
    Cell::graph_half(0, 0, 4, 1, binding)
}

/// Page 1, "Klang" ("Sound"): the oscillator the switch shows, the filter, the output (10
/// columns, layout B of the proposal).
fn page_sound(t: Lang) -> Page {
    let cells = vec![
        wavetable_graph(),
        Cell::half(
            0,
            1,
            1,
            Half::Bottom,
            WidgetKind::Segment,
            CellParam::Choice {
                key: OSC_VIEW,
                label: t.label_surf_osc(),
                options: OSCILLATORS.to_vec(),
            },
        ),
        Cell::half(
            1,
            1,
            2,
            Half::Bottom,
            WidgetKind::Dropdown,
            osc_param("table"),
        ),
        Cell::half(3, 1, 1, Half::Bottom, WidgetKind::Number, osc_param("oct")),
        Cell::span(0, 2, 2, 1, WidgetKind::KnobLarge, osc_param("pos")),
        Cell::span(2, 2, 1, 1, WidgetKind::Knob, osc_param("fine")),
        knob(3, 2, "osc_mix"),
        komponist_volt::filter_graph(4),
        half(4, 1, 2, Half::Bottom, WidgetKind::Dropdown, "filter_type"),
        knob_large(4, 2, "cutoff"),
        number(6, 1, "drive"),
        knob(6, 2, "resonance"),
        number(7, 1, "key_amount"),
        knob(7, 2, "env_amount"),
        knob(8, 0, "sub_level"),
        knob(9, 0, "noise_level"),
        number(8, 1, "hp_freq"),
        number(9, 1, "bend"),
        knob_large(8, 2, "volume"),
    ];
    Page {
        name: t.page_volt_sound().into(),
        cols: 10,
        cells,
        dividers: vec![3, 7],
    }
}

/// Page 3: volt's matrix page, and the global octave beside the voices (page 1 has the
/// oscillators' own octaves).
fn page_matrix(t: Lang) -> Page {
    let mut page = komponist_volt::page_matrix(t);
    page.cells
        .push(half(7, 0, 1, Half::Bottom, WidgetKind::Number, "octave"));
    page
}

fn layout(t: Lang) -> DeviceLayout {
    DeviceLayout {
        pages: vec![
            page_sound(t),
            komponist_volt::page_envelopes(t),
            page_matrix(t),
        ],
    }
}
