// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// #708: a stage covers its whole display over every app's window, the display with the menu bar
/// (the operator's, a laptop's only one) included. Whenever the app asks the operator something (an
/// alert, a sheet, an open or save panel) the stage on that display stands back under the app's
/// windows until it is answered (Cancel included), so the question can be seen. Nothing it draws
/// changes and no cue stops or fades. Every modal alert, sheet and panel the app runs goes through
/// here (`scripts/check-repo.sh` fails on one that does not).
@MainActor
final class StandBack {
    static let shared = StandBack()

    /// Questions up now.
    private(set) var questions = 0
    /// Told when the first question goes up (`true`) and when the last is answered (`false`).
    var changed: (@MainActor (Bool) -> Void)?

    var isAsking: Bool { questions > 0 }

    func begin() {
        questions += 1
        if questions == 1 { changed?(true) }
    }

    func end() {
        guard questions > 0 else { return }
        questions -= 1
        if questions == 0 { changed?(false) }
    }

    /// `alert`'s modal run, the stage standing back meanwhile.
    @discardableResult
    static func runModal(_ alert: NSAlert) -> NSApplication.ModalResponse {
        shared.begin()
        defer { shared.end() }
        return alert.runModal()
    }

    /// An open or save panel's modal run, the stage standing back meanwhile.
    static func runModal(_ panel: NSSavePanel) -> NSApplication.ModalResponse {
        shared.begin()
        defer { shared.end() }
        return panel.runModal()
    }

    /// `alert` as a sheet on `window`, the stage standing back until it is answered.
    static func beginSheet(
        _ alert: NSAlert, for window: NSWindow,
        completion: (@MainActor (NSApplication.ModalResponse) -> Void)? = nil
    ) {
        shared.begin()
        alert.beginSheetModal(for: window) { response in
            MainActor.assumeIsolated {
                shared.end()
                completion?(response)
            }
        }
    }

    /// `sheet` on `window`, the stage standing back until the sheet ends.
    static func beginSheet(_ sheet: NSWindow, on window: NSWindow) {
        shared.begin()
        window.beginSheet(sheet) { _ in MainActor.assumeIsolated { shared.end() } }
    }
}

/// Every question `base` asks, with the stage standing back while it is up (`StandBack`). The app
/// asks through this, whatever `base` is.
@MainActor
struct StandingBackAlerts: WorkspaceAlerts {
    let base: WorkspaceAlerts

    private func asking<T>(_ question: () -> T) -> T {
        StandBack.shared.begin()
        defer { StandBack.shared.end() }
        return question()
    }

    func confirmCloseWhilePlaying(workspace: String) -> Bool {
        asking { base.confirmCloseWhilePlaying(workspace: workspace) }
    }
    func confirmQuitWhilePlaying() -> Bool { asking { base.confirmQuitWhilePlaying() } }
    func confirmCloseInShowMode(workspace: String) -> Bool {
        asking { base.confirmCloseInShowMode(workspace: workspace) }
    }
    func confirmQuitInShowMode() -> Bool { asking { base.confirmQuitInShowMode() } }
    func confirmQuitDisconnected() -> Bool { asking { base.confirmQuitDisconnected() } }
    func unsavedChanges(workspace: String) -> SaveChoice { asking { base.unsavedChanges(workspace: workspace) } }
    func showError(_ message: String) { asking { base.showError(message) } }
    func showInfo(_ message: String) { asking { base.showInfo(message) } }
    func recoveryOffer(workspace: String, modifiedMs: Int64) -> RecoveryChoice? {
        asking { base.recoveryOffer(workspace: workspace, modifiedMs: modifiedMs) }
    }
    func offerAutosave(name: String, modifiedMs: Int64?, setAside: Bool) -> AutosaveChoice {
        asking { base.offerAutosave(name: name, modifiedMs: modifiedMs, setAside: setAside) }
    }
    func confirmDeleteAutosave(name: String) -> Bool { asking { base.confirmDeleteAutosave(name: name) } }
}
