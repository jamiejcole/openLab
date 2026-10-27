// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Add, rename, delete and undo against the fake engine, and the same keys doing nothing in Show Mode.
@MainActor
final class EditingUITests: XCTestCase {
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

    func testAddRenameDeleteAndUndo() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()

        app.typeKey("9", modifierFlags: .command)  // New Group Cue (QLab's key)
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        // The new cue is selected and shown in the inspector: its name is empty.
        XCTAssertEqual(name.value as? String ?? "", "")
        name.click()
        name.typeText("Storm\r")
        let storm = app.staticTexts["Storm"]
        XCTAssertTrue(storm.waitForExistence(timeout: 5))

        storm.click()  // focus back in the list
        app.typeKey(XCUIKeyboardKey.delete.rawValue, modifierFlags: .command)
        let gone = NSPredicate(format: "exists == false")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: gone, object: storm)], timeout: 10),
            .completed, "Cmd-Delete removes the selected cue")

        app.typeKey("z", modifierFlags: .command)
        XCTAssertTrue(app.staticTexts["Storm"].waitForExistence(timeout: 10), "undo brings the cue back")
    }

    func testEditingKeysDoNothingInShowMode() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(app.staticTexts["show-mode-indicator"].waitForExistence(timeout: 5))
        app.typeKey("9", modifierFlags: .command)
        app.typeKey(XCUIKeyboardKey.delete.rawValue, modifierFlags: .command)
        app.typeKey("z", modifierFlags: .command)
        XCTAssertFalse(app.staticTexts["Group"].waitForExistence(timeout: 2), "no cue is added in Show Mode")
        XCTAssertTrue(app.staticTexts["Intro"].exists, "nothing was deleted in Show Mode")
    }
}
