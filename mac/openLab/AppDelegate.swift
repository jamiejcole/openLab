// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

@main
final class AppDelegate: NSObject, NSApplicationDelegate {
    @MainActor private(set) lazy var mainWindowController = MainWindowController()

    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        app.run()
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.appearance = NSAppearance(named: .darkAqua)
        let menu = MainMenu.make()
        NSApp.mainMenu = menu.bar
        NSApp.windowsMenu = menu.windows
        NSApp.helpMenu = menu.help
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        showMainWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// A show must survive an accidental window close: never quit, and bring the window back.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        showMainWindow(nil)
        return false
    }

    @MainActor @objc func showMainWindow(_ sender: Any?) {
        guard let window = mainWindowController.window else { return }
        if window.isMiniaturized { window.deminiaturize(nil) }
        window.makeKeyAndOrderFront(nil)
    }
}
