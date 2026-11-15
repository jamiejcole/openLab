// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// How long the pointer rests on a control before its tooltip shows. macOS's own wait (1.5 s) is
/// too slow for an operator checking why a cue is broken mid-rehearsal.
enum ToolTipDelay {
    /// The wait, in seconds.
    static let seconds: TimeInterval = 0.65

    /// AppKit's tooltip manager reads this key once, as whole milliseconds, when the app object is
    /// made: a fractional number is cut to 0, which would show every tip at once.
    static let defaultsKey = "NSInitialToolTipDelay"
    static var milliseconds: Int { Int((seconds * 1000).rounded()) }

    /// Registers the wait as a default, so a value the user set (`defaults write`, a launch
    /// argument) still wins. Call before `NSApplication.shared` is first touched.
    static func register() {
        UserDefaults.standard.register(defaults: [defaultsKey: milliseconds])
    }
}
