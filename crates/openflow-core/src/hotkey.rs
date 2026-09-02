//! Parsing the shortcut strings the settings table stores.
//!
//! The parsed type is `global_hotkey::hotkey::HotKey`, which
//! `tauri-plugin-global-shortcut` re-exports as `Shortcut`, so one parse serves
//! both the Tauri shell and a host that drives `global-hotkey` directly.

pub use global_hotkey::hotkey::{Code, HotKey, Modifiers};

/// The hotkeys OpenFlow binds, and what each one is bound to out of the box.
pub const HOTKEY_DEFAULTS: &[(&str, &str)] = &[("record", "Option+V"), ("recopy", "Ctrl+Shift+V")];

pub fn default_shortcut(action: &str) -> Option<&'static str> {
    HOTKEY_DEFAULTS
        .iter()
        .find(|(name, _)| *name == action)
        .map(|(_, shortcut)| *shortcut)
}

/// The settings key holding the user's binding for `action`.
pub fn setting_key(action: &str) -> String {
    format!("hotkey_{}", action)
}

pub fn parse_shortcut(s: &str) -> Result<HotKey, String> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return Err("Empty shortcut".to_string());
    }

    let mut modifiers = Modifiers::empty();
    let mut key_str = "";

    for part in &parts {
        let lower = part.to_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" | "option" | "opt" => modifiers |= Modifiers::ALT,
            "cmd" | "command" | "meta" | "super" => modifiers |= Modifiers::META,
            _ => key_str = part,
        }
    }

    let code = match key_str.to_lowercase().as_str() {
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "escape" | "esc" => Code::Escape,
        "backspace" => Code::Backspace,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        _ => return Err(format!("Unknown key: {}", key_str)),
    };

    let mods = if modifiers.is_empty() {
        None
    } else {
        Some(modifiers)
    };
    Ok(HotKey::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_record_shortcut() {
        let shortcut = parse_shortcut("Option+V").expect("default record shortcut should parse");
        assert_eq!(shortcut, HotKey::new(Some(Modifiers::ALT), Code::KeyV));
    }

    #[test]
    fn parses_default_recopy_shortcut_case_insensitively() {
        let shortcut =
            parse_shortcut("ctrl+shift+v").expect("default recopy shortcut should parse");
        assert_eq!(
            shortcut,
            HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV)
        );
    }

    #[test]
    fn rejects_unknown_shortcut_keys() {
        assert!(parse_shortcut("Option+NotAKey").is_err());
    }

    #[test]
    fn maps_each_hotkey_action_to_its_registered_default() {
        assert_eq!(default_shortcut("record"), Some("Option+V"));
        assert_eq!(default_shortcut("recopy"), Some("Ctrl+Shift+V"));
        assert_eq!(default_shortcut("unknown"), None);
    }

    /// Every default in the table has to parse, or the fallback path in
    /// `Settings::shortcut` has nothing to fall back to.
    #[test]
    fn every_default_binding_parses() {
        for (action, shortcut) in HOTKEY_DEFAULTS {
            assert!(
                parse_shortcut(shortcut).is_ok(),
                "the default binding for {action} must parse"
            );
        }
    }
}
