import SwiftUI
import UniformTypeIdentifiers

struct RootView: View {
    @Bindable var model: AppModel

    var body: some View {
        NavigationSplitView {
            List(Page.allCases, selection: $model.page) { page in
                Label(page.rawValue, systemImage: page.symbol).tag(page)
            }
            .navigationSplitViewColumnWidth(min: 150, ideal: 190, max: 260)
        } detail: {
            VStack(spacing: 0) {
                MessagesView(model: model)
                switch model.page {
                case .devices: DevicesView(model: model)
                case .arrangement: ArrangementView(model: model)
                }
            }
            .navigationTitle(model.page.rawValue)
            .toolbar {
                ToolbarItem {
                    Toggle("Sharing", isOn: Binding(get: { model.state.sharing }, set: { model.setSharing($0) }))
                        .disabled(!model.state.ready)
                }
                ToolbarItem {
                    Button("Add a Device", systemImage: "plus") { model.discovering = true }
                        .disabled(!model.state.ready)
                }
            }
        }
        .frame(minWidth: 680, minHeight: 500)
        .sheet(isPresented: $model.discovering) { DiscoveryView(model: model) }
        // Stop accepting pairing requests along with the window.
        .onDisappear { model.discovering = false }
        .sheet(item: Binding(get: { model.state.pairing }, set: { if $0 == nil { model.command("confirm_pair", ["accept": false]) } })) { _ in
            PairingView(model: model).interactiveDismissDisabled()
        }
        .confirmationDialog(
            "Unpair \(model.unpair?.name ?? "device")?",
            isPresented: Binding(get: { model.unpair != nil }, set: { if !$0 { model.unpair = nil } }),
            titleVisibility: .visible
        ) {
            Button("Unpair", role: .destructive) {
                if let device = model.unpair { model.command("unpair", ["id": device.id]) }
                model.unpair = nil
            }
        } message: {
            Text("Sharing with this device will stop. Pair again to reconnect.")
        }
        .alert(
            "Legato needs attention",
            isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })
        ) {
            Button("OK") { model.error = nil }
        } message: {
            Text(model.error ?? "")
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard let target = model.dropTarget else {
                model.error = "Connect a device first to send it files."
                return false
            }
            model.send(urls, to: target.id)
            return true
        }
    }
}

/// Transfer failures stay until dismissed; other news shows one at a time.
struct MessagesView: View {
    var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(model.state.failures.enumerated()), id: \.offset) { index, text in
                banner(text, symbol: "exclamationmark.triangle.fill", tint: .red) {
                    model.command("dismiss_failure", ["index": index])
                }
            }
            if let notice = model.state.notice {
                banner(notice.text, symbol: "info.circle", tint: .secondary) { model.command("dismiss_notice") }
            }
        }
    }

    private func banner(_ text: String, symbol: String, tint: Color, dismiss: @escaping () -> Void) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Label { Text(text).textSelection(.enabled) } icon: { Image(systemName: symbol).foregroundStyle(tint) }
            Spacer()
            Button("Dismiss", action: dismiss).buttonStyle(.borderless)
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.bar)
        .overlay(alignment: .bottom) { Divider() }
    }
}

struct DevicesView: View {
    var model: AppModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if model.state.ready {
                    Label("\(model.state.name) · macOS", systemImage: "laptopcomputer").foregroundStyle(.secondary)
                }
                if !model.state.ready {
                    if let error = model.state.error, !model.starting {
                        ContentUnavailableView {
                            Label("Legato couldn’t start", systemImage: "exclamationmark.triangle")
                        } description: {
                            Text(error)
                        } actions: {
                            Button("Try Again") { Task { await model.start() } }
                        }
                    } else {
                        ProgressView("Starting Legato…").frame(maxWidth: .infinity)
                    }
                } else if model.state.paired.isEmpty {
                    ContentUnavailableView {
                        Label("Connect your first device", systemImage: "desktopcomputer.and.macbook")
                    } description: {
                        Text("Open Legato on both devices on the same network, then choose Add a Device on both.")
                    } actions: {
                        Button("Add a Device…") { model.discovering = true }.buttonStyle(.borderedProminent)
                    }
                } else {
                    ForEach(model.state.paired) { device in
                        DeviceRow(model: model, device: device)
                    }
                    Text("Drop files or folders on this window to send them to the device you’re using.")
                        .font(.callout).foregroundStyle(.secondary)
                }
            }
            .padding(24).frame(maxWidth: 900, alignment: .leading).frame(maxWidth: .infinity)
        }
    }
}

private struct DeviceRow: View {
    var model: AppModel
    var device: Device

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .top) {
                    Image(systemName: device.os == "Windows" ? "desktopcomputer" : "laptopcomputer").font(.title)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(device.name).font(.headline).textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(status).font(.callout).foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer()
                    Menu {
                        Button("Unpair…", role: .destructive) { model.unpair = device }
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .menuStyle(.borderlessButton).fixedSize()
                    .accessibilityLabel("Actions for \(device.name)")
                }
                Button("Send Files…", systemImage: "paperplane") { model.send(to: device) }
                    .disabled(!device.connected)
            }
            .padding(8).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var status: String {
        let os = device.os ?? "Device"
        if device.connected {
            if device.active { return "\(os) · Connected · in use" }
            return "\(os) · Connected" + (device.path.map { " · \($0)" } ?? "")
        }
        if model.state.sharing {
            return "\(os) · Not connected. Open Legato there and check that both devices are online."
        }
        return "\(os) · Sharing is off. Turn on Sharing to reconnect."
    }
}

struct DiscoveryView: View {
    var model: AppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Add a Device").font(.title2).bold()
            HStack {
                ProgressView().controlSize(.small)
                Text("Looking for Legato devices nearby…")
            }
            Text("Open Add a Device on the other device too, on the same network. Pairing requests are accepted only while this is open.")
                .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            List(model.state.nearby) { device in
                HStack {
                    Text(device.name)
                    if let os = device.os { Text(os).foregroundStyle(.secondary) }
                    Spacer()
                    Button("Pair…") {
                        dismiss()
                        model.command("pair", ["id": device.id])
                    }
                }
                .padding(.vertical, 6)
            }
            .overlay {
                if model.state.nearby.isEmpty {
                    Text("No devices found yet. Check that Add a Device is open on both devices.")
                        .foregroundStyle(.secondary).multilineTextAlignment(.center).padding()
                }
            }
            HStack {
                Spacer()
                Button("Done", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(24).frame(width: 460, height: 380)
    }
}

struct PairingView: View {
    var model: AppModel

    var body: some View {
        VStack(spacing: 20) {
            if let pairing = model.state.pairing {
                Text("Pair with \(pairing.name)?").font(.title2).bold().multilineTextAlignment(.center)
                if pairing.stage == .connecting {
                    ProgressView("Connecting…")
                } else {
                    Text(pairing.code)
                        .font(.system(size: 40, weight: .medium, design: .monospaced))
                        .textSelection(.enabled)
                        .accessibilityLabel("Pairing code \(pairing.code.map(String.init).joined(separator: " "))")
                    Text("Check that this code matches on both devices before accepting.")
                        .multilineTextAlignment(.center)
                    if pairing.stage == .waiting { ProgressView("Waiting for the other device…") }
                }
                HStack {
                    Button("Cancel", role: .cancel) { model.command("confirm_pair", ["accept": false]) }
                        .keyboardShortcut(.cancelAction)
                    if pairing.stage == .confirm {
                        Button("Codes Match") { model.command("confirm_pair", ["accept": true]) }
                            .buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                    }
                }
            }
        }
        .padding(28).frame(width: 420, height: 280)
    }
}
