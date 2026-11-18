//! Waveform editor metrics from the approved audio proposal §4 and mockups 04/05.
use komponist_tokens::{size, space};
/// Default horizontal zoom (audio proposal §10).
pub const PPB_DEFAULT: f32 = 220.0;
/// Horizontal zoom bounds, matching the piano roll's useful source detail range.
pub const PPB_MIN: f32 = 24.0;
/// Maximum zoom for detailed source trimming.
pub const PPB_MAX: f32 = 3520.0;
/// Amplitude gutter, matching the piano-roll keyboard (§4).
pub const GUTTER: f32 = size::KEYBOARD;
/// Editor ruler, matching the piano-roll ruler.
pub const RULER_H: f32 = 18.0;
/// Toolbar field widths from mockup 04, using the app's number-field sizing.
pub const GAIN_W: f32 = 96.0;
/// Fade fields include millisecond unit and label.
pub const FADE_W: f32 = 120.0;
/// Source-start field includes the millisecond time display.
pub const SOURCE_W: f32 = 168.0;
/// Original BPM field plus its label.
pub const TEMPO_W: f32 = 176.0;
/// Bar-count conversion field.
pub const BARS_W: f32 = 176.0;
/// Minimum file-chip width before showing its compact filename variant.
pub const FILE_W: f32 = 200.0;
/// Waveform channel inset, matching arrangement envelope padding.
pub const WAVE_PAD: f32 = space::SPACE_2;
/// Edge-handle hit band from the approved editor mockup.
pub const TRIM_HIT: f32 = 6.0;
/// Missing-file banner inset.
pub const BANNER_PAD: f32 = space::SPACE_4;
/// Missing-file banner control gap.
pub const BANNER_GAP: f32 = space::SPACE_2;
/// Wheel zoom step, matching piano-roll zoom behavior.
pub const ZOOM_STEP: f32 = 1.25;
/// Centered missing-source recovery block, following mockup05.
pub const BANNER_CONTENT_W: f32 = 640.0;
/// Explicit recovery-button widths leave space for both catalog languages.
pub const BANNER_REPLACE_W: f32 = 160.0;
/// Copy-path recovery action width.
pub const BANNER_COPY_W: f32 = 128.0;

/// Approved Warp mode field, allowing the labelled Re-Pitch value in English.
pub const MODE_W: f32 = 120.0;
/// Approved 640 px dock: all four source-interpretation controls fit on one compact row.
pub const MODE_ROW_MIN: f32 =
    MODE_W + TEMPO_W + BARS_W + DURATION_W + space::SPACE_1 * 3.0 + space::SPACE_2 * 2.0;
/// Below this width the file chip and explanatory pitch text give way to essential controls.
pub const FILE_ROW_MIN: f32 = 960.0;
/// Read-only source/song mapping footer, one compact row.
pub const SOURCE_READOUT_H: f32 = size::PANEL_HEADER;
/// Source clock labels must not collide at the narrow dock width (approved dual ruler).
pub const SOURCE_LABEL_MIN_W: f32 = 112.0;

/// Approved duration-factor space; enough for the English label and numeric value.
pub const DURATION_W: f32 = 104.0;
