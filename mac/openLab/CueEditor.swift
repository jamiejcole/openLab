// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Structural edits to the cue list (add, delete, reorder, renumber, undo, redo) for one window.
/// Every entry point refuses in Show Mode (`isLocked`), commits what is typed in the inspector
/// first, and sends core's commands through the store; the cue list follows the engine's echo.
///
/// Why it behaves this way (QLab 5's shortcuts page gives the keys only; where it is silent the safest
/// live-show choice is used): a new cue goes after the last selected cue, beside it, never inside a
/// selected group; delete has no prompt (one undo step) but refuses a cue that is running, paused,
/// waiting, fading, stopping or previewing, or a group holding one; deleting a group deletes its
/// children; a drop on a group appends to it, on a cue goes after it, never into the cue's own
/// descendants; the audio cue needs a patch. Undo and redo are the engine's and are not guarded: an
/// undo can remove an active cue (docs/handoff/m5-protocol-requests.md).
///
/// Part 2 (CueEditor+Clipboard.swift; QLab's pages are silent on drops): a Finder drop makes one audio
/// cue per file in drop order (one undo step) and is refused whole if any item is a folder or not audio;
/// one file dropped on an idle audio cue replaces its file; a cue dropped on a cue only reorders (targets
/// are set in the inspector). Copy, cut and paste carry cue subtrees as JSON on a private pasteboard
/// type (versioned); paste goes where a new cue would, as core's `paste` with the cues prepared like `prepare_paste`. Cmd-D clears
/// the selected cues' numbers (undoable, no prompt).
@MainActor
final class CueEditor {
    let store: WorkspaceStore
    weak var cueList: CueListViewController?
    var pasteboard: NSPasteboard = .general
    var isLocked: () -> Bool = { false }
    var commitPending: () async -> Bool = { true }
    /// Tells the operator why an edit was refused (a sheet in the app; recorded by tests).
    var notify: (String) -> Void = { _ in NSSound.beep() }

    init(store: WorkspaceStore, cueList: CueListViewController) {
        self.store = store
        self.cueList = cueList
    }

    // MARK: Pure rules (tested without a window)

    /// Cmd-Z / Shift-Cmd-Z while text is being edited belong to the text, never to the workspace:
    /// nil when no text is being edited (use the workspace), true when the text undid or redid,
    /// false when its undo stack is empty (beep).
    static func textUndo(_ text: NSText?, redo: Bool) -> Bool? {
        guard let text else { return nil }
        guard let manager = text.undoManager, redo ? manager.canRedo : manager.canUndo else { return false }
        if redo { manager.redo() } else { manager.undo() }
        return true
    }

    /// Where a new cue goes: right after the last selected cue, beside it (a selected group gets
    /// the cue after it, not inside); with nothing selected, at the end of the list.
    static func insertion(selected: [CueNode], roots: [CueNode]) -> (parent: ModelID?, index: Int) {
        guard let last = selected.last else { return (nil, roots.count) }
        let siblings = last.parent?.children ?? roots
        let at = siblings.firstIndex { $0 === last } ?? siblings.count - 1
        return (last.parent?.id, at + 1)
    }

    /// The nodes whose ancestors are not in the set, in the given order.
    static func topLevel(_ nodes: [CueNode]) -> [CueNode] {
        let ids = Set(nodes.map(\.id))
        return nodes.filter { node in
            var p = node.parent
            while let a = p {
                if ids.contains(a.id) { return false }
                p = a.parent
            }
            return true
        }
    }

    /// A drop into `parent` is refused when `parent` is a dragged cue or inside one.
    static func canDrop(_ dragged: [CueNode], into parent: CueNode?) -> Bool {
        var p = parent
        while let a = p {
            if dragged.contains(where: { $0 === a }) { return false }
            p = a.parent
        }
        return !dragged.isEmpty
    }

    /// The commands that put `dragged` (top-level, in list order) before child `index` of `parent`
    /// (nil: the list's top level), keeping their order. Each cue goes before the first child at or
    /// after `index` that is not itself dragged, so the indices stay right as the cues move one by one.
    static func moveCommands(
        list: ModelID, dragged: [CueNode], parent: CueNode?, index: Int, roots: [CueNode]
    ) -> [Command] {
        let children = parent?.children ?? roots
        let draggedIDs = Set(dragged.map(\.id))
        let anchor = children.dropFirst(max(0, index)).first { !draggedIDs.contains($0.id) }?.id
        var sim = children.map(\.id)
        var commands: [Command] = []
        for d in dragged {
            let same = d.parent === parent
            if same { sim.removeAll { $0 == d.id } }
            let at = anchor.flatMap { a in sim.firstIndex(of: a) } ?? sim.count
            sim.insert(d.id, at: at)
            commands.append(
                same ? .moveCue(d.id, index: at) : .reparentCue(d.id, list: list, parent: parent?.id, index: at))
        }
        return commands
    }

    /// Cues are busy when running, fading, waiting, paused or stopping, or being previewed; deleting
    /// one would cut a sound the audience may be hearing.
    func isBusy(_ node: CueNode) -> Bool {
        if store.previewing.contains(node.id) { return true }
        switch store.cueStates[node.id]?.state ?? .idle {
        case .idle, .loaded, .broken: break
        default: return true
        }
        return node.children.contains { isBusy($0) }
    }

    // MARK: Actions

    func begin() async -> Bool {
        guard !isLocked() else { return false }
        guard await commitPending() else {
            NSSound.beep()
            return false
        }
        return !isLocked()
    }

    func selectedNodes() -> [CueNode] { (cueList?.selectedIDs() ?? []).compactMap { cueList?.node($0) } }

    @discardableResult
    func add(_ kind: NewCueKind) async -> ModelID? {
        guard await begin(), let list = cueList?.listID else { return nil }
        let id = ModelID(UUID().uuidString.lowercased())
        guard let cue = kind.cueJSON(id: id, patch: store.model?.patches.first) else {
            notify(loc("edit.error.noPatch"))
            return nil
        }
        let place = Self.insertion(selected: selectedNodes(), roots: (cueList?.roots ?? []))
        let command = Command.addCue(cue, list: list, parent: place.parent, index: place.index)
        guard report(await store.send(command: command)) else { return nil }
        cueList?.selectAsUser([id])
        return id
    }

    func deleteSelection() async {
        guard await begin() else { return }
        let nodes = Self.topLevel(selectedNodes())
        guard !nodes.isEmpty else { return }
        if nodes.contains(where: isBusy) {
            notify(loc("edit.error.deleteBusy"))
            return
        }
        // The cue that takes the selection: the one after the last deleted, else the one before the first.
        let gone = Set(nodes.map(\.id))
        let siblings = nodes[0].parent?.children ?? (cueList?.roots ?? [])
        let first = siblings.firstIndex { gone.contains($0.id) } ?? 0
        let last = siblings.lastIndex { gone.contains($0.id) } ?? 0
        let after = siblings.dropFirst(last + 1).first { !gone.contains($0.id) }
        let before = siblings.prefix(first).last { !gone.contains($0.id) }
        let successor = (after ?? before ?? nodes[0].parent)?.id
        guard let command = Command.transaction(nodes.map { .removeCue($0.id) }) else { return }
        guard report(await store.send(command: command)) else { return }
        if let successor, cueList?.node(successor) != nil { cueList?.selectAsUser([successor]) }
    }

    func renumberSelection(start: Double, increment: Double) async {
        guard await begin() else { return }
        let ids = (cueList?.selectedIDs() ?? [])
        guard !ids.isEmpty else { return }
        guard start.isFinite, increment.isFinite, start >= 0, increment > 0 else {
            notify(loc("edit.error.renumberValues"))
            return
        }
        _ = report(await store.send(command: .renumber(ids, start: start, increment: increment)))
    }

    /// Moves the selected cues one place up (`by` -1) or down (+1) among their siblings. A cue
    /// already at the end of its group stays; the others of the selection still move.
    func moveSelection(by step: Int) async {
        guard await begin() else { return }
        let nodes = Self.topLevel(selectedNodes())
        var commands: [Command] = []
        let ordered = step < 0 ? nodes : nodes.reversed()
        for node in ordered {
            let siblings = node.parent?.children ?? (cueList?.roots ?? [])
            guard let i = siblings.firstIndex(where: { $0 === node }) else { continue }
            let to = i + step
            guard siblings.indices.contains(to) else { continue }
            // A neighbour that is selected too and could not move blocks this one as well.
            if nodes.contains(where: { $0 === siblings[to] })
                && !commands.contains(where: { moved(siblings[to].id, in: $0) })
            {
                continue
            }
            commands.append(.moveCue(node.id, index: to))
        }
        guard let command = Command.transaction(commands) else { return }
        _ = report(await store.send(command: command))
    }

    private func moved(_ id: ModelID, in command: Command) -> Bool { command.json["id"]?.stringValue == id.raw }

    /// A drop in the outline (already validated): `ids` go before child `index` of `parent`.
    func move(_ ids: [ModelID], to parent: ModelID?, at index: Int) async {
        guard await begin(), let list = cueList?.listID else { return }
        let parentNode = parent.flatMap { cueList?.node($0) }
        if parent != nil && parentNode == nil { return }
        let dragged = Self.topLevel(ids.compactMap { cueList?.node($0) })
        guard Self.canDrop(dragged, into: parentNode) else { return }
        let commands = Self.moveCommands(
            list: list, dragged: dragged, parent: parentNode, index: index, roots: (cueList?.roots ?? []))
        guard let command = Command.transaction(commands) else { return }
        _ = report(await store.send(command: command))
    }

    func undo() async {
        guard await begin() else { return }
        _ = report(await store.undo())
    }

    func redo() async {
        guard await begin() else { return }
        _ = report(await store.redo())
    }

    var canUndo: Bool { !isLocked() && store.document?.canUndo == true }
    var canRedo: Bool { !isLocked() && store.document?.canRedo == true }

    /// True when the engine took the edit; otherwise tells the operator.
    func report(_ result: Result<OkBody, EngineError>) -> Bool {
        switch result {
        case .success: return true
        case .failure(let error):
            if case .engine(_, let message) = error {
                notify(String(format: loc("edit.error.refused"), message))
            } else {
                notify(loc("edit.error.notConnected"))
            }
            return false
        }
    }
}
