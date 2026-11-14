//! Device chain sizes that the handoff gives only in the design data (`Screens.dc.html` `card()`,
//! `devicesChain()`, the `isDevices` markup and `dvDrop`; `ks-view.js` `devView`), with their
//! source. Values equal to a token use the token.

use komponist_tokens::{size, space};

/// The chain's header row (`height:28px; padding:0 8px; gap:10px`, 1 px `surface.0` line at the
/// bottom inside).
pub const HEADER_HEIGHT: f32 = size::PANEL_HEADER;
/// Horizontal padding of the header row (`padding:0 8px`).
pub const HEADER_PAD_X: f32 = space::SPACE_2;
/// Gap between colour mark, track name and info (`gap:10px`).
pub const HEADER_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
/// The track's colour mark (`width:8px; height:8px; border-radius:2px`).
pub const MARK: f32 = space::SPACE_2;

/// Padding of the card row (`padding:12px`).
pub const BODY_PAD: f32 = space::SPACE_3;
/// Gap between the cards (`gap:8px`).
pub const CARD_GAP: f32 = space::SPACE_2;

/// An instrument card (`card({…inst:1…, w:280})`; `devView`: `d.inst?280`).
pub const CARD_WIDTH_INSTRUMENT: f32 = 280.0;
/// An effect card (`card()`: `w=o.w||250`).
pub const CARD_WIDTH_EFFECT: f32 = 250.0;
/// A card with at most [`NARROW_PARAMS`] parameters, and a loading card (`w:200`: TAL-Chorus-LX,
/// Dragonfly Hall).
pub const CARD_WIDTH_NARROW: f32 = 200.0;
/// A crashed or missing card (`w:220`: Airwindows Cons.), room for its message.
pub const CARD_WIDTH_MESSAGE: f32 = 220.0;
/// Up to this many parameters make a narrow card with one column each (`cols:3`).
pub const NARROW_PARAMS: usize = 3;
/// Parameter columns of a full card (`repeat(${o.cols||4},1fr)`).
pub const COLUMNS: usize = 4;
/// Parameters a card shows (`devView`: `d.params.slice(0,8)`); the rest is in the plugin window.
pub const MAX_PARAMS: usize = 8;

/// The card's head (`headS`: `height:28`, `padding:'0 4px 0 8px'`, `gap:8`, 1 px `surface.1` line
/// at the bottom).
pub const HEAD_HEIGHT: f32 = size::PANEL_HEADER;
/// Left padding of the head.
pub const HEAD_PAD_LEFT: f32 = space::SPACE_2;
/// Right padding of the head.
pub const HEAD_PAD_RIGHT: f32 = space::SPACE_1;
/// Gap between the head's parts.
pub const HEAD_GAP: f32 = space::SPACE_2;
/// Format tag padding (`padding:1px 4px`): horizontal …
pub const TAG_PAD_X: f32 = space::SPACE_1;
/// … and vertical.
pub const TAG_PAD_Y: f32 = 1.0;
/// The red edge of a crashed card (`boxShadow:'inset 0 2px 0 '+C.clip`).
pub const CRASH_EDGE: f32 = space::SPACE_0_5;

/// Padding of the message block ("Lädt …", "Abgestürzt": `msgS` `padding:12`) …
pub const MESSAGE_PAD: f32 = space::SPACE_3;
/// … and the gap between its parts (`gap:8`).
pub const MESSAGE_GAP: f32 = space::SPACE_2;
/// The progress bar of a loading card (`progS`: `height:4`).
pub const PROGRESS_HEIGHT: f32 = space::SPACE_1;
/// The progress bar's fill (`progFill`: `width:'40%'`); loading reports no progress, so the
/// design's still picture is kept (animations are off).
pub const PROGRESS_FILL: f32 = 0.4;

/// Parameter grid padding (`paramsS`: `padding:'12px 8px'`): horizontal …
pub const GRID_PAD_X: f32 = space::SPACE_2;
/// … and vertical.
pub const GRID_PAD_Y: f32 = space::SPACE_3;
/// Gap between grid rows (`gap:'14px 6px'`) …
pub const GRID_ROW_GAP: f32 = space::SPACE_3 + space::SPACE_0_5;
/// … and columns.
pub const GRID_COLUMN_GAP: f32 = space::SPACE_1_5;
/// Gap between knob, name and value of a parameter (`gap:4px`).
pub const PARAM_GAP: f32 = space::SPACE_1;

/// The drop area after the last card (`width:150px`, `border:1px dashed`, `padding:12px`).
pub const DROP_AREA_WIDTH: f32 = 150.0;
/// Its padding.
pub const DROP_AREA_PAD: f32 = space::SPACE_3;
/// Dash and gap of its 1 px dashed border (Chromium draws CSS `dashed` at 1 px as 3 on, 3 off).
pub const DASH: [f32; 2] = [3.0, 3.0];
/// The insert line while dragging (`dvDrop`: `width:2`, `top:12`, `bottom:12`, centred in the gap).
pub const INSERT_LINE: f32 = space::SPACE_0_5;
