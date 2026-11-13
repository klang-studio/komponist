//! Step sequencer sizes and interaction numbers that the handoff gives only in the design data
//! (`Screens.dc.html` dock markup for `isStep` and `stepSeq()`, `ks-view.js` `stepView`,
//! `Prototyp.dc.html` `stepDown`, `stepLane`), with their source. Values equal to a token use
//! the token. Rule 2 wants component metrics in `komponist_ui::widgets::metrics`, which WP-42
//! may not change; the report asks to move them there if the orchestrator prefers one place.

use komponist_tokens::{size, space};

/// Toolbar row: `height:28px; padding:0 8px; gap:8px`, then a 1 px `surface.0` line below it
/// (`border-bottom`, outside the 28 as in the reference: 28 + 1).
pub const TOOLBAR_HEIGHT: f32 = size::PANEL_HEADER;
/// Horizontal padding of the toolbar (`padding:0 8px`).
pub const TOOLBAR_PAD_X: f32 = space::SPACE_2;
/// Gap between the toolbar's controls (`gap:8px`).
pub const TOOLBAR_GAP: f32 = space::SPACE_2;
/// The divider before the step info: `width:1px; height:16px; margin:0 4px` in `line.1`.
pub const DIVIDER_HEIGHT: f32 = space::SPACE_4;
/// Margin left and right of the divider (`margin:0 4px`).
pub const DIVIDER_MARGIN: f32 = space::SPACE_1;

/// "Länge" field: `width:108px`.
pub const FIELD_LENGTH_WIDTH: f32 = 108.0;
/// "Swing" field: `width:92px`.
pub const FIELD_SWING_WIDTH: f32 = 92.0;
/// "Vel" field: `width:84px`.
pub const FIELD_VELOCITY_WIDTH: f32 = 84.0;
/// "Wahr." field: `width:92px`.
pub const FIELD_PROBABILITY_WIDTH: f32 = 92.0;
/// "Ratch." field: `width:84px`.
pub const FIELD_RATCHET_WIDTH: f32 = 84.0;
/// Gap between the key and the value of the "Auflösung" dropdown (`gap:10px`); the dropdown's
/// own left padding (`space.2`) is part of it.
pub const RESOLUTION_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;

/// Row of step numbers: `height:18px` plus a 1 px `surface.0` line below.
pub const NUMBERS_HEIGHT: f32 = 18.0;

/// Row header: `width:180px` including its 1 px `surface.0` right edge.
pub const HEADER_WIDTH: f32 = 180.0;
/// Header padding left (`padding:0 6px 0 8px`).
pub const HEADER_PAD_LEFT: f32 = space::SPACE_2;
/// Header padding right.
pub const HEADER_PAD_RIGHT: f32 = space::SPACE_1_5;
/// Gap between the header's parts (`gap:6`).
pub const HEADER_GAP: f32 = space::SPACE_1_5;
/// Colour chip of a row: `chip` `width:3, height:14`.
pub const CHIP: [f32; 2] = [3.0, 14.0];
/// Row toggles M S: `tgl('M',0,16,14)`.
pub const ROW_TOGGLE: [f32; 2] = [16.0, 14.0];

/// Height of a step row: `height:26px`, plus a 1 px `surface.0` line below.
pub const ROW_HEIGHT: f32 = 26.0;
/// Gap between step cells (`gap:4px`).
pub const CELL_GAP: f32 = space::SPACE_1;
/// Horizontal padding of the cells (`padding:2px 8px`); also of the number row and the lane.
pub const CELLS_PAD_X: f32 = space::SPACE_2;
/// Vertical padding of the cells (`padding:2px 8px`).
pub const CELLS_PAD_Y: f32 = space::SPACE_0_5;
/// Gap between the parts of a ratchet step (`gap:2`).
pub const RATCHET_GAP: f32 = space::SPACE_0_5;
/// Corner radius of the parts of a ratchet step (`borderRadius:1`).
pub const RATCHET_RADIUS: f32 = 1.0;
/// The probability in a step sits this far from its right edge (`right:4`) …
pub const PROBABILITY_RIGHT: f32 = space::SPACE_1;
/// … and this far from its bottom edge (`bottom:1`).
pub const PROBABILITY_BOTTOM: f32 = 1.0;

/// Padding of the lane's left column (`padding:8px`).
pub const LANE_LABEL_PAD: f32 = space::SPACE_2;
/// Gap between the lane tabs and the row name below them (`gap:6px`).
pub const LANE_LABEL_GAP: f32 = space::SPACE_1_5;
/// Padding above the lane bars (`padding:6px 8px 4px`).
pub const LANE_PAD_TOP: f32 = space::SPACE_1_5;
/// Padding below the lane bars.
pub const LANE_PAD_BOTTOM: f32 = space::SPACE_1;
/// The base line under each lane bar: `borderBottom:'2px solid '+C.f2`.
pub const LANE_BASE_LINE: f32 = space::SPACE_0_5;
/// The lane always keeps the height of one control, so its tabs stay reachable when the rows
/// fill the dock (not a design value: the screens show the dock at 380, where the lane gets 88;
/// at the default dock height of 320 eight rows leave it 29). More rows than fit scroll.
pub const LANE_MIN_HEIGHT: f32 = size::CONTROL;

/// A press on a set step becomes a velocity drag after this much vertical movement
/// (`stepDown`: `Math.abs(dy)>3`); the same for the probability of Alt-drag.
pub const DRAG_THRESHOLD: f32 = 3.0;
/// Velocity of an auditioned row (`onHdr`: `voice(t,DRUM_P[r],100)`).
pub const AUDITION_VELOCITY: u8 = 100;
