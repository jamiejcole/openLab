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
}
