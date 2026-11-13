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
                Toggle("Mac Command key", isOn: Binding(get: { settings.remap }, set: { model.setting("remap", $0) }))
                Text("When a PC keyboard drives a Mac, Alt acts as Command and Windows as Option.")
                    .font(.caption).foregroundStyle(.secondary)
                Toggle("Reverse mouse wheel", isOn: Binding(get: { settings.invertWheel }, set: { model.setting("invert_wheel", $0) }))
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
