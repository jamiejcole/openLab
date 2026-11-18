// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The sidebar on the real engine's demo show: GO starts Intro, which is listed in Active Cues
/// until its panic button fades it out; the broken cues are listed in Warnings.
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

    /// The Warnings list while the audio device is away: Intro, playing when it went, is listed
    /// with its long reason wrapped inside the list; the device is one entry; five GOs on a cue that
    /// cannot play are one entry for that cue. In Show Mode New List is off (QLab 5 manual, Show Mode).
    func testWarningsCoalesceLongLinesWrapAndShowModeLocksNewList() async throws {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: Intro
        XCTAssertTrue(cueState("Running", of: "Intro", in: app).waitForExistence(timeout: 10), "Intro plays")
        await engineRequest(["type": "lose_device"])
        XCTAssertTrue(cueState("Paused", of: "Intro", in: app).waitForExistence(timeout: 10), "Intro shows paused")
        for _ in 0..<5 {
            cueRow("Door slam", in: app).click()
            app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        }
        tabButton("Warnings", in: app).click()
        let warnings = app.descendants(matching: .any).matching(identifier: "sidebar-warnings").firstMatch
        XCTAssertTrue(warnings.waitForExistence(timeout: 10))

        let device = warnings.staticTexts.matching(
            NSPredicate(format: "value == %@", "Audio device \u{201C}openLab Virtual Output\u{201D}"))
        XCTAssertTrue(waitFor("count == 1", device), "the device is one entry")
        let door = warnings.staticTexts.matching(NSPredicate(format: "value == '2.1  Door slam'"))
        XCTAssertTrue(waitFor("count == 1", door), "Door slam is one entry")
        let refused = warnings.staticTexts.matching(NSPredicate(format: "value CONTAINS 'Asked to start 5 times'"))
        XCTAssertTrue(waitFor("count == 1", refused), "its five refusals counted on it")

        // Intro's entry: its title, and under it why it shows paused (its device is not connected).
        let title = warnings.staticTexts["1  Intro"]
        XCTAssertTrue(title.waitForExistence(timeout: 10), "Intro is listed")
        let texts = warnings.staticTexts.allElementsBoundByIndex
        let at = try XCTUnwrap(texts.firstIndex { $0.value as? String == "1  Intro" })
        let reason = texts[at + 1]
        XCTAssertTrue(text(reason).contains("not connected"), "with its reason: \(text(reason))")
        XCTAssertGreaterThan(reason.frame.height, title.frame.height * 2, "the long reason wraps: \(reason.frame)")
        XCTAssertLessThanOrEqual(reason.frame.maxX, warnings.frame.maxX + 0.5, "inside the list")
        XCTAssertGreaterThanOrEqual(reason.frame.minY, title.frame.maxY - 0.5, "below its title, not over it")

        // Show Mode: New List is off and adds nothing; Edit Mode gives it back.
        tabButton("1 Cue List", in: app).click()
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
