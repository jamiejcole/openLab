// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The routing matrix of the patch editor (QLab 5 manual p. 190): a row per cue output (its number
/// and name, a level box laid over it by the editor, its meter) and a column per device output. The
/// box where a row meets the device output it plays on is lit; a click on another box of the row
/// sends the cue output there. Drawn by hand and only where visible, so 128 rows by 64 columns is
/// one view. Device outputs openLab cannot use on this device are drawn dimmed and take no click.
@MainActor
final class PatchRoutingView: NSView {
    static let rowH: CGFloat = 22
    static let headerH: CGFloat = 22
    static let labelW: CGFloat = 132
    static let levelW: CGFloat = 60
    static let meterW: CGFloat = 52
    static let cellW: CGFloat = 30
    static var crossX: CGFloat { labelW + levelW + meterW }

    /// Each cue output's label: its number, and its name when it has one.
    var rowLabels: [String] = [] {
        didSet { if rowLabels != oldValue { resize() } }
    }
    /// The device outputs drawn (every output the device has).
    var deviceOutputs = 0 {
        didSet { if deviceOutputs != oldValue { resize() } }
    }
    /// The device outputs a cue output can be sent to (the first `usable`).
    var usable = 0 {
        didSet { if usable != oldValue { needsDisplay = true } }
    }
    /// The device output (0-based) each cue output plays on; nil: none known (no device).
    var routes: [Int?] = [] {
        didSet { if routes != oldValue { needsDisplay = true } }
    }
    var isEnabled = true {
        didSet { if isEnabled != oldValue { needsDisplay = true } }
    }
    /// A box was clicked: send cue output `row` to device output `column` (both 0-based).
    var onRoute: ((_ row: Int, _ column: Int) -> Void)?

    override var isFlipped: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    private func resize() {
        setFrameSize(
            NSSize(
                width: Self.crossX + CGFloat(deviceOutputs) * Self.cellW + 8,
                height: Self.headerH + CGFloat(rowLabels.count) * Self.rowH + 4))
        needsDisplay = true
    }

    // MARK: Geometry

    func levelRect(row: Int) -> NSRect {
        NSRect(x: Self.labelW + 2, y: Self.headerH + CGFloat(row) * Self.rowH + 1, width: Self.levelW - 6, height: Self.rowH - 2)
    }

    /// Where the meter goes: one bar per row, for the first `rows` rows.
    func meterRect(rows: Int) -> NSRect {
        NSRect(
            x: Self.labelW + Self.levelW + 2, y: Self.headerH + 4, width: Self.meterW - 10,
            height: CGFloat(rows) * Self.rowH - 6)
    }

    func cellRect(row: Int, column: Int) -> NSRect {
        NSRect(
            x: Self.crossX + CGFloat(column) * Self.cellW, y: Self.headerH + CGFloat(row) * Self.rowH,
            width: Self.cellW, height: Self.rowH)
    }

    func cell(at p: NSPoint) -> (row: Int, column: Int)? {
        guard p.x >= Self.crossX, p.y >= Self.headerH else { return nil }
        let column = Int((p.x - Self.crossX) / Self.cellW)
        let row = Int((p.y - Self.headerH) / Self.rowH)
        guard row < rowLabels.count, column < deviceOutputs else { return nil }
        return (row, column)
    }

    // MARK: Drawing

    override func draw(_ dirty: NSRect) {
        NSBezierPath.clip(bounds)
        let small: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10), .foregroundColor: NSColor.secondaryLabelColor,
        ]
        let label: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.labelColor,
            .paragraphStyle: Self.truncating,
        ]
        let firstCol = max(0, Int((dirty.minX - Self.crossX) / Self.cellW))
        let lastCol = min(deviceOutputs - 1, Int((dirty.maxX - Self.crossX) / Self.cellW))
        if dirty.minY < Self.headerH {
            draw(loc("patchEditor.cueOutputs"), in: NSRect(x: 4, y: 0, width: Self.labelW - 8, height: Self.headerH), small)
            centred(loc("patchEditor.level"), in: NSRect(x: Self.labelW, y: 0, width: Self.levelW, height: Self.headerH), small)
            if firstCol <= lastCol {
                for c in firstCol...lastCol {
                    let r = NSRect(x: Self.crossX + CGFloat(c) * Self.cellW, y: 0, width: Self.cellW, height: Self.headerH)
                    centred("\(c + 1)", in: r, small)
                }
            }
        }
        let firstRow = max(0, Int((dirty.minY - Self.headerH) / Self.rowH))
        let lastRow = min(rowLabels.count - 1, Int((dirty.maxY - Self.headerH) / Self.rowH))
        guard firstRow <= lastRow else { return }
        for row in firstRow...lastRow {
            let y = Self.headerH + CGFloat(row) * Self.rowH
            if row % 2 == 1 {
                NSColor.labelColor.withAlphaComponent(0.04).setFill()
                NSRect(x: 0, y: y, width: bounds.width, height: Self.rowH).fill()
            }
            if dirty.minX < Self.labelW {
                draw(rowLabels[row], in: NSRect(x: 4, y: y, width: Self.labelW - 8, height: Self.rowH), label)
            }
            guard firstCol <= lastCol else { continue }
            for c in firstCol...lastCol { box(row: row, column: c) }
        }
    }

    private func box(row: Int, column: Int) {
        let r = cellRect(row: row, column: column).insetBy(dx: 2, dy: 2)
        let path = NSBezierPath(roundedRect: r, xRadius: 3, yRadius: 3)
        let routed = routes.indices.contains(row) && routes[row] == column
        let usableHere = column < usable
        if routed {
            (usableHere ? NSColor.controlAccentColor : NSColor.systemRed).withAlphaComponent(isEnabled ? 0.85 : 0.4)
                .setFill()
        } else {
            NSColor(white: 0, alpha: usableHere ? 0.35 : 0.12).setFill()
        }
        path.fill()
        NSColor.separatorColor.setStroke()
        path.lineWidth = 0.5
        path.stroke()
        guard routed else { return }
        let dot = NSBezierPath(ovalIn: NSRect(x: r.midX - 3, y: r.midY - 3, width: 6, height: 6))
        NSColor.white.setFill()
        dot.fill()
    }

    private static let truncating: NSParagraphStyle = {
        let p = NSMutableParagraphStyle()
        p.lineBreakMode = .byTruncatingTail
        return p
    }()

    private func draw(_ text: String, in r: NSRect, _ attributes: [NSAttributedString.Key: Any]) {
        let s = NSAttributedString(string: text, attributes: attributes)
        let h = s.size().height
        s.draw(with: NSRect(x: r.minX, y: r.midY - h / 2, width: r.width, height: h), options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    private func centred(_ text: String, in r: NSRect, _ attributes: [NSAttributedString.Key: Any]) {
        let s = NSAttributedString(string: text, attributes: attributes)
        let size = s.size()
        s.draw(at: NSPoint(x: r.midX - size.width / 2, y: r.midY - size.height / 2))
    }

    // MARK: Mouse

    override func mouseDown(with event: NSEvent) {
        guard isEnabled, let hit = cell(at: convert(event.locationInWindow, from: nil)), hit.column < usable else {
            return
        }
        onRoute?(hit.row, hit.column)
    }

    // MARK: Accessibility

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityValue() -> Any? {
        routes.enumerated().map { "\($0.offset + 1)→\($0.element.map { String($0 + 1) } ?? "-")" }.joined(separator: ", ")
    }
}
