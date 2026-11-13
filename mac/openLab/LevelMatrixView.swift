// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The value boxes of the cue levels (AUD-06, AUD-08), laid out as QLab's Levels tab is: the main
/// level and each cue output's level in the top row (the panel puts a fader above each), their
/// names under them, then a row per file channel with a crosspoint per cue output. Drawn by hand
/// and only where visible, so a 64 x 64 matrix is one view, not 4,096 (AUD-03).
///
/// A click selects a level and a double-click types in it. Dragging a level up or down changes it,
/// as in QLab (manual p. 181); a drag never goes above 0 dB.
@MainActor
final class LevelMatrixView: NSView {
    /// The workspace volume limits the levels live in.
    var limits = VolumeLimits() {
        didSet { if limits != oldValue { needsDisplay = true } }
    }
    /// The most cue-output columns shown: a patch can have 128 cue outputs, but the tab shows the
    /// first 64. Levels and routing to the others are kept on every edit (`LevelSet`), only not
    /// shown or edited here.
    static let maxShownOutputs = 64
    static let cellW: CGFloat = 46
    static let cellH: CGFloat = 20
    static let headW: CGFloat = 56
    /// The row of names under the top row's boxes.
    static let headerH: CGFloat = 18
    /// Where the crosspoint rows start.
    static var crossTop: CGFloat { cellH + headerH }

    var levels: LevelSet? {
        didSet { needsDisplay = true }
    }
    /// Levels that differ between the cues shown (several selected): drawn as mixed, not as a value.
    var mixed: Set<LevelSet.Slot> = [] {
        didSet { if mixed != oldValue { needsDisplay = true } }
    }
    /// The names of the cue outputs (from the cue's patch), drawn under their boxes; an empty or
    /// missing one shows the output's number (QLab 5 manual p. 189).
    var outputNames: [String] = [] {
        didSet { if outputNames != oldValue { needsDisplay = true } }
    }
    /// The cues shown are ganged differently: no gang letter or colour is drawn.
    var hidesGangs = false {
        didSet { if hidesGangs != oldValue { needsDisplay = true } }
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
    /// The mouse button goes down or up here: a press can become a drag at any point until it is up.
    var onPress: ((Bool) -> Void)?
    /// A level was double-clicked, or Return pressed on the chosen one: type in it.
    var onType: ((LevelSet.Slot) -> Void)?

    private var dragStartY: CGFloat = 0
    private var dragStartDb = 0.0
    private var dragSlot: LevelSet.Slot?
    private var dragging = false

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    func resize(inputs: Int, outputs: Int) {
        self.inputs = max(0, min(inputs, LevelSet.maxInputs))
        outputCount = max(0, min(outputs, Self.maxShownOutputs))
        setFrameSize(
            NSSize(
                width: Self.headW + CGFloat(outputCount) * Self.cellW,
                height: Self.crossTop + Self.cellH * CGFloat(self.inputs)))
        // A drag that began on a row the file no longer has must not keep editing it.
        if let s = dragSlot, !isInGrid(s) { dragSlot = nil }
        needsDisplay = true
    }

    private func isInGrid(_ slot: LevelSet.Slot) -> Bool {
        switch slot {
        case .master: return true
        case .output(let o): return o < outputCount
        case .cross(let i, let o): return i < inputs && o < outputCount
        }
    }

    // MARK: Geometry

    /// The cell of `slot`.
    func rect(of slot: LevelSet.Slot) -> NSRect {
        switch slot {
        case .master: return NSRect(x: 0, y: 0, width: Self.headW, height: Self.cellH)
        case .output(let o):
            return NSRect(x: Self.headW + CGFloat(o) * Self.cellW, y: 0, width: Self.cellW, height: Self.cellH)
        case .cross(let i, let o):
            return NSRect(
                x: Self.headW + CGFloat(o) * Self.cellW, y: Self.crossTop + Self.cellH * CGFloat(i),
                width: Self.cellW, height: Self.cellH)
        }
    }

    func slot(at p: NSPoint) -> LevelSet.Slot? {
        guard p.y >= 0, p.x >= 0 else { return nil }
        let col = p.x < Self.headW ? -1 : Int((p.x - Self.headW) / Self.cellW)
        guard col < outputCount else { return nil }
        if p.y < Self.cellH { return col < 0 ? .master : .output(col) }
        guard p.y >= Self.crossTop, col >= 0 else { return nil }
        let row = Int((p.y - Self.crossTop) / Self.cellH)
        return row < inputs ? .cross(input: row, output: col) : nil
    }

    // MARK: Drawing

    override func draw(_ dirty: NSRect) {
        NSBezierPath.clip(bounds)
        guard let levels else { return }
        let small = Self.labelFont
        let label: [NSAttributedString.Key: Any] = [.font: small, .foregroundColor: NSColor.secondaryLabelColor]
        // Only the cells in `dirty`: a scroll or a one-cell change redraws a handful, not the grid.
        let firstCol = max(0, Int((dirty.minX - Self.headW) / Self.cellW))
        let lastCol = min(outputCount - 1, Int((dirty.maxX - Self.headW) / Self.cellW))
        let namesRow = NSRect(x: 0, y: Self.cellH, width: bounds.width, height: Self.headerH)
        if dirty.minX < Self.headW {
            if dirty.minY < Self.cellH { cell(.master, levels) }
            if dirty.intersects(namesRow) {
                centred(
                    loc("levels.main"),
                    in: NSRect(x: 0, y: Self.cellH, width: Self.headW, height: Self.headerH), attributes: label)
            }
        }
        if firstCol <= lastCol {
            for o in firstCol...lastCol {
                if dirty.minY < Self.cellH { cell(.output(o), levels) }
                guard dirty.intersects(namesRow) else { continue }
                let r = NSRect(
                    x: Self.headW + CGFloat(o) * Self.cellW, y: Self.cellH, width: Self.cellW, height: Self.headerH)
                if !hidesGangs, let g = levels.gang(of: o) {
                    gangColour(g).setFill()
                    r.insetBy(dx: 1, dy: 1).fill()
                }
                centred(columnLabel(o), in: r, attributes: label)
            }
        }
        guard inputs > 0 else { return }
        let firstRow = max(0, Int((dirty.minY - Self.crossTop) / Self.cellH))
        let lastRow = min(inputs - 1, Int((dirty.maxY - Self.crossTop) / Self.cellH))
        guard firstRow <= lastRow else { return }
        for row in firstRow...lastRow {
            if dirty.minX < Self.headW {
                let r = NSRect(
                    x: 0, y: Self.crossTop + CGFloat(row) * Self.cellH, width: Self.headW, height: Self.cellH)
                centred("\(row + 1)", in: r, attributes: label)
            }
            guard firstCol <= lastCol else { continue }
            for o in firstCol...lastCol { cell(.cross(input: row, output: o), levels) }
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
        } else {
            // A value box is dark whatever it holds, as QLab's are; the number says how loud.
            NSColor(white: 0, alpha: 0.45).setFill()
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
        let text = mixed.contains(slot) ? loc("inspector.mixed.level") : (muted ? "M " : "") + LevelText.format(db)
        centred(
            text, in: r,
            attributes: [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular),
                .foregroundColor: mixed.contains(slot)
                    ? NSColor.secondaryLabelColor : muted ? NSColor.tertiaryLabelColor : NSColor.labelColor,
            ])
    }

    private func gangColour(_ g: Int) -> NSColor {
        let palette: [NSColor] = [.systemTeal, .systemPink, .systemIndigo, .systemOrange, .systemGreen, .systemPurple]
        return palette[g % palette.count].withAlphaComponent(0.35)
    }

    static let labelFont = NSFont.systemFont(ofSize: 10)

    /// The text drawn under cue output `o`'s box: its name from the patch (a two-line name on one
    /// line) or its number, with its gang letter; cut in the middle to the column's width, so
    /// names that differ only at the end ("Surround L", "Surround R") still read apart.
    func columnLabel(_ o: Int) -> String {
        let name = outputNames.indices.contains(o) ? outputNames[o] : ""
        var text = name.isEmpty ? "\(o + 1)" : name.replacingOccurrences(of: "\n", with: " ")
        if !hidesGangs, let g = levels?.gang(of: o) { text += " " + LevelSet.gangLetter(g) }
        return Self.middleCut(text, width: Self.cellW - 4, font: Self.labelFont)
    }

    /// `text`, or its start and end with an ellipsis between, as wide as `width` at most.
    static func middleCut(_ text: String, width: CGFloat, font: NSFont) -> String {
        func w(_ s: String) -> CGFloat { (s as NSString).size(withAttributes: [.font: font]).width }
        guard w(text) > width else { return text }
        let chars = Array(text)
        var keep = chars.count - 1
        while keep > 0 {
            let head = keep - keep / 2
            let candidate = String(chars.prefix(head)) + "…" + String(chars.suffix(keep - head))
            if w(candidate) <= width { return candidate }
            keep -= 1
        }
        return "…"
    }

    private func centred(_ text: String, in r: NSRect, attributes: [NSAttributedString.Key: Any]) {
        let s = NSAttributedString(string: text, attributes: attributes)
        let size = s.size()
        s.draw(at: NSPoint(x: r.midX - size.width / 2, y: r.midY - size.height / 2))
    }

    // MARK: Mouse and keys

    override func mouseDown(with event: NSEvent) {
        onPress?(true)
        window?.makeFirstResponder(self)
        let p = convert(event.locationInWindow, from: nil)
        dragSlot = slot(at: p)
        dragging = false
        guard let s = dragSlot else { return }
        selection = s
        onSelect?(s)
        if event.clickCount == 2, !isLocked {
            dragSlot = nil
            onPress?(false)
            return onType?(s) ?? ()
        }
        dragStartY = p.y
        // From -inf a drag starts at the bottom of the fader's travel.
        dragStartDb = max(levels?.db(s) ?? limits.min, limits.min)
    }

    override func mouseDragged(with event: NSEvent) {
        guard !isLocked, let s = dragSlot, levels != nil else { return }
        let dy = dragStartY - convert(event.locationInWindow, from: nil).y  // up is louder
        guard dragging || abs(dy) >= 3 else { return }
        let first = !dragging
        dragging = true
        // QLab keeps a drag from passing 0 dB (manual p. 181); a level already above it may move down.
        // No drag passes the workspace's maximum.
        let ceiling = min(max(0, dragStartDb), limits.max)
        let db = min(ceiling, (dragStartDb + Double(dy) * 0.25).rounded(toPlaces: 1))
        onDrag?(s, db, first)
    }

    override func mouseUp(with event: NSEvent) {
        if dragging { onDragEnd?() }
        dragging = false
        dragSlot = nil
        onPress?(false)
    }

    /// The gesture is over, mouse-up or not: later events of this press do nothing.
    func forgetPress() {
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
        case 36, 76: return isLocked ? () : onType?(selection) ?? ()
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
