// swift-tools-version: 6.2
import PackageDescription
import Foundation
let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
let libraryPath = ProcessInfo.processInfo.environment["LEGATO_BRIDGE_LIB_DIR"] ?? root.appendingPathComponent("target/debug").path
let frameworks = ["AVFoundation", "CoreAudio", "Metal", "UniformTypeIdentifiers", "ApplicationServices", "AppKit", "CoreFoundation", "CoreGraphics", "CoreMedia", "CoreVideo", "Foundation", "IOKit", "ScreenCaptureKit", "Security", "SystemConfiguration", "VideoToolbox"]
let package = Package(
    name: "LegatoMac",
    platforms: [.macOS("26.0")],
    products: [.executable(name: "Legato", targets: ["Legato"])],
    targets: [
        .systemLibrary(name: "LegatoBridge"),
        .executableTarget(name: "Legato", dependencies: ["LegatoBridge"],
            linkerSettings: [.unsafeFlags(["-L" + libraryPath, "-llegato_swift_bridge"])] + frameworks.map { .linkedFramework($0) } + [.linkedLibrary("iconv")]),
        .testTarget(name: "LegatoTests", dependencies: ["Legato"])
    ]
)
