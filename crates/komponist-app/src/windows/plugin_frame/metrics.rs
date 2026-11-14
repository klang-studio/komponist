//! Sizes of the plugin frame that the handoff gives only in the screen data (`CLAUDE.md` rule 2).
//!
//! Sources: `Screens.dc.html` S05 (`s.ovPlugin` markup, the frame header), `ks-view.js`
//! `pluginView` (window sizes, parameter grid) and `Prototyp.dc.html` (the parameter grid
//! markup). Where a value is a sum of spacing tokens it is written that way.

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

/// Plugin area of a GUI frame before the plugin reports its size: S05 "Fenstergröße folgt dem
/// Plugin · 820 × 524".
pub const GUI_AREA: [f32; 2] = [820.0, 524.0];

/// Body of the parameter view when it opens: `pluginView` `width:640, height:340` minus the
/// prototype's 36 px header.
pub const PARAMS_AREA: [f32; 2] = [640.0, 304.0];

/// Smallest body height of a resizable frame, so the header never stands alone.
pub const MIN_BODY_HEIGHT: f32 = size::CONTROL * 2.0;

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
