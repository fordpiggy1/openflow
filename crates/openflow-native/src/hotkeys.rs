//! Global hotkeys, on the same `global-hotkey` crate the Tauri plugin wraps.
//!
//! Hold-to-talk is the whole reason this is not a "shortcut fired" API:
//! `Pressed` starts the capture and `Released` ends it, and the engine's
//! watchdog covers the case where a release is swallowed by another app.

use std::sync::Arc;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use openflow_core::engine::Engine;
use openflow_core::settings::Settings;

/// The two actions OpenFlow binds, in the order the settings window shows them.
pub const ACTIONS: [&str; 2] = ["record", "recopy"];

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    record: Option<HotKey>,
    recopy: Option<HotKey>,
}

impl Hotkeys {
    /// Register both bindings from settings. A binding that will not register
    /// (another app already owns the chord) is reported and skipped rather than
    /// taking the app down: the other one still works, and Settings can rebind.
    pub fn new(settings: &Settings) -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|error| format!("Could not reach the global hotkey service: {}", error))?;
        let mut hotkeys = Self {
            manager,
            record: None,
            recopy: None,
        };
        for action in ACTIONS {
            let shortcut = settings.shortcut(action)?;
            match hotkeys.manager.register(shortcut) {
                Ok(()) => hotkeys.remember(action, Some(shortcut)),
                Err(error) => eprintln!(
                    "The {} shortcut could not be registered: {}. Choose another one in Settings.",
                    action, error
                ),
            }
        }
        Ok(hotkeys)
    }

    fn remember(&mut self, action: &str, shortcut: Option<HotKey>) {
        match action {
            "record" => self.record = shortcut,
            "recopy" => self.recopy = shortcut,
            _ => {}
        }
    }

    /// Route one hotkey event. Runs on the main thread, after the hop.
    pub fn dispatch(&self, engine: &Arc<Engine>, event: GlobalHotKeyEvent) {
        if self.record.map(|key| key.id()) == Some(event.id) {
            match event.state {
                HotKeyState::Pressed => engine.hotkey_pressed(),
                HotKeyState::Released => engine.hotkey_released(),
            }
        } else if self.recopy.map(|key| key.id()) == Some(event.id)
            && matches!(event.state, HotKeyState::Pressed)
        {
            engine.recopy();
        }
    }
}

/// `global-hotkey` calls this from its own thread. Nothing here touches AppKit
/// or the engine; both happen after the hop.
pub fn install_handler() {
    GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
        crate::events::on_main(move || {
            crate::app::with_app(|app| {
                let hotkeys = app.hotkeys().borrow();
                hotkeys.dispatch(app.engine(), event);
            });
        });
    }));
}
