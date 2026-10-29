// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The cue list against the fake engine's demo show (the first workspace): cue 1 running, a fade
/// cue at the playhead with a missing target, and a group "Act One" of three cues.
@MainActor
final class CueListUITests: XCTestCase {
    func testListShowsTheCues() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Act One"].exists)
        XCTAssertTrue(app.staticTexts["a.wav"].exists, "the audio cue's target column shows its file")
    }

    func testRunningCueShowsAStateIconWithALabel() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        let states = app.images.matching(identifier: "cue-state")
        XCTAssertTrue(states.firstMatch.waitForExistence(timeout: 5))
        let labels = (0..<states.count).map { states.element(boundBy: $0).label }
        XCTAssertTrue(labels.contains("Running"), "\(labels)")
        // The fade cue has a missing target: broken, by icon and label, not by colour.
        XCTAssertTrue(labels.contains("Broken"), "\(labels)")
    }

    func testGroupStartsExpandedAndCollapses() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Act One"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Door slam"].waitForExistence(timeout: 5), "groups open expanded")
        XCTAssertTrue(app.staticTexts["Rain"].exists)
        let states = app.images.matching(identifier: "cue-state")
        let labels = (0..<states.count).map { states.element(boundBy: $0).label }
        XCTAssertTrue(labels.contains("Loaded") && labels.contains("Paused"), "\(labels)")
        // The paused cue is blue and flagged: shape and flag symbol carry it, not only the tint.
        XCTAssertTrue(app.images["Blue"].exists)
        XCTAssertTrue(app.images["Flagged"].exists)
        let twisty = app.disclosureTriangles.firstMatch
        XCTAssertTrue(twisty.exists)
        twisty.click()
        XCTAssertTrue(waitFor("exists == false", app.staticTexts["Door slam"], timeout: 5))
    }

    /// M5.6b: double-clicking a Fade cue's Duration cell edits it in place (QLab 5, manual pp. 94-95);
    /// the engine stores it and the cell and the inspector show the new duration.
    func testFadeDurationIsEditedInTheDurationColumn() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        let cell = app.staticTexts["00:05.00"]  // the Fade cue's duration
        XCTAssertTrue(cell.waitForExistence(timeout: 5))
        cell.doubleClick()
        // The field opens focused with its text selected, so typing replaces it. It is laid over the
        // outline, which lists only its rows to accessibility, so it is reached by keyboard focus.
        app.typeText("1:30\r")
        XCTAssertTrue(app.staticTexts["01:30.00"].waitForExistence(timeout: 10), "the cell shows the stored duration")
        let inspector = app.textFields["inspector-duration"]
        XCTAssertTrue(waitFor("value == '01:30.00'", inspector, timeout: 5), "the inspector follows")
    }
}
