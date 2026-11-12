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
        let entry = store.key(for: path)
        layoutPersistence = (store, path, entry)
        let layout = store.layout(forEntry: entry)
        if let saved = layout.frame,
            let fitted = WindowLayout.fit(saved, screens: NSScreen.screens.map(\.visibleFrame), minSize: window.minSize)
        {
            window.setFrame(fitted, display: false)
        }
        window.contentView?.layoutSubtreeIfNeeded()
        pendingInspectorHeight = layout.inspectorHeight
        pendingSidebarWidth = layout.sidebarWidth
        // The preferred sizes are part of the layout whether or not their area is shown now.
        if let h = layout.inspectorHeight { inspectorHeightConstraint?.constant = max(h, Self.minInspectorHeight) }
        if let w = layout.sidebarWidth { sidebarWidthConstraint?.constant = max(w, 200) }
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
        // Raw strings first, and the resolved name is kept: resolving can touch a slow volume.
        if let current = layoutPersistence, current.path == path { return }
        let entry = store.key(for: path)
        if let current = layoutPersistence, current.key == entry { return }
        layoutPersistence = (store, path, entry)
        startSavingLayout()
        saveLayout()
    }

    /// Follows the dividers from the window's first moment: a drag sets the area's preferred size in
    /// a workspace with no file yet too (nothing is saved for it; without this the divider sprang back).
    func startSavingLayout() {
        guard layoutObservers.isEmpty else { return }
        sidebar.onTabChange = { [weak self] in self?.saveLayout() }
        let center = NotificationCenter.default
        // The centre keeps each block until it is removed (`stopSavingLayout`, when the window closes), and
        // a block that held its split view would keep the window's whole view tree alive after that.
        layoutObservers = [verticalSplit, horizontalSplit].map { split in
            center.addObserver(forName: NSSplitView.didResizeSubviewsNotification, object: split, queue: .main) {
                [weak self, weak split] note in
                // A divider moved by the user carries NSSplitViewUserResizeKey; a layout pass after a
                // window resize carries at most the divider's index.
                let keyed = note.userInfo?["NSSplitViewUserResizeKey"] != nil
                MainActor.assumeIsolated {
                    // The key is not documented. Without it a move still counts as a drag while the
                    // left button is down outside a live window resize (the window's own edge drag).
                    let mouse = NSEvent.pressedMouseButtons & 1 != 0 && self?.window?.inLiveResize != true
                    self?.splitMoved(byDrag: keyed || mouse, vertical: split === self?.verticalSplit)
                }
            }
        }
    }

    /// The window is closing: its observers leave the notification centre, which would otherwise hold
    /// them, and through them the window's views, for the life of the app.
    func stopSavingLayout() {
        layoutObservers.forEach(NotificationCenter.default.removeObserver)
        layoutObservers = []
    }

    /// A divider moved. A drag sets the preferred size of the area beside it (the soft size
    /// constraints would pull it back at the next layout pass); a window resize, which squeezes an
    /// area without the user choosing that, leaves what the user dragged.
    private func splitMoved(byDrag: Bool, vertical: Bool) {
        guard !isRestoringLayout else { return }
        if byDrag {
            if vertical, isVisible(.inspector), !isShowMode, inspector.view.frame.height > 0 {
                inspectorHeightConstraint?.constant = inspector.view.frame.height
            }
            if !vertical, isVisible(.sidebar), sidebar.view.frame.width > 0 {
                sidebarWidthConstraint?.constant = sidebar.view.frame.width
            }
        }
        saveLayout()
    }

    /// Writes the current layout. Show Mode's hidden inspector and toolbar are not layout: the
    /// shown areas are what the user asked for (`wanted`), and the split sizes are the preferred ones
    /// (what was dragged), so a window too small to show them does not overwrite them.
    func saveLayout() {
        guard !isRestoringLayout, let (store, _, entry) = layoutPersistence, let window else { return }
        var layout = store.layout(forEntry: entry)
        if !window.isMiniaturized, !window.styleMask.contains(.fullScreen) { layout.frame = window.frame }
        for area in LayoutArea.allCases { layout.shown[area] = wanted[area] ?? true }
        if let h = inspectorHeightConstraint?.constant, h > 0 { layout.inspectorHeight = h }
        if let w = sidebarWidthConstraint?.constant, w > 0 { layout.sidebarWidth = w }
        layout.sidebarTab = sidebar.tabs.selectedSegment
        store.save(layout, forEntry: entry)
    }

    /// Moves a divider to a saved place. That is the layout being restored, not the user dragging: the
    /// split's notification carries the user-resize key, which must not change the preferred sizes.
    private func placeDivider(_ split: NSSplitView, at position: CGFloat) {
        let was = isRestoringLayout
        isRestoringLayout = true
        defer { isRestoringLayout = was }
        split.setPosition(position, ofDividerAt: 0)
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
            guard most >= Self.minInspectorHeight else { return }  // no room for a usable inspector
            // The preferred size stays what was saved even where this window cannot show it yet.
            inspectorHeightConstraint?.constant = max(target, Self.minInspectorHeight)
            placeDivider(verticalSplit, at: room - min(max(target, Self.minInspectorHeight), most))
        case .sidebar:
            guard let target = pendingSidebarWidth else { return }
            pendingSidebarWidth = nil
            window?.contentView?.layoutSubtreeIfNeeded()
            let room = horizontalSplit.bounds.width - horizontalSplit.dividerThickness
            let most = room - Self.minCueListWidth
            guard most >= 200 else { return }
            sidebarWidthConstraint?.constant = max(target, 200)
            placeDivider(horizontalSplit, at: room - min(max(target, 200), most))
        case .toolbar, .masthead:
            break
        }
    }
}
