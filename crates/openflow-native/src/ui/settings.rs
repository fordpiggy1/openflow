//! The Settings window: one `NSWindow` with four tabs, autosaving.
//!
//! Every key in the parity checklist of `docs/native-port/PLAN.md` section 4
//! has a control here, with the same default the web settings screen shows for
//! an unset key. There is no Save button, matching the current app: a control
//! writes through `Settings` the moment it changes, and the three credentials
//! go to the keychain because `Settings::set` routes them there.
//!
//! Closing hides the window rather than releasing it, so reopening from the
//! menu bar is instant and no state is rebuilt.

use std::cell::RefCell;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{
    define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
    Message,
};
use objc2_app_kit::{
    NSBackingStoreType, NSComboBox, NSControl, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSEvent, NSEventMask, NSEventModifierFlags, NSPopUpButton,
    NSSecureTextField, NSSwitch, NSTabView, NSTabViewItem, NSTextDelegate, NSTextField, NSTextView,
    NSTextViewDelegate, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use openflow_core::engine::Engine;
use openflow_core::speech::SpeechRequest;
use openflow_core::transcribe::ModelInfo;

use crate::hotkeys;
use crate::overlay;
use crate::ui::{
    button, combo, label, note, popup, secure_field, switch_control, text_field, text_view, wire,
    Form, ROW,
};

const WINDOW_WIDTH: f64 = 420.0;
const WINDOW_HEIGHT: f64 = 560.0;
const TAB_WIDTH: f64 = 396.0;
const TAB_HEIGHT: f64 = 496.0;
/// The dictionary is sent to the transcriber as a spelling hint and the web
/// settings screen caps it here.
pub const DICTIONARY_LIMIT: usize = 800;
/// The voice preview textarea's cap, matching the web screen.
pub const PREVIEW_LIMIT: usize = 500;

// ── Option tables ─────────────────────────────────────────
// The stored value first, the menu title second. Index in the table is the
// index in the popup, which is the only mapping between the two.

const PROVIDERS: &[(&str, &str)] = &[
    ("groq", "Groq"),
    ("openrouter", "OpenRouter"),
    ("openai", "OpenAI"),
    ("deepgram", "Deepgram"),
    ("custom", "Custom endpoint"),
];
/// Deepgram transcribes only, so it is not offered for cleanup.
const FORMATTING_PROVIDERS: &[(&str, &str)] = &[
    ("groq", "Groq"),
    ("openrouter", "OpenRouter"),
    ("openai", "OpenAI"),
    ("custom", "Custom endpoint"),
];
const TTS_PROVIDERS: &[(&str, &str)] = &[
    ("groq", "Groq (Orpheus)"),
    ("openrouter", "OpenRouter (Gemini)"),
    ("openai", "OpenAI"),
    ("custom", "Self-hosted / LAN"),
];
const LANGUAGES: &[(&str, &str)] = &[
    ("", "Auto-detect"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("ru", "Russian"),
];
const THEMES: &[(&str, &str)] = &[("", "System"), ("light", "Light"), ("dark", "Dark")];
const INSERT_METHODS: &[(&str, &str)] = &[("paste", "Paste"), ("type", "Type")];
const TTS_FORMATS: &[(&str, &str)] = &[("mp3", "mp3"), ("wav", "wav")];
const RETENTIONS: &[(&str, &str)] = &[
    ("", "Never"),
    ("1", "1 day"),
    ("7", "7 days"),
    ("30", "30 days"),
    ("90", "90 days"),
];

/// The eight overlay anchors, in the order `overlay.html` lists them.
fn position_options() -> Vec<(String, String)> {
    overlay::POSITIONS
        .iter()
        .map(|(name, _, _)| {
            let pretty = name
                .split('-')
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            ((*name).to_string(), pretty)
        })
        .collect()
}

// ── Control tags ──────────────────────────────────────────

const TAG_MICROPHONE: isize = 1;
const TAG_INSERT_METHOD: isize = 2;
const TAG_PRESERVE_CLIPBOARD: isize = 3;
const TAG_OVERLAY_ONLY: isize = 4;
const TAG_OVERLAY_POSITION: isize = 5;
const TAG_THEME: isize = 6;
const TAG_LANGUAGE: isize = 7;
const TAG_HOTKEY_RECORD: isize = 8;
const TAG_HOTKEY_RECOPY: isize = 9;

const TAG_PROVIDER: isize = 20;
const TAG_PROVIDER_URL: isize = 21;
const TAG_API_KEY: isize = 22;
const TAG_SAME_PROVIDER: isize = 23;
const TAG_FORMATTING_PROVIDER: isize = 24;
const TAG_FORMATTING_URL: isize = 25;
const TAG_FORMATTING_KEY: isize = 26;
const TAG_FORMAT_ENABLED: isize = 27;
const TAG_STT_MODEL: isize = 28;
const TAG_CHAT_MODEL: isize = 29;
const TAG_FETCH_MODELS: isize = 30;

const TAG_TTS_ENABLED: isize = 40;
const TAG_TTS_PROVIDER: isize = 41;
const TAG_TTS_URL: isize = 42;
const TAG_TTS_KEY: isize = 43;
const TAG_TTS_MODEL: isize = 44;
const TAG_TTS_VOICE: isize = 45;
const TAG_TTS_FORMAT: isize = 46;

const TAG_SAVE_HISTORY: isize = 60;
const TAG_RETENTION: isize = 61;

/// Everything the window has to read back or write into.
struct Controls {
    microphone: Retained<NSPopUpButton>,
    microphone_ids: RefCell<Vec<String>>,
    hotkey_record: Retained<objc2_app_kit::NSButton>,
    hotkey_recopy: Retained<objc2_app_kit::NSButton>,
    insert_method: Retained<NSPopUpButton>,
    preserve_clipboard: Retained<NSSwitch>,
    overlay_only: Retained<NSSwitch>,
    overlay_position: Retained<NSPopUpButton>,
    theme: Retained<NSPopUpButton>,
    language: Retained<NSPopUpButton>,

    provider: Retained<NSPopUpButton>,
    provider_url: Retained<NSTextField>,
    api_key: Retained<NSSecureTextField>,
    same_provider: Retained<NSSwitch>,
    formatting_provider: Retained<NSPopUpButton>,
    formatting_url: Retained<NSTextField>,
    formatting_key: Retained<NSSecureTextField>,
    format_enabled: Retained<NSSwitch>,
    stt_model: Retained<NSComboBox>,
    chat_model: Retained<NSComboBox>,
    models_status: Retained<NSTextField>,

    tts_enabled: Retained<NSSwitch>,
    tts_provider: Retained<NSPopUpButton>,
    tts_url: Retained<NSTextField>,
    tts_key: Retained<NSSecureTextField>,
    tts_model: Retained<NSComboBox>,
    tts_voice: Retained<NSComboBox>,
    tts_format: Retained<NSPopUpButton>,
    preview_text: Retained<NSTextField>,
    voice_status: Retained<NSTextField>,

    dictionary: Retained<NSTextView>,
    dictionary_count: Retained<NSTextField>,
    save_history: Retained<NSSwitch>,
    retention: Retained<NSPopUpButton>,
    history_status: Retained<NSTextField>,

    /// The buttons that trigger an action rather than write a setting, in the
    /// order [`ACTION_SELECTORS`] names them.
    actions: Vec<Retained<objc2_app_kit::NSButton>>,
}

pub struct SettingsIvars {
    engine: Arc<Engine>,
    window: Retained<NSWindow>,
    tabs: Retained<NSTabView>,
    controls: Controls,
    /// The event monitor a hotkey field installs while it is listening.
    monitor: RefCell<Option<Retained<AnyObject>>>,
    recording_action: RefCell<Option<String>>,
    /// The speech request currently previewing, for the Stop button.
    preview_request: RefCell<Option<String>>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements; this class holds
    // only ivars and implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OpenFlowSettingsWindow"]
    #[ivars = SettingsIvars]
    pub struct SettingsWindow;

    unsafe impl NSObjectProtocol for SettingsWindow {}

    unsafe impl NSWindowDelegate for SettingsWindow {
        /// Hide, never close: the window is built once and kept.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.stop_recording_hotkey();
            // Resigning first responder ends editing, which is what commits a
            // key or a URL the user typed and never tabbed out of.
            self.ivars().window.makeFirstResponder(None);
            self.ivars().window.orderOut(None);
            false
        }
    }

    unsafe impl NSControlTextEditingDelegate for SettingsWindow {
        /// Live autosave, for the fields where a write is a row in SQLite.
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, notification: &NSNotification) {
            let Some(tag) = notified_tag(notification) else {
                return;
            };
            if !writes_on_end_editing(tag) {
                self.write(tag);
            }
        }

        /// Deferred autosave, for the fields where a write is a keychain item
        /// or an endpoint URL. Writing those per keystroke meant one keychain
        /// round trip per character typed into a key, and a delete the moment
        /// the field was momentarily empty; a half-typed URL is not an endpoint
        /// worth saving either.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, notification: &NSNotification) {
            let Some(tag) = notified_tag(notification) else {
                return;
            };
            if writes_on_end_editing(tag) {
                self.write(tag);
            }
        }
    }

    unsafe impl NSTextViewDelegate for SettingsWindow {}

    unsafe impl NSTextDelegate for SettingsWindow {
        /// The dictionary text view. Enforces the 800 character cap as it is
        /// typed rather than truncating silently on save.
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _notification: &NSNotification) {
            self.write_dictionary();
        }
    }

    impl SettingsWindow {
        #[unsafe(method(controlChanged:))]
        fn control_changed(&self, sender: &NSControl) {
            self.write(sender.tag());
        }

        #[unsafe(method(recordHotkey:))]
        fn record_hotkey(&self, sender: &NSControl) {
            let action = if { sender.tag() } == TAG_HOTKEY_RECORD {
                "record"
            } else {
                "recopy"
            };
            self.start_recording_hotkey(action);
        }

        #[unsafe(method(fetchModels:))]
        fn fetch_models(&self, _sender: &NSControl) {
            self.request_models();
        }

        #[unsafe(method(previewVoice:))]
        fn preview_voice(&self, _sender: &NSControl) {
            self.start_preview();
        }

        #[unsafe(method(stopVoice:))]
        fn stop_voice(&self, _sender: &NSControl) {
            self.stop_preview();
        }

        #[unsafe(method(clearHistory:))]
        fn clear_history(&self, _sender: &NSControl) {
            let message = match self.ivars().engine.clear_history() {
                Ok(removed) => format!("Deleted {} stored transcriptions.", removed),
                Err(error) => error,
            };
            self.set_text(&self.ivars().history_status_field(), &message);
        }
    }
);

impl SettingsIvars {
    fn history_status_field(&self) -> Retained<NSTextField> {
        self.controls.history_status.clone()
    }
}

impl SettingsWindow {
    pub fn new(app: &std::rc::Rc<crate::app::App>, mtm: MainThreadMarker) -> Retained<Self> {
        let engine = Arc::clone(app.engine());

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(WINDOW_WIDTH, WINDOW_HEIGHT),
                ),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str("OpenFlow Settings"));
        unsafe { window.setReleasedWhenClosed(false) };
        window.center();

        let tabs = {
            NSTabView::initWithFrame(
                NSTabView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(10.0, 10.0),
                    NSSize::new(WINDOW_WIDTH - 20.0, WINDOW_HEIGHT - 20.0),
                ),
            )
        };

        let (general, providers, voice, privacy, controls) = build_tabs(mtm);
        for (title, view) in [
            ("General", &general),
            ("Providers", &providers),
            ("Voice", &voice),
            ("Privacy", &privacy),
        ] {
            let item = unsafe {
                NSTabViewItem::initWithIdentifier(
                    NSTabViewItem::alloc(),
                    Some(&NSString::from_str(title)),
                )
            };
            {
                item.setLabel(&NSString::from_str(title));
                item.setView(Some(view));
            }
            tabs.addTabViewItem(&item);
        }
        if let Some(content) = window.contentView() {
            content.addSubview(&tabs);
        }

        let this = Self::alloc(mtm).set_ivars(SettingsIvars {
            engine,
            window,
            tabs,
            controls,
            monitor: RefCell::new(None),
            recording_action: RefCell::new(None),
            preview_request: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        this.ivars()
            .window
            .setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this.wire_actions();
        this.reload();
        this
    }

    /// Point every control at this object. Done in one place so a control that
    /// is added later and forgotten here simply does nothing, rather than
    /// firing an action on a stale target.
    fn wire_actions(&self) {
        let controls = &self.ivars().controls;
        let target: &AnyObject = self.as_ref();
        let changed: Sel = sel!(controlChanged:);

        for control in [
            controls.microphone.as_ref() as &NSControl,
            &controls.insert_method,
            &controls.overlay_position,
            &controls.theme,
            &controls.language,
            &controls.provider,
            &controls.formatting_provider,
            &controls.tts_provider,
            &controls.tts_format,
            &controls.retention,
        ] {
            wire(control, target, changed);
        }
        for control in [
            controls.preserve_clipboard.as_ref() as &NSControl,
            &controls.overlay_only,
            &controls.same_provider,
            &controls.format_enabled,
            &controls.tts_enabled,
            &controls.save_history,
        ] {
            wire(control, target, changed);
        }
        for control in [
            controls.provider_url.as_ref() as &NSControl,
            &controls.formatting_url,
            &controls.tts_url,
            &controls.preview_text,
        ] {
            wire(control, target, changed);
            unsafe {
                msg_send![
                    control,
                    setDelegate: Some(ProtocolObject::<dyn NSControlTextEditingDelegate>::from_ref(self))
                ]
            }
        }
        for control in [
            controls.api_key.as_ref() as &NSControl,
            &controls.formatting_key,
            &controls.tts_key,
        ] {
            wire(control, target, changed);
            unsafe {
                msg_send![
                    control,
                    setDelegate: Some(ProtocolObject::<dyn NSControlTextEditingDelegate>::from_ref(self))
                ]
            }
        }
        for control in [
            controls.stt_model.as_ref() as &NSControl,
            &controls.chat_model,
            &controls.tts_model,
            &controls.tts_voice,
        ] {
            wire(control, target, changed);
            unsafe {
                msg_send![
                    control,
                    setDelegate: Some(ProtocolObject::<dyn NSControlTextEditingDelegate>::from_ref(self))
                ]
            }
        }

        wire(&controls.hotkey_record, target, sel!(recordHotkey:));
        wire(&controls.hotkey_recopy, target, sel!(recordHotkey:));
        for (action, selector) in controls
            .actions
            .iter()
            .zip(ACTION_SELECTORS.map(|make| make()))
        {
            wire(action, target, selector);
        }
        controls
            .dictionary
            .setDelegate(Some(ProtocolObject::from_ref(self)));
    }

    pub fn present(&self) {
        let ivars = self.ivars();
        ivars.window.makeKeyAndOrderFront(None);
        if let Some(mtm) = MainThreadMarker::new() {
            objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
        }
    }

    pub fn select_tab(&self, tab: &str) {
        let index = match tab {
            "General" | "general" => 0,
            "Providers" | "providers" | "onboarding" => 1,
            "Voice" | "voice" => 2,
            "Privacy" | "privacy" | "history" => 3,
            _ => return,
        };
        self.ivars().tabs.selectTabViewItemAtIndex(index);
    }

    /// Force whatever field is being edited to commit, if this window is the
    /// one the user is typing in. Editing ends when first responder is given
    /// up, and that is what writes the deferred fields.
    pub fn commit_pending_edits(&self) {
        let window = &self.ivars().window;
        if window.isVisible() && window.isKeyWindow() {
            window.makeFirstResponder(None);
        }
    }

    pub fn set_voice_status(&self, message: &str) {
        self.set_text(&self.ivars().controls.voice_status, message);
    }

    fn set_text(&self, field: &NSTextField, text: &str) {
        field.setStringValue(&NSString::from_str(text));
    }

    // ── Reading settings into the controls ────────────────

    /// Fill every control from the database. Called when the window is built
    /// and every time it is shown, so a value another surface changed (the
    /// overlay's own drag, say) is never stale on screen.
    pub fn reload(&self) {
        let ivars = self.ivars();
        let settings = ivars.engine.settings();
        let controls = &ivars.controls;

        // General
        self.reload_microphones();
        // Show the chord that is actually registered, normalized: a binding
        // saved as "ctrl+shift+v" reads back as "Ctrl+Shift+V".
        let record = binding_text(settings, "record");
        let recopy = binding_text(settings, "recopy");
        {
            controls
                .hotkey_record
                .setTitle(&NSString::from_str(&record));
            controls
                .hotkey_recopy
                .setTitle(&NSString::from_str(&recopy));
        }
        select_value(
            &controls.insert_method,
            INSERT_METHODS,
            match settings.insert_method() {
                openflow_core::insert::InsertMethod::Type => "type",
                _ => "paste",
            },
        );
        set_switch(&controls.preserve_clipboard, settings.preserve_clipboard());
        set_switch(
            &controls.overlay_only,
            settings.overlay_only_while_recording(),
        );
        select_pairs(
            &controls.overlay_position,
            &position_options(),
            &settings.overlay_position(),
        );
        select_value(
            &controls.theme,
            THEMES,
            settings.theme().as_deref().unwrap_or(""),
        );
        select_value(
            &controls.language,
            LANGUAGES,
            settings.language().as_deref().unwrap_or(""),
        );

        // Providers
        let (provider, provider_url) = split_provider(&settings.provider_name());
        select_value(&controls.provider, PROVIDERS, &provider);
        self.set_text(&controls.provider_url, &provider_url);
        self.set_text(
            &controls.api_key,
            &settings.api_key().ok().flatten().unwrap_or_default(),
        );
        set_switch(&controls.same_provider, settings.same_provider());
        let (formatting, formatting_url) = split_provider(
            &settings
                .formatting_provider_name()
                .unwrap_or_else(|| settings.provider_name()),
        );
        select_value(
            &controls.formatting_provider,
            FORMATTING_PROVIDERS,
            &formatting,
        );
        self.set_text(&controls.formatting_url, &formatting_url);
        self.set_text(
            &controls.formatting_key,
            &settings
                .formatting_api_key()
                .ok()
                .flatten()
                .unwrap_or_default(),
        );
        set_switch(&controls.format_enabled, settings.format_enabled());
        self.set_combo(
            &controls.stt_model,
            &settings.stt_model().unwrap_or_default(),
        );
        self.set_combo(
            &controls.chat_model,
            &settings.chat_model().unwrap_or_default(),
        );

        // Voice
        set_switch(&controls.tts_enabled, settings.tts_enabled());
        let (tts, tts_url) = split_provider(&settings.tts_provider_name());
        select_value(&controls.tts_provider, TTS_PROVIDERS, &tts);
        self.set_text(&controls.tts_url, &tts_url);
        self.set_text(
            &controls.tts_key,
            &settings.tts_api_key().ok().flatten().unwrap_or_default(),
        );
        self.set_combo(
            &controls.tts_model,
            &settings.tts_model().unwrap_or_default(),
        );
        self.set_combo(
            &controls.tts_voice,
            &settings.tts_voice().unwrap_or_default(),
        );
        select_value(
            &controls.tts_format,
            TTS_FORMATS,
            &settings.tts_response_format(),
        );

        // Privacy
        let dictionary = settings.dictionary().unwrap_or_default();
        controls
            .dictionary
            .setString(&NSString::from_str(&dictionary));
        self.update_dictionary_count(dictionary.chars().count());
        set_switch(&controls.save_history, settings.save_history());
        select_value(
            &controls.retention,
            RETENTIONS,
            &settings
                .history_retention_days()
                .map(|days| days.to_string())
                .unwrap_or_default(),
        );
    }

    fn set_combo(&self, combo: &NSComboBox, value: &str) {
        combo.setStringValue(&NSString::from_str(value));
    }

    fn reload_microphones(&self) {
        let ivars = self.ivars();
        let controls = &ivars.controls;
        let devices = ivars.engine.list_audio_devices().unwrap_or_default();
        let mut ids = vec![String::new()];
        {
            controls.microphone.removeAllItems();
            controls
                .microphone
                .addItemWithTitle(&NSString::from_str("System default"));
            for device in &devices {
                let title = if device.is_default {
                    format!("{} (default)", device.name)
                } else {
                    device.name.clone()
                };
                controls
                    .microphone
                    .addItemWithTitle(&NSString::from_str(&title));
                ids.push(device.id.clone());
            }
        }
        let saved = ivars.engine.settings().microphone().unwrap_or_default();
        let index = ids.iter().position(|id| *id == saved).unwrap_or(0);
        controls.microphone.selectItemAtIndex(index as isize);
        *controls.microphone_ids.borrow_mut() = ids;
    }

    // ── Writing one control back ──────────────────────────

    fn write(&self, tag: isize) {
        let ivars = self.ivars();
        let settings = ivars.engine.settings();
        let controls = &ivars.controls;
        let result = match tag {
            TAG_MICROPHONE => {
                let index = { controls.microphone.indexOfSelectedItem() }.max(0) as usize;
                let ids = controls.microphone_ids.borrow();
                settings.set(
                    "microphone",
                    ids.get(index).map(String::as_str).unwrap_or(""),
                )
            }
            TAG_INSERT_METHOD => settings.set(
                "insert_method",
                selected_value(&controls.insert_method, INSERT_METHODS),
            ),
            TAG_PRESERVE_CLIPBOARD => settings.set(
                "preserve_clipboard",
                bool_setting(is_on(&controls.preserve_clipboard)),
            ),
            TAG_OVERLAY_ONLY => {
                let value = bool_setting(is_on(&controls.overlay_only));
                let written = settings.set("overlay_only_while_recording", value);
                crate::app::with_app(|app| app.overlay().apply_visibility_setting());
                written
            }
            TAG_OVERLAY_POSITION => {
                let options = position_options();
                let index = { controls.overlay_position.indexOfSelectedItem() }.max(0) as usize;
                let value = options
                    .get(index)
                    .map(|(value, _)| value.clone())
                    .unwrap_or_else(|| "left-center".to_string());
                let written = settings.set("overlay_position", &value);
                crate::app::with_app(|app| app.overlay().set_position(&value));
                written
            }
            TAG_THEME => {
                let value = selected_value(&controls.theme, THEMES);
                let written = settings.set("theme", value);
                if let Some(mtm) = MainThreadMarker::new() {
                    crate::app::apply_theme(settings.theme().as_deref(), mtm);
                }
                written
            }
            TAG_LANGUAGE => settings.set("language", selected_value(&controls.language, LANGUAGES)),
            TAG_PROVIDER | TAG_PROVIDER_URL => settings.set(
                "provider",
                &join_provider(
                    selected_value(&controls.provider, PROVIDERS),
                    &string_value(&controls.provider_url),
                ),
            ),
            TAG_API_KEY => settings.set("api_key", string_value(&controls.api_key).trim()),
            TAG_SAME_PROVIDER => settings.set(
                "same_provider",
                bool_setting(is_on(&controls.same_provider)),
            ),
            TAG_FORMATTING_PROVIDER | TAG_FORMATTING_URL => settings.set(
                "formatting_provider",
                &join_provider(
                    selected_value(&controls.formatting_provider, FORMATTING_PROVIDERS),
                    &string_value(&controls.formatting_url),
                ),
            ),
            TAG_FORMATTING_KEY => settings.set(
                "formatting_api_key",
                string_value(&controls.formatting_key).trim(),
            ),
            TAG_FORMAT_ENABLED => settings.set(
                "format_enabled",
                bool_setting(is_on(&controls.format_enabled)),
            ),
            TAG_STT_MODEL => settings.set("stt_model", string_value(&controls.stt_model).trim()),
            TAG_CHAT_MODEL => settings.set("chat_model", string_value(&controls.chat_model).trim()),
            TAG_TTS_ENABLED => {
                settings.set("tts_enabled", bool_setting(is_on(&controls.tts_enabled)))
            }
            TAG_TTS_PROVIDER | TAG_TTS_URL => settings.set(
                "tts_provider",
                &join_provider(
                    selected_value(&controls.tts_provider, TTS_PROVIDERS),
                    &string_value(&controls.tts_url),
                ),
            ),
            TAG_TTS_KEY => settings.set("tts_api_key", string_value(&controls.tts_key).trim()),
            TAG_TTS_MODEL => settings.set("tts_model", string_value(&controls.tts_model).trim()),
            TAG_TTS_VOICE => settings.set("tts_voice", string_value(&controls.tts_voice).trim()),
            TAG_TTS_FORMAT => settings.set(
                "tts_response_format",
                selected_value(&controls.tts_format, TTS_FORMATS),
            ),
            TAG_SAVE_HISTORY => {
                settings.set("save_history", bool_setting(is_on(&controls.save_history)))
            }
            TAG_RETENTION => settings.set(
                "history_retention_days",
                selected_value(&controls.retention, RETENTIONS),
            ),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.set_text(&controls.models_status, &error);
        }
    }

    fn write_dictionary(&self) {
        let ivars = self.ivars();
        let view = &ivars.controls.dictionary;
        let mut text = { view.string() }.to_string();
        if text.chars().count() > DICTIONARY_LIMIT {
            text = text.chars().take(DICTIONARY_LIMIT).collect();
            view.setString(&NSString::from_str(&text));
        }
        self.update_dictionary_count(text.chars().count());
        let _ = ivars.engine.settings().set("dictionary", text.trim());
    }

    fn update_dictionary_count(&self, used: usize) {
        self.set_text(
            &self.ivars().controls.dictionary_count,
            &format!("{}/{}", used, DICTIONARY_LIMIT),
        );
    }

    // ── Hotkey recorder ───────────────────────────────────

    /// Listen for the next chord and bind `action` to it. The monitor swallows
    /// the key event so the chord does not also reach whatever control has
    /// focus.
    fn start_recording_hotkey(&self, action: &str) {
        self.stop_recording_hotkey();
        let ivars = self.ivars();
        *ivars.recording_action.borrow_mut() = Some(action.to_string());
        // Take the current chord off the system first, or pressing it to
        // re-record it starts a capture instead of reaching the monitor.
        crate::app::with_app(|app| app.hotkeys().borrow_mut().suspend(action));
        let field = self.field_for(action);
        field.setTitle(&NSString::from_str("Press a shortcut..."));

        let this = self.retain();
        let block = block2::RcBlock::new(move |event: core::ptr::NonNull<NSEvent>| {
            // SAFETY: AppKit hands us a live event for the duration of the call.
            let event = unsafe { event.as_ref() };
            this.finish_recording_hotkey(event);
            core::ptr::null_mut()
        });
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
        };
        *ivars.monitor.borrow_mut() = monitor;
    }

    fn finish_recording_hotkey(&self, event: &NSEvent) {
        let ivars = self.ivars();
        let Some(action) = ivars.recording_action.borrow().clone() else {
            return;
        };
        let flags = event.modifierFlags();
        let characters = event.charactersIgnoringModifiers().map(|s| s.to_string());
        let Some(key) = hotkeys::key_name(event.keyCode(), characters.as_deref()) else {
            self.stop_recording_hotkey();
            return;
        };
        let chord = hotkeys::shortcut_string(
            flags.contains(NSEventModifierFlags::Control),
            flags.contains(NSEventModifierFlags::Option),
            flags.contains(NSEventModifierFlags::Shift),
            flags.contains(NSEventModifierFlags::Command),
            &key,
        );
        self.stop_recording_hotkey();
        let Some(chord) = chord else { return };

        let outcome = crate::app::with_app(|app| {
            app.hotkeys()
                .borrow_mut()
                .rebind(app.engine().settings(), &action, &chord)
        });
        match outcome {
            Some(Ok(())) => {
                self.field_for(&action)
                    .setTitle(&NSString::from_str(&chord));
            }
            Some(Err(error)) => self.set_text(&ivars.controls.models_status, &error),
            None => {}
        }
    }

    fn stop_recording_hotkey(&self) {
        let ivars = self.ivars();
        if let Some(monitor) = ivars.monitor.borrow_mut().take() {
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        // Put the suspended chord back before any rebind: `rebind` releases the
        // old registration itself, and it has to be there to release.
        crate::app::with_app(|app| app.hotkeys().borrow_mut().resume());
        if let Some(action) = ivars.recording_action.borrow_mut().take() {
            let current = binding_text(ivars.engine.settings(), &action);
            self.field_for(&action)
                .setTitle(&NSString::from_str(&current));
        }
    }

    fn field_for(&self, action: &str) -> Retained<objc2_app_kit::NSButton> {
        if action == "record" {
            self.ivars().controls.hotkey_record.clone()
        } else {
            self.ivars().controls.hotkey_recopy.clone()
        }
    }

    // ── Models and voice ──────────────────────────────────

    fn request_models(&self) {
        let ivars = self.ivars();
        self.set_text(&ivars.controls.models_status, "Fetching models...");
        let engine = Arc::clone(&ivars.engine);
        let provider = join_provider(
            selected_value(&ivars.controls.provider, PROVIDERS),
            &string_value(&ivars.controls.provider_url),
        );
        let key = string_value(&ivars.controls.api_key).trim().to_string();
        let key = (!key.is_empty()).then_some(key);
        crate::app::spawn(async move {
            let result = engine.fetch_models(Some(provider), key).await;
            crate::events::on_main(move || {
                crate::app::with_app(|app| {
                    app.with_settings(|window| window.models_loaded(&result))
                });
            });
        });
    }

    fn models_loaded(&self, result: &Result<Vec<ModelInfo>, String>) {
        let controls = &self.ivars().controls;
        match result {
            Ok(models) => {
                fill_combo(&controls.stt_model, models, "stt");
                fill_combo(&controls.chat_model, models, "chat");
                fill_combo(&controls.tts_model, models, "tts");
                self.set_text(
                    &controls.models_status,
                    &format!("{} models available.", models.len()),
                );
            }
            Err(error) => self.set_text(&controls.models_status, error),
        }
    }

    fn start_preview(&self) {
        let ivars = self.ivars();
        let text = string_value(&ivars.controls.preview_text);
        let text: String = text.trim().chars().take(PREVIEW_LIMIT).collect();
        if text.is_empty() {
            self.set_voice_status("Type something to preview first.");
            return;
        }
        self.stop_preview();

        // A fresh id per preview. Reusing one raced `speech::stream`'s job
        // table: a cancelled request is only removed once its task unwinds, so
        // Preview pressed twice in a row was refused as "already running".
        let request_id = format!("preview-{}", uuid::Uuid::new_v4());
        *ivars.preview_request.borrow_mut() = Some(request_id.clone());
        self.set_voice_status("Generating...");

        // Arm the gate before the stream exists. `speech::stream` emits the
        // first chunk from a tokio worker, and this host only sees `TtsStarted`
        // after a main-queue hop; opening the gate on that event would refuse
        // any chunk that overtook it.
        crate::app::with_app(|app| app.tts().arm(&request_id));

        let engine = Arc::clone(&ivars.engine);
        let request = SpeechRequest {
            text,
            model: Some(string_value(&ivars.controls.tts_model).trim().to_string()),
            voice: Some(string_value(&ivars.controls.tts_voice).trim().to_string()),
            response_format: Some(
                selected_value(&ivars.controls.tts_format, TTS_FORMATS).to_string(),
            ),
            request_id: Some(request_id.clone()),
        };
        crate::app::spawn(async move {
            // Failures before `TtsStarted` -- a bad key, an unreachable
            // endpoint, a colliding id -- never reach the event sink, so
            // without this the status line reads "Generating..." forever.
            if let Err(error) = engine.stream_speech(request).await {
                crate::events::on_main(move || {
                    crate::app::with_app(|app| {
                        app.with_settings(|window| window.set_voice_status_for(&request_id, &error))
                    });
                });
            }
        });
    }

    /// Set the voice status only while `request_id` is still the preview on
    /// screen, so a stale failure cannot overwrite a newer "Generating...".
    pub fn set_voice_status_for(&self, request_id: &str, message: &str) {
        if self.ivars().preview_request.borrow().as_deref() == Some(request_id) {
            self.set_voice_status(message);
        }
    }

    fn stop_preview(&self) {
        let ivars = self.ivars();
        let request = ivars.preview_request.borrow_mut().take();
        let _ = ivars.engine.cancel_speech(request.as_deref());
        crate::app::with_app(|app| app.tts().stop());
    }
}

// ── Value helpers ─────────────────────────────────────────

/// The tag on the control a text-editing notification came from.
fn notified_tag(notification: &NSNotification) -> Option<isize> {
    let object = notification.object()?;
    // SAFETY: every control this object is the delegate of is an `NSControl`,
    // and `tag` is declared on `NSView`.
    Some(unsafe { msg_send![&*object, tag] })
}

/// Whether a field's value is committed when editing ends rather than on every
/// keystroke: the three keychain slots and the three endpoint URLs.
fn writes_on_end_editing(tag: isize) -> bool {
    matches!(
        tag,
        TAG_API_KEY
            | TAG_FORMATTING_KEY
            | TAG_TTS_KEY
            | TAG_PROVIDER_URL
            | TAG_FORMATTING_URL
            | TAG_TTS_URL
    )
}

/// The binding for `action` as the recorder spells it.
fn binding_text(settings: &openflow_core::settings::Settings, action: &str) -> String {
    settings
        .shortcut(action)
        .map(|shortcut| hotkeys::describe(&shortcut))
        .unwrap_or_else(|_| "Not set".to_string())
}

fn is_on(switch: &NSSwitch) -> bool {
    switch.state() == NSControlStateValueOn
}

fn set_switch(switch: &NSSwitch, on: bool) {
    switch.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
}

/// Settings booleans are the literal strings the web screen writes.
fn bool_setting(on: bool) -> &'static str {
    if on {
        "true"
    } else {
        "false"
    }
}

fn string_value(control: &NSControl) -> String {
    { control.stringValue() }.to_string()
}

fn selected_value<'a>(popup: &NSPopUpButton, table: &'a [(&'a str, &'a str)]) -> &'a str {
    let index = { popup.indexOfSelectedItem() };
    if index < 0 {
        return table.first().map(|(value, _)| *value).unwrap_or("");
    }
    table
        .get(index as usize)
        .map(|(value, _)| *value)
        .unwrap_or_else(|| table.first().map(|(value, _)| *value).unwrap_or(""))
}

fn select_value(popup: &NSPopUpButton, table: &[(&str, &str)], value: &str) {
    let index = table
        .iter()
        .position(|(stored, _)| *stored == value)
        .unwrap_or(0);
    popup.selectItemAtIndex(index as isize);
}

fn select_pairs(popup: &NSPopUpButton, table: &[(String, String)], value: &str) {
    let index = table
        .iter()
        .position(|(stored, _)| stored == value)
        .unwrap_or(0);
    popup.selectItemAtIndex(index as isize);
}

fn fill_combo(combo: &NSComboBox, models: &[ModelInfo], kind: &str) {
    unsafe {
        combo.removeAllItems();
        for model in models.iter().filter(|model| model.model_type == kind) {
            combo.addItemWithObjectValue(&NSString::from_str(&model.id));
        }
    }
}

/// `custom:<url>` is one setting holding two fields, so the window splits it on
/// read and joins it on write.
pub fn split_provider(stored: &str) -> (String, String) {
    match stored.strip_prefix("custom:") {
        Some(url) => ("custom".to_string(), url.to_string()),
        None => (stored.to_string(), String::new()),
    }
}

pub fn join_provider(kind: &str, url: &str) -> String {
    if kind == "custom" {
        format!("custom:{}", url.trim().trim_end_matches('/'))
    } else {
        kind.to_string()
    }
}

// ── Tab construction ──────────────────────────────────────

#[allow(clippy::type_complexity)]
fn build_tabs(
    mtm: MainThreadMarker,
) -> (
    Retained<objc2_app_kit::NSView>,
    Retained<objc2_app_kit::NSView>,
    Retained<objc2_app_kit::NSView>,
    Retained<objc2_app_kit::NSView>,
    Controls,
) {
    // General
    let mut form = Form::new(mtm, TAB_WIDTH, TAB_HEIGHT);
    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Microphone", l));
    let microphone = popup(mtm, c, TAG_MICROPHONE, &[]);
    form.add(&microphone);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Record shortcut", l));
    let hotkey_record = button(mtm, c, "Option+V", TAG_HOTKEY_RECORD);
    form.add(&hotkey_record);
    let n = form.control_only(14.0);
    form.add(&note(mtm, "Hold to record, release to transcribe.", n));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Re-copy shortcut", l));
    let hotkey_recopy = button(mtm, c, "Ctrl+Shift+V", TAG_HOTKEY_RECOPY);
    form.add(&hotkey_recopy);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Insert text by", l));
    let insert_method = popup(mtm, c, TAG_INSERT_METHOD, &titles(INSERT_METHODS));
    form.add(&insert_method);
    let n = form.control_only(26.0);
    form.add(&note(
        mtm,
        "Paste sends Cmd+V and works everywhere. Type sends the characters and never touches the clipboard.",
        n,
    ));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Keep my clipboard", l));
    let preserve_clipboard = switch_control(mtm, switch_rect(c), TAG_PRESERVE_CLIPBOARD);
    form.add(&preserve_clipboard);
    let n = form.control_only(26.0);
    form.add(&note(
        mtm,
        "Puts back whatever you had copied once a dictation lands. Text and images only.",
        n,
    ));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Hide overlay when idle", l));
    let overlay_only = switch_control(mtm, switch_rect(c), TAG_OVERLAY_ONLY);
    form.add(&overlay_only);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Overlay position", l));
    let positions = position_options();
    let position_titles: Vec<&str> = positions.iter().map(|(_, title)| title.as_str()).collect();
    let overlay_position = popup(mtm, c, TAG_OVERLAY_POSITION, &position_titles);
    form.add(&overlay_position);
    let n = form.control_only(14.0);
    form.add(&note(mtm, "You can also drag the pill to move it.", n));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Appearance", l));
    let theme = popup(mtm, c, TAG_THEME, &titles(THEMES));
    form.add(&theme);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Language", l));
    let language = popup(mtm, c, TAG_LANGUAGE, &titles(LANGUAGES));
    form.add(&language);
    let general = form.view.clone();

    // Providers
    let mut form = Form::new(mtm, TAB_WIDTH, TAB_HEIGHT);
    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Transcription", l));
    let provider = popup(mtm, c, TAG_PROVIDER, &titles(PROVIDERS));
    form.add(&provider);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Endpoint URL", l));
    let provider_url = text_field(mtm, c, TAG_PROVIDER_URL);
    form.add(&provider_url);
    let n = form.control_only(14.0);
    form.add(&note(mtm, "Only used by Custom endpoint.", n));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "API key", l));
    let api_key = secure_field(mtm, c, TAG_API_KEY);
    form.add(&api_key);
    let n = form.control_only(14.0);
    form.add(&note(
        mtm,
        "Stored in the macOS keychain, never in the database.",
        n,
    ));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Same for cleanup", l));
    let same_provider = switch_control(mtm, switch_rect(c), TAG_SAME_PROVIDER);
    form.add(&same_provider);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Cleanup provider", l));
    let formatting_provider = popup(
        mtm,
        c,
        TAG_FORMATTING_PROVIDER,
        &titles(FORMATTING_PROVIDERS),
    );
    form.add(&formatting_provider);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Cleanup endpoint", l));
    let formatting_url = text_field(mtm, c, TAG_FORMATTING_URL);
    form.add(&formatting_url);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Cleanup key", l));
    let formatting_key = secure_field(mtm, c, TAG_FORMATTING_KEY);
    form.add(&formatting_key);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Smart cleanup", l));
    let format_enabled = switch_control(mtm, switch_rect(c), TAG_FORMAT_ENABLED);
    form.add(&format_enabled);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Speech-to-text model", l));
    let stt_model = combo(mtm, c, TAG_STT_MODEL);
    form.add(&stt_model);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Cleanup model", l));
    let chat_model = combo(mtm, c, TAG_CHAT_MODEL);
    form.add(&chat_model);

    let c = form.control_only(ROW);
    let fetch = button(mtm, c, "Fetch models", TAG_FETCH_MODELS);
    form.add(&fetch);
    let n = form.control_only(28.0);
    let models_status = note(mtm, "", n);
    form.add(&models_status);
    let providers = form.view.clone();

    // Voice
    let mut form = Form::new(mtm, TAB_WIDTH, TAB_HEIGHT);
    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Voice features", l));
    let tts_enabled = switch_control(mtm, switch_rect(c), TAG_TTS_ENABLED);
    form.add(&tts_enabled);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Speech endpoint", l));
    let tts_provider = popup(mtm, c, TAG_TTS_PROVIDER, &titles(TTS_PROVIDERS));
    form.add(&tts_provider);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Endpoint URL", l));
    let tts_url = text_field(mtm, c, TAG_TTS_URL);
    form.add(&tts_url);
    let n = form.control_only(26.0);
    form.add(&note(
        mtm,
        "Point this at a machine on your network to keep audio off the internet.",
        n,
    ));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Voice key", l));
    let tts_key = secure_field(mtm, c, TAG_TTS_KEY);
    form.add(&tts_key);
    let n = form.control_only(26.0);
    form.add(&note(
        mtm,
        "Optional. Left blank, the transcription key is reused only when the endpoint is the same one.",
        n,
    ));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Voice model", l));
    let tts_model = combo(mtm, c, TAG_TTS_MODEL);
    form.add(&tts_model);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Voice", l));
    let tts_voice = combo(mtm, c, TAG_TTS_VOICE);
    form.add(&tts_voice);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Audio format", l));
    let tts_format = popup(mtm, c, TAG_TTS_FORMAT, &titles(TTS_FORMATS));
    form.add(&tts_format);
    let n = form.control_only(14.0);
    form.add(&note(mtm, "Groq's Orpheus answers only in WAV.", n));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Preview text", l));
    let preview_text = text_field(mtm, c, 0);
    preview_text.setStringValue(&NSString::from_str(
        "OpenFlow turns what you say into text, wherever you are typing.",
    ));
    form.add(&preview_text);

    let c = form.control_only(ROW);
    let half = NSRect::new(c.origin, NSSize::new(120.0, c.size.height));
    let play = button(mtm, half, "Preview", 0);
    form.add(&play);
    let stop_frame = NSRect::new(
        NSPoint::new(c.origin.x + 128.0, c.origin.y),
        NSSize::new(90.0, c.size.height),
    );
    let stop = button(mtm, stop_frame, "Stop", 0);
    form.add(&stop);
    let n = form.control_only(28.0);
    let voice_status = note(mtm, "", n);
    form.add(&voice_status);
    let voice = form.view.clone();

    // Privacy
    let mut form = Form::new(mtm, TAB_WIDTH, TAB_HEIGHT);
    let (l, _) = form.row(ROW);
    form.add(&label(mtm, "Dictionary", l));
    let area = form.full(96.0);
    let (scroll, dictionary) = text_view(mtm, area);
    form.add(&scroll);
    let n = form.full(14.0);
    let dictionary_count = note(mtm, "0/800", n);
    form.add(&dictionary_count);
    let n = form.full(28.0);
    form.add(&note(
        mtm,
        "Names and terms to spell correctly, comma separated. Sent to Whisper as a spelling hint.",
        n,
    ));

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Save history", l));
    let save_history = switch_control(mtm, switch_rect(c), TAG_SAVE_HISTORY);
    form.add(&save_history);

    let (l, c) = form.row(ROW);
    form.add(&label(mtm, "Auto-delete after", l));
    let retention = popup(mtm, c, TAG_RETENTION, &titles(RETENTIONS));
    form.add(&retention);

    let c = form.control_only(ROW);
    let clear = button(
        mtm,
        NSRect::new(c.origin, NSSize::new(140.0, c.size.height)),
        "Clear history",
        0,
    );
    form.add(&clear);
    let n = form.control_only(28.0);
    let history_status = note(mtm, "", n);
    form.add(&history_status);
    let privacy = form.view.clone();

    let controls = Controls {
        microphone,
        microphone_ids: RefCell::new(vec![String::new()]),
        hotkey_record,
        hotkey_recopy,
        insert_method,
        preserve_clipboard,
        overlay_only,
        overlay_position,
        theme,
        language,
        provider,
        provider_url,
        api_key,
        same_provider,
        formatting_provider,
        formatting_url,
        formatting_key,
        format_enabled,
        stt_model,
        chat_model,
        models_status,
        tts_enabled,
        tts_provider,
        tts_url,
        tts_key,
        tts_model,
        tts_voice,
        tts_format,
        preview_text,
        voice_status,
        dictionary,
        dictionary_count,
        save_history,
        retention,
        history_status,
        actions: vec![fetch, play, stop, clear],
    };
    (general, providers, voice, privacy, controls)
}

/// The action buttons, in the order `build_tabs` creates them.
const ACTION_SELECTORS: [fn() -> Sel; 4] = [
    || sel!(fetchModels:),
    || sel!(previewVoice:),
    || sel!(stopVoice:),
    || sel!(clearHistory:),
];

/// A switch is 38 px wide whatever the column is; left-align it in the column.
fn switch_rect(column: NSRect) -> NSRect {
    NSRect::new(column.origin, NSSize::new(38.0, column.size.height))
}

fn titles(table: &'static [(&'static str, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(_, title)| *title).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider setting packs a kind and a URL into one string. Both halves
    /// have to survive a round trip, or a custom endpoint silently becomes Groq
    /// the next time the window saves.
    #[test]
    fn the_provider_setting_round_trips_through_two_controls() {
        for stored in [
            "groq",
            "openai",
            "openrouter",
            "deepgram",
            "custom:http://192.168.1.10:8880/v1",
        ] {
            let (kind, url) = split_provider(stored);
            assert_eq!(join_provider(&kind, &url), stored, "{stored}");
        }
        // A trailing slash is trimmed on write, the way the web screen trims it,
        // so `same_endpoint` can compare two custom URLs by string.
        assert_eq!(
            join_provider("custom", " http://10.0.0.5:8880/v1/ "),
            "custom:http://10.0.0.5:8880/v1"
        );
        assert_eq!(join_provider("groq", "http://ignored"), "groq");
    }

    /// Popups map a selected index to a stored value by table position. An index
    /// that no longer exists must fall back to the first entry, which is the
    /// shipped default in every table here.
    #[test]
    fn option_tables_have_the_shipped_default_first() {
        assert_eq!(PROVIDERS[0].0, openflow_core::settings::DEFAULT_PROVIDER);
        assert_eq!(
            TTS_PROVIDERS
                .iter()
                .position(|(value, _)| *value == openflow_core::settings::DEFAULT_TTS_PROVIDER),
            Some(1),
            "OpenRouter is the shipped voice default and has to be in the table"
        );
        assert_eq!(THEMES[0].0, "", "an unset theme means follow the system");
        assert_eq!(INSERT_METHODS[0].0, "paste");
        assert_eq!(RETENTIONS[0].0, "", "an unset retention means keep forever");
        assert_eq!(
            TTS_FORMATS[0].0,
            openflow_core::settings::DEFAULT_TTS_RESPONSE_FORMAT
        );
        assert_eq!(LANGUAGES[0].0, "", "an unset language means auto-detect");
    }

    /// Every anchor `overlay.rs` knows has to be offerable, or a pill dragged
    /// into a corner shows a position the popup cannot represent.
    #[test]
    fn the_position_popup_covers_every_anchor() {
        let options = position_options();
        assert_eq!(options.len(), overlay::POSITIONS.len());
        for (name, _, _) in overlay::POSITIONS {
            assert!(
                options.iter().any(|(value, _)| value == name),
                "{name} must be offered"
            );
        }
        assert_eq!(options[3].0, "left-center");
        assert_eq!(options[3].1, "Left Center");
    }

    /// A keystroke in a key field must not reach the keychain, and a keystroke
    /// in a URL field must not save half an endpoint. Everything else stays on
    /// the live path, or the window stops feeling like autosave.
    #[test]
    fn only_credentials_and_endpoints_wait_for_editing_to_end() {
        for tag in [
            TAG_API_KEY,
            TAG_FORMATTING_KEY,
            TAG_TTS_KEY,
            TAG_PROVIDER_URL,
            TAG_FORMATTING_URL,
            TAG_TTS_URL,
        ] {
            assert!(writes_on_end_editing(tag), "tag {tag} should be deferred");
        }
        for tag in [
            TAG_STT_MODEL,
            TAG_CHAT_MODEL,
            TAG_TTS_MODEL,
            TAG_TTS_VOICE,
            TAG_MICROPHONE,
            TAG_THEME,
            TAG_RETENTION,
            0,
        ] {
            assert!(!writes_on_end_editing(tag), "tag {tag} should be live");
        }
    }

    /// The two boolean spellings the settings table understands.
    #[test]
    fn switches_write_the_strings_the_settings_table_reads() {
        assert_eq!(bool_setting(true), "true");
        assert_eq!(bool_setting(false), "false");
        assert_eq!(
            openflow_core::insert::ClipboardPolicy::from_setting(Some(
                bool_setting(false).to_string()
            )),
            openflow_core::insert::ClipboardPolicy::Keep
        );
    }
}
