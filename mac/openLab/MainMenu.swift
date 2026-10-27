// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The main menu bar, built in code (no storyboards or XIBs).
@MainActor
enum MainMenu {
    struct Menus {
        let bar: NSMenu
        let windows: NSMenu
        let help: NSMenu
    }

    static func make() -> Menus {
        let windows = menu(
            "menu.window",
            [
                item("menu.window.minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
                item("menu.window.zoom", #selector(NSWindow.performZoom(_:))),
                item("menu.window.main", #selector(AppDelegate.showMainWindow(_:)), "0"),
                .separator(),
                item("menu.window.bringAllToFront", #selector(NSApplication.arrangeInFront(_:))),
            ]
        )
        let help = menu("menu.help", [item("menu.help.openLabHelp", #selector(NSApplication.showHelp(_:)), "?")])
        let bar = NSMenu()
        for submenu in [appMenu(), fileMenu(), editMenu(), windows, help] {
            let holder = NSMenuItem()
            holder.submenu = submenu
            bar.addItem(holder)
        }
        return Menus(bar: bar, windows: windows, help: help)
    }

    private static func appMenu() -> NSMenu {
        let hideOthers = item(
            "menu.app.hideOthers", #selector(NSApplication.hideOtherApplications(_:)), "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        return menu(
            "menu.app",
            [
                item("menu.app.about", #selector(NSApplication.orderFrontStandardAboutPanel(_:))),
                .separator(),
                item("menu.app.hide", #selector(NSApplication.hide(_:)), "h"),
                hideOthers,
                item("menu.app.showAll", #selector(NSApplication.unhideAllApplications(_:))),
                .separator(),
                item("menu.app.quit", #selector(NSApplication.terminate(_:)), "q"),
            ]
        )
    }

    private static func fileMenu() -> NSMenu {
        menu("menu.file", [item("menu.file.close", #selector(NSWindow.performClose(_:)), "w")])
    }

    private static func editMenu() -> NSMenu {
        menu(
            "menu.edit",
            [
                item("menu.edit.undo", Selector(("undo:")), "z"),
                item("menu.edit.redo", Selector(("redo:")), "Z"),
                .separator(),
                item("menu.edit.cut", #selector(NSText.cut(_:)), "x"),
                item("menu.edit.copy", #selector(NSText.copy(_:)), "c"),
                item("menu.edit.paste", #selector(NSText.paste(_:)), "v"),
                item("menu.edit.selectAll", #selector(NSText.selectAll(_:)), "a"),
            ]
        )
    }

    private static func menu(_ key: String, _ items: [NSMenuItem]) -> NSMenu {
        let menu = NSMenu(title: localized(key))
        items.forEach(menu.addItem)
        return menu
    }

    private static func item(_ key: String, _ action: Selector, _ keyEquivalent: String = "") -> NSMenuItem {
        NSMenuItem(title: localized(key), action: action, keyEquivalent: keyEquivalent)
    }

    private static func localized(_ key: String) -> String {
        NSLocalizedString(key, comment: "")
    }
}
