// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// SM-06: Window > Pre-Show Check runs the engine's check on the demo show (whose "Radio" cue is
/// broken by a missing file) and lists it; clicking the entry selects Radio in the cue list while
/// the workspace window keeps the keys (GO still works).
@MainActor
final class PreshowUITests: XCTestCase {
    func testMenuRunsTheCheckAndClickingAnEntrySelectsTheCue() {
        let app = launchDemo()

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Pre-Show Check"].click()

        let report = app.descendants(matching: .any).matching(identifier: "preshow-table").firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 10), "the report opens")
        let summary = app.staticTexts.matching(identifier: "preshow-summary").firstMatch
        XCTAssertTrue(waitFor("value ENDSWITH 'problems found.'", summary), "the summary counts the problems")
        let radio = report.staticTexts.matching(NSPredicate(format: "label BEGINSWITH '2.3  Radio'")).firstMatch
        XCTAssertTrue(radio.waitForExistence(timeout: 10), "the broken cue is listed")

        radio.click()
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        XCTAssertTrue(waitFor("count == 1", selected), "the cue is selected in the list")
        XCTAssertTrue(selected.firstMatch.staticTexts["Radio"].exists)

        // The report never takes the keys, even after a click on the list's empty area (an
        // NSTableView hit makes an ordinary panel key): GO still reaches the workspace.
        report.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueStates("Running", in: app).firstMatch.waitForExistence(timeout: 10), "GO ran")
    }

    /// Two workspaces: with "Second" holding the keys, choosing an entry in the demo workspace's
    /// report makes the demo window key, so Space fires GO there and not in the other workspace.
    func testChoosingAnEntryMakesItsWorkspaceKey() {
        let app = launchApp()
        let demo = app.windows.matching(NSPredicate(format: "identifier == 'main-window' AND title == 'Demo'"))
            .firstMatch
        XCTAssertTrue(demo.waitForExistence(timeout: 10))
        XCTAssertTrue(
            app.windows.matching(NSPredicate(format: "title == 'Second'")).firstMatch.waitForExistence(timeout: 10))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Demo"].click()
        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Pre-Show Check"].click()
        let report = app.descendants(matching: .any).matching(identifier: "preshow-table").firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 10))
        let radio = report.staticTexts.matching(NSPredicate(format: "label BEGINSWITH '2.3  Radio'")).firstMatch
        XCTAssertTrue(radio.waitForExistence(timeout: 10))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Second"].click()  // the other workspace takes the keys
        radio.click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(
            demo.images.matching(identifier: "cue-state").matching(NSPredicate(format: "label == 'Running'")).firstMatch
                .waitForExistence(timeout: 10), "GO ran in the demo workspace")
    }
}
