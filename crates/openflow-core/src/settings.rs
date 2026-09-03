//! Typed access to the settings table, and the one place that decides which
//! keys are secrets.
//!
//! Every default here matches what the settings UI shows for an unset key, so a
//! fresh install behaves the same whether the user has opened Settings or not.

use crate::db::Database;
use crate::hotkey::{self, HotKey};
use crate::insert::{ClipboardPolicy, InsertMethod};
use crate::secrets::SecretStore;
use crate::transcribe::Provider;

/// Keys that live in the OS keychain and never in the settings table.
pub const SECRET_SETTINGS: &[&str] = &["api_key", "formatting_api_key", "tts_api_key"];

pub fn is_secret_setting(key: &str) -> bool {
    SECRET_SETTINGS.contains(&key)
}

/// The transcription provider a fresh install uses.
pub const DEFAULT_PROVIDER: &str = "groq";
/// The voice provider a fresh install uses.
pub const DEFAULT_TTS_PROVIDER: &str = "openrouter";
/// Where the overlay pill sits until the user drags it somewhere else.
pub const DEFAULT_OVERLAY_POSITION: &str = "left-center";
/// The audio container speech is requested in when nothing else is chosen.
pub const DEFAULT_TTS_RESPONSE_FORMAT: &str = "mp3";

/// The settings table plus the keychain, behind one typed surface.
pub struct Settings {
    db: Database,
    secrets: SecretStore,
}

/// The live-preview decision, split out from the store so the rule can be read
/// and tested on its own. Anything that is not the literal "true" or "false"
/// falls back to the endpoint: a half-written setting must never be what starts
/// billing a hosted provider every 800 ms.
pub fn live_preview_allowed(setting: Option<&str>, provider: &Provider) -> bool {
    match setting {
        Some("true") => true,
        Some("false") => false,
        _ => provider.is_custom(),
    }
}

impl Settings {
    pub fn new(db: Database, secrets: SecretStore) -> Self {
        Self { db, secrets }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    /// Read any key by name, routing the secret ones to the keychain.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        if is_secret_setting(key) {
            self.secrets.get(key)
        } else {
            Ok(self.db.get_setting(key))
        }
    }

    /// Write any key by name, routing the secret ones to the keychain.
    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        if is_secret_setting(key) {
            self.secrets.set(key, value)
        } else {
            self.db.set_setting(key, value)
        }
    }

    /// Lift any secret still sitting in the settings table into the keychain,
    /// and only then drop the plaintext copy. A failed write leaves the
    /// plaintext where it is rather than losing the user's key.
    pub fn migrate_secrets(&self) {
        for key in SECRET_SETTINGS {
            let Some(plaintext) = self.db.get_setting(key) else {
                continue;
            };
            let secure_write_succeeded = match self.secrets.get(key) {
                Ok(Some(_)) => true,
                Ok(None) => match self.secrets.set(key, &plaintext) {
                    Ok(()) => true,
                    Err(error) => {
                        eprintln!("Could not migrate {} to secure storage: {}", key, error);
                        false
                    }
                },
                Err(error) => {
                    eprintln!("Could not inspect secure storage for {}: {}", key, error);
                    false
                }
            };
            if secure_write_succeeded {
                if let Err(error) = self.db.remove_setting(key) {
                    eprintln!("Could not remove migrated plaintext {}: {}", key, error);
                }
            }
        }
    }

    /// A toggle that is on until the user writes the literal "false".
    fn flag_on_by_default(&self, key: &str) -> bool {
        self.db
            .get_setting(key)
            .map(|v| v != "false")
            .unwrap_or(true)
    }

    /// A toggle that is off until the user writes the literal "true".
    fn flag_off_by_default(&self, key: &str) -> bool {
        self.db
            .get_setting(key)
            .map(|v| v == "true")
            .unwrap_or(false)
    }

    /// A text setting where blank is the same as unset.
    fn non_empty(&self, key: &str) -> Option<String> {
        self.db
            .get_setting(key)
            .filter(|value| !value.trim().is_empty())
    }

    // ── Providers and models ──────────────────────────────
    pub fn provider_name(&self) -> String {
        self.db
            .get_setting("provider")
            .unwrap_or_else(|| DEFAULT_PROVIDER.to_string())
    }

    pub fn provider(&self) -> Provider {
        Provider::from_str(&self.provider_name())
    }

    /// The formatting endpoint as stored. `None` means "the same one we
    /// transcribe with", which is what the caller substitutes.
    pub fn formatting_provider_name(&self) -> Option<String> {
        self.db.get_setting("formatting_provider")
    }

    pub fn formatting_provider(&self) -> Provider {
        Provider::from_str(
            &self
                .formatting_provider_name()
                .unwrap_or_else(|| self.provider_name()),
        )
    }

    pub fn tts_provider_name(&self) -> String {
        self.db
            .get_setting("tts_provider")
            .unwrap_or_else(|| DEFAULT_TTS_PROVIDER.to_string())
    }

    pub fn tts_provider(&self) -> Provider {
        Provider::from_str(&self.tts_provider_name())
    }

    /// Whether one provider serves both transcription and cleanup.
    pub fn same_provider(&self) -> bool {
        self.flag_on_by_default("same_provider")
    }

    pub fn format_enabled(&self) -> bool {
        self.flag_on_by_default("format_enabled")
    }

    /// Whether a recording should be previewed as it is spoken.
    ///
    /// Unset means yes for a self-hosted endpoint, where a preview costs a LAN
    /// round trip, and no for a hosted one, where it bills for one every
    /// 800 ms. Set either way, the user's choice stands.
    ///
    /// Turning it on for a hosted provider has two consequences a Settings
    /// checkbox has to say out loud, because neither is visible from the pill:
    ///
    /// - **Rate limits.** A 20 s dictation makes up to 25 readings, which is
    ///   75 requests a minute against Groq's 20 RPM for audio. The final take is
    ///   the request that queues behind them, so the 429 lands on the
    ///   transcription the user is actually waiting for, not on a preview.
    /// - **Billing.** Each reading re-uploads the whole recording so far, and
    ///   hosted transcription bills per minute of audio. Previewing a 20 s take
    ///   bills roughly the sum of 0.8 s, 1.6 s ... 20 s -- about 4 minutes of
    ///   audio for 20 seconds of speech, on top of the take itself.
    pub fn live_preview(&self) -> bool {
        live_preview_allowed(
            self.db.get_setting("live_preview").as_deref(),
            &self.provider(),
        )
    }

    pub fn stt_model(&self) -> Option<String> {
        self.db.get_setting("stt_model")
    }

    pub fn chat_model(&self) -> Option<String> {
        self.db.get_setting("chat_model")
    }

    pub fn language(&self) -> Option<String> {
        self.db.get_setting("language")
    }

    /// Names and terms sent to the transcriber as a spelling hint.
    pub fn dictionary(&self) -> Option<String> {
        self.db.get_setting("dictionary")
    }

    // ── Secrets ───────────────────────────────────────────
    pub fn api_key(&self) -> Result<Option<String>, String> {
        self.secrets.get("api_key")
    }

    pub fn formatting_api_key(&self) -> Result<Option<String>, String> {
        self.secrets.get("formatting_api_key")
    }

    pub fn tts_api_key(&self) -> Result<Option<String>, String> {
        self.secrets.get("tts_api_key")
    }

    // ── Voice ─────────────────────────────────────────────
    pub fn tts_enabled(&self) -> bool {
        self.flag_on_by_default("tts_enabled")
    }

    /// Blank means "use the provider's own default", so blank reads as unset.
    pub fn tts_model(&self) -> Option<String> {
        self.non_empty("tts_model")
    }

    pub fn tts_voice(&self) -> Option<String> {
        self.non_empty("tts_voice")
    }

    pub fn tts_response_format(&self) -> String {
        self.db
            .get_setting("tts_response_format")
            .unwrap_or_else(|| DEFAULT_TTS_RESPONSE_FORMAT.to_string())
            .to_ascii_lowercase()
    }

    // ── Capture and insertion ─────────────────────────────
    pub fn microphone(&self) -> Option<String> {
        self.db.get_setting("microphone")
    }

    pub fn insert_method(&self) -> InsertMethod {
        InsertMethod::from_setting(self.db.get_setting("insert_method"))
    }

    pub fn clipboard_policy(&self) -> ClipboardPolicy {
        ClipboardPolicy::from_setting(self.db.get_setting("preserve_clipboard"))
    }

    pub fn preserve_clipboard(&self) -> bool {
        self.clipboard_policy() == ClipboardPolicy::Restore
    }

    // ── History ───────────────────────────────────────────
    pub fn save_history(&self) -> bool {
        self.flag_on_by_default("save_history")
    }

    /// `None` means keep everything.
    pub fn history_retention_days(&self) -> Option<i64> {
        self.db
            .get_setting("history_retention_days")
            .and_then(|value| value.parse::<i64>().ok())
    }

    // ── Hotkeys ───────────────────────────────────────────
    /// The binding for `action` as a string, falling back to its default.
    pub fn hotkey(&self, action: &str) -> Option<String> {
        let default = hotkey::default_shortcut(action)?;
        Some(
            self.db
                .get_setting(&hotkey::setting_key(action))
                .unwrap_or_else(|| default.to_string()),
        )
    }

    /// The binding for `action`, parsed. A stored string that no longer parses
    /// degrades to the default rather than leaving the action unbound.
    pub fn shortcut(&self, action: &str) -> Result<HotKey, String> {
        let default =
            hotkey::default_shortcut(action).ok_or("Unknown hotkey action".to_string())?;
        let shortcut_str = self
            .db
            .get_setting(&hotkey::setting_key(action))
            .unwrap_or_else(|| default.to_string());
        hotkey::parse_shortcut(&shortcut_str).or_else(|_| hotkey::parse_shortcut(default))
    }

    pub fn set_hotkey(&self, action: &str, shortcut_str: &str) -> Result<(), String> {
        self.db
            .set_setting(&hotkey::setting_key(action), shortcut_str)
    }

    // ── Appearance ────────────────────────────────────────
    /// `None` means follow the system, which is what the UI does when the key
    /// holds neither "dark" nor "light".
    pub fn theme(&self) -> Option<String> {
        self.db
            .get_setting("theme")
            .filter(|value| value == "dark" || value == "light")
    }

    pub fn overlay_only_while_recording(&self) -> bool {
        self.flag_off_by_default("overlay_only_while_recording")
    }

    pub fn overlay_position(&self) -> String {
        self.db
            .get_setting("overlay_position")
            .unwrap_or_else(|| DEFAULT_OVERLAY_POSITION.to_string())
    }

    // ── Onboarding ────────────────────────────────────────
    /// Setup counts as done once a provider is saved. A self-hosted endpoint
    /// legitimately has no key, so the key cannot be the signal.
    pub fn onboarding_complete(&self) -> bool {
        self.db.get_setting("provider").is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_settings() -> Settings {
        let dir = std::env::temp_dir().join(format!("openflow-settings-{}", uuid::Uuid::new_v4()));
        let db = Database::new(dir.clone()).expect("a scratch database");
        Settings::new(db, SecretStore::new(dir))
    }

    #[test]
    fn live_preview_defaults_to_the_endpoint_that_is_free_to_ask() {
        let lan = Provider::from_str("custom:http://192.168.1.2:8882/v1");
        let hosted = Provider::from_str("groq");
        assert!(lan.is_custom());

        // Unset: on for the LAN box, off for anything that bills per request.
        assert!(live_preview_allowed(None, &lan));
        assert!(!live_preview_allowed(None, &hosted));

        // Set: the user's choice wins for either kind of endpoint.
        assert!(live_preview_allowed(Some("true"), &hosted));
        assert!(!live_preview_allowed(Some("false"), &lan));

        // Anything else is not a yes.
        assert!(!live_preview_allowed(Some(""), &hosted));
        assert!(!live_preview_allowed(Some("yes"), &hosted));
    }

    #[test]
    fn secret_keys_are_exactly_the_three_credentials() {
        assert!(is_secret_setting("api_key"));
        assert!(is_secret_setting("formatting_api_key"));
        assert!(is_secret_setting("tts_api_key"));
        assert!(!is_secret_setting("provider"));
        assert!(!is_secret_setting("dictionary"));
        // A near miss must not be routed to the keychain by accident.
        assert!(!is_secret_setting("api_key "));
    }

    /// An unset key has to behave like the settings UI's own default, or a
    /// fresh install and a visited-Settings install would transcribe
    /// differently.
    #[test]
    fn unset_keys_fall_back_to_the_shipped_defaults() {
        let settings = scratch_settings();

        assert_eq!(settings.provider_name(), "groq");
        assert_eq!(settings.formatting_provider_name(), None);
        assert_eq!(settings.tts_provider_name(), "openrouter");
        assert!(settings.same_provider());
        assert!(settings.format_enabled());
        assert!(settings.tts_enabled());
        assert!(settings.save_history());
        assert!(settings.preserve_clipboard());
        assert_eq!(settings.clipboard_policy(), ClipboardPolicy::Restore);
        assert_eq!(settings.insert_method(), InsertMethod::Paste);
        assert!(!settings.overlay_only_while_recording());
        assert_eq!(settings.overlay_position(), "left-center");
        assert_eq!(settings.tts_response_format(), "mp3");
        assert_eq!(settings.hotkey("record").as_deref(), Some("Option+V"));
        assert_eq!(settings.hotkey("recopy").as_deref(), Some("Ctrl+Shift+V"));
        assert_eq!(settings.hotkey("nonsense"), None);
        assert_eq!(settings.theme(), None);
        assert_eq!(settings.history_retention_days(), None);
        assert_eq!(settings.stt_model(), None);
        assert_eq!(settings.chat_model(), None);
        assert_eq!(settings.language(), None);
        assert_eq!(settings.dictionary(), None);
        assert_eq!(settings.microphone(), None);
        assert!(!settings.onboarding_complete());
    }

    #[test]
    fn stored_values_win_over_the_defaults() {
        let settings = scratch_settings();
        settings.set("provider", "openai").expect("write provider");
        settings.set("same_provider", "false").expect("write flag");
        settings.set("format_enabled", "false").expect("write flag");
        settings.set("save_history", "false").expect("write flag");
        settings
            .set("preserve_clipboard", "false")
            .expect("write flag");
        settings.set("insert_method", "type").expect("write method");
        settings
            .set("overlay_only_while_recording", "true")
            .expect("write flag");
        settings
            .set("history_retention_days", "30")
            .expect("write retention");
        settings
            .set("tts_response_format", "WAV")
            .expect("write format");
        settings
            .set("hotkey_record", "Cmd+Shift+K")
            .expect("write hotkey");
        settings.set("theme", "light").expect("write theme");

        assert_eq!(settings.provider_name(), "openai");
        assert!(matches!(settings.provider(), Provider::OpenAI));
        assert!(!settings.same_provider());
        assert!(!settings.format_enabled());
        assert!(!settings.save_history());
        assert!(!settings.preserve_clipboard());
        assert_eq!(settings.clipboard_policy(), ClipboardPolicy::Keep);
        assert_eq!(settings.insert_method(), InsertMethod::Type);
        assert!(settings.overlay_only_while_recording());
        assert_eq!(settings.history_retention_days(), Some(30));
        assert_eq!(settings.tts_response_format(), "wav");
        assert_eq!(settings.hotkey("record").as_deref(), Some("Cmd+Shift+K"));
        assert_eq!(settings.theme().as_deref(), Some("light"));
        assert!(settings.onboarding_complete());
        // Unset still means "the transcription provider" for formatting.
        assert!(matches!(settings.formatting_provider(), Provider::OpenAI));
    }

    /// A binding the user saved that no longer parses must not leave the action
    /// dead: it falls back to the shipped default.
    #[test]
    fn an_unparseable_binding_falls_back_to_the_default() {
        let settings = scratch_settings();
        settings
            .set("hotkey_record", "Option+NotAKey")
            .expect("write hotkey");
        assert_eq!(
            settings.shortcut("record").expect("a parsed shortcut"),
            hotkey::parse_shortcut("Option+V").expect("the default parses")
        );
        assert!(settings.shortcut("nonsense").is_err());
    }

    /// A blank voice model or voice means "the provider's own default", not an
    /// empty model name on the wire.
    #[test]
    fn blank_voice_fields_read_as_unset() {
        let settings = scratch_settings();
        settings.set("tts_model", "   ").expect("write model");
        settings.set("tts_voice", "").expect("write voice");
        assert_eq!(settings.tts_model(), None);
        assert_eq!(settings.tts_voice(), None);
    }
}
