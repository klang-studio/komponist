//! The shell through `egui_kittest`: shortcuts, panels, edges, dock, focus, playback, the
//! accessibility tree and the snapshot next to S01.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::{Role, Toggled};
use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::ProjectAction;
use komponist_app::app::key_to_egui;
use komponist_app::keymap::{KeyScope, Keymap};
use komponist_app::ui_state::{DockTab, FocusArea, Panel, SettingsSection};
use komponist_app::{Action, App, UiAction};
use komponist_engine::TransportState;
use komponist_i18n::{Lang, de, en};
use komponist_model::Tick;
use komponist_model::fixtures::demo_project;
use komponist_tokens::size;

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
    // `COMMAND` is Ctrl on Linux and ⌘ on macOS, as in the real input.
    let mut mods = Modifiers::NONE;
    if m.command {
        mods |= Modifiers::COMMAND;
    }
    Modifiers {
        alt: m.alt,
        shift: m.shift,
        ..mods
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
            chord.label(Lang::De),
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
    h.get_by_role_and_label(Role::Button, de::SHELL_BROWSER)
        .click();
    h.run_steps(2);
    assert!(h.state().ui().layout.browser_open);
    // The inspector's header collapses it.
    h.get_by_role_and_label(Role::Button, de::SHELL_INSPECTOR)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.inspector_open);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Num2);
    h.run_steps(2);
    assert!(h.state().ui().layout.inspector_open);
    // The dock's caret collapses it to its tab strip, a tab opens it.
    h.get_by_role_and_label(Role::Button, de::DOCK_COLLAPSE)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_open);
    h.get_by_role_and_label(Role::Tab, de::DOCK_MIXER).click();
    h.run_steps(2);
    let l = h.state().ui().layout;
    assert!(l.dock_open);
    assert_eq!(l.dock_tab, DockTab::Mixer);
    h.get_by_role_and_label(Role::Button, de::DOCK_MAXIMIZE)
        .click();
    h.run_steps(2);
    assert!(h.state().ui().layout.dock_maximized);
    h.get_by_role_and_label(Role::Button, de::DOCK_MAXIMIZED)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_maximized);
}

/// The dock strip is the i18n pilot: in English its tabs, buttons and accessible names switch.
#[test]
fn dock_strip_speaks_english() {
    let mut h = harness(demo().with_lang(Lang::En));
    assert_eq!(h.state().lang(), Lang::En);
    h.get_by_role_and_label(Role::Tab, en::DOCK_DEVICES);
    h.get_by_role_and_label(Role::Tab, en::DOCK_MIXER);
    h.get_by_role_and_label(Role::Button, en::DOCK_MAXIMIZE);
    h.get_by_role_and_label(Role::Button, en::DOCK_MORE);
    h.get_by_role_and_label(Role::Button, en::DOCK_COLLAPSE)
        .click();
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_open);
    h.get_by_role_and_label(Role::Button, en::DOCK_EXPAND);
    assert!(h.query_by_label(de::DOCK_DEVICES).is_none());
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
    for name in [
        de::SHELL_BROWSER_EDGE,
        de::SHELL_INSPECTOR_EDGE,
        de::SHELL_DOCK_EDGE,
    ] {
        h.get_by_role_and_label(Role::Splitter, name);
    }
}

#[test]
fn space_plays_the_demo_through_the_null_backend() {
    let mut h = harness(demo());
    let position = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Status, de::TRANSPORT_POSITION)
            .value()
            .unwrap_or_default()
    };
    assert_eq!(position(&h), "1.1.000");
    h.key_press(egui::Key::Space);
    assert!(step_until(&mut h, |a| a.engine().playhead() > Tick(960)));
    assert_eq!(h.state().transport(), TransportState::Playing);
    h.step();
    assert_ne!(position(&h), "1.1.000", "the position advances");
    let play = h.get_by_role_and_label(Role::Button, de::TRANSPORT_PLAY);
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
        komponist_model::ClipContent::Step(_) | komponist_model::ClipContent::Audio(_) => {
            unreachable!()
        }
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
        de::TRANSPORT_PLAY,
        de::TRANSPORT_STOP,
        de::TRANSPORT_RECORD,
        de::TRANSPORT_LOOP,
        de::TRANSPORT_METRONOME,
        de::TRANSPORT_COUNT_IN,
        de::TRANSPORT_MENU,
    ] {
        h.get_by_role_and_label(Role::Button, name);
    }
    h.get_by_role_and_label(Role::SpinButton, de::TRANSPORT_TEMPO);
    h.get_by_role_and_label(Role::Meter, de::TRANSPORT_MASTER);
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
    let cpu = h
        .get_by_role_and_label(Role::Status, de::TRANSPORT_CPU)
        .rect();
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

/// The count-in button keeps its width without the bar count (owner, 2026-11-15): the icon
/// alone while it is off, the icon and "1" while it is on.
#[test]
fn the_count_in_button_shows_no_digit_while_off() {
    use komponist_app::TransportAction;
    let mut results = SnapshotResults::new();
    let mut widths = Vec::new();
    for off in [false, true] {
        for ppp in [1.0, 2.0] {
            let mut app = demo();
            for panel in [Panel::Browser, Panel::Inspector, Panel::Dock] {
                app.dispatch(Action::Ui(UiAction::TogglePanel(panel)));
            }
            if off {
                app.dispatch(Action::Transport(TransportAction::ToggleCountIn));
            }
            let mut h = harness_at(app, ppp);
            h.run_steps(3);
            mask_cpu(&mut h);
            let width = h
                .get_by_role_and_label(Role::Button, de::TRANSPORT_COUNT_IN)
                .rect()
                .width();
            if ppp == 1.0 {
                widths.push(width);
            }
            if off {
                h.snapshot(format!("shell-count-in-off-{ppp}x"));
                results.extend_harness(&mut h);
            }
        }
    }
    assert_eq!(widths[0], widths[1], "the button does not change its width");
    results.unwrap();
}

/// A project that carries a count-in of 12 bars (the edit limits new values, a loaded project may
/// not): the badge shows both digits and the button grows for them, on or off alike.
#[test]
fn the_count_in_button_has_room_for_two_digits() {
    use komponist_app::TransportAction;
    let width_of = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Button, de::TRANSPORT_COUNT_IN)
            .rect()
            .width()
    };
    let one = {
        let mut h = harness_at(demo(), 1.0);
        h.run_steps(3);
        width_of(&h)
    };
    let mut project = demo_project();
    project.count_in_bars = 12;
    let mut app = App::headless(project);
    for panel in [Panel::Browser, Panel::Inspector, Panel::Dock] {
        app.dispatch(Action::Ui(UiAction::TogglePanel(panel)));
    }
    let mut h = harness_at(app, 2.0);
    h.run_steps(3);
    mask_cpu(&mut h);
    let twelve = width_of(&h);
    assert!(twelve > one, "two digits need more room: {twelve} vs {one}");
    h.snapshot("shell-count-in-12-bars-2x");
    let mut results = SnapshotResults::new();
    results.extend_harness(&mut h);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::ToggleCountIn));
    h.run_steps(3);
    assert_eq!(width_of(&h), one, "off, the count of 1 bar is shown again");
    results.unwrap();
}

#[test]
fn an_armed_track_turns_letters_into_notes() {
    use komponist_app::{EditAction, TransportAction};
    let mut app = demo();
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let track = app.project().tracks.iter().find(|t| t.arm).unwrap().id;
    let note = |key, velocity| {
        Action::Transport(TransportAction::LiveNote {
            track: Some(track),
            key,
            velocity,
        })
    };
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
        .get_by_role_and_label(Role::Button, de::TRANSPORT_MENU)
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
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Project(ProjectAction::SaveAs))
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    open_menu(&mut h);
    h.get_by_label_contains("Zum Startbildschirm").click();
    h.run_steps(3);
    assert_eq!(
        h.state().ui().screen,
        komponist_app::ui_state::Screen::Start
    );
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
    assert_eq!(
        h.state().ui().screen,
        komponist_app::ui_state::Screen::Project
    );
    h.get_by_label("Nicht speichern").click();
    h.run_steps(3);
    assert_eq!(
        h.state().ui().screen,
        komponist_app::ui_state::Screen::Start
    );
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

/// WP-47a: Esc during any open gesture (a mixer fader here, which no canvas handles itself)
/// cancels it: `CancelGesture` through the app's remap, the selection stays.
#[test]
fn esc_cancels_an_open_gesture_for_every_canvas() {
    use komponist_app::EditAction;
    use komponist_model::edits::{ChannelProp, TrackProp};
    let mut h = harness(demo());
    let track = h.state().project().tracks[1].id;
    let before = h.state().project().tracks[1].channel.volume_db;
    h.state_mut()
        .dispatch(Action::Edit(EditAction::BeginGesture {
            label: "Lautstärke".to_owned(),
        }));
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![track],
            prop: TrackProp::Channel(ChannelProp::VolumeDb(before - 3.0)),
        }));
    assert_eq!(
        h.state().project().tracks[1].channel.volume_db,
        before - 3.0
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().project().tracks[1].channel.volume_db, before);
    assert!(!h.state().doc().in_gesture());
}

/// WP-47a: the Esc that ends a text entry is not the shortcut's: the frame after a text field
/// lost the keyboard focus skips Esc, so the selection stays.
#[test]
fn the_esc_after_a_text_field_lost_focus_is_not_a_shortcut() {
    use komponist_app::actions::{SelectMode, SelectionChange};
    let mut h = harness(demo());
    let clip = h.state().project().tracks[2].clips[0].id;
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![clip],
            mode: SelectMode::Replace,
        })));
    // Ctrl F puts the focus into the browser's search field.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::F);
    h.run_steps(3);
    assert!(
        h.ctx.egui_wants_keyboard_input(),
        "the search field has the focus"
    );
    // The field loses the focus (a click elsewhere) in the frame whose input has the Esc.
    h.ctx.memory_mut(|m| {
        if let Some(id) = m.focused() {
            m.surrender_focus(id);
        }
    });
    h.event(Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    h.event(Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
    assert!(
        h.state().ui().selection.clips.contains(&clip),
        "that Esc did not clear the selection"
    );
    assert!(
        !h.ctx.egui_wants_keyboard_input(),
        "the field stayed unfocused"
    );
    // The next Esc does (in the arrangement; the browser has its own use of Esc).
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Focus(FocusArea::Arrangement)));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().ui().selection.clips.is_empty());
}

/// The demo with plugin windows open for Surge XT (Pad), Hexa (Bass) and Dexed (Keys), in that
/// order, in `lang`.
fn with_plugin_windows(lang: Lang) -> (App, [komponist_model::DeviceId; 3]) {
    let mut app = demo().with_lang(lang);
    let device = |app: &App, track: usize| app.project().tracks[track].channel.devices[0].id;
    let devices = [device(&app, 3), device(&app, 1), device(&app, 4)];
    for d in devices {
        app.dispatch(Action::Plugin(komponist_app::PluginAction::OpenWindow(d)));
    }
    (app, devices)
}

#[test]
fn the_menu_lists_the_open_plugin_windows_under_fenster() {
    // No window open: no "Fenster" in the menu.
    let mut h = harness(demo());
    open_menu(&mut h);
    assert!(h.query_by_label(de::APP_MENU_WINDOW).is_none());

    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let (app, [surge, ..]) = with_plugin_windows(lang);
        let mut h = harness(app);
        let menu = match lang {
            Lang::De => de::TRANSPORT_MENU,
            Lang::En => en::TRANSPORT_MENU,
        };
        let at = h.get_by_role_and_label(Role::Button, menu).rect().center();
        h.event(Event::PointerMoved(at));
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        h.run_steps(60);
        h.get_by_label(lang.app_menu_window()).click();
        h.run_steps(3);
        // Project order: Bass, Pad, Keys; built-in names capitalised.
        let pad = lang.app_menu_plugin_window("Surge XT", "Pad");
        for label in [
            lang.app_menu_plugin_window("Hexa", "Bass"),
            pad.clone(),
            lang.app_menu_plugin_window("Dexed", "Keys"),
        ] {
            assert!(h.query_by_label(&label).is_some(), "{label}");
        }
        let suffix = match lang {
            Lang::De => "",
            Lang::En => "-en",
        };
        mask_cpu(&mut h);
        h.snapshot(format!("shell-menu-windows{suffix}-1x"));
        results.extend_harness(&mut h);
        // A pick brings that window forward.
        h.get_by_label(&pad).click();
        h.run_steps(3);
        assert!(
            h.state()
                .dispatched()
                .any(|a| *a == Action::Plugin(komponist_app::PluginAction::FocusWindow(surge)))
        );
    }
}
