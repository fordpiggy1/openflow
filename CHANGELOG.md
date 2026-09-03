# Changelog

Newest first. Each entry names the change, the author, and what it touches.

## Unreleased

### OpenFlow for iPhone: local-only dictation, Milestone M1
By: Titan (with Claude)
Impact: `apps/ios/`, `docs/mobile/PLAN.md`, `CHANGELOG.md`

- `apps/ios/Packages/OpenFlowMobileCore` is the whole brain of the phone app in one dependency-free Swift 6 package: it builds and tests with the Command Line Tools alone, which is the M1 gate. 53 tests pass.
- `ModelManager` is the load/unload state machine from PLAN.md section 2, as one actor. It prewarms while the user is still speaking, refuses to prewarm in Low Power Mode or at serious thermal pressure, and unloads on the idle timer, on a memory warning, on heat and 20 s after backgrounding — except that a background unload waits for an in-flight transcription to deliver its text first. Conditions and the clock are injected, so every trigger is tested on a hand-cranked clock instead of waited out.
- `SilenceGate` and `AudioResampler` port `speech_level`, `is_silent`, `auto_gain` and the FIR anti-alias `downsample` from `src-tauri/src/audio.rs` with the same constants and the same test vectors, so the desktop and the phone can be cross-checked. The anti-alias test is paired with one proving that plain decimation fails it.
- `DictionaryPostPass` turns the desktop's 800-character dictionary into a deterministic post-pass, because Qwen ignores prompts. Whole-word match, longest entry first, dictionary casing wins, sentence-initial capitalisation preserved, no chaining — each rule documented and tested. A `heard -> Correct` form catches mishearings a prompt cannot express.
- The app, keyboard and widget targets are real Swift behind a hand-written `project.yml`; no `.xcodeproj` is committed. `FakeEngine` is wired behind `-D OPENFLOW_FAKE_ENGINE` so the whole product runs in the Simulator before any weights exist.
- The keyboard extension has no microphone, no model and no networking code, and reads one small JSON file from the App Group. `RequestsOpenAccess` is on only because iOS otherwise walls it off from that container; the reason is in the plist and in the README.
- `ModelDownloader` is the only file that touches the network, with a pinned URL and SHA-256 marked TODO until M2. A reviewer can check the offline claim with one grep: `grep -rn "URLSession|https://" apps/ios --include='*.swift'` returns that file and nothing else.
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
