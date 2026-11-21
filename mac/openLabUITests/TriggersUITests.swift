// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Record a hotkey on a cue in the inspector, then press it: the engine receives the `hotkey`,
/// matches it to that cue and starts it.
@MainActor
final class TriggersUITests: XCTestCase {
    func testRecordedHotkeyStartsTheCue() {
        let app = launchDemo()
        XCTAssertEqual(cueStates("Running", in: app).count, 0)

        selectCue("Rain", in: app)
        tabButton("Triggers", in: app).click()
        let hotkey = app.staticTexts["inspector-hotkey"]
        XCTAssertTrue(hotkey.waitForExistence(timeout: 5))
        hotkey.click()
        hotkey.typeKey("j", modifierFlags: [])
        XCTAssertTrue(waitFor("value == 'J'", hotkey), "the recorded key shows once the engine answered")

        app.typeKey("j", modifierFlags: [])
        XCTAssertTrue(cueState("Running", of: "Rain", in: app).waitForExistence(timeout: 10), "the hotkey started Rain")
    }

    /// Flow 8 of #784: the time-of-day time is reached and typed from the keyboard alone. With
    /// the fade-out box ticked its time field is the tab's other field: Tab from it reaches the
    /// time-of-day field within a few presses; "10:30" typed and Return is read as 00:10:30 and
    /// kept when the cue is shown again. The box and pop-ups take the keyboard as every AppKit
    /// control does with the system's keyboard navigation on.
    func testTheTimeOfDayFieldIsReachedAndTypedFromTheKeyboard() {
        let app = launchDemo()
        selectCue("Rain", in: app)
        tabButton("Triggers", in: app).click()
        let time = app.textFields["inspector-wall-clock-time"]
        XCTAssertTrue(time.waitForExistence(timeout: 5))
        XCTAssertTrue(time.isHittable, "the field is on screen, not clipped away")
        app.checkBoxes["inspector-fade-others"].click()
        let fadeTime = app.textFields["inspector-fade-others-time"]
        XCTAssertTrue(waitFor("isEnabled == true", fadeTime))
        fadeTime.click()
        XCTAssertTrue(waitFor("hasKeyboardFocus == true", fadeTime))
        var reached = false
        for _ in 0..<8 where !reached {
            app.typeKey(XCUIKeyboardKey.tab.rawValue, modifierFlags: [])
            reached = waitFor("hasKeyboardFocus == true", time, timeout: 1)
        }
        XCTAssertTrue(reached, "Tab reaches the time-of-day field")
        app.typeKey("a", modifierFlags: [.command])
        app.typeText("10:30\r")
        XCTAssertTrue(waitFor("value == '00:10:30'", time), "read from the right: 00:10:30")
        selectCue("Door slam", in: app)
        selectCue("Rain", in: app)
        tabButton("Triggers", in: app).click()
        XCTAssertTrue(waitFor("value == '00:10:30'", time), "kept")
    }
}
