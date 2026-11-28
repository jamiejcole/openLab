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

/// The stage editor's canvas (QLab 5 manual p. 229): the stage as a light rectangle with grid lines
/// (openLab's own simple grid), fitted to the view, its name and size in a box at its centre, and
/// each region on it as a rectangle of its colour labelled "A → route"; the selected region is
/// filled darker, a region with no route is lighter with a dashed outline and labelled by its letter
/// alone, and where regions overlap the overlap is hatched. A click on a region selects it (the
/// topmost under the pointer, the last in the list).
@MainActor
final class StageCanvasView: NSView {
    var stageSize = NSSize(width: 1920, height: 1080) { didSet { needsDisplay = true } }
    var stageName = "" { didSet { needsDisplay = true } }
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

    /// The label drawn in a region: its letter and its route's name, or the letter alone with no route.
    static func label(_ r: CanvasRegion) -> String {
        guard let route = r.route else { return r.letter }
        return String(format: loc("stageEditor.canvasLabel"), r.letter, route)
    }

    /// The stage's own label at its centre: its name over its size.
    var stageLabel: String {
        "\(stageName)\n\(String(format: loc("settings.video.resolution"), String(Int(stageSize.width)), String(Int(stageSize.height))))"
    }

    /// `stageLabel` with the name cut short, ending in an ellipsis, so the tag fits `width` (the
    /// stage's width): a long name must not make the whole label drop out, the size stays.
    func stageLabel(fitting width: CGFloat) -> String {
        let size = String(format: loc("settings.video.resolution"), String(Int(stageSize.width)), String(Int(stageSize.height)))
        let font = NSFont.systemFont(ofSize: 11, weight: .medium)
        let room = width - 2 - 10
        func wide(_ t: String) -> CGFloat { (t as NSString).size(withAttributes: [.font: font]).width }
        var name = stageName
        guard wide(name) > room else { return "\(name)\n\(size)" }
        while !name.isEmpty, wide(name + "…") > room { name.removeLast() }
        return "\(name)…\n\(size)"
    }

    /// Where two regions overlap, in view coordinates (each pair once).
    var overlaps: [NSRect] {
        let frames = regionFrames
        var out: [NSRect] = []
        for i in frames.indices {
            for j in frames.indices where j > i {
                let r = frames[i].intersection(frames[j])
                if !r.isEmpty, r.width > 0, r.height > 0 { out.append(r) }
            }
        }
        return out
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
        let frames = regionFrames
        for (i, (r, frame)) in zip(regions, frames).enumerated() {
            let selected = i == self.selected
            // No route: lighter, and dashed.
            let alpha: CGFloat = r.route == nil ? (selected ? 0.4 : 0.16) : (selected ? 0.62 : 0.3)
            r.colour.withAlphaComponent(alpha).setFill()
            frame.fill()
            let outline = NSBezierPath(rect: frame.insetBy(dx: 1, dy: 1))
            outline.lineWidth = selected ? 2.5 : 1.5
            if r.route == nil { outline.setLineDash([6, 4], count: 2, phase: 0) }
            r.colour.setStroke()
            outline.stroke()
        }
        // Overlaps hatched, over the regions' fills.
        for o in overlaps {
            NSGraphicsContext.saveGraphicsState()
            NSBezierPath.clip(o)
            NSColor(white: 0.15, alpha: 0.45).setStroke()
            let hatch = NSBezierPath()
            hatch.lineWidth = 1
            var x = o.minX - o.height
            while x < o.maxX {
                hatch.move(to: NSPoint(x: x, y: o.minY))
                hatch.line(to: NSPoint(x: x + o.height, y: o.maxY))
                x += 8
            }
            hatch.stroke()
            NSGraphicsContext.restoreGraphicsState()
        }
        drawTag(stageLabel(fitting: stage.width), at: NSPoint(x: stage.midX, y: stage.midY), within: stage, bold: false)
        for (i, (r, frame)) in zip(regions, frames).enumerated() {
            let selected = i == self.selected
            // A region covering the stage's middle has its label under the stage's own.
            let middle = frame.contains(NSPoint(x: stage.midX, y: stage.midY)) && frame.height > 60
            let y = middle ? frame.midY + 26 : frame.midY
            drawTag(Self.label(r), at: NSPoint(x: frame.midX, y: y), within: frame, bold: selected)
        }
    }

    /// A label on a small white tag centred on `point`, inside `room`; nothing where it does not fit.
    private func drawTag(_ text: String, at point: NSPoint, within room: NSRect, bold: Bool) {
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 11, weight: bold ? .bold : .medium),
            .foregroundColor: NSColor.black,
        ]
        let string = text as NSString
        let size = string.boundingRect(
            with: NSSize(width: 10_000, height: 10_000), options: [.usesLineFragmentOrigin], attributes: attributes
        ).size
        let tag = NSRect(
            x: (point.x - size.width / 2 - 5).rounded(), y: (point.y - size.height / 2 - 2).rounded(),
            width: ceil(size.width) + 10, height: ceil(size.height) + 4)
        guard tag.width <= room.width - 2, tag.height <= room.height - 2 else { return }
        NSColor(white: 1, alpha: 0.92).setFill()
        tag.fill()
        NSColor(white: 0.25, alpha: 1).setStroke()
        NSBezierPath(rect: tag.insetBy(dx: 0.5, dy: 0.5)).stroke()
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center
        var centred = attributes
        centred[.paragraphStyle] = paragraph
        string.draw(with: tag.insetBy(dx: 5, dy: 2), options: [.usesLineFragmentOrigin], attributes: centred)
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        if let i = region(at: point) { onSelect?(i) }
    }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? {
        ([stageName] + regions.map(Self.label)).joined(separator: ", ")
    }
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

/// A region's colour as a dot in the region table, beside its letter; a click selects the region,
/// and the selected one is ringed.
@MainActor
final class RegionChip: NSControl {
    var letter = "A" { didSet { needsDisplay = true } }
    var colour = NSColor.systemBlue { didSet { needsDisplay = true } }
    var isSelected = false { didSet { needsDisplay = true } }

    override var intrinsicContentSize: NSSize { NSSize(width: 16, height: 16) }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        let side = min(bounds.width, bounds.height) - 4
        let dot = NSRect(x: bounds.midX - side / 2, y: bounds.midY - side / 2, width: side, height: side)
        colour.setFill()
        NSBezierPath(ovalIn: dot).fill()
        if isSelected {
            NSColor.controlAccentColor.setStroke()
            let ring = NSBezierPath(ovalIn: dot.insetBy(dx: -1.5, dy: -1.5))
            ring.lineWidth = 1.5
            ring.stroke()
        }
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
