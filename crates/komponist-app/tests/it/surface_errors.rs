//! #227: what failed silently now reaches the user: rejected edits, a MIDI input that cannot be
//! opened, a saved audio input that does not start, a driver that cannot list its devices, and
//! the language-neutral clipboard marker of copied devices.

use egui::vec2;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::actions::SettingsAction;
use komponist_app::dialogs::settings::AudioDevices;
use komponist_app::engine_sync::EngineSync;
use komponist_app::ui_state::{SettingsSection, StatusKind};
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_audio::{AudioError, DeviceInfo, HostKind, MidiInputHub, StreamConfig};
use komponist_i18n::Lang;
use komponist_model::edits::{self, EditError, NewBus};
use komponist_model::fixtures::demo_project;
use komponist_model::{Project, TrackId, TrackOutput};

const LANGS: [Lang; 2] = [Lang::De, Lang::En];

fn status(app: &App) -> (String, StatusKind) {
    let s = app.ui().status.as_ref().expect("a status message");
    (s.text.clone(), s.kind)
}

// ------------------------------------------------------------------------------------------------
// Rejected edits
// ------------------------------------------------------------------------------------------------

#[test]
fn user_actionable_edit_errors_show_their_message_in_both_languages() {
    for lang in LANGS {
        let mut app = App::headless(demo_project()).with_lang(lang);
        let track = app.project().tracks[0].id;
        let name = app.project().tracks[0].name.clone();
        let cases = [
            (
                EditError::RoutingCycle,
                lang.status_edit_routing_cycle().to_owned(),
            ),
            (EditError::NotABus(track), lang.status_edit_not_a_bus(&name)),
            (
                EditError::AutomationExpansionTooLarge,
                lang.status_edit_automation_too_large().to_owned(),
            ),
            (
                EditError::MacroMappingLimit { index: 2 },
                lang.status_edit_macro_full(3),
            ),
            (
                EditError::IdsExhausted,
                lang.status_id_space_exhausted().to_owned(),
            ),
        ];
        for (error, text) in cases {
            app.record_edit_error(error.clone());
            assert_eq!(status(&app), (text, StatusKind::Error), "{error:?}");
            assert_eq!(app.last_error(), Some(&error));
        }
    }
}

#[test]
fn the_texts_name_what_the_user_can_act_on() {
    assert_eq!(
        Lang::De.status_edit_not_a_bus("Bass"),
        "Nicht möglich: „Bass“ ist kein Bus"
    );
    assert_eq!(
        Lang::En.status_edit_macro_full(3),
        "Not possible: track macro 3 already has 16 mappings"
    );
}

#[test]
fn bug_only_edit_errors_show_the_generic_text() {
    for lang in LANGS {
        let mut app = App::headless(demo_project()).with_lang(lang);
        for error in [
            EditError::TrackNotFound(TrackId(9_999)),
            EditError::IndexOutOfRange {
                what: "take",
                index: 7,
            },
            EditError::Invalid("tempo"),
        ] {
            app.record_edit_error(error.clone());
            assert_eq!(
                status(&app),
                (lang.status_edit_failed().to_owned(), StatusKind::Error)
            );
            assert_eq!(app.last_error(), Some(&error));
        }
    }
    assert_eq!(Lang::De.status_edit_failed(), "Bearbeitung nicht möglich");
    assert_eq!(Lang::En.status_edit_failed(), "Edit not possible");
}

#[test]
fn a_routing_cycle_from_the_dispatch_reaches_the_status_bar() {
    let (p, a) = edits::add_bus(&Project::new("Zyklus"), NewBus::default()).unwrap();
    let (p, b) = edits::add_bus(&p, NewBus::default()).unwrap();
    let p = edits::set_output(&p, &[a], TrackOutput::Bus(b)).unwrap();
    let mut app = App::headless(p);
    app.dispatch(Action::Edit(EditAction::SetOutput {
        tracks: vec![b],
        output: TrackOutput::Bus(a),
    }));
    assert_eq!(app.last_error(), Some(&EditError::RoutingCycle));
    assert_eq!(
        status(&app),
        (
            Lang::De.status_edit_routing_cycle().to_owned(),
            StatusKind::Error
        )
    );
}

#[test]
fn an_output_to_a_track_that_is_no_bus_names_the_track() {
    let mut app = App::headless(demo_project());
    let (first, second) = (app.project().tracks[0].clone(), app.project().tracks[1].id);
    app.dispatch(Action::Edit(EditAction::SetOutput {
        tracks: vec![second],
        output: TrackOutput::Bus(first.id),
    }));
    assert_eq!(app.last_error(), Some(&EditError::NotABus(first.id)));
    assert_eq!(status(&app).0, Lang::De.status_edit_not_a_bus(&first.name));
}

// ------------------------------------------------------------------------------------------------
// Device errors
// ------------------------------------------------------------------------------------------------

/// An app on the null backend with a MIDI hub that connects to no port at all, or `None` without
/// a MIDI backend.
fn app_with_midi_hub(lang: Lang) -> Option<App> {
    let config = StreamConfig {
        host: HostKind::Null,
        ..StreamConfig::default()
    };
    let (mut hub, receiver) = MidiInputHub::new().ok()?;
    // Never connects to a real port (rule 14): no port name starts with this.
    hub.only_connect_to("komponist-it-no-such-port");
    let (sync, started) = EngineSync::with_receiver(
        &config,
        Some(hub),
        receiver,
        Box::new(App::headless_plugin_host()),
    );
    started.unwrap();
    let settings = komponist_app::settings::Settings {
        audio: config,
        ..Default::default()
    };
    Some(
        App::new(
            komponist_model::Document::new(demo_project()),
            sync,
            settings,
            None,
        )
        .with_lang(lang),
    )
}

#[test]
fn a_midi_input_that_cannot_be_opened_shows_a_status() {
    for lang in LANGS {
        let Some(mut app) = app_with_midi_hub(lang) else {
            eprintln!("MIDI backend unavailable: skipped");
            return;
        };
        app.dispatch(Action::Settings(SettingsAction::SetMidiInput {
            name: "Ghost Keys".into(),
            enabled: true,
        }));
        assert_eq!(
            status(&app),
            (
                lang.status_midi_enable_failed("Ghost Keys"),
                StatusKind::Error
            )
        );
    }
}

#[test]
fn a_saved_audio_input_that_does_not_start_shows_a_status_with_the_settings_shortcut() {
    for lang in LANGS {
        let mut app = App::headless(demo_project()).with_lang(lang);
        app.report_audio_input_failed(&AudioError::NoDevice("gone".into()));
        let (text, kind) = status(&app);
        assert_eq!(kind, StatusKind::Error);
        let shortcut = app
            .keymap()
            .label_for(
                &Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)),
                lang,
            )
            .expect("the settings have a shortcut");
        assert_eq!(text, lang.status_audio_input_failed(&shortcut));
        assert!(text.contains(&shortcut), "{text}");
    }
}

/// A driver whose devices cannot be listed.
struct BrokenDevices;

impl AudioDevices for BrokenDevices {
    fn hosts(&self) -> Vec<HostKind> {
        vec![HostKind::Null]
    }
    fn devices(&self, _: HostKind) -> Vec<DeviceInfo> {
        Vec::new()
    }
    fn try_devices(&self, _: HostKind) -> Result<Vec<DeviceInfo>, AudioError> {
        Err(AudioError::Backend("no daemon".into()))
    }
}

fn settings_harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(SnapshotOptions::new())
        .with_max_steps(64)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)));
    h.run_steps(6);
    h
}

#[test]
fn a_device_list_that_cannot_be_read_is_named_in_the_settings_notice() {
    for lang in LANGS {
        let app = App::headless(demo_project())
            .with_lang(lang)
            .with_audio_devices(Box::new(BrokenDevices));
        let h = settings_harness(app);
        let reason = AudioError::Backend("no daemon".into()).to_string();
        let text = lang.settings_devices_unavailable(&reason);
        assert!(
            h.query_by_label(&text).is_some(),
            "the notice \"{text}\" is missing"
        );
    }
}

#[test]
fn a_readable_device_list_shows_no_unavailable_notice() {
    let app = App::headless(demo_project()).with_audio_devices(Box::new(
        komponist_app::dialogs::settings::FixedDevices::default(),
    ));
    let h = settings_harness(app);
    assert!(
        h.query_by_label_contains("Geräteliste nicht verfügbar")
            .is_none()
    );
}
