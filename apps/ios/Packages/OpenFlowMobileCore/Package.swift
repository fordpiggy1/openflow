// swift-tools-version: 6.0
import PackageDescription

// OpenFlowMobileCore is the whole brain of the phone app: the load/unload state
// machine, the audio maths, the dictionary post-pass and the stores. It has no
// dependencies at all, so it builds and tests with the Command Line Tools alone
// (no Xcode, no Metal toolchain) on the macOS host as well as on iOS.
//
// The engine packages (OpenFlowQwenEngine, OpenFlowWhisperEngine) are NOT listed
// here on purpose: they need MLX Swift / WhisperKit and the Metal toolchain, and
// making them dependencies would take `swift test` away from the CLT gate.
let package = Package(
    name: "OpenFlowMobileCore",
    platforms: [
        .iOS(.v18),
        .macOS(.v14),
    ],
    products: [
        .library(name: "OpenFlowMobileCore", targets: ["OpenFlowMobileCore"])
    ],
    targets: [
        .target(
            name: "OpenFlowMobileCore",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "OpenFlowMobileCoreTests",
            dependencies: ["OpenFlowMobileCore"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
