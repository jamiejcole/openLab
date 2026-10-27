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
    /// The folder to search for missing media (nil: cancelled).
    var pickFolderURL: @MainActor () -> URL? = AppController.folderPanel
    var pickSaveURL: @MainActor (String) -> URL? = AppController.savePanel
    var noteRecent: @MainActor (URL) -> Void = { NSDocumentController.shared.noteNewRecentDocumentURL($0) }
    var recentURLs: @MainActor () -> [URL] = { NSDocumentController.shared.recentDocumentURLs }
    var keyWindow: @MainActor () -> NSWindow? = { NSApp.keyWindow }
    var beep: @MainActor () -> Void = { NSSound.beep() }
    /// A text field or text view is being edited: keys that type must stay with it.
    var isTextEditing: @MainActor () -> Bool = { (NSApp.keyWindow?.firstResponder as? NSText)?.isEditable == true }
    /// Per workspace: its `settings.hotkeys` as last read (a change re-reads and warns once).
    /// Entries in the per-workspace key cache (tests: evicted when a workspace closes).
    var keyCacheCount: Int { keyCache.count }
    private var keySubs: [ModelID: AnyCancellable] = [:]
    private var keyCache: [ObjectIdentifier: (settings: JSONValue?, keys: TransportKeys)] = [:]

    private var subs: [AnyCancellable] = []
    private var keyMonitor: Any?
    /// How many monitors `installKeyMonitor` has registered (tests: exactly one).
    private(set) var keyMonitorInstalls = 0
    /// Bounds the waits on the quit and save paths (injected so tests never wait in real time).
    private let scheduler: Scheduler
    private let timeout: Double
    private var closing: Set<ModelID> = []
    private(set) var quitting = false

    init(session: EngineSession, scheduler: Scheduler = SystemScheduler(), timeout: Double = 5) {
        self.session = session
        self.scheduler = scheduler
        self.timeout = timeout
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
            wc.onPauseAll = { [weak self] in Task { await self?.run(.pauseAll, on: store) } }
            wc.onResumeAll = { [weak self] in Task { await self?.run(.resumeAll, on: store) } }
            wc.onPanic = { [weak self] in Task { _ = await self?.panicAll() } }
            wc.onCueAction = { [weak self] action in Task { await self?.run(action, on: store) } }
            wc.onRelink = { [weak self] in Task { await self?.relinkMedia(on: store) } }
            windows[doc] = wc
            // Hotkey warnings are logged when the settings arrive, not at the first key press.
            _ = keys(for: store)
            keySubs[doc] = store.changes.sink { [weak self] _ in
                MainActor.assumeIsolated { _ = self?.keys(for: store) }
            }
            if let n = session.workspaces.first(where: { $0.doc == doc }) { wc.window?.title = n.name }
            show(doc)
        }
        for doc in windows.keys where stores[doc] == nil { dropWindow(doc) }
        showLauncherIfNoWindows()
    }

    private func dropWindow(_ doc: ModelID) {
        guard let wc = windows.removeValue(forKey: doc) else { return }
        keyCache[ObjectIdentifier(wc.store)] = nil
        keySubs[doc] = nil
        wc.window?.close()
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

    // MARK: Esc

    /// Esc panics every workspace whichever window is key (launcher, alert, a field editor
    /// mid-edit), then the event carries on so the editor or alert still handles it.
    func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitorInstalls += 1
        // Deliberately also fires while an alert or panel is up: panic must never be blocked by UI.
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            let consumed = MainActor.assumeIsolated { self?.handleKeyDown(event) == nil }
            return consumed ? nil : event
        }
    }

    /// The monitor's body, callable from tests. Esc always goes on to the responder. A key-map
    /// chord is taken here (nil: consumed) because a focused table or outline view would otherwise
    /// swallow Space before the menu's key equivalent sees it; a text field being edited keeps its
    /// typing keys (`canPerform`).
    func handleKeyDown(_ event: NSEvent) -> NSEvent? {
        if Self.isPanicKey(event) {
            firePanic()
            return event
        }
        guard let chord = KeyChord(event: event), let command = frontKeys.command(for: chord), canPerform(command)
        else { return event }
        let isRepeat = event.isARepeat
        Task { await perform(command, isRepeat: isRepeat) }
        return nil
    }

    private func firePanic() { Task { _ = await panicAll() } }

    private nonisolated static func isPanicKey(_ event: NSEvent) -> Bool {
        let mods = event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .function])
        return event.keyCode == 53 && mods.isEmpty && !event.isARepeat
    }

    // MARK: Front-window routing

    var frontController: WorkspaceWindowController? {
        guard let key = keyWindow() else { return nil }
        return windows.values.first { $0.window === key }
    }

    /// GO on the list shown in the key window (the sidebar's selection).
    @discardableResult
    func goFront() async -> Result<OkBody, EngineError>? {
        guard let store = frontController?.store else { return nil }
        return await go(store)
    }

    @discardableResult
    func pauseAllFront() async -> Result<OkBody, EngineError>? {
        guard let store = frontController?.store else { return nil }
        return await run(.pauseAll, on: store)
    }

    // MARK: Transport commands (menu items and keys)

    /// The transport keys of `store`'s workspace. A show file whose hotkeys conflict or are
    /// unusable is logged to that store once per change (never a crash).
    func keys(for store: WorkspaceStore) -> TransportKeys {
        let settings = store.model?.settings
        let id = ObjectIdentifier(store)
        if let cached = keyCache[id], cached.settings == settings { return cached.keys }
        let keys = TransportKeys(settings: settings)
        keyCache[id] = (settings, keys)
        for warning in keys.warnings { _ = store.receive(.log(.warn, warning)) }
        return keys
    }

    /// The key window's keys (the defaults when no workspace window is key).
    var frontKeys: TransportKeys {
        frontController.map { keys(for: $0.store) } ?? TransportKeys(settings: nil)
    }

    /// Whether the menu item for `command` may fire now. Disabled items also let their key through
    /// to the text field being edited (see `KeyMap` for the precedence rule).
    func canPerform(_ command: TransportCommand) -> Bool {
        if command == .panic { return true }
        guard frontController != nil else { return false }
        if let chord = frontKeys.chord(for: command), chord.typesCharacters, isTextEditing() { return false }
        return true
    }

    /// Runs `command` on the key window's workspace (Panic: every workspace). A held key is ignored
    /// after its first press: a repeating GO would fire cue after cue.
    func perform(_ command: TransportCommand, isRepeat: Bool = false) async {
        guard !isRepeat else { return }
        if command == .panic {
            _ = await panicAll()
            return
        }
        guard let wc = frontController else { return }
        let store = wc.store
        let selected = wc.cueList.selectedIDs()
        let list = wc.cueList.listID  // the list picked in the sidebar
        switch command {
        case .go: _ = await go(store)
        case .pauseAll: await run(.pauseAll, on: store)
        case .resumeAll: await run(.resumeAll, on: store)
        case .stopAll: await run(.stopAll, on: store)  // atomic in the engine: every cue and preview
        case .panic: break
        case .playheadNext: if let list { await run(.playheadNext(list: list), on: store) }
        case .playheadPrevious: if let list { await run(.playheadPrevious(list: list), on: store) }
        case .loadSelected: if let id = selected.last { await run(.load(cue: id, at: 0), on: store) }
        case .startSelected: for id in selected { await run(.start(cue: id), on: store) }
        case .stopSelected: for id in selected { await run(.hardStop(cue: id), on: store) }
        case .resetSelected: for id in selected { await run(.reset(cue: id), on: store) }
        case .pauseResumeSelected:
            for id in selected {
                switch store.cueStates[id]?.state {
                case .paused?: await run(.resume(cue: id), on: store)
                case .running?, .fading?, .preWaiting?, .postWaiting?: await run(.pause(cue: id), on: store)
                default: break
                }
            }
        }
    }

    /// Esc: every workspace, by the engine's app-scope panic.
    func panicAll() async -> Result<OkBody, EngineError> {
        let result = await session.panicAll()
        if case .failure(let e) = result { for store in session.stores.values { report(e, on: store) } }
        return result
    }

    /// Sends a transport action. A refused or undeliverable one (not connected, timeout, refusal)
    /// beeps and goes to the show log: an operator pressing GO must never be left guessing.
    @discardableResult
    private func run(_ action: TransportAction, on store: WorkspaceStore) async -> Result<OkBody, EngineError> {
        let result = await store.transport(action)
        if case .failure(let e) = result { report(e, on: store) }
        return result
    }

    private func report(_ error: EngineError, on store: WorkspaceStore) {
        beep()
        _ = store.receive(.log(.error, String(format: loc("error.transportFailed"), describe(error))))
    }

    /// Asks for a folder and has the engine relink missing media from it (WS-03). The outcome is
    /// told to the operator and written to the show log; the cues that were fixed leave the
    /// Warnings tab by themselves when the engine reports them playable.
    func relinkMedia(on store: WorkspaceStore) async {
        guard let folder = pickFolderURL() else { return }
        switch await store.relink(folder: folder.path) {
        case .success(let ok):
            guard case .relink(let relinked, let ambiguous, let unmatched)? = ok.data else {
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
            windows.values.first { $0.store === store }?.footer.showNotice(message)
        case .failure(let e):
            let message = String(format: loc("relink.failed"), describe(e))
            _ = store.receive(.log(.error, message))
            alerts.showError(message)
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
        (frontController ?? windows.values.first)?.showLog()
    }

    @discardableResult
    private func go(_ store: WorkspaceStore) async -> Result<OkBody, EngineError>? {
        guard let list = windows.values.first(where: { $0.store === store })?.cueList.listID else { return nil }
        return await run(.go(list: list), on: store)
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
        case .failure(let e): alerts.showError(String(format: loc("error.openFailed"), describe(e)))
        }
        return result
    }

    // MARK: Close

    /// The user asked to close a workspace window. False: it stays open.
    @discardableResult
    func closeWorkspace(_ doc: ModelID) async -> Bool {
        guard let store = session.stores[doc], !quitting, closing.insert(doc).inserted else { return false }
        defer { closing.remove(doc) }
        // A one-workspace engine cannot close a doc (until M4.15f): closing would only hide a show
        // that keeps playing, so Close Window does nothing there.
        guard session.mode != .single else {
            NSSound.beep()
            return false
        }
        if isPlaying(store), !alerts.confirmCloseWhilePlaying(workspace: name(of: doc)) { return false }
        guard let discard = await resolveUnsaved(doc) else { return false }
        switch await bounded({ await self.session.close(doc: doc, discardChanges: discard) }) {
        case .success?:
            dropWindow(doc)
        case .failure(let e)?:
            alerts.showError(String(format: loc("error.closeFailed"), describe(e)))
            return false
        case nil:
            alerts.showError(String(format: loc("error.closeFailed"), loc("error.detail.timeout")))
            return false
        }
        showLauncherIfNoWindows()
        return true
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
        if engineUnreachable { return alerts.confirmQuitDisconnected() }
        for doc in Array(windows.keys) {
            if await !commitEdits(doc) { return false }
        }
        if session.stores.values.contains(where: isPlaying), !alerts.confirmQuitWhilePlaying() { return false }
        for info in session.workspaces where session.stores[info.doc]?.document?.dirty == true {
            show(info.doc)
            if await resolveUnsaved(info.doc) == nil { return false }
        }
        // A stalled flush must not trap the app; the engine still sees the socket drop.
        _ = await bounded { await self.session.shutdown() }
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

    private func describe(_ e: EngineError) -> String {
        switch e {
        case .notConnected, .disconnected: return loc("error.detail.notConnected")
        case .engine(_, let message) where !message.isEmpty: return message
        default:
            engineLog.error("engine call failed: \(String(describing: e), privacy: .public)")
            return loc("error.detail.generic")
        }
    }

    private func isPlaying(_ store: WorkspaceStore) -> Bool {
        store.cueStates.values.contains { $0.state == .running || $0.state == .fading }
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

    private func save(_ store: WorkspaceStore, name: String) async -> Bool {
        var saveAsURL: URL?
        let reply: Result<OkBody, EngineError>?
        if store.document?.path == nil {
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
            alerts.showError(String(format: loc("error.saveFailed"), describe(e)))
        case nil:
            alerts.showError(String(format: loc("error.saveFailed"), loc("error.detail.timeout")))
        }
        return false
    }

    private static func openPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if let type = UTType(filenameExtension: "openlab") { panel.allowedContentTypes = [type] }
        return panel.runModal() == .OK ? panel.url : nil
    }

    private static func folderPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = loc("relink.panel.message")
        panel.prompt = loc("relink.panel.prompt")
        return panel.runModal() == .OK ? panel.url : nil
    }

    private static func savePanel(_ name: String) -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        if let type = UTType(filenameExtension: "openlab") { panel.allowedContentTypes = [type] }
        return panel.runModal() == .OK ? panel.url : nil
    }
}
