// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The main menu bar, built in code (no storyboards or XIBs).
@MainActor
enum MainMenu {
    struct Menus {
        let bar: NSMenu
        let edit: NSMenu
        let cues: NSMenu
        let transport: NSMenu
        let windows: NSMenu
        let help: NSMenu
    }

    static func make(keys: TransportKeys = TransportKeys(settings: nil)) -> Menus {
        let edit = editMenu()
        let cues = cuesMenu()
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
        for submenu in [appMenu(), fileMenu(), edit, cues, viewMenu(), transport, windows, help] {
            let holder = NSMenuItem()
            holder.submenu = submenu
            bar.addItem(holder)
        }
        return Menus(bar: bar, edit: edit, cues: cues, transport: transport, windows: windows, help: help)
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
                item("menu.file.recover", #selector(AppDelegate.recoverAutosaves(_:))),
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
                item("menu.edit.undo", #selector(AppDelegate.undoEdit(_:)), "z"),
                item("menu.edit.redo", #selector(AppDelegate.redoEdit(_:)), "Z"),
                .separator(),
                item("menu.edit.cut", #selector(AppDelegate.cutCues(_:)), "x"),
                item("menu.edit.copy", #selector(AppDelegate.copyCues(_:)), "c"),
                item("menu.edit.paste", #selector(AppDelegate.pasteCues(_:)), "v"),
                deleteItem(),
                item("menu.edit.selectAll", #selector(NSText.selectAll(_:)), "a"),
            ]
        )
    }

    /// QLab: Delete is Cmd-Delete (Edit menu).
    private static func deleteItem() -> NSMenuItem {
        let delete = item("menu.edit.delete", #selector(AppDelegate.deleteCues(_:)), "\u{8}")
        delete.keyEquivalentModifierMask = [.command]
        return delete
    }

    /// New cues of each kind (QLab keys: Cmd-0 Audio, Cmd-6 Fade, Cmd-9 Group), Renumber (Cmd-R)
    /// and Move Up / Down (Option-Cmd-Up / Down). `representedObject` carries the kind.
    private static func cuesMenu() -> NSMenu {
        let keys: [NewCueKind: String] = [.audio: "0", .fade: "6", .group: "9"]
        var items: [NSMenuItem] = []
        for kind in NewCueKind.allCases {
            let entry = NSMenuItem(
                title: String(format: localized("menu.cues.new"), localized(kind.titleKey)),
                action: #selector(AppDelegate.newCue(_:)), keyEquivalent: keys[kind] ?? "")
            entry.representedObject = kind.rawValue
            items.append(entry)
        }
        items.append(.separator())
        items.append(item("menu.cues.renumber", #selector(AppDelegate.renumberCues(_:)), "r"))
        items.append(item("menu.cues.deleteNumbers", #selector(AppDelegate.deleteNumbers(_:)), "d"))
        let up = item(
            "menu.cues.moveUp", #selector(AppDelegate.moveCuesUp(_:)), String(UnicodeScalar(NSUpArrowFunctionKey)!))
        let down = item(
            "menu.cues.moveDown", #selector(AppDelegate.moveCuesDown(_:)),
            String(UnicodeScalar(NSDownArrowFunctionKey)!))
        for e in [up, down] { e.keyEquivalentModifierMask = [.command, .option] }
        return menu("menu.cues", items + [up, down])
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
            #selector(AppDelegate.undoEdit(_:)), #selector(AppDelegate.redoEdit(_:)),
            #selector(AppDelegate.deleteCues(_:)), #selector(AppDelegate.cutCues(_:)),
            #selector(AppDelegate.copyCues(_:)), #selector(AppDelegate.pasteCues(_:)),
            #selector(AppDelegate.deleteNumbers(_:)),
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
