// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// A divider dragged with a real press-and-drag is remembered per workspace file: quit and
/// start the app again on the same show files, and the size is where it was left.
@MainActor
final class WindowLayoutUITests: XCTestCase {
    /// The divider between the cue list and the inspector (a wide, thin splitter) or the sidebar (a tall one).
    private func divider(_ app: XCUIApplication, horizontal: Bool) -> XCUIElement {
        let all = app.windows["main-window"].splitters.allElementsBoundByIndex
        return all.first { horizontal ? $0.frame.width > $0.frame.height : $0.frame.height > $0.frame.width }
            ?? app.splitters.firstMatch
    }

    /// The inspector's height or the sidebar's width: the distance from its divider to the window's
    /// edge. Unlike the divider's position on the screen, it does not move with the window.
    private func size(_ app: XCUIApplication, horizontal: Bool) -> CGFloat {
        let window = app.windows["main-window"].frame
        let frame = divider(app, horizontal: horizontal).frame
        return horizontal ? window.maxY - frame.minY : window.maxX - frame.minX
    }

    /// Drags a divider with the mouse by `delta` points across it (negative: up or left), pressing
    /// on its middle. On the hosted runner the default press-and-drag is not delivered (the pointer
    /// never moves and the call waits about five seconds), so a drag that did not move the divider
    /// within a few seconds is tried again the other ways XCUITest offers. Returns what was tried,
    /// for the failure message.
    private func drag(_ app: XCUIApplication, horizontal: Bool, by delta: CGFloat) -> String {
        let before = size(app, horizontal: horizontal)
        let start = divider(app, horizontal: horizontal).coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = start.withOffset(CGVector(dx: horizontal ? 0 : delta, dy: horizontal ? delta : 0))
        let routes: [(String, () -> Void)] = [
            ("press and drag", { start.press(forDuration: 0.2, thenDragTo: end) }),
            (
                "press and drag, fast, held",
                { start.press(forDuration: 0.5, thenDragTo: end, withVelocity: .fast, thenHoldForDuration: 0.5) }
            ),
            (
                "press and drag, slow, held",
                { start.press(forDuration: 0.5, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.5) }
            ),
        ]
        var tried = ""
        for (name, route) in routes {
            route()
            let moved = poll(seconds: 3) { abs(size(app, horizontal: horizontal) - before) > 30 }
            tried += "\(name): \(moved ? "moved" : "no move"); pointer \(NSEvent.mouseLocation). "
            if moved { break }
        }
        return tried
    }

    /// True once `condition` holds, false after `seconds`.
    private func poll(seconds: Double = 10, _ condition: () -> Bool) -> Bool {
        for _ in 0..<Int(seconds * 2) {
            if condition() { return true }
            _ = XCTWaiter().wait(for: [], timeout: 0.5)
        }
        return false
    }

    /// Quits and starts the app again with the same arguments: the same Application Support folder
    /// and show files, a new engine, the workspace opened again.
    private func relaunch(_ app: XCUIApplication) {
        app.terminate()
        app.launch()
        XCTAssertTrue(app.windows["main-window"].waitForExistence(timeout: 20), "the workspace is open again")
        XCTAssertTrue(waitFor("isHittable == true", app.staticTexts["Intro"]), "the demo is loaded again")
    }

    func testADraggedInspectorDividerStaysWhereItWasLeft() {
        let app = launchDemo()
        let before = size(app, horizontal: true)
        let tried = drag(app, horizontal: true, by: -60)
        XCTAssertTrue(poll { size(app, horizontal: true) > before + 30 }, "the drag took. \(tried)")
        let dragged = size(app, horizontal: true)
        relaunch(app)
        XCTAssertEqual(size(app, horizontal: true), dragged, accuracy: 2)
    }

    func testADraggedSidebarDividerStaysWhereItWasLeft() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        XCTAssertTrue(divider(app, horizontal: false).waitForExistence(timeout: 10))
        let before = size(app, horizontal: false)
        let tried = drag(app, horizontal: false, by: -60)
        XCTAssertTrue(poll { size(app, horizontal: false) > before + 30 }, "the drag took. \(tried)")
        let dragged = size(app, horizontal: false)
        relaunch(app)
        XCTAssertEqual(size(app, horizontal: false), dragged, accuracy: 2)
    }
}
