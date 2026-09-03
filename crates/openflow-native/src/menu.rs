//! The application menu, which an accessory app never shows and still needs.
//!
//! `LSUIElement` apps have no menu bar, so it is tempting not to build a main
//! menu at all. That is the mistake this file exists to avoid: AppKit routes
//! key equivalents for a key window through `NSApp.mainMenu` before anything
//! else sees them. Without a main menu there is no Cmd+W to close a window, and
//! no Cmd+C, Cmd+V or Cmd+A inside the text fields of Settings and Onboarding,
//! which is exactly where a user pastes an API key.
//!
//! The same is true of Cmd+Q: an accessory app with no application menu has no
//! Quit shortcut at all, and the only way out is the tray.
//!
//! Every item here bar one is a first-responder action AppKit implements
//! itself, so there is nothing to wire and nothing that can go stale. The one
//! exception is About, below, and it is still Apple's panel.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionKey, NSAboutPanelOptionVersion,
    NSApplication, NSMenu, NSMenuItem,
};
use objc2_foundation::{NSDictionary, NSString};

/// Build and install the menu. Called once, before any window exists.
pub fn install(mtm: MainThreadMarker) {
    let main = NSMenu::new(mtm);

    // The application menu, which is where AppKit looks for Cmd+Q. The tray's
    // Quit item routes to `NSApplication::terminate` through
    // `EngineEvent::Navigate("quit")`; `terminate:` down the responder chain is
    // the same call, so the two agree.
    let application = submenu(mtm, &main, "OpenFlow");
    add_about(mtm, &application);
    application.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &application, "Quit OpenFlow", sel!(terminate:), "q");

    let edit = submenu(mtm, &main, "Edit");
    add(mtm, &edit, "Undo", sel!(undo:), "z");
    add(mtm, &edit, "Redo", sel!(redo:), "Z");
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    add(mtm, &edit, "Cut", sel!(cut:), "x");
    add(mtm, &edit, "Copy", sel!(copy:), "c");
    add(mtm, &edit, "Paste", sel!(paste:), "v");
    add(mtm, &edit, "Select All", sel!(selectAll:), "a");

    let window = submenu(mtm, &main, "Window");
    // `performClose:` asks the window's delegate first, so Cmd+W lands in the
    // same `windowShouldClose:` every window here implements: hide, do not
    // release.
    add(mtm, &window, "Close", sel!(performClose:), "w");
    add(mtm, &window, "Minimize", sel!(performMiniaturize:), "m");

    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}

fn submenu(mtm: MainThreadMarker, parent: &NSMenu, title: &str) -> Retained<NSMenu> {
    let title = NSString::from_str(title);
    let item = NSMenuItem::new(mtm);
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
    item.setTitle(&title);
    item.setSubmenu(Some(&menu));
    parent.addItem(&item);
    menu
}

// ── About ─────────────────────────────────────────────────

/// The About item, and the only one here with a target of its own.
///
/// `orderFrontStandardAboutPanel:` with a nil target already reaches
/// `NSApplication` down the responder chain, and it would show *a* panel --
/// but only the one AppKit can assemble from the bundle's `Info.plist`, which
/// means no version at all under `cargo run` (no bundle, no plist) and never
/// the commit, since there is no plist key the panel reads it from. So the
/// item targets an object that hands the panel the two strings explicitly.
///
/// It is still the stock panel: `NSAboutPanelOptionApplicationVersion` is the
/// marketing version and `NSAboutPanelOptionVersion` the build, and AppKit
/// renders the pair as `Version 0.1.0 (a1b2c3d)` -- the same text
/// `--version` prints, assembled by Apple's code. Nothing here draws a window.
fn add_about(mtm: MainThreadMarker, menu: &NSMenu) {
    let target = AboutTarget::new(mtm);
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("About OpenFlow"),
            Some(sel!(openflowShowAbout:)),
            &NSString::from_str(""),
        )
    };
    unsafe { item.setTarget(Some(&target)) };
    menu.addItem(&item);
    // `NSMenuItem.target` is a weak reference, so the menu owning the item is
    // not enough to keep the target alive; a released target turns the item
    // grey and the click into nothing. The menu is installed once for the life
    // of the process, so the target is held for the life of the process too.
    ABOUT_TARGET.with(|slot| *slot.borrow_mut() = Some(target));
}

thread_local! {
    static ABOUT_TARGET: RefCell<Option<Retained<AboutTarget>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class holds no
    // Drop-relevant state beyond its ivars.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowAboutTarget"]
    struct AboutTarget;

    unsafe impl NSObjectProtocol for AboutTarget {}

    impl AboutTarget {
        #[unsafe(method(openflowShowAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::from(self);
            let version = NSString::from_str(crate::version::VERSION);
            let commit = NSString::from_str(crate::version::COMMIT);
            let keys: [&NSAboutPanelOptionKey; 2] = unsafe {
                [
                    NSAboutPanelOptionApplicationVersion,
                    NSAboutPanelOptionVersion,
                ]
            };
            let values: [&AnyObject; 2] = [&version, &commit];
            let options = NSDictionary::from_slices(&keys, &values);
            // SAFETY: the dictionary's keys are the documented option keys and
            // its values are the `NSString`s those two keys are specified to
            // take.
            unsafe {
                NSApplication::sharedApplication(mtm)
                    .orderFrontStandardAboutPanelWithOptions(&options);
            }
            // An accessory app is not the active app when a menu action fires
            // from the tray, and a panel ordered front behind the frontmost
            // application is a panel nobody sees.
            NSApplication::sharedApplication(mtm).activate();
        }
    }
);

impl AboutTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // `set_ivars(())` even with no ivars: it is what turns the `Allocated`
        // into the `PartialInit` a super-`init` takes.
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn add(mtm: MainThreadMarker, menu: &NSMenu, title: &str, action: objc2::runtime::Sel, key: &str) {
    // Target `None`: the action goes down the responder chain to whatever text
    // field or window is first responder, which is what makes these work
    // without wiring.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    menu.addItem(&item);
}
