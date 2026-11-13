//! The komponist application.
//!
//! The app shell with layout 2a and dock, views (arrangement, piano roll, step sequencer, mixer,
//! inspector, browser, devices), dialogs, central action dispatch and the keymap. The only crate
//! that knows both egui and the engine. The library exists so that integration and acceptance
//! tests can drive the app headless; the binary `komponist` starts it.
//!
//! Contract modules (ARCHITECTURE §9.3):
//! - [`actions`]: [`Action`] and its sub-enums; every interaction as data.
//! - [`ui_state`]: [`UiState`] with one struct per panel, [`FocusArea`].
//! - [`keymap`]: [`Keymap`], key chords and the computer keyboard MIDI layer.
//!
//! The shell (WP-30):
//! - [`engine_sync`]: [`engine_sync::EngineSync`], document → program → engine, the audio stream.
//! - [`device_host`]: [`device_host::DeviceHost`], where processors come from; the seam for the
//!   plugin manager (WP-45).
//! - [`meters`]: meter ballistics fed from the engine once per frame.
//! - [`settings`]: the settings file, `~/.config/komponist/settings.json`.
//! - [`metrics`]: shell sizes the handoff gives only in the screen data.

pub mod actions;
pub mod device_host;
pub mod engine_sync;
pub mod keymap;
pub mod meters;
pub mod metrics;
pub mod settings;
pub mod ui_state;

pub use actions::{
    Action, EditAction, PluginAction, ProjectAction, SettingsAction, TransportAction, UiAction,
};
pub use keymap::{Key, KeyChord, Keymap, Modifiers};
pub use ui_state::{FocusArea, UiState};

// Unit tests of the engine wiring run the audio callback, which `komponist-audio` wraps in
// `assert_no_alloc`; that needs its allocator (debug builds only).
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
