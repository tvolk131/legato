import SwiftUI

@MainActor final class AppDelegate: NSObject, NSApplicationDelegate {
    static var model: AppModel?
    private var quitting = false
    private var engineTask: Task<Void, Never>?
    func applicationDidFinishLaunching(_ notification: Notification) {
        if let model = Self.model { engineTask = Task { await model.run() } }
    }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !quitting, let model = Self.model else { return .terminateNow }
        quitting = true
        Task { await model.shutdown(); sender.reply(toApplicationShouldTerminate: true) }
        return .terminateLater
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
}
@main struct LegatoApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var model: AppModel
    init() {
        let model = AppModel()
        _model = State(initialValue: model)
        AppDelegate.model = model
    }
    @Environment(\.openWindow) private var openWindow
    var body: some Scene {
        Window("Legato", id: "main") {
            RootView(model: model)
                .background(WindowLifecycle())
                .onChange(of: model.state.pairing?.id) { _, id in
                    if id != nil { model.discovering = false; openWindow(id: "main"); NSApp.activate(ignoringOtherApps: true) }
                }
        }.defaultSize(width: 880, height: 620)
            .commands {
                CommandGroup(after: .newItem) {
                    Button("Add a Device…") { model.discovering = true; openWindow(id: "main") }.keyboardShortcut("n")
                    Toggle("Sharing", isOn: Binding(get: { model.state.sharing }, set: { model.command("sharing", ["enabled": $0]) }))
                }
            }
        Settings { SettingsView(model: model) }
        MenuBarExtra {
            Button("Open Legato") { openWindow(id: "main"); NSApp.activate(ignoringOtherApps: true) }
            Toggle("Sharing", isOn: Binding(get: { model.state.sharing }, set: { model.command("sharing", ["enabled": $0]) }))
            SettingsLink()
            Divider()
            Button("Quit Legato") { NSApp.terminate(nil) }.keyboardShortcut("q")
        } label: {
            Label("Legato", systemImage: "desktopcomputer.and.macbook")
                .onAppear { model.openMainWindow = { openWindow(id: "main") } }
        }
    }
}

struct WindowLifecycle: NSViewRepresentable {
    func makeNSView(context: Context) -> ObserverView { ObserverView() }
    func updateNSView(_ view: ObserverView, context: Context) {}
    class ObserverView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            NotificationCenter.default.removeObserver(self)
            guard let window else { return }
            NotificationCenter.default.addObserver(self, selector: #selector(closing), name: NSWindow.willCloseNotification, object: window)
            NotificationCenter.default.addObserver(self, selector: #selector(opening), name: NSWindow.didBecomeKeyNotification, object: window)
        }
        @objc private func closing() { NSApp.setActivationPolicy(.accessory) }
        @objc private func opening() { NSApp.setActivationPolicy(.regular) }
    }
}
