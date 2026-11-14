// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Counts what the cue list asks of the table, so tests can tell a targeted update from a reload.
final class SpyOutlineView: NSOutlineView {
    var wholeReloads = 0
    var inserts = 0
    var removals = 0
    var moves = 0
    var itemReloads = 0
    var rowReloads: [(rows: IndexSet, columns: IndexSet)] = []

    override func reloadData() {
        wholeReloads += 1
        super.reloadData()
    }
    override func insertItems(
        at indexes: IndexSet, inParent parent: Any?, withAnimation: NSTableView.AnimationOptions = []
    ) {
        inserts += indexes.count
        super.insertItems(at: indexes, inParent: parent, withAnimation: withAnimation)
    }
    override func removeItems(
        at indexes: IndexSet, inParent parent: Any?, withAnimation: NSTableView.AnimationOptions = []
    ) {
        removals += indexes.count
        super.removeItems(at: indexes, inParent: parent, withAnimation: withAnimation)
    }
    override func moveItem(at fromIndex: Int, inParent oldParent: Any?, to toIndex: Int, inParent newParent: Any?) {
        moves += 1
        super.moveItem(at: fromIndex, inParent: oldParent, to: toIndex, inParent: newParent)
    }
    override func reloadItem(_ item: Any?, reloadChildren: Bool) {
        itemReloads += 1
        super.reloadItem(item, reloadChildren: reloadChildren)
    }
    override func reloadData(forRowIndexes rowIndexes: IndexSet, columnIndexes: IndexSet) {
        rowReloads.append((rowIndexes, columnIndexes))
        super.reloadData(forRowIndexes: rowIndexes, columnIndexes: columnIndexes)
    }

    /// inserts, removals, moves, whole-table reloads.
    var counts: [Int] { [inserts, removals, moves, wholeReloads] }
    var rowsReloaded: Int { rowReloads.reduce(0) { $0 + $1.rows.count } }
    func resetCounts() {
        (wholeReloads, inserts, removals, moves, itemReloads) = (0, 0, 0, 0, 0)
        rowReloads = []
    }
}

@MainActor
final class CueListHarness {
    let rig: Rig
    let spy = SpyOutlineView()
    let vc: CueListViewController
    var store: WorkspaceStore { rig.store }

    static let list = TestSnapshots.mainList.raw
    static let group: ModelID = "00000000-0000-0000-0000-000000000020"
    static let door: ModelID = "00000000-0000-0000-0000-000000000021"
    static let rain: ModelID = "00000000-0000-0000-0000-000000000022"
    static let radio: ModelID = "00000000-0000-0000-0000-000000000023"
    static let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
    static let fade: ModelID = "00000000-0000-0000-0000-00000000000b"

    /// The list shows `snapshot` (default: `demoSnapshot()`) with no engine (`Rig.offline`): what
    /// the list does with what the engine sends. `rig`: a real-engine rig to show instead. The table
    /// has a size so rows exist.
    init(snapshot: JSONValue? = nil, collapsed: Bool = true, rig real: Rig? = nil) async throws {
        if let real {
            rig = real
        } else {
            rig = try Rig.offline(snapshot ?? Self.demoSnapshot())
        }
        vc = CueListViewController(store: rig.store, outlineView: spy)
        vc.view.frame = NSRect(x: 0, y: 0, width: 1000, height: 600)
        vc.view.layoutSubtreeIfNeeded()
        // Groups open expanded; most tests want the compact list.
        func collapse(_ nodes: [CueNode]) {
            for n in nodes where n.isGroup {
                collapse(n.children)
                spy.collapseItem(n)
            }
        }
        if collapsed { collapse(vc.roots) }
        spy.resetCounts()
    }

    func receive(_ json: String) throws {
        XCTAssertEqual(store.receive(try ServerMessage.decode(json)), .applied)
    }

    /// The demo show (mac/TestShows/Demo) as the engine would send it mid-show: the golden welcome's
    /// runtime state (Intro running, the Fade at the playhead and reported broken, Intro's probed
    /// length) plus, in the group, Door slam loaded, Rain paused and Radio broken by a lost device,
    /// with Door slam's and Rain's probed lengths.
    static func demoSnapshot() throws -> JSONValue {
        var s = try TestSnapshots.show("Demo")
        let welcome = try XCTUnwrap(Fixtures.json("server", "welcome")["snapshot"])
        for key in ["revision", "runtime_seq", "problems", "group_warnings", "playheads", "warnings"] {
            s.set(key, welcome[key] ?? .null)
        }
        func state(_ cue: ModelID, _ state: String, reason: String? = nil) -> JSONValue {
            var o: [String: JSONValue] = ["cue": .string(cue.raw), "state": .string(state)]
            if let reason { o["reason"] = .string(reason) }
            return .object(o)
        }
        s.set(
            "cue_states",
            .array([
                state(TestSnapshots.intro, "running"), state(TestSnapshots.door, "loaded"),
                state(TestSnapshots.rain, "paused"), state(TestSnapshots.radio, "broken", reason: "device_lost"),
            ]))
        func length(_ cue: ModelID, _ file: Double, trimmed: Double, duration: Double) -> JSONValue {
            .object([
                "cue": .string(cue.raw), "file": .double(file), "trimmed": .double(trimmed),
                "duration": .double(duration),
            ])
        }
        s.set(
            "media_durations",
            .array([
                length(TestSnapshots.intro, 62.5, trimmed: 60, duration: 120),
                length(TestSnapshots.door, 1.25, trimmed: 1.25, duration: 1.25),
                length(TestSnapshots.rain, 95.5, trimmed: 95.5, duration: 95.5),
            ]))
        return s
    }

    static func upsertJSON(_ cue: JSONValue, parent: ModelID? = nil, index: Int) throws -> String {
        let p = parent.map { "\"\($0)\"" } ?? "null"
        return #"{"type":"cue","list":"\#(list)","parent":\#(p),"index":\#(index),"cue":\#(try cue.text())}"#
    }

    func change(upserts: [String] = [], deletes: [ModelID] = []) throws {
        let d = deletes.map { #"{"type":"cue","id":"\#($0)"}"# }.joined(separator: ",")
        try receive(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[\#(upserts.joined(separator: ","))],"deletes":[\#(d)]}"#
        )
    }

    func runtime(_ changes: String) throws {
        try receive(#"{"type":"runtime","seq":\#(store.runtimeSeq + 1),"changes":[\#(changes)]}"#)
    }

    func cue(
        _ id: String, _ name: String, kind: JSONValue = TestSnapshots.audioKind("media/x.wav"), color: String = "none"
    )
        -> JSONValue
    {
        TestSnapshots.cueJSON(id: id, number: nil, name: name, kind: kind, color: color)
    }

    /// The ids in table order, top to bottom.
    func visibleIDs() -> [ModelID] {
        (0..<spy.numberOfRows).compactMap { (spy.item(atRow: $0) as? CueNode)?.id }
    }

    func cell(_ column: String, _ id: ModelID) -> CueCell? {
        let row = spy.row(forItem: vc.node(id))
        let col = spy.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        return spy.view(atColumn: col, row: row, makeIfNecessary: true) as? CueCell
    }
}

@MainActor
final class CueListTests: XCTestCase {
    typealias H = CueListHarness

    // MARK: Model mapping

    func testTreeMatchesTheModel() async throws {
        let h = try await H()
        XCTAssertEqual(h.vc.roots.map(\.id), [H.intro, H.fade, H.group])
        let group = try XCTUnwrap(h.vc.node(H.group))
        XCTAssertEqual(group.children.map(\.id), [H.door, H.rain, H.radio])
        XCTAssertTrue(group.children.allSatisfy { $0.parent === group })
        XCTAssertEqual(h.vc.nodeCount, 6)
        XCTAssertEqual(h.vc.listID?.raw, H.list)
    }

    func testGroupsOpenExpandedAndCollapseToTheirRow() async throws {
        let h = try await H(collapsed: false)
        XCTAssertEqual(h.visibleIDs(), [H.intro, H.fade, H.group, H.door, H.rain, H.radio])
        // A group the engine adds later opens expanded too; one that was collapsed and moves stays so.
        let kids = [h.cue("00000000-0000-0000-0000-0000000000a1", "Kid")]
        let g = TestSnapshots.cueJSON(
            id: "00000000-0000-0000-0000-0000000000a0", number: nil, name: "New", kind: TestSnapshots.groupKind(kids))
        try h.change(upserts: [try H.upsertJSON(g, index: 0)])
        XCTAssertEqual(
            h.visibleIDs().prefix(2).map(\.raw),
            ["00000000-0000-0000-0000-0000000000a0", "00000000-0000-0000-0000-0000000000a1"])
        h.spy.collapseItem(try XCTUnwrap(h.vc.node(H.group)))
        let current = try XCTUnwrap(h.store.model?.cue(H.group))
        let regroup = TestSnapshots.cueJSON(
            id: H.group.raw, number: "2", name: "Act One",
            kind: TestSnapshots.groupKind(
                current.children.map { h.cue($0.id.raw, $0.name) }))
        try h.change(upserts: [try H.upsertJSON(regroup, index: 0)], deletes: [H.group])
        XCTAssertFalse(h.spy.isItemExpanded(try XCTUnwrap(h.vc.node(H.group))))
        XCTAssertFalse(h.visibleIDs().contains(H.door))
    }

    func testGroupsStartCollapsedAndExpandToTheirChildren() async throws {
        let h = try await H()
        XCTAssertEqual(h.visibleIDs(), [H.intro, H.fade, H.group])
        let group = try XCTUnwrap(h.vc.node(H.group))
        XCTAssertTrue(h.spy.isExpandable(group))
        XCTAssertFalse(h.spy.isExpandable(try XCTUnwrap(h.vc.node(H.intro))))
        h.spy.expandItem(group)
        XCTAssertEqual(h.visibleIDs(), [H.intro, H.fade, H.group, H.door, H.rain, H.radio])
        h.spy.collapseItem(group)
        XCTAssertEqual(h.visibleIDs().count, 3)
    }

    func testColumnsAreQLabOrder() {
        XCTAssertEqual(
            CueListViewController.columns.map(\.id),
            ["status", "type", "number", "name", "target", "preWait", "duration", "postWait", "continue"])
    }

    /// A default name follows a chain: Fade 4 of Fade 3 of Intro. Renaming Intro redraws row 4 at once
    /// (read with no click in between, which would repaint it).
    func testRenamingATargetRedrawsTheDefaultNamesThatFollowItDownTheChain() async throws {
        let h = try await H()
        let fade = try XCTUnwrap(h.store.model?.cue(H.fade))
        var second = fade.raw
        let fourth: ModelID = "00000000-0000-0000-0000-0000000000f4"
        second.set("id", .string(fourth.raw))
        second.set("number", .null)
        var kind = try XCTUnwrap(second["kind"])
        kind.set("target", .string(H.fade.raw))
        second.set("kind", kind)
        try h.change(upserts: [try H.upsertJSON(second, index: 2)])
        XCTAssertEqual(h.cell("name", fourth)?.label.stringValue, "Fade and stop Fade and stop Intro")
        var intro = try XCTUnwrap(h.store.model?.cue(H.intro)).raw
        intro.set("name", .string("entrance"))
        try h.change(upserts: [try H.upsertJSON(intro, index: 0)])
        XCTAssertEqual(h.cell("name", H.fade)?.label.stringValue, "Fade and stop entrance")
        XCTAssertEqual(h.cell("name", fourth)?.label.stringValue, "Fade and stop Fade and stop entrance")
    }

    func testCellsShowNumberNameTargetAndTimes() async throws {
        let h = try await H()
        XCTAssertEqual(h.cell("number", H.intro)?.label.stringValue, "1")
        XCTAssertEqual(h.cell("name", H.intro)?.label.stringValue, "Intro")
        XCTAssertEqual(h.cell("target", H.intro)?.label.stringValue, "")
        // The fade cue has no name and no number: its default name follows the cue it fades, and it points at cue 1.
        XCTAssertEqual(h.cell("name", H.fade)?.label.stringValue, "Fade and stop Intro")
        XCTAssertEqual(h.cell("duration", H.fade)?.label.stringValue, "00:05.00")
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "1")
        // Zero waits show as a dimmed 00:00.00, a non-zero one in the normal colour.
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:00.00")
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.textColor, .secondaryLabelColor)
        var timed = TestSnapshots.cueJSON(
            id: H.intro.raw, number: "1", name: "Intro", kind: TestSnapshots.audioKind("media/a.wav"), preWait: 1.5)
        timed.set("armed", .bool(true))
        try h.change(upserts: [try H.upsertJSON(timed, index: 0)])
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:01.50")
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.textColor, .labelColor)
        XCTAssertEqual(h.cell("continue", H.intro)?.images[0].isHidden, true)
    }

    func testTimeFormat() {
        XCTAssertEqual(CueTime.running(0), "00:00.00")
        XCTAssertEqual(CueTime.running(1), "00:01.00")
        XCTAssertEqual(CueTime.running(60), "01:00.00")
        XCTAssertEqual(CueTime.running(65.254), "01:05.25")
        // A stored time keeps its millisecond, so an edit field opened and closed changes nothing.
        XCTAssertEqual(CueTime.format(65.254), "01:05.254")
        XCTAssertEqual(CueTime.format(65.25), "01:05.25")
        XCTAssertEqual(CueTime.running(3661.5), "1:01:01.50")
        XCTAssertEqual(CueTime.running(.nan), "")
        XCTAssertEqual(CueTime.running(-1), "")
        XCTAssertEqual(CueTime.progress(elapsed: 2, remaining: 3), "00:02.00 / 00:05.00")
        XCTAssertEqual(CueTime.progress(elapsed: 2, remaining: nil), "00:02.00")
    }

    func testColourTintsRowFlagIsInStatusColumnAndContinueHasLabel() async throws {
        let h = try await H()
        let h2 = h.vc
        h.spy.expandItem(try XCTUnwrap(h2.node(H.group)))
        let name = try XCTUnwrap(h.cell("name", H.rain))
        // A colour adds no marker to the Name column; the flag is a status-column icon.
        XCTAssertTrue(name.images.allSatisfy(\.isHidden))
        let status = try XCTUnwrap(h.cell("status", H.rain))
        XCTAssertFalse(status.images[3].isHidden)
        XCTAssertEqual(status.images[3].accessibilityLabel(), loc("cue.flagged"))
        // A flagged cue inside a group: the open group shows no flag, the collapsed one a hollow flag.
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.group)).images[3].isHidden)
        h.spy.collapseItem(try XCTUnwrap(h2.node(H.group)))
        let groupFlag = try XCTUnwrap(h.cell("status", H.group)).images[3]
        XCTAssertEqual(groupFlag.accessibilityLabel(), loc("cue.flagged.inside"))
        h.spy.expandItem(try XCTUnwrap(h2.node(H.group)))
        let cont = try XCTUnwrap(h.cell("continue", H.rain))
        XCTAssertEqual(cont.images[0].accessibilityLabel(), loc("cue.continue.autoFollow"))
        // Row tint is the colour; a cue with no colour has none.
        let row = try XCTUnwrap(
            h.spy.rowView(atRow: h.spy.row(forItem: h2.node(H.rain)), makeIfNecessary: true) as? CueRowView)
        XCTAssertEqual(row.tint, NSColor.systemBlue)
        let plain = try XCTUnwrap(h.spy.rowView(atRow: 0, makeIfNecessary: true) as? CueRowView)
        XCTAssertNil(plain.tint)
    }

    func testCollapsedGroupFlagFollowsAChildFlaggedByTheEngine() async throws {
        let h = try await H()
        h.spy.collapseItem(try XCTUnwrap(h.vc.node(H.group)))
        func groupFlag() throws -> NSImageView { try XCTUnwrap(h.cell("status", H.group)).images[3] }
        XCTAssertFalse(try groupFlag().isHidden, "Rain is flagged")
        func rain(flagged: Bool) throws -> String {
            var json = TestSnapshots.cueJSON(
                id: H.rain.raw, number: "2.2", name: "Rain", kind: TestSnapshots.audioKind("media/r.wav"))
            json.set("flagged", .bool(flagged))
            return try H.upsertJSON(json, parent: H.group, index: 1)
        }
        // Only the child is upserted; the collapsed group's row must still redraw.
        try h.change(upserts: [try rain(flagged: false)])
        XCTAssertTrue(try groupFlag().isHidden)
        try h.change(upserts: [try rain(flagged: true)])
        XCTAssertEqual(try groupFlag().accessibilityLabel(), loc("cue.flagged.inside"))
        try h.change(deletes: [H.rain])
        XCTAssertTrue(try groupFlag().isHidden, "a deleted flagged child clears the flag")
    }

    func testDisarmedCueIsDimmed() async throws {
        let h = try await H()
        var cue = TestSnapshots.cueJSON(
            id: H.intro.raw, number: "1", name: "Intro", kind: TestSnapshots.audioKind("media/a.wav"))
        cue.set("armed", .bool(false))
        try h.change(upserts: [try H.upsertJSON(cue, index: 0)])
        XCTAssertEqual(h.cell("name", H.intro)?.label.alphaValue, 0.5)
        XCTAssertEqual(h.cell("name", H.fade)?.label.alphaValue, 1)
    }

    // MARK: Incremental updates

    func testInsertIsOneRowInsertNotAReload() async throws {
        let h = try await H()
        try h.change(upserts: [try H.upsertJSON(h.cue("00000000-0000-0000-0000-0000000000f1", "New"), index: 1)])
        XCTAssertEqual(h.vc.roots.map(\.id.raw).count, 4)
        XCTAssertEqual(h.visibleIDs()[1].raw, "00000000-0000-0000-0000-0000000000f1")
        XCTAssertEqual(h.spy.counts, [1, 0, 0, 0])
    }

    func testDeleteIsOneRowRemoval() async throws {
        let h = try await H()
        try h.change(deletes: [H.fade])
        XCTAssertEqual(h.visibleIDs(), [H.intro, H.group])
        XCTAssertNil(h.vc.node(H.fade))
        XCTAssertEqual(h.spy.counts, [0, 1, 0, 0])
    }

    func testDeletingAGroupForgetsItsChildren() async throws {
        let h = try await H()
        try h.change(deletes: [H.group])
        XCTAssertEqual(h.vc.nodeCount, 2)
        XCTAssertNil(h.vc.node(H.rain))
        XCTAssertEqual(h.spy.removals, 1)
    }

    func testChangeInPlaceReloadsThatRowOnly() async throws {
        let h = try await H()
        let renamed = TestSnapshots.cueJSON(
            id: H.intro.raw, number: "1", name: "Overture", kind: TestSnapshots.audioKind("media/a.wav"))
        try h.change(upserts: [try H.upsertJSON(renamed, index: 0)])
        XCTAssertEqual(h.cell("name", H.intro)?.label.stringValue, "Overture")
        XCTAssertEqual(h.spy.counts + [h.spy.itemReloads], [0, 0, 0, 0, 1])
    }

    func testMoveWithinTheListIsOneMoveAndKeepsOrderEqualToTheStore() async throws {
        let h = try await H()
        // The engine sends a group with its whole subtree; use the store's own copy for the children.
        let current = try XCTUnwrap(h.store.model?.cue(H.group))
        XCTAssertEqual(current.children.count, 3)
        var json = TestSnapshots.cueJSON(
            id: H.group.raw, number: "2", name: "Act One",
            kind: TestSnapshots.groupKind(
                current.children.map { c in
                    TestSnapshots.cueJSON(
                        id: c.id.raw, number: c.number, name: c.name, kind: TestSnapshots.audioKind("media/z.wav"))
                }), color: "green")
        json.set("flagged", .bool(false))
        try h.change(upserts: [try H.upsertJSON(json, index: 0)])
        XCTAssertEqual(h.vc.roots.map(\.id), [H.group, H.intro, H.fade])
        XCTAssertEqual(h.vc.roots.map(\.id), h.store.model?.cueLists.first?.cues.map(\.id))
        XCTAssertEqual(h.visibleIDs(), [H.group, H.intro, H.fade])
        XCTAssertEqual(h.spy.counts, [0, 0, 1, 0])
    }

    func testMovingDownTheListLandsAtTheEnginesIndex() async throws {
        let h = try await H()
        let intro = TestSnapshots.cueJSON(
            id: H.intro.raw, number: "1", name: "Intro", kind: TestSnapshots.audioKind("media/a.wav"))
        try h.change(upserts: [try H.upsertJSON(intro, index: 1)])
        XCTAssertEqual(h.vc.roots.map(\.id), [H.fade, H.intro, H.group])
        XCTAssertEqual(h.visibleIDs(), [H.fade, H.intro, H.group])
        try h.change(upserts: [try H.upsertJSON(intro, index: 2)])
        XCTAssertEqual(h.visibleIDs(), [H.fade, H.group, H.intro])
        XCTAssertEqual(h.visibleIDs(), h.store.model?.cueLists.first?.cues.map(\.id))
        XCTAssertEqual(h.spy.moves, 2)
    }

    func testMoveIntoAGroupMatchesTheStoreAndChildrenKeepTheirParent() async throws {
        let h = try await H()
        // Engine form of a move: a delete, then an upsert of the group that now holds the cue.
        let current = try XCTUnwrap(h.store.model?.cue(H.group))
        var kids = current.children.map { c in
            TestSnapshots.cueJSON(
                id: c.id.raw, number: c.number, name: c.name, kind: TestSnapshots.audioKind("media/z.wav"))
        }
        kids.insert(h.cue(H.fade.raw, "Fade moved"), at: 0)
        let group = TestSnapshots.cueJSON(
            id: H.group.raw, number: "2", name: "Act One", kind: TestSnapshots.groupKind(kids))
        try h.change(upserts: [try H.upsertJSON(group, index: 1)], deletes: [H.fade])
        let node = try XCTUnwrap(h.vc.node(H.group))
        XCTAssertEqual(node.children.map(\.id), [H.fade, H.door, H.rain, H.radio])
        XCTAssertEqual(h.vc.node(H.fade)?.parent, node)
        XCTAssertEqual(h.vc.roots.map(\.id), [H.intro, H.group])
        XCTAssertEqual(h.vc.nodeCount, 6)
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }

    func testGroupChildChangeKeepsExpansionOfTheGroup() async throws {
        let h = try await H()
        let group = try XCTUnwrap(h.vc.node(H.group))
        h.spy.expandItem(group)
        let kids = [
            TestSnapshots.cueJSON(
                id: H.door.raw, number: "2.1", name: "Door slam", kind: TestSnapshots.audioKind("media/d.wav")),
            TestSnapshots.cueJSON(
                id: H.rain.raw, number: "2.2", name: "Storm", kind: TestSnapshots.audioKind("media/r.wav")),
        ]
        let json = TestSnapshots.cueJSON(
            id: H.group.raw, number: "2", name: "Act One", kind: TestSnapshots.groupKind(kids))
        try h.change(upserts: [try H.upsertJSON(json, index: 2)])
        XCTAssertTrue(h.spy.isItemExpanded(group))
        XCTAssertEqual(h.visibleIDs(), [H.intro, H.fade, H.group, H.door, H.rain])
        XCTAssertNil(h.vc.node(H.radio))
        XCTAssertEqual(h.cell("name", H.rain)?.label.stringValue, "Storm")
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }

    func testACueThatStopsBeingAGroupLosesItsChildren() async throws {
        let h = try await H()
        let memo = TestSnapshots.cueJSON(
            id: H.group.raw, number: "2", name: "Act One", kind: .object(["type": .string("memo")]))
        try h.change(upserts: [try H.upsertJSON(memo, index: 2)])
        XCTAssertEqual(h.vc.node(H.group)?.children.count, 0)
        XCTAssertNil(h.vc.node(H.rain))
        XCTAssertEqual(h.vc.nodeCount, 3)
        XCTAssertFalse(h.spy.isExpandable(try XCTUnwrap(h.vc.node(H.group))))
    }

    func testChildUpsertUnderAnExpandedGroupInsertsOneRow() async throws {
        let h = try await H()
        h.spy.expandItem(try XCTUnwrap(h.vc.node(H.group)))
        h.spy.resetCounts()
        let id = "00000000-0000-0000-0000-0000000000f2"
        try h.change(upserts: [try H.upsertJSON(h.cue(id, "Kid"), parent: H.group, index: 1)])
        XCTAssertEqual(h.visibleIDs().map(\.raw)[4], id)
        XCTAssertEqual(h.vc.node(H.group)?.children.count, 4)
        XCTAssertEqual([h.spy.inserts, h.spy.wholeReloads], [1, 0])
    }

    func testRenameRefreshesTheTargetCellOfCuesThatPointAtIt() async throws {
        let h = try await H()
        let target = TestSnapshots.cueJSON(
            id: H.intro.raw, number: nil, name: "Overture", kind: TestSnapshots.audioKind("media/a.wav"))
        var fade = TestSnapshots.cueJSON(
            id: H.fade.raw, number: nil, name: "",
            kind: .object(["type": .string("fade"), "target": .string(H.intro.raw), "duration": .double(5)]))
        fade.set("armed", .bool(true))
        try h.change(upserts: [try H.upsertJSON(fade, index: 1)])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "1")
        h.spy.resetCounts()
        try h.change(upserts: [try H.upsertJSON(target, index: 0)])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "Overture")
        let fadeRow = h.spy.row(forItem: h.vc.node(H.fade))
        XCTAssertTrue(h.spy.rowReloads.contains { $0.rows.contains(fadeRow) })
    }

    func testSnapshotReloadsOnceAndKeepsSelectionAndExpansion() async throws {
        let h = try await H()
        h.spy.expandItem(try XCTUnwrap(h.vc.node(H.group)))
        h.vc.select([H.fade, H.rain])
        h.spy.resetCounts()
        var next = try H.demoSnapshot()
        next.set("revision", .int(900))
        let message = try ServerMessage.decode(
            try JSONValue.object(["type": .string("snapshot"), "snapshot": next]).text())
        h.store.receive(message)
        XCTAssertEqual(h.spy.wholeReloads, 1)
        XCTAssertEqual(Set(h.vc.selectedIDs()), [H.fade, H.rain])
        XCTAssertTrue(h.spy.isItemExpanded(h.vc.node(H.group)))
    }

    func testListShowsTheStoreThatConnectsLater() async throws {
        let engine = try await TestEngine.shared()
        let rig = Rig(engine: engine, workspace: try await engine.open("Demo"))
        let vc = CueListViewController(store: rig.store)
        XCTAssertEqual(vc.roots.count, 0)
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        XCTAssertEqual(vc.roots.count, 3)
    }

    func testTreeThatDisagreesWithTheEngineResyncsFromTheStore() async throws {
        let h = try await H()
        // An upsert under a parent the list never held: the store would call this a gap; the list
        // must not crash or keep a half-applied tree. (The store ignores it by refusing the message.)
        let msg = try ServerMessage.decode(
            #"{"type":"model_changed","revision":\#(h.store.revision + 1),"upserts":[\#(try H.upsertJSON(h.cue("00000000-0000-0000-0000-0000000000f3", "Lost"), parent: "00000000-0000-0000-0000-0000000000ee", index: 0))],"deletes":[]}"#
        )
        XCTAssertEqual(h.store.receive(msg), .gap)
        XCTAssertEqual(h.vc.roots.map(\.id), [H.intro, H.fade, H.group])
    }

    // MARK: Selection

    func testSelectionSurvivesInsertAndDeleteAbove() async throws {
        let h = try await H()
        h.vc.select([H.group])
        try h.change(upserts: [try H.upsertJSON(h.cue("00000000-0000-0000-0000-0000000000f4", "Top"), index: 0)])
        XCTAssertEqual(h.vc.selectedIDs(), [H.group])
        try h.change(deletes: [H.intro])
        XCTAssertEqual(h.vc.selectedIDs(), [H.group])
    }

    func testSelectionSurvivesAMove() async throws {
        let h = try await H()
        h.vc.select([H.intro, H.group])
        let moved = TestSnapshots.cueJSON(
            id: H.fade.raw, number: nil, name: "", kind: .object(["type": .string("memo")]))
        try h.change(upserts: [try H.upsertJSON(moved, index: 0)])
        XCTAssertEqual(h.spy.moves, 1)
        XCTAssertEqual(Set(h.vc.selectedIDs()), [H.intro, H.group])
    }

    func testSelectionOfADeletedCueIsDropped() async throws {
        let h = try await H()
        h.vc.select([H.intro, H.fade])
        try h.change(deletes: [H.fade])
        XCTAssertEqual(h.vc.selectedIDs(), [H.intro])
    }

    // MARK: State icons (UI-04)

    func testEveryStateHasALabelAndEveryNonIdleStateItsOwnSymbol() {
        var symbols = Set<String>()
        for state in WireCueState.allCases {
            let style = CueStateStyle.style(for: state)
            XCTAssertNotEqual(style.label, style.labelKey, "\(state) has no label string")
            XCTAssertFalse(style.label.isEmpty)
            if state == .idle {
                XCTAssertNil(style.icon)
                continue
            }
            let icon = try? XCTUnwrap(style.icon, "\(state)")
            XCTAssertEqual(icon?.resolvesToSymbol, true, "\(state)")
            XCTAssertTrue(symbols.insert(icon?.symbol ?? "").inserted, "\(state) shares a symbol")
        }
        XCTAssertEqual(
            Set(WireCueState.allCases.map { CueStateStyle.style(for: $0).label }).count, WireCueState.allCases.count)
    }

    func testEveryBrokenReasonHasAString() {
        for reason in WireBrokenReason.allCases {
            XCTAssertNotEqual(ProblemText.reason(reason), "broken.\(reason.rawValue)", "\(reason)")
        }
        XCTAssertEqual(
            CueStateStyle.brokenDetail(reason: .deviceLost, problems: [], groupWarning: true, label: \.raw),
            loc("broken.device_lost"))
        XCTAssertEqual(
            CueStateStyle.brokenDetail(reason: nil, problems: [], groupWarning: true, label: \.raw),
            loc("cue.broken.group"))
        XCTAssertNil(CueStateStyle.brokenDetail(reason: nil, problems: [], groupWarning: false, label: \.raw))
    }

    func testStateCellsCarryIconLabelAndBrokenTooltip() async throws {
        let h = try await H()
        h.spy.expandItem(try XCTUnwrap(h.vc.node(H.group)))
        func icon(_ id: ModelID) throws -> NSImageView { try XCTUnwrap(h.cell("status", id)).images[1] }
        XCTAssertEqual(try icon(H.intro).accessibilityLabel(), loc("cue.state.running"))
        XCTAssertFalse(try icon(H.intro).isHidden)
        XCTAssertEqual(try icon(H.door).accessibilityLabel(), loc("cue.state.loaded"))
        XCTAssertEqual(try icon(H.rain).accessibilityLabel(), loc("cue.state.paused"))
        XCTAssertEqual(try icon(H.radio).accessibilityLabel(), loc("cue.state.broken"))
        XCTAssertEqual(try icon(H.radio).toolTip, "\(loc("cue.state.broken")): \(loc("broken.device_lost"))")
        // Static problems break a cue even without a state entry (the fixture's fade cue).
        XCTAssertEqual(try icon(H.fade).accessibilityLabel(), loc("cue.state.broken"))
        // The tooltip reads as the Warnings tab does: fields filled in, never a raw `{target}`.
        let tip = try XCTUnwrap(try icon(H.fade).toolTip)
        XCTAssertFalse(tip.contains("{"), tip)
        XCTAssertTrue(tip.contains("00000000-0000-0000-0000-000000000063"), tip)
        // A problem type this build has no words for still shows its type, not nothing.
        let unknown = try ServerMessage.decode(
            #"{"type":"runtime","seq":1,"changes":[{"type":"problems","cue":"\#(H.fade)","problems":[{"type":"brand_new"}]}]}"#
        )
        guard case .runtime(_, let changes) = unknown, case .problems(_, let list) = changes.first else {
            return XCTFail("\(unknown)")
        }
        let raw = try XCTUnwrap(
            CueStateStyle.brokenDetail(reason: nil, problems: list, groupWarning: false, label: \.raw))
        XCTAssertTrue(raw.contains("brand_new") && !raw.contains("{"), raw)
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.group)).images[1].isHidden, "idle shows no icon")
    }

    func testPreviewingRowHasItsOwnLabelledMark() async throws {
        let h = try await H()
        func mark() throws -> NSImageView { try XCTUnwrap(h.cell("status", H.intro)).images[2] }
        XCTAssertTrue(try mark().isHidden)
        try h.runtime(#"{"type":"preview","cue":"\#(H.intro)","active":true}"#)
        XCTAssertFalse(try mark().isHidden)
        XCTAssertEqual(try mark().accessibilityLabel(), loc("cue.previewing"))
        XCTAssertEqual(
            try XCTUnwrap(h.cell("status", H.intro)).images[1].accessibilityLabel(), loc("cue.state.running"),
            "the show state is still shown beside it")
        try h.runtime(#"{"type":"preview","cue":"\#(H.intro)","active":false}"#)
        XCTAssertTrue(try mark().isHidden)
    }

    func testRuntimeStateChangeReloadsOnlyThatRow() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.group)","state":"running"}"#)
        XCTAssertEqual(h.spy.rowsReloaded, 1)
        XCTAssertEqual(h.spy.wholeReloads, 0)
        XCTAssertEqual(h.spy.rowReloads.first?.rows, IndexSet(integer: h.spy.row(forItem: h.vc.node(H.group))))
        XCTAssertEqual(
            try XCTUnwrap(h.cell("status", H.group)).images[1].accessibilityLabel(), loc("cue.state.running"))
        // A cue the list does not hold (another list's) costs nothing.
        h.spy.resetCounts()
        try h.runtime(#"{"type":"cue_state","cue":"nope","state":"running"}"#)
        XCTAssertEqual(h.spy.rowReloads.count, 0)
    }

    func testRuntimeChangeOnACollapsedChildReloadsNothingVisible() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.rain)","state":"running"}"#)
        XCTAssertEqual(h.spy.rowsReloaded, 0)
        h.spy.expandItem(try XCTUnwrap(h.vc.node(H.group)))
        XCTAssertEqual(try XCTUnwrap(h.cell("status", H.rain)).images[1].accessibilityLabel(), loc("cue.state.running"))
    }

    // MARK: Playhead

    func testPlayheadRowIsMarkedAndMovesWithoutAReload() async throws {
        let h = try await H()
        XCTAssertTrue(h.vc.isPlayheadRow(H.fade))
        let row = try XCTUnwrap(h.spy.rowView(atRow: 1, makeIfNecessary: true) as? CueRowView)
        XCTAssertTrue(row.isPlayhead)
        XCTAssertEqual(try XCTUnwrap(h.cell("status", H.fade)).images[0].accessibilityLabel(), loc("cue.playhead"))
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":"\#(H.intro)"}"#)
        XCTAssertTrue(h.vc.isPlayheadRow(H.intro))
        XCTAssertFalse(row.isPlayhead)
        XCTAssertEqual(h.spy.rowsReloaded, 2)
        XCTAssertEqual(h.spy.wholeReloads, 0)
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.fade)).images[0].isHidden)
    }

    func testPlayheadInsideACollapsedGroupMarksTheGroup() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":"\#(H.rain)"}"#)
        XCTAssertTrue(h.vc.isPlayheadInside(H.group))
        XCTAssertFalse(h.vc.isPlayheadRow(H.group))
        let mark = try XCTUnwrap(h.cell("status", H.group)).images[0]
        XCTAssertFalse(mark.isHidden)
        XCTAssertEqual(mark.accessibilityLabel(), loc("cue.playhead.inside"))
        // The child itself is the playhead row once the group opens.
        h.spy.expandItem(try XCTUnwrap(h.vc.node(H.group)))
        XCTAssertEqual(try XCTUnwrap(h.cell("status", H.rain)).images[0].accessibilityLabel(), loc("cue.playhead"))
        // Leaving the group clears the group's mark.
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":"\#(H.intro)"}"#)
        XCTAssertFalse(h.vc.isPlayheadInside(H.group))
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.group)).images[0].isHidden)
    }

    func testPlayheadPastTheEndMarksNothing() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":null}"#)
        XCTAssertFalse(h.vc.isPlayheadRow(H.fade))
        for id in [H.intro, H.fade, H.group] { XCTAssertFalse(h.vc.isPlayheadInside(id)) }
    }

    private func sentPlayheads(_ h: H) -> [ModelID?] {
        h.rig.wire.sent.compactMap {
            if case .transport(_, .setPlayhead(_, let cue)) = $0 { return cue }
            return nil
        }
    }

    /// The demo show on the real engine, the list's wire recorded from here on.
    private func live() async throws -> H {
        let h = try await H(rig: try await Rig.open())
        h.rig.wire.forgetSent()
        return h
    }

    /// Another client of the same workspace (an OSC peer, another Mac) moves the list's playhead.
    /// The list has heard it on return (unless its results are held: then it is on its way).
    private func moveElsewhere(_ h: H, to cue: ModelID?) async throws {
        let other = Rig(engine: h.rig.testEngine, workspace: h.rig.doc)
        guard await other.connect(),
            case .success = await other.store.transport(.setPlayhead(list: ModelID(H.list), cue: cue))
        else { return XCTFail("set_playhead elsewhere") }
        other.client.stop()
        if !h.rig.wire.holdResults { await h.rig.sync() }
    }

    private func playhead(_ h: H) -> ModelID? { h.store.playheads[ModelID(H.list)] }

    /// #496: GO on a group that was the last cue moved the playhead past the end of the list while
    /// the group's row stayed highlighted and in the inspector, so a mode change there and Space
    /// fired nothing, and clicking the highlighted row changed nothing. The selection now follows
    /// the playhead: past the end nothing is selected (the inspector shows no cue), and the click
    /// that selects the group again puts the playhead back on it.
    func testAPlayheadPastTheEndClearsTheSelectionAndTheNextClickSetsItAgain() async throws {
        let h = try await live()
        var shown: [[ModelID]] = []
        h.vc.onSelectionChange = { shown.append($0) }
        h.vc.selectAsUser([H.group])
        await assertEventually { self.playhead(h) == H.group }
        try await moveElsewhere(h, to: nil)
        await assertEventually { self.playhead(h) == nil }
        XCTAssertEqual(h.vc.selectedIDs(), [])
        XCTAssertEqual(shown.last, [], "the inspector shows no cue")
        h.vc.selectAsUser([H.group])
        await assertEventually { self.playhead(h) == H.group }
        XCTAssertEqual(sentPlayheads(h), [H.group, H.group])
    }

    /// The engine moving the playhead (GO, a Go To cue, OSC) moves the selection to the cue standing
    /// by, opening a collapsed group to show it; the inspector follows and nothing is sent back.
    func testTheSelectionFollowsAPlayheadTheEngineMoves() async throws {
        let h = try await live()
        var shown: [[ModelID]] = []
        h.vc.onSelectionChange = { shown.append($0) }
        try await moveElsewhere(h, to: H.rain)
        await assertEventually { h.vc.selectedIDs() == [H.rain] }
        XCTAssertTrue(h.spy.isItemExpanded(h.vc.node(H.group)))
        XCTAssertEqual(shown.last, [H.rain])
        try await moveElsewhere(h, to: H.intro)
        await assertEventually { h.vc.selectedIDs() == [H.intro] }
        await settle()
        XCTAssertEqual(sentPlayheads(h), [], "a followed selection sends no set_playhead")
    }

    /// A group inside a group, both closed: the playhead moving to the cue inside (a GO during a
    /// show) opens the outer group first, then the inner one, so the cue is selected and in view;
    /// a reveal of it (a jump to a target) does the same without moving the playhead. Expanding
    /// the inner group first leaves it closed and the selection empty.
    func testAPlayheadInsideTwoClosedGroupsOpensBothAndSelectsTheCue() async throws {
        let h = try await live()
        func cue(_ id: String, _ kind: JSONValue) -> JSONValue {
            var c = TestSnapshots.cueJSON(id: id, number: nil, name: id, kind: kind)
            c.set("triggers", .object(["hotkey": .null, "osc": .null]))
            c.set("second_trigger", .string("nothing"))
            c.set("second_trigger_on_release", .bool(false))
            return c
        }
        let (a, b, c): (ModelID, ModelID, ModelID) = (
            "00000000-0000-0000-0000-0000000000e1", "00000000-0000-0000-0000-0000000000e2",
            "00000000-0000-0000-0000-0000000000e3"
        )
        let inner = cue(b.raw, TestSnapshots.groupKind([cue(c.raw, TestSnapshots.audioKind("media/a.wav"))]))
        guard
            case .success = await h.store.send(
                command: .addCue(
                    cue(a.raw, TestSnapshots.groupKind([inner])), list: ModelID(H.list), parent: nil, index: 0))
        else { return XCTFail("add") }
        await assertEventually { h.vc.node(c) != nil }
        h.spy.collapseItem(h.vc.node(b))
        h.spy.collapseItem(h.vc.node(a))
        var shown: [[ModelID]] = []
        h.vc.onSelectionChange = { shown.append($0) }

        func assertShown(_ line: UInt = #line) {
            XCTAssertEqual(h.vc.selectedIDs(), [c], line: line)
            XCTAssertTrue(h.spy.isItemExpanded(h.vc.node(a)), line: line)
            XCTAssertTrue(h.spy.isItemExpanded(h.vc.node(b)), line: line)
            let row = h.spy.row(forItem: h.vc.node(c))
            XCTAssertTrue(row >= 0 && h.spy.rows(in: h.spy.visibleRect).contains(row), line: line)
        }
        try await moveElsewhere(h, to: c)
        await assertEventually { h.vc.selectedIDs() == [c] }
        assertShown()
        XCTAssertEqual(shown.last, [c])
        await settle()
        XCTAssertEqual(sentPlayheads(h), [], "a followed selection sends no set_playhead")
        XCTAssertEqual(playhead(h), c)

        // The reveal path (a jump to a target) with both closed again; the playhead stays.
        try await moveElsewhere(h, to: H.intro)
        await assertEventually { h.vc.selectedIDs() == [H.intro] }
        h.spy.collapseItem(h.vc.node(b))
        h.spy.collapseItem(h.vc.node(a))
        h.vc.reveal(c)
        assertShown()
        await settle()
        XCTAssertEqual(playhead(h), H.intro)
        XCTAssertEqual(sentPlayheads(h), [])
    }

    /// Only this list's own playhead moves the selection, and only when it changes: a group the
    /// operator closed stays closed when another list's playhead moves.
    func testAnotherListsPlayheadLeavesAClosedGroupClosed() async throws {
        let h = try await live()
        try await moveElsewhere(h, to: H.rain)
        await assertEventually { h.vc.selectedIDs() == [H.rain] }
        let group = try XCTUnwrap(h.vc.node(H.group))
        h.spy.collapseItem(group)
        let other: ModelID = "00000000-0000-0000-0000-0000000000c1"
        let cue: ModelID = "00000000-0000-0000-0000-0000000000c2"
        var memo = try XCTUnwrap(NewCueKind.memo.cueJSON(id: cue, patch: nil))
        memo.set("name", .string("Elsewhere"))
        guard
            let add = Command.transaction([
                .addCueList(id: other, name: "Other", index: 1), .addCue(memo, list: other, parent: nil, index: 0),
            ]),
            case .success = await h.store.send(command: add),
            case .success = await h.store.transport(.setPlayhead(list: other, cue: cue)),
            case .success = await h.store.transport(.setPlayhead(list: other, cue: nil))
        else { return XCTFail("other list") }
        await h.rig.sync()
        XCTAssertFalse(h.spy.isItemExpanded(group), "still closed")
        XCTAssertEqual(h.vc.selectedIDs(), [], "the closed group hides the selected cue")
    }

    /// Several cues selected by hand hold the playhead's cue: its echo leaves them selected.
    func testTheEchoOfAPlayheadInsideASelectionKeepsTheSelection() async throws {
        let h = try await live()
        h.vc.selectAsUser([H.intro, H.fade])
        await assertEventually { self.playhead(h) == H.fade }
        await settle()
        XCTAssertEqual(h.vc.selectedIDs(), [H.intro, H.fade])
    }

    /// Two quick clicks: the echo of the first, arriving while the second is unanswered, must not
    /// pull the selection back; once all are answered the selection stays on the last click.
    func testAStaleEchoDoesNotPullTheSelectionBack() async throws {
        let h = try await live()
        h.rig.wire.holdResults = true
        h.vc.selectAsUser([H.fade])
        h.vc.selectAsUser([H.group])
        await assertEventually("both echoes are in, neither answer") { self.playhead(h) == H.group }
        XCTAssertEqual(h.vc.selectedIDs(), [H.group])
        XCTAssertEqual(sentPlayheads(h), [H.fade, H.group])
        h.rig.wire.holdResults = false
        h.rig.wire.releaseResults()
        await h.rig.sync()
        await settle()
        XCTAssertEqual(h.vc.selectedIDs(), [H.group])
    }

    /// A click and a GO straight after it: the GO's playhead move arrives before the click's answer
    /// and is followed once the click is answered.
    func testAPlayheadMovedOnBeforeTheClickIsAnsweredIsFollowed() async throws {
        let h = try await live()
        h.rig.wire.holdResults = true
        h.vc.selectAsUser([H.fade])
        await assertEventually { self.playhead(h) == H.fade }
        try await moveElsewhere(h, to: H.group)
        await assertEventually { self.playhead(h) == H.group }
        XCTAssertEqual(h.vc.selectedIDs(), [H.fade], "the click is not answered yet")
        h.rig.wire.holdResults = false
        h.rig.wire.releaseResults()
        await assertEventually { h.vc.selectedIDs() == [H.group] }
    }

    /// The playhead on the group, a click on Intro and Space straight after it: Intro's echo and the
    /// GO's move back onto the group both arrive before the click is answered. The selection ends
    /// on the group, where the playhead and the standby display are.
    func testAGoBackOntoThePlayheadBeforeTheClickWasAnsweredIsFollowed() async throws {
        let h = try await live()
        guard case .success = await h.store.send(command: .removeCue(H.fade)) else { return XCTFail("remove") }
        h.vc.selectAsUser([H.group])
        await assertEventually { self.playhead(h) == H.group }
        await h.rig.sync()
        h.rig.wire.holdResults = true
        h.vc.selectAsUser([H.intro])
        let go = Task { await h.store.go(list: ModelID(H.list)) }
        try await h.rig.testEngine.advance(seconds: 0.05)
        await assertEventually("Intro ran and the group stands by") {
            h.store.cueStates[H.intro]?.state == .running && self.playhead(h) == H.group
        }
        XCTAssertEqual(h.vc.selectedIDs(), [H.intro], "the click is not answered yet")
        h.rig.wire.holdResults = false
        h.rig.wire.releaseResults()
        _ = await go.value
        await assertEventually { h.vc.selectedIDs() == [H.group] }
    }

    /// The highlighted row clicked again while the playhead is elsewhere (it was moved while text
    /// was typed) puts the playhead back on it.
    func testClickingTheHighlightedRowPutsThePlayheadBackOnIt() async throws {
        let h = try await live()
        h.vc.selectAsUser([H.intro])
        await assertEventually { self.playhead(h) == H.intro }
        await settle()
        // A playhead the selection did not follow (a hold), here set without the list hearing it.
        h.vc.sub = nil
        try await moveElsewhere(h, to: H.group)
        XCTAssertEqual(h.vc.selectedIDs(), [H.intro])
        h.vc.rowClicked(h.spy.row(forItem: h.vc.node(H.intro)))
        await assertEventually { self.playhead(h) == H.intro }
        XCTAssertEqual(sentPlayheads(h).last, H.intro)
    }

    /// A `set_playhead` the engine would not act on leaves no echo to clear what was last sent: the
    /// next click on that row must send again. Here the cue was deleted elsewhere, the list has not
    /// heard (its row is still there, as in the moment before the news arrives), and it comes back.
    func testAClickAfterARefusedPlayheadMoveStillMovesThePlayhead() async throws {
        let h = try await live()
        h.vc.sub = nil
        let other = Rig(engine: h.rig.testEngine, workspace: h.rig.doc)
        guard await other.connect(), let remove = Command.transaction([.removeCue(H.fade)]),
            case .success = await other.store.send(command: remove)
        else { return XCTFail("delete elsewhere") }
        h.vc.selectAsUser([H.fade])
        await assertEventually("sent and refused") { self.sentPlayheads(h) == [H.fade] }
        await h.rig.sync()
        await settle()
        guard case .success = await other.store.undo() else { return XCTFail("undo elsewhere") }
        other.client.stop()
        await h.rig.sync()
        XCTAssertNotEqual(playhead(h), H.fade)
        h.vc.rowClicked(h.spy.row(forItem: h.vc.node(H.fade)))
        await assertEventually("the click moved it") { self.playhead(h) == H.fade }
    }

    /// A, B, A within a round trip: the echo of A arriving before the second click must not make
    /// that click look like a repeat, or the engine ends on B with the highlight on A.
    func testClickingBackWithinARoundTripEndsOnThatCue() async throws {
        let h = try await live()
        h.vc.sub = nil
        let echoes = Counter()
        let counting = h.store.changes.sink { change in
            if case .runtime(_, let heads) = change, !heads.isEmpty { MainActor.assumeIsolated { echoes.count += 1 } }
        }
        defer { counting.cancel() }
        h.rig.wire.holdResults = true
        h.vc.selectAsUser([H.fade])
        await assertEventually { self.playhead(h) == H.fade }
        h.vc.selectAsUser([H.group])
        XCTAssertEqual(playhead(h), H.fade, "the second click's echo is not in yet")
        h.vc.handle(.runtime(cues: [], playheads: [ModelID(H.list)]))
        h.vc.selectAsUser([H.fade])
        await assertEventually("all three went out") { self.sentPlayheads(h).count == 3 }
        // The engine's order: each echo comes before its answer.
        await assertEventually("all three echoed") { echoes.count == 3 }
        h.rig.wire.holdResults = false
        h.rig.wire.releaseResults()
        await assertEventually { self.playhead(h) == H.fade }
        await h.rig.sync()
        XCTAssertEqual(playhead(h), H.fade)
        XCTAssertEqual(h.vc.selectedIDs(), [H.fade])
    }

    /// A playhead inside a collapsed group has no row: Move Playhead Next / Previous steps from the
    /// group's row, not from the top or bottom of the list (the next GO would fire the first cue).
    func testMovePlayheadFromInsideACollapsedGroupStepsFromTheGroup() async throws {
        let h = try await H(rig: try await Rig.open())
        let playhead = { h.store.playheads[ModelID(H.list)] }
        guard case .success = await h.store.transport(.setPlayhead(list: ModelID(H.list), cue: H.rain)) else {
            return XCTFail("set_playhead")
        }
        await assertEventually { playhead() == H.rain }
        // The selection following the playhead opened the group; the operator closes it again.
        h.spy.collapseItem(try XCTUnwrap(h.vc.node(H.group)))
        let rows = h.visibleIDs()
        let at = try XCTUnwrap(rows.firstIndex(of: H.group))
        XCTAssertFalse(rows.contains(H.rain), "collapsed")
        XCTAssertEqual(at, rows.count - 1, "the group is the last row")
        // Past the end of the list there is no row: the playhead stays where it is, never wrapping
        // to the top (the next GO would fire the first cue).
        h.vc.movePlayhead(by: 1)
        await h.rig.sync()
        XCTAssertEqual(playhead(), H.rain)
        // Up goes to the group's neighbour, not to the row above the playhead's hidden one.
        h.vc.movePlayhead(by: -1)
        await assertEventually { playhead() == rows[at - 1] }
    }

    func testMenuDialogAnswersSelectMovePlayheadOrNameTheProblem() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        let list = wc.cueList
        let target = try XCTUnwrap(list.roots.last { $0.cue.number != nil })
        let number = try XCTUnwrap(target.cue.number)
        XCTAssertNil(wc.applyCueNumber(number, movePlayhead: false))
        XCTAssertEqual(list.selectedIDs(), [target.id], "Select Cue selects it")
        XCTAssertNil(wc.applyCueNumber(number, movePlayhead: true))
        await assertEventually("the engine moved the playhead") {
            rig.store.playheads[ModelID(H.list)] == target.id
        }
        XCTAssertEqual(
            wc.applyCueNumber("no-such", movePlayhead: true), String(format: loc("cue.prompt.notFound"), "no-such"))
        XCTAssertEqual(
            wc.applyCueNumber("no-such", movePlayhead: false), String(format: loc("cue.prompt.notFound"), "no-such"))
        // Load to Time: a bad time (also a negative one: the engine's load takes 0 or more) is a notice
        // and no action.
        var loads: [TransportAction] = []
        wc.onCueAction = { loads.append($0) }
        for bad in ["", "abc", "-5", "1:75"] {
            XCTAssertEqual(wc.applyLoadTime(bad, to: target.id), loc("cue.prompt.badTime"), bad)
        }
        XCTAssertEqual(loads, [], "nothing is done for a bad time")
        XCTAssertNil(wc.applyLoadTime("1:02.5", to: target.id))
        XCTAssertEqual(loads, [.load(cue: target.id, at: 62.5)])
    }

    // MARK: Progress

    func testProgressTouchesOnlyTheRunningRowsDurationCell() async throws {
        let h = try await H()
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":2.5,"remaining":7.5}]}"#)
        XCTAssertEqual(h.spy.rowReloads.count, 1)
        let reload = try XCTUnwrap(h.spy.rowReloads.first)
        XCTAssertEqual(reload.rows, IndexSet(integer: 0))
        XCTAssertEqual(reload.columns.map { h.spy.tableColumns[$0].identifier.rawValue }, ["duration"])
        XCTAssertEqual(h.spy.wholeReloads, 0)
        // The cell counts the time gone while the cue runs.
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:02.50")
        // Next tick: the same row again; when it ends, that row once more.
        h.spy.resetCounts()
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":3.0,"remaining":null}]}"#)
        XCTAssertEqual(h.spy.rowsReloaded, 1)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:03.00")
        h.spy.resetCounts()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"idle"}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00")
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }

    /// A panicked cue's voice is still reported while it fades out, after the cue itself has ended:
    /// the cell must read the duration again, not stay on the last time counted.
    func testProgressReportedForACueThatHasEndedIsNotShown() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"running"}"#)
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":2.5,"remaining":7.5}]}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:02.50")
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"idle"}"#)
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":2.8,"remaining":7.2}]}"#)
        XCTAssertNil(h.vc.store.progress[H.intro])
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00", "the duration again")
        XCTAssertNil(h.cell("duration", H.intro)?.progress.progress, "and no bar")
    }

    /// A pre-wait or post-wait in progress counts in its own column, touching only those cells; it
    /// ends with the run.
    func testWaitsCountInTheirColumns() async throws {
        let h = try await H()
        h.spy.resetCounts()
        let waits = #"[{"cue":"\#(H.intro)","wait":"pre_wait","elapsed":0.5,"remaining":1.5}]"#
        try h.receive(#"{"type":"progress","items":[],"waits":\#(waits)}"#)
        let reload = try XCTUnwrap(h.spy.rowReloads.first)
        XCTAssertEqual(
            reload.columns.map { h.spy.tableColumns[$0].identifier.rawValue }, ["preWait", "postWait"])
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:00.50")
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"idle"}"#)
        XCTAssertNil(h.vc.store.waits[H.intro])
        XCTAssertEqual(
            h.cell("preWait", H.intro)?.label.stringValue,
            CueTime.running(h.vc.store.model?.cue(H.intro)?.raw["pre_wait"]?.doubleValue ?? 0))
    }

    // MARK: Playhead marker across model changes

    func testGroupLosesItsPlayheadMarkerWhenThePlayheadCueMovesOut() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":"\#(H.rain)"}"#)
        XCTAssertFalse(try XCTUnwrap(h.cell("status", H.group)).images[0].isHidden)
        try h.change(upserts: [try H.upsertJSON(h.cue(H.rain.raw, "Rain"), index: 0)], deletes: [H.rain])
        XCTAssertFalse(h.vc.isPlayheadInside(H.group))
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.group)).images[0].isHidden)
    }

    func testGroupGainsAPlayheadMarkerWhenThePlayheadCueMovesIn() async throws {
        let h = try await H()
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":"\#(H.intro)"}"#)
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.group)).images[0].isHidden)
        try h.change(
            upserts: [try H.upsertJSON(h.cue(H.intro.raw, "Intro"), parent: H.group, index: 0)], deletes: [H.intro])
        XCTAssertTrue(h.vc.isPlayheadInside(H.group))
        XCTAssertFalse(try XCTUnwrap(h.cell("status", H.group)).images[0].isHidden)
    }

    // MARK: Outline equals the store after any sequence of changes

    private func assertOutlineMatchesStore(_ h: CueListHarness, _ step: String) throws {
        let model = try XCTUnwrap(h.store.model?.cueLists.first)
        func walk(_ nodes: [CueNode], _ cues: [Cue], _ parent: CueNode?) {
            XCTAssertEqual(nodes.map(\.id), cues.map(\.id), step)
            for (n, c) in zip(nodes, cues) {
                XCTAssertTrue(n.parent === parent, "\(step): parent of \(n.id)")
                XCTAssertTrue(h.vc.node(n.id) === n, "\(step): index of \(n.id)")
                walk(n.children, c.children, n)
            }
        }
        walk(h.vc.roots, model.cues, nil)
        func all(_ nodes: [CueNode]) -> [ModelID] { nodes.flatMap { [$0.id] + all($0.children) } }
        XCTAssertEqual(h.vc.nodeCount, all(h.vc.roots).count, step)
        var shown: [ModelID] = []
        func flat(_ nodes: [CueNode]) {
            for n in nodes {
                shown.append(n.id)
                if n.isGroup && h.spy.isItemExpanded(n) { flat(n.children) }
            }
        }
        flat(h.vc.roots)
        XCTAssertEqual(h.visibleIDs(), shown, step)
        let fresh = CueListViewController(store: h.store)
        XCTAssertEqual(all(fresh.roots), all(h.vc.roots), "\(step): fresh reload")
    }

    func testSequencesOfChangesLeaveTheOutlineEqualToTheStore() async throws {
        let h = try await H(collapsed: false)
        func id(_ n: Int) -> String { String(format: "00000000-0000-0000-0000-0000000001%02d", n) }
        func group(_ n: Int, _ kids: [JSONValue] = []) -> JSONValue {
            TestSnapshots.cueJSON(id: id(n), number: nil, name: "G\(n)", kind: TestSnapshots.groupKind(kids))
        }
        func index(of cue: String) -> Int {
            h.store.model?.cueLists.first?.cues.firstIndex { $0.id.raw == cue } ?? 0
        }
        func up(_ cue: JSONValue, _ parent: String? = nil, _ at: Int) throws -> String {
            try H.upsertJSON(cue, parent: parent.map { ModelID($0) }, index: at)
        }
        try assertOutlineMatchesStore(h, "start")
        // 1. out of a group to the top
        try h.change(upserts: [try up(h.cue(H.rain.raw, "Rain"), nil, 0)], deletes: [H.rain])
        try assertOutlineMatchesStore(h, "move out")
        // 2. a new empty group; 3. between groups
        try h.change(upserts: [try up(group(1), nil, 2)])
        try assertOutlineMatchesStore(h, "new group")
        try h.change(upserts: [try up(h.cue(H.door.raw, "Door"), id(1), 0)], deletes: [H.door])
        try assertOutlineMatchesStore(h, "between groups")
        // 4. nested group with a child
        try h.change(upserts: [try up(group(2, [h.cue(id(20), "K1")]), id(1), 0)])
        try assertOutlineMatchesStore(h, "nested group")
        // 5. into a collapsed parent
        h.spy.collapseItem(try XCTUnwrap(h.vc.node(ModelID(id(1)))))
        try h.change(upserts: [try up(h.cue(id(21), "K2"), id(1), 1)])
        try assertOutlineMatchesStore(h, "into collapsed parent")
        // 6. a nested group with its subtree to the top
        try h.change(upserts: [try up(group(2, [h.cue(id(20), "K1")]), nil, 0)], deletes: [ModelID(id(2))])
        try assertOutlineMatchesStore(h, "nested group out")
        // 7. several order-dependent changes in one message
        try h.change(
            upserts: [
                try up(h.cue(id(30), "X"), nil, 0), try up(h.cue(id(31), "Y"), nil, 0),
                try up(h.cue(H.intro.raw, "Intro"), nil, 3), try up(h.cue(id(32), "Z"), nil, 1),
            ], deletes: [H.fade, H.intro])
        try assertOutlineMatchesStore(h, "order dependent")
        // 8. a cue moved from one group to another and back in one message
        try h.change(
            upserts: [
                try up(h.cue(H.radio.raw, "Radio"), id(1), 0), try up(h.cue(H.radio.raw, "Radio"), id(2), 0),
            ])
        try assertOutlineMatchesStore(h, "moved twice")
        // 9. delete a group whose child is selected
        h.spy.expandItem(try XCTUnwrap(h.vc.node(ModelID(id(1)))))
        h.vc.select([ModelID(id(21)), ModelID(id(30))])
        try h.change(deletes: [ModelID(id(1))])
        try assertOutlineMatchesStore(h, "delete group")
        XCTAssertEqual(h.vc.selectedIDs(), [ModelID(id(30))])
        // 10. a group re-sent in place with fewer children
        try h.change(upserts: [try up(group(2, [h.cue(id(20), "K1")]), nil, index(of: id(2)))])
        try assertOutlineMatchesStore(h, "group resent")
        XCTAssertEqual(h.visibleIDs().filter { $0.raw == H.radio.raw }.count, 0)
    }

    func testCuesTargetingAGroupsDescendantRefreshWhenTheGroupIsResent() async throws {
        let h = try await H(collapsed: false)
        let fade = TestSnapshots.cueJSON(
            id: H.fade.raw, number: nil, name: "",
            kind: .object(["type": .string("fade"), "target": .string(H.rain.raw), "duration": .double(5)]))
        try h.change(upserts: [try H.upsertJSON(fade, index: 1)])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "2.2")
        let kids = ["Door slam", "Storm", "Radio"].enumerated().map { i, n in
            h.cue([H.door, H.rain, H.radio][i].raw, n)
        }
        let group = TestSnapshots.cueJSON(
            id: H.group.raw, number: "2", name: "Act One", kind: TestSnapshots.groupKind(kids))
        try h.change(upserts: [try H.upsertJSON(group, index: 2)])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "Storm")
        try h.change(deletes: [H.group])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, loc("cue.target.missing"))
    }
}

// MARK: Row context menu, time header toggles, warning marker

extension CueListTests {
    private func menu(_ h: H, _ id: ModelID, pasteboard: NSPasteboard? = nil) throws -> NSMenu {
        if let pasteboard { h.vc.pasteboard = pasteboard }
        let menu = NSMenu()
        h.vc.prepareRowMenu(menu, row: h.spy.row(forItem: try XCTUnwrap(h.vc.node(id))))
        return menu
    }

    private func enabled(_ menu: NSMenu) -> [CueRowMenu.Item: Bool] {
        var out: [CueRowMenu.Item: Bool] = [:]
        menu.update()  // AppKit's own pass must not change what the rules decided
        // The Edit submenu's item is not one of the rule-driven items (its tag is -1).
        for item in menu.items where !item.isSeparatorItem && CueRowMenu.Item.allCases.indices.contains(item.tag) {
            out[CueRowMenu.Item.allCases[item.tag]] = item.isEnabled
        }
        return out
    }

    private func privatePasteboard() -> NSPasteboard {
        let pb = NSPasteboard(name: NSPasteboard.Name("dev.openlab.test.\(UUID().uuidString)"))
        pb.clearContents()
        return pb
    }

    func testRowMenuHasTheSupportedItemsInOrderOnExistingActions() async throws {
        let h = try await H()
        let m = try menu(h, H.intro, pasteboard: privatePasteboard())
        XCTAssertEqual(
            m.items.map { $0.isSeparatorItem ? "-" : $0.title },
            [
                "Preview", "Load", "Panic", "-", "Move Playhead to This Cue", "Reveal Target in Finder", "-", "Cut",
                "Copy", "Paste", "Paste Properties…", "Delete", "-", "Edit",
            ])
        XCTAssertTrue(
            m.items.filter { !$0.isSeparatorItem && $0.submenu == nil }.allSatisfy { $0.target === h.vc },
            "this window's own")
        let transport = m.items.filter { $0.action == #selector(CueListViewController.rowCommand(_:)) }
        XCTAssertEqual(
            transport.compactMap { $0.representedObject as? String },
            ["previewSelected", "loadSelected", "panicSelected"])
    }

    func testRightClickSelectsAnUnselectedRowButKeepsASelectionItIsIn() async throws {
        let h = try await H()
        h.vc.select([H.intro, H.fade])
        _ = try menu(h, H.fade)
        XCTAssertEqual(Set(h.vc.selectedIDs()), [H.intro, H.fade])
        _ = try menu(h, H.group)
        XCTAssertEqual(h.vc.selectedIDs(), [H.group])
        let empty = NSMenu()
        h.vc.prepareRowMenu(empty, row: -1)
        XCTAssertTrue(empty.items.isEmpty)
        XCTAssertEqual(h.vc.selectedIDs(), [H.group])
    }

    func testRowMenuItemsFollowSelectionStateAndClipboard() async throws {
        let h = try await H()
        let pb = privatePasteboard()
        // A group: V previews it; it has no file to reveal and nothing is running to panic.
        for (id, run) in h.store.cueStates where run.state.isActive {
            try h.runtime(#"{"type":"cue_state","cue":"\#(id)","state":"idle"}"#)
        }
        var e = enabled(try menu(h, H.group, pasteboard: pb))
        XCTAssertEqual(e[.preview], true, "a group previews")
        XCTAssertEqual(enabled(try menu(h, H.fade, pasteboard: pb))[.preview], false, "a fade has nothing to preview")
        e = enabled(try menu(h, H.group, pasteboard: pb))
        XCTAssertEqual(e[.panic], false)
        XCTAssertEqual(e[.revealTarget], false)
        XCTAssertEqual(e[.paste], false)
        for item in [CueRowMenu.Item.load, .movePlayhead, .cut, .copy, .delete] {
            XCTAssertEqual(e[item], true, "\(item)")
        }
        // An audio cue with a file on disk: preview and reveal work; a running cue enables Panic.
        let id = "00000000-0000-0000-0000-0000000000c1"
        try h.change(upserts: [
            try H.upsertJSON(h.cue(id, "File", kind: TestSnapshots.audioKind("/tmp/a.wav")), index: 3)
        ])
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"running"}"#)
        pb.setString("{}", forType: CueEditor.pasteType)
        e = enabled(try menu(h, ModelID(id), pasteboard: pb))
        XCTAssertEqual(e[.preview], true)
        XCTAssertEqual(e[.revealTarget], true)
        XCTAssertEqual(e[.panic], true)
        XCTAssertEqual(e[.paste], true)
        // A cue already holding the playhead has nowhere to move it.
        try h.runtime(#"{"type":"playhead","list":"\#(H.list)","cue":"\#(id)"}"#)
        XCTAssertEqual(enabled(try menu(h, ModelID(id)))[.movePlayhead], false)
    }

    func testShowModeLocksTheEditingItemsOfTheRowMenuOnly() async throws {
        let h = try await H()
        let pb = privatePasteboard()
        pb.setString("{}", forType: CueEditor.pasteType)
        h.store.setShowMode(true)
        let e = enabled(try menu(h, H.group, pasteboard: pb))
        for item in [CueRowMenu.Item.cut, .copy, .paste, .delete] { XCTAssertEqual(e[item], false, "\(item)") }
        XCTAssertEqual(e[.load], true)
        XCTAssertEqual(e[.movePlayhead], true)
    }

    func testMediaURLIsAbsoluteOrRelativeToTheSavedShow() {
        XCTAssertEqual(CueRowMenu.mediaURL("/a/b.wav", showPath: nil)?.path, "/a/b.wav")
        XCTAssertEqual(CueRowMenu.mediaURL("media/b.wav", showPath: "/shows/x.openlab")?.path, "/shows/media/b.wav")
        XCTAssertNil(CueRowMenu.mediaURL("media/b.wav", showPath: nil))
        XCTAssertNil(CueRowMenu.mediaURL("", showPath: "/shows/x.openlab"))
        XCTAssertNil(CueRowMenu.mediaURL(nil, showPath: "/shows/x.openlab"))
    }

    func testTimeHeaderTogglesBetweenTimeGoneAndTimeLeftPerWindow() async throws {
        let h = try await H()
        let other = try await H()
        let columns = h.spy.tableColumns
        let duration = try XCTUnwrap(columns.first { $0.identifier.rawValue == "duration" })
        XCTAssertTrue(duration.headerCell is TimeHeaderCell)
        XCTAssertFalse(try XCTUnwrap(columns.first { $0.identifier.rawValue == "number" }).headerCell is TimeHeaderCell)
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":2.5,"remaining":7.5}]}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:02.50", "counting up")
        // A header click (the delegate hook) switches Duration to the time left, in place.
        h.vc.outlineView(h.spy, didClick: duration)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:07.50")
        XCTAssertEqual((duration.headerCell as? TimeHeaderCell)?.shown, .remaining)
        XCTAssertEqual(duration.headerToolTip, loc("cuelist.time.showFull"))
        // The next tick keeps the choice; with no known end there is no time left, so it counts up.
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":3.0,"remaining":null}]}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:03.00")
        h.vc.toggleTimeShown("duration")
        XCTAssertEqual(h.vc.timeShown["duration"], .full)
        // Waits count up too; the toggle shows what is left.
        let waits = #"[{"cue":"\#(H.intro)","wait":"pre_wait","elapsed":0.5,"remaining":1.5}]"#
        try h.receive(#"{"type":"progress","items":[],"waits":\#(waits)}"#)
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:00.50")
        h.vc.toggleTimeShown("preWait")
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:01.50")
        // Another window keeps its own choice; non-time columns ignore the toggle.
        XCTAssertEqual(other.vc.timeShown["preWait"], .full)
        h.vc.toggleTimeShown("name")
        XCTAssertNil(h.vc.timeShown["name"])
    }

    /// One list of locked edits: the row menu and the Edit menu both read it.
    func testShowModeLocksTheSameEditsInBothMenus() {
        let edit = NSMenu()
        for lock in MainMenu.LockedEdit.allCases {
            edit.addItem(NSMenuItem(title: "\(lock)", action: lock.selector, keyEquivalent: ""))
        }
        MainMenu.lockEditItems(in: edit, locked: true)
        XCTAssertTrue(edit.items.allSatisfy { !$0.isEnabled })
        let context = CueRowMenu.Context(
            selectedCount: 1, anyPreviewable: true, clickedIsPlayhead: false, clickedHasMedia: true,
            anyActive: true, canPaste: true, canPasteProperties: true, showMode: true)
        for item in CueRowMenu.Item.allCases {
            XCTAssertEqual(
                CueRowMenu.isEnabled(item, context), CueRowMenu.lockedEdit(item) == nil, "\(item)")
        }
    }

    func testGroupWithABrokenDescendantShowsBrokenLikeItsChild() async throws {
        let h = try await H()
        try h.runtime(
            #"{"type":"cue_state","cue":"\#(H.group)","state":"broken"},{"type":"group_warning","group":"\#(H.group)","warning":true}"#
        )
        XCTAssertEqual(
            try XCTUnwrap(h.cell("status", H.group)).images[1].accessibilityLabel(), loc("cue.state.broken"))
    }

    /// With two workspaces open, the row menu acts on the window that was right-clicked, never the
    /// key window's selection.
    func testRowMenuActsOnTheClickedWindowNotAnotherOne() async throws {
        let a = try await H()
        let b = try await H()
        let editorA = CueEditor(store: a.store, cueList: a.vc)
        let editorB = CueEditor(store: b.store, cueList: b.vc)
        a.vc.editor = editorA
        b.vc.editor = editorB
        let pbA = privatePasteboard()
        let pbB = privatePasteboard()
        editorA.pasteboard = pbA
        editorB.pasteboard = pbB
        var commandsA: [TransportCommand] = []
        var commandsB: [TransportCommand] = []
        a.vc.onCommand = { command, _ in commandsA.append(command) }
        b.vc.onCommand = { command, _ in commandsB.append(command) }
        a.vc.select([H.intro])  // A holds a selection, as the key window
        let m = try menu(b, H.fade, pasteboard: privatePasteboard())
        func fire(_ item: CueRowMenu.Item) throws {
            let entry = try XCTUnwrap(m.items.first { !$0.isSeparatorItem && CueRowMenu.Item.allCases[$0.tag] == item })
            let target = try XCTUnwrap(entry.target as? CueListViewController)
            XCTAssertTrue(target === b.vc)
            _ = target.perform(try XCTUnwrap(entry.action), with: entry)
        }
        try fire(.load)
        XCTAssertEqual(commandsB, [.loadSelected])
        XCTAssertEqual(commandsA, [])
        try fire(.copy)
        await assertEventually { pbB.string(forType: CueEditor.pasteType) != nil }
        XCTAssertNil(pbA.string(forType: CueEditor.pasteType))
        XCTAssertTrue(try XCTUnwrap(pbB.string(forType: CueEditor.pasteType)).contains(H.fade.raw))
    }

    func testLeftAndRightArrowsCollapseAndExpandTheSelectedGroup() async throws {
        let h = try await H(collapsed: false)
        let group = try XCTUnwrap(h.vc.node(H.group))
        h.vc.select([H.group])
        func press(_ key: Int) {
            let chars = String(UnicodeScalar(key)!)
            let event = NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
                characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: 0)
            if let event { h.spy.keyDown(with: event) }
        }
        press(Int(NSLeftArrowFunctionKey))
        XCTAssertFalse(h.spy.isItemExpanded(group))
        press(Int(NSRightArrowFunctionKey))
        XCTAssertTrue(h.spy.isItemExpanded(group))
    }
}

/// The acceptance flows of the playhead and selection (#489) and of GO on a group after a mode
/// change (#496), done as the operator does them, through the app's own window, keys and inspector
/// on an engine of the test's own (stepped virtual device), checked by what the window shows: the
/// highlighted row, the playhead marker and running icons in the status column, and the standby
/// display.
@MainActor
final class PlayheadFlowTests: XCTestCase {
    var app: AppController!
    var engine: TestEngine!
    let wire = WireTap()
    private let sessionScheduler = ManualScheduler()
    var doc: ModelID = "none"
    var wc: WorkspaceWindowController { app.windows[doc]! }
    var store: WorkspaceStore { wc.store }
    var list: ModelID { wc.cueList.listID! }

    override func setUp() async throws {
        engine = try await TestEngine.start()
        NotesKeeper.folder = engine.folder.appendingPathComponent("kept-notes", isDirectory: true)
        let url = engine.infoURL
        let session = EngineSession(
            transport: wire, infoProvider: { EngineConnectionInfo.load(from: url) }, scheduler: sessionScheduler)
        app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { [unowned self] in app.windows[doc]?.window }
        session.start()
        await assertEventually { session.appClient.state == .connected }
        guard case .success(let new) = await session.newWorkspace() else { return XCTFail("new") }
        doc = new
        await assertEventually {
            self.app.windows[new]?.store.model != nil && self.app.windows[new]?.cueList.listID != nil
        }
        // The operator's own typing is what the key window's first responder says.
        app.isTextEditing = { [unowned self] in (wc.window?.firstResponder as? NSText)?.isEditable == true }
        wc.window?.setFrame(NSRect(x: 0, y: 0, width: 1400, height: 900), display: false)
    }

    override func tearDown() async throws {
        NotesNoticePanel.closeAll()
        app?.closeAllWindows()
        app = nil
        engine?.stop()
    }

    // MARK: Doing what the operator does

    func key(
        _ chars: String, code: UInt16, _ flags: NSEvent.ModifierFlags = [], windowNumber: Int = 0
    ) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: windowNumber,
                context: nil,
                characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code))
    }

    /// The GOs the window sent.
    func goes() -> Int {
        wire.sent.filter { if case .transport(_, .go) = $0 { return true } else { return false } }.count
    }

    /// The engine has answered a request sent now: replies sent before it are in. A broadcast can be
    /// overtaken by a reply (`hub::distribute`), so what it brings is waited for, not assumed.
    func sync() async { _ = await store.client.send({ .request(id: $0, .listPatchMappings) }) }

    /// `seconds` of device time.
    func play(_ seconds: Double) async throws {
        try await engine.advance(seconds: seconds)
        await sync()
    }

    /// Space, as the key monitor gets it, then a tenth of a second of the show.
    private func space() async throws {
        let before = goes()
        XCTAssertNil(app.handleKeyDown(try key(" ", code: 49)), "Space is GO")
        await assertEventually("GO was sent") { self.goes() == before + 1 }
        await sync()  // answered after the GO: the engine has run it
        try await play(0.1)
    }

    /// What a click on `id`'s row does to the selection, without a press (real presses, with the
    /// table's mouse tracking, are in `PlayheadClickUITests`).
    func click(_ id: ModelID) { wc.cueList.selectAsUser([id]) }

    /// The Mode tab's popup set to `mode`, as the operator picks it.
    private func choose(_ mode: String) async throws {
        let popup = wc.inspector.groupModePopup
        popup.selectItem(at: try XCTUnwrap(InspectorViewController.groupModes.firstIndex(of: mode)))
        XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
        await assertEventually("the engine took the mode") { self.mode(self.group) == mode }
    }

    private func mode(_ id: ModelID) -> String? { store.model?.cue(id)?.raw["kind"]?["mode"]?["type"]?.stringValue }

    // MARK: What the window shows

    /// The label of status image `index` (0: playhead marker, 1: run state) drawn in `id`'s row; nil
    /// when it shows none.
    private func status(_ id: ModelID, _ index: Int) -> String? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let row = ov.row(forItem: node)
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier("status"))
        guard row >= 0, col >= 0, let cell = ov.view(atColumn: col, row: row, makeIfNecessary: true) as? CueCell,
            !cell.images[index].isHidden
        else { return nil }
        return cell.images[index].accessibilityLabel()
    }

    private func running(_ id: ModelID) -> Bool { status(id, 1) == loc("cue.state.running") }

    private func standbyText(_ id: ModelID?) throws -> String {
        guard let id else { return loc("masthead.standby.none") }
        let cue = try XCTUnwrap(store.model?.cue(id))
        return [cue.number, store.model?.listName(cue)].compactMap { $0 }.joined(separator: " \u{00B7} ")
    }

    /// The highlighted rows, the playhead marker and the standby display are all on `id` (nil:
    /// nothing highlighted, no marker, nothing standing by).
    func assertAllOn(_ id: ModelID?, file: StaticString = #filePath, line: UInt = #line) async throws {
        await assertEventually("the highlight and the playhead", file: file, line: line) {
            self.wc.cueList.selectedIDs() == (id.map { [$0] } ?? []) && self.store.playheads[self.list] == id
        }
        let all = wc.cueList.roots.flatMap { [$0] + $0.children }.map(\.id)
        XCTAssertEqual(
            all.filter { status($0, 0) == loc("cue.playhead") }, id.map { [$0] } ?? [], file: file, line: line)
        XCTAssertEqual(wc.masthead.standbyField.stringValue, try standbyText(id), file: file, line: line)
    }

    // MARK: #489: the selection follows the playhead

    var cues: [ModelID] = []
    /// The left mouse button, as the cue list reads it: these tests drive gestures with no real mouse.
    private var mouseIsDown = true

    /// The issue's workspace: Audio cues 1, 2, 3 dropped into a new list, cue 1 clicked.
    private func threeCues() async throws {
        wc.cueList.isLeftButtonDown = { [unowned self] in mouseIsDown }
        let files = try tempFiles(["1.wav", "2.wav", "3.wav"])
        cues = await wc.editor.addFiles(files, parent: nil, index: 0)
        XCTAssertEqual(cues.count, 3)
        try await play(0)
        await playheadSettled(on: cues[2])
        click(cues[0])
        try await assertAllOn(cues[0])
    }

    /// The engine's move of the playhead onto a cue just made has arrived: a click before it would be
    /// followed away from.
    private func playheadSettled(on id: ModelID, line: UInt = #line) async {
        await assertEventually("the playhead arrived", line: line) { self.store.playheads[self.list] == id }
    }

    /// Another client of the workspace (an OSC peer's `/playhead`, another Mac) moves the playhead.
    private func moveElsewhere(to cue: ModelID?) async throws {
        let other = Rig(engine: engine, workspace: doc)
        guard await other.connect(), case .success = await other.store.transport(.setPlayhead(list: list, cue: cue))
        else { return XCTFail("set_playhead elsewhere") }
        other.client.stop()
        await sync()
    }

    func testGoMovesTheHighlight() async throws {
        try await threeCues()
        try await space()
        await assertEventually { running(cues[0]) }
        try await assertAllOn(cues[1])
    }

    func testRepeatedGoWalksTheList() async throws {
        try await threeCues()
        try await space()
        try await space()
        await assertEventually { running(cues[0]) && running(cues[1]) }
        try await assertAllOn(cues[2])
    }

    func testGoPanicSelectTheFirstCueGoFindsEscAHandler() async throws {
        try await threeCues()
        try await space()
        try await assertAllOn(cues[1])
        // Esc as the key path takes it: the key monitor, then the window's dispatch to the focused
        // table. It must find a handler (cancelOperation) rather than fall off the chain and beep.
        let tap = try CancelTap()
        defer { tap.restore() }
        let beeps = Counter()
        app.beep = { beeps.count += 1 }
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        let esc = try key("\u{1B}", code: 53)
        let passed = try XCTUnwrap(app.handleKeyDown(esc), "Esc goes on to the responder too")
        window.sendEvent(passed)
        XCTAssertEqual(tap.taken, 1, "the window found a handler for Esc: no system beep")
        XCTAssertEqual(beeps.count, 0, "the app did not beep")
        try await play(0.05)
        await assertEventually("panic stopped cue 1") {
            try? await self.play(0.5)
            return !self.running(self.cues[0]) && self.store.cueStates[self.cues[0]]?.state.isActive != true
        }
        try await assertAllOn(cues[1])
        click(cues[0])
        try await assertAllOn(cues[0])
        try await space()
        await assertEventually { running(cues[0]) }
        try await assertAllOn(cues[1])
    }

    /// Typing in the inspector holds the highlight while the playhead moves; the click on the
    /// highlighted row (which ends the typing) puts the playhead back on it.
    func testTheTableActionOnTheHighlightedRowResetsThePlayhead() async throws {
        try await threeCues()
        click(cues[1])
        try await assertAllOn(cues[1])
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.nameField))
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[1]], "held while the field has focus")
        // The click: the table takes focus, then its selection and action.
        let ov = wc.cueList.outlineView
        XCTAssertTrue(window.makeFirstResponder(ov))
        click(cues[1])
        wc.cueList.rowClicked(ov.row(forItem: wc.cueList.node(cues[1])))
        try await assertAllOn(cues[1])
        await settle()
        try await assertAllOn(cues[1])
    }

    func testAPlayheadMoveFromOutsideFollowsIntoTheTable() async throws {
        try await threeCues()
        try await moveElsewhere(to: cues[2])
        try await assertAllOn(cues[2])
        let ov = wc.cueList.outlineView
        let row = ov.row(forItem: wc.cueList.node(cues[2]))
        XCTAssertTrue(ov.visibleRect.intersects(ov.rect(ofRow: row)), "scrolled into view")
    }

    func testArrowKeysMoveBoth() async throws {
        try await threeCues()
        let ov = wc.cueList.outlineView
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(ov))
        func press(_ chars: String, _ code: UInt16) throws {
            let e = try key(chars, code: code, [.numericPad, .function])
            if let passed = app.handleKeyDown(e) { ov.keyDown(with: passed) }
        }
        try press(String(UnicodeScalar(NSDownArrowFunctionKey)!), 125)
        try await assertAllOn(cues[1])
        try press(String(UnicodeScalar(NSUpArrowFunctionKey)!), 126)
        try await assertAllOn(cues[0])
    }

    func testGoAtTheEndOfTheList() async throws {
        try await threeCues()
        click(cues[2])
        try await assertAllOn(cues[2])
        try await space()
        await assertEventually { running(cues[2]) }
        try await assertAllOn(nil)
    }

    // MARK: Selected-cue keys come from the model

    private func hk(_ key: String) -> String { AppControllerTests.hotkey(key) }

    private func isLoaded(_ id: ModelID) -> Bool { status(id, 1) == loc("cue.state.loaded") }

    /// A workspace with Load Selected moved from L to K: K loads the selected cue, L does nothing as
    /// Load Selected, and a cue with its hotkey on L starts in the app and the engine alike.
    func testRemappedLoadSelectedKeyLoadsAndTheOldKeyIsFreeForACueHotkey() async throws {
        try await threeCues()
        try await store.setHotkeys(#"{"load_selected":\#(hk("k"))}"#)
        await assertEventually { self.app.frontKeys.chord(for: .loadSelected) == KeyChord("k") }
        let l = try key("l", code: 37)
        XCTAssertTrue(app.handleKeyDown(l) === l, "L is no longer a transport key")
        await sync()
        await settle()
        XCTAssertFalse(isLoaded(cues[0]), "L did not load the selected cue")
        XCTAssertNil(app.handleKeyDown(try key("k", code: 40)), "K is Load Selected")
        await assertEventually("K loaded the selected cue") { self.isLoaded(self.cues[0]) }

        _ = await store.send(command: .setProperty(cues[1], .triggers(hotkey: KeyChord("l").hotkeyJSON, enabled: true)))
        await assertEventually { self.store.model?.cueHotkeys.contains { $0.chord == KeyChord("l") } == true }
        XCTAssertNil(app.handleKeyDown(try key("l", code: 37)), "the cue's hotkey takes L")
        await sync()
        try await play(0.1)
        await assertEventually("the engine started the cue on L") { self.running(self.cues[1]) }
        XCTAssertFalse(running(cues[0]))
    }

    /// Cues with their hotkeys on L and P, which the workspace still maps to Load and Pause/Resume
    /// Selected: the keys do the workspace's job, neither cue runs, and the log says so for both.
    func testACueHotkeyOnAStillMappedSelectedCueKeyNeverStartsTheCue() async throws {
        try await threeCues()
        for (cue, chord) in [(cues[1], "l"), (cues[2], "p")] {
            _ = await store.send(
                command: .setProperty(cue, .triggers(hotkey: KeyChord(chord).hotkeyJSON, enabled: true)))
        }
        await assertEventually { self.store.model?.cueHotkeys.count == 2 }
        XCTAssertNil(app.handleKeyDown(try key("l", code: 37)), "L is Load Selected")
        await assertEventually("L loaded the selected cue") { self.isLoaded(self.cues[0]) }
        XCTAssertFalse(running(cues[1]))
        try await space()
        await assertEventually { running(cues[0]) }
        click(cues[0])
        XCTAssertNil(app.handleKeyDown(try key("p", code: 35)), "P is Pause/Resume Selected")
        await assertEventually("P paused the selected cue") { self.status(self.cues[0], 1) == loc("cue.state.paused") }
        try await play(0.1)
        XCTAssertFalse(running(cues[1]))
        XCTAssertFalse(running(cues[2]))
        XCTAssertNil(status(cues[2], 1))
        for chord in ["L", "P"] {
            let warning = String(format: loc("hotkeys.warn.shadowed"), chord)
            XCTAssertTrue(
                store.logEntries.contains { $0.level == .warn && $0.message == warning }, "\(chord): \(warning)")
        }
    }

    /// Another client changes the mapping while this workspace is open: the next key press follows
    /// it, and the Transport menu shows the new key; no reopen.
    func testAMappingChangedByAnotherClientIsFollowedByTheNextKeyPress() async throws {
        try await threeCues()
        let other = Rig(engine: engine, workspace: doc)
        let connected = await other.connect()
        XCTAssertTrue(connected)
        addTeardownBlock { @MainActor in other.client.stop() }
        try await other.store.setHotkeys(#"{"load_selected":\#(hk("k"))}"#)
        await assertEventually { self.app.frontKeys.chord(for: .loadSelected) == KeyChord("k") }
        let menus = MainMenu.make()
        MainMenu.applyKeys(to: menus.transport, app.frontKeys)
        let item = try XCTUnwrap(menus.transport.items.first { $0.representedObject as? String == "loadSelected" })
        XCTAssertEqual(item.keyEquivalent, "k")
        XCTAssertNil(app.handleKeyDown(try key("k", code: 40)))
        await assertEventually { self.isLoaded(self.cues[0]) }

        click(cues[1])
        try await other.store.setHotkeys(#"{"load_selected":\#(hk("j"))}"#)
        await assertEventually { self.app.frontKeys.chord(for: .loadSelected) == KeyChord("j") }
        MainMenu.applyKeys(to: menus.transport, app.frontKeys)
        XCTAssertEqual(item.keyEquivalent, "j")
        let k = try key("k", code: 40)
        XCTAssertTrue(app.handleKeyDown(k) === k, "K is free again")
        XCTAssertNil(app.handleKeyDown(try key("j", code: 38)))
        await assertEventually("J loaded cue 2") { self.isLoaded(self.cues[1]) }
        XCTAssertFalse(isLoaded(cues[2]))
    }

    // MARK: In-place cue number and name (#493)

    var fade: ModelID = "none"

    /// The issue's workspace: Audio cue 1 named "intro", Audio cue 2, a Fade cue 3 fading cue 1 with its
    /// default name. Cue 2 is highlighted and the cue list has the focus, as after a click.
    func editWorkspace() async throws {
        wc.cueList.isLeftButtonDown = { false }
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        cues = await wc.editor.addFiles(try tempFiles(["a.wav", "b.wav"]), parent: nil, index: 0)
        XCTAssertEqual(cues.count, 2)
        let added = await wc.editor.add(.fade, after: [cues[1]])
        fade = try XCTUnwrap(added)
        for (id, number) in zip(cues + [fade], ["1", "2", "3"]) {
            guard case .success = await store.send(command: .setNumber(id, number)) else { return XCTFail("number") }
        }
        guard case .success = await store.send(command: .setProperty(cues[0], .name("intro"))),
            case .success = await store.send(command: .setProperty(fade, .target(cues[0])))
        else { return XCTFail("setup") }
        await sync()
        await assertEventually("the workspace is shown") {
            self.cell(self.fade, "name") == "Fade intro" && self.cell(self.cues[0], "name") == "intro"
        }
        try await focusList(on: cues[1])
    }

    func focusList(on id: ModelID) async throws {
        click(id)
        try await assertAllOn(id)
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
    }

    /// The text the cue list draws in `column` of `id`'s row.
    func cell(_ id: ModelID, _ column: String) -> String? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let row = ov.row(forItem: node)
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        guard row >= 0, col >= 0, let cell = ov.view(atColumn: col, row: row, makeIfNecessary: true) as? CueCell
        else { return nil }
        return cell.label.stringValue
    }

    /// A key as the app takes it: the key monitor, then the window's dispatch to the first responder.
    func press(_ chars: String, code: UInt16 = 0) throws {
        let window = try XCTUnwrap(wc.window)
        let event = try key(chars, code: code, windowNumber: window.windowNumber)
        if let passed = app.handleKeyDown(event) { window.sendEvent(passed) }
    }

    func type(_ text: String) throws {
        for c in text { try press(String(c), code: c == "." ? 47 : 0) }
    }

    func pressReturn() throws { try press("\r", code: 36) }

    /// The words of the sheet the window shows (a refusal or a notice).
    private func sheetTexts(_ window: NSWindow) -> [String] {
        func texts(_ view: NSView) -> [String] {
            ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap(texts)
        }
        return window.attachedSheet?.contentView.map(texts) ?? []
    }

    /// The cell being edited, with its text selected, is what the window is typing into.
    private func assertEditing(_ id: ModelID, _ column: String, text: String, line: UInt = #line) throws {
        XCTAssertEqual(wc.cueList.editingCell?.id, id, line: line)
        XCTAssertEqual(wc.cueList.editingCell?.column, column, line: line)
        let editor = try XCTUnwrap(wc.window?.firstResponder as? NSTextView, "typing in the cell", line: line)
        XCTAssertTrue(editor.delegate === wc.cueList.cellEditor, line: line)
        XCTAssertEqual(editor.string, text, line: line)
        XCTAssertEqual(
            editor.selectedRange(), NSRange(location: 0, length: (text as NSString).length), "selected", line: line)
    }

    /// `committed`: the field stays over its cell, not typed in, until the engine has answered.
    private func assertNotEditing(committed: Bool = false, line: UInt = #line) {
        XCTAssertNil(wc.cueList.editingCell, line: line)
        XCTAssertTrue(wc.window?.firstResponder === wc.cueList.outlineView, "the keys are the list's again", line: line)
        if !committed { XCTAssertNil(wc.cueList.cellEditor.superview, line: line) }
    }

    private func assertTheFieldLeaves() async {
        await assertEventually("the field has left its cell") { self.wc.cueList.cellEditor.superview == nil }
    }

    func testNKeyEditsTheNumberWithItsTextSelected() async throws {
        try await editWorkspace()
        try press("n", code: 45)
        try assertEditing(cues[1], "number", text: "2")
        XCTAssertEqual(goes(), 0)
        try await assertAllOn(cues[1])
    }

    func testQKeyEditsTheName() async throws {
        try await editWorkspace()
        try await focusList(on: cues[0])
        try press("q", code: 12)
        try assertEditing(cues[0], "name", text: "intro")
        XCTAssertEqual(goes(), 0)
    }

    /// QLab 5's pre-wait, duration and post-wait keys (E, D, W) open that cell of the selected cue.
    /// D on a cue whose duration is its file's opens nothing, and the key is still taken (no beep).
    func testEDAndWKeysOpenTheWaitAndDurationCells() async throws {
        try await editWorkspace()
        try await focusList(on: fade)
        for (chars, code, column) in [("e", UInt16(14), "preWait"), ("d", 2, "duration"), ("w", 13, "postWait")] {
            try press(chars, code: code)
            XCTAssertEqual(wc.cueList.editingCell?.id, fade, column)
            XCTAssertEqual(wc.cueList.editingCell?.column, column)
            XCTAssertTrue((wc.window?.firstResponder as? NSTextView)?.delegate === wc.cueList.cellEditor, column)
            wc.cueList.cancelCellEdit()
        }
        try await focusList(on: cues[0])
        let window = try XCTUnwrap(wc.window)
        XCTAssertNil(app.handleKeyDown(try key("d", code: 2, windowNumber: window.windowNumber)), "taken")
        XCTAssertNil(wc.cueList.editingCell, "an Audio cue's duration is not typed")
        XCTAssertEqual(goes(), 0)
    }

    /// C steps the selected cues' continue mode and F flags them: every selected cue takes the first
    /// one's next value, the list draws it, and one undo takes it back. Nothing in Show Mode.
    func testCAndFKeysStepTheContinueModeAndFlagTheSelection() async throws {
        try await editWorkspace()
        func mode(_ id: ModelID) -> String? { store.model?.cue(id)?.raw["continue_mode"]?.stringValue }
        func flagged(_ id: ModelID) -> Bool { store.model?.cue(id)?.raw["flagged"] == .bool(true) }
        try await focusList(on: cues[0])
        try press("c", code: 8)
        await assertEventually("auto-continue") { mode(self.cues[0]) == "auto_continue" }
        try press("c", code: 8)
        await assertEventually("auto-follow") { mode(self.cues[0]) == "auto_follow" }
        XCTAssertEqual(wc.cueList.flagMarker(try XCTUnwrap(wc.cueList.node(cues[0])), expanded: true), nil)
        wc.cueList.selectAsUser([cues[0], cues[1]])
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        try press("c", code: 8)
        await assertEventually("both go to the first cue's next mode") {
            mode(self.cues[0]) == "do_not_continue" && mode(self.cues[1]) == "do_not_continue"
        }
        try press("f", code: 3)
        await assertEventually("both are flagged") { flagged(self.cues[0]) && flagged(self.cues[1]) }
        XCTAssertEqual(wc.cueList.flagMarker(try XCTUnwrap(wc.cueList.node(cues[1])), expanded: true), .solid)
        await wc.editor.undo()
        await assertEventually("one undo unflags both") { !flagged(self.cues[0]) && !flagged(self.cues[1]) }
        wc.setShowMode(true)
        try press("f", code: 3)
        try press("c", code: 8)
        await sync()
        XCTAssertFalse(flagged(cues[0]), "Show Mode locks the keys")
        XCTAssertEqual(mode(cues[0]), "do_not_continue")
        wc.setShowMode(false)
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.inspector.nameField))
        try press("f", code: 3)
        await sync()
        XCTAssertFalse(flagged(cues[0]), "F typed in a field is a letter")
        XCTAssertEqual(goes(), 0)
    }

    // MARK: The cue-editing keys as a key map (TRG-07)

    private func notes(_ id: ModelID) -> String? { store.model?.cue(id)?.raw["notes"]?.stringValue }

    /// The notes key (O): the inspector comes up on its Basics tab with the caret in the selected cue's
    /// notes, whatever tab it showed and whether it was shown at all; what is typed is the cue's notes.
    func testOKeyOpensTheSelectedCuesNotesForTyping() async throws {
        try await editWorkspace()
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        try press("o", code: 31)
        XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.identifier as? String, "basics")
        XCTAssertTrue(wc.window?.firstResponder === wc.inspector.notesView, "the notes take the caret")
        try type("hi")
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        await assertEventually("the notes were saved to cue 2") { self.notes(self.cues[1]) == "hi" }
        XCTAssertEqual(goes(), 0)

        wc.setVisible(.inspector, false)
        try await focusList(on: cues[0])
        try press("o", code: 31)
        XCTAssertTrue(wc.isVisible(.inspector), "a hidden inspector is shown")
        XCTAssertTrue(wc.window?.firstResponder === wc.inspector.notesView)
        XCTAssertEqual(wc.inspector.notesView.string, "", "cue 1's notes, not cue 2's")
    }

    /// Double-clicking a Number, Name or Target cell opens it in place; Return keeps what was typed,
    /// Esc leaves the cue as it was. The double-click is the table's double action for that cell
    /// (`doubleClick(row:column:)`), not mouse presses; the keys go through the app's key handling.
    func testDoubleClickOnNumberNameOrTargetEditsInPlaceEnterCommitsEscLeavesItUnchanged() async throws {
        try await editWorkspace()
        for (id, column, typed, shown) in [
            (cues[1], "number", "2.5", "2.5"), (cues[0], "name", "entrance", "entrance"),
            (fade, "target", "2.5", "2.5"),
        ] {
            let before = cell(id, column)
            try doubleClick(id, column)
            await assertEventually("\(column) opens") { self.wc.cueList.editingCell?.column == column }
            XCTAssertEqual(wc.cueList.editingCell?.id, id)
            try type("99")
            try press("\u{1B}", code: 53)
            assertNotEditing()
            XCTAssertEqual(cell(id, column), before, "Esc leaves \(column) unchanged")

            try doubleClick(id, column)
            await assertEventually("\(column) opens again") { self.wc.cueList.editingCell?.column == column }
            try type(typed)
            try pressReturn()
            await assertEventually("Return keeps \(column)") { self.cell(id, column) == shown }
        }
        XCTAssertEqual(store.model?.cue(cues[1])?.number, "2.5")
        XCTAssertEqual(store.model?.cue(fade)?.raw["kind"]?["target"]?.stringValue, cues[1].raw)
        XCTAssertEqual(goes(), 0, "Return fired no GO")
    }

    /// What the table's double action does for a double-click on `id`'s `column` cell. The table's own
    /// mouse tracking needs the window to be key, which a test run cannot count on; the real presses
    /// are in `CueListUITests` (double-click Number, Name and Target, then Return).
    func doubleClick(_ id: ModelID, _ column: String) throws {
        let ov = wc.cueList.outlineView
        let row = ov.row(forItem: try XCTUnwrap(wc.cueList.node(id)))
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        XCTAssertTrue(row >= 0 && col >= 0, "\(column) of the row is shown")
        wc.cueList.doubleClick(row: row, column: col)
    }

    /// D on an Audio cue, whose duration is its file's: no field opens, nothing is sent, and the key is
    /// taken (it never reaches the window, which would beep).
    func testDOnACueWhoseDurationCannotBeEditedDoesNothingAndDoesNotBeep() async throws {
        try await editWorkspace()
        let beeps = Counter()
        app.beep = { beeps.count += 1 }
        let sent = wire.commands.count
        let window = try XCTUnwrap(wc.window)
        XCTAssertNil(app.handleKeyDown(try key("d", code: 2, windowNumber: window.windowNumber)), "taken")
        await sync()
        XCTAssertNil(wc.cueList.editingCell)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "the keys stay with the list")
        XCTAssertEqual(wire.commands.count, sent, "nothing was sent")
        XCTAssertEqual(beeps.count, 0)
        try await assertAllOn(cues[1])
    }

    /// Several cues selected: C and F give every one of them the value the first one moves to, in one
    /// undo step. N, Q and T open no field: a number, a name and a target belong to one cue (assumed,
    /// recorded in the QLab behaviour notes). E, D, W and O are in the tests below.
    func testSeveralCuesSelectedAnEditingKeyEditsTheValueOnAllOfThem() async throws {
        try await editWorkspace()
        func mode(_ id: ModelID) -> String? { store.model?.cue(id)?.raw["continue_mode"]?.stringValue }
        func flagged(_ id: ModelID) -> Bool { store.model?.cue(id)?.raw["flagged"] == .bool(true) }
        let all = cues + [fade]
        wc.cueList.selectAsUser(all)
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        try press("c", code: 8)
        await assertEventually("every cue auto-continues") { all.allSatisfy { mode($0) == "auto_continue" } }
        try press("f", code: 3)
        await assertEventually("every cue is flagged") { all.allSatisfy(flagged) }
        for id in all {
            XCTAssertEqual(wc.cueList.flagMarker(try XCTUnwrap(wc.cueList.node(id)), expanded: true), .solid)
        }
        await wc.editor.undo()
        await assertEventually("one undo unflags them all") { all.allSatisfy { !flagged($0) } }
        XCTAssertTrue(all.allSatisfy { mode($0) == "auto_continue" }, "and leaves the continue step")
        let beeps = Counter()
        app.beep = { beeps.count += 1 }
        let window = try XCTUnwrap(wc.window)
        for (chars, code) in [("n", UInt16(45)), ("q", 12), ("t", 17)] {
            XCTAssertNil(app.handleKeyDown(try key(chars, code: code, windowNumber: window.windowNumber)), chars)
            XCTAssertNil(wc.cueList.editingCell, chars)
            XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, chars)
        }
        XCTAssertEqual(beeps.count, 0)
        XCTAssertEqual(wc.cueList.selectedIDs().count, 3, "the selection stays")
    }

    // MARK: Editing keys with several cues selected (QLab 5.6.5 as measured)

    /// Three Audio cues, each given its own starting values, all selected with the list focused.
    private func threeAudioCuesSelected(
        modes: [String] = ["do_not_continue", "do_not_continue", "do_not_continue"],
        flags: [Bool] = [false, false, false], preWaits: [Double] = [0, 0, 0], postWaits: [Double] = [0, 0, 0],
        notes: [String] = ["", "", ""]
    ) async throws -> [ModelID] {
        try await editWorkspace()
        let three = await wc.editor.addFiles(try tempFiles(["x.wav", "y.wav", "z.wav"]), parent: nil, index: 0)
        XCTAssertEqual(three.count, 3)
        for (i, id) in three.enumerated() {
            for property: CueProperty in [
                .continueMode(modes[i]), .flagged(flags[i]), .preWait(preWaits[i]), .postWait(postWaits[i]),
                .notes(notes[i]),
            ] {
                guard case .success = await store.send(command: .setProperty(id, property)) else {
                    XCTFail("setup \(property)")
                    return three
                }
            }
        }
        await sync()
        wc.cueList.selectAsUser(three)
        await assertEventually("the inspector shows all three") { Set(self.wc.inspector.selection) == Set(three) }
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        return three
    }

    /// The continue mark the list draws for `id` (nil: none).
    private func continueMark(_ id: ModelID) -> String? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier("continue"))
        guard let cell = ov.view(atColumn: col, row: ov.row(forItem: node), makeIfNecessary: true) as? CueCell,
            let mark = cell.images.first, !mark.isHidden
        else { return nil }
        return mark.accessibilityLabel()
    }

    /// The inspector field the window is typing into.
    private func typingIn(_ field: NSTextField) -> Bool {
        (wc.window?.firstResponder as? NSTextView)?.delegate === field
    }

    /// C with three cues at do-not-continue, auto-continue and auto-follow: all three take the first
    /// one's next mode (auto-continue), the next press all auto-follow; one undo puts each back.
    func testSeveralCuesSelectedCGivesThemAllTheFirstCuesNextContinueMode() async throws {
        let three = try await threeAudioCuesSelected(modes: ["do_not_continue", "auto_continue", "auto_follow"])
        let cont = loc("cue.continue.autoContinue")
        let follow = loc("cue.continue.autoFollow")
        try press("c", code: 8)
        await assertEventually("all auto-continue") { three.allSatisfy { self.continueMark($0) == cont } }
        try press("c", code: 8)
        await assertEventually("all auto-follow") { three.allSatisfy { self.continueMark($0) == follow } }
        await wc.editor.undo()
        await assertEventually("one undo: all auto-continue again") {
            three.allSatisfy { self.continueMark($0) == cont }
        }
        await wc.editor.undo()
        await assertEventually("the next undo: each its own mode") {
            self.continueMark(three[0]) == nil && self.continueMark(three[1]) == cont
                && self.continueMark(three[2]) == follow
        }
        XCTAssertTrue(wc.window?.firstResponder === wc.cueList.outlineView, "the keys stay with the list")
        XCTAssertEqual(goes(), 0)
    }

    /// F with three cues unflagged, flagged, unflagged: all three flagged (the first one's toggle),
    /// the next press all unflagged; one undo puts the flags back as they were.
    func testSeveralCuesSelectedFGivesThemAllTheFirstCuesToggledFlag() async throws {
        let three = try await threeAudioCuesSelected(flags: [false, true, false])
        /// The flag the row draws; "none" when it draws none.
        func marker(_ id: ModelID) -> String {
            guard let node = wc.cueList.node(id) else { return "gone" }
            return wc.cueList.flagMarker(node, expanded: true).map { "\($0)" } ?? "none"
        }
        try press("f", code: 3)
        await assertEventually("all flagged") { three.map(marker) == ["solid", "solid", "solid"] }
        try press("f", code: 3)
        await assertEventually("all unflagged") { three.map(marker) == ["none", "none", "none"] }
        await wc.editor.undo()
        await assertEventually("one undo: all flagged") { three.map(marker) == ["solid", "solid", "solid"] }
        await wc.editor.undo()
        await assertEventually("the next undo: only the middle one") {
            three.map(marker) == ["none", "solid", "none"]
        }
        XCTAssertTrue(wc.window?.firstResponder === wc.cueList.outlineView)
    }

    /// E (and W) with three cues of different waits: the inspector's one wait field takes the caret,
    /// showing Mixed; 7 typed and Return gives all three rows 7 s, the keys go back to the list, and
    /// one undo puts each cue's own wait back.
    func testSeveralCuesSelectedEAndWTypeOneWaitIntoTheInspectorForAllOfThem() async throws {
        let three = try await threeAudioCuesSelected(preWaits: [0, 1, 2], postWaits: [3, 4, 5])
        wc.setVisible(.inspector, false)
        let beeps = Counter()
        app.beep = { beeps.count += 1 }
        for (chars, code, column, field, before) in [
            ("e", UInt16(14), "preWait", wc.inspector.preWaitField, ["00:00.00", "00:01.00", "00:02.00"]),
            ("w", 13, "postWait", wc.inspector.postWaitField, ["00:03.00", "00:04.00", "00:05.00"]),
        ] {
            wc.inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
            try press(chars, code: code)
            XCTAssertTrue(wc.isVisible(.inspector), "\(chars): the inspector is shown")
            XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.identifier as? String, "basics", chars)
            XCTAssertTrue(typingIn(field), "\(chars): the inspector's one field takes the caret")
            XCTAssertNil(wc.cueList.editingCell, "\(chars): no cell of the list opens")
            XCTAssertEqual(field.stringValue, "", "\(chars): the values differ")
            XCTAssertEqual(field.placeholderString, loc("inspector.mixed"), chars)
            try type("7")
            try pressReturn()
            XCTAssertTrue(wc.window?.firstResponder === wc.cueList.outlineView, "\(chars): Return gives the list the keys")
            await assertEventually("\(chars): every row shows 7 s") {
                three.allSatisfy { self.cell($0, column) == "00:07.00" }
            }
            await wc.editor.undo()
            await assertEventually("\(chars): one undo gives each its own again") {
                three.map { self.cell($0, column) } == before
            }
        }
        XCTAssertEqual(beeps.count, 0)
        XCTAssertEqual(goes(), 0, "Return fired no GO")
        XCTAssertEqual(Set(wc.cueList.selectedIDs()), Set(three), "the selection stays")
    }

    /// D with three Audio cues selected: their duration is their files', so nothing opens, nothing is
    /// sent, the inspector is not brought up, no beep, and the keys stay with the list. On several
    /// Fade cues D types their one duration in the inspector, as E types a pre-wait.
    func testSeveralCuesSelectedDOnAudioCuesDoesNothing() async throws {
        let three = try await threeAudioCuesSelected(preWaits: [0, 1, 2])
        wc.setVisible(.inspector, false)
        let beeps = Counter()
        app.beep = { beeps.count += 1 }
        let sent = wire.commands.count
        let window = try XCTUnwrap(wc.window)
        XCTAssertNil(app.handleKeyDown(try key("d", code: 2, windowNumber: window.windowNumber)), "taken")
        await sync()
        XCTAssertNil(wc.cueList.editingCell)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "the keys stay with the list")
        XCTAssertFalse(wc.isVisible(.inspector), "the inspector is not brought up")
        XCTAssertEqual(wire.commands.count, sent, "nothing was sent")
        XCTAssertEqual(beeps.count, 0)
        XCTAssertEqual(Set(wc.cueList.selectedIDs()), Set(three))

        // Two Fade cues, whose duration is typed: D puts the caret in the inspector's duration field.
        let added = await wc.editor.add(.fade, after: [fade])
        let second = try XCTUnwrap(added)
        wc.cueList.selectAsUser([fade, second])
        await assertEventually("the inspector shows both fades") { Set(self.wc.inspector.selection) == [self.fade, second] }
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        try press("d", code: 2)
        XCTAssertTrue(wc.isVisible(.inspector))
        XCTAssertTrue(typingIn(wc.inspector.durationField), "a fade's duration is typed in the inspector")
        try type("4")
        try pressReturn()
        await assertEventually("both fades show 4 s") {
            [self.fade, second].allSatisfy { self.cell($0, "duration") == "00:04.00" }
        }
    }

    /// O with three cues of different notes: the inspector's notes on the Basics tab take the caret;
    /// what is typed and ended with Return becomes every cue's notes, and one undo gives each its own.
    func testSeveralCuesSelectedOPutsTheCaretInTheInspectorNotesForAllOfThem() async throws {
        let three = try await threeAudioCuesSelected(notes: ["one", "two", ""])
        wc.setVisible(.inspector, false)
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        try press("o", code: 31)
        XCTAssertTrue(wc.isVisible(.inspector), "a hidden inspector is shown")
        XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.identifier as? String, "basics")
        XCTAssertTrue(wc.window?.firstResponder === wc.inspector.notesView, "the notes take the caret")
        XCTAssertNil(wc.cueList.editingCell)
        try type("cue")
        try pressReturn()
        XCTAssertTrue(wc.window?.firstResponder === wc.cueList.outlineView, "Return gives the list the keys")
        await assertEventually("all three hold the typed notes") { three.allSatisfy { self.notes($0) == "cue" } }
        await wc.editor.undo()
        await assertEventually("one undo gives each its own") {
            three.map { self.notes($0) ?? "" } == ["one", "two", ""]
        }
        XCTAssertEqual(goes(), 0)
    }

    /// Show Mode: none of the editing keys edits anything or opens a field, and none goes on to beep.
    func testInShowModeTheEditingKeysDoNothing() async throws {
        try await editWorkspace()
        wc.setShowMode(true)
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        let before = store.model?.cue(cues[1])?.raw
        let sent = wire.commands.count
        for (chars, code) in [
            ("n", UInt16(45)), ("q", 12), ("o", 31), ("t", 17), ("e", 14), ("d", 2), ("w", 13), ("c", 8), ("f", 3),
        ] {
            XCTAssertNil(app.handleKeyDown(try key(chars, code: code, windowNumber: window.windowNumber)), chars)
            XCTAssertNil(wc.cueList.editingCell, chars)
            XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, chars)
        }
        await sync()
        XCTAssertEqual(wire.commands.count, sent, "nothing was sent")
        XCTAssertEqual(store.model?.cue(cues[1])?.raw, before)
        XCTAssertFalse(wc.isVisible(.inspector), "O does not bring the inspector into Show Mode")
        XCTAssertEqual(goes(), 0)
    }

    /// A cue plays; its flag, continue mode and name are changed by their keys meanwhile: each edit
    /// lands, the cue keeps running, and nothing stops it.
    func testWhileACuePlaysAnEditingKeyStillEditsAndPlaybackIsNotInterrupted() async throws {
        try await editWorkspace()
        try await focusList(on: cues[0])
        try await space()
        await assertEventually("cue 1 plays") { self.running(self.cues[0]) }
        try await focusList(on: cues[0])
        try press("f", code: 3)
        await assertEventually { self.store.model?.cue(self.cues[0])?.raw["flagged"] == .bool(true) }
        try press("c", code: 8)
        await assertEventually {
            self.store.model?.cue(self.cues[0])?.raw["continue_mode"]?.stringValue == "auto_continue"
        }
        try press("q", code: 12)
        try assertEditing(cues[0], "name", text: "intro")
        try type("live")
        try pressReturn()
        await assertEventually("the row shows the new name") { self.cell(self.cues[0], "name") == "live" }
        try await play(0.1)
        XCTAssertTrue(running(cues[0]), "still playing")
        XCTAssertEqual(store.cueStates[cues[0]]?.state, .running)
        let stops = wire.sent.filter {
            if case .transport(_, let a) = $0 { return "\(a)".lowercased().contains("stop") } else { return false }
        }
        XCTAssertTrue(stops.isEmpty, "nothing was stopped: \(stops)")
    }

    /// The edit-duration key moved to K on the settings' Keys page: K opens the duration at once and D
    /// no longer does; the saved show file holds K.
    func testARemappedEditingKeyWorksAtOnceTheOldOneNoLongerAndTheMapIsSavedWithTheShow() async throws {
        try await editWorkspace()
        let sc = wc.settingsWindow
        let field = try XCTUnwrap(sc.keyFields[.editDuration])
        XCTAssertEqual(field.stringValue, "D")
        field.keyDown(with: try key("k", code: 40))
        await assertEventually("the field shows K") { field.stringValue == "K" }
        await assertEventually { self.app.frontKeys.editKey(for: KeyChord("k")) == .cell("duration") }
        try await focusList(on: fade)
        try press("k", code: 40)
        XCTAssertEqual(wc.cueList.editingCell?.column, "duration", "K edits the duration")
        wc.cueList.cancelCellEdit()
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        try press("d", code: 2)
        XCTAssertNil(wc.cueList.editingCell, "D no longer does")

        let path = FileManager.default.temporaryDirectory.appendingPathComponent("\(UUID().uuidString).openlab")
        addTeardownBlock { try? FileManager.default.removeItem(at: path) }
        guard case .success = await store.saveAs(path: path.path) else { return XCTFail("save") }
        let saved = JSONValue.parse(try String(contentsOf: path, encoding: .utf8))
        let keys = saved?["workspace"]?["settings"]?["hotkeys"]
        XCTAssertEqual(keys?["edit_duration"]?["key"]?.stringValue, "k")
        XCTAssertEqual(keys?["edit_name"]?["key"]?.stringValue, "q", "the other keys are kept")
    }

    func testReturnCommitsANumberTypedInPlace() async throws {
        try await editWorkspace()
        try press("n", code: 45)
        try type("2.5")
        XCTAssertEqual(wc.cueList.cellEditor.stringValue, "2.5", "typed into the cell")
        try pressReturn()
        assertNotEditing(committed: true)
        await assertEventually("the cell and the inspector show 2.5") {
            self.cell(self.cues[1], "number") == "2.5" && self.wc.inspector.numberField.stringValue == "2.5"
        }
        await assertTheFieldLeaves()
        try await assertAllOn(cues[1])
        XCTAssertEqual(goes(), 0, "Return does not fire GO")
    }

    func testDuplicateNumberIsRefusedAsTheInspectorDoes() async throws {
        try await editWorkspace()
        let window = try XCTUnwrap(wc.window)
        try press("n", code: 45)
        try type("1")
        try pressReturn()
        await assertEventually("the refusal is up") { window.attachedSheet != nil }
        XCTAssertTrue(sheetTexts(window).contains(loc("inspector.invalid.duplicateNumber")), "\(sheetTexts(window))")
        window.endSheet(try XCTUnwrap(window.attachedSheet))
        XCTAssertEqual(cell(cues[1], "number"), "2", "cue 2 keeps its number")
        XCTAssertEqual(store.model?.cue(cues[1])?.number, "2")
        XCTAssertEqual(store.model?.cue(cues[0])?.number, "1")
        XCTAssertNil(wc.cueList.editingCell)
    }

    func testEscCancelsTheEditWithoutABeep() async throws {
        try await editWorkspace()
        let tap = try CancelTap()
        defer { tap.restore() }
        try press("n", code: 45)
        try type("99")
        try press("\u{1B}", code: 53)
        assertNotEditing()
        // The field's editor took Esc (cancelling the edit), so it never ended up at the window, where
        // an Esc nothing handles would beep.
        XCTAssertEqual(tap.taken, 0)
        XCTAssertEqual(cell(cues[1], "number"), "2", "the old number")
        XCTAssertEqual(store.model?.cue(cues[1])?.number, "2")
        try await assertAllOn(cues[1])
    }

    func testNameEditedInPlaceFollowsIntoTheFadesDefaultName() async throws {
        try await editWorkspace()
        try await focusList(on: cues[0])
        try press("q", code: 12)
        try type("entrance")
        try pressReturn()
        await assertEventually("the name and the fade that follows it") {
            self.cell(self.cues[0], "name") == "entrance" && self.cell(self.fade, "name") == "Fade entrance"
        }
        XCTAssertEqual(store.model?.cue(fade)?.name, "", "the fade still has its default name")
    }

    func testClearingANameRestoresTheDefault() async throws {
        try await editWorkspace()
        try await focusList(on: cues[0])
        try press("q", code: 12)
        try press("\u{7F}", code: 51)  // Delete: the selected text goes
        try pressReturn()
        await assertEventually("the file's name is back, and the fade follows it") {
            self.cell(self.cues[0], "name") == "a.wav" && self.store.model?.cue(self.cues[0])?.name == ""
                && self.cell(self.fade, "name") == "Fade a.wav"
        }
    }

    func testEditingDoesNotMoveThePlayheadOrFireGo() async throws {
        try await editWorkspace()
        try await focusList(on: cues[0])
        try press("n", code: 45)
        try type("1.5")
        try pressReturn()
        await assertEventually { self.cell(self.cues[0], "number") == "1.5" }
        try press("q", code: 12)
        try type("opening")
        try pressReturn()
        await assertEventually { self.cell(self.cues[0], "name") == "opening" }
        try await assertAllOn(cues[0])
        XCTAssertEqual(wc.masthead.standbyField.stringValue, "1.5 \u{00B7} opening", "standby shows the edit, on cue 1")
        XCTAssertEqual(goes(), 0)
        for id in cues { XCTAssertFalse(running(id)) }
    }

    /// Letters, N and Q included, and Space are text in the cell: nothing fires, nothing re-opens.
    func testKeysTypedInTheCellAreTextAndFireNothing() async throws {
        try await editWorkspace()
        try press("q", code: 12)
        try press("n", code: 45)
        try press("q", code: 12)
        try press(" ", code: 49)
        XCTAssertEqual(wc.cueList.cellEditor.stringValue, "nq ")
        XCTAssertEqual(wc.cueList.editingCell?.column, "name", "N did not move the edit to the number")
        XCTAssertEqual(goes(), 0)
        try await play(0.1)
        for id in cues { XCTAssertFalse(running(id)) }
        try press("\u{1B}", code: 53)
        assertNotEditing()
    }

    func testNAndQDoNothingInShowModeOrWithoutOneHighlightedCueOrOffTheList() async throws {
        try await editWorkspace()
        let window = try XCTUnwrap(wc.window)
        wc.setShowMode(true)
        try press("n", code: 45)
        try press("q", code: 12)
        XCTAssertNil(wc.cueList.editingCell, "Show Mode locks editing")
        XCTAssertFalse(wc.cueList.beginCellEdit(cues[1], column: "name"))
        wc.setShowMode(false)
        wc.cueList.selectAsUser([cues[0], cues[1]])
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        try press("n", code: 45)
        XCTAssertNil(wc.cueList.editingCell, "two cues are highlighted")
        wc.cueList.selectAsUser([cues[1]])
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.nameField))
        try press("n", code: 45)
        XCTAssertNil(wc.cueList.editingCell, "the focus is in the inspector")
    }

    /// T (edit target): a cue target is typed as a cue number in the Target cell, checked as the inspector's
    /// field does (manual p. 120).
    func testTKeyEditsACueTargetAsACueNumber() async throws {
        try await editWorkspace()
        try await focusList(on: fade)
        try press("t", code: 17)
        try assertEditing(fade, "target", text: "1")
        XCTAssertEqual(wc.cueList.cellEditor.accessibilityLabel(), loc("cuelist.targetEditor.label"))
        XCTAssertEqual(wc.cueList.cellEditor.accessibilityIdentifier(), AX.cueTargetEditor)
        XCTAssertEqual(goes(), 0)
        // T typed in the open cell is text, and the cell is not reopened.
        try press("t", code: 17)
        XCTAssertEqual(wc.cueList.cellEditor.stringValue, "t")
        try press("\u{7F}", code: 51)
        try type("2")
        try pressReturn()
        assertNotEditing(committed: true)
        await assertEventually("the Fade targets cue 2, in the cell and the inspector") {
            self.store.model?.cue(self.fade)?.raw["kind"]?["target"]?.stringValue == self.cues[1].raw
                && self.cell(self.fade, "target") == "2" && self.wc.inspector.targetField.stringValue == "2"
        }
        await assertTheFieldLeaves()
        XCTAssertEqual(goes(), 0)
        // A number no cue has is refused as the inspector refuses it, and the target is kept.
        let window = try XCTUnwrap(wc.window)
        try press("t", code: 17)
        try type("99")
        try pressReturn()
        await assertEventually("the refusal is said in the inspector") {
            self.wc.inspector.messageLabel.stringValue == loc(InspectorEdit.Rejection.unknownTarget.messageKey)
        }
        XCTAssertNil(window.attachedSheet, "nothing modal: the window is not dimmed")
        assertNotEditing()
        XCTAssertEqual(cell(fade, "target"), "2")
        // Esc leaves it alone.
        try press("t", code: 17)
        try press("\u{1B}", code: 53)
        assertNotEditing()
    }

    /// With the inspector hidden its message line cannot be seen: the refusal is a sheet then, never silent.
    func testARefusedTypedTargetIsSeenWithTheInspectorHidden() async throws {
        try await editWorkspace()
        wc.setVisible(.inspector, false)
        try await focusList(on: fade)
        let window = try XCTUnwrap(wc.window)
        try press("t", code: 17)
        try type("99")
        try pressReturn()
        await assertEventually("the refusal is a sheet") { window.attachedSheet != nil }
        XCTAssertTrue(sheetTexts(window).contains(loc(InspectorEdit.Rejection.unknownTarget.messageKey)))
        window.endSheet(try XCTUnwrap(window.attachedSheet))
        XCTAssertEqual(cell(fade, "target"), "1", "the cell holds what the cue holds")
    }

    /// T on a file cue opens the file chooser; the chosen file replaces the cue's, and a cancelled
    /// chooser changes nothing.
    func testTKeyOnAFileCueChoosesAFile() async throws {
        try await editWorkspace()
        let wav = try tempFiles(["chosen.wav"])[0]
        var picked: URL?
        var asked = 0
        wc.pickAudioFile = {
            asked += 1
            return picked
        }
        let before = store.model?.cue(cues[1])?.raw["kind"]?["media"]?.stringValue
        try press("t", code: 17)
        XCTAssertEqual(asked, 0, "the chooser opens after the key is consumed, not inside it")
        await assertEventually { asked == 1 }
        XCTAssertNil(wc.cueList.editingCell, "a file target has no number to type")
        XCTAssertEqual(store.model?.cue(cues[1])?.raw["kind"]?["media"]?.stringValue, before, "cancelled")
        picked = wav
        try press("t", code: 17)
        await assertEventually { asked == 2 }
        await assertEventually {
            self.store.model?.cue(self.cues[1])?.raw["kind"]?["media"]?.stringValue == wav.standardizedFileURL.path
        }
        XCTAssertEqual(goes(), 0)
    }

    func testTKeyDoesNothingInShowModeOrOffTheListOrWithTwoCues() async throws {
        try await editWorkspace()
        let window = try XCTUnwrap(wc.window)
        var asked = 0
        wc.pickAudioFile = {
            asked += 1
            return nil
        }
        wc.setShowMode(true)
        try press("t", code: 17)
        XCTAssertEqual(asked, 0, "Show Mode locks the target")
        XCTAssertNil(wc.cueList.editingCell)
        XCTAssertFalse(wc.cueList.beginCellEdit(fade, column: "target"))
        wc.setShowMode(false)
        wc.cueList.selectAsUser([cues[0], cues[1]])
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        try press("t", code: 17)
        XCTAssertEqual(asked, 0, "two cues are highlighted")
        wc.cueList.selectAsUser([cues[1]])
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.nameField))
        try press("t", code: 17)
        XCTAssertEqual(asked, 0, "the focus is in the inspector")
        XCTAssertNil(wc.cueList.editingCell)
    }

    /// A cue that takes no target (a Memo) opens no editor.
    func testTKeyOnACueWithNoTargetOpensNothing() async throws {
        try await editWorkspace()
        let added = await wc.editor.add(.memo, after: [cues[1]])
        let memo = try XCTUnwrap(added)
        try await focusList(on: memo)
        try press("t", code: 17)
        XCTAssertNil(wc.cueList.editingCell)
    }

    /// The list has no type-to-select: a letter moves neither the highlight nor the playhead, the edit-target
    /// key included when it has nothing to edit (a Memo, a Network cue, anything in Show Mode), or the next GO
    /// would fire a cue the operator never chose.
    func testNoLetterKeyMovesTheSelectionOrThePlayhead() async throws {
        try await editWorkspace()
        guard case .success = await store.send(command: .setProperty(cues[0], .name("Thunder rumble"))) else {
            return XCTFail("rename")
        }
        let addedMemo = await wc.editor.add(.memo, after: [fade])
        let xylo = try XCTUnwrap(addedMemo)
        guard case .success = await store.send(command: .setProperty(xylo, .name("Xylophone"))) else {
            return XCTFail("rename")
        }
        let addedNet = await wc.editor.add(.network, after: [xylo])
        let net = try XCTUnwrap(addedNet)
        await sync()
        for id in [xylo, net] {
            try await focusList(on: id)
            try press("t", code: 17)
            try await assertAllOn(id)
            XCTAssertNil(wc.cueList.editingCell, "nothing to edit")
        }
        try await focusList(on: fade)
        try press("x", code: 7)
        try await assertAllOn(fade)
        try press("t", code: 17)
        try assertEditing(fade, "target", text: "1")
        try press("\u{1B}", code: 53)
        assertNotEditing()
        try await assertAllOn(fade)
        wc.setShowMode(true)
        for letter: (String, UInt16) in [("t", 17), ("x", 7), ("n", 45), ("q", 12)] {
            try await focusList(on: fade)
            try press(letter.0, code: letter.1)
            try await assertAllOn(fade)
            XCTAssertNil(wc.cueList.editingCell)
        }
    }

    /// A cue whose hotkey is N (a hand-edited show) while N is still the edit-number key: N edits, as
    /// every key of the map wins over a cue hotkey, and the cue does not start. Once the workspace
    /// gives the edit-number key up, N starts the cue and opens no editor.
    func testACueHotkeyOnNFiresOnlyOnceTheEditNumberKeyHasMoved() async throws {
        try await editWorkspace()
        let hotkey = try XCTUnwrap(KeyChord("n").hotkeyJSON)
        guard case .success = await store.send(command: .setProperty(cues[0], .triggers(hotkey: hotkey, enabled: true)))
        else { return XCTFail("hotkey") }
        await assertEventually { self.store.model?.cueHotkeys.isEmpty == false }
        try press("n", code: 45)
        XCTAssertEqual(wc.cueList.editingCell?.column, "number", "N is the workspace's edit-number key")
        try press("\u{1B}", code: 53)
        try await play(0.1)
        XCTAssertFalse(running(cues[0]), "the cue's hotkey yields to the key map")

        try await store.setHotkeys(#"{"edit_number":null}"#)
        await assertEventually { self.app.frontKeys.editKey(for: KeyChord("n")) == nil }
        try await focusList(on: cues[1])
        try press("n", code: 45)
        XCTAssertNil(wc.cueList.editingCell)
        await assertEventually("the hotkey's cue runs") { self.running(self.cues[0]) }
        try press("q", code: 12)
        XCTAssertEqual(wc.cueList.editingCell?.column, "name", "Q still edits")
    }

    /// The playhead moves while the cell is open: the highlight holds, the edit lands on the cue it was
    /// begun for, and the highlight follows once it ends.
    func testTheEditLandsOnItsCueWhileThePlayheadMoves() async throws {
        try await editWorkspace()
        try press("n", code: 45)
        try type("7")
        try await moveElsewhere(to: fade)
        await assertEventually { self.store.playheads[self.list] == self.fade }
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[1]], "held while the cell is open")
        XCTAssertEqual(wc.cueList.editingCell?.id, cues[1])
        try pressReturn()
        await assertEventually { self.store.model?.cue(self.cues[1])?.number == "7" }
        XCTAssertNotEqual(store.model?.cue(fade)?.number, "7")
        try await assertAllOn(fade)
    }

    func testACueDeletedDuringTheEditGivesANoticeAndDropsIt() async throws {
        try await editWorkspace()
        let window = try XCTUnwrap(wc.window)
        try press("n", code: 45)
        try type("8")
        guard case .success = await store.send(command: .removeCue(cues[1])) else { return XCTFail("remove") }
        await assertEventually("the edit was dropped, with a notice") {
            self.wc.cueList.editingCell == nil && window.attachedSheet != nil
        }
        XCTAssertTrue(sheetTexts(window).contains(loc("cuelist.edit.cueGone")), "\(sheetTexts(window))")
        window.endSheet(try XCTUnwrap(window.attachedSheet))
    }

    // MARK: Default names of cues that act on another

    /// An unnamed Fade, Stop and the rest are named after the cue they act on, on every surface: list,
    /// standby, Find, the inspector (a hint over an empty field) and the editor over the cell.
    func testAnUnnamedFadeIsNamedAfterItsTargetEverywhere() async throws {
        try await editWorkspace()
        try await focusList(on: fade)
        XCTAssertEqual(cell(fade, "name"), "Fade intro")
        XCTAssertEqual(wc.masthead.standbyField.stringValue, "3 \u{00B7} Fade intro")
        XCTAssertEqual(wc.inspector.nameField.stringValue, "", "nothing is stored")
        XCTAssertEqual(wc.inspector.nameField.placeholderString, "Fade intro")
        wc.setFindQuery("fade intro")
        XCTAssertEqual(wc.find.matches, [fade])
        try press("q", code: 12)
        XCTAssertEqual(wc.cueList.cellEditor.stringValue, "", "the editor starts empty for an unnamed cue")
        XCTAssertEqual(wc.cueList.cellEditor.placeholderString, "Fade intro")
        try pressReturn()
        await settle()
        XCTAssertEqual(store.model?.cue(fade)?.name, "", "Return on the untouched cell writes nothing back")
        // Stop Target When Done changes it.
        guard
            case .success = await store.send(
                command: .setProperty(fade, .kindFields(["stop_target_when_done": .bool(true)])))
        else { return XCTFail("kind") }
        await assertEventually { self.cell(self.fade, "name") == "Fade and stop intro" }
    }

    func testAnUnnamedStopCueIsNamedAfterItsTarget() async throws {
        try await editWorkspace()
        let added = await wc.editor.add(.stop, after: [fade])
        let stop = try XCTUnwrap(added)
        guard case .success = await store.send(command: .setProperty(stop, .target(cues[1]))) else {
            return XCTFail("target")
        }
        await assertEventually("named after cue 2, whose name is its file's") {
            self.cell(stop, "name") == "Stop b.wav"
        }
    }

    /// N or Q on a row scrolled out of sight scrolls it into view: the editor is never off screen.
    func testTheEditorOpensOnAScrolledAwayRow() async throws {
        try await editWorkspace()
        let more = await wc.editor.addFiles(try tempFiles((0..<83).map { "x\($0).wav" }), parent: nil, index: 0)
        XCTAssertEqual(more.count, 83)
        try await focusList(on: cues[1])
        let ov = wc.cueList.outlineView
        try XCTUnwrap(wc.window).setFrame(NSRect(x: 0, y: 0, width: 1400, height: 500), display: true)
        try XCTUnwrap(wc.window).contentView?.layoutSubtreeIfNeeded()
        ov.scrollRowToVisible(0)
        let row = ov.row(forItem: try XCTUnwrap(wc.cueList.node(cues[1])))
        XCTAssertFalse(ov.visibleRect.intersects(ov.rect(ofRow: row)), "the row is scrolled away")
        try press("n", code: 45)
        XCTAssertEqual(wc.cueList.editingCell?.id, cues[1])
        // The list can be one row tall on a small screen (CI: 40 pt in this window), so part of the
        // editor in view is what a row scrolled to can promise.
        XCTAssertTrue(ov.visibleRect.intersects(wc.cueList.cellEditor.frame), "the editor is on screen")
    }

    /// A letter held with fn is not the plain key (an arrow key carries fn).
    func testALetterHeldWithFnDoesNotOpenAnEditor() async throws {
        try await editWorkspace()
        let window = try XCTUnwrap(wc.window)
        let event = try key("n", code: 45, [.function], windowNumber: window.windowNumber)
        if let passed = app.handleKeyDown(event) { window.sendEvent(passed) }
        XCTAssertNil(wc.cueList.editingCell)
    }

    // MARK: Typing while the playhead moves

    /// A playhead move from outside while a name is typed in the inspector: the highlight and the
    /// inspector stay, so the typing keeps its focus and Space types instead of firing GO. Once the
    /// typing ends the name goes to its cue and the highlight follows the playhead.
    func testAPlayheadMoveWhileTypingKeepsTheFocusAndSpaceDoesNotGo() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let field = wc.inspector.nameField
        XCTAssertTrue(window.makeFirstResponder(field))
        let editor = try XCTUnwrap(field.currentEditor() as? NSTextView)
        editor.insertText("half typed", replacementRange: editor.selectedRange())
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]], "the selection stays")
        XCTAssertTrue(field.currentEditor() === editor, "still typing")
        let space = try key(" ", code: 49)
        XCTAssertTrue(app.handleKeyDown(space) === space, "Space goes to the field")
        await settle()
        try await play(0.1)
        XCTAssertEqual(goes(), 0, "no GO")
        for id in cues { XCTAssertFalse(running(id)) }
        XCTAssertEqual(editor.string, "half typed")
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]])
        // The typing ends (Return, Tab, a click away).
        window.makeFirstResponder(nil)
        try await assertAllOn(cues[2])
        await assertEventually("the name went to its own cue") {
            self.store.model?.cue(self.cues[0])?.name == "half typed"
        }
        XCTAssertNotEqual(store.model?.cue(cues[2])?.name, "half typed")
    }

    /// The same in the inspector's notes.
    func testAPlayheadMoveWhileTypingNotesKeepsTheSelection() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.inspector.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        notes.insertText("cue line", replacementRange: notes.selectedRange())
        try await moveElsewhere(to: cues[1])
        await assertEventually { self.store.playheads[self.list] == self.cues[1] }
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]])
        XCTAssertTrue(window.firstResponder === notes)
        window.makeFirstResponder(nil)
        try await assertAllOn(cues[1])
        await assertEventually { self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "cue line" }
    }

    // MARK: A gesture while the playhead moves

    /// The inspector tab, as a click on it shows it.
    private func showTab(_ id: String) { wc.inspector.tabView.selectTabViewItem(withIdentifier: id) }

    private func kind(_ id: ModelID) -> JSONValue? { store.model?.cue(id)?.raw["kind"] }

    /// Commands sent for cue `id` since the last `forgetSent`.
    private func commands(on id: ModelID) -> Int {
        wire.commands.filter { $0.json["id"]?.stringValue == id.raw }.count
    }

    /// The playhead moved off cue 1 and the highlight has stayed for the gesture.
    private func assertHeldOnFirstCue(playheadOn head: ModelID, line: UInt = #line) async {
        await assertEventually("the playhead moved", line: line) { self.store.playheads[self.list] == head }
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]], "held", line: line)
        XCTAssertEqual(wc.inspector.cueForEdit?.id, cues[0], "the inspector stays on the cue", line: line)
    }

    /// The whole of a fader drag lands on the cue it started on, and the highlight follows at the end.
    func testAFaderDragStaysOnItsCueWhileThePlayheadMoves() async throws {
        try await threeCues()
        let p = wc.inspector.audioLevels
        wire.forgetSent()
        p.beginGesture()
        p.fader.doubleValue = -6
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        p.fader.doubleValue = -12
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        p.endGesture()
        await assertEventually("the drag's cue took all of it") {
            self.kind(self.cues[0])?["levels"]?["master_db"]?.doubleValue == -12
        }
        try await assertAllOn(cues[1])
        XCTAssertEqual(commands(on: cues[1]), 0, "no edit went to the other cue")
    }

    /// A press in the level matrix can become a drag at any moment until the button is up.
    func testALevelMatrixDragStaysOnItsCueWhileThePlayheadMoves() async throws {
        try await threeCues()
        let matrix = wc.inspector.audioLevels.matrix
        let window = try XCTUnwrap(wc.window)
        let master = matrix.rect(of: .master)
        let start = NSPoint(x: master.midX, y: master.midY)
        func event(_ type: NSEvent.EventType, _ p: NSPoint) throws -> NSEvent {
            try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: matrix.convert(p, to: nil), modifierFlags: [], timestamp: 0,
                    windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        }
        wire.forgetSent()
        matrix.mouseDown(with: try event(.leftMouseDown, start))
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        // The view is flipped: down the screen is a larger y, and quieter.
        matrix.mouseDragged(with: try event(.leftMouseDragged, NSPoint(x: start.x, y: start.y + 40)))
        matrix.mouseUp(with: try event(.leftMouseUp, NSPoint(x: start.x, y: start.y + 40)))
        await assertEventually("the drag's cue took it") {
            self.kind(self.cues[0])?["levels"]?["master_db"]?.doubleValue == -10
        }
        try await assertAllOn(cues[1])
        XCTAssertEqual(commands(on: cues[1]), 0)
    }

    /// A trim is sent only when the handle is let go: it must go to the cue it was dragged on.
    func testAWaveformTrimDragStaysOnItsCueWhileThePlayheadMoves() async throws {
        try await threeCues()
        let wave = wc.inspector.timeLoops.waveform
        await assertEventually { wave.duration != nil }
        wave.frame = NSRect(x: 0, y: 0, width: 600, height: 100)
        wire.forgetSent()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0)))
        wave.drag(toX: wave.scale.x(0.3))
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        wave.endDrag()
        await assertEventually("the drag's cue took it") {
            abs((self.kind(self.cues[0])?["start_time"]?.doubleValue ?? 0) - 0.3) < 0.01
        }
        try await assertAllOn(cues[1])
        XCTAssertEqual(commands(on: cues[1]), 0)
    }

    /// What a system-provided menu item reports: AppKit refuses the fn flag on an item an app makes.
    private final class FnMenuItem: NSMenuItem {
        override var keyEquivalentModifierMask: NSEvent.ModifierFlags {
            get { .function }
            set {}
        }
    }

    /// A menu item whose key equivalent needs the fn key (macOS adds such items itself) is not the
    /// plain key: that key can still be recorded, and still fires.
    func testAMenuItemNeedingTheFnKeyDoesNotTakeThePlainKey() async throws {
        try await threeCues()
        let menu = NSMenu()
        menu.addItem(FnMenuItem(title: "fn key", action: nil, keyEquivalent: "j"))
        let chords: @MainActor () -> Set<KeyChord> = { HotkeyRules.menuChords(in: menu) }
        wc.inspector.menuChords = chords
        app.menuChords = chords
        XCTAssertTrue(wc.inspector.record(KeyChord("j")), wc.inspector.messageLabel.stringValue)
        await assertEventually("recorded") {
            self.store.model?.cue(self.cues[0])?.raw["triggers"]?["hotkey"]?["key"]?.stringValue == "j"
        }
        XCTAssertNil(app.handleKeyDown(try key("j", code: 38)), "the key goes to the cue's hotkey")
        try await play(0.1)
        await assertEventually("the cue runs") { running(cues[0]) }
    }

    /// fn-F is not F: the recorder does not take it for F, and a cue with hotkey F does not run; the
    /// key goes on to the menus. Keys that always carry the fn flag still count.
    func testAKeyHeldWithFnIsNotThePlainKey() async throws {
        try await threeCues()
        let fnJ = try key("j", code: 38, [.function])
        let recorder = wc.inspector.hotkeyField
        showTab("triggers")
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(recorder))
        wire.forgetSent()
        recorder.keyDown(with: fnJ)
        await sync()
        await settle()
        XCTAssertEqual(commands(on: cues[0]), 0, "the recorder took nothing")
        XCTAssertTrue(wc.inspector.record(KeyChord("j")), wc.inspector.messageLabel.stringValue)
        await assertEventually("recorded") {
            self.store.model?.cue(self.cues[0])?.raw["triggers"]?["hotkey"]?["key"]?.stringValue == "j"
        }
        XCTAssertTrue(app.handleKeyDown(fnJ) === fnJ, "the key passes on")
        try await play(0.1)
        await sync()
        await settle()
        XCTAssertFalse(running(cues[0]), "the cue does not run")
        XCTAssertNotNil(KeyChord(event: try downArrow), "an arrow carries the flag and is still a key")
    }

    /// The system's own items vary by macOS: an unmodified key among them would be refused as a hotkey
    /// and never fire, and this names it.
    func testTheMenuBarTakesNoUnmodifiedKey() {
        let unmodified = HotkeyRules.currentMenuChords().filter { $0.mask == 0 }.map(\.key)
        XCTAssertEqual(unmodified, [])
    }

    /// A key being recorded for a cue goes to that cue.
    func testAHotkeyBeingRecordedStaysOnItsCueWhileThePlayheadMoves() async throws {
        try await threeCues()
        let recorder = wc.inspector.hotkeyField
        showTab("triggers")
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(recorder))
        XCTAssertTrue(wc.window?.firstResponder === recorder)
        wire.forgetSent()
        try await moveElsewhere(to: cues[2])
        await assertHeldOnFirstCue(playheadOn: cues[2])
        recorder.keyDown(with: try key("j", code: 38))
        XCTAssertEqual(wc.inspector.messageLabel.stringValue, "", "the key was not refused")
        XCTAssertTrue(wc.window?.firstResponder === wc.cueList.outlineView, "the keys go back to the list")
        await assertEventually("recorded on its own cue") {
            self.store.model?.cue(self.cues[0])?.raw["triggers"]?["hotkey"]?["key"]?.stringValue == "j"
        }
        try await assertAllOn(cues[2])
        XCTAssertEqual(commands(on: cues[2]), 0)
    }

    /// Stands in for a control that tracks the mouse inside its `mouseDown` (a checkbox, a slider, a
    /// table row), as driving a real one needs a real mouse: the press goes through the window.
    private final class TrackingView: NSView {
        var whileTracking: () -> Void = {}
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
        override func mouseDown(with event: NSEvent) { whileTracking() }
    }

    /// A press on a `TrackingView`, from the run loop as a real event comes (a nested loop does not
    /// run the main queue from inside a main-queue job, and so not this test's own Tasks).
    private func pressOnTrackingView(whileTracking: @escaping () -> Void) async throws {
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        let content = try XCTUnwrap(window.contentView)
        // Away from the window's edges, which track a resize until a real mouse-up.
        let view = TrackingView(frame: NSRect(x: content.bounds.midX, y: content.bounds.midY, width: 20, height: 20))
        content.addSubview(view)
        defer { view.removeFromSuperview() }
        view.whileTracking = whileTracking
        let point = content.convert(NSPoint(x: view.frame.midX, y: view.frame.midY), to: nil)
        guard content.hitTest(point) === view else { return XCTFail("the press would land elsewhere") }
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            RunLoop.main.perform(inModes: [.common]) {
                MainActor.assumeIsolated {
                    let down = NSEvent.mouseEvent(
                        with: .leftMouseDown, location: point, modifierFlags: [], timestamp: 0,
                        windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)
                    if let down { window.sendEvent(down) } else { XCTFail("no event") }
                }
                done.resume()
            }
        }
    }

    /// Runs the main queue as a control's tracking loop does, until `done` or a long deadline.
    private func track(until done: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 20)
        while !done(), Date() < deadline {
            RunLoop.main.run(mode: .eventTracking, before: Date(timeIntervalSinceNow: 0.01))
        }
    }

    /// A checkbox acts when the button comes up: pressed on cue 1, it must not flag the cue the
    /// playhead moved to meanwhile.
    func testACheckboxHeldDownStaysOnItsCueWhileThePlayheadMoves() async throws {
        try await threeCues()
        wire.forgetSent()
        let moved = Counter()
        Task {
            try await moveElsewhere(to: cues[2])
            moved.count = 1
        }
        var heldOn: [ModelID] = []
        try await pressOnTrackingView { [self] in
            track { moved.count == 1 }
            heldOn = wc.cueList.selectedIDs()
            wc.inspector.flaggedCheck.performClick(nil)  // the mouse-up
        }
        XCTAssertEqual(moved.count, 1)
        XCTAssertEqual(heldOn, [cues[0]], "held while the button is down")
        await assertEventually("flagged its own cue") {
            self.store.model?.cue(self.cues[0])?.raw["flagged"] == .bool(true)
        }
        try await assertAllOn(cues[2])
        XCTAssertEqual(commands(on: cues[2]), 0)
    }

    /// A drag whose release never came (its view was taken away under it) must not hold for good:
    /// the next press ends it.
    func testTheNextPressEndsAGestureWhoseReleaseNeverCame() async throws {
        try await threeCues()
        wc.inspector.audioLevels.beginGesture()
        try await moveElsewhere(to: cues[2])
        await assertHeldOnFirstCue(playheadOn: cues[2])
        try await pressOnTrackingView {}
        try await assertAllOn(cues[2])
    }

    /// Opens `menu` for real (no mouse: the menu is closed from code), from the run loop as a real
    /// press does (`pressOnTrackingView`), runs `body` while it tracks, and returns when it has
    /// closed; `chosen` runs where a chosen item's action does, on the way out of the tracking. A
    /// timer closes it if `body` never finishes.
    private func whileOpen(
        _ menu: NSMenu, in view: NSView, chosen: @escaping @MainActor @Sendable () -> Void = {},
        _ body: @escaping @MainActor @Sendable () async throws -> Void
    ) async {
        nonisolated(unsafe) let menu = menu
        nonisolated(unsafe) let view = view
        nonisolated(unsafe) let finished = Counter()
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            RunLoop.main.perform(inModes: [.common]) {
                MainActor.assumeIsolated {
                    Task { @MainActor in
                        do { try await body() } catch { XCTFail("while the menu is open: \(error)") }
                        finished.count = 1
                        menu.cancelTracking()
                    }
                    let watchdog = Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 30_000_000_000)
                        if !Task.isCancelled { menu.cancelTracking() }
                    }
                    menu.popUp(positioning: nil, at: .zero, in: view)
                    watchdog.cancel()
                    chosen()
                }
                done.resume()
            }
        }
        XCTAssertEqual(finished.count, 1, "the engine's updates apply while a menu is open")
    }

    /// An inspector popup acts when its menu closes: the choice goes to the cue it was opened on.
    func testAPopupMenuStaysOnItsCueWhileThePlayheadMoves() async throws {
        try await threeCues()
        wire.forgetSent()
        let popup = wc.inspector.continuePopup
        let menu = try XCTUnwrap(popup.menu)
        let choose: @MainActor @Sendable () -> Void = {
            popup.selectItem(at: 1)
            XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
        }
        await whileOpen(menu, in: popup, chosen: choose) { [self] in
            try await moveElsewhere(to: cues[2])
            await assertHeldOnFirstCue(playheadOn: cues[2])
        }
        await assertEventually("set on its own cue") {
            self.store.model?.cue(self.cues[0])?.raw["continue_mode"]?.stringValue
                == InspectorViewController.continueModes[1]
        }
        try await assertAllOn(cues[2])
        XCTAssertEqual(commands(on: cues[2]), 0)
    }

    // MARK: The row menu acts on what it was opened on

    private func rowMenu(on id: ModelID) -> NSMenu {
        let menu = NSMenu()
        wc.cueList.prepareRowMenu(menu, row: wc.cueList.outlineView.row(forItem: wc.cueList.node(id)))
        return menu
    }

    private func item(_ item: CueRowMenu.Item, in menu: NSMenu) throws -> NSMenuItem {
        try XCTUnwrap(menu.items.first { $0.tag == CueRowMenu.Item.allCases.firstIndex(of: item) })
    }

    /// Right-click running cue 1; the playhead moves while the menu is open; Panic fades cue 1, the
    /// cue the menu was opened on, and no other; the highlight then follows the playhead.
    func testRowMenuPanicAfterThePlayheadMoves() async throws {
        try await threeCues()
        try await space()
        await assertEventually { running(cues[0]) }
        let menu = rowMenu(on: cues[0])
        await sync()
        await whileOpen(menu, in: wc.cueList.outlineView) { [self] in
            try await moveElsewhere(to: cues[2])
            await assertHeldOn(cues[0], playheadOn: cues[2])
        }
        wire.forgetSent()
        wc.cueList.rowCommand(try item(.panic, in: menu))
        await assertEventually("cue 1 fades out and stops") {
            try? await self.play(0.5)
            return !self.running(self.cues[0]) && self.store.cueStates[self.cues[0]]?.state.isActive != true
        }
        let panicked = wire.sent.compactMap { m -> ModelID? in
            if case .transport(_, .panicCue(let c)) = m { return c }
            return nil
        }
        XCTAssertEqual(panicked.first, cues[0])
        XCTAssertFalse(panicked.contains(cues[2]))
        try await assertAllOn(cues[2])
    }

    private func assertHeldOn(_ id: ModelID, playheadOn head: ModelID, line: UInt = #line) async {
        await assertEventually("the playhead moved", line: line) { self.store.playheads[self.list] == head }
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [id], "held", line: line)
    }

    /// Delete removes the cue the menu was opened on; the highlight and the playhead, which have
    /// moved elsewhere, stay where they are.
    func testRowMenuDeleteAfterThePlayheadMoves() async throws {
        try await threeCues()
        let menu = rowMenu(on: cues[0])
        try await moveElsewhere(to: cues[2])
        try await assertAllOn(cues[2])
        wc.cueList.rowDelete(try item(.delete, in: menu))
        await assertEventually { self.wc.cueList.roots.count == 2 }
        XCTAssertNil(wc.cueList.node(cues[0]), "the right-clicked cue went")
        await sync()
        await settle()
        try await assertAllOn(cues[2])
    }

    /// Load, Preview, Copy, Paste and Cut, each with the selection moved after the menu opened.
    func testRowMenuOtherCommandsAfterThePlayheadMoves() async throws {
        try await threeCues()
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("openlab-test-\(UUID().uuidString)"))
        addTeardownBlock { @MainActor in pasteboard.releaseGlobally() }
        wc.editor.pasteboard = pasteboard
        wc.cueList.pasteboard = pasteboard
        var menu = rowMenu(on: cues[0])
        try await moveElsewhere(to: cues[2])
        try await assertAllOn(cues[2])
        wc.cueList.rowCommand(try item(.load, in: menu))
        await assertEventually("Load") { self.status(self.cues[0], 1) == loc("cue.state.loaded") }
        XCTAssertNil(status(cues[2], 1))
        wc.cueList.rowCommand(try item(.preview, in: menu))
        await assertEventually("Preview") { self.store.previewing == [self.cues[0]] }
        // A second Preview on a cue being previewed does nothing (QLab check V3).
        wc.cueList.rowCommand(try item(.preview, in: menu))
        await sync()
        XCTAssertEqual(store.previewing, [cues[0]], "the preview plays on")
        _ = await store.transport(.stopPreview(cue: cues[0]))
        await assertEventually("Preview stopped") { self.store.previewing.isEmpty }
        wc.cueList.rowCopy(try item(.copy, in: menu))
        await assertEventually("Copy") { pasteboard.string(forType: CueEditor.pasteType) != nil }
        let copied = try XCTUnwrap(JSONValue.parse(try XCTUnwrap(pasteboard.string(forType: CueEditor.pasteType))))
        XCTAssertEqual(copied["cues"]?.arrayValue?.first?["id"]?.stringValue, cues[0].raw)
        wc.cueList.rowPaste(try item(.paste, in: menu))
        await assertEventually("Paste goes after it") { self.wc.cueList.roots.count == 4 }
        XCTAssertEqual(wc.cueList.roots[0].id, cues[0])
        XCTAssertEqual(wc.cueList.roots[2].id, cues[1], "the copy is between cue 1 and cue 2")
        // The list shows the copy before paste's reply; paste then selects it, which moves the playhead.
        try await assertAllOn(wc.cueList.roots[1].id)
        menu = rowMenu(on: cues[0])
        try await moveElsewhere(to: cues[2])
        try await assertAllOn(cues[2])
        wc.cueList.rowCut(try item(.cut, in: menu))
        await assertEventually("Cut") { self.wc.cueList.node(self.cues[0]) == nil }
        XCTAssertNotNil(wc.cueList.node(cues[2]))
        try await assertAllOn(cues[2])
    }

    // MARK: The keys after the selection moves

    private var downArrow: NSEvent {
        get throws { try key(String(UnicodeScalar(NSDownArrowFunctionKey)!), code: 125, [.numericPad, .function]) }
    }

    /// Focus in the level matrix stays there when GO moves to a cue that shows it too, so the arrows
    /// go on moving the matrix's selected cell and nothing beeps.
    func testFocusInTheLevelMatrixSurvivesGoAndTheArrowKeysStillAct() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        let panel = wc.inspector.audioLevels
        showTab("levels")
        XCTAssertTrue(window.makeFirstResponder(panel.matrix))
        let beeps = Counter()
        app.beep = { beeps.count += 1 }
        try await space()
        try await assertAllOn(cues[1])
        XCTAssertTrue(window.firstResponder === panel.matrix)
        let before = panel.cellLabel.stringValue
        window.sendEvent(try XCTUnwrap(app.handleKeyDown(try downArrow)))
        XCTAssertNotEqual(panel.cellLabel.stringValue, before, "the matrix took the key")
        XCTAssertEqual(beeps.count, 0)
    }

    /// When the next cue does not show the focused control, the list takes the keys: Down moves the
    /// highlight rather than driving nothing.
    func testFocusOnAControlTheNextCueHidesGoesToTheList() async throws {
        try await threeCues()
        let made = await wc.editor.add(.group)
        let group = try XCTUnwrap(made)
        await sync()
        click(cues[0])
        try await assertAllOn(cues[0])
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.audioLevels.matrix))
        XCTAssertTrue(window.firstResponder === wc.inspector.audioLevels.matrix)
        try await moveElsewhere(to: group)
        await assertEventually { self.wc.cueList.selectedIDs() == [group] }
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
        window.sendEvent(try XCTUnwrap(app.handleKeyDown(try downArrow)))
        await assertEventually("Down moved the highlight") { self.wc.cueList.selectedIDs() != [group] }
    }

    /// Hiding the sidebar while a control in it has the focus hands the keys to the list: Down moves the
    /// highlight rather than driving nothing.
    func testHidingTheSidebarWithFocusInItGivesTheKeysToTheList() async throws {
        try await threeCues()
        click(cues[0])
        try await assertAllOn(cues[0])
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        wc.setVisible(.sidebar, true)
        XCTAssertTrue(window.makeFirstResponder(wc.sidebar.listTable))
        wc.setVisible(.sidebar, false)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "the keys went to the list")
        window.sendEvent(try XCTUnwrap(app.handleKeyDown(try downArrow)))
        await assertEventually("Down moved the highlight") { self.wc.cueList.selectedIDs() != [self.cues[0]] }
    }

    /// Typing ended by a selection the app changes: the list takes the keys, not the window.
    func testASelectionThatEndsTypingLeavesTheKeysWithTheList() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.nameField))
        click(cues[1])
        try await assertAllOn(cues[1])
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
    }

    // MARK: The menu bar and the keys act on the selection at the moment they are used

    private lazy var menuBar: AppDelegate = {
        let delegate = AppDelegate()
        delegate.appController = app
        return delegate
    }()

    private func panicked() -> [ModelID] {
        wire.sent.compactMap { m -> ModelID? in
            if case .transport(_, .panicCue(let c)) = m { return c }
            return nil
        }
    }

    /// A menu is open on cue 1 while the playhead moves to `head`; `action` is what the chosen item
    /// runs, where an item's action runs (no real menu bar: opening one needs a real mouse).
    private func chooseFromOpenMenu(
        afterPlayheadMovesTo head: ModelID, _ action: @escaping @MainActor @Sendable () -> Void
    ) async throws {
        click(cues[0])
        try await assertAllOn(cues[0])
        let menu = NSMenu()
        menu.addItem(withTitle: "item", action: nil, keyEquivalent: "")
        await whileOpen(menu, in: wc.cueList.outlineView, chosen: action) { [self] in
            try await moveElsewhere(to: head)
            await assertHeldOn(cues[0], playheadOn: head)
        }
    }

    func testMainMenuDeleteAfterThePlayheadMoves() async throws {
        try await threeCues()
        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.deleteCues(nil) }
        await assertEventually { self.wc.cueList.roots.count == 2 }
        XCTAssertNil(wc.cueList.node(cues[0]), "the cue highlighted when the menu was used went")
        XCTAssertNotNil(wc.cueList.node(cues[2]))
        await sync()
        await settle()
        try await assertAllOn(cues[2])
    }

    func testMainMenuTransportAfterThePlayheadMoves() async throws {
        try await threeCues()
        func item(_ command: TransportCommand) -> NSMenuItem {
            let item = NSMenuItem(title: "", action: nil, keyEquivalent: "")
            item.representedObject = command.rawValue
            return item
        }
        nonisolated(unsafe) let load = item(.loadSelected)
        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.transportCommand(load) }
        await assertEventually("Load Selected") { self.status(self.cues[0], 1) == loc("cue.state.loaded") }
        XCTAssertNil(status(cues[2], 1))
        try await space()
        await assertEventually { running(cues[2]) }
        nonisolated(unsafe) let panic = item(.panicSelected)
        wire.forgetSent()
        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[1]) { [self] in menuBar.transportCommand(panic) }
        await assertEventually("Panic Selected") { !self.panicked().isEmpty }
        XCTAssertEqual(panicked(), [cues[0]])
    }

    func testMainMenuOtherEditsAfterThePlayheadMoves() async throws {
        try await threeCues()
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("openlab-test-\(UUID().uuidString)"))
        addTeardownBlock { @MainActor in pasteboard.releaseGlobally() }
        wc.editor.pasteboard = pasteboard
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        func index(_ id: ModelID) -> Int? { wc.cueList.roots.firstIndex { $0.id == id } }

        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.copyCues(nil) }
        await assertEventually("Copy") { pasteboard.string(forType: CueEditor.pasteType) != nil }
        let copied = try XCTUnwrap(JSONValue.parse(try XCTUnwrap(pasteboard.string(forType: CueEditor.pasteType))))
        XCTAssertEqual(copied["cues"]?.arrayValue?.first?["id"]?.stringValue, cues[0].raw)

        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.pasteCues(nil) }
        await assertEventually("Paste") { self.wc.cueList.roots.count == 4 }
        XCTAssertEqual(index(cues[1]), 2, "the copy is right after cue 1")
        await assertEventually("the copy is selected") {
            self.wc.cueList.selectedIDs() == [self.wc.cueList.roots[1].id]
        }

        let item = NSMenuItem(title: "", action: nil, keyEquivalent: "")
        item.representedObject = NewCueKind.memo.rawValue
        nonisolated(unsafe) let memo = item
        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.newCue(memo) }
        await assertEventually("New cue") { self.wc.cueList.roots.count == 5 }
        XCTAssertEqual(index(cues[0]), 0)
        XCTAssertEqual(wc.cueList.roots[1].cue.kindType, "memo", "the new cue is right after cue 1")
        await assertEventually("the new cue is selected") {
            self.wc.cueList.selectedIDs() == [self.wc.cueList.roots[1].id]
        }

        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.moveCuesDown(nil) }
        await assertEventually("Move Down") { index(self.cues[0]) == 1 }
        XCTAssertEqual(index(cues[2]), 4)

        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.deleteNumbers(nil) }
        await assertEventually("Delete Numbers") { self.store.model?.cue(self.cues[0])?.number == nil }
        XCTAssertNotNil(store.model?.cue(cues[2])?.number)

        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in menuBar.cutCues(nil) }
        await assertEventually("Cut") { self.wc.cueList.node(self.cues[0]) == nil }
        XCTAssertNotNil(wc.cueList.node(cues[2]))
    }

    /// The renumber dialog names the cues selected when it opened, whatever moves while it is up.
    func testRenumberDialogAfterThePlayheadMoves() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        wc.promptRenumber()
        await assertEventually("the dialog") { window.attachedSheet != nil }
        let sheet = try XCTUnwrap(window.attachedSheet)
        func field(_ view: NSView) -> NSTextField? {
            if let f = view as? NSTextField, f.accessibilityIdentifier() == "renumber-start" { return f }
            return view.subviews.lazy.compactMap(field).first
        }
        try XCTUnwrap(field(try XCTUnwrap(sheet.contentView))).stringValue = "10"
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        window.endSheet(sheet, returnCode: .alertFirstButtonReturn)
        await assertEventually("renumbered its own cue") { self.store.model?.cue(self.cues[0])?.number == "10" }
        XCTAssertEqual(store.model?.cue(cues[2])?.number, "3")
    }

    /// A key acts on what was highlighted when it was pressed: a playhead update already queued ahead
    /// of the key's work moves the highlight first, with the screen not yet showing it. The list is
    /// detached so that update can be put in that place.
    func testASelectedKeyActsOnTheCueHighlightedWhenItWasPressed() async throws {
        try await threeCues()
        try await space()
        click(cues[0])
        try await assertAllOn(cues[0])
        await assertEventually { running(cues[0]) }
        wc.cueList.sub = nil
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        wire.forgetSent()
        let queued = Task { @MainActor in
            self.wc.cueList.handle(.runtime(cues: [], playheads: [self.list]))
        }
        XCTAssertNil(app.handleKeyDown(try key("s", code: 1)), "S is Panic Selected")
        await queued.value
        await assertEventually("Panic Selected") { !self.panicked().isEmpty }
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[2]], "the update did come in between")
        XCTAssertEqual(panicked(), [cues[0]])
    }

    /// The list shown has its playhead's cue highlighted (the playhead and selection stay together).
    func testAnotherListShowsItsPlayheadHighlighted() async throws {
        try await threeCues()
        let other: ModelID = "00000000-0000-0000-0000-0000000000c1"
        let cue: ModelID = "00000000-0000-0000-0000-0000000000c2"
        var memo = try XCTUnwrap(NewCueKind.memo.cueJSON(id: cue, patch: nil))
        memo.set("name", .string("Elsewhere"))
        guard
            let add = Command.transaction([
                .addCueList(id: other, name: "Other", index: 1), .addCue(memo, list: other, parent: nil, index: 0),
            ]), case .success = await store.send(command: add),
            case .success = await store.transport(.setPlayhead(list: other, cue: cue))
        else { return XCTFail("other") }
        await sync()
        wc.cueList.showList(other)
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [cue], "standby and highlight agree after switching list")
        XCTAssertEqual(wc.inspector.cueForEdit?.id, cue)
        XCTAssertEqual(wc.masthead.standbyField.stringValue, try standbyText(cue))
        XCTAssertEqual(status(cue, 0), loc("cue.playhead"))
    }

    /// The masthead's notes of the standing-by cue are being typed and the playhead moves from
    /// outside: the field keeps its focus and text, and Space types a space instead of firing GO.
    func testAPlayheadMoveWhileTypingMastheadNotesKeepsTheFocusAndSpaceDoesNotGo() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.masthead.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("half", in: window)
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        XCTAssertTrue(window.firstResponder === notes, "still typing")
        XCTAssertEqual(notes.string, "half")
        XCTAssertEqual(wc.masthead.cueID, cues[0], "the masthead keeps the cue being typed for")
        try typeKeys(" g typed", in: window)  // Space and a letter: no GO, no hotkey
        await settle()
        try await play(0.1)
        XCTAssertEqual(goes(), 0, "no GO")
        for id in cues { XCTAssertFalse(running(id)) }
        XCTAssertEqual(store.playheads[list], cues[2], "standby unchanged")
        XCTAssertEqual(notes.string, "half g typed", "Space and the letter landed in the text")
        window.makeFirstResponder(nil)
        await assertEventually("the whole text went to the cue it was typed for") {
            self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "half g typed"
        }
    }

    /// Return ends the typing, as Escape does: the notes save and the masthead shows the cue now
    /// standing by.
    func testReturnInMastheadNotesEndsTypingAndShowsTheNewStandbyCue() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.masthead.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("for one", in: window)
        try await moveElsewhere(to: cues[1])
        await assertEventually { self.store.playheads[self.list] == self.cues[1] }
        XCTAssertEqual(wc.masthead.cueID, cues[0])
        try typeKeys("\r", in: window, code: 36)
        XCTAssertFalse(window.firstResponder === notes, "Return ended the typing")
        try await assertAllOn(cues[1])
        XCTAssertEqual(wc.masthead.cueID, cues[1])
        await assertEventually { self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "for one" }
    }

    /// Types `text` as key events through the app's key path and the window to its first responder.
    private func typeKeys(_ text: String, in window: NSWindow, code: UInt16 = 0) throws {
        for ch in text {
            let c = String(ch)
            let event = try XCTUnwrap(
                NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                    windowNumber: window.windowNumber, context: nil, characters: c, charactersIgnoringModifiers: c,
                    isARepeat: false, keyCode: c == " " ? 49 : code))
            if let passed = app.handleKeyDown(event) { window.sendEvent(passed) }
        }
    }

    /// Typing ends (here a click away): the notes go to the cue they were typed for and the
    /// masthead then shows the new standby cue.
    func testMastheadNotesTypedDuringAPlayheadMoveSaveToTheirOwnCue() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.masthead.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        notes.insertText("for one", replacementRange: notes.selectedRange())
        try await moveElsewhere(to: cues[1])
        await assertEventually { self.store.playheads[self.list] == self.cues[1] }
        XCTAssertEqual(wc.masthead.cueID, cues[0])
        window.makeFirstResponder(nil)
        try await assertAllOn(cues[1])
        XCTAssertEqual(wc.masthead.cueID, cues[1])
        XCTAssertEqual(notes.string, "", "the new standby cue's own notes")
        await assertEventually("the notes went to the cue they were typed for") {
            self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "for one"
        }
        XCTAssertNotEqual(store.model?.cue(cues[1])?.raw["notes"]?.stringValue, "for one")
    }

    // MARK: Typing in the masthead goes on after a command takes the text

    /// Save, and every edit command, sends the notes typed so far; the focus stays, and what is typed
    /// next must reach the cue as well.
    func testMastheadNotesTypedAfterASaveAreSavedToo() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.masthead.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("one", in: window)
        let saved = await wc.commitPendingEdits()
        XCTAssertTrue(saved)
        await assertEventually { self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "one" }
        XCTAssertTrue(window.firstResponder === notes, "the focus stays")
        try typeKeys(" two", in: window)
        window.makeFirstResponder(nil)
        await assertEventually("every character reached the cue") {
            self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "one two"
        }
    }

    /// The same with the playhead moving meanwhile: the masthead keeps the cue being typed for, and
    /// shows the new standby cue only when the typing ends.
    func testMastheadNotesTypedAfterASaveStayOnTheirCueWhenThePlayheadMoves() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.masthead.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("one", in: window)
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        let saved = await wc.commitPendingEdits()
        XCTAssertTrue(saved)
        XCTAssertEqual(wc.masthead.cueID, cues[0], "still the cue being typed for")
        XCTAssertEqual(notes.string, "one")
        try typeKeys("xyz", in: window)
        window.makeFirstResponder(nil)
        await assertEventually("saved to the cue it was typed for") {
            self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "onexyz"
        }
        XCTAssertEqual(store.model?.cue(cues[2])?.raw["notes"]?.stringValue ?? "", "")
        XCTAssertEqual(wc.masthead.cueID, cues[2])
    }

    func testTabInMastheadNotesEndsTyping() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        let notes = wc.masthead.notesView
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("half", in: window)
        try typeKeys("\t", in: window, code: 48)
        XCTAssertFalse(window.firstResponder === notes, "Tab ended the typing")
        await assertEventually("saved, with no tab character") {
            self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "half" && notes.string == "half"
        }
    }

    /// Return with no selection change to follow: the arrow keys work straight away.
    func testReturnInMastheadNotesLeavesTheKeysWithTheList() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        try typeKeys("note", in: window)
        try typeKeys("\r", in: window, code: 36)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
        window.sendEvent(try XCTUnwrap(app.handleKeyDown(try downArrow)))
        try await assertAllOn(cues[1])
    }

    // MARK: Notes the engine refuses or that are not delivered: a sheet with Copy Text, nothing held

    private let tooLongNotes = String(repeating: "x", count: 64 * 1024 + 1)

    /// Copy Text puts the text on a pasteboard of this test's own.
    private func notesPasteboard() -> NSPasteboard {
        let board = NSPasteboard(name: NSPasteboard.Name("openlab-test-\(UUID().uuidString)"))
        wc.notesPasteboard = board
        return board
    }

    /// Two characters by key, then a paste past the engine's 64 KiB limit.
    private func typeTooLong(into view: NSTextView) throws {
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(view))
        try typeKeys("ab", in: window)
        view.insertText(tooLongNotes, replacementRange: view.selectedRange())
    }

    /// The panel names the cue (unless `cue` is nil) and says the notes were not saved; it never takes
    /// the keys. Copy Text puts `text` on the pasteboard and closes it.
    private func answerNotice(
        _ board: NSPasteboard, containing reason: String, cue: String? = "1.wav", text: String,
        file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        await assertEventually("a panel", file: file, line: line) { !NotesNoticePanel.open.isEmpty }
        let panel = try XCTUnwrap(NotesNoticePanel.open.last?.panel, file: file, line: line)
        XCTAssertFalse(panel.canBecomeKey, "never key", file: file, line: line)
        XCTAssertFalse(panel.isKeyWindow, file: file, line: line)
        let said = sheetText(panel)
        XCTAssertTrue(said.contains("not saved"), said, file: file, line: line)
        XCTAssertTrue(said.contains(reason), said, file: file, line: line)
        if let cue { XCTAssertTrue(said.contains(cue), "names the cue: \(said)", file: file, line: line) }
        XCTAssertTrue(pressButton(loc("alert.copyText"), in: panel), file: file, line: line)
        await assertEventually("the panel closed", file: file, line: line) { NotesNoticePanel.open.isEmpty }
        XCTAssertEqual(board.string(forType: .string), text, "Copy Text", file: file, line: line)
    }

    /// Nothing is held: the fields show what the cue holds, and Save and Show Mode work.
    private func assertNothingHeld(file: StaticString = #filePath, line: UInt = #line) async {
        XCTAssertEqual(wc.masthead.notesView.string, "", file: file, line: line)
        XCTAssertEqual(wc.inspector.notesView.string, "", file: file, line: line)
        let ok = await wc.commitPendingEdits()
        XCTAssertTrue(ok, "Save is not blocked", file: file, line: line)
        await wc.requestShowMode(true)
        XCTAssertTrue(wc.isShowMode, "Show Mode is not blocked", file: file, line: line)
    }

    func testMastheadNotesTheEngineRefusesGiveANoticeWithCopyTextAndHoldNothing() async throws {
        try await threeCues()
        let board = notesPasteboard()
        try typeTooLong(into: wc.masthead.notesView)
        try typeKeys("\r", in: try XCTUnwrap(wc.window), code: 36)
        try await answerNotice(board, containing: "refused", text: "ab" + tooLongNotes)
        await assertEventually { self.wc.masthead.notesView.string == "" }
        await assertNothingHeld()
    }

    func testInspectorNotesTheEngineRefusesGiveANoticeWithCopyTextAndHoldNothing() async throws {
        try await threeCues()
        let board = notesPasteboard()
        try typeTooLong(into: wc.inspector.notesView)
        try typeKeys("\t", in: try XCTUnwrap(wc.window), code: 48)
        try await answerNotice(board, containing: "refused", text: "ab" + tooLongNotes)
        await assertEventually { self.wc.inspector.notesView.string == "" }
        await assertNothingHeld()
    }

    func testMastheadNotesThatCouldNotBeSentGiveANotice() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        try typeKeys("short", in: window)
        store.client.stop()
        try typeKeys("\r", in: window, code: 36)
        try await answerNotice(board, containing: "did not receive", text: "short")
    }

    func testInspectorNotesThatCouldNotBeSentGiveANotice() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.notesView))
        try typeKeys("short", in: window)
        store.client.stop()
        try typeKeys("\r", in: window, code: 36)
        try await answerNotice(board, containing: "did not receive", text: "short")
    }

    private func deleteCueElsewhere(_ id: ModelID) async {
        let other = Rig(engine: engine, workspace: doc)
        guard await other.connect(),
            let remove = Command.transaction([.removeCue(id)]),
            case .success = await other.store.send(command: remove)
        else { return XCTFail("delete elsewhere") }
        other.client.stop()
        await assertEventually { self.store.model?.cue(id) == nil }
    }

    /// The cue is deleted (from another client) while its notes are typed in the masthead.
    func testMastheadNotesTypedForACueDeletedMeanwhileGiveANotice() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        try typeKeys("lost", in: window)
        await deleteCueElsewhere(cues[0])
        try typeKeys("\r", in: window, code: 36)
        try await answerNotice(board, containing: "no longer exists", cue: nil, text: "lost")
    }

    /// The same for the inspector's notes.
    func testInspectorNotesTypedForACueDeletedMeanwhileGiveANotice() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.notesView))
        try typeKeys("lost", in: window)
        await deleteCueElsewhere(cues[0])
        window.makeFirstResponder(nil)
        try await answerNotice(board, containing: "no longer exists", cue: nil, text: "lost")
    }

    /// Notes refused with the playhead moved meanwhile: afterwards the standby line and selection
    /// are on the playhead, and Save and Show Mode work.
    func testNotesRefusedWithThePlayheadMovedLeaveTheStandbyOnThePlayhead() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        try typeTooLong(into: wc.masthead.notesView)
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        window.makeFirstResponder(nil)
        try await answerNotice(board, containing: "refused", text: "ab" + tooLongNotes)
        try await assertAllOn(cues[2])
        XCTAssertEqual(wc.masthead.cueID, cues[2])
        let ok = await wc.commitPendingEdits()
        XCTAssertTrue(ok, "Save is not blocked")
        await wc.requestShowMode(true)
        XCTAssertTrue(wc.isShowMode, "Show Mode is not blocked")
    }

    /// A refusal arriving in Show Mode raises no panel, no modal and no sheet: the footer has the message and
    /// the log the message and the text.
    func testANotesRefusalInShowModeRaisesNothingAndIsLogged() async throws {
        try await threeCues()
        _ = notesPasteboard()
        wc.setShowMode(true)
        await wc.editor.reportNotesNotSaved(
            of: cues[0], text: "typed text", .engine(code: .commandRefused, message: "too big"))
        XCTAssertNil(wc.window?.attachedSheet, "no sheet")
        XCTAssertTrue(NotesNoticePanel.open.isEmpty, "no panel")
        XCTAssertNil(NSApp.modalWindow, "nothing modal")
        XCTAssertTrue(wc.footer.noticeLabel.stringValue.contains(NotesKeeper.fileName))
        XCTAssertTrue(
            store.logEntries.contains { $0.message.contains("not saved") && $0.message.contains("typed text") })
    }

    /// Save with notes the engine refuses still being typed: the sheet, the typing ends and the field
    /// shows what the cue holds; a Return after it raises no second notice.
    func testSaveWithMastheadNotesTheEngineRefusesEndsTheTypingAfterOneNotice() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        try typeTooLong(into: wc.masthead.notesView)
        let ok = await wc.commitPendingEdits()
        XCTAssertTrue(ok, "Save goes on")
        try await answerNotice(board, containing: "refused", text: "ab" + tooLongNotes)
        XCTAssertEqual(wc.masthead.notesView.string, "")
        XCTAssertFalse(window.firstResponder === wc.masthead.notesView, "the typing ended")
        try typeKeys("\r", in: window, code: 36)
        await sync()
        XCTAssertTrue(NotesNoticePanel.open.isEmpty, "no second notice")
    }

    /// Notes being typed while another typed edit is invalid: the notice says so and Save stops.
    func testMastheadNotesWithAnInvalidTypedEditGiveANoticeAndSaveStops() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.preWaitField))
        try typeKeys("zz", in: window)  // not a time: invalid when committed
        wc.masthead.notesView.string = "typed"
        wc.masthead.textDidBeginEditing(Notification(name: NSText.didBeginEditingNotification))
        let ok = await wc.commitPendingEdits()
        XCTAssertFalse(ok, "Save stops")
        try await answerNotice(board, containing: "not valid", cue: nil, text: "typed")
    }

    /// Close with refused notes still being typed: the panel comes up and Close goes on without anyone
    /// answering it; the window closing leaves the panel, whose Copy Text still works.
    func testCloseWithNotesTheEngineRefusesGoesOnAndTheNoticeOutlivesTheWindow() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let alerts = ScriptedAlerts()
        alerts.saveChoices = [.dontSave]
        app.alerts = alerts
        try typeTooLong(into: wc.masthead.notesView)
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed, "Close goes on")
        XCTAssertNil(app.windows[doc], "the window is gone")
        try await answerNotice(board, containing: "refused", text: "ab" + tooLongNotes)
    }

    /// The inspector's notes, the same on Close.
    func testCloseWithInspectorNotesTheEngineRefusesGoesOnAndTheNoticeStays() async throws {
        try await threeCues()
        let board = notesPasteboard()
        let alerts = ScriptedAlerts()
        alerts.saveChoices = [.dontSave]
        app.alerts = alerts
        try typeTooLong(into: wc.inspector.notesView)
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed, "Close goes on")
        try await answerNotice(board, containing: "refused", text: "ab" + tooLongNotes)
    }

    /// With the notice up, Space is still GO and the panel is not key.
    func testGoStillFiresWithTheNotesNoticeUp() async throws {
        try await threeCues()
        _ = notesPasteboard()
        try typeTooLong(into: wc.masthead.notesView)
        try typeKeys("\r", in: try XCTUnwrap(wc.window), code: 36)
        await assertEventually("the notice is up") { !NotesNoticePanel.open.isEmpty }
        try await space()
        XCTAssertEqual(goes(), 1, "GO fired")
        XCTAssertEqual(NotesNoticePanel.open.count, 1)
        XCTAssertFalse(try XCTUnwrap(NotesNoticePanel.open.first).panel.isKeyWindow)
    }

    /// Asking for Show Mode with refused notes still being typed enters it: no sheet, no panel, and the
    /// footer and the log carry the notice.
    func testShowModeWithNotesTheEngineRefusesEntersAndLogsTheNotice() async throws {
        try await threeCues()
        _ = notesPasteboard()
        try typeTooLong(into: wc.masthead.notesView)
        await wc.requestShowMode(true)
        XCTAssertTrue(wc.isShowMode)
        XCTAssertNil(wc.window?.attachedSheet)
        XCTAssertTrue(NotesNoticePanel.open.isEmpty)
        XCTAssertTrue(wc.footer.noticeLabel.stringValue.contains(NotesKeeper.fileName))
        XCTAssertTrue(store.logEntries.contains { $0.message.contains("not saved") })
        let kept = try String(contentsOf: NotesKeeper.folder.appendingPathComponent(NotesKeeper.fileName))
        XCTAssertTrue(kept.contains("ab" + tooLongNotes), "the whole text, past 4000 characters, is in the file")
        let label = wc.footer.noticeLabel
        XCTAssertTrue(label.stringValue.contains(NotesKeeper.fileName), "the footer names the file")
        XCTAssertTrue(label.toolTip?.contains(NotesKeeper.folder.path) == true, "the tooltip has the path")
        wc.window?.layoutIfNeeded()
        XCTAssertLessThanOrEqual(
            label.attributedStringValue.size().width, label.frame.width, "the file name is drawn, not cut off")
    }

    /// The notice in Edit Mode names the file, and the file has the whole text.
    func testTheNotesNoticeNamesTheFileThatHoldsTheWholeText() async throws {
        try await threeCues()
        _ = notesPasteboard()
        try typeTooLong(into: wc.masthead.notesView)
        try typeKeys("\r", in: try XCTUnwrap(wc.window), code: 36)
        await assertEventually("the notice is up") { !NotesNoticePanel.open.isEmpty }
        let panel = try XCTUnwrap(NotesNoticePanel.open.last?.panel)
        XCTAssertTrue(sheetText(panel).contains(NotesKeeper.fileName))
        let kept = try String(contentsOf: NotesKeeper.folder.appendingPathComponent(NotesKeeper.fileName))
        XCTAssertTrue(kept.contains("ab" + tooLongNotes))
    }

    // MARK: Loading a list: the highlight goes with the playhead

    private func setPlayheads() -> Int {
        wire.sent.filter { if case .transport(_, .setPlayhead) = $0 { return true } else { return false } }.count
    }

    /// A show as it opens: the playhead's cue is highlighted and in the inspector, nothing is sent,
    /// and the first Down goes to the next row.
    func testOpeningAShowHighlightsThePlayheadCue() async throws {
        wire.forgetSent()
        doc = try await engine.open("Three")
        await assertEventually { self.app.windows[self.doc]?.cueList.roots.count == 3 }
        cues = wc.cueList.roots.map(\.id)
        try await assertAllOn(cues[0])
        XCTAssertEqual(wc.inspector.cueForEdit?.id, cues[0])
        XCTAssertEqual(setPlayheads(), 0, "opening moves nothing")
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        window.sendEvent(try XCTUnwrap(app.handleKeyDown(try downArrow)))
        try await assertAllOn(cues[1])
    }

    func testAnEmptyListOpensWithNothingHighlighted() async throws {
        XCTAssertEqual(wc.cueList.selectedIDs(), [])
        XCTAssertNil(wc.inspector.cueForEdit)
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, loc("inspector.empty"))
        XCTAssertEqual(wc.masthead.standbyField.stringValue, loc("masthead.standby.none"))
    }

    /// A reload in Show Mode highlights the same way and edits nothing.
    func testAReloadInShowModeHighlightsThePlayheadCueAndSendsNothing() async throws {
        try await threeCues()
        wc.setShowMode(true)
        await assertEventually { self.store.document?.showMode == true }
        wc.cueList.sub = nil
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        wire.forgetSent()
        wc.cueList.handle(.reset)
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[2]])
        await settle()
        XCTAssertEqual(setPlayheads(), 0)
    }

    /// A snapshot after a reconnect that moved the playhead: the highlight and the inspector follow.
    func testAReconnectThatMovedThePlayheadFollows() async throws {
        try await threeCues()
        wire.dropConnections()
        await assertEventually { self.store.connectionState != .connected }
        try await moveElsewhere(to: cues[2])
        await assertEventually("back and following") {
            await self.sessionScheduler.fire()
            return self.store.connectionState == .connected && self.wc.cueList.selectedIDs() == [self.cues[2]]
        }
        try await assertAllOn(cues[2])
        XCTAssertEqual(wc.inspector.cueForEdit?.id, cues[2])
    }

    /// A lagged client's resync arrives as the same reset: here with typing under way, which holds it.
    func testAResyncThatMovedThePlayheadFollowsOnceTypingEnds() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.nameField))
        wc.cueList.sub = nil
        try await moveElsewhere(to: cues[2])
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        wc.cueList.handle(.reset)
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]], "held by the typing")
        window.makeFirstResponder(nil)
        await assertEventually { self.wc.cueList.selectedIDs() == [self.cues[2]] }
    }

    /// Adds a list holding one Memo cue with the playhead on `head` (nil: past its end).
    private func addOtherList(playheadOn head: Bool) async throws -> (list: ModelID, cue: ModelID) {
        let other: ModelID = "00000000-0000-0000-0000-0000000000c1"
        let cue: ModelID = "00000000-0000-0000-0000-0000000000c2"
        let memo = try XCTUnwrap(NewCueKind.memo.cueJSON(id: cue, patch: nil))
        guard
            let add = Command.transaction([
                .addCueList(id: other, name: "Other", index: 1), .addCue(memo, list: other, parent: nil, index: 0),
            ]), case .success = await store.send(command: add),
            case .success = await store.transport(.setPlayhead(list: other, cue: head ? cue : nil))
        else {
            XCTFail("other list")
            return (other, cue)
        }
        await sync()
        return (other, cue)
    }

    /// A list with nothing standing by: nothing highlighted, and the inspector must not go on
    /// showing and editing a cue of the list left behind.
    func testSwitchingToAListWithNoPlayheadEmptiesTheInspector() async throws {
        try await threeCues()
        let other = try await addOtherList(playheadOn: false)
        wc.cueList.showList(other.list)
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [])
        XCTAssertNil(wc.inspector.cueForEdit)
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, loc("inspector.empty"))
        XCTAssertFalse(wc.inspector.emptyLabel.isHidden)
    }

    /// A list switch while a click's `set_playhead` is unanswered: the new list is followed once it is.
    func testAListSwitchWithinARoundTripOfAClickFollowsTheNewList() async throws {
        try await threeCues()
        let other = try await addOtherList(playheadOn: true)
        wire.holdResults = true
        click(cues[1])
        wc.cueList.showList(other.list)
        wire.holdResults = false
        wire.releaseResults()
        await assertEventually { self.wc.cueList.selectedIDs() == [other.cue] }
    }

    /// The toolbar adds after what was highlighted when it was used.
    func testToolbarAddAfterThePlayheadMoves() async throws {
        try await threeCues()
        try await chooseFromOpenMenu(afterPlayheadMovesTo: cues[2]) { [self] in wc.cueToolbar.onAdd?(.memo) }
        await assertEventually("the new cue") { self.wc.cueList.roots.count == 4 }
        XCTAssertEqual(wc.cueList.roots[0].id, cues[0])
        XCTAssertEqual(wc.cueList.roots[1].cue.kindType, "memo", "right after cue 1")
    }

    // MARK: A drag whose release never comes

    private func pressKey(_ event: NSEvent) throws {
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        if let passed = app.handleKeyDown(event) { window.sendEvent(passed) }
    }

    /// Show Mode comes on under a press in the level matrix: the matrix goes, its mouse-up never comes,
    /// and the highlight must not stay behind, or Down steps from a stale row and GO fires the wrong cue.
    func testShowModeDuringALevelMatrixPressLetsTheHighlightFollow() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        let matrix = wc.inspector.audioLevels.matrix
        let master = matrix.rect(of: .master)
        let down = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: matrix.convert(NSPoint(x: master.midX, y: master.midY), to: nil),
                modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber, context: nil, eventNumber: 0,
                clickCount: 1, pressure: 1))
        matrix.mouseDown(with: down)
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        wc.setShowMode(true)
        await assertEventually { self.store.showMode }
        await settle()
        try await assertAllOn(cues[1])
        try pressKey(try downArrow)
        try await assertAllOn(cues[2])
        try await space()
        await assertEventually("GO fired the cue the display stood by") { running(cues[2]) }
        XCTAssertFalse(running(cues[1]))
    }

    /// A release lost any other way: the button is up, and the next key both ends the hold and acts on
    /// the row the playhead is on. No further playhead move is needed.
    func testAGestureWhoseReleaseWasLostEndsOnTheNextKey() async throws {
        try await threeCues()
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        wc.inspector.audioLevels.beginGesture()
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        mouseIsDown = false
        try pressKey(try downArrow)
        try await assertAllOn(cues[2])
    }

    /// A release lost with nothing pressed since: the highlight waits, whatever the engine says, and
    /// nothing wrong can fire, as GO and the arrows are keys and the key ends the gesture first.
    func testALostReleaseWaitsThroughEngineUpdatesAndTheNextKeysActOnTheRightCue() async throws {
        try await threeCues()
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(wc.cueList.outlineView))
        wc.inspector.audioLevels.beginGesture()
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        mouseIsDown = false
        wc.cueList.handle(.progress([]))
        wc.cueList.handle(.runtime(cues: [], playheads: []))
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]], "the highlight waits")
        try pressKey(try downArrow)
        try await assertAllOn(cues[2])
        try await space()
        await assertEventually("GO ran the cue standing by") { running(cues[2]) }
        XCTAssertFalse(running(cues[0]) || running(cues[1]))
    }

    /// The next press ends the lost gesture everywhere: click cue 2, edit, and it lands on cue 2.
    func testTheNextPressAfterALostReleaseLeavesNothingOfTheGesture() async throws {
        try await threeCues()
        showTab("levels")
        let p = wc.inspector.audioLevels
        p.beginGesture()
        p.fader.doubleValue = -12
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        await assertEventually { self.master(self.cues[0]) == -12 }
        try await pressOnTrackingView {}
        XCTAssertNil(p.gesture, "the panel's gesture is over")
        click(cues[1])
        try await assertAllOn(cues[1])
        XCTAssertEqual(p.levels?.db(.master), master(cues[1]) ?? 0, "the panel shows cue 2")
        wire.forgetSent()
        p.fader.doubleValue = -3
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        await assertEventually("the edit landed on cue 2") { self.master(self.cues[1]) == -3 }
        XCTAssertEqual(master(cues[0]), -12)
        XCTAssertEqual(commands(on: cues[0]), 0)
    }

    /// While the button really is down, neither a key nor an update ends the hold.
    func testAHeldGestureHoldsThroughKeysAndUpdates() async throws {
        try await threeCues()
        let p = wc.inspector.audioLevels
        p.beginGesture()
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        wc.cueList.handle(.progress([]))
        try pressKey(try key("[", code: 33))  // Pause All: a key the monitor takes
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0]], "still held")
        p.endGesture()
        try await assertAllOn(cues[1])
    }

    // MARK: A gesture's edits land on the cue it began on, whatever arrives when

    private func matrixEvent(_ type: NSEvent.EventType, dy: CGFloat = 0) throws -> NSEvent {
        let matrix = wc.inspector.audioLevels.matrix
        let master = matrix.rect(of: .master)
        return try XCTUnwrap(
            NSEvent.mouseEvent(
                with: type, location: matrix.convert(NSPoint(x: master.midX, y: master.midY + dy), to: nil),
                modifierFlags: [], timestamp: 0, windowNumber: try XCTUnwrap(wc.window).windowNumber, context: nil,
                eventNumber: 0, clickCount: 1, pressure: 1))
    }

    private func master(_ id: ModelID) -> Double? { kind(id)?["levels"]?["master_db"]?.doubleValue }

    /// A release lost after the fader moved: the panel must show the new cue, and an edit there must
    /// not carry the old cue's master with it.
    func testALostGestureLeavesThePanelOnTheCueNowShown() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        let p = wc.inspector.audioLevels
        p.beginGesture()
        p.fader.doubleValue = -12
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        await assertEventually { self.master(self.cues[0]) == -12 }
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        mouseIsDown = false
        _ = app.handleKeyDown(try key("z", code: 6))
        try await assertAllOn(cues[1])
        XCTAssertEqual(p.levels?.db(.master), master(cues[1]) ?? 0, "the panel shows cue 2")
        p.select(.output(0))
        p.beginTyping(p.selection)  // the box opens for typing
        p.levelField.currentEditor()?.selectAll(nil)
        try typeKeys("-20", in: window)
        try typeKeys("\r", in: window, code: 36)
        await assertEventually("the edit landed") {
            self.kind(self.cues[1])?["levels"]?["outputs"]?.arrayValue?.first?["db"]?.doubleValue == -20
        }
        XCTAssertEqual(master(cues[1]), 0, "cue 1's master did not leak to cue 2")
    }

    /// The tail of a drag delivered after its gesture was given up goes nowhere.
    func testADragsTailAfterALostReleaseNeverReachesAnotherCue() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        let matrix = wc.inspector.audioLevels.matrix
        matrix.mouseDown(with: try matrixEvent(.leftMouseDown))
        matrix.mouseDragged(with: try matrixEvent(.leftMouseDragged, dy: 40))
        await assertEventually { self.master(self.cues[0]) == -10 }
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        mouseIsDown = false
        _ = app.handleKeyDown(try key("z", code: 6))
        try await assertAllOn(cues[1])
        wire.forgetSent()
        matrix.mouseDragged(with: try matrixEvent(.leftMouseDragged, dy: 60))
        matrix.mouseUp(with: try matrixEvent(.leftMouseUp, dy: 60))
        await sync()
        await settle()
        XCTAssertEqual(commands(on: cues[1]), 0, "nothing went to the cue it never started on")
    }

    private func dragTrim(to seconds: Double) async throws -> WaveformView {
        let wave = wc.inspector.timeLoops.waveform
        await assertEventually { wave.duration != nil }
        wave.frame = NSRect(x: 0, y: 0, width: 600, height: 100)
        wire.forgetSent()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0)))
        wave.drag(toX: wave.scale.x(seconds))
        return wave
    }

    private func assertTrim(of id: ModelID, is seconds: Double, line: UInt = #line) async {
        await assertEventually("the trim landed", line: line) {
            abs((self.kind(id)?["start_time"]?.doubleValue ?? 0) - seconds) < 0.01
        }
    }

    /// A lost release never drops the operator's trim: it lands on its own cue and only there, and the
    /// late mouse-up does nothing.
    func testAWaveformTrimWhoseReleaseWasLostLandsOnItsCue() async throws {
        try await threeCues()
        let wave = try await dragTrim(to: 0.3)
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        mouseIsDown = false
        _ = app.handleKeyDown(try key("z", code: 6))
        await assertTrim(of: cues[0], is: 0.3)
        try await assertAllOn(cues[1])
        wave.drag(toX: wave.scale.x(0.4))
        wave.endDrag()
        await sync()
        await settle()
        await assertTrim(of: cues[0], is: 0.3)
        XCTAssertEqual(kind(cues[1])?["start_time"]?.doubleValue ?? 0, 0)
        XCTAssertEqual(commands(on: cues[1]), 0)
    }

    /// Save and Close commit first: a menu command after a lost release, with no key and no press in
    /// the window, finds the trim saved.
    func testCommittingPendingEditsFinishesADragWhoseReleaseWasLost() async throws {
        try await threeCues()
        _ = try await dragTrim(to: 0.3)
        mouseIsDown = false
        let committed = await wc.commitPendingEdits()
        XCTAssertTrue(committed)
        await assertTrim(of: cues[0], is: 0.3)
    }

    /// The drag's view going away ends the gesture and commits what was dragged.
    func testATabChangeEndsAWaveformDragAndCommitsIt() async throws {
        try await threeCues()
        showTab("timeLoops")
        _ = try await dragTrim(to: 0.3)
        showTab("basics")
        await assertTrim(of: cues[0], is: 0.3)
        XCTAssertNil(wc.inspector.timeLoops.waveform.dragging)
    }

    /// The selection moves to a cue without the tab: the tabs change under the drag.
    func testASelectionThatChangesTheTabsEndsAWaveformDragAndCommitsIt() async throws {
        try await threeCues()
        let made = await wc.editor.add(.group)
        let group = try XCTUnwrap(made)
        await assertEventually { self.wc.cueList.selectedIDs() == [group] }
        click(cues[0])
        await assertEventually { self.wc.inspector.cueForEdit?.id == self.cues[0] }
        let wave = try await dragTrim(to: 0.3)
        wc.cueList.selectAsUser([group])
        XCTAssertNil(wave.dragging)
        await assertTrim(of: cues[0], is: 0.3)
    }

    func testHidingTheInspectorEndsAWaveformDragAndCommitsIt() async throws {
        try await threeCues()
        _ = try await dragTrim(to: 0.3)
        wc.setVisible(.inspector, false)
        await assertTrim(of: cues[0], is: 0.3)
        XCTAssertNil(wc.inspector.timeLoops.waveform.dragging)
    }

    func testClosingTheWindowEndsAWaveformDragAndCommitsIt() async throws {
        try await threeCues()
        _ = try await dragTrim(to: 0.3)
        wc.windowWillClose(Notification(name: NSWindow.willCloseNotification))
        await assertTrim(of: cues[0], is: 0.3)
        XCTAssertNil(wc.inspector.timeLoops.waveform.dragging)
    }

    /// Show Mode is the one exception: it locks the show, so a drag not yet committed is given up.
    func testShowModeGivesUpAWaveformDrag() async throws {
        try await threeCues()
        let wave = try await dragTrim(to: 0.3)
        wc.setShowMode(true)
        await assertEventually { self.store.showMode }
        XCTAssertNil(wave.dragging)
        await settle()
        await sync()
        XCTAssertEqual(commands(on: cues[0]), 0)
    }

    // MARK: A click while the engine's move is on its way

    /// The store still shows the old playhead because the engine's move has not arrived: a click on
    /// that row must still send, or the move lands and GO fires another cue.
    func testAClickDuringAnEngineMoveStillMovesThePlayhead() async throws {
        try await threeCues()
        wire.holdRuntime = true
        let other = Rig(engine: engine, workspace: doc)
        guard await other.connect(),
            case .success = await other.store.transport(.setPlayhead(list: list, cue: cues[1]))
        else { return XCTFail("set_playhead elsewhere") }
        other.client.stop()
        XCTAssertEqual(store.playheads[list], cues[0], "the move has not arrived here")
        wc.cueList.rowClicked(wc.cueList.outlineView.row(forItem: wc.cueList.node(cues[0])))
        await sync()
        wire.holdRuntime = false
        wire.releaseResults()
        try await assertAllOn(cues[0])
        await sync()
        await settle()
        try await assertAllOn(cues[0])
        try await space()
        await assertEventually("GO ran the clicked cue") { running(cues[0]) }
    }

    /// The selection moved under a gesture by anything but the follow (a menu key, another client
    /// deleting a cue): the edits stay bound to the cue the gesture began on.
    func testAFaderDragStaysOnItsCueWhenTheSelectionMovesUnderIt() async throws {
        try await threeCues()
        let p = wc.inspector.audioLevels
        wire.forgetSent()
        p.beginGesture()
        p.fader.doubleValue = -6
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        wc.cueList.selectAsUser([cues[1]])
        p.fader.doubleValue = -12
        _ = p.fader.target?.perform(p.fader.action, with: p.fader)
        p.endGesture()
        await assertEventually { self.master(self.cues[0]) == -12 }
        await sync()
        XCTAssertEqual(commands(on: cues[1]), 0)
        XCTAssertEqual(p.levels?.db(.master), master(cues[1]) ?? 0, "the panel shows cue 2 after the gesture")
    }

    func testALevelMatrixPressStaysOnItsCueWhenTheSelectionMovesUnderIt() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        let matrix = wc.inspector.audioLevels.matrix
        wire.forgetSent()
        matrix.mouseDown(with: try matrixEvent(.leftMouseDown))
        wc.cueList.selectAsUser([cues[1]])
        await settle()
        matrix.mouseDragged(with: try matrixEvent(.leftMouseDragged, dy: 40))
        matrix.mouseUp(with: try matrixEvent(.leftMouseUp, dy: 40))
        await assertEventually { self.master(self.cues[0]) == -10 }
        await sync()
        XCTAssertEqual(commands(on: cues[1]), 0)
    }

    func testAWaveformDragStaysOnItsCueWhenTheSelectionMovesUnderIt() async throws {
        try await threeCues()
        let wave = wc.inspector.timeLoops.waveform
        await assertEventually { wave.duration != nil }
        wave.frame = NSRect(x: 0, y: 0, width: 600, height: 100)
        wire.forgetSent()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0)))
        wave.drag(toX: wave.scale.x(0.3))
        wc.cueList.selectAsUser([cues[1]])
        wave.endDrag()
        await assertEventually { abs((self.kind(self.cues[0])?["start_time"]?.doubleValue ?? 0) - 0.3) < 0.01 }
        await sync()
        XCTAssertEqual(commands(on: cues[1]), 0)
    }

    // MARK: A refusal outlives the follow it was held behind

    /// Text refused on Return with a follow held: the follow runs at once and the operator must still
    /// see why the text went back. The next follow clears it.
    private func assertRefusalSurvivesTheHeldFollow(
        field: NSTextField, to head: ModelID, next: ModelID
    ) async throws {
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(field))
        field.currentEditor()?.selectAll(nil)
        try typeKeys("abc", in: window)
        try await moveElsewhere(to: head)
        await assertEventually { self.store.playheads[self.list] == head }
        try typeKeys("\r", in: window, code: 36)
        await assertEventually { self.wc.cueList.selectedIDs() == [head] }
        await settle()
        XCTAssertFalse(wc.inspector.messageLabel.stringValue.isEmpty, "the refusal is still on screen")
        try await moveElsewhere(to: next)
        await assertEventually { self.wc.cueList.selectedIDs() == [next] }
        XCTAssertEqual(wc.inspector.messageLabel.stringValue, "", "it does not follow the show for good")
    }

    /// No follow was held behind the typing: a later GO's follow clears the refusal, as it is about
    /// cue 1 and the inspector now shows cue 2.
    func testARefusalWithNoFollowHeldClearsOnTheNextGo() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.preWaitField))
        wc.inspector.preWaitField.currentEditor()?.selectAll(nil)
        try typeKeys("abc", in: window)
        try typeKeys("\r", in: window, code: 36)
        XCTAssertFalse(wc.inspector.messageLabel.stringValue.isEmpty)
        try await space()
        try await assertAllOn(cues[1])
        XCTAssertEqual(wc.inspector.messageLabel.stringValue, "")
    }

    func testAnInvalidReturnWithAHeldFollowKeepsItsMessage() async throws {
        try await threeCues()
        try await assertRefusalSurvivesTheHeldFollow(field: wc.inspector.preWaitField, to: cues[1], next: cues[2])
    }

    func testAnInvalidReturnInATimeAndLoopsFieldKeepsItsMessage() async throws {
        try await threeCues()
        showTab("timeLoops")
        await assertEventually { self.wc.inspector.timeLoops.waveform.duration != nil }
        try await assertRefusalSurvivesTheHeldFollow(
            field: wc.inspector.timeLoops.startField, to: cues[1], next: cues[2])
    }

    func testAnInvalidReturnInAFadeDurationFieldKeepsItsMessage() async throws {
        try await threeCues()
        let made = await wc.editor.add(.fade)
        let fade = try XCTUnwrap(made)
        await assertEventually { self.wc.inspector.cueForEdit?.id == fade }
        showTab("curve")
        try await assertRefusalSurvivesTheHeldFollow(
            field: wc.inspector.fadeSettings.durationField, to: cues[1], next: cues[2])
    }

    // MARK: Return and Esc in the inspector give the keys back

    /// QLab 5.2.2 and 5.2.4 (pp. 36-37): Return in a text field sends focus to the cue list. The held
    /// follow runs, Space fires GO and types nothing.
    func testReturnInAnInspectorFieldEndsTypingAndSpaceFiresGo() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.nameField))
        try typeKeys("Storm", in: window)
        try await moveElsewhere(to: cues[1])
        await assertHeldOnFirstCue(playheadOn: cues[1])
        try typeKeys("\r", in: window, code: 36)
        await assertEventually("saved") { self.store.model?.cue(self.cues[0])?.name == "Storm" }
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
        try await assertAllOn(cues[1])
        try await space()
        await assertEventually { running(cues[1]) }
        XCTAssertEqual(store.model?.cue(cues[0])?.name, "Storm")
    }

    /// The audio matrix's text fields are the ones QLab's change log names (p. 37).
    func testReturnInALevelFieldEndsTyping() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        let field = wc.inspector.audioLevels.levelField
        wc.inspector.audioLevels.beginTyping(wc.inspector.audioLevels.selection)  // the box opens for typing
        field.currentEditor()?.selectAll(nil)
        try typeKeys("-9", in: window)
        try typeKeys("\r", in: window, code: 36)
        await assertEventually("saved") { self.kind(self.cues[0])?["levels"]?["master_db"]?.doubleValue == -9 }
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
    }

    func testReturnInATimeAndLoopsFieldEndsTyping() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("timeLoops")
        await assertEventually { self.wc.inspector.timeLoops.waveform.duration != nil }
        let field = wc.inspector.timeLoops.startField
        XCTAssertTrue(window.makeFirstResponder(field))
        field.currentEditor()?.selectAll(nil)
        try typeKeys("0.2", in: window)
        try typeKeys("\r", in: window, code: 36)
        await assertEventually("saved") { abs((self.kind(self.cues[0])?["start_time"]?.doubleValue ?? 0) - 0.2) < 0.01 }
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
    }

    /// The slice count's field is put away only by the end of editing, which Esc does not report.
    func testEscInTheSliceCountFieldPutsItAwayAndChangesNothing() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("timeLoops")
        let panel = wc.inspector.timeLoops
        await assertEventually { panel.waveform.duration != nil }
        wc.inspector.sendTimeLoops(.addSlice(0.5))
        // The engine's answer, not only the panel's own preview of the edit: that answer redraws it.
        await assertEventually("a slice to count") {
            self.kind(self.cues[0])?["slices"]?.arrayValue?.isEmpty == false && !panel.waveform.sliceMarks.isEmpty
        }
        await sync()
        await settle()
        wire.forgetSent()
        panel.waveform.onEditCount?(0)
        XCTAssertFalse(panel.sliceCountField.isHidden)
        XCTAssertNotNil(panel.sliceCountField.currentEditor())
        try typeKeys("7", in: window)
        try typeKeys("\u{1B}", in: window, code: 53)
        XCTAssertTrue(panel.sliceCountField.isHidden)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
        await settle()
        XCTAssertEqual(commands(on: cues[0]), 0, "the count did not change")
    }

    func testReturnInAFadeDurationFieldEndsTyping() async throws {
        try await threeCues()
        let made = await wc.editor.add(.fade)
        let fade = try XCTUnwrap(made)
        await assertEventually { self.wc.inspector.cueForEdit?.id == fade }
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("curve")
        let field = wc.inspector.fadeSettings.durationField
        XCTAssertTrue(window.makeFirstResponder(field))
        field.currentEditor()?.selectAll(nil)
        try typeKeys("3", in: window)
        try typeKeys("\r", in: window, code: 36)
        await assertEventually("saved") { self.kind(fade)?["duration"]?.doubleValue == 3 }
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
    }

    /// The user's answer (rows 62, 69): Return ends the typing and only Shift-Return types a new line.
    private func assertOnlyShiftReturnTypesALine(in notes: NSTextView) async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("one", in: window)
        try pressKey(try key("\r", code: 36, [.shift]))
        XCTAssertTrue(window.firstResponder === notes, "Shift-Return keeps typing")
        try typeKeys("two", in: window)
        XCTAssertEqual(notes.string, "one\ntwo")
        try pressKey(try key("\r", code: 36, [.option]))
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "Option-Return ends it, as Return does")
        await assertEventually("saved, with no line from Option-Return") {
            self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue == "one\ntwo"
        }
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("!", in: window)
        try typeKeys("\r", in: window, code: 36)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "Return ends it")
        await assertEventually("saved, with no line from Return") {
            !(self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue ?? "").hasSuffix("\n")
                && (self.store.model?.cue(self.cues[0])?.raw["notes"]?.stringValue ?? "").contains("!")
        }
    }

    /// Caps Lock and the keypad's flags are not modifiers here: Shift-Return is still a line, and the
    /// next Space types rather than fires GO. Keypad Enter is a Return.
    private func assertShiftReturnWithOtherFlagsTypesALine(in notes: NSTextView) async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(notes))
        try typeKeys("a", in: window)
        try pressKey(try key("\r", code: 36, [.shift, .capsLock]))
        XCTAssertTrue(window.firstResponder === notes, "Caps Lock")
        try pressKey(try key("\u{3}", code: 76, [.shift, .numericPad]))
        XCTAssertTrue(window.firstResponder === notes, "keypad Enter with Shift")
        try typeKeys(" ", in: window)
        await settle()
        XCTAssertEqual(goes(), 0, "Space typed")
        XCTAssertEqual(notes.string, "a\n\n ")
        try pressKey(try key("\u{3}", code: 76, [.numericPad]))
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "keypad Enter ends the typing")
    }

    func testShiftReturnWithCapsLockOrKeypadTypesALineInTheInspectorNotes() async throws {
        try await assertShiftReturnWithOtherFlagsTypesALine(in: wc.inspector.notesView)
    }

    func testShiftReturnWithCapsLockOrKeypadTypesALineInTheMastheadNotes() async throws {
        try await assertShiftReturnWithOtherFlagsTypesALine(in: wc.masthead.notesView)
    }

    func testOnlyShiftReturnTypesALineInTheInspectorNotes() async throws {
        try await assertOnlyShiftReturnTypesALine(in: wc.inspector.notesView)
    }

    func testOnlyShiftReturnTypesALineInTheMastheadNotes() async throws {
        try await assertOnlyShiftReturnTypesALine(in: wc.masthead.notesView)
    }

    /// Esc drops the text and leaves the keys with the list: Down moves the highlight.
    private func assertEscDropsTheTextAndDownMovesTheHighlight(typingIn view: NSView) async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(view))
        try typeKeys("dropped", in: window)
        try typeKeys("\u{1B}", in: window, code: 53)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
        try pressKey(try downArrow)
        try await assertAllOn(cues[1])
        await sync()
        XCTAssertNotEqual(store.model?.cue(cues[0])?.name, "dropped")
        XCTAssertNotEqual(store.model?.cue(cues[0])?.raw["notes"]?.stringValue, "dropped")
    }

    func testEscInAnInspectorFieldDropsTheTextAndDownMovesTheHighlight() async throws {
        try await assertEscDropsTheTextAndDownMovesTheHighlight(typingIn: wc.inspector.nameField)
    }

    func testEscInTheInspectorNotesDropsTheTextAndDownMovesTheHighlight() async throws {
        try await assertEscDropsTheTextAndDownMovesTheHighlight(typingIn: wc.inspector.notesView)
    }

    func testEscInALevelFieldDropsTheTextAndDownMovesTheHighlight() async throws {
        try await threeCues()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        showTab("levels")
        let field = wc.inspector.audioLevels.levelField
        wc.inspector.audioLevels.beginTyping(wc.inspector.audioLevels.selection)  // the box opens for typing
        field.currentEditor()?.selectAll(nil)
        try typeKeys("-9", in: window)
        try typeKeys("\u{1B}", in: window, code: 53)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView)
        XCTAssertEqual(field.stringValue, LevelText.format(0), "the level it had")
        await settle()
        try await play(0)
        XCTAssertEqual(kind(cues[0])?["levels"]?["master_db"]?.doubleValue, 0)
        try pressKey(try downArrow)
        try await assertAllOn(cues[1])
    }

    // MARK: #496: GO on a group after a mode change

    private var group: ModelID = "none"
    private var child1: ModelID = "none"
    private var child2: ModelID = "none"
    private var cue2: ModelID = "none"

    /// The issue's workspace, made as the operator makes it: a new Group cue; two Audio cues dropped
    /// into it; Audio cue 2 dropped after it; the group's row clicked; `modes` chosen in the Mode
    /// tab, in order.
    private func groupWorkspace(_ modes: [String]) async throws {
        let files = try tempFiles(["one.wav", "two.wav", "three.wav"])
        let made = await wc.editor.add(.group)
        group = try XCTUnwrap(made)
        let kids = await wc.editor.addFiles(Array(files[0..<2]), parent: group, index: 0)
        XCTAssertEqual(kids.count, 2)
        (child1, child2) = (kids[0], kids[1])
        let after = await wc.editor.addFiles([files[2]], parent: nil, index: 1)
        cue2 = try XCTUnwrap(after.first)
        try await play(0)  // the files are probed
        await playheadSettled(on: cue2)
        await assertEventually { wc.cueList.roots.map(\.id) == [group, cue2] }
        await assertEventually { wc.cueList.node(group)?.children.map(\.id) == [child1, child2] }
        click(group)
        try await assertAllOn(group)
        for m in modes { try await choose(m) }
        try await assertAllOn(group)
    }

    /// Reproduce: the issue's steps where the group is the last cue and has been played once (as
    /// the first try of a new group is). The playhead went past the end of the list while the
    /// group's row stayed highlighted and in the inspector, so the operator changed the mode there,
    /// pressed Space and nothing played. Now the highlight and the inspector leave the group with
    /// the playhead, nothing shows as standing by, and the click that brings the group back puts
    /// the playhead on it: Space then plays it in its new mode.
    func testReproduceGoOnAGroupAfterAModeChange() async throws {
        let files = try tempFiles(["one.wav", "two.wav"])
        let made = await wc.editor.add(.group)
        group = try XCTUnwrap(made)
        let kids = await wc.editor.addFiles(files, parent: group, index: 0)
        (child1, child2) = (kids[0], kids[1])
        try await play(0)
        await playheadSettled(on: child2)
        click(group)
        try await assertAllOn(group)
        try await space()
        await assertEventually { running(child1) && running(child2) }
        try await assertAllOn(nil)
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, loc("inspector.empty"), "the inspector shows no cue")
        try await play(2)
        await assertEventually { !self.running(self.child1) && !self.running(self.child2) }
        click(group)
        try await assertAllOn(group)
        try await choose("playlist")
        try await space()
        await assertEventually("the group plays in its new mode") { running(group) && running(child1) }
        XCTAssertFalse(running(child2))
        try await assertAllOn(nil)
    }

    /// Timeline (QLab 5 manual p. 138): both children and the group run, cue 2 stands by.
    func testTimelineSpaceRunsBothChildrenAndStandsByCue2() async throws {
        try await groupWorkspace([])
        XCTAssertEqual(mode(group), "timeline", "a new group")
        try await space()
        await assertEventually { [group, child1, child2].allSatisfy { running($0) } }
        XCTAssertFalse(running(cue2))
        try await assertAllOn(cue2)
    }

    /// Playlist (p. 138): 1.1 runs, then 1.2 once it ends; cue 2 stands by.
    func testPlaylistSpaceRunsTheChildrenOneAfterTheOtherAndStandsByCue2() async throws {
        try await groupWorkspace(["playlist"])
        try await space()
        await assertEventually { running(group) && running(child1) }
        XCTAssertFalse(running(child2))
        try await assertAllOn(cue2)
        try await play(1)
        await assertEventually { self.running(self.child2) && !self.running(self.child1) }
        XCTAssertFalse(running(cue2))
        try await assertAllOn(cue2)
    }

    /// Start first child and enter (pp. 138-139): 1.1 runs, 1.2 stands by; a second Space runs 1.2
    /// and cue 2 stands by.
    func testStartFirstChildAndEnterSpaceRuns1Then12AndStandsByCue2() async throws {
        try await groupWorkspace(["start_first_and_enter"])
        try await space()
        await assertEventually { running(child1) }
        XCTAssertFalse(running(child2))
        try await assertAllOn(child2)
        try await space()
        await assertEventually { running(child2) }
        try await assertAllOn(cue2)
    }

    /// Start first child and go to the next cue (p. 139): 1.1 runs, 1.2 never starts, cue 2 stands by.
    func testStartFirstChildAndGoToNextSpaceRunsOnly11AndStandsByCue2() async throws {
        try await groupWorkspace(["start_first_and_go_to_next"])
        try await space()
        await assertEventually { running(child1) }
        try await assertAllOn(cue2)
        try await play(0.5)
        XCTAssertFalse(running(child2), "1.2 does not start")
        try await play(1)
        XCTAssertFalse(running(child2), "not after 1.1 either")
    }

    /// Start a random child (p. 139): one child runs and cue 2 stands by; once it has stopped, a click
    /// on the group's row and Space run the other (round robin).
    func testStartRandomChildSpaceRunsOneThenTheOtherAfterAClickOnTheGroup() async throws {
        try await groupWorkspace(["start_random"])
        try await space()
        let first = [child1, child2].filter(running)
        XCTAssertEqual(first.count, 1, "exactly one child")
        try await assertAllOn(cue2)
        try await play(2)
        await assertEventually { !self.running(self.child1) && !self.running(self.child2) }
        click(group)
        try await assertAllOn(group)
        try await space()
        let second = [child1, child2].filter(running)
        XCTAssertEqual(second.count, 1)
        XCTAssertNotEqual(first, second, "round robin")
        try await assertAllOn(cue2)
    }

    /// Mode change round trip: Timeline, Playlist, back to Timeline; the last mode chosen is used.
    func testModeChangeRoundTripRunsBothChildren() async throws {
        try await groupWorkspace(["playlist", "timeline"])
        try await space()
        await assertEventually { running(child1) && running(child2) }
        try await assertAllOn(cue2)
    }
}

/// What the cue list draws, on the real engine: the status, cue-type and Duration cells are rendered
/// and their pixels read, so what a test sees is what is on screen.
@MainActor
final class CueListDrawingTests: XCTestCase {
    typealias H = CueListHarness
    private let ten: ModelID = "00000000-0000-0000-0000-00000000000a"
    private let five: ModelID = "00000000-0000-0000-0000-0000000000f2"
    private let memo: ModelID = "00000000-0000-0000-0000-0000000000f3"
    private var window: NSWindow?

    override func tearDown() async throws {
        window?.close()
        window = nil
    }

    /// Audio cue 1 (10 s), Audio cue 2 (5 s), Memo cue 3: the Demo show with its cues replaced.
    private func timingRig(loopsForever: Bool = false) async throws -> Rig {
        let (five, memo) = (self.five, self.memo)
        return try await Rig.open(
            "Demo",
            edit: { folder in
                let media = folder.appendingPathComponent("media")
                try writeWAV(media.appendingPathComponent("ten.wav"), seconds: 10)
                try writeWAV(media.appendingPathComponent("five.wav"), seconds: 5)
                try editShow(folder.appendingPathComponent("Demo.openlab")) { ws in
                    guard var list = ws["cue_lists"]?.arrayValue?.first, var one = list["cues"]?.arrayValue?.first,
                        var kind = one["kind"]
                    else { return }
                    kind.set("media", .string("media/ten.wav"))
                    // The Demo loops until devamped (no duration); `loopsForever` keeps that for cue 1 only.
                    kind.set("last_slice_play_count", .int(loopsForever ? 0 : 1))
                    one.set("kind", kind)
                    var two = one
                    two.set("id", .string(five.raw))
                    two.set("number", .string("2"))
                    two.set("name", .string("Two"))
                    kind.set("media", .string("media/five.wav"))
                    kind.set("last_slice_play_count", .int(1))
                    two.set("kind", kind)
                    var note = one
                    note.set("id", .string(memo.raw))
                    note.set("number", .string("3"))
                    note.set("name", .string("Note"))
                    note.set("kind", .object(["type": .string("memo")]))
                    list.set("cues", .array([one, two, note]))
                    var lists = ws["cue_lists"]?.arrayValue ?? []
                    lists[0] = list
                    ws.set("cue_lists", .array(lists))
                }
            })
    }

    /// The cue list in a key window with the table focused, so a selected row is drawn as the user sees it.
    private func harness(_ rig: Rig) async throws -> H {
        let h = try await H(rig: rig)
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 400), styleMask: [.titled, .resizable],
            backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        w.appearance = NSAppearance(named: .aqua)  // the pixel checks assume dark icons on a light row
        w.contentView = h.vc.view
        w.makeKeyAndOrderFront(nil)
        w.makeFirstResponder(h.spy)
        window = w
        w.layoutIfNeeded()
        return h
    }

    /// The rendered pixels (0...1 RGB) of one cell of a row, as drawn inside the row (selection and
    /// alternating background included).
    private func pixels(_ h: H, _ column: String, _ id: ModelID) throws -> Pixels {
        try pixels(h.spy, column, item: h.vc.node(id))
    }

    /// The same on a dark list, where the playhead's pointer is white.
    private func darkPixels(_ h: H, _ column: String, _ id: ModelID) throws -> Pixels {
        h.spy.appearance = NSAppearance(named: .darkAqua)
        return try pixels(h.spy, column, item: h.vc.node(id), background: .black, fromRowEdge: true)
    }

    private func pixels(
        _ outline: NSOutlineView, _ column: String, item: Any?, background: NSColor = .white,
        fromRowEdge: Bool = false
    ) throws -> Pixels {
        let row = outline.row(forItem: item)
        let col = outline.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        XCTAssertGreaterThanOrEqual(row, 0)
        let rowView = try XCTUnwrap(outline.rowView(atRow: row, makeIfNecessary: true))
        rowView.layoutSubtreeIfNeeded()
        let cell = outline.frameOfCell(atColumn: col, row: row)
        let rowFrame = outline.rect(ofRow: row)
        // `fromRowEdge`: with what the row draws left of the cell (the playhead's pointer).
        let left = fromRowEdge ? 0 : cell.minX - rowFrame.minX
        let region = NSRect(x: left, y: 0, width: cell.maxX - rowFrame.minX - left, height: rowFrame.height)
        rowView.layoutSubtreeIfNeeded()
        let scale = max(1, rowView.window?.backingScaleFactor ?? 2)
        return Pixels(
            rep: try renderView(rowView, scale: scale, background: background), region: region, scale: scale)
    }

    /// `view` drawn into a bitmap on the window's light background (under a clear row).
    private func render(_ view: NSView, scale: CGFloat) throws -> NSBitmapImageRep {
        try renderView(view, scale: scale, background: .white)
    }

    struct Pixels {
        let rep: NSBitmapImageRep
        let region: NSRect
        let scale: CGFloat

        var all: [(x: Int, r: CGFloat, g: CGFloat, b: CGFloat)] {
            var out: [(Int, CGFloat, CGFloat, CGFloat)] = []
            let x0 = Int(region.minX * scale)
            let x1 = min(rep.pixelsWide, Int(region.maxX * scale))
            for y in 0..<rep.pixelsHigh {
                for x in x0..<max(x0, x1) {
                    guard let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                    out.append((x - x0, c.redComponent, c.greenComponent, c.blueComponent))
                }
            }
            return out
        }

        /// Pixels clearly unlike the cell's top-left pixel (its background, light or dark).
        func countDifferingFromBackground() -> Int {
            guard let first = all.first else { return 0 }
            return count { r, g, b in abs(r - first.r) + abs(g - first.g) + abs(b - first.b) > 0.6 }
        }

        func count(_ match: (CGFloat, CGFloat, CGFloat) -> Bool) -> Int { all.filter { match($0.r, $0.g, $0.b) }.count }
        /// Pixel columns (0...1 of the cell's width) the matching pixels reach.
        func extent(_ match: (CGFloat, CGFloat, CGFloat) -> Bool) -> ClosedRange<Double>? {
            let xs = all.filter { match($0.r, $0.g, $0.b) }.map(\.x)
            guard let lo = xs.min(), let hi = xs.max() else { return nil }
            let w = Double(region.width * scale)
            return (Double(lo) / w)...(Double(hi + 1) / w)
        }
    }

    // Colours as drawn: the system green (running), yellow (paused) and the playhead's grey.
    private static let green: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in g > 0.65 && r < 0.4 && b < 0.55 }
    private static let greenish: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in g - r > 0.08 && g - b > 0.04 }
    private static let yellowish: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in
        r > 0.85 && g > 0.7 && r - b > 0.2 && abs(r - g) < 0.3
    }
    /// The playhead's pointer on a dark list: white.
    private static let pointerWhite: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in
        r > 0.97 && g > 0.97 && b > 0.97
    }

    private func go(_ rig: Rig) async throws {
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance()
    }

    func testThePlayheadIsAWhiteTriangleAtTheRowsLeftEdge() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == self.ten }
        let one = try darkPixels(h, "status", ten)
        XCTAssertGreaterThan(one.count(Self.pointerWhite), 40, "cue 1 shows the white pointer")
        XCTAssertEqual(one.count(Self.green), 0, "and no green triangle")
        XCTAssertEqual(try darkPixels(h, "status", five).count(Self.pointerWhite), 0, "no other row shows a pointer")
        XCTAssertEqual(try darkPixels(h, "status", memo).count(Self.pointerWhite), 0)
        // From the row's very edge, about ten points wide: a triangle, widest at the edge.
        let reach = try XCTUnwrap(one.extent(Self.pointerWhite))
        XCTAssertEqual(reach.lowerBound, 0, accuracy: 0.02)
        XCTAssertEqual(reach.upperBound * Double(one.region.width), Double(CueRowView.pointerWidth), accuracy: 2)
    }

    func testRunningAndStandingByAtOnce() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        try await go(rig)
        await assertEventually { rig.store.cueStates[self.ten]?.state == .running }
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == self.five }
        let one = try darkPixels(h, "status", ten)
        XCTAssertGreaterThan(one.count(Self.green), 8, "cue 1 shows a green running triangle")
        XCTAssertEqual(one.count(Self.pointerWhite), 0)
        let two = try darkPixels(h, "status", five)
        XCTAssertGreaterThan(two.count(Self.pointerWhite), 40, "cue 2 shows the pointer")
        XCTAssertEqual(two.count(Self.green), 0)
        let running = try XCTUnwrap(h.cell("status", ten)).images[1]
        let pointer = try XCTUnwrap(h.cell("status", five)).images[0]
        XCTAssertEqual(running.accessibilityLabel(), loc("cue.state.running"))
        XCTAssertEqual(pointer.accessibilityLabel(), loc("cue.playhead"))
    }

    func testHighlightedRowsKeepTheRunningColourAndThePointer() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        try await go(rig)
        await assertEventually { rig.store.cueStates[self.ten]?.state == .running }
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == self.five }
        h.spy.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        h.spy.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: true)
        try await rig.advance()
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == self.five }
        XCTAssertEqual(h.spy.selectedRowIndexes, IndexSet([0, 1]))
        // A test process is not the active app, so AppKit draws the selection unfocused; draw it as a
        // focused one (white-on-accent is what turns template icons white).
        for row in 0...1 { h.spy.rowView(atRow: row, makeIfNecessary: true)?.isEmphasized = true }
        XCTAssertGreaterThan(try darkPixels(h, "status", ten).count(Self.green), 8, "running triangle stays green")
        XCTAssertGreaterThan(
            try darkPixels(h, "status", five).count(Self.pointerWhite), 40, "the pointer is drawn over the highlight")
    }

    func testCueTypeColumn() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        let ids = h.spy.tableColumns.map(\.identifier.rawValue)
        let status = try XCTUnwrap(ids.firstIndex(of: "status"))
        XCTAssertEqual(Array(ids[status...(status + 2)]), ["status", "type", "number"])
        for (cue, kind) in [(ten, "audio"), (five, "audio"), (memo, "memo")] {
            let type = try XCTUnwrap(h.cell("type", cue))
            XCTAssertFalse(type.images[0].isHidden)
            XCTAssertEqual(type.images[0].accessibilityLabel(), CueKindStyle.kindLabel(kind))
            let drawn = try pixels(h, "type", cue)
            XCTAssertGreaterThan(drawn.countDifferingFromBackground(), 8, "the icon is drawn for \(kind)")
        }
        // The Name cell shows only the name.
        let name = try XCTUnwrap(h.cell("name", ten))
        XCTAssertTrue(name.images.allSatisfy(\.isHidden))
        XCTAssertEqual(name.label.stringValue, "Intro")
        // The icon sits between the status and Number columns.
        let col = { h.spy.column(withIdentifier: NSUserInterfaceItemIdentifier($0)) }
        XCTAssertEqual(col("type"), col("status") + 1)
        XCTAssertEqual(col("number"), col("type") + 1)
    }

    /// The cell's middle 18 points, where its button is.
    private func middle(_ p: Pixels) -> Pixels {
        let region = NSRect(x: p.region.midX - 9, y: 0, width: 18, height: p.region.height)
        return Pixels(rep: p.rep, region: region, scale: p.scale)
    }

    private func drawnMask(_ p: Pixels) -> [Bool] { p.all.map { Self.drawn($0.r, $0.g, $0.b) } }

    /// `view` of `wc` in a window of its own, so a click can be hit-tested through the window and it draws.
    @discardableResult
    private func show(_ view: NSView) -> NSWindow {
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 400), styleMask: [.titled, .resizable],
            backing: .buffered, defer: false)
        w.isReleasedWhenClosed = false
        w.appearance = NSAppearance(named: .aqua)  // the pixel checks assume dark icons on a light row
        w.contentView = view
        w.makeKeyAndOrderFront(nil)
        w.layoutIfNeeded()
        window = w
        return w
    }

    /// A click on `button`: the middle of it must reach it through the window's hit test, then it is
    /// pressed. A synthetic press through `window.sendEvent` is not used: when the app is not active (CI),
    /// the press only brings the window forward and the button never fires.
    private func click(_ button: NSButton) throws {
        let window = try XCTUnwrap(button.window)
        let point = button.convert(NSPoint(x: button.bounds.midX, y: button.bounds.midY), to: nil)
        XCTAssertTrue(window.contentView?.hitTest(point) === button, "the click would land elsewhere")
        XCTAssertTrue(button.isEnabled)
        button.performClick(nil)
    }

    private static let drawn: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in r + g + b < 2.2 }

    /// The Demo show plus an Audio cue with no file, added through the store.
    private func targetRig() async throws -> (Rig, ModelID) {
        let rig = try await Rig.open()
        return (rig, try await add(.audio, rig))
    }

    /// Adds a new cue of `kind`, as the toolbar makes it with nothing selected, through the store.
    private func add(_ kind: NewCueKind, _ rig: Rig) async throws -> ModelID {
        let id = ModelID(UUID().uuidString.lowercased())
        let model = rig.store.model
        let cue = try XCTUnwrap(
            kind.cueJSON(id: id, patch: model?.patches.first, networkPatch: model?.networkPatches.first))
        let added = await rig.store.send(command: .addCue(cue, list: TestSnapshots.mainList, parent: nil, index: 0))
        guard case .success = added else { throw TestEngineError.notConnected }
        return id
    }

    /// The Target cell of `id` in a window controller's cue list.
    private func targetCell(_ wc: WorkspaceWindowController, _ id: ModelID) -> CueCell? {
        let o = wc.cueList.outlineView
        let col = o.column(withIdentifier: NSUserInterfaceItemIdentifier("target"))
        return o.view(atColumn: col, row: o.row(forItem: wc.cueList.node(id)), makeIfNecessary: true) as? CueCell
    }

    private func cellPixels(_ wc: WorkspaceWindowController, _ id: ModelID) throws -> Pixels {
        try pixels(wc.cueList.outlineView, "target", item: wc.cueList.node(id))
    }

    /// `button` as drawn, and a new button of the same shape beside it (so in the same window state)
    /// showing `icon`, as drawn.
    private func drawnGlyphs(_ button: NSButton, _ icon: Icon, label: String) throws -> (shown: Pixels, icon: Pixels) {
        let reference = NSButton(title: button.title, target: nil, action: nil)
        reference.frame = button.frame
        button.superview?.addSubview(reference)
        defer { reference.removeFromSuperview() }
        reference.isBordered = button.isBordered
        reference.bezelStyle = button.bezelStyle
        reference.imagePosition = button.imagePosition
        reference.imageScaling = button.imageScaling
        reference.image = icon.image(label, compact: true)
        let scale = max(1, button.window?.backingScaleFactor ?? 2)
        func drawn(_ view: NSButton) throws -> Pixels {
            Pixels(rep: try render(view, scale: scale), region: view.bounds, scale: scale)
        }
        return (try drawn(button), try drawn(reference))
    }

    func testTargetColumnForAFileCue() async throws {
        let (rig, empty) = try await targetRig()
        let memo = try await add(.memo, rig)
        let h = try await harness(rig)
        await assertEventually { h.vc.node(empty) != nil && h.vc.node(memo) != nil }
        let file = try XCTUnwrap(h.cell("target", H.intro))
        let none = try XCTUnwrap(h.cell("target", empty))
        for cell in [file, none] {
            XCTAssertFalse(cell.chooseButton.isHidden)
            XCTAssertEqual(cell.chooseButton.accessibilityLabel(), loc("inspector.target.choose"))
            XCTAssertEqual(cell.label.stringValue, "", "no file name")
            XCTAssertTrue(cell.images[0].isHidden)
        }
        // The same glyph is drawn with and without a file, and nothing else.
        let f = try pixels(h, "target", H.intro)
        XCTAssertGreaterThan(middle(f).count(Self.drawn), 8, "the glyph is drawn")
        XCTAssertGreaterThan(try pixels(h, "target", empty).count(Self.drawn), 8, "with no file too")
        // The same row without its file: a row's background shifts the glyph's edge pixels.
        _ = await rig.store.send(command: .setProperty(H.intro, .media(nil)))
        await assertEventually { (h.store.model?.cue(H.intro)?.raw["kind"]?["media"]?.stringValue ?? "").isEmpty }
        XCTAssertEqual(drawnMask(f), drawnMask(try pixels(h, "target", H.intro)), "the same glyph")
        XCTAssertEqual(f.count(Self.drawn), middle(f).count(Self.drawn), "centred, and all the cell holds")
        XCTAssertEqual(f.region.width, 60, accuracy: 1, "the column's default width")
        XCTAssertNotEqual(Icon.targetFile.symbol, Icon.fieldChoose.symbol)
        // Cues that take no target: nothing.
        for id in [H.group, memo] {
            let cell = try XCTUnwrap(h.cell("target", id))
            XCTAssertTrue(cell.chooseButton.isHidden)
            XCTAssertTrue(cell.images[0].isHidden)
            XCTAssertEqual(cell.label.stringValue, "")
            XCTAssertEqual(try pixels(h, "target", id).count(Self.drawn), 0)
        }
    }

    func testTargetColumnForACueTargetingCue() async throws {
        let rig = try await Rig.open()
        let stop = try await add(.stop, rig)
        let h = try await harness(rig)
        await assertEventually { h.vc.node(stop) != nil }
        func assertShows(_ id: ModelID, _ text: String, line: UInt = #line) throws {
            let cell = try XCTUnwrap(h.cell("target", id), line: line)
            XCTAssertEqual(cell.label.stringValue, text, line: line)
            XCTAssertFalse(cell.label.isHidden, line: line)
            XCTAssertTrue(cell.images[0].isHidden, "no marker", line: line)
            XCTAssertTrue(cell.chooseButton.isHidden, "no button", line: line)
            XCTAssertGreaterThan(try pixels(h, "target", id).count(Self.drawn), 8, "drawn", line: line)
        }
        try assertShows(H.fade, "1")
        XCTAssertEqual(h.cell("target", H.fade)?.label.alignment, .center)
        XCTAssertEqual(h.cell("target", H.fade)?.label.textColor, .labelColor)
        // No target: a question mark, in the colour QLab draws it.
        try assertShows(stop, "?")
        XCTAssertEqual(h.cell("target", stop)?.label.textColor, .systemYellow)
        // A target with no number: its name.
        _ = await rig.store.send(command: .setProperty(stop, .target(H.fade)))
        await assertEventually { h.cell("target", stop)?.label.stringValue == "Fade and stop Intro" }
        try assertShows(stop, "Fade and stop Intro")
        XCTAssertEqual(h.cell("target", stop)?.label.textColor, .labelColor)
        // Show Mode changes nothing here.
        h.vc.isEditLocked = true
        try assertShows(H.fade, "1")
    }

    func testTargetCellButtonChoosesAFile() async throws {
        let (rig, empty) = try await targetRig()
        let wc = WorkspaceWindowController(store: rig.store)
        let wav = try tempFiles(["chosen.wav"])[0]
        var picked: URL?
        var asked = 0
        wc.pickAudioFile = {
            asked += 1
            return picked
        }
        show(wc.cueList.view)
        await assertEventually { self.targetCell(wc, empty)?.chooseButton.isHidden == false }
        let fileMedia = rig.store.model?.cue(H.intro)?.raw["kind"]?["media"]?.stringValue
        let doorMedia = rig.store.model?.cue(H.door)?.raw["kind"]?["media"]?.stringValue
        let emptyCell = try XCTUnwrap(targetCell(wc, empty))
        XCTAssertEqual(emptyCell.chooseButton.toolTip, loc("inspector.target.choose"))
        wc.cueList.selectAsUser([H.fade])
        // Cancelling changes nothing.
        try click(emptyCell.chooseButton)
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(rig.store.model?.cue(empty)?.raw["kind"]?["media"]?.stringValue ?? "", "")
        // No file yet: the picked file is set, and the cell still draws only its button.
        picked = wav
        try click(emptyCell.chooseButton)
        await assertEventually {
            rig.store.model?.cue(empty)?.raw["kind"]?["media"]?.stringValue == wav.standardizedFileURL.path
        }
        XCTAssertEqual(asked, 2)
        XCTAssertEqual(wc.cueList.selectedIDs(), [H.fade], "the click leaves the selection")
        XCTAssertEqual(targetCell(wc, empty)?.label.stringValue, "", "no file name")
        let picks = try cellPixels(wc, empty)
        XCTAssertGreaterThan(picks.count(Self.drawn), 8, "the button is drawn")
        XCTAssertEqual(picks.count(Self.drawn), middle(picks).count(Self.drawn), "and nothing else")
        // A cue already holding a file: cancelled, then replaced; only that cue changes.
        let fileCell = try XCTUnwrap(targetCell(wc, H.intro))
        picked = nil
        try click(fileCell.chooseButton)
        XCTAssertEqual(asked, 3)
        XCTAssertEqual(rig.store.model?.cue(H.intro)?.raw["kind"]?["media"]?.stringValue, fileMedia)
        picked = wav
        try click(fileCell.chooseButton)
        await assertEventually {
            rig.store.model?.cue(H.intro)?.raw["kind"]?["media"]?.stringValue == wav.standardizedFileURL.path
        }
        XCTAssertEqual(asked, 4)
        XCTAssertEqual(rig.store.model?.cue(H.door)?.raw["kind"]?["media"]?.stringValue, doorMedia)
        // Show Mode: the column is drawn as in Edit Mode, and a click already on its way asks nothing.
        let onChoose = try XCTUnwrap(targetCell(wc, H.door)?.onChoose)
        let editPixels = try cellPixels(wc, H.door)
        wc.setShowMode(true)
        await settle()
        XCTAssertTrue([empty, H.intro, H.door].allSatisfy { self.targetCell(wc, $0)?.chooseButton.isHidden == false })
        XCTAssertEqual(drawnMask(try cellPixels(wc, H.door)), drawnMask(editPixels), "the same glyph in Show Mode")
        onChoose()
        XCTAssertEqual(asked, 4)
    }

    func testInspectorTargetFieldGlyphs() async throws {
        let (rig, empty) = try await targetRig()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        show(wc.inspector.view)
        wc.cueList.selectAsUser([empty])
        await assertEventually { wc.inspector.cueForEdit?.id == empty }
        XCTAssertFalse(wc.inspector.clearTargetButton.isEnabled, "no target: nothing to clear")
        XCTAssertNotNil(wc.inspector.chooseButton.image)
        wc.cueList.selectAsUser([H.door])
        await assertEventually { wc.inspector.cueForEdit?.id == H.door }
        let clear = wc.inspector.clearTargetButton
        XCTAssertTrue(clear.isEnabled)
        XCTAssertEqual(clear.image?.accessibilityDescription, loc("inspector.target.clear"))
        let drawnClear = try drawnGlyphs(clear, .fieldClear, label: loc("inspector.target.clear"))
        XCTAssertGreaterThan(drawnClear.shown.count(Self.drawn), 8, "the clear glyph is drawn")
        XCTAssertEqual(drawnMask(drawnClear.shown), drawnMask(drawnClear.icon), "from the icon table")
        let choose = try drawnGlyphs(wc.inspector.chooseButton, .fieldChoose, label: loc("inspector.target.choose"))
        XCTAssertGreaterThan(choose.shown.count(Self.drawn), 8, "the choose glyph is drawn")
        XCTAssertEqual(drawnMask(choose.shown), drawnMask(choose.icon), "from the icon table")
        clear.performClick(nil)
        await assertEventually { (rig.store.model?.cue(H.door)?.raw["kind"]?["media"]?.stringValue ?? "").isEmpty }
        await assertEventually { wc.inspector.targetField.stringValue == "" }
        XCTAssertEqual(targetCell(wc, H.door)?.label.stringValue, "", "the list cell holds no file")
        await assertEventually { !wc.inspector.clearTargetButton.isEnabled }
    }

    /// Flow: a Group cue's mode is changed in the inspector, and its row's type column shows that
    /// mode's drawing, read out as the mode's name, for every mode and back to the timeline.
    func testAGroupCuesModeIsChangedItsRowIconChangesToThatModesDrawing() async throws {
        let rig = try await Rig.open()
        let h = try await harness(rig)
        let inspector = InspectorViewController(store: rig.store)
        _ = inspector.view
        let group = H.group
        h.vc.selectAsUser([group])
        inspector.setSelection([group])
        await assertEventually { inspector.cueForEdit?.id == group }
        let tab = try XCTUnwrap(inspector.tabView.tabViewItems.first { $0.identifier as? String == "mode" })
        inspector.tabView.selectTabViewItem(tab)
        func rowIcon() -> NSImageView? { h.cell("type", group)?.images[0] }
        XCTAssertEqual(Icon.drawing(shownBy: rowIcon()?.image), .kindGroup)
        XCTAssertEqual(rowIcon()?.accessibilityLabel(), loc("cue.type.group.timeline"))
        let modes = InspectorViewController.groupModes.dropFirst() + ["timeline"]
        for mode in modes {
            let expected = try XCTUnwrap(CueKindStyle.groupModes[mode], mode)
            let popup = inspector.groupModePopup
            popup.selectItem(at: try XCTUnwrap(InspectorViewController.groupModes.firstIndex(of: mode)))
            _ = popup.sendAction(popup.action, to: popup.target)  // the menu choice, without a tracking menu
            await assertEventually(mode) {
                rig.store.model?.cue(group)?.raw["kind"]?["mode"]?["type"]?.stringValue == mode
            }
            await assertEventually("\(mode): the row's drawing") {
                Icon.drawing(shownBy: rowIcon()?.image) == expected.icon
            }
            XCTAssertEqual(rowIcon()?.accessibilityLabel(), loc(expected.labelKey), mode)
            XCTAssertEqual(rowIcon()?.isHidden, false, mode)
        }
    }

    func testDurationShowsTheDuration() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        await assertEventually { rig.store.mediaDurations[self.ten]?.duration == 10 }
        XCTAssertEqual(h.cell("duration", ten)?.label.stringValue, "00:10.00")
        XCTAssertEqual(h.cell("duration", five)?.label.stringValue, "00:05.00")
        XCTAssertEqual(h.cell("duration", memo)?.label.stringValue, "", "an instantaneous cue has none")
        XCTAssertEqual(try pixels(h, "duration", ten).count(Self.greenish), 0, "and no progress outline")
    }

    /// A cue that loops with no end: one pass before GO and after a stop; while it runs the cell counts
    /// inside the pass and starts again each time round, and the bar follows it.
    func testALoopingCueCountsInsideEachPassInTheDurationCell() async throws {
        let rig = try await timingRig(loopsForever: true)
        let h = try await harness(rig)
        await assertEventually { rig.store.mediaDurations[self.five]?.duration == 5 }
        XCTAssertNil(rig.store.mediaDurations[ten]?.duration, "the engine sends no duration for a forever loop")
        XCTAssertEqual(h.cell("duration", ten)?.label.stringValue, "00:10.00", "before GO")
        XCTAssertEqual(h.cell("duration", five)?.label.stringValue, "00:05.00", "a play-once cue is unchanged")
        try await go(rig)
        try await rig.advance(4)
        await assertEventually { (rig.store.progress[self.ten]?.elapsed ?? 0) >= 3.9 }
        let first = try XCTUnwrap(CueTime.parse(h.cell("duration", ten)?.label.stringValue ?? ""))
        XCTAssertEqual(first, 4, accuracy: 0.1, "four seconds into the first pass")
        let bar = try XCTUnwrap(try pixels(h, "duration", ten).extent(Self.greenish), "the bar is drawn")
        XCTAssertEqual(bar.upperBound, 0.4, accuracy: 0.06)
        try await rig.advance(8)  // into the second pass
        await assertEventually { (rig.store.progress[self.ten]?.elapsed ?? 0) >= 11.9 }
        XCTAssertEqual(rig.store.cueStates[ten]?.state, .running)
        let second = try XCTUnwrap(CueTime.parse(h.cell("duration", ten)?.label.stringValue ?? ""))
        XCTAssertEqual(second, 2, accuracy: 0.1, "it started again: two seconds into the second pass")
        XCTAssertEqual(
            try XCTUnwrap(try pixels(h, "duration", ten).extent(Self.greenish)).upperBound, 0.2, accuracy: 0.06)
        guard case .success = await rig.store.transport(.stop(cue: ten, fadeOut: nil)) else { return XCTFail("stop") }
        try await rig.advance(1)
        await assertEventually { rig.store.cueStates[self.ten]?.state != .running }
        XCTAssertEqual(h.cell("duration", ten)?.label.stringValue, "00:10.00", "after a stop")
    }

    func testRunningCueCountsAndShowsProgressInTheDurationCell() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        await assertEventually { rig.store.mediaDurations[self.ten]?.duration == 10 }
        try await go(rig)
        try await rig.advance(4)
        await assertEventually { (rig.store.progress[self.ten]?.elapsed ?? 0) >= 3.9 }
        let gone = try XCTUnwrap(CueTime.parse(h.cell("duration", ten)?.label.stringValue ?? ""))
        XCTAssertEqual(gone, 4, accuracy: 0.1, "the text counts the time gone")
        let drawn = try pixels(h, "duration", ten)
        let reach = try XCTUnwrap(drawn.extent(Self.greenish), "outlined and filled in green")
        XCTAssertEqual(reach.upperBound, 0.4, accuracy: 0.06, "to about 40 % of the cell")
        XCTAssertEqual(reach.lowerBound, 0, accuracy: 0.03)
        // It ends: the outline is gone and the cell reads the duration again.
        try await rig.advance(7)
        await assertEventually { rig.store.cueStates[self.ten]?.state != .running }
        XCTAssertEqual(try pixels(h, "duration", ten).count(Self.greenish), 0)
        XCTAssertEqual(h.cell("duration", ten)?.label.stringValue, "00:10.00")
    }

    func testPausedCueKeepsItsProgressInYellow() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        await assertEventually { rig.store.mediaDurations[self.ten]?.duration == 10 }
        try await go(rig)
        try await rig.advance(4)
        await assertEventually { (rig.store.progress[self.ten]?.elapsed ?? 0) >= 3.9 }
        guard case .success = await rig.store.transport(.pause(cue: ten)) else { return XCTFail("pause") }
        try await rig.advance(0.05)
        await assertEventually { rig.store.cueStates[self.ten]?.state == .paused }
        let status = try pixels(h, "status", ten)
        XCTAssertGreaterThan(status.count(Self.yellowish), 8, "the paused icon is drawn")
        XCTAssertEqual(try XCTUnwrap(h.cell("status", ten)).images[1].accessibilityLabel(), loc("cue.state.paused"))
        let first = try XCTUnwrap(try pixels(h, "duration", ten).extent(Self.yellowish), "progress drawn in yellow")
        XCTAssertEqual(first.upperBound, 0.4, accuracy: 0.06)
        XCTAssertEqual(try pixels(h, "duration", ten).count(Self.greenish), 0)
        try await rig.advance(2)
        let later = try XCTUnwrap(try pixels(h, "duration", ten).extent(Self.yellowish))
        XCTAssertEqual(later.upperBound, first.upperBound, accuracy: 0.02, "stays where it stopped")
    }
}
