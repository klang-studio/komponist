//! Device chain sizes. The frame, the grid and the chain come from `komponist_tokens::device`
//! (`design/device-tokens.json`); this module holds what the handoff gives only in its generator
//! (`geraete_page_generator.py` `frame()`, `collapsed()`, `foreign()`) or in the older chain
//! design (`Screens.dc.html` `devicesChain()`), with the source. Values equal to a token use it.

use komponist_tokens::{device, size, space};

/// The chain head's height (`device.chain.header_h`).
pub const HEADER_HEIGHT: f32 = device::chain::HEADER_H;
/// Horizontal padding of the chain head (`devicesChain()` `padding:0 8px`).
pub const HEADER_PAD_X: f32 = space::SPACE_2;
/// Gap between colour mark, track name and the chain (`devicesChain()` `gap:10px`).
pub const HEADER_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
/// The track's colour mark (`devicesChain()` `width:8px; height:8px; border-radius:2px`).
pub const MARK: f32 = space::SPACE_2;
/// Padding of a name in the chain head (`surface.3` chip behind a visible device; not in the
/// design, the chip needs some air): horizontal …
pub const CHIP_PAD_X: f32 = space::SPACE_1_5;
/// … and the gap around the „›“ between two names.
pub const CHAIN_SEPARATOR_GAP: f32 = space::SPACE_1_5;
/// The chain head's name separator (HANDOFF §9 `hexa › eq › presse`).
pub const CHAIN_SEPARATOR: &str = "\u{203a}";

/// Padding of the chain (`device.chain.padding`).
pub const BODY_PAD: f32 = device::chain::PADDING;
/// Gap between the frames (`device.chain.gap`).
pub const CARD_GAP: f32 = device::chain::GAP;
/// The drop slot at the end (`device.chain.drop_slot_w`).
pub const DROP_AREA_WIDTH: f32 = device::chain::DROP_SLOT_W;
/// The scrollbars' thickness (`device.chain.scrollbar_h`).
pub const SCROLLBAR: f32 = device::chain::SCROLLBAR_H;
/// The height below which the chain scrolls vertically: head, padding above and below, frame
/// (`28 + 2·10 + 244 = 292`; with the dock's tab strip `size.tab` 28 that is the 320 px of the
/// default dock, HANDOFF §3; owner 2026-11-14: below it the area scrolls).
pub const CHAIN_MIN_HEIGHT: f32 = HEADER_HEIGHT + 2.0 * BODY_PAD + device::frame::TOTAL_H;

/// Parameter columns of the auto-layout (`foreign()` `cols=4`; `params::AUTO_COLUMNS`).
pub const COLUMNS: usize = 4;
/// Columns of a frame that shows a message (loading, crashed, missing): `foreign()`'s 252 px.
pub const MESSAGE_COLUMNS: u8 = 4;

/// Left padding of the head (`device.frame.head_padding` `[0,4,0,8]`).
pub const HEAD_PAD_LEFT: f32 = device::frame::HEAD_PADDING[3];
/// Right padding of the head.
pub const HEAD_PAD_RIGHT: f32 = device::frame::HEAD_PADDING[1];
/// Gap between the head's parts (`device.frame.head_gap`).
pub const HEAD_GAP: f32 = device::frame::HEAD_GAP;
/// The power square's hit area (the square is 10 px, too small to hit; not in the design).
pub const POWER_HIT: f32 = size::CONTROL_COMPACT;
/// The name keeps at least this much room in the head (not in the design: about what `foreign()`
/// leaves for a name next to tag and „Öffnen“); a narrow frame widens.
pub const NAME_MIN_WIDTH: f32 = 64.0;
/// Format tag padding (`foreign()` `padding:1px 4px`): horizontal …
pub const TAG_PAD_X: f32 = space::SPACE_1;
/// … and vertical.
pub const TAG_PAD_Y: f32 = 1.0;
/// The preset button's text padding (`frame()` `padding:0 6px`).
pub const PRESET_PAD_X: f32 = space::SPACE_1_5;
/// The page switch's frame padding (`frame()` `padding:1px`) …
pub const PAGES_PAD: f32 = 1.0;
/// … and the gap between its segments (`gap:1px`).
pub const PAGES_GAP: f32 = 1.0;
/// Radius of the page switch's frame and of the ⋯ and preset buttons (`border-radius:3px`).
pub const HEAD_RADIUS: f32 = komponist_tokens::radius::M;
/// Radius of a page segment (`border-radius:2px`).
pub const PAGE_SEGMENT_RADIUS: f32 = komponist_tokens::radius::S;
/// Body opacity of a bypassed device (`filter:saturate(0.15);opacity:0.5`) …
pub const BYPASS_OPACITY: f32 = 0.5;
/// … and its saturation.
pub const BYPASS_SATURATION: f32 = 0.15;
/// The red edge of a crashed frame (`boxShadow:'inset 0 2px 0 '+C.clip`).
pub const CRASH_EDGE: f32 = space::SPACE_0_5;
/// A collapsed frame: padding above the power square (`padding-top:9px`) …
pub const COLLAPSED_TOP: f32 = 9.0;
/// … and the gap to the vertical name (`gap:10px`).
pub const COLLAPSED_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;

/// Padding of the message block ("Lädt …", "Abgestürzt": `msgS` `padding:12`) …
pub const MESSAGE_PAD: f32 = space::SPACE_3;
/// … and the gap between its parts (`gap:8`).
pub const MESSAGE_GAP: f32 = space::SPACE_2;
/// The progress bar of a loading card (`progS`: `height:4`).
pub const PROGRESS_HEIGHT: f32 = space::SPACE_1;
/// The progress bar's fill (`progFill`: `width:'40%'`); loading reports no progress, so the
/// design's still picture is kept (animations are off).
pub const PROGRESS_FILL: f32 = 0.4;

/// A knob cell: space above the knob (`W_knob` `padding-top:2px`), the gap between knob, name and
/// value (`gap:2px`).
pub const CELL_PAD_TOP: f32 = 2.0;
/// The gap between a knob, its name and its value.
pub const CELL_GAP: f32 = 2.0;

/// Dash and gap of the drop slot's 1 px dashed border (Chromium draws CSS `dashed` at 1 px as 3
/// on, 3 off).
pub const DASH: [f32; 2] = [3.0, 3.0];
/// Padding of the drop slot's hint (`dvDrop` `padding:12px`).
pub const DROP_AREA_PAD: f32 = space::SPACE_3;
/// The insert line while dragging (`dvDrop`: `width:2`, centred in the gap).
pub const INSERT_LINE: f32 = space::SPACE_0_5;

/// The add menu behind the drop slot (owner request 2026-11-15, not in the design): its width
/// (the slot is narrower than a device name plus format tag needs) …
pub const ADD_MENU_WIDTH: f32 = 240.0;
/// … its highest extent before the list scrolls …
pub const ADD_MENU_MAX_HEIGHT: f32 = 320.0;
/// … the space it keeps to the window edge …
pub const ADD_MENU_MARGIN: f32 = space::SPACE_2;
/// … the gap between a button and the menu that opens below it („+ Gerät“, „+ Effekt“, the mixer's „+“) …
pub const ADD_MENU_ANCHOR_GAP: f32 = space::SPACE_1;
/// … and the gap between its search field and the list.
pub const ADD_MENU_GAP: f32 = space::SPACE_1;
/// The „+“ above the drop slot's hint: its size …
pub const ADD_GLYPH: f32 = 12.0;
/// … stroke …
pub const ADD_GLYPH_STROKE: f32 = 1.5;
/// … and the gap to the hint.
pub const ADD_GLYPH_GAP: f32 = space::SPACE_2;

/// The sidechain chip in a device head (WP-104, routing proposal 08-sidechain-kopf (internal notes),
/// variant C, not in the handoff): the source dot's diameter …
pub const SC_DOT: f32 = 6.0;
/// … and the caret (like the preset button's `border-left/right:3px`, `border-top:4px`).
pub const SC_CARET: [f32; 2] = [6.0, 4.0];
