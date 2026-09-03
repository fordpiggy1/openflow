import Foundation

/// Thermal pressure, mirroring `ProcessInfo.ThermalState` without importing it,
/// so the core package stays testable on any host and the tests can drive the
/// value directly.
public enum ThermalPressure: Int, Sendable, Comparable, CaseIterable {
    case nominal = 0
    case fair = 1
    case serious = 2
    case critical = 3

    public static func < (lhs: ThermalPressure, rhs: ThermalPressure) -> Bool {
        lhs.rawValue < rhs.rawValue
    }
}

/// What the OS is telling us about power and heat at this instant.
public struct SystemConditionsSnapshot: Sendable, Equatable {
    public var lowPowerMode: Bool
    public var thermal: ThermalPressure

    public init(lowPowerMode: Bool = false, thermal: ThermalPressure = .nominal) {
        self.lowPowerMode = lowPowerMode
        self.thermal = thermal
    }

    /// PLAN.md section 2: never prewarm in Low Power Mode, and never at
    /// `.serious` or worse. In those cases the model loads only when there is
    /// audio waiting to be transcribed.
    public var allowsPrewarm: Bool {
        !lowPowerMode && thermal < .serious
    }

    /// PLAN.md section 2: `.serious` is also an unload trigger, not only a
    /// prewarm veto.
    public var demandsUnload: Bool {
        thermal >= .serious
    }
}

/// Injected so tests can drive Low Power Mode and thermal state; the app injects
/// `ProcessInfoConditions`.
public protocol SystemConditions: Sendable {
    func current() -> SystemConditionsSnapshot
}

/// The real thing. `ProcessInfo.processInfo` is documented as thread-safe.
public struct ProcessInfoConditions: SystemConditions {
    public init() {}

    public func current() -> SystemConditionsSnapshot {
        let info = ProcessInfo.processInfo
        let thermal: ThermalPressure
        switch info.thermalState {
        case .nominal: thermal = .nominal
        case .fair: thermal = .fair
        case .serious: thermal = .serious
        case .critical: thermal = .critical
        @unknown default: thermal = .serious
        }
        return SystemConditionsSnapshot(
            lowPowerMode: info.isLowPowerModeEnabled,
            thermal: thermal
        )
    }
}

/// A fixed snapshot, useful for previews and for the Simulator run with FakeEngine.
public struct StaticConditions: SystemConditions {
    private let snapshot: SystemConditionsSnapshot

    public init(_ snapshot: SystemConditionsSnapshot = SystemConditionsSnapshot()) {
        self.snapshot = snapshot
    }

    public func current() -> SystemConditionsSnapshot { snapshot }
}
