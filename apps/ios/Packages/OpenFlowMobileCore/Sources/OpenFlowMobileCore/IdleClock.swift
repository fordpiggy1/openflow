import Foundation

/// The only source of time `ModelManager` uses, so the idle timer and the
/// background grace delay are driven by the tests instead of waited out in real
/// time.
///
/// `nowSeconds()` is a monotonic reading with an arbitrary origin, used for the
/// transition log; it is synchronous so that a state transition is never split
/// across a suspension point. `sleep(seconds:)` is the delay primitive.
public protocol IdleClock: Sendable {
    func nowSeconds() -> Double
    /// Suspends for `seconds`. Returns (without throwing) if the task is
    /// cancelled, so callers must check `Task.isCancelled` afterwards.
    func sleep(seconds: Double) async
}

/// Wall-clock implementation used by the app.
public struct SystemIdleClock: IdleClock {
    public init() {}

    public func nowSeconds() -> Double {
        Date().timeIntervalSinceReferenceDate
    }

    public func sleep(seconds: Double) async {
        guard seconds > 0 else { return }
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }
}
