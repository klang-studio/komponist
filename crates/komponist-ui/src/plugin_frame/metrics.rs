//! Sizes of the plugin frame's header that the handoff gives only in the screen data
//! (`AGENTS.md` rule 2). The app's frame (`komponist-app` `windows::plugin_frame::metrics`) adds the
//! body's; the bridge's own frame window (macOS, #52) uses these.
//!
//! Sources: `Screens.dc.html` S05 (`s.ovPlugin` markup, the frame header) and `ks-view.js`
//! `pluginView`. Where a value is a sum of spacing tokens it is written that way.

use komponist_tokens::{size, space};

/// Height of the frame header. ARCHITECTURE §8.4 and the WP-50 brief fix it at 28
/// (`size.panel-header`); the S05 markup draws it 36 high (`height:36px`), see the report.
pub const HEADER: f32 = size::PANEL_HEADER;

/// Left padding of the header (`padding:0 6px 0 10px`).
pub const HEADER_PAD_LEFT: f32 = space::SPACE_2 + space::SPACE_0_5;

/// Right padding of the header (`padding:0 6px 0 10px`).
pub const HEADER_PAD_RIGHT: f32 = space::SPACE_1_5;

/// Gap between the header's items (`gap:8px`).
pub const HEADER_GAP: f32 = space::SPACE_2;

/// The bypass mark before the name (`width:12px; height:12px; border-radius:2px`; the radius is
/// `radius.s`).
pub const MARK: f32 = 12.0;

/// The separator after the track and format: `width:1px; height:16px`.
pub const SEPARATOR_HEIGHT: f32 = 16.0;

/// Extra room on both sides of the separator (`margin:0 4px`).
pub const SEPARATOR_MARGIN: f32 = space::SPACE_1;

/// Width of the preset field: the prototype's `width:150px` (S05 draws 170), so the header with
/// the design system's 24 px controls fits S05's 820 px frame.
pub const PRESET_WIDTH: f32 = 150.0;

/// Smallest body height of a resizable frame, so the header never stands alone.
pub const MIN_BODY_HEIGHT: f32 = size::CONTROL * 2.0;
