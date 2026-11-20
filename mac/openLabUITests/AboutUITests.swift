// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The About window with real keys on the real engine.
@MainActor
final class AboutUITests: XCTestCase {
    private func openAbout(_ app: XCUIApplication) -> XCUIElement {
        app.menuBars.menuBarItems["openLab"].click()
        app.menuBars.menuItems["About openLab"].click()
        let about = app.windows["about-window"]
        XCTAssertTrue(about.waitForExistence(timeout: 5), "About opens")
        return about
    }

    /// Show Mode → About from the app menu → close it → Space starts the cue: the workspace is key
    /// again. While About is key Space is not a GO (it is not a workspace window).
    func testAboutInShowModeSpaceIsNotGoWhileOpenAndGoesAgainAfterClosing() {
        let app = launchDemo()
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        app.staticTexts["Intro"].click()
        let running = cueStates("Running", in: app)
        let about = openAbout(app)
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertFalse(running.firstMatch.waitForExistence(timeout: 2), "no GO while About is key")
        app.typeKey("w", modifierFlags: .command)
        XCTAssertTrue(waitFor("exists == false", about, timeout: 5), "Cmd-W closes About")
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(running.firstMatch.waitForExistence(timeout: 10), "GO acts once About is closed")
    }

    /// Credits shown → Esc closes the window and, as everywhere, panics the playing cue.
    func testAboutEscWithCreditsShownClosesItAndPanicsTheShow() {
        let app = launchDemo()
        app.staticTexts["Intro"].click()
        let running = cueStates("Running", in: app)
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(running.firstMatch.waitForExistence(timeout: 10))
        let about = openAbout(app)
        about.buttons["about-credits-button"].click()
        XCTAssertTrue(about.scrollViews["about-credits"].waitForExistence(timeout: 5))
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", about, timeout: 5), "Esc closes About")
        XCTAssertTrue(waitFor("count == 0", running, timeout: 20), "Esc also panicked the show")
    }
}
