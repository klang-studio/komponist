//! Sizes of the plugin frame that the handoff gives only in the screen data (`AGENTS.md` rule 2).
//!
//! Sources: `Screens.dc.html` S05 (`s.ovPlugin` markup, the frame header), `ks-view.js`
//! `pluginView` (window sizes, parameter grid) and `Prototyp.dc.html` (the parameter grid
//! markup). Where a value is a sum of spacing tokens it is written that way.

// The header's measures, shared with the bridge's own frame window (macOS, #52).
use komponist_tokens::space;
pub use komponist_ui::plugin_frame::metrics::*;

/// Plugin area of a GUI frame before the plugin reports its size: S05 "Fenstergröße folgt dem
/// Plugin · 820 × 524".
pub const GUI_AREA: [f32; 2] = [820.0, 524.0];

/// Body of the parameter view when it opens: `pluginView` `width:640, height:340` minus the
/// prototype's 36 px header.
pub const PARAMS_AREA: [f32; 2] = [640.0, 304.0];

/// Columns of the parameter grid (`grid-template-columns:repeat(4,1fr)`).
pub const PARAM_COLUMNS: usize = 4;

/// Padding around the parameter grid (`padding:24px`).
pub const PARAM_PADDING: f32 = space::SPACE_6;

/// Gap between grid rows (`gap:20px 12px`).
pub const PARAM_ROW_GAP: f32 = space::SPACE_4 + space::SPACE_1;

/// Gap between grid columns (`gap:20px 12px`).
pub const PARAM_COLUMN_GAP: f32 = space::SPACE_3;

/// Gap between knob, name and value of one parameter (`gap:6px`).
pub const PARAM_ITEM_GAP: f32 = space::SPACE_1_5;

/// Width of the state message (loading, crashed) in the body: the device card's default width
/// (`card()`: `w=o.w||250`).
pub const MESSAGE_WIDTH: f32 = 250.0;

/// Padding of the state message (`msgS`: `padding:12`).
pub const MESSAGE_PADDING: f32 = space::SPACE_3;

/// Gap between the lines of the state message (`msgS`: `gap:8`).
pub const MESSAGE_GAP: f32 = space::SPACE_2;

/// The crashed card's red top edge (`card()`: `inset 0 2px 0` `C.clip`).
pub const CRASH_EDGE: f32 = 2.0 * komponist_tokens::line::WIDTH;

/// The loading bar (`progS`: `height:4, borderRadius:2`).
pub const PROGRESS_HEIGHT: f32 = space::SPACE_1;

/// Filled part of the loading bar in the design (`progFill`: `width:'40%'`); there is no real
/// progress to show.
pub const PROGRESS_FILL: f32 = 0.4;

/// Padding of the GUI placeholder box (`padding:12px 16px`).
pub const PLACEHOLDER_PADDING: [f32; 2] = [space::SPACE_4, space::SPACE_3];
