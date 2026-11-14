//! Layout metrics of the inspector (`Screens.dc.html` `sec`, `F`, `nf`, `swatchesFor`, `D`,
//! `trackSec`, `faderScale` and the inspector markup; `ks-view.js` `inspView`). Sums of spacing
//! tokens are written that way; the rest are the design's numbers.

use komponist_tokens::{size, space, typography};

/// Horizontal padding of a section (`padding:14px 12px`).
pub const PAD_X: f32 = space::SPACE_3;
/// Vertical padding of a section.
pub const PAD_Y: f32 = space::SPACE_3 + space::SPACE_0_5;
/// Gap between the parts of a section (`gap:10px`).
pub const GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
/// Gap between a name's chip and the name (`gap:8px`).
pub const NAME_GAP: f32 = space::SPACE_2;
/// The square chip of a clip or the project (`width:10, height:10, borderRadius:2`).
pub const CHIP: f32 = 10.0;
/// The bar chip of a track (`chipBar`: `width:3, height:16`).
pub const CHIP_BAR: [f32; 2] = [3.0, 16.0];
/// Gap between the field columns and rows (`grid-template-columns:1fr 1fr; gap:8px`).
pub const FIELD_GAP: f32 = space::SPACE_2;
/// Gap between a field's key and the field (`gap:4px`).
pub const FIELD_KEY_GAP: f32 = space::SPACE_1;
/// Height of a field cell: key (`type.small`), gap, field (`nf` `height:24`).
pub const FIELD_HEIGHT: f32 = typography::SMALL.line_height + FIELD_KEY_GAP + size::CONTROL;
/// Height of a colour swatch (`swatchesFor`: `height:14`).
pub const SWATCH_HEIGHT: f32 = 14.0;
/// Gap between the swatches (`repeat(14,1fr); gap:3px`).
pub const SWATCH_GAP: f32 = 3.0;
/// Distance of the current swatch's ring (`0 0 0 1px f1, 0 0 0 2px t1`).
pub const SWATCH_RING_OFFSET: f32 = 1.0;
/// Width of the dropdown keys' column (`grid-template-columns:72px minmax(0,1fr)`).
pub const DD_KEY_WIDTH: f32 = 72.0;
/// Column gap of the dropdown grid (`gap:6px 8px`).
pub const DD_COLUMN_GAP: f32 = space::SPACE_2;
/// Row gap of the dropdown grid.
pub const DD_ROW_GAP: f32 = space::SPACE_1_5;
/// Height of a device row (`D`: `height:28`).
pub const DEVICE_ROW: f32 = 28.0;
/// Gap between the device rows (`gap:2px`).
pub const DEVICE_GAP: f32 = space::SPACE_0_5;
/// Horizontal padding of a device row (`padding:'0 8px'`).
pub const DEVICE_PAD_X: f32 = space::SPACE_2;
/// Gap between the parts of a device row (`gap:8`).
pub const DEVICE_ITEM_GAP: f32 = space::SPACE_2;
/// The power square of a device row (`pw`: `width:10, height:10, borderRadius:2`).
pub const POWER: f32 = 10.0;
/// A take row: as high as a menu row (not in the design, which names the takes list only in the
/// S02 notes).
pub const TAKE_ROW: f32 = size::MENU_ROW;
/// Gap between the take rows (as the device rows).
pub const TAKE_GAP: f32 = space::SPACE_0_5;
/// Horizontal padding of a take row.
pub const TAKE_PAD_X: f32 = space::SPACE_2;
/// Height of the compact channel strip (`height:124px`; the meter's default length).
pub const CHANNEL_HEIGHT: f32 = komponist_ui::widgets::metrics::meter::LENGTH;
/// Gap between fader, meter and the values column (`gap:16px`).
pub const CHANNEL_GAP: f32 = space::SPACE_4;
/// Gap between a value's key and the value (`gap:4px`).
pub const VALUE_KEY_GAP: f32 = space::SPACE_1;
/// Gap between the pan knob and its key and value (`gap:10px`).
pub const PAN_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
/// Gap between the pan key and value (`gap:2px`).
pub const PAN_KEY_GAP: f32 = space::SPACE_0_5;
/// The M S toggles of the compact strip (`tgl(k, …, 28, 20)`).
pub const TOGGLE: [f32; 2] = [28.0, 20.0];
/// Gap between them (`gap:4px`).
pub const TOGGLE_GAP: f32 = space::SPACE_1;
