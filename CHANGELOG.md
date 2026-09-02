# Changelog

Newest first. Each entry names the change, the author, and what it touches.

## Unreleased

### Native macOS app: Rust + AppKit, no webview (Milestone A)
By: Titan (with Claude)
Impact: `crates/openflow-native/*`, `crates/openflow-core/src/engine.rs`, `Cargo.toml`, `scripts/bundle-native.sh`, `README.md`

- Milestone A of the native port (`docs/native-port/PLAN.md`). A second host over `openflow-core`: `NSApplication` in accessory mode, a `tray-icon` status item, a borderless `NSPanel` overlay and one `NSTabView` settings window, with no WebKit process tree and no JS bridge on the hotkey path. `cargo build -p openflow-native --release` produces `openflow-native` (the plain name is taken by the Tauri bin until Milestone C retires it, and the bundle installs it as `Contents/MacOS/openflow`); `scripts/bundle-native.sh` assembles and ad hoc signs `OpenFlow.app`.
- The overlay pill is ported from `overlay.html` rather than reinterpreted: 28 px tall, 28/82/72 px wide, the same body colour, the same per-position corner radii, the same ten waveform bars and three pulsing dots, the same eight anchors and drag-to-snap. It animates on one 30 Hz `NSTimer` that exists only while recording or transcribing, and positions against the screen's visible frame so it never sits under the menu bar or behind the Dock.
- The settings window covers every key in the parity checklist, autosaving on change with no Save button, with the three credentials in `NSSecureTextField`s that write to the keychain. First launch with no provider saved opens it on Providers.
- The event sink packages each event and hops to the main queue with `dispatch2`. It never calls back into the engine, because the engine emits with its own locks held; the one event whose delivery matters, a speech chunk, is refused synchronously when no preview is listening, which is what stops a cancelled download.
- The host owns the tokio runtime in a static that outlives the run loop, and `Engine::with_owned_runtime` is gone. An engine that owned its runtime could have a transcription task hold the last `Arc<Engine>` and drop the runtime from one of its own worker threads, which tokio turns into a panic. `EngineEvents` now documents both constraints on an implementation.
- Not yet here, all Milestone B: the onboarding wizard, the History window, the Plugins window, the local transcription runner, and streaming TTS beyond the settings preview. The tray's History item is present but disabled, and the Tauri app still builds and passes. Two known rough edges are deferred with them: transcripts, warnings and re-copy confirmations land in the status item's tooltip rather than a notification, because this bundle has no notification entitlement; and `rodio` pulls a second `cpal` (0.17) alongside the one capture uses (0.15), so the binary carries two CoreAudio client versions.

### Cargo workspace with a UI-free `openflow-core`
By: Titan (with Claude)
Impact: `Cargo.toml`, `crates/openflow-core/*`, `src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`, `package.json`, `.github/workflows/ci.yml`

- Milestone A, unit A1 of the native port (`docs/native-port/PLAN.md`). The repo is now a workspace, and everything the app does that does not draw a window lives in `crates/openflow-core`: `audio`, `transcribe`, `db`, `secrets` and `plugins` moved with `git mv` so their history follows, joined by new `insert`, `speech`, `hotkey`, `settings` and `engine` modules split out of `lib.rs`.
- `engine.rs` holds what `AppState` held (database, keychain, capture slot and its watchdog, cancellation tokens, plugin manager) and every pipeline body, and reports through an `EngineEvents` sink the host supplies. The Tauri host implements that sink with `app.emit` under today's event names, so `App.tsx` and `overlay.html` are untouched and every command keeps its name, arguments and return type.
- `settings.rs` is the one place that reads a setting: typed accessors whose defaults match what the settings UI shows for an unset key, plus the `is_secret_setting` gate and the plaintext-to-keychain migration.
- `src-tauri` now depends on `openflow-core`, `tauri` and two plugins, and on nothing that touches audio, HTTP or sqlite. `lib.rs` went from 1711 lines to 469.
- No behaviour changed. The watchdog window, the silence gate naming the device, the dictionary prompt, plugin hooks, history save and retention, and the clipboard policy per call site all work as before, and the 36 existing tests moved to the modules that now own them. Seven tests were added for the settings defaults, the hotkey table and the recording-state names.

### Resource sweep: idle footprint and stop-path cost
By: Titan (with Claude)
Impact: `src-tauri/src/audio.rs`, `src-tauri/src/transcribe.rs`, `overlay.html`

- The capture buffer is moved out and dropped after every take. `clear()` kept its capacity, so one long recording pinned up to 230 MB for the life of the app.
- The 95th-percentile level uses a selection instead of a full sort: O(n) rather than O(n log n) on the stop path, where the user is waiting.
- One `reqwest::Client` for the life of the process. Each request used to build its own TLS configuration and connection pool and discard them, forcing a new TCP and TLS handshake every call.
- The overlay pill no longer uses `backdrop-filter`, which re-renders a blur every frame anything moves behind it, for a 28 px element that is on screen from launch to quit.
- Measured idle: every animation in the app is scoped to an active state (boot, recording, transcribing, streaming), and the app plus its four WebKit helpers sit near 0.1% CPU.

### Keep the user's clipboard across dictations
By: Titan (with Claude)
Impact: `src-tauri/src/lib.rs`, `src/App.tsx`, `README.md`

- New `preserve_clipboard` setting, on by default. Under Paste, the previous clipboard contents (text or image) are snapshotted before the transcript is written and put back 500 ms after Cmd+V, and only if the clipboard still holds the transcript, so a copy the user made in between is never clobbered. Under Type, the clipboard is not written at all.
- The re-copy shortcut, tray recents, and the app's own copy buttons keep the transcript on the clipboard, since that is what they are for.
- Files and rich content cannot be captured through the clipboard crate and are left replaced; the setting's help text says so.

### Socket-level test for the credential isolation guarantee
By: Ford
Impact: `src-tauri/src/lib.rs`

- `selfhosted_request_carries_no_cloud_credential` binds a listener, sends a real speech request at it, and asserts on the bytes that crossed the socket: no `authorization` header, no transcription key, and the right path and body. The existing unit tests cover what `resolve_speech_key` returns, not what the request carries, and a LAN server that accepts any credential answers 200 either way — so a leak would be silent.

### Groq as the recommended provider, personal dictionary, Orpheus voice
By: Titan (with Claude)
Impact: `src/App.tsx`, `src-tauri/src/transcribe.rs`, `src-tauri/src/lib.rs`, `README.md`

- Groq is first in onboarding with the Recommended badge; fresh installs default to Groq for transcription, cleanup, and voice.
- Cleanup default on Groq is `openai/gpt-oss-20b` with `reasoning_effort: "low"`. Groq no longer serves Llama to standard accounts, so the old `llama-3.3-70b-versatile` default returned 404.
- New Dictionary setting: names and terms are sent to Whisper as the `prompt` spelling hint on Groq, OpenAI, and custom endpoints. Capped at 800 characters.
- Groq's Orpheus text-to-speech is reachable from the voice dropdown. Orpheus answers only in WAV, so the response format is chosen per provider and WAV providers play back after download rather than mid-stream. Voice model and voice defaults now resolve per provider in one place.
- OpenRouter's backend and frontend chat defaults now agree on `google/gemini-3.1-flash-lite-preview`.

### Post-merge sweep fixes
By: Titan (with Claude)
Impact: `src-tauri/src/transcribe.rs`, `src-tauri/src/lib.rs`, `src/App.tsx`, `src-tauri/icons/icon.ico`, `README.md`

- Windows build: `icon.ico` was a PNG with the wrong extension and failed the resource compiler (RC2175). Replaced with a real six-size ICO.
- Self-hosted endpoints work with no API key end to end. `ensure_api_key` now takes the provider; the pipeline, model fetch, and onboarding form no longer require a key for custom endpoints.
- Credentials never cross endpoints. The transcription key is shared only with the identical service (`same_endpoint`, `resolve_speech_key`); a different hosted provider or a LAN server never receives it.
- The voice preview and the Settings voice section are keyed on the selected voice provider, so the Self-hosted / LAN option is reachable from any transcription provider.
- Blank voice model and voice fields reach the backend blank instead of being replaced with Gemini names.
- Setup counts as complete once a provider is saved, even with no key.
- Whole-take silence gate: a recording whose loud part sits under -60 dBFS is refused with an error naming the input device, instead of being uploaded. Whisper hallucinates on silence and, with a dictionary prompt, echoes the prompt back. Found dogfooding with a virtual "Find My" device as the system default input.

### Audit follow-ups and self-hosted speech endpoints
By: Ford
Impact: `src-tauri/src/audio.rs`, `src-tauri/src/db.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/transcribe.rs`, `src/App.tsx`, `src-tauri/Cargo.toml`, `src-tauri/Info.plist`

- Anti-alias low-pass before decimating in `downsample`; gain keyed on the 95th percentile instead of the peak.
- Recording watchdog so a lost hotkey release times out; tray recents keyed by row id.
- History privacy: per-entry delete, clear all, save toggle, retention window.
- `reqwest` unified on 0.13 with rustls; `LSUIElement` and `NSLocalNetworkUsageDescription` added.
- Self-hosted speech endpoints on the LAN, with no Authorization header when there is no key.
