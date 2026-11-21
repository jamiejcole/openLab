// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The group boxes in the Name column (UI-07): one box per open group around its own Name cell and its
/// children's, coloured by mode, a nested group's inside its parent's, and a Playlist's next child marked
/// on the box's left edge. The Groups test show on the real engine: 1 Opening; 2 Storm (Timeline) { 2.1
/// Thunder (red), 2.2 Rain, 2.3 Inner playlist { 2.3.1, 2.3.2 } }; 3 Pre-show music (Playlist) { 3.1, 3.2,
/// 3.3 with a long name, 3 s Waits }; 4 Random; 5 Start First And Enter; 6 Start First; 7 an empty
/// Timeline group; 8 After the groups (red).
@MainActor
final class GroupBoxesTests: XCTestCase {
    typealias H = CueListHarness
    private var window: NSWindow?

    override func tearDown() async throws {
        window?.close()
        window = nil
    }

    // Colours as drawn (sRGB), light or dark: the system green, orange, purple and blue.
    private static let green: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in g - r > 0.3 && g - b > 0.2 }
    private static let orange: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in
        r > 0.85 && g > 0.4 && g < 0.75 && b < 0.3
    }
    private static let purple: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in r > 0.55 && b > 0.7 && g < 0.5 }
    private static let blue: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in b > 0.8 && r < 0.3 && g < 0.65 }

    /// The Groups show in a key window tall enough for every row, its groups open.
    private func harness(dark: Bool = true, show: String = "Groups") async throws -> H {
        let rig = try await Rig.open(show)
        let h = try await H(collapsed: false, rig: rig)
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 700), styleMask: [.titled, .resizable],
            backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        w.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        w.contentView = h.vc.view
        w.makeKeyAndOrderFront(nil)
        w.makeFirstResponder(h.spy)
        window = w
        w.layoutIfNeeded()
        if show == "Groups" {
            await assertEventually("the playlists' next children are known") { h.store.playlistNext.count == 2 }
        }
        return h
    }

    private func id(_ h: H, _ number: String) throws -> ModelID {
        func find(_ nodes: [CueNode]) -> CueNode? {
            for n in nodes {
                if n.cue.number == number { return n }
                if let f = find(n.children) { return f }
            }
            return nil
        }
        return try XCTUnwrap(find(h.vc.roots), "cue \(number)").id
    }

    private func row(_ h: H, _ number: String) throws -> CueRowView {
        let r = h.spy.row(forItem: h.vc.node(try id(h, number)))
        XCTAssertGreaterThanOrEqual(r, 0, "\(number) has a row")
        let view = try XCTUnwrap(h.spy.rowView(atRow: r, makeIfNecessary: true) as? CueRowView)
        view.layoutSubtreeIfNeeded()
        return view
    }

    /// A row as drawn, read at points of the row's own coordinates.
    private struct Drawn {
        let rep: NSBitmapImageRep
        let view: CueRowView
        let scale: CGFloat

        func colour(_ p: NSPoint) -> (CGFloat, CGFloat, CGFloat) {
            let y = view.isFlipped ? p.y : view.bounds.height - p.y
            let c = rep.colorAt(x: Int(p.x * scale), y: Int(y * scale))?.usingColorSpace(.sRGB)
            return (c?.redComponent ?? 0, c?.greenComponent ?? 0, c?.blueComponent ?? 0)
        }
        func matches(_ p: NSPoint, _ match: (CGFloat, CGFloat, CGFloat) -> Bool) -> Bool {
            let (r, g, b) = colour(p)
            return match(r, g, b)
        }
        /// The x of the left and right edges of the box at `level` on this row.
        func edges(_ level: Int) throws -> (left: CGFloat, right: CGFloat) {
            let column = try XCTUnwrap(view.nameColumn)
            return GroupBoxes.edges(level: level, column: column.rect, indent: column.indent)
        }
        /// The y of a box's top or bottom line (`rank`: boxes around it ending on the row too).
        var topLine: CGFloat { view.isFlipped ? 1.5 : view.bounds.height - 1.5 }
        func bottomLine(rank: Int = 0) -> CGFloat {
            let up = GroupBoxes.bottomOffset(rank: rank)
            return view.isFlipped ? view.bounds.height - up : up
        }
        var mid: CGFloat { view.bounds.midY }
    }

    private func drawn(_ view: CueRowView, dark: Bool = true) throws -> Drawn {
        view.layoutSubtreeIfNeeded()
        let scale: CGFloat = 2
        let rep = try renderView(view, scale: scale, background: dark ? .black : .white)
        return Drawn(rep: rep, view: view, scale: scale)
    }

    /// The Name label of `number`'s row in its row's coordinates.
    private func labelFrame(_ h: H, _ number: String) throws -> NSRect {
        let cell = try XCTUnwrap(h.cell("name", try id(h, number)))
        cell.layoutSubtreeIfNeeded()
        let row = try row(h, number)
        return cell.label.convert(cell.label.bounds, to: row)
    }

    /// Flow 1: each mode's box as QLab 5 draws it (manual pp. 138-139; UI map surfaces
    /// `inspector.group-*.basics`, whose cue lists show each mode's group): Timeline green, Playlist orange,
    /// Start Random purple, all square; Start First square blue; Start First And Enter blue with rounded
    /// corners.
    func testFlowEachGroupModesBoxIsDrawnInQLabsColourAndCorners() async throws {
        let h = try await harness()
        let expected: [(String, (CGFloat, CGFloat, CGFloat) -> Bool, Bool)] = [
            ("2", Self.green, false), ("3", Self.orange, false), ("4", Self.purple, false), ("5", Self.blue, true),
            ("6", Self.blue, false), ("7", Self.green, false),
        ]
        for (number, colour, rounded) in expected {
            let d = try drawn(try row(h, number))
            let (left, right) = try d.edges(0)
            XCTAssertTrue(d.matches(NSPoint(x: left, y: d.mid), colour), "\(number): its left edge in its colour")
            XCTAssertTrue(d.matches(NSPoint(x: right, y: d.mid), colour), "\(number): its right edge")
            XCTAssertTrue(d.matches(NSPoint(x: (left + right) / 2, y: d.topLine), colour), "\(number): its top edge")
            XCTAssertEqual(
                d.matches(NSPoint(x: left, y: d.topLine), colour), !rounded,
                "\(number): the top-left corner is \(rounded ? "rounded" : "square")")
        }
        XCTAssertEqual(GroupOutline.of(mode: "start_first_and_go_to_next").rounded, false)
        XCTAssertEqual(GroupOutline.of(mode: "start_first_and_enter").rounded, true)
        // A cue that is in no group draws no box.
        XCTAssertTrue(try row(h, "1").boxes.isEmpty)
        XCTAssertTrue(try row(h, "8").boxes.isEmpty)
    }

    /// Flow 2: an open Timeline group draws one green box around its Name cell and its children's
    /// (top edge on its own row, bottom on its last shown row); closed, it draws none.
    func testFlowAnOpenTimelineGroupIsOneGreenBoxAroundItsAndItsChildrensNameCellsAndClosedHasNone() async throws {
        let h = try await harness()
        let inside = ["2", "2.1", "2.2", "2.3", "2.3.1", "2.3.2"]
        for number in inside {
            let view = try row(h, number)
            let outer = try XCTUnwrap(view.boxes.first, number)
            XCTAssertEqual(outer.level, 0)
            XCTAssertEqual(outer.top, number == "2", "\(number): only the group's own row has the top edge")
            XCTAssertEqual(outer.bottom, number == "2.3.2", "\(number): only its last shown row has the bottom")
            let d = try drawn(view)
            let (left, right) = try d.edges(0)
            XCTAssertTrue(d.matches(NSPoint(x: left, y: d.mid), Self.green), "\(number): inside the green box")
            XCTAssertTrue(d.matches(NSPoint(x: right, y: d.mid), Self.green), number)
            // The box runs through the whole row, joining the rows above and below.
            XCTAssertTrue(d.matches(NSPoint(x: left, y: number == "2" ? d.topLine : 0.2), Self.green), number)
            // Inside the Name column.
            let column = try XCTUnwrap(view.nameColumn).rect
            XCTAssertTrue(left > column.minX && right < column.maxX, "\(number): inside the Name column")
        }
        let top = try drawn(try row(h, "2"))
        XCTAssertTrue(top.matches(NSPoint(x: 400, y: top.topLine), Self.green), "the top edge across the group's row")
        let child = try drawn(try row(h, "2.1"))
        XCTAssertFalse(child.matches(NSPoint(x: 400, y: child.topLine), Self.green), "no edge between its rows")
        let last = try drawn(try row(h, "2.3.2"))
        XCTAssertTrue(last.matches(NSPoint(x: 400, y: last.bottomLine()), Self.green), "the bottom edge")
        for outside in ["1", "8"] {
            let d = try drawn(try row(h, outside))
            XCTAssertFalse(d.matches(NSPoint(x: try d.edges(0).left, y: d.mid), Self.green), "\(outside) is outside")
        }
        // An empty group's box is its own row.
        XCTAssertEqual(try row(h, "7").boxes.map { [$0.top, $0.bottom] }, [[true, true]])

        // Closed: no box of its own; the rows that were inside it are gone and the next group's box is
        // its own colour.
        h.spy.collapseItem(h.vc.node(try id(h, "2")))
        XCTAssertTrue(try row(h, "2").boxes.isEmpty, "a closed group draws no box")
        let closed = try drawn(try row(h, "2"))
        XCTAssertFalse(closed.matches(NSPoint(x: try closed.edges(0).left, y: closed.mid), Self.green))
        XCTAssertFalse(closed.matches(NSPoint(x: 400, y: closed.topLine), Self.green))
        XCTAssertEqual(h.spy.row(forItem: h.vc.node(try id(h, "2.1"))), -1)
        h.spy.expandItem(h.vc.node(try id(h, "2")))
        XCTAssertEqual(try row(h, "2").boxes.first?.top, true, "open again: boxed again")
    }

    /// Flow 3: a Playlist group's box is orange and a small orange pointer on its left edge shows the
    /// child that plays next; it moves as the playlist plays on and back to the first child after a reset.
    func testFlowAPlaylistsMarkShowsTheChildThatPlaysNextAndMovesAsItPlaysAndAfterAReset() async throws {
        let h = try await harness()
        let rig = h.rig
        let playlist = try id(h, "3")
        func marked() throws -> [String] {
            try ["3.1", "3.2", "3.3"].filter { number in
                let view = try row(h, number)
                let d = try drawn(view)
                let left = try d.edges(0).left
                let drawnMark = d.matches(NSPoint(x: left + 2.5, y: d.mid), Self.orange)
                XCTAssertEqual(view.boxes.first?.marked, drawnMark, "\(number): the mark drawn is the one set")
                return drawnMark
            }
        }
        XCTAssertEqual(try marked(), ["3.1"], "not playing: its first child")
        let d = try drawn(try row(h, "3.1"))
        XCTAssertTrue(d.matches(NSPoint(x: try d.edges(0).left, y: d.mid), Self.orange), "the playlist's box is orange")

        let (second, third, first) = (try id(h, "3.2"), try id(h, "3.3"), try id(h, "3.1"))
        h.vc.selectAsUser([playlist])
        let list = try XCTUnwrap(h.vc.listID)
        await assertEventually { rig.store.playheads[list] == playlist }
        guard case .success = await rig.store.go(list: list) else { return XCTFail("go") }
        try await rig.advance(0.5)
        await assertEventually { rig.store.playlistNext[playlist] == second }
        XCTAssertEqual(try marked(), ["3.2"], "the first child plays: the second is next")
        try await rig.advance(3)
        await assertEventually { rig.store.playlistNext[playlist] == third }
        XCTAssertEqual(try marked(), ["3.3"])
        try await rig.advance(3)
        await assertEventually { rig.store.playlistNext[playlist] == nil }
        XCTAssertEqual(try marked(), [], "the last child plays: nothing comes after it")
        guard case .success = await rig.store.transport(.reset(cue: playlist)) else { return XCTFail("reset") }
        await assertEventually { rig.store.playlistNext[playlist] == first }
        XCTAssertEqual(try marked(), ["3.1"], "reset: its first child again")
    }

    /// Flow 4: a nested group's box is inside its parent's, the bottoms of both show where they end on one
    /// row, no edge crosses a name, and a selected or coloured row inside a box still reads.
    func testFlowNestedGroupsEachDrawTheirOwnBoxInsideTheirParentsAndNothingOverlapsTheText() async throws {
        let h = try await harness()
        let d = try drawn(try row(h, "2.3.2"))
        let outer = try d.edges(0)
        let inner = try d.edges(1)
        XCTAssertGreaterThan(inner.left, outer.left + 4, "the inner box is inside on the left")
        XCTAssertLessThan(inner.right, outer.right - 2, "and on the right")
        XCTAssertTrue(d.matches(NSPoint(x: inner.left, y: d.mid), Self.orange), "the inner playlist's box")
        XCTAssertTrue(d.matches(NSPoint(x: outer.left, y: d.mid), Self.green), "inside the timeline's")
        XCTAssertTrue(d.matches(NSPoint(x: 400, y: d.bottomLine(rank: 0)), Self.green), "the outer bottom")
        XCTAssertTrue(d.matches(NSPoint(x: 400, y: d.bottomLine(rank: 1)), Self.orange), "the inner bottom above it")
        XCTAssertEqual(try row(h, "2.3.1").boxes.map(\.level), [0, 1])
        // Nothing overlaps a name: every label lies between the edges of every box on its row.
        for number in ["2", "2.1", "2.2", "2.3", "2.3.1", "2.3.2", "3", "3.1", "3.3", "5", "7"] {
            let view = try row(h, number)
            let label = try labelFrame(h, number)
            let column = try XCTUnwrap(view.nameColumn)
            for part in view.boxes {
                let e = GroupBoxes.edges(level: part.level, column: column.rect, indent: column.indent)
                XCTAssertGreaterThan(label.minX, e.left + 1, "\(number): its name clears the box's left edge")
                XCTAssertLessThan(label.maxX, e.right - 1, "\(number): and its right edge")
            }
        }
        // The colour of 2.1 (red) and a selected row's highlight still show inside the box.
        let tinted = try drawn(try row(h, "2.1"))
        let plain = try drawn(try row(h, "2.2"))
        XCTAssertNotEqual(
            tinted.colour(NSPoint(x: 600, y: tinted.mid)).0, plain.colour(NSPoint(x: 600, y: plain.mid)).0,
            "the row's colour reads")
        h.spy.selectRowIndexes(
            IndexSet(integer: h.spy.row(forItem: h.vc.node(try id(h, "2.2")))), byExtendingSelection: false)
        let selected = try row(h, "2.2")
        selected.isEmphasized = true
        let s = try drawn(selected)
        XCTAssertTrue(s.matches(NSPoint(x: try s.edges(0).left, y: s.mid), Self.green), "the box over the highlight")
        let highlight = s.colour(NSPoint(x: 600, y: s.mid))
        XCTAssertGreaterThan(highlight.2, 0.5, "the highlight reads inside the box")
    }

    /// Flow: a cue as the last row of five nested groups: the five bottom edges close under its name, and
    /// no box line crosses the name's text, at any depth.
    func testFlowFiveNestedBoxesClosingOnOneRowLeaveTheNameClear() async throws {
        for dark in [true, false] {
            let h = try await harness(dark: dark, show: "Nested")
            let number = "1.1.1.1.1.1"
            let view = try row(h, number)
            XCTAssertEqual(view.boxes.filter(\.bottom).count, 5, "five boxes end on the row")
            let d = try drawn(view, dark: dark)
            let label = try labelFrame(h, number)
            let ink = label.insetBy(dx: 0, dy: 1)
            let top = view.isFlipped ? ink.minY : view.bounds.height - ink.maxY
            let colours = [Self.green, Self.orange, Self.purple, Self.blue]
            for part in view.boxes where part.bottom {
                let line = d.bottomLine(rank: part.bottomRank)
                let y = view.isFlipped ? line : view.bounds.height - line
                XCTAssertTrue(
                    y > top + ink.height || y < top,
                    "the bottom edge of level \(part.level) is outside the name's frame")
            }
            // No pixel of a box colour lies in the name's frame across its middle.
            let x = label.midX
            for step in stride(from: ink.minY, to: ink.maxY, by: 0.5) {
                let p = NSPoint(x: x, y: view.isFlipped ? step : view.bounds.height - step)
                XCTAssertFalse(
                    colours.contains { d.matches(p, $0) },
                    "a box line crosses the name at y \(step) (\(dark ? "dark" : "light"))")
            }
        }
    }

    /// Flow 5: light and dark, the least width with a long name, and a 2,000-row list scrolled through
    /// with its row views reused (the boxes are paths drawn by each row; nothing is made per row).
    func testFlowLightAndDarkTheLeastWidthALongNameAnd2000RowsScrolling() async throws {
        for dark in [false, true] {
            let h = try await harness(dark: dark)
            let d = try drawn(try row(h, "3.1"), dark: dark)
            let left = try d.edges(0).left
            XCTAssertTrue(d.matches(NSPoint(x: left, y: d.mid), Self.orange), "dark \(dark): the box")
            XCTAssertTrue(d.matches(NSPoint(x: left + 2.5, y: d.mid), Self.orange), "dark \(dark): the mark")
            let g = try drawn(try row(h, "2.1"), dark: dark)
            XCTAssertTrue(g.matches(NSPoint(x: try g.edges(0).left, y: g.mid), Self.green), "dark \(dark)")
            // The least width: the Name column at its narrowest; the long name ends in an ellipsis inside.
            let w = try XCTUnwrap(window)
            w.setContentSize(NSSize(width: 520, height: 700))
            w.layoutIfNeeded()
            let view = try row(h, "3.3")
            let column = try XCTUnwrap(view.nameColumn)
            XCTAssertLessThan(column.rect.width, 200, "the Name column is narrow")
            let label = try labelFrame(h, "3.3")
            let cell = try XCTUnwrap(h.cell("name", try id(h, "3.3")))
            XCTAssertGreaterThan(cell.label.attributedStringValue.size().width, label.width, "the long name is cut")
            for part in view.boxes {
                let e = GroupBoxes.edges(level: part.level, column: column.rect, indent: column.indent)
                XCTAssertLessThan(label.maxX, e.right - 1, "dark \(dark): it ends inside the box")
            }
            let narrow = try drawn(view, dark: dark)
            XCTAssertTrue(
                narrow.matches(NSPoint(x: try narrow.edges(0).right, y: narrow.mid), Self.orange),
                "dark \(dark): the right edge follows the narrow column")
            window?.close()
            window = nil
        }

        // 2,000 rows, groups open: scrolled from top to bottom, each step drawn.
        let h = try await H(snapshot: TestSnapshots.bigSnapshot(cues: 2000), collapsed: false)
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 600), styleMask: [.titled, .resizable],
            backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        w.contentView = h.vc.view
        window = w
        w.layoutIfNeeded()
        var views = Set<ObjectIdentifier>()
        var boxedRows = 0
        let clock = ContinuousClock()
        let took = clock.measure {
            for row in stride(from: 0, to: h.spy.numberOfRows, by: 10) {
                h.spy.scrollRowToVisible(row)
                w.displayIfNeeded()
                h.spy.enumerateAvailableRowViews { view, _ in
                    views.insert(ObjectIdentifier(view))
                    if let v = view as? CueRowView, !v.boxes.isEmpty { boxedRows += 1 }
                }
            }
        }
        let seconds = Double(took.components.seconds) + Double(took.components.attoseconds) / 1e18
        print("perf: 2000 rows with group boxes scrolled in 200 steps: \(seconds * 1000) ms")
        XCTAssertGreaterThan(boxedRows, 0, "the groups' rows are boxed")
        XCTAssertLessThan(views.count, 200, "row views are reused, not made per row")
        XCTAssertLessThan(seconds, 20)
    }
}
