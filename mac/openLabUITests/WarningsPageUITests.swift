// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Window > Workspace Status opens on its Warnings page, which lists the demo show's broken cue (its
/// "Radio" cue has a missing file); clicking the entry selects Radio in the cue list and gives the
/// workspace window the keys, where GO works. While the status window is key GO does nothing.
@MainActor
final class WarningsPageUITests: XCTestCase {
    func testTheMenuOpensTheListAndClickingAnEntrySelectsTheCue() {
        let app = launchDemo()

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Workspace Status"].click()

        let report = app.descendants(matching: .any).matching(identifier: "warnings-table").firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 10), "the list opens")
        let summary = app.staticTexts.matching(identifier: "warnings-summary").firstMatch
        XCTAssertTrue(waitFor("value == '1 warning'", summary), "the summary counts the one warning")
        let radio = report.staticTexts["2.3  Radio"]
        XCTAssertTrue(radio.waitForExistence(timeout: 10), "the broken cue is listed")

        radio.click()
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        XCTAssertTrue(waitFor("count == 1", selected), "the cue is selected in the list")
        XCTAssertTrue(selected.firstMatch.staticTexts["Radio"].exists)

        // A click on the list's empty area makes the status window key: Space fires no GO there.
        report.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertFalse(
            cueStates("Running", in: app).firstMatch.waitForExistence(timeout: 2), "no GO with the status window key")

        // A click back on the workspace (its title bar, clear of the status window) gives GO back.
        app.windows.matching(identifier: "main-window").firstMatch
            .coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0)).withOffset(CGVector(dx: 0, dy: 10)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueStates("Running", in: app).firstMatch.waitForExistence(timeout: 10), "GO ran")
    }

    /// The footer's warnings button opens the same page, and the Logs page is one of its tabs.
    func testTheFooterButtonOpensTheWarningsPageAndLogsIsATab() {
        let app = launchDemo()
        app.buttons["footer-warnings"].click()
        let list = app.descendants(matching: .any).matching(identifier: "warnings-table").firstMatch
        XCTAssertTrue(list.waitForExistence(timeout: 10), "the Warnings page opens")
        tabButton("Logs", in: app).click()
        let log = app.tables.matching(identifier: "log-table").firstMatch
        XCTAssertTrue(log.waitForExistence(timeout: 10), "the Logs page is a tab of the same window")
    }

    /// Two workspaces: with "Second" holding the keys, choosing an entry in the demo workspace's
    /// report makes the demo window key, so Space fires GO there and not in the other workspace.
    func testChoosingAnEntryMakesItsWorkspaceKey() {
        let app = launchApp()
        // A workspace window's title (and its Window menu item) names the workspace, then its list.
        let demoTitle = NSPredicate(format: "title BEGINSWITH 'Demo \u{2014} '")
        let secondTitle = NSPredicate(format: "title BEGINSWITH 'Second \u{2014} '")
        let demo = app.windows.matching(identifier: "main-window").matching(demoTitle).firstMatch
        XCTAssertTrue(demo.waitForExistence(timeout: 10))
        XCTAssertTrue(app.windows.matching(secondTitle).firstMatch.waitForExistence(timeout: 10))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems.matching(demoTitle).firstMatch.click()
        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Workspace Status"].click()
        let report = app.descendants(matching: .any).matching(identifier: "warnings-table").firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 10))
        let radio = report.staticTexts["2.3  Radio"]
        XCTAssertTrue(radio.waitForExistence(timeout: 10))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems.matching(secondTitle).firstMatch.click()  // the other workspace takes the keys
        radio.click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(
            demo.images.matching(identifier: "cue-state").matching(NSPredicate(format: "label == 'Running'")).firstMatch
                .waitForExistence(timeout: 10), "GO ran in the demo workspace")
    }
}
