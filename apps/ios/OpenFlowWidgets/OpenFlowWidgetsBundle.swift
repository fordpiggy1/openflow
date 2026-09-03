import SwiftUI
import WidgetKit

@main
struct OpenFlowWidgetsBundle: WidgetBundle {
    var body: some Widget {
        #if canImport(ActivityKit)
        DictationLiveActivity()
        #endif
        StartDictationControl()
    }
}
