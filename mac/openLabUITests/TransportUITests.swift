// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The GO key on the real engine: Space starts the cue at the playhead (the demo show's Intro).
@MainActor
final class TransportUITests: XCTestCase {
    func testSpaceStartsTheCueAtThePlayhead() {
        let app = launchDemo()
        XCTAssertEqual(cueStates("Running", in: app).count, 0, "nothing plays when the show opens")

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        XCTAssertTrue(cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10), "Intro runs after GO")
        XCTAssertEqual(cueStates("Running", in: app).count, 1, "and nothing else")
    }

    /// The Show Log window in front never takes the keyboard: a click in it, then Space, still fires GO.
    func testSpaceSendsGoWithTheLogWindowInFront() {
        let app = launchDemo()

        app.typeKey("l", modifierFlags: [.command, .option])  // Window > Show Log
        let log = app.descendants(matching: .any).matching(identifier: "log-window").firstMatch
        XCTAssertTrue(log.waitForExistence(timeout: 10))
        // The table's own area, not a label: a table view asks for key on a click.
        app.tables.matching(identifier: "log-table").firstMatch.coordinate(
            withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)
        ).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        XCTAssertTrue(
            cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10), "GO fired with the log window in front")
    }
}
