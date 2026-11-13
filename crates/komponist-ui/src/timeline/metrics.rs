//! Geometry of the timeline elements that the design data gives as plain numbers.
//!
//! Everything a token covers comes from `komponist-tokens` in the painters (`size.clip-header`,
//! `size.toggle`, `radius.*`, `space.*`, …). The values here have **no token**; each one names its
//! source in `Designsystem.dc.html` / `Screens.dc.html` / `ks-view.js`. The report of WP-22 lists
//! them as contract change requests, so they can move into `tokens.json` as a group.

/// Track header: width of the colour bar ("Farbleiste 3", `hdr().bar`).
pub const HEADER_COLOR_BAR: f32 = 3.0;
/// Track header: gap between the name and the instrument line (`gap:1px`).
pub const HEADER_TEXT_GAP: f32 = 1.0;
/// Track header: width of one meter channel ("Pegel 2 × 3", `meterH().box`).
pub const HEADER_METER_BAR: f32 = 3.0;
/// Track header: gap between the two meter channels (`gap:1px`).
pub const HEADER_METER_GAP: f32 = 1.0;
/// Track header meter: share of the height drawn in `meter.ok`; above it `meter.loud`
/// (`meterH`: `Math.min(l, .8)`).
pub const HEADER_METER_LOUD_FROM: f32 = 0.8;
/// Track header meter: height of the clip mark at the top (`meterH().clip`, `height:3`).
pub const HEADER_METER_CLIP_MARK: f32 = 3.0;

/// Track header: size of the ⚠ in front of an error in the instrument line (the glyph of
/// `type.small` in `headers` "plugin fehlt"; drawn as the Lucide icon, Plex has no ⚠).
pub const HEADER_WARNING_ICON: f32 = 12.0;

/// Clip: horizontal inset in its lane (`clipView`: `x = c.x·barW + 1`, `w = c.w·barW − 2`).
pub const CLIP_INSET_X: f32 = 1.0;
/// Clip: vertical inset in its lane ("Höhe = Spur − 6", `top:3`).
pub const CLIP_INSET_Y: f32 = 3.0;
/// Clip: smallest width (`clipView`: `Math.max(4, …)`).
pub const CLIP_MIN_WIDTH: f32 = 4.0;
/// Clip head: horizontal text padding (`padding:'0 5px'`).
pub const CLIP_HEAD_PAD_X: f32 = 5.0;
/// Clip head: diameter of the ● in front of the name of a recording clip ("● Aufnahme", the
/// glyph of the 10 px head text; drawn as a circle, Plex has no ●).
pub const CLIP_RECORD_DOT: f32 = 5.0;
/// Clip hover: width of the trim handles at both ends of the body (`width:5`).
pub const CLIP_HANDLE_WIDTH: f32 = 5.0;
/// Clip hover: width of the loop handle in the top right corner of the head (`width:10`).
pub const CLIP_LOOP_HANDLE_WIDTH: f32 = 10.0;
/// Clip: width of the active trim edge (`trim`: `width:2`, accent).
pub const CLIP_TRIM_EDGE: f32 = 2.0;
/// Clip: the loop mark ▾ in the head (`loopMark`: borders 3 + 3 wide, 5 high).
pub const CLIP_LOOP_MARK: [f32; 2] = [6.0, 5.0];

/// Clip preview: height of a note mark (`marks`: `height:2`).
pub const PREVIEW_NOTE_HEIGHT: f32 = 2.0;
/// Clip preview: space above the highest note mark in the body (`top: hh + 4`).
pub const PREVIEW_NOTE_TOP: f32 = 4.0;
/// Clip preview: body height not used by the pitch range (`bh − 10`).
pub const PREVIEW_NOTE_V_PAD: f32 = 10.0;
/// Clip preview: pitch range margin above and below the notes (`lo −= 2; hi += 2`).
pub const PREVIEW_PITCH_MARGIN: u8 = 2;
/// Clip preview: space above the first step row (`top: hh + 3`).
pub const PREVIEW_STEP_TOP: f32 = 3.0;
/// Clip preview: body height not used by step rows (`rh = (bh − 6) / rows`).
pub const PREVIEW_STEP_V_PAD: f32 = 6.0;
/// Clip preview: horizontal gap after a step (`sw − 1.5`, at least 1.5 wide).
pub const PREVIEW_STEP_GAP: f32 = 1.5;
/// Clip preview: horizontal offset of a step mark in its column (`k·sw + .5`).
pub const PREVIEW_STEP_OFFSET: f32 = 0.5;

/// Note: inset in its key row ("Höhe = Tastenhöhe − 2", `left: s·cw + 1`, `width: l·cw − 2`).
pub const NOTE_INSET: f32 = 1.0;
/// Note: smallest width (`prView`: `Math.max(3, …)`).
pub const NOTE_MIN_WIDTH: f32 = 3.0;
/// Note: the name is shown from this size on ("mit Name ab h 12, w 28").
pub const NOTE_LABEL_MIN: [f32; 2] = [28.0, 12.0];
/// Note: left padding of the name (`paddingLeft:3`).
pub const NOTE_LABEL_PAD: f32 = 3.0;
/// Note level of detail: from this height on, selection and playing are rings around the note;
/// below it the note is filled instead ("zoom · Taste 4 · Auswahl = Akzentfläche").
pub const NOTE_RING_MIN_HEIGHT: f32 = 4.0;

/// Ruler: height of the loop bar at the bottom (`loopBar`: `height:2`).
pub const RULER_LOOP_BAR: f32 = 2.0;
/// Ruler: height of the clip range bar of the editor ruler (`clipRange`: `height:3`).
pub const RULER_RANGE_BAR: f32 = 3.0;
/// Ruler: beat ticks are drawn from this bar width on (`arrView`: `barW >= 40`).
pub const RULER_BEAT_TICKS_MIN_BAR: f32 = 40.0;
/// Ruler: the playhead cap (`cap`: 9 × 6, `left: ph − 4`).
pub const PLAYHEAD_CAP: [f32; 2] = [9.0, 6.0];

/// Dashed outlines (Alt-copy original, dragged note): dash and gap length in px, as Chromium
/// draws a 1 px `dashed` border.
pub const DASH: f32 = 3.0;
