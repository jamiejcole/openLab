// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Transport commands from menu items and keys: each workspace's transport keys, GO, the
/// selected-cue actions, Panic, Relink Media and the show log they report failures to.
extension AppController {
    // MARK: Transport commands (menu items and keys)

    /// The transport keys of `store`'s workspace. A show file whose hotkeys conflict or are
    /// unusable is logged to that store once per change (never a crash).
    func keys(for store: WorkspaceStore) -> TransportKeys {
        let settings = store.model?.settings
        let id = ObjectIdentifier(store)
        let cueChords = Set((store.model?.cueHotkeys ?? []).map(\.chord))
        let cached = keyCache[id]
        let same = cached?.settings == settings && cached?.cueChords == cueChords
        let keys =
            same
            ? (cached?.keys ?? TransportKeys(settings: settings, cueChords: cueChords))
            : TransportKeys(settings: settings, cueChords: cueChords)
        // Keys from the show's settings win over cue hotkeys (a cue hotkey on one does not fire); the
        // fixed app defaults (L, P, V) already yielded to cue hotkeys inside `TransportKeys`.
        // A menu command's key (Cmd-J, Shift-Cmd-Down, ...) is taken before a cue hotkey is looked
        // at (`fireCueHotkeys`), so a cue on one never fires either: warn for it the same way.
        let transportShadowed = cueChords.intersection(keys.chords.values)
        let menuShadowed = cueChords.intersection(menuChords()).subtracting(transportShadowed)
        let shadowed = transportShadowed.union(menuShadowed)
        if let cached, same, cached.shadowed == shadowed { return cached.keys }
        keyCache[id] = (settings, cueChords, shadowed, keys)
        if !same {
            for warning in keys.warnings { _ = store.receive(.log(.warn, warning)) }
        }
        for chord in shadowed.subtracting(cached?.shadowed ?? []) {
            let key = transportShadowed.contains(chord) ? "hotkeys.warn.shadowed" : "hotkeys.warn.menuShadowed"
            _ = store.receive(.log(.warn, String(format: loc(key), chord.display)))
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

    /// Runs `command` on `target`, else the key window's workspace (Panic: every workspace). A held key is ignored
    /// after its first press: a repeating GO would fire cue after cue.
    func perform(_ command: TransportCommand, isRepeat: Bool = false, on target: WorkspaceWindowController? = nil)
        async
    {
        guard !isRepeat else { return }
        if command == .panic {
            _ = await panicAll()
            return
        }
        guard let wc = target ?? frontController else { return }
        let store = wc.store
        let selected = wc.cueList.selectedIDs()
        let list = wc.cueList.listID  // the list picked in the sidebar
        switch command {
        case .go: _ = await go(store)
        case .pauseAll: await run(.pauseAll, on: store)
        case .resumeAll: await run(.resumeAll, on: store)
        case .stopAll: await run(.stopAll, on: store)  // atomic in the engine: every cue and preview
        case .panic: break
        case .loadSelected: if let id = selected.last { await run(.load(cue: id, at: 0), on: store) }
        case .startSelected: await runAll(selected.map { .start(cue: $0) }, on: store)
        case .stopSelected: await runAll(selected.map { .hardStop(cue: $0) }, on: store)
        case .resetSelected: await runAll(selected.map { .reset(cue: $0) }, on: store)
        case .previewSelected:
            // Audio cues only (the engine ignores other kinds); each toggles on its own preview state.
            let audio = selected.filter { store.model?.cue($0)?.kindType == "audio" }
            await runAll(
                audio.map { store.previewing.contains($0) ? .stopPreview(cue: $0) : .preview(cue: $0) }, on: store)
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

    /// The selected cues act together: one `batch`, which the engine carries out at one scheduling
    /// time, so cues started together start on the same sample (NFR-21). A selection past the
    /// engine's batch limit goes as several batches, all out before any result is awaited. A
    /// repeated action is dropped (the engine refuses a batch naming a cue twice). Each failure is
    /// logged; one beep.
    private func runAll(_ actions: [TransportAction], on store: WorkspaceStore) async {
        var seen = Set<TransportAction>()
        let actions = actions.filter { seen.insert($0).inserted }
        guard actions.count > 1 else {
            if let only = actions.first { await run(only, on: store) }
            return
        }
        let batches = stride(from: 0, to: actions.count, by: TransportAction.maxBatch).map {
            TransportAction.batch(actions: Array(actions[$0..<min($0 + TransportAction.maxBatch, actions.count)]))
        }
        let failures = await store.transport(batches).compactMap { r -> EngineError? in
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
            func name(_ id: ModelID) -> String { store.model?.cue(id).map(WarningRows.cueTitle) ?? id.raw }
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
