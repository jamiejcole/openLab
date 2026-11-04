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

    func testCellsShowNumberNameTargetAndTimes() async throws {
        let h = try await H()
        XCTAssertEqual(h.cell("number", H.intro)?.label.stringValue, "1")
        XCTAssertEqual(h.cell("name", H.intro)?.label.stringValue, "Intro")
        XCTAssertEqual(h.cell("target", H.intro)?.label.stringValue, "a.wav")
        // The fade cue has no name and no number: its kind stands in, and it points at cue 1.
        XCTAssertEqual(h.cell("name", H.fade)?.label.stringValue, "Fade")
        XCTAssertEqual(h.cell("duration", H.fade)?.label.stringValue, "00:05.00")
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "1 Intro")
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
            id: H.intro.raw, number: "1", name: "Overture", kind: TestSnapshots.audioKind("media/a.wav"))
        var fade = TestSnapshots.cueJSON(
            id: H.fade.raw, number: nil, name: "",
            kind: .object(["type": .string("fade"), "target": .string(H.intro.raw), "duration": .double(5)]))
        fade.set("armed", .bool(true))
        try h.change(upserts: [try H.upsertJSON(fade, index: 1)])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "1 Intro")
        h.spy.resetCounts()
        try h.change(upserts: [try H.upsertJSON(target, index: 0)])
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "1 Overture")
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
    /// was typed) puts the playhead back on it; a click on a row the playhead is on sends nothing.
    func testClickingTheHighlightedRowPutsThePlayheadBackOnIt() async throws {
        let h = try await live()
        h.vc.selectAsUser([H.intro])
        await assertEventually { self.playhead(h) == H.intro }
        h.vc.rowClicked(h.spy.row(forItem: h.vc.node(H.intro)))
        await settle()
        XCTAssertEqual(sentPlayheads(h), [], "the show opens with the playhead on Intro")
        // A playhead the selection did not follow (H1 hold), here set without the list hearing it.
        h.vc.sub = nil
        try await moveElsewhere(h, to: H.group)
        XCTAssertEqual(h.vc.selectedIDs(), [H.intro])
        h.vc.rowClicked(h.spy.row(forItem: h.vc.node(H.intro)))
        await assertEventually { self.playhead(h) == H.intro }
        XCTAssertEqual(sentPlayheads(h), [H.intro])
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
        // The cell keeps reading the cue's duration (the probed 120 s) while it runs.
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00")
        // Next tick: the same row again; when it ends, that row once more.
        h.spy.resetCounts()
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":3.0,"remaining":null}]}"#)
        XCTAssertEqual(h.spy.rowsReloaded, 1)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00")
        h.spy.resetCounts()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"idle"}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00")
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }

    /// M18: a pre-wait or post-wait in progress counts down in its own column (QLab: "counting
    /// down its pre-wait"), touching only those cells; it ends with the run.
    func testWaitsCountDownInTheirColumns() async throws {
        let h = try await H()
        h.spy.resetCounts()
        let waits = #"[{"cue":"\#(H.intro)","wait":"pre_wait","elapsed":0.5,"remaining":1.5}]"#
        try h.receive(#"{"type":"progress","items":[],"waits":\#(waits)}"#)
        let reload = try XCTUnwrap(h.spy.rowReloads.first)
        XCTAssertEqual(
            reload.columns.map { h.spy.tableColumns[$0].identifier.rawValue }, ["preWait", "postWait"])
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:01.50")
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"idle"}"#)
        XCTAssertNil(h.vc.store.waits[H.intro])
        XCTAssertEqual(
            h.cell("preWait", H.intro)?.label.stringValue,
            CueTime.format(h.vc.store.model?.cue(H.intro)?.raw["pre_wait"]?.doubleValue ?? 0))
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
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "2.2 Rain")
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
        for item in menu.items where !item.isSeparatorItem {
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
                "Copy", "Paste", "Delete",
            ])
        XCTAssertTrue(m.items.filter { !$0.isSeparatorItem }.allSatisfy { $0.target === h.vc }, "this window's own")
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
        // A fade: no audio to preview, no file to reveal, nothing running to panic.
        for (id, run) in h.store.cueStates where run.state.isActive {
            try h.runtime(#"{"type":"cue_state","cue":"\#(id)","state":"idle"}"#)
        }
        var e = enabled(try menu(h, H.group, pasteboard: pb))
        XCTAssertEqual(e[.preview], false)
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

    func testTimeHeaderTogglesBetweenWholeTimeAndTimeLeftPerWindow() async throws {
        let h = try await H()
        let other = try await H()
        let columns = h.spy.tableColumns
        let duration = try XCTUnwrap(columns.first { $0.identifier.rawValue == "duration" })
        XCTAssertTrue(duration.headerCell is TimeHeaderCell)
        XCTAssertFalse(try XCTUnwrap(columns.first { $0.identifier.rawValue == "number" }).headerCell is TimeHeaderCell)
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":2.5,"remaining":7.5}]}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00")
        // A header click (the delegate hook) switches Duration to the time left, in place.
        h.vc.outlineView(h.spy, didClick: duration)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "00:07.50")
        XCTAssertEqual((duration.headerCell as? TimeHeaderCell)?.shown, .remaining)
        XCTAssertEqual(duration.headerToolTip, loc("cuelist.time.showFull"))
        // The next tick keeps the choice; an unknown end falls back to the duration.
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":3.0,"remaining":null}]}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "02:00.00")
        h.vc.toggleTimeShown("duration")
        XCTAssertEqual(h.vc.timeShown["duration"], .full)
        // Waits default to counting down; the toggle shows elapsed / total.
        let waits = #"[{"cue":"\#(H.intro)","wait":"pre_wait","elapsed":0.5,"remaining":1.5}]"#
        try h.receive(#"{"type":"progress","items":[],"waits":\#(waits)}"#)
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:01.50")
        h.vc.toggleTimeShown("preWait")
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "00:00.50 / 00:02.00")
        // Another window keeps its own choice; non-time columns ignore the toggle.
        XCTAssertEqual(other.vc.timeShown["preWait"], .remaining)
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
            selectedCount: 1, anyAudioSelected: true, clickedIsPlayhead: false, clickedHasMedia: true,
            anyActive: true, canPaste: true, showMode: true)
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
        a.vc.onCommand = { commandsA.append($0) }
        b.vc.onCommand = { commandsB.append($0) }
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
    private var app: AppController!
    private var engine: TestEngine!
    private let wire = WireTap()
    private var doc: ModelID = "none"
    private var wc: WorkspaceWindowController { app.windows[doc]! }
    private var store: WorkspaceStore { wc.store }
    private var list: ModelID { wc.cueList.listID! }

    override func setUp() async throws {
        engine = try await TestEngine.start()
        let url = engine.infoURL
        let session = EngineSession(
            transport: wire, infoProvider: { EngineConnectionInfo.load(from: url) }, scheduler: ManualScheduler())
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
        app?.closeAllWindows()
        app = nil
        engine?.stop()
    }

    // MARK: Doing what the operator does

    private func key(_ chars: String, code: UInt16, _ flags: NSEvent.ModifierFlags = []) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
                characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code))
    }

    /// The GOs the window sent.
    private func goes() -> Int {
        wire.sent.filter { if case .transport(_, .go) = $0 { return true } else { return false } }.count
    }

    /// Everything the engine sent the window so far is applied.
    private func sync() async { _ = await store.client.send({ .request(id: $0, .listPatchMappings) }) }

    /// `seconds` of device time.
    private func play(_ seconds: Double) async throws {
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

    /// A click on `id`'s row.
    private func click(_ id: ModelID) { wc.cueList.selectAsUser([id]) }

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
        return [cue.number, cue.listName].compactMap { $0 }.joined(separator: " \u{00B7} ")
    }

    /// The highlighted rows, the playhead marker and the standby display are all on `id` (nil:
    /// nothing highlighted, no marker, nothing standing by).
    private func assertAllOn(_ id: ModelID?, file: StaticString = #filePath, line: UInt = #line) async throws {
        await assertEventually("the highlight and the playhead", file: file, line: line) {
            self.wc.cueList.selectedIDs() == (id.map { [$0] } ?? []) && self.store.playheads[self.list] == id
        }
        let all = wc.cueList.roots.flatMap { [$0] + $0.children }.map(\.id)
        XCTAssertEqual(
            all.filter { status($0, 0) == loc("cue.playhead") }, id.map { [$0] } ?? [], file: file, line: line)
        XCTAssertEqual(wc.masthead.standbyField.stringValue, try standbyText(id), file: file, line: line)
    }

    // MARK: #489: the selection follows the playhead

    private var cues: [ModelID] = []

    /// The issue's workspace: Audio cues 1, 2, 3 dropped into a new list, cue 1 clicked.
    private func threeCues() async throws {
        let files = try tempFiles(["1.wav", "2.wav", "3.wav"])
        cues = await wc.editor.addFiles(files, parent: nil, index: 0)
        XCTAssertEqual(cues.count, 3)
        try await play(0)
        click(cues[0])
        try await assertAllOn(cues[0])
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
        XCTAssertTrue(running(cues[0]))
        try await assertAllOn(cues[1])
    }

    func testRepeatedGoWalksTheList() async throws {
        try await threeCues()
        try await space()
        try await space()
        XCTAssertTrue(running(cues[0]) && running(cues[1]))
        try await assertAllOn(cues[2])
    }

    func testGoPanicClickTheFirstCueGo() async throws {
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
        XCTAssertTrue(running(cues[0]))
        try await assertAllOn(cues[1])
    }

    /// Typing in the inspector holds the highlight while the playhead moves; the click on the
    /// highlighted row (which ends the typing) puts the playhead back on it.
    func testClickingTheHighlightedRowResetsThePlayhead() async throws {
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

    /// A click in the status column is a click on the row.
    func testClickingTheStatusColumn() async throws {
        try await threeCues()
        click(cues[2])
        try await assertAllOn(cues[2])
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

    func testMultiSelection() async throws {
        try await threeCues()
        let ov = wc.cueList.outlineView
        ov.selectRowIndexes(IndexSet(integer: ov.row(forItem: wc.cueList.node(cues[2]))), byExtendingSelection: true)
        await assertEventually { self.store.playheads[self.list] == self.cues[2] }
        await settle()
        XCTAssertEqual(wc.cueList.selectedIDs(), [cues[0], cues[2]])
        XCTAssertEqual(status(cues[2], 0), loc("cue.playhead"))
        XCTAssertNil(status(cues[0], 0))
        XCTAssertEqual(wc.masthead.standbyField.stringValue, try standbyText(cues[2]))
    }

    func testGoAtTheEndOfTheList() async throws {
        try await threeCues()
        click(cues[2])
        try await assertAllOn(cues[2])
        try await space()
        XCTAssertTrue(running(cues[2]))
        try await assertAllOn(nil)
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
        XCTAssertEqual(wc.cueList.roots.map(\.id), [group, cue2])
        XCTAssertEqual(wc.cueList.node(group)?.children.map(\.id), [child1, child2])
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
        click(group)
        try await assertAllOn(group)
        try await space()
        XCTAssertTrue(running(child1) && running(child2))
        try await assertAllOn(nil)
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, loc("inspector.empty"), "the inspector shows no cue")
        try await play(2)
        await assertEventually { !self.running(self.child1) && !self.running(self.child2) }
        click(group)
        try await assertAllOn(group)
        try await choose("playlist")
        try await space()
        XCTAssertTrue(running(group) && running(child1), "the group plays in its new mode")
        XCTAssertFalse(running(child2))
        try await assertAllOn(nil)
    }

    /// Timeline (QLab 5 manual p. 138): both children and the group run, cue 2 stands by.
    func testTimelineSpaceRunsBothChildrenAndStandsByCue2() async throws {
        try await groupWorkspace([])
        XCTAssertEqual(mode(group), "timeline", "a new group")
        try await space()
        for id in [group, child1, child2] { XCTAssertTrue(running(id)) }
        XCTAssertFalse(running(cue2))
        try await assertAllOn(cue2)
    }

    /// Playlist (p. 138): 1.1 runs, then 1.2 once it ends; cue 2 stands by.
    func testPlaylistSpaceRunsTheChildrenOneAfterTheOtherAndStandsByCue2() async throws {
        try await groupWorkspace(["playlist"])
        try await space()
        XCTAssertTrue(running(group) && running(child1))
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
        XCTAssertTrue(running(child1))
        XCTAssertFalse(running(child2))
        try await assertAllOn(child2)
        try await space()
        XCTAssertTrue(running(child2))
        try await assertAllOn(cue2)
    }

    /// Start first child and go to the next cue (p. 139): 1.1 runs, 1.2 never starts, cue 2 stands by.
    func testStartFirstChildAndGoToNextSpaceRunsOnly11AndStandsByCue2() async throws {
        try await groupWorkspace(["start_first_and_go_to_next"])
        try await space()
        XCTAssertTrue(running(child1))
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
        XCTAssertTrue(running(child1) && running(child2))
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
    private func timingRig() async throws -> Rig {
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
                    kind.set("last_slice_play_count", .int(1))  // the Demo loops until devamped: no duration
                    one.set("kind", kind)
                    var two = one
                    two.set("id", .string(five.raw))
                    two.set("number", .string("2"))
                    two.set("name", .string("Two"))
                    kind.set("media", .string("media/five.wav"))
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
        let row = h.spy.row(forItem: h.vc.node(id))
        let col = h.spy.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        XCTAssertGreaterThanOrEqual(row, 0)
        let rowView = try XCTUnwrap(h.spy.rowView(atRow: row, makeIfNecessary: true))
        rowView.layoutSubtreeIfNeeded()
        let cell = h.spy.frameOfCell(atColumn: col, row: row)
        let rowFrame = h.spy.rect(ofRow: row)
        let region = NSRect(x: cell.minX - rowFrame.minX, y: 0, width: cell.width, height: rowFrame.height)
        rowView.layoutSubtreeIfNeeded()
        let scale = max(1, rowView.window?.backingScaleFactor ?? 2)
        let rep = try XCTUnwrap(
            NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: Int(rowView.bounds.width * scale),
                pixelsHigh: Int(rowView.bounds.height * scale), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: rep))
        context.cgContext.setFillColor(NSColor.white.cgColor)  // the window's light background under a clear row
        context.cgContext.fill(CGRect(x: 0, y: 0, width: rep.pixelsWide, height: rep.pixelsHigh))
        context.cgContext.scaleBy(x: scale, y: scale)
        // Draws the view tree straight into the bitmap (cacheDisplay skips layer-backed subviews).
        rowView.displayIgnoringOpacity(rowView.bounds, in: context)
        return Pixels(rep: rep, region: region, scale: scale)
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
    private static let playheadGrey: (CGFloat, CGFloat, CGFloat) -> Bool = { r, g, b in
        abs(r - 0.62) < 0.04 && abs(g - 0.62) < 0.04 && abs(b - 0.62) < 0.04
    }

    private func go(_ rig: Rig) async throws {
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance()
    }

    func testPlayheadMarkerDistinctFromRunning() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == self.ten }
        let one = try pixels(h, "status", ten)
        XCTAssertGreaterThan(one.count(Self.playheadGrey), 8, "cue 1 shows the light grey pointer")
        XCTAssertEqual(one.count(Self.green), 0, "and no green triangle")
        XCTAssertEqual(try pixels(h, "status", five).count(Self.playheadGrey), 0, "no other row shows a pointer")
        XCTAssertEqual(try pixels(h, "status", memo).count(Self.playheadGrey), 0)
        // At the far left edge of the row.
        XCTAssertLessThan(try XCTUnwrap(one.extent(Self.playheadGrey)).lowerBound, 0.15)
    }

    func testRunningAndStandingByAtOnce() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        try await go(rig)
        await assertEventually { rig.store.cueStates[self.ten]?.state == .running }
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == self.five }
        let one = try pixels(h, "status", ten)
        XCTAssertGreaterThan(one.count(Self.green), 8, "cue 1 shows a green running triangle")
        XCTAssertEqual(one.count(Self.playheadGrey), 0)
        let two = try pixels(h, "status", five)
        XCTAssertGreaterThan(two.count(Self.playheadGrey), 8, "cue 2 shows the grey pointer")
        XCTAssertEqual(two.count(Self.green), 0)
        // The two read apart by colour and position (the icons' shapes are alike).
        let running = try XCTUnwrap(h.cell("status", ten)).images[1]
        let pointer = try XCTUnwrap(h.cell("status", five)).images[0]
        XCTAssertNotEqual(Icon.stateRunning.symbol, Icon.playhead.symbol)
        XCTAssertEqual(running.accessibilityLabel(), loc("cue.state.running"))
        XCTAssertEqual(pointer.accessibilityLabel(), loc("cue.playhead"))
    }

    func testHighlightedRowsKeepIconColours() async throws {
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
        XCTAssertGreaterThan(try pixels(h, "status", ten).count(Self.green), 8, "running triangle stays green")
        let two = try pixels(h, "status", five)
        XCTAssertGreaterThan(two.count(Self.playheadGrey), 8, "pointer stays grey")
        XCTAssertEqual(two.count { r, g, b in r > 0.97 && g > 0.97 && b > 0.97 }, 0, "and nothing turns white")
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

    func testGroupModeChangesTheTypeIcon() async throws {
        let rig = try await Rig.open()
        let h = try await harness(rig)
        let inspector = InspectorViewController(store: rig.store)
        _ = inspector.view
        let group = H.group
        h.vc.selectAsUser([group])
        inspector.setSelection([group])
        await assertEventually { inspector.cueForEdit?.id == group }
        let before = try pixels(h, "type", group)
        let beforeLabel = try XCTUnwrap(h.cell("type", group)).images[0].accessibilityLabel()
        XCTAssertEqual(beforeLabel, loc("cue.type.group.timeline"))
        let tab = try XCTUnwrap(inspector.tabView.tabViewItems.first { $0.identifier as? String == "mode" })
        inspector.tabView.selectTabViewItem(tab)
        let popup = inspector.groupModePopup
        popup.selectItem(at: InspectorViewController.groupModes.firstIndex(of: "playlist") ?? 1)
        _ = popup.sendAction(popup.action, to: popup.target)  // the menu choice, without a tracking menu
        await assertEventually {
            rig.store.model?.cue(group)?.raw["kind"]?["mode"]?["type"]?.stringValue == "playlist"
        }
        await assertEventually {
            self.window?.layoutIfNeeded()
            return (try? self.pixels(h, "type", group))?.rep.tiffRepresentation != before.rep.tiffRepresentation
        }
        XCTAssertEqual(
            try XCTUnwrap(h.cell("type", group)).images[0].accessibilityLabel(), loc("cue.type.group.playlist"))
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

    func testRunningCueShowsProgressInTheDurationCell() async throws {
        let rig = try await timingRig()
        let h = try await harness(rig)
        await assertEventually { rig.store.mediaDurations[self.ten]?.duration == 10 }
        try await go(rig)
        try await rig.advance(4)
        await assertEventually { (rig.store.progress[self.ten]?.elapsed ?? 0) >= 3.9 }
        XCTAssertEqual(h.cell("duration", ten)?.label.stringValue, "00:10.00", "still reads the duration")
        let drawn = try pixels(h, "duration", ten)
        let reach = try XCTUnwrap(drawn.extent(Self.greenish), "outlined and filled in green")
        XCTAssertEqual(reach.upperBound, 0.4, accuracy: 0.06, "to about 40 % of the cell")
        XCTAssertEqual(reach.lowerBound, 0, accuracy: 0.03)
        // It ends: the outline is gone.
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
