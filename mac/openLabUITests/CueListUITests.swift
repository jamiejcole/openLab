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

/// #489's click flows with real presses, on a show of three looping Audio cues: the table's mouse
/// tracking, its action and a press that ends an edit (QLab 5: pp. 91, 99-102).
@MainActor
final class PlayheadClickUITests: XCTestCase {
    private func launchThree() -> XCUIApplication {
        let app = launchApp(opening: ["Three"])
        XCTAssertTrue(waitFor("isHittable == true", app.staticTexts["One"]), "the show is ready")
        selectCue("One", in: app)
        assertAllOn("One", in: app)
        return app
    }

    private func row(_ title: String, in app: XCUIApplication) -> XCUIElement { cueRow(title, in: app) }

    /// The highlighted rows are `highlighted` (default: `title`), and the marker and the standby
    /// display are on `title`.
    private func assertAllOn(
        _ title: String, highlighted: [String]? = nil, in app: XCUIApplication, line: UInt = #line
    ) {
        let number = ["One": "1", "Two": "2", "Three": "3"][title] ?? ""
        let standby = app.staticTexts["standby-field"]
        XCTAssertTrue(waitFor("value == '\(number) \u{00B7} \(title)'", standby), "standby on \(title)", line: line)
        XCTAssertTrue(waitFor("isSelected == true", row(title, in: app)), "\(title) highlighted", line: line)
        for name in ["One", "Two", "Three"] {
            let on = (highlighted ?? [title]).contains(name)
            XCTAssertEqual(row(name, in: app).isSelected, on, "\(name) highlight", line: line)
            let marker = row(name, in: app).images.matching(identifier: "cue-playhead").firstMatch
            XCTAssertEqual(marker.exists, name == title, "\(name) marker", line: line)
        }
    }

    func testGoPanicClickTheFirstCueGo() {
        let app = launchThree()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "One", in: app).waitForExistence(timeout: 10))
        assertAllOn("Two", in: app)
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", cueState("Running", of: "One", in: app)), "One fades out and stops")
        assertAllOn("Two", in: app)
        row("One", in: app).click()
        assertAllOn("One", in: app)
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "One", in: app).waitForExistence(timeout: 10), "One runs again")
        assertAllOn("Two", in: app)
    }

    /// The press ends an edit: the field held the highlight on Two while the GO button moved the
    /// playhead on, and ending the edit must not move the highlight under the press.
    func testClickingTheHighlightedRowResetsThePlayhead() {
        let app = launchThree()
        row("Two", in: app).click()
        assertAllOn("Two", in: app)
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        name.click()
        XCTAssertTrue(waitFor("hasKeyboardFocus == true", name), "the field takes focus")
        // A button takes no focus, so the field keeps it (Space would type there).
        app.buttons["go-button"].click()
        XCTAssertTrue(cueState("Running", of: "Two", in: app).waitForExistence(timeout: 10))
        XCTAssertTrue(waitFor("hasKeyboardFocus == true", name), "the field kept the focus")
        let standby = app.staticTexts["standby-field"]
        XCTAssertTrue(waitFor("value == '3 \u{00B7} Three'", standby), "the playhead moved on")
        XCTAssertTrue(row("Two", in: app).isSelected, "held while the field has focus")
        row("Two", in: app).click()
        assertAllOn("Two", in: app)
    }

    func testClickingTheStatusColumn() {
        let app = launchThree()
        // The status column is the row's first 80 points; its images only show state.
        row("Three", in: app).coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: 60, dy: 12)).click()
        assertAllOn("Three", in: app)
    }

    func testMultiSelection() {
        let app = launchThree()
        XCUIElement.perform(withKeyModifiers: .command) { row("Three", in: app).click() }
        assertAllOn("Three", highlighted: ["One", "Three"], in: app)
    }
}
