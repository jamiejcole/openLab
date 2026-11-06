// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// What the app does once it is running and the engine is up (WS-08).
enum LaunchBehavior: String, CaseIterable {
    case launcher, restore, newBlank, nothing

    var titleKey: String {
        switch self {
        case .launcher: return "prefs.launch.launcher"
        case .restore: return "prefs.launch.restore"
        case .newBlank: return "prefs.launch.newBlank"
        case .nothing: return "prefs.launch.nothing"
        }
    }
}

/// The app-wide launch preference and the files that were open at the last quit. `defaults` is
/// nil in tests that do not look at it: nothing is read or written then.
struct LaunchPreferences {
    let defaults: UserDefaults?
    private static let behaviorKey = "launch-behavior"
    private static let filesKey = "launch-open-files"

    var behavior: LaunchBehavior {
        get { defaults?.string(forKey: Self.behaviorKey).flatMap(LaunchBehavior.init) ?? .launcher }
        nonmutating set { defaults?.set(newValue.rawValue, forKey: Self.behaviorKey) }
    }

    /// The saved files open at the last quit, in open order.
    var openFiles: [String] {
        get { defaults?.stringArray(forKey: Self.filesKey) ?? [] }
        nonmutating set { defaults?.set(newValue, forKey: Self.filesKey) }
    }
}
