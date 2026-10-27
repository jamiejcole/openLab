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

    static let list = FakeEngine.demoListID
    static let group: ModelID = "00000000-0000-0000-0000-000000000020"
    static let door: ModelID = "00000000-0000-0000-0000-000000000021"
    static let rain: ModelID = "00000000-0000-0000-0000-000000000022"
    static let radio: ModelID = "00000000-0000-0000-0000-000000000023"
    static let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
    static let fade: ModelID = "00000000-0000-0000-0000-00000000000b"

    /// The list shows `snapshot`, connected through the fake engine; the table has a size so rows exist.
    init(snapshot: JSONValue? = nil) async throws {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        rig = try Rig(snapshot: snapshot ?? FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        vc = CueListViewController(store: rig.store, outlineView: spy)
        vc.view.frame = NSRect(x: 0, y: 0, width: 1000, height: 600)
        vc.view.layoutSubtreeIfNeeded()
        spy.resetCounts()
    }

    func receive(_ json: String) throws {
        XCTAssertEqual(store.receive(try ServerMessage.decode(json)), .applied)
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
        _ id: String, _ name: String, kind: JSONValue = FakeEngine.audioKind("media/x.wav"), color: String = "none"
    )
        -> JSONValue
    {
        FakeEngine.cueJSON(id: id, number: nil, name: name, kind: kind, color: color)
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
            ["status", "number", "name", "target", "preWait", "duration", "postWait", "continue"])
    }

    func testCellsShowNumberNameTargetAndTimes() async throws {
        let h = try await H()
        XCTAssertEqual(h.cell("number", H.intro)?.label.stringValue, "1")
        XCTAssertEqual(h.cell("name", H.intro)?.label.stringValue, "Intro")
        XCTAssertEqual(h.cell("target", H.intro)?.label.stringValue, "a.wav")
        // The fade cue has no name and no number: its kind stands in, and it points at cue 1.
        XCTAssertEqual(h.cell("name", H.fade)?.label.stringValue, "Fade")
        XCTAssertEqual(h.cell("duration", H.fade)?.label.stringValue, "0:05.00")
        XCTAssertEqual(h.cell("target", H.fade)?.label.stringValue, "1 Intro")
        XCTAssertEqual(h.cell("preWait", H.intro)?.label.stringValue, "")
        XCTAssertEqual(h.cell("continue", H.intro)?.images[0].isHidden, true)
    }

    func testTimeFormat() {
        XCTAssertEqual(CueTimeFormat.clock(0), "0:00.00")
        XCTAssertEqual(CueTimeFormat.clock(65.254), "1:05.25")
        XCTAssertEqual(CueTimeFormat.clock(3661.5), "1:01:01.50")
        XCTAssertEqual(CueTimeFormat.clock(.nan), "")
        XCTAssertEqual(CueTimeFormat.clock(-1), "")
        XCTAssertEqual(CueTimeFormat.wait(0), "")
        XCTAssertEqual(CueTimeFormat.progress(elapsed: 2, remaining: 3), "0:02.00 / 0:05.00")
        XCTAssertEqual(CueTimeFormat.progress(elapsed: 2, remaining: nil), "0:02.00")
    }

    func testColourFlagAndContinueAreShownWithShapesAndLabels() async throws {
        let h = try await H()
        let h2 = h.vc
        h.spy.expandItem(try XCTUnwrap(h2.node(H.group)))
        let name = try XCTUnwrap(h.cell("name", H.rain))
        XCTAssertFalse(name.images[1].isHidden)
        XCTAssertEqual(name.images[1].accessibilityLabel(), loc("cue.colour.blue"))
        XCTAssertFalse(name.images[2].isHidden)
        XCTAssertEqual(name.images[2].accessibilityLabel(), loc("cue.flagged"))
        let cont = try XCTUnwrap(h.cell("continue", H.rain))
        XCTAssertEqual(cont.images[0].accessibilityLabel(), loc("cue.continue.autoFollow"))
        // Row tint is the colour; a cue with no colour has none.
        let row = try XCTUnwrap(
            h.spy.rowView(atRow: h.spy.row(forItem: h2.node(H.rain)), makeIfNecessary: true) as? CueRowView)
        XCTAssertEqual(row.tint, NSColor.systemBlue)
        let plain = try XCTUnwrap(h.spy.rowView(atRow: 0, makeIfNecessary: true) as? CueRowView)
        XCTAssertNil(plain.tint)
        // Every colour has its own shape (UI-04).
        let shapes = CueKindStyle.colours.values.map(\.symbol)
        XCTAssertEqual(Set(shapes).count, shapes.count)
    }

    func testDisarmedCueIsDimmed() async throws {
        let h = try await H()
        var cue = FakeEngine.cueJSON(
            id: H.intro.raw, number: "1", name: "Intro", kind: FakeEngine.audioKind("media/a.wav"))
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
        let renamed = FakeEngine.cueJSON(
            id: H.intro.raw, number: "1", name: "Overture", kind: FakeEngine.audioKind("media/a.wav"))
        try h.change(upserts: [try H.upsertJSON(renamed, index: 0)])
        XCTAssertEqual(h.cell("name", H.intro)?.label.stringValue, "Overture")
        XCTAssertEqual(h.spy.counts + [h.spy.itemReloads], [0, 0, 0, 0, 1])
    }

    func testMoveWithinTheListIsOneMoveAndKeepsOrderEqualToTheStore() async throws {
        let h = try await H()
        // The engine sends a group with its whole subtree; use the store's own copy for the children.
        let current = try XCTUnwrap(h.store.model?.cue(H.group))
        XCTAssertEqual(current.children.count, 3)
        var json = FakeEngine.cueJSON(
            id: H.group.raw, number: "2", name: "Act One",
            kind: FakeEngine.groupKind(
                current.children.map { c in
                    FakeEngine.cueJSON(
                        id: c.id.raw, number: c.number, name: c.name, kind: FakeEngine.audioKind("media/z.wav"))
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
        let intro = FakeEngine.cueJSON(
            id: H.intro.raw, number: "1", name: "Intro", kind: FakeEngine.audioKind("media/a.wav"))
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
            FakeEngine.cueJSON(id: c.id.raw, number: c.number, name: c.name, kind: FakeEngine.audioKind("media/z.wav"))
        }
        kids.insert(h.cue(H.fade.raw, "Fade moved"), at: 0)
        let group = FakeEngine.cueJSON(id: H.group.raw, number: "2", name: "Act One", kind: FakeEngine.groupKind(kids))
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
            FakeEngine.cueJSON(
                id: H.door.raw, number: "2.1", name: "Door slam", kind: FakeEngine.audioKind("media/d.wav")),
            FakeEngine.cueJSON(id: H.rain.raw, number: "2.2", name: "Storm", kind: FakeEngine.audioKind("media/r.wav")),
        ]
        let json = FakeEngine.cueJSON(id: H.group.raw, number: "2", name: "Act One", kind: FakeEngine.groupKind(kids))
        try h.change(upserts: [try H.upsertJSON(json, index: 2)])
        XCTAssertTrue(h.spy.isItemExpanded(group))
        XCTAssertEqual(h.visibleIDs(), [H.intro, H.fade, H.group, H.door, H.rain])
        XCTAssertNil(h.vc.node(H.radio))
        XCTAssertEqual(h.cell("name", H.rain)?.label.stringValue, "Storm")
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }

    func testACueThatStopsBeingAGroupLosesItsChildren() async throws {
        let h = try await H()
        let memo = FakeEngine.cueJSON(
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
        let target = FakeEngine.cueJSON(
            id: H.intro.raw, number: "1", name: "Overture", kind: FakeEngine.audioKind("media/a.wav"))
        var fade = FakeEngine.cueJSON(
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
        let snap = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        var next = FakeEngine.demoSnapshot(base: snap)
        next.set("revision", .int(900))
        let message = try ServerMessage.decode(
            try JSONValue.object(["type": .string("snapshot"), "snapshot": next]).text())
        h.store.receive(message)
        XCTAssertEqual(h.spy.wholeReloads, 1)
        XCTAssertEqual(Set(h.vc.selectedIDs()), [H.fade, H.rain])
        XCTAssertTrue(h.spy.isItemExpanded(h.vc.node(H.group)))
    }

    func testListShowsTheStoreThatConnectsLater() async throws {
        let rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: try Fixtures.json("server", "welcome")["snapshot"]))
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
        let moved = FakeEngine.cueJSON(id: H.fade.raw, number: nil, name: "", kind: .object(["type": .string("memo")]))
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
                XCTAssertNil(style.symbol)
                continue
            }
            let symbol = try? XCTUnwrap(style.symbol, "\(state)")
            XCTAssertNotNil(symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: nil) }, "\(state)")
            XCTAssertTrue(symbols.insert(symbol ?? "").inserted, "\(state) shares a symbol")
        }
        XCTAssertEqual(
            Set(WireCueState.allCases.map { CueStateStyle.style(for: $0).label }).count, WireCueState.allCases.count)
    }

    func testEveryBrokenReasonHasAString() {
        for reason in WireBrokenReason.allCases {
            let key = CueStateStyle.brokenReasonKey(reason)
            XCTAssertNotEqual(loc(key), key, "\(reason)")
        }
        XCTAssertEqual(
            CueStateStyle.brokenDetail(reason: .deviceLost, problems: [], groupWarning: true),
            loc("cue.broken.deviceLost"))
        XCTAssertEqual(
            CueStateStyle.brokenDetail(reason: nil, problems: [], groupWarning: true), loc("cue.broken.group"))
        XCTAssertNil(CueStateStyle.brokenDetail(reason: nil, problems: [], groupWarning: false))
    }

    func testProblemTextIsLocalisedOrShownRaw() {
        for t in [
            "no_target", "target_missing", "target_self", "target_wrong_kind", "no_media", "media_missing",
            "media_unsupported", "patch_missing", "device_unavailable", "network_patch_missing", "started_broken",
        ] {
            XCTAssertNotEqual(CueStateStyle.problemText(t), t, t)
        }
        XCTAssertEqual(CueStateStyle.problemText("brand_new_problem"), "brand_new_problem")
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
        XCTAssertEqual(try icon(H.radio).toolTip, "\(loc("cue.state.broken")): \(loc("cue.broken.deviceLost"))")
        // Static problems break a cue even without a state entry (the fixture's fade cue).
        XCTAssertEqual(try icon(H.fade).accessibilityLabel(), loc("cue.state.broken"))
        XCTAssertTrue(try XCTUnwrap(try icon(H.fade).toolTip).contains(loc("problem.target_missing")))
        XCTAssertTrue(try XCTUnwrap(h.cell("status", H.group)).images[1].isHidden, "idle shows no icon")
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

    // MARK: Progress

    func testProgressTouchesOnlyTheRunningRowsDurationCell() async throws {
        let h = try await H()
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":2.5,"remaining":7.5}]}"#)
        XCTAssertEqual(h.spy.rowReloads.count, 1)
        let reload = try XCTUnwrap(h.spy.rowReloads.first)
        XCTAssertEqual(reload.rows, IndexSet(integer: 0))
        XCTAssertEqual(reload.columns.map { h.spy.tableColumns[$0].identifier.rawValue }, ["duration"])
        XCTAssertEqual(h.spy.wholeReloads, 0)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "0:02.50 / 0:10.00")
        // Next tick: the same row again; when it ends, that row once more, back to its own duration.
        h.spy.resetCounts()
        try h.receive(#"{"type":"progress","items":[{"cue":"\#(H.intro)","elapsed":3.0,"remaining":null}]}"#)
        XCTAssertEqual(h.spy.rowsReloaded, 1)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "0:03.00")
        h.spy.resetCounts()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"idle"}"#)
        XCTAssertEqual(h.cell("duration", H.intro)?.label.stringValue, "")
        XCTAssertEqual(h.spy.wholeReloads, 0)
    }
}
