//! The declarative device layout lives in `komponist_model::params::layout` (internal devices in
//! `komponist-builtin` define theirs there); this module re-exports it and adds the egui geometry.

use egui::{Rect, pos2, vec2};

pub use komponist_model::params::layout::*;

/// The rectangle of `cell`, relative to the top-left corner of the device body.
pub fn cell_rect(cell: &Cell) -> Rect {
    let [x, y, w, h] = cell.bounds();
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::params::ParamId;

    #[test]
    fn cells_sit_on_the_grid_as_egui_rects() {
        let k = Cell::knob(2, 1, ParamId::new("a"));
        assert_eq!(
            cell_rect(&k),
            Rect::from_min_size(pos2(128.0, 76.0), vec2(56.0, 64.0))
        );
    }
}
