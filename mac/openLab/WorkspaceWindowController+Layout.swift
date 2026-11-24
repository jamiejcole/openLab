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
        // The GO-area toggle also sets the cue toolbar, so it goes first: the toolbar's own saved choice
        // (View > Cue Toolbar) is applied after it and is not overwritten.
        for area in [LayoutArea.masthead] + LayoutArea.allCases.filter({ $0 != .masthead }) {
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
        // A split view whose panes were laid out queues a save of its own arrangement for the next
        // run-loop pass (a delayed perform, whether or not it has an autosave name), and that timer
        // holds the split and so the window's views until it fires: on a slow or busy machine, well
        // after the window closed. Nothing here uses AppKit's own split autosave.
        for split in [verticalSplit, horizontalSplit] { NSObject.cancelPreviousPerformRequests(withTarget: split) }
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
    func placeDivider(_ split: NSSplitView, at position: CGFloat) {
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
            // The GO area, toolbar and bars are above the list in the same pane.
            let chrome = max(0, topColumn.frame.height - cueList.view.frame.height)
            let most = room - chrome - Self.minCueListHeight
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

#if DEBUG
    extension WorkspaceWindowController {
        /// The window at the size a test launched with, its top-left corner a little inside the screen's
        /// (so every corner can be pressed), and no larger than the screen's visible area.
        func placeForTest(_ size: TestWindowSize) {
            guard let window else { return }
            let visible = (window.screen ?? NSScreen.main)?.visibleFrame ?? window.frame
            var want = window.minSize
            if case .size(let s) = size {
                want = CGSize(width: max(s.width, want.width), height: max(s.height, want.height))
            }
            let width = min(want.width, visible.width - 100)
            let height = min(want.height, visible.height - 50)
            window.setFrame(
                NSRect(x: visible.minX + 80, y: visible.maxY - 40 - height, width: width, height: height),
                display: false)
        }
    }
#endif

/// The vertical split, whose divider press the window tracks itself (`pressDivider`).
final class InspectorSplitView: NSSplitView {
    /// Takes a press on the divider; true: handled, the split does nothing with it.
    var onPress: ((NSEvent) -> Bool)?

    override func mouseDown(with event: NSEvent) {
        if onPress?(event) == true { return }
        super.mouseDown(with: event)
    }
}

extension WorkspaceWindowController: NSSplitViewDelegate {
    /// The inspector's height with the divider at `y` (the split's own coordinates, flipped).
    fileprivate func inspectorHeight(atDividerY y: CGFloat) -> CGFloat {
        verticalSplit.bounds.height - y - verticalSplit.dividerThickness / 2
    }

    /// Whether the divider with the inspector `height` high closes it: less than half its least height
    /// would be left, as QLab 5's does. Above that the least height holds it.
    static func dragClosesInspector(height: CGFloat) -> Bool { height < minInspectorHeight / 2 }

    /// A press on the inspector's divider, tracked here so the inspector goes the moment the drag passes
    /// the line, while the button is still down, and comes back if the drag goes back up before the
    /// release (AppKit's own tracking decides nothing until the release). A divider placed by code never
    /// closes it.
    func pressDivider(_ event: NSEvent) -> Bool {
        guard let window, let drag = beginDividerDrag(at: event.locationInWindow) else { return false }
        window.trackEvents(
            matching: [.leftMouseDragged, .leftMouseUp], timeout: .infinity, mode: .eventTracking
        ) { next, stop in
            guard let next else { return }
            if next.type == .leftMouseUp {
                stop.pointee = true
            } else {
                drag.move(toWindowPoint: next.locationInWindow)
            }
        }
        drag.end()
        return true
    }

    /// The press at `point` (window coordinates) when it is on the inspector's divider.
    func beginDividerDrag(at point: NSPoint) -> DividerDrag? {
        guard window != nil, !isShowMode, isVisible(.inspector) else { return nil }
        let start = verticalSplit.convert(point, from: nil)
        let edge = inspector.view.frame.minY
        guard abs(start.y - edge) <= 5 else { return nil }
        return DividerDrag(window: self, grab: start.y - edge)
    }

    /// One press-drag-release of the divider.
    @MainActor
    final class DividerDrag {
        private unowned let wc: WorkspaceWindowController
        private let grab: CGFloat
        /// The inspector is hidden by the drag, and will stay closed if the button is let go like this.
        private(set) var closed = false

        init(window: WorkspaceWindowController, grab: CGFloat) {
            (wc, self.grab) = (window, grab)
        }

        /// The pointer is at `point` (window coordinates), the button down.
        func move(toWindowPoint point: NSPoint) {
            let split = wc.verticalSplit
            let y = split.convert(point, from: nil).y - grab
            move(toInspectorHeight: wc.inspectorHeight(atDividerY: y))
        }

        func move(toInspectorHeight height: CGFloat) {
            let split = wc.verticalSplit
            let closes = WorkspaceWindowController.dragClosesInspector(height: height)
            if closes != closed {
                closed = closes
                if closes { wc.inspector.endGestures() }
                wc.inspector.view.isHidden = closes
                wc.footer.toggles[.inspector]?.state = closes ? .off : .on
            }
            guard !closes else { return }
            let room = split.bounds.height - split.dividerThickness
            let held = max(height, WorkspaceWindowController.minInspectorHeight)
            wc.inspectorHeightConstraint?.constant = held
            wc.placeDivider(split, at: room - held)
        }

        /// The button is let go: closed for good if it was left closed, else the height is kept.
        func end() {
            if closed {
                wc.setVisible(.inspector, false)
            } else {
                wc.window?.contentView?.layoutSubtreeIfNeeded()
                if wc.inspector.view.frame.height > 0 {
                    wc.inspectorHeightConstraint?.constant = wc.inspector.view.frame.height
                }
                wc.saveLayout()
            }
        }
    }
}
