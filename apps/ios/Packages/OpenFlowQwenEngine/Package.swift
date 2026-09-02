// swift-tools-version: 6.0
import PackageDescription

// Milestone M2. This package is declared, documented and stubbed, but it is NOT
// part of the Command Line Tools gate and is NOT a dependency of
// OpenFlowMobileCore: MLX Swift needs the Metal toolchain, which only ships with
// Xcode. Adding it here would take `swift test` away from a machine that has
// only the Command Line Tools, which is the machine M1 was built on.
//
// To start M2: add the mlx-swift dependency below, attach it to the target, and
// uncomment the package entry under the OpenFlow target in apps/ios/project.yml.
let package = Package(
    name: "OpenFlowQwenEngine",
    platforms: [
        .iOS(.v18),
        .macOS(.v14),
    ],
    products: [
        .library(name: "OpenFlowQwenEngine", targets: ["OpenFlowQwenEngine"])
    ],
    dependencies: [
        .package(name: "OpenFlowMobileCore", path: "../OpenFlowMobileCore")
        // M2: .package(url: "<mlx-swift>", from: "<version>")
    ],
    targets: [
        .target(
            name: "OpenFlowQwenEngine",
            dependencies: ["OpenFlowMobileCore"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
