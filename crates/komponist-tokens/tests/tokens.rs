//! Spot checks of the generated constants against `design/tokens.json`.

use komponist_tokens::{BoxShadow, color, focus, line, radius, select, shadow, size, space};

#[test]
fn colours() {
    assert_eq!(color::SURFACE_0, [0x11, 0x12, 0x14, 0xff]);
    assert_eq!(color::SURFACE_5, [0x3b, 0x3d, 0x42, 0xff]);
    assert_eq!(
        color::SURFACE,
        [
            color::SURFACE_0,
            color::SURFACE_1,
            color::SURFACE_2,
            color::SURFACE_3,
            color::SURFACE_4,
            color::SURFACE_5
        ]
    );
    assert_eq!(color::TEXT_1, [0xe8, 0xe9, 0xeb, 0xff]);
    assert_eq!(color::TEXT_4, [0x4a, 0x4d, 0x52, 0xff]);
    assert_eq!(color::LINE_2, [0x45, 0x48, 0x4d, 0xff]);
    assert_eq!(color::ACCENT, [0x4e, 0xa7, 0xe6, 0xff]);
    assert_eq!(color::ACCENT_INK, [0x0b, 0x16, 0x20, 0xff]);
    assert_eq!(color::STATE_RECORD_SURFACE, [0x3a, 0x1d, 0x1f, 0xff]);
    assert_eq!(color::STATE_ERROR, [0xff, 0x5a, 0x5f, 0xff]);
    assert_eq!(color::METER_TRACK, color::SURFACE_0);
}

/// The colours komponist adds to the handoff tokens (`design/README.md`).
#[test]
fn komponist_colours() {
    assert_eq!(color::LINE_0, [0x23, 0x24, 0x27, 0xff]);
    assert_eq!(color::LINE, [color::LINE_0, color::LINE_1, color::LINE_2]);
    assert_eq!(color::STATE_RECORD_INK, [0xf2, 0xa6, 0xa8, 0xff]);
    assert_eq!(color::CANVAS_LANE_SELECTED, [0x1a, 0x1c, 0x1f, 0xff]);
    assert_eq!(color::CANVAS_BLACK_KEY_ROW, [0x1a, 0x1b, 0x1d, 0xff]);
    // `rgba(17,18,20,0.6)`: surface.0 at 60 % opacity.
    let [r, g, b, a] = color::CANVAS_OUTSIDE_CLIP;
    assert_eq!([r, g, b, 0xff], color::SURFACE_0);
    assert_eq!(a, (0.6_f64 * 255.0).round() as u8);
    // `rgba(78,167,230,0.10)`: the accent at 10 % (piano roll box selection).
    let [r, g, b, a] = color::CANVAS_BOX_SELECT;
    assert_eq!([r, g, b, 0xff], color::ACCENT);
    assert_eq!(a, (0.10_f64 * 255.0).round() as u8);
    // `rgba(232,233,235,0.16)`: text.1 at 16 % opacity.
    let [r, g, b, a] = color::CANVAS_CLIP_EDGE_SELECTED;
    assert_eq!([r, g, b, 0xff], color::TEXT_1);
    assert_eq!(a, (0.16_f64 * 255.0).round() as u8);
    // `#222326`: every other group of four steps in the step sequencer.
    assert_eq!(color::CANVAS_STEP_GROUP_ALT, [0x22, 0x23, 0x26, 0xff]);
    // `#1a1f26`: the mixer strip of the selected track.
    assert_eq!(color::CANVAS_STRIP_SELECTED, [0x1a, 0x1f, 0x26, 0xff]);
}

#[test]
fn lengths() {
    assert_eq!(space::SPACE_0_5, 2.0);
    assert_eq!(space::SPACE_1_5, 6.0);
    assert_eq!(space::SPACE_8, 32.0);
    assert_eq!((radius::S, radius::M, radius::L), (2.0, 3.0, 6.0));
    assert_eq!(line::WIDTH, 1.0);
    assert_eq!(size::TRANSPORT, 44.0);
    assert_eq!(size::TRACK_HEADER, 224.0);
    assert_eq!(
        (size::track::MIN, size::track::DEFAULT, size::track::MAX),
        (32.0, 56.0, 160.0)
    );
    assert_eq!(size::dock::DEFAULT, 320.0);
    assert_eq!(size::dock::MAX_VIEWPORT_HEIGHT_FRACTION, 0.7);
    assert_eq!(size::dock::COLLAPSED, 28.0);
    assert_eq!(size::key::DEFAULT, 12.0);
    assert_eq!(size::TOGGLE, [18.0, 16.0]);
    assert_eq!(size::FADER_THUMB_COMPACT, [24.0, 28.0]);
    assert_eq!(size::ICON, 16.0);
}

#[test]
fn focus_select_shadow() {
    assert_eq!(focus::OUTLINE_WIDTH, 1.0);
    assert_eq!(focus::OUTLINE_COLOR, color::ACCENT);
    assert_eq!(focus::OFFSET, 1.0);
    assert_eq!(
        select::RING,
        BoxShadow {
            offset: [0.0, 0.0],
            blur: 0.0,
            spread: 1.0,
            color: color::ACCENT
        }
    );
    // "0 6px 16px rgba(0,0,0,0.45)": alpha 0.45 × 255 = 114.75 → 115.
    assert_eq!(
        shadow::FLOAT,
        BoxShadow {
            offset: [0.0, 6.0],
            blur: 16.0,
            spread: 0.0,
            color: [0, 0, 0, 115]
        }
    );
    assert_eq!(
        shadow::EDGE,
        BoxShadow {
            offset: [0.0, 0.0],
            blur: 0.0,
            spread: 1.0,
            color: color::SURFACE_0
        }
    );
}
