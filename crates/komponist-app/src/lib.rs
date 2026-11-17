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
//!   implementations: [`device_host::PluginHost`] (hexa in process, everything else in bridges
//!   of the plugin manager) and [`device_host::BuiltinHost`] (hexa only, tests).
//! - [`meters`]: meter ballistics fed from the engine once per frame.
//! - [`record`]: [`record::Recording`], the take that grows while recording (WP-51).
//! - [`step_rows`]: which rows a new step clip gets per instrument and how rows are labelled
//!   (sampler pads "P1…", note names elsewhere; WP-54, WP-87).
//! - [`settings`]: the settings file, `~/.config/komponist/settings.json`
//!   (macOS: `~/Library/Application Support/komponist/settings.json`, Windows: `%APPDATA%\komponist\settings.json`).
//! - [`metrics`]: shell sizes the handoff gives only in the screen data.
//! - [`windows`]: windows besides the main one; [`windows::plugin_frame`], the S05 plugin frame
//!   around a plugin's own GUI (WP-50).
//!
//! Dialogs and the project lifecycle (WP-52):
//! - [`dialogs`]: S06 missing plugins, S07 settings, S09 start screen, new projects (S10) and the
//!   unsaved-changes prompt, the file browser.
//! - [`project_io`]: `.komponist` files, the recent list, new / open / save / save as.
//!
//! WAV export (WP-53b):
//! - [`export`]: the flow from "Exportieren" to the status bar message (engine detached, worker
//!   thread, devices, cancel); the S08 dialog is [`dialogs::export`].

/// The app id: desktop file and metainfo name, `StartupWMClass`, and the windows' Wayland app id /
/// X11 `WM_CLASS`. Reverse DNS of the repository (`tests/it/packaging.rs` checks the files).
pub const APP_ID: &str = "io.github.klang_studio.komponist";

pub mod actions;
pub mod app;
pub mod audio_import;
pub mod device_host;
pub mod dialogs;
pub mod dirs;
pub mod engine_sync;
pub mod export;
pub mod gain_trace;
pub mod keymap;
pub mod layout;
#[cfg(target_os = "macos")]
pub mod macos_delegate;
#[cfg(target_os = "macos")]
pub mod macos_menu;
pub mod master_scope;
pub mod meters;
pub mod metrics;
pub mod panels;
pub mod param_undo;
pub mod platform;
pub mod project_io;
pub mod record;
pub mod samples;
pub mod settings;
mod shell;
pub mod showcase;
pub mod spectrum;
pub mod step_rows;
pub mod ui_state;
pub mod undo_label;
pub mod views;
pub mod windows;

pub use actions::{
    Action, EditAction, PluginAction, ProjectAction, SettingsAction, TransportAction, UiAction,
};
pub use app::App;
pub use keymap::{Key, KeyChord, Keymap, Modifiers};
pub use ui_state::{FocusArea, UiState};

// Unit tests of the engine wiring run the audio callback, which `komponist-audio` wraps in
// `assert_no_alloc`; that needs its allocator (debug builds only).
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
