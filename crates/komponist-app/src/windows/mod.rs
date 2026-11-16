//! Windows besides the main one.
//!
//! - [`plugin_frame`]: the frame window around a plugin's own GUI or its parameter grid (S05,
//!   WP-50).
//! - [`window_list`]: the open plugin windows as the Window menu, the Dock menu (macOS) and the
//!   "⋯" menu list them (#95).

pub mod plugin_frame;
pub mod window_list;
