// swift-tools-version: 6.0
import PackageDescription

// Milestone M2, the fallback engine. Declared, documented and stubbed, and kept
// out of the Command Line Tools gate: WhisperKit pulls in CoreML model
// compilation, which needs Xcode. It is not a dependency of OpenFlowMobileCore.
//
// To start M2: add the WhisperKit dependency below, attach it to the target, and
// uncomment the package entry under the OpenFlow target in apps/ios/project.yml.
let package = Package(
    name: "OpenFlowWhisperEngine",
    platforms: [
        .iOS(.v18),
        .macOS(.v14),
    ],
    products: [
        .library(name: "OpenFlowWhisperEngine", targets: ["OpenFlowWhisperEngine"])
    ],
    dependencies: [
        .package(name: "OpenFlowMobileCore", path: "../OpenFlowMobileCore")
        // M2: .package(url: "<whisperkit>", from: "<version>")
    ],
    targets: [
        .target(
            name: "OpenFlowWhisperEngine",
            dependencies: ["OpenFlowMobileCore"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
