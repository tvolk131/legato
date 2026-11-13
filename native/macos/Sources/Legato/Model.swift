import AppKit
import Foundation
import LegatoBridge
import Observation

// The JSON contract with `crates/legato-swift-bridge`. Rust writes a sample of it to
// Tests/LegatoTests/Fixtures/snapshot.json, which the tests decode.

struct DeskRect: Decodable, Equatable {
    var x: Double, y: Double, width: Double, height: Double
    var cg: CGRect { CGRect(x: x, y: y, width: width, height: height) }
}

struct Device: Decodable, Identifiable, Equatable {
    var id: String
    var name: String
    /// "macOS" or "Windows", when known.
    var os: String?
    var connected = false
    /// How it's connected, e.g. "direct · 2 ms".
    var path: String?
    /// Input is flowing between this Mac and the device.
    var active = false
}

struct Pairing: Decodable, Identifiable, Equatable {
    enum Stage: String, Decodable { case connecting, confirm, waiting }
    var id: String
    var name: String
    var code: String
    var incoming: Bool
    var stage: Stage
}

struct Notice: Decodable, Equatable {
    var id: UInt64
    var text: String
}

struct Reveal: Decodable, Equatable {
    var sequence: UInt64
    var paths: [String]
}

struct ArrangedPeer: Decodable, Identifiable, Equatable {
    var id: String
    var name: String
    var placed: Bool
    var displays: [DeskRect]
    var primary: Int?
    var bounds: CGRect { displays.reduce(CGRect.null) { $0.union($1.cg) } }
}

struct Arrangement: Decodable, Equatable {
    var local: [DeskRect] = []
    var localPrimary: Int?
    var peers: [ArrangedPeer] = []
    var problems: [String] = []
}

struct Preferences: Decodable, Equatable {
    var clipboard = true
    /// Alt acts as Command and Win as Option when driving a Mac.
    var remap = true
    var invertWheel = false
    var pushDistance = 30.0
    /// The controlling device's id (or id prefix), if only one device may control.
    var controller: String?
}

struct Snapshot: Decodable, Equatable {
    var ready = false
    /// Why the engine isn't running.
    var error: String?
    var version = ""
    var id = ""
    var name = ""
    var stateDir = ""
    var sharing = false
    var settings = Preferences()
    var paired: [Device] = []
    var nearby: [Device] = []
    var pairing: Pairing?
    var notice: Notice?
    var failures: [String] = []
    var reveal: Reveal?
    var arrangement = Arrangement()

    static func decode(_ data: Data) throws -> Snapshot {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Snapshot.self, from: data)
    }
}

enum Page: String, CaseIterable, Identifiable {
    case devices = "Devices", arrangement = "Arrangement"
    var id: Self { self }
    var symbol: String { self == .devices ? "desktopcomputer" : "rectangle.3.group" }
}

/// Owns calls into the Rust engine, off the main thread. Some calls block briefly
/// (starting, turning sharing off, quitting) while input is handed back.
actor Bridge {
    func start() -> Bool { legato_start() }

    func snapshot() throws -> Snapshot {
        try Snapshot.decode(take(legato_snapshot()))
    }

    func command(_ json: String) throws {
        struct Reply: Decodable { var ok: Bool; var error: String? }
        let data = try take(json.withCString { legato_command($0) })
        let reply = try JSONDecoder().decode(Reply.self, from: data)
        if !reply.ok { throw LegatoError(reply.error ?? "The operation failed.") }
    }

    func shutdown() { legato_shutdown() }

    private func take(_ pointer: UnsafeMutablePointer<CChar>?) throws -> Data {
        guard let pointer else { throw LegatoError("Legato returned no data.") }
        defer { legato_free(pointer) }
        return Data(String(cString: pointer).utf8)
    }
}

struct LegatoError: LocalizedError {
    var errorDescription: String?
    init(_ message: String) { errorDescription = message }
}

@MainActor @Observable final class AppModel {
    /// The model the engine's change callback refreshes.
    private static weak var observed: AppModel?

    var state = Snapshot()
    /// An error to show in an alert.
    var error: String?
    var starting = false
    var discovering = false
    var unpair: Device?
    var page = Page.devices
    var loginEnabled = false
    /// Renders fixtures without starting the engine (screenshot tests).
    @ObservationIgnored var fixture = false
    /// Shows the main window; set by the scene, which owns `openWindow`.
    @ObservationIgnored var openMainWindow: (() -> Void)?
    @ObservationIgnored private let bridge = Bridge()
    @ObservationIgnored private var refreshing = false
    @ObservationIgnored private var refreshAgain = false
    @ObservationIgnored private var stopped = false

    func start() async {
        guard !fixture, !starting, !state.ready else { return }
        Self.observed = self
        // Called on an engine thread; coalesced in Rust until the next snapshot.
        legato_set_observer { Task { @MainActor in AppModel.observed?.refresh() } }
        starting = true
        _ = await bridge.start()
        starting = false
        refresh()
    }

    /// Reads the engine's state once more, now or right after a read in progress.
    func refresh() {
        guard !fixture, !stopped else { return }
        if refreshing { refreshAgain = true; return }
        refreshing = true
        Task {
            do { apply(try await bridge.snapshot()) } catch { report(error) }
            refreshing = false
            if refreshAgain { refreshAgain = false; refresh() }
        }
    }

    private func apply(_ next: Snapshot) {
        guard next != state, !stopped else { return }
        let incoming = next.pairing != nil && next.pairing?.id != state.pairing?.id
        let reveal = next.reveal.flatMap { $0.sequence != state.reveal?.sequence ? $0 : nil }
        state = next
        if let reveal {
            NSWorkspace.shared.activateFileViewerSelecting(reveal.paths.map { URL(fileURLWithPath: $0) })
        }
        if incoming {
            discovering = false
            showMainWindow()
        }
    }

    func showMainWindow() {
        NSApp.setActivationPolicy(.regular)
        openMainWindow?()
        NSApp.activate()
    }

    func command(_ action: String, _ fields: [String: Any] = [:]) {
        guard !fixture else { return }
        var object = fields
        object["action"] = action
        do {
            let json = String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
            Task {
                do { try await bridge.command(json) } catch { report(error) }
            }
        } catch { report(error) }
    }

    func setting(_ key: String, _ value: Any) { command("setting", ["key": key, "value": value]) }

    func setSharing(_ on: Bool) { command("sharing", ["enabled": on]) }

    func send(to device: Device) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.prompt = "Send"
        panel.message = "Choose files or folders to send to \(device.name)."
        panel.begin { result in
            if result == .OK { self.send(panel.urls, to: device.id) }
        }
    }

    func send(_ urls: [URL], to id: String) {
        command("send_files", ["id": id, "paths": urls.filter(\.isFileURL).map(\.path)])
    }

    /// Where dropped files go: the device in use, else the first connected one.
    var dropTarget: Device? {
        state.paired.first { $0.active && $0.connected } ?? state.paired.first(where: \.connected)
    }

    func report(_ error: Error) {
        let text = error.localizedDescription
        if self.error != text { self.error = text }
    }

    /// Stops sharing and networking so input is back to normal before quitting.
    func shutdown() async {
        stopped = true
        guard !fixture else { return }
        await bridge.shutdown()
    }
}
