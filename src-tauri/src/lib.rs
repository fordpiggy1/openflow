mod audio;
mod db;
mod plugins;
mod secrets;
mod transcribe;

use audio::{wav_duration_ms, AudioDevice, AudioRecorder};
use base64::Engine;
use db::{Database, Transcription};
use futures_util::StreamExt;
use plugins::{HookPayload, PluginInfo, PluginManager};
use secrets::SecretStore;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio_util::sync::CancellationToken;
use transcribe::{ModelInfo, Provider};

struct AppState {
    recorder: AudioRecorder,
    db: Database,
    secrets: SecretStore,
    plugin_manager: PluginManager,
    /// `Some(started_at)` while capturing. Carrying the start time rather than
    /// a bare bool lets a dropped hotkey-release event time out instead of
    /// wedging recording until the app restarts.
    recording: Mutex<Option<Instant>>,
    last_transcription: Mutex<Option<String>>,
    transcription_jobs: Mutex<HashMap<String, CancellationToken>>,
    speech_jobs: Mutex<HashMap<String, CancellationToken>>,
}

const SECRET_SETTINGS: &[&str] = &["api_key", "formatting_api_key", "tts_api_key"];

fn is_secret_setting(key: &str) -> bool {
    SECRET_SETTINGS.contains(&key)
}

// ── Settings ──────────────────────────────────────────────
#[tauri::command]
fn set_api_key(state: State<AppState>, key: String) -> Result<(), String> {
    state.secrets.set("api_key", &key)
}

#[tauri::command]
fn get_api_key(state: State<AppState>) -> Result<Option<String>, String> {
    state.secrets.get("api_key")
}

#[tauri::command]
fn get_setting(state: State<AppState>, key: String) -> Result<Option<String>, String> {
    if is_secret_setting(&key) {
        state.secrets.get(&key)
    } else {
        Ok(state.db.get_setting(&key))
    }
}

#[tauri::command]
fn set_setting(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    if is_secret_setting(&key) {
        state.secrets.set(&key, &value)
    } else {
        state.db.set_setting(&key, &value)
    }
}

// ── History ───────────────────────────────────────────────
#[tauri::command]
fn get_history(state: State<AppState>, limit: Option<usize>) -> Result<Vec<Transcription>, String> {
    state.db.get_history(limit.unwrap_or(50))
}

#[tauri::command]
fn search_history(state: State<AppState>, query: String) -> Result<Vec<Transcription>, String> {
    state.db.search_history(&query, 50)
}

// ── Models + Devices ──────────────────────────────────────
#[tauri::command]
async fn fetch_models(
    state: State<'_, AppState>,
    provider_name: Option<String>,
    api_key_override: Option<String>,
) -> Result<Vec<ModelInfo>, String> {
    let provider_str = provider_name
        .or_else(|| state.db.get_setting("provider"))
        .unwrap_or_else(|| "groq".to_string());
    let provider = Provider::from_str(&provider_str);
    // An empty key is valid for a self-hosted endpoint; transcribe::fetch_models
    // rejects it for every hosted provider.
    let api_key = api_key_override
        .or_else(|| state.secrets.get("api_key").ok().flatten())
        .unwrap_or_default();
    transcribe::fetch_models(&api_key, &provider).await
}

#[tauri::command]
fn list_audio_devices(state: State<AppState>) -> Result<Vec<AudioDevice>, String> {
    state.recorder.list_devices()
}

#[derive(Serialize)]
struct SpeechAudio {
    data_base64: String,
    mime_type: String,
    format: String,
    model: String,
}

#[derive(Serialize, Clone)]
struct SpeechStarted {
    request_id: String,
    model: String,
    format: String,
}

#[derive(Serialize, Clone)]
struct SpeechChunk {
    request_id: String,
    sequence: u64,
    data_base64: String,
}

#[derive(Serialize, Clone)]
struct SpeechResult {
    request_id: String,
    mime_type: String,
    format: String,
    model: String,
    bytes: u64,
}

#[derive(Serialize, Clone)]
struct SpeechError {
    request_id: String,
    error: String,
    cancelled: bool,
}

fn speech_settings(
    state: &AppState,
    model: Option<String>,
    voice: Option<String>,
    response_format: Option<String>,
) -> Result<(Provider, String, String, String, String), String> {
    let provider = Provider::from_str(
        &state
            .db
            .get_setting("tts_provider")
            .unwrap_or_else(|| "openrouter".to_string()),
    );
    let transcription_provider = Provider::from_str(
        &state
            .db
            .get_setting("provider")
            .unwrap_or_else(|| "groq".to_string()),
    );
    let api_key = resolve_speech_key(
        &provider,
        state.secrets.get("tts_api_key")?,
        state.secrets.get("api_key")?,
        same_endpoint(&provider, &transcription_provider),
    )?;
    // Each provider has its own model and voice names, so blank falls back
    // per provider rather than to Gemini's.
    let model = model
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .db
                .get_setting("tts_model")
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| provider.default_tts_model().to_string());
    let voice = voice
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .db
                .get_setting("tts_voice")
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| provider.default_tts_voice().to_string());
    let format = response_format
        .filter(|value| !value.trim().is_empty())
        .or_else(|| state.db.get_setting("tts_response_format"))
        .unwrap_or_else(|| "mp3".to_string())
        .to_ascii_lowercase();
    Ok((provider, api_key, model, voice, format))
}

/// True when two provider settings name the same service, so one credential
/// is valid for both. Two custom endpoints are the same only if their URLs are.
fn same_endpoint(a: &Provider, b: &Provider) -> bool {
    match (a, b) {
        (Provider::Custom { base_url: x }, Provider::Custom { base_url: y }) => x == y,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// Which credential a speech request may carry.
///
/// The transcription key is shared only with the same endpoint. A different
/// hosted provider cannot use it (an OpenRouter key is worthless at OpenAI),
/// and a self-hosted server must not receive it: that would send a cloud
/// credential in clear text across the LAN. With no usable key, a custom
/// endpoint proceeds unauthenticated and a hosted one is refused up front.
fn resolve_speech_key(
    provider: &Provider,
    dedicated: Option<String>,
    shared: Option<String>,
    same_endpoint_as_transcription: bool,
) -> Result<String, String> {
    let present = |key: Option<String>| key.filter(|value| !value.trim().is_empty());
    if let Some(key) = present(dedicated) {
        return Ok(key);
    }
    if same_endpoint_as_transcription {
        if let Some(key) = present(shared) {
            return Ok(key);
        }
    }
    if provider.is_custom() {
        return Ok(String::new());
    }
    Err(if same_endpoint_as_transcription {
        "No API key set. Add your key in Settings.".to_string()
    } else {
        "The voice provider needs its own API key. Use the provider you transcribe with, or point the speech endpoint at a self-hosted server.".to_string()
    })
}

#[tauri::command]
async fn synthesize_speech(
    state: State<'_, AppState>,
    text: String,
    model: Option<String>,
    voice: Option<String>,
    response_format: Option<String>,
) -> Result<SpeechAudio, String> {
    let (provider, api_key, model, voice, format) =
        speech_settings(&state, model, voice, response_format)?;
    let response =
        transcribe::request_speech(&text, &api_key, &provider, &model, &voice, &format).await?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read speech audio: {}", error))?;
    if bytes.len() > 50 * 1024 * 1024 {
        return Err("Generated speech is too large".to_string());
    }
    Ok(SpeechAudio {
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime_type: transcribe::speech_mime(&format).to_string(),
        format,
        model,
    })
}

#[tauri::command]
async fn stream_speech(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    model: Option<String>,
    voice: Option<String>,
    response_format: Option<String>,
    request_id: Option<String>,
) -> Result<SpeechResult, String> {
    let request_id = request_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if request_id.len() > 100
        || !request_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err("Speech request id is invalid".to_string());
    }
    let (provider, api_key, model, voice, format) =
        speech_settings(&state, model, voice, response_format)?;
    let cancellation = CancellationToken::new();
    {
        let mut jobs = state
            .speech_jobs
            .lock()
            .map_err(|_| "Speech job state is unavailable".to_string())?;
        if jobs.contains_key(&request_id) {
            return Err("A speech request with this id is already running".to_string());
        }
        jobs.insert(request_id.clone(), cancellation.clone());
    }
    let started = SpeechStarted {
        request_id: request_id.clone(),
        model: model.clone(),
        format: format.clone(),
    };
    let _ = app.emit("tts-started", &started);

    let result = async {
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err("Speech generation cancelled".to_string()),
            response = transcribe::request_speech(&text, &api_key, &provider, &model, &voice, &format) => response?,
        };
        let mut stream = response.bytes_stream();
        let mut sequence = 0_u64;
        let mut total = 0_u64;
        loop {
            let next = tokio::select! {
                _ = cancellation.cancelled() => return Err("Speech generation cancelled".to_string()),
                next = stream.next() => next,
            };
            let Some(chunk) = next else { break; };
            let chunk = chunk.map_err(|error| format!("Speech stream failed: {}", error))?;
            total = total.saturating_add(chunk.len() as u64);
            if total > 50 * 1024 * 1024 { return Err("Generated speech is too large".to_string()); }
            let payload = SpeechChunk {
                request_id: request_id.clone(),
                sequence,
                data_base64: base64::engine::general_purpose::STANDARD.encode(chunk),
            };
            app.emit("tts-audio-chunk", payload).map_err(|error| format!("Could not deliver speech audio: {}", error))?;
            sequence += 1;
        }
        let result = SpeechResult {
            request_id: request_id.clone(),
            mime_type: transcribe::speech_mime(&format).to_string(),
            format: format.clone(),
            model: model.clone(),
            bytes: total,
        };
        let _ = app.emit("tts-finished", &result);
        Ok(result)
    }.await;
    if let Err(error) = &result {
        let payload = SpeechError {
            request_id: request_id.clone(),
            error: error.clone(),
            cancelled: cancellation.is_cancelled(),
        };
        let _ = app.emit("tts-error", payload);
    }
    if let Ok(mut jobs) = state.speech_jobs.lock() {
        jobs.remove(&request_id);
    }
    result
}

#[tauri::command]
fn cancel_speech(state: State<AppState>, request_id: Option<String>) -> Result<bool, String> {
    let jobs = state
        .speech_jobs
        .lock()
        .map_err(|_| "Speech job state is unavailable".to_string())?;
    let mut cancelled = false;
    for (id, token) in jobs.iter() {
        if request_id
            .as_deref()
            .map(|requested| requested == id)
            .unwrap_or(true)
        {
            token.cancel();
            cancelled = true;
        }
    }
    Ok(cancelled)
}

// ── Recording ─────────────────────────────────────────────
#[tauri::command]
fn start_recording(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let mut recording = state
        .recording
        .lock()
        .map_err(|_| "Recording state is unavailable".to_string())?;
    if !recording_slot_free(&recording) {
        return Err("A recording is already active".to_string());
    }
    let device = state.db.get_setting("microphone");
    state.recorder.start(device)?;
    *recording = Some(Instant::now());
    if insert_method(&state.db) == InsertMethod::Type {
        prewarm_typing();
    }
    let _ = app.emit("recording-state", "recording");
    Ok(())
}

#[tauri::command]
async fn stop_recording_and_transcribe(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Transcription, String> {
    let wav_result = {
        let mut recording = state
            .recording
            .lock()
            .map_err(|_| "Recording state is unavailable".to_string())?;
        if recording.is_none() {
            return Err("No recording is active".to_string());
        }
        let result = state.recorder.stop();
        *recording = None;
        result
    };
    let wav_bytes = match wav_result {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = app.emit("recording-state", "idle");
            return Err(error);
        }
    };
    let (request_id, cancellation) = match register_transcription_job(&state) {
        Ok(job) => job,
        Err(error) => {
            let _ = app.emit("recording-state", "idle");
            return Err(error);
        }
    };
    let _ = app.emit("recording-state", "transcribing");
    let result =
        run_transcription_pipeline(&app, &state, wav_bytes, request_id, cancellation).await;
    if result.is_ok() {
        update_tray_menu(&app);
    }
    emit_idle_if_quiescent(&app, &state);
    result
}

#[tauri::command]
fn cancel_current_transcription(state: State<AppState>) -> Result<bool, String> {
    let jobs = state
        .transcription_jobs
        .lock()
        .map_err(|_| "Transcription state is unavailable".to_string())?;
    if jobs.is_empty() {
        Ok(false)
    } else {
        for cancellation in jobs.values() {
            cancellation.cancel();
        }
        Ok(true)
    }
}

#[tauri::command]
fn copy_last_transcription(state: State<AppState>) -> Result<(), String> {
    let last = state
        .last_transcription
        .lock()
        .map_err(|_| "Clipboard history is unavailable".to_string())?;
    match last.as_ref() {
        Some(text) => paste_to_clipboard(text, insert_method(&state.db)),
        None => Err("No previous transcription".to_string()),
    }
}

#[tauri::command]
fn delete_transcription(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    state.db.delete_transcription(&id)?;
    update_tray_menu(&app);
    Ok(())
}

#[tauri::command]
fn clear_history(app: AppHandle, state: State<AppState>) -> Result<usize, String> {
    let removed = state.db.clear_history()?;
    if let Ok(mut last) = state.last_transcription.lock() {
        *last = None;
    }
    update_tray_menu(&app);
    Ok(removed)
}

#[tauri::command]
fn copy_text(_state: State<AppState>, text: String) -> Result<(), String> {
    write_clipboard(&text)
}

/// Copy and paste in one step, for callers that want the keystroke.
#[tauri::command]
fn paste_text(state: State<AppState>, text: String) -> Result<(), String> {
    paste_to_clipboard(&text, insert_method(&state.db))
}

// ── Hotkey management ─────────────────────────────────────
#[tauri::command]
fn rebind_hotkey(
    app: AppHandle,
    state: State<AppState>,
    action: String,
    shortcut_str: String,
) -> Result<(), String> {
    if !matches!(action.as_str(), "record" | "recopy") {
        return Err("Unknown hotkey action".to_string());
    }
    let gs = app.global_shortcut();
    let old_key = format!("hotkey_{}", action);
    let new_shortcut = parse_shortcut(&shortcut_str)
        .map_err(|e| format!("Invalid shortcut '{}': {}", shortcut_str, e))?;
    let default = default_shortcut(&action).ok_or("Unknown hotkey action".to_string())?;
    let old = get_shortcut_from_settings(&state.db, &action, default).ok();
    let old_was_registered = old
        .as_ref()
        .map(|shortcut| gs.is_registered(*shortcut))
        .unwrap_or(false);
    if old.as_ref() == Some(&new_shortcut) && old_was_registered {
        return state.db.set_setting(&old_key, &shortcut_str);
    }
    gs.register(new_shortcut)
        .map_err(|e| format!("Failed to register shortcut: {}", e))?;
    if let Some(old_shortcut) = old.filter(|_| old_was_registered) {
        if let Err(error) = gs.unregister(old_shortcut) {
            let _ = gs.unregister(new_shortcut);
            return Err(format!("Could not replace the old shortcut: {}", error));
        }
    }
    if let Err(error) = state.db.set_setting(&old_key, &shortcut_str) {
        let _ = gs.unregister(new_shortcut);
        if let Some(old_shortcut) = old.filter(|_| old_was_registered) {
            let _ = gs.register(old_shortcut);
        }
        return Err(error);
    }
    Ok(())
}

// ── Plugins ───────────────────────────────────────────────
#[tauri::command]
fn list_plugins(state: State<AppState>) -> Vec<PluginInfo> {
    state.plugin_manager.list_plugins()
}

#[tauri::command]
fn enable_plugin(state: State<AppState>, id: String) -> Result<(), String> {
    state.plugin_manager.enable_plugin(&id)
}

#[tauri::command]
fn disable_plugin(state: State<AppState>, id: String) -> Result<(), String> {
    state.plugin_manager.disable_plugin(&id)
}

#[tauri::command]
fn install_plugin(state: State<AppState>, manifest: String) -> Result<PluginInfo, String> {
    state.plugin_manager.install_plugin(&manifest)
}

// ── Core logic ────────────────────────────────────────────
fn register_transcription_job(state: &AppState) -> Result<(String, CancellationToken), String> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancellation = CancellationToken::new();
    state
        .transcription_jobs
        .lock()
        .map_err(|_| "Transcription state is unavailable".to_string())?
        .insert(request_id.clone(), cancellation.clone());
    Ok((request_id, cancellation))
}

async fn run_transcription_pipeline(
    app: &AppHandle,
    state: &AppState,
    wav_bytes: Vec<u8>,
    request_id: String,
    cancellation: CancellationToken,
) -> Result<Transcription, String> {
    let result = run_transcription_pipeline_inner(state, cancellation, wav_bytes).await;
    if let Ok(mut active) = state.transcription_jobs.lock() {
        active.remove(&request_id);
    }
    match result {
        Ok((transcription, paste_warning)) => {
            if let Some(warning) = paste_warning {
                let _ = app.emit("transcription-warning", warning);
            }
            Ok(transcription)
        }
        Err(error) => Err(error),
    }
}

async fn run_transcription_pipeline_inner(
    state: &AppState,
    cancellation: CancellationToken,
    wav_bytes: Vec<u8>,
) -> Result<(Transcription, Option<String>), String> {
    let duration_ms = wav_duration_ms(&wav_bytes);

    // Empty is valid for a self-hosted endpoint; transcribe_audio rejects it
    // for every hosted provider.
    let transcription_key = state.secrets.get("api_key")?.unwrap_or_default();
    let language = state.db.get_setting("language");
    let provider_str = state
        .db
        .get_setting("provider")
        .unwrap_or_else(|| "groq".to_string());
    let transcription_provider = Provider::from_str(&provider_str);
    let format_enabled = state
        .db
        .get_setting("format_enabled")
        .map(|v| v != "false")
        .unwrap_or(true);
    let same_provider = state
        .db
        .get_setting("same_provider")
        .map(|v| v != "false")
        .unwrap_or(true);
    let stt_model = state.db.get_setting("stt_model");
    let chat_model = state.db.get_setting("chat_model");
    let dictionary = state.db.get_setting("dictionary");

    let raw_text = tokio::select! {
        _ = cancellation.cancelled() => return Err("Transcription cancelled".to_string()),
        result = transcribe::transcribe_audio(wav_bytes, &transcription_key, language.as_deref(), &transcription_provider, stt_model.as_deref(), dictionary.as_deref()) => result?,
    };
    let raw_text = state
        .plugin_manager
        .run_hook(
            "after_transcribe",
            HookPayload {
                raw_text: Some(raw_text),
                formatted_text: None,
                provider: Some(provider_str.clone()),
                language: language.clone(),
            },
        )?
        .raw_text
        .ok_or("Plugin removed the transcription text")?;

    let mut formatted = if format_enabled {
        let (fmt_provider, fmt_key) = if same_provider {
            (transcription_provider.clone(), transcription_key.clone())
        } else {
            let fp = state
                .db
                .get_setting("formatting_provider")
                .unwrap_or(provider_str.clone());
            let fmt_provider = Provider::from_str(&fp);
            // Share the transcription key only with the same endpoint. A
            // different server, hosted or on the LAN, never receives it.
            let fk = state
                .secrets
                .get("formatting_api_key")?
                .filter(|key| !key.trim().is_empty())
                .or_else(|| {
                    same_endpoint(&fmt_provider, &transcription_provider)
                        .then(|| transcription_key.clone())
                })
                .unwrap_or_default();
            (fmt_provider, fk)
        };
        tokio::select! {
            _ = cancellation.cancelled() => return Err("Transcription cancelled".to_string()),
            result = transcribe::format_text(&raw_text, &fmt_key, None, &fmt_provider, chat_model.as_deref()) => result?,
        }
    } else {
        raw_text.clone()
    };
    formatted = state
        .plugin_manager
        .run_hook(
            "after_format",
            HookPayload {
                raw_text: Some(raw_text.clone()),
                formatted_text: Some(formatted),
                provider: Some(provider_str.clone()),
                language: language.clone(),
            },
        )?
        .formatted_text
        .ok_or("Plugin removed the formatted text")?;

    let transcription = Transcription {
        id: uuid::Uuid::new_v4().to_string(),
        raw_text,
        formatted_text: Some(formatted.clone()),
        provider: provider_str,
        duration_ms,
        context_type: None,
        window_title: None,
        language,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    // Saving is opt-out, and an optional retention window trims old rows as we go.
    let history_enabled = state
        .db
        .get_setting("save_history")
        .map(|value| value != "false")
        .unwrap_or(true);
    if history_enabled {
        state.db.save_transcription(&transcription)?;
        if let Some(days) = state
            .db
            .get_setting("history_retention_days")
            .and_then(|value| value.parse::<i64>().ok())
        {
            let _ = state.db.prune_older_than(days);
        }
    }
    {
        let mut last = state
            .last_transcription
            .lock()
            .map_err(|_| "Clipboard history is unavailable".to_string())?;
        *last = Some(formatted);
    }

    let paste_warning = paste_to_clipboard(
        transcription
            .formatted_text
            .as_deref()
            .unwrap_or(&transcription.raw_text),
        insert_method(&state.db),
    )
    .err();

    Ok((transcription, paste_warning))
}

fn write_clipboard(text: &str) -> Result<(), String> {
    use arboard::Clipboard;
    let mut clipboard = Clipboard::new().map_err(|e| format!("Clipboard init failed: {}", e))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("Clipboard set failed: {}", e))
}

/// How text reaches the app the user is working in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InsertMethod {
    /// Put it on the clipboard, then send Cmd+V. Universal, because the
    /// keystroke is real and the app does the rest, but it overwrites whatever
    /// the user had copied.
    Paste,
    /// Synthesize keystrokes carrying the text itself. Leaves the clipboard
    /// alone and skips the paste round-trip, but the receiving app's
    /// autocorrect gets a say -- Notes rewrites `english` as `English`.
    Type,
}

impl InsertMethod {
    fn from_setting(value: Option<String>) -> Self {
        match value.as_deref().map(str::trim) {
            Some("type") => Self::Type,
            _ => Self::Paste,
        }
    }
}

fn insert_method(db: &Database) -> InsertMethod {
    InsertMethod::from_setting(db.get_setting("insert_method"))
}

/// Put `text` where the user is typing. Correct for the tray and the re-copy
/// hotkey, where focus is in the user's editor; wrong for a list inside
/// OpenFlow, which would insert into OpenFlow itself.
///
/// The clipboard is written either way. Under `Type` it is not the delivery
/// mechanism but the safety net: if insertion silently fails, the text is one
/// Cmd+V away instead of gone.
fn paste_to_clipboard(text: &str, method: InsertMethod) -> Result<(), String> {
    write_clipboard(text)?;
    match method {
        InsertMethod::Paste => simulate_paste(),
        InsertMethod::Type => type_text(text),
    }
}

/// Pay the first-post cost while the user is still talking.
///
/// A process's first posted event costs ~40ms. Left where `type_text` pays it,
/// the user waits through it *after* the transcription has already come back,
/// where it is a sixth of the whole round trip. Posted once when recording
/// starts, it lands in the seconds spent speaking and costs nothing visible.
///
/// Keycode 255 is not a key any keyboard reports, and no unicode payload is
/// attached, so nothing reaches the focused application. Measured alternatives,
/// both rejected: a modifier key only half-warms (12-16ms left on the next
/// post) and Fn may be bound to the emoji picker or an input-source switch; a
/// null event also only half-warms. With this one the next post is ~16us.
///
/// Fire and forget on its own thread: recording must not wait on it.
#[cfg(target_os = "macos")]
fn prewarm_typing() {
    use core_graphics::event::{CGEvent, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    std::thread::spawn(|| {
        const UNASSIGNED_KEYCODE: u16 = 255;
        let Ok(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            return;
        };
        if let Ok(down) = CGEvent::new_keyboard_event(source.clone(), UNASSIGNED_KEYCODE, true) {
            down.post(CGEventTapLocation::HID);
        }
        if let Ok(up) = CGEvent::new_keyboard_event(source, UNASSIGNED_KEYCODE, false) {
            up.post(CGEventTapLocation::HID);
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn prewarm_typing() {}

/// Send `text` as a synthesized keystroke, unicode payload and all.
///
/// Three things here are load-bearing, each one measured rather than assumed:
///
/// - **Empty text returns early.** `set_string("")` does not neutralize the
///   event, it leaves virtual keycode 0 to mean what it normally means, and
///   types a stray `a`.
/// - **One event carries the whole string.** 504 characters arrived intact in
///   ~60us, so there is nothing to gain by chunking and a dropped chunk to lose.
/// - **The warm-up is not decoration.** The first post of a process costs ~40ms,
///   and without paying it first the opening segment is swallowed -- silently,
///   and always at the start of the text.
#[cfg(target_os = "macos")]
fn type_text(text: &str) -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    if text.is_empty() {
        return Ok(());
    }

    let source = || {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "Could not reach the window server to type the text.".to_string())
    };

    // Warm up, then let the focused window settle before the real event.
    let _ = source()?;
    std::thread::sleep(Duration::from_millis(20));

    let src = source()?;
    let down = CGEvent::new_keyboard_event(src.clone(), 0, true)
        .map_err(|_| "Could not create the keystroke.".to_string())?;
    down.set_string(text);
    down.post(CGEventTapLocation::HID);

    let up = CGEvent::new_keyboard_event(src, 0, false)
        .map_err(|_| "Could not create the keystroke.".to_string())?;
    up.set_string(text);
    up.post(CGEventTapLocation::HID);
    Ok(())
}

/// Only macOS has a verified implementation. Everywhere else `Type` behaves as
/// `Paste` rather than shipping a guess about how synthesized input behaves on
/// a platform nobody tested.
#[cfg(not(target_os = "macos"))]
fn type_text(_text: &str) -> Result<(), String> {
    simulate_paste()
}

#[cfg(target_os = "macos")]
fn simulate_paste() -> Result<(), String> {
    use std::process::Command;
    std::thread::sleep(std::time::Duration::from_millis(200));
    let status = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to keystroke \"v\" using command down")
        .status()
        .map_err(|e| format!("Could not paste at the cursor: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err("Text was copied, but macOS blocked automatic paste. Grant OpenFlow Accessibility access.".to_string())
    }
}

#[cfg(target_os = "windows")]
fn simulate_paste() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = Command::new("powershell")
        .arg("-Command")
        .arg("Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('^v')")
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("Could not paste at the cursor: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err("Text was copied, but Windows blocked automatic paste.".to_string())
    }
}

#[cfg(target_os = "linux")]
fn simulate_paste() -> Result<(), String> {
    use std::process::Command;
    let result = Command::new("xdotool")
        .arg("key")
        .arg("ctrl+v")
        .output()
        .or_else(|_| {
            Command::new("ydotool")
                .arg("key")
                .arg("29:1")
                .arg("47:1")
                .arg("47:0")
                .arg("29:0")
                .output()
        })
        .map_err(|e| format!("Could not paste at the cursor: {}", e))?;
    if result.status.success() {
        Ok(())
    } else {
        Err("Text was copied, but the desktop blocked automatic paste.".to_string())
    }
}

// ── Shortcut parsing ──────────────────────────────────────
fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
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
    Ok(Shortcut::new(mods, code))
}

fn get_shortcut_from_settings(
    db: &Database,
    action: &str,
    default: &str,
) -> Result<Shortcut, String> {
    let key = format!("hotkey_{}", action);
    let shortcut_str = db.get_setting(&key).unwrap_or_else(|| default.to_string());
    parse_shortcut(&shortcut_str).or_else(|_| parse_shortcut(default))
}

fn default_shortcut(action: &str) -> Option<&'static str> {
    match action {
        "record" => Some("Option+V"),
        "recopy" => Some("Ctrl+Shift+V"),
        _ => None,
    }
}

/// A capture that has run this long is a stuck flag, not a real recording.
const MAX_RECORDING: Duration = Duration::from_secs(300);

/// True when no capture is in flight, or when the one on record is so old it
/// can only be the residue of a lost release event.
fn recording_slot_free(slot: &Option<Instant>) -> bool {
    match slot {
        None => true,
        Some(started) => started.elapsed() > MAX_RECORDING,
    }
}

// ── Hotkey handlers ───────────────────────────────────────
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn handle_hotkey_press(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(mut recording) = state.recording.lock() else {
        let _ = app.emit("transcription-error", "Recording state is unavailable");
        return;
    };
    if recording_slot_free(&recording) {
        *recording = Some(Instant::now());
        let device = state.db.get_setting("microphone");
        if let Err(e) = state.recorder.start(device) {
            eprintln!("Recording start failed: {}", e);
            *recording = None;
            let _ = app.emit("transcription-error", &e);
            return;
        }
        if insert_method(&state.db) == InsertMethod::Type {
            prewarm_typing();
        }
        let _ = app.emit("recording-state", "recording");
    }
}

fn emit_idle_if_quiescent(app: &AppHandle, state: &AppState) {
    let Ok(recording) = state.recording.lock() else {
        return;
    };
    if recording.is_some() {
        return;
    }
    let Ok(active) = state.transcription_jobs.lock() else {
        return;
    };
    if active.is_empty() {
        // Keep the recording lock through the emit. A new recording must publish
        // its "recording" state after this event, never before a stale "idle".
        let _ = app.emit("recording-state", "idle");
    }
}

fn handle_hotkey_release(app: &AppHandle) {
    let state = app.state::<AppState>();
    let wav_bytes = {
        let Ok(mut recording) = state.recording.lock() else {
            let _ = app.emit("transcription-error", "Recording state is unavailable");
            return;
        };
        if recording.is_none() {
            return;
        }
        let result = state.recorder.stop();
        *recording = None;
        result
    };

    let wav_bytes = match wav_bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = app.emit("transcription-error", &error);
            emit_idle_if_quiescent(app, &state);
            return;
        }
    };
    let (request_id, cancellation) = match register_transcription_job(&state) {
        Ok(job) => job,
        Err(error) => {
            let _ = app.emit("transcription-error", &error);
            emit_idle_if_quiescent(app, &state);
            return;
        }
    };
    let _ = app.emit("recording-state", "transcribing");

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app_handle.state::<AppState>();
        match run_transcription_pipeline(&app_handle, &state, wav_bytes, request_id, cancellation)
            .await
        {
            Ok(transcription) => {
                let _ = app_handle.emit("transcription-result", &transcription);
                update_tray_menu(&app_handle);
            }
            Err(e) => {
                let _ = app_handle.emit("transcription-error", &e);
            }
        }
        emit_idle_if_quiescent(&app_handle, &state);
    });
}

fn handle_recopy(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(last) = state.last_transcription.lock() else {
        return;
    };
    if let Some(text) = last.as_ref() {
        let _ = paste_to_clipboard(text, insert_method(&state.db));
        let _ = app.emit("recopy-success", "Copied last transcription");
    }
}

// ── Tray menu with recents ────────────────────────────────
fn build_tray_menu(
    app: &AppHandle,
) -> Result<tauri::menu::Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let state = app.state::<AppState>();
    let recents = state.db.get_history(20).unwrap_or_default();

    let mut builder = MenuBuilder::new(app);

    let show = MenuItemBuilder::with_id("show", "Show OpenFlow").build(app)?;
    builder = builder.item(&show);

    if !recents.is_empty() {
        builder = builder.separator();
        let label = MenuItemBuilder::with_id("_label_recents", "Recent Transcriptions")
            .enabled(false)
            .build(app)?;
        builder = builder.item(&label);

        for t in recents.iter() {
            let text = t.formatted_text.as_deref().unwrap_or(&t.raw_text);
            let preview: String = text.chars().take(40).collect();
            let display = if text.chars().count() > 40 {
                format!("{}...", preview)
            } else {
                preview
            };
            // Key by row id, not list index: indexing raced any transcription
            // that landed between building the menu and clicking it.
            let item = MenuItemBuilder::with_id(format!("recent:{}", t.id), &display).build(app)?;
            builder = builder.item(&item);
        }

        builder = builder.separator();
        let all = MenuItemBuilder::with_id("show_history", "All History...").build(app)?;
        builder = builder.item(&all);
    }

    builder = builder.separator();
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    builder = builder.item(&quit);

    Ok(builder.build()?)
}

fn update_tray_menu(app: &AppHandle) {
    if let Ok(menu) = build_tray_menu(app) {
        if let Some(tray) = app.tray_by_id("main_tray") {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

fn migrate_secrets(db: &Database, secrets: &SecretStore) {
    for key in SECRET_SETTINGS {
        let Some(plaintext) = db.get_setting(key) else {
            continue;
        };
        let secure_write_succeeded = match secrets.get(key) {
            Ok(Some(_)) => true,
            Ok(None) => match secrets.set(key, &plaintext) {
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
            if let Err(error) = db.remove_setting(key) {
                eprintln!("Could not remove migrated plaintext {}: {}", key, error);
            }
        }
    }
}

// ── App entry ─────────────────────────────────────────────
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    let state = app.state::<AppState>();
                    let Ok(record_shortcut) =
                        get_shortcut_from_settings(&state.db, "record", "Option+V")
                    else {
                        return;
                    };
                    let Ok(recopy_shortcut) =
                        get_shortcut_from_settings(&state.db, "recopy", "Ctrl+Shift+V")
                    else {
                        return;
                    };

                    if shortcut == &record_shortcut {
                        match event.state() {
                            ShortcutState::Pressed => handle_hotkey_press(app),
                            ShortcutState::Released => handle_hotkey_release(app),
                        }
                    } else if shortcut == &recopy_shortcut
                        && matches!(event.state(), ShortcutState::Pressed)
                    {
                        handle_recopy(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("No app dir: {}", e))?;
            let db = Database::new(app_dir).map_err(|e| format!("Database init failed: {}", e))?;
            let secrets = SecretStore::new(
                app.path()
                    .app_data_dir()
                    .map_err(|e| format!("No app dir: {}", e))?,
            );
            migrate_secrets(&db, &secrets);

            // Apply the retention policy at launch too, so a user who set it and
            // then left the app closed still gets old rows dropped.
            if let Some(days) = db
                .get_setting("history_retention_days")
                .and_then(|value| value.parse::<i64>().ok())
            {
                let _ = db.prune_older_than(days);
            }
            let last_transcription = db
                .get_history(1)?
                .into_iter()
                .next()
                .map(|item| item.formatted_text.unwrap_or(item.raw_text));

            app.manage(AppState {
                recorder: AudioRecorder::new(),
                db,
                secrets,
                plugin_manager: PluginManager::new(),
                recording: Mutex::new(None),
                last_transcription: Mutex::new(last_transcription),
                transcription_jobs: Mutex::new(HashMap::new()),
                speech_jobs: Mutex::new(HashMap::new()),
            });

            // Register hotkeys from settings (or defaults)
            let state = app.state::<AppState>();
            let record_shortcut = get_shortcut_from_settings(&state.db, "record", "Option+V")?;
            let recopy_shortcut = get_shortcut_from_settings(&state.db, "recopy", "Ctrl+Shift+V")?;

            app.global_shortcut()
                .register(record_shortcut)
                .unwrap_or_else(|e| eprintln!("Record hotkey failed: {}", e));
            app.global_shortcut()
                .register(recopy_shortcut)
                .unwrap_or_else(|e| eprintln!("Recopy hotkey failed: {}", e));

            // Tray with recents
            let menu = build_tray_menu(app.handle())?;

            let _tray = TrayIconBuilder::with_id("main_tray")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("OpenFlow - Ready")
                .icon({
                    let bytes = include_bytes!("../icons/icon.png");
                    tauri::image::Image::from_bytes(bytes)?
                })
                .icon_as_template(true)
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref();
                    match id {
                        "show" => {
                            show_main_window(app);
                        }
                        "show_history" => {
                            show_main_window(app);
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = app.emit("navigate", "history");
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        s if s.starts_with("recent:") => {
                            let row_id = &s["recent:".len()..];
                            let state = app.state::<AppState>();
                            if let Ok(Some(t)) = state.db.get_transcription(row_id) {
                                let text = t.formatted_text.as_deref().unwrap_or(&t.raw_text);
                                let _ = paste_to_clipboard(text, insert_method(&state.db));
                                let _ = app.emit("recopy-success", "Copied!");
                            }
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // Make overlay background transparent
            if let Some(overlay) = app.get_webview_window("overlay") {
                let _ = overlay.set_background_color(Some(tauri::utils::config::Color(0, 0, 0, 0)));
            }

            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            set_api_key,
            get_api_key,
            get_setting,
            set_setting,
            get_history,
            search_history,
            delete_transcription,
            clear_history,
            fetch_models,
            list_audio_devices,
            start_recording,
            stop_recording_and_transcribe,
            cancel_current_transcription,
            synthesize_speech,
            stream_speech,
            cancel_speech,
            copy_last_transcription,
            copy_text,
            paste_text,
            rebind_hotkey,
            list_plugins,
            enable_plugin,
            disable_plugin,
            install_plugin,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        #[cfg(target_os = "macos")]
        if matches!(event, tauri::RunEvent::Reopen { .. }) {
            show_main_window(app_handle);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (app_handle, event);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_method_defaults_to_paste() {
        // Anything that is not exactly "type" is Paste, so an unset setting, a
        // stale value, or a typo degrades to the universal path rather than to
        // the one whose behaviour depends on the receiving app.
        assert_eq!(InsertMethod::from_setting(None), InsertMethod::Paste);
        assert_eq!(
            InsertMethod::from_setting(Some(String::new())),
            InsertMethod::Paste
        );
        assert_eq!(
            InsertMethod::from_setting(Some("paste".to_string())),
            InsertMethod::Paste
        );
        assert_eq!(
            InsertMethod::from_setting(Some("typing".to_string())),
            InsertMethod::Paste
        );
        assert_eq!(
            InsertMethod::from_setting(Some("type".to_string())),
            InsertMethod::Type
        );
        assert_eq!(
            InsertMethod::from_setting(Some("  type  ".to_string())),
            InsertMethod::Type
        );
    }

    /// Empty text must not reach CGEvent. `set_string("")` leaves virtual
    /// keycode 0 holding its normal meaning, so the "empty" keystroke types
    /// a literal `a`. Observed, not theorised.
    #[test]
    #[cfg(target_os = "macos")]
    fn typing_empty_text_posts_nothing() {
        assert_eq!(type_text(""), Ok(()));
    }

    #[test]
    fn recording_slot_frees_itself_after_the_watchdog_window() {
        assert!(
            recording_slot_free(&None),
            "no capture means the slot is free"
        );

        let fresh = Some(Instant::now());
        assert!(
            !recording_slot_free(&fresh),
            "a live capture holds the slot"
        );

        // A release event that never arrived must not wedge recording forever.
        let stranded = Some(Instant::now() - MAX_RECORDING - Duration::from_secs(1));
        assert!(
            recording_slot_free(&stranded),
            "a capture older than the watchdog window must free the slot"
        );
    }

    #[test]
    fn speech_key_never_leaks_across_endpoints() {
        let lan = Provider::from_str("custom:http://192.168.1.10:8880/v1");
        let shared = Some("sk-or-hosted".to_string());

        // A self-hosted server gets no key unless it has its own.
        assert_eq!(
            resolve_speech_key(&lan, None, shared.clone(), false),
            Ok(String::new())
        );
        assert_eq!(
            resolve_speech_key(&lan, Some("local".to_string()), shared.clone(), false),
            Ok("local".to_string())
        );
        // The same self-hosted server that transcribes may reuse its key.
        assert_eq!(
            resolve_speech_key(&lan, None, shared.clone(), true),
            Ok("sk-or-hosted".to_string())
        );
        // A hosted voice provider shares the key only with itself.
        assert_eq!(
            resolve_speech_key(&Provider::OpenRouter, None, shared.clone(), true),
            Ok("sk-or-hosted".to_string())
        );
        assert!(resolve_speech_key(&Provider::OpenAI, None, shared.clone(), false).is_err());
        assert!(resolve_speech_key(&Provider::OpenRouter, None, None, true).is_err());
        assert!(
            resolve_speech_key(&Provider::OpenRouter, Some("  ".to_string()), None, true).is_err()
        );
    }

    /// The unit tests above prove `resolve_speech_key` *returns* no key for a
    /// self-hosted endpoint. They do not prove the HTTP request that goes out
    /// carries no key -- a header could still be added downstream, and a LAN
    /// server that accepts any credential would answer 200 either way.
    ///
    /// So stand up a listener, send a real request at it, and read the bytes
    /// that actually crossed the socket.
    #[test]
    fn selfhosted_request_carries_no_cloud_credential() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a throwaway endpoint");
        let port = listener.local_addr().expect("local addr").port();

        let capture = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept the speech request");
            let mut buf = vec![0u8; 8192];
            let read = stream.read(&mut buf).unwrap_or(0);
            // Non-empty: `validate_speech_response` rejects a zero-length body.
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 4\r\n\r\nID3\x04",
            );
            String::from_utf8_lossy(&buf[..read]).into_owned()
        });

        let lan = Provider::from_str(&format!("custom:http://127.0.0.1:{port}/v1"));
        let cloud_key = "sk-or-this-must-never-reach-the-lan";

        // Exactly what the speech command computes before it sends: a shared
        // transcription key exists, but the speech endpoint is a different one.
        let key = resolve_speech_key(&lan, None, Some(cloud_key.to_string()), false)
            .expect("a custom endpoint proceeds unauthenticated");

        let sent = tokio::runtime::Runtime::new()
            .expect("test runtime")
            .block_on(transcribe::request_speech(
                "probe", &key, &lan, "tts-1", "alloy", "mp3",
            ));
        assert!(
            sent.is_ok(),
            "the self-hosted request should go through: {:?}",
            sent.err()
        );

        let raw = capture.join().expect("capture thread");

        // reqwest lowercases header names, so match case-insensitively: the
        // point is that no credential header exists at all, not that one
        // exists carrying an empty value.
        let headers = raw.to_ascii_lowercase();
        assert!(
            !headers.contains("authorization"),
            "a self-hosted endpoint must receive no Authorization header, got:\n{raw}"
        );
        assert!(
            !raw.contains(cloud_key),
            "the transcription key leaked to the LAN, got:\n{raw}"
        );

        // Pin the positive contract too, so a future failure separates "leaked"
        // from "never reached the endpoint".
        assert!(
            raw.starts_with("POST /v1/audio/speech "),
            "expected a speech request, got:\n{raw}"
        );
        assert!(
            raw.contains(r#""model":"tts-1""#) && raw.contains(r#""voice":"alloy""#),
            "expected model and voice in the body, got:\n{raw}"
        );
    }

    #[test]
    fn same_endpoint_compares_custom_urls_and_hosted_variants() {
        let a = Provider::from_str("custom:http://10.0.0.5:8880/v1");
        let b = Provider::from_str("custom:http://10.0.0.5:8880/v1/");
        let c = Provider::from_str("custom:http://10.0.0.9:8880/v1");
        assert!(same_endpoint(&a, &b));
        assert!(!same_endpoint(&a, &c));
        assert!(same_endpoint(&Provider::OpenRouter, &Provider::OpenRouter));
        assert!(!same_endpoint(&Provider::OpenRouter, &Provider::OpenAI));
        assert!(!same_endpoint(&Provider::OpenRouter, &a));
    }

    #[test]
    fn parses_default_record_shortcut() {
        let shortcut = parse_shortcut("Option+V").expect("default record shortcut should parse");
        assert_eq!(shortcut, Shortcut::new(Some(Modifiers::ALT), Code::KeyV));
    }

    #[test]
    fn parses_default_recopy_shortcut_case_insensitively() {
        let shortcut =
            parse_shortcut("ctrl+shift+v").expect("default recopy shortcut should parse");
        assert_eq!(
            shortcut,
            Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV)
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
}
