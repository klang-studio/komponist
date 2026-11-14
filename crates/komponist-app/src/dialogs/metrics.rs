//! Sizes of the dialogs and the start screen that the handoff gives only in the screen markup
//! (`Screens.dc.html` S06, S07, S09; `Prototyp.dc.html` start screen), with their source (rule 2).
//! Values that are sums of spacing tokens are written that way.
//!
//! Type sizes without a type token (contract change request in the WP-52 report): the start
//! screen's wordmark (32 px / 600), the recent project names (14 px / 600) and the dialog body text
//! (13 px / 400, line 1.55). The 13 px texts use `type.track`'s size at another weight (as WP-40's
//! S10 hint does); 14 px and Mono 11 map to the nearest tokens (`type.track`, `type.value.s`).

use komponist_tokens::typography::TypeStyle;
use komponist_tokens::{space, typography};

/// Common to S06, S07, S08 and the dialogs without a screen of their own.
pub mod dialog {
    use super::space;

    /// Header bar of S07 and S08: `height:44px`.
    pub const HEADER_HEIGHT: f32 = 44.0;
    /// Header padding left: `padding:0 8px 0 20px`.
    pub const HEADER_PADDING_LEFT: f32 = space::SPACE_4 + space::SPACE_1;
    /// Header padding right.
    pub const HEADER_PADDING_RIGHT: f32 = space::SPACE_2;
    /// The × button: `width:28px; height:28px`.
    pub const CLOSE: f32 = 28.0;
    /// Padding of a dialog body (S06 `padding:20px 20px 0`, S08 `padding:20px`).
    pub const PADDING: f32 = space::SPACE_4 + space::SPACE_1;
    /// Footer padding (S08 `padding:14px 20px`).
    pub const FOOTER_PADDING_Y: f32 = space::SPACE_3 + space::SPACE_0_5;
    /// Dialog buttons: `height:28px; padding:0 12px`.
    pub const BUTTON_HEIGHT: f32 = 28.0;
    /// Their horizontal padding.
    pub const BUTTON_PADDING_X: f32 = space::SPACE_3;
    /// Gap between dialog buttons: `gap:8px`.
    pub const BUTTON_GAP: f32 = space::SPACE_2;
    /// Line height of the 13 px body text (`type.track`'s size): `line-height:1.55` → 20.15.
    pub const TEXT_LINE_HEIGHT: f32 = komponist_tokens::typography::TRACK.size * 1.55;
    /// Weight of the body text (`type.track`'s size at regular weight).
    pub const TEXT_WEIGHT: u16 = 400;
    /// Width of the small dialogs without a screen (file browser, unsaved changes): S06's width.
    pub const WIDTH: f32 = 560.0;
    /// Height of the file browser's list (12 rows of `size.menu-row`).
    pub const FILE_LIST_HEIGHT: f32 = 12.0 * komponist_tokens::size::MENU_ROW;
    /// Smallest room left around a dialog when the window is small.
    pub const MARGIN: f32 = space::SPACE_6;
    /// Label column of the file browser's fields ("Ordner", "Dateiname"). No screen shows a file
    /// browser; S08's label column (`120px`) shrunk to the narrower dialog.
    pub const FIELD_LABEL_WIDTH: f32 = 72.0;
}

/// S06 "Plugins fehlen" (`ovMissing` markup).
pub mod missing {
    use super::space;

    /// `width:560px`.
    pub const WIDTH: f32 = super::dialog::WIDTH;
    /// The red dot before the title: `width:8px; height:8px`.
    pub const DOT: f32 = 8.0;
    /// Gap between dot and title: `gap:10px`.
    pub const TITLE_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Gap between title and text: `gap:8px`.
    pub const TEXT_GAP: f32 = space::SPACE_2;
    /// List margin: `margin:16px 20px`.
    pub const LIST_MARGIN_Y: f32 = space::SPACE_4;
    /// Gap between list rows: `gap:2px`.
    pub const LIST_GAP: f32 = space::SPACE_0_5;
    /// Row padding: `padding:10px 12px`.
    pub const ROW_PADDING: [f32; 2] = [space::SPACE_3, space::SPACE_2 + space::SPACE_0_5];
    /// Row grid gap: `gap:4px 12px`.
    pub const ROW_GAP: [f32; 2] = [space::SPACE_3, space::SPACE_1];
    /// Format chip padding: `padding:1px 4px`.
    pub const CHIP_PADDING: [f32; 2] = [space::SPACE_1, 1.0];
    /// Footer: `padding:12px 20px 20px`.
    pub const FOOTER_PADDING_TOP: f32 = space::SPACE_3;
    /// Most rows shown before the list scrolls (the design shows two; a project may miss many).
    pub const LIST_MAX_HEIGHT: f32 = 320.0;
}

/// S07 "Einstellungen" (`ovSettings` markup, `setNav`, `buffers`, `sw`, `midiIns`, `pluginDirs`).
pub mod settings {
    use super::space;

    /// `width:800px; height:720px`.
    pub const SIZE: [f32; 2] = [800.0, 720.0];
    /// Navigation column: `width:180px; padding:12px 8px; gap:2px`.
    pub const NAV_WIDTH: f32 = 180.0;
    /// Navigation padding.
    pub const NAV_PADDING: [f32; 2] = [space::SPACE_2, space::SPACE_3];
    /// Navigation item gap.
    pub const NAV_GAP: f32 = space::SPACE_0_5;
    /// Navigation item: `setNav` `height:28, padding:'0 10px'`.
    pub const NAV_ITEM_HEIGHT: f32 = 28.0;
    /// Its horizontal padding.
    pub const NAV_ITEM_PADDING_X: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Content padding: `padding:20px 24px`.
    pub const CONTENT_PADDING: [f32; 2] = [space::SPACE_6, space::SPACE_4 + space::SPACE_1];
    /// Gap between sections: `gap:24px`.
    pub const SECTION_GAP: f32 = space::SPACE_6;
    /// Gap between a section label and its content: `gap:12px` (audio), `gap:10px` (lists).
    pub const SECTION_INNER_GAP: f32 = space::SPACE_3;
    /// Label column of the audio grid: `grid-template-columns:140px …`.
    pub const LABEL_COLUMN: f32 = 140.0;
    /// Grid gap: `gap:10px 16px`.
    pub const GRID_GAP: [f32; 2] = [space::SPACE_4, space::SPACE_2 + space::SPACE_0_5];
    /// Sample-rate field: `width:160px`.
    pub const RATE_WIDTH: f32 = 160.0;
    /// Gap between buffer segments and latency: `gap:12px`.
    pub const LATENCY_GAP: f32 = space::SPACE_3;
    /// Device notice: `padding:8px 10px; gap:10px`.
    pub const NOTICE_PADDING: [f32; 2] = [space::SPACE_2 + space::SPACE_0_5, space::SPACE_2];
    /// Gap between the notice's parts.
    pub const NOTICE_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Line height of the notice text (12 px, `type.ui`): `line-height:1.45`.
    pub const NOTICE_LINE_HEIGHT: f32 = komponist_tokens::typography::UI.size * 1.45;
    /// Status dots (notice, MIDI activity): `width:6px; height:6px`.
    pub const DOT: f32 = 6.0;
    /// MIDI input row: `height:28px; gap:12px`.
    pub const MIDI_ROW: f32 = 28.0;
    /// Gap in a MIDI input row.
    pub const MIDI_GAP: f32 = space::SPACE_3;
    /// MIDI port name column: `midiIns` `nameS` `width:220`.
    pub const MIDI_NAME_WIDTH: f32 = 220.0;
    /// Plugin folder row: `height:28px; padding:0 10px`, columns `48px minmax(0,1fr) auto`,
    /// `gap:12px`.
    pub const DIR_ROW: f32 = 28.0;
    /// Its horizontal padding.
    pub const DIR_PADDING_X: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Format column.
    pub const DIR_FORMAT_COLUMN: f32 = 48.0;
    /// Column gap.
    pub const DIR_GAP: f32 = space::SPACE_3;
    /// Gap between the "+ Ordner" and "Neu scannen" buttons: `gap:6px`.
    pub const DIR_BUTTON_GAP: f32 = space::SPACE_1_5;
}

/// S09 start screen (`isStart` markup, `S09.recentList`).
pub mod start {
    use super::space;

    /// Left column: `grid-template-columns:560px minmax(0,1fr)`.
    pub const LEFT_WIDTH: f32 = 560.0;
    /// Column padding: `padding:72px 64px`.
    pub const PADDING: [f32; 2] = [space::SPACE_8 * 2.0, 72.0];
    /// Gap between the left column's blocks: `gap:40px`.
    pub const LEFT_GAP: f32 = 40.0;
    /// Gap between wordmark and version line: `gap:6px`.
    pub const WORDMARK_GAP: f32 = space::SPACE_1_5;
    /// Letter spacing of the wordmark: `letter-spacing:-0.02em`.
    pub const WORDMARK_TRACKING: f32 = -0.02;
    /// Action buttons: `width:280px`, `height:32px; padding:0 12px`, `gap:8px`.
    pub const BUTTON_WIDTH: f32 = 280.0;
    /// Their height.
    pub const BUTTON_HEIGHT: f32 = space::SPACE_8;
    /// Their horizontal padding.
    pub const BUTTON_PADDING_X: f32 = space::SPACE_3;
    /// Gap between them.
    pub const BUTTON_GAP: f32 = space::SPACE_2;
    /// System rows: `gap:8px`, dot `6px`, `gap:10px` inside a row.
    pub const SYSTEM_GAP: f32 = space::SPACE_2;
    /// The status dot.
    pub const DOT: f32 = 6.0;
    /// Gap inside a system row.
    pub const SYSTEM_ROW_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Right column: `gap:16px`.
    pub const RIGHT_GAP: f32 = space::SPACE_4;
    /// Recent list and header: `max-width:1100px`.
    pub const RECENT_MAX_WIDTH: f32 = 1100.0;
    /// Filter field: `height:24px; width:220px`.
    pub const FILTER_WIDTH: f32 = 220.0;
    /// Recent row: `height:64, padding:'0 16px'`, columns `60px minmax(0,1fr) 200px 120px`,
    /// `gap:20`.
    pub const ROW_HEIGHT: f32 = 64.0;
    /// Its horizontal padding.
    pub const ROW_PADDING_X: f32 = space::SPACE_4;
    /// Colour bar column.
    pub const BARS_COLUMN: f32 = 60.0;
    /// Meta column ("124 BPM · 6 Spuren").
    pub const META_COLUMN: f32 = 200.0;
    /// Date column.
    pub const DATE_COLUMN: f32 = 120.0;
    /// Column gap.
    pub const ROW_GAP: f32 = space::SPACE_4 + space::SPACE_1;
    /// Gap between rows: `gap:2px`.
    pub const LIST_GAP: f32 = space::SPACE_0_5;
    /// Colour bars: `width:4, borderRadius:1`, `height:32px`, `gap:2px`, at most 10.
    pub const BAR: [f32; 2] = [4.0, space::SPACE_8];
    /// Gap between bars.
    pub const BAR_GAP: f32 = space::SPACE_0_5;
    /// Corner radius of a bar.
    pub const BAR_RADIUS: f32 = 1.0;
    /// Most bars per row (`hs.slice(0,10)`).
    pub const BARS_MAX: usize = 10;
    /// Gap between name and path: `gap:3px`.
    pub const NAME_GAP: f32 = 3.0;
    /// Extra space above the hint line: `margin-top:8px`.
    pub const HINT_TOP: f32 = space::SPACE_2;
    /// Offset of the selection outline: `outlineOffset:1`.
    pub const OUTLINE_OFFSET: f32 = 1.0;
}

/// The start screen's wordmark "komponist": 32 px, 600, `letter-spacing:-0.02em` (no type token;
/// `type.title` at twice its size and line height).
pub const WORDMARK: TypeStyle = TypeStyle {
    size: typography::TITLE.size * 2.0,
    line_height: typography::TITLE.line_height * 2.0,
    tracking_em: start::WORDMARK_TRACKING,
    ..typography::TITLE
};

/// Mono 11 of the start screen and S07 (paths, meta, latency): `type.value.s` (Mono 10), the
/// nearest token.
pub const MONO_SMALL: TypeStyle = TypeStyle {
    color: None,
    ..typography::VALUE_S
};
