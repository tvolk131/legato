import Foundation
import SwiftUI
import LegatoBridge

struct DeskRect: Codable, Equatable {
    var x: Double; var y: Double; var width: Double; var height: Double
    var cg: CGRect { CGRect(x: x, y: y, width: width, height: height) }
}
struct Device: Codable, Identifiable, Equatable {
    var id: String; var name: String; var os: String?
    var connected: Bool?; var path: String?; var active: Bool?
}
struct Pairing: Codable, Equatable {
    var id: String; var name: String; var code: String; var incoming: Bool; var stage: String
}
struct DeskPeer: Codable, Identifiable, Equatable {
    var id: String; var name: String; var placed: Bool; var displays: [DeskRect]; var primary: [Int]?
}
struct Arrangement: Codable, Equatable {
    var local: [DeskRect] = []; var peers: [DeskPeer] = []; var problems: [String] = []; var local_primary: [Int]?
}
struct Preferences: Codable, Equatable {
    struct Clipboard: Codable, Equatable { var enabled = true }
    struct Keys: Codable, Equatable { var remap = "auto" }
    struct Scrolling: Codable, Equatable { var invert_wheel = false }
    struct Switching: Codable, Equatable { var push_distance = 30.0 }
    struct Control: Codable, Equatable { var controller: String? }
    var clipboard = Clipboard(); var keys = Keys(); var scrolling = Scrolling()
    var switching = Switching(); var control = Control()
}
struct Reveal: Codable, Equatable { var sequence: UInt64; var paths: [String] }
struct Snapshot: Codable, Equatable {
    var ready = false; var error: String?; var version = ""; var state_dir = ""
    var id = ""; var name = "This Mac"; var sharing = false
    var config = Preferences(); var paired: [Device] = []; var nearby: [Device] = []
    var pairing: Pairing?; var notices: [String] = []; var failures: [String] = []
    var arrangement = Arrangement(); var reveal: Reveal?
}

/// Serializes bridge calls off AppKit's thread. The engine retains all input/network state.
actor Bridge {
    func start() -> Bool { legato_start() }
    func snapshot() throws -> Snapshot {
        guard let pointer = legato_snapshot() else { throw CocoaError(.coderReadCorrupt) }
        defer { legato_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        struct Startup: Decodable { var ready: Bool; var error: String? }
        let startup = try JSONDecoder().decode(Startup.self, from: data)
        if !startup.ready { var state = Snapshot(); state.error = startup.error; return state }
        return try JSONDecoder().decode(Snapshot.self, from: data)
    }
    func command(_ json: String) throws {
        let pointer = json.withCString { legato_command($0) }
        guard let pointer else { throw CocoaError(.coderReadCorrupt) }
        defer { legato_free(pointer) }
        struct Reply: Decodable { var ok: Bool; var error: String? }
        let reply = try JSONDecoder().decode(Reply.self, from: Data(String(cString: pointer).utf8))
        if !reply.ok { throw NSError(domain: "Legato", code: 1, userInfo: [NSLocalizedDescriptionKey: reply.error ?? "The operation failed."]) }
    }
    func shutdown() { legato_shutdown() }
}

@MainActor @Observable final class AppModel {
    var state = Snapshot()
    var error: String?
    var discovering = false
    var unpair: Device?
    var page = "Devices"
    var loginEnabled = false
    var fixture = false
    @ObservationIgnored private let bridge = Bridge()
    @ObservationIgnored var openMainWindow: (() -> Void)?
    func run() async {
        guard !fixture else { return }
        _ = await bridge.start()
        while !Task.isCancelled {
            do {
                let next = try await bridge.snapshot()
                if state != next {
                    let incoming = next.pairing?.id != state.pairing?.id && next.pairing != nil
                    let reveal = next.reveal.flatMap { $0.sequence != state.reveal?.sequence ? $0 : nil }
                    state = next
                    if let reveal { NSWorkspace.shared.activateFileViewerSelecting(reveal.paths.map { URL(fileURLWithPath: $0) }) }
                    if incoming {
                        discovering = false
                        NSApp.setActivationPolicy(.regular)
                        openMainWindow?()
                        NSApp.activate(ignoringOtherApps: true)
                    }
                }
            } catch { self.error = error.localizedDescription }
            try? await Task.sleep(for: .milliseconds(250))
        }
    }
    func command(_ action: String, _ fields: [String: Any] = [:]) {
        guard !fixture else { return }
        var object = fields; object["action"] = action
        do {
            let json = String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
            Task {
                do { try await bridge.command(json) }
                catch { self.error = error.localizedDescription }
            }
        } catch { self.error = error.localizedDescription }
    }
    func setting(_ key: String, _ value: Any) { command("setting", ["key": key, "value": value]) }
    func send(to device: Device) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true; panel.allowsMultipleSelection = true
        panel.prompt = "Send"
        panel.begin { result in
            if result == .OK { self.send(panel.urls, to: device.id) }
        }
    }
    func send(_ urls: [URL], to id: String) {
        command("send_files", ["id": id, "paths": urls.filter(\.isFileURL).map(\.path)])
    }
    func shutdown() async { await bridge.shutdown() }
}
