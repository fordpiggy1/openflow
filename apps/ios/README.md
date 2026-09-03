# OpenFlow for iPhone

A dictation app that runs the recogniser on the phone. No account, no server, no
analytics, no third-party SDK. The only network request it can make is the
one-time model download, and that lives in one file so you can check the claim
rather than believe it.

This directory is Milestone M1 of `docs/mobile/PLAN.md`: the core package with
its tests, the four targets with real Swift in them, and an XcodeGen spec that
generates a project. There is no speech engine yet -- M2 adds that.

## What iOS lets us build, and what it does not

These three constraints shape everything here, and the plan does not try to work
around them with tricks that get apps rejected:

1. **No system-wide insertion.** A third-party app cannot type into another app.
   The desktop's hotkey-hold-paste loop does not exist on the phone. Text reaches
   other apps through the clipboard, the Share sheet, or our own keyboard
   extension.
2. **Keyboard extensions cannot use the microphone**, and live under a memory cap
   measured in tens of megabytes. The model can never run inside the keyboard.
   The keyboard is a one-key "insert my last dictation" surface and nothing more.
3. **Background execution does not keep a 1 GB model resident.** A suspended app
   holding that much memory is the first thing jetsam kills. So "runs in the
   background" means the app loads the model on demand, fast, and drops it when
   the system asks, without losing the user's text.

The resulting interaction: trigger from the Action Button, Back Tap, a Control
Center control, a Lock Screen widget or the app icon; a small capture sheet
appears with the Dynamic Island showing state; speak; tap stop, or let it stop on
silence. The text is copied to the clipboard and can be inserted with one key
from the OpenFlow keyboard in whatever app you switch to. One trigger, one speak,
one paste.

## Layout

```
apps/ios/
  project.yml                 XcodeGen spec -- the four targets, ids, App Group
  OpenFlow/                   the app (SwiftUI, iOS 18+)
  OpenFlowKeyboard/           keyboard extension: one row, reads the App Group
  OpenFlowWidgets/            Live Activity + ControlWidget
  Packages/
    OpenFlowMobileCore/       the brain: state machine, audio maths, stores
    OpenFlowQwenEngine/       M2, MLX Swift. Stub; not in the CLT gate
    OpenFlowWhisperEngine/    M2, WhisperKit. Stub; not in the CLT gate
```

## Build and run

```bash
brew install xcodegen
cd apps/ios
xcodegen generate
open OpenFlow.xcodeproj
```

`OpenFlow.xcodeproj` is a build artefact and is not committed. `project.yml` is
the file to review and to change.

**The plists and entitlements are hand-written, and `project.yml` must never
grow an `info:` or `entitlements:` block.** Those keys do not point XcodeGen at
an existing file; they tell it to write one, and it rewrites that path from the
spec on every `xcodegen generate`. That would silently erase the keyboard's
`NSExtension` dict and `RequestsOpenAccess`, `NSMicrophoneUsageDescription`,
`UIBackgroundModes`, `CFBundleURLTypes`, `NSSupportsLiveActivities` and the App
Group in all three entitlements files. `INFOPLIST_FILE`,
`CODE_SIGN_ENTITLEMENTS` and `GENERATE_INFOPLIST_FILE: NO` under each target's
`settings.base` are what point the build at those files, and they are enough on
their own. If a generate ever wipes them, that is why.

### Running with no model, in the Simulator

The Debug configuration defines `OPENFLOW_FAKE_ENGINE`, which swaps in
`FakeEngine`: it loads instantly, returns a canned line, and reports a simulated
1 GB resident. Every screen, the keyboard, the Live Activity and the App Intent
can be exercised end to end before any weights exist. Nothing about the fake is
subtle -- the text it returns says it is the fake, so a fake build cannot be
mistaken for a working one.

To build without it, use the Release configuration or remove
`OPENFLOW_FAKE_ENGINE` from `SWIFT_ACTIVE_COMPILATION_CONDITIONS` in
`project.yml`. The app then reports that no engine is installed, which is the
truth until M2.

## Tests

The core package builds and tests with the Command Line Tools alone -- no Xcode,
no simulator, no Metal toolchain:

```bash
cd apps/ios/Packages/OpenFlowMobileCore
swift build
swift test
```

That is the gate this milestone was held to. It covers the model manager's
transitions for every trigger in PLAN.md section 2 (driven on a hand-cranked
clock, so a five-minute idle timer costs a microsecond), the silence gate and
resampler against the same vectors as the desktop's Rust tests, the dictionary
post-pass, the transcript store's retention window, the settings defaults and the
model store's checksum verification.

The Xcode gates run on a machine that has Xcode:

```bash
cd apps/ios && xcodegen generate
xcodebuild -scheme OpenFlow -destination 'generic/platform=iOS Simulator' build
```

## The privacy guarantee, and how to check it

The claim: your voice never leaves the phone, and the app talks to exactly one
host, once, to download the recogniser.

How to verify it without trusting the claim:

```bash
# Only ModelDownloader.swift may appear.
grep -rn "URLSession\|https://" apps/ios --include='*.swift'
```

`ModelDownloader` is the only type allowed to touch the network. Its URL and its
SHA-256 are compile-time constants -- there is no manifest fetch, no redirect
chasing, no remote config -- so the host the app can reach is fixed at build
time and visible in the source. A file whose digest does not match is deleted,
not used.

Two more things a reviewer can check:

- `OpenFlow/PrivacyInfo.xcprivacy` declares no collected data types, no tracking
  and no tracking domains, and lists only the required-reason APIs the code
  actually calls.
- `OpenFlow/OpenFlow.entitlements` carries the App Group and nothing else: no
  iCloud, no push, no associated domains.

## The keyboard's Allow Full Access

The keyboard extension's `Info.plist` sets `RequestsOpenAccess` to true, and it
is worth being blunt about why. iOS sandboxes a keyboard extension away from the
App Group container unless the user grants Allow Full Access. Without it the
keyboard cannot read the last transcript and has nothing to insert.

That is the only thing it buys OpenFlow. The keyboard target contains no
networking code at all -- there is nothing in it that could send a keystroke
anywhere -- it keeps no record of what you type, and it reads exactly one small
JSON file that this app wrote. If you would rather not grant it, the app still
works: the transcript is on the clipboard, and paste does the same job in one
more tap.

## Where the desktop and the phone agree

Nothing from the desktop Rust is linked into the phone app. What is shared is the
specification, copied deliberately so the two can be cross-checked:

- `SilenceGate` and `AudioResampler` port `speech_level`, `is_silent`,
  `auto_gain` and the FIR `downsample` from `src-tauri/src/audio.rs`, with the
  same constants and the same test vectors. If a test passes there and fails
  here, the implementations have drifted.
- `DictionaryPostPass.capped` reproduces `dictionary_prompt` from
  `src-tauri/src/transcribe.rs`, including the 800-scalar cap, so the same
  dictionary string means the same thing on both platforms.
- The load/unload policy in `ModelManager` is the contract the desktop's local
  runner adopts as well.

## Status

Milestone M1. No speech engine: `SpeechEngine` has a fake and two stubs, and the
model pin in `ModelDownloader` is a placeholder that refuses to download rather
than installing something the app cannot verify. M2 ports Qwen3-ASR-0.6B to MLX
Swift, wires WhisperKit, measures both on a real iPhone and picks one.
