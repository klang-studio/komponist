//! Automation proposal §9 and mockups 01–06.

/// Diameter of a visible point and its accessible hit area.
pub const POINT: f32 = 7.0;
pub const POINT_HIT: f32 = 10.0;
/// Diameter of the hollow diamond between control points.
pub const HANDLE: f32 = 5.0;
/// Width of the painted curve.
pub const CURVE_WIDTH: f32 = 1.5;
/// Clip envelopes need a stronger stroke against the piano roll and step sequencer grids.
pub const CLIP_CURVE_WIDTH: f32 = 2.5;
/// Keep the endpoint circles inside the clip plot, including at 0% and 100%.
pub const CLIP_VALUE_INSET: f32 = POINT / 2.0 + 1.0;
/// Dense points collapse into their pixel column.
pub const POINT_DENSITY: f32 = 3.0;
/// Chooser width, including the hints to the right of a parameter.
pub const CHOOSER_WIDTH: f32 = 280.0;
pub const CHOOSER_HEIGHT: f32 = 320.0;
/// Height of the compact mode chip.
pub const CHIP_HEIGHT: f32 = 20.0;
/// Toggle size and icon in the track header.
pub const TOGGLE_WIDTH: f32 = 18.0;
pub const TOGGLE_HEIGHT: f32 = 16.0;
pub const TOGGLE_ICON: f32 = 12.0;
/// The number of recently moved targets in the chooser.
pub const RECENT_TARGETS: usize = 3;

/// Clip lane height and chooser width (automation proposal §9.2, mockup 04).
pub const CLIP_HEIGHT_MIN: f32 = 48.0;
pub const CLIP_HEIGHT_DEFAULT: f32 = 96.0;
pub const CLIP_HEIGHT_MAX: f32 = 200.0;
pub const CLIP_CHOOSER_WIDTH: f32 = 142.0;
