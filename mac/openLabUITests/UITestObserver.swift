// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The UI test bundle's observer (its principal class): the test media the run made are removed
/// when the bundle ends, as the unit tests' observer removes theirs.
@objc(OpenLabUITestObserver)
final class UITestObserver: NSObject, XCTestObservation {
    override init() {
        super.init()
        XCTestObservationCenter.shared.addTestObserver(self)
    }

    func testBundleDidFinish(_ testBundle: Bundle) {
        TestMediaCache.removeAll()
    }
}
