// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// SM-06, NFR-07: the pre-show check window, its report rows and the click-through to the cue.
@MainActor
final class PreshowTests: XCTestCase {
    private let intro = TestSnapshots.intro
    private let radio = TestSnapshots.radio

    /// No engine: the demo show's model, for what the rows make of a report.
    private func demoRig() async throws -> Rig { try Rig.offline(TestSnapshots.show()) }

    /// The demo show on the real engine, whose check finds Radio's file missing.
    private func realRig() async throws -> Rig { try await Rig.open() }

    private func report(_ json: String) throws -> PreshowReport {
        try JSONDecoder().decode(PreshowReport.self, from: Data(json.utf8))
    }

    private func fixtureReport() throws -> PreshowReport {
        guard
            case .result(_, .ok(let ok)) = try ServerMessage.decode(try Fixtures.text("server", "result_preshow_check")),
            case .preshowCheck(let r)? = ok.data
        else { throw XCTSkip("fixture does not decode") }
        return r
    }

    private static let emptySummary =
        #""summary":{"broken_cues":0,"media":0,"patches":0,"network":0,"network_unchecked":0,"system":0,"total":0}"#

    // MARK: Report model

    func testRowsComeGroupedInFixedSectionOrderKeepingTheEnginesOrder() async throws {
        let rig = try await demoRig()
        let rows = PreshowRows.make(report: try fixtureReport(), model: rig.store.model)
        XCTAssertEqual(
            rows.filter(\.isHeading).map(\.section), [.brokenCues, .media, .devices, .network, .system])
        XCTAssertEqual(rows.first?.title, String(format: loc("preshow.section.brokenCues"), 2))
        // Every entry follows its own heading.
        var current: PreshowRow.Section?
        for r in rows {
            if r.isHeading { current = r.section } else { XCTAssertEqual(r.section, current, r.title) }
        }
        let entries = rows.filter { !$0.isHeading }
        let broken = entries.filter { $0.section == .brokenCues }
        // Names come from the app's model: the fade cue has no name, so its kind shows.
        XCTAssertEqual(broken.map(\.cue), [intro, TestSnapshots.fade])
        XCTAssertTrue(broken[0].title.contains("Intro"))
        XCTAssertEqual(broken[0].inspectorTab, "basics")
        XCTAssertTrue(broken[0].detail.contains("/Shows/storm.wav"))
        XCTAssertEqual(entries.filter { $0.section == .media }.map(\.title), ["/Shows/rain.mp3", "/Shows/storm.wav"])
        XCTAssertTrue(entries.first { $0.section == .media }?.detail.contains("unsupported codec") == true)

        let patches = entries.filter { $0.section == .devices }
        XCTAssertEqual(patches.first?.title, "Main")
        XCTAssertTrue(patches.first?.detail.contains("Scarlett 2i2") == true)
        XCTAssertEqual(patches.first?.cue, intro)
        XCTAssertTrue(patches[1].title.contains("00000000-0000-0000-0000-000000000009"))

        let network = entries.filter { $0.section == .network }
        // An unchecked destination is information; a missing one is a problem.
        XCTAssertEqual(network.map(\.isProblem), [false, true])
        XCTAssertTrue(network[0].detail.contains("10.0.0.5:8000"))
        // Cues 0c and 0d are not in this workspace: nothing to select.
        XCTAssertEqual(network.map(\.cue), [nil, nil])
        XCTAssertEqual(entries.filter { $0.section == .system }.count, 1)
        XCTAssertEqual(PreshowRows.summary(try fixtureReport()), String(format: loc("preshow.summary.many"), 3))
    }

    func testAllClearReportHasNoRowsAndSaysSo() throws {
        let r = try report(
            #"{"ok":true,\#(Self.emptySummary),"broken_cues":[],"media":[],"patches":[],"network":[],"system":[]}"#)
        XCTAssertEqual(PreshowRows.make(report: r, model: nil), [])
        XCTAssertEqual(PreshowRows.summary(r), loc("preshow.summary.ok"))
    }

    func testCueDeletedSinceTheCheckIsMarkedAndCannotBeChosen() async throws {
        let rig = try await demoRig()
        let gone = "00000000-0000-0000-0000-0000000000ff"
        let r = try report(
            #"{"ok":false,\#(Self.emptySummary),"broken_cues":[{"cue":"\#(gone)","list":"\#(TestSnapshots.mainList)","number":"9","name":"Thunder","problems":[{"type":"no_media"}]}],"#
                + #""media":[],"patches":[],"network":[],"system":[]}"#)
        let pc = WarningsPageController(store: rig.store)
        var inspected: [ModelID] = []
        pc.onInspect = { id, _ in inspected.append(id) }
        pc.show(r)
        let row = try XCTUnwrap(pc.rows.firstIndex { !$0.isHeading })
        XCTAssertEqual(pc.rows[row].title, String(format: loc("preshow.cue.deleted"), "9  Thunder"))
        XCTAssertNil(pc.rows[row].cue)
        XCTAssertFalse(pc.choose(row: row))
        XCTAssertFalse(pc.choose(row: 0))  // the heading
        XCTAssertEqual(inspected, [])
    }

    func testAnEntryNamesAtMostFiveCues() throws {
        let ids = (1...8).map { String(format: "\"00000000-0000-0000-0000-%012d\"", $0) }.joined(separator: ",")
        let r = try report(
            #"{"ok":false,\#(Self.emptySummary),"broken_cues":[],"media":[{"path":"/a.wav","status":"missing","cues":[\#(ids)]}],"#
                + #""patches":[],"network":[],"system":[]}"#)
        let media = try XCTUnwrap(PreshowRows.make(report: r, model: nil).last)
        XCTAssertTrue(
            media.detail.hasSuffix(
                String(format: loc("preshow.usedBy.more"), "", 3).trimmingCharacters(in: .whitespaces)))
        XCTAssertNil(media.cue)
    }

    func testPreloadOverBudgetAndSparePatchesAreInformationOnly() throws {
        let r = try report(
            #"{"ok":true,\#(Self.emptySummary),"broken_cues":[],"media":[],"#
                + #""patches":[{"patch":"00000000-0000-0000-0000-000000000001","name":"Spare","mapped":false,"resolution":{"type":"unmapped"},"cues":[]}],"#
                + #""network":[],"system":[{"type":"notice","code":"preload_over_budget","message":"over"}]}"#)
        let entries = PreshowRows.make(report: r, model: nil).filter { !$0.isHeading }
        XCTAssertEqual(entries.map(\.isProblem), [false, false])
        XCTAssertTrue(entries[0].detail.contains(loc("preshow.patch.unused")))
    }

    // MARK: Window

    func testDisconnectedEngineGreysTheItemAndClearsTheReport() async throws {
        let rig = try await realRig()
        let wc = WorkspaceWindowController(store: rig.store)
        let pc = wc.preshowWindow
        await pc.runCheck()
        XCTAssertFalse(pc.rows.isEmpty)
        XCTAssertTrue(pc.checkButton.isEnabled)

        rig.client.stop()  // the connection to the engine is gone
        await assertEventually { rig.store.connectionState != .connected }
        XCTAssertFalse(wc.canRunPreshowCheck)
        XCTAssertFalse(pc.checkButton.isEnabled)
        XCTAssertEqual(pc.rows, [])
        XCTAssertNil(pc.report)
        XCTAssertEqual(pc.summaryLabel.stringValue, loc("preshow.disconnected"))

        // Asked anyway (the window was open): it says nothing was checked, never "no problems".
        await pc.runCheck()
        XCTAssertEqual(pc.rows, [])
        XCTAssertEqual(pc.summaryLabel.stringValue, loc("preshow.disconnected"))
    }

    func testAReplyThatDoesNotDecodeShowsAFailureNotAnOldReport() async throws {
        let rig = try await realRig()
        let pc = WarningsPageController(store: rig.store)
        await pc.runCheck()
        XCTAssertFalse(pc.rows.isEmpty, "Radio's file is missing")
        rig.wire.garbleResults = true
        await pc.runCheck()
        XCTAssertEqual(pc.rows, [])
        XCTAssertEqual(pc.summaryLabel.stringValue, loc("preshow.failed"))
    }

    func testAnOlderAnswerDoesNotReplaceANewerCheck() async throws {
        let rig = try await realRig()
        let pc = WarningsPageController(store: rig.store)
        rig.wire.holdResults = true
        let first = Task { await pc.runCheck() }  // Radio's file is missing
        await assertEventually { rig.wire.requestCount { $0 == .preshowCheck } == 1 }
        XCTAssertTrue(pc.checking)
        XCTAssertFalse(pc.checkButton.isEnabled)
        // Another client points Radio at a file that is there; the engine takes in its probe.
        let other = Rig(engine: rig.testEngine, workspace: rig.doc)
        guard await other.connect() else { return XCTFail("other client") }
        defer { other.client.stop() }
        guard case .success = await other.store.send(command: .setProperty(radio, .media("media/a.wav"))) else {
            return XCTFail("relink Radio")
        }
        try await other.advance()
        let second = Task { await pc.runCheck() }
        await assertEventually("both answers held") {
            rig.wire.heldResults { if case .request(_, .preshowCheck) = $0 { return true } else { return false } } == 2
        }
        rig.wire.holdResults = false
        // The newer (no media problem) answer lands first; the older one must not replace it.
        rig.wire.releaseResults(reversed: true)
        await first.value
        await second.value
        XCTAssertFalse(pc.checking)
        XCTAssertTrue(pc.checkButton.isEnabled)
        XCTAssertEqual(pc.report?.summary.media, 0)
        XCTAssertFalse(pc.rows.contains { $0.section == .media }, "\(pc.rows)")
    }

    func testThePanelClosesWithItsWorkspaceAndNamesIt() async throws {
        let rig = try await realRig()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.workspaceName = "Lear"
        wc.showWindow(nil)
        await wc.showWorkspaceStatus().value
        let panel = try XCTUnwrap(wc.existingStatusWindow?.window)
        XCTAssertTrue(panel.isVisible)
        XCTAssertEqual(panel.title, String(format: loc("status.title.workspace"), "Lear"))
        wc.window?.close()
        XCTAssertFalse(panel.isVisible)
    }

    /// A click on any view that needs its panel to be key (an NSTableView does by default) would
    /// make the report key, and GO would stop reaching the workspace window.
    func testNothingInThePanelMakesItKey() async throws {
        let rig = try await realRig()
        let pc = WarningsPageController(store: rig.store)
        await pc.runCheck()
        let panel = try XCTUnwrap(pc.window as? NSPanel)
        XCTAssertTrue(panel.becomesKeyOnlyIfNeeded)
        func walk(_ v: NSView) -> [NSView] { [v] + v.subviews.flatMap(walk) }
        let views = walk(try XCTUnwrap(panel.contentView))
        XCTAssertTrue(views.contains { $0 === pc.table })
        for v in views { XCTAssertFalse(v.needsPanelToBecomeKey, "\(type(of: v))") }
        // A plain NSTableView's answer follows AppKit state, so the walk alone can pass with one
        // here and still fail on a show machine: the list must be the subclass that never needs it.
        XCTAssertFalse(panel.styleMask.contains(.miniaturizable), "restoring from the Dock could make it key")
        XCTAssertTrue(pc.table is NonKeyTableView)
    }

    func testWindowMenuHasThePreshowCheck() {
        let item = MainMenu.make().windows.items.first { $0.action == #selector(AppDelegate.showWorkspaceStatus(_:)) }
        XCTAssertEqual(item?.title, loc("menu.window.status"))
        XCTAssertEqual(item?.keyEquivalent, "W")
    }

    func testHidingAnEntryDropsItFromTheListAndSaysSoUntilUnhidden() async throws {
        let rig = try await demoRig()
        let pc = WarningsPageController(store: rig.store)
        pc.show(try fixtureReport())
        let all = pc.rows.filter { !$0.isHeading }.count
        let row = try XCTUnwrap(pc.rows.firstIndex { !$0.isHeading && $0.section == .media })
        let hiddenRow = pc.rows[row]
        pc.toggleHidden(row: row)
        XCTAssertEqual(pc.rows.filter { !$0.isHeading }.count, all - 1)
        XCTAssertFalse(pc.rows.contains { $0.hideKey == hiddenRow.hideKey })
        XCTAssertEqual(pc.hiddenCount, 1)
        XCTAssertEqual(
            pc.summaryLabel.stringValue,
            String(format: loc("preshow.summary.hidden"), PreshowRows.summary(try fixtureReport()), 1))
        // Shown again on request, dimmed; unhiding brings it back for good.
        pc.setShowHidden(true)
        let back = try XCTUnwrap(pc.rows.firstIndex { $0.hideKey == hiddenRow.hideKey })
        XCTAssertTrue(pc.rows[back].isHidden)
        pc.toggleHidden(row: back)
        pc.setShowHidden(false)
        XCTAssertEqual(pc.rows.filter { !$0.isHeading }.count, all)
        XCTAssertEqual(pc.hiddenCount, 0)
    }

    // MARK: Going from a warning to its cue (flows)

    private static let missing3: ModelID = "00000000-0000-0000-0000-0000000000f1"
    private static let actTwo: ModelID = "00000000-0000-0000-0000-0000000000f2"
    private static let missing41: ModelID = "00000000-0000-0000-0000-0000000000f3"
    private static let secondList: ModelID = "00000000-0000-0000-0000-000000000002"
    private static let missingB: ModelID = "00000000-0000-0000-0000-0000000000f4"

    /// The Demo show with: cue 3 (a file that is not there) after Act One, a group Act Two (the tests shut it)
    /// holding a second broken cue, `filler` memo cues in front of cue 3, and a second cue list with
    /// one broken cue. Intro (cue 1) is where the playhead starts.
    private func flowRig(filler: Int = 0) async throws -> Rig {
        try await Rig.open(
            "Demo",
            edit: { folder in
                try editShow(folder.appendingPathComponent("Demo.openlab")) { ws in
                    var lists = ws["cue_lists"]?.arrayValue ?? []
                    guard var cues = lists.first?["cues"]?.arrayValue, let intro = cues.first,
                        let group = cues.first(where: { $0["kind"]?["type"]?.stringValue == "group" }),
                        let radio = group["kind"]?["children"]?.arrayValue?.last
                    else { return }
                    func clone(_ cue: JSONValue, _ id: String, _ number: String, _ name: String, media: String? = nil)
                        -> JSONValue
                    {
                        var c = cue
                        c.set("id", .string(id))
                        c.set("number", .string(number))
                        c.set("name", .string(name))
                        if let media, var kind = c["kind"] {
                            kind.set("media", .string(media))
                            c.set("kind", kind)
                        }
                        return c
                    }
                    for n in 0..<filler {
                        var memo = clone(
                            intro, String(format: "00000000-0000-0000-0001-%012d", n), "m\(n)", "Filler \(n)")
                        memo.set("kind", .object(["type": .string("memo")]))
                        cues.append(memo)
                    }
                    cues.append(clone(intro, Self.missing3.raw, "3", "Gone", media: "media/gone.wav"))
                    var two = clone(group, Self.actTwo.raw, "4", "Act Two")
                    var twoKind = two["kind"] ?? .object([:])
                    twoKind.set("children", .array([clone(radio, Self.missing41.raw, "4.1", "Late radio")]))
                    two.set("kind", twoKind)
                    cues.append(two)
                    lists[0].set("cues", .array(cues))
                    var other = lists[0]
                    other.set("id", .string(Self.secondList.raw))
                    other.set("name", .string("Second"))
                    other.set(
                        "cues",
                        .array([clone(intro, Self.missingB.raw, "1", "Other list gone", media: "media/gone.wav")]))
                    lists.append(other)
                    ws.set("cue_lists", .array(lists))
                }
            })
    }

    private func row(_ pc: WarningsPageController, _ cue: ModelID) throws -> Int {
        try XCTUnwrap(pc.rows.firstIndex { $0.cue == cue && !$0.isHeading }, "the report has an entry for \(cue)")
    }

    /// The cue's row is drawn in full inside the list's visible area.
    private func assertRowInView(_ wc: WorkspaceWindowController, _ cue: ModelID, line: UInt = #line) throws {
        let outline = wc.cueList.outlineView
        wc.window?.layoutIfNeeded()
        let node = try XCTUnwrap(wc.cueList.node(cue), line: line)
        let r = outline.row(forItem: node)
        XCTAssertGreaterThanOrEqual(r, 0, "the row is shown", line: line)
        XCTAssertTrue(outline.visibleRect.contains(outline.rect(ofRow: r)), "row \(r) is fully on screen", line: line)
    }

    /// The status window's real panel (the Warnings page's own window is never shown).
    private func statusPanel(_ wc: WorkspaceWindowController) throws -> NSWindow {
        let panel = try XCTUnwrap(wc.existingStatusWindow?.window)
        XCTAssertTrue(panel.isVisible, "the status window is open")
        return panel
    }

    private func flowWindow(_ rig: Rig) -> WorkspaceWindowController {
        let wc = WorkspaceWindowController(store: rig.store)
        wc.window?.makeKeyAndOrderFront(nil)
        addTeardownBlock { @MainActor in
            wc.preshowWindow.close()
            wc.window?.close()
        }
        return wc
    }

    func testTopLevelBrokenCue() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        wc.setVisible(.inspector, false)
        XCTAssertTrue(wc.canRunPreshowCheck)
        await wc.showWorkspaceStatus().value
        let pc = wc.preshowWindow
        let report = try XCTUnwrap(pc.report, "the engine answered")
        XCTAssertFalse(report.ok)
        XCTAssertEqual(pc.summaryLabel.stringValue, PreshowRows.summary(report))
        XCTAssertTrue(pc.rows.contains { $0.section == .media && $0.title.contains("gone.wav") }, "the missing file")
        XCTAssertFalse(pc.inspectButton.isEnabled, "nothing chosen yet")
        XCTAssertEqual(pc.table.doubleAction, #selector(WarningsPageController.inspectClicked))
        XCTAssertEqual(rig.store.playheads[TestSnapshots.mainList], intro)
        let r = try row(pc, Self.missing3)
        // The click on the entry, the Inspect Cue button and a double-click all end the same way.
        for how in ["click", "button", "double-click"] {
            pc.table.deselectAll(nil)
            wc.cueList.selectAsUser([intro])
            wc.setVisible(.inspector, false)
            pc.table.selectRowIndexes([r], byExtendingSelection: false)  // a click chooses the entry
            if how != "click" {
                wc.cueList.selectAsUser([intro])
                wc.setVisible(.inspector, false)
                if how == "button" {
                    pc.inspectButton.performClick(nil)
                } else {
                    XCTAssertTrue(pc.table.sendAction(pc.table.doubleAction, to: pc.table.target))
                }
            }
            XCTAssertEqual(wc.cueList.selectedIDs(), [Self.missing3], how)
            XCTAssertFalse(try statusPanel(wc).isKeyWindow, "the status panel never takes the keys: \(how)")
            XCTAssertTrue(wc.isVisible(.inspector), how)
            XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.identifier as? String, "basics", how)
            try assertRowInView(wc, Self.missing3)
            XCTAssertEqual(rig.store.playheads[TestSnapshots.mainList], intro, "the playhead stays: \(how)")
        }
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.5)
        await assertEventually("GO fired cue 1, not the warned cue") {
            rig.store.progress[intro] != nil && rig.store.progress[Self.missing3] == nil
        }
    }

    func testCueInsideAClosedGroup() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        await wc.showWorkspaceStatus().value
        let pc = wc.preshowWindow
        let outline = wc.cueList.outlineView
        func open(_ id: ModelID) -> Bool { wc.cueList.node(id).map { outline.isItemExpanded($0) } ?? false }
        // Groups come up open; the operator has shut both.
        for g in [TestSnapshots.group, Self.actTwo] { outline.collapseItem(wc.cueList.node(g)) }
        XCTAssertFalse(open(TestSnapshots.group))
        XCTAssertFalse(open(Self.actTwo))
        try await rig.advance(0)
        let playheads = rig.store.playheads
        XCTAssertTrue(pc.choose(row: try row(pc, radio)))
        XCTAssertTrue(open(TestSnapshots.group), "Act One opened")
        XCTAssertEqual(wc.cueList.selectedIDs(), [radio])
        try assertRowInView(wc, radio)
        XCTAssertFalse(open(Self.actTwo), "the other group is left shut")
        XCTAssertTrue(pc.choose(row: try row(pc, Self.missing41)))
        XCTAssertTrue(open(Self.actTwo))
        XCTAssertTrue(open(TestSnapshots.group), "the group opened earlier stays open")
        XCTAssertEqual(wc.cueList.selectedIDs(), [Self.missing41])
        try assertRowInView(wc, Self.missing41)
        try await rig.advance(0)
        XCTAssertEqual(rig.store.playheads, playheads)
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.5)
        await assertEventually("GO fired the playhead cue, not a warned one") { rig.store.progress[intro] != nil }
    }

    func testCueInAnotherCueList() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        await wc.showWorkspaceStatus().value
        let pc = wc.preshowWindow
        wc.cueList.selectAsUser([TestSnapshots.fade])
        await assertEventually("the playhead followed the selection") {
            rig.store.playheads[TestSnapshots.mainList] == TestSnapshots.fade
        }
        let playheads = rig.store.playheads
        XCTAssertEqual(wc.cueList.listID, TestSnapshots.mainList)
        XCTAssertTrue(pc.choose(row: try row(pc, Self.missingB)))
        XCTAssertEqual(wc.cueList.listID, Self.secondList, "the list shows the cue's list")
        XCTAssertEqual(wc.cueList.selectedIDs(), [Self.missingB])
        try assertRowInView(wc, Self.missingB)
        XCTAssertEqual(wc.sidebar.shownList, Self.secondList, "the sidebar follows")
        let selected = wc.sidebar.listTable.selectedRow
        XCTAssertTrue(wc.sidebar.listRows.indices.contains(selected))
        if case .list(let i) = wc.sidebar.listRows[selected] {
            XCTAssertEqual(rig.store.model?.cueLists[i].id, Self.secondList, "the sidebar highlights it")
        } else {
            XCTFail("the sidebar highlights a list row")
        }
        try await rig.advance(0)
        XCTAssertEqual(rig.store.playheads, playheads, "no playhead moved")
        // Back on list A: its playhead is where it was, and its playhead cue is the selected one.
        wc.cueList.showList(TestSnapshots.mainList)
        XCTAssertEqual(wc.cueList.listID, TestSnapshots.mainList)
        try await rig.advance(0)
        XCTAssertEqual(rig.store.playheads, playheads)
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.fade])
    }

    func testShowMode() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        wc.setVisible(.inspector, false)
        wc.setShowMode(true)
        await assertEventually("Show Mode is on") { wc.isShowMode }
        await wc.showWorkspaceStatus().value
        let pc = wc.preshowWindow
        XCTAssertTrue(pc.rows.contains { $0.cue == Self.missing3 }, "warnings are listed in Show Mode")
        pc.table.selectRowIndexes([try row(pc, Self.missing3)], byExtendingSelection: false)
        wc.cueList.selectAsUser([intro])
        pc.inspectButton.performClick(nil)
        XCTAssertEqual(wc.cueList.selectedIDs(), [Self.missing3])
        XCTAssertFalse(wc.isVisible(.inspector))
        XCTAssertTrue(wc.isShowMode)
        XCTAssertEqual(rig.store.playheads[TestSnapshots.mainList], intro)
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.5)
        await assertEventually("GO fired the playhead cue") { rig.store.progress[intro] != nil }
    }

    func testEntryThatNamesNoCue() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        wc.setVisible(.inspector, false)
        let pc = wc.preshowWindow
        var beeps = 0
        pc.beep = { beeps += 1 }
        pc.show(
            try report(
                #"{"ok":false,\#(Self.emptySummary),"broken_cues":[],"media":[],"#
                    + #""patches":[{"patch":"00000000-0000-0000-0000-0000000000a9","name":"Spare","mapped":false,"resolution":{"type":"unmapped"},"cues":[]}],"#
                    + #""network":[],"system":[{"type":"notice","code":"preload_over_budget","message":"over"}]}"#))
        wc.cueList.selectAsUser([TestSnapshots.fade])
        let entries = pc.rows.indices.filter { !pc.rows[$0].isHeading }
        XCTAssertEqual(entries.count, 2)
        for r in entries {
            pc.table.selectRowIndexes([r], byExtendingSelection: false)
            XCTAssertNil(pc.rows[r].cue)
            XCTAssertFalse(pc.inspectButton.isEnabled, "no cue to inspect")
            XCTAssertFalse(pc.choose(row: r))
            // A double-click runs the action even with the button disabled: it must do nothing.
            XCTAssertTrue(pc.table.sendAction(pc.table.doubleAction, to: pc.table.target))
            XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.fade])
            XCTAssertFalse(wc.isVisible(.inspector))
        }
        XCTAssertEqual(beeps, 0, "no beep")
    }

    func testListScrolledWindowNarrow() async throws {
        let rig = try await flowRig(filler: 200)
        let wc = flowWindow(rig)
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(x: 0, y: 0, width: window.minSize.width, height: 600), display: true)
        await wc.showWorkspaceStatus().value
        let pc = wc.preshowWindow
        let outline = wc.cueList.outlineView
        window.layoutIfNeeded()
        outline.scrollRowToVisible(0)
        let target = try XCTUnwrap(wc.cueList.node(Self.missing3))
        let rowOfTarget = outline.row(forItem: target)
        XCTAssertGreaterThan(rowOfTarget, 150)
        XCTAssertFalse(outline.visibleRect.contains(outline.rect(ofRow: rowOfTarget)), "off screen to start")
        pc.table.selectRowIndexes([try row(pc, Self.missing3)], byExtendingSelection: false)
        XCTAssertEqual(wc.cueList.selectedIDs(), [Self.missing3])
        try assertRowInView(wc, Self.missing3)
        XCTAssertFalse(try statusPanel(wc).isKeyWindow, "the status panel does not take the keys")
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.5)
        await assertEventually("GO fired") { rig.store.progress[intro] != nil }
    }

    func testWarningsCueDeletedWhileTheListIsOpen() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        await wc.showWorkspaceStatus().value
        let pc = wc.preshowWindow
        var beeps = 0
        pc.beep = { beeps += 1 }
        let stale = try row(pc, Self.missing3)
        wc.cueList.selectAsUser([TestSnapshots.fade])
        guard case .success = await rig.store.send(command: .removeCue(Self.missing3)) else {
            return XCTFail("delete")
        }
        await assertEventually("the app's model dropped the cue") { rig.store.model?.cue(Self.missing3) == nil }
        pc.table.selectRowIndexes([stale], byExtendingSelection: false)
        pc.inspectClicked()
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.fade], "nothing is selected for a deleted cue")
        XCTAssertEqual(beeps, 2, "the click and the button each say nothing happened")
        // The next check no longer lists it.
        await pc.runCheck()
        XCTAssertFalse(pc.rows.contains { $0.cue == Self.missing3 })
        // A selection on the old report never reaches the rebuilt list.
        pc.table.selectRowIndexes([try row(pc, Self.missing41)], byExtendingSelection: false)
        wc.cueList.selectAsUser([TestSnapshots.fade])
        await pc.runCheck()
        XCTAssertEqual(pc.table.selectedRow, -1)
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.fade])
    }
}
