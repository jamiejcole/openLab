// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

@main
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation, NSMenuDelegate {
    /// `-OpenLabEngine fake` runs the in-process fake engine (UI tests); default is the real one.
    @MainActor private(set) lazy var engineSession = EngineMode.fromLaunchArguments().makeSession()
    @MainActor private(set) lazy var appController = AppController(session: engineSession)

    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        app.run()
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.appearance = NSAppearance(named: .darkAqua)
        let menu = MainMenu.make()
        menu.transport.delegate = self
        NSApp.mainMenu = menu.bar
        NSApp.windowsMenu = menu.windows
        NSApp.helpMenu = menu.help
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // The launcher shows until the engine reports a workspace (fake engine: at once).
        appController.showLauncher()
        appController.installKeyMonitor()
        engineSession.start()
        NSApp.activate(ignoringOtherApps: true)
    }

    /// A show must survive an accidental window close: never quit, and bring a window back.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        appController.reopen()
        return false
    }

    /// One save prompt per dirty workspace, then `goodbye { shutdown }`; Cancel aborts the quit.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if appController.quitting { return .terminateCancel }
        Task { @MainActor in
            let ok = await appController.prepareToQuit()
            NSApp.reply(toApplicationShouldTerminate: ok)
        }
        return .terminateLater
    }

    @MainActor @objc func newWorkspace(_ sender: Any?) { Task { await appController.newWorkspace() } }
    @MainActor @objc func openWorkspace(_ sender: Any?) { Task { await appController.openWorkspace() } }

    @MainActor @objc func showLog(_ sender: Any?) { appController.showLog() }

    @MainActor @objc func relinkMedia(_ sender: Any?) {
        guard let store = appController.frontController?.store else { return }
        Task { await appController.relinkMedia(on: store) }
    }

    @MainActor @objc func transportCommand(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return
        }
        // isARepeat raises on non-key events (a menu click).
        let event = NSApp.currentEvent
        let isRepeat = event?.type == .keyDown && event?.isARepeat == true
        Task { await appController.perform(command, isRepeat: isRepeat) }
    }

    @MainActor @objc func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(relinkMedia(_:)) || item.action == #selector(showLog(_:)) {
            return appController.frontController != nil
        }
        guard let raw = item.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return true
        }
        return appController.canPerform(command)
    }

    /// The Transport menu shows the key window's workspace keys.
    @MainActor func menuNeedsUpdate(_ menu: NSMenu) {
        MainMenu.applyKeys(to: menu, appController.frontKeys)
    }
}
