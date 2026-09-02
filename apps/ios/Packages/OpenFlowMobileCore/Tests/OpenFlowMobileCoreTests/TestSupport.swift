import Foundation
import Testing
@testable import OpenFlowMobileCore

/// A clock the test drives by hand. Nothing in these tests waits for real time,
/// so a five-minute idle timer costs a microsecond.
final class ManualClock: IdleClock, @unchecked Sendable {
    private let lock = NSLock()
    private var current: Double = 0
    private var nextIdentifier = 0
    private var waiters: [(id: Int, deadline: Double, continuation: CheckedContinuation<Void, Never>)] = []
    private var cancelledBeforeRegistering = Set<Int>()

    func nowSeconds() -> Double {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    /// How many sleepers are currently parked. Tests use this to know the code
    /// under test has actually reached its timer before advancing time.
    var sleeperCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return waiters.count
    }

    /// Locking lives in synchronous helpers: `NSLock.lock()` is unavailable from
    /// an async context, and rightly so.
    private func allocateIdentifier() -> Int {
        lock.lock()
        defer { lock.unlock() }
        let identifier = nextIdentifier
        nextIdentifier += 1
        return identifier
    }

    func sleep(seconds: Double) async {
        guard seconds > 0 else { return }
        let identifier = allocateIdentifier()

        await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                lock.lock()
                if cancelledBeforeRegistering.remove(identifier) != nil {
                    lock.unlock()
                    continuation.resume()
                    return
                }
                waiters.append((identifier, current + seconds, continuation))
                lock.unlock()
            }
        } onCancel: {
            self.wake(identifier: identifier)
        }
    }

    private func wake(identifier: Int) {
        lock.lock()
        if let index = waiters.firstIndex(where: { $0.id == identifier }) {
            let waiter = waiters.remove(at: index)
            lock.unlock()
            waiter.continuation.resume()
            return
        }
        cancelledBeforeRegistering.insert(identifier)
        lock.unlock()
    }

    func advance(by delta: Double) {
        lock.lock()
        current += delta
        let due = waiters.filter { $0.deadline <= current }
        waiters.removeAll { $0.deadline <= current }
        lock.unlock()
        for waiter in due { waiter.continuation.resume() }
    }

    /// Wait until at least `count` sleepers are parked, then advance.
    func advanceOnceSleeping(_ count: Int = 1, by delta: Double) async {
        await waitForSleepers(count)
        advance(by: delta)
    }

    func waitForSleepers(_ count: Int) async {
        for _ in 0..<2_000 {
            if sleeperCount >= count { return }
            await Task.yield()
            try? await Task.sleep(nanoseconds: 200_000)
        }
        Issue.record("no sleeper reached the clock; the code under test never armed its timer")
    }
}

/// Low Power Mode and thermal state, driven by the test.
final class MutableConditions: SystemConditions, @unchecked Sendable {
    private let lock = NSLock()
    private var snapshot: SystemConditionsSnapshot

    init(_ snapshot: SystemConditionsSnapshot = SystemConditionsSnapshot()) {
        self.snapshot = snapshot
    }

    func current() -> SystemConditionsSnapshot {
        lock.lock()
        defer { lock.unlock() }
        return snapshot
    }

    func set(lowPowerMode: Bool) {
        lock.lock()
        snapshot.lowPowerMode = lowPowerMode
        lock.unlock()
    }

    func set(thermal: ThermalPressure) {
        lock.lock()
        snapshot.thermal = thermal
        lock.unlock()
    }
}

/// Poll an actor until it reports what the test is waiting for. Bounded, so a
/// state machine that never gets there fails instead of hanging the suite.
func waitUntil(
    _ description: String,
    sourceLocation: SourceLocation = #_sourceLocation,
    _ condition: @Sendable () async -> Bool
) async {
    for _ in 0..<2_000 {
        if await condition() { return }
        await Task.yield()
        try? await Task.sleep(nanoseconds: 200_000)
    }
    Issue.record("timed out waiting for: \(description)", sourceLocation: sourceLocation)
}

// MARK: - Signal generators, ported from the tests in src-tauri/src/audio.rs

func tone(_ frequency: Float, _ rate: Double, _ seconds: Float) -> [Float] {
    let count = Int(Float(rate) * seconds)
    return (0..<count).map { sin(2 * .pi * frequency * Float($0) / Float(rate)) }
}

func energyAt(_ samples: [Float], _ rate: Double, _ frequency: Float) -> Float {
    let n = Float(samples.count)
    var re: Float = 0
    var im: Float = 0
    for (index, sample) in samples.enumerated() {
        let phase = 2 * Float.pi * frequency * Float(index) / Float(rate)
        re += sample * cos(phase)
        im += sample * sin(phase)
    }
    return ((re * re + im * im).squareRoot() / n) * 2
}

func rms(_ samples: [Float]) -> Float {
    guard !samples.isEmpty else { return 0 }
    let sum = samples.reduce(0.0) { $0 + Double($1) * Double($1) }
    return Float((sum / Double(samples.count)).squareRoot())
}

func temporaryDirectory(_ name: String = UUID().uuidString) -> URL {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("openflow-tests")
        .appendingPathComponent(name, isDirectory: true)
    try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}
