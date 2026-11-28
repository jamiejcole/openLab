// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class MainMenuTests: XCTestCase {
    func testMainMenuHasStandardTopLevelMenus() {
        let titles = MainMenu.make().bar.items.compactMap {
            $0.submenu?.title
        }
        XCTAssertEqual(titles, ["openLab", "File", "Edit", "Cues", "Tools", "View", "Transport", "Window", "Help"])
    }

    /// QLab's order: the workspace's files and settings after Save As; Jump to Targets with the
    /// numbering tools; the system's Services in the app menu.
    func testFileAndToolsFollowQLabsOrderAndTheAppMenuHasServices() throws {
        let menus = MainMenu.make()
        let file = try XCTUnwrap(menus.bar.items[1].submenu).items.map { $0.isSeparatorItem ? "-" : $0.title }
        // New Workspace from QLab File sits directly under New Workspace.
        let fileNew = try XCTUnwrap(file.firstIndex(of: loc("menu.file.new")))
        XCTAssertEqual(file[fileNew + 1], loc("menu.file.newFromQlab"))
        XCTAssertEqual(
            Array(file.suffix(4)),
            [loc("menu.file.saveAs"), "-", loc("menu.file.showFiles"), loc("menu.file.workspaceSettings")])
        XCTAssertEqual(
            menus.tools.items.prefix(5).map { $0.isSeparatorItem ? "-" : $0.title },
            [
                loc("menu.tools.loadToTime"), loc("menu.cues.renumber"), loc("menu.cues.deleteNumbers"),
                loc("menu.tools.jumpToTargets"), "-",
            ])
        let app = try XCTUnwrap(menus.bar.items[0].submenu)
        XCTAssertTrue(app.items.contains { $0.submenu === menus.services })
    }

    private func allItems(_ menu: NSMenu) -> [NSMenuItem] {
        menu.items.flatMap { [$0] + ($0.submenu.map(allItems) ?? []) }
    }

    /// The chord an item answers to: an upper-case letter means Shift, as AppKit reads it.
    private func chord(_ item: NSMenuItem) -> KeyChord {
        var mods = item.keyEquivalentModifierMask
        if item.keyEquivalent.lowercased() != item.keyEquivalent { mods.insert(.shift) }
        return KeyChord(item.keyEquivalent, mods)
    }

    /// Two items on one chord: the first would silently win. Transport items count at their defaults.
    func testNoTwoMenuItemsShareAKeyChord() {
        var seen: [KeyChord: String] = [:]
        for item in allItems(MainMenu.make().bar) where !item.keyEquivalent.isEmpty {
            let c = chord(item)
            XCTAssertNil(seen[c], "\(item.title) and \(seen[c] ?? "") share \(c.display)")
            seen[c] = item.title
        }
    }

    func testCuesMenuListsEveryToolbarKindInQLabsOrder() {
        let kinds = MainMenu.make().cues.items.compactMap { $0.representedObject as? String }
        XCTAssertEqual(
            kinds,
            ["group", "audio", "video", "fade", "network", "start", "stop", "pause", "load", "reset", "devamp"]
                + ["goto", "target", "arm", "disarm", "wait", "memo"])
        let toolbar = CueToolbarView.kinds.flatMap { $0 }.compactMap { $0 }
        XCTAssertEqual(Set(kinds), Set(toolbar.map(\.rawValue)), "every toolbar button has a menu item")
        let separators = MainMenu.make().cues.items.enumerated().filter { $0.element.isSeparatorItem }.map(\.offset)
        XCTAssertEqual(separators, [3, 5, 17])
    }

    func testRenumberAndDeleteNumbersAreInTheToolsMenu() {
        let menus = MainMenu.make()
        XCTAssertFalse(allItems(menus.cues).contains { $0.action == #selector(AppDelegate.renumberCues(_:)) })
        let keys = Dictionary(
            uniqueKeysWithValues: menus.tools.items.compactMap { i in i.action.map { (NSStringFromSelector($0), i) } })
        func key(_ s: Selector) -> String? { keys[NSStringFromSelector(s)]?.keyEquivalent }
        XCTAssertEqual(key(#selector(AppDelegate.loadToTime(_:))), "t")
        XCTAssertEqual(key(#selector(AppDelegate.renumberCues(_:))), "r")
        XCTAssertEqual(key(#selector(AppDelegate.deleteNumbers(_:))), "d")
        XCTAssertEqual(key(#selector(AppDelegate.revealTargetFile(_:))), "")
    }

    func testViewMenuHasQLabsItemsAndKeys() {
        let view = MainMenu.make().view
        func item(_ s: Selector) -> NSMenuItem? { view.items.first { $0.action == s } }
        let cmd: NSEvent.ModifierFlags = [.command]
        let shiftCmd: NSEvent.ModifierFlags = [.command, .shift]
        let expected: [(Selector, String, NSEvent.ModifierFlags)] = [
            (#selector(AppDelegate.selectCue(_:)), "j", cmd),
            (#selector(AppDelegate.selectNextCue(_:)), KeyChord.down, cmd),
            (#selector(AppDelegate.selectPreviousCue(_:)), KeyChord.up, cmd),
            (#selector(AppDelegate.selectNextInspectorTab(_:)), KeyChord.right, cmd),
            (#selector(AppDelegate.selectPreviousInspectorTab(_:)), KeyChord.left, cmd),
            (#selector(AppDelegate.movePlayheadToCue(_:)), "j", shiftCmd),
            (#selector(AppDelegate.movePlayheadNext(_:)), KeyChord.down, shiftCmd),
            (#selector(AppDelegate.movePlayheadPrevious(_:)), KeyChord.up, shiftCmd),
            (#selector(AppDelegate.moveCuesDown(_:)), KeyChord.down, [.command, .option]),
            (#selector(AppDelegate.moveCuesUp(_:)), KeyChord.up, [.command, .option]),
            (#selector(AppDelegate.enterEditMode(_:)), "[", shiftCmd),
            (#selector(AppDelegate.enterShowMode(_:)), "]", shiftCmd),
        ]
        for (selector, key, mods) in expected {
            XCTAssertEqual(item(selector)?.keyEquivalent, key, NSStringFromSelector(selector))
            XCTAssertEqual(item(selector)?.keyEquivalentModifierMask, mods, NSStringFromSelector(selector))
        }
        // The layout toggles: one item per footer toggle, Inspector Cmd-I and Sidebar Cmd-L.
        let toggles = view.items.filter { $0.action == #selector(AppDelegate.toggleLayoutArea(_:)) }
        XCTAssertEqual(toggles.compactMap { MainMenu.layoutArea(of: $0) }, [.inspector, .toolbar, .masthead, .sidebar])
        XCTAssertEqual(toggles.map(\.keyEquivalent), ["i", "", "", "l"])
    }

    func testFileAndWindowMenusHaveCloseAllAndTheLauncher() throws {
        let menus = MainMenu.make()
        let file = try XCTUnwrap(menus.bar.items[1].submenu)
        let close = try XCTUnwrap(file.items.first { $0.action == #selector(NSWindow.performClose(_:)) })
        XCTAssertEqual(close.title, "Close")
        let all = try XCTUnwrap(file.items.first { $0.action == #selector(AppDelegate.closeAllWorkspaces(_:)) })
        XCTAssertEqual(all.keyEquivalent, "w")
        XCTAssertEqual(all.keyEquivalentModifierMask, [.command, .option])
        let launcher = try XCTUnwrap(
            menus.windows.items.first { $0.action == #selector(AppDelegate.showLauncherWindow(_:)) })
        XCTAssertEqual(launcher.keyEquivalent, "1")
        XCTAssertEqual(launcher.keyEquivalentModifierMask, [.command, .shift])
    }

    /// Flow: every menu title uses the vocabulary of the toolbar, row menu and alerts; no placeholder
    /// or repeated item. The cue kinds are named as the toolbar names them, an ellipsis is the one
    /// character, the row menu's Reveal reads as the Tools item, and Undo and Redo are named in the
    /// words of the steps.
    func testMenuTitlesShareTheVocabularyOfTheToolbarAndRowMenuWithNoPlaceholders() {
        let bar = MainMenu.make().bar
        let titles = allItems(bar).filter { !$0.isSeparatorItem }
        for item in titles {
            XCTAssertFalse(item.title.isEmpty, "an item with no title")
            XCTAssertFalse(item.title.contains("..."), "\(item.title): an ellipsis is one character")
            XCTAssertFalse(item.title.lowercased().contains("todo"), item.title)
        }
        func walk(_ menu: NSMenu) {
            var seen = Set<String>()
            for item in menu.items where !item.isSeparatorItem && item.submenu == nil {
                let id = "\(item.title)|\(item.action.map(NSStringFromSelector) ?? "")|\(item.representedObject ?? "")"
                XCTAssertTrue(seen.insert(id).inserted, "\(item.title) is listed twice in \(menu.title)")
            }
            for item in menu.items { item.submenu.map(walk) }
        }
        walk(bar)
        let cues = MainMenu.make().cues.items.filter { $0.representedObject is String }
        let toolbar = CueToolbarView.groups.flatMap { $0 }.map { loc($0.key) }
        for name in toolbar {
            XCTAssertTrue(
                cues.contains { $0.title == String(format: loc("menu.cues.new"), name) }, "\(name) is in Cues and the toolbar")
        }
        XCTAssertEqual(
            loc("cuelist.menu.revealTarget"), loc("menu.tools.revealTarget"), "one name for the one action")
        XCTAssertEqual(loc("cuelist.menu.edit"), loc("menu.edit"))
        XCTAssertEqual(MainMenu.undoTitle(redo: false, step: nil), loc("menu.edit.undo"))
        for code in ["add_cue", "remove_cue", "move_cue", "paste_cues", "renumber_cues", "rename_cue", "edit_cue"] {
            XCTAssertNotEqual(loc("undo.step.\(code)"), "undo.step.\(code)", "\(code) has words")
            XCTAssertEqual(
                MainMenu.undoTitle(redo: true, step: code),
                String(format: loc("menu.edit.redoNamed"), loc("undo.step.\(code)")))
        }
        XCTAssertEqual(MainMenu.undoTitle(redo: false, step: "a_step_from_a_newer_engine"), loc("menu.edit.undo"))
    }

    /// The one list of locked edits names real items: each is on some menu, so Show Mode cannot
    /// miss a new edit item added to the list.
    func testEveryLockedEditHasAMenuItem() {
        let actions = Set(allItems(MainMenu.make().bar).compactMap(\.action))
        for lock in MainMenu.LockedEdit.allCases {
            XCTAssertTrue(actions.contains(lock.selector), "\(lock)")
        }
    }
}
