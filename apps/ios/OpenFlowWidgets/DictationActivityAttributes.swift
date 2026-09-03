import Foundation

#if canImport(ActivityKit)
import ActivityKit

/// The Live Activity's shape, shared by the app (which starts and updates it)
/// and the widget extension (which draws it).
///
/// PLAN.md section 5: the Live Activity is updated only on state changes, never
/// on a timer. There are three states and that is the whole vocabulary.
struct DictationActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable {
        public var stage: Stage
        /// Seconds of speech captured so far, for the expanded view.
        public var seconds: Double
        /// Set once the take is finished, so the pill can show the first words.
        public var preview: String?

        public init(stage: Stage, seconds: Double = 0, preview: String? = nil) {
            self.stage = stage
            self.seconds = seconds
            self.preview = preview
        }
    }

    public enum Stage: String, Codable, Hashable {
        case idle
        case recording
        case transcribing

        public var label: String {
            switch self {
            case .idle: return "Ready"
            case .recording: return "Listening"
            case .transcribing: return "Transcribing"
            }
        }

        public var systemImage: String {
            switch self {
            case .idle: return "mic"
            case .recording: return "waveform"
            case .transcribing: return "gearshape.2"
            }
        }
    }

    /// Fixed for the life of the activity.
    public var startedAt: Date

    public init(startedAt: Date = Date()) {
        self.startedAt = startedAt
    }
}
#endif
