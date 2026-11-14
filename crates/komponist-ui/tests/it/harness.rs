//! The snapshot harness and the foundation, rendered with `egui_kittest` (wgpu).
//!
//! - Sample snapshots at 1× and 2× (`tests/snapshots/*-1x.png`, `*-2x.png`).
//! - Pixel checks: 1 px lines are exactly one device pixel at 1× and exactly two at 2×, with no
//!   smeared neighbour rows or columns.
//! - Focus-visible: the ring appears after Tab and disappears after a click.
//! - The gallery shell: every interactive node has a name, the navigation works.

use egui::{Color32, Id, Key, LayerId, Order, Rect, Ui, pos2, vec2};
use egui_kittest::kittest::Queryable as _;
use komponist_tokens::color;
use komponist_tokens::egui_support::color32;
use komponist_ui::gallery_shell::{self, Gallery};
use komponist_ui::snapshot::{self, SCALES};
use komponist_ui::{State, focus, px};

#[test]
fn sample_snapshots() {
    let mut shots = snapshot::Snapshots::new();
    shots.add("state-examples", None, gallery_shell::state_examples);
    shots.add("shapes", None, gallery_shell::shape_examples);
    shots.finish();
}

#[test]
fn widget_in_state() {
    let mut shots = snapshot::Snapshots::new();
    for (_, state) in State::CONTROL_STATES {
        shots.add_state("state-sample", state, None, |ui| {
            gallery_shell::state_sample(ui, "Quantisieren", "Quantisieren");
        });
    }
    shots.finish();
}

const BG: [u8; 4] = color::SURFACE_0;
const INK: [u8; 4] = color::LINE_2;

/// Paints lines at fractional positions on a known background, in absolute coordinates.
fn line_scene(ui: &mut Ui) {
    let p = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Foreground, Id::unique("lines")));
    p.rect_filled(
        Rect::from_min_size(pos2(0.0, 0.0), vec2(64.0, 64.0)),
        0.0,
        color32(BG),
    );
    px::hline(&p, 4.0..=60.0, 10.3, color32(INK));
    px::vline(&p, 20.6, 16.0..=60.0, color32(INK));
    px::inner_border(
        &p,
        Rect::from_min_max(pos2(30.2, 30.4), pos2(50.7, 50.6)),
        color32(INK),
    );
}

fn pixel(img: &egui::ColorImage, x: usize, y: usize) -> Color32 {
    img.pixels[y * img.size[0] + x]
}

#[test]
fn one_px_lines_are_crisp() {
    let bg = color32(BG);
    let ink = color32(INK);
    for ppp in SCALES.into_iter().chain([1.5]) {
        let img = snapshot::render(vec2(64.0, 64.0), ppp, line_scene);
        let device = |v: f32| (v * ppp).round() as usize;
        let thick = device(px::line_width(ppp));
        assert_eq!(thick, (ppp.round() as usize).max(1));

        // Horizontal line: top edge at 10.3 snaps to the nearest device pixel edge.
        let top = device(px::snap(10.3, ppp));
        let x = device(30.0);
        for y in top - 2..top + thick + 2 {
            let expect = if (top..top + thick).contains(&y) {
                ink
            } else {
                bg
            };
            assert_eq!(pixel(&img, x, y), expect, "hline row {y} at {ppp}×");
        }
        // Vertical line.
        let left = device(px::snap(20.6, ppp));
        let y = device(40.0);
        for x in left - 2..left + thick + 2 {
            let expect = if (left..left + thick).contains(&x) {
                ink
            } else {
                bg
            };
            assert_eq!(pixel(&img, x, y), expect, "vline column {x} at {ppp}×");
        }
        // Inner border: the left edge is a crisp line inside the snapped rect.
        let left = device(px::snap(30.2, ppp));
        let y = device(40.0);
        for x in left - 1..left + thick + 2 {
            let expect = if (left..left + thick).contains(&x) {
                ink
            } else {
                bg
            };
            assert_eq!(pixel(&img, x, y), expect, "border column {x} at {ppp}×");
        }
    }
}

#[derive(Default)]
struct Probe {
    state: State,
    clicks: u32,
}

#[test]
fn focus_ring_only_after_keyboard() {
    let mut h = snapshot::builder::<Probe>(1.0)
        .with_size(vec2(200.0, 60.0))
        .build_ui_state(
            |ui, probe| {
                let r = snapshot::padded(ui, |ui| {
                    gallery_shell::state_sample(ui, "Quantisieren", "Quantisieren")
                });
                probe.state = State::of(ui, &r);
                probe.clicks += u32::from(r.clicked());
            },
            Probe::default(),
        );
    h.run();
    assert_eq!(h.state().state, State::REST);

    h.key_press(Key::Tab);
    h.run();
    assert!(focus::is_visible(&h.ctx));
    assert!(h.state().state.focused, "Tab focuses with a visible ring");
    h.snapshot(snapshot::file_stem("state-sample-keyboard-focus", 1.0));

    h.get_by_label("Quantisieren").click();
    h.run();
    assert_eq!(h.state().clicks, 1);
    assert!(!focus::is_visible(&h.ctx), "a click hides the ring");
    assert!(!h.state().state.focused);
}

#[test]
fn gallery_is_named_and_navigates() {
    let mut h = snapshot::builder::<Gallery>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .build_ui_state(|ui, g| g.show(ui), Gallery::designsystem());
    h.run();
    h.check_accessibility();
    assert_eq!(h.state().sections().len(), 8);
    let first = h.render().expect("render");
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    first.save(out.join("gallery-top.png")).unwrap();

    let label = h.state().sections()[2].nav_label();
    assert_eq!(label, "03 Zustandsregeln");
    h.get_by_label(&label).click();
    h.run();
    assert_eq!(h.state().current(), Some(2));
    let scrolled = h.render().expect("render");
    scrolled.save(out.join("gallery-03.png")).unwrap();
    assert_ne!(first.as_raw(), scrolled.as_raw(), "the page scrolled");

    // The whole page in one tall image, for review.
    let mut tall = snapshot::builder::<Gallery>(1.0)
        .with_size(vec2(1920.0, 4400.0))
        .build_ui_state(|ui, g| g.show(ui), Gallery::designsystem());
    tall.run();
    tall.render()
        .expect("render")
        .save(out.join("gallery-page.png"))
        .unwrap();

    // The live sample counts clicks. (By role: the filled cards of WP-20 have a menu entry
    // "Quantisieren" too.)
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Quantisieren")
        .click();
    h.run();
    h.get_by_label_contains("Klicks 1");
}
