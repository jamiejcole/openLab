// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// SM-06, NFR-07: the pre-show check window, its report rows and the click-through to the cue.
@MainActor
final class PreshowTests: XCTestCase {
    private let intro = CueListHarness.intro
    private let radio = CueListHarness.radio

    private func demoRig() async throws -> Rig {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        return rig
    }

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
        XCTAssertEqual(broken.map(\.cue), [intro, CueListHarness.fade])
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
            #"{"ok":false,\#(Self.emptySummary),"broken_cues":[{"cue":"\#(gone)","list":"\#(CueListHarness.list)","number":"9","name":"Thunder","problems":[{"type":"no_media"}]}],"#
                + #""media":[],"patches":[],"network":[],"system":[]}"#)
        let pc = PreshowWindowController(store: rig.store)
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

    func testCheckClickThroughRevealsTheCueAndOpensTheBasicsTab() async throws {
        let rig = try await demoRig()
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.setVisible(.inspector, false)
        let playheads = rig.store.playheads
        XCTAssertTrue(wc.canRunPreshowCheck)
        await wc.showPreshowCheck().value
        let pc = wc.preshowWindow
        XCTAssertEqual(pc.summaryLabel.stringValue, String(format: loc("preshow.summary.many"), 2))
        let row = try XCTUnwrap(pc.rows.firstIndex { $0.cue == radio && $0.section == .brokenCues })
        XCTAssertTrue(pc.choose(row: row))
        // Radio sits inside the collapsed group Act One: the group opens and Radio is selected.
        XCTAssertEqual(wc.cueList.selectedIDs(), [radio])
        XCTAssertTrue(wc.isVisible(.inspector))
        XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.identifier as? String, "basics")
        // A row in a report must never move what GO fires next.
        XCTAssertEqual(rig.store.playheads, playheads)
        wc.preshowWindow.close()
    }

    func testClickThroughInShowModeSelectsButLeavesTheInspector() async throws {
        let rig = try await demoRig()
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.setVisible(.inspector, false)
        wc.setShowMode(true)
        await wc.showPreshowCheck().value
        let pc = wc.preshowWindow
        let row = try XCTUnwrap(pc.rows.firstIndex { $0.cue == radio })
        XCTAssertTrue(pc.choose(row: row))
        XCTAssertEqual(wc.cueList.selectedIDs(), [radio])
        XCTAssertFalse(wc.isVisible(.inspector))
        pc.close()
    }

    func testDisconnectedEngineGreysTheItemAndClearsTheReport() async throws {
        let rig = try await demoRig()
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let wc = WorkspaceWindowController(store: rig.store)
        let pc = wc.preshowWindow
        await pc.runCheck()
        XCTAssertFalse(pc.rows.isEmpty)
        XCTAssertTrue(pc.checkButton.isEnabled)

        rig.engine.dropConnections()
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
        let rig = try await demoRig()
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let pc = PreshowWindowController(store: rig.store)
        await pc.runCheck()
        XCTAssertFalse(pc.rows.isEmpty)
        rig.engine.garbleResults = true
        await pc.runCheck()
        XCTAssertEqual(pc.rows, [])
        XCTAssertEqual(pc.summaryLabel.stringValue, loc("preshow.failed"))
    }

    func testAnOlderAnswerDoesNotReplaceANewerCheck() async throws {
        let rig = try await demoRig()
        let pc = PreshowWindowController(store: rig.store)
        rig.engine.holdResults = true
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let first = Task { await pc.runCheck() }
        await settle()
        XCTAssertTrue(pc.checking)
        XCTAssertFalse(pc.checkButton.isEnabled)
        rig.engine.preshowProblemDocs = []
        let second = Task { await pc.runCheck() }
        await settle()
        rig.engine.holdResults = false
        // The newer (all-clear) answer lands first; the older one with problems must not replace it.
        rig.engine.releaseResults(reversed: true)
        await first.value
        await second.value
        XCTAssertFalse(pc.checking)
        XCTAssertTrue(pc.checkButton.isEnabled)
        XCTAssertEqual(pc.report?.ok, true)
        XCTAssertEqual(pc.rows, [])
    }

    func testThePanelClosesWithItsWorkspaceAndNamesIt() async throws {
        let rig = try await demoRig()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.workspaceName = "Lear"
        wc.showWindow(nil)
        await wc.showPreshowCheck().value
        let panel = try XCTUnwrap(wc.preshowWindow.window)
        XCTAssertTrue(panel.isVisible)
        XCTAssertEqual(panel.title, String(format: loc("preshow.title.workspace"), "Lear"))
        wc.window?.close()
        XCTAssertFalse(panel.isVisible)
    }

    func testCheckAgainClearsTheSelectionAndRevealsNothing() async throws {
        let rig = try await demoRig()
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let pc = PreshowWindowController(store: rig.store)
        var inspected: [ModelID] = []
        pc.onInspect = { id, _ in inspected.append(id) }
        await pc.runCheck()
        let row = try XCTUnwrap(pc.rows.firstIndex { $0.cue == radio })
        pc.table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        XCTAssertEqual(inspected, [radio])
        await pc.runCheck()
        XCTAssertEqual(pc.table.selectedRow, -1)
        XCTAssertEqual(inspected, [radio], "a new report reveals nothing by itself")
    }

    /// A click on any view that needs its panel to be key (an NSTableView does by default) would
    /// make the report key, and GO would stop reaching the workspace window.
    func testNothingInThePanelMakesItKey() async throws {
        let rig = try await demoRig()
        rig.engine.preshowProblemDocs = Set(rig.engine.openDocs)
        let pc = PreshowWindowController(store: rig.store)
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
        let item = MainMenu.make().windows.items.first { $0.action == #selector(AppDelegate.showPreshowCheck(_:)) }
        XCTAssertEqual(item?.title, loc("menu.window.preshowCheck"))
        XCTAssertEqual(item?.keyEquivalent, "W")
    }
}
