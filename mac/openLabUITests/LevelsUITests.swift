// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector's Levels tab (Audio cue) and Fade tab (Fade cue) on the real engine, which
/// applies the cue edits and answers with `model_changed`.
@MainActor
final class LevelsUITests: XCTestCase {
    private func open(_ tab: String, in app: XCUIApplication) {
        tabButton(tab, in: app).click()
    }

    /// The middle of the value box at `row` (0 is the top row: the main level and the cue outputs;
    /// 1 and up are the file channels) and `column` (-1 is the main level's).
    private func cell(_ matrix: XCUIElement, row: Int, column: Int) -> XCUICoordinate {
        let x = column < 0 ? 28 : 56 + CGFloat(column) * 46 + 23
        let y = row == 0 ? 10 : 38 + CGFloat(row - 1) * 20 + 10
        return matrix.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: x, dy: y))
    }

    /// Double-clicks a value box, which opens it for typing, and returns the field laid over it.
    private func openCell(
        _ matrix: XCUIElement, row: Int, column: Int, field id: String, in app: XCUIApplication
    ) -> XCUIElement {
        let field = app.textFields[id]
        for _ in 0..<3 {
            cell(matrix, row: row, column: column).doubleClick()
            if field.waitForExistence(timeout: 4) { return field }
        }
        XCTFail("the value box did not open for typing")
        return field
    }

    private func type(_ text: String, into field: XCUIElement) {
        field.click()
        // Keystrokes sent before the click has made the field first responder are lost on a busy
        // runner, which left the field at its old value.
        XCTAssertTrue(waitFor("hasKeyboardFocus == true", field), "the field takes focus")
        field.typeKey("a", modifierFlags: .command)
        field.typeText(text + "\r")
    }

    func testSettingACueLevelAndACrosspoint() {
        let app = launchDemo()
        selectCue("Intro", in: app)
        open("Levels", in: app)

        let matrix = app.groups["levels-matrix"]
        XCTAssertTrue(matrix.waitForExistence(timeout: 5))
        var field = openCell(matrix, row: 0, column: -1, field: "levels-field", in: app)
        XCTAssertEqual(field.value as? String, "0.0", "the main level")
        type("-6", into: field)
        XCTAssertTrue(waitFor("exists == false", field), "Return closes the box")

        field = openCell(matrix, row: 2, column: 1, field: "levels-field", in: app)  // file channel 2 to output 2
        let label = app.staticTexts["levels-cell"]
        XCTAssertTrue(waitFor("value == 'File channel 2 to output 2'", label), "the crosspoint is the chosen level")
        XCTAssertEqual(field.value as? String, "-inf", "a route that is not listed is silent")
        type("-12", into: field)
        XCTAssertTrue(waitFor("exists == false", field))

        field = openCell(matrix, row: 0, column: -1, field: "levels-field", in: app)
        XCTAssertTrue(waitFor("value == '-6.0'", field), "the main level kept -6 dB")
        field.typeText("\r")
        XCTAssertTrue(waitFor("exists == false", field))
        field = openCell(matrix, row: 2, column: 1, field: "levels-field", in: app)
        XCTAssertTrue(waitFor("value == '-12.0'", field), "and the crosspoint -12 dB")
    }

    func testFadeCueTargetLevel() {
        let app = launchDemo()
        selectCue("Intro", in: app)
        app.typeKey(.downArrow, modifierFlags: [])  // the Fade cue below it
        open("Curve", in: app)

        let curve = app.popUpButtons["fade-curve"]
        XCTAssertTrue(curve.waitForExistence(timeout: 5))
        let duration = app.textFields["fade-duration"]
        XCTAssertEqual(duration.value as? String, "00:05.00")

        open("Levels", in: app)
        let matrix = app.groups["fade-matrix"]
        XCTAssertTrue(matrix.waitForExistence(timeout: 5))
        cell(matrix, row: 0, column: 1).click()  // cue output 2: not faded yet
        let active = app.checkBoxes["fade-active"]
        XCTAssertTrue(waitFor("value == 0", active), "this level is not faded yet")
        var field = openCell(matrix, row: 0, column: 1, field: "fade-field", in: app)
        type("-20", into: field)
        XCTAssertTrue(waitFor("value == 1", active), "typing a level makes the Fade move it")
        field = openCell(matrix, row: 0, column: 1, field: "fade-field", in: app)
        XCTAssertTrue(waitFor("value == '-20.0'", field), "the Fade moves output 2 to -20 dB")
        field.typeText("\r")

        open("Curve", in: app)
        type("3", into: duration)
        XCTAssertTrue(waitFor("value == '00:03.00'", duration), "the duration is stored")
    }
}
