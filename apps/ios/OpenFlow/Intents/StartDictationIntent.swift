import AppIntents
import Foundation

/// The seam that lets one intent definition be compiled into both the app and
/// the widget extension.
///
/// `ControlWidgetButton` needs the intent type at compile time inside the widget
/// extension, but the intent's real work lives in the app and touches the app's
/// controller. So the file is compiled into both targets and the app installs
/// the handler at launch; in the extension the handler is nil and `perform()`
/// simply opens the app, which is what `openAppWhenRun` was going to do anyway.
@MainActor
public final class DictationIntentBridge {
    public static let shared = DictationIntentBridge()

    private var handler: (@MainActor () async -> Void)?

    private init() {}

    public func register(_ handler: @escaping @MainActor () async -> Void) {
        self.handler = handler
    }

    func startDictation() async {
        await handler?()
    }
}

/// The Action Button, Back Tap, Control Center, Shortcuts and Siri entry point.
///
/// It opens the app, because a third-party app cannot record from the background
/// and cannot type into another app (PLAN.md section 0). What it buys is the
/// prewarm: the model starts loading while the sheet is still animating in,
/// which is two to three seconds the user never waits for.
struct StartDictationIntent: AppIntent {
    static let title: LocalizedStringResource = "Start dictation"
    static let description = IntentDescription(
        "Opens OpenFlow and starts listening. Recognition runs on this iPhone."
    )

    /// Required, and honest: capture needs the microphone and the sheet, so the
    /// app comes to the front. Claiming otherwise would be claiming background
    /// residency the OS does not grant.
    static let openAppWhenRun = true

    @MainActor
    func perform() async throws -> some IntentResult {
        await DictationIntentBridge.shared.startDictation()
        return .result()
    }
}

/// Makes the intent findable in Shortcuts and by voice with no setup.
struct OpenFlowShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: StartDictationIntent(),
            phrases: [
                "Start dictation with \(.applicationName)",
                "Dictate with \(.applicationName)",
            ],
            shortTitle: "Start dictation",
            systemImageName: "mic.fill"
        )
    }
}
