mod audio;
mod db;
mod plugins;
mod transcribe;

use audio::{AudioDevice, AudioRecorder};
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

// ── Settings ──────────────────────────────────────────────
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

#[tauri::command]
fn list_audio_devices(state: State<AppState>) -> Result<Vec<AudioDevice>, String> {
    state.recorder.list_devices()
}

// ── Recording ─────────────────────────────────────────────
#[tauri::command]
fn start_recording(state: State<AppState>) -> Result<(), String> {
    let device = state.db.get_setting("microphone");
    state.recorder.start(device)
}

#[tauri::command]
async fn stop_recording_and_transcribe(state: State<'_, AppState>) -> Result<Transcription, String> {
    run_transcription_pipeline(&state).await
}

#[tauri::command]
fn copy_last_transcription(state: State<AppState>) -> Result<(), String> {
    let last = state.last_transcription.lock().unwrap();
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
fn rebind_hotkey(app: AppHandle, state: State<AppState>, action: String, shortcut_str: String) -> Result<(), String> {
    let gs = app.global_shortcut();

    let old_key = format!("hotkey_{}", action);
    if let Some(old_str) = state.db.get_setting(&old_key) {
        if let Ok(old_shortcut) = parse_shortcut(&old_str) {
            let _ = gs.unregister(old_shortcut);
        }
    }

    let new_shortcut = parse_shortcut(&shortcut_str)
        .map_err(|e| format!("Invalid shortcut '{}': {}", shortcut_str, e))?;

    gs.register(new_shortcut)
        .map_err(|e| format!("Failed to register shortcut: {}", e))?;

    state.db.set_setting(&old_key, &shortcut_str)?;
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
async fn run_transcription_pipeline(state: &AppState) -> Result<Transcription, String> {
    let start_time = std::time::Instant::now();
    let wav_bytes = state.recorder.stop()?;

    let transcription_key = state.db.get_setting("api_key")
        .ok_or("No API key set. Add your API key in settings.")?;
    let language = state.db.get_setting("language");
    let provider_str = state.db.get_setting("provider").unwrap_or_else(|| "groq".to_string());
    let transcription_provider = Provider::from_str(&provider_str);
    let format_enabled = state.db.get_setting("format_enabled")
        .map(|v| v != "false").unwrap_or(true);
    let same_provider = state.db.get_setting("same_provider")
        .map(|v| v != "false").unwrap_or(true);
    let stt_model = state.db.get_setting("stt_model");
    let chat_model = state.db.get_setting("chat_model");

    let raw_text = transcribe::transcribe_audio(
        wav_bytes, &transcription_key, language.as_deref(),
        &transcription_provider, stt_model.as_deref(),
    ).await?;

    let formatted = if format_enabled {
        let (fmt_provider, fmt_key) = if same_provider {
            (transcription_provider.clone(), transcription_key.clone())
        } else {
            let fp = state.db.get_setting("formatting_provider").unwrap_or(provider_str.clone());
            let fk = state.db.get_setting("formatting_api_key").unwrap_or(transcription_key.clone());
            (Provider::from_str(&fp), fk)
        };
        transcribe::format_text(&raw_text, &fmt_key, None, &fmt_provider, chat_model.as_deref())
            .await.unwrap_or_else(|_| raw_text.clone())
    } else {
        raw_text.clone()
    };

    let _ = paste_to_clipboard(&formatted);

    let transcription = Transcription {
        id: uuid::Uuid::new_v4().to_string(),
        raw_text,
        formatted_text: Some(formatted.clone()),
        provider: provider_str,
        duration_ms: Some(start_time.elapsed().as_millis() as i64),
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

fn paste_to_clipboard(text: &str) -> Result<(), String> {
    use arboard::Clipboard;
    let mut clipboard = Clipboard::new().map_err(|e| format!("Clipboard init failed: {}", e))?;
    clipboard.set_text(text).map_err(|e| format!("Clipboard set failed: {}", e))?;

    simulate_paste();
    Ok(())
}

#[cfg(target_os = "macos")]
fn simulate_paste() {
    use std::process::Command;
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to keystroke \"v\" using command down")
        .spawn();
}

#[cfg(target_os = "windows")]
fn simulate_paste() {
    use std::process::Command;
    let _ = Command::new("powershell")
        .arg("-Command")
        .arg("Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('^v')")
        .output();
}

#[cfg(target_os = "linux")]
fn simulate_paste() {
    use std::process::Command;
    let _ = Command::new("xdotool").arg("key").arg("ctrl+v").output()
        .or_else(|_| Command::new("ydotool").arg("key").arg("29:1").arg("47:1").arg("47:0").arg("29:0").output());
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
        "a" => Code::KeyA, "b" => Code::KeyB, "c" => Code::KeyC, "d" => Code::KeyD,
        "e" => Code::KeyE, "f" => Code::KeyF, "g" => Code::KeyG, "h" => Code::KeyH,
        "i" => Code::KeyI, "j" => Code::KeyJ, "k" => Code::KeyK, "l" => Code::KeyL,
        "m" => Code::KeyM, "n" => Code::KeyN, "o" => Code::KeyO, "p" => Code::KeyP,
        "q" => Code::KeyQ, "r" => Code::KeyR, "s" => Code::KeyS, "t" => Code::KeyT,
        "u" => Code::KeyU, "v" => Code::KeyV, "w" => Code::KeyW, "x" => Code::KeyX,
        "y" => Code::KeyY, "z" => Code::KeyZ,
        "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2, "3" => Code::Digit3,
        "4" => Code::Digit4, "5" => Code::Digit5, "6" => Code::Digit6, "7" => Code::Digit7,
        "8" => Code::Digit8, "9" => Code::Digit9,
        "f1" => Code::F1, "f2" => Code::F2, "f3" => Code::F3, "f4" => Code::F4,
        "f5" => Code::F5, "f6" => Code::F6, "f7" => Code::F7, "f8" => Code::F8,
        "f9" => Code::F9, "f10" => Code::F10, "f11" => Code::F11, "f12" => Code::F12,
        _ => return Err(format!("Unknown key: {}", key_str)),
    };

    let mods = if modifiers.is_empty() { None } else { Some(modifiers) };
    Ok(Shortcut::new(mods, code))
}

fn get_shortcut_from_settings(db: &Database, action: &str, default: &str) -> Shortcut {
    let key = format!("hotkey_{}", action);
    let shortcut_str = db.get_setting(&key).unwrap_or_else(|| default.to_string());
    parse_shortcut(&shortcut_str).unwrap_or_else(|_| parse_shortcut(default).unwrap())
}

// ── Hotkey handlers ───────────────────────────────────────
fn handle_hotkey_press(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut recording = state.recording.lock().unwrap();
    if !*recording {
        *recording = true;
        let device = state.db.get_setting("microphone");
        if let Err(e) = state.recorder.start(device) {
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
        if !*recording { return; }
        *recording = false;
    }

    let _ = app.emit("recording-state", "transcribing");

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app_handle.state::<AppState>();
        match run_transcription_pipeline(&state).await {
            Ok(transcription) => {
                let _ = app_handle.emit("transcription-result", &transcription);
                update_tray_menu(&app_handle);
            }
            Err(e) => {
                let _ = app_handle.emit("transcription-error", &e);
            }
        }
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

// ── Tray menu with recents ────────────────────────────────
fn build_tray_menu(app: &AppHandle) -> Result<tauri::menu::Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let state = app.state::<AppState>();
    let recents = state.db.get_history(20).unwrap_or_default();

    let mut builder = MenuBuilder::new(app);

    let show = MenuItemBuilder::with_id("show", "Show OpenFlow").build(app)?;
    builder = builder.item(&show);

    if !recents.is_empty() {
        builder = builder.separator();
        let label = MenuItemBuilder::with_id("_label_recents", "Recent Transcriptions")
            .enabled(false).build(app)?;
        builder = builder.item(&label);

        for (i, t) in recents.iter().enumerate() {
            let text = t.formatted_text.as_deref().unwrap_or(&t.raw_text);
            let preview: String = text.chars().take(40).collect();
            let display = if text.len() > 40 {
                format!("{}...", preview)
            } else {
                preview
            };
            let item = MenuItemBuilder::with_id(
                &format!("recent_{}", i),
                &display,
            ).build(app)?;
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

// ── App entry ─────────────────────────────────────────────
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    let state = app.state::<AppState>();
                    let record_shortcut = get_shortcut_from_settings(&state.db, "record", "Option+V");
                    let recopy_shortcut = get_shortcut_from_settings(&state.db, "recopy", "Ctrl+Shift+V");

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

            // Register hotkeys from settings (or defaults)
            let state = app.state::<AppState>();
            let record_shortcut = get_shortcut_from_settings(&state.db, "record", "Option+V");
            let recopy_shortcut = get_shortcut_from_settings(&state.db, "recopy", "Ctrl+Shift+V");

            app.global_shortcut().register(record_shortcut)
                .unwrap_or_else(|e| eprintln!("Record hotkey failed: {}", e));
            app.global_shortcut().register(recopy_shortcut)
                .unwrap_or_else(|e| eprintln!("Recopy hotkey failed: {}", e));

            // Tray with recents
            let menu = build_tray_menu(app.handle())?;

            let _tray = TrayIconBuilder::with_id("main_tray")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("OpenFlow - Ready")
                .icon({
                    let bytes = include_bytes!("../icons/icon.png");
                    tauri::image::Image::from_bytes(bytes).unwrap_or_else(|_| app.default_window_icon().unwrap().clone())
                })
                .icon_as_template(true)
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref();
                    match id {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "show_history" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                                let _ = app.emit("navigate", "history");
                            }
                        }
                        "quit" => { app.exit(0); }
                        s if s.starts_with("recent_") => {
                            if let Ok(idx) = s.strip_prefix("recent_").unwrap_or("").parse::<usize>() {
                                let state = app.state::<AppState>();
                                if let Ok(history) = state.db.get_history(20) {
                                    if let Some(t) = history.get(idx) {
                                        let text = t.formatted_text.as_deref().unwrap_or(&t.raw_text);
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

            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            set_api_key, get_api_key, get_setting, set_setting,
            get_history, search_history,
            fetch_models, list_audio_devices,
            start_recording, stop_recording_and_transcribe,
            copy_last_transcription, copy_text,
            rebind_hotkey,
            list_plugins, enable_plugin, disable_plugin, install_plugin,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
