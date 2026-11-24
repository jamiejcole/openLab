// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// One region as the stage editor draws it.
struct CanvasRegion: Equatable {
    /// Its letter (A for the first region of the stage).
    var letter: String
    var colour: NSColor
    /// The name of the route it is sent to; nil: none (drawn dashed).
    var route: String?
    /// Where it is on the stage, in stage pixels from the top left.
    var bounds: RegionBounds
}

/// The stage editor's canvas (QLab 5 manual p. 229): the stage as a light rectangle with grid lines,
/// fitted to the view, and each region on it as a rectangle of its colour labelled with its letter
/// and its route's name; the selected region is filled darker, a region with no route has a dashed
/// outline. A click on a region selects it (the topmost under the pointer, the last in the list).
@MainActor
final class StageCanvasView: NSView {
    var stageSize = NSSize(width: 1920, height: 1080) { didSet { needsDisplay = true } }
    var regions: [CanvasRegion] = [] { didSet { needsDisplay = true } }
    var selected: Int? { didSet { needsDisplay = true } }
    /// A region was clicked.
    var onSelect: ((Int) -> Void)?

    static let margin: CGFloat = 12

    nonisolated override var isFlipped: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    /// The stage's rectangle in the view: fitted inside the margin, its shape kept, centred.
    var stageFrame: NSRect {
        let room = bounds.insetBy(dx: Self.margin, dy: Self.margin)
        guard stageSize.width > 0, stageSize.height > 0, room.width > 0, room.height > 0 else { return .zero }
        let scale = min(room.width / stageSize.width, room.height / stageSize.height)
        let size = NSSize(width: stageSize.width * scale, height: stageSize.height * scale)
        return NSRect(
            x: (room.midX - size.width / 2).rounded(), y: (room.midY - size.height / 2).rounded(), width: size.width,
            height: size.height)
    }

    /// Each region's rectangle in the view, in list order.
    var regionFrames: [NSRect] {
        let stage = stageFrame
        guard stageSize.width > 0, stageSize.height > 0 else { return [] }
        let sx = stage.width / stageSize.width
        let sy = stage.height / stageSize.height
        return regions.map { r in
            NSRect(
                x: stage.minX + CGFloat(r.bounds.x) * sx, y: stage.minY + CGFloat(r.bounds.y) * sy,
                width: CGFloat(r.bounds.width) * sx, height: CGFloat(r.bounds.height) * sy)
        }
    }

    /// The label drawn in region `i`: its letter and its route's name.
    static func label(_ r: CanvasRegion) -> String {
        "\(r.letter) — \(r.route ?? loc("stageEditor.noRoute"))"
    }

    /// The region under `point` (view coordinates): the last drawn, so the one on top.
    func region(at point: NSPoint) -> Int? {
        regionFrames.indices.last { regionFrames[$0].contains(point) }
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        let stage = stageFrame
        guard stage.width > 0 else { return }
        NSColor(white: 0.96, alpha: 1).setFill()
        stage.fill()
        // Grid lines: eighths of the stage each way, the centre lines a little darker.
        for i in 1..<8 {
            let x = (stage.minX + stage.width * CGFloat(i) / 8).rounded() + 0.5
            let y = (stage.minY + stage.height * CGFloat(i) / 8).rounded() + 0.5
            NSColor(white: i == 4 ? 0.62 : 0.8, alpha: 1).setStroke()
            let path = NSBezierPath()
            path.move(to: NSPoint(x: x, y: stage.minY))
            path.line(to: NSPoint(x: x, y: stage.maxY))
            path.move(to: NSPoint(x: stage.minX, y: y))
            path.line(to: NSPoint(x: stage.maxX, y: y))
            path.lineWidth = 1
            path.stroke()
        }
        NSColor(white: 0.3, alpha: 1).setStroke()
        let edge = NSBezierPath(rect: stage.insetBy(dx: 0.5, dy: 0.5))
        edge.lineWidth = 1
        edge.stroke()
        for (i, (r, frame)) in zip(regions, regionFrames).enumerated() {
            let selected = i == self.selected
            r.colour.withAlphaComponent(selected ? 0.62 : 0.28).setFill()
            frame.fill()
            let outline = NSBezierPath(rect: frame.insetBy(dx: 1, dy: 1))
            outline.lineWidth = selected ? 2.5 : 1.5
            if r.route == nil { outline.setLineDash([6, 4], count: 2, phase: 0) }
            r.colour.setStroke()
            outline.stroke()
            let text = Self.label(r) as NSString
            let attributes: [NSAttributedString.Key: Any] = [
                .font: NSFont.systemFont(ofSize: 12, weight: .semibold),
                .foregroundColor: selected ? NSColor.white : NSColor.black,
            ]
            let size = text.size(withAttributes: attributes)
            let room = frame.insetBy(dx: 4, dy: 2)
            guard room.width > 8, room.height > size.height else { continue }
            let at = NSRect(
                x: max(room.minX, frame.midX - size.width / 2), y: frame.midY - size.height / 2,
                width: min(size.width, room.width), height: size.height)
            text.draw(
                with: at, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine], attributes: attributes)
        }
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        if let i = region(at: point) { onSelect?(i) }
    }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? { regions.map(Self.label).joined(separator: ", ") }
}

/// The colour of region `index` of its stage (A, B, ...): sixteen of openLab's own, as many as a
/// stage may have regions.
enum RegionColour {
    static let all: [NSColor] = [
        NSColor(srgbRed: 0.18, green: 0.33, blue: 0.80, alpha: 1),
        NSColor(srgbRed: 0.90, green: 0.45, blue: 0.10, alpha: 1),
        NSColor(srgbRed: 0.20, green: 0.62, blue: 0.30, alpha: 1),
        NSColor(srgbRed: 0.60, green: 0.25, blue: 0.70, alpha: 1),
        NSColor(srgbRed: 0.82, green: 0.20, blue: 0.25, alpha: 1),
        NSColor(srgbRed: 0.10, green: 0.60, blue: 0.65, alpha: 1),
        NSColor(srgbRed: 0.75, green: 0.62, blue: 0.05, alpha: 1),
        NSColor(srgbRed: 0.85, green: 0.35, blue: 0.60, alpha: 1),
        NSColor(srgbRed: 0.40, green: 0.45, blue: 0.20, alpha: 1),
        NSColor(srgbRed: 0.35, green: 0.50, blue: 0.90, alpha: 1),
        NSColor(srgbRed: 0.55, green: 0.35, blue: 0.20, alpha: 1),
        NSColor(srgbRed: 0.30, green: 0.70, blue: 0.55, alpha: 1),
        NSColor(srgbRed: 0.70, green: 0.30, blue: 0.45, alpha: 1),
        NSColor(srgbRed: 0.45, green: 0.30, blue: 0.85, alpha: 1),
        NSColor(srgbRed: 0.55, green: 0.55, blue: 0.55, alpha: 1),
        NSColor(srgbRed: 0.95, green: 0.60, blue: 0.40, alpha: 1),
    ]

    static func of(_ index: Int) -> NSColor { all[index % all.count] }

    /// "A" for the first region, then B and on; past Z (never: at most 16 regions) two letters.
    static func letter(_ index: Int) -> String {
        let letters = Array("ABCDEFGHIJKLMNOPQRSTUVWXYZ")
        return index < letters.count ? String(letters[index]) : "\(letters[index / 26 % 26])\(letters[index % 26])"
    }
}

/// A region's letter on its colour in the region list; a click selects the region, and the
/// selected one is ringed.
@MainActor
final class RegionChip: NSControl {
    var letter = "A" { didSet { needsDisplay = true } }
    var colour = NSColor.systemBlue { didSet { needsDisplay = true } }
    var isSelected = false { didSet { needsDisplay = true } }

    override var intrinsicContentSize: NSSize { NSSize(width: 22, height: 18) }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        let box = bounds.insetBy(dx: 2, dy: 1)
        colour.setFill()
        NSBezierPath(roundedRect: box, xRadius: 3, yRadius: 3).fill()
        if isSelected {
            NSColor.controlAccentColor.setStroke()
            let ring = NSBezierPath(roundedRect: bounds.insetBy(dx: 0.75, dy: 0.25), xRadius: 4, yRadius: 4)
            ring.lineWidth = 1.5
            ring.stroke()
        }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 11, weight: .bold), .foregroundColor: NSColor.white,
        ]
        let text = letter as NSString
        let size = text.size(withAttributes: attributes)
        text.draw(at: NSPoint(x: box.midX - size.width / 2, y: box.midY - size.height / 2), withAttributes: attributes)
    }

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        sendAction(action, to: target)
    }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .button }
    override func accessibilityPerformPress() -> Bool {
        sendAction(action, to: target)
    }
}
