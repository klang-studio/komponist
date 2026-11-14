//! WP-47a widget variants: the toolbar fields and buttons of the editors.
//!
//! - `NumberField::key` ("Länge 16 Steps"), `Dropdown::label` / `compact` ("Raster 1/16"),
//!   `Button::chip` ("Quantisieren Q"), the small compact and the 28 / 32 px dialog buttons,
//!   `Segmented::tips`: sizes, text, interaction, snapshots at 1× and 2×.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Ui, Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_tokens::size;
use komponist_ui::snapshot::{self, Snapshots};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::dropdown::{Dropdown, GRID_ITEMS, Item};
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::tabs::Segmented;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{State, widgets};

const STEP: f32 = 1.0 / 60.0;

fn harness<S>(state: S, app: impl FnMut(&mut Ui, &mut S) + 'static) -> Harness<'static, S> {
    let mut app = app;
    let mut h = snapshot::builder::<S>(1.0)
        .with_size(vec2(420.0, 160.0))
        .with_step_dt(STEP)
        .build_ui_state(move |ui, s| snapshot::padded(ui, |ui| app(ui, s)), state);
    h.run();
    h
}

fn pause<S>(h: &mut Harness<'_, S>) {
    for _ in 0..30 {
        h.step();
    }
}

fn drag<S>(h: &mut Harness<'_, S>, from: Pos2, by: Vec2, steps: usize) {
    pause(h);
    h.event(Event::PointerMoved(from));
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    for i in 1..=steps {
        h.event(Event::PointerMoved(from + by * (i as f32 / steps as f32)));
    }
    h.event(Event::PointerButton {
        pos: from + by,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
}

fn double_click<S>(h: &mut Harness<'_, S>, at: Pos2) {
    pause(h);
    h.event(Event::PointerMoved(at));
    for pressed in [true, false, true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run();
}

fn length_field(v: &mut i32) -> NumberField<'_> {
    NumberField::new(v, "Länge")
        .key("Länge")
        .unit("Steps")
        .range(1.0..=32.0)
        .default_value(16.0)
        .width(108.0)
}

// ---------------------------------------------------------------------------------------------
// Number field with a key

#[test]
fn keyed_number_field_shows_value_with_unit_in_accesskit() {
    let h = harness(16, |ui, v| {
        ui.add(length_field(v));
    });
    let node = h.get_by_label("Länge");
    assert_eq!(
        node.accesskit_node().role(),
        egui::accesskit::Role::SpinButton
    );
    assert_eq!(node.value().as_deref(), Some("16 Steps"));
    assert_eq!(node.rect().height(), size::CONTROL);
    assert_eq!(node.rect().width(), 108.0);
}

#[test]
fn keyed_number_field_drags_and_types() {
    let mut h = harness(16, |ui, v| {
        ui.add(length_field(v));
    });
    let c = h.get_by_label("Länge").rect().center();
    drag(&mut h, c, vec2(0.0, -10.0), 10);
    assert_eq!(*h.state(), 26, "1 px = 1 step, up increases");
    // Text entry starts right of the key; Enter applies.
    double_click(&mut h, c);
    let entry = h.get_by_label("Länge Eingabe");
    assert!(entry.is_focused());
    assert!(
        entry.rect().left() > h.get_by_label("Länge").rect().left() + 20.0,
        "the entry starts after the key"
    );
    h.event(Event::Text("7".to_owned()));
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(*h.state(), 7);
}

#[test]
fn field_without_text_entry_ignores_double_click_and_enter() {
    let mut h = harness(4, |ui, v| {
        ui.add(
            NumberField::new(v, "Note")
                .key("Note")
                .range(0.0..=127.0)
                .no_text_entry()
                .format(|v| format!("N{v}"))
                .width(84.0),
        );
    });
    let c = h.get_by_label("Note").rect().center();
    double_click(&mut h, c);
    assert!(h.query_by_label("Note Eingabe").is_none());
    h.key_press(Key::Enter);
    h.run();
    assert!(h.query_by_label("Note Eingabe").is_none());
    assert_eq!(h.get_by_label("Note").value().as_deref(), Some("N4"));
}

// ---------------------------------------------------------------------------------------------
// Dropdown with a label, compact

#[test]
fn dropdown_label_and_compact_variants() {
    #[derive(Default)]
    struct S {
        a: usize,
        b: usize,
        plain: usize,
        rects: Vec<egui::Rect>,
    }
    let mut h = harness(
        S {
            a: 2,
            b: 2,
            plain: 2,
            rects: vec![],
        },
        |ui, s| {
            ui.horizontal(|ui| {
                let r1 = ui.add(Dropdown::new("Plain", &mut s.plain, &GRID_ITEMS));
                let r2 = ui.add(Dropdown::new("Mit Label", &mut s.a, &GRID_ITEMS).label("Raster"));
                let r3 = ui.add(
                    Dropdown::new("Kompakt", &mut s.b, &GRID_ITEMS)
                        .label("Raster")
                        .compact(),
                );
                s.rects = vec![r1.rect, r2.rect, r3.rect];
            });
        },
    );
    let [plain, labelled, compact] = [0, 1, 2].map(|i| h.state().rects[i]);
    assert_eq!(plain.height(), size::CONTROL);
    assert_eq!(labelled.height(), size::CONTROL);
    assert_eq!(compact.height(), size::CONTROL_COMPACT);
    assert!(
        labelled.width() > plain.width() - 1.0,
        "the label makes it wider"
    );
    // The accessible name stays the dropdown's; the value is "1/16".
    let node = h.get_by_label("Mit Label");
    assert_eq!(node.value().as_deref(), Some("1/16"));
    // It still works: the wheel over the field picks the next value.
    h.get_by_label("Kompakt").click();
    h.run();
    assert!(h.query_by_role(egui::accesskit::Role::ListBox).is_some());
    h.get_by_label("1/8").click();
    h.run();
    assert_eq!(h.state().b, 1);
}

// ---------------------------------------------------------------------------------------------
// Buttons

#[test]
fn button_sizes_and_chip() {
    #[derive(Default)]
    struct S {
        rects: Vec<egui::Rect>,
    }
    let mut h = harness(S::default(), |ui, s| {
        ui.horizontal(|ui| {
            let r = [
                ui.add(Button::new("Quantisieren")),
                ui.add(Button::new("Quantisieren · Q").chip("Q")),
                ui.add(Button::new("Snap").compact().small()),
                ui.add(Button::new("Abbrechen").dialog()),
                ui.add(Button::new("Öffnen").large().width(180.0).chip("Strg O")),
            ];
            s.rects = r.iter().map(|r| r.rect).collect();
        });
    });
    h.run();
    let r = &h.state().rects;
    assert_eq!(r[0].height(), size::CONTROL);
    assert_eq!(r[2].height(), size::CONTROL_COMPACT);
    assert_eq!(r[3].height(), 28.0);
    assert_eq!(r[4].height(), 32.0);
    assert_eq!(r[4].width(), 180.0);
    assert!(
        r[1].width() > r[0].width() + 8.0,
        "the chip adds its width and the gap"
    );
    // The chip is not part of the accessible name.
    assert!(h.query_by_label("Quantisieren · Q").is_some());
}

#[test]
fn ground_lifts_a_secondary_button_one_step() {
    use komponist_tokens::color;
    use komponist_tokens::egui_support::color32;
    let mut h = snapshot::builder::<()>(1.0)
        .with_size(vec2(160.0, 60.0))
        .build_ui(|ui| {
            snapshot::padded(ui, |ui| {
                ui.add(Button::new("Abbrechen").dialog().ground(3));
            });
        });
    h.run();
    let img = h.render().expect("render");
    let rect = h.get_by_label("Abbrechen").rect();
    let p = img.get_pixel(rect.left() as u32 + 3, rect.center().y as u32);
    let want = color32(color::SURFACE_4);
    assert_eq!([p[0], p[1], p[2]], [want.r(), want.g(), want.b()]);
}

// ---------------------------------------------------------------------------------------------
// Segmented with tooltips

#[test]
fn segment_tooltips_show_the_shortcut_after_the_delay() {
    let items = [
        Item::new("Zeiger"),
        Item::new("Stift"),
        Item::new("Radierer"),
    ];
    let tips = [
        Tip::new("Zeiger").shortcut("1"),
        Tip::new("Stift").shortcut("2"),
        Tip::new("Radierer").shortcut("3"),
    ];
    let mut h = harness(0_usize, move |ui, v| {
        ui.add(Segmented::new("Werkzeug", v, &items).tips(&tips));
    });
    h.get_by_label("Stift").hover();
    h.run_steps(20);
    assert_eq!(
        h.query_all_by_role(egui::accesskit::Role::Tooltip).count(),
        0
    );
    h.run_steps(30);
    assert!(h.query_all_by_role(egui::accesskit::Role::Tooltip).count() > 0);
    let desc = h.get_by_label("Stift").accesskit_node().description();
    assert_eq!(desc.as_deref(), Some("Stift · 2"));
}

// ---------------------------------------------------------------------------------------------
// Snapshots

#[test]
fn variant_snapshots() {
    let mut s = Snapshots::new();
    for (name, st) in [
        ("rest", State::REST),
        ("hover", State::HOVERED),
        ("focused", State::FOCUSED),
    ] {
        let _ = name;
        s.add_state("number-field-keyed", st, None, |ui| {
            let mut v = 16;
            ui.add(length_field(&mut v));
        });
        s.add_state("dropdown-label", st, None, |ui| {
            let mut v = 2;
            ui.add(Dropdown::new("Raster", &mut v, &GRID_ITEMS).label("Raster"));
        });
    }
    s.add_state("dropdown-label-compact", State::REST, None, |ui| {
        let mut v = 2;
        ui.add(
            Dropdown::new("Raster", &mut v, &GRID_ITEMS)
                .label("Raster")
                .compact(),
        );
    });
    s.add_state("button-chip", State::REST, None, |ui| {
        ui.add(Button::new("Quantisieren").chip("Q"));
    });
    s.add_state("button-small-on", State::REST, None, |ui| {
        ui.add(Button::new("Snap").compact().small().on(true));
    });
    s.add_state("button-dialog", State::REST, None, |ui| {
        ui.horizontal(|ui| {
            ui.add(Button::new("Abbrechen").dialog().ground(3).chip("Esc"));
            ui.add(Button::primary("Exportieren").dialog().chip("Enter"));
        });
    });
    s.add_state("button-large", State::REST, None, |ui| {
        ui.add(
            Button::primary("Neues Projekt")
                .large()
                .ground(2)
                .width(280.0)
                .chip("Strg N"),
        );
    });
    s.finish();
    let _ = widgets::metrics::MEDIUM;
}
