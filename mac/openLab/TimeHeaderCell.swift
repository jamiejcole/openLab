// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The cue list's header row. AppKit's own handling of a press on a column title draws that header
/// pressed (lighter, bold, its separator gone) and can drag the column to another place; QLab's headers
/// do neither. So a press on a title only waits for the button to come up and reports a click on the
/// column it came up in (`onClick`). A press on a resizable column's right edge still resizes it.
final class CueHeaderView: NSTableHeaderView {
    var onClick: ((NSTableColumn) -> Void)?
    /// How far a press on a column's right edge reaches into it and past it and still resizes it: the
    /// reach AppKit's own header tracking gives the edge.
    static let edgeInside: CGFloat = 3
    static let edgeOutside: CGFloat = 2

    override func mouseDown(with event: NSEvent) {
        guard let table = tableView, let window else { return super.mouseDown(with: event) }
        let point = location(of: event, in: window)
        if onResizableEdge(point, table) { return super.mouseDown(with: event) }
        let pressed = column(at: point)
        // Where the mouse last was while the button was down: the release counts as a click only over
        // the column pressed, as a button's does.
        var last = point
        while let next = window.nextEvent(matching: [.leftMouseUp, .leftMouseDragged]) {
            guard next.type == .leftMouseUp else {
                last = location(of: next, in: window)
                continue
            }
            if column(at: last) == pressed, table.tableColumns.indices.contains(pressed) {
                onClick?(table.tableColumns[pressed])
            }
            return
        }
    }

    /// Where `event` happened in this view. An event that is not this window's (none, or another
    /// window's) gives its place on the screen or in that window.
    private func location(of event: NSEvent, in window: NSWindow) -> NSPoint {
        let screen = event.window.map { $0.convertPoint(toScreen: event.locationInWindow) } ?? event.locationInWindow
        return convert(window.convertPoint(fromScreen: screen), from: nil)
    }

    private func onResizableEdge(_ point: NSPoint, _ table: NSTableView) -> Bool {
        table.tableColumns.indices.contains { i in
            let edge = headerRect(ofColumn: i).maxX
            return table.tableColumns[i].resizingMask.contains(.userResizingMask)
                && point.x >= edge - Self.edgeInside && point.x < edge + Self.edgeOutside
        }
    }
}

/// A time column's header: its title and a small chevron. Left means the column shows the time
/// left (counting down), right the whole time (counting up); a click on the header switches (see
/// `CueListViewController.toggleTimeShown`).
final class TimeHeaderCell: NSTableHeaderCell {
    var shown: TimeShown = .remaining

    private static let chevronWidth: CGFloat = 14

    /// The title's room and the chevron's, so a column is never made too narrow for both.
    override var cellSize: NSSize {
        var size = super.cellSize
        size.width += Self.chevronWidth
        return size
    }

    /// The chevron for what the column shows: left while it counts down, right while it counts up.
    static func icon(_ shown: TimeShown) -> Icon { shown == .remaining ? .timeToggleUp : .timeToggle }

    override func drawInterior(withFrame cellFrame: NSRect, in controlView: NSView) {
        var text = cellFrame
        text.size.width = max(0, text.width - Self.chevronWidth)
        super.drawInterior(withFrame: text, in: controlView)
        let image = Self.icon(shown).image(
            loc(shown == .remaining ? "cuelist.time.showFull" : "cuelist.time.showRemaining"), compact: true)
        // Bold and in the title's own colour, right after the title (QLab's mark beside "Pre-Wait").
        let size = NSSize(width: 11, height: 11)
        let titleEnd = cellFrame.minX + 4 + attributedStringValue.size().width
        let x = min(titleEnd + 1, cellFrame.maxX - Self.chevronWidth + 1)
        let rect = NSRect(x: x, y: cellFrame.midY - size.height / 2, width: size.width, height: size.height)
        Self.tinted(image, .labelColor, size: size).draw(in: rect)
    }

    /// A template image filled with `colour`; a template drawn directly would be black in dark mode.
    static func tinted(_ image: NSImage, _ colour: NSColor, size: NSSize, description: String? = nil) -> NSImage {
        let out = NSImage(size: size, flipped: false) { rect in
            image.draw(in: rect)
            colour.set()
            rect.fill(using: .sourceIn)
            return true
        }
        out.isTemplate = false
        out.accessibilityDescription = description
        return out
    }
}

/// The continue column's header: no title, a small mark that says what the column is (QLab's header
/// carries one over the continue symbols).
final class ContinueHeaderCell: NSTableHeaderCell {
    override func drawInterior(withFrame cellFrame: NSRect, in controlView: NSView) {
        // AppKit draws the last column's cell again over the blank space after the columns: only the
        // column's own header gets the mark.
        guard let header = controlView as? NSTableHeaderView, let table = header.tableView else { return }
        let column = table.column(withIdentifier: NSUserInterfaceItemIdentifier("continue"))
        guard column >= 0, abs(header.headerRect(ofColumn: column).midX - cellFrame.midX) < 1 else { return }
        let size = NSSize(width: 11, height: 11)
        let image = Icon.continueAuto.image(loc("cuelist.col.continue"), compact: true)
        let rect = NSRect(
            x: cellFrame.midX - size.width / 2, y: cellFrame.midY - size.height / 2, width: size.width,
            height: size.height)
        TimeHeaderCell.tinted(image, .labelColor, size: size).draw(in: rect)
    }
}
