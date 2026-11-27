// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// File New / Open, closing a workspace and quitting: the prompts (playing, Show Mode, unsaved
/// changes) and the bounded waits on the engine.
extension AppController {
    // MARK: File New / Open

    func newWorkspace() async {
        await afterLaunch()
        await createWorkspace()
    }

    /// `newWorkspace` without the wait for the engine: what the launch choice itself uses.
    func createWorkspace() async {
        await opened { await self.session.newWorkspace() }
    }

    func openWorkspace() async {
        guard let url = pickOpenURL() else { return }
        await openWorkspace(url: url)
    }

    /// `onFailure`: told of a failed open instead of the error alert (the launch restore).
    func openWorkspace(url: URL, onFailure: ((EngineError) -> Void)? = nil) async {
        await afterLaunch()
        await openNow(url: url, onFailure: onFailure)
    }

    /// `openWorkspace(url:)` without the wait for the engine: what the launch choice itself uses.
    func openNow(url: URL, onFailure: ((EngineError) -> Void)? = nil) async {
        let result = await opened(onFailure: onFailure) {
            let first = await self.session.open(path: url.path)
            // An autosave newer than the file (NFR-05): the engine opened nothing; ask, then open.
            guard case .failure(.recoveryOffered(_, let stored, let modified)) = first else { return first }
            // Named as the window will be: after the file, whatever name the show stores.
            let name = WorkspaceInfo.shownName(stored: stored, path: url.path)
            // A restore at launch raises no modal over a window in Show Mode (NFR-10), or one that may
            // yet enter it: the file stays closed, with its autosave, and the caller says so.
            if let onFailure, !self.restorePromptIsSafe {
                onFailure(.recoveryOffered(path: url.path, workspaceName: name, modifiedMs: modified))
                return first
            }
            guard let choice = self.alerts.recoveryOffer(workspace: name, modifiedMs: modified) else { return first }
            return await self.session.open(path: url.path, recovery: choice)
        }
        if case .success = result {
            noteRecent(url)
        } else if case .failure(.engine(.alreadyOpen, _)) = result,
            let doc = session.workspaces.first(where: { $0.path == url.path })?.doc
        {
            show(doc)
        }
    }

    /// Runs a new/open call: another workspace opens beside the ones open (protocol 2), so nothing
    /// unsaved is in the way.
    @discardableResult
    func opened(
        onFailure: ((EngineError) -> Void)? = nil, _ call: () async -> Result<ModelID, EngineError>
    ) async -> Result<ModelID, EngineError> {
        let result = await call()
        switch result {
        case .success(let doc): show(doc)
        case .failure(.engine(.alreadyOpen, _)), .failure(.recoveryOffered): break  // shown; or Cancel
        case .failure(let e):
            if let onFailure {
                onFailure(e)
            } else {
                gatedAlerts.showError(String(format: loc("error.openFailed"), describe(e)))
            }
        }
        return result
    }

    // MARK: Close

    /// The window of `store` asked to close. A window waiting for its show to be reopened after
    /// an engine restart has no doc to close yet: it stays (beep).
    @discardableResult
    func closeWorkspace(_ store: WorkspaceStore) async -> Bool {
        guard let doc = session.doc(of: store) else {
            beep()
            return false
        }
        return await closeWorkspace(doc)
    }

    /// The user asked to close a workspace window. False: it stays open.
    @discardableResult
    func closeWorkspace(_ doc: ModelID) async -> Bool {
        guard let store = session.stores[doc], !quitting, closing.insert(doc).inserted else { return false }
        defer { closing.remove(doc) }
        // One prompt: playing outranks Show Mode (QLab asks before closing a workspace in Show Mode).
        if isPlaying(store) {
            if !alerts.confirmCloseWhilePlaying(workspace: name(of: doc)) { return false }
        } else if windows[doc]?.isShowMode == true, !alerts.confirmCloseInShowMode(workspace: name(of: doc)) {
            return false
        }
        guard let discard = await resolveUnsaved(doc) else { return false }
        switch await bounded({ await self.session.close(doc: doc, discardChanges: discard) }) {
        case .success?:
            dropWindow(ObjectIdentifier(store))
        case .failure(let e)?:
            raise(String(format: loc("error.closeFailed"), describe(e)), from: store)
            return false
        case nil:
            raise(String(format: loc("error.closeFailed"), loc("error.detail.timeout")), from: store)
            return false
        }
        showLauncherIfNoWindows()
        return true
    }

    /// File > Close All: closes every workspace the way Close does (save prompt, Show Mode and
    /// playing confirmations), one after another; Cancel on any of them stops the rest.
    func closeAllWorkspaces() async {
        for doc in session.orderedStores.map(\.doc) {
            if !(await closeWorkspace(doc)) { return }
        }
    }

    // MARK: Quit

    /// True when the app may quit: the playing confirmation and every dirty workspace's save
    /// prompt were answered without Cancel, and `goodbye { shutdown }` went out.
    func prepareToQuit() async -> Bool {
        guard !quitting else { return false }
        savedAsPaths = [:]
        guard closing.isEmpty else {
            NSSound.beep()  // never a silent no-op: a close is still being answered
            return false
        }
        quitting = true
        defer { quitting = false }
        if session.connectionState == .retrying(.engineStopping) {
            // The engine is shutting down: no save, close or goodbye can reach it, and it writes
            // the recovery file of every workspace with unsaved changes itself. Nothing is left
            // running behind the app, so there is nothing to confirm (#386).
            engineLog.notice("quit while the engine is shutting down: no save prompts")
            keepUnsentNotes()
            supervisor?.appWillQuit()
            return true
        }
        if engineUnreachable {
            // An engine still coming up plays nothing and is ended at quit: nothing to warn about.
            if supervisor?.engineNotReached == nil, !alerts.confirmQuitDisconnected() { return false }
            keepUnsentNotes()
            supervisor?.appWillQuit()
            return true
        }
        for doc in session.order {
            if await windows[doc]?.commitPendingEdits() == false { return false }
        }
        if session.stores.values.contains(where: isPlaying) {
            if !alerts.confirmQuitWhilePlaying() { return false }
        } else if controllers.values.contains(where: \.isShowMode), !alerts.confirmQuitInShowMode() {
            return false
        }
        // What is typed in the settings takes effect at once, so only once the quit goes on.
        for doc in session.order {
            if await !commitSettings(doc) { return false }
        }
        var discarded: [ModelID] = []
        for info in session.workspaces where session.stores[info.doc]?.document?.dirty == true {
            show(info.doc)
            guard let discard = await resolveUnsaved(info.doc) else { return false }
            if discard { discarded.append(info.doc) }
        }
        // The quit is going ahead (no prompt was cancelled): what is open now, saved or not, is what
        // the next launch reopens. Not while launching, or with an engine not reached: the list
        // would be an incomplete one.
        if session.connectionState == .connected, !launching { recordOpenWorkspaces() }
        // Don't Save is answered only now that no later prompt cancelled the quit. The shutdown
        // writes a recovery file for every dirty workspace, so the dropped edits are discarded
        // first (the engine removes their autosave too). A stalled or failed close must not trap
        // the app: the workspace is then autosaved as before.
        for doc in discarded {
            let closed = await bounded { await self.session.close(doc: doc, discardChanges: true) }
            if case .success? = closed { continue }
            // A stalled engine would make each remaining close wait out its own bound.
            engineLog.error("could not discard the unsaved edits of doc \(doc.raw, privacy: .public) at quit")
            break
        }
        // A stalled flush must not trap the app; the engine still sees the socket drop.
        _ = await bounded { await self.session.shutdown() }
        supervisor?.appWillQuit()
        return true
    }

    // MARK: Helpers

    /// Quit with the engine lost: notes still being typed go to the notes file.
    private func keepUnsentNotes() {
        for wc in controllers.values { wc.keepUnsentNotes() }
    }

    /// Not connected although an engine may be running (so quitting would orphan it). No engine
    /// file / never connected is a normal state and quits freely.
    private var engineUnreachable: Bool {
        switch session.connectionState {
        case .connected, .idle, .retrying(.engineNotAvailable): return false
        default: return true
        }
    }

    /// `op`'s result, or nil if it has not finished within `timeout`. The timer holds only the gate,
    /// never this controller, and is cancelled once `op` answers: a timer outliving the close kept
    /// the controller, and with it every workspace window it had made, until it ran out.
    private func bounded<T: Sendable>(_ op: @escaping @MainActor () async -> T) async -> T? {
        let gate = Gate<T>()
        let (scheduler, timeout) = (scheduler, timeout)
        return await withCheckedContinuation { (cont: CheckedContinuation<T?, Never>) in
            gate.cont = cont
            let timer = Task { @MainActor in
                try? await scheduler.sleep(seconds: timeout)
                gate.finish(nil)
            }
            Task { @MainActor in
                gate.finish(await op())
                timer.cancel()
            }
        }
    }

    @MainActor private final class Gate<T: Sendable> {
        var cont: CheckedContinuation<T?, Never>?
        func finish(_ value: T?) {
            cont?.resume(returning: value)
            cont = nil
        }
    }

    func describe(_ e: EngineError) -> String {
        switch e {
        case .notConnected, .disconnected: return loc("error.detail.notConnected")
        case .engine(_, let message) where !message.isEmpty: return message
        case .timeout: return loc("error.detail.timeout")
        default:
            engineLog.error("engine call failed: \(String(describing: e), privacy: .public)")
            return loc("error.detail.generic")
        }
    }

    /// Any live cue: paused and waiting ones would lose their place on a close or quit too.
    private func isPlaying(_ store: WorkspaceStore) -> Bool {
        store.cueStates.values.contains { $0.state.isActive }
    }

    private func name(of doc: ModelID) -> String {
        session.workspaces.first { $0.doc == doc }?.name ?? session.stores[doc]?.model?.name ?? ""
    }

    /// The unsaved-changes prompt. nil: cancelled (or the save failed or was cancelled). Otherwise
    /// the `discard_changes` flag to send: true after Don't Save, false when clean or saved.
    private func resolveUnsaved(_ doc: ModelID) async -> Bool? {
        guard await commitEdits(doc), await commitSettings(doc) else { return nil }
        guard let store = session.stores[doc], store.document?.dirty == true else { return false }
        switch alerts.unsavedChanges(workspace: name(of: doc)) {
        case .cancel: return nil
        case .dontSave: return true
        case .save: return await save(store, name: name(of: doc)) ? false : nil
        }
    }

    /// Typed-in inspector edits go to the engine before a save, close or quit looks at the workspace.
    private func commitEdits(_ doc: ModelID) async -> Bool {
        await windows[doc]?.commitPendingEdits() ?? true
    }

    /// What is typed in the workspace settings, before a close or quit looks at the workspace.
    private func commitSettings(_ doc: ModelID) async -> Bool {
        await windows[doc]?.existingSettingsWindow?.commitPendingEdits() ?? true
    }

    /// File > Save / Save As (allowed in Show Mode, as in QLab). `asNew`: always ask for a path.
    /// An untitled workspace asks either way. Typed-in inspector edits are sent first.
    @discardableResult
    func saveWorkspace(_ store: WorkspaceStore, asNew: Bool = false) async -> Bool {
        guard let doc = session.doc(of: store) else {
            beep()
            return false
        }
        guard await commitEdits(doc) else { return false }
        // Settings already committed go into the file; what is still typed there stays typed.
        await windows[doc]?.existingSettingsWindow?.edits.answered()
        return await save(store, name: name(of: doc), asNew: asNew)
    }

    /// Save and Save As need a front workspace the engine has open. Show Mode does not block them.
    var canSaveFront: Bool {
        guard let store = frontController?.store else { return false }
        return session.doc(of: store) != nil
    }

    /// Recent workspaces the Open Recent menu offers: those not known to be gone, unreachable or in the
    /// Trash. A file not yet answered for is offered; the menu is filled again when an answer comes.
    func existingRecentURLs() -> [URL] {
        recentChecker.refresh(recentURLs(), bookmarks: recentBookmarks)
        return openableRecentURLs()
    }

    func openableRecentURLs() -> [URL] {
        recentURLs().filter { !isMissingRecent($0) && recentChecker.state(of: $0) != .unreachable }
    }

    /// An answer came for a recent file: the launcher and an open Open Recent menu are drawn again.
    func recentsChecked() {
        reloadLauncher(recheck: false)
        recentMenuRefill?()
    }

    func clearRecents() {
        forgetRecents()
        rememberedRecents = []
        reloadLauncher()
    }

    /// Takes one workspace off the recents list, keeping the others in order.
    func forgetRecent(_ url: URL) {
        let keep = recentURLs().filter { $0 != url }
        forgetRecents()
        for kept in keep.reversed() { noteRecent(kept) }
        rememberedRecents = rememberedRecents.filter { $0 != url }
        reloadLauncher()
    }

    /// The launcher lists the system's recent workspaces, then the ones it listed before whose file
    /// has gone (the system forgets those), marked as missing. A remembered file that was moved or
    /// renamed is found again by its bookmark and is not listed as gone (the system lists it at its
    /// new place); a file the bookmark cannot find is deleted, and stays listed as missing. The list
    /// is drawn at once from what the checker already knows; each file is checked off the main
    /// thread (`recheck`) and the list is drawn again as the answers come.
    func reloadLauncher(recheck: Bool = true) {
        let system = recentURLs()
        if recheck {
            recentChecker.refresh(Array(Set(system + rememberedRecents)), bookmarks: recentBookmarks)
        }
        // A remembered file the system no longer lists stays until a check says it is there (the
        // system lists it) or was moved; one not yet checked is listed as it was.
        let gone = rememberedRecents.filter { old in
            guard !system.contains(old) else { return false }
            switch recentChecker.state(of: old) {
            case .found: return false
            case .checking, .unreachable: return true
            case .missing:
                guard let now = recentChecker.probes[old]?.movedTo else { return true }
                // Moved (listed at its new place), or in the Trash, which is deleted; the system's
                // own entry for a trashed show is the one listed, as missing.
                return Self.inTrash(now) && !system.contains(now)
            }
        }
        let all = Array((system + gone).prefix(10))
        rememberedRecents = all
        var marks = recentBookmarks
        for url in all where marks[url.path] == nil {
            if let made = recentChecker.probes[url]?.newBookmark { marks[url.path] = made }
        }
        recentBookmarks = marks.filter { key, _ in all.contains { $0.path == key } }
        launcher.reload(
            recents: all, missing: Set(all.filter(isMissingRecent)),
            unreachable: Set(all.filter { recentChecker.state(of: $0) == .unreachable }))
    }

    /// A recent workspace the operator cannot open: its file is gone, or it is in the Trash. The
    /// rows are marked, and a choice is refused, by this one test. A file not answered for yet is
    /// not missing.
    func isMissingRecent(_ url: URL) -> Bool { recentChecker.state(of: url) == .missing || Self.inTrash(url) }

    /// A show moved to the Trash is deleted as far as the operator is concerned: it is listed as
    /// missing, never as one that opens from there.
    static func inTrash(_ url: URL) -> Bool {
        url.pathComponents.contains { $0 == ".Trash" || $0 == ".Trashes" }
    }

    /// The copy points the cues at their copies as an edit, which would leave a workspace the
    /// operator just saved with unsaved changes: once the copy ends, the workspace is saved again when
    /// it is unsaved and the operator sent no edit since `edits` (the count before the save began), so
    /// the window shows no unsaved mark and a close asks nothing. A change of the operator's own is
    /// never written without asking. Registered before the copy is asked for, so a copy that ends
    /// before the reply is handled is still seen.
    private func saveAgainWhenCopyEnds(_ store: WorkspaceStore, edits: Int) {
        let key = ObjectIdentifier(store)
        mediaCopyFollowers[key] = store.mediaCopied.first().sink { [weak self, weak store] _ in
            MainActor.assumeIsolated {
                self?.mediaCopyFollowers[key] = nil
                // Only the store is held while the engine answers (its client fails the request if it
                // stops): an app or window closed meanwhile is not kept alive by the save.
                Task { @MainActor [weak store] in
                    // The `document` message with the edit's dirty mark comes before the copy's report.
                    guard let store, store.document?.dirty == true, store.editsSent == edits else { return }
                    _ = await store.save()
                }
            }
        }
    }

    private func save(_ store: WorkspaceStore, name: String, asNew: Bool = false) async -> Bool {
        var saveAsURL: URL?
        let reply: Result<OkBody, EngineError>?
        var copyMedia = false
        let editsBefore = store.editsSent
        if asNew || store.document?.path == nil {
            let copies = store.isOn(.copyMediaOnAdd) ?? WorkspaceSwitch.copyMediaOnAdd.fallback
            guard let target = pickSaveURL(name, copies) else { return false }
            var url = target.url
            if target.newFolder {
                switch await SaveTarget.makeShowFolder(for: url) {
                case .success(let inside):
                    url = inside
                    copyMedia = true
                case .failure(let e):
                    raise(String(format: loc("error.saveFailed"), e.localizedDescription), from: store)
                    return false
                }
            }
            saveAsURL = url
            reply = await bounded { await store.saveAs(path: url.path) }
        } else {
            reply = await bounded { await store.save() }
        }
        switch reply {
        case .success?:
            if let saveAsURL {
                savedAsPaths[ObjectIdentifier(store)] = saveAsURL.path
                noteRecent(saveAsURL)
            }
            // The new show folder takes a copy of every file the cues target (WS-04); the copy runs
            // in the engine and its end shows in the window's footer.
            if copyMedia {
                saveAgainWhenCopyEnds(store, edits: editsBefore)
                if case .failure(let e) = await store.copyMediaIntoShowFolder() {
                    mediaCopyFollowers[ObjectIdentifier(store)] = nil
                    raise(describe(e), from: store)
                }
            }
            return true
        case .failure(let e)?:
            raise(String(format: loc("error.saveFailed"), describe(e)), from: store)
        case nil:
            raise(String(format: loc("error.saveFailed"), loc("error.detail.timeout")), from: store)
        }
        return false
    }
}
