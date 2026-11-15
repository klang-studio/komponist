//! Sizes of the arrangement that the handoff gives only in the screen data (`Screens.dc.html`
//! `arrangement()` and its markup, `ks-view.js` `arrView`, `clipView`, `Prototyp.dc.html`), with
//! their source. Sizes that are a spacing token, or a sum of them, are written that way.
//!
//! Rule 2 wants these in `komponist_ui::widgets::metrics`; WP-40 may not change that crate, so they
//! live here until the orchestrator moves them (contract change request in the WP-40 report).

use komponist_tokens::{size, space};

/// The ruler row: "6 Spuren" box left of the ruler (`width:224px; padding:0 10px`).
pub const COUNT_PADDING_X: f32 = space::SPACE_2 + space::SPACE_0_5;

/// Gap between the tools of the panel header (`gap:8px`).
pub const TOOL_GAP: f32 = space::SPACE_2;
/// Separator between the info text and the zoom buttons (`width:1px; height:16px`).
pub const TOOL_SEPARATOR_HEIGHT: f32 = 16.0;
/// Gap between the zoom buttons (`gap:2px`).
pub const ZOOM_BUTTON_GAP: f32 = space::SPACE_0_5;

/// "Neue Spur" below the tracks: padding (`padding:16px 14px`), horizontal and vertical.
pub const NEW_TRACK_PADDING: [f32; 2] = [space::SPACE_3 + space::SPACE_0_5, space::SPACE_4];
/// "Neue Spur": gap between title and hint (`gap:4px`).
pub const NEW_TRACK_GAP: f32 = space::SPACE_1;
/// Room kept below the last track so "Neue Spur" stays reachable when the tracks fill the view
/// (the screens show it in the free space below the tracks; not a design value).
pub const NEW_TRACK_MIN_HEIGHT: f32 = size::track::DEFAULT;

/// S10 empty project hint: width of the text block (`width:420px`).
pub const EMPTY_HINT_WIDTH: f32 = 420.0;
/// S10: gap between title, text and buttons (`gap:12px`).
pub const EMPTY_HINT_GAP: f32 = space::SPACE_3;
/// S10: extra space above the buttons (`margin-top:4px`).
pub const EMPTY_BUTTONS_TOP: f32 = space::SPACE_1;
/// S10: gap between the two buttons (`gap:8px`).
pub const EMPTY_BUTTON_GAP: f32 = space::SPACE_2;
/// S10: button height (`height:28px`).
pub const EMPTY_BUTTON_HEIGHT: f32 = 28.0;
/// S10: button padding (`padding:0 12px`).
pub const EMPTY_BUTTON_PADDING_X: f32 = space::SPACE_3;
/// S10: gap between "MIDI-Spur hinzufügen" and its shortcut (`gap:10px`).
pub const EMPTY_SHORTCUT_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;

/// Clip: width of the highlighted right edge of a selected clip (`clipView` `edge`: `width:6`).
pub const CLIP_EDGE_WIDTH: f32 = 6.0;
/// Clip: the length is dragged within this distance of the right end (`clipDown`:
/// `x > el.offsetWidth - 8`).
pub const CLIP_EDGE_GRAB: f32 = 8.0;

/// Track header: the height is dragged within this distance of the bottom edge (an interaction
/// size, not a design value; the edge itself is the 1 px separator).
pub const HEIGHT_GRAB: f32 = 3.0;
/// Track reorder: the insert line (`ix.header` "Einfügelinie 2 px akzent").
pub const INSERT_LINE: f32 = 2.0;

/// Minimum distance in px between two drawn grid lines. Where the chosen "Raster" would crowd, the
/// grid is drawn one musical level coarser ([`komponist_ui::timeline::display_step`]). `space.2`
/// (8 px): with 1 px lines that leaves 7 px of lane between lines (6 px was still read as a hatch
/// pattern on the 1x snapshots), and it is the threshold the ruler labels already use (not a
/// design value).
pub const GRID_MIN_SPACING: f32 = space::SPACE_2;
