// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A time column's header: its title and a small chevron. Down means the column shows the time
/// left, up the whole time; a click on the header switches (see `CueListViewController.toggleTimeShown`).
final class TimeHeaderCell: NSTableHeaderCell {
    var shown: TimeShown = .remaining

    private static let chevronWidth: CGFloat = 14

    override func drawInterior(withFrame cellFrame: NSRect, in controlView: NSView) {
        var text = cellFrame
        text.size.width = max(0, text.width - Self.chevronWidth)
        super.drawInterior(withFrame: text, in: controlView)
        let icon: Icon = shown == .remaining ? .timeToggle : .timeToggleUp
        let image = icon.image(
            loc(shown == .remaining ? "cuelist.time.showFull" : "cuelist.time.showRemaining"), compact: true)
        let size = NSSize(width: 9, height: 9)
        let rect = NSRect(
            x: cellFrame.maxX - Self.chevronWidth + 2, y: cellFrame.midY - size.height / 2, width: size.width,
            height: size.height)
        Self.tinted(image, .secondaryLabelColor, size: size).draw(in: rect)
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
