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
pub const TEMPO_W: f32 = 160.0;
/// Bar-count conversion field.
pub const BARS_W: f32 = 120.0;
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
