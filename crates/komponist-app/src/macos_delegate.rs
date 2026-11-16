//! What komponist adds to the application delegate (macOS): the Dock menu (`applicationDockMenu:`,
//! #95).
//!
//! winit 0.30 owns `NSApplication`'s delegate: its class `WinitApplicationDelegate` implements only
//! `applicationDidFinishLaunching:` and `applicationWillTerminate:`, and winit offers no hook to
//! add more. komponist therefore **adds methods to that class at runtime** (`class_addMethod`):
//!
//! - [`install`] (called in `main` before eframe starts the event loop) observes
//!   `NSApplicationWillFinishLaunchingNotification`. When it arrives winit has set its delegate,
//!   and AppKit has not yet asked it anything this module answers.
//! - The observer adds the missing methods to the delegate's class, never replacing one the class
//!   already implements (a later winit that adds its own keeps it, and the log says so), and sets
//!   the delegate again so AppKit reads anew which methods it implements.
//! - The methods only hand out what the app prepared ([`set_dock_menu`]) or queue what happened;
//!   nothing acts inside them.
//!
//! Alternatives: a proxy delegate that forwards everything else to winit's would have to be put in
//! place at the same moment and answer `respondsToSelector:` for winit's methods too, which is more
//! code for the same effect; a fork of winit is far heavier; `NSAppleEventManager` handlers cover
//! opening documents but not the Dock menu.

use std::cell::RefCell;
use std::ffi::CStr;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification, NSMenu};
use objc2_foundation::{NSNotification, NSNotificationCenter};

thread_local! {
    /// What `applicationDockMenu:` returns ([`set_dock_menu`]).
    static DOCK_MENU: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
}

/// Sets the menu the Dock shows below the app's windows (`None`: none).
pub fn set_dock_menu(menu: Option<Retained<NSMenu>>) {
    DOCK_MENU.with(|m| *m.borrow_mut() = menu);
}

/// Adds komponist's methods to the application delegate once winit has set it (module docs). Call
/// on the main thread before the event loop runs; does nothing elsewhere.
pub fn install() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let block = RcBlock::new(|_note: NonNull<NSNotification>| {
        if let Some(mtm) = MainThreadMarker::new() {
            extend_delegate(mtm);
        }
    });
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: a valid static notification name; the block lives as long as the observer, which
    // stays for the life of the process (the notification comes once).
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
            None,
            &block,
        )
    };
    std::mem::forget(observer);
}

/// One method komponist adds: selector, implementation, Objective-C type encoding.
struct Method {
    sel: Sel,
    imp: Imp,
    types: &'static CStr,
}

/// The methods to add.
fn methods() -> [Method; 1] {
    [Method {
        sel: sel!(applicationDockMenu:),
        // SAFETY: the signature matches the encoding `@@:@` (returns an object, takes `self`,
        // `_cmd` and the application); the runtime calls it only through that signature.
        imp: unsafe {
            std::mem::transmute::<
                unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut NSMenu,
                Imp,
            >(dock_menu)
        },
        types: c"@@:@",
    }]
}

fn extend_delegate(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        eprintln!("komponist: no application delegate; no Dock menu");
        return;
    };
    let object: &AnyObject = (*delegate).as_ref();
    let class: &AnyClass = object.class();
    for m in methods() {
        if class.responds_to(m.sel) {
            eprintln!(
                "komponist: the application delegate implements {} itself; komponist's is not added",
                m.sel.name().to_string_lossy()
            );
            continue;
        }
        // SAFETY: `class` is a registered class; `imp` has the signature `types` names.
        let added = unsafe {
            objc2::ffi::class_addMethod(
                std::ptr::from_ref(class).cast_mut(),
                m.sel,
                m.imp,
                m.types.as_ptr(),
            )
        };
        if !added.as_bool() {
            eprintln!(
                "komponist: {} not added to the application delegate",
                m.sel.name().to_string_lossy()
            );
        }
    }
    // AppKit may remember which methods a delegate implements when it is set.
    app.setDelegate(None);
    app.setDelegate(Some(&delegate));
}

/// `applicationDockMenu:`: the menu [`set_dock_menu`] set, or none.
unsafe extern "C-unwind" fn dock_menu(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
) -> *mut NSMenu {
    // Not autoreleased: `DOCK_MENU` keeps the menu alive, and the caller does not own the result
    // (Cocoa's naming rules).
    DOCK_MENU.with(|m| {
        m.borrow().as_ref().map_or(std::ptr::null_mut(), |menu| {
            Retained::as_ptr(menu).cast_mut()
        })
    })
}
