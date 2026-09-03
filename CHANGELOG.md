# Changelog

Newest first. Each entry names the change, the author, and what it touches.

## Unreleased

### One main window for the native host, with a sidebar instead of three windows
By: Ford (with Claude)
Impact: `crates/openflow-native/src/ui/{main_window,dictate,card}.rs` (new), `crates/openflow-native/src/ui/{history,plugins,mod}.rs`, `crates/openflow-native/src/{app.rs,tray.rs}`, `README.md`, `docs/native-port/PLAN.md`

- **The native host had no main screen.** The Tauri build has been a single window with five screens since the beginning; the native host reached Settings, History and Plugins as three independent `NSWindow`s from the menu bar, and the one screen the menu bar could never stand in for -- the main one, with the hold-to-record button -- did not exist at all. It does now, and the three windows became pages of it. Settings is still its own window; moving it in is the next change.
- **Dictate** is the web build's main screen, with its copy: the microphone, the three-state prompt, the hold button, the shortcut hints, and the last result as a card that copies it again when clicked. The button drives `hotkey_pressed` / `hotkey_released`, the same pair the global shortcut drives, so holding it is holding the shortcut -- silence gate, live preview and insert method are all decided downstream and cannot drift from the hotkey path.
- Holding needs a button that can express it. `NSButton`'s action fires once, on mouse-up, and its `mouseDown:` runs a tracking loop that does not return until the mouse is released, so the press half is unreachable through target/action. `HoldButton` overrides both halves and does not call super.
- **`NSSplitViewController` with a sidebar item, and no `NSTabView` anywhere.** The sidebar item is what supplies the vibrancy, the full-height layout that runs the sidebar up behind the title bar, and the collapse behaviour; an `NSVisualEffectView` painted by hand gets the translucency and none of the rest. `NSTabView`'s strip rides on the edge of a rectangle, which is the framed look this window exists to stop drawing -- pages swap in and out of a plain container instead, and their contents sit on rounded cards with a margin on every side.
- Cards are drawn, not layered. A `CGColor` handed to `layer.backgroundColor` is resolved once, so a card built in light mode keeps its light fill after the user switches to dark; `drawRect:` reads the semantic colour every time AppKit asks, and AppKit asks on an appearance change.
- Pages are built against the content pane's **measured** size. The window is laid out once before any page exists and each page is handed the rect it actually got, the same way the settings tabs ask `NSTabView` for their content rect rather than assuming one. The page sits inside the pane's safe area, so the first card clears the title bar that `FullSizeContentView` runs the content up behind.
- **Fixed on the way through: History's Clear all did not track the window.** It had a flexible bottom margin and nothing horizontal, so widening the window kept the button at its x while the search field beside it grew -- straight underneath it. The sidebar's column had the mirror of the same bug: fixed width against a resizable pane, so the selection pill was cut square against the divider instead of being inset from it.
- Layout stays on autoresizing masks. The split view controller uses auto layout internally to place its two panes, but that stops at the pane: inside it a page's frame is set by the container and its subviews spring off that, so no view in this crate mixes the two models.
- `has_visible_window` asks three windows now instead of four, and the tray's History and Plugins items open the main window on that page rather than a window of their own. Launch and Dock-reopen land on the main window; until it existed the stand-in was Settings, which opened the app on its own preferences.

### A Dock icon for as long as a window is open
By: Ford (with Claude)
Impact: `crates/openflow-native/src/app.rs`, `crates/openflow-native/src/ui/mod.rs`, `crates/openflow-native/src/ui/{settings,history,onboarding,plugins}.rs`

- The native host was an accessory app with no way out of that: the Tauri build puts OpenFlow in the Dock and the app switcher, and the native build never appeared in either, so a window that lost focus could only be found again through the status item. It now switches to `Regular` while any of its four windows is on screen and back to `Accessory` once they are all closed, which is the shape a menu bar app with real windows normally takes.
- The switch happens in the pair every window already goes through. `present_window` asks for the Dock icon *before* ordering the window front, because an accessory app cannot take focus the way a regular one can and activating first leaves the window up but behind whatever the user was in. A new `dismiss_window` orders out and gives the icon back, and the four `windowShouldClose` handlers call it instead of `orderOut` directly.
- Visibility is asked of the windows rather than counted. They are hidden rather than closed and AppKit can order them out itself, so a counter would drift and strand the Dock icon with nothing behind it.
- The pill and the status item are deliberately not windows for this purpose: an app that jumped into the Dock every time someone spoke would be worse than one that never appeared there at all. `LSUIElement` still decides how the app *launches* -- as an accessory, with no Dock icon until something asks for one.

### Native macOS app: the rest of the UI (Milestone B, part 1)
By: Titan (with Claude)
Impact: `crates/openflow-native/src/ui/*`, `crates/openflow-native/src/{app.rs,tray.rs,menu.rs,trace.rs}`, `crates/openflow-core/src/plugins.rs`, `README.md`

- **Settings did not appear from the menu bar.** The click path was right and the presentation was not: `makeKeyAndOrderFront:` plus the cooperative `NSApplication::activate()` does not bring an `LSUIElement` app forward when the click came from a status item, and if the frontmost app is full screen the window lands on another Space. The window was open and invisible. Every window is now presented the way Tauri's `set_focus` does it: `MoveToActiveSpace | FullScreenAuxiliary`, `orderFrontRegardless`, and `activateIgnoringOtherApps:`. Same fix on the Dock-reopen path.
- **Onboarding window.** The web wizard's flow and copy in five panels: welcome, provider (Groq first with the Recommended badge, self-hosted / LAN last), credentials (secure key field, endpoint URL, and a Test connection that calls `Engine::fetch_models` and fills the model fields), preferences (models, microphone, record shortcut) and a summary. An empty key is valid only for a custom endpoint, a custom endpoint needs a whole URL, and the credentials panel cannot be left until the connection is proven. Shown at first launch in place of Settings, and reopenable from Settings with "Run setup again". Nothing is written until Finish, in the same keys and formats the Settings window writes.
- **History window.** An `NSTableView` of time, text and provider with a search field over `Engine::search_history`, and Copy (clipboard only), Paste, Delete and Clear all behind an `NSAlert`. It reloads on `EngineEvent::HistoryChanged` and keeps whatever search is in the field.
- **Plugins window.** Name, version, hooks, status and description from `engine.plugins()`, an Enable/Disable toggle, Install from a folder through an `NSOpenPanel` that hands the folder's `manifest.json` to the same `install_plugin` the Tauri command calls, and Reveal in Finder.
- **Tray.** The History item is live and a Plugins item joins it. An application main menu goes in too: an accessory app never draws one, but AppKit routes key equivalents through it, so without it there was no Cmd+W on any window and no Cmd+V in the field where a user pastes an API key.
- **`OPENFLOW_TRACE=1`** logs tray click ids and window activations to stderr, silent by default, so a GUI-only launch can be diagnosed with `launchctl setenv`.
- Still Milestone B: the local transcription runner and streaming TTS beyond the settings preview.
### Outcome badge and live preview on the native pill
By: Ford (with Claude)
Impact: `crates/openflow-core/src/engine.rs`, `crates/openflow-core/src/settings.rs`, `crates/openflow-core/src/insert.rs`, `crates/openflow-native/src/overlay.rs`, `crates/openflow-native/src/app.rs`, `overlay.html`, `src-tauri/src/lib.rs`

- Brings the four merged overlay and perf changes into the native host, which branched before them and so drew neither the outcome badge nor a live preview. Milestone A ships both hosts, so the native pill going quiet where the Tauri one speaks would have been a regression on arrival.
- Reading a recording that is still running moved into the engine. `Engine::start_partials` re-transcribes the capture every `PARTIAL_INTERVAL` (800 ms) and emits `EngineEvent::TranscriptionPartial`; both hosts render it and neither owns the timing. The loop sleeps between readings rather than on a schedule, so a reading is never issued while one is outstanding -- the transcriber is serialized, and the take the user waits on at key-up can queue behind at most one. A generation counter, bumped whenever a capture starts or ends, drops a reading that was still in the air when the key came up.
- `PARTIAL_WINDOW` retires previews after 20 s. A reading costs the take it previews, and the cost grows with the square of the recording: about 40 ms of expected delay at key-up for a 10 s dictation against 220 ms for a 38 s one, measured against a LAN 0.6B. The last reading is marked `held` and drawn dimmed, because a preview that has stopped tracking and one whose transcriber has died look alike otherwise.
- `Outcome` is a view concern in `overlay.rs`, not a fifth `RecordingState`: the engine has no "done", and a display detail does not belong in a state machine two hosts share. Driven by the result and error events as `overlay.html` drives it, holding 1.2 s for success and 2.2 s for failure, at the resting width so the pill does not move while it is read. The `idle` that follows a result is ignored while a badge is up; the badge's own timer settles the pill.
- The pill draws text for the first time. `draw_partial` measures the longest suffix that fits, so the words being spoken now stay on screen while the beginning scrolls out, and fades the leading edge with columns of the body colour in place of the CSS mask. The waveform's width is now derived from its bar metrics rather than written twice, since the text starts where the bars end.
- `prewarm_typing` lands in `insert.rs` and runs from `began_capturing`, so both hosts pay the first CGEvent's ~40 ms while the user is still speaking rather than after the transcript arrives. The `live_preview` gate is a `Settings` accessor: unset means on for a self-hosted endpoint and off for a hosted one, since a request every 800 ms multiplies what one dictation bills.
- `transcribe_partial` and `live_preview_enabled` are gone as Tauri commands; the webview overlay listens for `transcription-partial` instead of polling. One implementation, in the crate both hosts share.
- **A reading of a silent window was uploaded with the dictionary prompt.** `snapshot` ran `auto_gain` but not the silence gate `stop` runs, so a pause longer than the interval before the user starts speaking became room tone boosted up to 20x -- and Whisper answers noise by echoing the prompt back, which is the known "Sop, Lark". The gate is now shared: `encode_partial` refuses a window that carried no voice, quiet real speech (10x to 50x above the line) still previews, and the loop treats the refusal as a reading skipped rather than a failure or a held line.
- **The preview bounded its own cost.** `PARTIAL_WINDOW` capped how long readings ran but nothing capped what one reading was allowed to cost, and `live_preview` defaults on for every custom endpoint -- against a LAN 1.7B a 20 s take makes ~2 s readings, so key-up queues behind one for most of a window that has not expired. Each reading is now timed, and the first to run over `PARTIAL_INTERVAL` emits what is on screen as `held` and stops the loop for that capture. The final take is what the user is waiting for.
- **The preview read the keychain 25 times a dictation.** Provider, model, key, language and dictionary are resolved once in `start_partials` and moved into the task. A settings change made during a capture no longer reaches that capture's previews -- they keep what the recording started with, and the take at key-up re-reads settings for itself.
- **A late outcome badge froze the recording pill.** A result or error for the previous take arrives while the next capture is running whenever the key goes down again before a take comes back. `show_outcome` stopped the waveform and snapped the pill to 28 px, and `settle_outcome` restored the width without restarting the animation, so the pill sat there wide and still. The badge is now skipped while recording (the notification fires either way) and the settle restarts the timer for any state that animates. Same guard on `overlay.html`, which had the same hole.
- The `live_preview` accessor now documents what enabling it on a hosted provider costs, for the Settings checkbox that does not exist yet: 25 readings in a 20 s dictation is 75 requests a minute against Groq's 20 RPM, and the 429 lands on the final take rather than a preview; and each reading re-uploads the whole buffer, so previewing 20 s of speech bills about 4 minutes of audio.
- **The pill's rounded corners were inverted.** `rounded_path` walks the rectangle clockwise but appended each corner with the four-argument `appendBezierPathWithArcWithCenter:radius:startAngle:endAngle:`, which is counter-clockwise by default, so an arc from 90 degrees to 0 was drawn the 270-degree way round. Each corner looped back through the body instead of rounding off, and the two loops together hollowed out the last 20 px of the pill -- the shape a smoke run read as inverted corners. All four arcs now use the `clockwise:` variant, keeping the walk that makes `corner_radii` read in visual order. A geometry test asserts with `containsPoint` that the rounded corners are cut away, the square ones are not, the body inboard of each arc is solid, and the path's bounds are the rect it was built from. The mic icon's own arc has increasing angles and was already right.
- **The native app opened no window on launch.** The Tauri build's main window is `"visible": true`, so it always presents something; the native host showed onboarding on a fresh install and otherwise nothing at all, which from the outside is a launch that did not happen. `start` now opens Settings once onboarding is complete. The tray is still the way in and out afterwards.

### Native macOS app: Rust + AppKit, no webview (Milestone A)
By: Titan (with Claude)
Impact: `crates/openflow-native/*`, `crates/openflow-core/src/engine.rs`, `Cargo.toml`, `scripts/bundle-native.sh`, `README.md`

- Milestone A of the native port (`docs/native-port/PLAN.md`). A second host over `openflow-core`: `NSApplication` in accessory mode, a `tray-icon` status item, a borderless `NSPanel` overlay and one `NSTabView` settings window, with no WebKit process tree and no JS bridge on the hotkey path. `cargo build -p openflow-native --release` produces `openflow-native` (the plain name is taken by the Tauri bin until Milestone C retires it, and the bundle installs it as `Contents/MacOS/openflow`); `scripts/bundle-native.sh` assembles and ad hoc signs `OpenFlow.app`.
- The overlay pill is ported from `overlay.html` rather than reinterpreted: 28 px tall, 28/82/72 px wide, the same body colour, the same per-position corner radii, the same ten waveform bars and three pulsing dots, the same eight anchors and drag-to-snap. It animates on one 30 Hz `NSTimer` that exists only while recording or transcribing, and positions against the screen's visible frame so it never sits under the menu bar or behind the Dock.
- The settings window covers every key in the parity checklist, autosaving on change with no Save button, with the three credentials in `NSSecureTextField`s that write to the keychain. First launch with no provider saved opens it on Providers.
- The event sink packages each event and hops to the main queue with `dispatch2`. It never calls back into the engine, because the engine emits with its own locks held; the one event whose delivery matters, a speech chunk, is refused synchronously when no preview is listening, which is what stops a cancelled download.
- The host owns the tokio runtime in a static that outlives the run loop, and `Engine::with_owned_runtime` is gone. An engine that owned its runtime could have a transcription task hold the last `Arc<Engine>` and drop the runtime from one of its own worker threads, which tokio turns into a panic. `EngineEvents` now documents both constraints on an implementation.
- Not yet here at the time of this entry, all Milestone B: the onboarding wizard, the History window, the Plugins window, the local transcription runner, and streaming TTS beyond the settings preview. The tray's History item is present but disabled, and the Tauri app still builds and passes. (The first three landed in the Milestone B entry above.) Two known rough edges are deferred with them: transcripts, warnings and re-copy confirmations land in the status item's tooltip rather than a notification, because this bundle has no notification entitlement; and `rodio` pulls a second `cpal` (0.17) alongside the one capture uses (0.15), so the binary carries two CoreAudio client versions.

### Cargo workspace with a UI-free `openflow-core`
By: Titan (with Claude)
Impact: `Cargo.toml`, `crates/openflow-core/*`, `src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`, `package.json`, `.github/workflows/ci.yml`

- Milestone A, unit A1 of the native port (`docs/native-port/PLAN.md`). The repo is now a workspace, and everything the app does that does not draw a window lives in `crates/openflow-core`: `audio`, `transcribe`, `db`, `secrets` and `plugins` moved with `git mv` so their history follows, joined by new `insert`, `speech`, `hotkey`, `settings` and `engine` modules split out of `lib.rs`.
- `engine.rs` holds what `AppState` held (database, keychain, capture slot and its watchdog, cancellation tokens, plugin manager) and every pipeline body, and reports through an `EngineEvents` sink the host supplies. The Tauri host implements that sink with `app.emit` under today's event names, so `App.tsx` and `overlay.html` are untouched and every command keeps its name, arguments and return type.
- `settings.rs` is the one place that reads a setting: typed accessors whose defaults match what the settings UI shows for an unset key, plus the `is_secret_setting` gate and the plaintext-to-keychain migration.
- `src-tauri` now depends on `openflow-core`, `tauri` and two plugins, and on nothing that touches audio, HTTP or sqlite. `lib.rs` went from 1711 lines to 469.
- No behaviour changed. The watchdog window, the silence gate naming the device, the dictionary prompt, plugin hooks, history save and retention, and the clipboard policy per call site all work as before, and the 36 existing tests moved to the modules that now own them. Seven tests were added for the settings defaults, the hotkey table and the recording-state names.

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
