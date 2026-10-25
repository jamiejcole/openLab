// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@MainActor
final class LaunchTests: XCTestCase {
    func testLaunchShowsMainWindow() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.windows["main-window"].waitForExistence(timeout: 10))
    }
}
