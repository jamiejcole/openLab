// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class MainWindowTests: XCTestCase {
    /// The rigs of tests that build their own show, kept alive for the test.
    private var retained: [Rig] = []

    private func ax(_ root: NSView, _ id: String) -> NSView? {
        if root.accessibilityIdentifier() == id { return root }
        for sub in root.subviews { if let hit = ax(sub, id) { return hit } }
        return nil
    }

    private func controller() throws -> WorkspaceWindowController {
        WorkspaceWindowController(store: try Rig.offline().store)
    }

    func testAllAreasPresent() throws {
        let wc = try controller()
        let root = try XCTUnwrap(wc.window?.contentView)
        for id in [
            AX.masthead, AX.goButton, AX.standbyField, AX.standbyNotes, AX.cueToolbar, AX.cueList,
            AX.inspector, AX.sidebar, AX.sidebarTabs, AX.transportReset, AX.transportPause,
            AX.transportResume, AX.transportPanic, AX.footer, AX.modeSwitch, AX.statusLabel,
            AX.settingsButton, AX.toggleToolbar, AX.toggleMasthead, AX.toggleInspector, AX.toggleSidebar,
        ] {
            XCTAssertNotNil(ax(root, id), id)
        }
        XCTAssertFalse(wc.masthead.goButton.accessibilityLabel()?.isEmpty ?? true)
    }

    func testOutlineColumnsInQLabOrder() throws {
        let outline = try controller().cueList.outlineView
        XCTAssertEqual(
            outline.tableColumns.map(\.identifier.rawValue),
            ["status", "type", "number", "name", "target", "preWait", "duration", "postWait", "continue"])
        XCTAssertTrue(outline.usesAlternatingRowBackgroundColors)
    }

    func testSidebarHiddenByDefault() throws {
        let wc = try controller()
        XCTAssertFalse(wc.isVisible(.sidebar))
        XCTAssertEqual(wc.footer.toggles[.sidebar]?.state, .off)
        for area in [LayoutArea.toolbar, .masthead, .inspector] { XCTAssertTrue(wc.isVisible(area)) }
    }

    func testEachFooterToggleHidesAndReshowsItsArea() throws {
        let wc = try controller()
        for area in LayoutArea.allCases {
            let button = try XCTUnwrap(wc.footer.toggles[area])
            let before = wc.isVisible(area)
            button.performClick(nil)
            XCTAssertEqual(wc.isVisible(area), !before, "\(area) first click")
            button.performClick(nil)
            XCTAssertEqual(wc.isVisible(area), before, "\(area) second click")
        }
    }

    func testInspectorShowsNoCueSelected() throws {
        let wc = try controller()
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, "No cue selected")
        XCTAssertFalse(wc.inspector.emptyLabel.isHidden)
    }

    func testFooterCountsCuesAndListsAndFollowsAddsAndDeletes() async throws {
        let rig = try Rig.offline(TestSnapshots.show())
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertEqual(wc.footer.countLabel.stringValue, "6 cues in 1 list")
        let id = "00000000-0000-0000-0000-0000000000d1"
        let cue = TestSnapshots.cueJSON(id: id, number: nil, name: "New", kind: TestSnapshots.audioKind("/tmp/a.wav"))
        let add =
            #"{"type":"cue","list":"\#(TestSnapshots.mainList)","parent":null,"index":0,"cue":\#(try cue.text())}"#
        let revision = rig.store.revision + 1
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(revision),"upserts":[\#(add)],"deletes":[]}"#)),
            .applied)
        XCTAssertEqual(wc.footer.countLabel.stringValue, "7 cues in 1 list")
        // Deleting a group takes its children with it.
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(revision + 1),"upserts":[],"deletes":[{"type":"cue","id":"\#(TestSnapshots.group)"}]}"#
                )), .applied)
        XCTAssertEqual(wc.footer.countLabel.stringValue, "3 cues in 1 list")
    }

    func testCountTextPluralForms() {
        XCTAssertEqual(FooterBar.countText(cues: 1, lists: 1), "1 cue in 1 list")
        XCTAssertEqual(FooterBar.countText(cues: 0, lists: 2), "0 cues in 2 lists")
    }

    func testEveryIconAndFallbackResolves() {
        for icon in Icon.allCases {
            let e = icon.entry
            XCTAssertNotNil(NSImage(systemSymbolName: e.symbol, accessibilityDescription: nil), "\(icon) \(e.symbol)")
            XCTAssertTrue(icon.resolvesToSymbol, "\(icon)")
            if let fb = e.fallback {
                XCTAssertNotNil(NSImage(systemSymbolName: fb, accessibilityDescription: nil), "\(icon) fallback \(fb)")
            }
            // Deployment target is macOS 12: a newer symbol must come with a fallback, and no row
            // may need more than 12 without one.
            XCTAssertLessThanOrEqual(e.minMacOS, 12, "\(icon) needs macOS \(e.minMacOS)")
            if e.minMacOS > 10 { XCTAssertNotNil(e.fallback, "\(icon) has no fallback") }
        }
        // The call sites that expose their icons use only table rows.
        XCTAssertFalse((CueToolbarView.icons + SidebarViewController.icons + FooterBar.icons).isEmpty)
    }

    func testMissingSymbolFallsBackThenShowsTheLocalisedNameAndLogsOnce() {
        defer { IconLookup.shared.setLookup(nil) }
        // The primary name vanishes: the fallback symbol shows.
        IconLookup.shared.setLookup { $0 != "gearshape" }
        XCTAssertNotNil(Icon.settings.image("Settings").accessibilityDescription)
        XCTAssertTrue(Icon.settings.resolvesToSymbol)
        XCTAssertEqual(IconLookup.shared.reportedNames, ["gearshape"])
        // Both vanish: a text image carrying the label, logged once per name.
        IconLookup.shared.setLookup { _ in false }
        let text = Icon.settings.image("Settings")
        XCTAssertEqual(text.accessibilityDescription, "Settings")
        XCTAssertTrue(text.isTemplate)
        XCTAssertGreaterThan(text.size.width, Icon.settings.image("S", compact: true).size.width)
        _ = Icon.settings.image("Settings")
        XCTAssertEqual(IconLookup.shared.reportedNames, ["gearshape"])
        _ = Icon.kindAudio.image("Audio")
        XCTAssertEqual(IconLookup.shared.reportedNames, ["gearshape", "speaker.wave.2"])
        XCTAssertFalse(Icon.settings.resolvesToSymbol)
    }

    func testEveryDottedStringLiteralInSourcesIsLocalised() throws {
        let dir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("openLab")
        let files = try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        XCTAssertFalse(files.isEmpty)
        let regex = try NSRegularExpression(pattern: "\"([a-z][A-Za-z]*(?:\\.[A-Za-z]+)+)\"")
        let bundle = Bundle(for: AppDelegate.self)
        var seen = 0
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for m in regex.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                guard let r = Range(m.range(at: 1), in: text) else { continue }
                let key = String(text[r])
                // SF Symbol names share the dotted shape; they are checked in testSymbolsResolve.
                if NSImage(systemSymbolName: key, accessibilityDescription: nil) != nil { continue }
                seen += 1
                XCTAssertNotEqual(
                    bundle.localizedString(forKey: key, value: "\u{1}", table: nil), "\u{1}",
                    "\(key) missing from Localizable.strings (\(file.lastPathComponent))")
            }
        }
        XCTAssertGreaterThan(seen, 30)
    }

    // MARK: Masthead standby and notes

    /// The demo show on the real engine, its wire recorded.
    private func demoController() async throws -> (WorkspaceWindowController, Rig) {
        let rig = try await Rig.open()
        return (WorkspaceWindowController(store: rig.store), rig)
    }

    /// The engine moves the playhead to `cue` (another window, an OSC client) and this one hears it.
    private func standBy(_ rig: Rig, _ cue: ModelID) async throws {
        guard case .success = await rig.store.transport(.setPlayhead(list: TestSnapshots.mainList, cue: cue)) else {
            return XCTFail("set_playhead")
        }
        try await rig.advance()
        await assertEventually { rig.store.playheads[TestSnapshots.mainList] == cue }
    }

    private func setNotesCommands(_ rig: Rig) -> [JSONValue] {
        rig.wire.commands.compactMap { c in c.json["property"]?["type"]?.stringValue == "notes" ? c.json : nil }
    }

    private func notes(_ rig: Rig, _ cue: ModelID) -> String? {
        rig.store.model?.cue(cue)?.raw["notes"]?.stringValue
    }

    func testMastheadShowsStandbyCueAsNumberDotNameWithItsNotes() async throws {
        let (wc, rig) = try await demoController()
        // The show's playhead (its first cue) is shown from the start.
        XCTAssertEqual(wc.masthead.cueID, TestSnapshots.intro)
        let cue = try XCTUnwrap(rig.store.model?.cue(TestSnapshots.intro))
        XCTAssertEqual(
            wc.masthead.standbyField.stringValue,
            [cue.number, cue.listName].compactMap { $0 }.joined(separator: " \u{00B7} "))
        XCTAssertTrue(wc.masthead.notesView.isEditable)
        // An unnumbered, unnamed cue: its name alone, from its kind.
        try await standBy(rig, TestSnapshots.fade)
        XCTAssertEqual(wc.masthead.cueID, TestSnapshots.fade)
        XCTAssertEqual(wc.masthead.standbyField.stringValue, "Fade")
    }

    func testMastheadNotesEditSendsTheInspectorNotesCommand() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        typeMastheadNotes(wc, "Hold for applause")
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await assertEventually { !self.setNotesCommands(rig).isEmpty }
        XCTAssertEqual(setNotesCommands(rig).last?["id"]?.stringValue, TestSnapshots.intro.raw)
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "Hold for applause")
        await assertEventually("the engine stored them") { self.notes(rig, TestSnapshots.intro) == "Hold for applause" }
    }

    func testMastheadNotesAreReadOnlyAndSendNothingInShowMode() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        wc.setShowMode(true)
        await assertEventually { wc.masthead.isLocked }
        XCTAssertFalse(wc.masthead.notesView.isEditable)
        XCTAssertFalse(wc.masthead.notesView.isSelectable, "a focused text view would swallow GO")
        wc.masthead.notesView.string = "typed anyway"
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await wc.editor.setNotes(of: TestSnapshots.intro, text: "typed anyway")
        await settle()
        XCTAssertTrue(setNotesCommands(rig).isEmpty)
        XCTAssertEqual(notes(rig, TestSnapshots.intro), "")
    }

    private func typeMastheadNotes(_ wc: WorkspaceWindowController, _ text: String) {
        wc.masthead.notesView.string = text
        wc.masthead.textDidBeginEditing(Notification(name: NSText.didBeginEditingNotification))
    }

    func testPlayheadMovingMidEditCommitsTheNotesToTheCueTheyWereTypedFor() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        typeMastheadNotes(wc, "for intro")
        try await standBy(rig, TestSnapshots.fade)
        await assertEventually { !self.setNotesCommands(rig).isEmpty }
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await Task.yield()
        let sent = setNotesCommands(rig)
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent.first?["id"]?.stringValue, TestSnapshots.intro.raw)
        XCTAssertEqual(sent.first?["property"]?["value"]?.stringValue, "for intro")
        XCTAssertNotEqual(wc.masthead.notesView.string, "for intro", "the new cue shows its own notes")
        await assertEventually { self.notes(rig, TestSnapshots.intro) == "for intro" }
        XCTAssertEqual(notes(rig, TestSnapshots.fade), "")
    }

    func testEnteringShowModeMidEditCommitsTheMastheadNotes() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        typeMastheadNotes(wc, "typed before show mode")
        await wc.requestShowMode(true)
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "typed before show mode")
        await assertEventually("the engine stored them") {
            self.notes(rig, TestSnapshots.intro) == "typed before show mode"
        }
    }

    func testSaveMidEditCommitsTheMastheadNotes() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        typeMastheadNotes(wc, "typed before save")
        let ok = await wc.commitPendingEdits()
        XCTAssertTrue(ok)
        XCTAssertEqual(setNotesCommands(rig).count, 1)
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "typed before save")
        await assertEventually("the engine stored them") { self.notes(rig, TestSnapshots.intro) == "typed before save" }
    }

    func testTypingForTheNewCueAfterAMidEditPlayheadMoveIsCommittedToIt() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        typeMastheadNotes(wc, "for intro")
        try await standBy(rig, TestSnapshots.fade)
        XCTAssertFalse(window.firstResponder === wc.masthead.notesView, "focus leaves so a new session starts")
        // The operator clicks back in and types for the new cue.
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        typeMastheadNotes(wc, "for fade")
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await assertEventually { self.setNotesCommands(rig).count == 2 }
        let sent = setNotesCommands(rig)
        XCTAssertEqual(sent.last?["id"]?.stringValue, TestSnapshots.fade.raw)
        XCTAssertEqual(sent.last?["property"]?["value"]?.stringValue, "for fade")
        await assertEventually { self.notes(rig, TestSnapshots.fade) == "for fade" }
        XCTAssertEqual(notes(rig, TestSnapshots.intro), "for intro")
    }

    func testNotesForADeletedCueReportInsteadOfVanishing() async throws {
        let (wc, _) = try await demoController()
        var notices: [String] = []
        wc.editor.notify = { notices.append($0) }
        let saved = await wc.editor.setNotes(of: "00000000-0000-0000-0000-0000000000ff", text: "x")
        XCTAssertFalse(saved)
        XCTAssertEqual(notices, [loc("masthead.notes.cueGone")])
    }

    func testEscapeInMastheadNotesCommitsAndReturnsFocus() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        typeMastheadNotes(wc, "typed then escape")
        let handled = wc.masthead.textView(
            wc.masthead.notesView, doCommandBy: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertTrue(handled)
        XCTAssertFalse(window.firstResponder === wc.masthead.notesView)
        await assertEventually { !self.setNotesCommands(rig).isEmpty }
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "typed then escape")
        XCTAssertFalse(
            wc.masthead.textView(wc.masthead.notesView, doCommandBy: #selector(NSResponder.insertNewline(_:))))
    }

    // MARK: Find (ED-09)

    private enum F {
        static let thunder: ModelID = "00000000-0000-0000-0000-0000000000a1"
        static let wind: ModelID = "00000000-0000-0000-0000-0000000000a2"
        static let group: ModelID = "00000000-0000-0000-0000-0000000000a3"
        static let door: ModelID = "00000000-0000-0000-0000-0000000000a4"
        static let zebra: ModelID = "00000000-0000-0000-0000-0000000000a5"
        static let other: ModelID = "00000000-0000-0000-0000-0000000000a7"
        static let net: ModelID = "00000000-0000-0000-0000-0000000000a6"
    }

    /// A show with a name, a number, notes, a file and a type to find, two of them inside a group.
    private func findController() async throws -> WorkspaceWindowController {
        func cue(_ id: ModelID, _ number: String?, _ name: String, _ notes: String, _ kind: JSONValue) -> JSONValue {
            var c = TestSnapshots.cueJSON(id: id.raw, number: number, name: name, kind: kind)
            c.set("notes", .string(notes))
            return c
        }
        let wait = JSONValue.object(["type": .string("wait"), "duration": .double(5)])
        let memo = JSONValue.object(["type": .string("memo")])
        let network = JSONValue.object([
            "type": .string("network"), "destination": .string("00000000-0000-0000-0000-000000000001"),
            "address": .string("/lx/go"),
            "args": .array([
                .object(["type": .string("string"), "value": .string("cuelist")]),
                .object(["type": .string("int"), "value": .int(42)]),
            ]),
        ])
        let kids = [cue(F.door, "3.1", "Door", "", wait), cue(F.zebra, "3.2", "Zebra", "", memo)]
        var snapshot = TestSnapshots.emptySnapshot(name: "Find")
        snapshot["workspace"]?.set(
            "cue_lists",
            .array([
                .object([
                    "id": .string(TestSnapshots.mainList.raw), "name": .string("Main"),
                    "cues": .array([
                        cue(F.thunder, "1", "Opening Light", "", TestSnapshots.audioKind("/show/media/Thunder.wav")),
                        cue(F.wind, "2", "Cue", "bring up the Wind chimes before the door", wait),
                        cue(F.group, "3", "Act Two", "", TestSnapshots.groupKind(kids)),
                        cue(F.net, "4", "Lights", "", network),
                    ]),
                ]),
                .object([
                    "id": .string("00000000-0000-0000-0000-0000000000b1"), "name": .string("Second"),
                    "cues": .array([cue(F.other, "1", "Back door", "", memo)]),
                ]),
            ]))
        let rig = try Rig.offline(snapshot)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        retained.append(rig)
        return wc
    }

    private func find(_ wc: WorkspaceWindowController, _ text: String) -> [ModelID] {
        wc.setFindQuery(text)
        return wc.find.matches
    }

    func testFindMatchesNameNumberNotesTypeAndFileIgnoringCase() async throws {
        let wc = try await findController()
        wc.openFind()
        XCTAssertEqual(find(wc, "OPENING light"), [F.thunder], "name")
        XCTAssertEqual(find(wc, "3.2"), [F.zebra], "number")
        XCTAssertEqual(find(wc, "wind CHIMES"), [F.wind], "notes")
        XCTAssertEqual(find(wc, "thunder.wav"), [F.thunder], "audio file name")
        XCTAssertEqual(find(wc, "/show/media"), [], "the folder is not the file name")
        XCTAssertEqual(find(wc, "MEMO"), [], "the cue type is not searched (QLab manual p. 153)")
        XCTAssertEqual(find(wc, "lx/GO"), [F.net], "Network address")
        XCTAssertEqual(find(wc, "CueList"), [F.net], "Network string argument")
        XCTAssertEqual(find(wc, "42"), [F.net], "Network number argument")
        XCTAssertEqual(find(wc, "door"), [F.wind, F.door], "list order, group children included")
        XCTAssertEqual(find(wc, "   "), [], "blank text finds nothing")
        XCTAssertEqual(find(wc, "no such thing"), [])
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "0 found")
        wc.findBar.field.stringValue = ""
        wc.setFindQuery("")
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "")
    }

    func testFindOpensCollapsedGroupsAndWrapsInBothDirections() async throws {
        let wc = try await findController()
        let outline = wc.cueList.outlineView
        let group = try XCTUnwrap(wc.cueList.node(F.group))
        outline.collapseItem(group)
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        XCTAssertEqual(find(wc, "door"), [F.wind, F.door])
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "2 found")
        wc.stepFind(forward: true)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.wind])
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "1 of 2")
        XCTAssertFalse(outline.isItemExpanded(group))
        wc.stepFind(forward: true)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.door])
        XCTAssertTrue(outline.isItemExpanded(group), "the group opens to show the found cue")
        wc.stepFind(forward: true)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.wind], "Next wraps to the first")
        wc.stepFind(forward: false)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.door], "Previous wraps to the last")
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "2 of 2")
    }

    func testFindStartsFromTheSelectionAndAClickEndsTheWalk() async throws {
        let wc = try await findController()
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        _ = find(wc, "door")
        wc.cueList.select([F.thunder])
        wc.stepFind(forward: true)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.wind], "the first found cue after the selection")
        wc.cueList.select([F.group])
        wc.stepFind(forward: false)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.wind], "the last found cue before the selection")
        wc.cueList.select([F.group])
        wc.stepFind(forward: true)
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.door])
    }

    func testSelectFoundSelectsEveryMatchIncludingInsideAClosedGroup() async throws {
        let wc = try await findController()
        let group = try XCTUnwrap(wc.cueList.node(F.group))
        wc.cueList.outlineView.collapseItem(group)
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        _ = find(wc, "door")
        XCTAssertTrue(wc.findBar.selectFoundButton.isEnabled)
        wc.findBar.selectFoundButton.performClick(nil)
        XCTAssertEqual(Set(wc.cueList.selectedIDs()), [F.wind, F.door])
        wc.findBar.field.stringValue = "nothing"
        _ = find(wc, "nothing")
        XCTAssertFalse(wc.findBar.selectFoundButton.isEnabled)
        XCTAssertFalse(wc.findBar.nextButton.isEnabled)
    }

    func testFindFollowsEditsWhileTheBarIsOpen() async throws {
        let wc = try await findController()
        wc.openFind()
        wc.findBar.field.stringValue = "zebra"
        _ = find(wc, "zebra")
        wc.stepFind(forward: true)
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "1 of 1")
        let memo = JSONValue.object(["type": .string("memo")])
        let renamed = TestSnapshots.cueJSON(id: F.zebra.raw, number: "3.2", name: "Lion", kind: memo)
        let upsert = try ServerMessage.decode(
            #"{"type":"model_changed","revision":\#(wc.store.revision + 1),"upserts":[{"type":"cue","list":"\#(TestSnapshots.mainList)","parent":"\#(F.group)","index":1,"cue":\#(try renamed.text())}],"deletes":[]}"#
        )
        XCTAssertEqual(wc.store.receive(upsert), .applied)
        XCTAssertEqual(wc.find.matches, [], "the renamed cue no longer matches")
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "0 found")
    }

    /// Combine does not deliver in subscription order. The cue list is detached so the window's
    /// own sink is guaranteed to run first, as in the bad order; the window's syncs that read the
    /// cue list must still end on the new model once the cue list has applied it.
    func testWindowSyncsFollowTheCueListWhateverTheDeliveryOrder() async throws {
        let wc = try await findController()
        let second = ModelID("00000000-0000-0000-0000-0000000000b1")
        _ = wc.sidebar.view
        let store = wc.store
        func playhead(_ list: String, _ cue: ModelID) throws {
            let change = #"{"type":"playhead","list":"\#(list)","cue":"\#(cue)"}"#
            let msg = try ServerMessage.decode(
                #"{"type":"runtime","seq":\#(store.runtimeSeq + 1),"changes":[\#(change)]}"#)
            XCTAssertEqual(store.receive(msg), .applied)
        }
        try playhead(TestSnapshots.mainList.raw, F.thunder)
        try playhead(second.raw, F.other)
        wc.cueList.showList(second)
        XCTAssertEqual(wc.masthead.cueID, F.other)
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 1)

        wc.cueList.sub = nil
        let delete = try ServerMessage.decode(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[],"deletes":[{"type":"cue_list","id":"\#(second.raw)"}]}"#
        )
        XCTAssertEqual(store.receive(delete), .applied)
        XCTAssertEqual(
            wc.footer.countLabel.stringValue,
            FooterBar.countText(cues: store.model?.cueCount ?? 0, lists: 1), "the count reads the store")
        wc.cueList.handle(.model(upserts: [], deletes: [.cueList(second)]))

        XCTAssertEqual(wc.cueList.listID, TestSnapshots.mainList)
        XCTAssertEqual(wc.masthead.cueID, F.thunder, "the standby line follows the list now shown")
        XCTAssertEqual(wc.sidebar.listTable.selectedRow, 0, "the highlight follows the shown list")
        XCTAssertEqual(wc.sidebar.listTable.numberOfRows, 1)
    }

    /// Playing cues send progress and runtime ticks up to 30 times a second; none may rescan the
    /// list, or a big show with the bar open would starve GO and Esc. Only a model change does.
    func testOnlyModelChangesRescanTheListWhileTheBarIsOpen() async throws {
        let wc = try await findController()
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        _ = find(wc, "door")
        let scans = wc.find.scans
        let store = wc.store
        let ticks = [
            #"{"type":"cue_state","cue":"\#(F.wind)","state":"running"}"#,
            #"{"type":"cue_state","cue":"\#(F.wind)","state":"idle"}"#,
        ]
        for change in ticks {
            let message = #"{"type":"runtime","seq":\#(store.runtimeSeq + 1),"changes":[\#(change)]}"#
            XCTAssertEqual(store.receive(try ServerMessage.decode(message)), .applied)
        }
        let progress = #"{"type":"progress","items":[{"cue":"\#(F.wind)","elapsed":1.0,"remaining":2.0}]}"#
        _ = store.receive(try ServerMessage.decode(progress))
        XCTAssertEqual(wc.find.scans, scans, "runtime and progress ticks do not scan")
        let renamed = TestSnapshots.cueJSON(
            id: F.door.raw, number: "3.1", name: "Gate", kind: .object(["type": .string("memo")]))
        let upsert = try ServerMessage.decode(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[{"type":"cue","list":"\#(TestSnapshots.mainList)","parent":"\#(F.group)","index":0,"cue":\#(try renamed.text())}],"deletes":[]}"#
        )
        XCTAssertEqual(store.receive(upsert), .applied)
        XCTAssertEqual(wc.find.scans, scans + 1, "a model change scans once, after the tree is updated")
        XCTAssertEqual(wc.find.matches, [F.wind])
    }

    /// Choosing a Warnings row can switch the shown list; the count must then be the new list's.
    func testRevealingACueInAnotherListRefreshesTheFindCount() async throws {
        let wc = try await findController()
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        XCTAssertEqual(find(wc, "door"), [F.wind, F.door])
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "2 found")
        wc.reveal(cue: F.other)
        XCTAssertEqual(wc.find.matches, [F.other])
        XCTAssertEqual(wc.findBar.countLabel.stringValue, "1 found")
    }

    func testDoneAndCmdFCycleGiveTheCueTypeRowBack() async throws {
        let wc = try await findController()
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        defer { window.orderOut(nil) }
        XCTAssertFalse(wc.cueToolbar.isHidden)
        XCTAssertTrue(wc.findBar.isHidden)
        // The three states of Cmd-F: open, cursor to the field, close.
        wc.toggleFind()
        XCTAssertTrue(wc.cueToolbar.isHidden)
        XCTAssertFalse(wc.findBar.isHidden)
        XCTAssertTrue(wc.findBar.fieldHasFocus)
        window.makeFirstResponder(wc.cueList.outlineView)
        wc.toggleFind()
        XCTAssertFalse(wc.findBar.isHidden, "the cursor moves to the field first")
        XCTAssertTrue(wc.findBar.fieldHasFocus)
        wc.toggleFind()
        XCTAssertTrue(wc.findBar.isHidden)
        XCTAssertFalse(wc.cueToolbar.isHidden)
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "the focus returns to the cue list")
        // Done clears the search.
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        wc.setFindQuery("door")
        wc.findBar.doneButton.performClick(nil)
        XCTAssertTrue(wc.findBar.isHidden)
        XCTAssertFalse(wc.cueToolbar.isHidden)
        XCTAssertEqual(wc.findBar.query, "")
        XCTAssertEqual(wc.find.matches, [])
        wc.openFind()
        XCTAssertEqual(wc.findBar.query, "", "a fresh search")
    }

    func testToolbarHiddenBeforeFindStaysHiddenAfter() async throws {
        let wc = try await findController()
        wc.setVisible(.toolbar, false)
        wc.openFind()
        XCTAssertFalse(wc.findBar.isHidden)
        XCTAssertFalse(wc.isVisible(.toolbar), "the View menu's state is the user's, not Find's")
        wc.closeFind()
        XCTAssertTrue(wc.cueToolbar.isHidden)
        wc.setVisible(.toolbar, true)
        wc.openFind()
        wc.setVisible(.toolbar, false)  // toggled while Find holds the row
        wc.closeFind()
        XCTAssertTrue(wc.cueToolbar.isHidden)
    }

    func testEscInTheFieldClosesTheBarAndReturnIsNext() async throws {
        let wc = try await findController()
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        _ = find(wc, "door")
        let (field, text) = (wc.findBar.field, NSTextView())
        XCTAssertTrue(wc.findBar.control(field, textView: text, doCommandBy: #selector(NSResponder.insertNewline(_:))))
        XCTAssertEqual(wc.cueList.selectedIDs(), [F.wind])
        XCTAssertFalse(wc.findBar.control(field, textView: text, doCommandBy: #selector(NSResponder.moveLeft(_:))))
        let esc = wc.findBar.control(field, textView: text, doCommandBy: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertTrue(esc)
        XCTAssertTrue(wc.findBar.isHidden)
        XCTAssertFalse(wc.cueToolbar.isHidden)
    }

    func testShowModeClosesFindAndRefusesIt() async throws {
        let wc = try await findController()
        wc.openFind()
        wc.findBar.field.stringValue = "door"
        _ = find(wc, "door")
        wc.setShowMode(true)
        XCTAssertTrue(wc.isShowMode)
        XCTAssertTrue(wc.findBar.isHidden, "Find is off in Show Mode")
        XCTAssertFalse(wc.find.isOpen)
        wc.toggleFind()
        wc.openFind()
        wc.findNext(forward: true)
        XCTAssertFalse(wc.find.isOpen)
        XCTAssertTrue(wc.findBar.isHidden)
        wc.setShowMode(false)
        XCTAssertFalse(wc.cueToolbar.isHidden, "the cue-type row is back with Edit Mode")
        wc.openFind()
        XCTAssertTrue(wc.find.isOpen)
    }

    // MARK: Show Mode sizes and contrast (SM-01, SM-03)

    func testMastheadMetricsGrowInShowModeByTheDocumentedProportions() {
        let window = NSSize(width: 1200, height: 800)
        let edit = MastheadMetrics.make(showMode: false, window: window)
        let show = MastheadMetrics.make(showMode: true, window: window)
        XCTAssertEqual(edit.goSize, NSSize(width: 84, height: 72))
        XCTAssertEqual(show.goSize, NSSize(width: 144, height: 104))
        XCTAssertEqual(show.standbyFontSize, edit.standbyFontSize * 2)
        XCTAssertEqual(show.notesHeight / edit.notesHeight, 1.7, accuracy: 0.02)
        XCTAssertEqual(show.notesFontSize / edit.notesFontSize, 1.7, accuracy: 0.1)
        XCTAssertGreaterThan(show.height, edit.height)
        // The GO button must still fit inside its masthead.
        XCTAssertLessThanOrEqual(show.goSize.height + 20, show.height)
        let tiny = MastheadMetrics.make(showMode: false, window: NSSize(width: 100, height: 100))
        XCTAssertGreaterThanOrEqual(tiny.goSize.width, 80)
        XCTAssertGreaterThanOrEqual(tiny.goSize.height, 56)
    }

    func testStandbyIsOneLineNumberDotName() async throws {
        let (wc, rig) = try await demoController()
        XCTAssertEqual(wc.masthead.standbyField.maximumNumberOfLines, 1)
        try await standBy(rig, TestSnapshots.intro)
        let cue = try XCTUnwrap(rig.store.model?.cue(TestSnapshots.intro))
        let number = try XCTUnwrap(cue.number, "the demo's Intro cue is numbered")
        XCTAssertTrue(wc.masthead.standbyField.stringValue.hasPrefix(number + " \u{00B7} "))
        wc.setShowMode(true)
        XCTAssertEqual(wc.masthead.standbyField.maximumNumberOfLines, 1)
        XCTAssertEqual(
            wc.masthead.standbyField.font?.pointSize,
            MastheadMetrics.make(showMode: true, window: wc.window?.contentView?.bounds.size ?? .zero).standbyFontSize)
    }

    func testShowModePaletteMeetsContrastTargets() {
        typealias P = ShowModePalette
        // WCAG AAA (7:1) for the text a performer reads at a distance, 3:1 for the GO button's edge.
        XCTAssertGreaterThanOrEqual(P.contrast(P.standbyText, P.background), 7)
        XCTAssertGreaterThanOrEqual(P.contrast(P.notesText, P.background), 7)
        XCTAssertGreaterThanOrEqual(P.contrast(P.goText, P.goFill), 7)
        XCTAssertGreaterThanOrEqual(P.contrast(P.goFill, P.background), 3)
        XCTAssertEqual(P.contrast(P.RGB(r: 1, g: 1, b: 1), P.RGB(r: 0, g: 0, b: 0)), 21, accuracy: 0.001)
        XCTAssertEqual(P.contrast(P.background, P.background), 1, accuracy: 0.001)
    }

    func testMastheadAppliesThePaletteInShowModeOnly() async throws {
        let (wc, _) = try await demoController()
        wc.setShowMode(true)
        let c = try XCTUnwrap(
            wc.masthead.layer?.backgroundColor.flatMap { NSColor(cgColor: $0) }?.usingColorSpace(.sRGB))
        XCTAssertEqual(c.redComponent, 0, accuracy: 0.001)
        let go = try XCTUnwrap(
            wc.masthead.goButton.layer?.backgroundColor.flatMap { NSColor(cgColor: $0) }?.usingColorSpace(.sRGB))
        XCTAssertEqual(Double(go.greenComponent), ShowModePalette.goFill.g, accuracy: 0.001)
        wc.setShowMode(false)
        XCTAssertNil(wc.masthead.layer?.backgroundColor)
    }
}

/// Inspector tabs and the window frame, on the real engine: what the user sees of the window when
/// they click through the tabs.
@MainActor
final class InspectorWindowFrameTests: XCTestCase {
    private let audio = CueListHarness.intro
    private let fade = CueListHarness.fade
    private let group = CueListHarness.group
    private let memo: ModelID = "00000000-0000-0000-0000-0000000000e1"
    private let userFrame = NSRect(x: 120, y: 140, width: 1100, height: 720)
    private var rig: Rig!
    private var wc: WorkspaceWindowController!

    override func setUp() async throws {
        rig = try await Rig.open()
        let list = try XCTUnwrap(rig.store.model?.cueLists.first?.id)
        let cue = try XCTUnwrap(NewCueKind.memo.cueJSON(id: memo, patch: nil))
        guard case .success = await rig.store.send(command: .addCue(cue, list: list, parent: nil, index: 3)) else {
            return XCTFail("add memo")
        }
        await assertEventually { self.rig.store.model?.cue(self.memo) != nil }
        wc = WorkspaceWindowController(store: rig.store)
        let window = try XCTUnwrap(wc.window)
        window.setFrame(userFrame, display: true)
        window.makeKeyAndOrderFront(nil)
        settleLayout()
    }

    override func tearDown() async throws {
        wc.window?.close()
        wc = nil
    }

    private var window: NSWindow { wc.window! }

    /// Lets the window run the layout pass a click on a tab triggers.
    private func settleLayout() {
        window.layoutIfNeeded()
        window.displayIfNeeded()
        window.layoutIfNeeded()
    }

    private func select(_ id: ModelID) async {
        wc.cueList.selectAsUser([id])
        await assertEventually { self.wc.inspector.cueForEdit?.id == id }
        settleLayout()
    }

    private func clickTab(_ id: String) throws {
        let tabs = wc.inspector.tabView
        let item = try XCTUnwrap(tabs.tabViewItems.first { $0.identifier as? String == id }, "tab \(id)")
        tabs.selectTabViewItem(item)
        settleLayout()
    }

    private struct Layout: Equatable {
        var frame: NSRect
        var cueList: NSRect
        var inspector: NSRect
    }

    private func layout() -> Layout {
        Layout(frame: window.frame, cueList: wc.cueList.view.frame, inspector: wc.inspector.view.frame)
    }

    func testFadeCurveTabKeepsTheWindow() async throws {
        await select(fade)
        try clickTab("basics")
        let before = layout()
        XCTAssertEqual(before.frame, userFrame)
        try clickTab("curve")
        XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.identifier as? String, "curve")
        XCTAssertEqual(window.frame, userFrame, "the window frame does not move or resize")
        // The curve editor fits inside the inspector.
        let editor = wc.inspector.fadeSettings.view
        let inspector = wc.inspector.view
        XCTAssertTrue(inspector.bounds.contains(editor.convert(editor.bounds, to: inspector)))
    }

    func testEveryInspectorTabKeepsTheWindowAndTheSplit() async throws {
        for cue in [audio, fade, group, memo] {
            await select(cue)
            let ids = InspectorTabs.ids(forKind: wc.store.model?.cue(cue)?.kindType ?? "")
            let before = layout()
            XCTAssertEqual(before.frame, userFrame)
            for id in ids + [ids[0]] {
                try clickTab(id)
                XCTAssertEqual(layout(), before, "tab \(id) of cue \(cue)")
            }
        }
    }

    func testCurveTabInASmallWindowKeepsTheWindow() async throws {
        window.setFrame(NSRect(origin: userFrame.origin, size: window.minSize), display: true)
        settleLayout()
        let small = window.frame
        XCTAssertEqual(small.size, window.minSize)
        await select(fade)
        XCTAssertEqual(window.frame, small)
        try clickTab("curve")
        XCTAssertEqual(window.frame, small, "the window neither grows nor shrinks")
    }

    func testSelectingACueWithoutTheCurveTabKeepsTheWindow() async throws {
        await select(fade)
        try clickTab("curve")
        let before = layout()
        await select(audio)
        XCTAssertFalse(InspectorTabs.ids(forKind: "audio").contains("curve"))
        XCTAssertEqual(window.frame, before.frame)
        XCTAssertEqual(layout().frame, userFrame)
    }
}
