import SwiftUI
import WidgetKit
import AppIntents

/// The Control Center / Lock Screen control. One tap, straight into a take.
///
/// It runs `StartDictationIntent`, the same intent the Action Button and
/// Shortcuts use, so there is exactly one path into a capture.
@available(iOS 18.0, *)
struct StartDictationControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "io.laisy.openflow.control.dictate") {
            ControlWidgetButton(action: StartDictationIntent()) {
                Label("Dictate", systemImage: "mic.fill")
            }
        }
        .displayName("OpenFlow Dictation")
        .description("Start a local dictation.")
    }
}
