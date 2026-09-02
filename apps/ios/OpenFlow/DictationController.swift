import Foundation
import Observation
import OpenFlowMobileCore

#if canImport(UIKit)
import UIKit
#endif

#if canImport(ActivityKit)
import ActivityKit
#endif

/// What the capture sheet is doing right now.
enum DictationPhase: Equatable {
    case idle
    case recording
    case transcribing
    case finished(String)
    case failed(String)
}

/// The app's one piece of shared state: it owns the model manager, the
/// microphone and the stores, and every screen and the App Intent talk to it.
///
/// Main-actor isolated because everything it drives is UI. The expensive work
/// lives on `ModelManager` and `AudioCapture`, which are their own actors.
@MainActor
@Observable
final class DictationController {
    static let shared = DictationController()

    private(set) var phase: DictationPhase = .idle
    private(set) var modelState: ModelState = .unloaded
    private(set) var residentBytes: Int = 0
    private(set) var history: [TranscriptRecord] = []
    /// Set by the App Intent so the app opens straight onto the capture sheet.
    var isCaptureSheetPresented = false

    let settings: SettingsStore
    private let manager: ModelManager
    private let clipboard: any ClipboardWriter
    private let store: TranscriptStore?
    private let engineIdentifier: String

    #if canImport(AVFoundation)
    private let capture = AudioCapture()
    #endif

    private var silenceRunSeconds: Double = 0
    private var levelPollTask: Task<Void, Never>?

    #if canImport(ActivityKit)
    private var activity: Activity<DictationActivityAttributes>?
    #endif

    private init() {
        let settings = SettingsStore.shared()
        self.settings = settings

        // The Simulator has no weights and no Metal-backed engine, so a build
        // with -D OPENFLOW_FAKE_ENGINE exercises the whole product -- sheet,
        // history, keyboard, Live Activity -- against a stub. PLAN.md section 6.
        #if OPENFLOW_FAKE_ENGINE
        let engine: any SpeechEngine = FakeEngine(loadSeconds: 0, transcribeSeconds: 0)
        #else
        let engine: any SpeechEngine = UnavailableEngine(choice: settings.engine)
        #endif
        self.engineIdentifier = engine.identifier
        self.manager = ModelManager(
            engine: engine,
            conditions: ProcessInfoConditions(),
            clock: SystemIdleClock(),
            policy: settings.modelPolicy
        )
        self.clipboard = SystemClipboardWriter()
        self.store = try? TranscriptStore.shared()

        // The App Intent lives in a file the widget extension also compiles, so
        // it reaches the controller through this hook rather than by importing
        // the app.
        DictationIntentBridge.shared.register { [weak self] in
            guard let self else { return }
            await self.prewarm(trigger: .intentPrewarm)
            self.isCaptureSheetPresented = true
            await self.startRecording()
        }
        Task { await self.refresh() }
    }

    // MARK: - Lifecycle wiring

    func applyChangedSettings() async {
        await manager.update(policy: settings.modelPolicy)
    }

    func handleMemoryWarning() async {
        await manager.handleMemoryWarning()
        await refresh()
    }

    func handleScenePhase(active: Bool) async {
        if active {
            await manager.handleEnterForeground()
        } else {
            await manager.handleEnterBackground()
        }
        await refresh()
    }

    func handleThermalChange() async {
        await manager.handleThermalChange()
        await refresh()
    }

    func refresh() async {
        modelState = await manager.state
        residentBytes = await manager.residentBytes
        if let store {
            history = store.loadHistory().reversed()
        }
    }

    func transitionLog() async -> [ModelTransition] {
        await manager.transitions
    }

    // MARK: - The capture loop

    /// Called by the App Intent before the sheet is even on screen, and again
    /// when recording actually starts. Both are refused politely in Low Power
    /// Mode; the model still loads when there is audio.
    func prewarm(trigger: ModelTrigger) async {
        await manager.prewarm(trigger: trigger)
        await refresh()
    }

    func startRecording() async {
        guard phase != .recording else { return }
        silenceRunSeconds = 0
        #if canImport(AVFoundation)
        do {
            try await capture.start()
        } catch {
            phase = .failed(describe(error))
            return
        }
        #endif
        phase = .recording
        startLiveActivity()
        // The load overlaps the speech: this is the whole latency argument.
        await prewarm(trigger: .captureStart)
        startLevelPolling()
    }

    func stopRecording() async {
        guard phase == .recording else { return }
        levelPollTask?.cancel()
        levelPollTask = nil

        #if canImport(UIKit)
        if settings.hapticOnStop {
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
        }
        #endif

        #if canImport(AVFoundation)
        let result: CaptureResult
        do {
            result = try await capture.stop()
        } catch AudioCaptureError.tooShort {
            phase = .failed("That was too short to transcribe.")
            return
        } catch {
            phase = .failed(describe(error))
            return
        }
        guard !result.isSilent else {
            phase = .failed(SilenceGate.rejectionMessage(deviceName: "the microphone"))
            await endLiveActivity(preview: nil)
            return
        }
        await transcribe(result)
        #else
        phase = .failed("Audio capture is not available on this platform.")
        #endif
    }

    func cancelRecording() async {
        levelPollTask?.cancel()
        levelPollTask = nil
        #if canImport(AVFoundation)
        await capture.cancel()
        #endif
        await endLiveActivity(preview: nil)
        phase = .idle
    }

    // MARK: - Live Activity

    /// Updated only on state changes, never on a timer (PLAN.md section 5). The
    /// elapsed count in the pill is drawn by SwiftUI from a start date, so it
    /// ticks without a single wake-up on our side.
    private func startLiveActivity() {
        #if canImport(ActivityKit)
        guard ActivityAuthorizationInfo().areActivitiesEnabled, activity == nil else { return }
        activity = try? Activity.request(
            attributes: DictationActivityAttributes(startedAt: Date()),
            content: .init(state: .init(stage: .recording), staleDate: nil)
        )
        #endif
    }

    private func updateLiveActivity(stage: DictationActivityAttributes.Stage, seconds: Double) {
        #if canImport(ActivityKit)
        guard let activity else { return }
        Task {
            await activity.update(.init(state: .init(stage: stage, seconds: seconds), staleDate: nil))
        }
        #endif
    }

    private func endLiveActivity(preview: String?) async {
        #if canImport(ActivityKit)
        guard let activity else { return }
        let trimmed = preview.map { String($0.prefix(80)) }
        await activity.end(
            .init(state: .init(stage: .idle, seconds: 0, preview: trimmed), staleDate: nil),
            dismissalPolicy: .after(.now + 4)
        )
        self.activity = nil
        #endif
    }

    #if canImport(AVFoundation)
    private func transcribe(_ result: CaptureResult) async {
        phase = .transcribing
        updateLiveActivity(stage: .transcribing, seconds: result.seconds)
        await refresh()
        do {
            let transcript = try await manager.transcribe(samples16k: result.samples16k)
            let corrected = DictionaryPostPass.apply(transcript.text, dictionary: settings.dictionary)
            deliver(corrected, seconds: result.seconds)
            phase = .finished(corrected)
            await endLiveActivity(preview: corrected)
        } catch {
            phase = .failed(describe(error))
            await endLiveActivity(preview: nil)
        }
        await refresh()
    }

    /// Clipboard first, then the store. The clipboard is what the user is about
    /// to paste; the store is what the keyboard will insert later.
    private func deliver(_ text: String, seconds: Double) {
        let expiry = settings.clipboardExpirySeconds
        clipboard.write(text, localOnly: true, expiresAfter: expiry > 0 ? Double(expiry) : nil)

        let record = TranscriptRecord(
            text: text,
            durationSeconds: seconds,
            engine: engineIdentifier
        )
        guard let store else { return }
        try? store.saveLast(record)
        if settings.saveHistory {
            try? store.append(record, retentionDays: settings.historyRetentionDays)
        }
    }

    /// Stop-on-silence, when the setting is on: the level has to stay under the
    /// gate's line for `silenceHoldMs` before the sheet ends the take itself.
    private func startLevelPolling() {
        guard settings.stopOnSilence else { return }
        let hold = Double(settings.silenceHoldMs) / 1_000
        levelPollTask = Task { [weak self] in
            let step = 0.1
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(step * 1_000_000_000))
                guard let self else { return }
                let level = await self.capture.currentLevel
                if level < SilenceGate.silenceLevel {
                    self.silenceRunSeconds += step
                } else {
                    self.silenceRunSeconds = 0
                }
                if self.silenceRunSeconds >= hold {
                    await self.stopRecording()
                    return
                }
                if await self.capture.watchdogTripped {
                    await self.stopRecording()
                    return
                }
            }
        }
    }
    #endif

    // MARK: - History

    func deleteHistory(id: UUID) async {
        try? store?.delete(id: id)
        await refresh()
    }

    func deleteAllHistory() async {
        try? store?.deleteAll()
        await refresh()
    }

    func copyToClipboard(_ text: String) {
        let expiry = settings.clipboardExpirySeconds
        clipboard.write(text, localOnly: true, expiresAfter: expiry > 0 ? Double(expiry) : nil)
    }

    private func describe(_ error: Error) -> String {
        if let engineError = error as? SpeechEngineError {
            switch engineError {
            case .modelUnavailable(let detail): return detail
            case .loadFailed(let detail): return detail
            case .noSpeechRecognised: return "Nothing recognisable was said."
            case .transcriptionFailed(let detail): return detail
            case .acceleratorUnavailable(let detail): return detail
            }
        }
        return (error as NSError).localizedDescription
    }
}

/// The engine slot before M2 fills it. It refuses rather than pretending, which
/// is the same rule PLAN.md section 7 applies to CPU fallback: fail loudly.
actor UnavailableEngine: SpeechEngine {
    nonisolated let identifier: String
    private let choice: EngineChoice

    init(choice: EngineChoice) {
        self.choice = choice
        self.identifier = choice.rawValue
    }

    var residentBytes: Int { 0 }

    func load() async throws {
        throw SpeechEngineError.modelUnavailable(
            "\(choice.displayName) is not built into this version yet. Milestone M2 adds it."
        )
    }

    func unload() async {}

    func transcribe(samples16k: [Float]) async throws -> Transcript {
        throw SpeechEngineError.modelUnavailable("No speech engine is installed.")
    }
}
