import Foundation
import OpenFlowMobileCore

/// Qwen3-ASR-0.6B on MLX Swift. Milestone M2.
///
/// The shape is settled and the behaviour is not: this type conforms to
/// `SpeechEngine` so the app can be built against it the moment the port lands,
/// and refuses every call until then. It never pretends to work, and it never
/// falls back to the CPU -- PLAN.md section 7.
public actor QwenSpeechEngine: SpeechEngine {
    public nonisolated let identifier = "qwen3-asr-0.6b-8bit"

    /// Where the weights live, once `ModelDownloader` has put them there.
    public let weights: URL

    public init(weights: URL) {
        self.weights = weights
    }

    /// About 1 GB once M2 loads real weights (PLAN.md section 3). Zero until
    /// then, which is true: nothing is resident.
    public var residentBytes: Int { 0 }

    public func load() async throws {
        throw SpeechEngineError.modelUnavailable(
            "The Qwen engine lands in Milestone M2. See Packages/OpenFlowQwenEngine/README.md."
        )
    }

    public func unload() async {}

    public func transcribe(samples16k: [Float]) async throws -> Transcript {
        throw SpeechEngineError.modelUnavailable("The Qwen engine lands in Milestone M2.")
    }
}
