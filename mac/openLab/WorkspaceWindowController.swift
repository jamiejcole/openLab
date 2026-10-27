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
        didSet { sidebar.onCueAction = onCueAction }
    }
    /// Relink missing media was asked for (Warnings row or File menu).
    var onRelink: (() -> Void)? {
        didSet { sidebar.onRelink = onRelink }
    }
    let masthead = MastheadView()
    let cueToolbar = CueToolbarView()
    let cueList: CueListViewController
    let inspector: InspectorViewController
    let sidebar: SidebarViewController
    private var sub: AnyCancellable?
    private var statusSub: AnyCancellable?
    private(set) lazy var logWindow = LogWindowController(store: store)
    let footer = FooterBar()
    private let verticalSplit = NSSplitView()
    private let horizontalSplit = NSSplitView()

    init(store: WorkspaceStore) {
        self.store = store
        cueList = CueListViewController(store: store)
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
        footer.onModeChange = { [weak self] show in self?.setShowMode(show) }
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

    /// Show Mode (SM-01) belongs to this window only and is not saved: the protocol has no field for
    /// it (docs/handoff/m5-protocol-requests.md). Locks the inspector and cue-type row; transport,
    /// hotkeys, the sidebar and the layout toggles keep working, as in QLab.
    private(set) var isShowMode = false

    func setShowMode(_ on: Bool) {
        guard on != isShowMode else { return }
        isShowMode = on
        inspector.isLocked = on
        cueToolbar.isLocked = on
        footer.setShowMode(on)
        let title = loc("window.main.title")
        window?.title = on ? String(format: loc("window.main.title.showMode"), title) : title
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
