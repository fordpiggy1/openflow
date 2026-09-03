import SwiftUI
import WidgetKit

#if canImport(ActivityKit)
import ActivityKit

/// The Dynamic Island pill and the Lock Screen banner for a take in progress.
///
/// Deliberately plain: an icon, a word, and a timer. Nothing here needs the
/// model, the transcript, or the network.
struct DictationLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: DictationActivityAttributes.self) { context in
            // Lock Screen / banner presentation.
            HStack(spacing: 12) {
                Image(systemName: context.state.stage.systemImage)
                    .font(.title2)
                    .foregroundStyle(context.state.stage == .recording ? .red : .primary)
                VStack(alignment: .leading, spacing: 2) {
                    Text("OpenFlow")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(context.state.preview ?? context.state.stage.label)
                        .font(.headline)
                        .lineLimit(1)
                }
                Spacer()
                Text(context.attributes.startedAt, style: .timer)
                    .font(.system(.title3, design: .rounded))
                    .monospacedDigit()
                    .frame(maxWidth: 64)
            }
            .padding()
            .activityBackgroundTint(Color.black.opacity(0.5))

        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Image(systemName: context.state.stage.systemImage)
                        .foregroundStyle(context.state.stage == .recording ? .red : .primary)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(context.attributes.startedAt, style: .timer)
                        .monospacedDigit()
                        .frame(maxWidth: 56)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.state.stage.label)
                        .font(.headline)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    if let preview = context.state.preview {
                        Text(preview).font(.caption).lineLimit(2)
                    } else {
                        Text("Recognition runs on this iPhone.")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            } compactLeading: {
                Image(systemName: context.state.stage.systemImage)
                    .foregroundStyle(context.state.stage == .recording ? .red : .primary)
            } compactTrailing: {
                Text(context.attributes.startedAt, style: .timer)
                    .monospacedDigit()
                    .frame(maxWidth: 40)
            } minimal: {
                Image(systemName: context.state.stage.systemImage)
                    .foregroundStyle(context.state.stage == .recording ? .red : .primary)
            }
        }
    }
}
#endif
