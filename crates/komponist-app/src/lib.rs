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
//! - [`app`]: [`App`] (document, view state, engine, views, keymap, settings) and
//!   [`App::dispatch`], the one place that applies actions; [`App::headless`] for tests.
//! - [`layout`]: layout 2a as pure rect math; `shell` draws it.
//! - [`panels`]: transport bar, status bar, the dock's tab strip.
//! - [`views`]: the [`views::View`] trait and the slots WP-40 … WP-44 fill (placeholders until
//!   then).
//! - [`engine_sync`]: [`engine_sync::EngineSync`], document → program → engine, the audio stream.
//! - [`device_host`]: [`device_host::DeviceHost`], where processors come from, and its
//!   implementations: [`device_host::PluginHost`] (pisynth in process, everything else in bridges
//!   of the plugin manager) and [`device_host::BuiltinHost`] (pisynth only, tests).
//! - [`meters`]: meter ballistics fed from the engine once per frame.
//! - [`record`]: [`record::Recording`], the take that grows while recording (WP-51).
//! - [`settings`]: the settings file, `~/.config/komponist/settings.json`.
//! - [`metrics`]: shell sizes the handoff gives only in the screen data.
//! - [`windows`]: windows besides the main one; [`windows::plugin_frame`], the S05 plugin frame
//!   around a plugin's own GUI (WP-50).

pub mod actions;
pub mod app;
pub mod device_host;
pub mod engine_sync;
pub mod keymap;
pub mod layout;
pub mod meters;
pub mod metrics;
pub mod panels;
pub mod record;
pub mod settings;
mod shell;
pub mod ui_state;
pub mod views;
pub mod windows;

pub use actions::{
    Action, EditAction, PluginAction, ProjectAction, SettingsAction, TransportAction, UiAction,
};
pub use app::App;
pub use keymap::{Key, KeyChord, Keymap, Modifiers};
pub use shell::names;
pub use ui_state::{FocusArea, UiState};

// Unit tests of the engine wiring run the audio callback, which `komponist-audio` wraps in
// `assert_no_alloc`; that needs its allocator (debug builds only).
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
