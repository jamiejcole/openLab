// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Remembering this window's layout on this machine (UI-02, `WindowLayout`). Restoring only sets
/// the frame, the splits, the shown areas and the sidebar tab: it never touches the first
/// responder, the selection or the key state.
extension WorkspaceWindowController {
    /// Applies what is saved for `path` and starts saving changes under it. Saved data that is
    /// damaged or does not fit the screens is corrected or skipped field by field.
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
        sidebar.onTabChange = { [weak self] in self?.saveLayout() }
        let center = NotificationCenter.default
        layoutObservers = [verticalSplit, horizontalSplit].map { split in
            center.addObserver(forName: NSSplitView.didResizeSubviewsNotification, object: split, queue: .main) {
                [weak self] _ in
                MainActor.assumeIsolated { self?.splitMoved() }
            }
        }
    }

    /// A divider moved (a drag, or the window's size): what the split is now is what it keeps, or the
    /// soft size constraints pull it back at the next layout pass.
    private func splitMoved() {
        guard !isRestoringLayout else { return }
        if isVisible(.inspector), !isShowMode, inspector.view.frame.height > 0 {
            inspectorHeightConstraint?.constant = inspector.view.frame.height
        }
        if isVisible(.sidebar), sidebar.view.frame.width > 0 {
            sidebarWidthConstraint?.constant = sidebar.view.frame.width
        }
        saveLayout()
    }

    /// Writes the current layout. Show Mode's hidden inspector and toolbar are not layout: the
    /// shown areas are what the user asked for (`wanted`), and the inspector's height is only read
    /// while it is shown.
    func saveLayout() {
        guard !isRestoringLayout, let (store, path) = layoutPersistence, let window else { return }
        var layout = store.layout(forPath: path)
        if !window.isMiniaturized, !window.styleMask.contains(.fullScreen) { layout.frame = window.frame }
        for area in LayoutArea.allCases { layout.shown[area] = wanted[area] ?? true }
        if isVisible(.inspector), !isShowMode, inspector.view.frame.height > 0 {
            layout.inspectorHeight = inspector.view.frame.height
        }
        if isVisible(.sidebar), sidebar.view.frame.width > 0 { layout.sidebarWidth = sidebar.view.frame.width }
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
            guard room >= 270 else { return }  // not enough room to keep 150 for the cue list and 120 here
            let height = min(max(target, 120), room - 150)
            inspectorHeightConstraint?.constant = height
            verticalSplit.setPosition(room - height, ofDividerAt: 0)
        case .sidebar:
            guard let target = pendingSidebarWidth else { return }
            pendingSidebarWidth = nil
            window?.contentView?.layoutSubtreeIfNeeded()
            let room = horizontalSplit.bounds.width - horizontalSplit.dividerThickness
            guard room >= 600 else { return }  // 400 for the cue list, 200 here
            let width = min(max(target, 200), room - 400)
            sidebarWidthConstraint?.constant = width
            horizontalSplit.setPosition(room - width, ofDividerAt: 0)
        case .toolbar, .masthead:
            break
        }
    }
}
