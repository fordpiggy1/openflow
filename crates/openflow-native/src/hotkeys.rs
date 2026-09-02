//! Global hotkeys, on the same `global-hotkey` crate the Tauri plugin wraps.
//!
//! Hold-to-talk is the whole reason this is not a "shortcut fired" API:
//! `Pressed` starts the capture and `Released` ends it, and the engine's
//! watchdog covers the case where a release is swallowed by another app.

use std::sync::Arc;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use openflow_core::engine::Engine;
use openflow_core::hotkey;
use openflow_core::settings::Settings;

/// The two actions OpenFlow binds, in the order the settings window shows them.
pub const ACTIONS: [&str; 2] = ["record", "recopy"];

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    record: Option<HotKey>,
    recopy: Option<HotKey>,
    /// The binding a hotkey recorder has temporarily taken off the system, so
    /// pressing it types a chord instead of starting a capture. At most one,
    /// because only one recorder can be listening.
    suspended: Option<(String, HotKey)>,
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
            suspended: None,
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

    fn current(&self, action: &str) -> Option<HotKey> {
        match action {
            "record" => self.record,
            "recopy" => self.recopy,
            _ => None,
        }
    }

    /// Release `action`'s chord while a recorder is listening for it.
    ///
    /// Without this, pressing the current record shortcut to re-record it would
    /// start a capture instead: the global registration wins, and the local
    /// event monitor never sees the key. The binding is remembered, not
    /// forgotten, and [`Self::resume`] puts it back.
    pub fn suspend(&mut self, action: &str) {
        self.resume();
        let Some(shortcut) = self.current(action) else {
            return;
        };
        if self.manager.unregister(shortcut).is_ok() {
            self.suspended = Some((action.to_string(), shortcut));
        }
    }

    /// Put back whatever [`Self::suspend`] took, if anything. A chord another
    /// app grabbed in the meantime leaves the action unbound rather than
    /// leaving the table claiming a registration that does not exist.
    pub fn resume(&mut self) {
        let Some((action, shortcut)) = self.suspended.take() else {
            return;
        };
        if self.manager.register(shortcut).is_err() {
            self.remember(&action, None);
        }
    }

    /// Point `action` at `shortcut_str` and save it. The new chord is
    /// registered before the old one is released, so a rejected chord leaves
    /// the action still working; and if saving fails the registration is rolled
    /// back, so the menu bar never disagrees with the database.
    pub fn rebind(
        &mut self,
        settings: &Settings,
        action: &str,
        shortcut_str: &str,
    ) -> Result<(), String> {
        if hotkey::default_shortcut(action).is_none() {
            return Err("Unknown hotkey action".to_string());
        }
        let new = hotkey::parse_shortcut(shortcut_str)
            .map_err(|error| format!("Invalid shortcut '{}': {}", shortcut_str, error))?;
        let old = self.current(action);
        if old == Some(new) {
            return settings.set_hotkey(action, shortcut_str);
        }
        self.manager
            .register(new)
            .map_err(|error| format!("Failed to register shortcut: {}", error))?;
        if let Some(old) = old {
            if let Err(error) = self.manager.unregister(old) {
                let _ = self.manager.unregister(new);
                return Err(format!("Could not replace the old shortcut: {}", error));
            }
        }
        if let Err(error) = settings.set_hotkey(action, shortcut_str) {
            let _ = self.manager.unregister(new);
            if let Some(old) = old {
                let _ = self.manager.register(old);
            }
            return Err(error);
        }
        self.remember(action, Some(new));
        Ok(())
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

// ── The recorder field's string format ────────────────────

/// macOS virtual key codes for the keys that have no printable character, so a
/// recorder field can name them the way `parse_shortcut` expects.
const SPECIAL_KEYS: &[(u16, &str)] = &[
    (36, "Enter"),
    (48, "Tab"),
    (49, "Space"),
    (51, "Backspace"),
    (53, "Escape"),
    (122, "F1"),
    (120, "F2"),
    (99, "F3"),
    (118, "F4"),
    (96, "F5"),
    (97, "F6"),
    (98, "F7"),
    (100, "F8"),
    (101, "F9"),
    (109, "F10"),
    (103, "F11"),
    (111, "F12"),
];

/// The key half of a chord, as `parse_shortcut` spells it, or `None` when the
/// key is one it cannot express.
pub fn key_name(key_code: u16, characters: Option<&str>) -> Option<String> {
    if let Some((_, name)) = SPECIAL_KEYS.iter().find(|(code, _)| *code == key_code) {
        return Some((*name).to_string());
    }
    let first = characters?.chars().next()?;
    if first.is_ascii_alphanumeric() {
        Some(first.to_ascii_uppercase().to_string())
    } else {
        None
    }
}

/// The chord as a settings string. Modifier order is fixed so the same chord
/// always writes the same string, and it is the order the shipped defaults use
/// (`Ctrl+Shift+V`, `Option+V`).
///
/// A chord with no modifier is refused: a bare letter as a global hotkey would
/// swallow that letter in every app on the machine.
pub fn shortcut_string(
    control: bool,
    option: bool,
    shift: bool,
    command: bool,
    key: &str,
) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if control {
        parts.push("Ctrl");
    }
    if option {
        parts.push("Option");
    }
    if shift {
        parts.push("Shift");
    }
    if command {
        parts.push("Cmd");
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// The chord a `HotKey` represents, for showing a saved binding in the recorder
/// field.
pub fn describe(shortcut: &HotKey) -> String {
    let mods = shortcut.mods;
    let key = code_name(shortcut.key).unwrap_or("?");
    // `HotKey::new` folds META into SUPER, so a chord built from "Cmd" carries
    // SUPER by the time it is stored. Reading only META would silently drop the
    // Command key from every description.
    shortcut_string(
        mods.contains(Modifiers::CONTROL),
        mods.contains(Modifiers::ALT),
        mods.contains(Modifiers::SHIFT),
        mods.contains(Modifiers::META) || mods.contains(Modifiers::SUPER),
        key,
    )
    .unwrap_or_else(|| key.to_string())
}

fn code_name(code: Code) -> Option<&'static str> {
    Some(match code {
        Code::Space => "Space",
        Code::Enter => "Enter",
        Code::Tab => "Tab",
        Code::Escape => "Escape",
        Code::Backspace => "Backspace",
        Code::KeyA => "A",
        Code::KeyB => "B",
        Code::KeyC => "C",
        Code::KeyD => "D",
        Code::KeyE => "E",
        Code::KeyF => "F",
        Code::KeyG => "G",
        Code::KeyH => "H",
        Code::KeyI => "I",
        Code::KeyJ => "J",
        Code::KeyK => "K",
        Code::KeyL => "L",
        Code::KeyM => "M",
        Code::KeyN => "N",
        Code::KeyO => "O",
        Code::KeyP => "P",
        Code::KeyQ => "Q",
        Code::KeyR => "R",
        Code::KeyS => "S",
        Code::KeyT => "T",
        Code::KeyU => "U",
        Code::KeyV => "V",
        Code::KeyW => "W",
        Code::KeyX => "X",
        Code::KeyY => "Y",
        Code::KeyZ => "Z",
        Code::Digit0 => "0",
        Code::Digit1 => "1",
        Code::Digit2 => "2",
        Code::Digit3 => "3",
        Code::Digit4 => "4",
        Code::Digit5 => "5",
        Code::Digit6 => "6",
        Code::Digit7 => "7",
        Code::Digit8 => "8",
        Code::Digit9 => "9",
        Code::F1 => "F1",
        Code::F2 => "F2",
        Code::F3 => "F3",
        Code::F4 => "F4",
        Code::F5 => "F5",
        Code::F6 => "F6",
        Code::F7 => "F7",
        Code::F8 => "F8",
        Code::F9 => "F9",
        Code::F10 => "F10",
        Code::F11 => "F11",
        Code::F12 => "F12",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorder writes settings strings, so everything it can produce has
    /// to survive `parse_shortcut` unchanged. A format the parser rejects would
    /// silently fall back to the default binding.
    #[test]
    fn every_recorded_chord_parses_back_to_itself() {
        let cases = [
            (false, true, false, false, "V", "Option+V"),
            (true, false, true, false, "V", "Ctrl+Shift+V"),
            (false, false, true, true, "K", "Shift+Cmd+K"),
            (true, true, true, true, "F5", "Ctrl+Option+Shift+Cmd+F5"),
            (false, false, false, true, "Space", "Cmd+Space"),
        ];
        for (control, option, shift, command, key, expected) in cases {
            let recorded = shortcut_string(control, option, shift, command, key)
                .unwrap_or_else(|| panic!("{expected} should be recordable"));
            assert_eq!(recorded, expected);
            let parsed = hotkey::parse_shortcut(&recorded)
                .unwrap_or_else(|error| panic!("{recorded} should parse: {error}"));
            assert_eq!(describe(&parsed), expected, "{recorded} should round-trip");
        }

        // The parser is order-insensitive, so a binding a user saved in the old
        // web settings screen still resolves to the same chord; only the
        // spelling the recorder writes back is fixed.
        assert_eq!(
            hotkey::parse_shortcut("Cmd+Shift+K"),
            hotkey::parse_shortcut("Shift+Cmd+K")
        );
    }

    /// The two shipped defaults are the strings this function has to be able to
    /// produce, or a user who re-records the default gets a different string.
    #[test]
    fn the_shipped_defaults_are_recordable_strings() {
        for (action, default) in hotkey::HOTKEY_DEFAULTS {
            let parsed = hotkey::parse_shortcut(default)
                .unwrap_or_else(|_| panic!("the {action} default must parse"));
            assert_eq!(
                describe(&parsed),
                *default,
                "the recorder must spell the {action} default exactly as shipped"
            );
        }
    }

    /// A bare key would take that key away from every app on the machine.
    #[test]
    fn a_chord_without_a_modifier_is_refused() {
        assert_eq!(shortcut_string(false, false, false, false, "V"), None);
        assert_eq!(shortcut_string(false, true, false, false, ""), None);
    }

    #[test]
    fn key_names_come_from_the_keycode_or_the_character() {
        assert_eq!(key_name(49, Some(" ")).as_deref(), Some("Space"));
        assert_eq!(key_name(53, Some("\u{1b}")).as_deref(), Some("Escape"));
        assert_eq!(key_name(96, None).as_deref(), Some("F5"));
        assert_eq!(key_name(9, Some("v")).as_deref(), Some("V"));
        assert_eq!(key_name(9, Some("7")).as_deref(), Some("7"));
        // A dead key or a punctuation mark `parse_shortcut` has no name for.
        assert_eq!(key_name(24, Some("=")), None);
        assert_eq!(key_name(9, None), None);
    }
}
