// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Another running copy of the app (an `NSRunningApplication`; a fake in tests).
protocol RunningCopy: AnyObject {
    var processIdentifier: pid_t { get }
    @discardableResult func activate(options: NSApplication.ActivationOptions) -> Bool
    /// The cooperative request (macOS 14+): honoured when `application` is active or yielded.
    @available(macOS 14, *)
    @discardableResult func activate(from application: NSRunningApplication, options: NSApplication.ActivationOptions)
        -> Bool
}

extension NSRunningApplication: RunningCopy {}

/// How a second copy brings the first forward as it exits (#336).
@MainActor
struct HandOverActivation {
    /// macOS 14+: activation is cooperative. `activateIgnoringOtherApps` has no effect there, and
    /// another app is let in only when the active app yields to it.
    var cooperative: Bool
    /// `NSApp.yieldActivation(to:)` (macOS 14+).
    var yield: @MainActor (RunningCopy) -> Void

    /// Called once this copy finished launching (it is the active app then, so it has
    /// something to yield).
    func bringForward(_ first: RunningCopy) {
        if cooperative, #available(macOS 14, *) {
            yield(first)
            first.activate(from: NSRunningApplication.current, options: [])
        } else {
            first.activate(options: [.activateIgnoringOtherApps])
        }
    }

    static var live: HandOverActivation {
        if #available(macOS 14, *) {
            return HandOverActivation(cooperative: true) { copy in
                if let app = copy as? NSRunningApplication { NSApp.yieldActivation(to: app) }
            }
        }
        return HandOverActivation(cooperative: false) { _ in }
    }
}

/// What `AppDelegate` takes from the system at launch, injected so the second-copy path is
/// unit-tested (#338): that path must build no menu and no engine session, since a second copy
/// that reached the shared engine could end every show the first runs with its Quit.
@MainActor
struct AppLaunch {
    /// The copy launched first, when this one has to hand over to it; nil: this copy runs.
    var runningFirst: @MainActor () -> RunningCopy?
    var makeEngine: @MainActor () -> (EngineSession, EngineSupervisor)
    var installMenu: @MainActor (_ delegate: NSMenuDelegate) -> Void
    var activation: HandOverActivation
    var terminate: @MainActor () -> Void

    static var live: AppLaunch {
        AppLaunch(
            runningFirst: {
                // The fake engine (UI tests) and the unit-test host share no engine with anyone.
                guard EngineMode.fromLaunchArguments() == .real, !EngineMode.isUnitTestHost() else { return nil }
                return SingleInstance.runningFirst()
            },
            makeEngine: { EngineMode.fromLaunchArguments().make() },
            installMenu: { delegate in
                NSApp.appearance = NSAppearance(named: .darkAqua)
                let menu = MainMenu.make()
                menu.transport.delegate = delegate
                menu.edit.delegate = delegate
                menu.openRecent.delegate = delegate
                NSApp.mainMenu = menu.bar
                NSApp.windowsMenu = menu.windows
                NSApp.helpMenu = menu.help
            },
            activation: .live,
            terminate: { NSApp.terminate(nil) })
    }
}
