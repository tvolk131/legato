import ServiceManagement
import SwiftUI

/// Started at login by the agent in Contents/Library/LaunchAgents: stay in the menu bar.
let launchedHidden = CommandLine.arguments.contains("--hidden")

@MainActor final class AppDelegate: NSObject, NSApplicationDelegate {
    static var model: AppModel?
    private var quitting = false

    func applicationWillFinishLaunching(_ notification: Notification) {
        guard let id = Bundle.main.bundleIdentifier else { return }
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: id)
            .filter { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier }
        if !others.isEmpty {
            // Only one Legato can use this Mac's identity. Show the running one instead.
            if !launchedHidden {
                DistributedNotificationCenter.default().postNotificationName(
                    Self.showNotification(id), object: nil, deliverImmediately: true)
            }
            exit(0)
        }
        DistributedNotificationCenter.default().addObserver(
            forName: Self.showNotification(id), object: nil, queue: .main
        ) { _ in MainActor.assumeIsolated { AppDelegate.model?.showMainWindow() } }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard let model = Self.model else { return }
        LoginItem.migrateFromLaunchAgent()
        if !launchedHidden { NSApp.activate() }
        Task { await model.start() }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows { Self.model?.showMainWindow() }
        return true
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !quitting, let model = Self.model else { return .terminateNow }
        quitting = true
        // Sharing must stop first so this Mac's input is never left captured.
        Task {
            await model.shutdown()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    private static func showNotification(_ id: String) -> Notification.Name {
        Notification.Name("\(id).show")
    }
}

@main struct LegatoApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var model: AppModel

    init() {
        let model = AppModel()
        _model = State(initialValue: model)
        AppDelegate.model = model
    }

    var body: some Scene {
        Window("Legato", id: "main") {
            RootView(model: model)
                .background(DockPresence())
        }
        .defaultSize(width: 880, height: 620)
        .defaultLaunchBehavior(launchedHidden ? .suppressed : .presented)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Add a Device…") {
                    model.discovering = true
                    model.showMainWindow()
                }
                .keyboardShortcut("n")
                .disabled(!model.state.ready)
                Toggle("Sharing", isOn: Binding(get: { model.state.sharing }, set: { model.setSharing($0) }))
                    .disabled(!model.state.ready)
            }
        }

        Settings { SettingsView(model: model) }

        MenuBarExtra {
            Button("Open Legato") { model.showMainWindow() }
            Toggle("Sharing", isOn: Binding(get: { model.state.sharing }, set: { model.setSharing($0) }))
                .disabled(!model.state.ready)
            SettingsLink()
            Divider()
            Button("Quit Legato") { NSApp.terminate(nil) }.keyboardShortcut("q")
        } label: {
            WindowOpener(model: model)
        }
    }
}

/// The menu bar icon. It always exists, so it hands the model a way to open the main
/// window even while that window is closed.
private struct WindowOpener: View {
    var model: AppModel
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        Label("Legato", systemImage: "desktopcomputer.and.macbook")
            .onAppear { model.openMainWindow = { openWindow(id: "main") } }
    }
}

/// Shows Legato in the Dock and app switcher only while its main window is open,
/// like the menu bar app it is.
private struct DockPresence: NSViewRepresentable {
    func makeNSView(context: Context) -> ObserverView { ObserverView() }
    func updateNSView(_ view: ObserverView, context: Context) {}

    final class ObserverView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            NotificationCenter.default.removeObserver(self)
            guard let window else { return }
            NotificationCenter.default.addObserver(
                self, selector: #selector(closing), name: NSWindow.willCloseNotification, object: window)
            NotificationCenter.default.addObserver(
                self, selector: #selector(opening), name: NSWindow.didBecomeKeyNotification, object: window)
        }
        @objc private func closing() { NSApp.setActivationPolicy(.accessory) }
        @objc private func opening() { NSApp.setActivationPolicy(.regular) }
    }
}

/// Opening Legato at login, through the launch agent bundled in the app. The agent
/// passes `--hidden` and restarts Legato if it crashes, at most once a minute.
enum LoginItem {
    static var service: SMAppService? {
        Bundle.main.bundleIdentifier.map { SMAppService.agent(plistName: "\($0).login.plist") }
    }

    static var enabled: Bool { service?.status == .enabled }
    static var needsApproval: Bool { service?.status == .requiresApproval }

    static func set(_ on: Bool) throws {
        guard let service else { throw LegatoError("Login items need the Legato app bundle.") }
        if on { try service.register() } else { try service.unregister() }
    }

    /// The Iced app opened at login through ~/Library/LaunchAgents/Legato.plist. Replace
    /// it with this app's agent, keeping the person's choice.
    static func migrateFromLaunchAgent() {
        let old = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/LaunchAgents/Legato.plist")
        guard let plist = NSDictionary(contentsOf: old),
              let program = (plist["ProgramArguments"] as? [String])?.first,
              program.hasSuffix(".app/Contents/MacOS/Legato")
        else { return }
        do {
            try FileManager.default.removeItem(at: old)
            if !enabled { try set(true) }
        } catch {
            NSLog("Legato: couldn't move the login item to this app: \(error)")
        }
    }
}
