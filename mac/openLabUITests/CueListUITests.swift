// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The cue list against the fake engine's demo show (the first workspace): cue 1 running, a fade
/// cue at the playhead with a missing target, and a collapsed group "Act One" of three cues.
@MainActor
final class CueListUITests: XCTestCase {
    private func launch() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        // The empty second workspace opens in front of the demo one; close it (nothing is playing in it).
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)
        let gone = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: gone, object: windows)], timeout: 10),
            .completed)
        return app
    }

    func testListShowsTheCues() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Act One"].exists)
        XCTAssertTrue(app.staticTexts["a.wav"].exists, "the audio cue's target column shows its file")
    }

    func testRunningCueShowsAStateIconWithALabel() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        let states = app.images.matching(identifier: "cue-state")
        XCTAssertTrue(states.firstMatch.waitForExistence(timeout: 5))
        let labels = (0..<states.count).map { states.element(boundBy: $0).label }
        XCTAssertTrue(labels.contains("Running"), "\(labels)")
        // The fade cue has a missing target: broken, by icon and label, not by colour.
        XCTAssertTrue(labels.contains("Broken"), "\(labels)")
    }

    func testGroupExpandsToItsChildren() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Act One"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.staticTexts["Door slam"].exists)
        let twisty = app.disclosureTriangles.firstMatch
        XCTAssertTrue(twisty.waitForExistence(timeout: 5))
        twisty.click()
        XCTAssertTrue(app.staticTexts["Door slam"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Rain"].exists)
        let states = app.images.matching(identifier: "cue-state")
        let labels = (0..<states.count).map { states.element(boundBy: $0).label }
        XCTAssertTrue(labels.contains("Loaded") && labels.contains("Paused"), "\(labels)")
        // The paused cue is blue and flagged: shape and flag symbol carry it, not only the tint.
        XCTAssertTrue(app.images["Blue"].exists)
        XCTAssertTrue(app.images["Flagged"].exists)
    }
}
