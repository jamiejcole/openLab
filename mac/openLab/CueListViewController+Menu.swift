// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The cue row's context menu (QLab 5 surface `context.cue-row`), cut down to what openLab does.
/// Commands go through the same actions as the menu bar (`AppDelegate`'s Transport and Edit items,
/// found along the responder chain), so there is one path for each; only Move Playhead and Reveal
/// are the list's own. Show Mode locks the editing items exactly as the Edit menu does.
enum CueRowMenu {
    enum Item: CaseIterable {
        case preview, load, panic, movePlayhead, revealTarget, cut, copy, paste, delete

        var titleKey: String {
            switch self {
            case .preview: return "cuelist.menu.preview"
            case .load: return "cuelist.menu.load"
            case .panic: return "cuelist.menu.panic"
            case .movePlayhead: return "cuelist.menu.movePlayhead"
            case .revealTarget: return "cuelist.menu.revealTarget"
            case .cut: return "menu.edit.cut"
            case .copy: return "menu.edit.copy"
            case .paste: return "menu.edit.paste"
            case .delete: return "menu.edit.delete"
            }
        }
    }

    /// What the rules need to know; the view controller fills it from the store.
    struct Context {
        var selectedCount: Int
        var anyAudioSelected: Bool
        var clickedIsPlayhead: Bool
        var clickedHasMedia: Bool
        var anyActive: Bool
        var canPaste: Bool
        var showMode: Bool
    }

    /// Show Mode locks the same four editing items as the Edit menu does (SM-01).
    static func isEnabled(_ item: Item, _ c: Context) -> Bool {
        switch item {
        case .preview: return c.anyAudioSelected
        case .load: return c.selectedCount > 0
        case .cut, .copy, .delete: return c.selectedCount > 0 && !locked(item, c)
        case .panic: return c.anyActive  // greyed unless something plays
        case .movePlayhead: return !c.clickedIsPlayhead
        case .revealTarget: return c.clickedHasMedia
        case .paste: return c.canPaste && !locked(item, c)
        }
    }

    /// The Edit-menu command an item stands for (nil: not an edit).
    static func lockedEdit(_ item: Item) -> MainMenu.LockedEdit? {
        switch item {
        case .cut: return .cut
        case .copy: return .copy
        case .paste: return .paste
        case .delete: return .delete
        default: return nil
        }
    }

    private static func locked(_ item: Item, _ c: Context) -> Bool {
        c.showMode && lockedEdit(item) != nil
    }

    /// The file a media path names: absolute as given, relative to the folder of the saved show.
    @MainActor static func mediaURL(_ media: String?, showPath: String?) -> URL? {
        guard let media, !media.isEmpty else { return nil }
        if media.hasPrefix("/") { return URL(fileURLWithPath: media) }
        guard let dir = CueEditor.sourceDir(showPath: showPath) else { return nil }
        return URL(fileURLWithPath: dir).appendingPathComponent(media)
    }
}

extension CueListViewController: NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        prepareRowMenu(menu, row: outlineView.clickedRow)
    }

    /// Fills `menu` for the cue at `row` (no cue under the pointer: an empty menu, which does not
    /// show). A right-click on a cue outside the selection selects it, as a click would.
    func prepareRowMenu(_ menu: NSMenu, row: Int) {
        menu.removeAllItems()
        guard row >= 0, let node = outlineView.item(atRow: row) as? CueNode else { return }
        if !selectedIDs().contains(node.id) { selectAsUser([node.id]) }
        let selected = selectedIDs().compactMap { self.node($0) }
        let context = CueRowMenu.Context(
            selectedCount: selected.count,
            anyAudioSelected: selected.contains { $0.cue.kindType == "audio" },
            clickedIsPlayhead: isPlayheadRow(node.id),
            clickedHasMedia: revealURL(node) != nil,
            anyActive: store.cueStates.values.contains { $0.state.isActive } || !store.previewing.isEmpty,
            canPaste: pasteboard.data(forType: CueEditor.pasteType) != nil, showMode: store.showMode)
        func entry(_ item: CueRowMenu.Item, _ action: Selector, command: String? = nil) -> NSMenuItem {
            let e = NSMenuItem(title: loc(item.titleKey), action: action, keyEquivalent: "")
            e.target = self
            e.representedObject = command ?? node.id.raw
            e.tag = CueRowMenu.Item.allCases.firstIndex(of: item) ?? -1
            return e
        }
        let transport = #selector(rowCommand(_:))
        let items: [NSMenuItem?] = [
            entry(.preview, transport, command: TransportCommand.previewSelected.rawValue),
            entry(.load, transport, command: TransportCommand.loadSelected.rawValue),
            entry(.panic, transport, command: TransportCommand.panic.rawValue),
            .separator(),
            entry(.movePlayhead, #selector(movePlayheadToCue(_:))),
            entry(.revealTarget, #selector(revealTargetInFinder(_:))),
            .separator(),
            entry(.cut, #selector(rowCut(_:))),
            entry(.copy, #selector(rowCopy(_:))),
            entry(.paste, #selector(rowPaste(_:))),
            entry(.delete, #selector(rowDelete(_:))),
        ]
        items.compactMap { $0 }.forEach(menu.addItem)
        // The rules decide; AppKit's own validation pass must not overwrite them.
        menu.autoenablesItems = false
        for item in menu.items where CueRowMenu.Item.allCases.indices.contains(item.tag) && !item.isSeparatorItem {
            item.isEnabled = CueRowMenu.isEnabled(CueRowMenu.Item.allCases[item.tag], context)
        }
    }

    func revealURL(_ node: CueNode) -> URL? {
        guard node.cue.kindType == "audio" else { return nil }
        return CueRowMenu.mediaURL(node.cue.raw["kind"]?["media"]?.stringValue, showPath: store.document?.path)
    }

    /// Preview, Load and Panic: the same command the Transport menu runs, on this window.
    @objc func rowCommand(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return
        }
        onCommand?(command)
    }

    // The editor's own methods, as the Edit menu calls them, but never the text field in focus.
    @objc func rowCut(_ sender: Any?) { Task { await editor?.cutSelection() } }
    @objc func rowCopy(_ sender: Any?) { Task { await editor?.copySelection() } }
    @objc func rowPaste(_ sender: Any?) { Task { await editor?.paste() } }
    @objc func rowDelete(_ sender: Any?) { Task { await editor?.deleteSelection() } }

    @objc func movePlayheadToCue(_ sender: NSMenuItem) {
        guard let list = listID, let raw = sender.representedObject as? String else { return }
        Task { await store.transport(.setPlayhead(list: list, cue: ModelID(raw))) }
    }

    @objc func revealTargetInFinder(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let node = node(ModelID(raw)),
            let url = revealURL(node)
        else { return }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    // MARK: View and Tools menu actions (the same commands the row menu and the keys run)

    /// The first cue in row order whose number is `number` (surrounding blanks ignored), through
    /// groups, collapsed or not. Nil: none, or `number` is empty.
    func cue(numbered number: String) -> ModelID? {
        let wanted = number.trimmingCharacters(in: .whitespaces)
        guard !wanted.isEmpty else { return nil }
        func find(_ nodes: [CueNode]) -> ModelID? {
            for node in nodes {
                if node.cue.number == wanted { return node.id }
                if let hit = find(node.children) { return hit }
            }
            return nil
        }
        return find(roots)
    }

    /// Move Playhead to Next / Previous Cue: the visible row `delta` from the playhead's, as the
    /// row menu's "Move Playhead" sends it. At either end nothing changes.
    func movePlayhead(by delta: Int) {
        guard let list = listID, outlineView.numberOfRows > 0 else { return }
        let count = outlineView.numberOfRows
        let current = store.playheads[list].flatMap { node($0) }.map { outlineView.row(forItem: $0) } ?? -1
        let from = current >= 0 ? current : (delta > 0 ? -1 : count)
        let row = min(max(from + delta, 0), count - 1)
        guard row != from, let node = outlineView.item(atRow: row) as? CueNode else { return }
        Task { await store.transport(.setPlayhead(list: list, cue: node.id)) }
    }

    /// Tools > Reveal Target File in Finder: the last selected cue's file, as the row menu does.
    @discardableResult
    func revealSelectedTarget() -> Bool {
        guard let id = selectedIDs().last, let node = node(id), let url = revealURL(node) else { return false }
        NSWorkspace.shared.activateFileViewerSelecting([url])
        return true
    }

    /// Whether Reveal Target File has a file to show (for the menu item).
    var canRevealSelectedTarget: Bool {
        selectedIDs().last.flatMap { node($0) }.flatMap { revealURL($0) } != nil
    }
}
