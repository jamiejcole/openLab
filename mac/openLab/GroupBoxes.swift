// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The box a Group is drawn with in the Name column, by the group's mode (QLab 5 manual pp. 138-139):
/// Timeline green, Playlist orange, Start Random purple, Start First blue, Start First And Enter blue
/// with rounded corners.
struct GroupOutline: Equatable {
    var colour: NSColor
    var rounded: Bool

    static func of(mode: String?) -> GroupOutline {
        switch mode {
        case "playlist": return GroupOutline(colour: .systemOrange, rounded: false)
        case "start_random": return GroupOutline(colour: .systemPurple, rounded: false)
        case "start_first_and_enter": return GroupOutline(colour: .systemBlue, rounded: true)
        case "start_first_and_go_to_next": return GroupOutline(colour: .systemBlue, rounded: false)
        default: return GroupOutline(colour: .systemGreen, rounded: false)
        }
    }
}

/// The part of one group's box that crosses a row: an open group's box runs around its own Name cell
/// and the Name cells of the rows shown inside it, as one box; a nested group's box sits inside its
/// parent's. A row draws the parts of every box it is in.
struct GroupBoxPart: Equatable {
    /// The group's depth in the list (0 at the top level): how far in its box is drawn.
    var level: Int
    var outline: GroupOutline
    /// The row is the group's own: the box's top edge.
    var top: Bool
    /// The row is the last one shown inside the box: its bottom edge.
    var bottom: Bool
    /// Of the boxes ending on this row, how many are around this one: inner boxes end a little higher
    /// so each bottom edge shows.
    var bottomRank: Int = 0
    /// The group is a playlist and this row is the child it plays next: a small pointer on the box's
    /// left edge, in its colour.
    var marked: Bool = false
}

enum GroupBoxes {
    /// Space between a box's left edge and that of the box around it is the outline's indentation per
    /// level; on the right each level is this much further in.
    static let rightStep: CGFloat = 3
    /// The inset of the outermost edges from the Name column's sides and from a row's top and bottom.
    static let edge: CGFloat = 1.5
    /// How much higher an inner box's bottom edge is than the one around it, on a row both end on.
    static let bottomStep: CGFloat = 2
    /// The next child's pointer: a solid triangle on the box's left edge, pointing at the row.
    static let markSize = NSSize(width: 6, height: 9)

    /// The parts of the boxes `node`'s row draws. `expanded` says whether a group is open; `next` gives a
    /// Playlist group's next child. A closed group with children has no box of its own (only the boxes
    /// around it); an empty group's box is its own row.
    static func parts(
        for node: CueNode, expanded: (CueNode) -> Bool, next: (ModelID) -> ModelID?
    ) -> [GroupBoxPart] {
        var groups: [CueNode] = []
        var up = node.parent
        while let g = up {
            groups.append(g)
            up = g.parent
        }
        groups.reverse()
        var parts: [GroupBoxPart] = []
        for (level, group) in groups.enumerated() {
            parts.append(
                GroupBoxPart(
                    level: level, outline: outline(group), top: false,
                    bottom: lastShown(in: group, expanded: expanded) === node, marked: next(group.id) == node.id))
        }
        if node.isGroup && (node.children.isEmpty || expanded(node)) {
            parts.append(
                GroupBoxPart(
                    level: groups.count, outline: outline(node), top: true,
                    bottom: lastShown(in: node, expanded: expanded) === node))
        }
        // Outermost first, so the outermost bottom edge is the lowest.
        var rank = 0
        for i in parts.indices where parts[i].bottom {
            parts[i].bottomRank = rank
            rank += 1
        }
        return parts
    }

    static func outline(_ group: CueNode) -> GroupOutline {
        .of(mode: group.cue.raw["kind"]?["mode"]?["type"]?.stringValue)
    }

    /// The last row shown inside `group`'s box: its last child's, or that child's last shown row when
    /// it is an open group itself; the group's own row when it is closed or empty.
    static func lastShown(in group: CueNode, expanded: (CueNode) -> Bool) -> CueNode {
        var node = group
        while node.isGroup, expanded(node), let last = node.children.last { node = last }
        return node
    }

    /// The left and right edges of a box at `level`, in the coordinates of `column` (the Name column's
    /// rect), on pixel centres for a one-point line.
    static func edges(level: Int, column: NSRect, indent: CGFloat) -> (left: CGFloat, right: CGFloat) {
        let left = column.minX + CGFloat(level) * indent + edge
        let right = column.maxX - edge - CGFloat(level) * rightStep
        return (left.rounded(.down) + 0.5, right.rounded(.down) - 0.5)
    }

    /// How far in from the Name cell's right side its text must end so no box line crosses it.
    static func textInset(_ parts: [GroupBoxPart]) -> CGFloat {
        guard let deepest = parts.map(\.level).max() else { return 0 }
        return edge + CGFloat(deepest) * rightStep + 2
    }

    /// The pointer's rect on a row of height `rowBounds` for a box whose left edge is at `left`.
    static func markRect(left: CGFloat, rowBounds: NSRect) -> NSRect {
        NSRect(
            x: left, y: (rowBounds.midY - markSize.height / 2).rounded(), width: markSize.width,
            height: markSize.height)
    }

    /// Draws `parts` on a row whose bounds are `rowBounds` (`flipped`: y grows down), the Name column at
    /// `column` in the row's coordinates. Paths only: nothing is cached or allocated per row beyond them.
    static func draw(
        _ parts: [GroupBoxPart], rowBounds: NSRect, column: NSRect, indent: CGFloat, flipped: Bool
    ) {
        guard !parts.isEmpty else { return }
        NSGraphicsContext.saveGraphicsState()
        defer { NSGraphicsContext.restoreGraphicsState() }
        NSBezierPath.clip(rowBounds)
        // Top and bottom of the row as drawn (a flipped row has its top at minY).
        let topY = flipped ? rowBounds.minY : rowBounds.maxY
        let bottomY = flipped ? rowBounds.maxY : rowBounds.minY
        let down: CGFloat = flipped ? 1 : -1
        for part in parts {
            let (left, right) = edges(level: part.level, column: column, indent: indent)
            guard right - left > 2 else { continue }
            let line = edge.rounded(.down) + 0.5
            let top: CGFloat? = part.top ? topY + down * line : nil
            let bottom: CGFloat? =
                part.bottom ? bottomY - down * (line + CGFloat(part.bottomRank) * bottomStep) : nil
            let path = edgePath(
                left: left, right: right, top: top, bottom: bottom, rowTop: topY, rowBottom: bottomY,
                radius: part.outline.rounded ? 4 : 0)
            path.lineWidth = 1
            part.outline.colour.setStroke()
            path.stroke()
            if part.marked {
                let mark = markRect(left: left, rowBounds: rowBounds)
                let pointer = NSBezierPath()
                pointer.move(to: NSPoint(x: mark.minX, y: mark.minY))
                pointer.line(to: NSPoint(x: mark.maxX, y: mark.midY))
                pointer.line(to: NSPoint(x: mark.minX, y: mark.maxY))
                pointer.close()
                part.outline.colour.setFill()
                pointer.fill()
            }
        }
    }

    /// The lines of one box on one row: its sides from the row's top to its bottom, and the top or bottom
    /// edge where `top` or `bottom` is given, turning with `radius` at those corners.
    private static func edgePath(
        left: CGFloat, right: CGFloat, top: CGFloat?, bottom: CGFloat?, rowTop: CGFloat, rowBottom: CGFloat,
        radius: CGFloat
    ) -> NSBezierPath {
        let path = NSBezierPath()
        let upper = top ?? rowTop
        let lower = bottom ?? rowBottom
        // Two corners joined by an edge, from the side's open end around to the other side's.
        func turn(from start: CGFloat, at y: CGFloat, to end: CGFloat) {
            path.move(to: NSPoint(x: left, y: start))
            if radius > 0 {
                path.appendArc(from: NSPoint(x: left, y: y), to: NSPoint(x: right, y: y), radius: radius)
                path.appendArc(from: NSPoint(x: right, y: y), to: NSPoint(x: right, y: end), radius: radius)
            } else {
                path.line(to: NSPoint(x: left, y: y))
                path.line(to: NSPoint(x: right, y: y))
            }
            path.line(to: NSPoint(x: right, y: end))
        }
        switch (top, bottom) {
        case (let t?, let b?):
            let rect = NSRect(x: left, y: min(t, b), width: right - left, height: abs(b - t))
            path.appendRoundedRect(rect, xRadius: radius, yRadius: radius)
        case (let t?, nil): turn(from: lower, at: t, to: lower)
        case (nil, let b?): turn(from: upper, at: b, to: upper)
        case (nil, nil):
            path.move(to: NSPoint(x: left, y: upper))
            path.line(to: NSPoint(x: left, y: lower))
            path.move(to: NSPoint(x: right, y: upper))
            path.line(to: NSPoint(x: right, y: lower))
        }
        return path
    }
}
