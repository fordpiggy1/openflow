mod audio;
mod db;
mod plugins;
mod transcribe;

use audio::AudioRecorder;
use db::{Database, Transcription};
use plugins::{PluginInfo, PluginManager};
use std::sync::Mutex;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use transcribe::{ModelInfo, Provider};

struct AppState {
    recorder: AudioRecorder,
    db: Database,
    plugin_manager: PluginManager,
    recording: Mutex<bool>,
    last_transcription: Mutex<Option<String>>,
}

// Settings commands
#[tauri::command]
fn set_api_key(state: State<AppState>, key: String) -> Result<(), String> {
    state.db.set_setting("api_key", &key)
}

#[tauri::command]
fn get_api_key(state: State<AppState>) -> Option<String> {
    state.db.get_setting("api_key")
}

#[tauri::command]
fn get_setting(state: State<AppState>, key: String) -> Option<String> {
    state.db.get_setting(&key)
}

#[tauri::command]
fn set_setting(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    state.db.set_setting(&key, &value)
}

// History commands
#[tauri::command]
fn get_history(state: State<AppState>, limit: Option<usize>) -> Result<Vec<Transcription>, String> {
    state.db.get_history(limit.unwrap_or(50))
}

#[tauri::command]
fn search_history(state: State<AppState>, query: String) -> Result<Vec<Transcription>, String> {
    state.db.search_history(&query, 50)
}

// Model fetching
#[tauri::command]
async fn fetch_models(state: State<'_, AppState>, provider_name: Option<String>, api_key_override: Option<String>) -> Result<Vec<ModelInfo>, String> {
    let provider_str = provider_name
        .or_else(|| state.db.get_setting("provider"))
        .unwrap_or_else(|| "groq".to_string());
    let provider = Provider::from_str(&provider_str);

    let api_key = api_key_override
        .or_else(|| state.db.get_setting("api_key"))
        .ok_or("No API key available")?;

    transcribe::fetch_models(&api_key, &provider).await
}

// Recording commands
#[tauri::command]
fn start_recording(state: State<AppState>) -> Result<(), String> {
    state.recorder.start()
}

#[tauri::command]
async fn stop_recording_and_transcribe(state: State<'_, AppState>) -> Result<Transcription, String> {
    let start_time = std::time::Instant::now();
    let wav_bytes = state.recorder.stop()?;

    let api_key = state.db.get_setting("api_key")
        .ok_or("No API key set. Add your Groq API key in settings.")?;
    let language = state.db.get_setting("language");
    let provider_str = state.db.get_setting("provider").unwrap_or_else(|| "groq".to_string());
    let provider = Provider::from_str(&provider_str);
    let format_enabled = state.db.get_setting("format_enabled")
        .map(|v| v != "false")
        .unwrap_or(true);

    let stt_model = state.db.get_setting("stt_model");
    let chat_model = state.db.get_setting("chat_model");

    let raw_text = transcribe::transcribe_audio(wav_bytes, &api_key, language.as_deref(), &provider, stt_model.as_deref()).await?;

    let formatted = if format_enabled {
        match transcribe::format_text(&raw_text, &api_key, None, &provider, chat_model.as_deref()).await {
            Ok(text) => text,
            Err(_) => raw_text.clone(),
        }
    } else {
        raw_text.clone()
    };

    if let Err(e) = paste_to_clipboard(&formatted) {
        eprintln!("Clipboard error: {}", e);
    }

    let duration_ms = start_time.elapsed().as_millis() as i64;
    let transcription = Transcription {
        id: uuid::Uuid::new_v4().to_string(),
        raw_text,
        formatted_text: Some(formatted.clone()),
        provider: provider_str,
        duration_ms: Some(duration_ms),
        context_type: None,
        window_title: None,
        language,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let _ = state.db.save_transcription(&transcription);

    {
        let mut last = state.last_transcription.lock().unwrap();
        *last = Some(formatted);
    }

    Ok(transcription)
}

#[tauri::command]
fn copy_last_transcription(state: State<AppState>) -> Result<(), String> {
    let last = state.last_transcription.lock().unwrap();
    match last.as_ref() {
        Some(text) => paste_to_clipboard(text),
        None => Err("No previous transcription".to_string()),
    }
}

// Plugin commands
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

fn paste_to_clipboard(text: &str) -> Result<(), String> {
    use arboard::Clipboard;
    let mut clipboard = Clipboard::new().map_err(|e| format!("Clipboard init failed: {}", e))?;
    clipboard.set_text(text).map_err(|e| format!("Clipboard set failed: {}", e))?;
    Ok(())
}

fn handle_hotkey_press(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut recording = state.recording.lock().unwrap();
    if !*recording {
        *recording = true;
        if let Err(e) = state.recorder.start() {
            eprintln!("Recording start failed: {}", e);
            *recording = false;
            return;
        }
        let _ = app.emit("recording-state", "recording");
    }
}

fn handle_hotkey_release(app: &AppHandle) {
    let state = app.state::<AppState>();
    {
        let mut recording = state.recording.lock().unwrap();
        if !*recording {
            return;
        }
        *recording = false;
    }

    let _ = app.emit("recording-state", "transcribing");

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app_handle.state::<AppState>();
        let start_time = std::time::Instant::now();

        let wav_bytes = match state.recorder.stop() {
            Ok(b) => b,
            Err(e) => {
                let _ = app_handle.emit("transcription-error", &e);
                let _ = app_handle.emit("recording-state", "idle");
                return;
            }
        };

        let api_key = match state.db.get_setting("api_key") {
            Some(k) => k,
            None => {
                let _ = app_handle.emit("transcription-error", "No API key set");
                let _ = app_handle.emit("recording-state", "idle");
                return;
            }
        };

        let language = state.db.get_setting("language");
        let provider_str = state.db.get_setting("provider").unwrap_or_else(|| "groq".to_string());
        let provider = Provider::from_str(&provider_str);
        let format_enabled = state.db.get_setting("format_enabled")
            .map(|v| v != "false")
            .unwrap_or(true);
        let stt_model = state.db.get_setting("stt_model");
        let chat_model = state.db.get_setting("chat_model");

        let raw_text = match transcribe::transcribe_audio(wav_bytes, &api_key, language.as_deref(), &provider, stt_model.as_deref()).await {
            Ok(t) => t,
            Err(e) => {
                let _ = app_handle.emit("transcription-error", &e);
                let _ = app_handle.emit("recording-state", "idle");
                return;
            }
        };

        let formatted = if format_enabled {
            match transcribe::format_text(&raw_text, &api_key, None, &provider, chat_model.as_deref()).await {
                Ok(text) => text,
                Err(_) => raw_text.clone(),
            }
        } else {
            raw_text.clone()
        };

        let _ = paste_to_clipboard(&formatted);

        let duration_ms = start_time.elapsed().as_millis() as i64;
        let transcription = Transcription {
            id: uuid::Uuid::new_v4().to_string(),
            raw_text,
            formatted_text: Some(formatted.clone()),
            provider: provider_str,
            duration_ms: Some(duration_ms),
            context_type: None,
            window_title: None,
            language,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let _ = state.db.save_transcription(&transcription);
        {
            let mut last = state.last_transcription.lock().unwrap();
            *last = Some(formatted);
        }

        let _ = app_handle.emit("transcription-result", &transcription);
        let _ = app_handle.emit("recording-state", "idle");
    });
}

fn handle_recopy(app: &AppHandle) {
    let state = app.state::<AppState>();
    let last = state.last_transcription.lock().unwrap();
    if let Some(text) = last.as_ref() {
        let _ = paste_to_clipboard(text);
        let _ = app.emit("recopy-success", "Copied last transcription");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    let record_shortcut = Shortcut::new(
                        Some(Modifiers::CONTROL | Modifiers::SHIFT),
                        Code::Space,
                    );
                    let recopy_shortcut = Shortcut::new(
                        Some(Modifiers::CONTROL | Modifiers::SHIFT),
                        Code::KeyV,
                    );

                    if shortcut == &record_shortcut {
                        match event.state() {
                            ShortcutState::Pressed => handle_hotkey_press(app),
                            ShortcutState::Released => handle_hotkey_release(app),
                        }
                    } else if shortcut == &recopy_shortcut {
                        if matches!(event.state(), ShortcutState::Pressed) {
                            handle_recopy(app);
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            let app_dir = app.path().app_data_dir()
                .map_err(|e| format!("No app dir: {}", e))?;
            let db = Database::new(app_dir)
                .map_err(|e| format!("Database init failed: {}", e))?;

            app.manage(AppState {
                recorder: AudioRecorder::new(),
                db,
                plugin_manager: PluginManager::new(),
                recording: Mutex::new(false),
                last_transcription: Mutex::new(None),
            });

            let record_shortcut = Shortcut::new(
                Some(Modifiers::CONTROL | Modifiers::SHIFT),
                Code::Space,
            );
            let recopy_shortcut = Shortcut::new(
                Some(Modifiers::CONTROL | Modifiers::SHIFT),
                Code::KeyV,
            );
            app.global_shortcut().register(record_shortcut)
                .unwrap_or_else(|e| eprintln!("Record hotkey failed: {}", e));
            app.global_shortcut().register(recopy_shortcut)
                .unwrap_or_else(|e| eprintln!("Recopy hotkey failed: {}", e));

            let show = MenuItemBuilder::with_id("show", "Show OpenFlow").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("OpenFlow - Ready")
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => { app.exit(0); }
                    _ => {}
                })
                .build(app)?;

            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            set_api_key,
            get_api_key,
            get_setting,
            set_setting,
            get_history,
            search_history,
            fetch_models,
            start_recording,
            stop_recording_and_transcribe,
            copy_last_transcription,
            list_plugins,
            enable_plugin,
            disable_plugin,
            install_plugin,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
