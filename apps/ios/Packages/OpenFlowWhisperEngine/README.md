# OpenFlowWhisperEngine

`SpeechEngine` on WhisperKit (CoreML, Neural Engine). Milestone M2, and the
fallback that ships first if the Qwen port is not sound.

## Why it exists

PLAN.md section 3 keeps two engines behind one protocol on purpose. WhisperKit is
lighter on battery than a GPU decoder, has an on-device large-v3-turbo, honours
the Whisper-style prompt (so the dictionary can go back to being a prompt on this
engine rather than a post-pass), and is proven in shipping iOS apps. If the Qwen
port is not sound by the end of M2, or the phone measurement is worse than
WhisperKit's, this one ships first and Qwen becomes the "fast" option later.

## Why it is a stub today

WhisperKit compiles CoreML models, which needs Xcode. Milestone M1 was built on a
machine with only the Command Line Tools, so this package is kept out of the gate
and out of the core package's dependency list.

## What M2 has to do

1. Add the WhisperKit dependency and attach it to the target.
2. Load large-v3-turbo from `ModelStore` and report `residentBytes`.
3. Pass `SettingsStore.dictionary` through as the prompt. `DictionaryPostPass`
   can still run after it; the two are not exclusive, and the post-pass catches
   what the prompt misses.
4. Fail with `SpeechEngineError.acceleratorUnavailable` rather than dropping to
   the CPU.
5. Measure against Qwen on the same phone and the same clips, and record both
   numbers in `docs/mobile/PLAN.md` section 3.
