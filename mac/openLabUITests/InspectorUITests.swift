// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector on the real engine, which applies name edits and sends the `model_changed`:
/// a rename shows in the list only after the command went out and came back.
@MainActor
final class InspectorUITests: XCTestCase {
    func testRenamingACueShowsInTheList() {
        let app = launchDemo()

        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        XCTAssertTrue(
            waitFor("value == 'Intro'", app.textFields["inspector-name"]),
            "the playhead cue is in the inspector at open")
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

/// The Time & Loops tab's waveform with real presses (QLab 5 manual, Audio cues, the waveform):
/// Door slam of the demo show plays a 30 s tone once.
@MainActor
final class TimeLoopsUITests: XCTestCase {
    /// The point `seconds` into the 30 s file across the waveform, at the height of its lanes (the
    /// ruler is the view's top 18 points).
    private func point(_ seconds: Double, on wave: XCUIElement) -> XCUICoordinate {
        wave.coordinate(withNormalizedOffset: .zero).withOffset(
            CGVector(dx: wave.frame.width * seconds / 30, dy: 48))
    }

    /// The remaining time Active Cues shows for the one cue that plays ("00:01.20  -00:13.80").
    private func remaining(in app: XCUIApplication) -> Double? {
        let times = app.staticTexts.matching(identifier: "active-cue-times").firstMatch
        guard let part = text(times).components(separatedBy: "-").last else { return nil }
        return seconds(part)
    }

    func testWaveformClickMovesThePlaybackPositionDragSelectsASectionAndAHandleDragTrims() throws {
        let app = launchApp(opening: ["Demo"]) { copies in
            try writeWAV(copies.appendingPathComponent("Demo/media/d.wav"), seconds: 30)
        }
        selectCue("Door slam", in: app)
        tabButton("Timing & Loops", in: app).click()
        let wave = app.descendants(matching: .any)["time-loops-waveform"]
        XCTAssertTrue(waitFor("isHittable == true", wave), "the waveform is shown")
        let start = app.textFields["time-loops-start"]
        let end = app.textFields["time-loops-end"]
        XCTAssertTrue(waitForTime(end, near: 30, tolerance: 0.01), "the whole file: \(text(end))")
        XCTAssertTrue(waitForTime(start, near: 0, tolerance: 0.01))

        // A press on the start handle dragged right trims the start; the playback does not move.
        point(0, on: wave).withOffset(CGVector(dx: 1, dy: 0)).press(
            forDuration: 0.2, thenDragTo: point(3, on: wave))
        XCTAssertTrue(waitForTime(start, near: 3, tolerance: 0.6), "the start is trimmed: \(text(start))")
        XCTAssertFalse(cueState("Loaded", of: "Door slam", in: app).exists, "a handle drag loads nothing")

        // A click moves the playback position there: the cue is loaded at it, and GO plays from it.
        point(15, on: wave).click()
        XCTAssertTrue(
            cueState("Loaded", of: "Door slam", in: app).waitForExistence(timeout: 10), "loaded at the click")
        app.checkBoxes["toggle-sidebar"].click()
        tabButton("0 Active Cues", in: app).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO, from the waveform's focus
        XCTAssertTrue(cueState("Running", of: "Door slam", in: app).waitForExistence(timeout: 10), "GO plays it")
        let left = try XCTUnwrap(remaining(in: app), "the remaining time is shown")
        XCTAssertEqual(left, 15, accuracy: 2.5, "played from about 15 s of 30, not from the start")
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", cueState("Running", of: "Door slam", in: app)), "stopped")
        // GO moved the selection on with the playhead (to Rain): Door slam is shown again.
        selectCue("Door slam", in: app)
        tabButton("Timing & Loops", in: app).click()
        XCTAssertTrue(waitForTime(end, near: 30, tolerance: 0.01), "Door slam's waveform: \(text(end))")

        // A drag across the waveform selects a section and moves nothing; Shift-I and Shift-O crop to it.
        point(18, on: wave).press(forDuration: 0.2, thenDragTo: point(24, on: wave))
        XCTAssertFalse(
            cueState("Loaded", of: "Door slam", in: app).waitForExistence(timeout: 1), "a drag loads nothing")
        XCTAssertTrue(waitForTime(start, near: 3, tolerance: 0.6), "and trims nothing")
        app.typeKey("i", modifierFlags: .shift)
        app.typeKey("o", modifierFlags: .shift)
        XCTAssertTrue(waitForTime(start, near: 18, tolerance: 0.6), "cropped to the section: \(text(start))")
        XCTAssertTrue(waitForTime(end, near: 24, tolerance: 0.6), "cropped to the section: \(text(end))")
    }
}
