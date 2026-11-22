import AppKit
import ApplicationServices
import CoreGraphics
import ServiceManagement
import SwiftUI

struct SettingsView: View {
    @Bindable var model: AppModel
    @State private var accessibility = AXIsProcessTrusted()
    @State private var monitoring = CGPreflightListenEventAccess()
    @State private var recording = CGPreflightScreenCaptureAccess()
    @State private var needsApproval = false
    /// The edge resistance while its slider is being dragged.
    @State private var push: Double?
    @State private var addingKeyMapping = false
    @Environment(\.scenePhase) private var phase

    private var settings: Preferences { model.state.settings }

    /// The full id of the configured controller, which settings may store as a prefix.
    private var controller: String {
        guard let saved = settings.controller else { return "" }
        return ([model.state.id] + model.state.paired.map(\.id)).first { $0.hasPrefix(saved) } ?? saved
    }

    var body: some View {
        Form {
            Section("Input") {
                Picker("Control from", selection: Binding(get: { controller }, set: { model.setting("controller", $0.isEmpty ? NSNull() : $0) })) {
                    Text("Any device").tag("")
                    Text("Only this Mac").tag(model.state.id)
                    ForEach(model.state.paired) { Text("Only \($0.name)").tag($0.id) }
                    if !controller.isEmpty && controller != model.state.id && !model.state.paired.contains(where: { $0.id == controller }) {
                        Text("Unavailable device").tag(controller)
                    }
                }
                LabeledContent("Edge resistance") {
                    Slider(value: Binding(get: { push ?? settings.pushDistance }, set: { push = $0 }), in: 0...100, step: 5) { editing in
                        if !editing, let push {
                            model.setting("push_distance", push)
                            self.push = nil
                        }
                    }
                    .accessibilityLabel("Edge resistance")
                    Text("\(Int(push ?? settings.pushDistance))").monospacedDigit().frame(width: 30)
                }
                Text("How far to push past an edge before switching devices.").font(.caption).foregroundStyle(.secondary)
                Toggle("Swap Command and Control on Windows", isOn: Binding(get: { settings.remap }, set: { model.setting("remap", $0) }))
                Text("When this Mac’s keyboard drives Windows, Command acts as Ctrl so ⌘C and ⌘V work as expected.")
                    .font(.caption).foregroundStyle(.secondary)
                Toggle("Reverse mouse wheel", isOn: Binding(get: { settings.invertWheel }, set: { model.setting("invert_wheel", $0) }))
            }
            Section("Key mappings") {
                ForEach(settings.keyMappings) { mapping in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(mapping.fromLabel)
                            Text("Acts as \(mapping.toLabel) \(mapping.target.place)")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Remove", systemImage: "trash", role: .destructive) {
                            model.removeKeyMapping(mapping)
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.borderless)
                        .help("Remove key mapping")
                        .accessibilityLabel("Remove mapping for \(mapping.fromLabel)")
                    }
                }
                Button("Add a Key Mapping…") { addingKeyMapping = true }
                    .disabled(!model.state.ready)
                Text("Make a key on this keyboard act as a different key on your other devices. Mappings apply on top of the Command/Control swap.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Sharing and startup") {
                Toggle("Share clipboard", isOn: Binding(get: { settings.clipboard }, set: { model.setting("clipboard", $0) }))
                Toggle("Open at login", isOn: Binding(get: { model.loginEnabled }, set: { setLogin($0) }))
                if needsApproval {
                    Button("Allow in Login Items…") { SMAppService.openSystemSettingsLoginItems() }
                }
            }
            Section("Permissions") {
                permission("Accessibility", granted: accessibility, pane: "Privacy_Accessibility")
                permission("Input Monitoring", granted: monitoring, pane: "Privacy_ListenEvent")
                permission("Screen Recording", granted: recording, pane: "Privacy_ScreenCapture")
                Text("Input sharing needs Accessibility and Input Monitoring. Hosting an extra display also needs Screen Recording.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("About") {
                LabeledContent("Version", value: model.state.version)
                LabeledContent("Device ID") { Text(model.state.id).textSelection(.enabled).font(.caption).lineLimit(3) }
                Button("Show Settings Folder in Finder") {
                    NSWorkspace.shared.open(URL(fileURLWithPath: model.state.stateDir))
                }
                .disabled(model.state.stateDir.isEmpty)
            }
        }
        .formStyle(.grouped).frame(width: 600, height: 660)
        .sheet(isPresented: $addingKeyMapping) {
            AddKeyMappingView { target, from, to in
                model.addKeyMapping(target: target, from: from, to: to)
            }
        }
        .onAppear { refresh() }
        .onChange(of: phase) { refresh() }
    }

    private func refresh() {
        accessibility = AXIsProcessTrusted()
        monitoring = CGPreflightListenEventAccess()
        recording = CGPreflightScreenCaptureAccess()
        guard !model.fixture else { return }
        model.loginEnabled = LoginItem.enabled
        needsApproval = LoginItem.needsApproval
    }

    private func setLogin(_ enabled: Bool) {
        guard !model.fixture else { return }
        do { try LoginItem.set(enabled) } catch { model.report(error) }
        refresh()
    }

    private func permission(_ name: String, granted: Bool, pane: String) -> some View {
        LabeledContent(name) {
            if granted {
                Label("Allowed", systemImage: "checkmark.circle").foregroundStyle(.secondary)
            } else {
                Button("Open Settings…") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
                        NSWorkspace.shared.open(url)
                    }
                }
            }
        }
    }
}

struct AddKeyMappingView: View {
    @State var target: KeyMappingTarget
    @State var from: KeyOption?
    @State var to: String
    @State var listening: Bool
    var onAdd: (KeyMappingTarget, String, String) -> Void
    @Environment(\.dismiss) private var dismiss

    init(
        target: KeyMappingTarget = .windows,
        from: KeyOption? = nil,
        to: String = "",
        listening: Bool = true,
        onAdd: @escaping (KeyMappingTarget, String, String) -> Void
    ) {
        _target = State(initialValue: target)
        _from = State(initialValue: from)
        _to = State(initialValue: to)
        _listening = State(initialValue: listening)
        self.onAdd = onAdd
    }

    private var targets: [KeyOption] { KeyTargets.current.options(for: target) }

    private var canAdd: Bool { !listening && from != nil && !to.isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Add a Key Mapping").font(.title2).bold()

            VStack(alignment: .leading, spacing: 6) {
                Text("Where").font(.headline)
                Picker("Where", selection: $target) {
                    ForEach(KeyMappingTarget.allCases) { option in
                        Text(option.title).tag(option)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .onChange(of: target) { _, next in
                    let valid = KeyTargets.current.options(for: next).contains { $0.code == to }
                    if !valid { to = "" }
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Key on this keyboard").font(.headline)
                HStack(spacing: 12) {
                    if listening {
                        HStack(spacing: 8) {
                            Image(systemName: "keyboard")
                                .foregroundStyle(.tint)
                            Text("Press a key on this keyboard…")
                                .foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 12).padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8)
                                .stroke(.tint, lineWidth: 1.5)
                        }
                    } else if let from {
                        Text(from.label)
                            .font(.headline)
                            .padding(.horizontal, 12).padding(.vertical, 8)
                            .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 8))
                        Button("Change") { listening = true }
                    }
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Acts as").font(.headline)
                Picker("Acts as", selection: $to) {
                    Text("Choose a key…").tag("")
                    ForEach(targets) { option in
                        Text(option.label).tag(option.code)
                    }
                }
                .labelsHidden()
                if to == "Fn" {
                    Text("Apps that listen for fn, like dictation apps, see it held while you hold the key.")
                        .font(.caption).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            HStack {
                Spacer()
                if listening {
                    Button("Cancel", role: .cancel) { dismiss() }
                } else {
                    Button("Cancel", role: .cancel) { dismiss() }
                        .keyboardShortcut(.cancelAction)
                }
                Button("Add") {
                    guard let from, !to.isEmpty else { return }
                    onAdd(target, from.code, to)
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canAdd)
            }
        }
        .padding(24)
        .frame(width: 420)
        .background(KeyCaptureMonitor(active: listening) { key in
            from = key
            listening = false
        })
    }
}

/// Captures a single physical key press (including modifiers and fn) while the Add a Key
/// Mapping sheet is listening, without triggering macOS's alert sound or dismissing the
/// sheet on Escape.
private struct KeyCaptureMonitor: NSViewRepresentable {
    var active: Bool
    var onKey: (KeyOption) -> Void

    func makeNSView(context: Context) -> MonitorView {
        let view = MonitorView()
        view.onKey = onKey
        view.active = active
        return view
    }

    func updateNSView(_ view: MonitorView, context: Context) {
        view.onKey = onKey
        view.active = active
    }

    static func dismantleNSView(_ view: MonitorView, coordinator: ()) {
        view.removeMonitor()
    }

    final class MonitorView: NSView {
        var onKey: ((KeyOption) -> Void)?
        var active = false {
            didSet { updateMonitor() }
        }
        nonisolated(unsafe) private var monitor: Any?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            updateMonitor()
        }

        deinit {
            if let monitor {
                DispatchQueue.main.async { NSEvent.removeMonitor(monitor) }
            }
        }

        private func updateMonitor() {
            if active, window != nil {
                installMonitor()
            } else {
                removeMonitor()
            }
        }

        private func installMonitor() {
            guard monitor == nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { [weak self] event in
                guard let self, self.active, let window = self.window, event.window === window else {
                    return event
                }
                if event.type == .keyDown {
                    if event.isARepeat { return nil }
                    if let key = KeyTargets.macKey(for: event.keyCode) {
                        self.onKey?(key)
                    }
                    return nil
                }
                // `flagsChanged` fires for both presses and releases of modifier keys.
                if isModifierKeyDown(keyCode: event.keyCode, rawFlags: event.modifierFlags.rawValue),
                   let key = KeyTargets.macKey(for: event.keyCode) {
                    self.onKey?(key)
                }
                return event
            }
        }

        func removeMonitor() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
        }
    }
}

/// Whether a `flagsChanged` event represents a modifier key going down rather than up.
/// Matches the device-specific IOKit flag bits used by `crates/legato-macos/src/capture.rs`.
func isModifierKeyDown(keyCode: UInt16, rawFlags: UInt) -> Bool {
    switch keyCode {
    case 57: return true // Caps Lock toggles on each press.
    case 59: return rawFlags & 0x0001 != 0 // Left Control
    case 56: return rawFlags & 0x0002 != 0 // Left Shift
    case 60: return rawFlags & 0x0004 != 0 // Right Shift
    case 55: return rawFlags & 0x0008 != 0 // Left Command
    case 54: return rawFlags & 0x0010 != 0 // Right Command
    case 58: return rawFlags & 0x0020 != 0 // Left Option
    case 61: return rawFlags & 0x0040 != 0 // Right Option
    case 62: return rawFlags & 0x2000 != 0 // Right Control
    case 63: return rawFlags & 0x0080_0000 != 0 // fn (Globe)
    default: return false
    }
}
