// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@MainActor
final class LaunchTests: XCTestCase {
    func testLaunchShowsMainWindow() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.windows["main-window"].waitForExistence(timeout: 10))
    }

    /// The fake engine needs no engine process, so the UI starts and shows its window with it.
    func testLaunchWithFakeEngineShowsMainWindow() {
        let app = XCUIApplication()
        app.launchArguments += ["-OpenLabEngine", "fake"]
        app.launch()
        XCTAssertTrue(app.windows["main-window"].waitForExistence(timeout: 10))
    }
}
