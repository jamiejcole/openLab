// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// What a reader thread read, handed back to the test thread.
private final class Collected: @unchecked Sendable {
    private let lock = NSLock()
    private let done = DispatchSemaphore(value: 0)
    private var bytes = Data()
    var data: Data { lock.withLock { bytes } }
    func set(_ data: Data) {
        lock.withLock { bytes = data }
        done.signal()
    }
    func wait() { done.wait() }
}

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

    /// Runs `body` with stderr captured and returns the layout reports AppKit made there (other lines, from
    /// other threads, are not this test's business). The pipe is drained on a thread so a noisy body cannot
    /// block on a full pipe.
    private func stderrText(_ body: () -> Void) -> String {
        let pipe = Pipe()
        let collected = Collected()
        let reader = Thread {
            collected.set(pipe.fileHandleForReading.readDataToEndOfFile())
        }
        reader.start()
        let saved = dup(STDERR_FILENO)
        fflush(stderr)
        dup2(pipe.fileHandleForWriting.fileDescriptor, STDERR_FILENO)
        body()
        fflush(stderr)
        dup2(saved, STDERR_FILENO)
        close(saved)
        try? pipe.fileHandleForWriting.close()
        collected.wait()
        return String(decoding: collected.data, as: UTF8.self).split(separator: "\n").filter {
            // macOS 27 also logs a negative view width from window creation: #530. NegativeFrameProbe names
            // the view in CI's log; this filter takes the message back once it is fixed.
            $0.contains("Conflicting constraints")
        }.joined(separator: "\n")
    }

    /// Every view under `root` (itself included) whose frame or bounds has a negative size, named by its class
    /// path and accessibility identifier: AppKit's own "width is negative" report (#530) names no view.
    private func negativeViews(_ root: NSView) -> [String] {
        func walk(_ view: NSView, _ path: String) -> [String] {
            let id = view.accessibilityIdentifier()
            let here = path + "/" + String(describing: type(of: view)) + (id.isEmpty ? "" : "[\(id)]")
            var found: [String] = []
            if view.frame.width < 0 || view.frame.height < 0 || view.bounds.width < 0 || view.bounds.height < 0 {
                found.append("\(here) frame \(view.frame) bounds \(view.bounds)")
            }
            for sub in view.subviews { found += walk(sub, here) }
            return found
        }
        return walk(root, "")
    }

    /// A real window, shown at the smallest size it allows, at a small screen's size and at a large one,
    /// with each area hidden and shown, lays out without AppKit reporting a conflicting constraint (the
    /// report is read from stderr: AppKit offers nothing else to assert on).
    func testWindowLaysOutCleanlyAtEverySize() throws {
        var made: WorkspaceWindowController?
        // The window is built inside the capture: its first layout is where a conflict used to show.
        let first = stderrText { made = try? controller() }
        let wc = try XCTUnwrap(made)
        let window = try XCTUnwrap(wc.window)
        func layout() { window.contentView?.layoutSubtreeIfNeeded() }
        var problems: [String] = first.isEmpty ? [] : ["built: \(first.prefix(200))"]
        if let root = window.contentView { problems += negativeViews(root).map { "built, negative: \($0)" } }
        func step(_ what: String, _ body: () -> Void) {
            let report = stderrText(body)
            if !report.isEmpty { problems.append("\(what): \(report.prefix(200))") }
            if let root = window.contentView { problems += negativeViews(root).map { "\(what), negative: \($0)" } }
        }
        step("shown") {
            window.orderFront(nil)
            layout()
        }
        for size in [
            window.minSize, NSSize(width: 1024, height: 700), NSSize(width: 1800, height: 1000), window.minSize,
        ] {
            step("size \(size)") {
                window.setContentSize(size)
                layout()
            }
            for area in LayoutArea.allCases {
                step("\(area) toggled") {
                    wc.setVisible(area, !wc.isVisible(area))
                    layout()
                }
                step("\(area) toggled back") {
                    wc.setVisible(area, !wc.isVisible(area))
                    layout()
                }
            }
        }
        window.close()
        XCTAssertEqual(problems, [], "AppKit reported a layout problem")
    }

    /// A cue-list column dragged to nothing leaves its cells zero wide: they lay out without a conflict.
    func testCueListCellsLayOutAtZeroWidth() {
        let cell = CueCell(identifier: NSUserInterfaceItemIdentifier("continue"))
        cell.translatesAutoresizingMaskIntoConstraints = false
        cell.widthAnchor.constraint(equalToConstant: 0).isActive = true
        let host = NSView(frame: NSRect(x: 0, y: 0, width: 100, height: 20))
        host.addSubview(cell)
        XCTAssertEqual(stderrText { host.layoutSubtreeIfNeeded() }, "")
    }

    func testAllAreasPresent() throws {
        let wc = try controller()
        wc.setVisible(.sidebar, true)  // hidden, the sidebar is out of the split
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
            [cue.number, rig.store.model?.listName(cue)].compactMap { $0 }.joined(separator: " \u{00B7} "))
        XCTAssertTrue(wc.masthead.notesView.isEditable)
        // An unnumbered, unnamed cue: its name alone, from its kind.
        try await standBy(rig, TestSnapshots.fade)
        XCTAssertEqual(wc.masthead.cueID, TestSnapshots.fade)
        XCTAssertEqual(
            wc.masthead.standbyField.stringValue, "Fade and stop Intro",
            "an unnamed Fade is named after the cue it fades")
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
        XCTAssertEqual(wc.masthead.notesView.string, "for intro", "the text stays while typing")
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await assertEventually { !self.setNotesCommands(rig).isEmpty }
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
        XCTAssertTrue(window.firstResponder === wc.masthead.notesView, "typing keeps its focus")
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        XCTAssertEqual(wc.masthead.cueID, TestSnapshots.fade, "the new cue shows once typing ends")
        // The operator types for the new cue.
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
        var notices: [NotesNotice] = []
        wc.editor.notesNotSaved = { notices.append($0) }
        let outcome = await wc.editor.setNotes(of: "00000000-0000-0000-0000-0000000000ff", text: "x")
        XCTAssertEqual(outcome, .notSaved)
        XCTAssertEqual(notices, [NotesNotice(message: loc("notes.notSaved.cueGone"), text: "x")])
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
        XCTAssertTrue(
            wc.masthead.textView(
                wc.masthead.notesView, doCommandBy: #selector(NSResponder.insertNewlineIgnoringFieldEditor(_:))),
            "Option-Return ends the typing as Return does")
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
    /// A frame the user might pick, inside the screen: AppKit shrinks a frame bigger than the screen (CI runners
    /// have small displays), and the tests compare the frame exactly.
    private lazy var userFrame: NSRect = {
        let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1024, height: 700)
        let width = min(1100, screen.width - 40).rounded(.down)
        let height = min(720, screen.height - 40).rounded(.down)
        return NSRect(x: screen.minX + 20, y: screen.minY + 20, width: width, height: height)
    }()
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
        // Nil when setUp skipped or threw before making the window.
        wc?.window?.close()
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

/// Window layout remembered per workspace file (UI-02), on the real engine: each flow closes the
/// workspace through the app and opens its file again, and checks the window the user then sees.
/// The layout is kept in a UserDefaults suite of the test's own.
@MainActor
final class WindowLayoutTests: XCTestCase {
    private var app: AppController!
    private var engine: TestEngine!
    private var alerts: ScriptedAlerts!
    private var defaults: UserDefaults!
    private var suite = ""

    override func setUp() async throws {
        engine = try await TestEngine.start()
        suite = "openlab-layout-test-\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        app = AppController(session: session, scheduler: ManualScheduler(), layoutDefaults: defaults)
        alerts = ScriptedAlerts()
        app.alerts = alerts
        session.start()
        await assertEventually { session.appClient.state == .connected }
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        defaults?.removePersistentDomain(forName: suite)
        engine?.stop()
    }

    // MARK: Doing what the operator does

    /// A show file at a path of its own.
    private func showFile(_ name: String) throws -> URL {
        let source = try XCTUnwrap(
            Bundle(for: WindowLayoutTests.self).url(forResource: "TestShows", withExtension: nil)?
                .appendingPathComponent("Second/Second.openlab"))
        var show = try XCTUnwrap(JSONValue.parse(try String(contentsOf: source, encoding: .utf8)))
        show["workspace"]?.set("id", .string(UUID().uuidString.lowercased()))
        show["workspace"]?.set("name", .string(name))
        let dir = engine.folder.appendingPathComponent("layout-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("\(name).openlab")
        try show.text().write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    /// File > Open of `url`: the window it gets, loaded.
    private func open(_ url: URL) async throws -> (doc: ModelID, wc: WorkspaceWindowController) {
        let before = Set(app.windows.keys)
        await app.openWorkspace(url: url)
        await assertEventually { app.windows.count == before.count + 1 }
        let doc = try XCTUnwrap(Set(app.windows.keys).subtracting(before).first)
        let wc = try XCTUnwrap(app.windows[doc])
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        wc.window?.layoutIfNeeded()
        return (doc, wc)
    }

    /// The window's close button, answered: the workspace is closed.
    private func close(_ doc: ModelID) async {
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed)
        await assertEventually { app.windows[doc] == nil }
    }

    /// The path the engine knows the file by: the layout is kept under it.
    private func knownPath(_ url: URL) async throws -> String {
        let (doc, _) = try await open(url)
        let path = try XCTUnwrap(app.session.workspaces.first { $0.doc == doc }?.path)
        await close(doc)
        return path
    }

    /// A frame the user might pick, inside the main screen so AppKit leaves it alone (CI screens are small).
    private func frame(shift: CGFloat = 0, _ wc: WorkspaceWindowController) -> NSRect {
        let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1024, height: 700)
        let min = wc.window?.minSize ?? NSSize(width: 800, height: 500)
        let width = max(min.width, Swift.min(1000, screen.width - 80 - shift)).rounded(.down)
        let height = max(min.height, Swift.min(780, screen.height - 80 - shift)).rounded(.down)
        return NSRect(x: screen.minX + 20 + shift, y: screen.minY + 20 + shift, width: width, height: height)
    }

    /// A height to drag the inspector to: clearly above the least its content allows on this screen
    /// (the divider pushed to the bottom shows that least), and leaving the cue list its room.
    private func draggedInspectorHeight(_ wc: WorkspaceWindowController) -> CGFloat {
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        wc.verticalSplit.setPosition(room, ofDividerAt: 0)
        settle(wc)
        let least = wc.inspector.view.frame.height
        return min(least + 90, room - 60).rounded(.down)
    }

    private func settle(_ wc: WorkspaceWindowController) {
        wc.window?.layoutIfNeeded()
        wc.window?.displayIfNeeded()
        wc.window?.layoutIfNeeded()
    }

    private func toggle(_ area: LayoutArea, in wc: WorkspaceWindowController) throws {
        try XCTUnwrap(wc.footer.toggles[area]).performClick(nil)
        settle(wc)
    }

    private func onScreen(_ frame: NSRect) -> Bool {
        NSScreen.screens.contains { $0.visibleFrame.insetBy(dx: -1, dy: -1).contains(frame) }
    }

    // MARK: Flows

    func testWindowFrameComesBack() async throws {
        let url = try showFile("Frame")
        let (doc, wc) = try await open(url)
        let wanted = frame(wc)
        wc.window?.setFrame(wanted, display: true)
        await close(doc)
        let (_, again) = try await open(url)
        XCTAssertEqual(again.window?.frame, wanted)
        XCTAssertTrue(
            again.window?.firstResponder === again.cueList.outlineView, "restoring left the keys with the cue list")
    }

    func testSplittersComeBack() async throws {
        let url = try showFile("Splits")
        let (doc, wc) = try await open(url)
        wc.window?.setFrame(frame(wc), display: true)
        try toggle(.sidebar, in: wc)
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        let inspector = draggedInspectorHeight(wc)
        wc.verticalSplit.setPosition(room - inspector, ofDividerAt: 0)
        settle(wc)
        let across = wc.horizontalSplit.bounds.width - wc.horizontalSplit.dividerThickness
        let sidebar: CGFloat = 360
        wc.horizontalSplit.setPosition(across - sidebar, ofDividerAt: 0)
        settle(wc)
        XCTAssertEqual(wc.inspector.view.frame.height, inspector, accuracy: 1, "the drag took")
        XCTAssertEqual(wc.sidebar.view.frame.width, sidebar, accuracy: 1, "the drag took")
        await close(doc)

        let (_, again) = try await open(url)
        settle(again)
        XCTAssertEqual(again.inspector.view.frame.height, inspector, accuracy: 1)
        XCTAssertEqual(again.sidebar.view.frame.width, sidebar, accuracy: 1)
        let split = again.horizontalSplit
        XCTAssertEqual(
            again.verticalSplit.frame.width + split.dividerThickness + again.sidebar.view.frame.width,
            split.bounds.width, accuracy: 1, "the cue list keeps the rest of the width")
    }

    func testShownAreasAndSidebarTabComeBack() async throws {
        let url = try showFile("Areas")
        let (doc, wc) = try await open(url)
        try toggle(.inspector, in: wc)
        try toggle(.sidebar, in: wc)
        wc.sidebar.tabs.selectedSegment = 1
        wc.sidebar.tabs.performClick(nil)
        XCTAssertFalse(wc.isVisible(.inspector))
        XCTAssertFalse(try XCTUnwrap(wc.sidebar.activeTable.enclosingScrollView).isHidden)
        await close(doc)

        let (_, again) = try await open(url)
        XCTAssertFalse(again.isVisible(.inspector))
        XCTAssertEqual(again.footer.toggles[.inspector]?.state, .off)
        XCTAssertTrue(again.isVisible(.sidebar))
        XCTAssertEqual(again.sidebar.tabs.selectedSegment, 1)
        XCTAssertFalse(try XCTUnwrap(again.sidebar.activeTable.enclosingScrollView).isHidden, "the active cues show")
        XCTAssertTrue(try XCTUnwrap(again.sidebar.listTable.enclosingScrollView).isHidden)
    }

    func testShowModeIsNotSavedAsLayout() async throws {
        let url = try showFile("Mode")
        let (doc, wc) = try await open(url)
        wc.window?.setFrame(frame(wc), display: true)
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        let inspector = draggedInspectorHeight(wc)
        wc.verticalSplit.setPosition(room - inspector, ofDividerAt: 0)
        settle(wc)
        await wc.requestShowMode(true)
        await assertEventually { wc.isShowMode && !wc.isVisible(.inspector) && !wc.isVisible(.toolbar) }
        settle(wc)
        await close(doc)  // the Show Mode confirmation is answered yes

        let (_, again) = try await open(url)
        settle(again)
        XCTAssertFalse(again.isShowMode, "Show Mode itself is not restored")
        XCTAssertTrue(again.isVisible(.inspector))
        XCTAssertTrue(again.isVisible(.toolbar))
        XCTAssertEqual(again.inspector.view.frame.height, inspector, accuracy: 1)
    }

    func testEachWorkspaceFileKeepsItsOwnFrame() async throws {
        let a = try showFile("Alpha")
        let b = try showFile("Beta")
        let (docA, wcA) = try await open(a)
        let frameA = frame(wcA)
        wcA.window?.setFrame(frameA, display: true)
        let (docB, wcB) = try await open(b)
        let frameB = frame(shift: 40, wcB)
        XCTAssertNotEqual(frameA, frameB)
        wcB.window?.setFrame(frameB, display: true)
        await close(docA)
        await close(docB)

        let (_, againA) = try await open(a)
        let (_, againB) = try await open(b)
        XCTAssertEqual(againA.window?.frame, frameA)
        XCTAssertEqual(againB.window?.frame, frameB)
    }

    func testASavedFrameThatNoLongerFitsIsBroughtOntoAScreen() async throws {
        let url = try showFile("Gone")
        let path = try await knownPath(url)
        // Saved while a second display was to the right of every screen there is now, and larger.
        let right = NSScreen.screens.map { $0.frame.maxX }.max() ?? 0
        var layout = WindowLayout()
        layout.frame = NSRect(x: right + 3000, y: 200, width: 9000, height: 7000)
        WindowLayoutStore(defaults: defaults).save(layout, forPath: path)

        let (_, wc) = try await open(url)
        let shown = try XCTUnwrap(wc.window?.frame)
        XCTAssertTrue(onScreen(shown), "\(shown) is on a connected screen")
        let screen = try XCTUnwrap(NSScreen.screens.first { $0.visibleFrame.insetBy(dx: -1, dy: -1).contains(shown) })
        XCTAssertLessThanOrEqual(shown.width, screen.visibleFrame.width)
        XCTAssertLessThanOrEqual(shown.height, screen.visibleFrame.height)
    }

    func testDamagedSavedLayoutOpensWithTheDefaultLayout() async throws {
        let url = try showFile("Damaged")
        let first = try await open(url)
        let defaultFrame = try XCTUnwrap(first.wc.window?.frame)
        let defaultInspector = first.wc.inspector.view.frame.height
        let path = try XCTUnwrap(app.session.workspaces.first { $0.doc == first.doc }?.path)
        await close(first.doc)
        defaults.removeObject(forKey: WindowLayoutStore.key)

        let nonsense: [String: Any] = [
            "frame": [-5.0, -5.0, -100.0, "x"], "inspectorHeight": -3.0, "sidebarWidth": "wide",
            "shown": 7, "sidebarTab": 99,
        ]
        for stored: Any in [nonsense, "text", [String: Any]()] {
            defaults.set([path: stored], forKey: WindowLayoutStore.key)
            let (doc, wc) = try await open(url)
            settle(wc)
            XCTAssertEqual(wc.window?.frame, defaultFrame)
            XCTAssertEqual(wc.inspector.view.frame.height, defaultInspector, accuracy: 1)
            XCTAssertTrue(wc.isVisible(.inspector))
            XCTAssertFalse(wc.isVisible(.sidebar))
            XCTAssertEqual(wc.sidebar.tabs.selectedSegment, 0)
            XCTAssertNil(wc.window?.attachedSheet, "no dialog")
            XCTAssertTrue(alerts.errors.isEmpty)
            await close(doc)
            defaults.removeObject(forKey: WindowLayoutStore.key)
        }
        defaults.set("not even a table", forKey: WindowLayoutStore.key)
        let (_, wc) = try await open(url)
        XCTAssertEqual(wc.window?.frame, defaultFrame)
    }

    // MARK: Review findings

    /// Save As: the layout follows the document to its new file, and the old file's entry stays as it was.
    func testSaveAsMovesTheLayoutToTheNewFile() async throws {
        let old = try showFile("Old")
        let new = engine.folder.appendingPathComponent("saved-\(UUID().uuidString).openlab")
        let (doc, wc) = try await open(old)
        let oldFrame = frame(wc)
        wc.window?.setFrame(oldFrame, display: true)
        app.pickSaveURL = { _ in new }
        let saved = await app.saveWorkspace(wc.store, asNew: true)
        XCTAssertTrue(saved)
        await assertEventually { self.app.session.workspaces.first { $0.doc == doc }?.path == new.path }
        let newFrame = frame(shift: 40, wc)
        wc.window?.setFrame(newFrame, display: true)
        await close(doc)

        let (_, again) = try await open(new)
        XCTAssertEqual(again.window?.frame, newFrame, "the new file opens where the window was left")
        let (_, original) = try await open(old)
        XCTAssertEqual(original.window?.frame, oldFrame, "the old file's entry was not overwritten")
    }

    /// File > New, arrange, save for the first time: the arrangement is kept, not replaced by the defaults.
    func testTheFirstSaveOfANewWorkspaceKeepsItsLayout() async throws {
        await app.newWorkspace()
        await assertEventually { self.app.windows.count == 1 }
        let (doc, wc) = try XCTUnwrap(app.windows.first)
        await assertEventually { wc.store.model != nil }
        let arranged = frame(wc)
        wc.window?.setFrame(arranged, display: true)
        try toggle(.inspector, in: wc)
        let url = engine.folder.appendingPathComponent("first-\(UUID().uuidString).openlab")
        app.pickSaveURL = { _ in url }
        let saved = await app.saveWorkspace(wc.store)
        XCTAssertTrue(saved)
        await assertEventually { self.app.session.workspaces.first { $0.doc == doc }?.path == url.path }
        await close(doc)

        let (_, again) = try await open(url)
        XCTAssertEqual(again.window?.frame, arranged)
        XCTAssertFalse(again.isVisible(.inspector))
    }

    /// The window is put where it was saved before it is shown, so a click on it never meets a window that moves.
    func testAReopenedWindowDoesNotMoveOrResizeOnceItIsShown() async throws {
        let url = try showFile("Still")
        let (doc, wc) = try await open(url)
        let wanted = frame(wc)
        wc.window?.setFrame(wanted, display: true)
        await close(doc)

        var moves: [(window: ObjectIdentifier, frame: NSRect)] = []
        let tokens = [NSWindow.didMoveNotification, NSWindow.didResizeNotification].map {
            NotificationCenter.default.addObserver(forName: $0, object: nil, queue: .main) { note in
                guard let sender = note.object.map({ ObjectIdentifier($0 as AnyObject) }) else { return }
                MainActor.assumeIsolated {
                    if let w = NSApp.windows.first(where: { ObjectIdentifier($0) == sender }), w.isVisible {
                        moves.append((sender, w.frame))
                    }
                }
            }
        }
        defer { tokens.forEach(NotificationCenter.default.removeObserver) }
        let (_, again) = try await open(url)
        await assertEventually { self.app.session.workspaces.count == self.app.windows.count }
        await openLabTests.settle()
        XCTAssertEqual(again.window?.frame, wanted)
        let mine = ObjectIdentifier(try XCTUnwrap(again.window))
        XCTAssertTrue(moves.filter { $0.window == mine }.isEmpty, "moved after it was shown: \(moves)")
    }

    func testTheCueListNeverCollapsesAndAWindowResizeKeepsTheDraggedHeight() async throws {
        let url = try showFile("Squeeze")
        let (doc, wc) = try await open(url)
        let big = frame(wc)
        wc.window?.setFrame(big, display: true)
        settle(wc)
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        // The divider dragged to the very top.
        wc.verticalSplit.setPosition(0, ofDividerAt: 0)
        settle(wc)
        XCTAssertGreaterThanOrEqual(wc.cueList.view.frame.height, 56, "the list is not squeezed out")
        let tall = draggedInspectorHeight(wc)
        wc.verticalSplit.setPosition(room - tall, ofDividerAt: 0)
        settle(wc)
        XCTAssertEqual(wc.inspector.view.frame.height, tall, accuracy: 1)
        wc.window?.setFrame(NSRect(origin: big.origin, size: wc.window?.minSize ?? big.size), display: true)
        settle(wc)
        XCTAssertGreaterThanOrEqual(wc.cueList.view.frame.height, 56, "a small window keeps the list")
        wc.window?.setFrame(big, display: true)
        settle(wc)
        XCTAssertEqual(wc.inspector.view.frame.height, tall, accuracy: 1, "the dragged height came back")
        let path = try XCTUnwrap(app.session.workspaces.first { $0.doc == doc }?.path)
        let saved = WindowLayoutStore(defaults: defaults).layout(forPath: path).inspectorHeight
        XCTAssertEqual(try XCTUnwrap(saved), tall, accuracy: 1, "the shrink did not overwrite it")
    }

    func testAWindowSqueezeKeepsTheDraggedSidebarWidth() async throws {
        let url = try showFile("SqueezeSide")
        let (doc, wc) = try await open(url)
        let big = frame(wc)
        wc.window?.setFrame(big, display: true)
        try toggle(.sidebar, in: wc)
        let across = wc.horizontalSplit.bounds.width - wc.horizontalSplit.dividerThickness
        // As wide as the inspector's own width allows the cue list to be squeezed to.
        let wide = min(481, across - 520).rounded(.down)
        wc.horizontalSplit.setPosition(across - wide, ofDividerAt: 0)
        settle(wc)
        XCTAssertEqual(wc.sidebar.view.frame.width, wide, accuracy: 1, "the drag took")
        wc.window?.setFrame(NSRect(origin: big.origin, size: wc.window?.minSize ?? big.size), display: true)
        settle(wc)
        wc.window?.setFrame(big, display: true)
        settle(wc)
        XCTAssertEqual(wc.sidebar.view.frame.width, wide, accuracy: 1, "the dragged width came back")
        let path = try XCTUnwrap(app.session.workspaces.first { $0.doc == doc }?.path)
        let saved = WindowLayoutStore(defaults: defaults).layout(forPath: path).sidebarWidth
        XCTAssertEqual(try XCTUnwrap(saved), wide, accuracy: 1, "the squeeze did not overwrite it")
    }

    /// The size of a hidden inspector is part of the layout too: Save As keeps it.
    func testSaveAsKeepsTheSizeOfAHiddenInspector() async throws {
        let old = try showFile("HiddenSize")
        let new = engine.folder.appendingPathComponent("hidden-\(UUID().uuidString).openlab")
        let (doc, wc) = try await open(old)
        wc.window?.setFrame(frame(wc), display: true)
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        let tall = draggedInspectorHeight(wc)
        wc.verticalSplit.setPosition(room - tall, ofDividerAt: 0)
        settle(wc)
        try toggle(.inspector, in: wc)
        app.pickSaveURL = { _ in new }
        let saved = await app.saveWorkspace(wc.store, asNew: true)
        XCTAssertTrue(saved)
        await assertEventually { self.app.session.workspaces.first { $0.doc == doc }?.path == new.path }
        await close(doc)

        let (_, again) = try await open(new)
        XCTAssertFalse(again.isVisible(.inspector))
        try toggle(.inspector, in: again)
        XCTAssertEqual(again.inspector.view.frame.height, tall, accuracy: 1)
    }

    /// An engine restart numbers docs again: a new untitled workspace must not inherit the file an
    /// earlier doc with the same id was opened from.
    func testAnOpenedPathIsUsedOnceByItsWindow() async throws {
        let url = try showFile("Once")
        _ = try await open(url)
        XCTAssertEqual(app.session.openedPathCount, 0, "the window took it")
    }

    /// Showing a hidden area in a window too small for it is not a drag: its saved size stays.
    func testShowingAHiddenInspectorInASmallWindowKeepsItsSavedSize() async throws {
        let url = try showFile("ShowSmall")
        let (doc, wc) = try await open(url)
        let big = frame(wc)
        wc.window?.setFrame(big, display: true)
        settle(wc)
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        let tall = draggedInspectorHeight(wc)
        wc.verticalSplit.setPosition(room - tall, ofDividerAt: 0)
        settle(wc)
        try toggle(.inspector, in: wc)
        await close(doc)

        let (second, again) = try await open(url)
        again.window?.setFrame(NSRect(origin: big.origin, size: again.window?.minSize ?? big.size), display: true)
        settle(again)
        try toggle(.inspector, in: again)
        again.window?.setFrame(big, display: true)
        settle(again)
        XCTAssertEqual(again.inspector.view.frame.height, tall, accuracy: 1, "back to the saved size")
        let path = try XCTUnwrap(app.session.workspaces.first { $0.doc == second }?.path)
        let saved = WindowLayoutStore(defaults: defaults).layout(forPath: path).inspectorHeight
        XCTAssertEqual(try XCTUnwrap(saved), tall, accuracy: 1, "and still saved")
    }

    func testTheMinimumWindowKeepsItsListAndInspectorSizes() async throws {
        let (_, wc) = try await open(try showFile("Small"))
        wc.window?.setFrame(NSRect(origin: frame(wc).origin, size: wc.window?.minSize ?? .zero), display: true)
        settle(wc)
        XCTAssertGreaterThanOrEqual(wc.cueList.view.frame.height, 56)
        XCTAssertEqual(wc.inspector.view.frame.height, 240, accuracy: 1)
    }

    // MARK: Logic

    func testFitBringsAFrameOntoTheScreenAndKeepsAGoodOneAsItIs() {
        let screens = [NSRect(x: 0, y: 0, width: 1440, height: 800), NSRect(x: 1440, y: 0, width: 1000, height: 700)]
        let minSize = NSSize(width: 800, height: 500)
        let good = NSRect(x: 100, y: 100, width: 900, height: 600)
        XCTAssertEqual(WindowLayout.fit(good, screens: screens, minSize: minSize), good)
        XCTAssertEqual(
            WindowLayout.fit(NSRect(x: 5000, y: 5000, width: 900, height: 600), screens: screens, minSize: minSize),
            NSRect(x: 270, y: 100, width: 900, height: 600), "no screen: centred on the first")
        XCTAssertEqual(
            WindowLayout.fit(NSRect(x: 1300, y: -50, width: 3000, height: 3000), screens: screens, minSize: minSize),
            NSRect(x: 1440, y: 0, width: 1000, height: 700), "larger than the screen it mostly sits on")
        XCTAssertEqual(
            WindowLayout.fit(NSRect(x: 100, y: 100, width: 10, height: 10), screens: screens, minSize: minSize)?.size,
            minSize)
        XCTAssertNil(WindowLayout.fit(good, screens: [], minSize: minSize))
    }

    func testStoredLayoutRoundTripsAndDropsOnlyItsBadFields() {
        var layout = WindowLayout()
        layout.frame = NSRect(x: 1, y: 2, width: 900, height: 600)
        layout.inspectorHeight = 300
        layout.sidebarWidth = 350
        layout.shown = [.inspector: false, .sidebar: true]
        layout.sidebarTab = 2
        XCTAssertEqual(WindowLayout(stored: layout.stored), layout)
        var damaged = layout.stored
        damaged["frame"] = [Double.nan, 0, 10, 10]
        damaged["sidebarTab"] = -1
        let read = WindowLayout(stored: damaged)
        XCTAssertNil(read.frame)
        XCTAssertNil(read.sidebarTab)
        XCTAssertEqual(read.inspectorHeight, 300)
        XCTAssertEqual(read.shown, layout.shown)
    }

    func testTheStoredTableIsCapped() {
        let store = WindowLayoutStore(defaults: defaults)
        for i in 0..<(WindowLayoutStore.limit + 20) { store.save(WindowLayout(), forPath: "/f\(i)") }
        let count = defaults.dictionary(forKey: WindowLayoutStore.key)?.count ?? 0
        XCTAssertLessThanOrEqual(count, WindowLayoutStore.limit)
    }
}
