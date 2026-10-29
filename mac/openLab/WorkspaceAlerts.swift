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
    /// Close or quit a workspace that is in Show Mode and not playing (SM-05). True: go on.
    func confirmCloseInShowMode(workspace: String) -> Bool
    func confirmQuitInShowMode() -> Bool
    /// The engine is not answering at quit. True: quit anyway.
    func confirmQuitDisconnected() -> Bool
    func unsavedChanges(workspace: String) -> SaveChoice
    func showError(_ message: String)
    func showInfo(_ message: String)
    /// `open` found an autosave newer than the show file (NFR-05). nil: Cancel, nothing opens.
    func recoveryOffer(workspace: String, modifiedMs: Int64) -> RecoveryChoice?
    /// After an unclean engine exit: open this autosave as an untitled workspace? True: open it.
    /// `setAside`: the engine set it aside as not newer than its show file (it may still be).
    func offerAutosave(name: String, modifiedMs: Int64?, setAside: Bool) -> Bool
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

    func confirmCloseInShowMode(workspace: String) -> Bool {
        run("alert.closeShowMode.title", "alert.closeShowMode.message", ["alert.cancel", "alert.close"]) == 1
    }

    func confirmQuitInShowMode() -> Bool {
        run("alert.quitShowMode.title", "alert.quitShowMode.message", ["alert.cancel", "alert.quit"]) == 1
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

    func showInfo(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.messageText = message
        alert.addButton(withTitle: loc("alert.ok"))
        alert.runModal()
    }

    func recoveryOffer(workspace: String, modifiedMs: Int64) -> RecoveryChoice? {
        let alert = NSAlert()
        alert.messageText = String(format: loc("alert.recoveryOffer.title"), workspace)
        alert.informativeText = String(format: loc("alert.recoveryOffer.message"), Self.date(modifiedMs))
        for key in ["alert.recoveryOffer.restore", "alert.cancel", "alert.recoveryOffer.ignore"] {
            alert.addButton(withTitle: loc(key))
        }
        switch alert.runModal() {
        case .alertFirstButtonReturn: return .restore
        case .alertThirdButtonReturn: return .ignore
        default: return nil
        }
    }

    func offerAutosave(name: String, modifiedMs: Int64?, setAside: Bool) -> Bool {
        let alert = NSAlert()
        alert.messageText = String(format: loc("alert.autosave.title"), name)
        alert.informativeText = String(
            format: loc(setAside ? "alert.autosave.setAsideMessage" : "alert.autosave.message"),
            modifiedMs.map(Self.date) ?? "?")
        for key in ["alert.autosave.open", "alert.autosave.skip"] { alert.addButton(withTitle: loc(key)) }
        return alert.runModal() == .alertFirstButtonReturn
    }

    private static func date(_ ms: Int64) -> String {
        DateFormatter.localizedString(
            from: Date(timeIntervalSince1970: Double(ms) / 1000), dateStyle: .medium, timeStyle: .medium)
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
