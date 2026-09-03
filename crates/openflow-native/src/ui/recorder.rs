//! The hotkey recorder's event monitor, shared by Settings and Onboarding.
//!
//! Both windows let the user press a chord and bind it. The window-specific
//! part (which button to retitle, which action to rebind, what to do when the
//! user gives up) stays in the window; what is shared is the fiddly half: a
//! local key-down monitor that has to swallow the keystroke, be removed before
//! the callback can start another recording, and never outlive the window.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

use crate::hotkeys;

/// The chord a key-down event spells, or `None` when it is one
/// `parse_shortcut` cannot express: a bare letter, or a key with no name.
pub fn chord_from_event(event: &NSEvent) -> Option<String> {
    let flags = event.modifierFlags();
    let characters = event.charactersIgnoringModifiers().map(|s| s.to_string());
    let key = hotkeys::key_name(event.keyCode(), characters.as_deref())?;
    hotkeys::shortcut_string(
        flags.contains(NSEventModifierFlags::Control),
        flags.contains(NSEventModifierFlags::Option),
        flags.contains(NSEventModifierFlags::Shift),
        flags.contains(NSEventModifierFlags::Command),
        &key,
    )
}

/// A one-shot listener for the next key chord.
///
/// The monitor is held behind an `Rc` because the handler block has to be able
/// to remove it: the block is `'static` and cannot borrow the recorder.
#[derive(Default)]
pub struct ChordRecorder {
    monitor: Rc<RefCell<Option<Retained<AnyObject>>>>,
}

impl ChordRecorder {
    /// Listen for the next chord and hand it to `done`, which receives `None`
    /// when the key pressed cannot be a global shortcut. Any previous listener
    /// is torn down first, so two Record buttons cannot both be armed.
    pub fn start(&self, done: impl Fn(Option<String>) + 'static) {
        self.stop();
        let slot = Rc::clone(&self.monitor);
        let block = block2::RcBlock::new(move |event: core::ptr::NonNull<NSEvent>| {
            // SAFETY: AppKit hands us a live event for the duration of the call.
            let chord = chord_from_event(unsafe { event.as_ref() });
            // Remove the monitor before reporting. `done` is free to start
            // another recording, and a monitor left installed would go on
            // swallowing every keystroke in the app.
            let monitor = slot.borrow_mut().take();
            if let Some(monitor) = monitor {
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
            done(chord);
            // Swallow the key: the chord must not also reach whatever control
            // has focus behind the recorder.
            core::ptr::null_mut()
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
        };
        *self.monitor.borrow_mut() = monitor;
    }

    /// Stop listening. Safe to call when nothing is armed.
    pub fn stop(&self) {
        let monitor = self.monitor.borrow_mut().take();
        if let Some(monitor) = monitor {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    }
}
