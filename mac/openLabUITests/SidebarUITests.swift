// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The sidebar's Active Cues tab against the fake engine's demo show: Intro runs and Rain is paused
/// from the start; GO starts "Door slam", and the fake takes a cue to idle when it is panicked.
@MainActor
final class SidebarUITests: XCTestCase {
    func testRunningCueAppearsInActiveCuesAndItsPanicButtonStopsIt() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.checkBoxes["toggle-sidebar"].click()
        tabButton("Active Cues", in: app).click()
        let panics = app.buttons.matching(identifier: "active-cue-panic")
        XCTAssertTrue(waitFor("count == 2", panics), "Intro (running) and Rain (paused)")

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: Door slam starts
        XCTAssertTrue(waitFor("count == 3", panics), "the started cue is listed")
        XCTAssertTrue(app.staticTexts["2.1  Door slam"].exists)

        panics.element(boundBy: 2).click()  // Door slam's own panic button
        XCTAssertTrue(waitFor("count == 2", panics), "a panicked cue leaves the panel")
        XCTAssertFalse(app.staticTexts["2.1  Door slam"].exists)
    }

    /// The demo show's "Radio" cue is broken by a lost device: it is listed in Warnings with why,
    /// and choosing the row selects the cue in the list.
    func testBrokenCueAppearsInWarningsAndClickingItSelectsTheCue() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.checkBoxes["toggle-sidebar"].click()
        app.radioButtons["Warnings"].click()
        let warnings = app.descendants(matching: .any).matching(identifier: "sidebar-warnings").firstMatch
        XCTAssertTrue(warnings.waitForExistence(timeout: 10))
        let row = warnings.staticTexts["2.3  Radio"]
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the broken cue is listed")
        XCTAssertTrue(
            warnings.staticTexts["The audio device was lost while it was playing."].exists, "with its reason")
        XCTAssertTrue(app.staticTexts["warnings-count"].exists, "the count is shown")
        row.click()
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        XCTAssertTrue(waitFor("count == 1", selected), "the cue is selected in the list")
        XCTAssertTrue(selected.firstMatch.staticTexts["Radio"].exists)
    }

    /// CL-01: New List adds a list and shows it; New Cart is off (carts are not in v0.0.1).
    func testNewListAddsAListAndNewCartIsDisabled() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.checkBoxes["toggle-sidebar"].click()
        XCTAssertFalse(app.buttons["new-cart-button"].isEnabled)
        app.buttons["new-list-button"].click()
        let intro = app.staticTexts["Intro"]
        XCTAssertTrue(waitFor("exists == false", intro), "the new empty list is shown in place of the demo cues")
    }
}
