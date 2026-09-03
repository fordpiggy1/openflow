# OpenFlowQwenEngine

`SpeechEngine` on MLX Swift, running Qwen3-ASR-0.6B at 8-bit. Milestone M2.

## Why it is a stub today

MLX Swift compiles Metal kernels, and the Metal toolchain ships with Xcode, not
with the Command Line Tools. Milestone M1 was built on a machine that has only
the Command Line Tools, and its gate is `swift build && swift test` inside
`Packages/OpenFlowMobileCore`. So this package exists, conforms to the protocol,
and is deliberately kept out of that gate and out of the core package's
dependency list. Nothing in the app links it yet.

## What M2 has to do

1. Add the MLX Swift dependency to `Package.swift` and attach it to the target.
2. Port the audio encoder and wire the Qwen3 decoder (`mlx-swift-examples` has
   the decoder; the encoder is the new work).
3. Load the weights from `ModelStore`, memory-mapped, so the aggressive unload in
   `ModelManager` is cheap to reverse.
4. Report `residentBytes` honestly. The Settings screen shows it.
5. Fail loudly if the GPU is unavailable. `SpeechEngineError.acceleratorUnavailable`
   exists for exactly this; a silent CPU fallback is forbidden by PLAN.md
   section 7.
6. Measure on a real iPhone and write the numbers into `docs/mobile/PLAN.md`
   section 3 next to the M4 laptop measurement (0.40 s warm for an 8.7 s clip,
   1.0 GB resident, 3 s load).

## Known quality cost

Measured 2026-09-02 on an M4 laptop: 0.6B wrote "intro dot lie" for "entro.ly",
and Qwen ignores the Whisper-style prompt. That is why the dictionary is a
post-pass (`DictionaryPostPass`) rather than a prompt on this platform.

## Uncommenting it

`apps/ios/project.yml` already declares the package. Uncomment its line under the
`OpenFlow` target's `dependencies`.
