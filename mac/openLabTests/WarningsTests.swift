// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// DX-01: the Warnings tab, and the words for every problem and warning the engine can send.
@MainActor
final class WarningsTests: XCTestCase {
    static let second: ModelID = "00000000-0000-0000-0000-000000000002"
    private let rain = TestSnapshots.rain
    private let radio = TestSnapshots.radio

    private var rig: Rig!
    private var store: WorkspaceStore { rig.store }
    private let spy = SpyTableView()
    private var wc: WorkspaceWindowController!
    private var side: SidebarViewController!

    /// No engine: the demo show with a second, empty list, its Radio broken by a lost device.
    private func make() async throws {
        var snap = try TestSnapshots.show()
        var lists = snap["workspace"]?["cue_lists"]?.arrayValue ?? []
        lists.append(.object(["id": .string(Self.second.raw), "name": .string("Second"), "cues": .array([])]))
        snap["workspace"]?.set("cue_lists", .array(lists))
        snap.set(
            "cue_states",
            .array([
                .object(["cue": .string(radio.raw), "state": .string("broken"), "reason": .string("device_lost")])
            ]))
        rig = try Rig.offline(snap)
        makeWindow()
    }

    private func makeWindow() {
        wc = WorkspaceWindowController(store: store)
        // The sidebar the tests watch: a second one around a spy table.
        side = SidebarViewController(store: store, warningsTable: spy)
        side.view.frame = NSRect(x: 0, y: 0, width: 280, height: 600)
    }

    private func receive(_ json: String) throws {
        XCTAssertEqual(store.receive(try ServerMessage.decode(json)), .applied)
    }

    private func runtime(_ changes: String) throws {
        try receive(#"{"type":"runtime","seq":\#(store.runtimeSeq + 1),"changes":[\#(changes)]}"#)
    }

    private func missingMedia(_ cue: ModelID, _ media: String = "media/r.wav") throws {
        try runtime(
            #"{"type":"cue_state","cue":"\#(cue)","state":"broken"},"#
                + #"{"type":"problems","cue":"\#(cue)","problems":[{"type":"media_missing","media":"\#(media)"}]}"#)
    }

    // MARK: Text for every variant

    private func warningVariants() throws -> [(String, WireWarning)] {
        try Fixtures.files("server").filter { $0.lastPathComponent.hasPrefix("warnings_") }.flatMap {
            file -> [(String, WireWarning)] in
            let text = try String(contentsOf: file, encoding: .utf8)
            guard case .warnings(let list) = try ServerMessage.decode(text) else { return [] }
            return list.map { (file.lastPathComponent, $0) }
        }
    }

    private func problemVariants() throws -> [(String, WireProblem)] {
        try Fixtures.files("server").filter { $0.lastPathComponent.hasPrefix("runtime_problem_") }.flatMap {
            file -> [(String, WireProblem)] in
            let text = try String(contentsOf: file, encoding: .utf8)
            guard case .runtime(_, let changes) = try ServerMessage.decode(text) else { return [] }
            return changes.flatMap { c -> [(String, WireProblem)] in
                if case .problems(_, let list) = c { return list.map { (file.lastPathComponent, $0) } }
                return []
            }
        }
    }

    func testEveryWarningFixtureHasLocalisedText() throws {
        let variants = try warningVariants()
        XCTAssertGreaterThanOrEqual(variants.count, 25)
        var seen = Set<String>()
        for (file, w) in variants {
            let text = ProblemText.warning(w, label: { $0.raw })
            XCTAssertFalse(text.isEmpty, file)
            XCTAssertFalse(text.contains("{"), "\(file): unfilled field in \(text)")
            XCTAssertFalse(text.hasPrefix("Unrecognised"), "\(file): no text for \(w.type)")
            XCTAssertFalse(text.hasPrefix("warning."), "\(file): key leaked")
            if w.type != "notice" { seen.insert(text) }
        }
        // Every fixture (types and their reasons) reads differently: none shares a generic line.
        XCTAssertEqual(seen.count, variants.filter { $0.1.type != "notice" }.count)
    }

    func testEveryProblemFixtureHasLocalisedText() throws {
        let variants = try problemVariants()
        XCTAssertGreaterThanOrEqual(variants.count, 15)
        for (file, p) in variants {
            let text = ProblemText.problem(p, label: { $0.raw })
            XCTAssertFalse(text.contains("{"), "\(file): unfilled field in \(text)")
            XCTAssertFalse(text.hasPrefix("Unrecognised"), "\(file): no text for \(p.type)")
            XCTAssertFalse(text.hasPrefix("problem."), "\(file): key leaked")
        }
    }

    func testProblemTextNamesTheThingsItIsAbout() throws {
        let missing = try XCTUnwrap(problemVariants().first { $0.0.contains("media_missing") }).1
        XCTAssertTrue(ProblemText.problem(missing, label: { $0.raw }).contains("audio/storm.wav"))
        let channels = try XCTUnwrap(problemVariants().first { $0.0.contains("channels_missing") }).1
        let text = ProblemText.problem(channels, label: { _ in "Main" })
        XCTAssertTrue(
            text.contains("Scarlett 2i2") && text.contains("Main") && text.contains("4") && text.contains("2"), text)
        let target = try XCTUnwrap(problemVariants().first { $0.0.contains("target_missing") }).1
        XCTAssertTrue(ProblemText.problem(target, label: { _ in "2.1  Door slam" }).contains("2.1  Door slam"))
    }

    /// An older engine may still send the removed `unsupported` warning (M4.6c): the message
    /// decodes and the row reads as an unrecognised warning.
    func testLegacyUnsupportedWarningDecodesAsUnknown() throws {
        let m = try ServerMessage.decode(
            #"{"type":"warnings","warnings":[{"type":"unsupported","cue":"\#(rain)"}]}"#)
        guard case .warnings(let ws) = m, let w = ws.first else { return XCTFail("\(m)") }
        XCTAssertEqual(w.type, "unsupported")
        XCTAssertTrue(ProblemText.warning(w, label: { $0.raw }).contains("unsupported"))
    }

    func testUnknownVariantsShowTheirRawType() throws {
        let w = try JSONDecoder().decode(WireVariant.self, from: Data(#"{"type":"flux_capacitor","x":1}"#.utf8))
        XCTAssertTrue(ProblemText.warning(w, label: { $0.raw }).contains("flux_capacitor"))
        XCTAssertTrue(ProblemText.problem(w, label: { $0.raw }).contains("flux_capacitor"))
        // A known type with a reason this build does not know falls back to the type's own line.
        let r = try JSONDecoder().decode(
            WireVariant.self, from: Data(#"{"type":"go_ignored","list":"L","reason":"new_reason"}"#.utf8))
        XCTAssertFalse(ProblemText.warning(r, label: { $0.raw }).contains("{"))
    }

    func testNoticeWithoutLocalisedCodeShowsTheEnginesMessage() throws {
        let w = try XCTUnwrap(warningVariants().first { $0.1.type == "notice" }).1
        XCTAssertEqual(ProblemText.warning(w, label: { $0.raw }), w.json["message"]?.stringValue)
    }

    // MARK: The tab

    func testBrokenCueIsListedWithItsReasonAndNoticesFollow() async throws {
        try await make()
        let row = try XCTUnwrap(side.warningRows.first { $0.cue == radio })
        XCTAssertEqual(row.title, "2.3  Radio")
        XCTAssertEqual(row.detail, loc("broken.device_lost"))
        XCTAssertFalse(row.canRelink)
        XCTAssertEqual(side.warningsTable.numberOfRows, side.warningRows.count)
        try receive(
            #"{"type":"warnings","warnings":[{"type":"notice","code":"media_damaged","message":"intro.wav has a damaged tail"}]}"#
        )
        XCTAssertEqual(side.warningRows.last?.title, "intro.wav has a damaged tail")
        XCTAssertEqual(side.warningsCount.stringValue, String(format: loc("warnings.many"), side.warningRows.count))
    }

    func testRowsUpdateInPlaceWithoutReloading() async throws {
        try await make()
        spy.wholeReloads = 0
        let before = side.warningRows.count
        try missingMedia(rain)
        XCTAssertEqual(side.warningRows.count, before + 1)
        XCTAssertEqual(spy.inserts, 1)
        XCTAssertEqual(spy.removals, 0)
        let row = try XCTUnwrap(side.warningRows.first { $0.cue == rain })
        XCTAssertTrue(row.canRelink)
        XCTAssertTrue(row.detail.contains("media/r.wav"))
        // Rain precedes Radio in the list, so its row goes before it.
        XCTAssertLessThan(
            try XCTUnwrap(side.warningRows.firstIndex { $0.cue == rain }),
            try XCTUnwrap(side.warningRows.firstIndex { $0.cue == radio }))

        // A different file name changes the text of that one row only.
        side.clearWarningUpdateLog()
        try missingMedia(rain, "media/other.wav")
        XCTAssertEqual(side.updatedWarningRows, ["c:\(rain.raw)"])
        XCTAssertEqual(spy.inserts, 1)
        XCTAssertEqual(spy.removals, 0)

        // The engine reports it fixed: the row goes.
        try runtime(
            #"{"type":"problems","cue":"\#(rain)","problems":[]},{"type":"cue_state","cue":"\#(rain)","state":"idle"}"#)
        XCTAssertEqual(side.warningRows.count, before)
        XCTAssertEqual(spy.removals, 1)
        XCTAssertEqual(spy.wholeReloads, 0, "no change reloaded the whole table")
    }

    func testRuntimeStateChangesOfHealthyCuesDoNotRebuildTheList() async throws {
        try await make()
        let rows = side.warningRows
        side.clearWarningUpdateLog()
        try runtime(#"{"type":"cue_state","cue":"\#(TestSnapshots.door)","state":"running"}"#)
        XCTAssertEqual(side.warningRows, rows)
        XCTAssertTrue(side.updatedWarningRows.isEmpty)
    }

    func testWorkspaceWarningsAreReplacedAndRuntimeWarningsAppended() async throws {
        try await make()
        try receive(#"{"type":"warnings","warnings":[]}"#)
        let base = side.warningRows.count
        try receive(
            #"{"type":"warnings","warnings":[{"type":"load_ignored","cue":"\#(rain)"},{"type":"cue_broken","cue":"\#(radio)"}]}"#
        )
        XCTAssertEqual(side.warningRows.count, base + 2)
        try receive(#"{"type":"warnings","warnings":[{"type":"cue_broken","cue":"\#(radio)"}]}"#)
        XCTAssertEqual(side.warningRows.count, base + 1)
        try runtime(#"{"type":"warning","warning":{"type":"late_start","cue":"\#(rain)","late_ms":120}}"#)
        XCTAssertEqual(side.warningRows.count, base + 2)
        XCTAssertTrue(try XCTUnwrap(side.warningRows.last).title.contains("120"))
        // The same warning twice is two rows (each occurrence stays visible).
        try runtime(#"{"type":"warning","warning":{"type":"late_start","cue":"\#(rain)","late_ms":120}}"#)
        XCTAssertEqual(side.warningRows.count, base + 3)
        XCTAssertEqual(Set(side.warningRows.map(\.id)).count, side.warningRows.count)
    }

    func testSnapshotReplacesTheRows() async throws {
        try await make()
        XCTAssertTrue(side.warningRows.contains { $0.cue == radio })
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        try receive(#"{"type":"snapshot","snapshot":\#(try base.text())}"#)
        XCTAssertFalse(side.warningRows.contains { $0.cue == radio })
        XCTAssertEqual(side.warningRows, WarningRows.make(store: store))
    }

    // MARK: Click-through

    private func chooseRow(of cue: ModelID) throws {
        let row = try XCTUnwrap(wc.sidebar.warningRows.firstIndex { $0.cue == cue })
        wc.sidebar.warningsTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
    }

    func testChoosingAWarningSelectsItsCueAndOpensItsGroup() async throws {
        try await make()
        let outline = wc.cueList.outlineView
        let group = try XCTUnwrap(wc.cueList.node(TestSnapshots.group))
        outline.collapseItem(group)
        XCTAssertEqual(outline.row(forItem: wc.cueList.node(radio)), -1)
        try chooseRow(of: radio)
        XCTAssertEqual(wc.cueList.selectedIDs(), [radio])
        XCTAssertTrue(outline.isItemExpanded(group))
    }

    func testChoosingAWarningSwitchesToTheListThatHoldsTheCue() async throws {
        try await make()
        wc.sidebar.listTable.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        XCTAssertEqual(wc.cueList.listID, Self.second)
        try chooseRow(of: radio)
        XCTAssertEqual(wc.cueList.listID, TestSnapshots.mainList)
        XCTAssertEqual(wc.cueList.selectedIDs(), [radio])
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 0)
    }

    func testChoosingAWarningNeverMovesThePlayhead() async throws {
        rig = try await Rig.open()
        makeWindow()
        XCTAssertEqual(store.playheads[TestSnapshots.mainList], TestSnapshots.intro)
        try chooseRow(of: radio)
        XCTAssertEqual(wc.cueList.selectedIDs(), [radio])
        await settle()
        try await rig.advance()
        XCTAssertEqual(
            store.playheads[TestSnapshots.mainList], TestSnapshots.intro,
            "selecting a warning must not change what GO fires")
        guard case .success = await store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.1)
        await assertEventually("GO still fires Intro") { self.store.cueStates[TestSnapshots.intro]?.state == .running }
    }

    /// The real engine's demo show: Radio's file is missing, so it is listed with that reason and
    /// can be relinked.
    func testTheEnginesBrokenCueIsListedWithItsReason() async throws {
        rig = try await Rig.open()
        makeWindow()
        await assertEventually { self.side.warningRows.contains { $0.cue == self.radio } }
        let row = try XCTUnwrap(side.warningRows.first { $0.cue == radio })
        XCTAssertEqual(row.title, "2.3  Radio")
        XCTAssertTrue(row.detail.contains("media/o.wav"), row.detail)
        XCTAssertTrue(row.canRelink)
    }

    func testRowChangesFromTheStoreNeverRevealACue() async throws {
        try await make()
        var reveals: [ModelID] = []
        wc.sidebar.onSelectCue = { reveals.append($0) }
        try chooseRow(of: radio)
        XCTAssertEqual(reveals, [radio], "once per click")
        wc.cueList.select([TestSnapshots.door])
        // A row inserted above the selected one moves the table's selection; an edit redraws a row.
        try missingMedia(rain)
        try missingMedia(rain, "media/other.wav")
        try receive(#"{"type":"warnings","warnings":[]}"#)
        XCTAssertEqual(reveals, [radio])
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.door])
    }

    func testRowWithoutACueCannotBeSelected() async throws {
        try await make()
        try receive(#"{"type":"warnings","warnings":[{"type":"unknown_list","list":"\#(Self.second)"}]}"#)
        let row = try XCTUnwrap(wc.sidebar.warningRows.firstIndex { $0.cue == nil })
        XCTAssertFalse(wc.sidebar.tableView(wc.sidebar.warningsTable, shouldSelectRow: row))
    }

    func testRelinkButtonAsksTheWindowToRelink() async throws {
        try await make()
        var asked = 0
        wc.onRelink = { asked += 1 }
        wc.sidebar.onRelink?()
        XCTAssertEqual(asked, 1)
    }
}
