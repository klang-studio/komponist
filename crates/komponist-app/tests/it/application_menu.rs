//! Shared-menu ownership, stale native commands and portable modal behaviour (#108).
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::application_menu::{Backend, Command, Item, NativeCommand, Snapshot};
use komponist_app::keymap::{Binding, KeyScope};
use komponist_app::platform::Platform;
use komponist_app::ui_state::{Dialog, FocusArea};
use komponist_app::windows::window_list::WindowEntry;
use komponist_app::{Action, App, EditAction, Key, KeyChord, Keymap, UiAction};
use komponist_i18n::Lang;
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceId, Project};

fn commands(items: &[Item]) -> Vec<Command> {
    items.iter().filter_map(|item| item.command).collect()
}
fn snapshot(lang: Lang, platform: Platform, backend: Backend, windows: &[WindowEntry]) -> Snapshot {
    Snapshot::new(
        lang,
        &Keymap::default_bindings_for(platform),
        backend,
        windows,
        true,
    )
}
fn harness(app: App, ppp: f32) -> Harness<'static, App> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let mut h = komponist_ui::snapshot::builder(ppp)
        .with_size(vec2(1280.0, 800.0))
        .with_options(SnapshotOptions::new().output_path(path))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

#[test]
fn backend_installation_owns_navigation_independently_of_shortcut_platform() {
    for lang in [Lang::De, Lang::En] {
        for platform in [Platform::Pc, Platform::Mac] {
            for backend in [Backend::Egui, Backend::AppKit, Backend::Win32] {
                for count in 0..=3 {
                    let windows: Vec<_> = (0..count)
                        .map(|i| WindowEntry {
                            device: DeviceId(u64::MAX - i),
                            label: format!("Plugin {i} · Track"),
                        })
                        .collect();
                    let s = snapshot(lang, platform, backend, &windows);
                    assert_eq!(s, snapshot(lang, platform, backend, &windows));
                    let expected = [Command::About, Command::Settings, Command::Quit];
                    let visible_application = if backend == Backend::Egui {
                        s.egui_application()
                    } else {
                        s.native_application()
                    };
                    for command in expected {
                        assert_eq!(
                            commands(visible_application)
                                .iter()
                                .filter(|&&c| c == command)
                                .count(),
                            1
                        );
                    }
                    assert!(
                        if backend == Backend::Egui {
                            s.native_application()
                        } else {
                            s.egui_application()
                        }
                        .is_empty()
                    );
                    let visible_windows = if backend == Backend::Egui {
                        s.egui_window()
                    } else {
                        s.native_window()
                    };
                    let plugin_commands: Vec<_> = commands(visible_windows)
                        .into_iter()
                        .filter(|c| matches!(c, Command::FocusPlugin(_)))
                        .collect();
                    assert_eq!(
                        plugin_commands,
                        windows
                            .iter()
                            .map(|entry| Command::FocusPlugin(entry.device))
                            .collect::<Vec<_>>()
                    );
                    if backend != Backend::AppKit && count == 0 {
                        assert!(visible_windows.is_empty());
                    }
                    assert_eq!(
                        s.dock().len(),
                        if backend == Backend::AppKit {
                            count as usize
                        } else {
                            0
                        }
                    );
                    assert_eq!(
                        commands(&s.project),
                        [
                            Command::New,
                            Command::Open,
                            Command::ImportAudio,
                            Command::NewAudioTrack,
                            Command::Save,
                            Command::SaveAs,
                            Command::CollectAndSave,
                            Command::Export,
                            Command::StartScreen
                        ]
                    );
                    if backend != Backend::AppKit {
                        assert!(
                            !commands(visible_application)
                                .iter()
                                .any(|c| matches!(c, Command::Native(_)))
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn appkit_keeps_its_application_and_responder_order() {
    let s = snapshot(Lang::En, Platform::Mac, Backend::AppKit, &[]);
    assert_eq!(
        commands(s.native_application()),
        [
            Command::About,
            Command::Settings,
            Command::Native(NativeCommand::Hide),
            Command::Native(NativeCommand::HideOthers),
            Command::Native(NativeCommand::ShowAll),
            Command::Quit
        ]
    );
    assert_eq!(
        commands(s.native_window()),
        [
            Command::Native(NativeCommand::Minimize),
            Command::Native(NativeCommand::Zoom)
        ]
    );
    assert_eq!(
        s.native_application()
            .iter()
            .filter(|item| item.command.is_none())
            .count(),
        3
    );
}

#[test]
fn labels_and_chords_follow_catalogs_and_the_current_keymap() {
    let mut map = Keymap::new();
    map.bind(Binding {
        chord: KeyChord::command_shift(Key::S),
        scope: KeyScope::Global,
        action: Command::Settings.action().unwrap(),
    });
    for lang in [Lang::De, Lang::En] {
        let s = Snapshot::new(lang, &map, Backend::Egui, &[], true);
        let settings = s
            .egui_application()
            .iter()
            .find(|item| item.command == Some(Command::Settings))
            .unwrap();
        assert_eq!(settings.label, lang.app_menu_settings());
        for platform in [Platform::Pc, Platform::Mac] {
            assert_eq!(
                settings.shortcut_label(lang, platform),
                map.label_for_on(&Command::Settings.action().unwrap(), lang, platform)
            );
        }
        let about = s
            .egui_application()
            .iter()
            .find(|item| item.command == Some(Command::About))
            .unwrap();
        assert_eq!(about.label, lang.app_menu_about());
        assert!(about.chord.is_none());
        assert_eq!(s.window_title, lang.app_menu_window());
    }
}

#[test]
fn stale_windows_and_disabled_native_commands_do_nothing() {
    let mut app = App::headless(demo_project());
    let device = app.project().tracks[1].channel.devices[0].id;
    app.dispatch(Action::Plugin(komponist_app::PluginAction::OpenWindow(
        device,
    )));
    assert!(app.menu_command_enabled(Command::FocusPlugin(device)));
    app.dispatch(Action::Plugin(komponist_app::PluginAction::CloseWindow(
        device,
    )));
    let before = app.dispatched().count();
    assert!(!app.dispatch_menu(Command::FocusPlugin(device)));
    assert!(!app.dispatch_menu(Command::FocusPlugin(DeviceId(u64::MAX))));
    assert_eq!(app.dispatched().count(), before);
    app.dispatch(Action::Plugin(komponist_app::PluginAction::OpenWindow(
        device,
    )));
    assert!(app.menu_command_enabled(Command::FocusPlugin(device)));
    app.dispatch(Action::Edit(EditAction::RemoveDevice(device)));
    assert!(!app.dispatch_menu(Command::FocusPlugin(device)));
    assert!(app.dispatch_menu(Command::About));
    assert!(!app.dispatch_menu(Command::Settings));
    assert!(!app.dispatch_menu(Command::Quit));
    assert_eq!(app.ui().dialog, Some(Dialog::About));
    app.dispatch(Action::Ui(UiAction::CloseDialog));
    app.set_menu_backend(Backend::AppKit);
    assert!(app.menu_snapshot().egui_application().is_empty());
    app.set_menu_backend(Backend::Egui);
    assert!(!app.menu_snapshot().egui_application().is_empty());
}

#[test]
fn about_and_settings_restore_focus_and_about_exposes_localized_metadata() {
    for lang in [Lang::De, Lang::En] {
        let mut app = App::headless(Project::new("Menu")).with_lang(lang);
        app.dispatch(Action::Ui(UiAction::Focus(FocusArea::Browser)));
        assert!(app.dispatch_menu(Command::About));
        let mut h = harness(app, 1.0);
        h.get_by_role_and_label(Role::Dialog, lang.app_menu_about());
        h.get_by_label(&lang.about_version(env!("CARGO_PKG_VERSION")));
        h.get_by_label(&lang.about_license(env!("CARGO_PKG_LICENSE")));
        h.get_by_role_and_label(Role::Button, lang.dialog_close())
            .click();
        h.run_steps(3);
        assert_eq!(h.state().ui().dialog, None);
        assert_eq!(h.state().ui().focus, FocusArea::Browser);
        assert!(h.state_mut().dispatch_menu(Command::About));
        h.run_steps(3);
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert_eq!(h.state().ui().dialog, None);
        assert_eq!(h.state().ui().focus, FocusArea::Browser);
        assert!(h.state_mut().dispatch_menu(Command::Settings));
        h.run_steps(3);
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert_eq!(h.state().ui().dialog, None);
        assert_eq!(h.state().ui().focus, FocusArea::Browser);
    }
}

#[test]
fn queued_quit_preserves_dirty_cancel_and_save_flow() {
    let dir = crate::common::scratch("menu-quit");
    let path = dir.join("Menu.komponist");
    let project = Project::new("Menu");
    komponist_app::project_io::save_project(&path, &project).unwrap();
    let mut app = App::headless(project).with_lang(Lang::En);
    app.dispatch(Action::Project(komponist_app::ProjectAction::OpenPath(
        path.clone(),
    )));
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    assert!(app.dispatch_menu(Command::Quit));
    let mut h = harness(app, 1.0);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().doc().is_dirty());
    assert!(h.state().menu_command_enabled(Command::Quit));
    assert!(h.state_mut().dispatch_menu(Command::Quit));
    h.run_steps(3);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert!(!h.state().doc().is_dirty());
    assert!(h.state_mut().close_requested());
    assert!(!h.state_mut().dispatch_menu(Command::Quit));
    assert_eq!(
        komponist_app::project_io::load_project(&path)
            .unwrap()
            .tracks
            .len(),
        1
    );
}

#[test]
fn about_and_fallback_menu_snapshots_in_both_languages() {
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let suffix = if lang == Lang::En { "-en" } else { "" };
        for (ppp, width, height, variant) in [
            (1.0, 720.0, 400.0, ""),
            (2.0, 720.0, 400.0, ""),
            (1.0, 360.0, 300.0, "-narrow"),
        ] {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
            let mut h = komponist_ui::snapshot::builder(ppp)
                .with_context_setup(move |ctx| {
                    komponist_ui::install(ctx);
                    komponist_ui::set_lang(ctx, lang);
                })
                .with_size(vec2(width, height))
                .with_options(SnapshotOptions::new().output_path(path))
                .build_ui(move |ui| {
                    komponist_app::dialogs::about::show(ui.ctx(), lang);
                });
            h.run_steps(3);
            h.snapshot(format!("application-about{variant}{suffix}-{ppp}x"));
            results.extend_harness(&mut h);
        }
        let app = App::headless(Project::new("Menu")).with_lang(lang);
        let mut h = harness(app, 1.0);
        h.get_by_role_and_label(Role::Button, lang.transport_menu())
            .click();
        h.run_steps(4);
        h.get_by_role_and_label(Role::MenuItem, lang.app_menu_about());
        if let Some(cpu) = h.query_by_role_and_label(Role::Status, lang.transport_cpu()) {
            h.mask(cpu.rect().expand(2.0));
        }
        h.snapshot(format!("application-fallback{suffix}-1x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

#[test]
fn installed_native_navigation_is_absent_from_the_actual_local_menu() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    let device = app.project().tracks[1].channel.devices[0].id;
    app.dispatch(Action::Plugin(komponist_app::PluginAction::OpenWindow(
        device,
    )));
    app.set_menu_backend(Backend::AppKit);
    let mut h = harness(app, 1.0);
    h.get_by_role_and_label(Role::Button, Lang::En.transport_menu())
        .click();
    h.run_steps(4);
    h.get_by_role_and_label(Role::MenuItem, Lang::En.transport_menu_save());
    assert!(
        h.query_by_role_and_label(Role::MenuItem, Lang::En.app_menu_window())
            .is_none()
    );
    assert!(
        h.query_by_role_and_label(Role::MenuItem, Lang::En.app_menu_about())
            .is_none()
    );
    h.state_mut().set_menu_backend(Backend::Egui);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, Lang::En.transport_menu())
        .click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::MenuItem, Lang::En.app_menu_window());
    h.get_by_role_and_label(Role::MenuItem, Lang::En.app_menu_about());
}

#[test]
fn fallback_menu_takes_keyboard_commands_and_returns_focus_to_its_button() {
    use egui::Key;

    for lang in [Lang::De, Lang::En] {
        let mut h = harness(App::headless(Project::new("Menu")).with_lang(lang), 1.0);
        let button = h.get_by_role_and_label(Role::Button, lang.transport_menu());
        button.focus();
        h.run_steps(2);
        h.key_press(Key::Enter);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Menu, lang.transport_menu());
        h.key_press(Key::Escape);
        h.run_steps(3);
        assert!(
            h.query_by_role_and_label(Role::Menu, lang.transport_menu())
                .is_none()
        );
        assert!(
            h.get_by_role_and_label(Role::Button, lang.transport_menu())
                .is_focused()
        );

        // Space opens the focused button. It then activates the highlighted
        // About entry without toggling playback or re-opening the menu.
        h.key_press(Key::Space);
        h.run_steps(3);
        h.key_press(Key::ArrowUp);
        h.run_steps(2);
        h.key_press(Key::ArrowUp);
        h.run_steps(2);
        h.key_press(Key::ArrowUp);
        h.run_steps(2);
        h.key_press(Key::Space);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Dialog, lang.app_menu_about());
        assert_eq!(
            h.state().transport(),
            komponist_engine::TransportState::Stopped
        );
        h.key_press(Key::Escape);
        h.run_steps(3);
        assert_eq!(h.state().ui().dialog, None);
        assert!(
            h.get_by_role_and_label(Role::Button, lang.transport_menu())
                .is_focused()
        );
        assert!(
            h.query_by_role_and_label(Role::Menu, lang.transport_menu())
                .is_none()
        );
    }
}

#[test]
fn removing_an_open_plugin_cancels_the_stale_window_menu() {
    let mut app = App::headless(demo_project()).with_lang(Lang::En);
    let device = app.project().tracks[1].channel.devices[0].id;
    app.dispatch(Action::Plugin(komponist_app::PluginAction::OpenWindow(
        device,
    )));
    let lang = Lang::En;
    let mut h = harness(app, 1.0);
    h.get_by_role_and_label(Role::Button, lang.transport_menu())
        .click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::MenuItem, lang.app_menu_window())
        .click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Menu, lang.app_menu_window());
    h.state_mut()
        .dispatch(Action::Plugin(komponist_app::PluginAction::CloseWindow(
            device,
        )));
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(
        h.query_by_role_and_label(Role::Menu, lang.app_menu_window())
            .is_none()
    );
    assert!(
        h.query_by_role_and_label(Role::Menu, lang.transport_menu())
            .is_none()
    );
    assert!(h.state().plugin_window_entries().is_empty());
}
