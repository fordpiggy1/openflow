import Foundation
import OpenFlowMobileCore

/// WhisperKit on the Neural Engine. Milestone M2.
///
/// Stubbed for the same reason as the Qwen engine, and with the same rule: it
/// refuses rather than pretending, and it never runs on the CPU quietly.
public actor WhisperSpeechEngine: SpeechEngine {
    public nonisolated let identifier = "whisper-large-v3-turbo"

    public let weights: URL
    /// The dictionary, which this engine can take as a prompt rather than only
    /// as a post-pass -- the one behavioural difference between the two engines.
    public let prompt: String?

    public init(weights: URL, prompt: String? = nil) {
        self.weights = weights
        self.prompt = DictionaryPostPass.capped(prompt)
    }

    public var residentBytes: Int { 0 }

    public func load() async throws {
        throw SpeechEngineError.modelUnavailable(
            "The Whisper engine lands in Milestone M2. See Packages/OpenFlowWhisperEngine/README.md."
        )
    }

    public func unload() async {}

    public func transcribe(samples16k: [Float]) async throws -> Transcript {
        throw SpeechEngineError.modelUnavailable("The Whisper engine lands in Milestone M2.")
    }
}
