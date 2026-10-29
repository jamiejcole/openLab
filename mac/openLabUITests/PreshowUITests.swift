// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// SM-06: Window > Pre-Show Check runs the check against the fake engine (whose demo show reports
/// "Radio" broken by a missing file) and lists it; clicking the entry selects Radio in the cue
/// list while the workspace window keeps the keys (GO still works).
@MainActor
final class PreshowUITests: XCTestCase {
    private func launch() -> XCUIApplication {
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
        return app
    }

    func testMenuRunsTheCheckAndClickingAnEntrySelectsTheCue() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Pre-Show Check"].click()

        let report = app.descendants(matching: .any).matching(identifier: "preshow-table").firstMatch
        XCTAssertTrue(report.waitForExistence(timeout: 10), "the report opens")
        let summary = app.staticTexts.matching(identifier: "preshow-summary").firstMatch
        let found = NSPredicate(format: "value == '2 problems found.'")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: found, object: summary)], timeout: 10),
            .completed, "the summary counts the problems")
        let radio = report.staticTexts.matching(NSPredicate(format: "label BEGINSWITH '2.3  Radio'")).firstMatch
        XCTAssertTrue(radio.waitForExistence(timeout: 10), "the broken cue is listed")

        radio.click()
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        let one = NSPredicate(format: "count == 1")
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: one, object: selected)], timeout: 10),
            .completed, "the cue is selected in the list")
        XCTAssertTrue(selected.firstMatch.staticTexts["Radio"].exists)

        // The report never takes the keys, even after a click on the list's empty area (an
        // NSTableView hit makes an ordinary panel key): GO still reaches the workspace.
        report.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.95)).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        app.checkBoxes["toggle-sidebar"].click()
        app.radioButtons["Active Cues"].click()
        XCTAssertTrue(app.staticTexts["2.1  Door slam"].waitForExistence(timeout: 10), "GO ran")
    }

    /// Two workspaces: with "Second" holding the keys, choosing an entry in the demo workspace's
    /// report makes the demo window key, so Space fires GO there and not in the other workspace.
    func testChoosingAnEntryMakesItsWorkspaceKey() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        let demo = app.windows.matching(NSPredicate(format: "identifier == 'main-window' AND title == 'Test'"))
            .firstMatch
        XCTAssertTrue(demo.waitForExistence(timeout: 10))
        XCTAssertTrue(
            app.windows.matching(NSPredicate(format: "title == 'Second'")).firstMatch.waitForExistence(timeout: 10))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Test"].click()
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
        demo.checkBoxes["toggle-sidebar"].click()
        demo.radioButtons["Active Cues"].click()
        XCTAssertTrue(demo.staticTexts["2.1  Door slam"].waitForExistence(timeout: 10), "GO ran in the demo workspace")
    }
}
