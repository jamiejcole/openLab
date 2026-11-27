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
    /// A closed window gives up its views: every test (and every workspace an operator closes) makes
    /// one, and views that outlived their windows piled up until the test machine's window server stalled.
    /// They go at once, with no run-loop turn: a split view's own queued layout save held them until a
    /// later pass, which on a busy machine came after any fixed number of turns (#679).
    func testAClosedWindowsViewsAreReleased() throws {
        weak var split: NSSplitView?
        weak var list: NSView?
        try autoreleasepool {
            let wc = try controller()
            wc.window?.layoutIfNeeded()
            split = wc.horizontalSplit
            list = wc.cueList.view
            XCTAssertFalse(wc.layoutObservers.isEmpty, "the dividers are followed from the start")
            wc.window?.close()
            XCTAssertTrue(wc.layoutObservers.isEmpty, "and no longer once the window has closed")
        }
        XCTAssertNil(split, "the split view went with its window")
        XCTAssertNil(list, "and the cue list with it")
    }

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
            $0.contains("Conflicting constraints") || $0.contains("Invalid view geometry")
        }.joined(separator: "\n")
    }

    /// A real window, shown at the smallest size it allows, at a small screen's size and at a large one,
    /// with each area hidden and shown, lays out without AppKit reporting a conflicting constraint or an invalid view
    /// geometry (both are read from stderr: AppKit offers nothing else to assert on).
    func testWindowLaysOutCleanlyAtEverySize() throws {
        var made: WorkspaceWindowController?
        // The window is built inside the capture: its first layout is where a conflict used to show.
        let first = stderrText { made = try? controller() }
        let wc = try XCTUnwrap(made)
        let window = try XCTUnwrap(wc.window)
        func layout() { window.contentView?.layoutSubtreeIfNeeded() }
        var problems: [String] = first.isEmpty ? [] : ["built: \(first.prefix(200))"]
        func step(_ what: String, _ body: () -> Void) {
            let report = stderrText(body)
            if !report.isEmpty { problems.append("\(what): \(report.prefix(200))") }
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

    /// Windows whose tab views gain their tabs while being built (the inspector's, the settings window's and
    /// the patch editor's) build without AppKit reporting an invalid view geometry: a tab view with no size
    /// lays its tab strip out at a negative width on macOS 27 (#530).
    func testTabViewsAreSizedBeforeTheirTabsAreAdded() throws {
        let store = try Rig.offline(TestSnapshots.show()).store
        let patch = try XCTUnwrap(store.model?.patches.first?.id)
        var built: [AnyObject] = []
        XCTAssertEqual(
            stderrText {
                built.append(WorkspaceWindowController(store: store))
                built.append(WorkspaceSettingsWindowController(store: store))
                built.append(
                    PatchEditorWindowController(model: WorkspaceSettingsModel(store: store), patch: patch))
            }, "")
        XCTAssertEqual(built.count, 3)
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
            AX.transportResume, AX.transportPanic, AX.footer, AX.modeSwitch,
            AX.settingsButton, AX.toggleMasthead, AX.toggleInspector, AX.toggleSidebar,
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

    /// In a narrow window the Name column is the one that gives up width: the Pre-Wait, Duration and
    /// Post-Wait columns keep their widths and are drawn whole inside the list. Widened again, the time
    /// columns are unchanged and the Name column takes the room back.
    func testTheNameColumnGivesUpWidthInANarrowWindowAndTheTimesStayWhole() throws {
        let wc = try controller()
        let window = try XCTUnwrap(wc.window)
        window.orderFront(nil)
        defer { window.close() }
        let outline = wc.cueList.outlineView
        func column(_ id: String) throws -> NSTableColumn {
            try XCTUnwrap(outline.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(id)))
        }
        func size(_ width: CGFloat) {
            window.setFrame(NSRect(x: 0, y: 0, width: width, height: 640), display: true)
            window.contentView?.layoutSubtreeIfNeeded()
        }
        let times = ["preWait", "duration", "postWait"]
        let widths = Dictionary(uniqueKeysWithValues: CueListViewController.columns.map { ($0.id, $0.width) })
        // Scroll bars always shown (a system setting) take the most room from the list.
        outline.enclosingScrollView?.scrollerStyle = .legacy
        // A window that opens at its narrowest: fitted before any resize.
        size(window.minSize.width)
        let last = outline.rect(ofColumn: outline.numberOfColumns - 1)
        let shown = try XCTUnwrap(outline.enclosingScrollView).contentView.bounds
        XCTAssertLessThanOrEqual(last.maxX, shown.maxX, "every column is inside a window that opens narrow")
        size(1200)
        let wideName = try column("name").width
        size(window.minSize.width)
        XCTAssertEqual(window.frame.width, window.minSize.width, "the narrowest window")
        let visible = try XCTUnwrap(outline.enclosingScrollView).contentView.bounds
        let lastColumn = outline.rect(ofColumn: outline.numberOfColumns - 1)
        XCTAssertLessThanOrEqual(lastColumn.maxX, visible.maxX, "the last column is in view at the least width")
        for id in times {
            XCTAssertEqual(try column(id).width, try XCTUnwrap(widths[id]), "\(id) keeps its width")
            let header = outline.rect(ofColumn: outline.column(withIdentifier: NSUserInterfaceItemIdentifier(id)))
            XCTAssertGreaterThanOrEqual(header.minX, visible.minX, "\(id) starts inside the list")
            XCTAssertLessThanOrEqual(header.maxX, visible.maxX, "\(id) ends inside the list, not cut off")
        }
        XCTAssertLessThan(try column("name").width, wideName, "the Name column gave up the width")
        XCTAssertGreaterThanOrEqual(try column("name").width, CueListViewController.nameMinWidth)
        size(1200)
        XCTAssertLessThanOrEqual(
            outline.frame.width, try XCTUnwrap(outline.enclosingScrollView).contentView.bounds.width,
            "the list is no wider than what shows: no sideways scrolling in a wide window")
        for id in times { XCTAssertEqual(try column(id).width, try XCTUnwrap(widths[id]), "\(id) after widening") }
        XCTAssertEqual(try column("name").width, wideName, accuracy: 1, "the Name column took the room back")
    }

    func testSidebarHiddenByDefault() throws {
        let wc = try controller()
        XCTAssertFalse(wc.isVisible(.sidebar))
        XCTAssertEqual(wc.footer.toggles[.sidebar]?.state, .off)
        for area in [LayoutArea.toolbar, .masthead, .inspector] { XCTAssertTrue(wc.isVisible(area)) }
    }

    func testEachFooterToggleHidesAndReshowsItsArea() throws {
        let wc = try controller()
        // The cue toolbar has no footer button of its own: the masthead's hides it with the GO area.
        for area in LayoutArea.allCases where area != .toolbar {
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
        wc.cueList.outlineView.deselectAll(nil)
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

    func testEveryIconResolves() {
        for icon in Icon.allCases {
            XCTAssertTrue(icon.resolvesToSymbol, "\(icon)")
            let image = icon.image("label")
            XCTAssertEqual(image.accessibilityDescription, "label", "\(icon)")
            XCTAssertTrue(image.isTemplate, "\(icon)")
            if let symbol = icon.symbol {
                XCTAssertNil(icon.drawing, "\(icon)")
                XCTAssertNotNil(NSImage(systemSymbolName: symbol, accessibilityDescription: nil), "\(icon) \(symbol)")
            } else if icon != .transportPanic {
                // One of openLab's own drawings: in the bundle, a template, on the 16-point grid.
                XCTAssertNotNil(icon.drawing, "\(icon)")
                XCTAssertEqual(image.size, NSSize(width: 16, height: 16), "\(icon)")
                XCTAssertEqual(IconLookup.drawingName(of: image), icon.drawing, "\(icon)")
            }
        }
        // The call sites that expose their icons use only table rows.
        XCTAssertFalse((CueToolbarView.icons + SidebarViewController.icons + FooterBar.icons).isEmpty)
    }

    /// Flow (#698): each of the 34 approved drawings replaces its SF Symbol: the concept names its image set,
    /// and no symbol is left in the table for it (the table's source names only the symbols still in use).
    func testEachApprovedConceptDrawsItsOwnImageSetAndNoSymbolRemains() throws {
        let approved: [Icon: String] = [
            .stateRunning: "state-running", .statePaused: "state-paused", .stateLoaded: "state-loaded",
            .stateBroken: "state-broken", .stateStopping: "state-stopping",
            .flagged: "flagged", .flaggedInside: "flagged-inside", .warningMarker: "warning-marker",
            .outputOverridden: "output-overridden", .preview: "preview", .continueAuto: "continue-auto",
            .continueFollow: "continue-follow", .transportReset: "transport-reset", .transportPause: "transport-pause",
            .transportResume: "transport-resume", .activeCuePanic: "active-cue-panic",
            .layoutMasthead: "layout-masthead", .layoutInspector: "layout-inspector", .layoutSidebar: "layout-sidebar",
            .settings: "settings",
            .statusWarning: "status-warning", .statusAlarm: "status-alarm", .playhead: "playhead",
            .timeToggle: "time-toggle", .timeToggleUp: "time-toggle-up", .targetFile: "target-file",
            .targetFileCopying: "target-file-copying",
            .fieldClear: "field-clear", .fieldChoose: "field-choose", .fieldJump: "field-jump",
            .fieldReveal: "field-reveal",
            .reservedPopOut: "reserved-pop-out",
        ]
        XCTAssertEqual(approved.count, 32)
        for (icon, name) in approved {
            XCTAssertEqual(icon.drawing, name, "\(icon)")
            XCTAssertNil(icon.symbol, "\(icon) keeps a symbol")
            XCTAssertEqual(IconLookup.drawingName(of: icon.image("label")), name, "\(icon) draws its image set")
        }
        let file = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("openLab/Icons.swift")
        let text = try String(contentsOf: file, encoding: .utf8)
        let regex = try NSRegularExpression(pattern: "\"([a-z][a-z0-9]*(?:\\.[a-z0-9]+)*)\"")
        let symbols = Set(
            regex.matches(in: text, range: NSRange(text.startIndex..., in: text)).compactMap { m in
                Range(m.range(at: 1), in: text).map { String(text[$0]) }
            }.filter { NSImage(systemSymbolName: $0, accessibilityDescription: nil) != nil })
        XCTAssertEqual(symbols, Set(Icon.allCases.compactMap(\.symbol)), "only the rows still in use name symbols")
        XCTAssertEqual(symbols, ["pause.fill", "play.fill", "trash"])
    }

    func testAMissingSymbolOrDrawingShowsTheLocalisedNameAndLogsOnce() {
        defer { IconLookup.shared.setLookup(nil) }
        IconLookup.shared.setLookup { _ in false }
        let text = Icon.targetFileMissing.image("Alarm")
        XCTAssertEqual(text.accessibilityDescription, "Alarm")
        XCTAssertTrue(text.isTemplate)
        XCTAssertGreaterThan(text.size.width, Icon.targetFileMissing.image("A", compact: true).size.width)
        _ = Icon.targetFileMissing.image("Alarm")
        XCTAssertEqual(IconLookup.shared.reportedNames, ["trash"])
        _ = Icon.settings.image("Settings")
        XCTAssertEqual(IconLookup.shared.reportedNames, ["trash", "settings"])
        XCTAssertEqual(Icon.settings.image("Settings").accessibilityDescription, "Settings")
        XCTAssertFalse(Icon.settings.resolvesToSymbol)
        XCTAssertFalse(Icon.targetFileMissing.resolvesToSymbol)
    }

    /// Flow (#698): the footer, the inspector's Target field buttons and the transport strip show the approved
    /// drawings: Reset, Pause and Resume All the circle outline, Panic All its own picture, and the footer's
    /// all-well mark green while its words keep the text colour.
    func testTheFooterFieldButtonsAndTransportShowTheApprovedDrawings() async throws {
        let (wc, rig) = try await demoController()
        _ = rig
        wc.window?.layoutIfNeeded()
        let footer = wc.footer
        for (area, name) in [
            (LayoutArea.masthead, "layout-masthead"), (.inspector, "layout-inspector"),
            (.sidebar, "layout-sidebar"),
        ] {
            XCTAssertEqual(IconLookup.drawingName(of: footer.toggles[area]?.image), name, "\(area)")
        }
        XCTAssertEqual(IconLookup.drawingName(of: footer.settingsButton.image), "settings")
        footer.showWarnings(count: 2)
        XCTAssertEqual(IconLookup.drawingName(of: footer.warningsButton.image), "status-warning")
        XCTAssertEqual(footer.warningsButton.contentTintColor, MarkColour.caution, "yellow, as in the list")
        let inspector = wc.inspector
        inspector.loadViewIfNeeded()
        XCTAssertEqual(IconLookup.drawingName(of: inspector.clearTargetButton.image), "field-clear")
        XCTAssertEqual(IconLookup.drawingName(of: inspector.jumpTargetButton.image), "field-jump")
        XCTAssertEqual(IconLookup.drawingName(of: inspector.chooseButton.image), "field-choose")
        wc.sidebar.loadViewIfNeeded()
        let strip = wc.sidebar.transportButtons
        XCTAssertEqual(IconLookup.drawingName(of: strip[AX.transportReset]?.image), "transport-reset")
        XCTAssertEqual(IconLookup.drawingName(of: strip[AX.transportPause]?.image), "transport-pause")
        XCTAssertEqual(IconLookup.drawingName(of: strip[AX.transportResume]?.image), "transport-resume")
        let panic = try XCTUnwrap(strip[AX.transportPanic]?.image)
        XCTAssertNil(IconLookup.drawingName(of: panic), "Panic All keeps its own picture")
        XCTAssertEqual(panic.accessibilityDescription, loc("transport.panic"))
        XCTAssertEqual(IconLookup.drawingName(of: ActiveCueCell().panicButton.image), "active-cue-panic")
        // Each drawing is shown at the 16 points it is drawn on, never scaled down until its shapes run together.
        wc.cueList.selectAsUser([TestSnapshots.fade])
        await assertEventually { !inspector.jumpTargetButton.isHidden }
        wc.window?.layoutIfNeeded()
        let buttons: [(String, NSButton)] =
            [
                ("clear", inspector.clearTargetButton), ("jump", inspector.jumpTargetButton),
                ("settings", footer.settingsButton),
            ] + footer.toggles.map { ("\($0.key)", $0.value) }
        for (name, button) in buttons {
            let cell = try XCTUnwrap(button.cell as? NSButtonCell, name)
            let drawn = cell.imageRect(forBounds: button.bounds).size
            XCTAssertGreaterThanOrEqual(drawn.width, 16, "\(name) drawn \(drawn)")
            XCTAssertGreaterThanOrEqual(drawn.height, 16, "\(name) drawn \(drawn)")
        }
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
        await assertEventually { rig.store.engineHeads[TestSnapshots.mainList] == cue }
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
            wc.masthead.standbyField.stringValue, "fade and stop Intro",
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

    /// A workspace just opened has its playhead on the first cue; that cue is selected and the inspector shows it,
    /// not "no cue selected" (the playhead and the selection are locked together).
    func testAnOpenedWorkspaceSelectsThePlayheadCueAndTheInspectorShowsIt() async throws {
        let (wc, rig) = try await demoController()
        wc.window?.contentView?.layoutSubtreeIfNeeded()
        let head = try XCTUnwrap(rig.store.playheads[TestSnapshots.mainList], "the playhead is on a cue")
        await assertEventually("the playhead's cue is selected") { wc.cueList.selectedIDs() == [head] }
        await assertEventually("the inspector shows a cue") { wc.inspector.emptyLabel.isHidden }
    }

    /// The standby line's box is as tall as its text and the room above and below it, in both modes, so the
    /// letters' tails are not cut off.
    func testStandbyLineBoxHoldsItsWholeText() async throws {
        let (wc, rig) = try await demoController()
        try await standBy(rig, TestSnapshots.intro)
        for show in [false, true] {
            wc.setShowMode(show)
            wc.window?.contentView?.layoutSubtreeIfNeeded()
            let field = wc.masthead.standbyField
            let font = try XCTUnwrap(field.font)
            let line = ceil(font.ascender - font.descender + font.leading)
            XCTAssertGreaterThanOrEqual(
                field.frame.height, line + 2 * standbyFieldInset,
                "show mode \(show): the box holds the text's whole line")
        }
    }

    /// A cue with a 120-character name standing by: the window keeps its width and the name is cut short
    /// inside the masthead, in Show Mode at 1200 pt and in Edit Mode at the smallest width the window allows.
    func testALongStandbyNameIsCutShortAndNeverWidensTheWindow() async throws {
        let (wc, rig) = try await demoController()
        let window = try XCTUnwrap(wc.window)
        window.orderFront(nil)
        defer { window.close() }
        let long = String(repeating: "Thunder rumble in the distance ", count: 4)
        XCTAssertGreaterThanOrEqual(long.count, 120)
        func check(_ what: String, width: CGFloat) {
            window.setContentSize(NSSize(width: width, height: 700))
            window.contentView?.layoutSubtreeIfNeeded()
            let field = wc.masthead.standbyField
            XCTAssertEqual(window.contentView?.bounds.width ?? 0, width, accuracy: 0.5, "\(what): the window's width")
            let frame = field.convert(field.bounds, to: wc.masthead)
            XCTAssertTrue(wc.masthead.bounds.contains(frame), "\(what): the name at \(frame) is inside the masthead")
            XCTAssertGreaterThan(
                field.intrinsicContentSize.width, field.bounds.width, "\(what): the name is longer than its field")
            XCTAssertEqual(field.lineBreakMode, .byTruncatingTail, "\(what): cut short with an ellipsis")
        }
        window.setContentSize(NSSize(width: 1200, height: 700))
        guard case .success = await rig.store.send(command: .setProperty(TestSnapshots.intro, .name(long))) else {
            return XCTFail("rename")
        }
        await assertEventually { wc.masthead.standbyField.stringValue.hasSuffix(long) }
        wc.setShowMode(true)
        check("Show Mode at 1200", width: 1200)
        wc.setShowMode(false)
        check("Edit Mode at the least width", width: window.minSize.width)
    }

    /// Flow: the cue toolbar shows openLab's own drawing for each kind (a 16-point template read out
    /// as the kind's name, not the text stand-in), each whole inside its button, at the default
    /// window size and at the least one.
    func testTheCueToolbarShowsTheSameDrawingsEachWholeInsideItsButton() async throws {
        let (wc, _) = try await demoController()
        let window = try XCTUnwrap(wc.window)
        window.orderFront(nil)
        defer { window.close() }
        let content = try XCTUnwrap(window.contentView)
        func check(_ what: String, size: NSSize) throws {
            window.setFrame(NSRect(origin: window.frame.origin, size: size), display: true)
            content.layoutSubtreeIfNeeded()
            XCTAssertEqual(window.frame.size, size, what)
            let bar = wc.cueToolbar
            XCTAssertFalse(bar.isHidden, what)
            var shown: [Icon] = []
            for control in bar.segmentedControls {
                let frame = control.convert(control.bounds, to: content)
                XCTAssertTrue(content.bounds.contains(frame), "\(what): the button at \(frame) is in the window")
                XCTAssertTrue(
                    bar.convert(bar.bounds, to: content).contains(frame), "\(what): the button is in the toolbar")
                for i in 0..<control.segmentCount {
                    let image = try XCTUnwrap(control.image(forSegment: i), "\(what) \(i)")
                    let icon = try XCTUnwrap(Icon.drawing(shownBy: image), "\(what) \(i): a drawing, not text")
                    shown.append(icon)
                    XCTAssertTrue(image.isTemplate, "\(what) \(icon)")
                    XCTAssertEqual(image.size, NSSize(width: 16, height: 16), "\(what) \(icon)")
                    XCTAssertEqual(image.accessibilityDescription, control.toolTip(forSegment: i), "\(what) \(icon)")
                    XCTAssertLessThanOrEqual(image.size.width, control.width(forSegment: i), "\(what) \(icon)")
                    XCTAssertLessThanOrEqual(image.size.height, control.bounds.height, "\(what) \(icon)")
                }
            }
            XCTAssertEqual(shown, CueToolbarView.icons, what)
            // The label each button is read out by is its kind's name.
            let labels = Dictionary(uniqueKeysWithValues: CueKindStyle.kinds.values.map { ($0.icon, loc($0.labelKey)) })
            for (control, icons) in zip(bar.segmentedControls, CueToolbarView.kinds) {
                for i in icons.indices {
                    XCTAssertEqual(
                        control.image(forSegment: i)?.accessibilityDescription,
                        Icon.drawing(shownBy: control.image(forSegment: i)).flatMap { labels[$0] }, what)
                }
            }
        }
        try check("the default size", size: window.frame.size)
        try check("the least size", size: window.minSize)
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

    /// Opening the sidebar (the footer's toggle) on each tab of each kind at the least window size: the
    /// window keeps the size the user gave it, the sidebar is shown and the inspector's content stays
    /// inside the inspector.
    func testOpeningTheSidebarAtTheLeastSizeKeepsTheWindowOnEveryTab() async throws {
        let video = try await addVideoCue()
        let fadeOnVideo = try await addFade(aimedAt: video)
        window.setFrame(NSRect(origin: userFrame.origin, size: window.minSize), display: true)
        settleLayout()
        let least = window.frame
        XCTAssertEqual(least.size, window.minSize)
        let sidebar = try XCTUnwrap(wc.footer.toggles[.sidebar])
        for cue in [audio, video, fade, fadeOnVideo, group, memo] {
            await select(cue)
            // The tabs the cue shows (a Fade aimed at a Video cue has a Geometry tab).
            let ids = wc.inspector.tabView.tabViewItems.compactMap { $0.identifier as? String }
            XCTAssertFalse(ids.isEmpty)
            if cue == fadeOnVideo { XCTAssertTrue(ids.contains("fadeGeometry"), "\(ids)") }
            for id in ids {
                // Each tab from the same start: the least window with the sidebar closed.
                if wc.isVisible(.sidebar) { sidebar.performClick(nil) }
                window.setFrame(least, display: true)
                try clickTab(id)
                settleLayout()
                let before = window.frame
                XCTAssertFalse(wc.isVisible(.sidebar), "the sidebar starts closed")
                sidebar.performClick(nil)
                settleLayout()
                XCTAssertTrue(wc.isVisible(.sidebar), "the sidebar opens (tab \(id))")
                XCTAssertEqual(window.frame, before, "the window keeps its frame with the sidebar open on tab \(id)")
                // Nothing is cut or overlapping sideways: what a tab lays out sits inside its width and
                // no two things side by side share a place.
                let content = try XCTUnwrap(wc.inspector.tabView.selectedTabViewItem?.view)
                XCTAssertEqual(sideways(content, depth: 3), [], "tab \(id) of cue \(cue) is cut or overlapping")
                XCTAssertGreaterThan(wc.sidebar.view.frame.width, 150, "the sidebar has its room")
                if id == "fadeGeometry" { try assertStageShape(wc.inspector.fadeGeometry.thumbnail, tab: id) }
                if id == "geometry" { try assertStageShape(wc.inspector.geometry.thumbnail, tab: id) }
            }
        }
    }

    /// An Audio cue's Levels tab shown at the least window size, with and without the sidebar: the window
    /// keeps its frame and the tab's content stays inside the inspector (it scrolls if it needs more room).
    func testTheLevelsTabAtTheLeastSizeKeepsTheWindow() async throws {
        window.setFrame(NSRect(origin: userFrame.origin, size: window.minSize), display: true)
        settleLayout()
        let least = window.frame
        XCTAssertEqual(least.size, window.minSize)
        await select(audio)
        for sidebarOpen in [false, true] {
            if wc.isVisible(.sidebar) != sidebarOpen { try XCTUnwrap(wc.footer.toggles[.sidebar]).performClick(nil) }
            try clickTab("basics")
            try clickTab("levels")
            XCTAssertEqual(window.frame, least, "the window keeps its frame (sidebar open: \(sidebarOpen))")
            let content = try XCTUnwrap(wc.inspector.tabView.selectedTabViewItem?.view)
            XCTAssertEqual(sideways(content, depth: 3), [], "the Levels tab is cut or overlapping")
            // The column of controls is whole in what the user sees of it: nothing scrolled out of its area,
            // with overlay scrollers and with always-visible ones (which take room beside the column).
            let levels = wc.inspector.audioLevels
            for style in [NSScroller.Style.overlay, .legacy] {
                levels.sideScroll.scrollerStyle = style
                settleLayout()
                let clip = levels.sideScroll.contentView
                let column = try XCTUnwrap(levels.sideScroll.documentView)
                let what = "\(style == .legacy ? "legacy" : "overlay") scrollers, sidebar open: \(sidebarOpen)"
                XCTAssertLessThanOrEqual(column.frame.height, clip.bounds.height + 0.5, "the column fits: \(what)")
                XCTAssertLessThanOrEqual(column.frame.width, clip.bounds.width + 0.5, "no scroller over it: \(what)")
                XCTAssertTrue(levels.board.bounds.contains(levels.meter.frame), "the meters lie on the board: \(what)")
                for control in [
                    levels.defaultButton, levels.silentButton, levels.gangsButton, levels.clearMutesButton,
                    levels.editPatchButton,
                ] {
                    let frame = control.convert(control.bounds, to: clip)
                    XCTAssertTrue(clip.bounds.insetBy(dx: -0.5, dy: -0.5).contains(frame), "\(control.title): \(what)")
                }
            }
            levels.sideScroll.scrollerStyle = .overlay
        }
    }

    /// A control in the Basics target row wider than here (as on another macOS, whose controls and fonts
    /// measure differently), so wide that the path would need less than its preferred 90 pt: at the least
    /// window size, with the sidebar open, the file's path gives way, not the sidebar or the window. The
    /// inspector spans the window under the sidebar, so the row has the window's width; the control is
    /// widened by what the path has beyond 90 pt and 16 pt more.
    func testTheBasicsTargetRowGivesWayWhenItsControlsAreWider() async throws {
        window.setFrame(NSRect(origin: userFrame.origin, size: window.minSize), display: true)
        settleLayout()
        let least = window.frame
        await select(audio)
        try clickTab("basics")
        let path = wc.inspector.targetPath
        XCTAssertFalse(path.isHidden, "an Audio cue's target is its file's path")
        if !wc.isVisible(.sidebar) { try XCTUnwrap(wc.footer.toggles[.sidebar]).performClick(nil) }
        settleLayout()
        XCTAssertTrue(wc.isVisible(.sidebar))
        let spare = path.frame.width - 90
        XCTAssertGreaterThan(spare, 0, "the path has its preferred width to begin with")
        let choose = wc.inspector.chooseButton
        let wider = choose.widthAnchor.constraint(equalToConstant: choose.frame.width + spare + 16)
        wider.isActive = true
        defer { wider.isActive = false }
        settleLayout()
        XCTAssertEqual(window.frame, least, "the window keeps its frame")
        XCTAssertEqual(wc.sidebar.view.frame.width, 280, accuracy: 3, "the sidebar keeps its width")
        XCTAssertLessThan(path.frame.width, 90, "the path gave way")
        XCTAssertGreaterThanOrEqual(path.frame.width, 40, "and keeps a usable width")
        let content = try XCTUnwrap(wc.inspector.tabView.selectedTabViewItem?.view)
        XCTAssertEqual(sideways(content, depth: 3), [], "nothing is cut or overlapping")
    }

    /// The sidebar's tabs naming more than they do here (a busy show's "12 Active Cues", or the same
    /// labels on another macOS, whose segmented controls measure wider): opening the sidebar at the least
    /// window size on the Basics tab keeps the sidebar's width and the window's frame; the tabs fit the
    /// sidebar instead.
    func testTheSidebarKeepsItsWidthWhenItsTabsAreWider() async throws {
        window.setFrame(NSRect(origin: userFrame.origin, size: window.minSize), display: true)
        settleLayout()
        let least = window.frame
        await select(audio)
        try clickTab("basics")
        let tabs = wc.sidebar.tabs
        // Two tabs share the row, so labels this long stand for a busy show on a macOS that measures
        // segmented controls wider.
        tabs.setLabel("12 Cue Lists, carts and groups", forSegment: 0)
        tabs.setLabel("12 Active Cues running now", forSegment: 1)
        // The tabs ask for more than the sidebar's 280 pt, less its 10 pt margins, by more than the 12 pt
        // a newer macOS adds to the shorter labels.
        XCTAssertGreaterThan(tabs.intrinsicContentSize.width, 280 - 20 + 12)
        try XCTUnwrap(wc.footer.toggles[.sidebar]).performClick(nil)
        settleLayout()
        XCTAssertTrue(wc.isVisible(.sidebar))
        XCTAssertEqual(window.frame, least, "the window keeps its frame")
        XCTAssertEqual(wc.sidebar.view.frame.width, 280, accuracy: 1, "the sidebar keeps its width")
        let inSidebar = tabs.convert(tabs.bounds, to: wc.sidebar.view)
        XCTAssertTrue(wc.sidebar.view.bounds.contains(inSidebar), "the tabs fit the sidebar: \(inSidebar)")
        XCTAssertEqual(tabs.label(forSegment: 1), "12 Active Cues running now")
    }

    /// The Triggers tab at the least window size with the sidebar open: the hotkey field's prompt stays on
    /// one line (cut short), so the field is as tall as the Clear button beside it, not two lines tall.
    func testTheHotkeyPromptStaysOnOneLineInTheNarrowTriggersTab() async throws {
        window.setFrame(NSRect(origin: userFrame.origin, size: window.minSize), display: true)
        await select(audio)
        try clickTab("triggers")
        if !wc.isVisible(.sidebar) { try XCTUnwrap(wc.footer.toggles[.sidebar]).performClick(nil) }
        settleLayout()
        let field = wc.inspector.hotkeyField
        XCTAssertEqual(field.stringValue, "", "no key: the prompt shows")
        XCTAssertEqual(field.frame.width, 100, accuracy: 0.5)
        let prompt = (field.placeholderString ?? "") as NSString
        let font = try XCTUnwrap(field.font)
        let promptWidth = prompt.size(withAttributes: [.font: font]).width
        XCTAssertLessThanOrEqual(promptWidth, field.frame.width - 10, "the whole prompt shows, not cut")
        XCTAssertLessThanOrEqual(field.frame.height, wc.inspector.hotkeyClear.frame.height + 4, "one line tall")
    }

    /// The Geometry tab's preview draws its stage (and the black round it) in the stage's shape, inside
    /// the view, whatever the view's own shape.
    private func assertStageShape(_ t: GeometryThumbnail, tab: String, line: UInt = #line) throws {
        let aspect = t.stage.width / t.stage.height
        for (name, r) in [("stage", t.stageRect), ("backdrop", t.backdropRect)] {
            XCTAssertGreaterThan(r.height, 0, "\(tab): the \(name) is drawn", line: line)
            XCTAssertEqual(
                r.width / max(r.height, 1), aspect, accuracy: 0.05, "\(tab): the \(name) keeps the stage's shape",
                line: line)
            let inside = t.bounds.insetBy(dx: -0.5, dy: -0.5).contains(r)
            XCTAssertTrue(inside, "\(tab): the \(name) is inside the view", line: line)
        }
        XCTAssertTrue(t.backdropRect.contains(t.stageRect), "\(tab): the stage is on its black", line: line)
    }

    /// Where `view`'s visible children (down `depth` levels) run past it on the left or right, or lie
    /// over one another, as text.
    private func sideways(_ view: NSView, depth: Int) -> [String] {
        let kids = view.subviews.filter { !$0.isHidden && $0.frame.width > 0 && $0.frame.height > 0 }
        var found: [String] = []
        for (n, kid) in kids.enumerated() {
            if kid.frame.minX < -3 || kid.frame.maxX > view.bounds.width + 3 {
                found.append("\(type(of: kid)) \(kid.frame) runs outside \(view.bounds.width) pt")
            }
            for other in kids.dropFirst(n + 1) {
                let over = kid.frame.intersection(other.frame)
                if over.width > 0.5, over.height > 0.5, !(kid is NSScrollView), !(other is NSScrollView) {
                    found.append("\(type(of: kid)) \(kid.frame) overlaps \(type(of: other)) \(other.frame)")
                }
            }
            if depth > 1, !(kid is NSControl), !(kid is NSScrollView) {
                found += sideways(kid, depth: depth - 1)
            }
        }
        return found
    }

    /// A Fade cue aimed at `target`, after the others.
    private func addFade(aimedAt target: ModelID) async throws -> ModelID {
        let id: ModelID = "00000000-0000-0000-0000-0000000000e3"
        let list = try XCTUnwrap(rig.store.model?.cueLists.first?.id)
        var cue = try XCTUnwrap(NewCueKind.fade.cueJSON(id: id, patch: nil))
        var kind = try XCTUnwrap(cue["kind"])
        kind.set("target", .string(target.raw))
        cue.set("kind", kind)
        let added = await rig.store.send(command: .addCue(cue, list: list, parent: nil, index: 5))
        guard case .success = added else { return try XCTUnwrap(nil as ModelID?, "the engine adds the fade") }
        await assertEventually { self.rig.store.model?.cue(id) != nil }
        return id
    }

    private func addVideoCue() async throws -> ModelID {
        let id: ModelID = "00000000-0000-0000-0000-0000000000e2"
        let list = try XCTUnwrap(rig.store.model?.cueLists.first?.id)
        let cue = try XCTUnwrap(NewCueKind.video.cueJSON(id: id, patch: nil))
        guard case .success = await rig.store.send(command: .addCue(cue, list: list, parent: nil, index: 4)) else {
            throw XCTSkip("no video cue")
        }
        await assertEventually { self.rig.store.model?.cue(id) != nil }
        return id
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
        // The workspace list is its own message and can arrive after the window's snapshot.
        await assertEventually { app.session.workspaces.first { $0.doc == doc }?.path != nil }
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
            again.topColumn.frame.width + split.dividerThickness + again.sidebar.view.frame.width,
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

    /// View > Cue Toolbar is remembered on its own beside the GO-area toggle, which also hides the
    /// toolbar: the toolbar hidden alone stays hidden, and the toolbar shown again under a hidden GO
    /// area stays shown.
    func testTheCueToolbarChoiceComesBackApartFromTheGoArea() async throws {
        let url = try showFile("Toolbar")
        let (doc, wc) = try await open(url)
        app.keyWindow = { wc.window }
        app.toggleLayout(.toolbar)
        XCTAssertTrue(wc.cueToolbar.isHidden)
        XCTAssertFalse(wc.masthead.isHidden)
        await close(doc)

        let (doc2, again) = try await open(url)
        XCTAssertTrue(again.cueToolbar.isHidden, "the toolbar hidden alone stays hidden")
        XCTAssertFalse(again.masthead.isHidden, "the GO area stays")
        XCTAssertFalse(again.isVisible(.toolbar))

        // The reverse: the GO area hidden (which hides the toolbar too), then the toolbar shown alone.
        app.keyWindow = { again.window }
        try toggle(.masthead, in: again)
        XCTAssertTrue(again.masthead.isHidden)
        app.toggleLayout(.toolbar)
        XCTAssertFalse(again.cueToolbar.isHidden)
        await close(doc2)

        let (_, third) = try await open(url)
        XCTAssertTrue(third.masthead.isHidden, "the GO area stays hidden")
        XCTAssertFalse(third.cueToolbar.isHidden, "the toolbar shown alone stays shown")
        XCTAssertEqual(third.footer.toggles[.masthead]?.state, .off)
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
        app.pickSaveURL = { _, _ in SaveTarget(url: new) }
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
        app.pickSaveURL = { _, _ in SaveTarget(url: url) }
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

    /// The divider dragged down to leave the inspector 120 pt: it stays at its least height or more, and
    /// the Time & Loops Start and End fields are whole inside it.
    func testADividerDragNeverLeavesTheInspectorShorterThanItsLeastHeight() async throws {
        let url = try showFile("ShortInspector")
        let (_, wc) = try await open(url)
        wc.window?.setFrame(frame(wc), display: true)
        let added = await wc.editor.add(.audio)
        let cue = try XCTUnwrap(added, "a new audio cue")
        wc.cueList.selectAsUser([cue])
        await assertEventually { wc.inspector.cueForEdit?.id == cue }
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "timeLoops")
        settle(wc)
        let room = wc.verticalSplit.bounds.height - wc.verticalSplit.dividerThickness
        wc.verticalSplit.setPosition(room - 120, ofDividerAt: 0)
        settle(wc)
        let least = WorkspaceWindowController.minInspectorHeight
        XCTAssertGreaterThanOrEqual(wc.inspector.view.frame.height, least, "the drag stopped at the least height")
        let panel = wc.inspector.timeLoops
        for (name, field) in [("Start", panel.startField), ("End", panel.endField)] {
            let frame = field.convert(field.bounds, to: wc.inspector.view)
            XCTAssertTrue(
                wc.inspector.view.bounds.contains(frame),
                "\(name) at \(frame) is outside the inspector \(wc.inspector.view.bounds)")
        }
    }

    /// U3: the inspector's divider dragged. Well below the least height the inspector is closed while the
    /// button is still down, dragged back up while still held it is open again (only the release
    /// decides), a little below the least height it stays at the least; left closed, the footer button
    /// brings it back. A real press on the divider runs the same drag.
    func testDraggingTheInspectorDividerFarDownClosesTheInspectorWhileHeldAndBackUpOpensIt() async throws {
        let url = try showFile("SnapInspector")
        let (_, wc) = try await open(url)
        wc.window?.setFrame(frame(wc), display: true)
        settle(wc)
        let split = wc.verticalSplit
        let least = WorkspaceWindowController.minInspectorHeight
        func press() throws -> WorkspaceWindowController.DividerDrag {
            let y = split.bounds.height - wc.inspector.view.frame.height - split.dividerThickness / 2
            return try XCTUnwrap(
                wc.beginDividerDrag(at: split.convert(NSPoint(x: 60, y: y), to: nil)), "a press on the divider")
        }
        // Just past the least height: stays open at the least.
        var drag = try press()
        drag.move(toInspectorHeight: least + 20)
        drag.move(toInspectorHeight: least - 10)
        XCTAssertTrue(wc.isVisible(.inspector), "just past the least height, still open while held")
        drag.end()
        settle(wc)
        XCTAssertTrue(wc.isVisible(.inspector))
        XCTAssertGreaterThanOrEqual(wc.inspector.view.frame.height, least)
        // Far down: closed while the button is still down (nothing has ended the drag yet).
        drag = try press()
        drag.move(toInspectorHeight: least - 30)
        XCTAssertTrue(wc.isVisible(.inspector))
        drag.move(toInspectorHeight: 10)
        XCTAssertFalse(wc.isVisible(.inspector), "closed the moment the drag passed the line, button still down")
        XCTAssertEqual(wc.footer.toggles[.inspector]?.state, .off)
        // Back up, still held: open again.
        drag.move(toInspectorHeight: least + 40)
        XCTAssertTrue(wc.isVisible(.inspector), "open again when dragged back up while held")
        XCTAssertEqual(wc.footer.toggles[.inspector]?.state, .on)
        drag.end()
        settle(wc)
        XCTAssertTrue(wc.isVisible(.inspector), "released above the line: open")
        XCTAssertGreaterThanOrEqual(wc.inspector.view.frame.height, least)
        // Far down and let go there: closed for good, and the footer button brings it back.
        drag = try press()
        drag.move(toInspectorHeight: 10)
        XCTAssertFalse(wc.isVisible(.inspector))
        drag.end()
        XCTAssertFalse(wc.isVisible(.inspector), "left closed")
        XCTAssertEqual(wc.footer.toggles[.inspector]?.state, .off)
        try toggle(.inspector, in: wc)
        XCTAssertTrue(wc.isVisible(.inspector))
        XCTAssertGreaterThanOrEqual(wc.inspector.view.frame.height, least, "it comes back at a usable height")
        // The split's own mouse-down takes a press on the divider (the window's tracking loop, the release
        // queued): the height is kept and nothing else moves.
        let window = try XCTUnwrap(wc.window)
        let y = split.bounds.height - wc.inspector.view.frame.height - split.dividerThickness / 2
        let at = split.convert(NSPoint(x: 60, y: y), to: nil)
        func event(_ type: NSEvent.EventType, at point: NSPoint? = nil) throws -> NSEvent {
            try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: point ?? at, modifierFlags: [],
                    timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
                    eventNumber: 0, clickCount: 1, pressure: 1))
        }
        NSApp.postEvent(try event(.leftMouseUp), atStart: false)
        let before = wc.inspector.view.frame.height
        split.mouseDown(with: try event(.leftMouseDown))
        XCTAssertTrue(wc.isVisible(.inspector))
        XCTAssertEqual(wc.inspector.view.frame.height, before, accuracy: 1)
        // The same press with drag events through the split's own mouse-down: down to the window's foot and
        // let go there closes it; down and back up before letting go leaves it open.
        let low = split.convert(NSPoint(x: 60, y: split.bounds.height - 10), to: nil)
        for type in [NSEvent.EventType.leftMouseDragged, .leftMouseDragged, .leftMouseUp] {
            NSApp.postEvent(try event(type, at: low), atStart: false)
        }
        split.mouseDown(with: try event(.leftMouseDown))
        XCTAssertFalse(wc.isVisible(.inspector), "a real drag to the foot closes the inspector")
        try toggle(.inspector, in: wc)
        settle(wc)
        let y2 = split.bounds.height - wc.inspector.view.frame.height - split.dividerThickness / 2
        let start = split.convert(NSPoint(x: 60, y: y2), to: nil)
        for (type, point) in [
            (NSEvent.EventType.leftMouseDragged, low), (.leftMouseDragged, start), (.leftMouseUp, start),
        ] {
            NSApp.postEvent(try event(type, at: point), atStart: false)
        }
        split.mouseDown(with: try event(.leftMouseDown, at: start))
        XCTAssertTrue(wc.isVisible(.inspector), "dragged down and back up while held: still open")
    }

    /// A saved inspector height below the least comes back as the least.
    func testASavedInspectorHeightBelowTheLeastComesBackAsTheLeast() async throws {
        let url = try showFile("SavedShort")
        let (doc, wc) = try await open(url)
        wc.window?.setFrame(frame(wc), display: true)
        settle(wc)
        let path = try XCTUnwrap(app.session.workspaces.first { $0.doc == doc }?.path)
        await close(doc)
        let store = WindowLayoutStore(defaults: defaults)
        var layout = store.layout(forPath: path)
        layout.inspectorHeight = 150
        store.save(layout, forPath: path)
        let (_, again) = try await open(url)
        settle(again)
        let least = WorkspaceWindowController.minInspectorHeight
        XCTAssertEqual(again.inspectorHeightConstraint?.constant, least, "150 was raised to the least height")
        XCTAssertGreaterThanOrEqual(again.inspector.view.frame.height, least)
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
        app.pickSaveURL = { _, _ in SaveTarget(url: new) }
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
        XCTAssertGreaterThanOrEqual(wc.cueList.view.frame.height, WorkspaceWindowController.minCueListHeight)
        // The least window has no room for the inspector's usual height: it keeps what the list leaves.
        XCTAssertGreaterThanOrEqual(wc.inspector.view.frame.height, 240)
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

    func testDefaultFrameFitsASmallScreenAndKeepsALargeOneAtDefaultSize() {
        let minSize = NSSize(width: 800, height: 500)
        let default1200 = NSRect(x: 0, y: 0, width: 1200, height: 828)
        let small = NSRect(x: 0, y: 0, width: 1024, height: 768 - 25)
        XCTAssertEqual(
            WindowLayout.fit(default1200, screens: [small], minSize: minSize),
            NSRect(x: 0, y: 0, width: 1024, height: 743), "the largest size that fits")
        let large = NSRect(x: 0, y: 0, width: 2560, height: 1415)
        XCTAssertEqual(WindowLayout.fit(default1200, screens: [large], minSize: minSize)?.size, default1200.size)
    }

    func testANewWorkspaceWindowOpensInsideItsScreensVisibleFrame() async throws {
        let (_, wc) = try await open(try showFile("Fits"))
        let window = try XCTUnwrap(wc.window)
        let visible = try XCTUnwrap((window.screen ?? NSScreen.main)?.visibleFrame)
        XCTAssertTrue(visible.insetBy(dx: -1, dy: -1).contains(window.frame), "\(window.frame) not in \(visible)")
    }

    func testStoredLayoutRoundTripsAndDropsOnlyItsBadFields() {
        var layout = WindowLayout()
        layout.frame = NSRect(x: 1, y: 2, width: 900, height: 600)
        layout.inspectorHeight = 300
        layout.sidebarWidth = 350
        layout.shown = [.inspector: false, .sidebar: true]
        layout.sidebarTab = 1
        XCTAssertEqual(WindowLayout(stored: layout.stored), layout)
        // A layout saved when the sidebar had a third tab (Warnings) names a tab that is gone: it is
        // dropped, and the sidebar opens on its first tab.
        var old = layout.stored
        old["sidebarTab"] = 2
        XCTAssertNil(WindowLayout(stored: old).sidebarTab)
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

final class ToolTipDelayTests: XCTestCase {
    /// The app (the test host) registered its own wait at launch, in the whole milliseconds AppKit reads.
    func testTheAppRegistersItsTooltipDelayAtLaunch() {
        let registered = UserDefaults.standard.volatileDomain(forName: UserDefaults.registrationDomain)
        XCTAssertEqual(registered[ToolTipDelay.defaultsKey] as? Int, 650)
        XCTAssertEqual(ToolTipDelay.seconds, 0.65)
    }

    /// Registered at launch, not set: a delay the user chose for openLab still wins. (The registration
    /// domain is the process's, so this reads what launch registered; the test registers nothing.)
    func testAUsersOwnTooltipDelayStillWins() throws {
        let name = "openlab.test.tooltip.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        XCTAssertEqual(defaults.integer(forKey: ToolTipDelay.defaultsKey), 650)
        defaults.set(100, forKey: ToolTipDelay.defaultsKey)
        XCTAssertEqual(defaults.integer(forKey: ToolTipDelay.defaultsKey), 100)
    }
}

// MARK: Workspace window layout (QLab 5 comparison, UI map workspace.window / workspace.footer)

extension MainWindowTests {
    private func laidOut(_ wc: WorkspaceWindowController, width: CGFloat = 1200, height: CGFloat = 800) {
        wc.window?.setContentSize(NSSize(width: width, height: height))
        for _ in 0..<3 { wc.window?.layoutIfNeeded() }
    }

    private func inWindow(_ view: NSView) -> NSRect { view.convert(view.bounds, to: nil) }

    /// Flow: sidebar open → it starts at the top of the window beside the GO area and ends above the
    /// inspector; the inspector spans the window's width.
    func testTheSidebarStartsBesideTheGoAreaAndEndsAboveTheInspectorWhichSpansTheWindow() async throws {
        let (wc, _) = try await demoController()
        laidOut(wc)
        wc.setVisible(.sidebar, true)
        laidOut(wc)
        let content = try XCTUnwrap(wc.window?.contentView)
        let side = inWindow(wc.sidebar.view)
        let go = inWindow(wc.masthead)
        let inspector = inWindow(wc.inspector.view)
        XCTAssertEqual(side.maxY, go.maxY, accuracy: 0.5, "the sidebar starts where the GO area starts")
        XCTAssertGreaterThanOrEqual(side.minX, go.maxX - 0.5, "beside the GO area, not under it")
        XCTAssertGreaterThanOrEqual(side.minY, inspector.maxY - 0.5, "and ends above the inspector")
        XCTAssertEqual(inspector.width, inWindow(content).width, accuracy: 0.5, "the inspector spans the window")
        XCTAssertEqual(inspector.minX, 0, accuracy: 0.5)
        let toolbar = inWindow(wc.cueToolbar)
        XCTAssertLessThanOrEqual(toolbar.maxX, side.minX + 0.5, "the toolbar row is beside it too")
        wc.setVisible(.sidebar, false)
        laidOut(wc)
        XCTAssertEqual(inWindow(wc.masthead).width, inWindow(content).width, accuracy: 0.5, "the GO area spans again")
    }

    /// Flow: the footer's warning mark sits with the right-hand buttons; one button hides the GO area and
    /// toolbar together; no audio text or clock while all is well.
    func testTheFooterKeepsTheWarningMarkWithTheRightButtonsAndOneButtonHidesTheGoAreaAndToolbar() async throws {
        let (wc, rig) = try await demoController()
        laidOut(wc)
        let footer = wc.footer
        footer.showWarnings(count: 2)
        laidOut(wc)
        let warn = footer.convert(footer.warningsButton.bounds, from: footer.warningsButton)
        let inspector = try XCTUnwrap(footer.toggles[.inspector])
        let sidebar = try XCTUnwrap(footer.toggles[.sidebar])
        let inspectorFrame = footer.convert(inspector.bounds, from: inspector)
        let sidebarFrame = footer.convert(sidebar.bounds, from: sidebar)
        let settings = footer.convert(footer.settingsButton.bounds, from: footer.settingsButton)
        let count = footer.convert(footer.countLabel.bounds, from: footer.countLabel)
        XCTAssertLessThanOrEqual(warn.maxX, inspectorFrame.minX, "the warning mark is left of the inspector button")
        XCTAssertLessThan(inspectorFrame.maxX, sidebarFrame.minX + 1)
        XCTAssertLessThan(sidebarFrame.maxX, settings.minX + 1)
        XCTAssertLessThan(inspectorFrame.minX - warn.maxX, 24, "and beside the right-hand buttons")
        XCTAssertGreaterThan(warn.minX, count.maxX, "not among the centre text")
        XCTAssertNil(footer.toggles[.toolbar], "no separate toolbar button")
        // The one button.
        let masthead = try XCTUnwrap(footer.toggles[.masthead])
        masthead.performClick(nil)
        XCTAssertTrue(wc.masthead.isHidden)
        XCTAssertTrue(wc.cueToolbar.isHidden)
        masthead.performClick(nil)
        XCTAssertFalse(wc.masthead.isHidden)
        XCTAssertFalse(wc.cueToolbar.isHidden)
        // The openLab-only audio line and wall clock are gone while all is well.
        await assertEventually("the engine's status arrives") {
            try? await rig.advance(1)
            return rig.store.status != nil
        }
        XCTAssertTrue(footer.alarmButton.isHidden, "all well: the footer says nothing about the audio")
        XCTAssertFalse(
            footer.subviews.flatMap(\.subviews).contains {
                $0 is NSTextField && $0.accessibilityIdentifier() == "wall-clock"
            })
    }

    /// Flow: the window at its least width with the sidebar open → every list column is whole; the first
    /// column gave up its blank width.
    func testAtTheLeastWidthWithTheSidebarOpenEveryListColumnIsWhole() async throws {
        let (wc, _) = try await demoController()
        laidOut(wc)
        wc.setVisible(.sidebar, true)
        let least = try XCTUnwrap(wc.window?.minSize)
        laidOut(wc, width: least.width, height: 700)
        let scroll = try XCTUnwrap(wc.cueList.view as? NSScrollView)
        let outline = wc.cueList.outlineView
        XCTAssertEqual(
            wc.window?.frame.width ?? 0, least.width, accuracy: 0.5, "the sidebar and the bars do not widen the window")
        let widest = outline.tableColumns.map(\.width).reduce(0, +)
        XCTAssertFalse(
            scroll.hasHorizontalScroller,
            "the columns fit: nothing to scroll sideways (outline frame \(outline.frame.width), spacing \(outline.intercellSpacing)) \(outline.tableColumns.map { "\($0.identifier.rawValue)=\($0.width)/\($0.minWidth)" }) in \(scroll.frame.width) inside \(scroll.contentView.bounds.width)"
        )
        XCTAssertLessThanOrEqual(widest, scroll.contentView.bounds.width, "every column is inside the list")
        for column in outline.tableColumns {
            XCTAssertGreaterThanOrEqual(column.width, column.minWidth, column.identifier.rawValue)
            XCTAssertGreaterThanOrEqual(
                column.width, column.headerCell.cellSize.width - 0.5, "\(column.identifier.rawValue)'s header is whole")
        }
        for id in ["preWait", "duration", "postWait"] {
            let column = outline.column(withIdentifier: NSUserInterfaceItemIdentifier(id))
            for row in 0..<outline.numberOfRows {
                guard let cell = outline.view(atColumn: column, row: row, makeIfNecessary: true) as? CueCell,
                    !cell.label.isHidden
                else { continue }
                XCTAssertLessThanOrEqual(
                    cell.label.attributedStringValue.size().width, outline.tableColumns[column].width - 4,
                    "\(id) row \(row) is whole: \(cell.label.stringValue)")
            }
        }
        // Every cue-type button is inside the toolbar row, none cut off at the sidebar's edge.
        let bar = wc.cueToolbar
        for control in bar.segmentedControls {
            let frame = bar.convert(control.bounds, from: control)
            XCTAssertLessThanOrEqual(
                frame.maxX, bar.bounds.maxX + 0.5, "a toolbar group reaches past the row: \(frame)")
            XCTAssertGreaterThanOrEqual(frame.minX, 0)
        }
        XCTAssertGreaterThanOrEqual(
            bar.segmentedControls.first?.width(forSegment: 0) ?? 0, CueToolbarView.leastSegmentWidth)
        let slotsEnd = CueCell.statusSlots.map { $0.x + $0.width }.max() ?? 0
        XCTAssertLessThanOrEqual(CueCell.statusWidth - slotsEnd, 4, "no blank width left in the first column")
    }

    /// Flow: the GO button and standby line are drawn alike in Edit and Show Mode, which only enlarges
    /// them; readable in light and dark appearance.
    func testTheGoButtonAndStandbyLineLookTheSameInShowModeJustLargerAndReadInLightAndDark() async throws {
        let (wc, _) = try await demoController()
        laidOut(wc)
        let m = wc.masthead
        func fill() -> CGColor? { m.goButton.layer?.backgroundColor }
        let editFill = fill()
        let editSize = m.goButton.frame.size
        let editFont = m.standbyField.font?.pointSize ?? 0
        let editBorder = m.goButton.layer?.borderColor
        XCTAssertEqual(m.goButton.layer?.borderWidth, 3, "a green edge")
        XCTAssertEqual(
            NSColor(cgColor: try XCTUnwrap(editBorder))?.usingColorSpace(.sRGB)?.greenComponent ?? 0,
            NSColor.systemGreen.usingColorSpace(.sRGB)?.greenComponent ?? 1, accuracy: 0.2)
        wc.setShowMode(true)
        laidOut(wc)
        XCTAssertEqual(fill(), editFill, "the same face")
        XCTAssertEqual(m.goButton.layer?.borderColor, editBorder)
        XCTAssertGreaterThan(m.goButton.frame.width, editSize.width)
        XCTAssertGreaterThan(m.goButton.frame.height, editSize.height)
        XCTAssertGreaterThan(m.standbyField.font?.pointSize ?? 0, editFont)
        // Contrast of the texts on their boxes, in each appearance.
        func luminance(_ c: NSColor) -> Double {
            let c = c.usingColorSpace(.sRGB) ?? c
            func lin(_ v: CGFloat) -> Double {
                Double(v) <= 0.03928 ? Double(v) / 12.92 : pow((Double(v) + 0.055) / 1.055, 2.4)
            }
            return 0.2126 * lin(c.redComponent) + 0.7152 * lin(c.greenComponent) + 0.0722 * lin(c.blueComponent)
        }
        func contrast(_ a: NSColor, _ b: NSColor) -> Double {
            let (x, y) = (luminance(a), luminance(b))
            return (max(x, y) + 0.05) / (min(x, y) + 0.05)
        }
        XCTAssertGreaterThanOrEqual(contrast(MastheadView.goText, MastheadView.goFill), 4.5)
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let appearance = try XCTUnwrap(NSAppearance(named: name))
            appearance.performAsCurrentDrawingAppearance {
                XCTAssertGreaterThanOrEqual(contrast(.labelColor, MastheadView.boxFill), 4.5, "\(name)")
            }
        }
    }

    /// Flow: the window title names the shown cue list.
    func testTheWindowTitleNamesTheShownCueList() async throws {
        let (wc, rig) = try await demoController()
        XCTAssertEqual(wc.window?.title, "openLab \u{2014} Main", "Edit Mode names the list from the start")
        wc.workspaceName = "Demo"
        XCTAssertEqual(wc.window?.title, "Demo \u{2014} Main")
        let other = ModelID(UUID().uuidString.lowercased())
        guard case .success = await rig.store.send(command: .addCueList(id: other, name: "Other", index: 1)) else {
            return XCTFail("add list")
        }
        await assertEventually { rig.store.model?.cueLists.contains { $0.id == other } == true }
        wc.cueList.showList(other)
        await assertEventually("the title follows the shown list") { wc.window?.title == "Demo \u{2014} Other" }
        guard case .success = await rig.store.send(command: .renameCueList(id: other, name: "Cart")) else {
            return XCTFail("rename list")
        }
        await assertEventually("and its rename") { wc.window?.title == "Demo \u{2014} Cart" }
        wc.setShowMode(true)
        await assertEventually {
            wc.window?.title == String(format: loc("window.main.title.showMode"), "Demo \u{2014} Cart")
        }
    }
}
