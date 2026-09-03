# OpenFlow native port: Rust + AppKit, no webview

Status: plan v1, 2026-09-02. Author: Titan (with Claude). Base commit: a464398 (feat/preserve-clipboard, which contains PR #6 and PR #7).

## 0. Why, and what "done" means

The current app is Rust for everything that matters (capture, network, insert, storage) and a WKWebView for the UI. The port keeps the Rust and replaces the WebKit process tree with AppKit windows. Expected effect, from the measurement in PR #7: the four WebKit helper processes disappear (roughly 50 to 70 MB of the 79 to 101 MB idle RSS), the 28 px overlay becomes a native layer, and there is no JS bridge on the hotkey path. There is no transcription-latency gain from this port; that comes from the local runner (section 7).

Done means: a user can install `OpenFlow.app` built from `crates/openflow-native`, configure it in a native Settings window, hold the hotkey, speak, release, and have the text inserted, with the tray, overlay pill, clipboard preservation, silence gate, dictionary, and keychain secrets all behaving exactly as the Tauri build does today. The Tauri app keeps building and passing throughout; it is deleted only in Milestone C.

## 1. Layout after the port

```
Cargo.toml                       workspace: crates/openflow-core, crates/openflow-native, src-tauri
crates/openflow-core/            UI-free library. Everything below moves here unchanged unless noted.
  src/audio.rs                   from src-tauri/src/audio.rs
  src/transcribe.rs              from src-tauri/src/transcribe.rs
  src/db.rs                      from src-tauri/src/db.rs
  src/secrets.rs                 from src-tauri/src/secrets.rs
  src/plugins.rs                 from src-tauri/src/plugins.rs
  src/settings.rs                NEW: typed accessors over db settings (keys listed in section 4)
  src/insert.rs                  from lib.rs: InsertMethod, ClipboardPolicy, ClipboardSnapshot,
                                 paste_to_clipboard, schedule_clipboard_restore, clipboard_still_ours,
                                 type_text, simulate_paste
  src/speech.rs                  from lib.rs: speech_settings, same_endpoint, resolve_speech_key,
                                 Speech* structs, synthesize/stream helpers minus the emit calls
  src/hotkey.rs                  from lib.rs: parse_shortcut and the hotkey action/default tables,
                                 re-expressed on the `global-hotkey` crate's HotKey type
  src/engine.rs                  NEW: the pipeline as a UI-free state machine (section 3)
crates/openflow-native/          AppKit binary, macOS only
  src/main.rs                    NSApplication bootstrap, delegate, run loop
  src/events.rs                  EngineEvents impl that hops to the main thread
  src/hotkeys.rs                 global-hotkey registration + press/release dispatch
  src/tray.rs                    tray-icon + muda menu, recents, open windows, quit
  src/overlay.rs                 NSPanel pill with a custom NSView (section 5)
  src/tts_player.rs              rodio playback for the voice preview
  src/ui/settings.rs             native Settings window (section 6)
  src/ui/onboarding.rs           Milestone B
  src/ui/history.rs              Milestone B
  src/ui/plugins.rs              Milestone B
  Info.plist, entitlements.plist copied from src-tauri (same bundle id io.laisy.openflow)
scripts/bundle-native.sh         assembles OpenFlow.app, codesigns ad hoc, optional DMG
src-tauri/                       thin glue over openflow-core; commands and emits stay, bodies shrink
```

Rules for the move: `git mv` the five reusable modules so history follows them; tests move with their module; no behavior change inside a moved file except the `use` paths. Where lib.rs code is split into core, the Tauri command keeps its signature and calls the core function.

## 2. Crates and why

| Need | Crate | Note |
|---|---|---|
| AppKit | objc2 0.6, objc2-app-kit 0.3, objc2-foundation 0.3 | already in Cargo.lock via tao/wry, so nothing new compiles that did not before. Use `define_class!` for the overlay view and the app delegate. |
| Main-thread hops | dispatch2 | `DispatchQueue::main().exec_async`. No polling timers anywhere; idle CPU must stay at the PR #7 floor. |
| Global hotkeys | global-hotkey 0.8 | the crate under tauri-plugin-global-shortcut. `HotKeyState::Pressed` and `Released` give hold-to-talk. Register on the main thread after NSApplication is up. |
| Tray | tray-icon 0.24 + muda 0.19 | the crates under Tauri's tray. Use `set_event_handler`, hop to main. |
| Audio out | rodio 0.22 (features: wav, mp3 via symphonia) | replaces MediaSource. WAV providers (Groq Orpheus) play after download; mp3 providers stream through a `rodio::Decoder` over a channel-backed reader. |
| Everything else | cpal, hound, reqwest, arboard, rusqlite, tokio, core-graphics | unchanged |

Not used: cacao (beta, unmaintained), tauri anything inside the native crate.

## 3. Engine contract (core, UI-free)

```rust
pub enum EngineEvent {
    RecordingState(RecordingState),            // Idle | Recording | Transcribing | Formatting
    TranscriptionResult(Transcription),        // the db row, unchanged
    TranscriptionWarning(String),
    TranscriptionError(String),
    RecopySuccess(String),
    HistoryChanged,                             // the recents list is stale
    TtsStarted(SpeechStarted), TtsChunk(SpeechChunk),
    TtsFinished(SpeechResult), TtsError(SpeechError),
    Navigate(String),                           // tray "Settings", "History" etc.
}
pub trait EngineEvents: Send + Sync + 'static { fn emit(&self, event: EngineEvent) -> Result<(), String>; }

pub struct Engine { /* db, secrets, capture slot, watchdog, cancel tokens, plugin manager, spawner */ }
impl Engine {
    pub fn new(app_dir: PathBuf, sink: Arc<dyn EngineEvents>, spawn: Spawner) -> Result<Arc<Self>, String>;
    pub fn start_recording(&self) -> Result<(), String>;
    pub fn stop_and_transcribe(self: &Arc<Self>);       // spawns on the tokio runtime, emits events
    pub fn cancel_transcription(&self) -> bool;
    pub fn recopy(&self);
    pub fn paste_text(&self, text: &str) -> Result<(), String>;
    pub fn copy_text(&self, text: &str) -> Result<(), String>;
    pub fn preview_speech(&self, text: &str) -> Result<String /*request id*/, String>;
    pub fn cancel_speech(&self, id: Option<&str>);
    pub fn settings(&self) -> &Settings;  pub fn history(&self) -> ...;  pub fn plugins(&self) -> ...;
    pub fn list_audio_devices(&self) -> Result<Vec<AudioDevice>, String>;
    pub fn fetch_models(&self, provider, key, base_url) -> impl Future<...>;
}
```

The payloads are the structs the app already had (`Transcription`, `SpeechStarted`, `SpeechChunk`, `SpeechResult`, `SpeechError`), byte-identical on the wire, which is what let A1 move the pipeline without touching the frontend. `emit` returns a `Result` because the speech stream stops when a chunk cannot be delivered; every other emit ignores it. Two rules bind any implementation: it runs on whichever thread finished the work, and it can run while the engine holds its own locks, so it must never call back into the engine synchronously. The *host* owns the tokio runtime and hands the engine a spawner over its handle; an engine that owned the runtime could have a task drop it from one of its own worker threads.

The Tauri glue implements `EngineEvents` with `app.emit(name, payload)` using today's event names so `App.tsx` and `overlay.html` need no changes. The native crate implements it by hopping to the main thread and updating the overlay, tray, and open windows directly. The existing pipeline semantics move verbatim: watchdog, silence gate error naming the device, dictionary prompt, formatting fallback, plugin hooks, history save and retention, clipboard policy per call site (pipeline and paste use the setting, recopy/tray/copy keep).

## 4. Parity checklist (the reviewer checks every line)

Settings keys, all read through `settings.rs`, defaults identical to `App.tsx`:
`provider, api_key (keychain), custom base url, models (transcription model), format_enabled, formatting_provider, formatting_api_key (keychain), formatting model, tts_enabled, tts_provider, tts_api_key (keychain), tts_model, tts_voice, tts_response_format, microphone, hotkey_record, hotkey_recopy, dictionary (800 chars), preserve_clipboard (default true), insert_method (paste|type), overlay_only_while_recording, overlay_position, save_history, history_retention_days, theme, onboarding complete`.
Secret keys never touch the settings table; `is_secret_setting` gate moves to core.

Behaviors: hold-to-talk on `hotkey_record` (press starts, release stops), recopy hotkey, tray menu (status line, recents by row id, Settings, History, Plugins, Quit), overlay states idle/recording/transcribing with positions from `overlay.html` (left-center default, bottom-left/center/right and the others the file defines; drag to reposition persists `overlay_position`), `overlay_only_while_recording` hides the pill when idle, silence gate, credential isolation (`same_endpoint`), no Authorization header on keyless custom endpoints (the socket test moves to core), Type never writes the clipboard, Restore only when clipboard still ours, LSUIElement (no Dock icon) with Settings opened from the tray, single instance.

## 5. Overlay pill

`NSPanel` with `NSWindowStyleMask::Borderless | NonactivatingPanel`, level `NSStatusWindowLevel`, `hasShadow = false`, `opaque = false`, `backgroundColor = clear`, `collectionBehavior = CanJoinAllSpaces | Stationary | FullScreenAuxiliary`, `ignoresMouseEvents = false` (drag to reposition, same as today). Content view is a `define_class!` NSView subclass whose `drawRect:` paints the rounded pill (`rgba(26,19,50,0.94)`, same geometry and radii as `overlay.html` per position) and, while recording, the ten waveform bars, while transcribing, the three dots. Animation uses one `NSTimer` at 30 Hz that exists only while state is Recording or Transcribing and is invalidated on Idle. Width transitions match the HTML (28 px idle, 82 px recording, 72 px transcribing) via `NSAnimationContext`. `wantsLayer = true` so the redraw is a layer composite, not a window repaint.

## 6. Settings window (Milestone A scope)

One `NSWindow` (titled, closable, 420x560, hidden on close, not released) with an `NSTabView`: General, Providers, Voice, Privacy. Controls are stock AppKit: `NSPopUpButton` for provider/model/microphone/insert method/theme, `NSSecureTextField` for keys, `NSTextField` for base URL and models, `NSTextView` for dictionary with a live counter, `NSSwitch` for toggles, a hotkey recorder field that captures the next key chord and stores the same string format `parse_shortcut` accepts. A "Fetch models" button calls `Engine::fetch_models` and fills the popup. Every control writes through `settings.rs` on change (no Save button, matching the current autosave). Voice tab has a Preview button that plays through `tts_player`. Onboarding, History, and Plugins windows are Milestone B; in Milestone A the tray shows Settings only, and first launch with no provider saved opens Settings on the Providers tab.

## 7. Local runner (design now, build in Milestone B)

New setting `transcription_backend = remote | local` plus `local_only = true|false`.

- `LocalRunner` (core) supervises a child process that serves OpenAI-compatible `POST /v1/audio/transcriptions` on `127.0.0.1:<free port>`, restarts it on crash with backoff, and exposes readiness. The engine choice is decided by the benchmark in `docs/native-port/local-runner-benchmark.md`: an MLX Qwen3-ASR sidecar (needs a Python runtime, best accuracy on the LAN numbers we have) or whisper.cpp through `whisper-rs` (in-process, no runtime, Metal). The abstraction is the same either way.
- When `transcription_backend = local`, the transcription provider is forced to `Custom("http://127.0.0.1:<port>/v1")` with no key.
- When `local_only = true`, `transcribe::client()` is built with a request guard that rejects any URL whose host is not loopback. This is the invariant the toggle promises ("no requests leave this machine") and it must have a unit test that proves a hosted URL is refused while a loopback URL passes. Formatting and TTS are disabled in the UI while local_only is on unless their provider is also loopback.

## 8. Milestones and gates

**Milestone A (this unit):** sections 1 to 6. Deliverable: `cargo build -p openflow-native --release` produces a binary; `scripts/bundle-native.sh` produces `OpenFlow.app`; the Tauri app still builds and its 36 tests plus the moved tests pass from the workspace root.

**Milestone B:** onboarding wizard, History window (list, search, delete, clear), Plugins window (list, enable, disable, install from folder), local runner (section 7), streaming TTS.

**Milestone C:** retire `src-tauri`, `src/`, Vite and npm from CI; native build job on `macos-latest`; DMG in release workflow; stable local signing identity so TCC grants survive rebuilds.

Gates for every milestone, run from the repo root:
```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
npm run check                         (until Milestone C)
npm run tauri:build -- --no-bundle    (until Milestone C)
```
CHANGELOG.md entry under Unreleased with By: and Impact: lines. Commit trailers per the repository's attribution rule. No API keys or secrets in any file, ever; `grep -r gsk_ .` must return nothing.

## 9. What the implementer must not do

Do not launch the GUI binary from a shell (macOS TCC binds the grant to the shell host; smoke runs happen with Titan through `open -a`). Do not change any moved file's behavior in the same commit as the move. Do not add polling timers, busy loops, or a webview of any kind. Do not touch `src/App.tsx` or `overlay.html` in Milestone A. Do not store secrets anywhere but `secrets.rs`.
