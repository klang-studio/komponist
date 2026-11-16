//! The frame header: its drawing code lives in `komponist-ui` (`plugin_frame::header`), its model
//! and events in `komponist-ipc` (`frame`), because on macOS the bridge draws the same header in
//! its own frame window (#52). Re-exported here for the app's frame and its tests.

pub use komponist_ui::plugin_frame::header::*;
