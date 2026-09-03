//! The application menu, which an accessory app never shows and still needs.
//!
//! `LSUIElement` apps have no menu bar, so it is tempting not to build a main
//! menu at all. That is the mistake this file exists to avoid: AppKit routes
//! key equivalents for a key window through `NSApp.mainMenu` before anything
//! else sees them. Without a main menu there is no Cmd+W to close a window, and
//! no Cmd+C, Cmd+V or Cmd+A inside the text fields of Settings and Onboarding,
//! which is exactly where a user pastes an API key.
//!
//! Every item here is a first-responder action AppKit implements itself, so
//! there is nothing to wire and nothing that can go stale.

use objc2::rc::Retained;
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

/// Build and install the menu. Called once, before any window exists.
pub fn install(mtm: MainThreadMarker) {
    let main = NSMenu::new(mtm);

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
