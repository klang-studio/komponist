//! The grids of the "Raster" dropdown, shared by the arrangement header and the piano roll
//! toolbar (`Screens.dc.html`: "1 Takt" in the arrangement, "1/16" in the editor).

use komponist_i18n::Lang;
use komponist_model::Grid;
use komponist_ui::widgets::dropdown::Item;

/// The grids in the order of the dropdown.
pub const GRIDS: [Grid; 9] = [
    Grid::Bar,
    Grid::Half,
    Grid::Quarter,
    Grid::Eighth,
    Grid::Sixteenth,
    Grid::ThirtySecond,
    Grid::QuarterTriplet,
    Grid::EighthTriplet,
    Grid::SixteenthTriplet,
];

/// The label of `grid` ("1 Takt", "1/16"; "1 Bar", "1/4 Triplet").
pub fn grid_label(lang: Lang, grid: Grid) -> &'static str {
    match grid {
        Grid::Bar => lang.grid_bar(),
        Grid::Half => lang.grid_half(),
        Grid::Quarter => lang.grid_quarter(),
        Grid::Eighth => lang.grid_eighth(),
        Grid::Sixteenth => lang.grid_sixteenth(),
        Grid::ThirtySecond => lang.grid_thirty_second(),
        Grid::QuarterTriplet => lang.grid_quarter_triplet(),
        Grid::EighthTriplet => lang.grid_eighth_triplet(),
        Grid::SixteenthTriplet => lang.grid_sixteenth_triplet(),
    }
}

/// The dropdown entries, one per grid.
pub fn items(lang: Lang) -> Vec<Item<'static>> {
    GRIDS
        .iter()
        .map(|g| Item::new(grid_label(lang, *g)))
        .collect()
}

/// The index of `grid` in [`GRIDS`].
pub fn index(grid: Grid) -> usize {
    GRIDS.iter().position(|g| *g == grid).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_indices() {
        assert_eq!(grid_label(Lang::De, Grid::Bar), "1 Takt");
        assert_eq!(grid_label(Lang::De, Grid::Sixteenth), "1/16");
        assert_eq!(grid_label(Lang::De, Grid::EighthTriplet), "1/8 Triole");
        assert_eq!(grid_label(Lang::En, Grid::Bar), "1 Bar");
        assert_eq!(GRIDS.len(), 9);
        assert_eq!(items(Lang::De).len(), GRIDS.len());
        for (i, g) in GRIDS.iter().enumerate() {
            assert_eq!(index(*g), i);
        }
    }
}
