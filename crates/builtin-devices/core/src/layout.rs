//! Helpers to build device panels from the model's layout vocabulary.

use komponist_model::params::ParamId;
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, Page, WidgetKind};

/// `CellParam::Id` of `key`.
pub fn param(key: &str) -> CellParam {
    CellParam::Id(ParamId::new(key))
}

/// `CellParam::Template` of `template` (`b{band}_freq`).
pub fn template(template: &str) -> CellParam {
    CellParam::Template(template.into())
}

/// A knob in one cell.
pub fn knob(col: u8, row: u8, key: &str) -> Cell {
    Cell::knob(col, row, ParamId::new(key))
}

/// A large knob over two cells.
pub fn knob_large(col: u8, row: u8, key: &str) -> Cell {
    Cell::span(col, row, 2, 1, WidgetKind::KnobLarge, param(key))
}

/// A toggle button in a half cell.
pub fn toggle(col: u8, row: u8, cols: u8, half: Half, p: CellParam) -> Cell {
    Cell::half(col, row, cols, half, WidgetKind::ToggleButton, p)
}

/// A one-page layout.
pub fn single_page(cols: u8, cells: Vec<Cell>) -> DeviceLayout {
    DeviceLayout::single(Page {
        name: String::new(),
        cols,
        cells,
        dividers: Vec::new(),
    })
}
