// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector against the fake engine, which applies name edits and sends the `model_changed`:
/// a rename shows in the list only after the command went out and came back.
@MainActor
final class InspectorUITests: XCTestCase {
    func testRenamingACueShowsInTheList() {
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

        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["No cue selected"].exists)
        app.staticTexts["Intro"].click()
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        XCTAssertEqual(name.value as? String, "Intro")
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Thunder\r")

        XCTAssertTrue(app.staticTexts["Thunder"].waitForExistence(timeout: 10), "the list shows the new name")
        XCTAssertFalse(app.staticTexts["Intro"].exists)
    }

    func testGroupSetToPlaylistAndLoop() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)
        let one = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: one, object: windows)], timeout: 10),
            .completed)

        XCTAssertTrue(app.staticTexts["Act One"].waitForExistence(timeout: 10))
        app.staticTexts["Act One"].click()
        let tab = app.tabs["Mode"].exists ? app.tabs["Mode"] : app.radioButtons["Mode"]
        XCTAssertTrue(tab.waitForExistence(timeout: 5))
        tab.click()
        let mode = app.popUpButtons["inspector-group-mode"]
        XCTAssertTrue(mode.waitForExistence(timeout: 5))
        let loop = app.checkBoxes["inspector-group-loop"]
        XCTAssertFalse(loop.isEnabled, "loop is for playlists")
        mode.click()
        app.menuItems["Playlist - one child at a time"].click()
        let enabled = NSPredicate(format: "isEnabled == true")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: enabled, object: loop)], timeout: 10),
            .completed)
        loop.click()
        let on = NSPredicate(format: "value == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: on, object: loop)], timeout: 10),
            .completed)
    }

    func testPreviewButtonMarksTheRowAndClears() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        app.typeKey("w", modifierFlags: .command)
        let one = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: one, object: windows)], timeout: 10),
            .completed)

        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        let button = app.buttons["inspector-preview"]
        XCTAssertTrue(button.waitForExistence(timeout: 5))
        XCTAssertEqual(button.title, "Preview")
        let marks = app.images.matching(identifier: "cue-preview").matching(
            NSPredicate(format: "label == 'Previewing'"))
        XCTAssertEqual(marks.count, 0)

        button.click()
        XCTAssertTrue(marks.firstMatch.waitForExistence(timeout: 10), "the row shows it is previewing")
        XCTAssertEqual(button.title, "Stop Preview")

        button.click()
        let gone = NSPredicate(format: "count == 0")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: gone, object: marks)], timeout: 10),
            .completed)
        XCTAssertEqual(button.title, "Preview")
    }
}
