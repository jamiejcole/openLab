// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

/// The workspace window on a real screen: what an offscreen render cannot show.
@MainActor
final class WorkspaceLayoutUITests: XCTestCase {
    /// Nested's list with a Wait cue first, so GO starts a cue that keeps running while the list is scrolled.
    private func prepareLongList(_ copies: URL) throws {
        let file = copies.appendingPathComponent("Nested/Nested.openlab")
        var root = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
        var workspace = try XCTUnwrap(root["workspace"] as? [String: Any])
        var lists = try XCTUnwrap(workspace["cue_lists"] as? [[String: Any]])
        var cues = try XCTUnwrap(lists[0]["cues"] as? [[String: Any]])
        var wait = try XCTUnwrap(cues.first { ($0["kind"] as? [String: Any])?["type"] as? String == "memo" })
        wait["id"] = "00000000-0000-0000-0000-0000000009a1"
        wait["number"] = "W"
        wait["name"] = "Hold"
        wait["kind"] = ["type": "wait", "duration": 600.0]
        cues.insert(wait, at: 0)
        lists[0]["cues"] = cues
        workspace["cue_lists"] = lists
        root["workspace"] = workspace
        try JSONSerialization.data(withJSONObject: root).write(to: file)
    }

    /// The pixels of `band` (window points) of the window as drawn on the screen.
    private func pixels(of window: XCUIElement, band: CGRect) throws -> [UInt8] {
        let shot = window.screenshot().image
        let cg = try XCTUnwrap(shot.cgImage(forProposedRect: nil, context: nil, hints: nil))
        let scale = CGFloat(cg.width) / window.frame.width
        let crop = CGRect(
            x: band.minX * scale, y: band.minY * scale, width: band.width * scale, height: band.height * scale)
        let part = try XCTUnwrap(cg.cropping(to: crop.integral))
        let rep = NSBitmapImageRep(cgImage: part)
        return Array(UnsafeBufferPointer(start: rep.bitmapData, count: rep.bytesPerRow * rep.pixelsHigh))
    }

    /// Flow: a long list scrolled while a cue plays → no row is drawn through the column header.
    func testALongListScrolledWhileACuePlaysDrawsNoRowThroughTheColumnHeader() throws {
        let app = launchApp(opening: ["Nested"], prepare: { try self.prepareLongList($0) })
        let window = app.windows.firstMatch
        let list = app.outlines["cue-list"]
        XCTAssertTrue(list.waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["Hold"].waitForExistence(timeout: 10), "the wait cue is listed")
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO: the Wait cue runs
        XCTAssertTrue(
            cueState("Running", of: "Hold", in: app).waitForExistence(timeout: 10), "the Wait cue plays")
        // The header's band: the strip just above the list's first row, on the real screen.
        let top = list.frame.minY - window.frame.minY
        let band = CGRect(x: list.frame.minX - window.frame.minX, y: top - 20, width: list.frame.width, height: 18)
        let atTop = try pixels(of: window, band: band)
        let rows = CGRect(x: band.minX, y: top + 2, width: band.width, height: 60)
        let rowsAtTop = try pixels(of: window, band: rows)
        XCTAssertFalse(atTop.isEmpty)
        for _ in 0..<4 {
            list.scroll(byDeltaX: 0, deltaY: -300)
            // The wait's time keeps counting while rows pass under the header.
            let scrolled = try pixels(of: window, band: band)
            XCTAssertEqual(scrolled, atTop, "the header band is the same whatever rows are scrolled behind it")
        }
        XCTAssertNotEqual(try pixels(of: window, band: rows), rowsAtTop, "the list really did scroll")
    }
}
