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

        // The report never takes the keys: GO still reaches the workspace (Door slam starts).
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        app.checkBoxes["toggle-sidebar"].click()
        app.radioButtons["Active Cues"].click()
        XCTAssertTrue(app.staticTexts["2.1  Door slam"].waitForExistence(timeout: 10), "GO ran")
    }
}
