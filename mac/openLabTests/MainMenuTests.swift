// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class MainMenuTests: XCTestCase {
    func testMainMenuHasStandardTopLevelMenus() {
        let titles = MainMenu.make(keyMap: KeyMap(store: AppControllerTests.scratchDefaults())).bar.items.compactMap {
            $0.submenu?.title
        }
        XCTAssertEqual(titles, ["openLab", "File", "Edit", "Transport", "Window", "Help"])
    }
}
