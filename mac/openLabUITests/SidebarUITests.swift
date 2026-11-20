// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The sidebar on the real engine's demo show: GO starts Intro, which is listed in Active Cues
/// until its panic button fades it out; the Warnings list is on the status window.
@MainActor
final class SidebarUITests: XCTestCase {
    func testRunningCueAppearsInActiveCuesAndItsPanicButtonStopsIt() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        tabButton("0 Active Cues", in: app).click()  // the tab counts them
        let panics = app.buttons.matching(identifier: "active-cue-panic")
        XCTAssertEqual(panics.count, 0, "nothing plays when the show opens")

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: Intro starts
        XCTAssertTrue(waitFor("count == 1", panics), "the started cue is listed")
        XCTAssertTrue(app.staticTexts["1  Intro"].exists)

        panics.element(boundBy: 0).click()  // Intro's own panic button: it fades out and stops
        XCTAssertTrue(waitFor("count == 0", panics), "a panicked cue leaves the panel")
        XCTAssertFalse(app.staticTexts["1  Intro"].exists)
        XCTAssertEqual(cueStates("Running", in: app).count, 0, "and it stopped")
    }

    /// CL-01: New List adds a list and shows it; there is no New Cart button (carts are not built).
    func testNewListAddsAListAndThereIsNoNewCart() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        XCTAssertFalse(app.buttons["New Cart"].exists)
        app.buttons["new-list-button"].click()
        let intro = app.staticTexts["Intro"]
        XCTAssertTrue(waitFor("exists == false", intro), "the new empty list is shown in place of the demo cues")
    }

    /// In Show Mode New List is off (QLab 5 manual, Show Mode); Edit Mode gives it back.
    func testShowModeLocksNewList() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        let newList = app.buttons["new-list-button"]
        XCTAssertTrue(waitFor("isEnabled == true", newList))
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(waitFor("isEnabled == false", newList), "New List is off in Show Mode")
        newList.click()
        let two = app.descendants(matching: .any).matching(
            NSPredicate(format: "label == '2 Cue Lists' OR title == '2 Cue Lists'")
        ).firstMatch
        XCTAssertFalse(two.waitForExistence(timeout: 2), "no list was added")
        XCTAssertTrue(tabButton("1 Cue List", in: app).exists)
        app.radioGroups["mode-switch"].radioButtons["Edit"].click()
        XCTAssertTrue(waitFor("isEnabled == true", newList), "and on again in Edit Mode")
    }
}
