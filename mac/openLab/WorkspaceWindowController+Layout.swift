// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Remembering this window's layout on this machine (UI-02, `WindowLayout`). Restoring only sets
/// the frame, the splits, the shown areas and the sidebar tab: it never touches the first
/// responder, the selection or the key state.
extension WorkspaceWindowController {
    /// Applies what is saved for `path` and starts saving changes under it. Saved data that is
    /// damaged or does not fit the screens is corrected or skipped field by field. Called before
    /// the window is shown, so it never moves a window the operator can already see.
    func restoreLayout(from store: WindowLayoutStore, path: String) {
        guard let window else { return }
        isRestoringLayout = true
        defer { isRestoringLayout = false }
        layoutPersistence = (store, path)
        let layout = store.layout(forPath: path)
        if let saved = layout.frame,
            let fitted = WindowLayout.fit(saved, screens: NSScreen.screens.map(\.visibleFrame), minSize: window.minSize)
        {
            window.setFrame(fitted, display: false)
        }
        window.contentView?.layoutSubtreeIfNeeded()
        pendingInspectorHeight = layout.inspectorHeight
        pendingSidebarWidth = layout.sidebarWidth
        for area in LayoutArea.allCases {
            if let on = layout.shown[area] { setVisible(area, on) }
        }
        // An area shown by default has no entry to show it again: its split is applied now.
        for area in [LayoutArea.inspector, .sidebar] where isVisible(area) { applyPendingSplit(area) }
        if let tab = layout.sidebarTab { sidebar.showTab(tab) }
        startSavingLayout()
    }

    /// The window's file became known (or changed: Save As, the first Save of a new workspace) while
    /// the window is already up: what is on screen now is the layout, kept under the file's path.
    /// Nothing is applied, so the window never moves.
    func bindLayout(to store: WindowLayoutStore, path: String) {
        if let current = layoutPersistence, store.key(for: current.path) == store.key(for: path) { return }
        layoutPersistence = (store, path)
        startSavingLayout()
        saveLayout()
    }

    private func startSavingLayout() {
        guard layoutObservers.isEmpty else { return }
        sidebar.onTabChange = { [weak self] in self?.saveLayout() }
        let center = NotificationCenter.default
        layoutObservers = [verticalSplit, horizontalSplit].map { split in
            center.addObserver(forName: NSSplitView.didResizeSubviewsNotification, object: split, queue: .main) {
                [weak self] _ in
                MainActor.assumeIsolated { self?.splitMoved() }
            }
        }
    }

    /// A divider moved: what the split is now is what it keeps (the soft size constraints would pull it
    /// back at the next layout pass). Except when the cue list is down to its minimum and the area came
    /// out smaller than it was asked to be: a window too small for both squeezed it, and the size the
    /// user dragged stays.
    private func splitMoved() {
        guard !isRestoringLayout else { return }
        let list = cueList.view.frame
        if isVisible(.inspector), !isShowMode, let kept = inspectorHeightConstraint?.constant {
            let height = inspector.view.frame.height
            if height > 0, !(height < kept && list.height <= Self.minCueListHeight + 0.5) {
                inspectorHeightConstraint?.constant = height
            }
        }
        if isVisible(.sidebar), let kept = sidebarWidthConstraint?.constant {
            let width = sidebar.view.frame.width
            if width > 0, !(width < kept && list.width <= Self.minCueListWidth + 0.5) {
                sidebarWidthConstraint?.constant = width
            }
        }
        saveLayout()
    }

    /// Writes the current layout. Show Mode's hidden inspector and toolbar are not layout: the
    /// shown areas are what the user asked for (`wanted`), and the split sizes are the preferred ones
    /// (what was dragged), so a window too small to show them does not overwrite them.
    func saveLayout() {
        guard !isRestoringLayout, let (store, path) = layoutPersistence, let window else { return }
        var layout = store.layout(forPath: path)
        if !window.isMiniaturized, !window.styleMask.contains(.fullScreen) { layout.frame = window.frame }
        for area in LayoutArea.allCases { layout.shown[area] = wanted[area] ?? true }
        if isVisible(.inspector), !isShowMode, let h = inspectorHeightConstraint?.constant, h > 0 {
            layout.inspectorHeight = h
        }
        if isVisible(.sidebar), let w = sidebarWidthConstraint?.constant, w > 0 { layout.sidebarWidth = w }
        layout.sidebarTab = sidebar.tabs.selectedSegment
        store.save(layout, forPath: path)
    }

    /// Gives a just-shown area the split size that was saved for it.
    func applyPendingSplit(_ area: LayoutArea) {
        switch area {
        case .inspector:
            guard let target = pendingInspectorHeight else { return }
            pendingInspectorHeight = nil
            window?.contentView?.layoutSubtreeIfNeeded()
            let room = verticalSplit.bounds.height - verticalSplit.dividerThickness
            let most = room - Self.minCueListHeight
            guard most >= 120 else { return }  // no room for a usable inspector
            // The preferred size stays what was saved even where this window cannot show it yet.
            inspectorHeightConstraint?.constant = max(target, 120)
            verticalSplit.setPosition(room - min(max(target, 120), most), ofDividerAt: 0)
        case .sidebar:
            guard let target = pendingSidebarWidth else { return }
            pendingSidebarWidth = nil
            window?.contentView?.layoutSubtreeIfNeeded()
            let room = horizontalSplit.bounds.width - horizontalSplit.dividerThickness
            let most = room - Self.minCueListWidth
            guard most >= 200 else { return }
            sidebarWidthConstraint?.constant = max(target, 200)
            horizontalSplit.setPosition(room - min(max(target, 200), most), ofDividerAt: 0)
        case .toolbar, .masthead:
            break
        }
    }
}
