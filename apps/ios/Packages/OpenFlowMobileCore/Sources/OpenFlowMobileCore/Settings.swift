import Foundation

/// Which recogniser runs. PLAN.md section 3: both sit behind `SpeechEngine`, and
/// Milestone M2 decides which one ships first.
public enum EngineChoice: String, Sendable, CaseIterable, Identifiable {
    case qwen06
    case whisper

    public var id: String { rawValue }

    public var displayName: String {
        switch self {
        case .qwen06: return "Qwen3-ASR 0.6B"
        case .whisper: return "Whisper (Neural Engine)"
        }
    }
}

/// The App Group every OpenFlow process shares: app, keyboard extension, widgets.
public enum AppGroup {
    public static let identifier = "group.io.laisy.openflow"
}

/// Every setting from PLAN.md section 4, over an injected `UserDefaults` suite.
///
/// `UserDefaults` is documented as thread-safe, which is why this is
/// `@unchecked Sendable`: the unchecked part is the compiler's inability to see
/// that, not a claim of our own.
public final class SettingsStore: @unchecked Sendable {
    /// The defaults from PLAN.md section 4, in one place so the tests and the UI
    /// cannot drift from the plan independently.
    public enum Defaults {
        public static let engine: EngineChoice = .qwen06
        public static let stopOnSilence = false
        public static let silenceHoldMs = 1_200
        public static let dictionary = ""
        public static let clipboardExpirySeconds = 60
        public static let saveHistory = true
        public static let historyRetentionDays = 30
        public static let unloadAfterMinutes = 5
        public static let prewarmOnCapture = true
        public static let hapticOnStop = true
        public static let onboardingComplete = false
    }

    public enum Key: String, CaseIterable {
        case engine
        case stopOnSilence
        case silenceHoldMs
        case dictionary
        case clipboardExpirySeconds
        case saveHistory
        case historyRetentionDays
        case unloadAfterMinutes
        case prewarmOnCapture
        case hapticOnStop
        case onboardingComplete
    }

    private let defaults: UserDefaults

    public init(defaults: UserDefaults) {
        self.defaults = defaults
    }

    /// The shared suite the app, the keyboard and the widgets all read.
    /// Falls back to `.standard` when the App Group is not provisioned yet, so a
    /// development build without the entitlement still runs.
    public static func shared(appGroup: String = AppGroup.identifier) -> SettingsStore {
        SettingsStore(defaults: UserDefaults(suiteName: appGroup) ?? .standard)
    }

    // MARK: - Values

    public var engine: EngineChoice {
        get { (defaults.string(forKey: Key.engine.rawValue).flatMap(EngineChoice.init(rawValue:))) ?? Defaults.engine }
        set { defaults.set(newValue.rawValue, forKey: Key.engine.rawValue) }
    }

    public var stopOnSilence: Bool {
        get { bool(.stopOnSilence, Defaults.stopOnSilence) }
        set { defaults.set(newValue, forKey: Key.stopOnSilence.rawValue) }
    }

    /// How long the level must stay under the silence line before a
    /// stop-on-silence capture ends itself.
    public var silenceHoldMs: Int {
        get { int(.silenceHoldMs, Defaults.silenceHoldMs) }
        set { defaults.set(max(200, newValue), forKey: Key.silenceHoldMs.rawValue) }
    }

    /// Names and terms, same 800-character budget as the desktop's Whisper
    /// prompt. On the phone it drives `DictionaryPostPass`, because Qwen ignores
    /// prompts (PLAN.md section 3).
    public var dictionary: String {
        get { defaults.string(forKey: Key.dictionary.rawValue) ?? Defaults.dictionary }
        set { defaults.set(DictionaryPostPass.capped(newValue) ?? "", forKey: Key.dictionary.rawValue) }
    }

    /// Seconds before the pasteboard item expires. Zero means never.
    public var clipboardExpirySeconds: Int {
        get { int(.clipboardExpirySeconds, Defaults.clipboardExpirySeconds) }
        set { defaults.set(max(0, newValue), forKey: Key.clipboardExpirySeconds.rawValue) }
    }

    public var saveHistory: Bool {
        get { bool(.saveHistory, Defaults.saveHistory) }
        set { defaults.set(newValue, forKey: Key.saveHistory.rawValue) }
    }

    public var historyRetentionDays: Int {
        get { int(.historyRetentionDays, Defaults.historyRetentionDays) }
        set { defaults.set(max(1, newValue), forKey: Key.historyRetentionDays.rawValue) }
    }

    /// 1...30 minutes, or 0 for "keep loaded while the app is open".
    public var unloadAfterMinutes: Int {
        get { int(.unloadAfterMinutes, Defaults.unloadAfterMinutes) }
        set { defaults.set(min(30, max(0, newValue)), forKey: Key.unloadAfterMinutes.rawValue) }
    }

    public var prewarmOnCapture: Bool {
        get { bool(.prewarmOnCapture, Defaults.prewarmOnCapture) }
        set { defaults.set(newValue, forKey: Key.prewarmOnCapture.rawValue) }
    }

    public var hapticOnStop: Bool {
        get { bool(.hapticOnStop, Defaults.hapticOnStop) }
        set { defaults.set(newValue, forKey: Key.hapticOnStop.rawValue) }
    }

    public var onboardingComplete: Bool {
        get { bool(.onboardingComplete, Defaults.onboardingComplete) }
        set { defaults.set(newValue, forKey: Key.onboardingComplete.rawValue) }
    }

    // MARK: - Derived

    public var modelPolicy: ModelPolicy {
        ModelPolicy(prewarmOnCapture: prewarmOnCapture, unloadAfterMinutes: unloadAfterMinutes)
    }

    /// Wipes every OpenFlow key. Used by the Settings "reset" row and the tests.
    public func resetAll() {
        for key in Key.allCases {
            defaults.removeObject(forKey: key.rawValue)
        }
    }

    // MARK: - Reading with a real default

    /// `UserDefaults.bool(forKey:)` returns false for a missing key, which is the
    /// wrong answer for every setting here whose default is true. These two read
    /// the object first so an unset key falls back to the planned default.
    private func bool(_ key: Key, _ fallback: Bool) -> Bool {
        guard let value = defaults.object(forKey: key.rawValue) as? Bool else { return fallback }
        return value
    }

    private func int(_ key: Key, _ fallback: Int) -> Int {
        guard let value = defaults.object(forKey: key.rawValue) as? Int else { return fallback }
        return value
    }
}
