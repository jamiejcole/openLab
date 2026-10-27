// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class MainMenuTests: XCTestCase {
    func testMainMenuHasStandardTopLevelMenus() {
        let titles = MainMenu.make().bar.items.compactMap {
            $0.submenu?.title
        }
        XCTAssertEqual(titles, ["openLab", "File", "Edit", "Cues", "View", "Transport", "Window", "Help"])
    }
}
