//! The native macOS app menu (issue #47, owner decision 6): "komponist" with About, Settings …,
//! Hide / Hide Others / Show All and Quit; "Fenster" with Minimize, Zoom and the open plugin windows,
//! and the same plugin windows in the Dock menu (#95). Everything else stays in the egui menu bar.
//!
//! Plugin frames on macOS are windows of their bridge processes (#52), so AppKit lists none of them
//! under komponist. The Window and Dock menus list them from
//! [`window_list`](crate::windows::window_list) and are rebuilt when that list or the language
//! changes ([`MacMenu::update`], every frame). A pick queues
//! [`Command::FocusPluginWindow`]: the frame comes to the front with the focus
//! (`PluginAction::FocusWindow`). The Dock menu reaches AppKit through `applicationDockMenu:`
//! ([`crate::macos_delegate`]). Frames the app draws itself (the parameter view, a loading or
//! crashed plugin) are windows of komponist that AppKit would list in the Dock menu as well; they
//! are left out of AppKit's list (`excludedFromWindowsMenu`), so each frame is listed once.
//! Minimize and Zoom go to the key window through the responder chain; there is no
//! `setWindowsMenu`, so AppKit adds no window list of its own to "Fenster".
//!
//! The menu is plain AppKit through `objc2-app-kit` (already linked by eframe and winit). The
//! standard items (Hide …, About) target AppKit itself. Settings and Quit target a small
//! Objective-C object ([`Target`]) that only **queues** a [`Command`] and asks egui for a frame;
//! the eframe shell (`main.rs`) drains the queue at the start of the next frame and dispatches the
//! command's [`Action`] through [`crate::app::App::dispatch`] like a shortcut. So Quit asks to
//! save a dirty project like the close button does, and nothing happens inside a menu callback.
//!
//! ⌘Q and ⌘, are the menu items' key equivalents: AppKit offers a key to the main menu before the
//! window sees it, so the keymap's own ⌘Q / ⌘, bindings only matter where no menu exists (tests).
//!
//! Must be installed after winit has created its default menu (`did_finish_launching`), that is
//! in eframe's app-creation callback; it replaces the default one.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use komponist_i18n::Lang;
use komponist_model::DeviceId;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSDictionary, NSString, ns_string};

use crate::actions::{Action, PluginAction, ProjectAction, UiAction};
use crate::ui_state::SettingsSection;
use crate::windows::window_list::{self, WindowEntry, WindowMenuItem};

/// What a menu item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// "Settings …" (⌘,).
    OpenSettings,
    /// "Quit komponist" (⌘Q).
    Quit,
    /// A plugin window in the Window or Dock menu (#95).
    FocusPluginWindow(DeviceId),
}

impl Command {
    /// The action the app runs for this command.
    pub fn action(self) -> Action {
        match self {
            Command::OpenSettings => Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)),
            Command::Quit => Action::Project(ProjectAction::Quit),
            Command::FocusPluginWindow(device) => Action::Plugin(PluginAction::FocusWindow(device)),
        }
    }
}

/// Commands the menu has queued; drained by the shell each frame.
type Inbox = Arc<Mutex<Vec<Command>>>;

struct Ivars {
    inbox: Inbox,
    ctx: egui::Context,
}

define_class!(
    // The target of the Settings and Quit items.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KomponistMenuTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            self.push(Command::OpenSettings);
        }

        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) {
            // The standard panel with name and version: the process is not an app bundle yet.
            let app = NSApplication::sharedApplication(self.mtm());
            // SAFETY: the keys are the documented `NSAboutPanelOption` names.
            unsafe { app.orderFrontStandardAboutPanelWithOptions(&about_options()) };
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            self.push(Command::Quit);
        }

        // A plugin window of the Window or Dock menu: the item's tag is the device id.
        #[unsafe(method(focusPluginWindow:))]
        fn focus_plugin_window(&self, sender: Option<&NSMenuItem>) {
            if let Some(id) = sender.and_then(|item| u64::try_from(item.tag()).ok()) {
                self.push(Command::FocusPluginWindow(DeviceId(id)));
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    fn new(mtm: MainThreadMarker, inbox: Inbox, ctx: egui::Context) -> Retained<Target> {
        let this = Target::alloc(mtm).set_ivars(Ivars { inbox, ctx });
        unsafe { msg_send![super(this), init] }
    }

    /// Queues `command` and wakes the event loop: the window may not be redrawing.
    fn push(&self, command: Command) {
        let ivars = self.ivars();
        if let Ok(mut q) = ivars.inbox.lock() {
            q.push(command);
        }
        ivars.ctx.request_repaint();
    }
}

/// The installed menu: keeps the items to retitle them when the UI language changes.
pub struct MacMenu {
    inbox: Inbox,
    lang: Lang,
    // Retained so the target outlives the items (items hold their target weakly).
    target: Retained<Target>,
    about: Retained<NSMenuItem>,
    settings: Retained<NSMenuItem>,
    hide: Retained<NSMenuItem>,
    hide_others: Retained<NSMenuItem>,
    show_all: Retained<NSMenuItem>,
    quit: Retained<NSMenuItem>,
    /// "Fenster", rebuilt by [`MacMenu::update`].
    window_menu: Retained<NSMenu>,
    window_root: Retained<NSMenuItem>,
    /// The plugin windows the menus list now.
    windows: Vec<WindowEntry>,
    /// The number of the app's own windows when frames were last left out of AppKit's list.
    app_windows: usize,
}

impl MacMenu {
    /// Installs the menu as the application's main menu in `lang`; `None` off the main thread.
    pub fn install(ctx: &egui::Context, lang: Lang) -> Option<MacMenu> {
        let mtm = MainThreadMarker::new()?;
        let inbox = Inbox::default();
        let target = Target::new(mtm, inbox.clone(), ctx.clone());
        let app = NSApplication::sharedApplication(mtm);

        let item = |title: &str, action: Option<objc2::runtime::Sel>, key: &str| {
            item(mtm, title, action, key)
        };
        let about = item("", Some(sel!(showAbout:)), "");
        let settings = item("", Some(sel!(openSettings:)), ",");
        // SAFETY: `Target` implements the item's selector and `target` keeps it alive.
        unsafe { settings.setTarget(Some(&target)) };
        let hide = item("", Some(sel!(hide:)), "h");
        let hide_others = item("", Some(sel!(hideOtherApplications:)), "h");
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        let show_all = item("", Some(sel!(unhideAllApplications:)), "");
        let quit = item("", Some(sel!(quit:)), "q");
        // SAFETY: `Target` implements the item's selector and `target` keeps it alive.
        unsafe { quit.setTarget(Some(&target)) };

        // SAFETY: `Target` implements the item's selector and `target` keeps it alive.
        unsafe { about.setTarget(Some(&target)) };

        let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("komponist"));
        app_menu.addItem(&about);
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&settings);
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&hide);
        app_menu.addItem(&hide_others);
        app_menu.addItem(&show_all);
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&quit);

        let root_item = NSMenuItem::new(mtm);
        root_item.setSubmenu(Some(&app_menu));
        let window_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
        let window_root = NSMenuItem::new(mtm);
        window_root.setSubmenu(Some(&window_menu));
        let main = NSMenu::new(mtm);
        main.addItem(&root_item);
        main.addItem(&window_root);
        app.setMainMenu(Some(&main));

        let mut menu = MacMenu {
            inbox,
            lang,
            target,
            about,
            settings,
            hide,
            hide_others,
            show_all,
            quit,
            window_menu,
            window_root,
            windows: Vec::new(),
            app_windows: 0,
        };
        menu.retitle(lang);
        menu.rebuild_windows(mtm);
        Some(menu)
    }

    /// Follows the UI language and the open plugin windows (`windows`, from
    /// [`App::plugin_window_entries`](crate::app::App::plugin_window_entries)); call every frame.
    /// Rebuilds only what changed.
    pub fn update(&mut self, lang: Lang, windows: &[WindowEntry]) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let relabel = lang != self.lang;
        if relabel {
            self.retitle(lang);
        }
        if relabel || windows != self.windows.as_slice() {
            self.windows = windows.to_vec();
            self.rebuild_windows(mtm);
            self.app_windows = 0;
        }
        self.leave_frames_out_of_appkit_list(mtm);
    }

    /// Fills "Fenster" and the Dock menu anew.
    fn rebuild_windows(&self, mtm: MainThreadMarker) {
        let lang = self.lang;
        let window_item = |entry: &WindowEntry| {
            let item = item(mtm, &entry.label, Some(sel!(focusPluginWindow:)), "");
            item.setTag(isize::try_from(entry.device.0).unwrap_or(-1));
            // SAFETY: `Target` implements the item's selector and `self.target` keeps it alive.
            unsafe { item.setTarget(Some(&self.target)) };
            item
        };
        let menu = &self.window_menu;
        menu.setTitle(&NSString::from_str(lang.app_menu_window()));
        menu.removeAllItems();
        for entry in window_list::window_menu(lang, &self.windows) {
            let item = match entry {
                WindowMenuItem::Minimize(title) => {
                    item(mtm, title, Some(sel!(performMiniaturize:)), "m")
                }
                WindowMenuItem::Zoom(title) => item(mtm, title, Some(sel!(performZoom:)), ""),
                WindowMenuItem::Separator => NSMenuItem::separatorItem(mtm),
                WindowMenuItem::Window(entry) => window_item(entry),
            };
            menu.addItem(&item);
        }
        self.window_root
            .setTitle(&NSString::from_str(lang.app_menu_window()));
        let dock = if self.windows.is_empty() {
            None
        } else {
            let dock = NSMenu::new(mtm);
            for entry in &self.windows {
                dock.addItem(&window_item(entry));
            }
            Some(dock)
        };
        crate::macos_delegate::set_dock_menu(dock);
    }

    /// Frames the app draws itself are windows of komponist: AppKit would list them in the Dock
    /// menu next to this module's entries. Runs when the number of the app's windows changed (a
    /// frame's viewport appears a pass after its entry).
    fn leave_frames_out_of_appkit_list(&mut self, mtm: MainThreadMarker) {
        let app_windows = NSApplication::sharedApplication(mtm).windows();
        if app_windows.count() == self.app_windows {
            return;
        }
        self.app_windows = app_windows.count();
        let titles: HashSet<String> = self
            .windows
            .iter()
            .map(|w| format!("{} · {}", w.label, komponist_ui::WORDMARK))
            .collect();
        for window in app_windows.iter() {
            if titles.contains(&window.title().to_string()) {
                window.setExcludedFromWindowsMenu(true);
            }
        }
    }

    fn retitle(&mut self, lang: Lang) {
        self.lang = lang;
        let set = |item: &NSMenuItem, text: &str| item.setTitle(&NSString::from_str(text));
        set(&self.about, lang.app_menu_about());
        set(&self.settings, lang.app_menu_settings());
        set(&self.hide, lang.app_menu_hide());
        set(&self.hide_others, lang.app_menu_hide_others());
        set(&self.show_all, lang.app_menu_show_all());
        set(&self.quit, lang.app_menu_quit());
    }

    /// The commands queued since the last call, oldest first.
    pub fn drain(&self) -> Vec<Command> {
        self.inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}

/// A menu item; `action` goes to its target, or along the responder chain without one.
fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: the selectors are AppKit's standard actions or the `Target` methods.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

/// The options of the standard About panel: name and version (an unbundled binary has none).
fn about_options() -> Retained<NSDictionary<NSString, AnyObject>> {
    let name = NSString::from_str("komponist");
    let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
    let keys = [
        ns_string!("ApplicationName"),
        ns_string!("ApplicationVersion"),
    ];
    let values: [&AnyObject; 2] = [&name, &version];
    NSDictionary::from_slices(&keys, &values)
}
