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
        XCTAssertTrue(app.buttons["cue-target-choose"].firstMatch.exists, "the audio cue's button")
        XCTAssertFalse(app.staticTexts["a.wav"].exists, "and not its file name")
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

    /// Double-clicking a Fade cue's Duration cell edits it in place (QLab 5, manual pp. 94-95);
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
        // A real open: no first click, the playhead's cue is already highlighted.
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

    // MARK: Editing a cell in place (#493)

    /// Double-clicking cue Two's Number cell, typing and Return: the cell, the highlight and the
    /// inspector show the new number. The single click before it only selects.
    func testDoubleClickNumber() {
        let app = launchThree()
        let number = app.staticTexts["2"]
        number.click()
        assertAllOn("Two", in: app)
        XCTAssertFalse(app.textFields["cue-number-editor"].exists, "a single click edits nothing")
        number.doubleClick()
        app.typeText("2.5\r")
        XCTAssertTrue(app.staticTexts["2.5"].waitForExistence(timeout: 10), "the cell shows 2.5")
        XCTAssertTrue(waitFor("value == '2.5'", app.textFields["inspector-number"]), "the inspector follows")
        XCTAssertTrue(row("Two", in: app).isSelected, "Two stays highlighted")
        XCTAssertTrue(
            waitFor("value == '2.5 \u{00B7} Two'", app.staticTexts["standby-field"]), "standby is still on Two")
    }

    /// Double-clicking Intro's Name cell, typing and Return: the cell shows the name and the Fade cue
    /// of Intro, which has its default name, follows it.
    func testDoubleClickName() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Fade and stop Intro"].waitForExistence(timeout: 10), "the fade follows Intro")
        app.staticTexts["Intro"].doubleClick()
        app.typeText("entrance\r")
        XCTAssertTrue(app.staticTexts["entrance"].waitForExistence(timeout: 10), "the cell shows entrance")
        XCTAssertTrue(
            app.staticTexts["Fade and stop entrance"].waitForExistence(timeout: 10), "the fade's name follows")
        XCTAssertFalse(cueState("Running", of: "entrance", in: app).exists, "Return did not fire GO")
    }

    /// In Show Mode a double-click edits nothing.
    func testDoubleClickInShowModeDoesNothing() {
        let app = launchThree()
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(app.staticTexts["show-mode-indicator"].waitForExistence(timeout: 5))
        app.staticTexts["2"].doubleClick()
        XCTAssertFalse(app.textFields["cue-number-editor"].waitForExistence(timeout: 1), "no editor opens")
        XCTAssertTrue(app.staticTexts["2"].exists, "the number is unchanged")
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

/// #494's flows that need real mouse and key events (QLab 5 manual pp. 94, 120, 127), on the "Targets" show.
/// A drag lands on the Target cell, as only that cell sets a target (5.0's change log); between rows it reorders.
@MainActor
final class CueTargetUITests: XCTestCase {
    private func launchTargets() -> XCUIApplication {
        let app = launchApp(opening: ["Targets"])
        XCTAssertTrue(waitFor("isHittable == true", cueRow("intro", in: app)), "the show is ready")
        return app
    }

    private func outline(_ app: XCUIApplication) -> XCUIElement { app.outlines["cue-list"] }

    /// The unnamed Fade's row: the only one that shows its number, 3.
    private func fadeRow(_ app: XCUIApplication) -> XCUIElement { cueRow("3", in: app) }

    private func drag(_ from: XCUIElement, to: XCUICoordinate) {
        center(from).press(forDuration: 0.4, thenDragTo: to)
    }

    private func center(_ element: XCUIElement) -> XCUICoordinate {
        element.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
    }

    /// The titles down the list, top to bottom, read from its rows: the inspector can show the same words.
    private func titles(_ app: XCUIApplication) -> [String] {
        ["intro", "pad", "note"].map { ($0, cueRow($0, in: app).frame.minY) }
            .sorted { $0.1 < $1.1 }.map(\.0)
    }

    private func assertFadeTarget(_ shown: String, line: UInt = #line) {
        let app = XCUIApplication()
        XCTAssertTrue(
            fadeRow(app).staticTexts[shown].waitForExistence(timeout: 10), "the Fade's Target column shows \(shown)",
            line: line)
    }

    func testDragTheActingCueOntoItsTarget() {
        let app = launchTargets()
        let introTarget = cueRow("intro", in: app).buttons["cue-target-choose"]
        XCTAssertTrue(introTarget.waitForExistence(timeout: 5))
        drag(fadeRow(app).staticTexts["3"], to: center(introTarget))
        assertFadeTarget("1")
        XCTAssertTrue(app.staticTexts["Fade intro"].waitForExistence(timeout: 10), "its default name follows")
        XCTAssertEqual(titles(app), ["intro", "pad", "note"])
        XCTAssertLessThan(
            cueRow("pad", in: app).frame.minY, fadeRow(app).frame.minY, "the Fade stayed below pad")
    }

    func testDragTheTargetOntoTheActingCue() {
        let app = launchTargets()
        let cell = fadeRow(app).staticTexts["?"]
        XCTAssertTrue(cell.waitForExistence(timeout: 5))
        drag(cueRow("intro", in: app).staticTexts["intro"], to: center(cell))
        assertFadeTarget("1")
        XCTAssertEqual(titles(app), ["intro", "pad", "note"])
        XCTAssertLessThan(cueRow("intro", in: app).frame.minY, fadeRow(app).frame.minY)
    }

    func testDropBetweenRowsStillReorders() {
        let app = launchTargets()
        let row = cueRow("intro", in: app)
        XCTAssertTrue(row.waitForExistence(timeout: 5))
        let gap = row.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 1.0))
        drag(fadeRow(app).staticTexts["3"], to: gap)
        let moved = NSPredicate { _, _ in
            MainActor.assumeIsolated { self.fadeRow(app).frame.minY < self.cueRow("pad", in: app).frame.minY }
        }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: moved, object: nil)], timeout: 10), .completed,
            "the Fade moved between intro and pad")
        assertFadeTarget("?")
    }

    /// Typing a number with real keys, the name beside it, then the jump button: the highlight, the
    /// inspector and the standby display all land on the target.
    func testTypeATargetNumberAndJumpToIt() {
        let app = launchTargets()
        fadeRow(app).staticTexts["3"].click()
        let field = app.textFields["inspector-target"]
        XCTAssertTrue(waitFor("isEnabled == true", field), "a Fade's target can be typed")
        let jump = app.buttons["inspector-target-jump"]
        XCTAssertEqual(jump.isEnabled, false, "no target to go to")
        field.click()
        app.typeText("1\r")
        assertFadeTarget("1")
        XCTAssertTrue(waitFor("value == 'intro'", app.staticTexts["inspector-target-note"]), "the name is beside it")
        XCTAssertTrue(waitFor("isEnabled == true", jump))
        // The row reads left to right, nothing on top of anything.
        let row = [
            field.frame, app.staticTexts["inspector-target-note"].frame, app.buttons["inspector-target-clear"].frame,
            jump.frame,
        ]
        for (left, right) in zip(row, row.dropFirst()) { XCTAssertLessThanOrEqual(left.maxX, right.minX) }
        jump.click()
        XCTAssertTrue(waitFor("isSelected == true", cueRow("intro", in: app)), "the target is highlighted")
        XCTAssertTrue(waitFor("value == 'intro'", app.textFields["inspector-name"]), "the inspector shows it")
        XCTAssertTrue(
            waitFor("value == '1 \u{00B7} intro'", app.staticTexts["standby-field"]), "the standby display is on it")
    }

    /// A real double-click on a file cue's Target field opens the chooser; the choosing itself is in
    /// `CueTargetFlowTests.testFileTargetChooser`.
    func testDoubleClickingTheFileFieldOpensTheChooser() {
        let app = launchTargets()
        selectCue("intro", in: app)
        // Not typed in, so accessibility reports it as text and not as a text field.
        let field = app.staticTexts["inspector-target"]
        XCTAssertTrue(waitFor("isHittable == true", field))
        field.doubleClick()
        XCTAssertTrue(app.buttons["Choose"].waitForExistence(timeout: 10), "the file chooser opens")
        // A key, not the Cancel button: the first element of that name is the panel's Touch Bar one.
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", app.buttons["Choose"]), "and closes")
    }
}
