// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

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
    let cueList: CueListViewController
    let editor: CueEditor
    let inspector: InspectorViewController
    let sidebar: SidebarViewController
    private var sub: AnyCancellable?
    private var statusSub: AnyCancellable?
    private var showModeSub: AnyCancellable?
    private(set) lazy var logWindow = LogWindowController(store: store)
    private(set) lazy var preshowWindow: PreshowWindowController = {
        let pc = PreshowWindowController(store: store)
        pc.onInspect = { [weak self] id, tab in self?.inspect(cue: id, tab: tab) }
        return pc
    }()
    let footer = FooterBar()
    private let verticalSplit = NSSplitView()
    private let horizontalSplit = NSSplitView()

    init(store: WorkspaceStore) {
        self.store = store
        cueList = CueListViewController(store: store)
        editor = CueEditor(store: store, cueList: cueList)
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
        cueList.onSelectionChange = { [weak inspector] in inspector?.setSelection($0) }
        editor.isLocked = { [weak self] in self?.isShowMode ?? true }
        editor.commitPending = { [weak self] in await self?.commitPendingEdits() ?? true }
        editor.notify = { [weak self] message in self?.showNotice(message) }
        cueList.onMove = { [weak self] ids, parent, index in
            Task { await self?.editor.move(ids, to: parent, at: index) }
        }
        cueList.onFileDrop = { [weak self] drop in
            Task {
                switch drop {
                case .replace(let id, let url): await self?.editor.replaceFile(of: id, with: url)
                case .insert(let urls, let parent, let index):
                    await self?.editor.addFiles(urls, parent: parent, index: index)
                }
            }
        }
        cueToolbar.onAdd = { [weak self] kind in Task { await self?.editor.add(kind) } }
        window.contentView = buildContent()
        window.initialFirstResponder = cueList.outlineView
        window.center()
        // The cue list's own subscription is older, so it has switched lists by the time this runs.
        sub = store.changes.sink { [weak self] _ in
            MainActor.assumeIsolated { self?.syncSidebarList() }
        }
        sidebar.shownProvider = { [weak self] in self?.cueList.listID }
        sidebar.onSelectList = { [weak self] id in
            self?.cueList.showList(id)
            self?.syncSidebarList()
        }
        sidebar.onSelectCue = { [weak self] id in self?.reveal(cue: id) }
        // Both publish before they change: use the delivered values.
        statusSub = store.$status.combineLatest(store.$connectionState).sink { [weak self] status, state in
            MainActor.assumeIsolated { self?.footer.show(status: status, connected: state == .connected) }
        }
        syncSidebarList()
        // The store's mode moves on the operator's choice and on the engine's word (a reconnect,
        // another client): the window follows either. Published before it changes: use the value.
        showModeSub = store.$showMode.sink { [weak self] on in
            MainActor.assumeIsolated { self?.applyShowMode(on) }
        }
        footer.onModeChange = { [weak self] show in Task { await self?.requestShowMode(show) } }
        footer.onToggle = { [weak self] area, on in self?.setVisible(area, on) }
        setVisible(.sidebar, false)
        wire(masthead.goButton, #selector(goClicked))
        wire(sidebar.transportButtons[AX.transportPause], #selector(pauseAllClicked))
        wire(sidebar.transportButtons[AX.transportResume], #selector(resumeAllClicked))
        wire(sidebar.transportButtons[AX.transportPanic], #selector(panicClicked))
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

    /// The pre-show check asks the engine: without a connection there is nothing to ask (the menu
    /// item is greyed rather than showing an empty, all-clear-looking report).
    var canRunPreshowCheck: Bool { store.connectionState == .connected }

    /// Opens the pre-show check (SM-06) and runs it. The task's value tells tests it answered.
    @discardableResult
    func showPreshowCheck() -> Task<Void, Never> {
        Task { await preshowWindow.present() }
    }

    /// A pre-show report entry was chosen: reveals the cue (the playhead stays) and, in Edit Mode,
    /// shows the inspector on the tab that holds what to fix, as QLab's "Inspect Cue" does.
    func inspect(cue id: ModelID, tab: String?) {
        reveal(cue: id)
        window?.orderFront(nil)
        guard !isShowMode else { return }
        setVisible(.inspector, true)
        if let tab, inspector.tabView.indexOfTabViewItem(withIdentifier: tab) != NSNotFound {
            inspector.tabView.selectTabViewItem(withIdentifier: tab)
        }
    }

    /// Show Mode (SM-01) belongs to this window only and is not saved; the store tells the engine
    /// (`set_show_mode`, so it holds off sleep, SM-07) and restores it after a reconnect. Locks the
    /// inspector and cue-type row; transport, hotkeys, the sidebar and the layout toggles keep
    /// working, as in QLab.
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
        isShowMode = on
        inspector.isLocked = on
        cueToolbar.isLocked = on
        cueList.isEditLocked = on
        sidebar.relinkLocked = on
        footer.setShowMode(on)
        refreshTitle()
    }

    /// Why an edit was refused: a sheet on this window (beep when it has none).
    func showNotice(_ message: String) {
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

    private static func plain(_ v: Double) -> String { v == v.rounded() ? String(Int(v)) : String(v) }

    /// The workspace's name (set by the app as the engine reports it); Show Mode adds a suffix.
    var workspaceName = loc("window.main.title") {
        didSet { refreshTitle() }
    }

    private func refreshTitle() {
        window?.title = isShowMode ? String(format: loc("window.main.title.showMode"), workspaceName) : workspaceName
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

    /// Commits what is typed in the inspector. False: it was invalid or refused, so the caller
    /// (Save, Close, Quit) must stop.
    func commitPendingEdits() async -> Bool { await inspector.commitPendingEdits() }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        onCloseRequest?()
        return false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func setVisible(_ area: LayoutArea, _ visible: Bool) {
        switch area {
        case .toolbar: cueToolbar.isHidden = !visible
        case .masthead: masthead.isHidden = !visible
        case .inspector: inspector.view.isHidden = !visible
        case .sidebar: sidebar.view.isHidden = !visible
        }
        footer.toggles[area]?.state = visible ? .on : .off
    }

    func isVisible(_ area: LayoutArea) -> Bool {
        switch area {
        case .toolbar: return !cueToolbar.isHidden
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

        let stack = NSStackView(views: [masthead, cueToolbar, horizontalSplit, footer])
        stack.orientation = .vertical
        stack.spacing = 0
        stack.distribution = .fill
        for v in [masthead, cueToolbar, horizontalSplit, footer] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        // These views have no intrinsic height; without explicit heights the stack stretches them.
        masthead.heightAnchor.constraint(equalToConstant: 100).isActive = true
        cueToolbar.heightAnchor.constraint(equalToConstant: 36).isActive = true
        footer.heightAnchor.constraint(equalToConstant: 36).isActive = true
        for v in [masthead, cueToolbar, footer] {
            v.setContentHuggingPriority(.required, for: .vertical)
            v.setContentCompressionResistancePriority(.required, for: .vertical)
        }
        horizontalSplit.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        return stack
    }
}
