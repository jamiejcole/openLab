// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The routing matrix of the patch editor (QLab 5 manual p. 190): a row per cue output (its number
/// and name, a level box laid over it by the editor, its meter) and a column per device output. Each
/// box is that crosspoint's level: lit with its level where the cue output reaches the device output,
/// empty where it does not. A double-click (or Return on the chosen box) types a level in it. Drawn
/// by hand and only where visible, so 128 rows by 64 columns is one view. Device outputs openLab
/// cannot use on this device are drawn dimmed and take no typing, except to clear routing the show
/// holds for them.
@MainActor
final class PatchRoutingView: NSView {
    static let rowH: CGFloat = 22
    static let headerH: CGFloat = 22
    static let labelW: CGFloat = 132
    static let levelW: CGFloat = 60
    static let meterW: CGFloat = 52
    static let cellW: CGFloat = 46
    static var crossX: CGFloat { labelW + levelW + meterW }

    /// Each cue output's label: its number, and its name when it has one.
    var rowLabels: [String] = [] {
        didSet { if rowLabels != oldValue { resize() } }
    }
    /// The device outputs drawn (every output the device has, and any the routing names past them).
    var deviceOutputs = 0 {
        didSet { if deviceOutputs != oldValue { resize() } }
    }
    /// The device outputs this device plays (the first `usable`).
    var usable = 0 {
        didSet {
            if usable != oldValue {
                needsDisplay = true
                onChange?()
            }
        }
    }
    /// The levels drawn in the boxes.
    var routing = PatchRouting() {
        didSet {
            if routing != oldValue {
                needsDisplay = true
                onChange?()
            }
        }
    }
    var isEnabled = true {
        didSet { if isEnabled != oldValue { needsDisplay = true } }
    }
    /// The box chosen by a click (cue output, device output), outlined.
    var selected: (row: Int, column: Int)? {
        didSet { needsDisplay = true }
    }
    /// A box was double-clicked, or Return pressed on the chosen one: type a level in it.
    var onType: ((_ row: Int, _ column: Int) -> Void)?
    /// The matrix's size or contents changed: the panes that float over it follow.
    var onChange: (() -> Void)?

    nonisolated override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { isEnabled }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    /// The width of what shows the matrix: it is at least that wide, so row stripes reach across.
    var minWidth: CGFloat = 0 {
        didSet { if minWidth != oldValue { resize() } }
    }

    private func resize() {
        setFrameSize(
            NSSize(
                width: max(minWidth, Self.crossX + CGFloat(deviceOutputs) * Self.cellW + 8),
                height: Self.headerH + CGFloat(rowLabels.count) * Self.rowH + 4))
        if let s = selected, s.row >= rowLabels.count || s.column >= deviceOutputs { selected = nil }
        needsDisplay = true
        onChange?()
    }

    /// Whether the box of cue output `row` and device output `column` can be typed in.
    func canType(row: Int, column: Int) -> Bool {
        isEnabled && row < rowLabels.count && column < deviceOutputs
            && (column < usable || routing.isListed(row, column))
    }

    // MARK: Geometry

    func levelRect(row: Int) -> NSRect {
        NSRect(
            x: Self.labelW + 2, y: Self.headerH + CGFloat(row) * Self.rowH + 1, width: Self.levelW - 6,
            height: Self.rowH - 2)
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

    /// The box itself: where a level is drawn and typed.
    func boxRect(row: Int, column: Int) -> NSRect {
        cellRect(row: row, column: column).insetBy(dx: 2, dy: 2)
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
        let firstCol = max(0, Int((dirty.minX - Self.crossX) / Self.cellW))
        let lastCol = min(deviceOutputs - 1, Int((dirty.maxX - Self.crossX) / Self.cellW))
        let firstRow = max(0, Int((dirty.minY - Self.headerH) / Self.rowH))
        let lastRow = min(rowLabels.count - 1, Int((dirty.maxY - Self.headerH) / Self.rowH))
        guard firstRow <= lastRow else { return }
        for row in firstRow...lastRow {
            let y = Self.headerH + CGFloat(row) * Self.rowH
            if row % 2 == 1 {
                NSColor.labelColor.withAlphaComponent(0.04).setFill()
                NSRect(x: 0, y: y, width: bounds.width, height: Self.rowH).fill()
            }
            guard firstCol <= lastCol else { continue }
            for c in firstCol...lastCol { box(row: row, column: c) }
        }
    }

    private static let levelFont = NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .medium)

    private func box(row: Int, column: Int) {
        let r = boxRect(row: row, column: column)
        let path = NSBezierPath(roundedRect: r, xRadius: 3, yRadius: 3)
        let level = routing.level(row, column)
        let usableHere = column < usable
        let deviceMuted = routing.deviceOutput(column).mute
        if level != nil {
            let lit: NSColor = usableHere ? .controlAccentColor : .systemRed
            lit.withAlphaComponent(!isEnabled ? 0.4 : deviceMuted ? 0.45 : 0.85).setFill()
        } else {
            // Empty reads as empty: the field colour on this device's outputs, a faint wash past them.
            (usableHere ? NSColor.textBackgroundColor : NSColor.labelColor.withAlphaComponent(0.06)).setFill()
        }
        path.fill()
        let isSelected = selected.map { $0.row == row && $0.column == column } ?? false
        (isSelected ? NSColor.keyboardFocusIndicatorColor : NSColor.separatorColor).setStroke()
        path.lineWidth = isSelected ? 2 : 0.5
        path.stroke()
        guard let level else { return }
        let text = NSAttributedString(
            string: LevelText.format(level),
            attributes: [.font: Self.levelFont, .foregroundColor: NSColor.white])
        let size = text.size()
        text.draw(at: NSPoint(x: r.midX - size.width / 2, y: r.midY - size.height / 2))
    }

    // MARK: The floating panes

    /// The header row: what each column is (fixed above the rows as they scroll).
    private static let small: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 10), .foregroundColor: NSColor.secondaryLabelColor,
    ]

    /// The titles of the left columns, in the corner that stays put however the matrix scrolls.
    func drawCorner() {
        draw(
            loc("patchEditor.cueOutputs"), in: NSRect(x: 4, y: 0, width: Self.labelW - 8, height: Self.headerH),
            Self.small)
        centred(
            loc("patchEditor.level"), in: NSRect(x: Self.labelW, y: 0, width: Self.levelW, height: Self.headerH),
            Self.small)
    }

    /// The device outputs' numbers over their columns (the corner covers the left columns' part).
    func drawHeader(in pane: NSRect) {
        let small = Self.small
        let muted: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10, weight: .semibold), .foregroundColor: NSColor.systemRed,
        ]
        // Columns scrolled under the corner (over the left columns) are not drawn: no digit of
        // theirs shows at its edge.
        let shown = visibleRect.minX + Self.crossX
        for c in 0..<deviceOutputs {
            let r = NSRect(x: Self.crossX + CGFloat(c) * Self.cellW, y: 0, width: Self.cellW, height: Self.headerH)
            guard r.intersects(pane), r.maxX > shown else { continue }
            // A muted device output says so over its column.
            if routing.deviceOutput(c).mute {
                centred(String(format: loc("patchEditor.mutedColumn"), c + 1), in: r, muted)
            } else {
                centred("\(c + 1)", in: r, small)
            }
        }
    }

    /// The left columns' labels and stripes (fixed beside the boxes as they scroll sideways),
    /// in the pane's own coordinates (row 0 at the top).
    func drawRowLabels(in dirty: NSRect, width: CGFloat) {
        let label: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.labelColor,
            .paragraphStyle: Self.truncating,
        ]
        let firstRow = max(0, Int(dirty.minY / Self.rowH))
        let lastRow = min(rowLabels.count - 1, Int(dirty.maxY / Self.rowH))
        guard firstRow <= lastRow else { return }
        for row in firstRow...lastRow {
            let y = CGFloat(row) * Self.rowH
            if row % 2 == 1 {
                NSColor.labelColor.withAlphaComponent(0.04).setFill()
                NSRect(x: 0, y: y, width: width, height: Self.rowH).fill()
            }
            draw(rowLabels[row], in: NSRect(x: 4, y: y, width: Self.labelW - 8, height: Self.rowH), label)
        }
    }

    private static let truncating: NSParagraphStyle = {
        let p = NSMutableParagraphStyle()
        p.lineBreakMode = .byTruncatingTail
        return p
    }()

    private func draw(_ text: String, in r: NSRect, _ attributes: [NSAttributedString.Key: Any]) {
        let s = NSAttributedString(string: text, attributes: attributes)
        let h = s.size().height
        s.draw(
            with: NSRect(x: r.minX, y: r.midY - h / 2, width: r.width, height: h),
            options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    private func centred(_ text: String, in r: NSRect, _ attributes: [NSAttributedString.Key: Any]) {
        let s = NSAttributedString(string: text, attributes: attributes)
        let size = s.size()
        s.draw(at: NSPoint(x: r.midX - size.width / 2, y: r.midY - size.height / 2))
    }

    // MARK: Mouse and keys

    override func mouseDown(with event: NSEvent) {
        guard isEnabled, let hit = cell(at: convert(event.locationInWindow, from: nil)) else { return }
        selected = hit
        window?.makeFirstResponder(self)
        if event.clickCount == 2, canType(row: hit.row, column: hit.column) { onType?(hit.row, hit.column) }
    }

    override func keyDown(with event: NSEvent) {
        guard let s = selected else { return super.keyDown(with: event) }
        switch event.keyCode {
        case 36, 76:
            if canType(row: s.row, column: s.column) { onType?(s.row, s.column) }
        case 123: selected = (s.row, max(0, s.column - 1))
        case 124: selected = (s.row, min(deviceOutputs - 1, s.column + 1))
        case 125: selected = (min(rowLabels.count - 1, s.row + 1), s.column)
        case 126: selected = (max(0, s.row - 1), s.column)
        default: super.keyDown(with: event)
        }
    }

    // MARK: Accessibility

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    /// Every crosspoint heard, as "cue output→device output level".
    override func accessibilityValue() -> Any? {
        var parts: [String] = []
        for row in rowLabels.indices {
            for column in 0..<deviceOutputs {
                if let db = routing.level(row, column) {
                    parts.append("\(row + 1)→\(column + 1) \(LevelText.format(db))")
                }
            }
        }
        return parts.joined(separator: ", ")
    }
}

/// A part of the routing matrix that stays in view as the matrix scrolls: the header row (above the
/// rows, as they scroll up and down), the left columns with the cue outputs' labels, level boxes
/// and meter (beside the boxes, as they scroll sideways), or the corner where the two meet, with the
/// left columns' titles, which stays put either way. Opaque, so what scrolls under it is hidden.
@MainActor
final class PatchRoutingPane: NSView {
    enum Part { case header, rows, corner }
    let part: Part
    weak var matrix: PatchRoutingView?

    init(_ part: Part) {
        self.part = part
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    nonisolated override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }

    override func draw(_ dirty: NSRect) {
        NSBezierPath.clip(bounds)
        NSColor.windowBackgroundColor.setFill()
        dirty.fill()
        switch part {
        case .header: matrix?.drawHeader(in: dirty)
        case .rows: matrix?.drawRowLabels(in: dirty, width: bounds.width)
        case .corner: matrix?.drawCorner()
        }
    }
}

/// A scroll view that says when it has laid out its clip view and scrollers, so what sits beside
/// them follows.
@MainActor
final class TilingScrollView: NSScrollView {
    var onTile: (() -> Void)?

    override func tile() {
        super.tile()
        onTile?()
    }

    override func layout() {
        super.layout()
        onTile?()
    }
}
