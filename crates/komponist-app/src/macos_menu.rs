//! The native macOS app menu (issue #47, owner decision 6): "komponist" with About, Settings …,
//! Hide / Hide Others / Show All and Quit. Everything else stays in the egui menu bar.
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

use std::sync::{Arc, Mutex};

use komponist_i18n::Lang;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSDictionary, NSString, ns_string};

use crate::actions::{Action, ProjectAction, UiAction};
use crate::ui_state::SettingsSection;

/// What a menu item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// "Settings …" (⌘,).
    OpenSettings,
    /// "Quit komponist" (⌘Q).
    Quit,
}

impl Command {
    /// The action the app runs for this command.
    pub fn action(self) -> Action {
        match self {
            Command::OpenSettings => Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)),
            Command::Quit => Action::Project(ProjectAction::Quit),
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
    _target: Retained<Target>,
    about: Retained<NSMenuItem>,
    settings: Retained<NSMenuItem>,
    hide: Retained<NSMenuItem>,
    hide_others: Retained<NSMenuItem>,
    show_all: Retained<NSMenuItem>,
    quit: Retained<NSMenuItem>,
}

impl MacMenu {
    /// Installs the menu as the application's main menu in `lang`; `None` off the main thread.
    pub fn install(ctx: &egui::Context, lang: Lang) -> Option<MacMenu> {
        let mtm = MainThreadMarker::new()?;
        let inbox = Inbox::default();
        let target = Target::new(mtm, inbox.clone(), ctx.clone());
        let app = NSApplication::sharedApplication(mtm);

        let item = |title: &str, action: Option<objc2::runtime::Sel>, key: &str| {
            // SAFETY: the selectors are NSApplication's standard actions or the `Target` methods.
            unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    action,
                    &NSString::from_str(key),
                )
            }
        };
        let about = item("", Some(sel!(showAbout:)), "");
        let settings = item("", Some(sel!(openSettings:)), ",");
        // SAFETY: `Target` implements the item's selector and `_target` keeps it alive.
        unsafe { settings.setTarget(Some(&target)) };
        let hide = item("", Some(sel!(hide:)), "h");
        let hide_others = item("", Some(sel!(hideOtherApplications:)), "h");
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        let show_all = item("", Some(sel!(unhideAllApplications:)), "");
        let quit = item("", Some(sel!(quit:)), "q");
        // SAFETY: `Target` implements the item's selector and `_target` keeps it alive.
        unsafe { quit.setTarget(Some(&target)) };

        // SAFETY: `Target` implements the item's selector and `_target` keeps it alive.
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
        let main = NSMenu::new(mtm);
        main.addItem(&root_item);
        app.setMainMenu(Some(&main));

        let mut menu = MacMenu {
            inbox,
            lang,
            _target: target,
            about,
            settings,
            hide,
            hide_others,
            show_all,
            quit,
        };
        menu.retitle(lang);
        Some(menu)
    }

    /// Retitles the items when the UI language changed.
    pub fn set_lang(&mut self, lang: Lang) {
        if lang != self.lang {
            self.retitle(lang);
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
