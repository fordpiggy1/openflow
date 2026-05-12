# OpenFlow

Free, open-source voice transcription for your desktop. Hold a hotkey, speak, and formatted text appears at your cursor.

Built with [Tauri](https://tauri.app) (~15MB binary, not Electron's 200MB+).

## Features

- **Global hotkey** (Ctrl+Shift+Space) -- hold to record, release to transcribe from any app
- **Smart formatting** -- LLM cleans up punctuation, paragraphs, and tone
- **100+ languages** -- powered by Whisper, auto-detect or set a preferred language
- **Transcription history** -- searchable, persistent across sessions
- **Re-copy hotkey** (Ctrl+Shift+V) -- paste last transcription again without opening the app
- **System tray** -- always running, out of the way
- **Dark and light mode** -- follows your preference
- **BYO API key** -- no subscription, no vendor lock-in, your key stays in your OS keychain

## How it works

```
Hold hotkey -> Record audio -> Groq Whisper transcribes -> LLM formats -> Clipboard + paste
```

Audio is captured at your device's native sample rate, downsampled to 16kHz WAV, and sent to the Groq API for transcription. An optional LLM pass adds punctuation and formatting. The result is copied to your clipboard.

## Install

### Prerequisites

- [Rust](https://rustup.rs/) (1.88+)
- [Node.js](https://nodejs.org/) (18+)

### From source

```bash
git clone https://github.com/laisyio/openflow.git
cd openflow
npm install
cargo tauri dev
```

### Pre-built binaries

Coming soon. Check [Releases](https://github.com/laisyio/openflow/releases).

## Setup

1. Get a free API key from [console.groq.com/keys](https://console.groq.com/keys)
2. Launch OpenFlow
3. Paste your API key in the onboarding screen
4. Hold **Ctrl+Shift+Space** and start talking

## Tech stack

| Layer | Technology |
|-------|-----------|
| Desktop shell | Tauri v2 (Rust) |
| Frontend | React 19 + TypeScript |
| Audio capture | cpal (Rust) |
| Speech-to-text | Groq Whisper Large v3 Turbo |
| Formatting | Groq Llama 3.3 70B |
| Storage | SQLite |
| Secrets | OS keychain |

## Keyboard shortcuts

| Shortcut | Action |
|----------|--------|
| Ctrl+Shift+Space | Hold to record, release to transcribe |
| Ctrl+Shift+V | Re-copy last transcription to clipboard |

## Roadmap

- [ ] Multi-provider support (OpenAI, Deepgram, local Whisper)
- [ ] Plugin architecture (voice-to-Obsidian, voice-to-Notion, etc.)
- [ ] Real-time streaming transcription
- [ ] Context-aware formatting (screenshots active window for better formatting)
- [ ] Meeting aftermath mode

## Alternatives

| App | License | Binary size | Platform |
|-----|---------|-------------|----------|
| **OpenFlow** | MIT | ~15MB | Mac/Win/Linux |
| WisprFlow | Proprietary ($8-10/mo) | ~200MB | Mac/Win |
| OpenWhispr | MIT | ~200MB | Mac/Win/Linux |
| FreeFlow | Open source | Native | Mac only |
| VoiceInk | Open source ($25) | Native | Mac only |

## Contributing

PRs welcome. Please open an issue first for anything beyond a small fix.

## License

MIT
