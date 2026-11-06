// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Transport commands from menu items and keys: each workspace's transport keys, GO, the
/// selected-cue actions, Panic, Reset All, Relink Media and the show log they report failures to.
extension AppController {
    // MARK: Transport commands (menu items and keys)

    /// The transport keys of `store`'s workspace. A show file whose hotkeys conflict or are
    /// unusable is logged to that store once per change (never a crash).
    func keys(for store: WorkspaceStore) -> TransportKeys {
        let settings = store.model?.settings
        let hotkeys = settings?["hotkeys"]
        let id = ObjectIdentifier(store)
        let cueChords = Set((store.model?.cueHotkeys ?? []).map(\.chord))
        let menu = menuChords()
        let cached = keyCache[id]
        // Only the key map moves keys: another setting or a cue hotkey changing re-reads and re-warns nothing
        // about the map (a cue hotkey moving is told through `shadowed` below).
        let same = cached?.hotkeys == hotkeys
        let keys = same ? (cached?.keys ?? TransportKeys(settings: settings)) : TransportKeys(settings: settings)
        // Keys from the show's settings win over cue hotkeys (a cue hotkey on one does not fire), V and S included.
        // A menu command's key (Cmd-J, Shift-Cmd-Down, ...) is taken before a cue hotkey is looked
        // at (`fireCueHotkeys`), so a cue on one never fires either: warn for it the same way.
        let transportShadowed = cueChords.intersection(keys.chords.values)
        let menuShadowed = cueChords.intersection(menu).subtracting(transportShadowed)
        let shadowed = transportShadowed.union(menuShadowed)
        // A key-map chord a menu command also owns still binds (the key monitor takes it first, as
        // QLab does): the menu command has lost its key in this workspace, and the operator is told.
        let lost = keys.chords.filter { menu.contains($0.value) }
        if let cached, same, cached.shadowed == shadowed, cached.lost == lost { return cached.keys }
        keyCache[id] = (hotkeys, shadowed, lost, keys)
        if !same {
            for warning in keys.warnings { _ = store.receive(.log(.warn, warning)) }
        }
        for chord in shadowed.subtracting(cached?.shadowed ?? []) {
            let key = transportShadowed.contains(chord) ? "hotkeys.warn.shadowed" : "hotkeys.warn.menuShadowed"
            _ = store.receive(.log(.warn, String(format: loc(key), chord.display)))
        }
        if cached?.lost != lost {
            func text(_ entry: (key: TransportCommand, value: KeyChord)) -> String {
                String(format: loc("hotkeys.warn.menuLost"), loc(entry.key.titleKey), entry.value.display)
            }
            store.setAppNotices(lost.sorted { $0.key.rawValue < $1.key.rawValue }.map(text))
            for entry in lost where cached?.lost[entry.key] != entry.value {
                _ = store.receive(.log(.warn, text(entry)))
            }
        }
        return keys
    }

    /// The key window's keys (the defaults when no workspace window is key).
    var frontKeys: TransportKeys {
        frontController.map { keys(for: $0.store) } ?? TransportKeys(settings: nil)
    }

    /// Select Next / Select Previous: the selection of the front window's cue list moves by `delta`
    /// rows (one cue selected, the playhead follows).
    func selectAdjacentCue(by delta: Int) {
        frontController?.cueList.selectAdjacent(by: delta)
    }

    /// View > Playhead Next / Previous: the front list's playhead moves by `delta` rows.
    func movePlayheadAdjacent(by delta: Int) {
        frontController?.cueList.movePlayhead(by: delta)
    }

    /// View > Select Next / Previous Inspector Tab (the inspector must be showing).
    func selectAdjacentInspectorTab(by delta: Int) {
        guard let front = frontController, front.isVisible(.inspector) else { return }
        if delta > 0 {
            front.inspector.tabView.selectNextTabViewItem(nil)
        } else {
            front.inspector.tabView.selectPreviousTabViewItem(nil)
        }
    }

    /// View > Inspector / Toolbar / GO Area / Sidebar: the footer toggle buttons' action.
    func toggleLayout(_ area: LayoutArea) {
        guard let front = frontController else { return }
        front.setVisible(area, !front.isVisible(area))
    }

    /// Whether the menu item for `command` may fire now. Disabled items also let their key through
    /// to the text field being edited (see `KeyMap` for the precedence rule).
    func canPerform(_ command: TransportCommand) -> Bool {
        if command == .panic { return true }
        guard frontController != nil else { return false }
        if let chord = frontKeys.chord(for: command), chord.typesCharacters, isTextEditing() { return false }
        return true
    }

    /// `perform` for a key or menu item, with the front window and its selection as of now: the
    /// selection can move with the playhead before the Task runs, and the command is for what the
    /// operator saw.
    func performNow(_ command: TransportCommand, isRepeat: Bool) {
        let front = frontController
        let cues = front?.cueList.selectedIDs()
        Task { await perform(command, isRepeat: isRepeat, on: front, cues: cues) }
    }

    /// Runs `command` on `target`, else the key window's workspace (Panic: every workspace). A held key is ignored
    /// after its first press: a repeating GO would fire cue after cue.
    func perform(
        _ command: TransportCommand, isRepeat: Bool = false, on target: WorkspaceWindowController? = nil,
        cues: [ModelID]? = nil
    )
        async
    {
        guard !isRepeat else { return }
        if command == .panic {
            _ = await panicAll()
            return
        }
        guard let wc = target ?? frontController else { return }
        let store = wc.store
        let selected = cues ?? wc.cueList.selectedIDs()
        let list = wc.cueList.listID  // the list picked in the sidebar
        switch command {
        case .go: _ = await go(store)
        case .pauseAll: await run(.pauseAll, on: store)
        case .resumeAll: await run(.resumeAll, on: store)
        // QLab's sidebar reset: no confirmation, as for the other workspace transport buttons.
        case .resetAll: await run(.resetAll, on: store)
        case .stopAll: await run(.stopAll, on: store)  // atomic in the engine: every cue and preview
        case .panic: break
        case .loadSelected: if let id = selected.last { await run(.load(cue: id, at: 0), on: store) }
        case .startSelected: await runAll(selected.map { .start(cue: $0) }, on: store)
        case .stopSelected:
            await runAll(selected.map { .hardStop(cue: $0) }, alongside: previewStops(of: selected, in: wc), on: store)
        case .resetSelected: await runAll(selected.map { .reset(cue: $0) }, on: store)
        // A second press on cues still fading from the first stops them at once: the engine
        // decides that per cue (QLab: S twice is a hard stop of the selection).
        // A group's panic already fades its whole subtree, so a selected descendant of a selected
        // cue is left out: its own panic_cue would read as a second press and hard-cut it.
        case .panicSelected:
            let roots = CueEditor.topLevel(selected.compactMap { wc.cueList.node($0) })
            await runAll(roots.map { .panicCue(cue: $0.id) }, alongside: previewStops(of: selected, in: wc), on: store)
        case .previewSelected:
            // QLab starts a preview of every selected audio cue and group; a press on one already being
            // previewed does nothing (checks V1, V3), so it is left out rather than sent for the engine to
            // refuse. A Network cue's preview sends its message once (the inspector's Send) and holds no
            // state, so it is never in `previewing` and goes out on every press.
            let nodes = selected.compactMap { wc.cueList.node($0) }
            let previewable = nodes.filter { node in
                switch node.cue.kindType {
                case "network": return true
                case "audio", "group":
                    return !store.previewing.contains(node.id)
                        && !coveredByGroupPreview(node, nodes, previewing: store.previewing)
                default: return false
                }
            }
            await runAll(previewable.map { .preview(cue: $0.id) }, on: store)
        case .pauseResumeSelected:
            let actions: [TransportAction] = selected.compactMap { id in
                switch store.cueStates[id]?.state {
                // A cue that broke while paused (a playlist child held under Pause) reads `broken`;
                // the wire does not say it is also paused. Resume on a broken cue that is not paused
                // is a no-op in the engine, so offering it is safe and the only way to release the hold.
                case .paused?, .broken?: return .resume(cue: id)
                case .running?, .fading?, .preWaiting?, .postWaiting?: return .pause(cue: id)
                default: return nil
                }
            }
            await runAll(actions, on: store)
        }
    }

    /// A selected audio cue or group inside a selected, armed group is already started by that group's
    /// preview, so a press names only the group (the engine would warn "already being previewed"). A
    /// child under a disarmed group is not covered: the group's preview leaves it out.
    private func coveredByGroupPreview(_ node: CueNode, _ selected: [CueNode], previewing: Set<ModelID>) -> Bool {
        let ids = Set(selected.map(\.id))
        var disarmedBelow = node.cue.raw["armed"] == .bool(false)
        var up = node.parent
        while let ancestor = up {
            let armed = ancestor.cue.raw["armed"] != .bool(false)
            // An ancestor already previewing is not sent by this press, so it starts nothing.
            if ids.contains(ancestor.id), armed, !disarmedBelow, !previewing.contains(ancestor.id) { return true }
            disarmedBelow = disarmedBelow || !armed
            up = ancestor.parent
        }
        return false
    }

    /// A preview is a playing cue to the operator, so Stop and Panic on the selection end it too (QLab
    /// treats a previewed cue as an ordinary playing one). One stop per selected previewing cue: a
    /// group's stop ends only the previews it started, not a child previewed on its own. They go
    /// apart from the cue's own stop or panic, which a batch could not hold (it cannot name a cue
    /// twice), but in the same send, so nothing lands between them.
    private func previewStops(of selected: [ModelID], in wc: WorkspaceWindowController) -> [TransportAction] {
        selected.filter { wc.store.previewing.contains($0) }.map { .stopPreview(cue: $0) }
    }

    /// The selected cues act together: one `batch`, which the engine carries out at one scheduling
    /// time, so cues started together start on the same sample (NFR-21). A selection past the
    /// engine's batch limit goes as several batches, all out before any result is awaited. A
    /// repeated action is dropped (the engine refuses a batch naming a cue twice). Each failure is
    /// logged; one beep.
    /// `alongside` actions go as messages of their own, ahead of the batches but in the same send.
    private func runAll(
        _ actions: [TransportAction], alongside separate: [TransportAction] = [], on store: WorkspaceStore
    ) async {
        var seen = Set<TransportAction>()
        let actions = actions.filter { seen.insert($0).inserted }
        let separate = separate.filter { seen.insert($0).inserted }
        let batches: [TransportAction] =
            actions.count > 1
            ? stride(from: 0, to: actions.count, by: TransportAction.maxBatch).map {
                .batch(actions: Array(actions[$0..<min($0 + TransportAction.maxBatch, actions.count)]))
            } : actions
        let messages = separate + batches
        guard messages.count > 1 else {
            if let only = messages.first { await run(only, on: store) }
            return
        }
        let failures = await store.transport(messages).compactMap { r -> EngineError? in
            if case .failure(let e) = r { return e } else { return nil }
        }
        guard !failures.isEmpty else { return }
        beep()
        for e in failures { log(e, on: store) }
    }

    /// Esc: every workspace, by the engine's app-scope panic.
    func panicAll() async -> Result<OkBody, EngineError> {
        let result = await session.panicAll()
        if case .failure(let e) = result {
            beep()
            for (_, store) in session.orderedStores { log(e, on: store) }
        }
        return result
    }

    /// Sends a transport action. A refused or undeliverable one (not connected, timeout, refusal)
    /// beeps and goes to the show log: an operator pressing GO must never be left guessing.
    @discardableResult
    func run(_ action: TransportAction, on store: WorkspaceStore) async -> Result<OkBody, EngineError> {
        let result = await store.transport(action)
        if case .failure(let e) = result { report(e, on: store) }
        return result
    }

    private func report(_ error: EngineError, on store: WorkspaceStore) {
        beep()
        log(error, on: store)
    }

    func log(_ error: EngineError, on store: WorkspaceStore) {
        _ = store.receive(.log(.error, String(format: loc("error.transportFailed"), describe(error))))
    }

    /// Asks for a folder and has the engine relink missing media from it (WS-03). The outcome is
    /// told to the operator and written to the show log; the cues that were fixed leave the
    /// Warnings tab by themselves when the engine reports them playable.
    func relinkMedia(on store: WorkspaceStore) async {
        // It rewrites the workspace's media paths: an edit, locked in Show Mode (SM-01).
        if controllers[ObjectIdentifier(store)]?.isShowMode == true {
            NSSound.beep()
            return
        }
        guard let folder = pickFolderURL() else { return }
        switch await store.relink(folder: folder.path) {
        case .success(let ok):
            guard case .relink(let relinked, let ambiguous, let unmatched, _, _)? = ok.data else {
                report(.encoding("relink result has no relink data"), on: store)
                return
            }
            let message = Self.relinkSummary(relinked: relinked, ambiguous: ambiguous.count, unmatched: unmatched.count)
            // Not modal: a relink is done while the show goes on. The summary is in the footer and
            // the log, with one log record for every cue that still needs attention.
            let clean = ambiguous.isEmpty && unmatched.isEmpty
            _ = store.receive(.log(clean ? .info : .warn, message))
            func name(_ id: ModelID) -> String {
                store.model.flatMap { m in m.cue(id).map { WarningRows.cueTitle($0, name: m.listName($0)) } } ?? id.raw
            }
            for a in ambiguous {
                let text = String(
                    format: loc("relink.log.ambiguous"), name(a.cue), a.candidates.joined(separator: ", "))
                _ = store.receive(.log(.warn, text))
            }
            for id in unmatched {
                _ = store.receive(.log(.warn, String(format: loc("relink.log.unmatched"), name(id))))
            }
            controllers[ObjectIdentifier(store)]?.footer.showNotice(message)
        case .failure(let e):
            let message = String(format: loc("relink.failed"), describe(e))
            raise(message, from: store)
        }
    }

    static func relinkSummary(relinked: Int, ambiguous: Int, unmatched: Int) -> String {
        var parts = [String(format: loc("relink.relinked"), relinked)]
        if ambiguous > 0 { parts.append(String(format: loc("relink.ambiguous"), ambiguous)) }
        if unmatched > 0 { parts.append(String(format: loc("relink.unmatched"), unmatched)) }
        return parts.joined(separator: " ")
    }

    /// Opens the log window of the key workspace (the first one when none is key).
    func showLog() {
        (frontController ?? allWindows.first)?.showLog()
    }

    @discardableResult
    func go(_ store: WorkspaceStore) async -> Result<OkBody, EngineError>? {
        guard let list = controllers[ObjectIdentifier(store)]?.cueList.listID else { return nil }
        return await run(.go(list: list), on: store)
    }
}
