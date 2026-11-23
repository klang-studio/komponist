//! AppKit adapter of the shared application-menu model (#108).
//!
//! Native callbacks queue typed commands and wake the main frame. The shell validates each
//! command before dispatch, including About (which retains AppKit's standard panel). System
//! Hide/Minimize/Zoom commands remain responder-chain actions. Changed snapshots are installed
//! only when no application, Window or Dock popup is tracking.

pub use crate::application_menu::Command;
use crate::application_menu::{Item, NativeCommand, Snapshot};
use crate::keymap::{Key, KeyChord};
use komponist_model::DeviceId;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSEventModifierFlags, NSMenu, NSMenuDelegate, NSMenuItem, NSMenuItemValidation,
};
use objc2_foundation::{NSDictionary, NSString, ns_string};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

type Inbox = Arc<Mutex<Vec<Command>>>;
struct Ivars {
    inbox: Inbox,
    ctx: egui::Context,
    tracking: Cell<usize>,
    enabled: Cell<bool>,
    next_tag: Cell<isize>,
    plugins: RefCell<HashMap<isize, DeviceId>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KomponistMenuTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) { self.push(Command::Settings); }
        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) { self.push(Command::About); }
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) { self.push(Command::Quit); }
        #[unsafe(method(focusPluginWindow:))]
        fn focus_plugin_window(&self, sender: Option<&NSMenuItem>) {
            let device = sender.and_then(|item| self.ivars().plugins.borrow().get(&item.tag()).copied());
            if let Some(device) = device { self.push(Command::FocusPlugin(device)); }
        }
    }
    unsafe impl NSObjectProtocol for Target {}
    unsafe impl NSMenuItemValidation for Target {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.ivars().enabled.get() && (item.tag() < 0 || self.ivars().plugins.borrow().contains_key(&item.tag()))
        }
    }
    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.ivars().tracking.set(self.ivars().tracking.get().saturating_add(1));
        }
        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            self.ivars().tracking.set(self.ivars().tracking.get().saturating_sub(1));
            self.ivars().ctx.request_repaint();
        }
    }
);
impl Target {
    fn new(mtm: MainThreadMarker, inbox: Inbox, ctx: egui::Context) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            inbox,
            ctx,
            tracking: Cell::new(0),
            enabled: Cell::new(true),
            next_tag: Cell::new(1),
            plugins: RefCell::new(HashMap::new()),
        });
        unsafe { msg_send![super(this), init] }
    }
    fn push(&self, command: Command) {
        if let Ok(mut q) = self.ivars().inbox.lock() {
            q.push(command);
        }
        self.ivars().ctx.request_repaint();
    }
}

pub struct MacMenu {
    inbox: Inbox,
    target: Retained<Target>,
    application_menu: Retained<NSMenu>,
    window_menu: Retained<NSMenu>,
    window_root: Retained<NSMenuItem>,
    snapshot: Snapshot,
    app_windows: usize,
}
impl MacMenu {
    /// Called after winit creates its default menu. `None` leaves the egui fallback intact.
    pub fn install(ctx: &egui::Context, snapshot: &Snapshot) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let inbox = Inbox::default();
        let target = Target::new(mtm, inbox.clone(), ctx.clone());
        let application_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("komponist"));
        let window_menu = NSMenu::new(mtm);
        let root = NSMenuItem::new(mtm);
        root.setSubmenu(Some(&application_menu));
        let window_root = NSMenuItem::new(mtm);
        window_root.setSubmenu(Some(&window_menu));
        let main = NSMenu::new(mtm);
        main.addItem(&root);
        main.addItem(&window_root);
        for menu in [&application_menu, &window_menu] {
            menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        }
        NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
        let menu = Self {
            inbox,
            target,
            application_menu,
            window_menu,
            window_root,
            snapshot: snapshot.clone(),
            app_windows: 0,
        };
        menu.rebuild(mtm);
        Some(menu)
    }

    /// No native-menu mutation while a popup is active; a close schedules the deferred update.
    pub fn update(&mut self, snapshot: &Snapshot) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if self.target.ivars().tracking.get() != 0 {
            return;
        }
        if &self.snapshot != snapshot {
            self.snapshot = snapshot.clone();
            self.rebuild(mtm);
            self.app_windows = 0;
        }
        self.leave_frames_out_of_appkit_list(mtm);
    }

    fn rebuild(&self, mtm: MainThreadMarker) {
        self.target.ivars().plugins.borrow_mut().clear();
        self.target
            .ivars()
            .enabled
            .set(self.snapshot.command_enabled(Command::Settings));
        self.application_menu.removeAllItems();
        for entry in self.snapshot.native_application() {
            self.application_menu.addItem(&self.item(mtm, entry));
        }
        self.window_menu.removeAllItems();
        self.window_menu
            .setTitle(&NSString::from_str(&self.snapshot.window_title));
        self.window_root
            .setTitle(&NSString::from_str(&self.snapshot.window_title));
        for entry in self.snapshot.native_window() {
            self.window_menu.addItem(&self.item(mtm, entry));
        }
        let dock = if self.snapshot.dock().is_empty() {
            None
        } else {
            let dock = NSMenu::new(mtm);
            dock.setDelegate(Some(ProtocolObject::from_ref(&*self.target)));
            for entry in self.snapshot.dock() {
                dock.addItem(&self.item(mtm, entry));
            }
            Some(dock)
        };
        crate::macos_delegate::set_dock_menu(dock);
    }

    fn item(&self, mtm: MainThreadMarker, entry: &Item) -> Retained<NSMenuItem> {
        let Some(command) = entry.command else {
            return NSMenuItem::separatorItem(mtm);
        };
        let (selector, targeted, fixed_tag) = match command {
            Command::About => (sel!(showAbout:), true, -1),
            Command::Settings => (sel!(openSettings:), true, -2),
            Command::Quit => (sel!(quit:), true, -3),
            Command::FocusPlugin(_) => (sel!(focusPluginWindow:), true, 0),
            Command::Native(NativeCommand::Hide) => (sel!(hide:), false, 0),
            Command::Native(NativeCommand::HideOthers) => (sel!(hideOtherApplications:), false, 0),
            Command::Native(NativeCommand::ShowAll) => (sel!(unhideAllApplications:), false, 0),
            Command::Native(NativeCommand::Minimize) => (sel!(performMiniaturize:), false, 0),
            Command::Native(NativeCommand::Zoom) => (sel!(performZoom:), false, 0),
            _ => unreachable!("project actions remain in egui"),
        };
        let (key, modifiers) = key_equivalent(entry.chord);
        // SAFETY: the selector is implemented by Target or is an AppKit responder action.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(&entry.label),
                Some(selector),
                &NSString::from_str(&key),
            )
        };
        item.setKeyEquivalentModifierMask(modifiers);
        item.setEnabled(entry.enabled);
        item.setTag(fixed_tag);
        if targeted {
            // SAFETY: retained Target implements the selector and outlives all menu items.
            unsafe { item.setTarget(Some(&self.target)) };
        }
        if let Command::FocusPlugin(device) = command {
            let tag = self.target.ivars().next_tag.get();
            if let Some(next) = tag.checked_add(1) {
                self.target.ivars().next_tag.set(next);
                self.target.ivars().plugins.borrow_mut().insert(tag, device);
                item.setTag(tag);
            } else {
                item.setEnabled(false);
            }
        }
        item
    }

    /// Standard AppKit panel, called by the shell after validating a queued About command.
    pub fn show_about(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: `mtm` proves this runs on the main thread, as AppKit requires. The options dictionary
        // holds only the documented keys `ApplicationName` and `ApplicationVersion`, both with NSString
        // values, and stays alive for the whole call; AppKit copies what it keeps.
        unsafe {
            NSApplication::sharedApplication(mtm)
                .orderFrontStandardAboutPanelWithOptions(&about_options())
        };
    }

    pub fn drain(&self) -> Vec<Command> {
        self.inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
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
            .snapshot
            .dock()
            .iter()
            .map(|w| format!("{} · {}", w.label, komponist_ui::WORDMARK))
            .collect();
        for window in app_windows.iter() {
            if titles.contains(&window.title().to_string()) {
                window.setExcludedFromWindowsMenu(true);
            }
        }
    }
}

/// Application chords come from Keymap; only AppKit responder chords are defined by the model.
fn key_equivalent(chord: Option<KeyChord>) -> (String, NSEventModifierFlags) {
    let Some(chord) = chord else {
        return (String::new(), NSEventModifierFlags::empty());
    };
    let key = match chord.key {
        Key::Comma => ",".to_owned(),
        key => {
            let name = format!("{key:?}");
            if name.len() == 1 {
                name.to_ascii_lowercase()
            } else {
                String::new()
            }
        }
    };
    let mut modifiers = NSEventModifierFlags::empty();
    if chord.mods.command {
        modifiers |= NSEventModifierFlags::Command;
    }
    if chord.mods.shift {
        modifiers |= NSEventModifierFlags::Shift;
    }
    if chord.mods.alt {
        modifiers |= NSEventModifierFlags::Option;
    }
    (key, modifiers)
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
