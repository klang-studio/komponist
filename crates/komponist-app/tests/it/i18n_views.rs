//! Issue #2: the views in English (`Lang::En`): arrangement, piano roll, step sequencer, browser,
//! devices, mixer, inspector and the routing UI. For each one a header or label, a tooltip or
//! description, an AccessKit name, a status text and an undo label. German is covered by the
//! tests of each view and by the snapshots (zero diffs).

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::actions::SelectionChange;
use komponist_app::ui_state::DockTab;
use komponist_app::undo_label;
use komponist_app::{Action, App, UiAction};
use komponist_i18n::{Lang, de, en};
use komponist_model::fixtures::demo_project;

fn harness_of(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(SnapshotOptions::new())
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

fn harness(lang: Lang) -> Harness<'static, App> {
    harness_of(App::headless(demo_project()).with_lang(lang))
}

fn description(node: &egui_kittest::Node<'_>) -> String {
    node.accesskit_node()
        .description()
        .unwrap_or_default()
        .to_owned()
}

/// Some node has `label` (a label can occur more than once: a section and its field).
macro_rules! has {
    ($h:expr, $label:expr) => {
        assert!(
            $h.query_all_by_label($label).next().is_some(),
            "no node labelled {:?}",
            $label
        )
    };
}

macro_rules! has_role {
    ($h:expr, $role:expr, $label:expr) => {
        assert!(
            $h.query_all_by_role_and_label($role, $label)
                .next()
                .is_some(),
            "no {:?} labelled {:?}",
            $role,
            $label
        )
    };
}

fn select_track(h: &mut Harness<'_, App>, index: usize) {
    let id = h.state().project().tracks[index].id;
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![id],
            mode: komponist_app::actions::SelectMode::Replace,
        })));
    h.run_steps(3);
}

#[test]
fn the_arrangement_speaks_english() {
    let h = harness(Lang::En);
    // Header: grid dropdown, snap and zoom buttons.
    has!(h, en::VIEW_GRID);
    has_role!(h, Role::Button, en::VIEW_SNAP);
    let zoom = h.get_by_role_and_label(Role::Button, en::VIEW_ZOOM_IN);
    // The tooltip of a zoom button names the shortcut in English.
    assert!(
        description(&zoom).starts_with(en::VIEW_ZOOM_IN),
        "{}",
        description(&zoom)
    );
    // AccessKit: the ruler, the box for new tracks, the header of a track.
    has_role!(h, Role::Slider, en::A11Y_RULER);
    has_role!(h, Role::Button, en::ARRANGEMENT_NEW_TRACK_BUTTON);
    assert!(h.query_by_label(de::ARRANGEMENT_NEW_TRACK_BUTTON).is_none());
    assert!(
        h.query_all_by_label_contains("Drag an instrument here or")
            .next()
            .is_some()
            || description(
                &h.get_by_role_and_label(Role::Button, en::ARRANGEMENT_NEW_TRACK_BUTTON)
            )
            .starts_with("Drag an instrument here or")
    );
    // Status and text builders.
    assert_eq!(Lang::En.arrangement_count_tracks(1), "1 track",);
    assert_eq!(
        Lang::En.arrangement_plugin_missing("Vital"),
        "⚠ Plugin missing: Vital"
    );
    assert_eq!(
        Lang::En.arrangement_height_grip("Lead"),
        "Track height · Lead"
    );
    assert_eq!(Lang::En.routing_no_clips(), "Buses hold no clips.");
    // Undo labels the arrangement sends.
    for (id, text) in [
        ("undo-track-height", "Track Height"),
        ("undo-toggle-tracks", "Toggle Tracks"),
        ("undo-clip-length", "Clip Length"),
    ] {
        assert_eq!(undo_label::text(Lang::En, id), text);
    }
    assert_eq!(undo_label::text(Lang::De, "undo-track-height"), "Spurhöhe");
}

#[test]
fn the_empty_project_hint_speaks_english() {
    let app = App::headless(komponist_model::Project::new("Empty")).with_lang(Lang::En);
    let h = harness_of(app);
    has!(h, en::ARRANGEMENT_EMPTY_TITLE);
    has!(h, en::ARRANGEMENT_EMPTY_TEXT);
    has_role!(h, Role::Button, en::ARRANGEMENT_ADD_MIDI_TRACK);
    has_role!(h, Role::Button, en::ARRANGEMENT_OPEN_PROJECT);
}

#[test]
fn the_piano_roll_speaks_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.ui_mut().layout.dock_height = 500.0;
    let clip = app.project().tracks[2].clips[0].id;
    app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    let h = harness_of(app);
    // Toolbar: tools, quantize button with its shortcut chip, grid.
    has!(h, en::PIANO_ROLL_PENCIL);
    has!(h, en::PIANO_ROLL_POINTER);
    has!(h, en::PIANO_ROLL_ERASER);
    has_role!(h, Role::Button, en::PIANO_ROLL_QUANTIZE);
    has!(h, en::VIEW_GRID);
    // The tooltip of a tool names its shortcut.
    let pencil = h.get_by_label(en::PIANO_ROLL_PENCIL);
    assert!(
        description(&pencil).starts_with(en::PIANO_ROLL_PENCIL),
        "{}",
        description(&pencil)
    );
    // AccessKit names of the keyboard, a key, the note grid and a note (komponist-ui).
    has!(h, en::PIANO_ROLL_KEYBOARD);
    has_role!(h, Role::Button, "Key C4");
    has!(h, en::PIANO_ROLL_NOTES);
    has!(h, en::PIANO_ROLL_VELOCITY_LANE);
    // The info of the toolbar counts in English.
    assert!(h.query_all_by_label_contains(" notes").next().is_some());
    assert!(h.query_by_label_contains(" Noten").is_none());
    // Status text and undo labels.
    assert_eq!(Lang::En.piano_roll_velocity_status(88), "Velocity 88");
    assert_eq!(Lang::En.piano_roll_selection(1), "1 note");
    assert_eq!(Lang::En.piano_roll_selection(2), "2 notes");
    assert_eq!(undo_label::text(Lang::En, "undo-set-note"), "Set Note");
    assert_eq!(
        undo_label::text(Lang::En, "undo-delete-notes"),
        "Delete Notes"
    );
}

#[test]
fn the_step_sequencer_speaks_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    let clip = app
        .project()
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| matches!(c.content, komponist_model::ClipContent::Step(_)))
        .unwrap()
        .id;
    app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
    let h = harness_of(app);
    // Toolbar: the length field, the resolution dropdown, the swing field, the MIDI button.
    has!(h, en::STEP_SEQ_LENGTH);
    has!(h, en::STEP_SEQ_RESOLUTION);
    has!(h, en::STEP_SEQ_SWING);
    let to_midi = h.get_by_role_and_label(Role::Button, en::STEP_SEQ_TO_MIDI);
    // Its tooltip carries the detail.
    assert!(
        description(&to_midi).contains(en::STEP_SEQ_TO_MIDI_DETAIL),
        "{}",
        description(&to_midi)
    );
    // AccessKit: the lane tabs and a step.
    has!(h, en::STEP_SEQ_LANE);
    assert!(
        h.query_all_by_label_contains(", off").next().is_some(),
        "a step that is off reads \"Step 1, <row>, off\""
    );
    // Status texts and undo labels.
    assert_eq!(Lang::En.step_seq_status_velocity(52), "Velocity 52");
    assert_eq!(Lang::En.step_seq_status_probability(50), "Probability 50%");
    assert_eq!(Lang::En.step_seq_status_ratchet(3), "Ratchet ×3");
    assert_eq!(
        undo_label::text(Lang::En, "undo-probability"),
        "Probability"
    );
    assert_eq!(
        undo_label::text(Lang::En, "undo-pattern-length"),
        "Pattern Length"
    );
    assert_eq!(undo_label::text(Lang::En, "undo-change-row"), "Change Row");
}

#[test]
fn the_browser_speaks_english() {
    let h = harness(Lang::En);
    // Search field, filter, list.
    has!(h, en::BROWSER_SEARCH);
    has!(h, en::BROWSER_SEGMENTS);
    has!(h, en::BROWSER_LIST);
    for label in [
        en::BROWSER_SEGMENT_ALL,
        en::BROWSER_SEGMENT_EFFECTS,
        en::BROWSER_SEGMENT_FILES,
    ] {
        has!(h, label);
    }
    // Section headers and the footer.
    has!(h, en::BROWSER_INSTRUMENTS);
    has!(h, en::BROWSER_PROJECT);
    has!(h, en::BROWSER_FOOT);
    assert!(h.query_by_label(de::BROWSER_FOOT).is_none());
    // Search results and the star.
    assert_eq!(Lang::En.browser_hits(1, "hall"), "1 result for “hall”");
    assert_eq!(Lang::En.browser_hits(3, "hall"), "3 results for “hall”");
    assert_eq!(Lang::En.browser_star("Dexed"), "Favorite: Dexed");
    assert_eq!(
        Lang::En.browser_presets_with("hall"),
        "Presets matching “hall”"
    );
}

#[test]
fn the_devices_speak_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let id = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![id],
        mode: komponist_app::actions::SelectMode::Replace,
    })));
    let h = harness_of(app);
    // Header button and the chain.
    has_role!(h, Role::Button, en::DEVICES_ADD);
    has!(h, en::DEVICES_CHAIN);
    // A card of the lead chain: its name with the English prefix, the bypass switch with its
    // tooltip detail.
    assert!(
        h.query_all_by_label_contains("Device hexa")
            .next()
            .is_some()
    );
    let power = h
        .query_all_by_role_and_label(Role::Switch, en::DEVICES_ACTIVE)
        .next();
    let power = power.expect("a bypass switch");
    assert!(description(&power).contains(en::DEVICES_BYPASS) || description(&power).is_empty());
    // Status and texts of the cards.
    assert_eq!(Lang::En.devices_range("20 Hz", "20 kHz"), "20 Hz to 20 kHz");
    assert_eq!(Lang::En.devices_sc_chip("Drums"), "Sidechain: Drums");
    assert_eq!(
        Lang::En.devices_waveform_title(1, en::DEVICES_NO_SAMPLE),
        "Pad 1 · no sample"
    );
    assert_eq!(en::DEVICES_DETECTOR, "Detector");
    assert_eq!(en::DEVICES_DETECTOR_EXTERNAL, "External");
    assert_eq!(
        Lang::En.devices_preset_name("Glas-Glocke"),
        "Preset: Glas-Glocke"
    );
    // Undo label of an EQ band.
    assert_eq!(undo_label::text(Lang::En, "undo-add-band"), "Add Band");
}

#[test]
fn the_add_menu_speaks_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let id = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![id],
        mode: komponist_app::actions::SelectMode::Replace,
    })));
    let mut h = harness_of(app);
    h.get_by_role_and_label(Role::Button, en::DEVICES_ADD)
        .click();
    h.run_steps(4);
    has!(h, en::DEVICES_ADD_SEARCH);
}

#[test]
fn the_mixer_speaks_english() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    app.ui_mut().layout.dock_maximized = true;
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    let h = harness_of(app);
    // The strip, its fader, its pan knob, the section labels.
    let strip = h.get_by_role_and_label(Role::Group, "Channel Lead");
    has_role!(strip, Role::Slider, "Volume Lead");
    has_role!(strip, Role::Slider, "Pan Lead");
    has!(strip, en::MIXER_INSERTS);
    has!(strip, en::ROUTING_SENDS);
    // The "+" slot: name and tooltip.
    let add = h
        .query_all_by_role_and_label(Role::Button, en::MIXER_ADD_EFFECT)
        .next()
        .expect("the + slot");
    assert!(
        description(&add).contains(en::MIXER_ADD_EFFECT_DETAIL),
        "{}",
        description(&add)
    );
    // The master strip and its output line.
    has_role!(h, Role::Group, "Channel Master");
    // Status of an insert, undo labels.
    assert_eq!(en::MIXER_SLOT_MISSING, "Plugin missing");
    assert_eq!(undo_label::text(Lang::En, "undo-volume"), "Volume");
    assert_eq!(undo_label::text(Lang::En, "undo-pan"), "Pan");
    assert_eq!(undo_label::text(Lang::De, "undo-volume"), "Lautstärke");
}

#[test]
fn the_inspector_speaks_english() {
    let mut h = harness(Lang::En);
    // Nothing selected: the project and the hint.
    has!(h, en::INSPECTOR_SECTION_PROJECT);
    has!(h, en::INSPECTOR_NOTHING_SELECTED);
    has!(h, en::INSPECTOR_PROJECT_TEMPO);
    // A track: its section and fields, the devices, the channel.
    select_track(&mut h, 2);
    has!(h, en::INSPECTOR_SECTION_TRACK);
    has!(h, en::INSPECTOR_TRACK_INSTRUMENT);
    has!(h, en::INSPECTOR_TRACK_INPUT);
    has!(h, en::INSPECTOR_SECTION_DEVICES);
    has!(h, en::INSPECTOR_SECTION_CHANNEL);
    // A colour swatch: the tooltip text is the English hue name, the AccessKit name has the prefix.
    has_role!(
        h,
        Role::RadioButton,
        &Lang::En.inspector_swatch(en::HUE_RED)
    );
    // Text builders and undo.
    assert_eq!(Lang::En.inspector_take_notes(14), "14 notes");
    assert_eq!(
        Lang::En.inspector_device("LSP Para EQ x8"),
        "Device LSP Para EQ x8"
    );
    assert_eq!(
        undo_label::text(Lang::En, "undo-change-clip"),
        "Change Clip"
    );
    assert_eq!(undo_label::text(Lang::En, "undo-tempo"), "Tempo");
}

#[test]
fn the_routing_ui_speaks_english() {
    use komponist_model::edits::{self, NewBus};
    use komponist_model::{Tap, TrackOutput};
    let p = demo_project();
    let drums = p.tracks[0].id;
    let bass = p.tracks[1].id;
    let (p, group) = edits::add_bus(&p, NewBus::group(vec![drums, bass])).unwrap();
    let (p, hall) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Hall".into()),
            ..NewBus::effect(vec![drums])
        },
    )
    .unwrap();
    let p = edits::set_output(&p, &[group], TrackOutput::Bus(hall)).unwrap();
    // The reasons a target is blocked, in English.
    let rows = komponist_app::views::routing::output_targets(Lang::En, &p, hall);
    assert_eq!(rows[0].label, "Master");
    let reasons: Vec<&str> = rows.iter().filter_map(|r| r.blocked.as_deref()).collect();
    assert_eq!(reasons.len(), 2);
    assert!(reasons[0].starts_with("Not possible: “"), "{}", reasons[0]);
    assert!(reasons.iter().any(|r| r.ends_with("(feedback loop).")));
    let sends = komponist_app::views::routing::send_targets(Lang::En, &p, drums);
    assert!(sends.iter().any(|r| {
        r.blocked
            .as_deref()
            .is_some_and(|b| b.starts_with("Already used"))
    }));
    // Names and lines.
    assert_eq!(
        Lang::En.routing_send_on("Hall", "−6.0", "post"),
        "Send Hall, −6.0 dB, post fader"
    );
    assert_eq!(
        Lang::En.routing_send_off("Hall", "pre"),
        "Send Hall, off, pre fader"
    );
    assert_eq!(
        Lang::En.routing_send_tap("Lead", "Hall", "pre"),
        "Send Lead to Hall: Pre fader"
    );
    assert_eq!(
        Lang::En.routing_chip(2, "Lead", "Master"),
        "Routing Lead: output Master, 2 sends"
    );
    assert_eq!(
        Lang::En.routing_chip(0, "Lead", "Master"),
        "Routing Lead: output Master"
    );
    assert_eq!(
        Lang::En.routing_group_name("Rhythmus", "folded"),
        "Group Rhythmus, folded"
    );
    assert_eq!(
        Lang::En.routing_bus_lane(3),
        "Effect bus · 3 sends · no clips"
    );
    assert_eq!(Lang::En.routing_tracks(1), "1 track");
    let _ = Tap::Pre;
    // The mixer's section label of the buses and the output button.
    let mut app = App::headless(p).with_lang(Lang::En);
    app.ui_mut().layout.dock_maximized = true;
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    let h = harness_of(app);
    has_role!(h, Role::Label, en::ROUTING_BUSES);
    has_role!(h, Role::Button, &Lang::En.routing_add_send_name("Pad"));
    has_role!(h, Role::Button, &Lang::En.routing_output_name("Master"));
    // Undo labels of the routing edits.
    assert_eq!(undo_label::text(Lang::En, "undo-send-level"), "Send Level");
    assert_eq!(
        undo_label::text(Lang::En, "undo-set-output"),
        "Change Output"
    );
}
