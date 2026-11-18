// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
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
        XCTAssertFalse(app.outlines["cue-list"].staticTexts["a.wav"].exists, "and not its file name")
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
        // The inspector's time fields show milliseconds; the list keeps hundredths.
        let inspector = app.textFields["inspector-duration"]
        XCTAssertTrue(waitFor("value == '01:30.000'", inspector, timeout: 5), "the inspector follows")
    }
}

/// The cue list's group, header and columns while a cue plays and the window changes size, with
/// real keys and presses (QLab 5 manual, the cue list).
@MainActor
final class CueListLayoutUITests: XCTestCase {
    private static let titles = ["Number", "Name", "Target", "Pre-Wait", "Duration", "Post-Wait"]

    /// The demo show, Intro a 30 s tone playing.
    private func launchPlaying() -> XCUIApplication {
        let app = launchApp(opening: ["Demo"]) { copies in
            try writeWAV(copies.appendingPathComponent("Demo/media/a.wav"), seconds: 30)
        }
        XCTAssertTrue(waitFor("isHittable == true", app.staticTexts["Intro"]), "the demo is ready")
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10), "Intro plays")
        return app
    }

    private func header(_ title: String, in app: XCUIApplication) -> XCUIElement {
        app.outlines["cue-list"].buttons[title]
    }

    /// The window dragged by its corner to its least size.
    private func shrinkToLeast(_ app: XCUIApplication) {
        let window = app.windows.firstMatch
        window.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1)).withOffset(CGVector(dx: -2, dy: -2))
            .press(forDuration: 0.2, thenDragTo: window.coordinate(withNormalizedOffset: .zero))
    }

    /// Each cell `row` shows lies under its column's title.
    private func assertUnderTitles(_ row: String, cells: [String: String], in app: XCUIApplication, line: UInt = #line)
    {
        let r = cueRow(row, in: app)
        for (title, value) in cells {
            let cell = r.staticTexts[value].firstMatch
            let column = header(title, in: app)
            XCTAssertTrue(cell.exists && column.exists, "\(row): \(title) \(value)", line: line)
            XCTAssertTrue(
                column.frame.minX - 1 <= cell.frame.minX && cell.frame.maxX <= column.frame.maxX + 1,
                "\(row)'s \(value) \(cell.frame) under \(title) \(column.frame)", line: line)
        }
    }

    func testGroupClosedAndOpenedByTriangleAndArrowsWhileACuePlaysAndTheWindowIsResized() {
        let app = launchPlaying()
        let door = app.staticTexts["Door slam"]
        selectCue("Act One", in: app)
        app.typeKey(XCUIKeyboardKey.leftArrow.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", door), "Left closes the group")
        shrinkToLeast(app)
        app.typeKey(XCUIKeyboardKey.rightArrow.rawValue, modifierFlags: [])
        XCTAssertTrue(door.waitForExistence(timeout: 5), "Right opens it at the least size")
        assertUnderTitles("Door slam", cells: ["Number": "2.1", "Name": "Door slam"], in: app)
        let triangle = cueRow("Act One", in: app).disclosureTriangles.firstMatch
        triangle.click()
        XCTAssertTrue(waitFor("exists == false", door), "the triangle closes it")
        let window = app.windows.firstMatch
        window.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1)).withOffset(CGVector(dx: -2, dy: -2))
            .press(
                forDuration: 0.2,
                thenDragTo: window.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1))
                    .withOffset(CGVector(dx: 300, dy: 100)))
        triangle.click()
        XCTAssertTrue(door.waitForExistence(timeout: 5), "and opens it in the larger window")
        assertUnderTitles("Door slam", cells: ["Number": "2.1", "Name": "Door slam"], in: app)
        assertUnderTitles("Intro", cells: ["Number": "1", "Name": "Intro"], in: app)
        XCTAssertTrue(cueState("Running", of: "Intro", in: app).exists, "Intro played on throughout")
        XCTAssertTrue(cueRow("Act One", in: app).isSelected, "the group stays selected")
    }

    /// At the least window size every column title is shown, in order, side by side inside the
    /// list, and above the inspector.
    func testTheHeaderAtTheLeastWindowSize() {
        let app = launchPlaying()
        shrinkToLeast(app)
        let list = app.outlines["cue-list"]
        let inspector = app.descendants(matching: .any)["inspector-tabs"]
        var right = -CGFloat.infinity
        for title in Self.titles {
            let h = header(title, in: app)
            XCTAssertTrue(
                h.isHittable, "\(title) is shown; headers: \(list.buttons.allElementsBoundByIndex.map(\.label))")
            XCTAssertGreaterThanOrEqual(h.frame.minX, right - 0.5, "\(title) is right of the one before")
            XCTAssertLessThanOrEqual(h.frame.maxX, app.windows.firstMatch.frame.maxX + 0.5, "\(title) is in the window")
            XCTAssertLessThan(h.frame.maxY, inspector.frame.minY, "\(title) is above the inspector")
            right = h.frame.maxX
        }
    }

    /// A column made wider than the window has room for: the list scrolls sideways and every title
    /// stays over its column.
    func testASidewaysScrollKeepsTitlesOverTheirColumns() {
        let app = launchPlaying()
        shrinkToLeast(app)
        let list = app.outlines["cue-list"]
        let pre = header("Pre-Wait", in: app)
        let edge = pre.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 0.5)).withOffset(CGVector(dx: -1, dy: 0))
        edge.press(forDuration: 0.2, thenDragTo: edge.withOffset(CGVector(dx: 600, dy: 0)))
        // AppKit scrolls the list along as the edge is dragged past the list's right edge: back to its start first.
        list.scroll(byDeltaX: 2000, deltaY: 0)
        let before = header("Duration", in: app).frame.minX
        list.scroll(byDeltaX: -400, deltaY: 0)
        XCTAssertLessThan(header("Duration", in: app).frame.minX, before - 50, "the list scrolled sideways")
        assertUnderTitles("Intro", cells: ["Number": "1", "Name": "Intro"], in: app)
        let duration = cueRow("Intro", in: app).staticTexts.matching(NSPredicate(format: "value BEGINSWITH '00:'"))
        XCTAssertGreaterThan(duration.count, 0, "Intro's times are shown")
        let column = header("Duration", in: app).frame
        XCTAssertTrue(
            duration.allElementsBoundByIndex.contains {
                column.minX - 1 <= $0.frame.minX && $0.frame.maxX <= column.maxX + 1
            },
            "a time under Duration \(column): \(duration.allElementsBoundByIndex.map(\.frame))")
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
        let lit = highlighted ?? [title]
        let awaited = lit.contains(title) ? title : lit.first ?? title
        XCTAssertTrue(waitFor("isSelected == true", row(awaited, in: app)), "\(awaited) highlighted", line: line)
        for name in ["One", "Two", "Three"] {
            let on = lit.contains(name)
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
        XCTAssertTrue(app.staticTexts["fade and stop Intro"].waitForExistence(timeout: 10), "the fade follows Intro")
        app.staticTexts["Intro"].doubleClick()
        app.typeText("entrance\r")
        XCTAssertTrue(app.staticTexts["entrance"].waitForExistence(timeout: 10), "the cell shows entrance")
        XCTAssertTrue(
            app.staticTexts["fade and stop entrance"].waitForExistence(timeout: 10), "the fade's name follows")
        XCTAssertFalse(cueState("Running", of: "entrance", in: app).exists, "Return did not fire GO")
    }

    /// Double-clicking a Fade cue's Target cell opens a field over it; a cue number typed there and Return
    /// retargets the Fade: the cell and the inspector show the new target, and Return fires no GO.
    func testDoubleClickTargetRetargetsTheFade() {
        let app = launchDemo()
        let fade = row("fade and stop Intro", in: app)
        XCTAssertTrue(fade.waitForExistence(timeout: 10))
        // Its Target cell shows the target's number, the only "1" in the row (the Fade has no number of its
        // own): the columns move with the window's width, so the cell is found by what it shows.
        let cell = fade.staticTexts["1"].firstMatch
        XCTAssertTrue(cell.waitForExistence(timeout: 10), "the Target cell shows 1")
        cell.doubleClick()
        // As the other cell editors, the field is reached by keyboard focus, not by an accessibility query.
        app.typeText("2.1\r")
        XCTAssertTrue(
            waitFor("value == '2.1'", app.textFields["inspector-target"]), "the inspector shows the new target")
        XCTAssertTrue(app.staticTexts["fade and stop Door slam"].waitForExistence(timeout: 10), "the name follows it")
        XCTAssertFalse(cueStates("Running", in: app).firstMatch.exists, "Return did not fire GO")
    }

    /// T on a file cue opens the file chooser from a real key press. The chooser is modal and takes Esc as
    /// its Cancel: the cue keeps running, and the next Esc is the panic.
    func testTKeyOpensTheFileChooserEscClosesItAndTheNextEscPanics() {
        let app = launchThree()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "One", in: app).waitForExistence(timeout: 10))
        row("One", in: app).click()
        app.typeKey("t", modifierFlags: [])
        let panel = app.dialogs.firstMatch
        XCTAssertTrue(panel.waitForExistence(timeout: 10), "the file chooser opens")
        XCTAssertTrue(cueState("Running", of: "One", in: app).exists, "and One still runs")
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", panel), "Esc closed the chooser")
        XCTAssertTrue(cueState("Running", of: "One", in: app).exists, "and One still runs")
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", cueState("Running", of: "One", in: app)), "the next Esc panicked One")
    }

    /// In Show Mode a double-click edits nothing.
    func testDoubleClickInShowModeDoesNothing() {
        let app = launchThree()
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(
            waitFor("exists == false", app.textFields["inspector-name"], timeout: 5), "Show Mode hides the inspector")
        app.staticTexts["2"].doubleClick()
        XCTAssertFalse(app.textFields["cue-number-editor"].waitForExistence(timeout: 1), "no editor opens")
        XCTAssertTrue(app.staticTexts["2"].exists, "the number is unchanged")
    }

    /// A press in the status column moves the playhead there and leaves the highlight where it was;
    /// the next GO fires that cue.
    func testClickingTheStatusColumn() {
        let app = launchThree()
        // The status column is the row's first 80 points; its images only show state.
        row("Three", in: app).coordinate(withNormalizedOffset: .zero)
            .withOffset(CGVector(dx: 60, dy: 12)).click()
        assertAllOn("Three", highlighted: ["One"], in: app)
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "Three", in: app).waitForExistence(timeout: 10), "GO fires Three")
        XCTAssertFalse(cueState("Running", of: "One", in: app).exists, "and not the highlighted One")
    }

    func testMultiSelection() {
        let app = launchThree()
        XCUIElement.perform(withKeyModifiers: .command) { row("Three", in: app).click() }
        assertAllOn("Three", highlighted: ["One", "Three"], in: app)
    }
}

/// #494's flows that need real mouse and key events (QLab 5 manual pp. 94, 120, 127), on the "Targets" show.
/// Drag-to-target is tested through the cue list on the real engine (`CueTargetFlowTests`): a synthetic drag
/// does not move the pointer on the CI Macs.
@MainActor
final class CueTargetUITests: XCTestCase {
    private func launchTargets() -> XCUIApplication {
        let app = launchApp(opening: ["Targets"])
        XCTAssertTrue(waitFor("isHittable == true", cueRow("intro", in: app)), "the show is ready")
        return app
    }

    /// The unnamed Fade's row: the only one that shows its number, 3.
    private func fadeRow(_ app: XCUIApplication) -> XCUIElement { cueRow("3", in: app) }

    private func assertFadeTarget(_ shown: String, line: UInt = #line) {
        let app = XCUIApplication()
        XCTAssertTrue(
            fadeRow(app).staticTexts[shown].waitForExistence(timeout: 10), "the Fade's Target column shows \(shown)",
            line: line)
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
        // The file is drawn as a path: a double-click anywhere on it opens the chooser.
        let field = app.descendants(matching: .any)["inspector-target-path"]
        XCTAssertTrue(waitFor("isHittable == true", field))
        field.doubleClick()
        XCTAssertTrue(app.buttons["Choose"].waitForExistence(timeout: 10), "the file chooser opens")
        // A key, not the Cancel button: the first element of that name is the panel's Touch Bar one.
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", app.buttons["Choose"]), "and closes")
    }
}

/// The list tools with a real mouse and the real menu bar: a cue-type button dragged into the list,
/// and the window taken full screen (QLab 5, manual pp. 119 and 91).
@MainActor
final class ListToolsUITests: XCTestCase {
    /// The cue-type button labelled `name` in the toolbar above the list (a segment of one of its
    /// segmented controls).
    private func toolbarButton(_ name: String, in app: XCUIApplication) -> XCUIElement {
        let kinds = [XCUIElement.ElementType.radioButton, .button].map(\.rawValue)
        return app.descendants(matching: .any).matching(
            NSPredicate(format: "label == %@ AND elementType IN %@", name, kinds)
        ).firstMatch
    }

    private func selectedIndex(_ rows: XCUIElementQuery) -> Int? {
        (0..<rows.count).first { rows.element(boundBy: $0).isSelected }
    }

    /// Drags `button` onto `row` as a hand would: slowly, resting over the row before the release. The
    /// drag session starts only once the press has moved, and on a slow runner a fast drag was released
    /// before the session had seen the list under it.
    private func drag(_ button: XCUIElement, onto row: XCUIElement) {
        button.press(forDuration: 0.3, thenDragTo: row, withVelocity: .slow, thenHoldForDuration: 0.5)
    }

    func testAToolbarButtonDraggedIntoTheListMakesACueWhereItLandsAndInAnOpenGroupJoinsIt() {
        let app = launchDemo()
        let rows = app.outlines["cue-list"].outlineRows
        // Intro, the Fade, Act One and its three cues, the group open.
        XCTAssertTrue(waitFor("count == 6", rows), "the demo's rows: \(rows.count)")
        let memo = toolbarButton("Memo", in: app)
        XCTAssertTrue(memo.waitForExistence(timeout: 10), "the Memo button")
        // Dropped on Intro's row: the new cue is made just after it, and selected.
        drag(memo, onto: cueRow("Intro", in: app))
        XCTAssertTrue(waitFor("count == 7", rows), "a cue was made: \(rows.count)")
        XCTAssertTrue(waitFor("selected == true", rows.element(boundBy: 1)), "right after Intro")
        XCTAssertTrue(rows.element(boundBy: 0).staticTexts["Intro"].exists, "Intro keeps its place")
        // Dropped on the open group's row: the new cue joins the group, after its last cue.
        let wait = toolbarButton("Wait", in: app)
        drag(wait, onto: cueRow("Act One", in: app))
        XCTAssertTrue(waitFor("count == 8", rows), "a second cue was made: \(rows.count)")
        XCTAssertEqual(selectedIndex(rows), 7, "after Radio, the group's last cue")
        // Closing the group hides it with the group's other cues: it is inside.
        let twisty = cueRow("Act One", in: app).disclosureTriangles.firstMatch
        XCTAssertTrue(twisty.exists)
        twisty.click()
        XCTAssertTrue(waitFor("count == 4", rows), "the group's four cues are hidden: \(rows.count)")
    }

    func testEnterFullScreenTakesTheWorkspaceWindowFullScreenAndBack() {
        let app = launchDemo()
        // The workspace window itself: the first window in the list need not be it while the app comes up.
        let window = app.windows.matching(identifier: "main-window").firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 10))
        let before = window.frame
        app.menuBars.menuBarItems["View"].click()
        let enter = app.menuBars.menuItems["Enter Full Screen"].firstMatch
        XCTAssertTrue(enter.waitForExistence(timeout: 5))
        XCTAssertTrue(enter.isEnabled)
        enter.click()
        // Full screen: the window fills its screen, and the item now takes it back.
        let screen = NSScreen.main?.frame.size ?? .zero
        XCTAssertTrue(
            waitUntil { window.frame.width >= screen.width - 1 && window.frame.height >= screen.height - 1 },
            "the window fills the screen: \(window.frame) of \(screen)")
        // The menu bar hides in full screen; the item's keys take it back.
        app.typeKey("f", modifierFlags: [.command, .shift])
        XCTAssertTrue(
            waitUntil { abs(window.frame.width - before.width) <= 1 },
            "the window is back: \(window.frame), was \(before)")
    }

    /// True once `condition` holds, false after `timeout` seconds; the waiter polls it.
    private func waitUntil(timeout: TimeInterval = 10, _ condition: @escaping @MainActor () -> Bool) -> Bool {
        let met = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in MainActor.assumeIsolated { condition() } }, object: nil)
        return XCTWaiter().wait(for: [met], timeout: timeout) == .completed
    }
}
