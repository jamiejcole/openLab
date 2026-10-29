// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The GO key against the fake engine, which starts the demo show's loaded cue when it receives
/// `go` for that workspace (and does nothing for the empty second one).
@MainActor
final class TransportUITests: XCTestCase {
    func testSpaceSendsGoForTheFrontWorkspace() {
        let app = launchDemo()
        let states = app.images.matching(identifier: "cue-state")
        let loaded = states.matching(NSPredicate(format: "label == 'Loaded'"))
        XCTAssertTrue(loaded.firstMatch.waitForExistence(timeout: 10))

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        XCTAssertTrue(waitFor("count == 0", loaded), "the cue that was loaded is running after GO")
    }
}
