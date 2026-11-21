// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The Warnings page of the status window: live rows (no Check Again, no snapshot), hide and show
/// hidden, and the click-through to the cue. Real engine; the status window's own tests are in
/// `StatusWindowTests`.
@MainActor
final class WarningsPageTests: XCTestCase {
    private let intro = TestSnapshots.intro
    private let radio = TestSnapshots.radio

    private func realRig() async throws -> Rig { try await Rig.open() }

    // MARK: Live list

    /// Every button title and label on the page.
    private func words(_ v: NSView) -> [String] {
        var out: [String] = []
        if let b = v as? NSButton { out += [b.title, b.accessibilityLabel() ?? ""] }
        if let t = v as? NSTextField { out.append(t.stringValue) }
        return out + v.subviews.flatMap(words)
    }

    /// Flow: Status > Warnings with a missing file → the list shows its cue; the file is fixed, the row
    /// goes; broken again, it comes back; nothing is pressed, and the page has no Check Again button.
    func testTheListIsLiveAndHasNoCheckAgainButton() async throws {
        let rig = try await realRig()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.showWindow(nil)
        addTeardownBlock { @MainActor in wc.window?.close() }
        await wc.showWorkspaceStatus().value
        let page = wc.warningsPage
        XCTAssertTrue(page.rows.contains { $0.cue == radio }, "the missing file's cue is listed")
        XCTAssertEqual(page.summaryLabel.stringValue, String(format: loc("warnings.one"), 1))
        let content = try XCTUnwrap(wc.existingStatusWindow?.tabs)
        XCTAssertFalse(words(content).contains { $0.localizedCaseInsensitiveContains("check again") })

        guard case .success = await rig.store.send(command: .setProperty(radio, .media("media/d.wav"))) else {
            return XCTFail("fix")
        }
        await assertEventually("the fixed cue's row goes") {
            try? await rig.advance()
            return !page.rows.contains { $0.cue == self.radio }
        }
        XCTAssertEqual(page.summaryLabel.stringValue, loc("warnings.none"))
        XCTAssertEqual(page.table.numberOfRows, 0)
        guard case .success = await rig.store.send(command: .setProperty(radio, .media("media/gone.wav"))) else {
            return XCTFail("break")
        }
        await assertEventually("a new problem's row appears") {
            try? await rig.advance()
            return page.rows.contains { $0.cue == self.radio }
        }
        XCTAssertEqual(page.table.numberOfRows, page.rows.count)
    }

    /// Flow: the page is opened for the first time with a warning already there → it lists it at once,
    /// drawn, without waiting for anything to change.
    func testThePageShowsTheCurrentWarningsTheMomentItIsMade() async throws {
        let rig = try await realRig()
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertNil(wc.existingStatusWindow)
        let page = wc.warningsPage
        let row = try XCTUnwrap(page.rows.firstIndex { $0.cue == radio }, "Radio's missing file is listed")
        XCTAssertEqual(page.table.numberOfRows, page.rows.count)
        let cell = try XCTUnwrap(page.table.view(atColumn: 0, row: row, makeIfNecessary: true) as? WarningCell)
        XCTAssertEqual(cell.title.stringValue, "2.3  Radio")
        XCTAssertEqual(page.summaryLabel.stringValue, String(format: loc("warnings.one"), 1))
    }

    /// Flow: Show Mode, a warning arrives → nothing modal: no sheet, no modal window; the footer's count
    /// changes and the open Status window lists it.
    func testAWarningInShowModeRaisesNothingModalAndShowsInTheFooterAndThePage() async throws {
        let rig = try await Rig.open("Second")
        let wc = WorkspaceWindowController(store: rig.store)
        wc.showWindow(nil)
        addTeardownBlock { @MainActor in wc.window?.close() }
        wc.setShowMode(true)
        await assertEventually { wc.isShowMode }
        await wc.showWorkspaceStatus().value
        let page = wc.warningsPage
        XCTAssertTrue(page.rows.isEmpty)
        XCTAssertTrue(wc.footer.warningsButton.isHidden)
        let late = #"{"type":"late_start","cue":"\#(TestSnapshots.door)","late_ms":120}"#
        XCTAssertEqual(
            rig.store.receive(try ServerMessage.decode(#"{"type":"warnings","warnings":[\#(late)]}"#)), .applied)
        XCTAssertNil(NSApp.modalWindow)
        XCTAssertNil(wc.window?.attachedSheet)
        XCTAssertTrue(wc.isShowMode)
        XCTAssertEqual(wc.footer.warningsButton.title, "1")
        XCTAssertEqual(page.rows.count, 1, "the open Status window lists it")
        XCTAssertEqual(page.summaryLabel.stringValue, String(format: loc("warnings.one"), 1))
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
    /// make the list key, and GO would stop reaching the workspace window.
    func testNothingInThePanelMakesItKey() async throws {
        let rig = try await realRig()
        let pc = WarningsPageController(store: rig.store)
        pc.show(WarningRows.make(store: rig.store))
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

    /// Flow: hide a row → it leaves the list and the summary counts it hidden; Show hidden brings it
    /// back dimmed; unhide returns it for good. A changed problem on the same cue is a new row.
    func testHidingARowDropsItFromTheListAndSaysSoUntilUnhidden() async throws {
        let rig = try await realRig()
        let pc = WarningsPageController(store: rig.store)
        pc.show(WarningRows.make(store: rig.store))
        let count = pc.rows.count
        XCTAssertGreaterThan(count, 0)
        let row = try XCTUnwrap(pc.rows.firstIndex { $0.cue == radio })
        let hidden = pc.rows[row]
        pc.toggleHidden(row: row)
        XCTAssertEqual(pc.rows.count, count - 1)
        XCTAssertFalse(pc.rows.contains { $0.id == hidden.id })
        let summary = String(format: loc(count == 1 ? "warnings.one" : "warnings.many"), count)
        XCTAssertEqual(pc.summaryLabel.stringValue, String(format: loc("warnings.summary.hidden"), summary, 1))
        pc.setShowHidden(true)
        let back = try XCTUnwrap(pc.rows.firstIndex { $0.id == hidden.id })
        XCTAssertTrue(pc.isHidden(pc.rows[back]))
        pc.table.selectRowIndexes([back], byExtendingSelection: false)
        XCTAssertEqual(pc.hideButton.title, loc("warnings.unhide"))
        pc.toggleHidden(row: back)
        pc.setShowHidden(false)
        XCTAssertEqual(pc.rows.count, count)
        XCTAssertEqual(pc.summaryLabel.stringValue, summary)
        // A hidden problem that changes is a new one: it is listed again.
        pc.toggleHidden(row: try XCTUnwrap(pc.rows.firstIndex { $0.id == hidden.id }))
        XCTAssertFalse(pc.rows.contains { $0.id == hidden.id })
        var changed = pc.all
        let i = try XCTUnwrap(changed.firstIndex { $0.id == hidden.id })
        changed[i] = WarningRow(
            id: hidden.id, cue: hidden.cue, title: hidden.title, detail: hidden.detail + " (another reason)",
            canRelink: hidden.canRelink)
        pc.show(changed)
        XCTAssertTrue(pc.rows.contains { $0.id == hidden.id }, "the changed problem is not the one that was hidden")
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
        try XCTUnwrap(pc.rows.firstIndex { $0.cue == cue }, "the list has a row for \(cue)")
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
            wc.warningsPage.close()
            wc.window?.close()
        }
        return wc
    }

    func testTopLevelBrokenCue() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        wc.setVisible(.inspector, false)
        await wc.showWorkspaceStatus().value
        let pc = wc.warningsPage
        XCTAssertTrue(
            pc.rows.contains { $0.cue == Self.missing3 && $0.detail.contains("gone.wav") }, "the missing file")
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
        let pc = wc.warningsPage
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
        let pc = wc.warningsPage
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
        let pc = wc.warningsPage
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

    func testRowThatNamesNoCue() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        wc.setVisible(.inspector, false)
        let pc = wc.warningsPage
        let notices =
            #"{"type":"notice","code":"patch_no_system_output","message":"x"},"#
            + #"{"type":"notice","code":"device_lost","message":"y","device":"Interface"}"#
        XCTAssertEqual(
            rig.store.receive(try ServerMessage.decode(#"{"type":"warnings","warnings":[\#(notices)]}"#)), .applied)
        wc.cueList.selectAsUser([TestSnapshots.fade])
        let entries = pc.rows.indices.filter { pc.rows[$0].cue == nil }
        XCTAssertGreaterThanOrEqual(entries.count, 2)
        for r in entries {
            pc.table.selectRowIndexes([r], byExtendingSelection: false)
            XCTAssertFalse(pc.inspectButton.isEnabled, "no cue to inspect")
            XCTAssertFalse(pc.choose(row: r))
            // A double-click runs the action even with the button disabled: it must do nothing.
            XCTAssertTrue(pc.table.sendAction(pc.table.doubleAction, to: pc.table.target))
            XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.fade])
            XCTAssertFalse(wc.isVisible(.inspector))
        }
    }

    func testListScrolledWindowNarrow() async throws {
        let rig = try await flowRig(filler: 200)
        let wc = flowWindow(rig)
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(x: 0, y: 0, width: window.minSize.width, height: 600), display: true)
        await wc.showWorkspaceStatus().value
        let pc = wc.warningsPage
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

    /// A cue deleted while the list is open takes its row with it, with no check to run.
    func testACueDeletedWhileTheListIsOpenLeavesTheList() async throws {
        let rig = try await flowRig()
        let wc = flowWindow(rig)
        await wc.showWorkspaceStatus().value
        let pc = wc.warningsPage
        let count = pc.rows.count
        pc.table.selectRowIndexes([try row(pc, Self.missing3)], byExtendingSelection: false)
        wc.cueList.selectAsUser([TestSnapshots.fade])
        guard case .success = await rig.store.send(command: .removeCue(Self.missing3)) else {
            return XCTFail("delete")
        }
        await assertEventually("the row went") { !pc.rows.contains { $0.cue == Self.missing3 } }
        XCTAssertEqual(pc.rows.count, count - 1)
        XCTAssertEqual(pc.table.selectedRow, -1, "a selection on the gone row is dropped")
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.fade], "and no cue was selected for it")
    }
}
