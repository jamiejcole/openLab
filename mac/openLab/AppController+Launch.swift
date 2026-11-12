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
        let asked = filesToOpen
        filesToOpen = []
        if live {
            engineLog.notice("launch: the engine already runs workspaces: the launch choice is not applied")
        } else {
            switch behavior {
            case .launcher, .nothing: break
            // The app was started by opening a show: that show, not a blank one beside it.
            case .newBlank: if asked.isEmpty { await newWorkspace() }
            case .restore: await restoreWorkspaces(launchPreferences.openFiles)
            }
        }
        for url in asked { await openWorkspace(url: url) }
        launching = false
        launchApplied = true
        // Asked for while those were opening.
        let late = filesToOpen
        filesToOpen = []
        for url in late { await openWorkspace(url: url) }
        // Nothing opened (nothing to restore, every file missing): the launcher, not an empty app.
        if live || behavior != .nothing { showLauncherIfNoWindows() }
    }

    /// Show files the system asked the app to open (a double-click in Finder, a drop on the Dock
    /// icon). Until the engine is up and the launch choice is done they wait, then open after it.
    func openFiles(_ urls: [URL]) {
        let shows = urls.filter { $0.pathExtension.lowercased() == "openlab" }
        guard !shows.isEmpty else { return }
        if !launchApplied {
            filesToOpen += shows
        } else {
            Task { for url in shows { await openWorkspace(url: url) } }
        }
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

    /// Whether the restore may raise its modal prompt: every window is known to be out of Show Mode.
    /// A window with no document yet has no known mode (it has its mode with the snapshot) and could
    /// enter Show Mode under the prompt, where the transport keys would do nothing until it is answered.
    var restorePromptIsSafe: Bool {
        allWindows.allSatisfy { !$0.isShowMode && $0.store.document != nil }
    }

    /// A window just opened has its mode with the snapshot, a moment after the open returned. Giving
    /// it a moment spares a needless skip of the next file's prompt; `restorePromptIsSafe` decides
    /// whether that prompt comes up. Bounded (200 ms): a stalled engine must not hold the restore.
    private func windowsKnowTheirMode() async {
        for _ in 0..<100 {
            if allWindows.allSatisfy({ $0.store.document != nil }) { return await Task.yield() }
            try? await Task.sleep(nanoseconds: 2_000_000)
        }
    }

    /// The saved files now open, for the next launch.
    func recordOpenWorkspaces() {
        // A workspace just saved at the quit prompt may not know its new path yet, and isn't in the list yet.
        launchPreferences.openFiles = session.orderedStores.compactMap {
            savedAsPaths[ObjectIdentifier($0.store)] ?? session.info(of: $0.store, doc: $0.doc).path
        }
    }
}
