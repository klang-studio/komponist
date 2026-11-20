//! Hexa is componist's six-operator FM synthesizer (ARCHITECTURE §8.5, D6).
//!
//! [`HexaProcessor`] runs a `Synth<1>` from the vendored `hexa-core` DSP engine as an in-process
//! engine processor. The DSP was derived from the pisynth hardware synthesizer.
//!
//! # API
//!
//! - [`HexaProcessor`]: the processor. [`HexaProcessor::new`] (a [`Patch`]) or
//!   [`HexaProcessor::from_state`] (device state, `None` = "Init"), then `activate` and
//!   `Command::InsertProcessor`.
//! - [`Macro`]: the 8 macros of the device card with their stable parameter ids 0–7, ranges,
//!   [`Macro::get`] / [`Macro::set`] on a patch and [`Macro::text`] for the value display. The
//!   mapping to patch fields is documented in [`macros`].
//! - State: [`encode_state`] / [`decode_state`] (the patch as JSON, see [`state`]).
//! - Factory presets: [`presets`] (index, name, category; "Presets · hexa" in the browser),
//!   [`categories`], [`preset_patch`], [`PRESET_COUNT`].
//! - [`device`]: the device card (WP-86): the parameters of hexa's layout ([`device::params`],
//!   each a view onto a [`Patch`] field, [`device::Field::get`] / [`device::Field::set`]) and the
//!   four-page layout ([`device::layout`]). The macros keep their wire ids 0–7; the card's own
//!   start at 8.
//! - [`algorithms`]: the 32 fixed algorithms the Algorithmus macro writes.
//!
//! # How the app drives it
//!
//! - **Create:** `processor_from_state(device.state.as_deref())`, activate at the engine
//!   rate, insert into the device's slot.
//! - **Card knob (or macro):** `Command::SetParam { slot, id: field.wire(), value }` to the engine,
//!   and `field.set(&mut patch, value)` on the document's copy (state = `encode_state(&patch)`),
//!   with `field = Field::from_wire(id)`. Both run the same function, so the running patch and the
//!   document stay equal.
//! - **Undo, redo or any other state change:** [`device::param_changes`] diffs the running patch
//!   against the new one into card parameters; sent as `SetParam`s in that order they turn the
//!   running patch into the new one, and sounding notes and effect tails go on (#19).
//! - **A change no card parameter reaches** (filters, effects, modulation, … as between most
//!   presets; `param_changes` gives `None`): build a new processor from the new state and insert
//!   it into the same slot; the engine retires the old one. Sounding notes and effect tails of
//!   the old processor end there. A preset is the state `encode_state(&preset_patch(index)?)`.
//!
//! # Realtime
//!
//! `process` and `reset` never allocate, lock or block (tested inside `assert_no_alloc`). Costs
//! measured on the development machine (12 cores, release build, 48 kHz, 2× oversampling, under
//! heavy parallel load; `tests/hexa/bench.rs`), per call of 256 frames (5.33 ms):
//!
//! | Load | Time | Share |
//! |---|---:|---:|
//! | 8-key chord, "Init" (2 operators) | 52 µs | 1.0 % |
//! | 8-key chord, "E-Piano" (6 operators, stereo) | 116 µs | 2.2 % |
//! | 8-key chord, "Supersaw" / "Strings" (unison, all 16 voices) | 300–330 µs | 5.7–6.1 % |
//! | one macro change (patch copy, `Macro::set`, `Synth::set_patch`) | 1.1 µs | |
//! | `reset` (rewrites the 2 MB synth in place) | 27 µs | |
//!
//! The cost scales linearly with the block length. Every event offset splits the render, and a
//! split costs one extra control-rate step, so many events at distinct offsets in one call cost
//! more than the same events at one offset. Creating a processor (UI thread) takes ~80 µs.

use komponist_device_core::CatalogSpec;
use komponist_i18n::Lang;
use komponist_model::DeviceRole;
use komponist_model::project::{BUILTIN_HEXA, HEXA_NAME};

pub mod algorithms;
pub mod device;
pub mod macros;
mod processor;
pub mod state;

pub use hexa_core::Patch;
pub use macros::Macro;
pub use processor::HexaProcessor;
pub use state::{
    PRESET_COUNT, PresetInfo, StateError, categories, decode_state, default_patch, encode_state,
    preset_patch, presets,
};

/// Hexa's entry in the built-in device catalog.
pub static CATALOG: CatalogSpec = CatalogSpec {
    id: BUILTIN_HEXA,
    name: HEXA_NAME,
    name_in: Lang::device_name_hexa,
    role: DeviceRole::Instrument,
    features: &["instrument", "synthesizer"],
    sidechain: false,
    vendor: "klang-studio",
};

/// Creates a processor from the saved patch, falling back to the Init patch for invalid state.
pub fn processor_from_state(state: Option<&[u8]>) -> HexaProcessor {
    HexaProcessor::from_state(state).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use komponist_model::PluginFormat;

    use super::*;

    #[test]
    fn catalog_keeps_the_persisted_id_and_localized_name() {
        let plugin = CATALOG.plugin_ref();
        assert_eq!(plugin.format, PluginFormat::Builtin);
        assert_eq!(plugin.id, "pisynth");
        assert_eq!(plugin.name, "Hexa");
        assert_eq!(plugin.vendor, "klang-studio");
        assert_eq!(CATALOG.role, DeviceRole::Instrument);
        assert_eq!(CATALOG.features, ["instrument", "synthesizer"]);
        assert!(!CATALOG.sidechain);
        assert_eq!((CATALOG.name_in)(Lang::De), "Hexa");
        assert_eq!((CATALOG.name_in)(Lang::En), "Hexa");
    }

    #[test]
    fn the_factory_still_plays_init_if_the_saved_patch_is_invalid() {
        let patch = preset_patch(1).unwrap();
        assert_eq!(
            processor_from_state(Some(&encode_state(&patch))).patch(),
            &patch,
        );
        assert_eq!(processor_from_state(None).patch(), &default_patch());
        assert_eq!(
            processor_from_state(Some(b"invalid JSON")).patch(),
            &default_patch(),
        );
    }
}
