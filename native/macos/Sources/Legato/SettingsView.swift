import SwiftUI
import ServiceManagement
import ApplicationServices
import CoreGraphics

struct SettingsView: View {
    @Bindable var model: AppModel
    @State private var accessibility = AXIsProcessTrusted()
    @State private var monitoring = CGPreflightListenEventAccess()
    @State private var recording = CGPreflightScreenCaptureAccess()
    @State private var push: Double?
    @Environment(\.scenePhase) private var phase
    var controller: String {
        guard let saved = model.state.config.control.controller else { return "" }
        return ([model.state.id] + model.state.paired.map(\.id)).first(where: { $0.hasPrefix(saved) }) ?? saved
    }
    var body: some View {
        Form {
            Section("Input") {
                Picker("Control from", selection: Binding(get: { controller }, set: { model.setting("controller", $0.isEmpty ? NSNull() : $0 as Any) })) {
                    Text("Any device").tag("")
                    Text("Only this device").tag(model.state.id)
                    ForEach(model.state.paired) { Text("Only \($0.name)").tag($0.id) }
                    if !controller.isEmpty && controller != model.state.id && !model.state.paired.contains(where: { $0.id == controller }) { Text("Unavailable device").tag(controller) }
                }
                LabeledContent("Edge resistance") {
                    Slider(value: Binding(get: { push ?? model.state.config.switching.push_distance }, set: { push = $0 }), in: 0...100, step: 5) { editing in
                        if !editing, let push { model.setting("push_distance", push); self.push = nil }
                    }.accessibilityLabel("Edge resistance")
                    Text("\(Int(push ?? model.state.config.switching.push_distance))").monospacedDigit().frame(width: 30)
                }
                Text("How far to push past an edge before switching devices.").font(.caption).foregroundStyle(.secondary)
                Toggle("Mac Command key", isOn: Binding(get: { model.state.config.keys.remap == "auto" }, set: { model.setting("remap", $0 ? "auto" : "none") }))
                Text("Use the key next to the space bar as Command when driving a Mac.").font(.caption).foregroundStyle(.secondary)
                Toggle("Reverse mouse wheel", isOn: Binding(get: { model.state.config.scrolling.invert_wheel }, set: { model.setting("invert_wheel", $0) }))
            }
            Section("Sharing and startup") {
                Toggle("Share clipboard", isOn: Binding(get: { model.state.config.clipboard.enabled }, set: { model.setting("clipboard", $0) }))
                Toggle("Open at login", isOn: Binding(get: { model.loginEnabled }, set: { setLogin($0) }))
                if SMAppService.mainApp.status == .requiresApproval {
                    Button("Review Login Item in System Settings") { SMAppService.openSystemSettingsLoginItems() }
                }
            }
            Section("Permissions") {
                permission("Accessibility", granted: accessibility, pane: "Privacy_Accessibility")
                permission("Input Monitoring", granted: monitoring, pane: "Privacy_ListenEvent")
                permission("Screen Recording", granted: recording, pane: "Privacy_ScreenCapture")
                Text("Input sharing needs Accessibility and Input Monitoring. Hosting an extra display also needs Screen Recording.").font(.caption).foregroundStyle(.secondary)
            }
            Section("About") {
                LabeledContent("Version", value: model.state.version)
                LabeledContent("Device ID") { Text(model.state.id).textSelection(.enabled).font(.caption).lineLimit(3) }
                Button("Show Settings Folder in Finder") { NSWorkspace.shared.open(URL(fileURLWithPath: model.state.state_dir)) }.disabled(model.state.state_dir.isEmpty)
            }
        }.formStyle(.grouped).frame(width: 600, height: 660)
        .onAppear { refresh() }.onChange(of: phase) { _, _ in refresh() }
    }
    func refresh() {
        accessibility = AXIsProcessTrusted(); monitoring = CGPreflightListenEventAccess(); recording = CGPreflightScreenCaptureAccess()
        model.loginEnabled = SMAppService.mainApp.status == .enabled
    }
    func setLogin(_ enabled: Bool) {
        guard !model.fixture else { return }
        do { if enabled { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }; refresh() }
        catch { model.error = error.localizedDescription }
    }
    func permission(_ name: String, granted: Bool, pane: String) -> some View {
        LabeledContent(name) {
            if granted { Label("Allowed", systemImage: "checkmark.circle").foregroundStyle(.secondary) }
            else { Button("Open Settings…") { if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") { NSWorkspace.shared.open(url) } } }
        }
    }
}
