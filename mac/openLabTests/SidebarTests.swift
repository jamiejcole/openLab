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
    static let intro: ModelID = TestSnapshots.intro  // running in `make()`
    static let rain: ModelID = TestSnapshots.rain  // paused in `make()`
    static let door: ModelID = TestSnapshots.door  // loaded in `make()`

    private var rig: Rig!
    private var store: WorkspaceStore { rig.store }
    private let spy = SpyTableView()
    private var side: SidebarViewController!

    /// No engine: the demo show plus a second, empty list "Second", with Intro running, Rain
    /// paused, Door slam loaded and Radio broken.
    private func make() async throws -> WorkspaceWindowController {
        var snap = try TestSnapshots.show()
        var lists = snap["workspace"]?["cue_lists"]?.arrayValue ?? []
        lists.append(.object(["id": .string(Self.second.raw), "name": .string("Second"), "cues": .array([])]))
        snap["workspace"]?.set("cue_lists", .array(lists))
        func state(_ cue: ModelID, _ s: String) -> JSONValue {
            .object(["cue": .string(cue.raw), "state": .string(s)])
        }
        snap.set(
            "cue_states",
            .array([
                state(Self.intro, "running"), state(Self.rain, "paused"), state(Self.door, "loaded"),
                state(TestSnapshots.radio, "broken"),
            ]))
        rig = try Rig.offline(snap)
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

    // MARK: Transport strip

    /// Every transport button is enabled and does something: the window controller gives it an
    /// action, Reset all included (QLab's top row: reset, pause, resume, panic).
    func testEveryTransportButtonIsEnabledWithAnAction() async throws {
        let wc = try await make()
        wc.sidebar.loadViewIfNeeded()
        XCTAssertEqual(wc.sidebar.transportButtons.count, 4)
        for (id, button) in wc.sidebar.transportButtons {
            XCTAssertTrue(button.isEnabled, "\(id) is disabled")
            XCTAssertNotNil(button.action, "\(id) has no action")
            XCTAssertNotNil(button.target, "\(id) has no target")
        }
        let reset = try XCTUnwrap(wc.sidebar.transportButtons[AX.transportReset])
        XCTAssertEqual(reset.toolTip, loc("transport.reset"))
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

    /// CL-01: the states that count as active are pinned, so a new state must pick a side.
    func testWhichCueStatesCountAsActive() {
        let active: Set<WireCueState> = [.running, .paused, .fading, .preWaiting, .postWaiting, .stopping]
        XCTAssertEqual(Set(WireCueState.allCases.filter(\.isActive)), active)
        XCTAssertFalse(WireCueState.idle.isActive || WireCueState.loaded.isActive || WireCueState.broken.isActive)
    }

    /// CL-01: New List asks the engine for an empty list after the last one and shows it once the
    /// engine has accepted; New Cart stays off (CL-05).
    func testNewListAddsAnEmptyListAtTheEndAndShowsIt() async throws {
        let wired = try await Rig.open()
        side = SidebarViewController(store: wired.store, activeTable: spy)
        side.view.frame = NSRect(x: 0, y: 0, width: 280, height: 600)
        XCTAssertTrue(side.newListButton.isEnabled)
        XCTAssertFalse(side.newCartButton.isEnabled)
        var picked: ModelID?
        side.onSelectList = { picked = $0 }
        side.newListButton.performClick(nil)
        await assertEventually { picked != nil }
        // The engine added it after the demo's one list, named in order, empty; the sidebar lists it.
        let lists = try XCTUnwrap(wired.store.model?.cueLists)
        XCTAssertEqual(lists.map(\.name), ["Main", "Cue List 2"])
        XCTAssertEqual(lists.last?.id, picked)
        XCTAssertEqual(lists.last?.cues.count, 0)
        XCTAssertEqual(side.lists.map(\.name), ["Main", "Cue List 2"])
        let sent = wired.wire.commands.filter { $0.json["type"]?.stringValue == "add_cue_list" }
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent.first?.json["index"], .int(1))
    }

    /// The right-click menu on a list row renames and deletes it; Show Mode and an engine refusal
    /// are both handled.
    func testListRowMenuRenamesAndDeletesAndRefusesInShowMode() async throws {
        let wired = try await Rig.open()
        let wc = WorkspaceWindowController(store: wired.store)
        side = SidebarViewController(store: wired.store, activeTable: spy)
        let list = try XCTUnwrap(side.lists.first).id
        let sent = { (type: String) -> Int in
            wired.wire.commands.filter { $0.json["type"]?.stringValue == type }.count
        }
        await side.rename(list: list, to: "Front of House")
        XCTAssertEqual(wired.store.model?.cueLists.first?.name, "Front of House", "the engine renamed it")
        await assertEventually { self.side.lists.first?.name == "Front of House" }
        var notices: [String] = []
        side.onNotice = { notices.append($0) }
        // The list has cues: the engine refuses to remove it, and the operator is told.
        await side.delete(list: list)
        XCTAssertEqual(sent("remove_cue_list"), 1)
        XCTAssertEqual(notices.count, 1, "the engine's refusal is shown")
        XCTAssertEqual(wired.store.model?.cueLists.map(\.id), [list], "the list is still there")
        wc.setShowMode(true)
        await assertEventually { wc.sidebar.listEditLocked }
        side.listEditLocked = true
        await side.rename(list: list, to: "Other")
        await side.delete(list: list)
        XCTAssertEqual(sent("rename_cue_list"), 1, "nothing is sent in Show Mode")
        XCTAssertEqual(sent("remove_cue_list"), 1)
        XCTAssertEqual(wired.store.model?.cueLists.first?.name, "Front of House")
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
        XCTAssertEqual(cell.times.stringValue, "Paused  00:02.50  -00:07.50")  // rain is paused: frozen times
        XCTAssertEqual(cell.bar.doubleValue, 0.25, accuracy: 0.001)
        // Progress for a cue that is not in the panel touches only the row that lost its progress.
        side.clearUpdateLog()
        try receive(#"{"type":"progress","items":[{"cue":"\#(Self.door)","elapsed":1,"remaining":1}]}"#)
        XCTAssertEqual(side.updatedActiveRows, [Self.rain])
    }

    func testPausedAndStoppingCuesKeepTheirProgressAcrossStateChanges() async throws {
        _ = try await make()
        try state(Self.door, "running")
        try receive(#"{"type":"progress","items":[{"cue":"\#(Self.door)","elapsed":1,"remaining":3}]}"#)
        var progressChanges = 0
        let sub = store.changes.sink { if case .progress = $0 { progressChanges += 1 } }
        defer { sub.cancel() }
        try state(Self.door, "paused")
        XCTAssertEqual(store.progress[Self.door]?.elapsed, 1, "paused keeps the frozen progress")
        try state(Self.door, "stopping")
        XCTAssertEqual(store.progress[Self.door]?.elapsed, 1)
        XCTAssertEqual(progressChanges, 0, "no drop and re-add on each state change")
        let cell = ActiveCueCell()
        side.fill(cell, Self.door)
        XCTAssertEqual(cell.times.stringValue, "Stopping  00:01.00  -00:03.00")
        try state(Self.door, "idle")
        XCTAssertNil(store.progress[Self.door])
    }

    func testCueWithoutProgressShowsItsState() async throws {
        _ = try await make()
        let cell = ActiveCueCell()
        side.fill(cell, Self.rain)  // paused, and no progress arrived for it
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

    func testWallClockTimerEndsWhenTheLabelIsGone() throws {
        var timer: Timer?
        weak var gone: WallClockLabel?
        autoreleasepool {
            let window = NSWindow(
                contentRect: .init(x: 0, y: 0, width: 200, height: 50), styleMask: [], backing: .buffered, defer: true)
            window.isReleasedWhenClosed = false
            let label = WallClockLabel()
            window.contentView?.addSubview(label)
            timer = label.timer
            gone = label
            window.close()  // leaves the label attached, as closing a workspace window does
        }
        XCTAssertNil(gone)
        let t = try XCTUnwrap(timer)
        // Either the window's teardown detached the label or the next tick finds it gone.
        if t.isValid { t.fire() }
        XCTAssertFalse(t.isValid)
    }

    func testFooterHasTheClock() async throws {
        let wc = try await make()
        XCTAssertEqual(wc.footer.clock.accessibilityIdentifier(), AX.wallClock)
        XCTAssertFalse(wc.footer.clock.stringValue.isEmpty)
    }
}
