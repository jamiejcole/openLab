// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// One line of the Triggers tab that shows on a single line where the tab is wide enough and breaks
/// its tail onto a second line, indented, where it is not (the inspector narrowed by the sidebar at
/// the least window).
@MainActor
final class WrappingRow {
    let view: NSStackView
    private let first = NSStackView()
    private let second = NSStackView()
    private let head: [NSView]
    private let tail: [NSView]

    /// `indent` is how far the second line starts from the left edge.
    init(head: [NSView], tail: [NSView], indent: CGFloat, spacing: CGFloat = 6) {
        self.head = head
        self.tail = tail
        for s in [first, second] {
            s.orientation = .horizontal
            s.alignment = .firstBaseline
            s.spacing = spacing
        }
        second.edgeInsets = NSEdgeInsets(top: 0, left: indent, bottom: 0, right: 0)
        view = NSStackView(views: [first, second])
        view.orientation = .vertical
        view.alignment = .leading
        view.spacing = 4
        setWrapped(false)
    }

    func setWrapped(_ wrapped: Bool) {
        first.setViews(wrapped ? head : head + tail, in: .leading)
        second.setViews(wrapped ? tail : [], in: .leading)
        second.isHidden = !wrapped || tail.isEmpty
    }
}

/// The Triggers tab's two columns with a divider between them. Wide enough for every row on one
/// line they are laid out so; narrower, their rows wrap (`WrappingRow`). The width a one-line
/// layout needs is measured while it is shown, so a longer translation moves the break.
@MainActor
final class TriggersColumns: NSStackView {
    var rows: [WrappingRow] = []
    private(set) var compact = false
    private var wideWidth: CGFloat = 0

    override func layout() {
        super.layout()
        if !compact { wideWidth = fittingSize.width }
        // The room is the tab's, not this view's own width: left alone this view keeps the width its
        // controls ask for.
        let narrow = (superview?.bounds.width ?? bounds.width) < wideWidth
        if narrow != compact {
            compact = narrow
            for row in rows { row.setWrapped(narrow) }
            needsLayout = true
        }
    }
}

/// The thin vertical rule between the columns, in the system's separator colour in either appearance.
final class SeparatorLine: NSView {
    override var wantsUpdateLayer: Bool { true }
    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
    }
    required init?(coder: NSCoder) { nil }
    override var intrinsicContentSize: NSSize { NSSize(width: 1, height: NSView.noIntrinsicMetric) }
    override func updateLayer() {
        layer?.backgroundColor = NSColor.separatorColor.cgColor
    }
    override func viewDidChangeEffectiveAppearance() { needsDisplay = true }
}
