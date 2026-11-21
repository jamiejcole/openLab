// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// DX-01: the Warnings page of the status window (live), and the words for every problem and warning the engine can send.
@MainActor
final class WarningsTests: XCTestCase {
    static let second: ModelID = "00000000-0000-0000-0000-000000000002"
    private let rain = TestSnapshots.rain
    private let radio = TestSnapshots.radio

    private var rig: Rig!
    private var store: WorkspaceStore { rig.store }
    private var wc: WorkspaceWindowController!
    private var side: WarningsPageController!

    override func tearDown() async throws {
        wc?.window?.close()
        (wc, side, rig) = (nil, nil, nil)
        ownEngine?.stop()
    }

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
        // The page the tests watch: the status window's Warnings list, as the footer's button opens it.
        side = wc.warningsPage
        side.window?.setContentSize(NSSize(width: 520, height: 400))
        side.window?.contentView?.layoutSubtreeIfNeeded()
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

    /// ui-qa D3: a cue on a patch on the system output with no output device selected on the Mac
    /// says that, as the settings table does, not that a device named "system output" is missing.
    func testACueOnTheSystemOutputWithNoDeviceSaysTheMacHasNoOutputSelected() throws {
        let p = try JSONDecoder().decode(
            WireProblem.self,
            from: Data(
                #"{"type":"device_unavailable","patch":"00000000-0000-0000-0000-000000000001","resolution":{"type":"device_missing","device":"system output"}}"#
                    .utf8))
        let text = ProblemText.problem(p, label: { _ in "Main" })
        XCTAssertEqual(
            text,
            loc("problem.device_unavailable.system_output_missing").replacingOccurrences(of: "{patch}", with: "Main"))
        XCTAssertFalse(text.contains("“system output”"), text)
    }

    /// An older engine may still send the removed `unsupported` warning: the message
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
        let row = try XCTUnwrap(side.rows.first { $0.cue == radio })
        XCTAssertEqual(row.title, "2.3  Radio")
        XCTAssertEqual(row.detail, loc("broken.device_lost"))
        XCTAssertFalse(row.canRelink)
        XCTAssertEqual(side.table.numberOfRows, side.rows.count)
        try receive(
            #"{"type":"warnings","warnings":[{"type":"notice","code":"media_damaged","message":"intro.wav has a damaged tail"}]}"#
        )
        XCTAssertEqual(side.rows.last?.title, "intro.wav has a damaged tail")
        XCTAssertEqual(side.summaryLabel.stringValue, String(format: loc("warnings.many"), side.rows.count))
    }

    /// #770: a Video cue whose picture failed while playing is one warning: its own entry, saying
    /// why in the engine's words, not a second entry for the engine's notice of the same failure.
    func testAPictureThatFailedIsOneWarningOnItsCueSayingWhy() async throws {
        try await make()
        let why = "the Video cue's picture failed: the movie could not be read"
        try runtime(
            #"{"type":"cue_state","cue":"\#(rain)","state":"broken","reason":"decode_failed"},"#
                + #"{"type":"warning","warning":{"type":"notice","code":"video_decode_failed","#
                + #""message":"\#(why)","cue":"\#(rain)"}}"#)
        let rows = side.rows.filter { $0.cue == rain || $0.detail.contains(why) || $0.title.contains(why) }
        XCTAssertEqual(rows.count, 1, "\(side.rows)")
        XCTAssertEqual(rows.first?.cue, rain)
        XCTAssertEqual(rows.first?.detail, why)
        // Reset and playable again: the failure is past, not listed on its own.
        try runtime(#"{"type":"cue_state","cue":"\#(rain)","state":"idle"}"#)
        XCTAssertFalse(side.rows.contains { $0.cue == rain || $0.title.contains(why) }, "\(side.rows)")
    }

    func testRowsUpdateInPlaceWithoutReloading() async throws {
        try await make()
        let reloads = side.wholeReloads
        let before = side.rows.count
        try missingMedia(rain)
        XCTAssertEqual(side.rows.count, before + 1)
        XCTAssertEqual(side.table.numberOfRows, before + 1)
        let row = try XCTUnwrap(side.rows.first { $0.cue == rain })
        XCTAssertTrue(row.canRelink)
        XCTAssertTrue(row.detail.contains("media/r.wav"))
        // Rain precedes Radio in the list, so its row goes before it.
        XCTAssertLessThan(
            try XCTUnwrap(side.rows.firstIndex { $0.cue == rain }),
            try XCTUnwrap(side.rows.firstIndex { $0.cue == radio }))

        // A different file name changes the text of that one row only.
        side.clearUpdateLog()
        try missingMedia(rain, "media/other.wav")
        XCTAssertEqual(side.updatedRows, ["c:\(rain.raw)"])
        XCTAssertEqual(side.table.numberOfRows, before + 1)

        // The engine reports it fixed: the row goes, with nothing pressed.
        try runtime(
            #"{"type":"problems","cue":"\#(rain)","problems":[]},{"type":"cue_state","cue":"\#(rain)","state":"idle"}"#)
        XCTAssertEqual(side.rows.count, before)
        XCTAssertEqual(side.table.numberOfRows, before)
        XCTAssertEqual(side.wholeReloads, reloads, "no change reloaded the whole table")
    }

    func testRuntimeStateChangesOfHealthyCuesDoNotRebuildTheList() async throws {
        try await make()
        let rows = side.rows
        side.clearUpdateLog()
        try runtime(#"{"type":"cue_state","cue":"\#(TestSnapshots.door)","state":"running"}"#)
        XCTAssertEqual(side.rows, rows)
        XCTAssertTrue(side.updatedRows.isEmpty)
    }

    /// A warned Fade's title carries its default name, which follows its target: renaming the target
    /// (a cue with no warning of its own) retitles the row.
    func testAWarnedCuesTitleFollowsItsTargetsRename() async throws {
        try await make()
        let fade = TestSnapshots.fade
        try missingMedia(fade)
        XCTAssertTrue(side.rows.contains { $0.title.hasSuffix("fade and stop Intro") }, "\(side.rows)")
        var intro = try XCTUnwrap(store.model?.cue(TestSnapshots.intro)).raw
        intro.set("name", .string("entrance"))
        let upsert = try CueListHarness.upsertJSON(intro, index: 0)
        try receive(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#)
        XCTAssertTrue(side.rows.contains { $0.title.hasSuffix("fade and stop entrance") }, "\(side.rows)")
    }

    func testWorkspaceWarningsAreReplacedAndRuntimeWarningsAppended() async throws {
        try await make()
        try receive(#"{"type":"warnings","warnings":[]}"#)
        let base = side.rows.count
        try receive(
            #"{"type":"warnings","warnings":[{"type":"load_ignored","cue":"\#(rain)"},{"type":"cue_broken","cue":"\#(radio)"}]}"#
        )
        // Radio is broken: the refusal joins its entry, so only Rain's warning adds one.
        XCTAssertEqual(side.rows.count, base + 1)
        try receive(#"{"type":"warnings","warnings":[{"type":"cue_broken","cue":"\#(radio)"}]}"#)
        XCTAssertEqual(side.rows.count, base)
        try runtime(#"{"type":"warning","warning":{"type":"late_start","cue":"\#(rain)","late_ms":120}}"#)
        XCTAssertEqual(side.rows.count, base + 1)
        // A warning about a cue: the cue is the title, what happened the line under it.
        let late = try XCTUnwrap(side.rows.last)
        XCTAssertEqual(late.title, "2.2  Rain")
        XCTAssertEqual(late.detail, "Started 120 ms late.")
        XCTAssertEqual(late.cue, rain)
        // A refusal to start the broken Radio is told on Radio's own entry.
        let radioRow = try XCTUnwrap(side.rows.first { $0.cue == radio })
        XCTAssertEqual(radioRow.title, "2.3  Radio")
        XCTAssertTrue(radioRow.detail.hasSuffix(loc("warnings.refused.one")), radioRow.detail)
        // The same warning again is the same entry, with its latest words and a count.
        try runtime(#"{"type":"warning","warning":{"type":"late_start","cue":"\#(rain)","late_ms":340}}"#)
        XCTAssertEqual(side.rows.count, base + 1)
        XCTAssertEqual(
            side.rows.last?.detail, String(format: loc("warnings.repeated"), "Started 340 ms late.", 2))
        XCTAssertEqual(Set(side.rows.map(\.id)).count, side.rows.count)
    }

    func testSnapshotReplacesTheRows() async throws {
        try await make()
        XCTAssertTrue(side.rows.contains { $0.cue == radio })
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        try receive(#"{"type":"snapshot","snapshot":\#(try base.text())}"#)
        XCTAssertFalse(side.rows.contains { $0.cue == radio })
        XCTAssertEqual(side.rows, WarningRows.make(store: store))
    }

    // MARK: Click-through

    private func chooseRow(of cue: ModelID) throws {
        let row = try XCTUnwrap(side.rows.firstIndex { $0.cue == cue })
        side.table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
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
        await assertEventually { self.side.rows.contains { $0.cue == self.radio } }
        let row = try XCTUnwrap(side.rows.first { $0.cue == radio })
        XCTAssertEqual(row.title, "2.3  Radio")
        XCTAssertTrue(row.detail.contains("media/o.wav"), row.detail)
        XCTAssertTrue(row.canRelink)
    }

    func testRowChangesFromTheStoreNeverRevealACue() async throws {
        try await make()
        var reveals: [ModelID] = []
        side.onInspect = { id, _ in reveals.append(id) }
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

    /// A row about no cue (a device, a list) can be selected, so it can be hidden, but "Inspect Cue" is
    /// off for it and choosing it reveals nothing.
    func testRowWithoutACueCanBeSelectedAndHiddenButNotInspected() async throws {
        try await make()
        try receive(#"{"type":"warnings","warnings":[{"type":"unknown_list","list":"\#(Self.second)"}]}"#)
        let row = try XCTUnwrap(side.rows.firstIndex { $0.cue == nil })
        XCTAssertTrue(side.tableView(side.table, shouldSelectRow: row), "the page's own delegate lets it be selected")
        var reveals = 0
        side.onInspect = { _, _ in reveals += 1 }
        side.table.selectRowIndexes([row], byExtendingSelection: false)
        XCTAssertEqual(side.table.selectedRow, row)
        XCTAssertFalse(side.inspectButton.isEnabled, "Inspect Cue is off: there is no cue")
        XCTAssertTrue(side.hideButton.isEnabled)
        XCTAssertEqual(reveals, 0, "selecting it reveals nothing")
        let id = side.rows[row].id
        side.hideButton.performClick(nil)
        XCTAssertFalse(side.rows.contains { $0.id == id }, "hidden")
        XCTAssertTrue(side.all.contains { $0.id == id }, "and still counted")
    }

    /// A reason too long for one line (a cue its lost device left paused) wraps, and its row is tall
    /// enough to show all of it, at the sidebar's width and at a narrower one.
    func testAReasonThatWrapsGetsARowTallEnoughForAllOfIt() async throws {
        try await make()
        try runtime(#"{"type":"cue_state","cue":"\#(rain)","state":"paused","reason":"device_lost"}"#)
        let row = try XCTUnwrap(side.rows.firstIndex { $0.cue == rain })
        XCTAssertEqual(side.rows[row].detail, loc("paused.device_lost"))
        for width in [CGFloat(280), 200] {
            let height = WarningCell.height(of: side.rows[row], width: width)
            let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: row) as? WarningCell)
            cell.frame = NSRect(x: 0, y: 0, width: width, height: height)
            cell.layoutSubtreeIfNeeded()
            cell.layoutSubtreeIfNeeded()
            let detail = cell.convert(cell.detail.bounds, from: cell.detail)
            let needed = cell.detail.attributedStringValue.boundingRect(
                with: NSSize(width: detail.width - 4, height: .greatestFiniteMagnitude),
                options: [.usesLineFragmentOrigin, .usesFontLeading]
            ).height
            XCTAssertGreaterThan(detail.width, width / 2, "at \(width)")
            XCTAssertGreaterThan(needed, 20, "wraps at \(width)")
            XCTAssertTrue(cell.bounds.contains(detail), "\(detail) in \(cell.bounds) at \(width)")
            XCTAssertGreaterThanOrEqual(detail.height + 1, needed, "at \(width)")
        }
    }

    /// A missing file whose reason wraps keeps its Find Missing Files button inside its row, so the
    /// next warning never covers it.
    func testAWrappedMissingFileRowHoldsItsButton() async throws {
        try await make()
        try missingMedia(rain, "media/A Long Folder For The Second Act/and-another-folder/rain-bed-take-two.wav")
        let row = try XCTUnwrap(side.rows.firstIndex { $0.cue == rain })
        XCTAssertTrue(side.rows[row].canRelink)
        for width in [CGFloat(280), 200] {
            let height = WarningCell.height(of: side.rows[row], width: width)
            let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: row) as? WarningCell)
            cell.frame = NSRect(x: 0, y: 0, width: width, height: height)
            cell.layoutSubtreeIfNeeded()
            cell.layoutSubtreeIfNeeded()
            let detail = cell.convert(cell.detail.bounds, from: cell.detail)
            let button = cell.convert(cell.relinkButton.bounds, from: cell.relinkButton)
            XCTAssertGreaterThan(detail.height, 20, "the reason wraps at \(width)")
            XCTAssertFalse(button.intersects(detail), "\(button) clear of the reason \(detail) at \(width)")
            XCTAssertTrue(cell.bounds.contains(detail), "\(detail) in \(cell.bounds) at \(width)")
            XCTAssertTrue(cell.bounds.contains(button), "\(button) in \(cell.bounds) at \(width)")
            XCTAssertLessThan(cell.bounds.maxY - button.maxY, 8, "no spare line under the button at \(width)")
        }
    }

    /// A selected row's reasons are drawn in the selection's text colour, not the faint grey.
    func testASelectedRowsReasonsTakeTheSelectionsTextColour() async throws {
        try await make()
        let row = try XCTUnwrap(side.rows.firstIndex { $0.cue == radio })
        let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: row) as? WarningCell)
        cell.backgroundStyle = .emphasized
        XCTAssertEqual(cell.detail.textColor, .alternateSelectedControlTextColor)
        cell.backgroundStyle = .normal
        XCTAssertEqual(cell.detail.textColor, .secondaryLabelColor)
    }

    func testRelinkButtonAsksTheWindowToRelink() async throws {
        try await make()
        var asked = 0
        wc.onRelink = { asked += 1 }
        side.onRelink?()
        XCTAssertEqual(asked, 1)
    }

    // MARK: One entry per problem, in words (#700)

    /// The test's own engine: its virtual output is unplugged and plugged back in.
    private var ownEngine: TestEngine?

    /// The demo show on an engine of the test's own, Rain's number cleared, in a window.
    private func openWithoutRainsNumber() async throws {
        let engine = try await TestEngine.start()
        ownEngine = engine
        rig = try await Rig.open("Demo", on: engine)
        makeWindow()
        wc.window?.setContentSize(NSSize(width: 1200, height: 800))
        wc.window?.contentView?.layoutSubtreeIfNeeded()
        guard case .success = await store.setNumber(rain, nil) else { return XCTFail("number") }
        try await rig.advance()
        XCTAssertNil(store.model?.cue(rain)?.number)
    }

    private func device(lose: Bool) async throws {
        guard case .success = await rig.testEngine.request(lose ? .loseDevice() : .restoreDevice()) else {
            return XCTFail("device")
        }
        // A device plugged back in is found at the next list read and reopened.
        try await rig.advance(lose ? 0.5 : 2)
    }

    /// GO on `cue`: the playhead put on it first, as an operator clicks it then presses GO.
    private func go(on cue: ModelID) async throws {
        let list = TestSnapshots.mainList
        guard case .success = await store.transport(.setPlayhead(list: list, cue: cue)) else { return XCTFail("ph") }
        guard case .success = await store.go(list: list) else { return XCTFail("go") }
        try await rig.advance(0.1)
    }

    /// What the window's Warnings list draws: each row's title and reasons, from its cell.
    private func shownRows() throws -> [(title: String, detail: String)] {
        return try (0..<side.table.numberOfRows).map { row in
            let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: row) as? WarningCell)
            return (cell.title.stringValue, cell.detail.isHidden ? "" : cell.detail.stringValue)
        }
    }

    /// A media file and a volume whose names look like ids, as notices quoting paths carry them.
    private static let uuidFile = "Media/0f8e2b7a-1c3d-4e5f-8a9b-0c1d2e3f4a5b.wav"
    private static let uuidVolume = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d"
    private static let userNames = [uuidFile, uuidVolume]

    /// Notices whose engine text names a file or a volume, as the engine words them.
    private static let pathNotices: [JSONValue] = [
        .object([
            "type": .string("notice"), "code": .string("damaged_media"),
            "message": .string("\(uuidFile): 2 damaged part(s); they play as silence"),
        ]),
        .object([
            "type": .string("notice"), "code": .string("media_volume_not_responding"),
            "message": .string("volume \"\(uuidVolume)\" does not answer: its files are not checked"),
        ]),
    ]

    private static let uuid = try? NSRegularExpression(
        pattern: "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")

    private func assertNoID(_ text: String?, _ context: String, file: StaticString = #filePath, line: UInt = #line) {
        guard var text, let re = Self.uuid else { return }
        // A file or volume the user named like an id is their name, not an internal id.
        for name in Self.userNames { text = text.replacingOccurrences(of: name, with: "") }
        XCTAssertNil(
            re.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)), "\(context): \(text)", file: file,
            line: line)
    }

    /// Every text the Warnings list, its count line and the footer button show or speak, tooltips
    /// included, is free of internal ids.
    private func assertListShowsNoID(_ context: String) throws {
        for row in 0..<side.table.numberOfRows {
            let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: row) as? WarningCell)
            for text in [cell.title.stringValue, cell.detail.stringValue, cell.accessibilityLabel()] {
                assertNoID(text, context)
            }
            for view in [cell, cell.title, cell.detail, cell.relinkButton] { assertNoID(view.toolTip, context) }
        }
        let button = wc.footer.warningsButton
        for text in [side.summaryLabel.stringValue, button.title, button.toolTip, button.accessibilityLabel()] {
            assertNoID(text, context)
        }
    }

    func testACueWithoutANumberRefusedAsBrokenIsNamedByItsNameNeverByAnID() async throws {
        try await openWithoutRainsNumber()
        try await device(lose: true)
        try await go(on: rain)
        await assertEventually("the refusal is listed") {
            self.store.runtimeWarnings.contains { $0.type == "cue_broken" }
        }
        let rows = try shownRows()
        XCTAssertTrue(
            rows.contains { $0.title == "Rain" && $0.detail.contains(loc("warnings.refused.one")) }, "\(rows)")
        try assertListShowsNoID("after GO on Rain")
    }

    /// Every warning and problem the engine can send (each fixture's variant), about a cue with no
    /// number and about ids the workspace does not hold, as a workspace warning and as one at run
    /// time, plus a notice whose engine text quotes ids: no id reaches the list.
    func testNoWarningTheEngineCanRaiseShowsAnInternalID() async throws {
        try await make()
        var cue = try XCTUnwrap(store.model?.cue(rain)).raw
        cue.set("number", .null)
        let upsert = try CueListHarness.upsertJSON(cue, parent: TestSnapshots.group, index: 1)
        try receive(#"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#)
        XCTAssertNil(store.model?.cue(rain)?.number)
        let gone = "6c1e43d2-58a1-4b7e-9f0e-1d2c3b4a5f60"
        var warnings = try warningVariants().map(\.1.json)
        warnings.append(
            .object([
                "type": .string("notice"), "code": .string("load_repair"),
                "message": .string("cue \(rain.raw): target \(gone) does not exist"), "cue": .string(rain.raw),
            ]))
        warnings += Self.pathNotices
        for id in [rain.raw, gone] {
            let ws = try warnings.map { w -> String in
                guard case .object(var o) = w else { return try w.text() }
                for (k, v) in o where ["cue", "target", "list", "patch"].contains(k) && v.stringValue != nil {
                    o[k] = .string(id)
                }
                return try JSONValue.object(o).text()
            }
            try receive(#"{"type":"warnings","warnings":[\#(ws.joined(separator: ","))]}"#)
            try runtime(ws.map { #"{"type":"warning","warning":\#($0)}"# }.joined(separator: ","))
            for (file, p) in try problemVariants() {
                var o = p.json
                for k in ["target", "patch"] where o[k]?.stringValue != nil { o.set(k, .string(id)) }
                try runtime(#"{"type":"problems","cue":"\#(rain)","problems":[\#(try o.text())]}"#)
                try assertListShowsNoID("\(file) on \(id)")
            }
            XCTAssertGreaterThan(side.rows.count, 10)
            try assertListShowsNoID("warnings on \(id)")
            for row in side.rows { assertNoID(row.title + row.detail, "row \(row.id)") }
        }
    }

    /// A media file or volume named like an id is shown by its name, untouched; a show-file repair's
    /// ids are named in words.
    func testAFileOrVolumeNamedLikeAnIDIsShownAsItIs() async throws {
        try await make()
        let repair = JSONValue.object([
            "type": .string("notice"), "code": .string("load_repair"),
            "message": .string("cue \(rain.raw): target 6c1e43d2-58a1-4b7e-9f0e-1d2c3b4a5f60 does not exist"),
        ])
        let list = try (Self.pathNotices + [repair]).map { try $0.text() }.joined(separator: ",")
        try receive(#"{"type":"warnings","warnings":[\#(list)]}"#)
        let titles = side.rows.map(\.title)
        XCTAssertTrue(titles.contains("\(Self.uuidFile): 2 damaged part(s); they play as silence"), "\(titles)")
        XCTAssertTrue(titles.contains { $0.contains("volume \"\(Self.uuidVolume)\"") }, "\(titles)")
        XCTAssertTrue(
            titles.contains("cue 2.2  Rain: target \(loc("warnings.unknownItem")) does not exist"), "\(titles)")
    }

    func testGOPressedFiveTimesOnTheSameBrokenCueIsOneEntryForThatCue() async throws {
        try await openWithoutRainsNumber()
        try await device(lose: true)
        for _ in 0..<5 { try await go(on: rain) }
        await assertEventually("five refusals") {
            self.store.runtimeWarnings.filter { $0.type == "cue_broken" }.count == 5
        }
        let rows = try shownRows()
        let rains = rows.filter { $0.title == "Rain" }
        XCTAssertEqual(rains.count, 1, "\(rows)")
        XCTAssertTrue(rains[0].detail.contains(String(format: loc("warnings.refused.many"), 5)), rains[0].detail)
        // Once Rain can play again its refusals are past; a later loss counts afresh.
        try await device(lose: false)
        await assertEventually("Rain playable") { self.store.problems[self.rain] == nil }
        XCTAssertFalse(try shownRows().contains { $0.title == "Rain" })
        try await device(lose: true)
        try await go(on: rain)
        await assertEventually("Rain listed again") { (try? self.shownRows())?.contains { $0.title == "Rain" } == true }
        let again = try XCTUnwrap(try shownRows().first { $0.title == "Rain" })
        XCTAssertTrue(again.detail.contains(loc("warnings.refused.one")), again.detail)
    }

    func testOneDeviceLostIsOneEntryForTheDeviceInPlainWords() async throws {
        try await openWithoutRainsNumber()
        let deviceTitle = String(format: loc("warnings.device"), "openLab Virtual Output")
        for loss in 1...2 {
            try await device(lose: true)
            await assertEventually("loss \(loss) listed") {
                (try? self.shownRows())?.contains { $0.title == deviceTitle } == true
            }
            let rows = try shownRows()
            let devices = rows.filter { $0.title.contains("openLab Virtual Output") }
            XCTAssertEqual(devices.count, 1, "loss \(loss): \(rows)")
            XCTAssertEqual(devices.first?.detail, loc("warnings.device.device_lost"))
            XCTAssertEqual(rows.filter { $0.title.isEmpty }.count, 0)
            // The engine's log words never reach the list.
            for row in rows {
                XCTAssertFalse((row.title + row.detail).contains("cue(s)"), "\(row)")
                XCTAssertFalse((row.title + row.detail).contains("audio device \""), "\(row)")
            }
            // The whole entry is drawn: its cell is as tall as its wrapped title and words.
            let index = try XCTUnwrap(side.rows.firstIndex { $0.title == deviceTitle })
            let width = side.table.bounds.width
            XCTAssertGreaterThan(width, 100)
            let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: index) as? WarningCell)
            cell.frame = NSRect(
                x: 0, y: 0, width: width, height: WarningCell.height(of: side.rows[index], width: width))
            cell.layoutSubtreeIfNeeded()
            for field in [cell.title, cell.detail] {
                let frame = cell.convert(field.bounds, from: field)
                let needed = field.attributedStringValue.boundingRect(
                    with: NSSize(width: frame.width - 4, height: .greatestFiniteMagnitude),
                    options: [.usesLineFragmentOrigin, .usesFontLeading]
                ).height
                XCTAssertGreaterThanOrEqual(frame.height + 1, needed, "\(field.stringValue)")
                XCTAssertTrue(cell.bounds.contains(frame), "\(frame) in \(cell.bounds)")
            }
            // Back: the entry goes.
            try await device(lose: false)
            await assertEventually("loss \(loss): gone once back") {
                (try? self.shownRows())?.contains { $0.title == deviceTitle } == false
            }
        }
    }

    /// A long device name is shortened in its title, never cut off: the title wraps.
    func testALongDeviceNameIsShortenedAndItsEntryWraps() async throws {
        try await make()
        let name = "Front of House Main Left Right Interface With A Very Long Name Indeed (USB)"
        try receive(
            #"{"type":"warnings","warnings":[{"type":"notice","code":"device_lost","message":"x","device":"\#(name)"},"#
                + #"{"type":"notice","code":"patch_device_missing","message":"y","device":"\#(name)"}]}"#)
        let rows = side.rows.filter { $0.cue == nil }
        XCTAssertEqual(rows.count, 1, "\(rows)")
        let row = try XCTUnwrap(rows.first)
        XCTAssertTrue(row.title.contains("…"), row.title)
        XCTAssertLessThan(row.title.count, name.count)
        XCTAssertEqual(row.detail, loc("warnings.device.device_lost"))
        XCTAssertGreaterThan(WarningCell.height(of: row, width: 220), WarningCell.height(of: row, width: 600))
    }

    /// A patch on the system output with no output device selected on the Mac is one entry like a
    /// device's: a short title (the choice as the patch pop-up names it), the reason on the line
    /// under it; two such patches are still one entry. The cell draws both lines whole.
    func testTheSystemOutputWithNoDeviceIsOneEntryWithATitleAndAReason() async throws {
        try await make()
        try receive(
            #"{"type":"warnings","warnings":[{"type":"notice","code":"patch_no_system_output","message":"Patch \"Main\" is on the system output"},"#
                + #"{"type":"notice","code":"patch_no_system_output","message":"Patch \"Side\" is on the system output"}]}"#
        )
        let rows = side.rows.filter { $0.cue == nil }
        XCTAssertEqual(rows.count, 1, "\(rows)")
        let row = try XCTUnwrap(rows.first)
        XCTAssertEqual(row.title, loc("settings.audio.systemOutputNone"))
        XCTAssertEqual(row.detail, loc("warnings.systemOutputNone"))
        XCTAssertFalse(row.title.contains("\""), row.title)
        let index = try XCTUnwrap(side.rows.firstIndex { $0.id == row.id })
        side.table.reloadData()
        let width = side.table.bounds.width
        let cell = try XCTUnwrap(side.tableView(side.table, viewFor: nil, row: index) as? WarningCell)
        cell.frame = NSRect(x: 0, y: 0, width: width, height: WarningCell.height(of: row, width: width))
        cell.layoutSubtreeIfNeeded()
        XCTAssertEqual(cell.title.stringValue, row.title)
        XCTAssertEqual(cell.detail.stringValue, row.detail)
        for field in [cell.title, cell.detail] {
            let frame = cell.convert(field.bounds, from: field)
            let needed = field.attributedStringValue.boundingRect(
                with: NSSize(width: frame.width - 4, height: .greatestFiniteMagnitude),
                options: [.usesLineFragmentOrigin, .usesFontLeading]
            ).height
            XCTAssertGreaterThanOrEqual(frame.height + 1, needed, "\(field.stringValue)")
            XCTAssertTrue(cell.bounds.contains(frame), "\(frame) in \(cell.bounds)")
        }
    }

    /// A device move that has to wait for its file shows on the cue's own entry, in our words.
    func testADelayedMoveShowsOnTheCuesEntry() throws {
        let w = try JSONDecoder().decode(
            WireWarning.self,
            from: Data(
                #"{"type":"notice","code":"move_delayed","message":"cue 1: engine words","cue":"00000000-0000-0000-0000-00000000000a"}"#
                    .utf8))
        XCTAssertEqual(ProblemText.warningDetail(w, label: { $0.raw }), loc("warning.notice.move_delayed"))
    }

    /// A cue starved by the disk is one entry on the cue, counted up by each notice's count (the
    /// engine tells repeats at most every ten seconds with how many since); a device's dropouts are
    /// one entry for the device saying how many so far; a slow disk is one entry of its own. A
    /// count no engine sends (huge, negative) is bounded, never a trap.
    func testUnderrunsAddUpOnTheCuesEntryAndDropoutsAndASlowDiskAreEntriesInOurWords() async throws {
        try await make()
        try receive(#"{"type":"warnings","warnings":[]}"#)
        let base = side.rows.count
        func underrun(_ count: String) throws {
            try runtime(
                #"{"type":"warning","warning":{"type":"notice","code":"underrun","message":"engine words","cue":"\#(rain)","count":\#(count)}}"#
            )
        }
        try underrun("1")
        try underrun("4")
        XCTAssertEqual(side.rows.count, base + 1)
        let starved = try XCTUnwrap(side.rows.last)
        XCTAssertEqual(starved.cue, rain)
        XCTAssertEqual(starved.detail, String(format: loc("warnings.repeated"), loc("warning.notice.underrun"), 5))
        try underrun("1e300")
        try underrun("-3")
        XCTAssertEqual(side.rows.count, base + 1)
        XCTAssertEqual(
            side.rows.last?.detail, String(format: loc("warnings.repeated"), loc("warning.notice.underrun"), 1_000_006))

        try receive(
            #"{"type":"warnings","warnings":[{"type":"notice","code":"audio_dropouts","message":"engine words","device":"Out","count":7},{"type":"notice","code":"disk_behind","message":"engine words"}]}"#
        )
        let device = try XCTUnwrap(side.rows.first { $0.title == String(format: loc("warnings.device"), "Out") })
        XCTAssertEqual(
            device.detail, loc("warnings.device.audio_dropouts").replacingOccurrences(of: "{count}", with: "7"))
        XCTAssertTrue(side.rows.contains { $0.title == loc("warning.notice.disk_behind") })
        XCTAssertFalse(side.rows.contains { ($0.title + $0.detail).contains("engine words") })
        // Recovered: the engine's list no longer has them, and the entries go.
        try receive(#"{"type":"warnings","warnings":[]}"#)
        XCTAssertFalse(side.rows.contains { $0.title.contains("Out") || $0.title == loc("warning.notice.disk_behind") })
    }

    func testTheCountInTheFooterMatchesTheEntriesShown() async throws {
        try await openWithoutRainsNumber()
        try await device(lose: true)
        for _ in 0..<5 { try await go(on: rain) }
        try await go(on: TestSnapshots.door)
        await assertEventually("six refusals") {
            self.store.runtimeWarnings.filter { $0.type == "cue_broken" }.count == 6
        }
        func check(_ context: String) throws {
            let shown = try shownRows().count
            XCTAssertEqual(wc.footer.warningsButton.title, String(shown), context)
            XCTAssertEqual(
                side.summaryLabel.stringValue,
                shown == 1 ? loc("warnings.one") : String(format: loc("warnings.many"), shown), context)
        }
        // Intro, Door slam, Rain and Radio broken, and the device: five entries.
        let lost = try shownRows()
        XCTAssertEqual(lost.count, 5, "\(lost)")
        try check("lost")
        try await device(lose: false)
        await assertEventually("back") { self.store.problems[self.rain] == nil }
        try check("back")
        let back = try shownRows()
        XCTAssertEqual(back.count, 1, "only Radio's missing file: \(back)")
    }
}
