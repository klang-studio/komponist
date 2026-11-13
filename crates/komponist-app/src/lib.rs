//! The komponist application.
//!
//! The app shell with layout 2a and dock, views (arrangement, piano roll, step sequencer, mixer,
//! inspector, browser, devices), dialogs, central action dispatch and the keymap. The only crate that
//! knows both egui and the engine. The library exists so that integration and acceptance tests can
//! drive the app headless; the binary `komponist` starts it.
//!
//! Scaffold only: the contracts arrive with WP-01, the shell with WP-30.
