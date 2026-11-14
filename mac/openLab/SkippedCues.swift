// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

extension Cue {
    /// Disarmed with "skip if disarmed" set: passed over as though it were not there (QLab 5,
    /// manual p. 128). An armed cue's box has no effect.
    var isSkipped: Bool { raw["armed"] == .bool(false) && raw["skip_if_disarmed"] == .bool(true) }
}
