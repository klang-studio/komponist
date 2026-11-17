//! Shared application, project and Window menus (#108).
//!
//! Adapters project these definitions; the installed backend, never shortcut-label platform or
//! a compile-time OS check, owns application/window navigation. Native picks are revalidated
//! by `App::dispatch_menu` at the next frame boundary. This module never runs on the audio thread.

use komponist_i18n::Lang;
use komponist_model::DeviceId;

use crate::App;
use crate::actions::{Action, EditAction, PluginAction, ProjectAction, UiAction};
use crate::keymap::{Key, KeyChord, Keymap, Modifiers};
use crate::platform::Platform;
use crate::ui_state::SettingsSection;
use crate::windows::window_list::WindowEntry;

/// Actual installed presentation. A failed or absent native installation stays `Egui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    #[default]
    Egui,
    AppKit,
    Win32,
}

/// System responder operations; they are not synthetic cross-platform application actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCommand {
    Hide,
    HideOthers,
    ShowAll,
    Minimize,
    Zoom,
}

/// Stable semantic identity, shared by every menu adapter and the command inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    About,
    Settings,
    Quit,
    New,
    Open,
    ImportAudio,
    NewAudioTrack,
    Save,
    SaveAs,
    CollectAndSave,
    Export,
    StartScreen,
    FocusPlugin(DeviceId),
    Native(NativeCommand),
}

impl Command {
    /// Native-only operations stay in their adapter; all other actions use central dispatch.
    pub fn action(self) -> Option<Action> {
        Some(match self {
            Self::About => Action::Ui(UiAction::OpenAbout),
            Self::Settings => Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)),
            Self::Quit => Action::Project(ProjectAction::Quit),
            Self::New => Action::Project(ProjectAction::New),
            Self::Open => Action::Project(ProjectAction::Open),
            Self::ImportAudio => Action::Project(ProjectAction::ImportAudio),
            Self::NewAudioTrack => Action::Edit(EditAction::NewAudioTrack),
            Self::Save => Action::Project(ProjectAction::Save),
            Self::SaveAs => Action::Project(ProjectAction::SaveAs),
            Self::CollectAndSave => Action::Project(ProjectAction::CollectAndSave),
            Self::Export => Action::Project(ProjectAction::OpenExportDialog),
            Self::StartScreen => Action::Project(ProjectAction::ShowStartScreen),
            Self::FocusPlugin(device) => Action::Plugin(PluginAction::FocusWindow(device)),
            Self::Native(_) => return None,
        })
    }
}

/// A localized command or separator. Chords remain data until an adapter formats them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub command: Option<Command>,
    pub label: String,
    pub chord: Option<KeyChord>,
    pub enabled: bool,
}

impl Item {
    fn separator() -> Self {
        Self {
            command: None,
            label: String::new(),
            chord: None,
            enabled: false,
        }
    }

    pub fn shortcut_label(&self, lang: Lang, platform: Platform) -> Option<String> {
        self.chord.map(|chord| chord.label_on(lang, platform))
    }
}

/// One comparable snapshot; adapters update native state only when this changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub backend: Backend,
    pub lang: Lang,
    pub window_title: String,
    pub project: Vec<Item>,
    application: Vec<Item>,
    window: Vec<Item>,
    dock: Vec<Item>,
}

impl Snapshot {
    pub fn new(
        lang: Lang,
        keymap: &Keymap,
        backend: Backend,
        windows: &[WindowEntry],
        enabled: bool,
    ) -> Self {
        let item = |command: Command, label: &str| Item {
            command: Some(command),
            label: label.to_owned(),
            chord: command
                .action()
                .and_then(|action| keymap.chord_for(&action)),
            enabled,
        };
        let project = [
            (Command::New, lang.transport_menu_new()),
            (Command::Open, lang.transport_menu_open()),
            (Command::ImportAudio, lang.menu_import_audio()),
            (Command::NewAudioTrack, lang.menu_new_audio_track()),
            (Command::Save, lang.transport_menu_save()),
            (Command::SaveAs, lang.transport_menu_save_as()),
            (Command::CollectAndSave, lang.transport_menu_collect()),
            (Command::Export, lang.transport_menu_export()),
            (Command::StartScreen, lang.transport_menu_start()),
        ]
        .into_iter()
        .map(|(command, title)| item(command, title))
        .collect();
        let mut application = vec![item(Command::About, lang.app_menu_about())];
        if backend == Backend::AppKit {
            application.push(Item::separator());
        }
        application.push(item(Command::Settings, lang.app_menu_settings()));
        let native = |command, title: &str, chord| Item {
            command: Some(Command::Native(command)),
            label: title.to_owned(),
            chord,
            enabled: true,
        };
        if backend == Backend::AppKit {
            application.push(Item::separator());
            application.extend([
                native(
                    NativeCommand::Hide,
                    lang.app_menu_hide(),
                    Some(KeyChord::command(Key::H)),
                ),
                native(
                    NativeCommand::HideOthers,
                    lang.app_menu_hide_others(),
                    Some(KeyChord {
                        key: Key::H,
                        mods: Modifiers {
                            command: true,
                            alt: true,
                            ..Modifiers::NONE
                        },
                    }),
                ),
                native(NativeCommand::ShowAll, lang.app_menu_show_all(), None),
                Item::separator(),
            ]);
        }
        application.push(item(Command::Quit, lang.app_menu_quit()));
        let dock: Vec<_> = windows
            .iter()
            .map(|entry| item(Command::FocusPlugin(entry.device), &entry.label))
            .collect();
        let mut window = Vec::new();
        if backend == Backend::AppKit {
            window.extend([
                native(
                    NativeCommand::Minimize,
                    lang.app_menu_minimize(),
                    Some(KeyChord::command(Key::M)),
                ),
                native(NativeCommand::Zoom, lang.app_menu_zoom(), None),
            ]);
            if !dock.is_empty() {
                window.push(Item::separator());
            }
        }
        window.extend(dock.iter().cloned());
        Self {
            backend,
            lang,
            window_title: lang.app_menu_window().to_owned(),
            project,
            application,
            window,
            dock,
        }
    }

    /// Local fallback application section, omitted only after native installation succeeds.
    pub fn egui_application(&self) -> &[Item] {
        if self.backend == Backend::Egui {
            &self.application
        } else {
            &[]
        }
    }

    pub fn egui_window(&self) -> &[Item] {
        if self.backend == Backend::Egui {
            &self.window
        } else {
            &[]
        }
    }

    pub fn native_application(&self) -> &[Item] {
        if self.backend != Backend::Egui {
            &self.application
        } else {
            &[]
        }
    }

    pub fn native_window(&self) -> &[Item] {
        if self.backend != Backend::Egui {
            &self.window
        } else {
            &[]
        }
    }

    /// AppKit's Dock is a separate OS entry point, with the same stable plugin identities.
    pub fn dock(&self) -> &[Item] {
        if self.backend == Backend::AppKit {
            &self.dock
        } else {
            &[]
        }
    }

    pub fn command_enabled(&self, command: Command) -> bool {
        self.project
            .iter()
            .chain(&self.application)
            .chain(&self.window)
            .any(|item| item.command == Some(command) && item.enabled)
    }
}

impl App {
    /// Call only after a native adapter has successfully installed; `Egui` restores the fallback.
    pub fn set_menu_backend(&mut self, backend: Backend) {
        self.shell.menu_backend = backend;
    }

    pub fn menu_snapshot(&self) -> Snapshot {
        Snapshot::new(
            self.lang,
            &self.keymap,
            self.shell.menu_backend,
            &self.plugin_window_entries(),
            self.ui.dialog.is_none()
                && self.dialogs.overlay.is_none()
                && !self.dialogs.close_now
                && !self.dialogs.quit_confirmed,
        )
    }

    /// Reject queued picks whose target closed, became disabled, or is now behind a modal.
    pub fn menu_command_enabled(&self, command: Command) -> bool {
        self.menu_snapshot().command_enabled(command)
    }

    /// Called before `App::show` for native events; egui selections use the same action dispatcher.
    pub fn dispatch_menu(&mut self, command: Command) -> bool {
        if !self.menu_command_enabled(command) {
            return false;
        }
        if let Some(action) = command.action() {
            self.dispatch(action);
            true
        } else {
            false
        }
    }
}
