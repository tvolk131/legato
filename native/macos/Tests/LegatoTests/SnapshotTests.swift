import AppKit
import SwiftUI
import Testing
@testable import Legato

/// Renders the real native controls without starting the engine, injecting input,
/// changing the clipboard, requesting permissions, or displaying a visible window.
@Suite(.serialized) @MainActor struct SnapshotTests {
    private let snapshots = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("Snapshots")
    private var artifacts: URL {
        URL(fileURLWithPath: ProcessInfo.processInfo.environment["LEGATO_SNAPSHOT_ARTIFACTS"] ?? "/tmp/legato-swift-snapshots")
    }

    @Test func testDevicesLightAndDark() throws {
        let model = fixture()
        try assertSnapshot("devices-light", view: DevicesView(model: model), size: CGSize(width: 680, height: 500))
        try assertSnapshot("devices-dark", view: DevicesView(model: model), size: CGSize(width: 680, height: 500), dark: true)
    }

    @Test func testDevicesEmptyOfflineAndLongName() throws {
        let model = fixture()
        model.state.paired = []
        try assertSnapshot("devices-empty", view: DevicesView(model: model), size: CGSize(width: 520, height: 460))
        model.state.paired = [Device(id: "pc", name: "Windows workstation with three landscape displays and a very long device name", os: "Windows")]
        model.state.sharing = false
        try assertSnapshot("devices-offline-compact", view: DevicesView(model: model), size: CGSize(width: 440, height: 440))
    }

    @Test func testTransferFailure() throws {
        let model = fixture()
        model.state.failures = ["Couldn’t send files to \"Windows PC\": the connection closed before the transfer was saved."]
        model.state.notice = Notice(id: 1, text: "Sent 2 MB to \"Windows PC\".")
        try assertSnapshot("transfer-failure", view: VStack(spacing: 0) { MessagesView(model: model); DevicesView(model: model) }, size: CGSize(width: 680, height: 500))
    }

    @Test func testArrangement() throws {
        let model = fixture()
        try assertSnapshot("arrangement", view: ArrangementView(model: model), size: CGSize(width: 760, height: 520))
        model.state.arrangement.peers[0].placed = false
        try assertSnapshot("arrangement-unplaced-dark", view: ArrangementView(model: model), size: CGSize(width: 490, height: 500), dark: true)
    }

    @Test func testPairingAndDiscovery() throws {
        let model = fixture()
        model.state.pairing = Pairing(id: "pc", name: "Windows PC", code: "042916", incoming: true, stage: .confirm)
        try assertSnapshot("pairing-confirm", view: PairingView(model: model), size: CGSize(width: 420, height: 280))
        model.state.pairing?.stage = .waiting
        try assertSnapshot("pairing-waiting-dark", view: PairingView(model: model), size: CGSize(width: 420, height: 280), dark: true)
        model.state.nearby = [Device(id: "new", name: "Nearby MacBook Pro", os: "macOS")]
        try assertSnapshot("discovery", view: DiscoveryView(model: model), size: CGSize(width: 460, height: 380))
        model.state.nearby = []
        try assertSnapshot("discovery-empty-dark", view: DiscoveryView(model: model), size: CGSize(width: 460, height: 380), dark: true)
    }

    @Test func testSettings() throws {
        let model = fixture()
        // Fixture mode prevents any login-item changes. Permission rows show the
        // process's real read-only permission state and are excluded from comparison.
        try assertSnapshot("settings-input", view: SettingsView(model: model), size: CGSize(width: 600, height: 660), crop: CGRect(x: 0, y: 0, width: 600, height: 410))
    }

    @Test func testDecodesTheBridgeContract() throws {
        // Written by the Rust bridge's tests from its real types, so drift fails here.
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("Fixtures/snapshot.json")
        let snapshot = try Snapshot.decode(Data(contentsOf: url))
        #expect(snapshot.ready)
        #expect(snapshot.stateDir == "/tmp/legato-fixture")
        #expect(snapshot.settings.pushDistance == 30)
        #expect(snapshot.settings.controller == nil)
        #expect(snapshot.paired.map(\.os) == ["Windows", "macOS"])
        #expect(snapshot.paired.first?.active == true)
        #expect(snapshot.pairing?.stage == .confirm)
        #expect(snapshot.notice?.id == 3)
        #expect(snapshot.reveal?.paths.count == 1)
        #expect(snapshot.arrangement.localPrimary == 0)
        #expect(snapshot.arrangement.peers.first?.bounds == CGRect(x: -416, y: -1440, width: 2560, height: 1440))
        #expect(snapshot.arrangement.problems.count == 1)
    }

    private func fixture() -> AppModel {
        let model = AppModel()
        model.fixture = true
        model.state.ready = true
        model.state.version = "0.3.0-alpha.30"
        model.state.id = "mac-fixture"
        model.state.name = "MacBook Pro"
        model.state.stateDir = "/tmp/legato-fixture"
        model.state.sharing = true
        model.state.paired = [
            Device(id: "pc", name: "Windows PC", os: "Windows", connected: true, path: "direct · 2 ms", active: true),
            Device(id: "laptop", name: "Travel laptop", os: "Windows")
        ]
        model.state.arrangement = Arrangement(
            local: [DeskRect(x: 0, y: 0, width: 1728, height: 1117)],
            peers: [ArrangedPeer(id: "pc", name: "Windows PC", placed: true, displays: [
                DeskRect(x: -2976, y: -1440, width: 2560, height: 1440),
                DeskRect(x: -416, y: -1440, width: 2560, height: 1440),
                DeskRect(x: 2144, y: -1440, width: 2560, height: 1440)
            ])])
        model.state.arrangement.localPrimary = 0
        model.state.arrangement.peers[0].primary = 1
        return model
    }

    private func assertSnapshot<V: View>(_ name: String, view: V, size: CGSize, dark: Bool = false, crop: CGRect? = nil) throws {
        let image = try render(view, size: size, dark: dark)
        let actual: CGImage
        if let crop {
            let pixels = CGRect(x: crop.minX * scale, y: crop.minY * scale, width: crop.width * scale, height: crop.height * scale)
            guard let cropped = image.cropping(to: pixels) else { throw SnapshotError.renderFailed }
            actual = cropped
        } else { actual = image }
        let png = NSBitmapImageRep(cgImage: actual).representation(using: .png, properties: [:])!
        try FileManager.default.createDirectory(at: artifacts, withIntermediateDirectories: true)
        try png.write(to: artifacts.appendingPathComponent(name + ".png"))
        let expectedSize = crop?.size ?? size
        #expect(actual.width == Int(expectedSize.width * scale))
        #expect(actual.height == Int(expectedSize.height * scale))
        let lhs = try rgba(actual)
        // A blank render would make comparisons pass without UI coverage.
        var colors = Set<UInt32>()
        for index in stride(from: 0, to: lhs.count, by: 4) {
            let red = UInt32(lhs[index]) << 16
            let green = UInt32(lhs[index + 1]) << 8
            colors.insert(red | green | UInt32(lhs[index + 2]))
        }
        #expect(colors.count > 64, "Rendered snapshot is blank or missing native content")
        let baselineURL = snapshots.appendingPathComponent(name + ".png")
        if ProcessInfo.processInfo.environment["LEGATO_UPDATE_SNAPSHOTS"] == "1" {
            try FileManager.default.createDirectory(at: snapshots, withIntermediateDirectories: true)
            try png.write(to: baselineURL)
            let metadata: [String: Any] = [
                "macOS": ProcessInfo.processInfo.operatingSystemVersionString,
                "architecture": "arm64",
                "pixelsPerPoint": Int(scale),
                "locale": "en_US",
                "renderer": "NSHostingView.cacheDisplay at a fixed 2x scale; unordered offscreen window",
                "comparison": "0.5% changed pixels, RGB threshold 16"
            ]
            try JSONSerialization.data(withJSONObject: metadata, options: [.prettyPrinted, .sortedKeys]).write(to: snapshots.appendingPathComponent("metadata.json"))
        }
        // Hosted runner OS patches can change native system controls. Smoke mode
        // still renders all fixtures and checks geometry/nonblank native content.
        if ProcessInfo.processInfo.environment["LEGATO_SNAPSHOT_SMOKE"] == "1" { return }
        guard let baseline = NSImage(contentsOf: baselineURL)?.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
            Issue.record("Missing baseline \(baselineURL.path). Set LEGATO_UPDATE_SNAPSHOTS=1 to record it.")
            return
        }
        #expect(actual.width == baseline.width)
        #expect(actual.height == baseline.height)
        guard actual.width == baseline.width, actual.height == baseline.height else { return }
        let rhs = try rgba(baseline)
        var changed = 0
        for index in stride(from: 0, to: lhs.count, by: 4) {
            // Ignore tiny antialiasing/color-management variation. Animating native
            // spinners are allowed by the small changed-pixel fraction below.
            if (0..<3).contains(where: { abs(Int(lhs[index + $0]) - Int(rhs[index + $0])) > 16 }) { changed += 1 }
        }
        let fraction = Double(changed) / Double(actual.width * actual.height)
        #expect(fraction <= 0.005, "\(name): \(String(format: "%.2f", fraction * 100))% pixels differ; actual image: \(artifacts.path)")
    }

    private func render<V: View>(_ view: V, size: CGSize, dark: Bool) throws -> CGImage {
        _ = NSApplication.shared
        let hosting = NSHostingView(rootView: view.frame(width: size.width, height: size.height).background(Color(nsColor: .windowBackgroundColor)).environment(\.colorScheme, dark ? .dark : .light).environment(\.locale, Locale(identifier: "en_US")).transaction { $0.animation = nil })
        hosting.frame = CGRect(origin: .zero, size: size)
        hosting.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        let window = FixedScaleWindow(contentRect: hosting.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        window.contentView = hosting
        window.appearance = hosting.appearance
        window.isReleasedWhenClosed = false
        // Attaching content to an unordered offscreen window renders SwiftUI
        // content without ordering a window, activating the app or taking focus.
        hosting.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        hosting.layoutSubtreeIfNeeded()
        // A bitmap of fixed pixel size: the result must not depend on the attached
        // displays' scale, which `bitmapImageRepForCachingDisplay` would use.
        guard let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale),
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
        else { throw SnapshotError.renderFailed }
        bitmap.size = size
        hosting.cacheDisplay(in: hosting.bounds, to: bitmap)
        window.contentView = nil
        window.close()
        guard let image = bitmap.cgImage else { throw SnapshotError.renderFailed }
        // Flatten onto an opaque background so transparent pixels compare consistently.
        let pixels = CGSize(width: size.width * scale, height: size.height * scale)
        guard let context = CGContext(data: nil, width: Int(pixels.width), height: Int(pixels.height), bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { throw SnapshotError.renderFailed }
        context.setFillColor((dark ? NSColor.black : NSColor.white).cgColor)
        context.fill(CGRect(origin: .zero, size: pixels))
        context.draw(image, in: CGRect(origin: .zero, size: pixels))
        guard let flattened = context.makeImage() else { throw SnapshotError.renderFailed }
        return flattened
    }

    private func rgba(_ image: CGImage) throws -> [UInt8] {
        var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
        let rendered = bytes.withUnsafeMutableBytes { raw -> Bool in
            guard let context = CGContext(data: raw.baseAddress, width: image.width, height: image.height, bitsPerComponent: 8, bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        guard rendered else { throw SnapshotError.renderFailed }
        return bytes
    }
    private enum SnapshotError: Error { case renderFailed }
    private let scale: CGFloat = 2
}

/// Renders as on a Retina display whatever displays this Mac has attached.
private final class FixedScaleWindow: NSWindow {
    override var backingScaleFactor: CGFloat { 2 }
}

