// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The GO key against the fake engine, which starts the demo show's loaded cue when it receives
/// `go` for that workspace (and does nothing for the empty second one).
@MainActor
final class TransportUITests: XCTestCase {
    func testSpaceSendsGoForTheFrontWorkspace() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)  // the empty second workspace is in front: close it
        let one = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: one, object: windows)], timeout: 10),
            .completed)
        let states = app.images.matching(identifier: "cue-state")
        let loaded = states.matching(NSPredicate(format: "label == 'Loaded'"))
        XCTAssertTrue(loaded.firstMatch.waitForExistence(timeout: 10))

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        let gone = NSPredicate(format: "count == 0")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: gone, object: loaded)], timeout: 10),
            .completed, "the cue that was loaded is running after GO")
    }

    /// The Show Log window in front never takes the keyboard: a click in it, then Space, still fires GO.
    func testSpaceSendsGoWithTheLogWindowInFront() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)
        let one = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: one, object: windows)], timeout: 10),
            .completed)
        let states = app.images.matching(identifier: "cue-state")
        let loaded = states.matching(NSPredicate(format: "label == 'Loaded'"))
        XCTAssertTrue(loaded.firstMatch.waitForExistence(timeout: 10))

        app.typeKey("l", modifierFlags: [.command])  // Window > Show Log
        let log = app.windows.matching(identifier: "log-window").firstMatch
        XCTAssertTrue(log.waitForExistence(timeout: 10))
        // The table's own area, not a label: a table view asks for key on a click.
        log.tables.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.9)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])

        let gone = NSPredicate(format: "count == 0")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: gone, object: loaded)], timeout: 10),
            .completed, "GO fired with the log window in front")
    }
}
