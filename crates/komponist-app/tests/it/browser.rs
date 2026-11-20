//! The browser (WP-44) through `egui_kittest`: segments, Strg F search (typing filters at once,
//! ↑ ↓, Enter adds to the selected track, Esc), favourites (persisted in the settings), folders,
//! double-click, drags (an egui payload of type `PluginRef`, taken by the arrangement and the
//! device chain), presets and takes, and the snapshot next to the S01 browser.

use crate::fake_host;

use std::path::PathBuf;

use egui::accesskit::{Role, Toggled};
use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::settings::Settings;
use komponist_app::ui_state::{BrowserSegment, DockTab, FocusArea};
use komponist_app::{Action, App, EditAction, PluginAction, SettingsAction, UiAction};
use komponist_i18n::{Lang, de};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipId, PluginRef};

use fake_host::{FakeHost, plugin, screens_catalog};

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
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    h
}

/// The demo with the S01/S05 catalog; Dragonfly Hall Reverb starred as in S01.
fn app() -> App {
    let mut app = App::headless(demo_project()).with_device_host(Box::new(FakeHost::new()));
    app.dispatch(Action::Settings(SettingsAction::ToggleFavorite(plugin(
        &screens_catalog(),
        "hall",
    ))));
    app
}

fn select_track(app: &mut App, index: usize) {
    let track = app.project().tracks[index].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshot_next_to_s01() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let mut app = app();
        // S01 marks hexa in "Instrumente" (`sel:1`) and hovers Surge XT.
        app.ui_mut().browser.highlighted = Some(5);
        let mut h = harness_at(app, ppp);
        h.run_steps(2);
        let surge = h.get_by_role_and_label(Role::ListItem, "Surge XT").rect();
        h.hover_at(surge.center());
        h.run_steps(2);
        mask_cpu(&mut h);
        h.snapshot(format!("browser-s01-{ppp}x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// Pointer and key helpers

fn press(h: &mut Harness<'_, App>, pos: Pos2, button: PointerButton, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

fn click_at(h: &mut Harness<'_, App>, pos: Pos2) {
    // Half a second apart, so two clicks are no double-click.
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    press(h, pos, PointerButton::Primary, true);
    press(h, pos, PointerButton::Primary, false);
    h.run_steps(2);
}

fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        press(h, pos, PointerButton::Primary, true);
        press(h, pos, PointerButton::Primary, false);
        h.step();
    }
    h.run_steps(2);
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    press(h, from, PointerButton::Primary, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, PointerButton::Primary, false);
    h.run_steps(3);
}

fn key(h: &mut Harness<'_, App>, key: egui::Key) {
    h.key_press(key);
    h.run_steps(2);
}

fn row(h: &Harness<'_, App>, name: &str) -> egui::Rect {
    h.get_by_role_and_label(Role::ListItem, name).rect()
}

fn row_names(h: &Harness<'_, App>) -> Vec<String> {
    h.get_by_role_and_label(Role::List, de::BROWSER_LIST)
        .get_all_by_role(Role::ListItem)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect()
}

fn adds(app: &App) -> Vec<(PluginRef, DropTarget)> {
    app.dispatched()
        .filter_map(|a| match a {
            Action::Plugin(PluginAction::Add { plugin, target }) => {
                Some((plugin.clone(), target.clone()))
            }
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------
// Segments, sections, folders

#[test]
fn segments_filter_the_list() {
    let mut h = harness(app());
    let segments = h.get_by_role_and_label(Role::RadioGroup, de::BROWSER_SEGMENTS);
    let instr = segments
        .get_by_role_and_label(Role::RadioButton, "Instr.")
        .rect();
    click_at(&mut h, instr.center());
    assert_eq!(h.state().ui().browser.segment, BrowserSegment::Instruments);
    let rows = row_names(&h);
    assert!(rows.contains(&"Surge XT".to_owned()));
    assert!(!rows.iter().any(|r| r.starts_with("LSP")));
    let fx = h.get_by_role_and_label(Role::RadioButton, "Effekte").rect();
    click_at(&mut h, fx.center());
    let rows = row_names(&h);
    assert!(rows.contains(&"Dragonfly Hall Reverb".to_owned()));
    assert!(!rows.contains(&"Surge XT".to_owned()));
    h.get_by_role_and_label(Role::Label, "Hall und Delay");
    let files = h.get_by_role_and_label(Role::RadioButton, "Dateien").rect();
    click_at(&mut h, files.center());
    h.get_by_role_and_label(Role::Label, de::BROWSER_NO_TAKES);
    h.get_by_role_and_label(Role::Label, de::BROWSER_NO_EXPORTS);
}

#[test]
fn folders_open_and_close() {
    let mut h = harness(app());
    // S01: the category folders start closed.
    assert!(!row_names(&h).contains(&"Dragonfly Plate Reverb".to_owned()));
    let folder = h.get_by_role_and_label(Role::ListItem, "Hall und Delay");
    assert_eq!(folder.accesskit_node().data().is_expanded(), Some(false));
    let r = folder.rect();
    click_at(&mut h, r.center());
    let rows = row_names(&h);
    let i = rows.iter().position(|r| r == "Hall und Delay").unwrap();
    assert_eq!(
        rows[i + 1..i + 6],
        [
            "Delay",
            "Hall",
            "Dragonfly Early Reflections",
            "Dragonfly Hall Reverb",
            "Dragonfly Plate Reverb"
        ]
    );
    assert_eq!(
        h.get_by_role_and_label(Role::ListItem, "Hall und Delay")
            .accesskit_node()
            .data()
            .is_expanded(),
        Some(true)
    );
    click_at(&mut h, r.center());
    assert!(!row_names(&h).contains(&"Dragonfly Plate Reverb".to_owned()));
}

#[test]
fn large_file_browser_creates_visible_nodes_and_scrolls_to_a_keyboard_mark() {
    let directory = tempfile::tempdir().unwrap();
    let folder_count = 120;
    let folder_name = |index| format!("Library {index:03}");
    for index in 0..folder_count {
        std::fs::create_dir(directory.path().join(folder_name(index))).unwrap();
    }
    let mut app = app();
    app.ui_mut().browser.segment = BrowserSegment::Files;
    app.ui_mut().browser.directory = Some(directory.path().to_path_buf());
    let mut h = harness(app);
    let first = folder_name(0);
    let last = folder_name(folder_count - 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while h.query_by_role_and_label(Role::ListItem, &first).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "directory was not listed"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        h.run_steps(2);
    }
    assert!(h.query_by_role_and_label(Role::ListItem, &last).is_none());

    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::F);
    h.run_steps(2);
    assert!(
        h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
            .is_focused()
    );
    // The first three file rows are the header, current path and parent folder.
    let last_index = 3 + folder_count - 1;
    h.state_mut().ui_mut().browser.highlighted = Some(last_index - 1);
    key(&mut h, egui::Key::ArrowDown);
    h.run_steps(48);
    assert_eq!(h.state().ui().browser.highlighted, Some(last_index));
    let selected = h.get_by_role_and_label(Role::ListItem, &last);
    assert_eq!(selected.accesskit_node().is_selected(), Some(true));
    assert!(h.query_by_role_and_label(Role::ListItem, &first).is_none());
    key(&mut h, egui::Key::Enter);
    assert_eq!(
        h.state().ui().browser.directory.as_deref(),
        Some(directory.path().join(last).as_path())
    );
}

// ------------------------------------------------------------------------------------------------
// S05 "Browser-Suche": Strg F, typing filters at once, ↑ ↓ choose, Enter adds to the selected track.

#[test]
fn search_types_filters_moves_and_adds_to_the_selected_track() {
    let mut app = app();
    select_track(&mut app, 3);
    let pad = app.project().tracks[3].id;
    let mut h = harness(app);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::F);
    h.run_steps(2);
    assert_eq!(h.state().ui().focus, FocusArea::Browser);
    assert!(
        h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
            .is_focused()
    );
    h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
        .type_text("hall");
    h.run_steps(2);
    assert_eq!(h.state().ui().browser.query, "hall");
    h.get_by_role_and_label(Role::Heading, &Lang::De.browser_hits(2, "hall"));
    // The first hit is marked: the internal "Hall".
    let first = h.get_by_role_and_label(Role::ListItem, "Hall");
    assert_eq!(first.accesskit_node().is_selected(), Some(true));
    // ↓ marks the next hit, Enter adds it to Pad.
    key(&mut h, egui::Key::ArrowDown);
    let next = h.get_by_role_and_label(Role::ListItem, "Dragonfly Hall Reverb");
    assert_eq!(next.accesskit_node().is_selected(), Some(true));
    key(&mut h, egui::Key::Enter);
    let hall = plugin(&screens_catalog(), "hall");
    assert_eq!(adds(h.state()), [(hall, DropTarget::Track(pad))]);
    let chain = &h.state().project().tracks[3].channel.devices;
    assert_eq!(chain.last().unwrap().plugin.name, "Dragonfly Hall Reverb");
    // The dock shows the chain; the field keeps the focus for the next search.
    assert_eq!(h.state().ui().layout.dock_tab, DockTab::Devices);
    assert!(
        h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
            .is_focused()
    );
    // Esc clears the text, a second Esc leaves the field.
    key(&mut h, egui::Key::Escape);
    assert_eq!(h.state().ui().browser.query, "");
    assert!(
        h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
            .is_focused()
    );
    key(&mut h, egui::Key::Escape);
    assert!(
        !h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
            .is_focused()
    );
}

#[test]
fn enter_on_an_instrument_adds_a_track_and_an_effect_needs_a_track() {
    let mut h = harness(app());
    let tracks = h.state().project().tracks.len();
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::F);
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH)
        .type_text("surge");
    h.run_steps(2);
    key(&mut h, egui::Key::Enter);
    let p = h.state().project();
    assert_eq!(p.tracks.len(), tracks + 1);
    assert_eq!(
        p.tracks.last().unwrap().channel.devices[0].plugin.name,
        "Surge XT"
    );
    // An effect without a selected track: the status bar says why nothing happens.
    h.state_mut().ui_mut().selection.tracks.clear();
    let field = h.get_by_role_and_label(Role::TextInput, de::BROWSER_SEARCH);
    field.type_text("x"); // "surgex": no hits
    h.run_steps(2);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetBrowserQuery(String::new())));
    h.run_steps(2);
    let row = row(&h, "Airwindows Consolidated");
    double_click(&mut h, row.center());
    let status = h.state().ui().status.as_ref().map(|s| s.text.clone());
    assert_eq!(status.as_deref(), Some("Effekte brauchen eine Spur"));
}

// ------------------------------------------------------------------------------------------------
// Favourites: the star, persisted in the settings.

#[test]
fn the_star_toggles_a_favourite_and_the_settings_keep_it() {
    let dir = std::env::temp_dir().join(format!("komponist-wp44-fav-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("settings.json");
    let engine = komponist_app::engine_sync::EngineSync::null(Box::new(FakeHost::new()));
    let doc = komponist_model::Document::new(demo_project());
    let app = App::new(doc, engine, Settings::default(), Some(path.clone()));
    let mut h = harness(app);
    let star_label = Lang::De.browser_star("Surge XT");
    let star = h
        .get_all_by_role_and_label(Role::Button, &star_label)
        .next()
        .unwrap();
    assert_eq!(star.accesskit_node().toggled(), Some(Toggled::False));
    let r = star.rect();
    click_at(&mut h, r.center());
    let surge = plugin(&screens_catalog(), "surge");
    assert!(h.state().settings().favorites.contains(&surge));
    let rows = row_names(&h);
    assert_eq!(&rows[..3], ["Hexa", "Klopfer", "Surge XT"], "Favoriten");
    // Saved: a new app with the file shows it as a favourite.
    let saved = Settings::load(&path).unwrap();
    assert!(saved.favorites.contains(&surge));
    let engine = komponist_app::engine_sync::EngineSync::null(Box::new(FakeHost::new()));
    let doc = komponist_model::Document::new(demo_project());
    let h2 = harness(App::new(doc, engine, saved, None));
    assert_eq!(&row_names(&h2)[..3], ["Hexa", "Klopfer", "Surge XT"]);
    // And off again.
    let star = h
        .get_all_by_role_and_label(Role::Button, &star_label)
        .next()
        .unwrap()
        .rect();
    click_at(&mut h, star.center());
    assert!(!Settings::load(&path).unwrap().favorites.contains(&surge));
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// Double-click and drag

#[test]
fn double_click_on_an_instrument_makes_a_track() {
    let mut h = harness(app());
    let tracks = h.state().project().tracks.len();
    let surge = h
        .get_all_by_role_and_label(Role::ListItem, "Dexed")
        .next()
        .unwrap()
        .rect();
    double_click(&mut h, surge.center());
    let p = h.state().project();
    assert_eq!(p.tracks.len(), tracks + 1);
    assert_eq!(
        p.tracks.last().unwrap().channel.devices[0].plugin.name,
        "Dexed"
    );
    assert_eq!(adds(h.state())[0].1, DropTarget::NewTrack { index: None });
}

#[test]
fn a_row_dragged_onto_a_track_header_loads_the_instrument() {
    let mut h = harness(app());
    let keys = h.state().project().tracks[4].id;
    let row = row(&h, "Surge XT");
    let header = h
        .get_all_by_role(Role::Row)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("Spur Keys"))
        })
        .unwrap()
        .rect();
    drag(&mut h, row.center(), header.center());
    let p = h.state().project();
    assert_eq!(
        p.track(keys).unwrap().channel.devices[0].plugin.name,
        "Surge XT"
    );
    assert_eq!(
        adds(h.state()),
        [(plugin(&screens_catalog(), "surge"), DropTarget::Track(keys))]
    );
}

#[test]
fn a_row_dragged_onto_the_empty_area_makes_a_track() {
    let mut h = harness(app());
    let tracks = h.state().project().tracks.len();
    let row = row(&h, "Surge XT");
    drag(&mut h, row.center(), pos2(900.0, 650.0));
    assert_eq!(h.state().project().tracks.len(), tracks + 1);
}

// ------------------------------------------------------------------------------------------------
// Presets · hexa, Projekt (takes)

#[test]
fn a_preset_loads_into_the_selected_tracks_hexa() {
    let mut app = app();
    select_track(&mut app, 2);
    let device = app.project().tracks[2].channel.devices[0].id;
    let before = app.project().tracks[2].channel.devices[0].state.clone();
    let mut h = harness(app);
    let r = h.get_by_role_and_label(Role::ListItem, "Tasten").rect();
    click_at(&mut h, r.center());
    let first = komponist_hexa::presets()
        .find(|p| p.category == "Tasten")
        .unwrap();
    let preset = h.get_by_role_and_label(Role::ListItem, first.name).rect();
    double_click(&mut h, preset.center());
    assert!(h.state().dispatched().any(|a| *a
        == Action::Plugin(PluginAction::ApplyHexaPreset {
            device,
            preset: first.index
        })));
    let after = h.state().project().tracks[2].channel.devices[0]
        .state
        .clone();
    assert_ne!(before, after);
    // Undoable.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(
        h.state().project().tracks[2].channel.devices[0].state,
        before
    );
}

#[test]
fn a_preset_without_a_hexa_on_the_selected_track_makes_a_new_hexa_track() {
    let mut app = app();
    // Pad plays Surge XT: no hexa there.
    select_track(&mut app, 3);
    let tracks = app.project().tracks.len();
    let mut h = harness(app);
    let r = h.get_by_role_and_label(Role::ListItem, "Tasten").rect();
    click_at(&mut h, r.center());
    let first = komponist_hexa::presets()
        .find(|p| p.category == "Tasten")
        .unwrap();
    let preset = h.get_by_role_and_label(Role::ListItem, first.name).rect();
    double_click(&mut h, preset.center());
    h.run_steps(2);
    let p = h.state().project().clone();
    assert_eq!(p.tracks.len(), tracks + 1);
    let new = p.tracks.last().unwrap();
    let device = &new.channel.devices[0];
    assert_eq!(
        device.state.as_deref(),
        Some(&*komponist_hexa::encode_state(
            &komponist_hexa::preset_patch(first.index).unwrap()
        ))
    );
    // One undo step removes the track again.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks.len(), tracks);
}

#[test]
fn takes_are_listed_and_open_their_clip() {
    let p = demo_project();
    let clip: ClipId = p.tracks[2].clips[0].id;
    let (p, _) = komponist_model::edits::add_take(&p, clip, "Take 1".into()).unwrap();
    let (p, _) = komponist_model::edits::add_take(&p, clip, "Take 2".into()).unwrap();
    let mut app = App::headless(p).with_device_host(Box::new(FakeHost::new()));
    // Files also shows filesystem navigation. Use an empty fixture directory so this take
    // interaction never depends on the owner's home folder or its directory count.
    app.ui_mut().browser.directory = Some(crate::export::scratch("browser-takes"));
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Files,
    )));
    let mut h = harness(app);
    let rows = row_names(&h);
    assert_eq!(
        rows,
        [
            Lang::De.browser_parent_folder(),
            "Take 1 · Lead",
            "Take 2 · Lead"
        ]
    );
    let take = row(&h, "Take 1 · Lead");
    double_click(&mut h, take.center());
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Edit(EditAction::SelectTake { clip, take: 1 }))
    );
    assert_eq!(h.state().ui().editor.clip, Some(clip));
}
