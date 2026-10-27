// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine
import UniformTypeIdentifiers

/// The app-level owner of the windows (ARCHITECTURE section 9): one `WorkspaceWindowController`
/// per workspace the session holds, each given only its own store, plus the launcher shown when
/// none is open. It also runs File New/Open, the close and quit flows and the front-window
/// routing. No NSDocument.
@MainActor
final class AppController {
    let session: EngineSession
    private(set) var windows: [ModelID: WorkspaceWindowController] = [:]
    let launcher = LauncherWindowController()

    // Seams for tests (no modal loops, no panels, no real recents list or key window).
    var alerts: WorkspaceAlerts = AppKitAlerts()
    var pickOpenURL: @MainActor () -> URL? = AppController.openPanel
    var pickSaveURL: @MainActor (String) -> URL? = AppController.savePanel
    var noteRecent: @MainActor (URL) -> Void = { NSDocumentController.shared.noteNewRecentDocumentURL($0) }
    var recentURLs: @MainActor () -> [URL] = { NSDocumentController.shared.recentDocumentURLs }
    var keyWindow: @MainActor () -> NSWindow? = { NSApp.keyWindow }

    private var subs: [AnyCancellable] = []

    init(session: EngineSession) {
        self.session = session
        launcher.onNew = { [weak self] in Task { await self?.newWorkspace() } }
        launcher.onOpen = { [weak self] in Task { await self?.openWorkspace() } }
        launcher.onOpenRecent = { [weak self] url in Task { await self?.openWorkspace(url: url) } }
        // $stores / $workspaces publish before the property changes: use the delivered values.
        subs.append(session.$stores.sink { [weak self] in self?.sync(stores: $0) })
        subs.append(session.$workspaces.sink { [weak self] in self?.updateTitles($0) })
    }

    // MARK: Windows

    private func sync(stores: [ModelID: WorkspaceStore]) {
        for doc in stores.keys.sorted(by: { $0.raw < $1.raw }) where windows[doc] == nil {
            guard let store = stores[doc] else { continue }
            let wc = WorkspaceWindowController(store: store)
            wc.onCloseRequest = { [weak self] in Task { await self?.closeWorkspace(doc) } }
            wc.onGo = { [weak self] in Task { await self?.go(store) } }
            wc.onPauseAll = { Task { _ = await store.transport(.pauseAll) } }
            wc.onPanic = { [weak self] in Task { _ = await self?.panicAll() } }
            windows[doc] = wc
            if let n = session.workspaces.first(where: { $0.doc == doc }) { wc.window?.title = n.name }
            show(doc)
        }
        for doc in windows.keys where stores[doc] == nil {
            windows.removeValue(forKey: doc)?.window?.close()
        }
        showLauncherIfNoWindows()
    }

    private func updateTitles(_ list: [WorkspaceInfo]) {
        for info in list {
            guard let window = windows[info.doc]?.window else { continue }
            window.title = info.name
            window.isDocumentEdited = info.dirty
        }
    }

    /// Fronts (creating nothing) the window of `doc` and hides the launcher.
    func show(_ doc: ModelID) {
        guard let window = windows[doc]?.window else { return }
        if window.isMiniaturized { window.deminiaturize(nil) }
        window.makeKeyAndOrderFront(nil)
        launcher.window?.orderOut(nil)
    }

    private var hasWorkspaceWindow: Bool {
        windows.values.contains { $0.window?.isVisible == true || $0.window?.isMiniaturized == true }
    }

    func showLauncherIfNoWindows() {
        if !hasWorkspaceWindow { showLauncher() }
    }

    func showLauncher() {
        launcher.reload(recents: recentURLs())
        launcher.window?.makeKeyAndOrderFront(nil)
    }

    /// Dock click with nothing on screen: bring back a minimised workspace, else the launcher.
    func reopen() {
        if let doc = windows.first(where: { $0.value.window?.isMiniaturized == true })?.key {
            show(doc)
        } else if !hasWorkspaceWindow {
            showLauncher()
        }
    }

    func closeAllWindows() {
        for wc in windows.values { wc.window?.close() }
        launcher.window?.close()
    }

    // MARK: Front-window routing

    var frontController: WorkspaceWindowController? {
        guard let key = keyWindow() else { return nil }
        return windows.values.first { $0.window === key }
    }

    /// GO on the first cue list of the key window's workspace. (The selected list arrives with
    /// the sidebar, M5.8; full transport routing is M5.11.)
    @discardableResult
    func goFront() async -> Result<OkBody, EngineError>? {
        guard let store = frontController?.store else { return nil }
        return await go(store)
    }

    @discardableResult
    func pauseAllFront() async -> Result<OkBody, EngineError>? {
        guard let store = frontController?.store else { return nil }
        return await store.transport(.pauseAll)
    }

    /// Esc: every workspace, by the engine's app-scope panic.
    func panicAll() async -> Result<OkBody, EngineError> { await session.panicAll() }

    @discardableResult
    private func go(_ store: WorkspaceStore) async -> Result<OkBody, EngineError>? {
        guard let list = store.model?.cueLists.first?.id else { return nil }
        return await store.go(list: list)
    }

    // MARK: File New / Open

    func newWorkspace() async {
        await opened { await self.session.newWorkspace(discardChanges: $0) }
    }

    func openWorkspace() async {
        guard let url = pickOpenURL() else { return }
        await openWorkspace(url: url)
    }

    func openWorkspace(url: URL) async {
        let result = await opened { await self.session.open(path: url.path, discardChanges: $0) }
        if case .success = result {
            noteRecent(url)
        } else if case .failure(.engine(.alreadyOpen, _)) = result,
            let doc = session.workspaces.first(where: { $0.path == url.path })?.doc
        {
            show(doc)
        }
    }

    /// Runs a new/open call. A one-workspace engine replaces its workspace in place and refuses
    /// while it is dirty: ask Save / Don't Save for it, then retry.
    @discardableResult
    private func opened(_ call: (Bool) async -> Result<ModelID, EngineError>) async -> Result<ModelID, EngineError> {
        var result = await call(false)
        if case .failure(.engine(.unsavedChanges, _)) = result {
            guard let discard = await resolveUnsaved(EngineSession.singleDoc) else { return result }
            result = await call(discard)
        }
        switch result {
        case .success(let doc): show(doc)
        case .failure(.engine(.alreadyOpen, _)): break
        case .failure(let e): alerts.showError(String(format: loc("error.openFailed"), "\(e)"))
        }
        return result
    }

    // MARK: Close

    /// The user asked to close a workspace window. False: it stays open.
    @discardableResult
    func closeWorkspace(_ doc: ModelID) async -> Bool {
        guard let store = session.stores[doc] else { return false }
        if isPlaying(store), !alerts.confirmCloseWhilePlaying(workspace: name(of: doc)) { return false }
        guard let discard = await resolveUnsaved(doc) else { return false }
        switch await session.close(doc: doc, discardChanges: discard) {
        case .success:
            windows.removeValue(forKey: doc)?.window?.close()
        case .failure(.unsupported):
            // A one-workspace engine cannot close it: hide the window; New / Open bring it back.
            windows[doc]?.window?.close()
        case .failure(let e):
            alerts.showError(String(format: loc("error.closeFailed"), "\(e)"))
            return false
        }
        showLauncherIfNoWindows()
        return true
    }

    // MARK: Quit

    /// True when the app may quit: the playing confirmation and every dirty workspace's save
    /// prompt were answered without Cancel, and `goodbye { shutdown }` went out.
    func prepareToQuit() async -> Bool {
        if session.stores.values.contains(where: isPlaying), !alerts.confirmQuitWhilePlaying() { return false }
        for info in session.workspaces where session.stores[info.doc]?.document?.dirty == true {
            show(info.doc)
            if await resolveUnsaved(info.doc) == nil { return false }
        }
        await session.shutdown()
        return true
    }

    // MARK: Helpers

    private func isPlaying(_ store: WorkspaceStore) -> Bool {
        store.cueStates.values.contains { $0.state == .running || $0.state == .fading }
    }

    private func name(of doc: ModelID) -> String {
        session.workspaces.first { $0.doc == doc }?.name ?? session.stores[doc]?.model?.name ?? ""
    }

    /// The unsaved-changes prompt. nil: cancelled (or the save failed or was cancelled). Otherwise
    /// the `discard_changes` flag to send: true after Don't Save, false when clean or saved.
    private func resolveUnsaved(_ doc: ModelID) async -> Bool? {
        guard let store = session.stores[doc], store.document?.dirty == true else { return false }
        switch alerts.unsavedChanges(workspace: name(of: doc)) {
        case .cancel: return nil
        case .dontSave: return true
        case .save: return await save(store, name: name(of: doc)) ? false : nil
        }
    }

    private func save(_ store: WorkspaceStore, name: String) async -> Bool {
        let result: Result<OkBody, EngineError>
        var saveAsURL: URL?
        if store.document?.path == nil {
            guard let url = pickSaveURL(name) else { return false }
            saveAsURL = url
            result = await store.saveAs(path: url.path)
        } else {
            result = await store.save()
        }
        switch result {
        case .success:
            if let saveAsURL { noteRecent(saveAsURL) }
            return true
        case .failure(let e):
            alerts.showError(String(format: loc("error.saveFailed"), "\(e)"))
            return false
        }
    }

    private static func openPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if let type = UTType(filenameExtension: "openlab") { panel.allowedContentTypes = [type] }
        return panel.runModal() == .OK ? panel.url : nil
    }

    private static func savePanel(_ name: String) -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        if let type = UTType(filenameExtension: "openlab") { panel.allowedContentTypes = [type] }
        return panel.runModal() == .OK ? panel.url : nil
    }
}
