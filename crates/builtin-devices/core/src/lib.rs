//! Shared API for built-in devices in komponist.
//!
//! This internal API is unstable and may change with the application. A device defines its
//! parameters, layout and DSP. This crate adapts those definitions to the engine's audio and event
//! interfaces, supplies shared DSP and layout helpers, and handles parameter-map state and presets.
//! Device implementations and registration live in other crates.

#![deny(missing_docs)]

use komponist_i18n::Lang;
use komponist_model::params::{ParamInfo, ValueTexts};

pub mod dsp;
pub mod layout;
pub mod midi;
mod processor;
mod spec;

pub use processor::{Dsp, INITIAL_RATE, InternalProcessor, SAMPLE_RATES};
pub use spec::{CatalogSpec, Preset, Spec, SpecCache, StateError, StateParams, no_presets};

/// The localized switch values and percent suffix for device parameters.
pub fn value_texts(lang: Lang) -> ValueTexts {
    ValueTexts {
        on: lang.value_on(),
        off: lang.value_off(),
        percent: lang.value_percent(),
    }
}

/// Prepares a macro control for display, automation and optional modulation.
/// The device supplies the mapping from the macro value to its own DSP state.
pub fn macro_param(
    info: ParamInfo,
    lang: Lang,
    format: impl Fn(f64) -> String + Send + Sync + 'static,
    modulatable: bool,
) -> ParamInfo {
    info.texts(value_texts(lang))
        .formatted(format)
        .flags(true, modulatable)
}

/// The name of preset category `category` in `lang`. Categories are stored as their German
/// names (`hexa_core::categories`, [`Preset::category`]); a category the catalog
/// does not know is returned as it is. Preset names are English everywhere and not translated.
pub fn category_text(lang: Lang, category: &str) -> &str {
    match category {
        "Init" => lang.preset_category_init(),
        "Tasten" => lang.preset_category_keys(),
        "Mallets & Glocken" => lang.preset_category_mallets(),
        "Gezupft" => lang.preset_category_plucked(),
        "Bass" => lang.preset_category_bass(),
        "Blech & Holz" => lang.preset_category_winds(),
        "Streicher & Pads" => lang.preset_category_strings_pads(),
        "Leads" => lang.preset_category_leads(),
        "Synth" => lang.preset_category_synth(),
        "Effekte & Perkussion" => lang.preset_category_fx_percussion(),
        "Verzerrt & Lo-Fi" => lang.preset_category_distorted(),
        "Wavefolder" => lang.preset_category_wavefolder(),
        "Rhythmisch" => lang.preset_category_rhythmic(),
        "Filter-Operatoren" => lang.preset_category_filter_operators(),
        "Matrix" => lang.preset_category_matrix(),
        "Techno & House" => lang.preset_category_techno_house(),
        "Trance & Rave" => lang.preset_category_trance_rave(),
        "Synthbass" => lang.preset_category_synthbass(),
        "Chiptune" => lang.preset_category_chiptune(),
        "Ambient & Glitch" => lang.preset_category_ambient(),
        "Lead" => lang.preset_category_lead(),
        "Pad" => lang.preset_category_pad(),
        "Pluck" => lang.preset_category_pluck(),
        "Brass" => lang.preset_category_brass(),
        "FX" => lang.preset_category_fx(),
        other => other,
    }
}
