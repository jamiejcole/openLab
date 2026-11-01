// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Add, rename, delete and undo on the real engine, and the same keys doing nothing in Show Mode.
@MainActor
final class EditingUITests: XCTestCase {
    func testAddRenameDeleteAndUndo() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()

        app.typeKey("0", modifierFlags: .command)  // New Group Cue (QLab's key)
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
        XCTAssertTrue(waitFor("exists == false", storm), "Cmd-Delete removes the selected cue")

        app.typeKey("z", modifierFlags: .command)
        XCTAssertTrue(app.staticTexts["Storm"].waitForExistence(timeout: 10), "undo brings the cue back")
    }

    func testCopyAndPasteDuplicatesACue() {
        let app = launchDemo()
        // Rows of the list only: the standby display above it names the playhead's cue too.
        let intro = app.outlines.matching(identifier: "cue-list").outlineRows.containing(
            NSPredicate(format: "label == 'Intro' OR value == 'Intro'"))
        XCTAssertTrue(waitFor("count == 1", intro))
        app.staticTexts["Intro"].firstMatch.click()
        app.typeKey("c", modifierFlags: .command)
        let copied = NSPredicate { _, _ in NSPasteboard.general.types?.contains(.init("dev.openlab.cue-data")) == true }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: copied, object: nil)], timeout: 10), .completed)
        app.typeKey("v", modifierFlags: .command)
        XCTAssertTrue(waitFor("count == 2", intro), "the pasted copy shows in the list")
    }

    func testEditingKeysDoNothingInShowMode() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(app.staticTexts["show-mode-indicator"].waitForExistence(timeout: 5))
        app.typeKey("0", modifierFlags: .command)
        app.typeKey(XCUIKeyboardKey.delete.rawValue, modifierFlags: .command)
        app.typeKey("z", modifierFlags: .command)
        XCTAssertFalse(app.staticTexts["Group"].waitForExistence(timeout: 2), "no cue is added in Show Mode")
        XCTAssertTrue(app.staticTexts["Intro"].exists, "nothing was deleted in Show Mode")
    }
}
