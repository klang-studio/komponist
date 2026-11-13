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
//! The shell, dispatch and engine sync arrive with WP-30, the views with WP-40 … WP-44.

// Contract stage (WP-01): bodies are `todo!()` until WP-30 implements them, so parameters are
// unused for now. Remove this allow when the implementation lands.
#![allow(unused_variables)]

pub mod actions;
pub mod keymap;
pub mod ui_state;

pub use actions::{
    Action, EditAction, PluginAction, ProjectAction, SettingsAction, TransportAction, UiAction,
};
pub use keymap::{Key, KeyChord, Keymap, Modifiers};
pub use ui_state::{FocusArea, UiState};
