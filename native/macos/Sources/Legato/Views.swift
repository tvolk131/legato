import SwiftUI
import UniformTypeIdentifiers

struct RootView: View {
    @Bindable var model: AppModel
    var body: some View {
        NavigationSplitView {
            List(selection: $model.page) {
                Label("Devices", systemImage: "desktopcomputer").tag("Devices")
                Label("Arrangement", systemImage: "rectangle.3.group").tag("Arrangement")
            }
            .navigationSplitViewColumnWidth(min: 150, ideal: 190, max: 260)
        } detail: {
            VStack(spacing: 0) {
                MessagesView(model: model)
                if model.page == "Arrangement" { ArrangementView(model: model) }
                else { DevicesView(model: model) }
            }
            .navigationTitle(model.page)
            .toolbar {
                ToolbarItem { Toggle("Sharing", isOn: Binding(get: { model.state.sharing }, set: { model.command("sharing", ["enabled": $0]) })).disabled(!model.state.ready) }
                ToolbarItem { Button("Add a Device", systemImage: "plus") { model.discovering = true }.disabled(!model.state.ready) }
            }
        }
        .frame(minWidth: 680, minHeight: 500)
        .sheet(isPresented: $model.discovering) { DiscoveryView(model: model) }
        .sheet(item: Binding(get: { model.state.pairing }, set: { if $0 == nil { model.command("confirm_pair", ["accept": false]) } })) { _ in
            PairingView(model: model).interactiveDismissDisabled()
        }
        .confirmationDialog("Unpair device?", isPresented: Binding(get: { model.unpair != nil }, set: { if !$0 { model.unpair = nil } }), titleVisibility: .visible) {
            Button("Unpair", role: .destructive) {
                if let device = model.unpair { model.command("unpair", ["id": device.id]) }; model.unpair = nil
            }
        } message: { Text("Sharing with \(model.unpair?.name ?? "this device") will stop. Pair again to reconnect.") }
        .alert("Legato needs attention", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {
            Button("OK") { model.error = nil }
        } message: { Text(model.error ?? "") }
        .dropDestination(for: URL.self) { urls, _ in
            guard let target = model.state.paired.first(where: { $0.active == true && $0.connected == true }) ?? model.state.paired.first(where: { $0.connected == true }) else { return false }
            model.send(urls, to: target.id); return true
        }
    }
}
extension Pairing: Identifiable {}

struct MessagesView: View {
    var model: AppModel
    var body: some View {
        if let error = model.state.error { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red).padding() }
        ScrollView { ForEach(Array(model.state.failures.enumerated()), id: \.offset) { index, text in
            HStack { Label(text, systemImage: "exclamationmark.triangle"); Spacer(); Button("Dismiss") { model.command("dismiss_failures", ["index": index]) } }
                .padding(10).background(.red.opacity(0.08))
        } }.frame(maxHeight: model.state.failures.isEmpty ? 0 : 100)
        if let notice = model.state.notices.last {
            HStack { Text(notice).textSelection(.enabled); Spacer(); Button("Dismiss") { model.command("dismiss_notices") } }.padding(10).background(.quaternary)
        }
    }
}
struct DevicesView: View {
    var model: AppModel
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Label("\(model.state.name) · macOS", systemImage: "laptopcomputer").foregroundStyle(.secondary)
                if !model.state.ready { ProgressView("Starting Legato…") }
                else if model.state.paired.isEmpty {
                    ContentUnavailableView {
                        Label("Connect your first device", systemImage: "desktopcomputer.and.macbook")
                    } description: { Text("Open Legato on both devices on the same network, then add a device.") }
                    actions: { Button("Add a Device…") { model.discovering = true }.buttonStyle(.borderedProminent) }
                } else {
                    ForEach(model.state.paired) { device in
                        GroupBox {
                            VStack(alignment: .leading, spacing: 12) {
                                HStack(alignment: .top) {
                                    Image(systemName: device.os?.lowercased().contains("windows") == true ? "desktopcomputer" : "laptopcomputer").font(.title)
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(device.name).font(.headline).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                                        Text(status(device)).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                                    }
                                    Spacer()
                                    Menu { Button("Unpair…", role: .destructive) { model.unpair = device } } label: { Image(systemName: "ellipsis") }.menuStyle(.borderlessButton).fixedSize().accessibilityLabel("Actions for \(device.name)")
                                }
                                Button("Send Files…", systemImage: "paperplane") { model.send(to: device) }.disabled(device.connected != true)
                            }.padding(8).frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    Text("Drop files or folders here to send them to the device you’re using.").font(.callout).foregroundStyle(.secondary)
                }
            }.padding(24).frame(maxWidth: 900, alignment: .leading).frame(maxWidth: .infinity)
        }
    }
    func status(_ device: Device) -> String {
        let os = device.os ?? "Device"
        if device.connected == true { return "\(os) · \(device.active == true ? "Connected · in use" : ("Connected " + (device.path ?? "")))" }
        return "\(os) · " + (model.state.sharing ? "Not connected. Open Legato there and check that both devices are online." : "Sharing is off. Turn on Sharing to reconnect.")
    }
}
struct DiscoveryView: View {
    var model: AppModel
    var unpaired: [Device] { model.state.nearby.filter { nearby in !model.state.paired.contains { $0.id == nearby.id } } }
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Add a Device").font(.title2).bold()
            HStack { ProgressView().controlSize(.small); Text("Looking for Legato devices nearby…") }
            Text("Open Legato on the other device and connect both devices to the same network.").foregroundStyle(.secondary)
            List(unpaired) { device in
                HStack { Text(device.name); Spacer(); Button("Pair…") { dismiss(); model.command("pair", ["id": device.id]) } }.padding(.vertical, 6)
            }.overlay { if unpaired.isEmpty { Text("No devices found yet. Check that Legato is open on both devices.").foregroundStyle(.secondary).padding() } }
            HStack { Spacer(); Button("Done", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction) }
        }.padding(24).frame(width: 460, height: 380)
    }
}
struct PairingView: View {
    var model: AppModel
    var body: some View {
        VStack(spacing: 20) {
            if let pairing = model.state.pairing {
                Text("Pair with \(pairing.name)?").font(.title2).bold().multilineTextAlignment(.center)
                if pairing.stage == "connecting" { ProgressView("Connecting…") }
                else {
                    Text(pairing.code).font(.system(size: 40, weight: .medium, design: .monospaced)).textSelection(.enabled).accessibilityLabel("Pairing code \(pairing.code)")
                    Text("Check that this code matches on both devices before accepting.").multilineTextAlignment(.center)
                    if pairing.stage == "waiting" { ProgressView("Waiting for the other device…") }
                }
                HStack {
                    Button("Cancel", role: .cancel) { model.command("confirm_pair", ["accept": false]) }.keyboardShortcut(.cancelAction)
                    if pairing.stage == "confirm" { Button("Codes Match") { model.command("confirm_pair", ["accept": true]) }.buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction) }
                }
            }
        }.padding(28).frame(width: 420, height: 280)
    }
}
