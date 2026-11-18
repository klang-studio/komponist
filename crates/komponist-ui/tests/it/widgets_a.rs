//! WP-20 widgets (controls and navigation): a snapshot per component × state at 1× and 2×
//! (`tests/snapshots/<component>-<variant>-<state>-{1x,2x}.png`), the gallery cards, and the
//! interaction tests (keyboard, pointer, AccessKit).

use egui::{Key, Modifiers, Ui, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, space};
use komponist_ui::snapshot::{self, Snapshots};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{self, ContextMenu, DemoAction};
use komponist_ui::widgets::dropdown::{Dropdown, GRID_ITEMS, Item as DropItem};
use komponist_ui::widgets::panel_header::{self, PanelHeader};
use komponist_ui::widgets::switch::Switch;
use komponist_ui::widgets::tabs::{self, Item, Segmented, TabBar};
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{State, focus, state};

// ---------------------------------------------------------------------------------------------
// Snapshots

#[test]
fn button_snapshots() {
    let mut s = Snapshots::new();
    for (_, st) in State::CONTROL_STATES {
        s.add_state("button-secondary", st, None, |ui| {
            ui.add(Button::new("Quantisieren"));
        });
        s.add_state("button-primary", st, None, |ui| {
            ui.add(Button::primary("Exportieren"));
        });
        s.add_state("button-icon", st, None, |ui| {
            ui.add(Button::icon(Icon::Plus, "Hinzufügen"));
        });
    }
    for st in [State::REST, State::HOVERED] {
        s.add_state("button-on", st, None, |ui| {
            ui.add(Button::new("Snap").on(true));
        });
    }
    for st in [State::REST, State::HOVERED, State::PRESSED] {
        s.add_state("button-frameless", st, None, |ui| {
            ui.add(Button::icon(Icon::Plus, "Hinzufügen").frameless(1));
        });
    }
    s.finish();
}

fn toggles(ui: &mut Ui, on: bool, implied: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_0_5;
        for kind in ToggleKind::ALL {
            let _ = TrackToggle::new(kind, on).implied(implied).show(ui);
        }
    });
}

#[test]
fn toggle_snapshots() {
    let mut s = Snapshots::new();
    for st in [State::REST, State::HOVERED, State::DISABLED] {
        s.add_state("toggle-off", st, None, |ui| toggles(ui, false, false));
    }
    for st in [State::REST, State::HOVERED, State::DISABLED] {
        s.add_state("toggle-on", st, None, |ui| toggles(ui, true, false));
    }
    s.add_state("toggle-implied", State::REST, None, |ui| {
        let _ = TrackToggle::new(ToggleKind::Mute, false)
            .implied(true)
            .show(ui);
    });
    // I off on an armed track (it monitors anyway): the same implied look.
    s.add_state("toggle-implied-monitor", State::REST, None, |ui| {
        let _ = TrackToggle::new(ToggleKind::Monitor, false)
            .implied(true)
            .show(ui);
    });
    s.finish();
}

#[test]
fn switch_snapshots() {
    let mut s = Snapshots::new();
    for (_, st) in State::CONTROL_STATES {
        s.add_state("switch-off", st, None, |ui| {
            ui.add(Switch::new(&mut false, "Schalter"));
        });
    }
    for st in [State::REST, State::FOCUSED, State::DISABLED] {
        s.add_state("switch-on", st, None, |ui| {
            ui.add(Switch::new(&mut true, "Schalter"));
        });
    }
    s.finish();
}

#[test]
fn dropdown_snapshots() {
    let mut s = Snapshots::new();
    for (_, st) in State::CONTROL_STATES {
        s.add_state("dropdown", st, None, |ui| {
            ui.add(Dropdown::new("Raster", &mut 2, &GRID_ITEMS));
        });
    }
    s.add_state("dropdown-open", State::REST, None, |ui| {
        ui.add(Dropdown::new("Raster", &mut 2, &GRID_ITEMS).open_still(Some(3)));
    });
    s.finish();
}

#[test]
fn tab_snapshots() {
    let mut s = Snapshots::new();
    for st in [State::REST, State::HOVERED, State::FOCUSED] {
        s.add_state("tab-active", st, None, |ui| {
            tabs::tab(ui, "Piano Roll", true, true);
        });
    }
    for (_, st) in State::CONTROL_STATES {
        s.add_state("tab-inactive", st, None, |ui| {
            tabs::tab(ui, "Step Sequencer", false, true);
        });
    }
    for st in [State::REST, State::HOVERED] {
        s.add_state("segment-active", st, None, |ui| {
            tabs::segment(ui, "Arrangement", true, true);
        });
    }
    for (_, st) in State::CONTROL_STATES {
        s.add_state("segment-inactive", st, None, |ui| {
            tabs::segment(ui, "Mixer", false, true);
        });
    }
    // `Segmented::equal_width`: the browser's segments, four equal columns over 244 px
    // (S01: browser 260 − 2 × 8 padding).
    s.add_state("segmented-equal-width", State::REST, None, |ui| {
        ui.set_width(244.0);
        let mut selected = 0;
        let items = [
            tabs::Item::new("Alle"),
            tabs::Item::new("Instr."),
            tabs::Item::new("Effekte"),
            tabs::Item::new("Dateien"),
        ];
        ui.add(tabs::Segmented::new("Browser-Filter", &mut selected, &items).equal_width());
    });
    assert_eq!(tabs::equal_segment_width(244.0, 4), 58.5);
    for (name, ring) in [("segmented", false), ("segmented-focused", true)] {
        s.add_state(name, State::REST, None, |ui| {
            tabs::segmented_frame(
                ui,
                "Werkzeug",
                |_| ring,
                |ui| {
                    tabs::segment(ui, "Zeiger", false, true);
                    tabs::segment(ui, "Stift", true, true);
                    tabs::segment(ui, "Radierer", false, true);
                },
            );
        });
    }
    s.finish();
}

fn header(ui: &mut Ui, title: &str, open: bool, focused: bool) {
    ui.set_width(320.0);
    let mut open = open;
    PanelHeader::new(title, &mut open)
        .focused(focused)
        .shortcut("Strg 2")
        .show(ui, |ui| {
            panel_header::action(ui, Icon::Ellipsis, "Mehr");
            panel_header::action(ui, Icon::Plus, "Hinzufügen");
        });
}

#[test]
fn panel_header_snapshots() {
    let mut s = Snapshots::new();
    for st in [State::REST, State::HOVERED, State::PRESSED, State::FOCUSED] {
        s.add_state("panel-header-open", st, None, |ui| {
            header(ui, "Browser", true, false);
        });
    }
    s.add_state("panel-header-collapsed", State::REST, None, |ui| {
        header(ui, "Inspector", false, false);
    });
    s.add_state("panel-header-panel-focused", State::REST, None, |ui| {
        header(ui, "Piano Roll · Lead A", true, true);
    });
    s.finish();
}

#[test]
fn context_menu_snapshots() {
    let swatches = context_menu::track_swatches();
    let mut s = Snapshots::new();
    let entries = context_menu::demo_entries(&swatches, Some(5), true);
    let menu = ContextMenu::new("Clip", &entries);
    s.add_state("context-menu", State::REST, None, |ui| {
        menu.show_still(ui, None, None);
    });
    s.add_state("context-menu-highlight-colors", State::REST, None, |ui| {
        menu.show_still(ui, Some(4), Some(6));
    });
    let sub = [
        context_menu::Entry::action("Takt", 0).shortcut("1"),
        context_menu::Entry::action("Schlag", 1).disabled(),
    ];
    let with_sub = [
        context_menu::Entry::action("Kopieren", 2).shortcut("Strg C"),
        context_menu::Entry::submenu("Raster", &sub),
    ];
    s.add_state("context-menu-submenu", State::REST, None, |ui| {
        ContextMenu::new("Raster", &with_sub).show_still(ui, None, Some(1));
    });
    s.finish();
}

#[test]
fn tooltip_snapshots() {
    let mut s = Snapshots::new();
    s.add("tooltip-shortcut", None, |ui| {
        Tip::new("Quantisieren").shortcut("Q").show_inline(ui);
    });
    s.add("tooltip-two-lines", None, |ui| {
        Tip::new("Solo")
            .shortcut("S")
            .detail("Alt-Klick: nur diese Spur solo")
            .show_inline(ui);
    });
    s.finish();
}

/// The gallery cards of WP-20 at the design's card width (560 + padding).
#[test]
fn gallery_card_snapshots() {
    use komponist_ui::widgets::{button, context_menu, dropdown, panel_header, tabs, toggle};
    let cards: [(&str, komponist_ui::gallery_shell::CardContent); 7] = [
        ("card-button", button::gallery),
        ("card-toggle", toggle::gallery),
        ("card-dropdown", dropdown::gallery),
        ("card-tabs", tabs::gallery),
        ("card-panel-header", panel_header::gallery),
        ("card-context-menu", context_menu::gallery),
        ("card-tooltip", komponist_ui::widgets::tooltip::gallery),
    ];
    let mut s = Snapshots::new();
    for (name, card) in cards {
        s.add(name, None, |ui| {
            // A card of the design page: surface.1, padding 20, content 560 wide or more.
            egui::Frame::new()
                .fill(color32(color::SURFACE_1))
                .inner_margin(egui::Margin::same((space::SPACE_4 + space::SPACE_1) as i8))
                .show(ui, |ui| {
                    ui.set_width(612.0);
                    card(ui);
                });
        });
    }
    s.finish();
}

// ---------------------------------------------------------------------------------------------
// Interaction

fn h<S>(
    size: egui::Vec2,
    state: S,
    app: impl FnMut(&mut Ui, &mut S) + 'static,
) -> egui_kittest::Harness<'static, S> {
    let mut h = snapshot::builder::<S>(1.0)
        .with_size(size)
        .build_ui_state(app, state);
    h.run();
    h
}

#[derive(Default)]
struct Clicks {
    n: u32,
    state: State,
}

#[test]
fn button_keyboard_and_tooltip() {
    let mut h = h(vec2(300.0, 120.0), Clicks::default(), |ui, s| {
        let r = snapshot::padded(ui, |ui| ui.add(Button::new("Quantisieren").shortcut("Q")));
        s.n += u32::from(r.clicked());
        s.state = State::of(ui, &r);
    });
    h.key_press(Key::Tab);
    h.run();
    assert!(h.state().state.focused, "Tab focuses, ring visible");
    h.key_press(Key::Enter);
    h.run();
    h.key_press(Key::Space);
    h.run();
    assert_eq!(h.state().n, 2, "Enter and Space click");

    // Tooltip after 600 ms of hover, with the shortcut.
    let node = h.get_by_label("Quantisieren");
    node.hover();
    h.run_steps(20);
    assert!(h.query_by_label_contains("Quantisieren Q").is_none());
    h.run_steps(30);
    assert!(
        h.query_all_by_role(egui::accesskit::Role::Tooltip).count() > 0,
        "tooltip shown after the delay"
    );
    let desc = h
        .get_by_label("Quantisieren")
        .accesskit_node()
        .description();
    assert_eq!(desc.as_deref(), Some("Quantisieren · Q"));
}

#[derive(Default)]
struct Tracks {
    mute: [bool; 3],
    solo: [bool; 3],
    events: Vec<(usize, ToggleEvent)>,
}

/// Three tracks, each with M and S; the "arrangement" applies the toggle events.
fn tracks_ui(ui: &mut Ui, t: &mut Tracks) {
    let mut rows = Vec::new();
    let mut events = Vec::new();
    snapshot::padded(ui, |ui| {
        for i in 0..3 {
            let row = ui.horizontal(|ui| {
                ui.set_height(32.0);
                let name = format!("Spur {i}");
                let implied = t.solo.iter().any(|&s| s) && !t.solo[i];
                let m = TrackToggle::new(ToggleKind::Mute, t.mute[i])
                    .implied(implied)
                    .track(&name)
                    .show(ui);
                let s = TrackToggle::new(ToggleKind::Solo, t.solo[i])
                    .track(&name)
                    .show(ui);
                (m.event, s.event)
            });
            rows.push(row.response.rect);
            let (m, s) = row.inner;
            events.extend(m.map(|e| (i, e)));
            events.extend(s.map(|e| (3 + i, e)));
        }
    });
    // Apply them like the arrangement will.
    for (who, e) in events {
        let (flags, i) = if who < 3 {
            (&mut t.mute, who)
        } else {
            (&mut t.solo, who - 3)
        };
        match e {
            ToggleEvent::Set { value, exclusive } => {
                if exclusive {
                    *flags = [false; 3];
                }
                flags[i] = value;
            }
            ToggleEvent::Sweep { value, from, to } => {
                let (lo, hi) = (from.y.min(to.y), from.y.max(to.y));
                for (r, row) in rows.iter().enumerate() {
                    if row.max.y >= lo && row.min.y <= hi {
                        flags[r] = value;
                    }
                }
            }
        }
        t.events.push((who, e));
    }
}

#[test]
fn toggles_press_sweep_and_alt() {
    let mut h = h(vec2(200.0, 160.0), Tracks::default(), tracks_ui);
    let m0 = h.get_by_label("Mute · Spur 0").rect().center();
    let m2 = h.get_by_label("Mute · Spur 2").rect().center();

    // A press toggles at once.
    h.drag_at(m0);
    h.run();
    assert_eq!(h.state().mute, [true, false, false]);
    assert_eq!(
        h.state().events[0],
        (
            0,
            ToggleEvent::Set {
                value: true,
                exclusive: false
            }
        )
    );
    // Dragging down sweeps the value over the other tracks.
    h.hover_at(m2);
    h.run();
    h.drop_at(m2);
    h.run();
    assert_eq!(
        h.state().mute,
        [true, true, true],
        "sweep reached all three"
    );
    let sweeps: Vec<_> = h
        .state()
        .events
        .iter()
        .filter(|(_, e)| matches!(e, ToggleEvent::Sweep { .. }))
        .collect();
    assert!(!sweeps.is_empty());
    if let ToggleEvent::Sweep { value, from, to } = sweeps[0].1 {
        assert!(value);
        assert_eq!(from, m0);
        assert_eq!(to, m2);
    }

    // Alt-click on Solo: only this track.
    h.get_by_label("Solo · Spur 1").click();
    h.run();
    h.get_by_label("Solo · Spur 2")
        .click_modifiers(Modifiers::ALT);
    h.run();
    assert_eq!(h.state().solo, [false, false, true]);
    assert_eq!(
        h.state().events.last().map(|e| e.1),
        Some(ToggleEvent::Set {
            value: true,
            exclusive: true
        })
    );
    // Alt-click on Mute is a plain toggle.
    h.get_by_label("Mute · Spur 1")
        .click_modifiers(Modifiers::ALT);
    h.run();
    assert_eq!(h.state().mute, [true, false, true]);

    // Press and release within one pass (a quick click on a slow frame) flips once.
    let m1 = h.get_by_label("Mute · Spur 1").rect().center();
    h.hover_at(m1);
    h.run();
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: m1,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run();
    assert_eq!(h.state().mute, [true, true, true]);

    // AccessKit click flips once.
    h.get_by_label("Solo · Spur 0").click_accesskit();
    h.run();
    assert_eq!(h.state().solo, [true, false, true]);
    // A plain click (press + release without move) flips once, not twice.
    h.get_by_label("Solo · Spur 0").click();
    h.run();
    assert_eq!(h.state().solo, [false, false, true]);
}

#[test]
fn switch_click_and_space() {
    let mut h = h(vec2(120.0, 60.0), false, |ui, on| {
        snapshot::padded(ui, |ui| ui.add(Switch::new(on, "Normalisieren")));
    });
    h.get_by_label("Normalisieren").click();
    h.run();
    assert!(*h.state());
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Space);
    h.run();
    assert!(!*h.state());
    let node = h.get_by_label("Normalisieren");
    assert_eq!(node.accesskit_node().role(), egui::accesskit::Role::Switch);
}

#[derive(Default)]
struct Pick {
    value: usize,
    changes: u32,
}

fn dropdown_ui(ui: &mut Ui, p: &mut Pick) {
    snapshot::padded(ui, |ui| {
        let r = ui.add(Dropdown::new("Raster", &mut p.value, &GRID_ITEMS));
        p.changes += u32::from(r.changed());
        ui.add_space(200.0);
        ui.label("unten");
    });
}

fn is_open<S>(h: &egui_kittest::Harness<'_, S>) -> bool {
    h.query_by_role(egui::accesskit::Role::ListBox).is_some()
}

#[test]
fn dropdown_ignores_enter_after_a_port_list_refresh_reorders_options() {
    let mut h = h(
        vec2(300.0, 320.0),
        (
            Pick {
                value: 0,
                changes: 0,
            },
            false,
        ),
        |ui, (pick, refreshed)| {
            let ids = if *refreshed {
                [None, Some("USB:A"), Some("USB:B")]
            } else {
                [None, Some("USB:B"), Some("USB:C")]
            };
            let items = ["Automatic", "USB", "USB"].map(DropItem::new);
            let response = ui.add(Dropdown::new("Ports", &mut pick.value, &items).option_ids(&ids));
            pick.changes += u32::from(response.changed());
        },
    );
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(is_open(&h));
    h.key_press(Key::ArrowDown);
    h.run();
    h.state_mut().1 = true;
    h.key_press(Key::Enter);
    h.run();
    assert!(!is_open(&h));
    assert_eq!(h.state().0.changes, 0);
    assert_eq!(h.state().0.value, 0);

    h.key_press(Key::Enter);
    h.run();
    h.key_press(Key::ArrowDown);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().0.value, 1);
    assert_eq!(h.state().0.changes, 1);
}

#[test]
fn a_fitting_dropdown_reveals_and_can_select_its_last_port() {
    let mut h = h(
        vec2(300.0, 240.0),
        Pick {
            value: 0,
            changes: 0,
        },
        |ui, pick| {
            let names: Vec<_> = (0..64).map(|port| format!("Port {port:02}")).collect();
            let items: Vec<_> = names.iter().map(|name| DropItem::new(name)).collect();
            let response =
                ui.add(Dropdown::new("Ports", &mut pick.value, &items).fitting_list(120.0, &[]));
            pick.changes += u32::from(response.changed());
        },
    );
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    h.event(egui::Event::Text("Port 63".into()));
    h.run_steps(4);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 240.0));
    let last_port = |h: &egui_kittest::Harness<'_, Pick>| {
        h.get_by_role_and_label(egui::accesskit::Role::ListBoxOption, "Port 63")
            .rect()
    };
    assert!(viewport.contains_rect(last_port(&h)));

    h.hover_at(last_port(&h).center());
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 2_000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    // Expire the prefix without moving the pointer or selecting a different row.
    h.run_steps(90);
    assert!(!viewport.contains_rect(last_port(&h)));

    h.event(egui::Event::Text("Port 63".into()));
    h.run_steps(4);
    assert!(viewport.contains_rect(last_port(&h)));
    h.get_by_role_and_label(egui::accesskit::Role::ListBoxOption, "Port 63")
        .click();
    h.run();
    assert_eq!(h.state().value, 63);
    assert_eq!(h.state().changes, 1);
}

#[test]
fn dropdown_keyboard() {
    let mut h = h(
        vec2(300.0, 320.0),
        Pick {
            value: 2,
            changes: 0,
        },
        dropdown_ui,
    );
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(is_open(&h), "Enter opens");
    // ↓ moves to 1/32, ↓ to 1/16 Triole, ↓ stops before the disabled "Adaptiv".
    for _ in 0..3 {
        h.key_press(Key::ArrowDown);
        h.run();
    }
    h.key_press(Key::Enter);
    h.run();
    assert!(!is_open(&h), "Enter takes the entry and closes");
    assert_eq!(h.state().value, 4);
    assert_eq!(h.state().changes, 1);

    // Esc closes without a change; focus stays on the field.
    h.key_press(Key::Space);
    h.run();
    assert!(is_open(&h));
    h.key_press(Key::ArrowUp);
    h.run();
    h.key_press(Key::Escape);
    h.run();
    assert!(!is_open(&h));
    assert_eq!(h.state().value, 4);
    assert!(h.get_by_label("Raster").is_focused(), "Esc keeps the focus");

    // Typing in the open list jumps to the first match.
    h.key_press(Key::ArrowDown);
    h.run();
    assert!(is_open(&h), "↓ opens the focused field");
    h.event(egui::Event::Text("1/3".into()));
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().value, 3, "1/3 → 1/32");

    // Typing on the closed field chooses directly.
    h.event(egui::Event::Text("1/8".into()));
    h.run();
    assert_eq!(h.state().value, 1);
}

#[test]
fn dropdown_pointer_and_wheel() {
    let mut h = h(
        vec2(300.0, 320.0),
        Pick {
            value: 2,
            changes: 0,
        },
        dropdown_ui,
    );
    h.get_by_label("Raster").click();
    h.run();
    assert!(is_open(&h));
    // Disabled entries cannot be chosen.
    h.get_by_label("Adaptiv").click();
    h.run();
    assert_eq!(h.state().value, 2);
    h.get_by_label("1/4").click();
    h.run();
    assert_eq!(h.state().value, 0);
    assert!(!is_open(&h));

    // A press outside closes.
    h.get_by_label("Raster").click();
    h.run();
    assert!(is_open(&h));
    h.get_by_label("unten").click();
    h.run();
    assert!(!is_open(&h));

    // Wheel over the closed field: down = next, up = previous.
    let field = h.get_by_label("Raster").rect().center();
    let wheel = |h: &mut egui_kittest::Harness<'_, Pick>, y: f32| {
        h.hover_at(field);
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        });
        h.run();
    };
    wheel(&mut h, -1.0);
    assert_eq!(h.state().value, 1);
    wheel(&mut h, -1.0);
    wheel(&mut h, -1.0);
    wheel(&mut h, -1.0);
    wheel(&mut h, -1.0);
    assert_eq!(h.state().value, 4, "stops before the disabled entry");
    wheel(&mut h, 1.0);
    assert_eq!(h.state().value, 3);
}

#[test]
fn dropdown_open_snapshot() {
    // The floating list as it opens in the app.
    let mut h = snapshot::builder::<Pick>(1.0)
        .with_size(vec2(160.0, 240.0))
        .build_ui_state(
            dropdown_ui,
            Pick {
                value: 2,
                changes: 0,
            },
        );
    h.run();
    h.get_by_label("Raster").click();
    h.run();
    h.get_by_label("1/32").hover();
    h.run();
    h.snapshot("dropdown-floating-open-1x");
}

#[derive(Default)]
struct Views {
    tab: usize,
    tool: usize,
}

const TABS: [Item<'static>; 4] = [
    Item::new("Piano Roll"),
    Item::new("Step Sequencer"),
    Item::new("Geräte").disabled(),
    Item::new("Automation"),
];
const TOOLS: [Item<'static>; 3] = [
    Item::new("Zeiger"),
    Item::new("Stift"),
    Item::new("Radierer"),
];

#[test]
fn tabs_and_segments_keys() {
    let mut h = h(vec2(500.0, 80.0), Views::default(), |ui, v| {
        snapshot::padded(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(TabBar::new("Editor", &mut v.tab, &TABS));
                ui.add(Segmented::new("Werkzeug", &mut v.tool, &TOOLS));
            });
        });
    });
    h.get_by_label("Step Sequencer").click();
    h.run();
    assert_eq!(h.state().tab, 1);
    // Only the active tab is a Tab stop; → skips the disabled tab and keeps the focus.
    h.key_press(Key::Tab);
    h.run();
    assert!(h.get_by_label("Step Sequencer").is_focused());
    h.key_press(Key::ArrowRight);
    h.run();
    assert_eq!(h.state().tab, 3);
    assert!(h.get_by_label("Automation").is_focused());
    h.key_press(Key::ArrowRight);
    h.run();
    assert_eq!(h.state().tab, 0, "wraps around");
    // Next Tab stop: the active segment.
    h.key_press(Key::Tab);
    h.run();
    assert!(h.get_by_label("Zeiger").is_focused());
    h.key_press(Key::ArrowLeft);
    h.run();
    assert_eq!(h.state().tool, 2);
    h.get_by_label("Stift").click();
    h.run();
    assert_eq!(h.state().tool, 1);
    let tab = h.get_by_label("Piano Roll");
    assert_eq!(tab.accesskit_node().role(), egui::accesskit::Role::Tab);
    assert!(
        h.get_by_role(egui::accesskit::Role::TabList)
            .accesskit_node()
            .label()
            .is_some()
    );
}

#[derive(Default)]
struct Panel {
    open: bool,
    actions: u32,
}

#[test]
fn panel_header_toggles() {
    let mut h = h(
        vec2(320.0, 60.0),
        Panel {
            open: true,
            actions: 0,
        },
        |ui, p| {
            snapshot::padded(ui, |ui| {
                PanelHeader::new("Browser", &mut p.open)
                    .shortcut("Strg 1")
                    .show(ui, |ui| {
                        if panel_header::action(ui, Icon::Plus, "Ordner hinzufügen").clicked() {
                            p.actions += 1;
                        }
                    });
            });
        },
    );
    h.get_by_label("Ordner hinzufügen").click();
    h.run();
    assert_eq!(
        (h.state().open, h.state().actions),
        (true, 1),
        "an action does not collapse"
    );
    h.get_by_label("Browser").click();
    h.run();
    assert!(!h.state().open);
    assert!(
        h.query_by_label("Ordner hinzufügen").is_none(),
        "no actions when collapsed"
    );
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().open, "Enter expands");
    assert_eq!(
        h.get_by_label("Browser")
            .accesskit_node()
            .data()
            .is_expanded(),
        Some(true)
    );
}

#[derive(Default)]
struct MenuTest {
    hue: usize,
    chosen: Vec<DemoAction>,
}

fn menu_ui(ui: &mut Ui, m: &mut MenuTest) {
    let swatches = context_menu::track_swatches();
    let entries = context_menu::demo_entries(&swatches, Some(m.hue), true);
    snapshot::padded(ui, |ui| {
        let (_, target) = ui.allocate_exact_size(vec2(120.0, 40.0), egui::Sense::click());
        target.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Clip Beat A"));
        if let Some(a) = ContextMenu::new("Clip", &entries).show_on(&target) {
            if let DemoAction::Color(h) = a {
                m.hue = h;
            }
            m.chosen.push(a);
        }
        ui.add_space(300.0);
        ui.label("woanders");
    });
}

fn menu_open<S>(h: &egui_kittest::Harness<'_, S>) -> bool {
    h.query_by_label("Ausschneiden").is_some()
}

#[test]
fn context_menu_keys() {
    let mut h = h(
        vec2(600.0, 420.0),
        MenuTest {
            hue: 5,
            chosen: vec![],
        },
        menu_ui,
    );
    let target = h.get_by_label("Clip Beat A").rect().center();
    h.get_by_label("Clip Beat A").click_secondary();
    h.run();
    assert!(menu_open(&h));
    // Opens at the pointer.
    let menu_rect = h.get_by_label("Ausschneiden").rect();
    assert!(
        (menu_rect.left() - target.x).abs() < 12.0 && (menu_rect.top() - target.y).abs() < 12.0
    );

    // ↓ ↓ ↓ skips the disabled "Einfügen" and the separator: Duplizieren.
    for _ in 0..3 {
        h.key_press(Key::ArrowDown);
        h.run();
    }
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().chosen, [DemoAction::Duplicate]);
    assert!(!menu_open(&h), "running an entry closes the menu");

    // → on "Farbe" opens the colour grid at the current colour; → → Enter picks two further.
    h.get_by_label("Clip Beat A").click_secondary();
    h.run();
    for _ in 0..5 {
        h.key_press(Key::ArrowDown);
        h.run();
    }
    h.key_press(Key::ArrowRight);
    h.run();
    assert!(h.query_by_label("Grün").is_some(), "colour grid open");
    h.key_press(Key::ArrowRight);
    h.run();
    h.key_press(Key::ArrowRight);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().chosen.last(), Some(&DemoAction::Color(7)));
    assert_eq!(h.state().hue, 7);

    // Esc closes the submenu first, then the menu.
    h.get_by_label("Clip Beat A").click_secondary();
    h.run();
    for _ in 0..5 {
        h.key_press(Key::ArrowDown);
        h.run();
    }
    h.key_press(Key::ArrowRight);
    h.run();
    h.key_press(Key::Escape);
    h.run();
    assert!(menu_open(&h) && h.query_by_label("Grün").is_none());
    h.key_press(Key::Escape);
    h.run();
    assert!(!menu_open(&h));
}

#[test]
fn context_menu_pointer() {
    let mut h = h(
        vec2(600.0, 420.0),
        MenuTest {
            hue: 5,
            chosen: vec![],
        },
        menu_ui,
    );
    h.get_by_label("Clip Beat A").click_secondary();
    h.run();
    h.get_by_label("Einfügen").click();
    h.run();
    assert!(
        h.state().chosen.is_empty() && menu_open(&h),
        "disabled entries do nothing"
    );
    h.get_by_label("Farbe").hover();
    h.run();
    h.get_by_label("Rosa").click();
    h.run();
    assert_eq!(h.state().chosen, [DemoAction::Color(13)]);
    assert!(!menu_open(&h));
    // A press outside closes.
    h.get_by_label("Clip Beat A").click_secondary();
    h.run();
    h.get_by_label("woanders").click();
    h.run();
    assert!(!menu_open(&h));
    assert_eq!(h.state().chosen.len(), 1);
}

#[test]
fn context_menu_open_snapshot() {
    let mut h = snapshot::builder::<MenuTest>(1.0)
        .with_size(vec2(520.0, 380.0))
        .build_ui_state(
            menu_ui,
            MenuTest {
                hue: 5,
                chosen: vec![],
            },
        );
    h.run();
    h.get_by_label("Clip Beat A").click_secondary();
    h.run();
    h.get_by_label("Farbe").hover();
    h.run();
    h.snapshot("context-menu-floating-open-1x");
}

/// Forced samples are pictures: the gallery's Tab order reaches only live widgets.
#[test]
fn forced_samples_take_no_focus() {
    let mut h = h(vec2(400.0, 80.0), (), |ui, ()| {
        snapshot::padded(ui, |ui| {
            ui.horizontal(|ui| {
                state::force(ui, State::HOVERED, |ui| {
                    ui.add(Button::new("Probe"));
                    ui.add(Switch::new(&mut true, "Probe-Schalter"));
                });
                ui.add(Button::new("Echt"));
            });
        });
    });
    h.key_press(Key::Tab);
    h.run();
    assert!(h.get_by_label("Echt").is_focused());
    assert!(!h.get_by_label("Probe").is_focused());
    assert!(focus::is_visible(&h.ctx));
}
