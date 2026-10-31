// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector against the fake engine, which applies name edits and sends the `model_changed`:
/// a rename shows in the list only after the command went out and came back.
@MainActor
final class InspectorUITests: XCTestCase {
    func testRenamingACueShowsInTheList() {
        let app = launchDemo()

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
        let app = launchDemo()

        selectCue("Act One", in: app)
        tabButton("Mode", in: app).click()
        let mode = app.popUpButtons["inspector-group-mode"]
        XCTAssertTrue(mode.waitForExistence(timeout: 5))
        let loop = app.checkBoxes["inspector-group-loop"]
        XCTAssertFalse(loop.isEnabled, "loop is for playlists")
        // A popup opened while another process holds focus can fail to show its
        // menu; close it and open it once more before failing.
        let playlist = app.menuItems["Playlist - one child at a time"]
        mode.click()
        if !playlist.waitForExistence(timeout: 3) {
            app.typeKey(.escape, modifierFlags: [])
            mode.click()
        }
        XCTAssertTrue(playlist.waitForExistence(timeout: 5))
        playlist.click()
        XCTAssertTrue(waitFor("isEnabled == true", loop))
        loop.click()
        XCTAssertTrue(waitFor("value == 1", loop))
    }

    func testPreviewButtonMarksTheRowAndClears() {
        let app = launchDemo()

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
        XCTAssertTrue(waitFor("count == 0", marks))
        XCTAssertEqual(button.title, "Preview")
    }
}
