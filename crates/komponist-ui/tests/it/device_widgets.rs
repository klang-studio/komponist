//! WP-83 device widgets: the gallery page „Geräte-Widgets“ card by card (snapshots at 1× and 2×),
//! sizes against the generator's geometry, interaction and AccessKit of every widget.

use egui::Ui;
use komponist_ui::snapshot::Snapshots;
use komponist_ui::widgets::device::gallery;

/// Width of a card on the page (`page::CARDS_COMPONENTS`).
const CARD_WIDTH: f32 = 560.0;

/// Card `index` of the cards of all groups of the section, in order.
fn card(ui: &mut Ui, index: usize) {
    ui.set_width(CARD_WIDTH);
    let section = gallery::section();
    let cards: Vec<_> = section.groups.iter().flat_map(|g| g.cards.iter()).collect();
    (cards[index].content.expect("content"))(ui);
}

#[test]
fn gallery_cards() {
    let mut shots = Snapshots::new();
    let titles: Vec<&str> = gallery::section()
        .groups
        .iter()
        .flat_map(|g| g.cards.iter())
        .map(|c| c.title)
        .collect();
    for (i, title) in titles.iter().enumerate() {
        let slug: String = title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        shots.add(&format!("device-card-{slug}"), None, |ui| card(ui, i));
    }
    shots.finish();
}

mod behaviour {
    use egui::accesskit::Role;
    use egui::{Ui, vec2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use komponist_tokens::{TrackPalette, device};
    use komponist_ui::snapshot;
    use komponist_ui::widgets::device::choice::{
        DropdownCell, Glyph, Segment, SegmentOption, ToggleButton, Wave,
    };
    use komponist_ui::widgets::device::fm_algorithm::{FmAlgorithm, Topology};
    use komponist_ui::widgets::device::knob_cell::KnobCell;
    use komponist_ui::widgets::device::meter_gr::MeterGr;
    use komponist_ui::widgets::device::mini_fader::{self, MiniFader};
    use komponist_ui::widgets::device::number::Number;
    use komponist_ui::widgets::device::pad_grid::{Pad, PadGrid};
    use komponist_ui::widgets::device::{cell_height, cell_width, half_cell};
    use komponist_ui::widgets::dropdown::Item;

    fn harness<S>(state: S, app: impl FnMut(&mut Ui, &mut S) + 'static) -> Harness<'static, S> {
        let mut app = app;
        let mut h = snapshot::builder::<S>(1.0)
            .with_size(vec2(480.0, 320.0))
            .build_ui_state(move |ui, s| snapshot::padded(ui, |ui| app(ui, s)), state);
        h.run();
        h
    }

    #[test]
    fn sizes_follow_the_grid() {
        assert_eq!(half_cell(1), vec2(56.0, 30.0));
        assert_eq!(half_cell(2), vec2(116.0, 30.0));
        assert_eq!(cell_width(1), device::grid::CELL[0]);
        assert_eq!(cell_height(3), 200.0);
        // The mini fader's thumb travels h − 30 − 28.
        let layout = mini_fader::Layout::new(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            vec2(56.0, cell_height(3)),
        ));
        assert_eq!(layout.travel, 142.0);
        assert_eq!(layout.thumb(1.0).top(), 4.0);
        assert_eq!(layout.thumb(0.0).bottom(), 4.0 + 142.0 + 28.0);
        assert_eq!(layout.track().width(), 4.0);
    }

    #[test]
    fn every_widget_has_a_role_and_a_german_name() {
        let opts = [
            SegmentOption::text("LC"),
            SegmentOption::glyph("Sinus", Glyph::Wave(Wave::Sine)),
        ];
        let items = [Item::new("808 Rostock"), Item::new("Studio Kit")];
        let pal = TrackPalette::new(45);
        let pads = [Pad::new("Kick", "C1"), Pad::new("Snare", "D1")];
        let algo = Topology {
            modulates: &[0, 1],
            feedback: None,
        };
        let h = harness(
            (0.5_f64, 0.3_f64, 8_i32, 0_usize, true, 0_usize),
            move |ui, s| {
                ui.add(KnobCell::new(&mut s.0, "Cutoff"));
                ui.add(KnobCell::new(&mut s.1, "Größe").large());
                ui.add(Number::new(&mut s.2, "Stimmen").range(1.0..=16.0).step(1.0));
                ui.add(Segment::new("Typ", &opts, &mut s.3).cols(2));
                ui.add(ToggleButton::new("Loop", &mut s.4));
                ui.add(DropdownCell::new("Kit", &items, &mut s.3));
                ui.add(MiniFader::new(&mut s.1, "Op 1"));
                ui.add(MeterGr::new([0.5, 0.2, 0.4], "\u{2212}4.1"));
                PadGrid::new(&pads, 2, &pal).selected(Some(s.5)).show(ui);
                FmAlgorithm::new(1, algo, &pal).show(ui);
            },
        );
        h.check_accessibility();
        h.get_by_role_and_label(Role::Slider, "Cutoff");
        h.get_by_role_and_label(Role::Slider, "Größe");
        h.get_by_role_and_label(Role::SpinButton, "Stimmen");
        h.get_by_role_and_label(Role::RadioButton, "Typ: LC");
        h.get_by_role_and_label(Role::RadioButton, "Typ: Sinus");
        h.get_by_role_and_label(Role::Button, "Loop");
        h.get_by_role_and_label(Role::ComboBox, "Kit");
        h.get_by_role_and_label(Role::Slider, "Op 1");
        h.get_by_role_and_label(Role::Meter, "Pegel und Gain Reduction");
        // One node per pad and per operator.
        h.get_by_role_and_label(Role::Button, "Pad Kick");
        h.get_by_role_and_label(Role::Button, "Pad Snare");
        h.get_by_role_and_label(Role::Button, "Operator 1, Träger");
        h.get_by_role_and_label(Role::Button, "Operator 2, Modulator");
    }

    #[test]
    fn clicks_change_segments_toggles_and_pads() {
        let opts = [
            SegmentOption::text("LC"),
            SegmentOption::text("LS"),
            SegmentOption::text("Gl."),
        ];
        let pal = TrackPalette::new(45);
        let pads = [Pad::new("Kick", "C1"), Pad::new("Snare", "D1")];
        let mut h = harness((0_usize, false, None::<usize>), move |ui, s| {
            ui.add(Segment::new("Typ", &opts, &mut s.0));
            ui.add(ToggleButton::new("Loop", &mut s.1));
            if let Some(c) = PadGrid::new(&pads, 2, &pal).show(ui).clicked {
                s.2 = Some(c);
            }
        });
        h.get_by_role_and_label(Role::RadioButton, "Typ: Gl.")
            .click();
        h.run();
        assert_eq!(h.state().0, 2);
        h.get_by_role_and_label(Role::Button, "Loop").click();
        h.run();
        assert!(h.state().1);
        h.get_by_role_and_label(Role::Button, "Pad Snare").click();
        h.run();
        assert_eq!(h.state().2, Some(1));
    }

    #[test]
    fn meter_click_is_reported() {
        let mut h = harness(0_u32, |ui, n| {
            if ui.add(MeterGr::new([0.0; 3], "0.0")).clicked() {
                *n += 1;
            }
        });
        h.get_by_role_and_label(Role::Meter, "Pegel und Gain Reduction")
            .click();
        h.run();
        assert_eq!(*h.state(), 1);
    }

    #[test]
    fn fm_click_selects_an_operator() {
        let pal = TrackPalette::new(150);
        let algo = Topology {
            modulates: &[0, 1, 0, 4],
            feedback: Some(3),
        };
        let mut h = harness(None::<usize>, move |ui, s| {
            if let Some(c) = FmAlgorithm::new(5, algo, &pal).show(ui).clicked {
                *s = Some(c);
            }
        });
        h.get_by_role_and_label(Role::Button, "Operator 4, Modulator")
            .click();
        h.run();
        assert_eq!(*h.state(), Some(3));
    }
}
