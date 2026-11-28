// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The stage editor's resize dialog (QLab 5 manual p. 227, UI map `dialog.resize-stage.demo`): a
/// small window of its own, run modally over the stage editor, with the new width and height, a box
/// that keeps the stage's shape (ticked: typing one side sets the other), what happens to the
/// regions (scaled with the stage, or kept at their pixel size about the centre, cut to the new
/// stage or padded around), a picture of the new shape, Cancel and OK. OK hands the new size and,
/// for crop or pad, the regions as they become; Cancel hands nothing.
@MainActor
final class ResizeStageDialog: NSWindowController, NSTextFieldDelegate {
    /// What the regions do when the stage changes size.
    enum Mode: Equatable {
        /// Scaled with the stage (the engine's own rule: edges rounded, kept inside).
        case scale
        /// Kept at their pixel size and place about the stage's centre, cut at the new stage's
        /// edges; one that would fall wholly outside is moved in to the nearest edge.
        case cropOrPad
    }

    struct Result: Equatable {
        var width: Int
        var height: Int
        var mode: Mode
        /// The regions as they become (crop or pad); the old ones for scale (the engine scales them).
        var regions: [RegionRow]
    }

    let widthField = NSTextField()
    let heightField = NSTextField()
    let keepShapeCheck = NSButton(checkboxWithTitle: loc("resize.keepShape"), target: nil, action: nil)
    let modePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let preview = ResizePreview()
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    let cancelButton = NSButton(title: loc("routeEditor.cancel"), target: nil, action: nil)
    let okButton = NSButton(title: loc("routeEditor.ok"), target: nil, action: nil)
    /// The stage's size and regions when the dialog opened.
    let original: (width: Int, height: Int)
    let regions: [RegionRow]
    /// Told once: the result on OK, nil on Cancel.
    var onDone: ((Result?) -> Void)?
    private var done = false

    static let id = "resize-stage"

    init(width: Int, height: Int, regions: [RegionRow]) {
        original = (width, height)
        self.regions = regions
        let window = PanicKeyWindow(
            contentRect: NSRect(x: 0, y: 0, width: 380, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.title = loc("resize.title")
        window.identifier = NSUserInterfaceItemIdentifier(Self.id)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        let content = build()
        window.contentView = content
        widthField.stringValue = String(width)
        heightField.stringValue = String(height)
        updatePreview()
        content.layoutSubtreeIfNeeded()
        window.setContentSize(content.fittingSize)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    private func label(_ key: String) -> NSTextField {
        let l = NSTextField(labelWithString: loc(key))
        l.alignment = .right
        return l
    }

    private func build() -> NSView {
        for (f, id, key) in [
            (widthField, "width", "settings.video.width"), (heightField, "height", "settings.video.height"),
        ] {
            f.setAX("\(Self.id)-\(id)", label: loc(key))
            f.alignment = .right
            f.font = .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
            f.delegate = self
            f.widthAnchor.constraint(equalToConstant: 64).isActive = true
        }
        keepShapeCheck.setAX("\(Self.id)-keep-shape", label: loc("resize.keepShape"))
        keepShapeCheck.state = .on
        keepShapeCheck.target = self
        keepShapeCheck.action = #selector(keepShapeClicked)
        modePopup.setAX("\(Self.id)-mode", label: loc("resize.regions"))
        modePopup.addItem(withTitle: loc("resize.mode.scale"))
        modePopup.addItem(withTitle: loc("resize.mode.cropOrPad"))
        modePopup.target = self
        modePopup.action = #selector(modeChosen)
        preview.setAX("\(Self.id)-preview", label: loc("resize.preview"))
        preview.widthAnchor.constraint(equalToConstant: 300).isActive = true
        preview.heightAnchor.constraint(equalToConstant: 150).isActive = true
        messageLabel.setAX("\(Self.id)-message")
        messageLabel.textColor = .systemRed
        messageLabel.preferredMaxLayoutWidth = 320
        cancelButton.setAX("\(Self.id)-cancel")
        cancelButton.bezelStyle = .rounded
        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.target = self
        cancelButton.action = #selector(cancelClicked)
        okButton.setAX("\(Self.id)-ok")
        okButton.bezelStyle = .rounded
        okButton.keyEquivalent = "\r"
        okButton.target = self
        okButton.action = #selector(okClicked)
        let by = NSTextField(labelWithString: "×")
        by.textColor = .secondaryLabelColor
        let size = NSStackView(views: [widthField, by, heightField])
        size.spacing = 6
        let grid = NSGridView(views: [
            [label("resize.size"), size],
            [NSGridCell.emptyContentView, keepShapeCheck],
            [label("resize.regions"), modePopup],
        ])
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .firstBaseline
        grid.rowSpacing = 10
        grid.columnSpacing = 8
        let buttons = NSStackView(views: [cancelButton, okButton])
        buttons.spacing = 10
        let stack = NSStackView(views: [grid, preview, messageLabel, buttons])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 14
        stack.edgeInsets = NSEdgeInsets(top: 18, left: 20, bottom: 16, right: 20)
        // A centred stack's fitting width leaves out its side insets; pinned, the window keeps them.
        stack.widthAnchor.constraint(equalTo: preview.widthAnchor, constant: 40).isActive = true
        buttons.setContentHuggingPriority(.required, for: .horizontal)
        return stack
    }

    /// Runs the dialog modally over `parent` (the stage standing back meanwhile, as for any
    /// question); returns when it is done.
    func run(over parent: NSWindow) {
        guard let window else { return }
        let p = parent.frame
        window.setFrameTopLeftPoint(NSPoint(x: p.midX - window.frame.width / 2, y: p.maxY - 60))
        _ = StandBack.runModal(window)
        window.orderOut(nil)
        // A modal run ended from outside (a quit aborts it) is a Cancel.
        finish(nil)
    }

    private func finish(_ result: Result?) {
        guard !done else { return }
        done = true
        if let window, NSApp.modalWindow === window { NSApp.stopModal() }
        window?.orderOut(nil)
        onDone?(result)
    }

    /// Ends the dialog as Cancel does (its stage went).
    func cancel() { finish(nil) }

    @objc private func cancelClicked() { finish(nil) }

    @objc private func keepShapeClicked() {
        if keepShapeCheck.state == .on { follow(from: widthField) }
    }

    @objc private func modeChosen() { updatePreview() }

    var mode: Mode { modePopup.indexOfSelectedItem == 1 ? .cropOrPad : .scale }

    /// The side typed in `field`, if it is a whole number of pixels a stage may have.
    private func side(_ field: NSTextField) -> Int? {
        guard let n = Int(field.stringValue.trimmingCharacters(in: .whitespaces)), (1...MaxStagePixels.side).contains(n)
        else { return nil }
        return n
    }

    /// With the shape kept, the other side follows the one typed in `field`.
    private func follow(from field: NSTextField) {
        guard keepShapeCheck.state == .on, let n = side(field), original.width > 0, original.height > 0 else { return }
        if field === widthField {
            let h = Int((Double(n) * Double(original.height) / Double(original.width)).rounded())
            heightField.stringValue = String(min(MaxStagePixels.side, max(1, h)))
        } else {
            let w = Int((Double(n) * Double(original.width) / Double(original.height)).rounded())
            widthField.stringValue = String(min(MaxStagePixels.side, max(1, w)))
        }
    }

    private func updatePreview() {
        let w = side(widthField) ?? original.width
        let h = side(heightField) ?? original.height
        preview.newSize = NSSize(width: w, height: h)
        preview.regions =
            mode == .cropOrPad
            ? Self.cropOrPad(regions, from: original, to: (w, h)).map(\.bounds)
            : regions.map { Self.scaled($0.bounds, from: original, to: (w, h)) }
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        follow(from: field)
        updatePreview()
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        follow(from: field)
        updatePreview()
    }

    /// OK: a size outside 1 to 16384 keeps the dialog up saying why; an unchanged size is Cancel.
    @objc func okClicked() {
        guard let w = side(widthField), let h = side(heightField) else {
            messageLabel.stringValue = loc("settings.video.badSize")
            return
        }
        guard (w, h) != original else { return finish(nil) }
        let mode = self.mode
        let shaped = mode == .cropOrPad ? Self.cropOrPad(regions, from: original, to: (w, h)) : regions
        finish(Result(width: w, height: h, mode: mode, regions: shaped))
    }

    /// The regions of a stage resized from `old` to `new` keeping their pixel size and their place
    /// relative to the stage's centre (the stage grows or shrinks evenly on each side), then cut at
    /// the new stage's edges. A region left with nothing inside is moved in to the nearest edge with
    /// its size (no larger than the stage), so none is lost.
    static func cropOrPad(_ regions: [RegionRow], from old: (width: Int, height: Int), to new: (Int, Int))
        -> [RegionRow]
    {
        let dx = Int((Double(new.0 - old.width) / 2).rounded(.down))
        let dy = Int((Double(new.1 - old.height) / 2).rounded(.down))
        return regions.map { r in
            var out = r
            let (x, w) = fit(start: r.bounds.x + dx, length: r.bounds.width, within: new.0)
            let (y, h) = fit(start: r.bounds.y + dy, length: r.bounds.height, within: new.1)
            out.bounds = RegionBounds(x: x, y: y, width: w, height: h)
            return out
        }
    }

    /// A span from `start` of `length` cut to `0..<limit`; one wholly outside is put at the nearest
    /// end, at most `limit` long.
    private static func fit(start: Int, length: Int, within limit: Int) -> (Int, Int) {
        let lo = max(0, start)
        let hi = min(limit, start + length)
        if hi - lo >= 1 { return (lo, hi - lo) }
        let kept = min(length, limit)
        return start + length <= 0 ? (0, kept) : (limit - kept, kept)
    }

    /// A region scaled with its stage, for the preview (the engine does the same when it applies).
    static func scaled(_ b: RegionBounds, from old: (width: Int, height: Int), to new: (Int, Int)) -> RegionBounds {
        guard old.width > 0, old.height > 0 else { return b }
        let sx = Double(new.0) / Double(old.width)
        let sy = Double(new.1) / Double(old.height)
        let x0 = Int((Double(b.x) * sx).rounded())
        let y0 = Int((Double(b.y) * sy).rounded())
        let x1 = min(new.0, Int((Double(b.x + b.width) * sx).rounded()))
        let y1 = min(new.1, Int((Double(b.y + b.height) * sy).rounded()))
        return RegionBounds(x: x0, y: y0, width: max(1, x1 - x0), height: max(1, y1 - y0))
    }
}

/// The resize dialog's picture: the new stage's shape with its regions as they will be (our own
/// drawing).
@MainActor
final class ResizePreview: NSView {
    var newSize = NSSize(width: 1920, height: 1080) { didSet { needsDisplay = true } }
    var regions: [RegionBounds] = [] { didSet { needsDisplay = true } }

    nonisolated override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        guard newSize.width > 0, newSize.height > 0 else { return }
        let room = bounds.insetBy(dx: 4, dy: 4)
        let scale = min(room.width / newSize.width, room.height / newSize.height)
        let stage = NSRect(
            x: room.midX - newSize.width * scale / 2, y: room.midY - newSize.height * scale / 2,
            width: newSize.width * scale, height: newSize.height * scale)
        NSColor(white: 0.92, alpha: 1).setFill()
        stage.fill()
        for (i, r) in regions.enumerated() {
            let f = NSRect(
                x: stage.minX + CGFloat(r.x) * scale, y: stage.minY + CGFloat(r.y) * scale,
                width: CGFloat(r.width) * scale, height: CGFloat(r.height) * scale)
            RegionColour.of(i).withAlphaComponent(0.35).setFill()
            f.fill()
            RegionColour.of(i).setStroke()
            NSBezierPath(rect: f.insetBy(dx: 0.5, dy: 0.5)).stroke()
        }
        NSColor(white: 0.3, alpha: 1).setStroke()
        NSBezierPath(rect: stage.insetBy(dx: 0.5, dy: 0.5)).stroke()
    }
}
