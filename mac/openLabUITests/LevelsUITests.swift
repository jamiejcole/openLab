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

/// The patch editor's device pop-up with real presses, on the test engine's two virtual outputs
/// (QLab 5 manual, Audio settings and the patch editor).
@MainActor
final class PatchEditorUITests: XCTestCase {
    private let first = "openLab Virtual Output"
    private let second = "openLab Virtual Output 2"

    /// Picks `title` in `popup`'s menu.
    private func choose(_ title: String, in popup: XCUIElement, app: XCUIApplication, line: UInt = #line) {
        let item = app.menuItems[title]
        popup.click()
        // A pop-up opened while another process holds focus can fail to show its menu: once more.
        if !item.waitForExistence(timeout: 3) {
            app.typeKey(.escape, modifierFlags: [])
            popup.click()
        }
        XCTAssertTrue(item.waitForExistence(timeout: 5), "\(title) is offered", line: line)
        item.click()
        XCTAssertTrue(waitFor("value == '\(title)'", popup), "\(title) is shown", line: line)
    }

    func testPatchEditorDeviceChosenWhileACuePlaysTheSystemOutputAndALossAndReturn() async {
        let app = launchDemo()
        await engineRequest(["type": "restore_device", "device": "openlab:virtual-2"])  // plugged in
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: Intro, a loop
        let playing = cueState("Running", of: "Intro", in: app)
        XCTAssertTrue(playing.waitForExistence(timeout: 10), "Intro plays")

        app.buttons["settings-button"].click()
        tabButton("Audio", in: app).click()
        let edit = app.buttons["settings-patch-edit-1"]
        XCTAssertTrue(edit.waitForExistence(timeout: 10), "the patch table")
        edit.click()
        let editor = app.windows["patch-editor-window"]
        XCTAssertTrue(editor.waitForExistence(timeout: 10), "the patch editor opens")
        let device = editor.popUpButtons["patch-editor-device"]
        XCTAssertTrue(waitFor("value == '\(first)'", device), "on the first output")

        // Another device while Intro plays: the editor shows it, and Intro plays on.
        choose(second, in: device, app: app)
        XCTAssertTrue(playing.exists, "Intro plays on")
        XCTAssertFalse(cueState("Broken", of: "Intro", in: app).exists, "and is not broken")

        // The system output names the Mac's output device, and follows it when it changes.
        choose("System output (\(first))", in: device, app: app)
        await engineRequest(["type": "set_default_device", "device": "openlab:virtual-2"])
        XCTAssertTrue(
            waitFor("value == 'System output (\(second))'", device), "the system output follows the Mac's choice")
        XCTAssertTrue(playing.exists, "Intro plays on")

        // The patch's device lost and back: the editor says it is not connected, then shows it again.
        choose(first, in: device, app: app)
        await engineRequest(["type": "lose_device", "device": "openlab:virtual"])
        XCTAssertTrue(waitFor("value == '\(first) (not connected)'", device), "the editor shows the device gone")
        let problem = editor.staticTexts["patch-editor-problem"]
        XCTAssertTrue(waitFor("value CONTAINS 'not connected'", problem), "and why: \(text(problem))")
        await engineRequest(["type": "restore_device", "device": "openlab:virtual"])
        XCTAssertTrue(waitFor("value == '\(first)'", device), "the device is back")
        XCTAssertTrue(waitFor("exists == false", problem), "and the problem line goes")
    }
}
