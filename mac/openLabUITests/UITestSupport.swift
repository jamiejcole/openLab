// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Launching the app on the fake engine and waiting on the UI, shared by every UI test.
extension XCTestCase {
    /// The app on the fake engine, which opens its demo workspace and an empty second one.
    @MainActor func launchFake(_ arguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"] + arguments
        app.launch()
        return app
    }

    /// The fake engine's demo workspace alone: the empty second one opens in front and is closed
    /// (nothing plays in it, so it closes without asking).
    @MainActor func launchDemo() -> XCUIApplication {
        let app = launchFake()
        let windows = app.windows.matching(identifier: "main-window")
        XCTAssertTrue(windows.element(boundBy: 1).waitForExistence(timeout: 10))
        // Cmd-W closes the key window: wait until the front window is the empty one (the demo's
        // cue is not in it) and the demo, behind it, has loaded its list: a demo window that has not
        // loaded yet also lacks the cue, and Cmd-W would close it instead.
        let emptyInFront = NSPredicate { _, _ in
            let front = windows.element(boundBy: 0)
            return front.exists && !front.staticTexts["Intro"].exists
                && windows.element(boundBy: 1).staticTexts["Intro"].exists
        }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: emptyInFront, object: nil)], timeout: 10),
            .completed, "the empty workspace is in front")
        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(waitFor("count == 1", windows), "the empty workspace closes")
        XCTAssertTrue(waitFor("isHittable == true", app.staticTexts["Intro"]), "the demo is ready")
        return app
    }

    /// True once `object` matches the predicate `format`, false after `timeout` seconds.
    @MainActor func waitFor(_ format: String, _ object: Any?, timeout: TimeInterval = 10) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: format), object: object)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }

    /// Selects the cue row titled `title` and waits until the list shows it selected. A click that
    /// lands while the list is still laying out selects nothing, which left the inspector without
    /// its tabs; the click is repeated until the selection shows.
    @MainActor func selectCue(_ title: String, in app: XCUIApplication) {
        let row = app.staticTexts[title]
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the cue \(title) is listed")
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        for _ in 0..<3 {
            row.click()
            if waitFor("count == 1", selected, timeout: 4) { return }
        }
        XCTFail("the cue \(title) does not get selected")
    }

    /// The inspector or sidebar tab titled `title`, once it has laid out: the tab bar is a segmented
    /// control (radio buttons) or a tab view (tabs), and the same query is retried until the control
    /// exists instead of reading `.exists` once while the pane is still laying out (#361). The
    /// subscript matches the element's accessibility identifier, label or title.
    @MainActor func tabButton(_ title: String, in app: XCUIApplication) -> XCUIElement {
        let radio = app.radioButtons[title]
        let tab = app.tabs[title]
        for _ in 0..<20 {
            if radio.waitForExistence(timeout: 0.5) { return radio }
            if tab.waitForExistence(timeout: 0.5) { return tab }
        }
        XCTFail("no tab \(title)")
        return radio
    }
}
