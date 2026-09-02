import AppIntents

/// Makes `StartDictationIntent` findable in Shortcuts and by voice with no setup.
///
/// This lives in its own file, and not next to the intent, because
/// `StartDictationIntent.swift` is compiled into the widget extension as well
/// (the ControlWidget needs the intent type at compile time). An
/// `AppShortcutsProvider` must be declared once, by the app, in the app's own
/// bundle: two targets each declaring one is a duplicate registration, and the
/// extension has no business advertising Siri phrases at all. Only the OpenFlow
/// app target compiles this file.
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
