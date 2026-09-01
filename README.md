# OpenFlow

OpenFlow is an open-source desktop dictation app built with Tauri, React, and Rust. Hold a configurable global shortcut, speak, and OpenFlow sends the completed recording to your chosen speech-to-text provider. It can optionally clean up the transcript, copy it to the clipboard, and ask the operating system to paste it into the focused app.

The project is currently an early source build. There are no official pre-built or notarized releases yet.

## What works

- Guided onboarding for Groq, OpenAI, OpenRouter, Deepgram, and custom OpenAI-compatible endpoints
- Configurable transcription and cleanup models with provider model discovery
- Global hold-to-record shortcut (`Option+V` by default) and re-copy shortcut (`Ctrl+Shift+V` by default)
- Searchable local transcription history
- Microphone selection, language hints, light/dark themes, a system tray menu, and a movable status overlay
- Optional LLM cleanup for punctuation, paragraphs, and spoken editing commands
- OpenRouter Gemini 3.1 Flash TTS Preview with selectable voices and cancellable response streaming
- Local executable hooks after transcription and formatting

### Streaming scope

Speech-to-text is not live streaming: OpenFlow records locally, then uploads the finished WAV and waits for a transcript. The Gemini TTS preview progressively appends ordered MP3 chunks and starts playback while the response is still downloading when the system webview supports Media Source Extensions; otherwise it falls back to playback after download.

## Providers

| Provider | Transcription | Text cleanup | TTS preview |
| --- | --- | --- | --- |
| OpenRouter | Whisper models | Chat models | Gemini 3.1 Flash TTS Preview |
| Groq | Whisper models | Chat models | No |
| OpenAI | Whisper-compatible endpoint | Chat models | Not exposed in the current UI |
| Deepgram | Nova models | Use a separate cleanup provider or disable cleanup | No |
| Custom | OpenAI-compatible audio/transcriptions endpoint | OpenAI-compatible chat/completions endpoint | Not exposed in the current UI |

Model availability and billing are controlled by the provider. OpenFlow does not proxy requests or include hosted inference.

## Privacy and permissions

- API credentials are stored using macOS Keychain, Windows DPAPI, or Linux Secret Service. Linux requires an unlocked keyring and the `secret-tool` command.
- Recordings are held in memory for transcription and sent to the provider selected in Settings. Cleanup sends transcript text to the selected cleanup provider. Gemini voice previews send their text to OpenRouter.
- Transcript history is stored locally in an unencrypted SQLite database in the operating system's application-data directory.
- Auto-paste requires operating-system automation/accessibility permission. If permission is denied or a paste helper is unavailable, the transcript should still be available in OpenFlow and on the clipboard.
- Enabled plugins are local executables and are not sandboxed. They receive transcript data over standard input. Only install and enable plugins you trust.

## Build from source

### Prerequisites

- [Node.js](https://nodejs.org/) `^20.19.0` or `>=22.12.0`
- npm `>=10.8`
- A current stable [Rust toolchain](https://rustup.rs/)
- Platform prerequisites from the [Tauri v2 setup guide](https://v2.tauri.app/start/prerequisites/)

Linux additionally needs WebKitGTK 4.1, ALSA development headers, an app-indicator library, and Secret Service tools. On Ubuntu 22.04:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential libasound2-dev libayatana-appindicator3-dev \
  libsecret-tools librsvg2-dev libssl-dev libwebkit2gtk-4.1-dev
```

Clone and launch the development app:

```bash
git clone https://github.com/laisyio/openflow.git
cd openflow
npm ci
npm run tauri:dev
```

The first recording prompts for microphone access. Auto-paste may separately prompt for Accessibility or Automation access on macOS. Grant only the permissions you want OpenFlow to use.

## Development commands

```bash
# Strict TypeScript check and production frontend build
npm run check

# Rust formatting, Clippy, and tests
npm run check:rust

# Dependency advisory gate
npm run audit:dependencies

# Build the platform's desktop bundles
npm run tauri:build
```

CI runs these checks from a clean install and compiles the desktop app on macOS, Windows, and Ubuntu.

## How dictation works

1. Hold the record shortcut and speak.
2. Release it to stop recording.
3. OpenFlow converts the captured audio to a mono 16 kHz WAV.
4. The selected provider transcribes the completed recording.
5. If enabled, the selected chat model cleans up the text.
6. Enabled `after_transcribe` and `after_format` hooks can transform the result.
7. OpenFlow saves the result to local history and copies it to the clipboard.
8. The platform paste helper attempts to paste into the app focused at completion time.

Because network and cleanup latency vary, keep the intended destination focused until processing finishes. Review generated text before using it in commands, code, or other sensitive contexts.

## Plugin hooks

Plugins live under `~/.openflow/plugins/<plugin-id>/`. An enabled plugin may declare `after_transcribe` and/or `after_format` plus a relative executable `entrypoint` in `manifest.json`. OpenFlow passes a JSON payload on standard input and expects the updated payload as JSON on standard output. Hooks run serially with a five-second timeout.

Plugin entrypoints run with the user's operating-system permissions. There is currently no plugin marketplace, signature verification, or sandbox.

## Current limitations

- No live partial transcription or meeting mode
- No context capture from the active window or screen
- Auto-paste depends on platform permissions and helpers, and targets whichever app is focused when processing completes
- Linux auto-paste requires `xdotool` or `ydotool`; Wayland compositor support varies
- Provider APIs can change independently of OpenFlow
- Pre-built signed/notarized installers and automatic updates are not available yet

## License

[MIT](LICENSE)
