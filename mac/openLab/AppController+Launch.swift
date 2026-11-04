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
    func applyLaunchBehavior() async {
        let behavior = launchPreferences.behavior
        switch behavior {
        case .launcher, .nothing: break
        case .newBlank: await newWorkspace()
        case .restore: await restoreWorkspaces(launchPreferences.openFiles)
        }
        launching = false
        // Nothing opened (nothing to restore, every file missing): the launcher, not an empty app.
        if behavior != .nothing { showLauncherIfNoWindows() }
    }

    /// Opens `paths` in order. A file that cannot be opened is reported in the log and the footer
    /// and skipped: never a dialog, since the app may be starting in front of a show.
    private func restoreWorkspaces(_ paths: [String]) async {
        var failures: [String] = []
        for path in paths {
            var failed: EngineError?
            await openWorkspace(url: URL(fileURLWithPath: path)) { failed = $0 }
            if let failed {
                let message = String(
                    format: loc("error.restoreFailed"), URL(fileURLWithPath: path).lastPathComponent,
                    describe(failed))
                engineLog.error("launch: \(message, privacy: .public)")
                failures.append(message)
            }
        }
        guard !failures.isEmpty, let wc = allWindows.first else { return }
        for message in failures { wc.quietNotice(message, error: true) }
    }

    /// The saved files now open, for the next launch.
    func recordOpenWorkspaces() {
        launchPreferences.openFiles = session.workspaces.compactMap(\.path)
    }
}
