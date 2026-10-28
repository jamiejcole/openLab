// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The cue levels matrix mixer (AUD-06, AUD-08): the master and each cue output's level in the top
/// row, a row per file channel below it with a crosspoint per cue output. Drawn by hand and only
/// where visible, so a 64 x 64 matrix is one view, not 4,096 (AUD-03).
///
/// A click selects a level; the panel's strip below edits it (field, fader, mute). Dragging a
/// level up or down changes it, as in QLab (manual p. 181); a drag never goes above 0 dB.
@MainActor
final class LevelMatrixView: NSView {
    static let cellW: CGFloat = 46
    static let cellH: CGFloat = 20
    static let headW: CGFloat = 56
    static let headerH: CGFloat = 18

    var levels: LevelSet? {
        didSet { needsDisplay = true }
    }
    private(set) var inputs = 0
    private(set) var outputCount = 0
    var selection: LevelSet.Slot = .master {
        didSet { needsDisplay = true }
    }
    var isLocked = false

    /// A click or arrow key chose this level.
    var onSelect: ((LevelSet.Slot) -> Void)?
    /// The mouse is dragging this level to `db`; `first` is true for the first move of a drag.
    var onDrag: ((LevelSet.Slot, Double, _ first: Bool) -> Void)?
    var onDragEnd: (() -> Void)?

    private var dragStartY: CGFloat = 0
    private var dragStartDb = 0.0
    private var dragSlot: LevelSet.Slot?
    private var dragging = false

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    func resize(inputs: Int, outputs: Int) {
        self.inputs = max(0, min(inputs, LevelSet.maxChannels))
        outputCount = max(0, min(outputs, LevelSet.maxChannels))
        setFrameSize(
            NSSize(
                width: Self.headW + CGFloat(outputCount) * Self.cellW,
                height: Self.headerH + Self.cellH * CGFloat(self.inputs + 1)))
        needsDisplay = true
    }

    // MARK: Geometry

    /// The cell of `slot`.
    func rect(of slot: LevelSet.Slot) -> NSRect {
        switch slot {
        case .master: return NSRect(x: 0, y: Self.headerH, width: Self.headW, height: Self.cellH)
        case .output(let o):
            return NSRect(
                x: Self.headW + CGFloat(o) * Self.cellW, y: Self.headerH, width: Self.cellW, height: Self.cellH)
        case .cross(let i, let o):
            return NSRect(
                x: Self.headW + CGFloat(o) * Self.cellW, y: Self.headerH + Self.cellH * CGFloat(i + 1),
                width: Self.cellW, height: Self.cellH)
        }
    }

    func slot(at p: NSPoint) -> LevelSet.Slot? {
        guard p.y >= Self.headerH, p.x >= 0 else { return nil }
        let row = Int((p.y - Self.headerH) / Self.cellH)
        let col = p.x < Self.headW ? -1 : Int((p.x - Self.headW) / Self.cellW)
        guard row <= inputs, col < outputCount else { return nil }
        if row == 0 { return col < 0 ? .master : .output(col) }
        return col < 0 ? nil : .cross(input: row - 1, output: col)
    }

    // MARK: Drawing

    override func draw(_ dirty: NSRect) {
        guard let levels else { return }
        let small = NSFont.systemFont(ofSize: 10)
        let label: [NSAttributedString.Key: Any] = [.font: small, .foregroundColor: NSColor.secondaryLabelColor]
        // Only the cells in `dirty`: a scroll or a one-cell change redraws a handful, not the grid.
        let firstCol = max(0, Int((dirty.minX - Self.headW) / Self.cellW))
        let lastCol = min(outputCount - 1, Int((dirty.maxX - Self.headW) / Self.cellW))
        let firstRow = max(0, Int((dirty.minY - Self.headerH) / Self.cellH))
        let lastRow = min(inputs, Int((dirty.maxY - Self.headerH) / Self.cellH))
        if dirty.minY < Self.headerH, firstCol <= lastCol {
            for o in firstCol...lastCol {
                let r = NSRect(
                    x: Self.headW + CGFloat(o) * Self.cellW, y: 0, width: Self.cellW, height: Self.headerH)
                var text = "\(o + 1)"
                if let g = levels.gang(of: o) {
                    gangColour(g).setFill()
                    r.insetBy(dx: 1, dy: 1).fill()
                    text += " " + LevelSet.gangLetter(g)
                }
                centred(text, in: r, attributes: label)
            }
        }
        if firstRow <= lastRow {
            for row in firstRow...lastRow {
                if dirty.minX < Self.headW {
                    if row == 0 {
                        cell(.master, levels)
                    } else {
                        let r = NSRect(
                            x: 0, y: Self.headerH + CGFloat(row) * Self.cellH, width: Self.headW, height: Self.cellH)
                        centred("\(row)", in: r, attributes: label)
                    }
                }
                guard firstCol <= lastCol else { continue }
                for o in firstCol...lastCol { cell(row == 0 ? .output(o) : .cross(input: row - 1, output: o), levels) }
            }
        }
    }

    private func cell(_ slot: LevelSet.Slot, _ levels: LevelSet) {
        let r = rect(of: slot).insetBy(dx: 1, dy: 1)
        let db = levels.db(slot)
        let active = levels.isActive(slot)
        let muted = levels.isMuted(slot)
        let path = NSBezierPath(roundedRect: r, xRadius: 3, yRadius: 3)
        if !active {
            NSColor.quaternaryLabelColor.setFill()
        } else if levels.isFade {
            NSColor.systemYellow.withAlphaComponent(0.55).setFill()
        } else if let db, db > LevelText.silence {
            // Louder routes are denser; -inf and a muted route stay plain.
            let t = CGFloat((max(db, LevelText.floorDb) - LevelText.floorDb) / (LevelText.maxDb - LevelText.floorDb))
            NSColor.controlAccentColor.withAlphaComponent(muted ? 0.1 : 0.12 + 0.5 * t).setFill()
        } else {
            NSColor.controlBackgroundColor.setFill()
        }
        path.fill()
        NSColor.separatorColor.setStroke()
        path.lineWidth = 0.5
        path.stroke()
        if slot == selection {
            NSColor.keyboardFocusIndicatorColor.setStroke()
            path.lineWidth = 2
            path.stroke()
        }
        guard active, let db else { return }
        let text = (muted ? "M " : "") + LevelText.format(db)
        centred(
            text, in: r,
            attributes: [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular),
                .foregroundColor: muted ? NSColor.tertiaryLabelColor : NSColor.labelColor,
            ])
    }

    private func gangColour(_ g: Int) -> NSColor {
        let palette: [NSColor] = [.systemTeal, .systemPink, .systemIndigo, .systemOrange, .systemGreen, .systemPurple]
        return palette[g % palette.count].withAlphaComponent(0.35)
    }

    private func centred(_ text: String, in r: NSRect, attributes: [NSAttributedString.Key: Any]) {
        let s = NSAttributedString(string: text, attributes: attributes)
        let size = s.size()
        s.draw(at: NSPoint(x: r.midX - size.width / 2, y: r.midY - size.height / 2))
    }

    // MARK: Mouse and keys

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        let p = convert(event.locationInWindow, from: nil)
        dragSlot = slot(at: p)
        dragging = false
        guard let s = dragSlot else { return }
        selection = s
        onSelect?(s)
        dragStartY = p.y
        // From -inf a drag starts at the bottom of the fader's travel.
        dragStartDb = max(levels?.db(s) ?? LevelText.floorDb, LevelText.floorDb)
    }

    override func mouseDragged(with event: NSEvent) {
        guard !isLocked, let s = dragSlot, levels != nil else { return }
        let dy = dragStartY - convert(event.locationInWindow, from: nil).y  // up is louder
        guard dragging || abs(dy) >= 3 else { return }
        let first = !dragging
        dragging = true
        // QLab keeps a drag from passing 0 dB (manual p. 181); a level already above it may move down.
        let ceiling = max(0, dragStartDb)
        let db = min(ceiling, (dragStartDb + Double(dy) * 0.25).rounded(toPlaces: 1))
        onDrag?(s, db, first)
    }

    override func mouseUp(with event: NSEvent) {
        if dragging { onDragEnd?() }
        dragging = false
        dragSlot = nil
    }

    override func keyDown(with event: NSEvent) {
        let (dc, dr): (Int, Int)
        switch event.keyCode {
        case 123: (dc, dr) = (-1, 0)
        case 124: (dc, dr) = (1, 0)
        case 125: (dc, dr) = (0, 1)
        case 126: (dc, dr) = (0, -1)
        default: return super.keyDown(with: event)
        }
        // Column -1 is the master's; row 0 the outputs'; rows below are file channels.
        var col = -1
        var row = 0
        switch selection {
        case .master: break
        case .output(let o): col = o
        case .cross(let i, let o): (row, col) = (i + 1, o)
        }
        col = max(row == 0 ? -1 : 0, min(outputCount - 1, col + dc))
        row = max(0, min(inputs, row + dr))
        if row > 0, col < 0 { col = 0 }
        let next: LevelSet.Slot = row == 0 ? (col < 0 ? .master : .output(col)) : .cross(input: row - 1, output: col)
        selection = next
        onSelect?(next)
    }

    // MARK: Accessibility

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityValue() -> Any? {
        "\(inputs) x \(outputCount)"
    }
}

extension Double {
    fileprivate func rounded(toPlaces places: Int) -> Double {
        let f = pow(10.0, Double(places))
        return (self * f).rounded() / f
    }
}
