// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Video pages' editors: a stage's editor window and a route's editor sheet.
extension WorkspaceSettingsWindowController {
    /// Opens the editor of `stage` (one window per stage; a second Edit brings it forward).
    @discardableResult
    func openStageEditor(_ stage: ModelID) -> StageEditorWindowController {
        let editor =
            stageEditors[stage] ?? commitsAsItGoes(StageEditorWindowController(store: model.store, stage: stage))
        stageEditors[stage] = editor
        stageEditor = editor
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

    /// Opens the editor of `route` as a sheet on this window (none while one is up, or for a
    /// route gone since its row was drawn).
    @discardableResult
    func openRouteEditor(_ route: ModelID) -> RouteEditorController? {
        guard routeEditor == nil, let editor = RouteEditorController(store: model.store, route: route) else {
            return nil
        }
        routeEditor = editor
        editor.onDone = { [weak self] in self?.endRouteEditor() }
        if presentsEditors, let window, let sheet = editor.window {
            StandBack.beginSheet(sheet, on: window)
        }
        return editor
    }

    /// Takes the route editor down, if it is up.
    func endRouteEditor() {
        guard let editor = routeEditor else { return }
        routeEditor = nil
        if let sheet = editor.window, let parent = sheet.sheetParent {
            parent.endSheet(sheet)
        }
        editor.close()
    }

    /// Whether `window` is this window or one of its editors (Edit > Undo then acts on this
    /// workspace).
    func owns(_ window: NSWindow) -> Bool {
        self.window === window || stageEditors.values.contains { $0.window === window }
            || routeEditor?.window === window
    }
}
