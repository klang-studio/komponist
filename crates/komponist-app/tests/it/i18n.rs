//! WP-73: the app shell in English (`Lang::En`): transport bar, menu, status bar, status messages
//! and undo labels. German is covered by the tests next to the code and by the snapshots (zero
//! diffs); the dock pilot's English test lives in `shell.rs`.

use std::time::Instant;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::actions::{EditAction, ProjectAction};
use komponist_app::panels::statusbar;
use komponist_app::platform::Platform;
use komponist_app::ui_state::{FocusArea, StatusKind};
use komponist_app::undo_label;
use komponist_app::{Action, App, UiAction};
use komponist_i18n::{Lang, de, en};
use komponist_model::fixtures::demo_project;

fn harness(lang: Lang) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(SnapshotOptions::new())
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, app: &mut App| app.show(ui),
            App::headless(demo_project()).with_lang(lang),
        );
    h.run_steps(2);
    h
}

fn status(h: &Harness<'_, App>) -> statusbar::StatusModel {
    statusbar::model(h.state(), Instant::now())
}

#[test]
fn the_transport_bar_speaks_english() {
    let h = harness(Lang::En);
    for name in [en::TRANSPORT_PLAY, en::TRANSPORT_STOP, en::TRANSPORT_RECORD] {
        h.get_by_role_and_label(Role::Button, name);
    }
    h.get_by_label(en::TRANSPORT_LOOP);
    h.get_by_label(en::TRANSPORT_METRONOME);
    h.get_by_label(en::TRANSPORT_COUNT_IN);
    h.get_by_role_and_label(Role::Button, en::TRANSPORT_MENU);
    h.get_by_role_and_label(Role::Status, en::TRANSPORT_POSITION);
    h.get_by_role_and_label(Role::Status, en::TRANSPORT_TIME);
    h.get_by_role_and_label(Role::Status, en::TRANSPORT_METER);
    // The readout has a "%" without a space in English.
    let cpu = h.get_by_role_and_label(Role::Status, en::TRANSPORT_CPU);
    let value = cpu.accesskit_node().value().unwrap_or_default().to_owned();
    assert!(value.ends_with('%') && !value.ends_with(" %"), "{value}");
    assert!(h.query_by_label(de::TRANSPORT_PLAY).is_none());
    assert!(h.query_by_label(de::TRANSPORT_METER).is_none());
}

/// A transport tooltip: the English name and the English shortcut ("Space").
#[test]
fn a_transport_tooltip_names_the_shortcut_in_english() {
    let h = harness(Lang::En);
    let play = h.get_by_role_and_label(Role::Button, en::TRANSPORT_PLAY);
    let description = play
        .accesskit_node()
        .description()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(description, format!("{} · Space", en::TRANSPORT_PLAY));
    let de_h = harness(Lang::De);
    let play = de_h.get_by_role_and_label(Role::Button, de::TRANSPORT_PLAY);
    let description = play
        .accesskit_node()
        .description()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(description, format!("{} · Leertaste", de::TRANSPORT_PLAY));
}

#[test]
fn the_menu_items_are_title_case_in_english() {
    let mut h = harness(Lang::En);
    let at = h
        .get_by_role_and_label(Role::Button, en::TRANSPORT_MENU)
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
    h.run_steps(4);
    for item in [
        "New Project",
        "Open Project…",
        "Save",
        "Save As…",
        "Collect and Save",
        "Export WAV…",
        "Go to Start Screen",
    ] {
        assert!(
            h.query_all_by_label_contains(item).next().is_some(),
            "{item}"
        );
    }
    h.get_by_label_contains("Export WAV…").click();
    h.run_steps(3);
    assert!(matches!(
        h.state().ui().dialog,
        Some(komponist_app::ui_state::Dialog::Export(_))
    ));
}

#[test]
fn the_panels_and_the_empty_dock_speak_english() {
    let mut h = harness(Lang::En);
    h.get_by_role_and_label(Role::Label, en::SHELL_ARRANGEMENT);
    h.get_by_role_and_label(Role::Splitter, en::SHELL_DOCK_EDGE);
    h.get_by_role_and_label(Role::Splitter, en::SHELL_BROWSER_EDGE);
    h.state_mut().dispatch(Action::Ui(UiAction::SetDockTab(
        komponist_app::ui_state::DockTab::Devices,
    )));
    h.run_steps(2);
    h.get_by_label(en::SHELL_NO_TRACK);
    h.get_by_label(en::SHELL_NO_TRACK_HINT);
}

#[test]
fn the_status_bar_follows_the_language() {
    let mut h = harness(Lang::En);
    let lead = h.state().project().tracks[2].clone();
    h.state_mut()
        .dispatch(Action::Ui(UiAction::OpenClip(lead.clips[0].id)));
    h.run_steps(2);
    assert_eq!(h.state().ui().focus, FocusArea::PianoRoll);
    let s = status(&h);
    assert!(s.left.starts_with("Lead A · 0 of "), "{}", s.left);
    assert!(s.left.ends_with(" notes selected"), "{}", s.left);
    let hint = Platform::current().pick(
        "Ctrl+drag a note: velocity · Alt+drag: copy",
        "⌘+drag a note: velocity · Alt+drag: copy",
    );
    assert_eq!(s.mid, hint);
    // The null backend of the headless app: the output line says "samples" and the chip text.
    assert_eq!(s.right, "No audio device · 48 kHz · 256 samples · 5.3 ms");
    assert_eq!(en::STATUS_AUDIO_STOPPED, "Audio stopped");
    assert_eq!(Lang::En.status_clips_selected(1), "1 clip selected");
    assert_eq!(Lang::En.status_clips_selected(4), "4 clips selected");
    assert_eq!(Lang::En.status_devices("Lead", 1), "Lead · 1 device");
    assert_eq!(Lang::En.status_devices("Lead", 3), "Lead · 3 devices");
}

#[test]
fn status_messages_are_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.dispatch(Action::Project(ProjectAction::New));
    let text = app.ui().status.as_ref().unwrap().text.clone();
    let created = Platform::current().pick(
        "New project created · Ctrl+T adds a MIDI track",
        "New project created · ⌘T adds a MIDI track",
    );
    assert_eq!(text, created);
    // R without an armed or selected track.
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.set_status(Lang::En.record_no_track(), StatusKind::Error);
    assert_eq!(
        app.ui().status.as_ref().unwrap().text,
        "No track armed · R in the track header"
    );
    assert_eq!(Lang::En.status_midi_connected("HDA"), "HDA connected",);
    assert_eq!(Lang::En.status_midi_disconnected("HDA"), "HDA disconnected",);
    assert_eq!(
        Lang::En.status_state_load_failed("Surge XT"),
        "Surge XT: state could not be loaded"
    );
    assert_eq!(
        Lang::En.record_recorded("Take 1", 14),
        "Take 1 recorded · 14 notes · Q quantizes"
    );
    assert_eq!(
        Lang::En.record_recorded("Take 1", 1),
        "Take 1 recorded · 1 note · Q quantizes"
    );
    assert_eq!(
        Lang::En.status_collected(2, "Night Drive Samples"),
        "Collected and saved · 2 samples in “Night Drive Samples”"
    );
}

#[test]
fn a_missing_project_file_says_why_in_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    let dir = std::env::temp_dir().join(format!("komponist-i18n-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Missing.komponist");
    app.dispatch(Action::Project(ProjectAction::OpenPath(path)));
    let text = app.ui().status.as_ref().unwrap().text.clone();
    assert_eq!(
        text,
        "“Missing.komponist” could not be opened: file not found"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An undo label is an id; the app resolves it in the UI language, also after a switch.
#[test]
fn undo_labels_follow_the_language() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.dispatch(Action::Edit(EditAction::QuantizeSelection));
    // Nothing selected: no step. Duplicate nothing either. Add a track instead.
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let label = app.doc().undo_label().unwrap().to_owned();
    assert_eq!(label, "undo-new-midi-track");
    assert_eq!(app.undo_text().as_deref(), Some("New MIDI Track"));
    let app = app.with_lang(Lang::De);
    assert_eq!(app.undo_text().as_deref(), Some("Neue MIDI-Spur"));
    // A label with a name in it.
    assert_eq!(
        undo_label::text(Lang::En, &undo_label::param("Cutoff")),
        "Change Cutoff"
    );
}

#[test]
fn the_macos_hints_name_command_and_the_linux_hints_name_ctrl() {
    for t in Lang::ALL {
        let mac = [
            t.project_new_created_mac(),
            t.status_notes_hint_mac(),
            t.status_clips_hint_mac(),
            t.status_empty_hint_mac(),
            t.routing_popover_hint_mac(),
        ];
        let pc = [
            t.project_new_created(),
            t.status_notes_hint(),
            t.status_clips_hint(),
            t.status_empty_hint(),
            t.routing_popover_hint(),
        ];
        for (mac, pc) in mac.into_iter().zip(pc) {
            assert!(mac.contains('⌘'), "{t:?}: {mac}");
            assert!(
                !mac.contains("Ctrl") && !mac.contains("Strg"),
                "{t:?}: {mac}"
            );
            assert!(
                !pc.contains('⌘') && (pc.contains("Ctrl") || pc.contains("Strg")),
                "{t:?}: {pc}"
            );
        }
    }
}
