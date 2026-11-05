// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// What opens at launch (WS-08): the launcher, the workspaces open at the last quit, a new blank
/// workspace, or nothing.
extension AppController {
    var launchPreferences: LaunchPreferences { LaunchPreferences(defaults: launchDefaults) }

    /// Called as the app starts, before the engine is up: the launcher shows now unless another
    /// choice is made, which keeps it away until `applyLaunchBehavior` has run.
    func beginLaunch() {
        launching = launchPreferences.behavior != .launcher
        if !launching { showLauncher() }
    }

    /// Carries out the choice once the engine is up and its recovery check has been asked.
    /// Restoring only opens files: no cue is started and no playhead moves.
    /// An engine that already runs workspaces (the app was restarted under a running show; its
    /// welcome lists them) keeps them as the show: nothing is opened beside them.
    func applyLaunchBehavior() async {
        let behavior = launchPreferences.behavior
        let live = !session.workspaces.isEmpty || !session.stores.isEmpty
        if live {
            engineLog.notice("launch: the engine already runs workspaces: the launch choice is not applied")
        } else {
            switch behavior {
            case .launcher, .nothing: break
            case .newBlank: await newWorkspace()
            case .restore: await restoreWorkspaces(launchPreferences.openFiles)
            }
        }
        launching = false
        // Nothing opened (nothing to restore, every file missing): the launcher, not an empty app.
        if live || behavior != .nothing { showLauncherIfNoWindows() }
    }

    /// Opens `paths` in order. A file that cannot be opened is reported in the log and the footer
    /// and skipped: never a dialog, since the app may be starting in front of a show.
    private func restoreWorkspaces(_ paths: [String]) async {
        var failures: [String] = []
        for path in paths {
            await windowsKnowTheirMode()
            var failed: EngineError?
            await openWorkspace(url: URL(fileURLWithPath: path)) { failed = $0 }
            if let failed {
                let name = URL(fileURLWithPath: path).lastPathComponent
                let message: String
                if case .recoveryOffered = failed {
                    message = String(format: loc("error.restoreAutosave"), name)
                } else {
                    message = String(format: loc("error.restoreFailed"), name, describe(failed))
                }
                engineLog.error("launch: \(message, privacy: .public)")
                failures.append(message)
            }
        }
        guard !failures.isEmpty else { return }
        // With a window open its footer and log carry them; with none, the launcher that follows.
        if let wc = allWindows.first {
            for message in failures { wc.quietNotice(message, error: true) }
        } else {
            launcher.showNotice(failures.joined(separator: "\n"))
        }
    }

    /// A window just opened has its mode with the snapshot, a moment after the open returned: the
    /// next file's prompt must know whether Show Mode is up. Bounded (200 ms): a stalled engine
    /// must not hold the restore.
    private func windowsKnowTheirMode() async {
        for _ in 0..<100 {
            if allWindows.allSatisfy({ $0.store.document != nil }) { return await Task.yield() }
            try? await Task.sleep(nanoseconds: 2_000_000)
        }
    }

    /// The saved files now open, for the next launch.
    func recordOpenWorkspaces() {
        // The store's own path first: a workspace just saved at the quit prompt is not in the list yet.
        launchPreferences.openFiles = session.orderedStores.compactMap {
            session.info(of: $0.store, doc: $0.doc).path
        }
    }
}
