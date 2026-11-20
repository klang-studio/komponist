use super::SPEC;

#[test]
fn the_layout_is_valid_and_has_one_page_of_eight_columns() {
    let layout = SPEC.layout();
    assert_eq!(layout.pages.len(), 1);
    assert_eq!(layout.pages[0].cols, 8);
}
