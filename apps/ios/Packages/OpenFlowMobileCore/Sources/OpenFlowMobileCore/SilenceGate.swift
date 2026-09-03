import Foundation

/// The desktop's whole-take silence gate and auto gain, ported from
/// `src-tauri/src/audio.rs` with the same constants and the same test vectors so
/// the two implementations can be cross-checked line for line.
///
/// Nothing here is a re-derivation. `TARGET_PEAK`, `MAX_GAIN`, `SILENCE_LEVEL`,
/// the 95th-percentile index rule and the `1e-4` gain floor are copied. Changing
/// one of them here without changing it there is a bug in this file.
public enum SilenceGate {
    /// Amplitude the 95th-percentile sample should reach after boosting. An
    /// amplitude target, not an RMS one: p95 of speech runs about 1.4x its RMS,
    /// and 0.21 lands the voiced RMS in the 0.13-0.20 band with headroom for
    /// transients. (audio.rs `TARGET_PEAK`)
    public static let targetPeak: Float = 0.21

    /// (audio.rs `MAX_GAIN`)
    public static let maxGain: Float = 20.0

    /// -60 dBFS. A take whose loud part sits under this carried no voice: a
    /// muted, virtual, or permission-blocked input. (audio.rs `SILENCE_LEVEL`)
    public static let silenceLevel: Float = 1e-3

    /// Below this the take has no measurable level at all and boosting it would
    /// only amplify a noise floor. (audio.rs `auto_gain`)
    public static let gainFloor: Float = 1e-4

    /// 95th percentile of |sample|: the level of the loud part of a take, which
    /// leading silence and a single transient both leave alone.
    ///
    /// Keyed on a percentile, not the absolute peak, because the peak is
    /// whatever single loudest thing happened -- a cough, a desk bump, one hard
    /// key press -- so a `peak > 0.5 => give up` rule throws away the boost for
    /// the entire quiet take.
    public static func speechLevel(_ samples: [Float]) -> Float {
        guard !samples.isEmpty else { return 0 }
        var magnitudes = samples.map { abs($0) }
        magnitudes.sort()
        // Rust: ((len as f32 * 0.95) as usize).min(len - 1) -- truncating, not
        // rounding. Float32 arithmetic is used deliberately so the index matches.
        let scaled = Float(magnitudes.count) * 0.95
        let index = min(Int(scaled), magnitudes.count - 1)
        return magnitudes[index]
    }

    /// A take whose loud part sits under -60 dBFS carried no voice.
    ///
    /// This is a whole-take gate, not per-sample silence stripping: that was
    /// removed twice from the desktop (3a9ebee, 0865284) for cutting speech from
    /// low-gain mics, and a quiet real take still measures 10x to 50x above this
    /// line.
    public static func isSilent(_ samples: [Float]) -> Bool {
        speechLevel(samples) < silenceLevel
    }

    /// Boost quiet recordings so the speech-to-text model gets a usable level.
    /// Never clips: the result is clamped to [-1, 1].
    public static func autoGain(_ samples: [Float]) -> [Float] {
        guard !samples.isEmpty else { return [] }
        let level = speechLevel(samples)
        if level < gainFloor { return samples }
        let gain = min(max(targetPeak / level, 1.0), maxGain)
        return samples.map { min(max($0 * gain, -1.0), 1.0) }
    }

    /// The desktop refuses to upload a dead take rather than let Whisper
    /// hallucinate over it. On the phone there is no upload, but the same take
    /// would make Qwen invent a sentence, so the sheet says so instead.
    public static func rejectionMessage(deviceName: String) -> String {
        "No sound reached OpenFlow from \"\(deviceName)\". Check the microphone permission in Settings."
    }
}
