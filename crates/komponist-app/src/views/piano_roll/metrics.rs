//! Piano roll sizes and interaction numbers that the handoff gives only in the design data
//! (`Screens.dc.html` `pianoRoll()` and its markup, `ks-view.js` `prView`, `Prototyp.dc.html`
//! `prHit`, `prDown`, `prVel`, `openClip`), with their source. Values equal to a token use the
//! token. Rule 2 wants component metrics in `komponist_ui::widgets::metrics`, which WP-41 may not
//! change; the report asks to move them there if the orchestrator prefers one place.

use komponist_model::Tick;
use komponist_tokens::{size, space};

/// Toolbar row above the ruler: `height:28px`, `padding:0 8px`, `gap:8px`, 1 px `surface.0` line
/// at the bottom (inside the 28).
pub const TOOLBAR_HEIGHT: f32 = size::PANEL_HEADER;
/// Horizontal padding of the toolbar row (`padding:0 8px`).
pub const TOOLBAR_PAD_X: f32 = space::SPACE_2;
/// Gap between the toolbar's controls (`gap:8px`).
pub const TOOLBAR_GAP: f32 = space::SPACE_2;
/// The divider before the selection info: `width:1px; height:16px; margin:0 4px` in `line.1`.
pub const DIVIDER_HEIGHT: f32 = space::SPACE_4;
/// Margin left and right of the divider (`margin:0 4px`).
pub const DIVIDER_MARGIN: f32 = space::SPACE_1;
/// Gap between the zoom buttons at the right end (`gap:2px`).
pub const ZOOM_GAP: f32 = space::SPACE_0_5;

/// The "Vel" field: `fs('Vel','66',84)`.
pub const FIELD_VELOCITY_WIDTH: f32 = 84.0;
/// The "Länge" field: `fs('Länge','1/8',96)`.
pub const FIELD_LENGTH_WIDTH: f32 = 96.0;
/// The "Note" field: `fs('Note','A4',84)`.
pub const FIELD_NOTE_WIDTH: f32 = 84.0;

/// The editor ruler: `height:18px` with a 1 px `surface.0` line at the bottom (inside the 18).
/// Its left corner over the keyboard is `surface.2`.
pub const RULER_HEIGHT: f32 = 18.0;

/// Black keys are shorter than the white ones by `marginRight: b?16:0`.
pub const BLACK_KEY_INSET: f32 = space::SPACE_4;
/// Right padding of the key names on the C keys (`paddingRight:5`).
pub const KEY_LABEL_PAD: f32 = 5.0;

/// Highest velocity stem in the lane: `h = n.v/127*52`.
pub const VELOCITY_MAX_HEIGHT: f32 = 52.0;
/// Stems stand this far above the lane's bottom (`bottom:4`).
pub const VELOCITY_BOTTOM: f32 = space::SPACE_1;
/// Width of a velocity stem (`width:2`).
pub const VELOCITY_STEM_WIDTH: f32 = space::SPACE_0_5;
/// Height of the head on top of a stem (`height:3`).
pub const VELOCITY_HEAD_HEIGHT: f32 = 3.0;
/// The head is the note width minus this (`n.l*cw-3`) …
pub const VELOCITY_HEAD_SHORTEN: f32 = 3.0;
/// … at most this wide (`Math.min(…,14)`) …
pub const VELOCITY_HEAD_MAX_WIDTH: f32 = 14.0;
/// … and at least this wide (`Math.max(3,…)` in `prView`).
pub const VELOCITY_HEAD_MIN_WIDTH: f32 = 3.0;
/// Padding of the lane's label column ("VEL" at the top, "127" at the bottom, `padding:6px`).
pub const VELOCITY_LABEL_PAD: f32 = space::SPACE_1_5;
/// A press in the lane takes the note whose start is nearest, if it is at most this far away
/// (`prVel`: `bd*g.cw>10`).
pub const VELOCITY_PICK_DISTANCE: f32 = 10.0;

/// The right end of a note is its resize edge over this width (`prHit`: `x>(n.s+n.l)*cw-6`) …
pub const EDGE_GRAB: f32 = space::SPACE_1_5;
/// … if the note is wider than this (`n.l*cw>10`).
pub const EDGE_MIN_WIDTH: f32 = 10.0;

/// Ctrl-drag ↕ on a note: one velocity step per 1.5 px (`(y0-ev.clientY)/m.S/1.5`).
pub const VELOCITY_DRAG_PX_PER_STEP: f32 = 1.5;
/// Shortest length an edge drag without snap leaves (`Math.max(.125,l)` in 1/16 steps).
pub const FREE_MIN_LENGTH: Tick = Tick(30);

/// Velocity of notes set with the pencil or a double-click (`prCreate`: `v:100`).
pub const NEW_NOTE_VELOCITY: u8 = 100;
/// Velocity of a key or note played while clicking (`previewKey`: `voice(t,p,100)`).
pub const AUDITION_VELOCITY: u8 = 100;

/// Highest key the top edge scrolls to (`openClip`, `onWheel`: `Math.min(120, …)`).
pub const TOP_KEY_MAX: u8 = 120;
/// Opening a clip shows its highest note this many keys below the top edge (`hi+3`) …
pub const FIT_ABOVE: i32 = 3;
/// … or its lowest note this many rows above the bottom edge (`lo+nKeys-2`).
pub const FIT_BELOW: i32 = 2;
/// Opening an empty clip centres this range (`hi = 72`, `lo = 60` without notes).
pub const FIT_EMPTY: (u8, u8) = (60, 72);
/// Keys scrolled per wheel notch (`onWheel`: `m.prTop ± 2`).
pub const WHEEL_KEYS: i32 = 2;

/// Most notes that get their own AccessKit node in one frame. Not a design value: a screen
/// reader cannot use thousands of nodes, and each costs a widget; the first ones in time order
/// get one.
pub const MAX_NOTE_NODES: usize = 512;

/// Minimum distance in px between two drawn grid lines. Where the chosen "Raster" would crowd, the
/// grid is drawn one musical level coarser ([`komponist_ui::timeline::display_step`]). `space.2`
/// (8 px): with 1 px lines that leaves 7 px of lane between lines (6 px was still read as a hatch
/// pattern on the 1x snapshots), and it is the threshold the ruler labels already use (not a
/// design value).
pub const GRID_MIN_SPACING: f32 = space::SPACE_2;
