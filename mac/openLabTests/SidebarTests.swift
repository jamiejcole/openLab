// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Counts what the Active Cues panel asks of its table.
final class SpyTableView: NSTableView {
    var wholeReloads = 0
    var inserts = 0
    var removals = 0
    override func reloadData() {
        wholeReloads += 1
        super.reloadData()
    }
    override func insertRows(at indexes: IndexSet, withAnimation: NSTableView.AnimationOptions = []) {
        inserts += indexes.count
        super.insertRows(at: indexes, withAnimation: withAnimation)
    }
    override func removeRows(at indexes: IndexSet, withAnimation: NSTableView.AnimationOptions = []) {
        removals += indexes.count
        super.removeRows(at: indexes, withAnimation: withAnimation)
    }
}

@MainActor
final class SidebarTests: XCTestCase {
    static let main: ModelID = "00000000-0000-0000-0000-000000000001"
    static let second: ModelID = "00000000-0000-0000-0000-000000000002"
    static let intro: ModelID = CueListHarness.intro  // running in the fixture
    static let rain: ModelID = CueListHarness.rain  // paused in the demo show
    static let door: ModelID = CueListHarness.door  // loaded in the demo show

    private var rig: Rig!
    private var store: WorkspaceStore { rig.store }
    private let spy = SpyTableView()
    private var side: SidebarViewController!

    /// The demo show plus a second, empty list "Second".
    private func make() async throws -> WorkspaceWindowController {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        var snap = FakeEngine.demoSnapshot(base: base)
        var lists = snap["workspace"]?["cue_lists"]?.arrayValue ?? []
        lists.append(.object(["id": .string(Self.second.raw), "name": .string("Second"), "cues": .array([])]))
        snap["workspace"]?.set("cue_lists", .array(lists))
        rig = try Rig(snapshot: snap)
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let wc = WorkspaceWindowController(store: store)
        // The sidebar the tests watch: a second one around a spy table.
        side = SidebarViewController(store: store, activeTable: spy)
        side.view.frame = NSRect(x: 0, y: 0, width: 280, height: 600)
        return wc
    }

    private func receive(_ json: String) throws {
        XCTAssertEqual(store.receive(try ServerMessage.decode(json)), .applied)
    }

    private func runtime(_ changes: String) throws {
        try receive(#"{"type":"runtime","seq":\#(store.runtimeSeq + 1),"changes":[\#(changes)]}"#)
    }

    private func state(_ cue: ModelID, _ state: String) throws {
        try runtime(#"{"type":"cue_state","cue":"\#(cue)","state":"\#(state)"}"#)
    }

    private func model(upserts: String = "", deletes: String = "") throws {
        try receive(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[\#(upserts)],"deletes":[\#(deletes)]}"#
        )
    }

    private func pick(_ wc: WorkspaceWindowController, row: Int) {
        wc.sidebar.listTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
    }

    // MARK: Cue lists

    func testListsShowEveryListAndTheShownOneIsSelected() async throws {
        let wc = try await make()
        XCTAssertEqual(wc.sidebar.lists.map(\.name), ["Main", "Second"])
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 0)
        XCTAssertEqual(wc.cueList.listID, Self.main)
    }

    func testSelectingAListSwitchesTheOutlineAndTheListGoAddresses() async throws {
        let wc = try await make()
        XCTAssertGreaterThan(wc.cueList.nodeCount, 0)
        pick(wc, row: 1)
        XCTAssertEqual(wc.cueList.listID, Self.second)
        XCTAssertEqual(wc.cueList.nodeCount, 0)
        pick(wc, row: 0)
        XCTAssertEqual(wc.cueList.listID, Self.main)
        XCTAssertGreaterThan(wc.cueList.nodeCount, 0)
    }

    func testEachWindowHasItsOwnSelectedList() async throws {
        let a = try await make()
        let b = WorkspaceWindowController(store: store)
        pick(a, row: 1)
        XCTAssertEqual(a.cueList.listID, Self.second)
        XCTAssertEqual(b.cueList.listID, Self.main)
        XCTAssertEqual(b.sidebar.listTable.selectedRow, 0)
    }

    func testListUpsertRenamesWithoutSwitchingAndAddsNewLists() async throws {
        let wc = try await make()
        pick(wc, row: 1)
        try model(upserts: #"{"type":"cue_list","id":"\#(Self.second)","name":"Renamed","index":1}"#)
        XCTAssertEqual(wc.sidebar.lists.map(\.name), ["Main", "Renamed"])
        XCTAssertEqual(wc.cueList.listID, Self.second)
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 1)
        // A new list in front moves the row, not the view.
        try model(upserts: #"{"type":"cue_list","id":"new","name":"Front","index":0}"#)
        XCTAssertEqual(wc.sidebar.lists.map(\.name), ["Front", "Main", "Renamed"])
        XCTAssertEqual(wc.cueList.listID, Self.second)
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 2)
    }

    func testRemovingTheShownListFallsBackToTheFirst() async throws {
        let wc = try await make()
        pick(wc, row: 1)
        try model(deletes: #"{"type":"cue_list","id":"\#(Self.second)"}"#)
        XCTAssertEqual(wc.sidebar.lists.map(\.name), ["Main"])
        XCTAssertEqual(wc.cueList.listID, Self.main)
        XCTAssertGreaterThan(wc.cueList.nodeCount, 0)
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 0)
        XCTAssertEqual(wc.sidebar.shownList, Self.main)
    }

    func testRemovingAnotherListKeepsTheShownOne() async throws {
        let wc = try await make()
        try model(deletes: #"{"type":"cue_list","id":"\#(Self.second)"}"#)
        XCTAssertEqual(wc.cueList.listID, Self.main)
        XCTAssertGreaterThan(wc.cueList.nodeCount, 0)
    }

    func testRemovingEveryListEmptiesTheOutline() async throws {
        let wc = try await make()
        try model(deletes: #"{"type":"cue_list","id":"\#(Self.main)"},{"type":"cue_list","id":"\#(Self.second)"}"#)
        XCTAssertNil(wc.cueList.listID)
        XCTAssertEqual(wc.cueList.nodeCount, 0)
        XCTAssertTrue(wc.sidebar.lists.isEmpty)
    }

    func testShowListIgnoresAnUnknownList() async throws {
        let wc = try await make()
        wc.cueList.showList("nope")
        XCTAssertEqual(wc.cueList.listID, Self.main)
    }

    // MARK: Active cues

    func testActiveCuesListRunningAndPausedCuesOnly() async throws {
        _ = try await make()
        XCTAssertEqual(side.activeCues, [Self.intro, Self.rain])  // loaded and broken cues are not active
    }

    func testCuesAppearAndDisappearWithRuntimeChangesWithoutAFullReload() async throws {
        _ = try await make()
        _ = side.activeTable.numberOfRows
        let reloads = spy.wholeReloads
        try state(Self.door, "running")
        XCTAssertEqual(side.activeCues, [Self.intro, Self.rain, Self.door])  // in the order they started
        XCTAssertEqual(spy.inserts, 1)
        try state(Self.intro, "idle")
        XCTAssertEqual(side.activeCues, [Self.rain, Self.door])
        XCTAssertEqual(spy.removals, 1)
        try state(Self.rain, "post_waiting")  // still active: redrawn, not removed
        XCTAssertEqual(side.activeCues, [Self.rain, Self.door])
        XCTAssertEqual(spy.wholeReloads, reloads)
        try state(Self.door, "broken")
        XCTAssertEqual(side.activeCues, [Self.rain])
    }

    func testProgressUpdatesTouchOnlyTheirRows() async throws {
        _ = try await make()
        side.clearUpdateLog()
        try receive(#"{"type":"progress","items":[{"cue":"\#(Self.rain)","elapsed":2.5,"remaining":7.5}]}"#)
        XCTAssertEqual(side.updatedActiveRows, [Self.rain])
        XCTAssertEqual(spy.inserts + spy.removals, 0)
        let cell = ActiveCueCell()
        side.fill(cell, Self.rain)
        XCTAssertEqual(cell.times.stringValue, "00:02.50  -00:07.50")
        XCTAssertEqual(cell.bar.doubleValue, 0.25, accuracy: 0.001)
        // Progress for a cue that is not in the panel touches only the row that lost its progress.
        side.clearUpdateLog()
        try receive(#"{"type":"progress","items":[{"cue":"\#(Self.door)","elapsed":1,"remaining":1}]}"#)
        XCTAssertEqual(side.updatedActiveRows, [Self.rain])
    }

    func testCueWithoutProgressShowsItsState() async throws {
        _ = try await make()
        let cell = ActiveCueCell()
        side.fill(cell, Self.rain)  // paused: the engine reports no progress for it
        XCTAssertEqual(cell.times.stringValue, "Paused")
        XCTAssertEqual(cell.pauseButton.accessibilityLabel(), "Resume")
        side.fill(cell, Self.intro)
        XCTAssertEqual(cell.pauseButton.accessibilityLabel(), "Pause")
    }

    func testDeletingAnActiveCueRemovesItsRow() async throws {
        _ = try await make()
        try model(deletes: #"{"type":"cue","id":"\#(Self.rain)"}"#)
        XCTAssertEqual(side.activeCues, [Self.intro])
    }

    func testSnapshotRebuildsThePanel() async throws {
        _ = try await make()
        try state(Self.door, "running")
        XCTAssertEqual(side.activeCues.count, 3)
        let snap = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let json = try JSONValue.object(["type": .string("snapshot"), "snapshot": snap]).text()
        XCTAssertEqual(store.receive(try ServerMessage.decode(json)), .applied)
        XCTAssertEqual(side.activeCues, [Self.intro])
    }

    func testPerCueControlsReportTheRightActions() async throws {
        _ = try await make()
        var actions: [TransportAction] = []
        side.onCueAction = { actions.append($0) }
        func cell(_ row: Int) throws -> ActiveCueCell {
            try XCTUnwrap(side.activeTable.view(atColumn: 0, row: row, makeIfNecessary: true) as? ActiveCueCell)
        }
        try cell(0).pauseButton.performClick(nil)  // intro runs: pause
        try cell(1).pauseButton.performClick(nil)  // rain is paused: resume
        try cell(1).panicButton.performClick(nil)
        XCTAssertEqual(actions, [.pause(cue: Self.intro), .resume(cue: Self.rain), .panicCue(cue: Self.rain)])
    }

    func testWindowForwardsCueActionsToItsOwner() async throws {
        let wc = try await make()
        var actions: [TransportAction] = []
        wc.onCueAction = { actions.append($0) }
        let cell = try XCTUnwrap(
            wc.sidebar.activeTable.view(atColumn: 0, row: 0, makeIfNecessary: true) as? ActiveCueCell)
        cell.panicButton.performClick(nil)
        XCTAssertEqual(actions, [.panicCue(cue: Self.intro)])
    }

    // MARK: Wall clock

    func testWallClockFormatsTheInjectedTime() throws {
        let utc = try XCTUnwrap(TimeZone(identifier: "UTC"))
        let label = WallClockLabel()
        label.dateFormatter = WallClockLabel.makeFormatter(locale: Locale(identifier: "en_GB"), timeZone: utc)
        label.now = { Date(timeIntervalSince1970: 14 * 3600 + 3 * 60 + 9) }
        label.refresh()
        XCTAssertEqual(label.stringValue, "14:03:09")
        label.now = { Date(timeIntervalSince1970: 14 * 3600 + 3 * 60 + 10) }
        label.refresh()
        XCTAssertEqual(label.stringValue, "14:03:10")
        label.dateFormatter = WallClockLabel.makeFormatter(locale: Locale(identifier: "en_US"), timeZone: utc)
        label.refresh()
        XCTAssertEqual(label.stringValue.replacingOccurrences(of: "\u{202f}", with: " "), "2:03:10 PM")
    }

    func testFooterHasTheClock() async throws {
        let wc = try await make()
        XCTAssertEqual(wc.footer.clock.accessibilityIdentifier(), AX.wallClock)
        XCTAssertFalse(wc.footer.clock.stringValue.isEmpty)
    }
}
