import Foundation

/// One recognised take.
public struct Transcript: Sendable, Equatable {
    /// The recognised text, already trimmed. Never empty when the engine
    /// succeeded; an engine that recognised nothing throws instead, the same
    /// rule the desktop applies in `parse_transcription_response`.
    public var text: String
    /// Wall-clock seconds the recognition itself took, for the diagnostics screen.
    public var latencySeconds: Double

    public init(text: String, latencySeconds: Double) {
        self.text = text
        self.latencySeconds = latencySeconds
    }
}

public enum SpeechEngineError: Error, Equatable, Sendable {
    /// Weights are missing or failed their checksum.
    case modelUnavailable(String)
    /// The engine could not bring the weights into memory.
    case loadFailed(String)
    /// Recognition ran but produced nothing usable.
    case noSpeechRecognised
    /// Recognition failed outright.
    case transcriptionFailed(String)
    /// The only accelerator we accept was unavailable. PLAN.md section 5: never
    /// fall back to CPU silently, fail loudly instead.
    case acceleratorUnavailable(String)
}

/// The one seam between the app and whatever recognises speech.
///
/// Declared as an `Actor` protocol so an implementation gets its state isolation
/// for free and `ModelManager` can drive it from its own actor without any lock.
/// Milestone M2 fills this in twice: MLX Swift Qwen3-ASR-0.6B and WhisperKit.
public protocol SpeechEngine: Actor {
    /// A short identifier for the diagnostics screen, e.g. "qwen3-asr-0.6b-8bit".
    nonisolated var identifier: String { get }

    /// Bytes the weights occupy right now. Zero when unloaded. Surfaced in
    /// Settings so the 1 GB cost from PLAN.md section 0 is visible, not implied.
    var residentBytes: Int { get }

    /// Bring the weights into memory. Must be idempotent: calling it while
    /// already loaded is a no-op, not a second allocation.
    func load() async throws

    /// Drop the weights. Must be idempotent and must not throw; the caller is
    /// often reacting to a memory warning and has nowhere to put an error.
    ///
    /// **It must not tear down state a `transcribe(samples16k:)` still running
    /// on this actor is using.** `ModelManager.handleMemoryWarning()` calls this
    /// while a transcription may be in flight -- deliberately, because PLAN.md
    /// section 2 says a memory warning gets no grace period -- and it relies on
    /// actor serialisation to make that safe: the call is queued behind whatever
    /// is executing and runs when the actor is next free.
    ///
    /// That is only a guarantee for an implementation that does its work in one
    /// unbroken stretch. An engine that suspends *inside* `transcribe` -- which
    /// M2's MLX engine will, awaiting GPU work between decoder steps -- gives
    /// this call a window to run in the middle of a recognition. Such an engine
    /// must either hold the weights alive for the take in progress and free them
    /// when it finishes, or cancel that take and throw
    /// `SpeechEngineError.transcriptionFailed`. Freeing memory out from under a
    /// suspended `transcribe` is a crash, not an optimisation.
    func unload() async

    /// Recognise 16 kHz mono Float32 samples in [-1, 1].
    func transcribe(samples16k: [Float]) async throws -> Transcript
}
