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
        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(waitFor("count == 1", windows), "the empty workspace closes")
        return app
    }

    /// True once `object` matches the predicate `format`, false after `timeout` seconds.
    @MainActor func waitFor(_ format: String, _ object: Any?, timeout: TimeInterval = 10) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: format), object: object)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }

    /// The inspector or sidebar tab titled `title`, once it has laid out: the tab bar is a radio
    /// button (segmented control) or a tab, depending on the control, and is found inside the control
    /// named by its accessibility identifier, waiting rather than testing `.exists` at once (#361).
    /// Falls back to the whole app if the control's identifier does not scope the query.
    @MainActor func tabButton(_ title: String, in app: XCUIApplication, within container: String) -> XCUIElement {
        let isTab = NSPredicate(
            format: "label == %@ AND (elementType == %d OR elementType == %d)", title,
            XCUIElement.ElementType.radioButton.rawValue, XCUIElement.ElementType.tab.rawValue)
        let box = app.descendants(matching: .any).matching(identifier: container).firstMatch
        let scoped = box.descendants(matching: .any).matching(isTab).firstMatch
        if box.waitForExistence(timeout: 10), scoped.waitForExistence(timeout: 5) { return scoped }
        let anywhere = app.descendants(matching: .any).matching(isTab).firstMatch
        XCTAssertTrue(anywhere.waitForExistence(timeout: 10), "tab \(title)")
        return anywhere
    }
}
