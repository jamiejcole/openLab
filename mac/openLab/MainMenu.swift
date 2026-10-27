// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The main menu bar, built in code (no storyboards or XIBs).
@MainActor
enum MainMenu {
    struct Menus {
        let bar: NSMenu
        let edit: NSMenu
        let transport: NSMenu
        let windows: NSMenu
        let help: NSMenu
    }

    static func make(keys: TransportKeys = TransportKeys(settings: nil)) -> Menus {
        let edit = editMenu()
        let windows = menu(
            "menu.window",
            [
                item("menu.window.minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
                item("menu.window.zoom", #selector(NSWindow.performZoom(_:))),
                .separator(),
                .separator(),
                logItem(),
                item("menu.window.bringAllToFront", #selector(NSApplication.arrangeInFront(_:))),
            ]
        )
        let help = menu("menu.help", [item("menu.help.openLabHelp", #selector(NSApplication.showHelp(_:)), "?")])
        let transport = transportMenu(keys)
        let bar = NSMenu()
        for submenu in [appMenu(), fileMenu(), edit, viewMenu(), transport, windows, help] {
            let holder = NSMenuItem()
            holder.submenu = submenu
            bar.addItem(holder)
        }
        return Menus(bar: bar, edit: edit, transport: transport, windows: windows, help: help)
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
        menu(
            "menu.file",
            [
                item("menu.file.new", #selector(AppDelegate.newWorkspace(_:)), "n"),
                item("menu.file.open", #selector(AppDelegate.openWorkspace(_:)), "o"),
                .separator(),
                item("menu.file.relink", #selector(AppDelegate.relinkMedia(_:))),
                .separator(),
                item("menu.file.close", #selector(NSWindow.performClose(_:)), "w"),
            ]
        )
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

    /// QLab's View menu has Enter Edit Mode (shift-cmd-[) and Enter Show Mode (shift-cmd-]).
    private static func viewMenu() -> NSMenu {
        let edit = item("menu.view.editMode", #selector(AppDelegate.enterEditMode(_:)), "[")
        let show = item("menu.view.showMode", #selector(AppDelegate.enterShowMode(_:)), "]")
        for e in [edit, show] { e.keyEquivalentModifierMask = [.command, .shift] }
        return menu("menu.view", [edit, show])
    }

    /// Show Mode (SM-01) turns the editing items off by hand; Copy and Select All change nothing.
    static func lockEditItems(in edit: NSMenu, locked: Bool) {
        edit.autoenablesItems = !locked
        let editing = [
            Selector(("undo:")), Selector(("redo:")), #selector(NSText.cut(_:)), #selector(NSText.paste(_:)),
        ]
        for entry in edit.items {
            guard let action = entry.action else { continue }
            entry.isEnabled = !(locked && editing.contains(action))
        }
    }

    /// Items carry their command in `representedObject`; keys come from the key map, so they follow
    /// a remap (see `applyKeys`). Panic has no key equivalent here: Esc is handled app-wide.
    private static func transportMenu(_ keys: TransportKeys) -> NSMenu {
        var items: [NSMenuItem] = []
        for command in TransportCommand.allCases {
            if command == .playheadPrevious || command == .loadSelected { items.append(.separator()) }
            let entry = item(command.titleKey, #selector(AppDelegate.transportCommand(_:)))
            entry.representedObject = command.rawValue
            items.append(entry)
        }
        let transport = menu("menu.transport", items)
        applyKeys(to: transport, keys)
        return transport
    }

    static func applyKeys(to transport: NSMenu, _ keys: TransportKeys) {
        for entry in transport.items {
            guard let raw = entry.representedObject as? String, let command = TransportCommand(rawValue: raw)
            else { continue }
            let chord = keys.chord(for: command)
            entry.keyEquivalent = chord?.key ?? ""
            entry.keyEquivalentModifierMask = chord?.modifiers ?? []
        }
    }

    private static func logItem() -> NSMenuItem {
        let log = item("menu.window.log", #selector(AppDelegate.showLog(_:)), "l")
        log.keyEquivalentModifierMask = [.command, .option]
        return log
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
