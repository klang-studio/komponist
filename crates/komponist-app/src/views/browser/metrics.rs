//! Browser sizes that the handoff gives only in the design data (`Screens.dc.html` `B()`, `segs`,
//! `br()` and the browser markup; `ks-view.js` `browserView`), with their source. Values equal to
//! a token use the token.

use komponist_tokens::{size, space};

/// Padding around the search field and the segments (`padding:8px`) and the gap between them
/// (`gap:8px`).
pub const TOP_PAD: f32 = space::SPACE_2;
/// Height of the search field (`searchS`: `height:24`).
pub const SEARCH_HEIGHT: f32 = size::CONTROL;
/// Horizontal padding inside the search field (`padding:'0 8px'`).
pub const SEARCH_PAD_X: f32 = space::SPACE_2;
/// Horizontal padding of the row list (`padding:0 4px`).
pub const LIST_PAD_X: f32 = space::SPACE_1;
/// A plugin, preset or folder row (`B()`: `height:24`).
pub const ROW_HEIGHT: f32 = size::CONTROL;
/// A section row ("FAVORITEN", `B()` with `sec`: `height:26`, text at the bottom).
pub const SECTION_HEIGHT: f32 = 26.0;
/// Space below a section's text (`padding:'0 8px 4px'`).
pub const SECTION_PAD_BOTTOM: f32 = space::SPACE_1;
/// Horizontal padding of a row (`padding:'0 8px 0 '+(8+lvl*12)+'px'`).
pub const ROW_PAD_X: f32 = space::SPACE_2;
/// Indent per level inside a folder (`lvl*12`).
pub const LEVEL_INDENT: f32 = space::SPACE_3;
/// Gap between icon, name and tag (`gap:8`).
pub const ROW_GAP: f32 = space::SPACE_2;
/// The icon column (★ ☆ ▸ ·): `iconS` `width:10`, glyphs at `fontSize:10`.
pub const ICON: f32 = 10.0;
/// Horizontal padding of a tag ("CLAP", counts): `padding:'1px 4px'`.
pub const TAG_PAD_X: f32 = space::SPACE_1;
/// Vertical padding of a tag (`padding:'1px 4px'`).
pub const TAG_PAD_Y: f32 = 1.0;
/// Footer padding (`padding:10px 12px`): horizontal …
pub const FOOT_PAD_X: f32 = space::SPACE_3;
/// … and vertical.
pub const FOOT_PAD_Y: f32 = space::SPACE_2 + space::SPACE_0_5;
/// Offset of the drag ghost from the pointer (not in the design: the browser's own drag image
/// sits at the pointer; a small offset keeps the drop position visible).
pub const GHOST_OFFSET: f32 = space::SPACE_3;
