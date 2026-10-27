// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The sidebar's Active Cues tab against the fake engine's demo show: Intro runs and Rain is paused
/// from the start; GO starts "Door slam", and the fake takes a cue to idle when it is panicked.
@MainActor
final class SidebarUITests: XCTestCase {
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

    private func count(_ query: XCUIElementQuery, is n: Int) -> Bool {
        let p = NSPredicate(format: "count == \(n)")
        return XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: p, object: query)], timeout: 10)
            == .completed
    }

    func testRunningCueAppearsInActiveCuesAndItsPanicButtonStopsIt() {
        let app = launch()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.checkBoxes["toggle-sidebar"].click()
        app.segmentedControls["sidebar-tabs"].buttons["Active Cues"].click()
        let panics = app.buttons.matching(identifier: "active-cue-panic")
        XCTAssertTrue(count(panics, is: 2), "Intro (running) and Rain (paused)")

        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: Door slam starts
        XCTAssertTrue(count(panics, is: 3), "the started cue is listed")
        XCTAssertTrue(app.staticTexts["2.1  Door slam"].exists)

        panics.element(boundBy: 2).click()  // Door slam's own panic button
        XCTAssertTrue(count(panics, is: 2), "a panicked cue leaves the panel")
        XCTAssertFalse(app.staticTexts["2.1  Door slam"].exists)
    }
}
