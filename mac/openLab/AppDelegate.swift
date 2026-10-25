// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

@main
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindow: NSWindow?

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
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 800),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = NSLocalizedString("window.main.title", comment: "Main window title")
        window.identifier = NSUserInterfaceItemIdentifier("main-window")
        window.isReleasedWhenClosed = false
        window.center()
        window.makeKeyAndOrderFront(nil)
        mainWindow = window
        NSApp.activate(ignoringOtherApps: true)
    }
}
