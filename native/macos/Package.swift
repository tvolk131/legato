// swift-tools-version: 6.2
import Foundation
import PackageDescription

// The Rust engine is linked as a static library: build it first with
// `cargo build -p legato-swift-bridge` (see README.md). Release builds point
// LEGATO_BRIDGE_LIB_DIR at their own Cargo output.
let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
let libraryPath = ProcessInfo.processInfo.environment["LEGATO_BRIDGE_LIB_DIR"] ?? root.appendingPathComponent("target/debug").path
// Frameworks the engine's macOS crates use.
let frameworks = ["AVFoundation", "AppKit", "ApplicationServices", "CoreAudio", "CoreFoundation", "CoreGraphics", "CoreMedia", "CoreVideo", "Foundation", "IOKit", "Metal", "ScreenCaptureKit", "Security", "SystemConfiguration", "UniformTypeIdentifiers", "VideoToolbox"]

let package = Package(
    name: "LegatoMac",
    platforms: [.macOS("26.0")],
    products: [.executable(name: "Legato", targets: ["Legato"])],
    targets: [
        .systemLibrary(name: "LegatoBridge"),
        .executableTarget(
            name: "Legato",
            dependencies: ["LegatoBridge"],
            linkerSettings: [.unsafeFlags(["-L" + libraryPath, "-llegato_swift_bridge"])]
                + frameworks.map { .linkedFramework($0) } + [.linkedLibrary("iconv")]),
        .testTarget(name: "LegatoTests", dependencies: ["Legato"], exclude: ["Fixtures"]),
    ]
)
