// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The sidebar on the real engine's demo show: GO starts Intro, which is listed in Active Cues
/// until its panic button fades it out; the broken cues are listed in Warnings.
@MainActor
final class SidebarUITests: XCTestCase {
    func testRunningCueAppearsInActiveCuesAndItsPanicButtonStopsIt() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        tabButton("Active Cues", in: app).click()
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

    /// The demo show's "Radio" cue is broken by its missing file: it is listed in Warnings with why,
    /// and choosing the row selects the cue in the list.
    func testBrokenCueAppearsInWarningsAndClickingItSelectsTheCue() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        tabButton("Warnings", in: app).click()
        let warnings = app.descendants(matching: .any).matching(identifier: "sidebar-warnings").firstMatch
        XCTAssertTrue(warnings.waitForExistence(timeout: 10))
        let row = warnings.staticTexts["2.3  Radio"]
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the broken cue is listed")
        XCTAssertTrue(
            warnings.staticTexts.matching(NSPredicate(format: "value BEGINSWITH 'Media file not found'")).firstMatch
                .exists, "with its reason")
        XCTAssertTrue(app.staticTexts["warnings-count"].exists, "the count is shown")
        row.click()
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        XCTAssertTrue(waitFor("count == 1", selected), "the cue is selected in the list")
        XCTAssertTrue(selected.firstMatch.staticTexts["Radio"].exists)
    }

    /// CL-01: New List adds a list and shows it; New Cart is off (carts are not in v0.0.1).
    func testNewListAddsAListAndNewCartIsDisabled() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        XCTAssertFalse(app.buttons["new-cart-button"].isEnabled)
        app.buttons["new-list-button"].click()
        let intro = app.staticTexts["Intro"]
        XCTAssertTrue(waitFor("exists == false", intro), "the new empty list is shown in place of the demo cues")
    }
}
