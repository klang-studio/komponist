//! Acceptance flow 1 (HANDOFF §9): new project → MIDI track with Strg T or an instrument dragged
//! in from the browser.

use crate::acceptance_support;

use acceptance_support::*;
use egui::Key;
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use komponist_app::App;
use komponist_model::fixtures::demo_project;

#[test]
fn new_project_then_ctrl_t_and_a_dragged_instrument_make_two_tracks() {
    let mut h = harness(App::headless(demo_project()));
    // Strg N: an empty, clean project.
    ctrl(&mut h, Key::N);
    assert!(h.state().project().tracks.is_empty());
    assert!(!h.state().doc().is_dirty());

    // Strg T: an armed pisynth track, selected.
    ctrl(&mut h, Key::T);
    let p = h.state().project();
    assert_eq!(p.tracks.len(), 1);
    let first = p.tracks[0].id;
    assert!(p.tracks[0].arm);
    assert_eq!(p.tracks[0].channel.devices[0].plugin.name, "pisynth");
    assert!(h.state().ui().selection.tracks.contains(&first));
    h.get_by_role_and_label(Role::Row, &format!("Spur {}, pisynth", p.tracks[0].name));

    // A browser row dragged onto the empty area below the header: a second track.
    let row = h
        .get_all_by_role_and_label(Role::ListItem, "pisynth")
        .next()
        .unwrap()
        .rect();
    let header = rect_of(&h, Role::Row, "Spur ");
    drag(
        &mut h,
        row.center(),
        header.left_bottom() + egui::vec2(300.0, 300.0),
    );
    let p = h.state().project();
    assert_eq!(p.tracks.len(), 2);
    assert_eq!(p.tracks[1].channel.devices[0].plugin.name, "pisynth");
    assert!(
        p.tracks[1].arm,
        "the new track is armed, the old one is not"
    );
    assert!(!p.tracks[0].arm);
    assert!(h.state().doc().is_dirty());

    // Undo twice: back to the empty project.
    ctrl(&mut h, Key::Z);
    ctrl(&mut h, Key::Z);
    assert!(h.state().project().tracks.is_empty());
}
