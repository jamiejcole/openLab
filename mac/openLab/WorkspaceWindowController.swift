// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine
import UniformTypeIdentifiers

/// One open workspace's window and its QLab-style layout: masthead, cue-type row, cue list over
/// inspector, optional right sidebar, footer. Built in code; it holds only its own store. Close
/// requests and the GO / Pause All / Panic controls are handed to `AppController` through the
/// closures below: this class never closes itself or talks to the session.
@MainActor
final class WorkspaceWindowController: NSWindowController, NSWindowDelegate {
    let store: WorkspaceStore
    /// Asked on every user close (Cmd-W, close button); the window stays open until the owner closes it.
    var onCloseRequest: (() -> Void)?
    /// These address this window's own store: a click can land on a window that is not key.
    var onGo: (() -> Void)?
    var onPauseAll: (() -> Void)?
    var onResumeAll: (() -> Void)?
    var onPanic: (() -> Void)?
    var onResetAll: (() -> Void)?
    /// A transport command from the cue row menu, for this window.
    var onCommand: ((TransportCommand) -> Void)? {
        didSet { cueList.onCommand = onCommand }
    }
    /// Pause, resume or panic of one cue, from the Active Cues panel.
    var onCueAction: ((TransportAction) -> Void)? {
        didSet {
            sidebar.onCueAction = onCueAction
            inspector.onTransport = onCueAction
        }
    }
    /// Relink missing media was asked for (Warnings row or File menu).
    var onRelink: (() -> Void)? {
        didSet { sidebar.onRelink = onRelink }
    }
    let masthead = MastheadView()
    let cueToolbar = CueToolbarView()
    let findBar = FindBarView()
    /// Find (ED-09): the typed search, what it found and the bar's state (`WorkspaceWindowController+Find`).
    var find = FindState()
    let cueList: CueListViewController
    let editor: CueEditor
    let inspector: InspectorViewController
    let sidebar: SidebarViewController
    private var sub: AnyCancellable?
    private var statusSub: AnyCancellable?
    private var showModeSub: AnyCancellable?
    private var logController: LogWindowController?
    private var closeObserver: NSObjectProtocol?
    var logWindow: LogWindowController {
        if let logController { return logController }
        let made = LogWindowController(store: store)
        logController = made
        return made
    }
    /// The log window if it was ever made (closing a workspace must not make one just to close it).
    var existingLogWindow: LogWindowController? { logController }
    private var preshowController: PreshowWindowController?
    /// The pre-show check panel, made on first use. It closes with this window.
    var preshowWindow: PreshowWindowController {
        if let pc = preshowController { return pc }
        let pc = PreshowWindowController(store: store)
        pc.onInspect = { [weak self] id, tab in self?.inspect(cue: id, tab: tab) }
        preshowController = pc
        return pc
    }
    private var settingsController: WorkspaceSettingsWindowController?
    /// The workspace settings window, made on first use. It closes with this window.
    var settingsWindow: WorkspaceSettingsWindowController {
        if let sc = settingsController { return sc }
        let sc = WorkspaceSettingsWindowController(store: store)
        settingsController = sc
        return sc
    }
    let footer = FooterBar()
    /// The audio file chosen for an Audio cue (nil: cancelled). A seam for tests.
    var pickAudioFile: @MainActor () -> URL? = WorkspaceWindowController.audioPanel
    private let verticalSplit = NSSplitView()
    private let horizontalSplit = NSSplitView()

    init(store: WorkspaceStore) {
        self.store = store
        cueList = CueListViewController(store: store)
        editor = CueEditor(store: store, cueList: cueList)
        cueList.editor = editor
        sidebar = SidebarViewController(store: store)
        inspector = InspectorViewController(store: store)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 800),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = loc("window.main.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.mainWindow)
        window.isReleasedWhenClosed = false  // closing must never destroy the window (M5.17)
        window.minSize = NSSize(width: 800, height: 500)
        super.init(window: window)
        window.delegate = self
        // The log window belongs to this workspace: it goes when the workspace's window does.
        closeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: window, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.logController?.close() }
        }
        cueList.onSelectionChange = { [weak inspector] in inspector?.setSelection($0) }
        editor.isLocked = { [weak self] in self?.isShowMode ?? true }
        editor.commitPending = { [weak self] in await self?.commitPendingEdits() ?? true }
        editor.notify = { [weak self] message in self?.showNotice(message) }
        editor.trackPending = { [weak inspector] command, cue in inspector?.trackPending(command, cue: cue) ?? {} }
        cueList.onMove = { [weak self] ids, parent, index in
            Task { await self?.editor.move(ids, to: parent, at: index) }
        }
        cueList.onEditDuration = { [weak self] id, text in
            Task { await self?.editor.setDuration(of: id, text: text) }
        }
        cueList.onFileDrop = { [weak self] drop, skipped in
            Task {
                switch drop {
                case .replace(let id, let url): await self?.editor.replaceFile(of: id, with: url)
                case .insert(let urls, let parent, let index):
                    await self?.editor.addFiles(urls, parent: parent, index: index, skipped: skipped)
                }
            }
        }
        inspector.onChooseFile = { [weak self] id in
            guard let self, let url = self.pickAudioFile() else { return }
            Task { await self.editor.replaceFile(of: id, with: url) }
        }
        cueToolbar.onAdd = { [weak self] kind in Task { await self?.editor.add(kind) } }
        window.contentView = buildContent()
        window.initialFirstResponder = cueList.outlineView
        window.center()
        // The cue list's own subscription is older, so it has switched lists by the time this runs.
        sub = store.changes.sink { [weak self] _ in
            MainActor.assumeIsolated {
                self?.syncSidebarList()
                self?.syncFooterCount()
                self?.syncMasthead()
            }
        }
        wireFind()
        masthead.onCommitNotes = { [weak self] id, text in Task { await self?.editor.setNotes(of: id, text: text) } }
        sidebar.shownProvider = { [weak self] in self?.cueList.listID }
        sidebar.onSelectList = { [weak self] id in
            self?.cueList.showList(id)
            self?.syncSidebarList()
            self?.syncMasthead()
            self?.refreshFind()
        }
        sidebar.onNotice = { [weak self] in self?.showNotice($0) }
        sidebar.onSelectCue = { [weak self] id in self?.reveal(cue: id) }
        // Both publish before they change: use the delivered values.
        statusSub = store.$status.combineLatest(store.$connectionState).sink { [weak self] status, state in
            MainActor.assumeIsolated { self?.footer.show(status: status, connected: state == .connected) }
        }
        syncSidebarList()
        syncFooterCount()
        syncMasthead()
        // The store's mode moves on the operator's choice and on the engine's word (a reconnect,
        // another client): the window follows either. Published before it changes: use the value.
        showModeSub = store.$showMode.sink { [weak self] on in
            MainActor.assumeIsolated { self?.applyShowMode(on) }
        }
        footer.onModeChange = { [weak self] show in Task { await self?.requestShowMode(show) } }
        footer.onToggle = { [weak self] area, on in self?.setVisible(area, on) }
        footer.onSettings = { [weak self] in self?.showSettings() }
        setVisible(.sidebar, false)
        wire(masthead.goButton, #selector(goClicked))
        wire(sidebar.transportButtons[AX.transportPause], #selector(pauseAllClicked))
        wire(sidebar.transportButtons[AX.transportResume], #selector(resumeAllClicked))
        wire(sidebar.transportButtons[AX.transportPanic], #selector(panicClicked))
        wire(sidebar.transportButtons[AX.transportReset], #selector(resetAllClicked))
    }

    /// Shows `id` in its list and selects it (a Warnings row was chosen). The selection is made
    /// like a restored one, so the playhead does not move: a click in a list of warnings must
    /// never change what GO fires next.
    func reveal(cue id: ModelID) {
        guard let list = store.model?.cueLists.first(where: { Self.contains(id, in: $0.cues) }) else { return }
        if cueList.listID != list.id { cueList.showList(list.id) }
        syncSidebarList()
        cueList.reveal(id)
    }

    private static func contains(_ id: ModelID, in cues: [Cue]) -> Bool {
        cues.contains { $0.id == id || contains(id, in: $0.children) }
    }

    func showLog() {
        logWindow.showWindow(nil)
        Task { await logWindow.refresh() }
    }

    /// Opens the workspace settings (the footer gear, File > Workspace Settings).
    @discardableResult
    func showSettings() -> Task<Void, Never> {
        let sc = settingsWindow
        sc.window?.title = String(format: loc("settings.title.workspace"), workspaceName)
        return sc.present()
    }

    /// The pre-show check asks the engine: without a connection there is nothing to ask (the menu
    /// item is greyed rather than showing an empty, all-clear-looking report).
    var canRunPreshowCheck: Bool { store.connectionState == .connected }

    /// Opens the pre-show check (SM-06) and runs it. The task's value tells tests it answered.
    @discardableResult
    func showPreshowCheck() -> Task<Void, Never> {
        let pc = preshowWindow
        pc.window?.title = String(format: loc("preshow.title.workspace"), workspaceName)
        return Task { await pc.present() }
    }

    /// A pre-show report entry was chosen: reveals the cue (the playhead stays) and, in Edit Mode,
    /// shows the inspector on the tab that holds what to fix, as QLab's "Inspect Cue" does.
    func inspect(cue id: ModelID, tab: String?) {
        reveal(cue: id)
        // Key, not only in front: GO goes to the key window, and with two workspaces the other
        // one may hold the keys while this one's report is used.
        if window?.isMiniaturized == true { window?.deminiaturize(nil) }
        window?.makeKeyAndOrderFront(nil)
        guard !isShowMode else { return }
        setVisible(.inspector, true)
        if let tab, inspector.tabView.indexOfTabViewItem(withIdentifier: tab) != NSNotFound {
            inspector.tabView.selectTabViewItem(withIdentifier: tab)
        }
    }

    /// Show Mode (SM-01) belongs to this window; the store tells the engine (`set_show_mode`, so it
    /// holds off sleep, SM-07) and restores it from the engine's word after a reconnect. It hides
    /// the cue-type row and the whole inspector (their toggles wait, greyed, and the choice comes
    /// back on leaving) and enlarges the masthead; transport, hotkeys, the sidebar and the masthead
    /// toggle keep working, as in QLab.
    private(set) var isShowMode = false

    /// The operator asked for a mode (switch, menu). Entering Show Mode first commits what is typed
    /// in the inspector, like Save and Close; if that is invalid or refused the window stays in
    /// Edit Mode with the field's message showing, so it never locks over unsaved text.
    func requestShowMode(_ on: Bool) async {
        if on, !isShowMode, await !commitPendingEdits() {
            NSSound.beep()
            footer.setShowMode(false)
            return
        }
        setShowMode(on)
    }

    /// Enters or leaves Show Mode now, without committing typed edits (callers do that first).
    func setShowMode(_ on: Bool) {
        guard on != store.showMode else { return }
        store.setShowMode(on)
    }

    private func applyShowMode(_ on: Bool) {
        guard on != isShowMode else { return }
        // Find is off in Show Mode (QLab): the bar gives the row back before the mode hides it.
        if on { closeFind() }
        isShowMode = on
        inspector.isLocked = on
        masthead.isLocked = on
        cueToolbar.isLocked = on
        cueList.isEditLocked = on
        sidebar.relinkLocked = on
        sidebar.listEditLocked = on
        footer.setShowMode(on)
        for area in Self.hiddenInShowMode { show(area, on ? false : wanted[area] ?? true) }
        refreshMasthead()
        refreshTitle()
    }

    /// The areas Show Mode takes away.
    static let hiddenInShowMode: [LayoutArea] = [.toolbar, .inspector]
    /// What the footer toggles last asked for. In Show Mode the shown state differs from it.
    private var wanted: [LayoutArea: Bool] = [:]

    private func refreshMasthead() {
        let size = window?.contentView?.bounds.size ?? NSSize(width: 1200, height: 800)
        masthead.apply(showMode: isShowMode, window: size)
    }

    func windowDidResize(_ notification: Notification) {
        refreshMasthead()
    }

    /// A notice without an interruption: a log record and the footer line.
    func quietNotice(_ message: String, error: Bool) {
        _ = store.receive(.log(error ? .error : .warn, message))
        footer.showNotice(message, error: error)
    }

    /// Why an edit was refused: a sheet on this window (beep when it has none).
    func showNotice(_ message: String) {
        // Show Mode raises nothing modal (NFR-10): the footer and the show log carry it.
        if isShowMode { return quietNotice(message, error: false) }
        guard let window else { return NSSound.beep() }
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: loc("edit.ok"))
        alert.beginSheetModal(for: window)
    }

    /// Asks for the first number and increment, then renumbers the selected cues (ED-08).
    func promptRenumber() {
        guard !isShowMode, let window, !cueList.selectedIDs().isEmpty else { return NSSound.beep() }
        let settings = store.model?.settings
        let start = NSTextField(string: Self.plain(settings?["auto_number_start"]?.doubleValue ?? 1))
        let step = NSTextField(string: Self.plain(settings?["auto_number_increment"]?.doubleValue ?? 1))
        let form = NSGridView(views: [
            [NSTextField(labelWithString: loc("edit.renumber.start")), start],
            [NSTextField(labelWithString: loc("edit.renumber.increment")), step],
        ])
        form.frame = NSRect(x: 0, y: 0, width: 220, height: 56)
        start.setAccessibilityIdentifier("renumber-start")
        step.setAccessibilityIdentifier("renumber-increment")
        let alert = NSAlert()
        alert.messageText = loc("edit.renumber.title")
        alert.accessoryView = form
        alert.addButton(withTitle: loc("edit.renumber.ok"))
        alert.addButton(withTitle: loc("edit.cancel"))
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn, let self else { return }
            guard let a = Double(start.stringValue), let b = Double(step.stringValue) else {
                return self.showNotice(loc("edit.error.renumberValues"))
            }
            Task { await self.editor.renumberSelection(start: a, increment: b) }
        }
    }

    /// One text field in a sheet; `done` gets the text when the first button is chosen.
    private func askText(title: String, label: String, ok: String, field: String, done: @escaping (String) -> Void) {
        guard let window else { return }
        let input = NSTextField(string: field)
        input.placeholderString = label
        input.frame = NSRect(x: 0, y: 0, width: 220, height: 24)
        input.setAccessibilityLabel(label)
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = label
        alert.accessoryView = input
        alert.addButton(withTitle: ok)
        alert.addButton(withTitle: loc("edit.cancel"))
        alert.beginSheetModal(for: window) { response in
            if response == .alertFirstButtonReturn { done(input.stringValue) }
        }
    }

    /// View > Select Cue... / Move Playhead to Cue...: asks for a cue number and then selects that
    /// cue as a click would (the playhead follows), or only moves the playhead.
    func promptCue(movePlayhead: Bool) {
        let key = movePlayhead ? "menu.view.movePlayheadToCue" : "menu.view.selectCue"
        askText(title: loc(key + ".title"), label: loc("cue.prompt.number"), ok: loc(key + ".ok"), field: "") {
            [weak self] text in
            guard let self, let problem = self.applyCueNumber(text, movePlayhead: movePlayhead) else { return }
            self.showNotice(problem)
        }
    }

    /// The dialog's answer for Select Cue / Move Playhead to Cue. Nil: done; else the notice to show.
    func applyCueNumber(_ text: String, movePlayhead: Bool) -> String? {
        guard let id = cueList.cue(numbered: text) else { return String(format: loc("cue.prompt.notFound"), text) }
        if movePlayhead { cueList.setPlayhead(id) } else { cueList.selectAsUser([id]) }
        return nil
    }

    /// Tools > Load to Time...: loads the last selected cue to a typed time, the engine's `load`
    /// command (the Transport menu's Load Selected uses 0).
    func promptLoadToTime() {
        guard !isShowMode, let id = cueList.selectedIDs().last else { return NSSound.beep() }
        askText(
            title: loc("menu.tools.loadToTime.title"), label: loc("cue.prompt.time"),
            ok: loc("menu.tools.loadToTime.ok"), field: "00:00.00"
        ) {
            [weak self] text in
            guard let self, let problem = self.applyLoadTime(text, to: id) else { return }
            self.showNotice(problem)
        }
    }

    /// The dialog's answer for Load to Time. Nil: the load was sent; else the notice to show. The
    /// engine's `load` takes 0 or more seconds, so a negative time is refused like any bad time.
    func applyLoadTime(_ text: String, to id: ModelID) -> String? {
        guard let seconds = CueTime.parse(text) else { return loc("cue.prompt.badTime") }
        loadToTime(id, seconds)
        return nil
    }

    func loadToTime(_ id: ModelID, _ seconds: Double) {
        onCueAction?(.load(cue: id, at: seconds))
    }

    private static func plain(_ v: Double) -> String { v == v.rounded() ? String(Int(v)) : String(v) }

    /// The workspace's name (set by the app as the engine reports it); Show Mode adds a suffix.
    var workspaceName = loc("window.main.title") {
        didSet { refreshTitle() }
    }

    private func refreshTitle() {
        window?.title = isShowMode ? String(format: loc("window.main.title.showMode"), workspaceName) : workspaceName
    }

    /// The footer's cue count follows the model (adds, deletes, a new snapshot).
    /// The shown list's playhead cue, with its notes.
    func syncMasthead() {
        let id = cueList.listID.flatMap { store.playheads[$0] }
        masthead.show(standby: id.flatMap { store.model?.cue($0) })
    }

    func syncFooterCount() {
        let model = store.model
        footer.showCount(cues: model?.cueCount ?? 0, lists: model?.cueLists.count ?? 0)
    }

    private func syncSidebarList() {
        sidebar.syncSelection()
    }

    private func wire(_ button: NSButton?, _ action: Selector) {
        button?.target = self
        button?.action = action
    }

    @objc private func goClicked() { onGo?() }
    @objc private func pauseAllClicked() { onPauseAll?() }
    @objc private func resumeAllClicked() { onResumeAll?() }
    @objc private func panicClicked() { onPanic?() }
    @objc private func resetAllClicked() { onResetAll?() }

    /// Commits what is typed in the inspector. False: it was invalid or refused, so the caller
    /// (Save, Close, Quit) must stop.
    /// Commits what is typed in the inspector and in the cue list's Duration cell and the masthead notes, and waits for the
    /// engine's answers; false when an edit was invalid or refused (Save, Close, Quit stop).
    func commitPendingEdits() async -> Bool {
        var ok = true
        if let edit = cueList.takeDurationEdit() { ok = await editor.setDuration(of: edit.id, text: edit.text) }
        if let notes = masthead.takePendingNotes() {
            ok = await editor.setNotes(of: notes.id, text: notes.text) && ok
        }
        return await inspector.commitPendingEdits() && ok
    }

    /// A report about a workspace that is gone must not float on over the next one.
    func windowWillClose(_ notification: Notification) {
        preshowController?.close()
        settingsController?.close()
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        onCloseRequest?()
        return false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func setVisible(_ area: LayoutArea, _ visible: Bool) {
        if isShowMode, Self.hiddenInShowMode.contains(area) { return }
        wanted[area] = visible
        show(area, visible)
    }

    private func show(_ area: LayoutArea, _ visible: Bool) {
        switch area {
        case .toolbar:
            // While Find holds the row, the toggle only decides what comes back when it closes.
            if find.isOpen { find.toolbarWasHidden = !visible } else { cueToolbar.isHidden = !visible }
        case .masthead: masthead.isHidden = !visible
        case .inspector: inspector.view.isHidden = !visible
        case .sidebar: sidebar.view.isHidden = !visible
        }
        footer.toggles[area]?.state = visible ? .on : .off
        footer.toggles[area]?.isEnabled = !(isShowMode && Self.hiddenInShowMode.contains(area))
    }

    func isVisible(_ area: LayoutArea) -> Bool {
        switch area {
        case .toolbar: return find.isOpen ? !find.toolbarWasHidden : !cueToolbar.isHidden
        case .masthead: return !masthead.isHidden
        case .inspector: return !inspector.view.isHidden
        case .sidebar: return !sidebar.view.isHidden
        }
    }

    private func buildContent() -> NSView {
        verticalSplit.isVertical = false
        verticalSplit.dividerStyle = .thin
        verticalSplit.addArrangedSubview(cueList.view)
        verticalSplit.addArrangedSubview(inspector.view)
        verticalSplit.setHoldingPriority(NSLayoutConstraint.Priority(250), forSubviewAt: 0)
        verticalSplit.setHoldingPriority(NSLayoutConstraint.Priority(251), forSubviewAt: 1)
        let inspectorHeight = inspector.view.heightAnchor.constraint(equalToConstant: 240)
        inspectorHeight.priority = NSLayoutConstraint.Priority(750)
        inspectorHeight.isActive = true

        horizontalSplit.isVertical = true
        horizontalSplit.dividerStyle = .thin
        horizontalSplit.addArrangedSubview(verticalSplit)
        horizontalSplit.addArrangedSubview(sidebar.view)
        horizontalSplit.setHoldingPriority(NSLayoutConstraint.Priority(251), forSubviewAt: 1)
        let sidebarWidth = sidebar.view.widthAnchor.constraint(equalToConstant: 280)
        sidebarWidth.priority = NSLayoutConstraint.Priority(300)
        sidebarWidth.isActive = true

        findBar.isHidden = true
        let stack = NSStackView(views: [masthead, cueToolbar, findBar, horizontalSplit, footer])
        stack.orientation = .vertical
        stack.spacing = 0
        stack.distribution = .fill
        for v in [masthead, cueToolbar, findBar, horizontalSplit, footer] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        // These views have no intrinsic height; without explicit heights the stack stretches them.
        cueToolbar.heightAnchor.constraint(equalToConstant: 36).isActive = true
        findBar.heightAnchor.constraint(equalToConstant: 36).isActive = true
        footer.heightAnchor.constraint(equalToConstant: 36).isActive = true
        for v in [masthead, cueToolbar, findBar, footer] {
            v.setContentHuggingPriority(.required, for: .vertical)
            v.setContentCompressionResistancePriority(.required, for: .vertical)
        }
        horizontalSplit.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        return stack
    }
}

extension WorkspaceWindowController {
    /// Only the types a drop accepts (`CueEditor.audioExtensions`).
    static func audioPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = loc("inspector.target.choosePrompt")
        panel.allowedContentTypes = CueEditor.audioExtensions.sorted().compactMap { UTType(filenameExtension: $0) }
        return panel.runModal() == .OK ? panel.url : nil
    }
}
