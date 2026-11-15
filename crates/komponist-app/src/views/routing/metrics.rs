//! Metrics of the routing UI. Sources: the routing proposal's mockups (internal notes), numbers
//! given there; sums of spacing tokens are written that way (rule 2).

use komponist_tokens::{size, space};

/// The send row's height, as an insert slot's (`02-mixer-voll`: 20 px rows with 2 px gaps).
pub const ROW_HEIGHT: f32 = size::CONTROL_COMPACT;
/// Gap between send rows (`03-send-varianten` B: 2 px).
pub const ROW_GAP: f32 = space::SPACE_0_5;
/// Diameter of the bus colour dot in a row (`02-mixer-voll`).
pub const DOT: f32 = 6.0;
/// Left padding of a row's content (as an insert slot's text).
pub const ROW_PAD_X: f32 = space::SPACE_1_5;
/// Gap between the dot and the name.
pub const DOT_GAP: f32 = space::SPACE_1;
/// A wide row's „pre“ / „post“ chip: width and gap (`04-mixer-dock`, `07-inspektor` A).
pub const TAP_CHIP_WIDTH: f32 = 34.0;
/// The „×“ button of a wide row (`04-mixer-dock`).
pub const REMOVE_WIDTH: f32 = 18.0;
/// Gap between the parts of a wide row.
pub const PART_GAP: f32 = space::SPACE_1;
/// The send level range the row's fill shows: −60 dB (empty) to +6 dB (full). Not a design
/// value; the row has no scale, only the fill, so the lower end is where a send is „nearly off“.
pub const FILL_RANGE_DB: [f32; 2] = [-60.0, 6.0];
/// Level change of one arrow key press on a focused row in dB (Shift: a tenth).
pub const KEY_STEP_DB: f32 = 0.5;
/// The dock strip's routing chip (`04-mixer-dock`: 22 × 16 px).
pub const CHIP: [f32; 2] = [22.0, 16.0];
/// Diameter of a dot in the chip (`04-mixer-dock`).
pub const CHIP_DOT: f32 = 5.0;
/// Width of the routing popover in the dock (`04-mixer-dock`: about 240 px).
pub const POPOVER_WIDTH: f32 = 240.0;
/// Most send rows a strip shows before the list scrolls (`03-send-varianten` B: from 4 sends on).
pub const MAX_STRIP_SEND_ROWS: usize = 4;
