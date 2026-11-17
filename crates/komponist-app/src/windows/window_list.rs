//! The open plugin windows as menus list them (#95): the Window menu of the native menu bar and the
//! Dock menu on macOS (`macos_menu`), and the "Fenster" submenu of the transport bar's "⋯" menu on
//! every platform. On macOS a plugin frame is a window of its bridge process, so macOS itself does
//! not list it under komponist; these menus are the way to pick one.
//!
//! One entry per open plugin window, in project order (tracks top to bottom, each chain left to
//! right, the master last), so the list stays put when frames open and close. The label is the
//! frame's window title without the wordmark, "Surge XT · Pad" (`app-menu-plugin-window`). Picking
//! an entry sends [`PluginAction::FocusWindow`](crate::actions::PluginAction::FocusWindow): the
//! frame comes to the front with the keyboard focus.

use komponist_i18n::Lang;
use komponist_model::{DeviceId, Project};

use crate::app::App;
use crate::ui_state::PluginWindow;

/// One open plugin window in a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    /// The device whose window it is.
    pub device: DeviceId,
    /// "Surge XT · Pad".
    pub label: String,
}

/// The entries for `windows` in `lang`, in project order; windows of devices that are gone are left
/// out.
pub fn entries(lang: Lang, project: &Project, windows: &[PluginWindow]) -> Vec<WindowEntry> {
    let open = |id: DeviceId| windows.iter().any(|w| w.device == id);
    let tracks = project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter().map(move |d| (t.name.as_str(), d)));
    let master = project
        .master
        .devices
        .iter()
        .map(|d| (lang.dialog_master(), d));
    tracks
        .chain(master)
        .filter(|(_, d)| open(d.id))
        .map(|(track, d)| WindowEntry {
            device: d.id,
            label: lang.app_menu_plugin_window(crate::views::plugin_name(lang, &d.plugin), track),
        })
        .collect()
}

impl App {
    /// The open plugin windows as the menus list them (module docs).
    pub fn plugin_window_entries(&self) -> Vec<WindowEntry> {
        entries(self.lang, self.project(), &self.ui.plugin_windows)
    }
}

/// One item of the Window menu of the native menu bar (macOS), top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowMenuItem<'a> {
    /// "Im Dock ablegen" (⌘M): minimises the key window.
    Minimize(&'static str),
    /// "Zoomen": zooms the key window.
    Zoom(&'static str),
    /// A separator line.
    Separator,
    /// An open plugin window.
    Window(&'a WindowEntry),
}

/// The Window menu in `lang`: Minimize and Zoom, then the open plugin windows below a separator.
pub fn window_menu(lang: Lang, entries: &[WindowEntry]) -> Vec<WindowMenuItem<'_>> {
    use crate::application_menu::{Backend, Command, NativeCommand, Snapshot};
    let snapshot = Snapshot::new(
        lang,
        &crate::Keymap::default_bindings_for(crate::platform::Platform::Mac),
        Backend::AppKit,
        entries,
        true,
    );
    snapshot
        .native_window()
        .iter()
        .filter_map(|item| match item.command {
            Some(Command::Native(NativeCommand::Minimize)) => {
                Some(WindowMenuItem::Minimize(lang.app_menu_minimize()))
            }
            Some(Command::Native(NativeCommand::Zoom)) => {
                Some(WindowMenuItem::Zoom(lang.app_menu_zoom()))
            }
            Some(Command::FocusPlugin(device)) => entries
                .iter()
                .find(|entry| entry.device == device)
                .map(WindowMenuItem::Window),
            None => Some(WindowMenuItem::Separator),
            _ => None,
        })
        .collect()
}
