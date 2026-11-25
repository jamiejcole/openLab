// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The routing matrix of the patch editor (QLab 5 manual p. 190): a row per cue output (its number
/// and name, a level box laid over it by the editor, its meter) and a column per device output, under a
/// strip per column (the fader, mute and level box the editor lays over the header, and the label drawn
/// here) and the captions of the left columns, the inputs and the crosspoints. Each
/// box is that crosspoint's level: lit with its level where the cue output reaches the device output,
/// empty where it does not. A double-click (or Return on the chosen box) types a level in it. Drawn
/// by hand and only where visible, so 128 rows by 64 columns is one view. Device outputs openLab
/// cannot use on this device are drawn dimmed and take no typing, except to clear routing the show
/// holds for them.
@MainActor
final class PatchRoutingView: NSView {
    static let rowH: CGFloat = 22
    /// The strips and their captions: the header is as tall as both.
    static let headerH: CGFloat = 178
    static let labelW: CGFloat = 92
    static let levelW: CGFloat = 45
    /// The meter's column, drawn only while a cue plays through the row: a silent row shows nothing there.
    static let meterW: CGFloat = 20
    static let cellW: CGFloat = 43
    static var crossX: CGFloat { labelW + levelW + meterW }

    // The strip's parts, top to bottom, in the header's coordinates.
    static let faderRect = (y: CGFloat(4), h: CGFloat(86), w: CGFloat(20))
    static let muteRect = (y: CGFloat(94), h: CGFloat(16), w: CGFloat(24))
    static let stripBoxY: CGFloat = 114
    static let stripLabelY: CGFloat = 138
    static let captionY: CGFloat = 156

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
            x: Self.labelW + Self.levelW + 2, y: Self.headerH + 4, width: Self.meterW - 8,
            height: CGFloat(rows) * Self.rowH - 6)
    }

    /// Where a strip's fader goes: centred on `midX`.
    static func faderFrame(midX: CGFloat) -> NSRect {
        NSRect(x: midX - faderRect.w / 2, y: faderRect.y, width: faderRect.w, height: faderRect.h)
    }

    /// Where a device output's mute toggle goes.
    static func muteFrame(midX: CGFloat) -> NSRect {
        NSRect(x: midX - muteRect.w / 2, y: muteRect.y, width: muteRect.w, height: muteRect.h)
    }

    /// Where a strip's level box goes: as wide as a crosspoint box, so the two line up.
    static func stripBoxFrame(midX: CGFloat) -> NSRect {
        NSRect(x: midX - (cellW - 4) / 2, y: stripBoxY, width: cellW - 4, height: rowH - 2)
    }

    /// Where the main strip's level box goes: exactly over the cue outputs' level boxes below it.
    static var mainBoxFrame: NSRect {
        NSRect(x: labelW + 2, y: stripBoxY, width: levelW - 6, height: rowH - 2)
    }

    /// The middle of the strip over device output `column`: the middle of its crosspoint column.
    static func stripMidX(column: Int) -> CGFloat { crossX + (CGFloat(column) + 0.5) * cellW }

    /// The middle of the main strip: over the input boxes.
    static var mainMidX: CGFloat { labelW + 2 + (levelW - 6) / 2 }

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

    /// The header's text styles.
    private static let small: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 10), .foregroundColor: NSColor.secondaryLabelColor,
    ]
    private static let strip: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 10, weight: .medium), .foregroundColor: NSColor.labelColor,
    ]

    /// The main strip's label and the captions of the left columns, in the corner that stays put
    /// however the matrix scrolls.
    func drawCorner() {
        centred(
            loc("patchEditor.main"),
            in: NSRect(x: Self.mainMidX - 24, y: Self.stripLabelY, width: 48, height: 14), Self.strip)
        draw(
            loc("patchEditor.cueOutputs"),
            in: NSRect(x: 4, y: Self.captionY, width: Self.labelW - 8, height: 18), Self.small)
        centred(
            loc("patchEditor.inputs"),
            in: NSRect(x: Self.labelW + 2, y: Self.captionY, width: Self.levelW - 6, height: 18), Self.small)
    }

    /// The device outputs' labels under their strips and the crosspoints caption (the corner covers
    /// the left columns' part).
    func drawHeader(in pane: NSRect) {
        let muted: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10, weight: .semibold), .foregroundColor: NSColor.systemRed,
        ]
        // Columns scrolled under the corner (over the left columns) are not drawn: no digit of
        // theirs shows at its edge.
        let shown = visibleRect.minX + Self.crossX
        for c in 0..<deviceOutputs {
            let r = NSRect(x: Self.crossX + CGFloat(c) * Self.cellW, y: Self.stripLabelY, width: Self.cellW, height: 14)
            guard r.intersects(pane), r.maxX > shown else { continue }
            // A muted device output says so under its strip.
            if routing.deviceOutput(c).mute {
                centred(String(format: loc("patchEditor.mutedColumn"), c + 1), in: r, muted)
            } else {
                centred("\(c + 1)", in: r, Self.strip)
            }
        }
        if deviceOutputs > 0 {
            draw(
                loc("patchEditor.crosspoints"),
                in: NSRect(x: Self.crossX + 2, y: Self.captionY, width: 160, height: 18), Self.small)
        }
    }

    /// The left columns' labels and stripes (fixed beside the boxes as they scroll sideways),
    /// in the pane's own coordinates (row 0 at the top).
    func drawRowLabels(in dirty: NSRect, width: CGFloat) {
        let label: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.labelColor,
            .paragraphStyle: Self.rowLabelStyle,
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
            draw(rowLabels[row], in: Self.rowLabelRect(y: y), label)
        }
    }

    /// A cue output's label sits against its level box, right-aligned.
    static let rowLabelStyle: NSParagraphStyle = {
        let p = NSMutableParagraphStyle()
        p.lineBreakMode = .byTruncatingTail
        p.alignment = .right
        return p
    }()

    /// Where a row's label is drawn, in the left pane's coordinates, `y` the row's top.
    static func rowLabelRect(y: CGFloat) -> NSRect {
        NSRect(x: 4, y: y, width: labelW - 8, height: rowH)
    }

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

/// A level box laid over the routing matrix: drawn as the matrix draws its own boxes (a plain rounded
/// outline, no bezel), so that its text has room in a box as narrow as a crosspoint's.
@MainActor
final class LevelBoxField: NSTextField {
    override class var cellClass: AnyClass? {
        get { LevelBoxCell.self }
        set {}
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        isBezeled = false
        isBordered = false
        drawsBackground = false
        focusRingType = .none
        wantsLayer = true
        layer?.cornerRadius = 3
        layer?.borderWidth = 0.5
        refreshColours()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    private func refreshColours() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.textBackgroundColor.cgColor
            layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        refreshColours()
    }
}

/// Draws and edits its text in the middle of the box, as the matrix draws its own levels.
final class LevelBoxCell: NSTextFieldCell {
    override func drawingRect(forBounds rect: NSRect) -> NSRect {
        var r = super.drawingRect(forBounds: rect)
        let h = cellSize(forBounds: r).height
        if h < r.height {
            r.origin.y += (r.height - h) / 2
            r.size.height = h
        }
        return r
    }
}

/// A part of the routing matrix that stays in view as the matrix scrolls: the header row (above the
/// rows, as they scroll up and down), the left columns with the cue outputs' labels and level boxes
/// (beside the boxes, as they scroll sideways), or the corner where the two meet, with the
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
