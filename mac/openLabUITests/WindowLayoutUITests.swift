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

    /// Presses on the divider's first point across it and drags. A divider is one point thick and
    /// its centre is half a point in, which a 1x display (the hosted runner's) rounds to the point
    /// beside the divider: the press then misses it and nothing moves.
    private func drag(_ divider: XCUIElement, horizontal: Bool, by delta: CGFloat) {
        let frame = divider.frame
        let along = horizontal ? (frame.width / 2).rounded() : (frame.height / 2).rounded()
        let origin = divider.coordinate(withNormalizedOffset: .zero)
        let start = origin.withOffset(CGVector(dx: horizontal ? along : 0, dy: horizontal ? 0 : along))
        start.press(
            forDuration: 0.2,
            thenDragTo: start.withOffset(CGVector(dx: horizontal ? 0 : delta, dy: horizontal ? delta : 0)))
    }

    /// True once `condition` holds, false after ten seconds.
    private func poll(_ condition: () -> Bool) -> Bool {
        for _ in 0..<20 {
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
        let before = divider(app, horizontal: true).frame.minY
        drag(divider(app, horizontal: true), horizontal: true, by: -60)
        XCTAssertTrue(poll { divider(app, horizontal: true).frame.minY < before - 30 }, "the drag took")
        let dragged = divider(app, horizontal: true).frame.minY
        relaunch(app)
        XCTAssertEqual(divider(app, horizontal: true).frame.minY, dragged, accuracy: 2)
    }

    func testADraggedSidebarDividerStaysWhereItWasLeft() {
        let app = launchDemo()
        app.checkBoxes["toggle-sidebar"].click()
        XCTAssertTrue(divider(app, horizontal: false).waitForExistence(timeout: 10))
        let before = divider(app, horizontal: false).frame.minX
        drag(divider(app, horizontal: false), horizontal: false, by: -60)
        XCTAssertTrue(poll { divider(app, horizontal: false).frame.minX < before - 30 }, "the drag took")
        let dragged = divider(app, horizontal: false).frame.minX
        relaunch(app)
        XCTAssertEqual(divider(app, horizontal: false).frame.minX, dragged, accuracy: 2)
    }
}
