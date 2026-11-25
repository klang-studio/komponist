//! #3: the dialogs, the start screen and the plugin frame header in English (`Lang::En`). Each dialog
//! gets its title, a button, a field label and an AccessKit name checked; dates and plurals are
//! English too. German is covered by `dialogs.rs`, `export.rs`, `plugin_frame.rs` and the snapshots
//! (zero diffs).

use std::path::PathBuf;
use std::time::SystemTime;

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use komponist_app::actions::{Action, ProjectAction, UiAction};
use komponist_app::dialogs::file::{FileBrowser, Purpose};
use komponist_app::dialogs::format::{self, LocalTime};
use komponist_app::dialogs::{export, missing_plugins, new_project, settings, start};
use komponist_app::platform::Platform;
use komponist_app::project_io::ProjectSummary;
use komponist_app::ui_state::{SettingsSection, StartView};
use komponist_app::windows::plugin_frame::header::{self, HeaderModel, Mark};
use komponist_app::{App, ui_state::PluginView};
use komponist_i18n::{Lang, de, en};
use komponist_model::fixtures::demo_project;
use komponist_plugins::MissingPlugin;

fn app_harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

fn english_app() -> App {
    App::headless(demo_project()).with_lang(Lang::En)
}

/// A harness for a view that gets the language from the context, as the app sets it.
fn view_harness<S>(
    lang: Lang,
    state: S,
    mut ui_fn: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) -> Harness<'static, S> {
    let mut h = komponist_ui::snapshot::builder::<S>(1.0)
        .with_size(vec2(1280.0, 720.0))
        .build_ui_state(
            move |ui, s: &mut S| {
                komponist_ui::set_lang(ui.ctx(), lang);
                ui_fn(ui, s);
            },
            state,
        );
    h.run_steps(3);
    h
}

fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> LocalTime {
    // Days since 1970-01-01 of the dates the tests use (checked against `LocalTime::day_number`).
    let probe = LocalTime {
        year,
        month,
        day,
        hour,
        minute,
        unix: 0,
    };
    let unix = probe.day_number() * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60;
    LocalTime { unix, ..probe }
}

// ------------------------------------------------------------------------------------------------
// Dates, relative times and plurals
// ------------------------------------------------------------------------------------------------

#[test]
fn dates_and_relative_times_are_english_and_24_hour() {
    let t = Lang::En;
    let now = at(2026, 11, 14, 16, 30);
    assert_eq!(
        format::relative_date(t, &at(2026, 11, 14, 16, 30), &now),
        "just now"
    );
    assert_eq!(
        format::relative_date(t, &at(2026, 11, 14, 16, 29), &now),
        "1 minute ago"
    );
    assert_eq!(
        format::relative_date(t, &at(2026, 11, 14, 16, 5), &now),
        "25 minutes ago"
    );
    assert_eq!(
        format::relative_date(t, &at(2026, 11, 14, 15, 10), &now),
        "1 hour ago"
    );
    assert_eq!(
        format::relative_date(t, &at(2026, 11, 14, 14, 20), &now),
        "2 hours ago"
    );
    assert_eq!(
        format::relative_date(t, &at(2026, 11, 13, 23, 59), &now),
        "yesterday"
    );
    assert_eq!(
        format::relative_date(t, &at(2026, 10, 12, 9, 0), &now),
        "Oct 12"
    );
    assert_eq!(
        format::relative_date(t, &at(2025, 10, 2, 9, 0), &now),
        "Oct 2, 2025"
    );
    assert_eq!(
        format::day_and_time(t, &at(2026, 11, 14, 9, 12), &now),
        "today 09:12"
    );
    assert_eq!(
        format::day_and_time(t, &at(2026, 11, 13, 21, 5), &now),
        "yesterday 21:05"
    );
    assert_eq!(
        format::day_and_time(t, &at(2026, 9, 1, 18, 0), &now),
        "Sep 1 18:00"
    );
    assert_eq!(
        format::latency(t, 256, 48_000, std::time::Duration::ZERO),
        "5.3 ms latency"
    );
}

#[test]
fn counts_use_the_english_plural() {
    let summary = |tracks| ProjectSummary {
        name: "Night".into(),
        bpm: 124.0,
        tracks,
        hues: Vec::new(),
        modified: None,
    };
    assert_eq!(summary(1).meta(Lang::En), "124 BPM · 1 track");
    assert_eq!(summary(6).meta(Lang::En), "124 BPM · 6 tracks");
    assert_eq!(summary(6).meta(Lang::De), "124 BPM · 6 Spuren");
    assert_eq!(missing_plugins::title(Lang::En, 1), "1 Plugin Missing");
    assert_eq!(missing_plugins::title(Lang::En, 2), "2 Plugins Missing");
    // The status bar's "opened with problems" text takes its title from the catalog, too.
    assert_eq!(
        missing_plugins::title_for(Lang::En, 0, 1),
        "1 Sample Missing"
    );
    assert_eq!(
        missing_plugins::title_for(Lang::En, 2, 3),
        "2 Plugins and 3 Samples Missing"
    );
    assert_eq!(
        missing_plugins::title_for(Lang::De, 2, 3),
        "2 Plugins und 3 Samples fehlen"
    );
    assert_eq!(
        Lang::En.project_opened_problems("Night", missing_plugins::title_for(Lang::En, 1, 0)),
        "Night opened · 1 Plugin Missing"
    );
    assert_eq!(Lang::En.project_repaired(1), "1 item repaired on open");
    assert_eq!(
        Lang::En.project_opened_repaired("Night", Lang::En.project_repaired(3)),
        "Night opened · 3 items repaired on open"
    );
    assert_eq!(
        Lang::De.project_repaired(1),
        "1 Stelle beim Öffnen repariert"
    );
    assert_eq!(
        Lang::De.project_opened_repaired("Night", Lang::De.project_repaired(3)),
        "Night geöffnet · 3 Stellen beim Öffnen repariert"
    );
}

// ------------------------------------------------------------------------------------------------
// Start screen
// ------------------------------------------------------------------------------------------------

#[test]
fn the_start_screen_speaks_english() {
    let mut app = english_app();
    app.show_start_screen();
    let h = app_harness(app);
    h.get_by_role_and_label(Role::Button, en::START_NEW);
    h.get_by_role_and_label(Role::Button, en::START_OPEN);
    h.get_by_role_and_label(Role::Button, en::START_TEMPLATE);
    h.get_by_label(en::START_SYSTEM);
    h.get_by_role_and_label(Role::Label, en::START_RECENT);
    h.get_by_label(en::START_HINT);
    h.get_by_role_and_label(Role::TextInput, en::START_FILTER);
    assert_eq!(en::START_OPEN, "Open Project…");
    assert!(h.query_by_label(de::START_NEW).is_none());
    // The system rows: the plugin row names the settings in English.
    assert!(
        h.query_all_by_label_contains("found · Settings")
            .next()
            .is_some()
    );
}

#[test]
fn the_recent_list_shows_english_dates_and_a_missing_file() {
    let model = start::Model {
        version: start::version_line(Lang::En),
        new_shortcut: "Ctrl+N".into(),
        open_shortcut: "Ctrl+O".into(),
        recent: vec![
            start::recent_row(
                Lang::En,
                PathBuf::from("/nowhere/Gone.komponist"),
                None,
                SystemTime::now(),
            ),
            start::RecentRow {
                path: PathBuf::from("/home/kim/Night.komponist"),
                name: "Night".into(),
                path_label: "~/Night.komponist".into(),
                meta: "124 BPM · 6 tracks".into(),
                date: format::relative_date(
                    Lang::En,
                    &at(2026, 11, 14, 14, 20),
                    &at(2026, 11, 14, 16, 30),
                ),
                hues: vec![45, 250],
                missing: false,
                recovery: None,
                copy_only: false,
            },
        ],
        system: Vec::new(),
        message: None,
    };
    let platform = if cfg!(windows) {
        "Windows"
    } else {
        Platform::current().pick("Linux", "macOS")
    };
    assert!(
        model
            .version
            .ends_with(&format!("{platform} · CLAP and VST3"))
    );
    // Each platform names itself, in both languages.
    for (lang, formats) in [(Lang::De, "CLAP und VST3"), (Lang::En, "CLAP and VST3")] {
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(
            start::version_line_for(lang, Platform::Pc),
            format!("{v} · Linux · {formats}")
        );
        assert_eq!(
            start::version_line_for(lang, Platform::Mac),
            format!("{v} · macOS · {formats}")
        );
    }
    assert_eq!(model.recent[0].meta, "File missing");
    let h = view_harness(Lang::En, StartView::default(), move |ui, view| {
        let rect = ui.ctx().content_rect();
        let _ = start::show(ui, rect, &model, view, true);
    });
    h.get_by_role_and_label(Role::ListItem, "Night, 124 BPM · 6 tracks");
    h.get_by_role_and_label(Role::ListItem, "Gone, File missing");
    h.get_by_role_and_label(Role::List, en::START_RECENT);
}

// ------------------------------------------------------------------------------------------------
// Settings (S07)
// ------------------------------------------------------------------------------------------------

#[test]
fn settings_speak_english() {
    let mut app = english_app();
    app.dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
    let h = app_harness(app);
    h.get_by_label(en::SETTINGS_TITLE);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CLOSE);
    // The navigation tabs are Title Case.
    h.get_by_role_and_label(Role::Tab, en::SETTINGS_NAV_DISPLAY);
    h.get_by_role_and_label(Role::Tab, en::SETTINGS_NAV_KEYBOARD);
    // Labels and names of the controls (sentence case).
    h.get_by_role_and_label(Role::Label, en::SETTINGS_DRIVER);
    h.get_by_role_and_label(Role::ComboBox, en::SETTINGS_DEVICE);
    h.get_by_role_and_label(Role::ComboBox, en::SETTINGS_RATE);
    h.get_by_role_and_label(Role::Label, en::SETTINGS_BUFFER);
    // The MIDI list's empty text and the plugin folders' buttons.
    h.get_by_value(en::SETTINGS_NO_MIDI);
    h.get_by_role_and_label(Role::Button, en::SETTINGS_ADD_FOLDER);
    h.get_by_role_and_label(Role::Button, en::SETTINGS_RESCAN);
    h.get_by_role_and_label(Role::Label, en::SETTINGS_SCALE);
    h.get_by_label(en::SETTINGS_SCALE_AUTO);
    // "125%", no space.
    h.get_by_label("125%");
    h.get_by_label_contains("ms latency");
    assert!(h.query_by_label(de::SETTINGS_TITLE).is_none());
}

#[test]
fn the_midi_rows_and_the_scan_summary_are_english() {
    let row = |enabled, connected| settings::MidiRow {
        name: "Port".into(),
        label: "Port".into(),
        enabled,
        connected,
        active: false,
        busy: false,
    };
    assert_eq!(row(false, true).info(Lang::En), "off");
    assert_eq!(row(true, true).info(Lang::En), "active · channels 1–16");
    assert_eq!(row(true, false).info(Lang::En), "not connected");
    assert_eq!(
        settings::scan_summary(Lang::En, None, false, SystemTime::now()),
        "No scan yet"
    );
    assert_eq!(
        settings::scan_summary(Lang::En, None, true, SystemTime::now()),
        "Scanning…"
    );
    assert_eq!(settings::scale_label(Lang::En, None), "Automatic");
    assert_eq!(settings::scale_label(Lang::En, Some(1.5)), "150%");
    assert_eq!(settings::scale_label(Lang::De, Some(1.5)), "150 %");
    assert_eq!(
        settings::section_name(Lang::En, SettingsSection::Display),
        "Appearance"
    );
    let config = komponist_audio::StreamConfig::default();
    let (entries, _) =
        settings::device_entries(Lang::En, komponist_audio::HostKind::PipeWire, &[], &config);
    assert_eq!(entries[0].label, "Default Device");
}

// ------------------------------------------------------------------------------------------------
// File browser
// ------------------------------------------------------------------------------------------------

#[test]
fn the_file_browser_speaks_english() {
    let mut app = english_app();
    app.dispatch(Action::Project(ProjectAction::Open));
    let h = app_harness(app);
    h.get_by_role_and_label(Role::Dialog, en::FILE_OPEN_TITLE);
    h.get_by_role_and_label(Role::Button, en::FILE_OPEN);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CANCEL);
    h.get_by_label(en::FILE_FOLDER);
    h.get_by_role_and_label(Role::Button, en::FILE_PARENT);
    h.get_by_role_and_label(Role::List, en::FILE_LIST);
}

#[test]
fn the_save_browser_says_what_is_wrong_in_english() {
    let dir = std::env::temp_dir().join(format!("komponist-i18n-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Night.komponist"), "{}").unwrap();
    let mut b = FileBrowser::new(Lang::En, Purpose::Save, dir.clone(), "Night".into());
    assert_eq!(b.title(Lang::En), "Save Project");
    assert_eq!(b.primary(Lang::En), "Save");
    assert!(b.confirm().is_none());
    assert_eq!(
        b.message.as_deref(),
        Some("“Night.komponist” already exists. Replace?")
    );
    assert_eq!(b.primary(Lang::En), "Replace");
    b.name = "nope/Night".into();
    assert!(b.confirm().is_none());
    assert!(b.message.as_deref().unwrap().contains("does not exist"));
    b.location = dir.join("missing").display().to_string();
    assert!(b.go_to_location().is_none());
    assert!(b.message.as_deref().unwrap().ends_with(" does not exist"));
    let folder = FileBrowser::new(Lang::En, Purpose::ExportFolder, dir.clone(), String::new());
    assert_eq!(folder.title(Lang::En), "Choose Export Folder");
    assert_eq!(folder.primary(Lang::En), "Choose Folder");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// New project, the unsaved-changes prompt, "New Track"
// ------------------------------------------------------------------------------------------------

#[test]
fn the_unsaved_changes_prompt_speaks_english() {
    let h = view_harness(Lang::En, (), |ui, _| {
        let _ = new_project::show_unsaved(ui.ctx(), "Night");
    });
    h.get_by_label(en::NEW_PROJECT_UNSAVED_TITLE);
    assert_eq!(en::NEW_PROJECT_UNSAVED_TITLE, "Save Changes?");
    h.get_by_label("“Night” has unsaved changes. Without saving, they are lost.");
    h.get_by_role_and_label(Role::Button, en::NEW_PROJECT_SAVE);
    h.get_by_role_and_label(Role::Button, en::NEW_PROJECT_DISCARD);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CANCEL);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CLOSE);
}

#[test]
fn a_new_project_and_the_template_are_named_in_english() {
    assert_eq!(new_project::empty_project(Lang::En).name, "Untitled");
    assert_eq!(new_project::empty_project(Lang::De).name, "Unbenannt");
    let p = {
        let (drums, hexa) =
            new_project::template_instruments(&komponist_plugins::Catalog::builtin()).unwrap();
        new_project::template(Lang::En, drums, hexa)
    };
    assert_eq!(p.name, "Beat Sketch");
    // The "From Template" button carries the template's name as its hint.
    assert_eq!(en::NEW_PROJECT_TEMPLATE_NAME, "Beat Sketch");
}

#[test]
fn the_new_track_dialog_speaks_english() {
    let mut app = english_app();
    app.dispatch(Action::Ui(UiAction::OpenNewTrackDialog));
    let mut h = app_harness(app);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Dialog, en::NEW_TRACK_TITLE);
    h.get_by_role_and_label(Role::Button, en::NEW_TRACK_CREATE);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CANCEL);
    h.get_by_role_and_label(Role::RadioButton, en::NEW_TRACK_MIDI);
    h.get_by_role_and_label(Role::RadioButton, en::NEW_TRACK_BUS);
    h.get_by_role_and_label(Role::SpinButton, en::NEW_TRACK_COUNT);
    h.get_by_role_and_label(Role::TextInput, en::NEW_TRACK_NAME);
    h.get_by_role_and_label(Role::TextInput, en::NEW_TRACK_SEARCH);
    h.get_by_role_and_label(Role::ListBoxOption, en::NEW_TRACK_NO_INSTRUMENT);
    h.get_by_role_and_label(Role::ListBox, en::NEW_TRACK_INSTRUMENT);
    assert!(h.query_by_label(de::NEW_TRACK_TITLE).is_none());
}

// ------------------------------------------------------------------------------------------------
// Missing plugins and samples (S06)
// ------------------------------------------------------------------------------------------------

#[test]
fn the_missing_plugins_dialog_speaks_english() {
    let project = demo_project();
    let arp = project.tracks.iter().find(|t| t.name == "Arp").unwrap();
    let missing = vec![MissingPlugin {
        track: Some(arp.id),
        device: arp.channel.devices[0].id,
        role: komponist_model::DeviceRole::Instrument,
        index: 0,
        plugin: arp.channel.devices[0].plugin.clone(),
    }];
    let model = missing_plugins::model(Lang::En, &project, &missing);
    assert_eq!(model.rows[0].place, "Track Arp · Instrument");
    let name = model.rows[0].name.clone();
    let h = view_harness(Lang::En, (), move |ui, _| {
        let _ = missing_plugins::show(ui.ctx(), &model, true);
    });
    h.get_by_label(&en_title(1));
    h.get_by_role_and_label(Role::Button, en::MISSING_CHECK_FOLDERS);
    h.get_by_role_and_label(Role::Button, en::MISSING_CONTINUE_ONE);
    h.query_all_by_label_contains(&name).next().unwrap();
    h.get_by_label_contains("This device is not installed");
    assert_eq!(
        Lang::En
            .missing_text(2, "Night")
            .split('.')
            .nth(1)
            .unwrap()
            .trim(),
        "These devices are not installed or are not in a plugin folder"
    );
}

fn en_title(n: usize) -> String {
    missing_plugins::title(Lang::En, n)
}

#[test]
fn missing_samples_have_english_places_and_a_search_button() {
    use komponist_app::dialogs::missing_plugins::{MissingSample, with_samples};
    let project = demo_project();
    let track = project.tracks.iter().find(|t| t.name == "Drums").unwrap();
    let samples = [MissingSample {
        device: track.channel.devices[0].id,
        track: Some(track.id),
        pad: 2,
        name: "Kick".into(),
        file: "kick.wav".into(),
    }];
    let model = with_samples(
        Lang::En,
        missing_plugins::model(Lang::En, &project, &[]),
        &project,
        &samples,
    );
    assert_eq!(model.samples[0].place, "Track Drums · Pad 3");
    assert_eq!(model.samples[0].format, "SAMPLE");
    let h = view_harness(Lang::En, (), move |ui, _| {
        let _ = missing_plugins::show(ui.ctx(), &model, true);
    });
    h.get_by_label("1 Sample Missing");
    h.get_by_role_and_label(Role::Button, en::MISSING_SEARCH);
    h.get_by_role_and_label(Role::Button, en::MISSING_CONTINUE_SAMPLE);
}

// ------------------------------------------------------------------------------------------------
// Export (S08)
// ------------------------------------------------------------------------------------------------

#[test]
fn the_export_dialog_speaks_english() {
    let mut h = app_harness(english_app());
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    h.run_steps(3);
    h.get_by_label(en::EXPORT_TITLE);
    h.get_by_role_and_label(Role::RadioButton, en::EXPORT_SONG);
    h.get_by_role_and_label(Role::RadioButton, en::EXPORT_SELECTION);
    h.get_by_role_and_label(Role::ComboBox, en::EXPORT_RATE);
    h.get_by_role_and_label(Role::RadioButton, en::EXPORT_BITS_24);
    h.get_by_role_and_label(Role::Switch, en::EXPORT_NORMALIZE);
    h.get_by_label(en::EXPORT_NORMALIZE_ON);
    h.get_by_role_and_label(Role::TextInput, en::EXPORT_FILE);
    h.get_by_role_and_label(Role::Button, en::EXPORT_CHANGE);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CANCEL);
    h.get_by_role_and_label(Role::Button, en::EXPORT_EXPORT);
    assert_eq!(en::EXPORT_TITLE, "Export WAV");
    assert_eq!(en::EXPORT_SHOW_IN_FOLDER, "Show in Folder");
    // "Change" opens the file browser with its English title.
    h.get_by_role_and_label(Role::Button, en::EXPORT_CHANGE)
        .click();
    h.run_steps(3);
    h.get_by_value(en::EXPORT_FOLDER_TITLE);
}

#[test]
fn the_export_progress_and_device_dialogs_speak_english() {
    use komponist_app::actions::ExportSettings;
    use komponist_app::dialogs::export::{DeviceRow, DeviceState, Model, ProgressView, Stage};
    let settings = komponist_app::settings::default_export("Night", PathBuf::from("/tmp"));
    let progress = Model {
        settings: ExportSettings {
            file_name: "Night".into(),
            ..settings.clone()
        },
        stage: Stage::Progress(ProgressView {
            fraction: 0.62,
            done: "00:19.6".into(),
            total: "00:33.0".into(),
            speed: Some(6.24),
            peak_db: -2.4,
            remaining: Some(1.2),
            cancelling: false,
        }),
        keys: true,
    };
    let h = view_harness(Lang::En, progress, |ui, model| {
        let _ = export::show(ui.ctx(), model);
    });
    h.get_by_label("Exporting Night.wav");
    h.get_by_role_and_label(Role::ProgressIndicator, en::EXPORT_PROGRESS);
    h.get_by_label("00:19.6 of 00:33.0");
    h.get_by_label(format!("{}: 6.2 × real time", en::EXPORT_SPEED).as_str());
    h.get_by_label(format!("{}: about 2 s", en::EXPORT_REMAINING).as_str());
    h.get_by_label(format!("{}: \u{2212}2.4 dBFS", en::EXPORT_PEAK).as_str());
    h.get_by_role_and_label(Role::Button, en::DIALOG_CANCEL);

    let devices = Model {
        settings: ExportSettings {
            file_name: "Night".into(),
            ..settings
        },
        stage: Stage::Devices(vec![DeviceRow {
            name: "Vital".into(),
            track: "Arp".into(),
            state: DeviceState::Missing,
        }]),
        keys: true,
    };
    let h = view_harness(Lang::En, devices, |ui, model| {
        let _ = export::show(ui.ctx(), model);
    });
    h.get_by_label(en::EXPORT_WAITING);
    h.get_by_role_and_label(Role::ListItem, "Vital · Arp · missing");
    h.get_by_role_and_label(Role::Button, en::EXPORT_WITHOUT);
}

// ------------------------------------------------------------------------------------------------
// Plugin frame header
// ------------------------------------------------------------------------------------------------

#[test]
fn the_plugin_frame_header_speaks_english() {
    let model = HeaderModel {
        name: "Surge XT".into(),
        sub: "Pad · CLAP".into(),
        mark: Mark::Active,
        bypassed: false,
        presets: vec!["Init".into(), "Pad".into()],
        preset: Some(0),
        preset_changed: false,
        can_save: true,
        view: PluginView::Gui,
        has_gui: true,
        keep_on_top: false,
        close_shortcut: "Esc".into(),
        mapping_chip: None,
    };
    let h = view_harness(Lang::En, model, |ui, model| {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(900.0, 28.0));
        let _ = header::show(ui, rect, model);
    });
    h.get_by_role_and_label(Role::Button, en::PLUGIN_FRAME_BYPASS);
    h.get_by_role_and_label(Role::Button, en::PLUGIN_FRAME_SAVE);
    h.get_by_role_and_label(Role::Button, en::PLUGIN_FRAME_KEEP_ON_TOP);
    h.get_by_role_and_label(Role::Button, en::DIALOG_CLOSE);
    h.get_by_role_and_label(Role::Button, en::PLUGIN_FRAME_PRESET_PREV);
    h.get_by_role_and_label(Role::Button, en::PLUGIN_FRAME_PRESET_NEXT);
    h.get_by_role_and_label(Role::ComboBox, en::PLUGIN_FRAME_PRESET);
    h.get_by_role_and_label(Role::RadioButton, en::PLUGIN_FRAME_VIEW_PARAMS);
    assert_eq!(en::PLUGIN_FRAME_VIEW_PARAMS, "Parameters");
    assert!(
        h.query_by_label(de::PLUGIN_FRAME_BYPASS).is_some(),
        "Bypass stays Bypass"
    );
    assert!(h.query_by_label(de::PLUGIN_FRAME_SAVE).is_none());
}

#[test]
fn the_plugin_frame_body_texts_are_english() {
    use komponist_app::windows::plugin_frame::body::{self, BodyModel};
    let h = view_harness(Lang::En, BodyModel::Loading, |ui, model| {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(600.0, 300.0));
        let _ = body::show(ui, rect, model);
    });
    h.get_by_label(en::PLUGIN_FRAME_LOADING);
    h.get_by_label(en::PLUGIN_FRAME_LOADING_DETAIL);
    let h = view_harness(
        Lang::En,
        BodyModel::Missing {
            name: "Dexed".into(),
        },
        |ui, model| {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(600.0, 300.0));
            let _ = body::show(ui, rect, model);
        },
    );
    h.get_by_label("Dexed is missing");
    let h = view_harness(
        Lang::En,
        BodyModel::Gui {
            native: false,
            name: "Surge XT".into(),
        },
        |ui, model| {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(600.0, 300.0));
            let _ = body::show(ui, rect, model);
        },
    );
    h.get_by_label("Plugin GUI · Surge XT draws it itself");
    h.get_by_label(en::PLUGIN_FRAME_GUI_PLACEHOLDER_DETAIL);
}
