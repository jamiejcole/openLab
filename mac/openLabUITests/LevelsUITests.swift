// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector's Levels tab (Audio cue) and Fade tab (Fade cue) against the fake engine, which
/// applies the cue edits and answers with `model_changed`.
@MainActor
final class LevelsUITests: XCTestCase {
    private func launch() -> XCUIApplication {
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
        return app
    }

    private func open(_ tab: String, in app: XCUIApplication) {
        let item = app.tabs[tab].exists ? app.tabs[tab] : app.radioButtons[tab]
        XCTAssertTrue(item.waitForExistence(timeout: 5), tab)
        item.click()
    }

    private func wait(_ element: XCUIElement, _ format: String, _ message: String) {
        XCTAssertEqual(
            XCTWaiter().wait(
                for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: format), object: element)], timeout: 10),
            .completed, message)
    }

    /// Clicks the matrix cell at `row` (0 is the cue outputs' row) and `column` (-1 is the master's).
    private func clickCell(_ matrix: XCUIElement, row: Int, column: Int) {
        let x = column < 0 ? 28 : 56 + CGFloat(column) * 46 + 23
        let y = 18 + CGFloat(row) * 20 + 10
        matrix.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: x, dy: y)).click()
    }

    private func type(_ text: String, into field: XCUIElement) {
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeText(text + "\r")
    }

    func testSettingACueLevelAndACrosspoint() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        open("Levels", in: app)

        let field = app.textFields["levels-field"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        XCTAssertEqual(field.value as? String, "0.0", "the cue master")
        type("-6", into: field)
        wait(field, "value == '-6.0'", "the cue master takes -6 dB")

        let matrix = app.groups["levels-matrix"]
        XCTAssertTrue(matrix.waitForExistence(timeout: 5))
        clickCell(matrix, row: 2, column: 1)  // file channel 2 to cue output 2
        let label = app.staticTexts["levels-cell"]
        wait(label, "value == 'File channel 2 to output 2'", "the clicked crosspoint is selected")
        XCTAssertEqual(field.value as? String, "-inf", "a route that is not listed is silent")
        type("-12", into: field)
        wait(field, "value == '-12.0'", "the crosspoint takes -12 dB")

        clickCell(matrix, row: 0, column: -1)
        wait(field, "value == '-6.0'", "the master kept its level")
        clickCell(matrix, row: 2, column: 1)
        wait(field, "value == '-12.0'", "and so did the crosspoint")
    }

    func testFadeCueTargetLevel() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        app.typeKey(.downArrow, modifierFlags: [])  // the Fade cue below it
        open("Fade", in: app)

        let curve = app.popUpButtons["fade-curve"]
        XCTAssertTrue(curve.waitForExistence(timeout: 5))
        let duration = app.textFields["fade-duration"]
        XCTAssertEqual(duration.value as? String, "00:05.00")

        let matrix = app.groups["fade-matrix"]
        XCTAssertTrue(matrix.waitForExistence(timeout: 5))
        clickCell(matrix, row: 0, column: 1)  // cue output 2: not faded yet
        let active = app.checkBoxes["fade-active"]
        wait(active, "value == 0", "this level is not faded yet")
        let field = app.textFields["fade-field"]
        type("-20", into: field)
        wait(field, "value == '-20.0'", "the Fade moves output 2 to -20 dB")
        wait(active, "value == 1", "which makes it active")

        type("3", into: duration)
        wait(duration, "value == '00:03.00'", "the duration is stored")
    }
}
