// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// One row of the outline: a cue of the shown list, with its children when it is a group. A class
/// because `NSOutlineView` identifies items by object identity.
final class CueNode: NSObject {
    let id: ModelID
    var cue: Cue
    weak var parent: CueNode?
    var children: [CueNode] = []

    init(cue: Cue, parent: CueNode?) {
        id = cue.id
        self.cue = cue
        self.parent = parent
    }

    var isGroup: Bool { cue.kindType == "group" }
    var targetID: ModelID? { cue.raw["kind"]?["target"]?.stringValue.map { ModelID($0) } }
}

/// Row background: the cue's colour as a tint, and the playhead's pointer on its row: a solid
/// triangle at the row's left edge, as tall as the row, pointing at the cue (QLab's playhead).
final class CueRowView: NSTableRowView {
    var tint: NSColor? { didSet { if tint != oldValue { needsDisplay = true } } }
    var isPlayhead = false { didSet { if isPlayhead != oldValue { needsDisplay = true } } }
    /// Disarmed or skipped: drawn over the colour, and over the highlight of a selected row.
    var pattern: RowPattern? { didSet { if pattern != oldValue { needsDisplay = true } } }
    /// The parts of the group boxes that cross this row's Name cell (`GroupBoxes`).
    var boxes: [GroupBoxPart] = [] { didSet { if boxes != oldValue { needsDisplay = true } } }
    static let pointerWidth: CGFloat = 10
    /// The distance between a pattern's lines.
    static let patternPitch: CGFloat = 8

    override func drawBackground(in dirtyRect: NSRect) {
        super.drawBackground(in: dirtyRect)
        if let tint {
            tint.withAlphaComponent(0.22).setFill()
            bounds.fill()
        }
        if !isSelected {
            drawPattern()
            drawPointer()
            drawBoxes()
        }
    }

    override func drawSelection(in dirtyRect: NSRect) {
        super.drawSelection(in: dirtyRect)
        drawPattern()
        drawPointer()
        drawBoxes()
    }

    /// The Name column's rect in this row's coordinates and the outline's indentation per level, from
    /// the table the row is in; nil outside one.
    var nameColumn: (rect: NSRect, indent: CGFloat)? {
        var view = superview
        while let v = view, !(v is NSOutlineView) { view = v.superview }
        guard let table = view as? NSOutlineView else { return nil }
        let index = table.column(withIdentifier: NSUserInterfaceItemIdentifier("name"))
        guard index >= 0 else { return nil }
        let rect = convert(table.rect(ofColumn: index), from: table)
        return (
            NSRect(x: rect.minX, y: bounds.minY, width: rect.width, height: bounds.height), table.indentationPerLevel
        )
    }

    private func drawBoxes() {
        guard !boxes.isEmpty, let column = nameColumn else { return }
        GroupBoxes.draw(boxes, rowBounds: bounds, column: column.rect, indent: column.indent, flipped: isFlipped)
    }

    private func drawPattern() {
        guard let pattern else { return }
        NSGraphicsContext.saveGraphicsState()
        defer { NSGraphicsContext.restoreGraphicsState() }
        NSBezierPath.clip(bounds)
        let pitch = Self.patternPitch
        let path = NSBezierPath()
        switch pattern {
        case .disarmed:
            // Lines rising to the right, one every `pitch` along the row.
            var x = bounds.minX - bounds.height
            while x < bounds.maxX {
                path.move(to: NSPoint(x: x, y: bounds.minY))
                path.line(to: NSPoint(x: x + bounds.height, y: bounds.maxY))
                x += pitch
            }
            path.lineWidth = 1.5
        case .skipped:
            // Upright lines that swing a little to each side, a full swing every `pitch` of height.
            var x = bounds.minX + pitch / 2
            while x < bounds.maxX {
                path.move(to: NSPoint(x: x, y: bounds.minY))
                var y = bounds.minY
                var side: CGFloat = 1
                while y < bounds.maxY {
                    path.curve(
                        to: NSPoint(x: x, y: y + pitch / 2), controlPoint1: NSPoint(x: x + 2.5 * side, y: y),
                        controlPoint2: NSPoint(x: x + 2.5 * side, y: y + pitch / 2))
                    y += pitch / 2
                    side = -side
                }
                x += pitch
            }
            path.lineWidth = 1
        }
        let onHighlight = isSelected && isEmphasized
        (onHighlight ? NSColor.white.withAlphaComponent(0.3) : NSColor.labelColor.withAlphaComponent(0.16)).setStroke()
        path.stroke()
    }

    private func drawPointer() {
        guard isPlayhead else { return }
        let path = NSBezierPath()
        path.move(to: NSPoint(x: 0, y: 0))
        path.line(to: NSPoint(x: Self.pointerWidth, y: bounds.midY))
        path.line(to: NSPoint(x: 0, y: bounds.height))
        path.close()
        // White on the dark list and on a highlighted row; the text colour on a light list.
        (isSelected && isEmphasized ? NSColor.white : PlayheadStyle.colour).setFill()
        path.fill()
    }
}

/// The outline of the Target cell a dragged cue is over: solid when the drop sets the target, dashed
/// when the pointer is on the row but outside the cell, where the drop moves the cue instead.
final class TargetDropOutline: NSView {
    enum Kind: Equatable { case sets, near }
    var kind: Kind = .sets { didSet { if kind != oldValue { needsDisplay = true } } }
    /// The row under it is highlighted: the solid outline is white there, the accent colour elsewhere.
    var onSelectedRow = false { didSet { if onSelectedRow != oldValue { needsDisplay = true } } }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        let path = NSBezierPath(roundedRect: bounds.insetBy(dx: 1.5, dy: 1.5), xRadius: 5, yRadius: 5)
        path.lineWidth = 2
        switch kind {
        case .sets:
            let colour: NSColor = onSelectedRow ? .white : .controlAccentColor
            colour.withAlphaComponent(0.25).setFill()
            path.fill()
            colour.setStroke()
        case .near:
            path.setLineDash([4, 3], count: 2, phase: 0)
            NSColor.white.setStroke()
        }
        path.stroke()
    }
}

/// The list's clip view: a press on the filler AppKit draws under a short table (outside the table's own
/// frame) deselects as one on the table's empty rows does.
final class BlankDeselectingClipView: NSClipView {
    override func mouseDown(with event: NSEvent) {
        if let table = documentView as? NSTableView {
            window?.makeFirstResponder(table)
            table.deselectAll(nil)
        } else {
            super.mouseDown(with: event)
        }
    }
}

/// The list's outline view: it tells the list when a drag leaves it.
final class CueOutlineView: NSOutlineView {
    var onDragExit: (() -> Void)?
    /// A plain press in the first (playhead) column on a row: the row's index. The press then goes on
    /// as a click on the row; the list uses this to move the playhead when it is not locked to the selection.
    var onStatusPress: ((Int) -> Void)?

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let row = row(at: point)
        if event.clickCount == 1, row >= 0, column(at: point) == 0,
            event.modifierFlags.intersection([.command, .shift, .control, .option]).isEmpty
        {
            onStatusPress?(row)
        }
        // A press on a row's empty space below the cues ends the selection (QLab), with no cue selected.
        if row < 0, event.modifierFlags.intersection([.command, .shift, .control]).isEmpty {
            window?.makeFirstResponder(self)
            deselectAll(nil)
            return
        }
        super.mouseDown(with: event)
    }

    /// Empty rows always left below the last cue (QLab keeps blank rows to click on), so the list never
    /// ends on its last cue.
    static let spareRows = 3

    override func setFrameSize(_ newSize: NSSize) {
        // AppKit makes the table at least as tall as its clip view, which already shows empty rows when the
        // cues are few; only a list that fills its view gets the room added.
        var size = newSize
        if numberOfRows > 0 {
            let spare = CGFloat(Self.spareRows) * (rowHeight + intercellSpacing.height)
            size.height = max(newSize.height, rect(ofRow: numberOfRows - 1).maxY + spare)
        }
        super.setFrameSize(size)
    }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        super.draggingExited(sender)
        onDragExit?()
    }

    // A group opens and closes at once, its disclosure triangle included. While AppKit's slide runs, a
    // change of the columns (the window resized, the sidebar or Show Mode toggled) does not reach the
    // rows outside the slide: their cells stayed where the old columns were, over the new ones, and
    // a running cue's time was drawn twice.
    override func expandItem(_ item: Any?, expandChildren: Bool) {
        withoutAnimation { super.expandItem(item, expandChildren: expandChildren) }
    }

    override func collapseItem(_ item: Any?, collapseChildren: Bool) {
        withoutAnimation { super.collapseItem(item, collapseChildren: collapseChildren) }
    }

    private func withoutAnimation(_ body: () -> Void) {
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0
            context.allowsImplicitAnimation = false
            body()
        }
    }
}

/// A running cue's progress behind its Duration text: the whole cell outlined in the colour (so a playing
/// cue stands out, as QLab 5's does), filled from the left up to `fraction` of its width (nothing drawn
/// when nil).
final class ProgressFillView: NSView {
    var progress: (fraction: Double, colour: NSColor)? {
        didSet {
            if progress?.fraction != oldValue?.fraction || progress?.colour != oldValue?.colour { needsDisplay = true }
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        guard let progress else { return }
        let box = bounds.insetBy(dx: 1, dy: 3)
        let track = NSBezierPath(roundedRect: box, xRadius: 3, yRadius: 3)
        progress.colour.setStroke()
        track.lineWidth = 1
        track.stroke()
        let rect = NSRect(
            x: box.minX, y: box.minY, width: box.width * CGFloat(min(1, max(0, progress.fraction))),
            height: box.height)
        guard rect.width >= 1 else { return }
        progress.colour.withAlphaComponent(0.25).setFill()
        NSBezierPath(roundedRect: rect, xRadius: 3, yRadius: 3).fill()
    }
}

/// A label that truncates at the tail, and draws nothing where not even one letter and the ellipsis fit.
/// AppKit's own tail truncation, given less than that, draws the first letter cut through its middle.
final class WholeGlyphLabel: NSTextField {
    /// Whether `width` holds the first letter and an ellipsis (the least a truncated text can show).
    static func fits(_ text: NSAttributedString, in width: CGFloat) -> Bool {
        if width >= text.size().width { return true }
        guard text.length > 0 else { return true }
        let first = text.attributedSubstring(from: NSRange(location: 0, length: 1)).mutableCopy()
        guard let shown = first as? NSMutableAttributedString else { return true }
        shown.append(NSAttributedString(string: "\u{2026}", attributes: text.attributes(at: 0, effectiveRange: nil)))
        // The cell insets its text a little on each side.
        return width - 4 >= shown.size().width
    }

    override func draw(_ dirtyRect: NSRect) {
        if !Self.fits(attributedStringValue, in: bounds.width) { return }
        super.draw(dirtyRect)
    }
}

/// A cell: up to four small images around one label. The first (status) column has five images, each in
/// a fixed slot (`statusSlots`): playhead (0), state (1), the cue's type icon (4), preview or flag (2, 3).
/// Duration column: `progress` behind the label.
final class CueCell: NSTableCellView {
    /// How far in from the cell's right side the text ends, so the group boxes' right edges never cross
    /// it (`GroupBoxes.textInset`).
    var textInset: CGFloat = 0 { didSet { trailing?.constant = -2 - textInset } }
    private var trailing: NSLayoutConstraint?

    let images = [NSImageView(), NSImageView(), NSImageView(), NSImageView(), NSImageView()]
    /// A fading cue's state mark: a turning cluster of dots over the state slot (image 1 keeps the label and tooltip).
    let fadingDots = PulsingDots()
    /// The first column's slots, left to right, as (image, x from the cell's left edge, width). Each mark
    /// has its own place whether or not the others are shown, so every row's marks line up down the list:
    /// the playhead's room at the row's edge (the row draws the pointer there), the state, then the type
    /// icon beside it (as in QLab), then one slot the preview or override mark and the flag share: QLab shows
    /// a flag in the status area, not a place of its own.
    static let statusSlots: [(image: Int, x: CGFloat, width: CGFloat)] = [
        (0, 0, 10), (1, 10, 16), (4, 28, 16), (2, 46, 16), (3, 46, 16),
    ]
    /// The first column's width: the last slot and a margin. The column cannot be resized.
    static let statusWidth: CGFloat = 64
    /// The colours of the tinted images, kept so a highlighted row can draw them in its text colour.
    private var tints: [Int: (icon: Icon, image: NSImage, tint: NSColor, label: String)] = [:]
    let label = WholeGlyphLabel(labelWithString: "")
    let progress = ProgressFillView()
    /// An Audio cue's button in the Target cell, with or without a file; clicking it calls `onChoose`.
    let chooseButton = NSButton(title: "", target: nil, action: nil)
    var onChoose: (() -> Void)?

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        progress.frame = bounds
        progress.autoresizingMask = [.width, .height]
        addSubview(progress)
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        label.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        for image in images { image.imageScaling = .scaleProportionallyDown }
        if identifier.rawValue == "status" {
            for slot in Self.statusSlots {
                let image = images[slot.image]
                image.translatesAutoresizingMaskIntoConstraints = false
                addSubview(image)
                NSLayoutConstraint.activate([
                    image.leadingAnchor.constraint(equalTo: leadingAnchor, constant: slot.x),
                    image.widthAnchor.constraint(equalToConstant: slot.width),
                    image.heightAnchor.constraint(equalToConstant: 16),
                    image.centerYAnchor.constraint(equalTo: centerYAnchor),
                ])
            }
            fadingDots.isHidden = true
            fadingDots.translatesAutoresizingMaskIntoConstraints = false
            addSubview(fadingDots)
            let slot = images[1]
            NSLayoutConstraint.activate([
                fadingDots.leadingAnchor.constraint(equalTo: slot.leadingAnchor),
                fadingDots.widthAnchor.constraint(equalTo: slot.widthAnchor),
                fadingDots.heightAnchor.constraint(equalTo: slot.heightAnchor),
                fadingDots.centerYAnchor.constraint(equalTo: slot.centerYAnchor),
            ])
        } else {
            let views: [NSView] = [images[0], label, images[1], images[2], images[3]]
            let stack = NSStackView(views: views)
            stack.spacing = 4
            stack.translatesAutoresizingMaskIntoConstraints = false
            addSubview(stack)
            // A column dragged to zero width gives the cell zero width: a required trailing inset then
            // contradicts the stack's own minimum ("Conflicting constraints detected").
            let trailing = stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2)
            trailing.priority = .defaultHigh
            self.trailing = trailing
            NSLayoutConstraint.activate([
                stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
                trailing,
                stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            ])
            for image in images {
                image.setContentHuggingPriority(.required, for: .horizontal)
                image.widthAnchor.constraint(equalToConstant: 16).isActive = true
            }
        }
        chooseButton.isBordered = false
        chooseButton.imagePosition = .imageOnly
        chooseButton.imageScaling = .scaleProportionallyDown
        chooseButton.isHidden = true
        chooseButton.target = self
        chooseButton.action = #selector(chooseClicked)
        chooseButton.translatesAutoresizingMaskIntoConstraints = false
        addSubview(chooseButton)
        NSLayoutConstraint.activate([
            chooseButton.widthAnchor.constraint(equalToConstant: 16),
            chooseButton.centerXAnchor.constraint(equalTo: centerXAnchor),
            chooseButton.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        chooseButton.setAccessibilityIdentifier("cue-target-choose")
        textField = label
        // Cell state is set on every reuse, so no stale identifier survives.
        switch identifier.rawValue {
        case "status":
            images[0].setAccessibilityIdentifier("cue-playhead")
            images[1].setAccessibilityIdentifier("cue-state")
            images[2].setAccessibilityIdentifier("cue-preview")
            images[3].setAccessibilityIdentifier("cue-flag")
            images[4].setAccessibilityIdentifier("cue-type")
        case "continue":
            images[0].setAccessibilityIdentifier("cue-continue")
        case "target":
            label.alignment = .center
        default: break
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    @objc private func chooseClicked() { onChoose?() }

    /// Shows the Target cell's button.
    /// `file`: the cue's file as the show holds it, its tooltip under the label. `copying`: the file is
    /// being copied into the show folder (WS-04), drawn as such until the cue switches to the copy.
    func showChoose(missing: Bool, copying: Bool = false, file: String? = nil, onChoose: @escaping () -> Void) {
        let label = loc(
            copying ? "cuelist.target.copying" : missing ? "cuelist.target.fileMissing" : "inspector.target.choose")
        let icon: Icon = copying ? .targetFileCopying : missing ? .targetFileMissing : .targetFile
        let image = icon.image(label, compact: true)
        // Plain, not a template, for the same reason as `setImage`.
        chooseButton.image = TimeHeaderCell.tinted(
            image, copying ? MarkColour.caution : .secondaryLabelColor, size: image.size, description: label)
        chooseButton.setAccessibilityLabel(label)
        chooseButton.toolTip = file.map { "\(label)\n\($0)" } ?? label
        chooseButton.isHidden = false
        self.onChoose = onChoose
    }

    /// Shows `symbol` in image `i` (hidden when nil) with an accessibility label and tooltip.
    func setImage(_ i: Int, icon: Icon?, label: String?, tint: NSColor? = nil, tooltip: String? = nil) {
        let view = images[i]
        tints[i] = nil
        guard let icon, let label else {
            view.isHidden = true
            view.image = nil
            view.toolTip = nil
            return
        }
        view.isHidden = false
        let image = icon.image(label, compact: true)
        // A tinted icon is drawn as a plain image in its colour, and in the selection's text colour on a
        // highlighted row (QLab draws a selected row's marks so). A clear one only keeps its slot.
        if let tint, tint != .clear { tints[i] = (icon, image, tint, label) }
        view.image = tint.map { tinted(image, $0, label) } ?? image
        view.contentTintColor = nil
        view.setAccessibilityLabel(label)
        view.toolTip = tooltip ?? label
    }

    /// Shows the moving dots of a cue on its way out in place of state image 1's picture (on) or the picture again (off). The
    /// image view keeps its label and tooltip either way.
    func showStopping(_ on: Bool) {
        showsDots = on
        fadingDots.isHidden = !on
        // The still picture is taken out of the image view, not faded to nothing: an image view that only
        // had its alpha lowered still holds the old mark, and anything that sets its image again (a
        // selection's tint) drew it back under the dots.
        if on {
            images[1].image = nil
        } else if let mark = tints[1] {
            images[1].image = tinted(mark.image, mark.tint, mark.label)
        }
        images[1].alphaValue = 1
        if on, let mark = tints[1] { fadingDots.colour = drawnColour(mark.tint) }
    }

    /// The dots stand in for state image 1's picture.
    private(set) var showsDots = false

    private func tinted(_ image: NSImage, _ tint: NSColor, _ label: String) -> NSImage {
        TimeHeaderCell.tinted(image, drawnColour(tint), size: image.size, description: label)
    }

    private func drawnColour(_ tint: NSColor) -> NSColor {
        tint != .clear && backgroundStyle == .emphasized ? NSColor.alternateSelectedControlTextColor : tint
    }

    #if DEBUG
        /// Test seam, not in a Release build: the icon of tinted mark `i` and the colour it is drawn in now; nil
        /// for a hidden or untinted mark. A tinted copy keeps no drawing name, so a test reads it here.
        func drawnMark(_ i: Int) -> (icon: Icon, colour: NSColor)? {
            guard let mark = tints[i], !images[i].isHidden else { return nil }
            return (mark.icon, drawnColour(mark.tint))
        }
    #endif

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            guard backgroundStyle != oldValue else { return }
            for (i, mark) in tints where !(i == 1 && showsDots) {
                images[i].image = tinted(mark.image, mark.tint, mark.label)
            }
            if let mark = tints[1] { fadingDots.colour = drawnColour(mark.tint) }
        }
    }
}

/// The cue list: an outline of one cue list of the workspace, the one the sidebar picked (the first
/// until then). Each window has its own view controller, so each has its own selected list.
/// It mirrors the store's tree in `CueNode`s and applies `StoreChange`s as row inserts, removals,
/// moves and reloads; it reloads the whole table only for a snapshot or when it has lost sync.
/// Editing goes through `CueEditor`; this class only offers the drag and drop, which it hands to `onMove`,
/// and the in-place Number, Name and Duration edit, which it hands to `onEditCell`.
@MainActor
final class CueListViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate,
    NSTextFieldDelegate
{
    /// QLab order. The first and last columns carry no title. The first holds the playhead, the state and
    /// the cue's type icon; it has no type column of its own.
    static let columns: [(id: String, titleKey: String?, width: CGFloat)] = [
        ("status", nil, CueCell.statusWidth), ("number", "cuelist.col.number", 46), ("name", "cuelist.col.name", 280),
        ("target", "cuelist.col.target", 42), ("preWait", "cuelist.col.preWait", 72),
        ("duration", "cuelist.col.duration", 84), ("postWait", "cuelist.col.postWait", 72),
        ("continue", nil, 26),
    ]
    static let rowHeight: CGFloat = 24
    static let indentationPerLevel: CGFloat = 8
    /// The least the Name column shrinks to; a narrower list scrolls sideways.
    static let nameMinWidth: CGFloat = 60
    /// The least width of the Name editor, whatever the cell's.
    static let nameEditorMinWidth: CGFloat = 180

    let store: WorkspaceStore
    let outlineView: NSOutlineView
    private(set) var roots: [CueNode] = []
    private(set) var listID: ModelID?
    /// The list the sidebar picked. Once set it stays, so a reorder of lists never switches the view;
    /// when that list is deleted the first list takes over.
    private var chosenList: ModelID?
    private var nodes: [ModelID: CueNode] = [:]
    /// Cues that point at a cue, so a rename refreshes their Target cell.
    private var targeters: [ModelID: Set<ModelID>] = [:]
    /// Internal so a test can detach it and deliver changes in a chosen order.
    var sub: AnyCancellable?
    /// The engine's network output override (PB-10): Network cues show a red circle while it is on.
    private(set) var networkOutputDisabled = false
    private var overrideSub: AnyCancellable?
    /// While `applyModel` runs: every cue it touched (their targeters refresh), the groups it created
    /// (shown expanded, like QLab) and the existing groups that were collapsed (they stay so).
    private var batching = false
    /// The tree is being replaced: its collapses and expands are not the operator's, so they must not move or close an open Duration editor.
    private var rebuilding = false
    private var batchTouched = Set<ModelID>()
    private var batchNewGroups = Set<ModelID>()
    private var batchCollapsed = Set<ModelID>()
    /// Whether a running time counts up (the time gone) or down (the time left), per time column;
    /// each window has its own. Every column counts up until its header is clicked, as QLab's do.
    /// Kept for the life of the window only (no window-state persistence exists yet).
    static let timeColumns = ["preWait", "duration", "postWait"]
    private(set) var timeShown: [String: TimeShown] = [
        "preWait": .full, "duration": .full, "postWait": .full,
    ]
    /// Where Paste looks for cues (tests use a private one).
    var pasteboard: NSPasteboard = .general
    /// This window's editor and transport: the row menu acts on the window that was clicked, not
    /// on the key window (the menu-bar actions do).
    weak var editor: CueEditor?
    var onCommand: ((TransportCommand, [ModelID]) -> Void)?
    /// The cues the row menu was opened on: what its commands act on, wherever the selection has
    /// moved by the time one is chosen (the engine's updates apply while a menu is open).
    var rowMenuCues: [ModelID]?
    /// The row menu's Paste Cue Properties: the sheet for these cues.
    var onPasteProperties: (([ModelID]) -> Void)?

    /// `outlineView` is a parameter so tests can count the calls made on it.
    init(store: WorkspaceStore, outlineView: NSOutlineView = CueOutlineView()) {
        self.store = store
        self.outlineView = outlineView
        super.init(nibName: nil, bundle: nil)
        sub = store.changes.sink { [weak self] change in
            MainActor.assumeIsolated { self?.handle(change) }
        }
        (outlineView as? CueOutlineView)?.onStatusPress = { [weak self] row in self?.statusPressed(row) }
        // Published before it changes: use the delivered value.
        overrideSub = store.$status.map { $0?.networkOutputDisabled ?? false }.removeDuplicates().sink {
            [weak self] disabled in
            MainActor.assumeIsolated { self?.outputOverrideChanged(disabled) }
        }
        rebuild()
    }

    private func outputOverrideChanged(_ disabled: Bool) {
        networkOutputDisabled = disabled
        let network = Set(nodes.values.filter { $0.cue.kindType == "network" }.map(\.id))
        if !network.isEmpty { refresh(network, columns: ["status"]) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The Name column is sized to what the other columns leave of the list's width, whenever the list is
    /// laid out (a window that opens narrow included) or another column is resized.
    override func viewDidLayout() {
        super.viewDidLayout()
        fitNameColumn()
        solidHeader()
        keepSelectionInViewWhenShorter()
    }

    private var lastVisibleHeight: CGFloat?

    /// A list made shorter (the window or the inspector resized, a divider moved) keeps its selected row in
    /// view, as a selection change does: the clip view keeps its scroll position, so the row slid out below.
    /// Only a shrink that cut off a selected row that was in view moves the list: a list the operator
    /// scrolled away from the selection, or one that grows, is left where it is.
    private func keepSelectionInViewWhenShorter() {
        let clip = outlineView.enclosingScrollView?.contentView
        // The rows' visible part: where scroll bars always show, the sideways scroller lies over the clip
        // view's lower edge as an inset, so its appearing shortens the list without the clip view changing.
        let insets = clip?.contentInsets ?? NSEdgeInsets()
        let height = (clip?.bounds.height ?? 0) - insets.top - insets.bottom
        defer { lastVisibleHeight = height }
        guard let clip, let was = lastVisibleHeight, height < was, outlineView.selectedRow >= 0 else { return }
        let row = outlineView.rect(ofRow: outlineView.selectedRow)
        let top = clip.bounds.minY + insets.top
        guard row.minY >= top - 0.5, row.maxY <= top + was + 0.5 else { return }
        outlineView.scrollRowToVisible(outlineView.selectedRow)
    }

    /// The header's row is solid, so the rows scrolled up behind it never show through it: AppKit leaves
    /// a table's header clear over a blur of the rows, and a list squeezed to one row (the least window
    /// with the inspector shown) showed them through its titles. The header sits in a clip view of the
    /// scroll view's own, made once the table is in it, which fills its area with the list's colour.
    private func solidHeader() {
        guard let clip = outlineView.headerView?.superview as? NSClipView, !clip.drawsBackground else { return }
        clip.backgroundColor = outlineView.backgroundColor
        clip.drawsBackground = true
    }

    /// The width is worked out from the scroll view's own frame, with room always kept for a vertical scroll
    /// bar (some systems always show one): never from the clip view, whose width changes as scroll bars come
    /// and go and would feed back into the fit.
    private func fitNameColumn() {
        guard let scroll = view as? NSScrollView,
            let name = outlineView.tableColumns.first(where: { $0.identifier.rawValue == "name" })
        else { return }
        let inside = NSScrollView.contentSize(
            forFrameSize: scroll.frame.size, horizontalScrollerClass: nil, verticalScrollerClass: nil,
            borderType: scroll.borderType, controlSize: .regular, scrollerStyle: .legacy
        ).width
        let room = inside - NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
        let others = outlineView.tableColumns.filter { $0 !== name }.reduce(0) { $0 + $1.width }
        let extra = tableExtra(name, others: others)
        let left = (room - fitTimeColumns(room: room, extra: extra, name: name) - extra).rounded(.down)
        let width = max(name.minWidth, left)
        if abs(name.width - width) >= 1 { name.width = width }
        // The sideways scroller only while the columns are wider than the list. Where scroll bars always
        // show, a scroller kept for nothing took the room of the least list's one row. The
        // vertical scroller is left as it is (its room is always kept above).
        let overflows = left < name.minWidth
        if scroll.hasHorizontalScroller != overflows { scroll.hasHorizontalScroller = overflows }
    }

    /// The widths the operator (or the spec) gave the time columns, kept apart from what fits shows.
    private var preferredTimeWidths: [String: CGFloat] = [:]
    /// Time columns the operator dragged: they keep that width (the list scrolls sideways instead), as
    /// QLab keeps a resized column.
    private var draggedTimeColumns: Set<String> = []
    private var fitting = false
    /// What the Name column is kept at while the time columns can still give width: room for the
    /// indent of a nested cue and the first letters of its name.
    static let nameComfortWidth: CGFloat = 160

    /// Gives the time columns the operator has not dragged up to their slack (down to their least, a
    /// header and the widest time the list shows) so the Name column keeps `nameComfortWidth` where it
    /// can; with room they go back to the widths they were given. Returns the width of every column but
    /// Name.
    private func fitTimeColumns(room: CGFloat, extra: CGFloat, name: NSTableColumn) -> CGFloat {
        let columns = outlineView.tableColumns
        let timed = columns.filter { Self.timeColumns.contains($0.identifier.rawValue) }
        for c in timed where preferredTimeWidths[c.identifier.rawValue] == nil {
            preferredTimeWidths[c.identifier.rawValue] = c.width
        }
        func preferred(_ c: NSTableColumn) -> CGFloat {
            max(preferredTimeWidths[c.identifier.rawValue] ?? c.width, c.minWidth)
        }
        let giving = timed.filter { !draggedTimeColumns.contains($0.identifier.rawValue) }
        let fixed = columns.filter { $0 !== name && !giving.contains($0) }.reduce(0) { $0 + $1.width }
        let wanted = giving.reduce(0) { $0 + preferred($1) }
        let timeWidth = widestTimeWidth()
        func least(_ c: NSTableColumn) -> CGFloat { max(c.minWidth, (timeWidth + 5).rounded(.up)) }
        let slack = giving.reduce(0) { $0 + max(0, preferred($1) - least($1)) }
        let deficit = max(0, Self.nameComfortWidth - (room - fixed - wanted - extra))
        let take = slack > 0 ? min(1, deficit / slack) : 0
        fitting = true
        defer { fitting = false }
        var total = fixed
        for c in giving {
            let slackHere = max(0, preferred(c) - least(c))
            let width = (preferred(c) - slackHere * take).rounded(.up)
            if abs(c.width - width) >= 1 { c.width = width }
            total += c.width
        }
        return total
    }

    /// Whether a time in the list reaches an hour (its cell then reads `h:mm:ss.cc`). Kept up to date as
    /// cues change, so a layout does not scan the list; a change lays the columns out again.
    private var listHasHours = false

    private func longestTime(_ node: CueNode) -> Double {
        // A running time with no stored end (a looping group, a still held on screen) can pass an hour
        // while it counts: the columns widen when it does, once, and narrow when it stops.
        max(
            node.cue.raw["pre_wait"]?.doubleValue ?? 0, node.cue.postWait, restingDuration(node) ?? 0,
            shownDurationSeconds(node) ?? 0)
    }

    private func updateHoursShown(touched: Set<ModelID>? = nil) {
        let hourLong = 3599.995
        let now: Bool
        if let touched, touched.contains(where: { nodes[$0].map { longestTime($0) >= hourLong } ?? false }) {
            now = true
        } else if touched != nil, !listHasHours {
            now = false
        } else {
            now = nodes.values.contains { longestTime($0) >= hourLong }
        }
        guard now != listHasHours else { return }
        listHasHours = now
        view.needsLayout = true
    }

    /// The width of the widest time a time column can show: `h:mm:ss.cc` where a time in the list
    /// reaches an hour, else `mm:ss.cc`, in the widest digit the font has (its digits are not all alike).
    func widestTimeWidth() -> CGFloat {
        let font = WholeGlyphLabel(labelWithString: "").font ?? .systemFont(ofSize: NSFont.systemFontSize)
        func width(_ text: String) -> CGFloat { (text as NSString).size(withAttributes: [.font: font]).width }
        let digit = "0123456789".max { width(String($0)) < width(String($1)) }.map(String.init) ?? "0"
        let hours = listHasHours
        let pattern = hours ? "d:dd:dd.dd" : "dd:dd.dd"
        return width(pattern.replacingOccurrences(of: "d", with: digit))
    }

    /// The narrowest a time column goes: its header and the widest time the list shows.
    func timeLeast(of column: NSTableColumn) -> CGFloat {
        max(column.minWidth, (widestTimeWidth() + 5).rounded(.up))
    }

    private var measuredExtra: CGFloat?

    /// What the table adds to its columns' widths (cell spacing and its own insets, which depend on the
    /// system's table style): measured once, with the Name column made wide enough that the table's frame
    /// is its content's width and not the scroll view's.
    private func tableExtra(_ name: NSTableColumn, others: CGFloat) -> CGFloat {
        if let measuredExtra { return measuredExtra }
        let was = name.width
        name.width = 10_000
        outlineView.tile()
        let extra = outlineView.frame.width - others - name.width
        name.width = was
        measuredExtra = extra
        return extra
    }

    override func loadView() {
        for spec in Self.columns {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(spec.id))
            column.title = spec.titleKey.map(loc) ?? ""
            if Self.timeColumns.contains(spec.id) {
                column.headerCell = TimeHeaderCell(textCell: column.title)
                updateTimeHeader(column)
            }
            if spec.id == "continue" { column.headerCell = ContinueHeaderCell(textCell: "") }
            // No column is narrower than its header's text, so a header never truncates.
            let least = max(24, column.headerCell.cellSize.width.rounded(.up))
            switch spec.id {
            case "status":
                // The first column's marks sit in fixed slots: its width is fixed.
                column.minWidth = spec.width
                column.maxWidth = spec.width
                column.resizingMask = []
            case "name":
                // The Name column takes up and gives up width with the window; the others keep theirs, so
                // the times stay readable in a narrow window.
                column.minWidth = max(Self.nameMinWidth, least)
                column.resizingMask = []
            default:
                column.minWidth = least
                column.resizingMask = [.userResizingMask]
            }
            column.width = max(spec.width, column.minWidth)
            outlineView.addTableColumn(column)
        }
        // QLab's columns are resized only, never reordered, and a pressed header does not change.
        outlineView.allowsColumnReordering = false
        let header = CueHeaderView(frame: outlineView.headerView?.frame ?? .zero)
        header.onClick = { [weak self] in self?.toggleTimeShown($0.identifier.rawValue) }
        outlineView.headerView = header
        outlineView.columnAutoresizingStyle = .noColumnAutoresizing
        outlineView.outlineTableColumn = outlineView.tableColumns.first { $0.identifier.rawValue == "name" }
        outlineView.usesAlternatingRowBackgroundColors = true
        // Rows reach both edges of the list, so the playhead's pointer sits at the window's edge.
        if #available(macOS 11.0, *) { outlineView.style = .fullWidth }
        // The system's own 17-point gap between columns is blank width the least window cannot spare: the
        // columns sit close, as QLab's do. Set after the style, which sets the gap.
        outlineView.intercellSpacing = NSSize(width: 3, height: outlineView.intercellSpacing.height)
        // The least Name column (its neighbours at their least, each header whole) still shows a letter of
        // a cue five groups deep: a nesting level takes only what its disclosure triangle needs.
        outlineView.indentationPerLevel = Self.indentationPerLevel
        outlineView.allowsMultipleSelection = true
        // Letters are cue hotkeys and edit keys here; none may jump the selection, and with it the playhead.
        outlineView.allowsTypeSelect = false
        // A fixed height keeps AppKit from asking for every row's height.
        outlineView.rowHeight = Self.rowHeight
        outlineView.dataSource = self
        outlineView.delegate = self
        outlineView.setAX(AX.cueList, label: loc("cuelist.label"))
        (outlineView as? CueOutlineView)?.onDragExit = { [weak self] in
            self?.showTargetOutline(nil)
        }
        outlineView.registerForDraggedTypes([Self.dragType, CueToolbarView.dragType, .fileURL])
        outlineView.setDraggingSourceOperationMask(.move, forLocal: true)
        outlineView.target = self
        outlineView.action = #selector(clicked)
        outlineView.doubleAction = #selector(doubleClicked)
        NotificationCenter.default.addObserver(
            self, selector: #selector(textEditingEnded(_:)), name: NSText.didEndEditingNotification, object: nil)
        NotificationCenter.default.addObserver(
            self, selector: #selector(textEditingEnded(_:)), name: HotkeyRecorder.didEndRecording, object: nil)
        NotificationCenter.default.addObserver(
            self, selector: #selector(menuTrackingBegan(_:)), name: NSMenu.didBeginTrackingNotification, object: nil)
        NotificationCenter.default.addObserver(
            self, selector: #selector(menuTrackingEnded(_:)), name: NSMenu.didEndTrackingNotification, object: nil)
        let menu = NSMenu()
        menu.delegate = self
        outlineView.menu = menu
        cellEditor.delegate = self
        cellEditor.usesSingleLineMode = true
        cellEditor.cell?.isScrollable = true
        cellEditor.font = .systemFont(ofSize: NSFont.systemFontSize)
        let scroll = NSScrollView()
        scroll.contentView = BlankDeselectingClipView()
        scroll.documentView = outlineView
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        view = scroll
        outlineView.reloadData()
        // The tree was built before the table had a data source, so nothing is open yet.
        func open(_ nodes: [CueNode]) {
            for node in nodes where node.isGroup {
                outlineView.expandItem(node)
                open(node.children)
            }
        }
        open(roots)
    }

    // MARK: Reading the tree (tests and later features)

    func node(_ id: ModelID) -> CueNode? { nodes[id] }
    var nodeCount: Int { nodes.count }
    var nodeIDs: Set<ModelID> { Set(nodes.keys) }

    func selectedIDs() -> [ModelID] {
        outlineView.selectedRowIndexes.compactMap { (outlineView.item(atRow: $0) as? CueNode)?.id }
    }

    /// Selecting a cue by hand moves the playhead to it (QLab's default); rebuilds that restore the
    /// selection must not, so they raise this flag.
    private var restoringSelection = false
    private var sentPlayhead: ModelID?

    func select(_ ids: [ModelID]) {
        restoringSelection = true
        defer { restoringSelection = false }
        let rows = IndexSet(ids.compactMap { nodes[$0] }.map { outlineView.row(forItem: $0) }.filter { $0 >= 0 })
        outlineView.selectRowIndexes(rows, byExtendingSelection: false)
    }

    /// Opens the groups above `node`, outermost first: a group inside a collapsed group cannot be
    /// expanded until the one around it is.
    func openGroups(above node: CueNode) {
        var chain: [CueNode] = []
        var p = node.parent
        while let a = p {
            chain.append(a)
            p = a.parent
        }
        for group in chain.reversed() { outlineView.expandItem(group) }
    }

    /// Selects `id` and scrolls to it, opening any collapsed groups above it. Like `select`, it
    /// does not move the playhead.
    func reveal(_ id: ModelID) {
        if let node = nodes[id] { openGroups(above: node) }
        select([id])
        if let node = nodes[id] {
            let row = outlineView.row(forItem: node)
            if row >= 0 { outlineView.scrollRowToVisible(row) }
        }
        // The inspector shows the cue now selected, as for a click on it.
        onSelectionChange?(selectedIDs())
    }

    /// The list to show: the chosen one while it exists, else the first.
    private func shownList() -> CueList? {
        let lists = store.model?.cueLists ?? []
        return lists.first { $0.id == chosenList } ?? lists.first
    }

    /// Shows another cue list (the sidebar's selection). Unknown ids are ignored.
    func showList(_ id: ModelID) {
        guard id != listID, store.model?.cueLists.contains(where: { $0.id == id }) == true else { return }
        chosenList = id
        rebuild()
        followAfterLoad()
        onModelApplied?()
    }

    /// Show Mode: rows cannot be dragged, nothing can be dropped and no cell can be edited; a
    /// cell being typed in is dropped (callers commit it first, as the inspector's fields).
    var isEditLocked = false {
        didSet {
            if isEditLocked { cancelCellEdit() }
            guard isEditLocked != oldValue else { return }
            refresh(Set(nodes.keys), columns: ["target"])
        }
    }
    /// A validated drop: the cues go before child `index` of the parent (nil: top level).
    var onMove: (([ModelID], ModelID?, Int) -> Void)?
    /// A validated drop on a Target cell: the cues to set, and the cue they now target.
    var onDropTarget: (([ModelID], ModelID) -> Void)?
    var dragged: [CueNode] = []
    static let dragType = NSPasteboard.PasteboardType("dev.openlab.cue-id")

    /// Select Next / Previous: selects the visible row `delta` rows from the last selected one (from
    /// before the first row, or after the last, when nothing is selected), as a click would: the
    /// inspector follows and the playhead moves. At either end nothing changes.
    func selectAdjacent(by delta: Int) {
        let count = outlineView.numberOfRows
        guard count > 0 else { return }
        let from = outlineView.selectedRow >= 0 ? outlineView.selectedRow : (delta > 0 ? -1 : count)
        let row = min(max(from + delta, 0), count - 1)
        guard row != from, let node = outlineView.item(atRow: row) as? CueNode else { return }
        selectAsUser([node.id])
    }

    /// Selects and shows `ids` as a click would: the inspector follows and the playhead moves.
    func selectAsUser(_ ids: [ModelID]) {
        for id in ids { if let node = nodes[id] { openGroups(above: node) } }
        let rows = IndexSet(ids.compactMap { nodes[$0] }.map { outlineView.row(forItem: $0) }.filter { $0 >= 0 })
        outlineView.selectRowIndexes(rows, byExtendingSelection: false)
        if let row = rows.first { outlineView.scrollRowToVisible(row) }
    }

    /// The selection as of the last change, to tell which cue the user selected last.
    private var lastSelection: Set<ModelID> = []

    /// The cue the playhead goes to when the selection (in row order) becomes `selected` from
    /// `previous`: the most recently selected one (QLab 5). A click or Shift-arrow adds one cue;
    /// a Shift-click range adds several, and `latest` (the table's last selected row) is the one
    /// clicked. A selection that only shrinks (a click on one of several selected) goes to `latest`.
    static func playheadTarget(selected: [ModelID], previous: Set<ModelID>, latest: ModelID?) -> ModelID? {
        let added = selected.filter { !previous.contains($0) }
        if added.count == 1 { return added[0] }
        if let latest, added.isEmpty || added.contains(latest) { return latest }
        return added.last ?? selected.last
    }

    /// Told of every selection change (the inspector follows it).
    var onSelectionChange: (([ModelID]) -> Void)?
    /// Told after every selection change the list draws (a click, a key, a restore), once any batch
    /// of model changes is done, so the sidebar's tree can show the same cue.
    var onSelectionDrawn: (() -> Void)?
    /// Called once the tree matches a reset or a model change, so a listener (Find) reads `roots`
    /// after the update and not before it, whatever order the store's subscribers run in.
    var onModelApplied: (() -> Void)?

    /// The cue that takes the selection when the delete now under way removes the selected cues (set by
    /// the editor before it sends the command). The list applies it in the same turn as the removal, so
    /// no frame is drawn with nothing selected.
    var successorOnDelete: ModelID?

    /// A removal took the selected cues out: the successor is selected now, in the turn that drew the removal.
    private func takeSuccessor(after deletes: [Delete]) {
        guard let successor = successorOnDelete, !deletes.isEmpty, selectedIDs().isEmpty,
            nodes[successor] != nil
        else { return }
        successorOnDelete = nil
        selectAsUser([successor])
    }

    /// The playhead's cue was removed while selected: the playhead's move on is not followed.
    private var headRemovedWhileSelected = false

    func outlineViewSelectionDidChange(_ notification: Notification) {
        // Programmatic restores (select, rebuild, batches) are not the user's choice.
        if !restoringSelection && !batching { onSelectionChange?(selectedIDs()) }
        let selected = selectedIDs()
        let previous = lastSelection
        lastSelection = Set(selected)
        if !rebuilding, !batching { onSelectionDrawn?() }
        let lastRow = outlineView.selectedRow >= 0 ? outlineView.selectedRow : nil
        let latest = lastRow.flatMap { (outlineView.item(atRow: $0) as? CueNode)?.id }
        guard !restoringSelection, !batching, !followingPlayhead, locksPlayhead,
            let id = Self.playheadTarget(selected: selected, previous: previous, latest: latest)
        else { return }
        sendPlayhead(id)
    }

    /// A click on a row (the table's action, after any selection change). Clicking the one row that
    /// is already highlighted changes no selection, so it puts the playhead back on that cue here
    /// (the playhead can be elsewhere while a text field was being typed in).
    @objc private func clicked() {
        let column = outlineView.clickedColumn
        if outlineView.tableColumns.indices.contains(column),
            outlineView.tableColumns[column].identifier.rawValue == "continue"
        {
            showContinueMenu(row: outlineView.clickedRow)
        }
        rowClicked(outlineView.clickedRow)
    }

    /// A continue mode was chosen from a row's own menu.
    var onSetContinue: ((ModelID, String) -> Void)?

    /// The three continue modes for the cue of `row`, the current one ticked (nil: no menu, as in
    /// Show Mode or off the rows).
    func continueMenu(row: Int) -> NSMenu? {
        guard !isEditLocked, let node = outlineView.item(atRow: row) as? CueNode else { return nil }
        let current = node.cue.raw["continue_mode"]?.stringValue ?? "do_not_continue"
        let menu = NSMenu()
        for mode in InspectorViewController.continueModes {
            let item = menu.addItem(
                withTitle: loc("inspector.continue.\(mode)"), action: #selector(continueChosen(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = [node.id.raw, mode]
            item.state = mode == current ? .on : .off
        }
        return menu
    }

    /// A click on a row's continue cell opens the modes there (QLab 5: the cell is a menu).
    private func showContinueMenu(row: Int) {
        guard let menu = continueMenu(row: row) else { return }
        let column = outlineView.column(withIdentifier: NSUserInterfaceItemIdentifier("continue"))
        let cell = outlineView.frameOfCell(atColumn: column, row: row)
        menu.popUp(positioning: nil, at: NSPoint(x: cell.minX, y: cell.maxY), in: outlineView)
    }

    @objc private func continueChosen(_ sender: NSMenuItem) {
        guard let pair = sender.representedObject as? [String], pair.count == 2 else { return }
        onSetContinue?(ModelID(pair[0]), pair[1])
    }

    /// A press in a row's playhead column (QLab 5): the playhead goes to that cue. Locked to the
    /// selection, the row click that follows moves it; unlocked, nothing else would.
    func statusPressed(_ row: Int) {
        guard !locksPlayhead, let node = outlineView.item(atRow: row) as? CueNode else { return }
        sendPlayhead(node.id)
    }

    func rowClicked(_ row: Int) {
        guard row >= 0, locksPlayhead, let node = outlineView.item(atRow: row) as? CueNode,
            selectedIDs() == [node.id]
        else { return }
        sendPlayhead(node.id)
    }

    /// Workspace Settings, General, "lock playhead to selection" (on by default): the selection and
    /// the playhead move together. Off, a click or an arrow key moves only the selection and a GO
    /// or an OSC move of the playhead only the playhead. A new workspace window still opens on the
    /// playhead's cue.
    private var locksPlayhead: Bool { store.isOn(.lockPlayhead) != false }

    /// Moves the playhead to `id` for the selection. Compared only with what was last sent: A, B, A
    /// within one round trip must still end on A, and the store's playhead can be behind the engine's
    /// (a move on its way), where a click that sent nothing would leave GO on another cue.
    private func sendPlayhead(_ id: ModelID) {
        guard let list = listID, sentPlayhead != id else { return }
        sentPlayhead = id
        lastSentHead = id
        playheadSends += 1
        store.showPlayheadAhead(list: list, cue: id)
        // The store, not this controller, is held while the engine answers: a window closed meanwhile
        // is not kept alive by its answer.
        Task { [weak self, store] in
            let result = await store.transport(.setPlayhead(list: list, cue: id))
            var seq: UInt64?
            if case .success(let ok) = result {
                // An answer without a seq (none is expected) settles at once.
                if case .transport(let s)? = ok.data { seq = s } else { seq = store.runtimeSeq }
            }
            store.playheadAnswered(list: list, seq: seq)
            self?.playheadSendAnswered(list: list)
        }
    }

    /// The selection is being moved to the playhead: the inspector follows, nothing is sent.
    private(set) var followingPlayhead = false
    /// `set_playhead`s sent for the selection and not yet answered. Until they are, the engine may
    /// still echo an earlier one, so the selection does not follow the playhead meanwhile.
    private var playheadSends = 0
    /// The cue the last of those was for.
    private var lastSentHead: ModelID?
    /// The list's playhead moved while sends were unanswered.
    private var headMovedWhileSending = false
    /// The list's playhead as last seen here: the selection follows only a change of this list's
    /// own playhead (another list's, or a runtime batch that leaves it, must not reopen groups).
    private var seenHead: ModelID?
    /// The selection did not follow a playhead move because of `isTextEditing` or a `FollowHold`.
    private(set) var followHeld = false
    private var followHolds = Set<FollowHold>()
    /// Whether the left mouse button is down as a key arrives (tests: the mouse they stand in for).
    var isLeftButtonDown: () -> Bool = { NSEvent.pressedMouseButtons & 1 != 0 }

    /// What holds the selection where it is while the playhead moves, as typing does: the inspector
    /// edits the selected cue, so moving the selection under one of these would send the rest of an
    /// edit to another cue.
    enum FollowHold {
        /// The mouse button is down in this window, in a control that tracks until it comes up (a
        /// fader, a checkbox, a table row): only known around the press (`WorkspaceWindow`).
        case press
        /// The inspector's own drags, which see the press and the release as separate events.
        case gesture
        /// A menu is open (the row menu, an inspector popup, the menu bar).
        case menu
    }

    var isGestureHeld: Bool { followHolds.contains(.gesture) }

    /// Starts or ends a hold; the end of the last one follows a playhead move held meanwhile.
    func setFollowHold(_ hold: FollowHold, _ on: Bool) {
        if on {
            followHolds.insert(hold)
        } else {
            followHolds.remove(hold)
            resumeHeldFollow()
        }
    }

    private func playheadSendAnswered(list: ModelID) {
        playheadSends = max(0, playheadSends - 1)
        guard playheadSends == 0 else { return }
        // The last send is answered, so a repeat click may send again. A send that failed, or that the
        // engine only warned about (a cue deleted meanwhile), leaves no echo, so it must be allowed to.
        sentPlayhead = nil
        let moved = headMovedWhileSending
        headMovedWhileSending = false
        // The list changed meanwhile: its follow waited for this answer.
        guard list == listID else { return followPlayhead(force: true) }
        // On the cue sent last: its echo is in and the selection is already there. Elsewhere, it
        // moved on by itself after the click (a GO straight after it, another client): follow.
        if moved, store.playheads[list] != lastSentHead { followPlayhead() }
    }

    /// The tree was replaced (a show loaded, a reconnect's or a resync's snapshot, another list shown):
    /// the selection goes to the list's playhead, as for any other move, and the inspector is told
    /// even if that moves nothing, as a rebuild's own restore of the selection does not tell it.
    private func followAfterLoad() {
        if playheadSends > 0 { headMovedWhileSending = true } else { followPlayhead(force: true) }
        onSelectionChange?(selectedIDs())
    }

    /// A window opened on a store that already holds the workspace: the selection goes to the playhead's cue.
    func followPlayheadAtOpen() { followAfterLoad() }

    /// This list's playhead may have moved (a runtime change): follow it if it did.
    private func playheadChanged() {
        guard let list = listID else { return }
        let head = store.playheads[list]
        guard head != seenHead else { return }
        seenHead = head
        if headRemovedWhileSelected {
            headRemovedWhileSelected = false
            return
        }
        if playheadSends > 0 {
            headMovedWhileSending = true
        } else {
            followPlayhead()
        }
    }

    /// Text is being typed in this window (an inspector field, the notes, the masthead's notes, a
    /// cell being edited in the list), or a hotkey is being recorded: moving the selection would
    /// take the operator's focus, and their next Space would fire GO.
    private var isTextEditing: Bool {
        let responder = view.window?.firstResponder
        return (responder as? NSText)?.isEditable == true || responder is HotkeyRecorder
    }

    /// Typing in this window ended: a playhead move held meanwhile is followed now.
    @objc private func textEditingEnded(_ note: Notification) {
        guard (note.object as? NSView)?.window === view.window else { return }
        resumeHeldFollow()
    }

    @objc private func menuTrackingBegan(_ note: Notification) { setFollowHold(.menu, true) }
    @objc private func menuTrackingEnded(_ note: Notification) { setFollowHold(.menu, false) }

    /// Later, not now: the field editor still holds focus while it reports the end, and a popup's
    /// action, which edits the selected cue, comes after its menu reports the end.
    private func resumeHeldFollow() {
        guard followHeld else { return }
        Task { @MainActor [weak self] in self?.followHeldNow() }
    }

    /// Follows a playhead move held until now, if nothing holds it any more.
    func followHeldNow() {
        guard followHeld, !isTextEditing, followHolds.isEmpty else { return }
        followHeld = false
        followPlayhead()
    }

    /// The playhead and the selection stay together (QLab 5's default, its workspace setting that
    /// links them): when the engine moves the playhead (GO, a Go To cue, OSC, an edit), the
    /// selection moves to the cue standing by, opening the groups above it, and the inspector
    /// follows; with no cue standing by (past the end of the list) nothing is selected. A selection
    /// that already holds the playhead's cue (several selected by hand) is left as it is. While text
    /// is being typed in the window, or a `FollowHold` lasts, the selection stays, and follows once
    /// that ends.
    private func followPlayhead(force: Bool = false) {
        guard playheadSends == 0, force || locksPlayhead, let list = listID else { return }
        if isTextEditing || !followHolds.isEmpty {
            followHeld = true
            return
        }
        followHeld = false
        let selected = selectedIDs()
        let head = store.playheads[list]
        if let head {
            guard !selected.contains(head), let node = nodes[head] else { return }
            followingPlayhead = true
            defer { followingPlayhead = false }
            openGroups(above: node)
            let row = outlineView.row(forItem: node)
            guard row >= 0 else { return }
            outlineView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            outlineView.scrollRowToVisible(row)
        } else if !selected.isEmpty {
            followingPlayhead = true
            defer { followingPlayhead = false }
            outlineView.deselectAll(nil)
        }
    }

    func isPlayheadRow(_ id: ModelID) -> Bool { listID.flatMap { store.playheads[$0] } == id }

    // MARK: Store changes

    func handle(_ change: StoreChange) {
        switch change {
        case .reset:
            rebuild()
            followAfterLoad()
            onModelApplied?()
        case .model(let upserts, let deletes):
            applyModel(upserts: upserts, deletes: deletes)
            takeSuccessor(after: deletes)
            onModelApplied?()
        case .runtime(let cues, let heads):
            refresh(cues.union(heads))
            if !heads.isEmpty { playheadChanged() }
        case .progress(let ids):
            // A Wait cue's post-wait counts with its action.
            let waits = ids.filter { nodes[$0]?.cue.kindType == "wait" }
            refresh(ids.subtracting(waits), columns: ["duration"])
            if !waits.isEmpty { refresh(waits, columns: ["duration", "postWait"]) }
        case .waits(let ids): refresh(ids, columns: ["preWait", "postWait"])
        case .notices: break
        }
    }

    /// Replaces the tree from the store and reloads the table, keeping selection and expansion.
    func rebuild() {
        rebuilding = true
        defer { rebuilding = false }
        let selected = selectedIDs()
        let known = Set(nodes.keys)
        defer { updateHoursShown() }
        let expanded = Set(nodes.values.filter { $0.isGroup && outlineView.isItemExpanded($0) }.map(\.id))
        nodes = [:]
        targeters = [:]
        let list = shownList()
        listID = list?.id
        chosenList = list?.id
        seenHead = list.flatMap { store.playheads[$0.id] }
        // A resync replaces the list: a skip waiting for an undone add's playhead move no longer applies.
        headRemovedWhileSelected = false
        roots = (list?.cues ?? []).map { makeNode($0, parent: nil) }
        outlineView.reloadData()
        func expand(_ node: CueNode) {
            // Groups the list has not shown before open expanded.
            if node.isGroup && (expanded.contains(node.id) || !known.contains(node.id)) {
                outlineView.expandItem(node)
            }
            node.children.forEach(expand)
        }
        roots.forEach(expand)
        select(selected)
        placeCellEditor()
    }

    private func makeNode(_ cue: Cue, parent: CueNode?) -> CueNode {
        let node = CueNode(cue: cue, parent: parent)
        node.children = cue.children.map { makeNode($0, parent: node) }
        nodes[cue.id] = node
        if batching {
            batchTouched.insert(cue.id)
            if node.isGroup { batchNewGroups.insert(cue.id) }
        }
        track(node)
        return node
    }

    private func track(_ node: CueNode) {
        if let target = node.targetID { targeters[target, default: []].insert(node.id) }
    }

    private func untrack(_ node: CueNode) {
        if let target = node.targetID { targeters[target]?.remove(node.id) }
    }

    private func forget(_ node: CueNode) {
        untrack(node)
        if nodes[node.id] === node { nodes[node.id] = nil }
        node.children.forEach(forget)
    }

    private func applyModel(upserts: [Upsert], deletes: [Delete]) {
        guard shownList()?.id == listID else { return rebuild() }
        guard let listID else { return }
        let selected = selectedIDs()
        var expanded = Set<ModelID>()
        batching = true
        batchTouched = []
        batchNewGroups = []
        batchCollapsed = []
        defer { batching = false }
        // A group's hollow flag depends on its descendants, so every group above a changed cue
        // (before and after the change) redraws its status cell.
        var flagGroups = Set<ModelID>()
        func noteAncestors(_ id: ModelID) {
            var n = nodes[id]?.parent
            while let g = n {
                flagGroups.insert(g.id)
                n = g.parent
            }
        }
        for case .cue(let id) in deletes { noteAncestors(id) }
        for case .cue(_, _, _, let cue) in upserts { noteAncestors(cue.id) }
        for case .cue(let id) in deletes {
            // The cue the playhead stood on was removed while it was selected (an undone add): the
            // engine moves the playhead on, and the selection stays empty, as in QLab.
            if id == seenHead, selected.contains(id) { headRemovedWhileSelected = true }
            batchTouched.insert(id)
            remove(id, expanded: &expanded)
        }
        for case .cue(let list, let parent, let index, let cue) in upserts {
            batchTouched.insert(cue.id)
            if list != listID {
                remove(cue.id, expanded: &expanded)  // moved to another list
            } else if !upsert(cue, parent: parent, index: index, expanded: &expanded) {
                return rebuild()  // the engine's tree and ours disagree: resync from the store
            }
        }
        for case .cue(_, _, _, let cue) in upserts { noteAncestors(cue.id) }
        for id in expanded.union(batchNewGroups.subtracting(batchCollapsed)) {
            if let n = nodes[id] { outlineView.expandItem(n) }
        }
        if selectedIDs() != selected { select(selected) }
        // Cues that point at a changed cue (or anything in a changed group) show its name and number.
        // A default name follows its target's, which may itself be a default name: walk the chain
        // (targets can loop, so each cue once).
        var pointing = Set<ModelID>()
        var pending = Array(batchTouched)
        while let id = pending.popLast() {
            for t in targeters[id] ?? [] where pointing.insert(t).inserted { pending.append(t) }
        }
        if !pointing.isEmpty { refresh(pointing, columns: ["target", "name"]) }
        let flagRows = flagGroups.filter { nodes[$0] != nil }
        if !flagRows.isEmpty { refresh(flagRows, columns: ["status"]) }
        updateHoursShown(touched: deletes.isEmpty ? batchTouched : nil)
        refreshBoxes()
        placeCellEditor()
    }

    private func remove(_ id: ModelID, expanded: inout Set<ModelID>) {
        guard let node = nodes[id], let index = siblings(of: node).firstIndex(where: { $0 === node }) else { return }
        collectExpanded(node, into: &expanded)
        collectSubtree(node)
        let parent = node.parent
        if let parent { parent.children.remove(at: index) } else { roots.remove(at: index) }
        forget(node)
        outlineView.removeItems(at: IndexSet(integer: index), inParent: parent, withAnimation: [])
    }

    private func siblings(of node: CueNode) -> [CueNode] { node.parent?.children ?? roots }

    private func collectExpanded(_ node: CueNode, into set: inout Set<ModelID>) {
        guard node.isGroup else { return }
        if outlineView.isItemExpanded(node) { set.insert(node.id) } else { batchCollapsed.insert(node.id) }
        for child in node.children { collectExpanded(child, into: &set) }
    }

    private func collectSubtree(_ node: CueNode) {
        batchTouched.insert(node.id)
        node.children.forEach(collectSubtree)
    }

    /// Puts the cue where the engine says, like the store does: it leaves its old place and goes to
    /// `index` among the parent's children. False when the tree cannot take it.
    private func upsert(_ cue: Cue, parent parentID: ModelID?, index: Int, expanded: inout Set<ModelID>) -> Bool {
        var parent: CueNode?
        if let parentID {
            guard let p = nodes[parentID] else { return false }
            parent = p
        }
        guard let existing = nodes[cue.id] else {
            let node = makeNode(cue, parent: parent)
            let at = min(max(index, 0), parent?.children.count ?? roots.count)
            if let parent { parent.children.insert(node, at: at) } else { roots.insert(node, at: at) }
            outlineView.insertItems(at: IndexSet(integer: at), inParent: parent, withAnimation: [])
            return true
        }
        var ancestor = parent
        while let a = ancestor {
            if a === existing { return false }
            ancestor = a.parent
        }
        guard let oldIndex = siblings(of: existing).firstIndex(where: { $0 === existing }) else { return false }
        let oldParent = existing.parent
        let count = (parent?.children.count ?? roots.count) - (oldParent === parent ? 1 : 0)
        let newIndex = min(max(index, 0), count)
        let moved = oldParent !== parent || oldIndex != newIndex
        if moved {
            if let oldParent { oldParent.children.remove(at: oldIndex) } else { roots.remove(at: oldIndex) }
            if let parent {
                parent.children.insert(existing, at: newIndex)
            } else {
                roots.insert(existing, at: newIndex)
            }
            existing.parent = parent
            outlineView.moveItem(at: oldIndex, inParent: oldParent, to: newIndex, inParent: parent)
        }
        replaceContent(existing, with: cue, expanded: &expanded)
        return true
    }

    /// New cue data for a node already in the tree; a group's subtree is reconciled by id so its
    /// children keep their expansion state.
    private func replaceContent(_ node: CueNode, with cue: Cue, expanded: inout Set<ModelID>) {
        untrack(node)
        node.cue = cue
        track(node)
        if node.isGroup {
            collectExpanded(node, into: &expanded)
            collectSubtree(node)
            node.children = reconcile(node, with: cue.children)
            outlineView.reloadItem(node, reloadChildren: true)
        } else {
            node.children.forEach(forget)  // a cue that stopped being a group holds no children
            node.children = []
            outlineView.reloadItem(node, reloadChildren: true)
        }
        configureRow(node)
    }

    private func reconcile(_ parent: CueNode, with cues: [Cue]) -> [CueNode] {
        var old = Dictionary(parent.children.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        var result: [CueNode] = []
        for cue in cues {
            if let node = old.removeValue(forKey: cue.id) {
                untrack(node)
                node.cue = cue
                track(node)
                node.children = reconcile(node, with: cue.children)
                result.append(node)
            } else {
                result.append(makeNode(cue, parent: parent))
            }
        }
        old.values.forEach(forget)
        return result
    }

    // MARK: Runtime display

    /// Redraws only the rows of `ids`, only `columns` of them.
    private func refresh(_ ids: Set<ModelID>, columns: [String]? = nil) {
        var rows = IndexSet()
        func add(_ node: CueNode) {
            let row = outlineView.row(forItem: node)
            guard row >= 0 else { return }
            rows.insert(row)
            configureRow(node, row: row)
        }
        for id in ids { if let node = nodes[id] { add(node) } }
        if columns == nil || !Set(columns ?? []).isDisjoint(with: Self.timeColumns) {
            updateHoursShown(touched: ids)
        }
        guard !rows.isEmpty else { return }
        let all = outlineView.tableColumns
        let wanted = columns.map { ids in IndexSet(all.indices.filter { ids.contains(all[$0].identifier.rawValue) }) }
        outlineView.reloadData(forRowIndexes: rows, columnIndexes: wanted ?? IndexSet(all.indices))
    }

    private func configureRow(_ node: CueNode, row: Int? = nil) {
        let r = row ?? outlineView.row(forItem: node)
        guard r >= 0, let view = outlineView.rowView(atRow: r, makeIfNecessary: false) as? CueRowView else { return }
        configure(view, node)
    }

    private func configure(_ view: CueRowView, _ node: CueNode) {
        let colour = CueKindStyle.shownColour(of: node.cue, played: store.played.contains(node.id))
        view.tint = CueKindStyle.colours[colour]?.tint
        view.pattern = RowPattern.of(node.cue)
        view.isPlayhead = isPlayheadRow(node.id)
        view.boxes = boxParts(node)
    }

    /// The group boxes `node`'s row draws: every open group around it, and its own when it is an open
    /// or empty group, with the Playlist groups' next children marked.
    func boxParts(_ node: CueNode) -> [GroupBoxPart] {
        GroupBoxes.parts(
            for: node, expanded: { [outlineView] in outlineView.isItemExpanded($0) },
            next: { [store] in store.playlistNext[$0] })
    }

    /// Opening, closing, adding, removing or moving rows changes which row each box ends on, so every
    /// shown row looks again (only the rows on screen have views).
    private func refreshBoxes() {
        outlineView.enumerateAvailableRowViews { view, row in
            guard let view = view as? CueRowView, let node = outlineView.item(atRow: row) as? CueNode else { return }
            view.boxes = boxParts(node)
            let name = outlineView.column(withIdentifier: NSUserInterfaceItemIdentifier("name"))
            guard name >= 0 else { return }
            (outlineView.view(atColumn: name, row: row, makeIfNecessary: false) as? CueCell)?.textInset =
                GroupBoxes.textInset(view.boxes)
        }
    }

    // MARK: NSOutlineViewDataSource

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        (item as? CueNode)?.children.count ?? (item == nil ? roots.count : 0)
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        if let node = item as? CueNode { return node.children[index] }
        return roots[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? CueNode)?.isGroup ?? false
    }

    // MARK: Drag and drop

    func outlineView(_ outlineView: NSOutlineView, pasteboardWriterForItem item: Any) -> NSPasteboardWriting? {
        guard !isEditLocked, let node = item as? CueNode else { return nil }
        let pb = NSPasteboardItem()
        pb.setString(node.id.raw, forType: Self.dragType)
        return pb
    }

    func outlineView(
        _ outlineView: NSOutlineView, draggingSession session: NSDraggingSession, willBeginAt screenPoint: NSPoint,
        forItems draggedItems: [Any]
    ) {
        let items = draggedItems.compactMap { $0 as? CueNode }
        // Dragging a selected row drags the whole selection, in list order.
        let selected = selectedIDs().compactMap { nodes[$0] }
        dragged = CueEditor.topLevel(selected.contains { n in items.contains { $0 === n } } ? selected : items)
    }

    func outlineView(
        _ outlineView: NSOutlineView, draggingSession session: NSDraggingSession, endedAt screenPoint: NSPoint,
        operation: NSDragOperation
    ) {
        dragged = []
        showTargetOutline(nil)
    }

    /// The outline drawn over a row's Target cell during a drag of cues (nil while there is none).
    let targetOutline = TargetDropOutline()

    /// Outlines `node`'s Target cell as `kind`, or takes the outline away.
    func showTargetOutline(_ shown: (node: CueNode, kind: TargetDropOutline.Kind)?) {
        let row = shown.map { outlineView.row(forItem: $0.node) } ?? -1
        let column = outlineView.column(withIdentifier: NSUserInterfaceItemIdentifier("target"))
        guard let shown, row >= 0, column >= 0 else {
            targetOutline.removeFromSuperview()
            return
        }
        targetOutline.kind = shown.kind
        targetOutline.onSelectedRow = outlineView.isRowSelected(row)
        targetOutline.frame = outlineView.frameOfCell(atColumn: column, row: row)
        if targetOutline.superview !== outlineView { outlineView.addSubview(targetOutline) }
    }

    /// Which Target cell a drag of `dragged` at `point` (in the outline) outlines, and how: the row
    /// under the pointer when the dragged cues and it can be made a cue and its target.
    func targetOutlineFor(_ dragged: [CueNode], at point: NSPoint) -> (node: CueNode, kind: TargetDropOutline.Kind)? {
        guard !isEditLocked, !dragged.isEmpty,
            let node = outlineView.item(atRow: outlineView.row(at: point)) as? CueNode,
            CueEditor.targetDrop(dragged.map(\.cue), onto: node.cue) != nil
        else { return nil }
        let column = outlineView.column(at: point)
        let inCell =
            outlineView.tableColumns.indices.contains(column)
            && outlineView.tableColumns[column].identifier.rawValue == "target"
        return (node, inCell ? .sets : .near)
    }

    /// Where a drop lands, as (parent, index): dropping on a group puts the cue at its end, on any
    /// other cue after it.
    private func dropTarget(_ item: Any?, _ index: Int) -> (parent: CueNode?, index: Int) {
        let node = item as? CueNode
        if index != NSOutlineViewDropOnItemIndex { return (node, index) }
        guard let node else { return (nil, roots.count) }
        if node.isGroup { return (node, node.children.count) }
        let siblings = node.parent?.children ?? roots
        return (node.parent, (siblings.firstIndex { $0 === node } ?? siblings.count - 1) + 1)
    }

    /// What a drop of the list's own rows would do.
    enum CueDrop: Equatable {
        /// These cues now target that one; the order is left alone.
        case setTarget([ModelID], ModelID)
        case move([ModelID], ModelID?, Int)
    }

    /// What dropping `dragged` does, or nil to refuse it. Only a drop on a row's Target cell sets a target
    /// (QLab 5, manual p. 94, and 5.0's change log: it makes a target hard to change by accident while
    /// reordering); where no target can be set, or anywhere else on the row, it reorders as ever.
    func cueDrop(_ dragged: [CueNode], item: Any?, index: Int, inTargetColumn: Bool) -> CueDrop? {
        guard !isEditLocked, !dragged.isEmpty else { return nil }
        if inTargetColumn, index == NSOutlineViewDropOnItemIndex, let node = item as? CueNode,
            let set = CueEditor.targetDrop(dragged.map(\.cue), onto: node.cue)
        {
            return .setTarget(set.cues, set.target)
        }
        let target = dropTarget(item, index)
        guard CueEditor.canDrop(dragged, into: target.parent) else { return nil }
        return .move(dragged.map(\.id), target.parent?.id, target.index)
    }

    func perform(_ drop: CueDrop) {
        switch drop {
        case .setTarget(let cues, let target): onDropTarget?(cues, target)
        case .move(let ids, let parent, let at): onMove?(ids, parent, at)
        }
    }

    /// Whether the drag is over the Target column.
    private func overTargetColumn(_ info: NSDraggingInfo) -> Bool {
        let column = outlineView.column(at: outlineView.convert(info.draggingLocation, from: nil))
        return outlineView.tableColumns.indices.contains(column)
            && outlineView.tableColumns[column].identifier.rawValue == "target"
    }

    /// What a drop of files from Finder would do.
    enum FileDrop: Equatable {
        case replace(ModelID, URL)
        case insert([URL], ModelID?, Int)
    }

    /// A toolbar button was dropped: a new cue of that kind goes at that place (parent nil: the list's top
    /// level).
    var onKindDrop: ((NewCueKind, ModelID?, Int) -> Void)?

    /// The kind a drag from the toolbar carries.
    private func draggedKind(_ info: NSDraggingInfo) -> NewCueKind? {
        info.draggingPasteboard.string(forType: CueToolbarView.dragType).flatMap(NewCueKind.init(rawValue:))
    }

    /// The plan, and how many of the dragged files it leaves out (not audio).
    var onFileDrop: ((FileDrop, Int) -> Void)?
    /// The Target cell's button of an Audio cue was clicked.
    var onChooseFile: ((ModelID) -> Void)?

    /// The file URLs of a drag from Finder (empty for a drag of our own rows).
    private func fileURLs(_ info: NSDraggingInfo) -> [URL] {
        guard dragged.isEmpty else { return [] }
        let options: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        return info.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: options) as? [URL] ?? []
    }

    /// What a file drop does, or nil to refuse it: at least one sound, movie or still, nothing in
    /// Show Mode. A single file dropped on the row of a cue of its kind (a sound on an Audio cue, a
    /// movie or still on a Video cue) replaces that cue's file; anything else makes cues from the
    /// media files where it lands (other files in the drop are left out).
    func fileDropPlan(_ urls: [URL], item: Any?, index: Int) -> FileDrop? {
        let media = CueEditor.mediaExtensionFiles(urls)
        guard !isEditLocked, !media.isEmpty else { return nil }
        if urls.count == 1, index == NSOutlineViewDropOnItemIndex, let node = item as? CueNode,
            CueEditor.cueKind(forFile: urls[0])?.wireType == node.cue.kindType
        {
            return .replace(node.id, urls[0])
        }
        let target = dropTarget(item, index)
        return .insert(media, target.parent?.id, target.index)
    }

    /// How many dragged files `plan` leaves out (not media files).
    static func skipped(_ urls: [URL], _ plan: FileDrop) -> Int {
        if case .insert(let media, _, _) = plan { return urls.count - media.count }
        return 0
    }

    func outlineView(
        _ outlineView: NSOutlineView, validateDrop info: NSDraggingInfo, proposedItem item: Any?,
        proposedChildIndex index: Int
    ) -> NSDragOperation {
        if draggedKind(info) != nil {
            guard !isEditLocked else { return [] }
            // The outline draws its insertion line where the drop is set.
            let place = dropTarget(item, index)
            outlineView.setDropItem(place.parent, dropChildIndex: place.index)
            return .copy
        }
        let files = fileURLs(info)
        showTargetOutline(targetOutlineFor(dragged, at: outlineView.convert(info.draggingLocation, from: nil)))
        if !files.isEmpty {
            guard let plan = fileDropPlan(files, item: item, index: index) else { return [] }
            if case .insert(_, let parent, let at) = plan {
                outlineView.setDropItem(parent.flatMap { nodes[$0] }, dropChildIndex: at)
            }
            return .copy
        }
        switch cueDrop(dragged, item: item, index: index, inTargetColumn: overTargetColumn(info)) {
        case nil: return []
        case .setTarget?: return .move  // the row stays highlighted: the drop is on it, not between rows
        case .move(_, let parent, let at)?:
            outlineView.setDropItem(parent.flatMap { nodes[$0] }, dropChildIndex: at)
            return .move
        }
    }

    func outlineView(
        _ outlineView: NSOutlineView, acceptDrop info: NSDraggingInfo, item: Any?, childIndex index: Int
    ) -> Bool {
        showTargetOutline(nil)
        if let kind = draggedKind(info) {
            guard !isEditLocked else { return false }
            let place = dropTarget(item, index)
            onKindDrop?(kind, place.parent?.id, place.index)
            return true
        }
        let files = fileURLs(info)
        if !files.isEmpty {
            guard let plan = fileDropPlan(files, item: item, index: index) else { return false }
            onFileDrop?(plan, Self.skipped(files, plan))
            return true
        }
        guard let drop = cueDrop(dragged, item: item, index: index, inTargetColumn: overTargetColumn(info)) else {
            return false
        }
        perform(drop)
        return true
    }

    // MARK: In-place cell editing (QLab 5: double-click a cell of the Number, Name or Duration column,
    // manual pp. 92-95; the edit-number and edit-name keys open the same fields)

    /// The columns that edit in place, and the inspector field each one is.
    static let editableColumns: [String: InspectorField] = [
        "number": .number, "name": .name, "duration": .duration, "target": .target, "preWait": .preWait,
        "postWait": .postWait,
    ]

    /// The field laid over the cell being edited. It lives in the outline, not in the cell, so the
    /// per-tick refresh of the Duration column (progress) and model echoes cannot replace it mid-typing.
    let cellEditor = NSTextField()
    /// The cue and column being typed in.
    private(set) var editingCell: (id: ModelID, column: String)?
    /// A cell typed in and committed (Return, Tab or clicking away): the window controller sends it
    /// through `CueEditor`, which checks it as the inspector does, and returns once the engine has
    /// answered (the model already holds the change then: `model_changed` comes before the reply).
    var onEditCell: ((ModelID, String, String) async -> Void)?
    /// Counts committed edits, so only the latest one's answer takes the field off the cell.
    private var settling = 0
    /// The cue being edited was deleted: the edit is dropped, and the operator told.
    var onEditedCueGone: (() -> Void)?

    @objc private func doubleClicked() { doubleClick(row: outlineView.clickedRow, column: outlineView.clickedColumn) }

    /// A double-click on the cell at `row`, `column` (the table's clicked row and column).
    func doubleClick(row: Int, column: Int) {
        guard row >= 0, outlineView.tableColumns.indices.contains(column),
            let node = outlineView.item(atRow: row) as? CueNode
        else { return }
        let name = outlineView.tableColumns[column].identifier.rawValue
        guard Self.editableColumns[name] != nil else { return }
        // After the click is handled: the outline takes first responder back when its mouse
        // tracking ends, which would close a field opened from inside it.
        let id = node.id
        Task { @MainActor [weak self] in self?.beginCellEdit(id, column: name) }
    }

    /// The cell-editing keys (number, name, target, pre-wait, duration, post-wait: N, Q, T, E, D, W by
    /// default): the selected cue's cell opens for typing,
    /// or for T on a file cue the file chooser opens (after the key is consumed, not inside it). With
    /// several cues highlighted the pre-wait, duration and post-wait keys put the caret in the
    /// inspector's one field for them all (`onEditSeveral`), as QLab 5 does; the number, name and
    /// target keys open nothing. Only with the list itself focused. With the list focused the key is
    /// always taken (it does nothing in Show Mode or on a cue with nothing to edit); false (not taken)
    /// only while text is being typed or the focus is elsewhere (the key is a letter then).
    @discardableResult
    func beginEditFromKey(_ column: String) -> Bool {
        guard !isTextEditing, view.window?.firstResponder === outlineView else { return false }
        // From here the key is an edit key of the list: taken even when it does nothing (Show Mode, several
        // cues, a cue with nothing to edit), so it never goes on as a letter to the list.
        let selected = selectedIDs()
        if selected.count > 1 {
            if !isEditLocked, let field = Self.editableColumns[column], Self.severalCueFields.contains(field) {
                onEditSeveral?(field, selected)
            }
            return true
        }
        guard let id = selected.first else { return true }
        // A file cue's target is chosen in the file chooser, a cue target's typed as a cue number.
        if column == "target", store.model?.cue(id)?.takesFile == true {
            guard !isEditLocked, onChooseFile != nil else { return true }
            // The chooser is modal: opening it inside the key monitor would nest its loop there.
            Task { @MainActor [weak self] in
                guard let self, !self.isEditLocked else { return }
                self.onChooseFile?(id)
            }
            return true
        }
        _ = beginCellEdit(id, column: column)
        return true
    }

    /// The notes key (O): the window shows the inspector on its Basics tab with the caret in the
    /// selected cues' notes (`onEditNotes`; several cues share the one notes field). Taken or not as
    /// `beginEditFromKey`; it does nothing in Show Mode (the inspector is hidden there).
    @discardableResult
    func beginNotesFromKey() -> Bool {
        guard !isTextEditing, view.window?.firstResponder === outlineView else { return false }
        let selected = selectedIDs()
        guard !isEditLocked, !selected.isEmpty, selected.allSatisfy({ nodes[$0] != nil }) else { return true }
        onEditNotes?(selected)
        return true
    }

    /// Shows the inspector's notes of the selected cues for typing (the notes key).
    var onEditNotes: (([ModelID]) -> Void)?

    /// The fields an editing key opens in the inspector when several cues are selected: the list has a
    /// cell per cue, the inspector one field whose typed value goes to every selected cue.
    static let severalCueFields: Set<InspectorField> = [.preWait, .duration, .postWait]
    /// Puts the caret in the inspector's field for the selected cues (an editing key, several cues).
    var onEditSeveral: ((InspectorField, [ModelID]) -> Void)?

    /// The continue-mode and flag keys (C, F): every selected cue takes the value the first one moves
    /// to, as one undo step (QLab sets a batch-edited property to the same value on all, manual
    /// p. 123). Taken or not as `beginEditFromKey`.
    @discardableResult
    func performEditKey(_ key: TransportKeys.EditKey) -> Bool {
        guard !isTextEditing, view.window?.firstResponder === outlineView else { return false }
        let selected = selectedIDs()
        guard !isEditLocked, let first = selected.first.flatMap({ node($0) })?.cue else { return true }
        let property: CueProperty
        switch key {
        case .cell, .notes: return true
        case .cycleContinue:
            let modes = ["do_not_continue", "auto_continue", "auto_follow"]
            let now = modes.firstIndex(of: first.raw["continue_mode"]?.stringValue ?? "") ?? 0
            property = .continueMode(modes[(now + 1) % modes.count])
        case .toggleFlag: property = .flagged(first.raw["flagged"] != .bool(true))
        }
        Task { await editor?.setProperty(property, of: selected) }
        return true
    }

    /// Opens `column` ("number", "name", "target" or "duration") of `id` for typing, its text selected. False
    /// (nothing happens) in Show Mode or for a Duration that is not set by hand. A cell already open
    /// elsewhere is committed first.
    @discardableResult
    func beginCellEdit(_ id: ModelID, column: String = "duration") -> Bool {
        guard !isEditLocked, let field = Self.editableColumns[column], let node = nodes[id],
            outlineView.row(forItem: node) >= 0,
            field != .duration || InspectorEdit.editsDuration(node.cue, media: store.mediaDurations[id]),
            field != .target || InspectorEdit.targetKinds.contains(node.cue.kindType),
            // An auto-follow cue's post-wait is its duration, shown and not typed.
            field != .postWait || node.cue.raw["continue_mode"]?.stringValue != "auto_follow",
            let model = store.model
        else { return false }
        if let pending = takeCellEdit() { send(pending) }
        settling += 1  // a field still shown for an earlier commit is this edit's now
        cellEditor.isEditable = true
        editingCell = (id, column)
        cellEditor.stringValue = InspectorEdit.display(
            field, cue: node.cue, model: model, media: store.mediaDurations[node.id], inList: true)
        // An unnamed cue's editor starts empty, with the name it follows as a hint.
        cellEditor.placeholderString = column == "name" ? node.cue.targetDefaultName(lookup: cueLookup) : nil
        switch column {
        case "number": cellEditor.setAX(AX.cueNumberEditor, label: loc("cuelist.numberEditor.label"))
        case "name": cellEditor.setAX(AX.cueNameEditor, label: loc("cuelist.nameEditor.label"))
        case "target": cellEditor.setAX(AX.cueTargetEditor, label: loc("cuelist.targetEditor.label"))
        default: cellEditor.setAX(AX.cueDurationEditor, label: loc("cuelist.durationEditor.label"))
        }
        outlineView.scrollRowToVisible(outlineView.row(forItem: node))
        placeCellEditor()
        outlineView.addSubview(cellEditor)
        view.window?.makeFirstResponder(cellEditor)
        return true
    }

    /// Ends the in-place edit and returns what was typed, to be sent (nil when none was open).
    /// `keepShown`: the field gives up the caret but stays over its cell with the typed text.
    func takeCellEdit(keepShown: Bool = false) -> (id: ModelID, column: String, text: String)? {
        guard let cell = editingCell else { return nil }
        editingCell = nil
        let text = cellEditor.stringValue
        if keepShown {
            cellEditor.isEditable = false
            if cellEditor.currentEditor() != nil { view.window?.makeFirstResponder(outlineView) }
        } else {
            closeCellEditor()
        }
        return (cell.id, cell.column, text)
    }

    private func send(_ edit: (id: ModelID, column: String, text: String)) {
        guard let onEditCell else { return }
        Task { await onEditCell(edit.id, edit.column, edit.text) }
    }

    /// A committed edit: the field stays over the cell, showing what was typed, until the engine has
    /// answered. Taken away at once it would uncover the old value for the moment before the change
    /// comes back, which reads as a flash of the old name.
    private func commitCellEdit() {
        guard let edit = takeCellEdit(keepShown: true) else { return }
        settling += 1
        let mine = settling
        guard let onEditCell else { return closeCellEditor() }
        Task { [weak self] in
            await onEditCell(edit.id, edit.column, edit.text)
            guard let self, self.settling == mine, self.editingCell == nil else { return }
            self.closeCellEditor()
        }
    }

    /// Ends the in-place edit without sending anything (Esc, Show Mode, the cue gone).
    func cancelCellEdit() {
        guard editingCell != nil else { return }
        editingCell = nil
        closeCellEditor()
    }

    private func closeCellEditor() {
        // `editingCell` is already nil, so the end-of-editing this causes sends nothing.
        if cellEditor.currentEditor() != nil { view.window?.makeFirstResponder(outlineView) }
        cellEditor.removeFromSuperview()
        outlineView.setAccessibilityChildren(nil)  // back to the rows alone
    }

    /// The outline reports only its rows to accessibility; the open field is added to them (and made
    /// to name the outline as its parent) so VoiceOver and UI tests can reach it (#355).
    private func exposeCellEditor() {
        outlineView.setAccessibilityChildren(nil)
        let rows = outlineView.accessibilityChildren() ?? []
        cellEditor.setAccessibilityParent(outlineView)
        outlineView.setAccessibilityChildren(rows + [cellEditor])
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        refreshFlag(notification)
        refreshBoxes()
        replaceEditorAfterLayoutChange()
    }
    func outlineViewItemDidExpand(_ notification: Notification) {
        refreshFlag(notification)
        refreshBoxes()
        replaceEditorAfterLayoutChange()
    }

    /// A group's hollow flag shows only while it is collapsed, so opening or closing redraws it.
    private func refreshFlag(_ notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? CueNode else { return }
        refresh([node.id], columns: ["status"])
    }

    enum FlagMarker { case solid, hollow }

    /// QLab shows a flag in the status column, and on a collapsed group a hollow flag when a cue
    /// inside it is flagged. A cue's colour never adds a marker: it only tints the row.
    func flagMarker(_ node: CueNode, expanded: Bool) -> FlagMarker? {
        func flagged(_ n: CueNode) -> Bool { n.cue.raw["flagged"] == .bool(true) }
        func anyFlagged(_ n: CueNode) -> Bool { n.children.contains { flagged($0) || anyFlagged($0) } }
        if flagged(node) { return .solid }
        return node.isGroup && !expanded && anyFlagged(node) ? .hollow : nil
    }
    func outlineViewColumnDidResize(_ notification: Notification) {
        // Not while the Name column itself is being measured or set.
        let resized = notification.userInfo?["NSTableColumn"] as? NSTableColumn
        if let id = resized?.identifier.rawValue, !fitting, Self.timeColumns.contains(id), let resized {
            preferredTimeWidths[id] = resized.width
            draggedTimeColumns.insert(id)
        }
        if resized?.identifier.rawValue != "name" { fitNameColumn() }
        // The group boxes are drawn by the rows at the Name column's edges.
        outlineView.enumerateAvailableRowViews { view, _ in
            if let view = view as? CueRowView, !view.boxes.isEmpty { view.needsDisplay = true }
        }
        replaceEditorAfterLayoutChange()
    }
    func outlineViewColumnDidMove(_ notification: Notification) { replaceEditorAfterLayoutChange() }
    private func replaceEditorAfterLayoutChange() { if !rebuilding && !batching { placeCellEditor() } }

    /// Keeps the open editor over its cue's cell after rows moved; closes it if the cue is gone (the
    /// operator is told) or no longer has a duration to set (another client changed its kind).
    private func placeCellEditor() {
        guard let cell = editingCell else { return }
        guard let node = nodes[cell.id] else {
            cancelCellEdit()
            onEditedCueGone?()
            return
        }
        guard cell.column != "duration" || InspectorEdit.editsDuration(node.cue, media: store.mediaDurations[cell.id]),
            cell.column != "target" || InspectorEdit.targetKinds.contains(node.cue.kindType),
            let column = outlineView.tableColumns.firstIndex(where: { $0.identifier.rawValue == cell.column })
        else { return cancelCellEdit() }
        let row = outlineView.row(forItem: node)
        guard row >= 0 else { return cancelCellEdit() }
        var frame = outlineView.frameOfCell(atColumn: column, row: row)
        if cell.column == "name" {
            // A deeply nested row leaves its Name cell too narrow to see what is typed: the editor is at
            // least `nameEditorMinWidth` wide, reaching over the columns to its right, and kept inside the list.
            frame.size.width = max(frame.width, Self.nameEditorMinWidth)
            frame.origin.x = max(0, min(frame.minX, outlineView.bounds.maxX - frame.width))
        }
        cellEditor.frame = frame
        exposeCellEditor()
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard obj.object as? NSTextField === cellEditor else { return }
        commitCellEdit()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard control === cellEditor, selector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        cancelCellEdit()
        return true
    }

    // MARK: NSOutlineViewDelegate

    func outlineView(_ outlineView: NSOutlineView, rowViewForItem item: Any) -> NSTableRowView? {
        guard let node = item as? CueNode else { return nil }
        let id = NSUserInterfaceItemIdentifier("cue-row")
        let view = (outlineView.makeView(withIdentifier: id, owner: nil) as? CueRowView) ?? CueRowView()
        view.identifier = id
        configure(view, node)
        return view
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let node = item as? CueNode, let id = tableColumn?.identifier else { return nil }
        let cell = (outlineView.makeView(withIdentifier: id, owner: nil) as? CueCell) ?? CueCell(identifier: id)
        fill(cell, column: id.rawValue, node: node)
        return cell
    }

    // MARK: Cell content

    /// The state a cue shows: its run state, else broken when it has static problems. A cue its lost
    /// device stopped reads paused, with the loss as its detail.
    func displayState(_ id: ModelID) -> (state: WireCueState, detail: String?) {
        let run = store.cueStates[id]
        let problems = store.problems[id] ?? []
        let state = run?.state ?? (problems.isEmpty ? .idle : .broken)
        if state == .paused, let reason = run?.reason { return (state, ProblemText.pausedReason(reason)) }
        guard state == .broken else { return (state, nil) }
        let detail = CueStateStyle.brokenDetail(
            reason: run?.reason, problems: problems, groupWarning: store.groupWarnings.contains(id),
            label: WarningRows.labeler(store.model))
        return (state, detail)
    }

    // MARK: Time column headers

    /// The header chevron: switches the column between the full time and the time left.
    func toggleTimeShown(_ column: String) {
        guard Self.timeColumns.contains(column) else { return }
        timeShown[column] = timeShown[column] == .full ? .remaining : .full
        guard let index = outlineView.tableColumns.firstIndex(where: { $0.identifier.rawValue == column }) else {
            return
        }
        updateTimeHeader(outlineView.tableColumns[index])
        outlineView.headerView?.needsDisplay = true
        let rows = outlineView.numberOfRows
        if rows > 0 {
            outlineView.reloadData(forRowIndexes: IndexSet(0..<rows), columnIndexes: IndexSet(integer: index))
        }
        placeCellEditor()
    }

    private func updateTimeHeader(_ column: NSTableColumn) {
        let shown = timeShown[column.identifier.rawValue] ?? .remaining
        (column.headerCell as? TimeHeaderCell)?.shown = shown
        column.headerToolTip = loc(shown == .full ? "cuelist.time.showRemaining" : "cuelist.time.showFull")
    }

    func outlineView(_ outlineView: NSOutlineView, didClick tableColumn: NSTableColumn) {
        toggleTimeShown(tableColumn.identifier.rawValue)
    }

    private func fill(_ cell: CueCell, column: String, node: CueNode) {
        let cue = node.cue
        cell.label.stringValue = ""
        cell.label.alphaValue = cue.raw["armed"] == .bool(false) ? 0.5 : 1
        cell.label.textColor = .labelColor
        cell.label.isHidden = true
        for i in cell.images.indices { cell.setImage(i, icon: nil, label: nil) }
        cell.chooseButton.isHidden = true
        cell.onChoose = nil
        cell.progress.progress = nil
        cell.textInset = 0
        func text(_ s: String) {
            cell.label.isHidden = false
            cell.label.stringValue = s
        }
        switch column {
        case "status":
            // The row draws the pointer at its left edge; this slot keeps the room and the label. A closed group
            // around the playhead shows nothing here, as in QLab.
            if isPlayheadRow(node.id) {
                cell.setImage(0, icon: .playhead, label: loc("cue.playhead"), tint: .clear)
            }
            let (state, detail) = displayState(node.id)
            let style = CueStateStyle.style(for: state)
            if let icon = style.icon {
                cell.setImage(
                    1, icon: icon, label: style.label, tint: style.tint,
                    tooltip: detail.map { "\(style.label): \($0)" })
            }
            cell.showStopping(state == .stopping)
            // A preview is outside the show, so it has its own mark next to the cue's state.
            cell.images[2].setAccessibilityIdentifier("cue-preview")
            if store.previewing.contains(node.id) {
                cell.setImage(2, icon: .preview, label: loc("cue.previewing"), tint: .systemBlue)
            } else if cue.kindType == "network" && networkOutputDisabled {
                // A Network cue's preview holds no state, so it never takes that slot for a preview mark.
                cell.images[2].setAccessibilityIdentifier("cue-override")
                cell.setImage(2, icon: .outputOverridden, label: loc("cue.outputOverridden"), tint: .systemRed)
            }
            // The flag shares the preview's slot and gives way to a preview or override mark, which can be up
            // for a long time (an override lasts until it is turned off): the mark that holds the slot then
            // names the flag in its label and tooltip, so it is never lost.
            let flag = flagMarker(node, expanded: outlineView.isItemExpanded(node))
            let mark = cell.images[2]
            switch flag {
            case .solid where mark.isHidden:
                cell.setImage(3, icon: .flagged, label: loc("cue.flagged"), tint: .systemOrange)
            case .hollow where mark.isHidden:
                cell.setImage(3, icon: .flaggedInside, label: loc("cue.flagged.inside"), tint: .systemOrange)
            case .solid, .hollow:
                let flagText = loc(flag == .solid ? "cue.flagged" : "cue.flagged.inside")
                let label = String(format: loc("cue.markWithFlag"), mark.accessibilityLabel() ?? "", flagText)
                mark.setAccessibilityLabel(label)
                mark.toolTip = label
            case nil: break
            }
            if let style = CueKindStyle.typeStyle(kind: cue.raw["kind"]) {
                cell.setImage(4, icon: style.icon, label: style.label)
            }
        case "number": text(cue.number ?? "")
        case "name":
            // An unnamed cue with no default name shows an empty cell (QLab 5, manual p. 93).
            text(shownName(cue))
            cell.textInset = GroupBoxes.textInset(boxParts(node))
        case "target":
            if cue.takesFile {
                fillFileTargetButton(cell, cue)
            } else if InspectorEdit.targetKinds.contains(cue.kindType) {
                let shown = targetText(cue)
                text(shown ?? loc("cue.target.missing"))
                if shown == nil { cell.label.textColor = .systemYellow }
            }
        case "preWait", "postWait":
            // A wait in progress counts, as QLab's columns do.
            if let wait = store.waits[cue.id]?[column == "preWait" ? .preWait : .postWait] {
                text(CueTime.running(timeShown[column] == .full ? wait.elapsed : wait.remaining))
                break
            }
            // A Wait cue's post-wait is its duration (QLab 5, manual p. 688): running, it counts
            // with the action in every continue mode, not only when the engine times a post-wait.
            if column == "postWait", cue.kindType == "wait", let p = store.progress[cue.id], let left = p.remaining {
                text(CueTime.running(timeShown[column] == .full ? p.elapsed : left))
                if cue.raw["continue_mode"]?.stringValue == "auto_follow" {
                    cell.label.textColor = .secondaryLabelColor
                }
                break
            }
            // An auto-follow cue goes on when it ends: its post-wait reads its duration (QLab 5).
            if column == "postWait", cue.raw["continue_mode"]?.stringValue == "auto_follow",
                let length = restingDuration(node)
            {
                text(CueTime.running(length))
                cell.label.textColor = .secondaryLabelColor
                break
            }
            let seconds = column == "preWait" ? cue.raw["pre_wait"]?.doubleValue ?? 0 : cue.postWait
            text(CueTime.running(seconds))
            if seconds == 0 { cell.label.textColor = .secondaryLabelColor }  // zero is dimmed, as in QLab
        case "duration":
            text(durationText(node))
            cell.progress.progress = durationProgress(node)
        case "continue":
            if let mode = cue.raw["continue_mode"]?.stringValue.flatMap({ CueKindStyle.continueModes[$0] }) {
                cell.setImage(0, icon: mode.icon, label: loc(mode.labelKey))
            }
        default: break
        }
    }

    /// The name the list shows for `cue` (see `Cue.shownName`), found through the list's own tree.
    func shownName(_ cue: Cue) -> String { cue.shownName(lookup: cueLookup) }

    private var cueLookup: (ModelID) -> Cue? { { [weak self] in self?.nodes[$0]?.cue ?? self?.store.model?.cue($0) } }

    /// An Audio cue's Target cell is one button that chooses its file: a file glyph, or a trash can while the
    /// file is missing. It is empty in a locked (Show Mode) list, as QLab's Target column is.
    private func fillFileTargetButton(_ cell: CueCell, _ cue: Cue) {
        let id = cue.id
        // Drawn in Show Mode too, so the column does not change with the mode; the click does nothing there.
        // A file that is gone shows a trash can until it is back (the engine re-checks, so it clears itself).
        let missing = (store.problems[id] ?? []).contains { $0.type == "media_missing" }
        let file = cue.raw["kind"]?["media"]?.stringValue
        cell.showChoose(missing: missing, copying: store.copyingMedia.contains(id), file: file) { [weak self] in
            guard let self, !self.isEditLocked else { return }
            self.onChooseFile?(id)
        }
    }

    /// The target's number, or its name if it has none; nil with no target or one that is gone.
    private func targetText(_ cue: Cue) -> String? {
        guard let id = cue.raw["kind"]?["target"]?.stringValue.map({ ModelID($0) }),
            let target = cueLookup(id)
        else { return nil }
        if let number = target.number, !number.isEmpty { return number }
        return target.listName(lookup: cueLookup)
    }

    /// The Duration column's text: the cue's duration; while it runs or previews, the time gone, or
    /// the time left when the column shows that. A cue looping a slice with no end counts inside the slice
    /// and starts again each pass. Instantaneous cues (and Audio cues whose file is unknown) have none.
    private func durationText(_ node: CueNode) -> String {
        shownDurationSeconds(node).map(CueTime.running) ?? ""
    }

    /// The seconds the Duration cell reads, nil for none; the column widths are worked out from the same.
    private func shownDurationSeconds(_ node: CueNode) -> Double? {
        let remaining = timeShown["duration"] == .remaining
        // A cue loaded to a time stands there like a paused one: its Duration reads the point.
        if let stand = loadedStand(node) {
            guard let length = stand.length else { return stand.at }
            return remaining ? length - stand.at : stand.at
        }
        if let p = store.shownProgress(node.id) { return shownRunningTime(node, p) }
        return restingDuration(node)
    }

    /// The time the Duration cell reads for a running cue: the time gone, or the time left when the column
    /// shows that; inside the pass for a cue looping with no end.
    private func shownRunningTime(_ node: CueNode, _ p: ProgressItem) -> Double {
        let remaining = timeShown["duration"] == .remaining
        if let left = p.remaining { return remaining ? left : p.elapsed }
        if let pass = loopPass(node, p) { return remaining ? pass.length - pass.gone : pass.gone }
        return p.elapsed
    }

    /// Where a loaded cue stands in its action (Load to Time, a Load cue, a click in its timeline), and
    /// its length when known; nil for one loaded at its start.
    private func loadedStand(_ node: CueNode) -> (at: Double, length: Double?)? {
        guard store.cueStates[node.id]?.state == .loaded, let stand = store.loadedAt[node.id] else { return nil }
        // A click in the timeline stands at a point of the file; the cue's own time starts at its start trim.
        let fromFile = stand.position.map { $0 - (node.cue.raw["kind"]?["start_time"]?.doubleValue ?? 0) } ?? 0
        let at = stand.time > 0 ? stand.time : fromFile
        guard at > 0 else { return nil }
        guard let length = restingDuration(node), length > 0 else { return (at, nil) }
        return (min(at, length), length)
    }

    private func loopPass(_ node: CueNode, _ p: ProgressItem) -> (gone: Double, length: Double)? {
        LoopPass.of(node.cue, p, fileDuration: store.mediaDurations[node.id]?.file)
    }

    func restingDuration(_ node: CueNode) -> Double? {
        switch node.cue.kindType {
        case "fade", "wait": return node.cue.raw["kind"]?["duration"]?.doubleValue ?? 0
        case "audio": return store.mediaDurations[node.id]?.shownDuration
        case "video": return InspectorEdit.videoDuration(node.cue, media: store.mediaDurations[node.id])
        default: return nil
        }
    }

    /// How far a running, paused or previewing cue is, drawn in its Duration cell: green running,
    /// yellow paused (the Active Cues bar's colours).
    private func durationProgress(_ node: CueNode) -> (fraction: Double, colour: NSColor)? {
        if let stand = loadedStand(node) {
            guard let length = stand.length else { return nil }
            return (stand.at / length, MarkColour.caution)
        }
        guard let p = store.shownProgress(node.id) else { return nil }
        let paused = displayState(node.id).state == .paused
        let colour: NSColor = paused ? MarkColour.caution : .systemGreen
        guard let left = p.remaining else {
            return loopPass(node, p).map { ($0.gone / $0.length, colour) }
        }
        let total = p.elapsed + left
        guard total > 0 else { return nil }
        return (p.elapsed / total, colour)
    }
}
