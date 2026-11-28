// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Video pages' editors: a stage's editor window and a route's editor dialog.
extension WorkspaceSettingsWindowController {
    /// Opens the editor of `stage` (one window per stage; a second Edit brings it forward). It goes
    /// when its stage is removed, and an undo bringing the stage back does not reopen it.
    @discardableResult
    func openStageEditor(_ stage: ModelID) -> StageEditorWindowController {
        let editor =
            stageEditors[stage] ?? commitsAsItGoes(StageEditorWindowController(store: model.store, stage: stage))
        stageEditors[stage] = editor
        stageEditor = editor
        editor.presentsDialogs = presentsEditors
        editor.onGone = { [weak self, weak editor] in
            guard let self, let editor else { return }
            if self.stageEditors[stage] === editor { self.stageEditors[stage] = nil }
            if self.stageEditor === editor { self.stageEditor = nil }
        }
        if presentsEditors { editor.present() } else { editor.render() }
        return editor
    }

    /// What is typed in an editor is committed as its window goes, as the settings window's is, never by
    /// its next opening (which may be during a show).
    func commitsAsItGoes<Editor: NSWindowController>(_ editor: Editor) -> Editor {
        if let window = editor.window {
            subs.append(
                NotificationCenter.default.publisher(for: NSWindow.willCloseNotification, object: window).sink { note in
                    MainActor.assumeIsolated { _ = (note.object as? NSWindow)?.makeFirstResponder(nil) }
                })
        }
        return editor
    }

    /// Opens the editor of `route` as a dialog run modally over the app (none while one is up, or
    /// for a route gone since its row was drawn); returns once it is up (off screen) or, on screen,
    /// once it is done.
    @discardableResult
    func openRouteEditor(_ route: ModelID) -> RouteEditorController? {
        guard routeEditor == nil, let editor = RouteEditorController(store: model.store, route: route) else {
            return nil
        }
        routeEditor = editor
        editor.onDone = { [weak self] in self?.endRouteEditor() }
        if presentsEditors { editor.run(over: window) }
        return editor
    }

    /// Takes the route editor down, if it is up.
    func endRouteEditor() {
        guard let editor = routeEditor else { return }
        routeEditor = nil
        if let w = editor.window, NSApp.modalWindow === w { NSApp.stopModal() }
        editor.close()
    }

    /// Whether `window` is this window or one of its editors (Edit > Undo then acts on this
    /// workspace).
    func owns(_ window: NSWindow) -> Bool {
        self.window === window || stageEditors.values.contains { $0.window === window }
            || routeEditor?.window === window
            || stageEditors.values.contains { $0.resizeDialog?.window === window }
    }
}
