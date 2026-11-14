//! The shell through `egui_kittest`: shortcuts, panels, edges, dock, focus, playback, the
//! accessibility tree and the snapshot next to S01.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::{Role, Toggled};
use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::app::key_to_egui;
use komponist_app::keymap::{KeyScope, Keymap};
use komponist_app::panels::dock::names as dock_names;
use komponist_app::panels::transport::names as tp;
use komponist_app::ui_state::{DockTab, FocusArea, Panel, SettingsSection};
use komponist_app::actions::ProjectAction;
use komponist_app::{Action, App, UiAction, names};
use komponist_engine::TransportState;
use komponist_model::Tick;
use komponist_model::fixtures::demo_project;
use komponist_tokens::size;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn harness_at(app: App, ppp: f32) -> Harness<'static, App> {
    komponist_ui::snapshot::builder::<App>(ppp)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_max_steps(64)
        // Frames of a 60 Hz display, so two clicks fit egui's double-click window.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = harness_at(app, 1.0);
    h.run_steps(2);
    h
}

fn demo() -> App {
    App::headless(demo_project())
}

/// Steps the harness until `done` holds (the null backend runs on its own thread).
fn step_until(h: &mut Harness<'_, App>, mut done: impl FnMut(&App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        h.step();
        if done(h.state()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn egui_mods(m: komponist_app::Modifiers) -> Modifiers {
    Modifiers {
        alt: m.alt,
        ctrl: m.ctrl,
        shift: m.shift,
        mac_cmd: false,
        command: m.ctrl,
    }
}

#[test]
fn every_shortcut_dispatches_its_action() {
    let keymap = Keymap::default_bindings();
    for binding in keymap.bindings() {
        let mut app = demo();
        let area = match binding.scope {
            KeyScope::Global | KeyScope::Canvases => FocusArea::Arrangement,
            KeyScope::Area(area) => area,
        };
        if area == FocusArea::Dialog {
            app.dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
        } else {
            app.dispatch(Action::Ui(UiAction::Focus(area)));
        }
        let mut h = harness(app);
        h.state_mut().clear_dispatched();
        let chord = binding.chord;
        h.key_press_modifiers(egui_mods(chord.mods), key_to_egui(chord.key));
        h.run_steps(2);
        let got: Vec<&Action> = h.state().dispatched().collect();
        assert!(
            got.contains(&&binding.action),
            "{} ({:?}) should dispatch {:?}, got {got:?}",
            chord.label(),
            binding.scope,
            binding.action
        );
    }
}

#[test]
fn tab_toggles_the_mixer_only_from_a_canvas() {
    let mut h = harness(demo());
    // Click into the inspector: Tab moves the widget focus there, the layout stays.
    let inspector = pos2(W - 100.0, 500.0);
    click_at(&mut h, inspector);
    assert_eq!(h.state().ui().focus, FocusArea::Inspector);
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    assert_eq!(h.state().ui().layout.dock_tab, DockTab::Editor);
    assert!(!h.state().ui().layout.dock_maximized);
    // Click into the arrangement: Tab maximises the mixer, Tab again restores.
    click_at(&mut h, pos2(800.0, 300.0));
    assert_eq!(h.state().ui().focus, FocusArea::Arrangement);
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    let l = h.state().ui().layout;
    assert_eq!(l.dock_tab, DockTab::Mixer);
    assert!(l.dock_maximized);
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_maximized);
}

fn click_at(h: &mut Harness<'_, App>, pos: Pos2) {
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(2);
}

fn double_click_at(h: &mut Harness<'_, App>, pos: Pos2) {
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.step();
    }
    h.run_steps(2);
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    let steps = 6;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        h.hover_at(from + (to - from) * t);
        h.step();
    }
    h.drop_at(to);
    h.run_steps(2);
}

#[test]
fn panels_collapse_and_open_again() {
    let mut h = harness(demo());
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Num1);
    h.run_steps(2);
    assert!(!h.state().ui().layout.browser_open);
    // The rail opens it again.
    h.get_by_role_and_label(Role::Button, names::BROWSER)
        .click();
    h.run_steps(2);
    assert!(h.state().ui().layout.browser_open);
    // The inspector's header collapses it.
    h.get_by_role_and_label(Role::Button, names::INSPECTOR)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.inspector_open);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Num2);
    h.run_steps(2);
    assert!(h.state().ui().layout.inspector_open);
    // The dock's caret collapses it to its tab strip, a tab opens it.
    h.get_by_role_and_label(Role::Button, dock_names::COLLAPSE)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_open);
    h.get_by_role_and_label(Role::Tab, dock_names::MIXER)
        .click();
    h.run_steps(2);
    let l = h.state().ui().layout;
    assert!(l.dock_open);
    assert_eq!(l.dock_tab, DockTab::Mixer);
    h.get_by_role_and_label(Role::Button, dock_names::MAXIMIZE)
        .click();
    h.run_steps(2);
    assert!(h.state().ui().layout.dock_maximized);
    h.get_by_role_and_label(Role::Button, dock_names::MAXIMIZED)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_maximized);
}

#[test]
fn edges_resize_panels_and_double_click_resets() {
    let mut h = harness(demo());
    let edge_y = 500.0;
    // Browser: right edge at 260 (+ the 1 px gap).
    drag(&mut h, pos2(260.5, edge_y), pos2(330.0, edge_y));
    assert_eq!(h.state().ui().layout.browser_width, 330.0);
    drag(&mut h, pos2(330.5, edge_y), pos2(900.0, edge_y));
    assert_eq!(h.state().ui().layout.browser_width, size::browser::MAX);
    double_click_at(&mut h, pos2(size::browser::MAX + 0.5, edge_y));
    assert_eq!(h.state().ui().layout.browser_width, size::browser::DEFAULT);
    // Inspector: left edge at 1920 − 280.
    drag(&mut h, pos2(1639.5, edge_y), pos2(1600.0, edge_y));
    assert_eq!(h.state().ui().layout.inspector_width, 320.0);
    // Dock: top edge at 1057 − 320; 70 % of the window at most.
    let top = 1057.0 - size::dock::DEFAULT - 0.5;
    drag(&mut h, pos2(900.0, top), pos2(900.0, 600.0));
    assert_eq!(h.state().ui().layout.dock_height, 457.0);
    drag(&mut h, pos2(900.0, 599.5), pos2(900.0, 60.0));
    assert_eq!(h.state().ui().layout.dock_height, H * 0.7);
    double_click_at(&mut h, pos2(900.0, 1057.0 - H * 0.7 - 0.5));
    assert_eq!(h.state().ui().layout.dock_height, size::dock::DEFAULT);
    // The edges are splitters with names.
    for name in [names::BROWSER_EDGE, names::INSPECTOR_EDGE, names::DOCK_EDGE] {
        h.get_by_role_and_label(Role::Splitter, name);
    }
}

#[test]
fn space_plays_the_demo_through_the_null_backend() {
    let mut h = harness(demo());
    let position = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Status, tp::POSITION)
            .value()
            .unwrap_or_default()
    };
    assert_eq!(position(&h), "1.1.000");
    h.key_press(egui::Key::Space);
    assert!(step_until(&mut h, |a| a.engine().playhead() > Tick(960)));
    assert_eq!(h.state().transport(), TransportState::Playing);
    h.step();
    assert_ne!(position(&h), "1.1.000", "the position advances");
    let play = h.get_by_role_and_label(Role::Button, tp::PLAY);
    assert_eq!(play.accesskit_node().toggled(), Some(Toggled::True));
    h.key_press(egui::Key::Space);
    assert!(step_until(&mut h, |a| a.transport() == TransportState::Stopped));
}

#[test]
fn clicks_focus_areas_and_the_status_bar_follows() {
    let mut h = harness(demo());
    let lead = h.state().project().tracks[2].clone();
    h.state_mut()
        .dispatch(Action::Ui(UiAction::OpenClip(lead.clips[0].id)));
    h.run_steps(2);
    // The editor tab carries the clip name, the editor kind and the accent line (focused).
    let tab = h.get_by_role_and_label(Role::Tab, "Lead A · Piano Roll");
    assert_eq!(tab.accesskit_node().is_selected(), Some(true));
    assert_eq!(h.state().ui().focus, FocusArea::PianoRoll);
    let notes = match &lead.clips[0].content {
        komponist_model::ClipContent::Midi(m) => m.active().notes.len(),
        komponist_model::ClipContent::Step(_) => unreachable!(),
    };
    h.get_by_role_and_label(
        Role::Label,
        &format!("Lead A · 0 von {notes} Noten ausgewählt"),
    );
    click_at(&mut h, pos2(800.0, 300.0));
    assert_eq!(h.state().ui().focus, FocusArea::Arrangement);
    h.get_by_role_and_label(Role::Label, "1 Clip ausgewählt");
    // The null backend of the headless app.
    h.get_by_role_and_label(
        Role::Label,
        "Kein Audio-Gerät · 48 kHz · 256 Samples · 5.3 ms",
    );
}

#[test]
fn every_interactive_node_has_a_german_name() {
    let mut app = demo();
    let lead = app.project().tracks[2].clone();
    app.dispatch(Action::Ui(UiAction::OpenClip(lead.clips[0].id)));
    let h = harness(app);
    let interactive = [
        Role::Button,
        Role::Tab,
        Role::Splitter,
        Role::SpinButton,
        Role::RadioButton,
        Role::CheckBox,
        Role::Meter,
    ];
    let mut seen = 0;
    for node in h.root().children_recursive() {
        let n = node.accesskit_node();
        if interactive.contains(&n.role()) {
            seen += 1;
            let label = n.label().unwrap_or_default();
            assert!(!label.trim().is_empty(), "{:?} without a name", n.role());
        }
    }
    assert!(seen >= 15, "only {seen} interactive nodes");
    for name in [
        tp::PLAY,
        tp::STOP,
        tp::RECORD,
        tp::LOOP,
        tp::METRONOME,
        tp::COUNT_IN,
        tp::MENU,
    ] {
        h.get_by_role_and_label(Role::Button, name);
    }
    h.get_by_role_and_label(Role::SpinButton, tp::TEMPO);
    h.get_by_role_and_label(Role::Meter, tp::MASTER);
}

#[test]
fn panel_sizes_are_saved_in_the_settings() {
    let dir = std::env::temp_dir().join(format!("komponist-shell-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("settings.json");
    let engine = komponist_app::engine_sync::EngineSync::null(Box::new(
        komponist_app::device_host::BuiltinHost::new(),
    ));
    let app = App::new(
        komponist_model::Document::new(demo_project()),
        engine,
        komponist_app::settings::Settings::default(),
        Some(path.clone()),
    );
    let mut h = harness(app);
    h.state_mut().dispatch(Action::Ui(UiAction::ResizePanel {
        panel: Panel::Browser,
        size: 300.0,
    }));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Num3);
    h.run_steps(2);
    let saved = komponist_app::settings::Settings::load(&path).unwrap();
    assert_eq!(saved.panels.browser_width, 300.0);
    assert!(!saved.panels.dock_open);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The S01 state: Lead A open in the piano roll (focused), playhead at 10.2.120, stopped.
fn s01() -> App {
    let mut app = demo();
    let lead = app.project().tracks[2].clone();
    app.dispatch(Action::Ui(UiAction::OpenClip(lead.clips[0].id)));
    app.dispatch(Action::Transport(komponist_app::TransportAction::Locate(
        Tick(36_000),
    )));
    app
}

/// The CPU readout depends on the machine; masked in the snapshots.
fn mask_cpu(h: &mut Harness<'_, App>) {
    // Label, bar and value.
    let cpu = h.get_by_role_and_label(Role::Status, tp::CPU).rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshots_of_the_shell() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let mut h = harness_at(s01(), ppp);
        assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(36_000)));
        h.run_steps(2);
        mask_cpu(&mut h);
        h.snapshot(format!("shell-s01-{ppp}x"));
        results.extend_harness(&mut h);
    }

    // Everything collapsed: rails and the 28 px tab strip.
    let mut app = demo();
    for panel in [Panel::Browser, Panel::Inspector, Panel::Dock] {
        app.dispatch(Action::Ui(UiAction::TogglePanel(panel)));
    }
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    mask_cpu(&mut h);
    h.snapshot("shell-collapsed-1x");
    results.extend_harness(&mut h);

    // S04: Tab from the arrangement, mixer maximised, Lead selected.
    let mut app = demo();
    let lead = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(
        komponist_app::actions::SelectionChange::Tracks {
            tracks: vec![lead],
            mode: komponist_app::actions::SelectMode::Replace,
        },
    )));
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    mask_cpu(&mut h);
    h.snapshot("shell-mixer-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

#[test]
fn an_armed_track_turns_letters_into_notes() {
    use komponist_app::{EditAction, TransportAction};
    let note = |key, velocity| {
        Action::Transport(TransportAction::LiveNote {
            track: None,
            key,
            velocity,
        })
    };
    let mut app = demo();
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let mut h = harness(app);
    h.state_mut().clear_dispatched();
    h.key_press(egui::Key::A);
    h.key_press(egui::Key::Z);
    h.key_press(egui::Key::L);
    h.run_steps(2);
    let got: Vec<Action> = h.state().dispatched().cloned().collect();
    assert!(
        got.contains(&note(60, 100)) && got.contains(&note(60, 0)),
        "{got:?}"
    );
    // Z plays G# instead of fitting the view (ARCHITECTURE §13.2).
    assert!(got.contains(&note(68, 100)));
    assert!(!got.contains(&Action::Ui(UiAction::ZoomToFit)));
    // Keys that are no notes keep their shortcut.
    assert!(got.contains(&Action::Transport(TransportAction::ToggleLoop)));
}

/// Opens the "⋯" menu as egui-mcp clicks: move, press and release in one batch.
fn open_menu(h: &mut Harness<'_, App>) {
    let at = h
        .get_by_role_and_label(Role::Button, tp::MENU)
        .rect()
        .center();
    h.event(Event::PointerMoved(at));
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    // Longer than the tooltip delay: the menu stays open.
    h.run_steps(60);
}

#[test]
fn the_menu_saves_under_a_new_name_and_goes_to_the_start_screen() {
    let mut h = harness(demo());
    open_menu(&mut h);
    h.get_by_label_contains("Speichern unter").click();
    h.run_steps(3);
    assert!(h.state().dispatched().any(|a| *a == Action::Project(ProjectAction::SaveAs)));
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    open_menu(&mut h);
    h.get_by_label_contains("Zum Startbildschirm").click();
    h.run_steps(3);
    assert_eq!(h.state().ui().screen, komponist_app::ui_state::Screen::Start);
}

#[test]
fn the_start_screen_asks_before_leaving_unsaved_changes() {
    let mut app = demo();
    app.dispatch(Action::Edit(komponist_app::EditAction::NewMidiTrack));
    assert!(app.doc().is_dirty());
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::ShowStartScreen));
    h.run_steps(3);
    h.get_by_value("Änderungen speichern?");
    assert_eq!(h.state().ui().screen, komponist_app::ui_state::Screen::Project);
    h.get_by_label("Nicht speichern").click();
    h.run_steps(3);
    assert_eq!(h.state().ui().screen, komponist_app::ui_state::Screen::Start);
}

#[test]
fn the_menu_opens_the_export_dialog() {
    let mut h = harness(demo());
    open_menu(&mut h);
    h.get_by_label_contains("WAV exportieren").click();
    h.run_steps(3);
    assert!(matches!(
        h.state().ui().dialog,
        Some(komponist_app::ui_state::Dialog::Export(_))
    ));
    assert_eq!(h.state().ui().focus, FocusArea::Dialog);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().ui().dialog.is_none());
}

#[test]
fn a_missing_bridge_executable_is_reported_in_the_status_bar() {
    let mut app = App::headless(komponist_model::fixtures::demo_project());
    app.report_missing_bridge(std::path::Path::new("/nowhere/komponist-bridge"));
    let status = app.ui().status.clone().expect("a status message");
    assert!(status.text.starts_with("komponist-bridge fehlt"), "{}", status.text);
    assert_eq!(status.kind, komponist_app::ui_state::StatusKind::Error);
}
