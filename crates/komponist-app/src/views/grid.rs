//! The grids of the "Raster" dropdown, shared by the arrangement header and the piano roll
//! toolbar (`Screens.dc.html`: "1 Takt" in the arrangement, "1/16" in the editor).

use komponist_model::Grid;
use komponist_ui::widgets::dropdown::Item;

/// The grids in the order of the dropdown, with their labels ("1 Takt", "1/16", "1/8 Triole").
pub const GRIDS: [(Grid, &str); 9] = [
    (Grid::Bar, "1 Takt"),
    (Grid::Half, "1/2"),
    (Grid::Quarter, "1/4"),
    (Grid::Eighth, "1/8"),
    (Grid::Sixteenth, "1/16"),
    (Grid::ThirtySecond, "1/32"),
    (Grid::QuarterTriplet, "1/4 Triole"),
    (Grid::EighthTriplet, "1/8 Triole"),
    (Grid::SixteenthTriplet, "1/16 Triole"),
];

/// The label of `grid` ("1 Takt", "1/16").
pub fn grid_label(grid: Grid) -> &'static str {
    GRIDS
        .iter()
        .find(|(g, _)| *g == grid)
        .map_or("", |(_, l)| l)
}

/// The dropdown entries, one per grid.
pub fn items() -> Vec<Item<'static>> {
    GRIDS.iter().map(|(_, l)| Item::new(l)).collect()
}

/// The index of `grid` in [`GRIDS`].
pub fn index(grid: Grid) -> usize {
    GRIDS.iter().position(|(g, _)| *g == grid).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_indices() {
        assert_eq!(grid_label(Grid::Bar), "1 Takt");
        assert_eq!(grid_label(Grid::Sixteenth), "1/16");
        assert_eq!(grid_label(Grid::EighthTriplet), "1/8 Triole");
        assert_eq!(GRIDS.len(), 9);
        assert_eq!(items().len(), GRIDS.len());
        for (i, (g, _)) in GRIDS.iter().enumerate() {
            assert_eq!(index(*g), i);
        }
    }
}
