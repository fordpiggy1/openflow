# Changelog

Newest first. Each entry names the change, the author, and what it touches.

## Unreleased

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
