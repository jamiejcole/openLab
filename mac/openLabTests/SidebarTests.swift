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

    /// Flow: sidebar shown → two tabs only (cue lists and active cues); no Warnings tab. The footer's
    /// warnings button leads to the Status window's Warnings page instead.
    func testTheSidebarHasTwoTabsAndTheFootersWarningsButtonOpensStatus() async throws {
        let wc = try await make()
        wc.sidebar.loadViewIfNeeded()
        XCTAssertEqual(wc.sidebar.tabs.segmentCount, 2)
        let labels = (0..<2).map { wc.sidebar.tabs.label(forSegment: $0) }
        XCTAssertTrue(labels[0].contains("Cue List"), labels[0])
        XCTAssertTrue(labels[1].contains("Active Cue"), labels[1])
        for tab in 0..<2 {
            wc.sidebar.showTab(tab)
            XCTAssertFalse((labels[tab]).localizedCaseInsensitiveContains("warning"))
        }
        wc.footer.showWarnings(count: 2)
        wc.footer.warningsButton.performClick(nil)
        XCTAssertEqual(wc.existingStatusWindow?.page, .warnings)
        XCTAssertEqual(wc.sidebar.tabs.selectedSegment, 1, "the sidebar did not change tab")
        wc.existingStatusWindow?.close()
    }

    /// Each transport icon is drawn inside its button's shape with a margin, at the default and the
    /// minimum sidebar width: the glyph at the size the button sets, against the cell's content
    /// area.
    func testTransportIconsFitInsideTheirButtons() async throws {
        let wc = try await make()
        wc.sidebar.loadViewIfNeeded()
        for width in [280, SidebarViewController.minWidth] {
            wc.sidebar.view.frame = NSRect(x: 0, y: 0, width: width, height: 600)
            wc.sidebar.view.layoutSubtreeIfNeeded()
            // The column is laid out at this width (nothing in it holds the sidebar wider).
            XCTAssertEqual(wc.sidebar.tabs.frame.width, width - 20, accuracy: 0.5, "tabs at \(width)")
            for (id, button) in wc.sidebar.transportButtons {
                let cell = try XCTUnwrap(button.cell as? NSButtonCell)
                let content = cell.drawingRect(forBounds: button.bounds)
                let image = try XCTUnwrap(button.image)
                XCTAssertEqual(image.size.width, SidebarViewController.transportIconSize, "\(id)")
                XCTAssertLessThanOrEqual(image.size.height + 4, content.height, "\(id) height at \(width)")
                XCTAssertLessThanOrEqual(image.size.width + 4, content.width, "\(id) width at \(width)")
            }
        }
    }

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

    /// The names the Cue Lists table draws, one per row (not what the controller holds).
    private func drawnListNames(_ side: SidebarViewController) -> [String] {
        side.view.layoutSubtreeIfNeeded()
        return (0..<side.listTable.numberOfRows).map { row in
            let cell = side.listTable.view(atColumn: 0, row: row, makeIfNecessary: true) as? NSTableCellView
            return cell?.textField?.stringValue ?? "(no cell)"
        }
    }

    /// The Cue Lists tab draws a row with each list's name: the one list of a show, then a second added
    /// with New List as its own named row.
    func testEachCueListIsDrawnAsARowWithItsName() async throws {
        let wired = try await Rig.open()
        side = SidebarViewController(store: wired.store, activeTable: spy)
        side.view.frame = NSRect(x: 0, y: 0, width: 280, height: 600)
        XCTAssertEqual(drawnListNames(side), ["Main"])
        side.newListButton.performClick(nil)
        await assertEventually { self.drawnListNames(self.side) == ["Main", "Cue List 2"] }
        let cell = try XCTUnwrap(side.listTable.view(atColumn: 0, row: 1, makeIfNecessary: true) as? NSTableCellView)
        let name = try XCTUnwrap(cell.textField)
        cell.layoutSubtreeIfNeeded()
        XCTAssertTrue(cell.bounds.contains(name.frame), "the name at \(name.frame) is inside its row")
        XCTAssertGreaterThan(name.frame.width, 40)
    }

    /// CL-01: New List asks the engine for an empty list after the last one and shows it once the
    /// engine has accepted; there is no New Cart button.
    func testNewListAddsAnEmptyListAtTheEndAndShowsIt() async throws {
        let wired = try await Rig.open()
        side = SidebarViewController(store: wired.store, activeTable: spy)
        side.view.frame = NSRect(x: 0, y: 0, width: 280, height: 600)
        XCTAssertTrue(side.newListButton.isEnabled)
        func buttons(_ v: NSView) -> [NSButton] { (v as? NSButton).map { [$0] } ?? v.subviews.flatMap(buttons) }
        let carts = buttons(side.view).map(\.title).filter { $0.contains("Cart") }
        XCTAssertEqual(carts, [], "no New Cart button: carts are not built")
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

    /// Show Mode locks every way to add, rename or delete a list: New List (button and its action),
    /// rename and delete refuse, a press adds nothing and the window stays on its list.
    /// Choosing another existing list still works there; back in Edit Mode New List adds again.
    func testInShowModeNoListCanBeAddedRenamedOrDeletedButListsCanStillBeChosen() async throws {
        let wired = try await Rig.open()
        let wc = WorkspaceWindowController(store: wired.store)
        let side = try XCTUnwrap(wc.sidebar)
        side.newListButton.performClick(nil)
        await assertEventually("a second list in Edit Mode") { wired.store.model?.cueLists.count == 2 }
        let lists = try XCTUnwrap(wired.store.model?.cueLists.map(\.id))
        await assertEventually { wc.cueList.listID == lists[1] }
        wc.setShowMode(true)
        await assertEventually { side.listEditLocked }
        XCTAssertFalse(side.newListButton.isEnabled)
        side.newListButton.performClick(nil)
        side.addList()
        try await wired.advance(0.2)
        XCTAssertEqual(wired.store.model?.cueLists.map(\.id), lists, "nothing added")
        XCTAssertEqual(wc.cueList.listID, lists[1], "the window stays on its list")
        XCTAssertEqual(wired.wire.commands.filter { $0.json["type"]?.stringValue == "add_cue_list" }.count, 1)
        // Rename and delete refuse too.
        await side.rename(list: lists[0], to: "Other")
        await side.delete(list: lists[1])
        try await wired.advance(0.2)
        XCTAssertEqual(wired.store.model?.cueLists.map(\.id), lists)
        XCTAssertEqual(wired.store.model?.cueLists.first?.name, "Main")
        // Choosing an existing list is still allowed.
        side.listTable.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        XCTAssertEqual(wc.cueList.listID, lists[0])
        // Back in Edit Mode New List adds again.
        wc.setShowMode(false)
        await assertEventually { !side.listEditLocked }
        XCTAssertTrue(side.newListButton.isEnabled)
        side.newListButton.performClick(nil)
        await assertEventually("a third list") { wired.store.model?.cueLists.count == 3 }
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

    /// An Active Cues tab with nothing playing says so; the note goes when a cue plays and on any other tab.
    func testActiveCuesTabSaysNothingIsPlayingWhenItIsEmpty() async throws {
        _ = try await make()
        side.showTab(1)
        XCTAssertTrue(side.activeEmptyNote.isHidden, "two cues are active: no note")
        try state(Self.intro, "idle")
        try state(Self.rain, "idle")
        XCTAssertEqual(side.activeCues, [])
        XCTAssertFalse(side.activeEmptyNote.isHidden)
        XCTAssertEqual(side.activeEmptyNote.stringValue, loc("sidebar.active.empty"))
        side.showTab(0)
        XCTAssertTrue(side.activeEmptyNote.isHidden, "not over the other tabs")
        side.showTab(1)
        try state(Self.door, "running")
        XCTAssertTrue(side.activeEmptyNote.isHidden)
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

    /// A long name with only a time under it (a loop with no end, a held cue) uses the row's width up to
    /// the buttons; the panic button is at the row's trailing edge, not beside the text.
    func testALongNameWithASingleTimeUsesTheRowsWidth() throws {
        let cell = ActiveCueCell()
        cell.frame = NSRect(x: 0, y: 0, width: 280, height: 56)
        cell.title.stringValue = "1  Thunder rumble in the distance, rolling in from stage left and building"
        cell.times.stringValue = CueTime.running(3.96)
        cell.layoutSubtreeIfNeeded()
        func inCell(_ v: NSView) -> NSRect { v.convert(v.bounds, to: cell) }
        let title = inCell(cell.title)
        let pause = inCell(cell.pauseButton)
        let panic = inCell(cell.panicButton)
        let timesWidth = (cell.times.stringValue as NSString).size(withAttributes: [.font: cell.times.font!]).width
        XCTAssertGreaterThan(title.width, timesWidth + 60, "the name is not cut to the width of the time text")
        XCTAssertGreaterThan(title.width, cell.bounds.width - pause.width - panic.width - 40, "it takes the room")
        XCTAssertGreaterThan(panic.maxX, cell.bounds.width - 14, "the panic button is at the row's edge")
        XCTAssertGreaterThanOrEqual(title.minX + 4, pause.maxX, "the name follows the pause button")
        XCTAssertLessThanOrEqual(title.maxX, panic.minX, "and ends before the panic button")
    }

    /// Flow: an active cue with a long name and its times under it (Intro loops until devamped: the time into its
    /// pass and the time left in it) → in the window's own Active Cues the name uses the row's width up to the
    /// buttons and is not cut to the width of the time text.
    func testAnActiveCueWithALongNameAndItsTimesUsesTheRowsWidthInActiveCues() async throws {
        let long = "Thunder rumble in the distance, rolling in from stage left"
        let wired = try await Rig.open(
            "Demo",
            edit: { folder in
                try editShow(folder.appendingPathComponent("Demo.openlab")) { ws in
                    guard var list = ws["cue_lists"]?.arrayValue?.first, var cues = list["cues"]?.arrayValue,
                        var intro = cues.first
                    else { return }
                    intro.set("name", .string(long))
                    cues[0] = intro
                    list.set("cues", .array(cues))
                    var lists = ws["cue_lists"]?.arrayValue ?? []
                    lists[0] = list
                    ws.set("cue_lists", .array(lists))
                }
            })
        let wc = WorkspaceWindowController(store: wired.store)
        defer { wc.window?.close() }
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(x: 0, y: 0, width: 1100, height: 700), display: true)
        wc.setVisible(.sidebar, true)
        wc.sidebar.tabs.selectedSegment = 1
        wc.sidebar.tabs.performClick(nil)
        guard case .success = await wired.store.transport(.start(cue: Self.intro)) else {
            return XCTFail("start Intro")
        }
        try await wired.advance(0.5)
        await assertEventually { wc.sidebar.activeCues == [Self.intro] && wired.store.progress[Self.intro] != nil }
        window.layoutIfNeeded()
        let cell = try XCTUnwrap(
            wc.sidebar.activeTable.view(atColumn: 0, row: 0, makeIfNecessary: true) as? ActiveCueCell)
        cell.layoutSubtreeIfNeeded()
        XCTAssertTrue(cell.title.stringValue.hasSuffix(long), "the row names Intro: \(cell.title.stringValue)")
        XCTAssertTrue(cell.times.stringValue.contains("-"), "the pass's time left too: \(cell.times.stringValue)")
        func inCell(_ v: NSView) -> NSRect { v.convert(v.bounds, to: cell) }
        let (title, pause, panic) = (inCell(cell.title), inCell(cell.pauseButton), inCell(cell.panicButton))
        let timesWidth = (cell.times.stringValue as NSString).size(withAttributes: [.font: cell.times.font!]).width
        XCTAssertGreaterThan(cell.bounds.width, 200, "the sidebar has its room")
        // The times carry the time left too, so they can be most of a narrow row: the name is wider than
        // them (not cut to their width) and, below, takes the room the buttons leave.
        XCTAssertGreaterThan(title.width, timesWidth, "the name is not cut to the width of the time text")
        XCTAssertGreaterThan(title.width, cell.bounds.width - pause.width - panic.width - 40, "it takes the room")
        XCTAssertGreaterThan(panic.maxX, cell.bounds.width - 14, "the panic button is at the row's edge")
        XCTAssertLessThanOrEqual(title.maxX, panic.minX, "the name ends before the panic button")
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

    /// Flow: Intro loops until devamped; on its third pass the cue list's Duration cell and the real Active
    /// Cues row show the same time into the pass, and the row the time left in it.
    func testALoopingAudioCueOnItsThirdPassShowsTheSameTimeInTheCellAndInActiveCues() async throws {
        let wired = try await Rig.open("Demo")
        let wc = WorkspaceWindowController(store: wired.store)
        defer { wc.window?.close() }
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(x: 0, y: 0, width: 1100, height: 700), display: true)
        wc.setVisible(.sidebar, true)
        wc.sidebar.tabs.selectedSegment = 1
        wc.sidebar.tabs.performClick(nil)
        guard case .success = await wired.store.transport(.start(cue: Self.intro)) else {
            return XCTFail("start Intro")
        }
        try await wired.advance(0.5)
        await assertEventually {
            wired.store.progress[Self.intro] != nil && wired.store.mediaDurations[Self.intro]?.file != nil
        }
        let pass = try XCTUnwrap(wired.store.mediaDurations[Self.intro]?.file)
        try await wired.advance(2 * pass + 1)
        await assertEventually { wc.sidebar.activeCues == [Self.intro] }
        let outline = wc.cueList.outlineView
        let node = try XCTUnwrap(wc.cueList.node(Self.intro))
        let column = outline.column(withIdentifier: NSUserInterfaceItemIdentifier("duration"))
        let cell = try XCTUnwrap(
            outline.view(atColumn: column, row: outline.row(forItem: node), makeIfNecessary: true) as? CueCell)
        let shown = try XCTUnwrap(CueTime.parse(cell.label.stringValue), "the Duration cell: \(cell.label.stringValue)")
        XCTAssertLessThan(shown, pass + 0.01, "the cell counts inside the third pass, not the whole time")
        let row = try XCTUnwrap(
            wc.sidebar.activeTable.view(atColumn: 0, row: 0, makeIfNecessary: true) as? ActiveCueCell)
        XCTAssertTrue(
            row.times.stringValue.hasPrefix(CueTime.running(shown)),
            "Active Cues \(row.times.stringValue) vs the cell \(cell.label.stringValue)")
        XCTAssertTrue(row.times.stringValue.contains("  -"), "with the time left in the pass: \(row.times.stringValue)")
        XCTAssertEqual(row.bar.doubleValue, shown / pass, accuracy: 0.001, "the bar is how far through the pass it is")
    }
}
