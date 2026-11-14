//! Spot checks of the generated device constants against `design/device-tokens.json`.

use komponist_tokens::{device, widget};

#[test]
fn grid() {
    assert_eq!(device::grid::CELL, [56.0, 64.0]);
    assert_eq!(device::grid::GAP, 4.0);
    assert_eq!(device::grid::PADDING, 8.0);
    assert_eq!(device::grid::ROWS, 3.0);
    assert_eq!(device::grid::HALF_CELL_H, 30.0);
    assert_eq!(device::grid::HALF_OFFSET_BOTTOM, 34.0);
    assert_eq!(device::grid::BODY_H, 216.0);
    assert_eq!(device::grid::COL_X, "8 + col * 60");
    assert_eq!(device::grid::DEVICE_W, "16 + 60 * cols - 4");
}

#[test]
fn frame() {
    use device::frame::*;
    assert_eq!(HEAD_H + BODY_H, TOTAL_H);
    assert_eq!(BODY_H, device::grid::BODY_H);
    assert_eq!(TOTAL_H, 244.0);
    assert_eq!(HEAD_PADDING, [0.0, 4.0, 0.0, 8.0]);
    assert_eq!(HEAD_GAP, 8.0);
    assert_eq!(POWER, [10.0, 10.0]);
    assert_eq!(TRACK_BAR, [3.0, 14.0]);
    assert_eq!(MENU_BUTTON, [20.0, 20.0]);
    assert_eq!(COLLAPSED_W, 28.0);
    assert_eq!(preset_dropdown::H, 20.0);
    assert_eq!(preset_dropdown::MIN_W, 96.0);
    assert_eq!(preset_dropdown::MAX_W, 150.0);
    assert_eq!(RADIUS, "radius.m");
    assert_eq!(POWER_ON, "state.play");
}

#[test]
fn chain() {
    use device::chain::*;
    assert_eq!((GAP, PADDING, HEADER_H), (8.0, 10.0, 28.0));
    assert_eq!((DROP_SLOT_W, SCROLLBAR_H), (120.0, 4.0));
}

#[test]
fn widgets() {
    assert_eq!(widget::knob::DIAMETER, 28.0);
    assert_eq!(widget::knob::SVG, 32.0);
    assert_eq!(widget::knob::SWEEP, 270.0);
    assert_eq!(widget::knob::INDICATOR, 1.5);
    assert_eq!(widget::knob::mod_ring::R_OFFSET, 2.5);
    assert_eq!(widget::knob::automation_dot::SIZE, 5.0);
    assert_eq!(widget::knob_large::DIAMETER, 44.0);
    assert_eq!(widget::number::FIELD_H, 18.0);
    assert_eq!(widget::segment::SEG_H, 16.0);
    assert_eq!(widget::mini_fader::THUMB, [24.0, 28.0]);
    assert_eq!(widget::mini_fader::LABEL_AREA_H, 30.0);
    assert_eq!(widget::graph::handle::R, 5.0);
    assert_eq!(widget::graph::curve::FILL_OPACITY, 0.14);
    assert_eq!(widget::meter_gr::BAR_W, 6.0);
    assert_eq!(widget::pad_grid::PAD, [56.0, 64.0]);
    assert_eq!(widget::pages::MAX_PAGES, 4.0);
    assert_eq!(widget::label::FONT, "sans 10/12");
    assert_eq!(widget::value::FONT, "mono 10/12 tnum");
    assert_eq!(widget::knob::CELL, [1.0, 1.0]);
    assert_eq!(widget::segment::CELL, ["n", "0.5"]);
}
