# Local runner benchmark: Qwen3-ASR on Apple silicon (MLX)

Measured 2026-09-02 on a MacBook Air M4, 16 GB, macOS 26.5, mlx 0.32.2, mlx-audio 0.5.1, Python 3.12. Clip: `take.wav`, 8.7 s of speech, the same clip used for the Groq and LAN timings in PR #7. Method: `bench_qwen.py` (load, one cold run, median of five warm runs, `mx.get_peak_memory`), then `bench_load.py` in a fresh process with the weights already on disk.

## Results

| | Qwen3-ASR-0.6B-8bit | Qwen3-ASR-1.7B-8bit | whisper.cpp large-v3-turbo q5_0 (Metal) | LAN box 1.7B-8bit (M-series, over Wi-Fi) | Groq whisper-large-v3-turbo + dictionary |
|---|---|---|---|---|---|
| Warm inference, 8.7 s clip | 0.40 s (min 0.395) | 0.98 s (min 0.954) | 1.8 s wall (encode 1.22 s, load 0.16 s) | 1.1 s | 1.7 s |
| First inference after load (Metal compile) | 0.68 s | 1.23 s | 2.15 s | n/a | n/a |
| Load weights from disk, fresh process | 2.98 s | 2.54 s | 0.16 s (mmap) | n/a | n/a |
| Active memory while resident | 1.03 GB | 2.49 GB | 870 MB RSS | remote | remote |
| Process max RSS | 518 MB | 1.37 GB | 870 MB | remote | remote |
| Weights on disk | 969 MB | 699 MB (plus tokenizer, total under 2 GB) | 574 MB | | |
| "the entro.ly leaderboard" | "the intro dot lie leaderboard" | "the intro.ly leaderboard" | "the intro.ly leaderboard" (prompt not honored for this term) | "the intro.ly leaderboard" | "the Entro.LY leaderboard" |
| "fast pay ledger" | "fast pay ledger" | "fast pay ledger" | "FastPay ledger" (prompt honored) | "fast pay ledger" | "FastPay ledger" |
| Filler words | keeps "Um", "Ah" | keeps "ah", drops "Um" | keeps both, lowercases everything | same as local 1.7B | drops both |

## What this means for the runner

- Speed: 0.6B is 2.4 times faster than 1.7B on the same machine and 4 times faster than Groq end to end. For a 10 s dictation the user-visible wait drops from about 1.7 s to about 0.4 s, and there is no network variance.
- Accuracy: 0.6B loses proper nouns that 1.7B keeps. Neither Qwen size honors the Whisper `prompt`, so the Dictionary setting does nothing on the local runner; a post-pass (the existing cleanup step, or a deterministic dictionary replace) has to carry the spellings. 1.7B is the safer default for a user who dictates product names; 0.6B is the right choice when speed matters more than names, and the cleanup step is on.
- Memory: a resident 0.6B sidecar holds about 1 GB of unified memory, 1.7B about 2.5 GB. That is not "nothing", so the runner must unload after an idle window (the reload penalty is about 3.7 s for 0.6B, 3.8 s for 1.7B, paid on the first dictation after the window) and the Settings UI must show the resident cost.
- Runtime: MLX needs Python and about 600 MB of packages. Bundling that into OpenFlow.app roughly triples the download. whisper.cpp (large-v3-turbo, q5_0, Metal) needs no runtime and honors the prompt for plain words (FastPay), but it is 4.5 times slower than Qwen 0.6B and no faster than Groq: whisper's encoder always processes a 30 s window, so short dictations pay a fixed 1.2 s encode. It loads in 0.16 s because the weights are memory-mapped, which is the reload behavior the runner wants regardless of engine.

## Recommendation

Build the runner abstraction engine-agnostic (PLAN.md section 7) and ship the MLX Qwen sidecar as the engine: 1.7B as the default model (keeps proper nouns), 0.6B offered as "fast" in Settings, the dictionary applied as a deterministic post-pass since neither Qwen size honors a prompt, prewarm the model when recording starts so the 3 s load hides behind the user speaking, and unload after 10 minutes idle. whisper.cpp is ruled out as the primary engine on speed: it matches the cloud round trip it was meant to beat. It stays a candidate for a no-Python build later, at the cost of the 4x speed gain.
