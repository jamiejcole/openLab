// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The main menu bar, built in code (no storyboards or XIBs).
@MainActor
enum MainMenu {
    struct Menus {
        let bar: NSMenu
        let edit: NSMenu
        let openRecent: NSMenu
        let cues: NSMenu
        let tools: NSMenu
        let view: NSMenu
        let transport: NSMenu
        let windows: NSMenu
        let help: NSMenu
    }

    static func make(keys: TransportKeys = TransportKeys(settings: nil)) -> Menus {
        let edit = editMenu()
        let (file, openRecent) = fileMenu()
        let cues = cuesMenu()
        let tools = toolsMenu()
        let view = viewMenu()
        let windows = menu(
            "menu.window",
            [
                item("menu.window.minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
                item("menu.window.zoom", #selector(NSWindow.performZoom(_:))),
                .separator(),
                launcherItem(),
                .separator(),
                logItem(),
                preshowItem(),
                item("menu.window.bringAllToFront", #selector(NSApplication.arrangeInFront(_:))),
            ]
        )
        let help = menu(
            "menu.help",
            [
                item("menu.help.openLabHelp", #selector(NSApplication.showHelp(_:)), "?"),
                .separator(),
                item("menu.help.crashReports", #selector(AppDelegate.revealCrashReports(_:))),
            ])
        let transport = transportMenu(keys)
        let bar = NSMenu()
        for submenu in [appMenu(), file, edit, cues, tools, view, transport, windows, help] {
            let holder = NSMenuItem()
            holder.submenu = submenu
            bar.addItem(holder)
        }
        return Menus(
            bar: bar, edit: edit, openRecent: openRecent, cues: cues, tools: tools, view: view, transport: transport,
            windows: windows, help: help
        )
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

    /// QLab's File menu order: New, Open, Open Recent, Close, Save, Save As. The Open Recent
    /// submenu is filled when it opens (`AppDelegate.menuNeedsUpdate`).
    private static func fileMenu() -> (file: NSMenu, openRecent: NSMenu) {
        let recent = NSMenu(title: localized("menu.file.openRecent"))
        let recentItem = NSMenuItem(title: recent.title, action: nil, keyEquivalent: "")
        recentItem.submenu = recent
        let file = menu(
            "menu.file",
            [
                item("menu.file.new", #selector(AppDelegate.newWorkspace(_:)), "n"),
                item("menu.file.open", #selector(AppDelegate.openWorkspace(_:)), "o"),
                recentItem,
                item("menu.file.recover", #selector(AppDelegate.recoverAutosaves(_:))),
                .separator(),
                item("menu.file.relink", #selector(AppDelegate.relinkMedia(_:))),
                .separator(),
                item("menu.file.workspaceSettings", #selector(AppDelegate.showWorkspaceSettings(_:)), ","),
                .separator(),
                item("menu.file.close", #selector(NSWindow.performClose(_:)), "w"),
                closeAllItem(),
                item("menu.file.save", #selector(AppDelegate.saveWorkspace(_:)), "s"),
                item("menu.file.saveAs", #selector(AppDelegate.saveWorkspaceAs(_:)), "S"),
            ]
        )
        return (file, recent)
    }

    /// The items of the Open Recent submenu for `urls`: one per workspace, then Clear Menu.
    static func fillRecent(_ menu: NSMenu, _ urls: [URL]) {
        menu.removeAllItems()
        for url in urls.prefix(10) {
            let entry = NSMenuItem(
                title: url.deletingPathExtension().lastPathComponent,
                action: #selector(AppDelegate.openRecent(_:)), keyEquivalent: "")
            entry.representedObject = url
            entry.toolTip = url.path
            menu.addItem(entry)
        }
        if urls.isEmpty {
            let none = NSMenuItem(title: localized("menu.file.openRecent.none"), action: nil, keyEquivalent: "")
            none.isEnabled = false
            menu.addItem(none)
        } else {
            menu.addItem(.separator())
            menu.addItem(item("menu.file.openRecent.clear", #selector(AppDelegate.clearRecents(_:))))
        }
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

    /// The Cues menu's kinds in QLab's order: media (Group, Audio), Fade, the control cues, then
    /// Wait and Memo. Kinds QLab has but openLab lacks are absent; Target has no model cue.
    static let cueMenuGroups: [[NewCueKind]] = [
        [.group, .audio], [.fade], [.start, .stop, .pause, .load, .reset, .devamp, .goto, .arm, .disarm],
        [.wait, .memo],
    ]

    /// New cues of each kind (QLab keys: Cmd-0 Group, Cmd-1 Audio, Cmd-7 Fade; the other types have none).
    /// `representedObject` carries the kind.
    private static func cuesMenu() -> NSMenu {
        let keys: [NewCueKind: String] = [.group: "0", .audio: "1", .fade: "7"]
        var items: [NSMenuItem] = []
        for (n, group) in cueMenuGroups.enumerated() {
            if n > 0 { items.append(.separator()) }
            for kind in group {
                let entry = NSMenuItem(
                    title: String(format: localized("menu.cues.new"), localized(kind.titleKey)),
                    action: #selector(AppDelegate.newCue(_:)), keyEquivalent: keys[kind] ?? "")
                entry.representedObject = kind.rawValue
                items.append(entry)
            }
        }
        return menu("menu.cues", items)
    }

    /// QLab's Tools menu, the parts openLab has: Load to Time (Cmd-T), Renumber (Cmd-R), Delete
    /// Numbers (Cmd-D), Reveal Target File in Finder.
    private static func toolsMenu() -> NSMenu {
        menu(
            "menu.tools",
            [
                item("menu.tools.loadToTime", #selector(AppDelegate.loadToTime(_:)), "t"),
                item("menu.cues.renumber", #selector(AppDelegate.renumberCues(_:)), "r"),
                item("menu.cues.deleteNumbers", #selector(AppDelegate.deleteNumbers(_:)), "d"),
                .separator(),
                item("menu.tools.revealTarget", #selector(AppDelegate.revealTargetFile(_:))),
            ])
    }

    /// The layout area a View menu toggle stands for (its tag; the footer buttons do the same).
    static func layoutArea(of item: NSMenuItem) -> LayoutArea? {
        LayoutArea.allCases.indices.contains(item.tag) ? LayoutArea.allCases[item.tag] : nil
    }

    /// QLab's View menu, the parts openLab has: the layout toggles (Cmd-I Inspector, Cmd-L Sidebar),
    /// selection and playhead movement, Move Selected Cues, Edit / Show Mode.
    private static func viewMenu() -> NSMenu {
        func area(_ key: String, _ area: LayoutArea, _ keyEquivalent: String = "") -> NSMenuItem {
            let entry = item(key, #selector(AppDelegate.toggleLayoutArea(_:)), keyEquivalent)
            entry.tag = LayoutArea.allCases.firstIndex(of: area) ?? -1
            return entry
        }
        func chord(_ e: NSMenuItem, _ mods: NSEvent.ModifierFlags) -> NSMenuItem {
            e.keyEquivalentModifierMask = mods
            return e
        }
        let (down, up, left, right) = (KeyChord.down, KeyChord.up, KeyChord.left, KeyChord.right)
        let cmd: NSEvent.ModifierFlags = [.command]
        let shiftCmd: NSEvent.ModifierFlags = [.command, .shift]
        // Select Next / Previous are fixed, not in the key map (QLab): the selection moves, and the
        // playhead follows it.
        return menu(
            "menu.view",
            [
                area("menu.view.inspector", .inspector, "i"),
                area("menu.view.toolbar", .toolbar),
                area("menu.view.masthead", .masthead),
                area("menu.view.sidebar", .sidebar, "l"),
                .separator(),
                chord(item("menu.view.selectCue", #selector(AppDelegate.selectCue(_:)), "j"), cmd),
                chord(item("menu.view.selectNext", #selector(AppDelegate.selectNextCue(_:)), down), cmd),
                chord(item("menu.view.selectPrevious", #selector(AppDelegate.selectPreviousCue(_:)), up), cmd),
                chord(item("menu.view.nextTab", #selector(AppDelegate.selectNextInspectorTab(_:)), right), cmd),
                chord(item("menu.view.previousTab", #selector(AppDelegate.selectPreviousInspectorTab(_:)), left), cmd),
                .separator(),
                chord(item("menu.view.movePlayheadToCue", #selector(AppDelegate.movePlayheadToCue(_:)), "j"), shiftCmd),
                chord(item("menu.view.playheadNext", #selector(AppDelegate.movePlayheadNext(_:)), down), shiftCmd),
                chord(
                    item("menu.view.playheadPrevious", #selector(AppDelegate.movePlayheadPrevious(_:)), up), shiftCmd),
                .separator(),
                chord(item("menu.view.moveDown", #selector(AppDelegate.moveCuesDown(_:)), down), [.command, .option]),
                chord(item("menu.view.moveUp", #selector(AppDelegate.moveCuesUp(_:)), up), [.command, .option]),
                .separator(),
                chord(item("menu.view.editMode", #selector(AppDelegate.enterEditMode(_:)), "["), shiftCmd),
                chord(item("menu.view.showMode", #selector(AppDelegate.enterShowMode(_:)), "]"), shiftCmd),
            ]
        )
    }

    /// The editing commands Show Mode (SM-01) locks: the one list both the Edit menu and the cue
    /// row menu read. (The editor refuses them too, `CueEditor.begin`.)
    enum LockedEdit: CaseIterable {
        case undo, redo, delete, cut, copy, paste, deleteNumbers, renumber, moveUp, moveDown

        var selector: Selector {
            switch self {
            case .undo: return #selector(AppDelegate.undoEdit(_:))
            case .redo: return #selector(AppDelegate.redoEdit(_:))
            case .delete: return #selector(AppDelegate.deleteCues(_:))
            case .cut: return #selector(AppDelegate.cutCues(_:))
            case .copy: return #selector(AppDelegate.copyCues(_:))
            case .paste: return #selector(AppDelegate.pasteCues(_:))
            case .deleteNumbers: return #selector(AppDelegate.deleteNumbers(_:))
            case .renumber: return #selector(AppDelegate.renumberCues(_:))
            case .moveUp: return #selector(AppDelegate.moveCuesUp(_:))
            case .moveDown: return #selector(AppDelegate.moveCuesDown(_:))
            }
        }
    }

    /// Show Mode (SM-01) turns the editing items off by hand; Select All changes nothing.
    static func lockEditItems(in edit: NSMenu, locked: Bool) {
        edit.autoenablesItems = !locked
        let editing = LockedEdit.allCases.map(\.selector)
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
            if command == .loadSelected { items.append(.separator()) }
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

    private static func closeAllItem() -> NSMenuItem {
        let all = item("menu.file.closeAll", #selector(AppDelegate.closeAllWorkspaces(_:)), "w")
        all.keyEquivalentModifierMask = [.command, .option]
        return all
    }

    private static func launcherItem() -> NSMenuItem {
        let launcher = item("menu.window.launcher", #selector(AppDelegate.showLauncherWindow(_:)), "1")
        launcher.keyEquivalentModifierMask = [.command, .shift]
        return launcher
    }

    private static func logItem() -> NSMenuItem {
        let log = item("menu.window.log", #selector(AppDelegate.showLog(_:)), "l")
        log.keyEquivalentModifierMask = [.command, .option]
        return log
    }

    /// QLab keeps its warnings in Workspace Status (Window menu, shift-cmd-W); the pre-show check
    /// takes that place and key.
    private static func preshowItem() -> NSMenuItem {
        item("menu.window.preshowCheck", #selector(AppDelegate.showPreshowCheck(_:)), "W")
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
