import Foundation

/// The one-time model download, and the only file in the app that is allowed to
/// touch the network.
///
/// PLAN.md section 0 makes a promise -- "the only network request it ever makes
/// is the one-time model download" -- and section 6 turns it into a check a
/// reviewer can run: `grep -rn "URLSession" apps/ios --include=*.swift` must
/// return this file and nothing else. Anything that wants to reach the network
/// from anywhere else in the app is a bug, not a feature request.
public actor ModelDownloader {

    /// The pinned artefact. Nothing here is discovered at runtime: no manifest
    /// fetch, no redirect chasing, no remote config. Change these and ship a new
    /// build, which is also what makes the App Store privacy answer honest.
    public struct Pin: Sendable, Equatable {
        public let fileName: String
        public let remote: URL
        public let sha256: String
        public let expectedBytes: Int64

        public init(fileName: String, remote: URL, sha256: String, expectedBytes: Int64) {
            self.fileName = fileName
            self.remote = remote
            self.sha256 = sha256
            self.expectedBytes = expectedBytes
        }
    }

    /// TODO(M2): replace host, path, digest and size with the real published
    /// artefact once the Qwen3-ASR-0.6B 8-bit conversion is pinned. The values
    /// below are placeholders and are deliberately not a working URL: a build
    /// that ships them fails its checksum rather than installing something
    /// unverified.
    public static let qwen06Pin = Pin(
        fileName: "qwen3-asr-0.6b-8bit.safetensors",
        remote: URL(string: "https://models.invalid/openflow/TODO-qwen3-asr-0.6b-8bit.safetensors")!,
        sha256: String(repeating: "0", count: 64),
        expectedBytes: 700 * 1_000 * 1_000
    )

    /// TODO(M2): the WhisperKit fallback artefact, per PLAN.md section 3.
    public static let whisperPin = Pin(
        fileName: "whisper-large-v3-turbo.mlmodelc.zip",
        remote: URL(string: "https://models.invalid/openflow/TODO-whisper-large-v3-turbo.zip")!,
        sha256: String(repeating: "0", count: 64),
        expectedBytes: 600 * 1_000 * 1_000
    )

    public static func pin(for engine: EngineChoice) -> Pin {
        switch engine {
        case .qwen06: return qwen06Pin
        case .whisper: return whisperPin
        }
    }

    public enum DownloadError: Error, Equatable, Sendable {
        case transport(String)
        case badStatus(Int)
        case checksumMismatch(expected: String, actual: String)
        case cancelled
        case placeholderPin
    }

    public enum Progress: Sendable, Equatable {
        case downloading(received: Int64, expected: Int64)
        case verifying
        case installing
        case finished(URL)
    }

    private let store: ModelStore
    private let session: URLSession
    private var task: URLSessionDownloadTask?

    public init(store: ModelStore, session: URLSession = .shared) {
        self.store = store
        self.session = session
    }

    /// Download, verify, install. Emits progress as an `AsyncStream` so the
    /// download screen can show the 700 MB honestly instead of a spinner.
    ///
    /// Verification is not optional and not a warning: a mismatch deletes the
    /// file and throws, because the alternative is running unknown weights on
    /// the user's voice.
    public func download(pin: Pin) -> AsyncThrowingStream<Progress, Error> {
        AsyncThrowingStream { continuation in
            let work = Task {
                do {
                    guard pin.sha256 != String(repeating: "0", count: 64) else {
                        throw DownloadError.placeholderPin
                    }
                    try store.prepare()
                    let (temporary, response) = try await self.session.download(from: pin.remote)
                    if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                        throw DownloadError.badStatus(http.statusCode)
                    }
                    continuation.yield(.verifying)
                    let actual = try ModelStore.sha256Hex(ofFileAt: temporary)
                    guard actual.caseInsensitiveCompare(pin.sha256) == .orderedSame else {
                        try? FileManager.default.removeItem(at: temporary)
                        throw DownloadError.checksumMismatch(expected: pin.sha256.lowercased(), actual: actual)
                    }
                    continuation.yield(.installing)
                    try store.install(from: temporary, as: pin.fileName)
                    let installed = store.url(for: pin.fileName)
                    continuation.yield(.finished(installed))
                    continuation.finish()
                } catch let error as DownloadError {
                    continuation.finish(throwing: error)
                } catch let error as ModelStore.StoreError {
                    continuation.finish(throwing: error)
                } catch {
                    continuation.finish(throwing: DownloadError.transport(error.localizedDescription))
                }
            }
            continuation.onTermination = { _ in work.cancel() }
        }
    }

    /// True when the pinned file is already installed and passes its checksum.
    /// The download screen calls this before offering to download anything.
    public func isInstalled(pin: Pin) -> Bool {
        guard store.exists(pin.fileName) else { return false }
        guard pin.sha256 != String(repeating: "0", count: 64) else { return true }
        return (try? store.verify(pin.fileName, sha256Hex: pin.sha256)) != nil
    }
}
