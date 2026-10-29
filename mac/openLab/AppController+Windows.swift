// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The workspace windows: one per store (bound or waiting for a reattach), their titles, the
/// launcher shown when none is open, and the key window's controller.
extension AppController {
    // MARK: Windows

    /// A window for every store (bound or waiting for a reattach), made in open order; a store
    /// that left the session takes its window with it.
    func sync() {
        let live = session.orderedStores.map(\.store) + session.detached.map(\.store)
        for store in live where controllers[ObjectIdentifier(store)] == nil {
            let wc = WorkspaceWindowController(store: store)
            wc.onCloseRequest = { [weak self, weak store] in
                guard let self, let store else { return }
                Task { await self.closeWorkspace(store) }
            }
            wc.onGo = { [weak self] in Task { await self?.go(store) } }
            wc.onPauseAll = { [weak self] in Task { await self?.run(.pauseAll, on: store) } }
            wc.onResumeAll = { [weak self] in Task { await self?.run(.resumeAll, on: store) } }
            wc.onPanic = { [weak self] in Task { _ = await self?.panicAll() } }
            wc.onCueAction = { [weak self] action in Task { await self?.run(action, on: store) } }
            wc.onRelink = { [weak self] in Task { await self?.relinkMedia(on: store) } }
            controllers[ObjectIdentifier(store)] = wc
            // Hotkey warnings are logged when the settings arrive, not at the first key press.
            _ = keys(for: store)
            // Only model changes can move keys (runtime ticks must not walk every cue).
            keySubs[ObjectIdentifier(store)] = store.changes.sink { [weak self] change in
                switch change {
                case .reset, .model: MainActor.assumeIsolated { _ = self?.keys(for: store) }
                case .runtime, .progress, .waits, .notices: break
                }
            }
            if let doc = session.doc(of: store), let n = session.workspaces.first(where: { $0.doc == doc }) {
                wc.workspaceName = n.name
            }
            show(wc)
        }
        let liveIDs = Set(live.map(ObjectIdentifier.init))
        for id in controllers.keys where !liveIDs.contains(id) { dropWindow(id) }
        showLauncherIfNoWindows()
    }

    func dropWindow(_ id: ObjectIdentifier) {
        guard let wc = controllers.removeValue(forKey: id) else { return }
        keyCache[id] = nil
        keySubs[id] = nil
        wc.window?.close()
    }

    func updateTitles(_ list: [WorkspaceInfo]) {
        for info in list {
            guard let wc = windows[info.doc], let window = wc.window else { continue }
            wc.workspaceName = info.name
            window.isDocumentEdited = info.dirty
        }
    }

    /// Fronts (creating nothing) the window of `doc` and hides the launcher.
    func show(_ doc: ModelID) {
        guard let wc = windows[doc] else { return }
        show(wc)
    }

    private func show(_ wc: WorkspaceWindowController) {
        guard let window = wc.window else { return }
        if window.isMiniaturized { window.deminiaturize(nil) }
        window.makeKeyAndOrderFront(nil)
        launcher.window?.orderOut(nil)
    }

    private var hasWorkspaceWindow: Bool {
        controllers.values.contains { $0.window?.isVisible == true || $0.window?.isMiniaturized == true }
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
        if let wc = allWindows.first(where: { $0.window?.isMiniaturized == true }) {
            show(wc)
        } else if !hasWorkspaceWindow {
            showLauncher()
        }
    }

    func closeAllWindows() {
        for wc in controllers.values { wc.window?.close() }
        launcher.window?.close()
    }

    // MARK: Front-window routing

    var frontController: WorkspaceWindowController? {
        guard let key = keyWindow() else { return nil }
        return controllers.values.first { $0.window === key }
    }
}
