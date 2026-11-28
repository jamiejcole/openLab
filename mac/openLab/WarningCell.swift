// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// One Warnings row: a marker symbol (so a warning is not told by colour alone), the title, the
/// reasons below it and, for missing media, a button that opens Find Missing Files. Laid out by
/// hand from the same measurements `height(of:width:)` adds up, so the row is always as tall as
/// what it draws, however the title and the reasons wrap: no warning is ever cut off.
final class WarningCell: NSTableCellView {
    let title = NSTextField(wrappingLabelWithString: "")
    let detail = NSTextField(wrappingLabelWithString: "")
    let relinkButton = NSButton(title: loc("warnings.relink"), target: nil, action: nil)
    let marker = NSImageView(image: WarningCell.markerImage)

    private static let markerImage = Icon.warningMarker.image(loc("warnings.marker"), compact: true)
    private static let brokenImage = Icon.stateBroken.image(loc("cue.state.broken"), compact: true)

    /// A broken cue is marked with the red cross its list row has; a notice with the caution mark.
    func setBroken(_ broken: Bool) {
        marker.image = broken ? Self.brokenImage : Self.markerImage
        marker.contentTintColor = broken ? .systemRed : MarkColour.caution
    }
    private static let titleFont = NSFont.systemFont(ofSize: 12, weight: .semibold)
    private static let detailFont = NSFont.systemFont(ofSize: 11)
    private static let inset: CGFloat = 6
    private static let top: CGFloat = 4
    private static let spacing: CGFloat = 2

    init() {
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("warning")
        marker.contentTintColor = MarkColour.caution
        title.font = Self.titleFont
        title.maximumNumberOfLines = 0
        detail.font = Self.detailFont
        detail.textColor = .secondaryLabelColor
        detail.maximumNumberOfLines = 0
        relinkButton.bezelStyle = .rounded
        relinkButton.controlSize = .small
        relinkButton.setAccessibilityIdentifier(AX.warningRelink)
        for v in [marker, title, detail, relinkButton] { addSubview(v) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    nonisolated override var isFlipped: Bool { true }

    /// On a selected row (the accent fill) the reasons take the selection's text colour, as the
    /// title does; the secondary grey would be faint on the accent.
    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            detail.textColor =
                backgroundStyle == .emphasized ? .alternateSelectedControlTextColor : .secondaryLabelColor
        }
    }

    private static let sizer: NSTextField = {
        let f = NSTextField(wrappingLabelWithString: "")
        f.font = detailFont
        return f
    }()
    private static let titleSizer: NSTextField = {
        let f = NSTextField(wrappingLabelWithString: "")
        f.font = titleFont
        return f
    }()
    private static let buttonSize: NSSize = {
        let b = NSButton(title: loc("warnings.relink"), target: nil, action: nil)
        b.bezelStyle = .rounded
        b.controlSize = .small
        return b.intrinsicContentSize
    }()

    /// Where the title, reasons and button start, and how wide they may be, in a cell `width` wide.
    private static func textColumn(_ width: CGFloat) -> (x: CGFloat, width: CGFloat) {
        let x = inset + markerImage.size.width + inset
        return (x, max(40, width - x - inset))
    }

    /// The height `text` takes wrapped in `width`, measured as the field draws it.
    private static func detailHeight(_ text: String, width: CGFloat) -> CGFloat {
        wrappedHeight(text, width: width, in: sizer)
    }

    /// The title's height wrapped in `width`: at least one line, so an empty title keeps its place.
    private static func titleHeight(_ text: String, width: CGFloat) -> CGFloat {
        wrappedHeight(text.isEmpty ? " " : text, width: width, in: titleSizer)
    }

    private static func wrappedHeight(_ text: String, width: CGFloat, in field: NSTextField) -> CGFloat {
        guard !text.isEmpty else { return 0 }
        field.stringValue = text
        return ceil(field.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: width, height: 100_000)).height ?? 0)
    }

    /// The height a row needs in a cell `width` wide: the title and the reasons wrap, so they are
    /// measured at the width they get beside the marker, not counted by line breaks.
    static func height(of w: WarningRow, width: CGFloat) -> CGFloat {
        let column = textColumn(width)
        var h = top + titleHeight(w.title, width: column.width)
        if !w.detail.isEmpty { h += spacing + detailHeight(w.detail, width: column.width) }
        if w.canRelink { h += spacing + buttonSize.height }
        return ceil(max(h, top + markerImage.size.height)) + top
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let column = Self.textColumn(bounds.width)
        marker.frame = NSRect(origin: NSPoint(x: Self.inset, y: Self.top), size: Self.markerImage.size)
        var y = Self.top
        let titleHeight = Self.titleHeight(title.stringValue, width: column.width)
        title.frame = NSRect(x: column.x, y: y, width: column.width, height: titleHeight)
        y += titleHeight
        if !detail.isHidden {
            let h = Self.detailHeight(detail.stringValue, width: column.width)
            detail.frame = NSRect(x: column.x, y: y + Self.spacing, width: column.width, height: h)
            y += Self.spacing + h
        }
        if !relinkButton.isHidden {
            let size = NSSize(width: min(Self.buttonSize.width, column.width), height: Self.buttonSize.height)
            relinkButton.frame = NSRect(origin: NSPoint(x: column.x, y: y + Self.spacing), size: size)
        }
    }
}
