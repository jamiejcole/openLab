// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// File New / Open, closing a workspace and quitting: the prompts (playing, Show Mode, unsaved
/// changes) and the bounded waits on the engine.
extension AppController {
    // MARK: File New / Open

    func newWorkspace() async {
        await opened { await self.session.newWorkspace() }
    }

    func openWorkspace() async {
        guard let url = pickOpenURL() else { return }
        await openWorkspace(url: url)
    }

    func openWorkspace(url: URL) async {
        let result = await opened {
            let first = await self.session.open(path: url.path)
            // An autosave newer than the file (NFR-05): the engine opened nothing; ask, then open.
            guard case .failure(.recoveryOffered(_, let name, let modified)) = first else { return first }
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
    private func opened(_ call: () async -> Result<ModelID, EngineError>) async -> Result<ModelID, EngineError> {
        let result = await call()
        switch result {
        case .success(let doc): show(doc)
        case .failure(.engine(.alreadyOpen, _)), .failure(.recoveryOffered): break  // shown; or Cancel
        case .failure(let e): gatedAlerts.showError(String(format: loc("error.openFailed"), describe(e)))
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
            supervisor?.appWillQuit()
            return true
        }
        if engineUnreachable {
            // An engine still coming up plays nothing and is ended at quit: nothing to warn about.
            if supervisor?.engineNotReached == nil, !alerts.confirmQuitDisconnected() { return false }
            supervisor?.appWillQuit()
            return true
        }
        for doc in session.order {
            if await !commitEdits(doc) { return false }
        }
        if session.stores.values.contains(where: isPlaying) {
            if !alerts.confirmQuitWhilePlaying() { return false }
        } else if controllers.values.contains(where: \.isShowMode), !alerts.confirmQuitInShowMode() {
            return false
        }
        var discarded: [ModelID] = []
        for info in session.workspaces where session.stores[info.doc]?.document?.dirty == true {
            show(info.doc)
            guard let discard = await resolveUnsaved(info.doc) else { return false }
            if discard { discarded.append(info.doc) }
        }
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

    /// Not connected although an engine may be running (so quitting would orphan it). No engine
    /// file / never connected is a normal state and quits freely.
    private var engineUnreachable: Bool {
        switch session.connectionState {
        case .connected, .idle, .retrying(.engineNotAvailable): return false
        default: return true
        }
    }

    /// `op`'s result, or nil if it has not finished within `timeout`.
    private func bounded<T: Sendable>(_ op: @escaping @MainActor () async -> T) async -> T? {
        let gate = Gate<T>()
        return await withCheckedContinuation { (cont: CheckedContinuation<T?, Never>) in
            gate.cont = cont
            Task { @MainActor in gate.finish(await op()) }
            Task { @MainActor in
                try? await scheduler.sleep(seconds: timeout)
                gate.finish(nil)
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
        guard await commitEdits(doc) else { return nil }
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

    /// File > Save / Save As (allowed in Show Mode, as in QLab). `asNew`: always ask for a path.
    /// An untitled workspace asks either way. Typed-in inspector edits are sent first.
    @discardableResult
    func saveWorkspace(_ store: WorkspaceStore, asNew: Bool = false) async -> Bool {
        guard let doc = session.doc(of: store) else {
            beep()
            return false
        }
        guard await commitEdits(doc) else { return false }
        return await save(store, name: name(of: doc), asNew: asNew)
    }

    /// Save and Save As need a front workspace the engine has open. Show Mode does not block them.
    var canSaveFront: Bool {
        guard let store = frontController?.store else { return false }
        return session.doc(of: store) != nil
    }

    /// Recent workspaces whose file is still there: a moved or deleted one would fail on every choice.
    func existingRecentURLs() -> [URL] {
        recentURLs().filter(recentExists)
    }

    func clearRecents() {
        forgetRecents()
        launcher.reload(recents: existingRecentURLs())
    }

    private func save(_ store: WorkspaceStore, name: String, asNew: Bool = false) async -> Bool {
        var saveAsURL: URL?
        let reply: Result<OkBody, EngineError>?
        if asNew || store.document?.path == nil {
            guard let url = pickSaveURL(name) else { return false }
            saveAsURL = url
            reply = await bounded { await store.saveAs(path: url.path) }
        } else {
            reply = await bounded { await store.save() }
        }
        switch reply {
        case .success?:
            if let saveAsURL { noteRecent(saveAsURL) }
            return true
        case .failure(let e)?:
            raise(String(format: loc("error.saveFailed"), describe(e)), from: store)
        case nil:
            raise(String(format: loc("error.saveFailed"), loc("error.detail.timeout")), from: store)
        }
        return false
    }
}
