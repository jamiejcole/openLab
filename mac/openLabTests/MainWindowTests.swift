// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class MainWindowTests: XCTestCase {
    private func ax(_ root: NSView, _ id: String) -> NSView? {
        if root.accessibilityIdentifier() == id { return root }
        for sub in root.subviews { if let hit = ax(sub, id) { return hit } }
        return nil
    }

    private func controller() throws -> WorkspaceWindowController {
        WorkspaceWindowController(store: try Rig().store)
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
            ["status", "number", "name", "target", "preWait", "duration", "postWait", "continue"])
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
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertEqual(wc.footer.countLabel.stringValue, "6 cues in 1 list")
        let id = "00000000-0000-0000-0000-0000000000d1"
        let cue = FakeEngine.cueJSON(id: id, number: nil, name: "New", kind: FakeEngine.audioKind("/tmp/a.wav"))
        let add = try CueListHarness.upsertJSON(cue, index: 0)
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
                    #"{"type":"model_changed","revision":\#(revision + 1),"upserts":[],"deletes":[{"type":"cue","id":"\#(CueListHarness.group)"}]}"#
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

    private func demoController() async throws -> (WorkspaceWindowController, Rig) {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        return (WorkspaceWindowController(store: rig.store), rig)
    }

    private func standBy(_ rig: Rig, _ cue: ModelID) throws {
        let change = #"{"type":"playhead","list":"\#(CueListHarness.list)","cue":"\#(cue)"}"#
        let msg = try ServerMessage.decode(
            #"{"type":"runtime","seq":\#(rig.store.runtimeSeq + 1),"changes":[\#(change)]}"#)
        XCTAssertEqual(rig.store.receive(msg), .applied)
    }

    private func setNotesCommands(_ rig: Rig) -> [JSONValue] {
        rig.engine.received.compactMap {
            if case .command(_, let c, _) = $0, c.json["property"]?["type"]?.stringValue == "notes" { return c.json }
            return nil
        }
    }

    func testMastheadShowsStandbyCueAsNumberDotNameWithItsNotes() async throws {
        let (wc, rig) = try await demoController()
        // The snapshot's playhead is shown from the start (an unnumbered cue: its name alone).
        XCTAssertEqual(wc.masthead.cueID, CueListHarness.fade)
        XCTAssertEqual(wc.masthead.standbyField.stringValue, "Fade")
        try standBy(rig, CueListHarness.intro)
        let cue = try XCTUnwrap(rig.store.model?.cue(CueListHarness.intro))
        XCTAssertEqual(
            wc.masthead.standbyField.stringValue,
            [cue.number, cue.listName].compactMap { $0 }.joined(separator: " \u{00B7} "))
        XCTAssertTrue(wc.masthead.notesView.isEditable)
    }

    func testMastheadNotesEditSendsTheInspectorNotesCommand() async throws {
        let (wc, rig) = try await demoController()
        try standBy(rig, CueListHarness.intro)
        typeMastheadNotes(wc, "Hold for applause")
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await assertEventually { !self.setNotesCommands(rig).isEmpty }
        XCTAssertEqual(setNotesCommands(rig).last?["id"]?.stringValue, CueListHarness.intro.raw)
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "Hold for applause")
    }

    func testMastheadNotesAreReadOnlyAndSendNothingInShowMode() async throws {
        let (wc, rig) = try await demoController()
        try standBy(rig, CueListHarness.intro)
        wc.setShowMode(true)
        await assertEventually { wc.masthead.isLocked }
        XCTAssertFalse(wc.masthead.notesView.isEditable)
        XCTAssertFalse(wc.masthead.notesView.isSelectable, "a focused text view would swallow GO")
        wc.masthead.notesView.string = "typed anyway"
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await wc.editor.setNotes(of: CueListHarness.intro, text: "typed anyway")
        XCTAssertTrue(setNotesCommands(rig).isEmpty)
    }

    private func typeMastheadNotes(_ wc: WorkspaceWindowController, _ text: String) {
        wc.masthead.notesView.string = text
        wc.masthead.textDidBeginEditing(Notification(name: NSText.didBeginEditingNotification))
    }

    func testPlayheadMovingMidEditCommitsTheNotesToTheCueTheyWereTypedFor() async throws {
        let (wc, rig) = try await demoController()
        try standBy(rig, CueListHarness.intro)
        typeMastheadNotes(wc, "for intro")
        try standBy(rig, CueListHarness.fade)
        await assertEventually { !self.setNotesCommands(rig).isEmpty }
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await Task.yield()
        let sent = setNotesCommands(rig)
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent.first?["id"]?.stringValue, CueListHarness.intro.raw)
        XCTAssertEqual(sent.first?["property"]?["value"]?.stringValue, "for intro")
        XCTAssertNotEqual(wc.masthead.notesView.string, "for intro", "the new cue shows its own notes")
    }

    func testEnteringShowModeMidEditCommitsTheMastheadNotes() async throws {
        let (wc, rig) = try await demoController()
        try standBy(rig, CueListHarness.intro)
        typeMastheadNotes(wc, "typed before show mode")
        await wc.requestShowMode(true)
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "typed before show mode")
    }

    func testSaveMidEditCommitsTheMastheadNotes() async throws {
        let (wc, rig) = try await demoController()
        try standBy(rig, CueListHarness.intro)
        typeMastheadNotes(wc, "typed before save")
        let ok = await wc.commitPendingEdits()
        XCTAssertTrue(ok)
        XCTAssertEqual(setNotesCommands(rig).count, 1)
        XCTAssertEqual(setNotesCommands(rig).last?["property"]?["value"]?.stringValue, "typed before save")
    }

    func testTypingForTheNewCueAfterAMidEditPlayheadMoveIsCommittedToIt() async throws {
        let (wc, rig) = try await demoController()
        try standBy(rig, CueListHarness.intro)
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        typeMastheadNotes(wc, "for intro")
        try standBy(rig, CueListHarness.fade)
        XCTAssertFalse(window.firstResponder === wc.masthead.notesView, "focus leaves so a new session starts")
        // The operator clicks back in and types for the new cue.
        XCTAssertTrue(window.makeFirstResponder(wc.masthead.notesView))
        typeMastheadNotes(wc, "for fade")
        wc.masthead.textDidEndEditing(Notification(name: NSText.didEndEditingNotification))
        await assertEventually { self.setNotesCommands(rig).count == 2 }
        let sent = setNotesCommands(rig)
        XCTAssertEqual(sent.last?["id"]?.stringValue, CueListHarness.fade.raw)
        XCTAssertEqual(sent.last?["property"]?["value"]?.stringValue, "for fade")
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
        try standBy(rig, CueListHarness.intro)
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
        try standBy(rig, CueListHarness.intro)
        let cue = try XCTUnwrap(rig.store.model?.cue(CueListHarness.intro))
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
