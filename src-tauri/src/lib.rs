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
    recording: Mutex<bool>,
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
    let api_key = api_key_override
        .or_else(|| state.secrets.get("api_key").ok().flatten())
        .ok_or("No API key available")?;
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
    let api_key = state
        .secrets
        .get("tts_api_key")?
        .or_else(|| state.secrets.get("api_key").ok().flatten())
        .ok_or("No API key set. Add your OpenRouter key in Settings.")?;
    let model = model
        .filter(|value| !value.trim().is_empty())
        .or_else(|| state.db.get_setting("tts_model"))
        .unwrap_or_else(|| transcribe::GEMINI_TTS_MODEL.to_string());
    let voice = voice
        .filter(|value| !value.trim().is_empty())
        .or_else(|| state.db.get_setting("tts_voice"))
        .unwrap_or_else(|| "Kore".to_string());
    let format = response_format
        .filter(|value| !value.trim().is_empty())
        .or_else(|| state.db.get_setting("tts_response_format"))
        .unwrap_or_else(|| "mp3".to_string())
        .to_ascii_lowercase();
    Ok((provider, api_key, model, voice, format))
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
    if *recording {
        return Err("A recording is already active".to_string());
    }
    let device = state.db.get_setting("microphone");
    state.recorder.start(device)?;
    *recording = true;
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
        if !*recording {
            return Err("No recording is active".to_string());
        }
        let result = state.recorder.stop();
        *recording = false;
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
        Some(text) => paste_to_clipboard(text),
        None => Err("No previous transcription".to_string()),
    }
}

#[tauri::command]
fn copy_text(_state: State<AppState>, text: String) -> Result<(), String> {
    paste_to_clipboard(&text)
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

    let transcription_key = state
        .secrets
        .get("api_key")?
        .ok_or("No API key set. Add your API key in settings.")?;
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

    let raw_text = tokio::select! {
        _ = cancellation.cancelled() => return Err("Transcription cancelled".to_string()),
        result = transcribe::transcribe_audio(wav_bytes, &transcription_key, language.as_deref(), &transcription_provider, stt_model.as_deref()) => result?,
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
            let fk = state
                .secrets
                .get("formatting_api_key")?
                .unwrap_or(transcription_key.clone());
            (Provider::from_str(&fp), fk)
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

    state.db.save_transcription(&transcription)?;
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
    )
    .err();

    Ok((transcription, paste_warning))
}

fn paste_to_clipboard(text: &str) -> Result<(), String> {
    use arboard::Clipboard;
    let mut clipboard = Clipboard::new().map_err(|e| format!("Clipboard init failed: {}", e))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("Clipboard set failed: {}", e))?;

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
    if !*recording {
        *recording = true;
        let device = state.db.get_setting("microphone");
        if let Err(e) = state.recorder.start(device) {
            eprintln!("Recording start failed: {}", e);
            *recording = false;
            let _ = app.emit("transcription-error", &e);
            return;
        }
        let _ = app.emit("recording-state", "recording");
    }
}

fn emit_idle_if_quiescent(app: &AppHandle, state: &AppState) {
    let Ok(recording) = state.recording.lock() else {
        return;
    };
    if *recording {
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
        if !*recording {
            return;
        }
        let result = state.recorder.stop();
        *recording = false;
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
        let _ = paste_to_clipboard(text);
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

        for (i, t) in recents.iter().enumerate() {
            let text = t.formatted_text.as_deref().unwrap_or(&t.raw_text);
            let preview: String = text.chars().take(40).collect();
            let display = if text.chars().count() > 40 {
                format!("{}...", preview)
            } else {
                preview
            };
            let item = MenuItemBuilder::with_id(format!("recent_{}", i), &display).build(app)?;
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
                recording: Mutex::new(false),
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
                        s if s.starts_with("recent_") => {
                            if let Ok(idx) =
                                s.strip_prefix("recent_").unwrap_or("").parse::<usize>()
                            {
                                let state = app.state::<AppState>();
                                if let Ok(history) = state.db.get_history(20) {
                                    if let Some(t) = history.get(idx) {
                                        let text =
                                            t.formatted_text.as_deref().unwrap_or(&t.raw_text);
                                        let _ = paste_to_clipboard(text);
                                        let _ = app.emit("recopy-success", "Copied!");
                                    }
                                }
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
