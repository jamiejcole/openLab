// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

enum SaveChoice: Equatable { case save, dontSave, cancel }

/// Every prompt the window/quit flows can raise. A protocol so tests script the answers instead
/// of running a modal loop.
@MainActor
protocol WorkspaceAlerts {
    /// Close a workspace while cues play. True: close it.
    func confirmCloseWhilePlaying(workspace: String) -> Bool
    /// Quit while cues play anywhere. True: go on to the save prompts and quit.
    func confirmQuitWhilePlaying() -> Bool
    /// The engine is not answering at quit. True: quit anyway.
    func confirmQuitDisconnected() -> Bool
    func unsavedChanges(workspace: String) -> SaveChoice
    func showError(_ message: String)
}

/// The real thing. Cancel is the default (Return) button on the destructive prompts: a stray
/// keypress during a show must not close a playing workspace.
@MainActor
struct AppKitAlerts: WorkspaceAlerts {
    func confirmCloseWhilePlaying(workspace: String) -> Bool {
        run("alert.closePlaying.title", "alert.closePlaying.message", ["alert.cancel", "alert.close"]) == 1
    }

    func confirmQuitWhilePlaying() -> Bool {
        run("alert.quitPlaying.title", "alert.quitPlaying.message", ["alert.cancel", "alert.quit"]) == 1
    }

    func confirmQuitDisconnected() -> Bool {
        run("alert.quitDisconnected.title", "alert.quitDisconnected.message", ["alert.cancel", "alert.quitAnyway"]) == 1
    }

    func unsavedChanges(workspace: String) -> SaveChoice {
        let alert = NSAlert()
        alert.messageText = String(format: loc("alert.unsaved.title"), workspace)
        alert.informativeText = loc("alert.unsaved.message")
        for key in ["alert.save", "alert.cancel", "alert.dontSave"] { alert.addButton(withTitle: loc(key)) }
        switch alert.runModal() {
        case .alertFirstButtonReturn: return .save
        case .alertThirdButtonReturn: return .dontSave
        default: return .cancel
        }
    }

    func showError(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = message
        alert.addButton(withTitle: loc("alert.ok"))
        alert.runModal()
    }

    /// Index of the clicked button.
    private func run(_ title: String, _ message: String, _ buttons: [String]) -> Int {
        let alert = NSAlert()
        alert.messageText = loc(title)
        alert.informativeText = loc(message)
        for key in buttons { alert.addButton(withTitle: loc(key)) }
        return alert.runModal().rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
    }
}
