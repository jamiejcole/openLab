// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The cue list on the real engine's demo show: Intro, a Fade cue of Intro, and a
/// group "Act One" of three cues.
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
        // Radio's file is missing: broken, by icon and label, not by colour.
        XCTAssertTrue(cueState("Broken", of: "Radio", in: app).waitForExistence(timeout: 10))
        XCTAssertEqual(cueStates("Running", in: app).count, 0, "nothing plays when the show opens")
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: Intro, at the playhead
        XCTAssertTrue(cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10), "Intro runs")
    }

    func testGroupStartsExpandedAndCollapses() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Act One"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Door slam"].waitForExistence(timeout: 5), "groups open expanded")
        XCTAssertTrue(app.staticTexts["Rain"].exists)
        // The L key loads the selected cue: its row shows it.
        selectCue("Door slam", in: app)
        app.typeKey("l", modifierFlags: [])
        XCTAssertTrue(cueState("Loaded", of: "Door slam", in: app).waitForExistence(timeout: 10), "Door slam is loaded")
        // Rain is flagged: the flag is a status-column icon; its blue colour adds no marker.
        XCTAssertTrue(app.images["Flagged"].exists)
        XCTAssertFalse(app.images["Blue"].exists)
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
