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
