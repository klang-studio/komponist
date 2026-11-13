//! Sizes of the app shell that the handoff defines only in the screen data (`Screens.dc.html`
//! `tp`, `tab`, `dock`, the shell markup; `ks-view.js` `geo`; `Prototyp.dc.html`), with their
//! source. Rule 2: no inline literals in the shell code; sizes with a token use the token.
//!
//! These belong next to `komponist_ui::widgets::metrics`, which WP-30 may not change; the report
//! asks to move them there if the orchestrator prefers one place.

use komponist_tokens::{size, space};

/// Transport bar (`Screens.dc.html` shell markup and `tp`).
pub mod transport {
    use super::{size, space};

    /// Width of the wordmark box ("komponist" + project name + dirty dot): `width:236px`.
    pub const WORDMARK_WIDTH: f32 = 236.0;
    /// Gap between wordmark, project name and dirty dot: `gap:10px`.
    pub const WORDMARK_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// The dirty dot: `dirtyS` `width:5, height:5`.
    pub const DIRTY_DOT: f32 = 5.0;
    /// Play, stop and record buttons: `playS` / `recS` `width:30, height:28`.
    pub const BUTTON: [f32; 2] = [30.0, 28.0];
    /// The play triangle: `triS` `borderLeft:9`, `borderTop/Bottom:6` → 9 × 12.
    pub const PLAY_TRIANGLE: [f32; 2] = [9.0, 12.0];
    /// The play triangle sits right of centre: `marginLeft:2`.
    pub const PLAY_TRIANGLE_OFFSET: f32 = space::SPACE_0_5;
    /// The stop square: `width:9px; height:9px; border-radius:1px`.
    pub const STOP_SQUARE: f32 = 9.0;
    /// The record dot: `dotS` `width:10, height:10`.
    pub const RECORD_DOT: f32 = 10.0;
    /// Separator between groups: `width:1px; height:20px`.
    pub const SEPARATOR_HEIGHT: f32 = size::CONTROL_COMPACT;
    /// Position and time box: `width:200px`, gap 10.
    pub const POSITION_WIDTH: f32 = 200.0;
    /// Gap between position and time: `gap:10px`.
    pub const POSITION_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Tempo field: `height:24px; width:92px`.
    pub const TEMPO_WIDTH: f32 = 92.0;
    /// "Kein Audio-Gerät" chip: dot `width:6px; height:6px`.
    pub const ERROR_DOT: f32 = 6.0;
    /// CPU bar: `width:48px; height:4px; border-radius:2px`.
    pub const CPU_BAR: [f32; 2] = [48.0, 4.0];
    /// CPU value column: `width:30px`.
    pub const CPU_VALUE_WIDTH: f32 = 30.0;
    /// Master meter: `width:72px`, two bars.
    pub const MASTER_METER_LENGTH: f32 = 72.0;
}

/// Dock tab strip (`Screens.dc.html` shell markup, `dock`; `ks-view.js` `dockView`).
pub mod dock {
    use super::space;

    /// The "+" after the tabs: `padding:0 10px`.
    pub const PLUS_PADDING_X: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Right padding of the "Maximieren" group: `padding-right:4px`.
    pub const ACTIONS_PADDING_RIGHT: f32 = space::SPACE_1;
    /// Gap between "Maximieren" and the caret button: `gap:2px`.
    pub const ACTIONS_GAP: f32 = space::SPACE_0_5;
}

/// Layout 2a (`ks-view.js` `geo`, `Prototyp.dc.html`).
pub mod layout {
    use super::size;

    /// A collapsed browser or inspector becomes a rail this wide (`geo`: `brW = panels.br ? 260 :
    /// 24`).
    pub const RAIL_WIDTH: f32 = size::CONTROL;
    /// The caret of a rail sits this far below its top (`padding-top:10px`).
    pub const RAIL_CARET_TOP: f32 = 10.0;
    /// Gap between panels, where the surface.0 ground shows as a 1 px line (`gap:1px`).
    pub const GAP: f32 = 1.0;
    /// Half width of the grab zone around a panel edge for resizing. An interaction size, not a
    /// design value: the edge itself is the 1 px gap.
    pub const GRAB: f32 = 3.0;
}

/// Arrangement view defaults (`Prototyp.dc.html` model: `barW:72`; zoom clamps `24 … 160`, factor
/// 1.25 per step in `key()`).
pub mod arrangement {
    /// Default horizontal zoom: px per bar.
    pub const PX_PER_BAR: f32 = 72.0;
    /// Smallest horizontal zoom.
    pub const PX_PER_BAR_MIN: f32 = 24.0;
    /// Largest horizontal zoom.
    pub const PX_PER_BAR_MAX: f32 = 160.0;
    /// Zoom factor per `+` / `−` step.
    pub const ZOOM_STEP: f32 = 1.25;
    /// Vertical zoom range (Alt + / Alt −) as a factor on the stored track heights. Not in the
    /// design; chosen so that the track height range 32 … 160 stays reachable.
    pub const V_ZOOM: [f32; 2] = [0.5, 3.0];
}

/// Piano roll view defaults until WP-41 tunes them.
pub mod piano_roll {
    /// Default horizontal zoom in px per bar (S01 shows bars 9–12 over about 1300 px).
    pub const PX_PER_BAR: f32 = 320.0;
    /// MIDI key at the top edge by default (C6; S01 shows the range around C4 … C5).
    pub const TOP_KEY: u8 = 84;
}
