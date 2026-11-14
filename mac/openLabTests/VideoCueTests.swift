// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The Video cue in the app (CT-03, VID-03; QLab 5 manual pp. 209-212), each flow of #643 on the
/// real engine with a stage host on a virtual display: the operator's action on the window, then
/// what the list, the inspector and the footer show. The picture itself is read back from the
/// stage host where it is cheap (the frame number of the test card each test movie carries).
@MainActor
final class VideoCueTests: XCTestCase {
    var app: AppController!
    var engine: TestEngine!
    var doc: ModelID = "none"
    var folder: URL!
    var wc: WorkspaceWindowController { app.windows[doc]! }
    var store: WorkspaceStore { wc.store }

    // The Video test show (mac/TestShows/Video): stages and cues.
    static let stage: ModelID = "00000000-0000-0000-0000-0000000000f1"
    static let side: ModelID = "00000000-0000-0000-0000-0000000000f2"
    static let movie: ModelID = "00000000-0000-0000-0000-0000000000a1"
    static let still: ModelID = "00000000-0000-0000-0000-0000000000a2"
    static let timedStill: ModelID = "00000000-0000-0000-0000-0000000000a3"
    static let empty: ModelID = "00000000-0000-0000-0000-0000000000a4"
    static let long: ModelID = "00000000-0000-0000-0000-0000000000a5"
    static let noStage: ModelID = "00000000-0000-0000-0000-0000000000a6"
    static let longName = "A very long name for a projection clip used in the second act.mov"

    override func setUp() async throws {
        engine = try await TestEngine.start(displays: "V1:1920x1080")
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) })
        app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { [unowned self] in app.windows[doc]?.window }
        app.isTextEditing = { false }
        app.beep = {}
        session.start()
        await assertEventually { session.appClient.state == .connected }
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        engine?.stop()
    }

    /// Opens a fresh copy of the Video show with its media made: a 4 s H.264 movie, a PNG and a
    /// JPEG still, and a 2 s movie with a long name.
    func openVideoShow(edit: ((URL) throws -> Void)? = nil) async throws {
        folder = engine.folder.appendingPathComponent("video-\(UUID().uuidString)", isDirectory: true)
        let source = try XCTUnwrap(TestEngine.testShows).appendingPathComponent("Video")
        try FileManager.default.copyItem(at: source, to: folder)
        let media = folder.appendingPathComponent("media", isDirectory: true)
        try writeTestMedia(media.appendingPathComponent("clip.mov"), seconds: 4)
        try writeTestMedia(media.appendingPathComponent("still.png"), seconds: 0)
        try writeTestMedia(media.appendingPathComponent("card.jpg"), seconds: 0)
        try writeTestMedia(media.appendingPathComponent(Self.longName), seconds: 2)
        try edit?(folder)
        guard
            case .success(let opened) = await app.session.open(
                path: folder.appendingPathComponent("Video.openlab").path)
        else { return XCTFail("open") }
        doc = opened
        try await windowUp()
        // The engine's media probes finish as background work, waited for by an advance.
        let probed = await eventually {
            try? await self.play(0)
            return self.store.model?.cue(Self.movie) == nil || self.store.mediaDurations[Self.movie]?.picture != nil
        }
        if !probed {
            XCTFail(
                "the movie was never probed: \(store.problems[Self.movie] ?? []) \(String(engine.logText.suffix(3000)))"
            )
        }
    }

    /// A new untitled workspace (one stage per display, as File > New makes it).
    func openNewWorkspace() async throws {
        await assertEventually("the stage host listed its display") {
            guard case .success(let ok) = await self.engine.request(.listDisplays),
                case .displays(let list, _)? = ok.data
            else { return false }
            return !list.isEmpty
        }
        guard case .success(let made) = await app.session.newWorkspace() else { return XCTFail("new") }
        doc = made
        try await windowUp()
    }

    private func windowUp() async throws {
        await assertEventually("the window shows the workspace") {
            self.app.windows[self.doc]?.store.model != nil && self.app.windows[self.doc]?.cueList.listID != nil
        }
        wc.window?.setFrame(NSRect(x: 0, y: 0, width: 1300, height: 860), display: false)
        wc.window?.layoutIfNeeded()
    }

    // MARK: Doing what the operator does

    func sync() async { _ = await store.client.send({ .request(id: $0, .listPatchMappings) }) }

    func play(_ seconds: Double) async throws {
        try await engine.advance(seconds: seconds)
        await sync()
    }

    func click(_ id: ModelID) { wc.cueList.selectAsUser([id]) }

    /// The masthead's GO button, then the engine's answer.
    func go() async throws {
        wc.masthead.goButton.performClick(nil)
        await sync()
        try await play(0)
    }

    func perform(_ command: TransportCommand, on cue: ModelID) async throws {
        await app.perform(command, on: wc, cues: [cue])
        await sync()
        try await play(0)
    }

    /// `condition` comes true while the engine's background work (probes, file checks) is let run,
    /// device time standing still.
    func soon(
        _ message: String = "never became true", line: UInt = #line, _ condition: @MainActor () async -> Bool
    ) async {
        let ok = await eventually {
            try? await self.play(0)
            return await condition()
        }
        XCTAssertTrue(ok, message, line: line)
    }

    // MARK: What the window shows

    /// The text drawn in `column` of `id`'s row.
    func cell(_ id: ModelID, _ column: String) -> String? { cueCell(id, column)?.label.stringValue }

    func cueCell(_ id: ModelID, _ column: String) -> CueCell? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let row = ov.row(forItem: node)
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        guard row >= 0, col >= 0 else { return nil }
        return ov.view(atColumn: col, row: row, makeIfNecessary: true) as? CueCell
    }

    /// The run state the row's status cell draws (its icon's label), nil when none.
    func status(_ id: ModelID) -> String? {
        guard let cell = cueCell(id, "status"), !cell.images[1].isHidden else { return nil }
        return cell.images[1].accessibilityLabel()
    }

    /// The broken mark's tooltip: the reasons, as the Warnings tab reads them.
    func brokenTip(_ id: ModelID) -> String? {
        guard status(id) == loc("cue.state.broken") else { return nil }
        return cueCell(id, "status")?.images[1].toolTip
    }

    /// The Target column draws the missing-file mark for `id`.
    func missingMark(_ id: ModelID) -> Bool {
        cueCell(id, "target")?.chooseButton.accessibilityLabel() == loc("cuelist.target.fileMissing")
    }

    /// Seconds the Duration column shows for `id`.
    func duration(_ id: ModelID) -> Double? { cell(id, "duration").flatMap(CueTime.parse) }

    /// The row's Duration cell shows `seconds` of the cue's run, as an Audio cue's counts up (the
    /// engine's `progress` for the cue).
    func expectRowTime(_ id: ModelID, _ seconds: Double, _ what: String, line: UInt = #line) {
        XCTAssertEqual(duration(id) ?? -1, seconds, accuracy: 0.1, what, line: line)
        XCTAssertNotNil(store.progress[id], "\(what): progress", line: line)
    }

    /// The test card's frame number `stage` shows now (nil: no card, black).
    func shownFrame(_ stage: ModelID = VideoCueTests.stage) async -> UInt16? {
        guard
            case .success(let ok) = await store.client.send({
                .request(id: $0, .stageProbe(stage: stage, points: [[0.5, 0.5]]))
            }),
            case .stageProbe(let frame, _)? = ok.data
        else { return nil }
        return frame
    }

    // MARK: Flow 1: adding a Video cue

    /// The Video toolbar button, then its Cues menu item (Cmd-3, as QLab 5's): each adds a Video cue
    /// on the workspace's first stage, drawn with the Video drawing and named "Video"; with no file
    /// it is broken with that reason, as an Audio cue with no file is.
    func testTheVideoToolbarButtonOrItsShortcutAddsAVideoCueBrokenWithNoFile() async throws {
        try await openNewWorkspace()
        let firstStage = try XCTUnwrap(store.model?.stages.first?.id, "a new workspace has a stage per display")
        let group = try XCTUnwrap(CueToolbarView.kinds.firstIndex { $0 == [.video] }, "the Video button")
        XCTAssertEqual(group, 2, "after Audio, as QLab's toolbar")
        let before = wc.cueList.roots.count
        wc.cueToolbar.add(group: group, segment: 0)
        await soon("a cue was added and selected") {
            self.wc.cueList.roots.count == before + 1 && self.wc.cueList.selectedIDs().count == 1
        }
        let added = try XCTUnwrap(wc.cueList.selectedIDs().first)
        try await assertNewVideoCue(added, stage: firstStage)

        // Cues > Video (Cmd-3).
        let delegate = AppDelegate()
        delegate.appController = app
        let items = MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items)
        let entry = try XCTUnwrap(items.first { $0.keyEquivalent == "3" && $0.keyEquivalentModifierMask == [.command] })
        XCTAssertEqual(entry.representedObject as? String, "video")
        XCTAssertTrue(delegate.validateMenuItem(entry))
        delegate.newCue(entry)
        await soon("the menu added a second cue") { self.wc.cueList.roots.count == before + 2 }
        let second = try XCTUnwrap(wc.cueList.selectedIDs().first)
        XCTAssertNotEqual(second, added)
        try await assertNewVideoCue(second, stage: firstStage)
        wc.setShowMode(true)
        XCTAssertFalse(delegate.validateMenuItem(entry), "no new cues in Show Mode")
    }

    private func assertNewVideoCue(_ id: ModelID, stage: ModelID, line: UInt = #line) async throws {
        await soon("broken with no file", line: line) {
            self.brokenTip(id)?.contains(loc("problem.no_media")) == true
        }
        let cue = try XCTUnwrap(store.model?.cue(id), line: line)
        XCTAssertEqual(cue.kindType, "video", line: line)
        XCTAssertEqual(cue.raw["kind"]?["stage"]?.stringValue, stage.raw, line: line)
        let type = try XCTUnwrap(cueCell(id, "type"), line: line).images[0]
        XCTAssertEqual(Icon.drawing(shownBy: type.image), .kindVideo, line: line)
        XCTAssertEqual(type.accessibilityLabel(), loc("cue.type.video"), line: line)
        XCTAssertEqual(
            cell(id, "name"), loc("cue.type.video"), "an unnamed cue with no file reads as its kind", line: line)
        XCTAssertEqual(cell(id, "duration"), "", line: line)
        XCTAssertFalse(brokenTip(id)?.contains(loc("problem.no_stage")) ?? true, "it has a stage", line: line)
    }

    /// A workspace with no stage: the new Video cue has none, and says so as well as its missing file.
    func testAVideoCueAddedWhereThereIsNoStageIsBrokenForThatToo() async throws {
        try await openVideoShow { folder in
            try editShow(folder.appendingPathComponent("Video.openlab")) { ws in
                ws.set("stages", .array([]))
                ws.set(
                    "cue_lists",
                    .array([
                        .object([
                            "id": .string("00000000-0000-0000-0000-0000000000f3"), "name": .string("Main"),
                            "cues": .array([]),
                        ])
                    ]))
            }
        }
        let added = await wc.editor.add(.video)
        let id = try XCTUnwrap(added)
        await soon("no stage") { self.brokenTip(id)?.contains(loc("problem.no_stage")) == true }
        XCTAssertTrue(brokenTip(id)?.contains(loc("problem.no_media")) == true)
    }

    // MARK: Flow 2: a file makes or retargets a Video cue

    /// A movie and a still dropped on the list each make a Video cue named after its file, the
    /// movie with its length; a file chosen for the cue with none retargets it; a sound file is
    /// never a Video cue's target.
    func testAVideoFileDroppedOnTheListOrChosenAsTheTargetMakesOrRetargetsAVideoCue() async throws {
        try await openVideoShow()
        let media = folder.appendingPathComponent("media")
        let movie = media.appendingPathComponent("clip.mov")
        let png = media.appendingPathComponent("still.png")
        let wav = media.appendingPathComponent("tone.wav")
        try writeTestMedia(wav, seconds: 1)
        let plan = try XCTUnwrap(wc.cueList.fileDropPlan([movie, png, wav], item: nil, index: 6))
        XCTAssertEqual(plan, .insert([movie, png, wav], nil, 6), "movies and stills are taken, as sounds are")
        wc.cueList.onFileDrop?(plan, CueListViewController.skipped([movie, png, wav], plan))
        await soon("three cues were made") { self.wc.cueList.roots.count == 9 }
        let made = wc.cueList.roots.suffix(3).map(\.id)
        XCTAssertEqual(made.compactMap { store.model?.cue($0)?.kindType }, ["video", "video", "audio"])
        await soon("each is named after its file") {
            self.cell(made[0], "name") == "clip.mov" && self.cell(made[1], "name") == "still.png"
        }
        await soon("the movie's length") { self.duration(made[0]) == 4 }
        await soon("the still is probed") { self.store.mediaDurations[made[1]]?.picture?.type == "still" }
        XCTAssertEqual(cell(made[1], "duration"), "", "a still has no length until one is typed")
        XCTAssertNil(status(made[0]), "playable: no broken mark")

        // A still dropped on a Video cue's row replaces its file; a sound dropped there makes a cue.
        let on = NSOutlineViewDropOnItemIndex
        XCTAssertEqual(
            wc.cueList.fileDropPlan([png], item: wc.cueList.node(Self.empty), index: on), .replace(Self.empty, png))
        XCTAssertEqual(
            wc.cueList.fileDropPlan([wav], item: wc.cueList.node(Self.empty), index: on), .insert([wav], nil, 4))

        // Choose on the cue with no file: the chooser asks for pictures, and the cue follows the file.
        var asked: [NewCueKind] = []
        wc.pickFile = { kind in
            asked.append(kind)
            return movie
        }
        click(Self.empty)
        await soon { self.wc.inspector.chooseButton.isEnabled }
        wc.inspector.chooseButton.performClick(nil)
        await soon("retargeted") { self.cell(Self.empty, "name") == "clip.mov" }
        XCTAssertEqual(asked, [.video])
        await soon("its length") { self.duration(Self.empty) == 4 }
        await soon("no longer broken") { self.status(Self.empty) == nil }

        // A sound chosen for a Video cue is refused with a reason, and nothing changes.
        var notices: [String] = []
        wc.editor.notify = { notices.append($0) }
        await wc.editor.replaceFile(of: Self.still, with: wav)
        XCTAssertEqual(notices, [loc("edit.error.notAPicture")])
        XCTAssertEqual(store.model?.cue(Self.still)?.raw["kind"]?["media"]?.stringValue, "media/still.png")
    }

    /// A still's duration is typed in the list's Duration cell, and emptied again; a movie's is not typed.
    /// Typed into the inspector's Duration field and the list's Duration cell, as the operator types:
    /// a still's duration is set, emptied back to "until stopped" (the cell blank, the still running
    /// past its old length), and 0 means "until stopped" too; a movie's Duration is not typed.
    func testAStillsDurationIsTypedAndClearedAndAMoviesIsNot() async throws {
        try await openVideoShow()
        await soon { self.store.mediaDurations[Self.still]?.picture?.type == "still" }
        click(Self.still)
        let inspector = wc.inspector
        await soon { inspector.cue?.id == Self.still && inspector.durationField.isEnabled }
        try typeInto(inspector.durationField, "3")
        await soon("the typed length") { self.duration(Self.still) == 3 }
        XCTAssertEqual(store.model?.cue(Self.still)?.raw["kind"]?["duration"]?.doubleValue, 3)
        try typeInto(inspector.durationField, "")
        await soon("emptied in the inspector: until stopped") { self.cell(Self.still, "duration") == "" }
        XCTAssertEqual(store.model?.cue(Self.still)?.raw["kind"]?["duration"], .null)
        XCTAssertEqual(inspector.messageLabel.stringValue, "", "no refusal")

        // The list's cell: set, then emptied.
        XCTAssertTrue(wc.cueList.beginCellEdit(Self.still))
        try typeInto(wc.cueList.cellEditor, "2")
        await soon("typed in the list") { self.duration(Self.still) == 2 }
        XCTAssertTrue(wc.cueList.beginCellEdit(Self.still))
        try typeInto(wc.cueList.cellEditor, "")
        await soon("emptied in the list") { self.cell(Self.still, "duration") == "" }
        XCTAssertEqual(store.model?.cue(Self.still)?.raw["kind"]?["duration"], .null)

        // 0 is "until stopped" too (a still ending as it starts would only flash).
        XCTAssertTrue(wc.cueList.beginCellEdit(Self.timedStill))
        try typeInto(wc.cueList.cellEditor, "0")
        await soon("0 clears it") { self.store.model?.cue(Self.timedStill)?.raw["kind"]?["duration"] == .null }
        XCTAssertEqual(cell(Self.timedStill, "duration"), "")

        // Both play on past their old lengths until stopped.
        for id in [Self.still, Self.timedStill] { try await perform(.startSelected, on: id) }
        try await play(5)
        XCTAssertEqual(status(Self.still), loc("cue.state.running"), "no duration: runs until stopped")
        XCTAssertEqual(status(Self.timedStill), loc("cue.state.running"), "0: runs until stopped")

        XCTAssertFalse(wc.cueList.beginCellEdit(Self.movie), "a movie's length comes from its file")
    }

    /// D with two stills selected puts the caret in the inspector's one Duration field and what is
    /// typed goes to both; with a movie among them D opens nothing (a movie's length is its file's).
    func testDWithSeveralStillsSelectedTypesTheirDurationAndNotWithAMovie() async throws {
        try await openVideoShow()
        await soon {
            [Self.still, Self.timedStill].allSatisfy { self.store.mediaDurations[$0]?.picture?.type == "still" }
        }
        let window = try XCTUnwrap(wc.window)
        let inspector = wc.inspector
        wc.cueList.selectAsUser([Self.still, Self.timedStill])
        await soon { inspector.isBatch }
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        XCTAssertTrue(wc.cueList.beginEditFromKey("duration"))
        XCTAssertTrue(inspector.durationField.currentEditor() != nil, "the Duration field has the caret")
        try typeInto(inspector.durationField, "4")
        await soon("both take it") {
            [Self.still, Self.timedStill].allSatisfy {
                self.store.model?.cue($0)?.raw["kind"]?["duration"]?.doubleValue == 4
            }
        }
        XCTAssertEqual(duration(Self.still), 4)
        XCTAssertEqual(duration(Self.timedStill), 4)

        wc.cueList.selectAsUser([Self.still, Self.movie])
        await soon { inspector.isBatch && !inspector.durationField.isEnabled }
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        XCTAssertTrue(wc.cueList.beginEditFromKey("duration"), "the key is taken")
        XCTAssertTrue(window.firstResponder === wc.cueList.outlineView, "and opens nothing")
    }

    /// Types `text` into `view` through the window's field editor, as the keyboard does: the text
    /// there is selected and replaced (emptied for ""), then Return.
    func typeInto(_ view: NSView, _ text: String) throws {
        let window = try XCTUnwrap(wc.window)
        if (view as? NSControl)?.currentEditor() == nil {
            XCTAssertTrue(window.makeFirstResponder(view), "the field takes the focus")
        }
        let editor = try XCTUnwrap(window.firstResponder as? NSTextView, "typing in a field")
        editor.selectAll(nil)
        editor.deleteBackward(nil)
        if !text.isEmpty { editor.insertText(text, replacementRange: editor.selectedRange()) }
        let enter = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
        window.sendEvent(enter)
    }

    // MARK: Flow 3: GO and the transport

    /// GO on the movie: the row counts up as the picture plays from its first frame, and the cue
    /// ends at its last frame, the row back to its length.
    func testGOPlaysTheMovieTheRowCountsUpAndTheCueEndsAtItsLastFrame() async throws {
        try await openVideoShow()
        click(Self.movie)
        try await go()
        await soon("running") { self.status(Self.movie) == loc("cue.state.running") }
        let first = await shownFrame()
        XCTAssertLessThanOrEqual(first ?? 99, 1, "from its first frame")
        try await play(1)
        expectRowTime(Self.movie, 1, "the row counts up")
        let second = await shownFrame()
        let frame = try XCTUnwrap(second)
        XCTAssertEqual(Double(frame), 25, accuracy: 3, "the picture is a second in")
        try await play(2.9)
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "still playing before its end")
        try await play(0.2)
        await soon("ended at its last frame") { self.status(Self.movie) == nil }
        XCTAssertEqual(duration(Self.movie), 4, "the row reads its length again")
        let after = await shownFrame()
        XCTAssertNil(after, "the stage is black again")
    }

    /// A still shows until stopped; a still with a typed duration ends after it.
    func testAStillShowsUntilStoppedAndOneWithADurationEndsAfterIt() async throws {
        try await openVideoShow()
        await soon { self.store.mediaDurations[Self.still]?.picture?.type == "still" }
        click(Self.still)
        try await go()
        try await play(10)
        XCTAssertEqual(status(Self.still), loc("cue.state.running"), "a still runs until stopped")
        let shown = await shownFrame()
        XCTAssertEqual(shown, 0, "showing the still")
        expectRowTime(Self.still, 10, "the row counts up")
        try await perform(.stopSelected, on: Self.still)
        await soon("stopped") { self.status(Self.still) == nil }

        XCTAssertEqual(duration(Self.timedStill), 2, "the typed length")
        click(Self.timedStill)
        try await go()
        try await play(1.9)
        XCTAssertEqual(status(Self.timedStill), loc("cue.state.running"))
        try await play(0.2)
        await soon("ended after its duration") { self.status(Self.timedStill) == nil }
    }

    /// Pause holds the frame and the row's time; resume plays on; load and load to time ready the
    /// cue (the row shows it loaded); GO from a time starts the picture there; panic stops it.
    func testStopPauseResumePanicLoadAndLoadToTimeActOnAVideoCueAsOnAnAudioCue() async throws {
        try await openVideoShow()
        click(Self.movie)
        try await go()
        try await play(1)
        // A running cue's file is not replaced, in words for any file cue.
        var notices: [String] = []
        wc.editor.notify = { notices.append($0) }
        await wc.editor.replaceFile(of: Self.movie, with: folder.appendingPathComponent("media/still.png"))
        XCTAssertEqual(notices, [loc("edit.error.replaceBusy")])
        XCTAssertFalse(notices.joined().lowercased().contains("audio"), "\(notices)")
        try await perform(.pauseResumeSelected, on: Self.movie)
        await soon("paused") { self.status(Self.movie) == loc("cue.state.paused") }
        expectRowTime(Self.movie, 1, "the row holds where it paused")
        let frame = await shownFrame()
        try await play(1)
        expectRowTime(Self.movie, 1, "and still holds a second later")
        let heldFrame = await shownFrame()
        XCTAssertEqual(heldFrame, frame, "the picture holds its frame")
        try await perform(.pauseResumeSelected, on: Self.movie)
        await soon("resumed") { self.status(Self.movie) == loc("cue.state.running") }
        try await play(0.5)
        expectRowTime(Self.movie, 1.5, "it counts on from where it paused")
        try await perform(.stopSelected, on: Self.movie)
        await soon("stopped") { self.status(Self.movie) == nil }

        try await perform(.loadSelected, on: Self.movie)
        await soon("loaded") { self.status(Self.movie) == loc("cue.state.loaded") }
        XCTAssertNil(wc.applyLoadTime("2", to: Self.movie))
        await sync()
        try await play(0)
        await soon("loaded to 2 s") { self.status(Self.movie) == loc("cue.state.loaded") }
        try await perform(.startSelected, on: Self.movie)
        await soon { self.status(Self.movie) == loc("cue.state.running") }
        try await play(0.5)
        expectRowTime(Self.movie, 2.5, "it started from 2 s")
        let probed = await shownFrame()
        let there = try XCTUnwrap(probed)
        XCTAssertEqual(Double(there), 62, accuracy: 3, "the picture too")

        wc.onPanic?()
        await soon("panic stopped it") {
            try? await self.play(0.5)
            return self.status(Self.movie) == nil
        }
        await soon("and the picture went") { await self.shownFrame() == nil }
    }

    // MARK: Flow 4: the I/O tab

    /// The I/O tab of a Video cue: its file, the format the probe read, the stage and its size, and
    /// the audio patch off with its reason; a still shows its format; stage none breaks the cue,
    /// which says why and does not play.
    func testTheIOTabShowsTheFileFormatStageAndAudioPatchAndStageNoneBreaksTheCue() async throws {
        try await openVideoShow()
        click(Self.movie)
        let inspector = wc.inspector
        await soon("the inspector shows the movie") { inspector.cue?.id == Self.movie }
        let io = try XCTUnwrap(inspector.tabView.tabViewItems.first { $0.identifier as? String == "io" })
        inspector.tabView.selectTabViewItem(io)
        let v = inspector.videoIO
        XCTAssertEqual(
            v.fileLabel.stringValue, folder.appendingPathComponent("media/clip.mov").standardizedFileURL.path)
        await soon("the format") {
            v.formatLabel.stringValue
                == String(format: loc("inspector.io.videoFormat"), loc("inspector.io.codec.h264"), "288 × 160", "25")
        }
        XCTAssertEqual(v.stagePopup.titleOfSelectedItem, "Stage 1")
        XCTAssertEqual(v.stageSizeLabel.stringValue, "1920 × 1080")
        XCTAssertEqual(v.stagePopup.itemTitles, ["Stage 1", "Side screen", "", loc("inspector.io.stageNone")])
        XCTAssertFalse(inspector.outputPatchPopup.isEnabled, "no sound from video files yet")
        XCTAssertFalse(inspector.editPatchButton.isEnabled)
        XCTAssertFalse(v.audioNote.isHidden)
        XCTAssertEqual(v.audioNote.stringValue, loc("inspector.io.noVideoSound"))
        XCTAssertTrue(v.stagePopup.isEnabled)

        // The side screen: its size follows.
        v.stagePopup.selectItem(at: 1)
        XCTAssertTrue(v.stagePopup.sendAction(v.stagePopup.action, to: v.stagePopup.target))
        await soon("on the side screen") {
            self.store.model?.cue(Self.movie)?.raw["kind"]?["stage"]?.stringValue == Self.side.raw
        }
        await soon { v.stageSizeLabel.stringValue == "1280 × 720" }

        // None: broken with that reason, and GO does not play it.
        v.stagePopup.selectItem(at: v.stagePopup.numberOfItems - 1)
        XCTAssertTrue(v.stagePopup.sendAction(v.stagePopup.action, to: v.stagePopup.target))
        await soon("broken: no stage") { self.brokenTip(Self.movie)?.contains(loc("problem.no_stage")) == true }
        XCTAssertEqual(v.stagePopup.titleOfSelectedItem, loc("inspector.io.stageNone"))
        XCTAssertEqual(v.stageSizeLabel.stringValue, "")
        try await perform(.startSelected, on: Self.movie)
        try await play(0.5)
        XCTAssertNotEqual(status(Self.movie), loc("cue.state.running"), "a cue on no stage does not play")
        let side = await shownFrame(Self.side)
        XCTAssertNil(side)

        // A still's format; an Audio cue's I/O tab has the patch alone, on.
        click(Self.still)
        await soon("the still's format") {
            v.formatLabel.stringValue
                == String(format: loc("inspector.io.stillFormat"), loc("inspector.io.still.png"), "288 × 160")
        }
        let audio = await wc.editor.add(.audio)
        let audioID = try XCTUnwrap(audio)
        click(audioID)
        await soon { inspector.cue?.id == audioID }
        XCTAssertTrue(inspector.outputPatchPopup.isEnabled)
        XCTAssertTrue(v.videoRows.allSatisfy(\.isHidden))
        XCTAssertEqual(v.noteRow?.isHidden, true)
    }

    // MARK: Flow 5: missing file, relink, Show Mode

    /// A movie that goes missing shows the missing-file mark and reason; Relink finds it in a folder
    /// and the cue plays again.
    func testAMissingVideoFileShowsTheMarkAndReasonAndRelinkFindsIt() async throws {
        try await openVideoShow()
        let file = folder.appendingPathComponent("media/clip.mov")
        let moved = engine.folder.appendingPathComponent("found-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: moved, withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: file, to: moved.appendingPathComponent("clip.mov"))
        await soon("missing") {
            try? await self.play(0.5)
            return self.brokenTip(Self.movie)?.contains("clip.mov") == true
                && (self.store.problems[Self.movie] ?? []).contains { $0.type == "media_missing" }
        }
        XCTAssertTrue(missingMark(Self.movie), "the Target column's missing mark")
        app.pickFolderURL = { moved }
        await app.relinkMedia(on: store)
        await soon("relinked") {
            self.store.model?.cue(Self.movie)?.raw["kind"]?["media"]?.stringValue?.hasSuffix("clip.mov") == true
                && self.status(Self.movie) == nil
        }
        XCTAssertFalse(missingMark(Self.movie))
        click(Self.movie)
        try await go()
        await soon { self.status(Self.movie) == loc("cue.state.running") }
    }

    /// In Show Mode GO plays a Video cue; a movie that cannot be decoded part way breaks only its
    /// own cue, with the reason in its row and a warning in the footer, while the other plays on.
    func testInShowModeGOPlaysVideoAndADecodeFailureBreaksOnlyThatCueWithAFooterNotice() async throws {
        try await openVideoShow { folder in
            // ProRes: each frame stands alone, so cutting the file removes the frames after the cut
            // while the index still promises them: it probes as playable and fails as it plays.
            let doomed = folder.appendingPathComponent("media/\(Self.longName)")
            try makeTestMedia(doomed, codec: "prores", frames: 250)
            let bytes = try Data(contentsOf: doomed)
            try bytes.prefix(bytes.count / 5).write(to: doomed)
            try writeTestMedia(folder.appendingPathComponent("media/clip.mov"), seconds: 10)
        }
        await soon("the damaged movie still probes") { self.store.mediaDurations[Self.long]?.picture != nil }
        wc.setShowMode(true)
        await soon { self.store.showMode }
        click(Self.movie)
        try await go()
        await soon("GO plays in Show Mode") { self.status(Self.movie) == loc("cue.state.running") }
        try await perform(.startSelected, on: Self.long)
        var broke = false
        for _ in 0..<20 where !broke {
            try await play(0.25)
            _ = await shownFrame(Self.side)  // a failure is reported after a draw
            try await play(0)
            broke = status(Self.long) == loc("cue.state.broken")
        }
        XCTAssertTrue(broke, "the damaged movie breaks")
        XCTAssertTrue(brokenTip(Self.long)?.contains(loc("broken.decode_failed")) == true)
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "the other cue plays on")
        await soon("the footer counts it") { !self.wc.footer.warningsButton.isHidden }
        let rows = WarningRows.make(store: store)
        XCTAssertTrue(rows.contains { $0.cue == Self.long && $0.detail.contains(loc("broken.decode_failed")) })
        XCTAssertNil(wc.window?.attachedSheet, "Show Mode raises no sheet")
    }

    /// A Video cue's Triggers tab has the fade-and-stop-others controls, and they act: the movie
    /// plays, the still is set to fade and stop the others over 0.5 s, and GO on the still stops
    /// the movie (its picture gone) while the still runs on.
    func testAVideoCuesTriggersTabFadesAndStopsTheOthers() async throws {
        try await openVideoShow()
        let inspector = wc.inspector
        click(Self.still)
        await soon { inspector.cue?.id == Self.still }
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        XCTAssertNotNil(inspector.fadeOthersCheck.superview, "on the tab")
        XCTAssertTrue(inspector.fadeOthersCheck.isEnabled)
        inspector.fadeOthersCheck.performClick(nil)
        await soon { InspectorEdit.fadeOthers(self.store.model!.cue(Self.still)!).enabled }
        try typeInto(inspector.fadeOthersTimeField, "0.5")
        await soon { InspectorEdit.fadeOthers(self.store.model!.cue(Self.still)!).duration == 0.5 }
        XCTAssertTrue(inspector.duckCheck.isEnabled, "duck others is offered too")

        try await perform(.startSelected, on: Self.movie)
        try await play(0.5)
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"))
        try await perform(.startSelected, on: Self.still)
        try await play(0.7)
        await soon("the movie faded and stopped") { self.status(Self.movie) == nil }
        XCTAssertEqual(status(Self.still), loc("cue.state.running"), "never the cue itself")
        let shown = await shownFrame()
        XCTAssertEqual(shown, 0, "only the still is on the stage")
    }

    /// Paste Properties from a still on the side screen with a 2 s duration onto a still and a movie
    /// on Stage 1: the I/O category offers the stage, Basics the duration; both go to the side
    /// screen, the still takes 2 s, the movie keeps its file's length; one undo puts both back. A
    /// still with no duration pasted onto one with a duration makes it "until stopped", never 0.
    func testPastePropertiesCopiesAVideoCuesStageAndAStillsDuration() async throws {
        try await openVideoShow()
        await soon {
            [Self.still, Self.timedStill].allSatisfy { self.store.mediaDurations[$0]?.picture?.type == "still" }
        }
        wc.presentSheet = { _ in }
        wc.pasteDefaults = UserDefaults(suiteName: "openlab-test-\(UUID().uuidString)") ?? .standard
        let board = NSPasteboard.withUniqueName()
        wc.editor.pasteboard = board
        guard case .success = await store.send(command: .setProperty(Self.timedStill, .stage(Self.side))) else {
            return XCTFail("stage")
        }
        await soon { self.store.model?.cue(Self.timedStill)?.raw["kind"]?["stage"]?.stringValue == Self.side.raw }
        let copied = await wc.editor.copySelection(of: [Self.timedStill])
        XCTAssertTrue(copied)

        func paste(onto cues: [ModelID], _ properties: [PasteProperty]) throws {
            wc.promptPasteProperties(onto: cues)
            let sheet = try XCTUnwrap(wc.pasteSheet, "the sheet is up")
            for p in PasteProperty.allCases {
                guard let box = sheet.propertyChecks[p] else { continue }
                if (box.state == .on) != properties.contains(p) { box.performClick(nil) }
            }
            sheet.applyButton.performClick(nil)
        }
        wc.cueList.selectAsUser([Self.still, Self.movie])
        wc.promptPasteProperties(onto: [Self.still, Self.movie])
        let offered = try XCTUnwrap(wc.pasteSheet)
        XCTAssertNotNil(offered.propertyChecks[.stage], "the I/O category offers the stage")
        XCTAssertNotNil(offered.propertyChecks[.duration], "a still's duration is offered")
        XCTAssertNil(offered.propertyChecks[.outputPatch], "no audio patch for Video cues")
        offered.cancelButton.performClick(nil)
        try paste(onto: [Self.still, Self.movie], [.stage, .duration])
        await soon("both on the side screen") {
            [Self.still, Self.movie].allSatisfy {
                self.store.model?.cue($0)?.raw["kind"]?["stage"]?.stringValue == Self.side.raw
            }
        }
        await soon("the still takes 2 s") { self.duration(Self.still) == 2 }
        XCTAssertEqual(duration(Self.movie), 4, "the movie keeps its file's length")
        XCTAssertEqual(store.model?.cue(Self.movie)?.raw["kind"]?["duration"], .null)
        click(Self.still)
        let io = wc.inspector.videoIO
        await soon {
            io.stagePopup.titleOfSelectedItem == "Side screen" && io.stageSizeLabel.stringValue == "1280 × 720"
        }

        await wc.editor.undo()
        await soon("one undo puts both back") {
            [Self.still, Self.movie].allSatisfy {
                self.store.model?.cue($0)?.raw["kind"]?["stage"]?.stringValue == Self.stage.raw
            } && self.cell(Self.still, "duration") == ""
        }

        // "Until stopped" pastes as itself.
        let again = await wc.editor.copySelection(of: [Self.still])
        XCTAssertTrue(again)
        try paste(onto: [Self.timedStill], [.duration])
        await soon("no duration, not 0") { self.store.model?.cue(Self.timedStill)?.raw["kind"]?["duration"] == .null }
        XCTAssertEqual(cell(Self.timedStill, "duration"), "")
    }

    // MARK: Flow 6: tabs and icon

    /// A Video cue's inspector has Basics, Triggers and I/O; several selected keep I/O, the stage
    /// they share shown and set on all; the row draws the Video drawing.
    func testAVideoCueShowsBasicsTriggersAndIOTabsAndItsKindIconInTheList() async throws {
        try await openVideoShow()
        click(Self.movie)
        let inspector = wc.inspector
        await soon { inspector.cue?.id == Self.movie }
        XCTAssertEqual(
            inspector.tabView.tabViewItems.compactMap { $0.identifier as? String }, ["basics", "triggers", "io"])
        XCTAssertEqual(Icon.drawing(shownBy: inspector.kindIcon.image), .kindVideo)
        for id in [Self.movie, Self.still, Self.empty, Self.long] {
            XCTAssertEqual(Icon.drawing(shownBy: cueCell(id, "type")?.images[0].image), .kindVideo, "\(id)")
        }
        XCTAssertTrue(inspector.durationField.isEnabled == false, "a movie's length is its file's")

        wc.cueList.selectAsUser([Self.movie, Self.still])
        await soon { inspector.isBatch }
        XCTAssertEqual(
            inspector.tabView.tabViewItems.compactMap { $0.identifier as? String }, ["basics", "triggers", "io"])
        let v = inspector.videoIO
        XCTAssertEqual(v.stagePopup.titleOfSelectedItem, "Stage 1", "the stage both share")
        XCTAssertEqual(v.fileLabel.stringValue, loc("inspector.mixed"))
        v.stagePopup.selectItem(at: 1)
        XCTAssertTrue(v.stagePopup.sendAction(v.stagePopup.action, to: v.stagePopup.target))
        await soon("both on the side screen") {
            [Self.movie, Self.still].allSatisfy {
                self.store.model?.cue($0)?.raw["kind"]?["stage"]?.stringValue == Self.side.raw
            }
        }
        wc.cueList.selectAsUser([Self.movie, Self.timedStill])
        await soon("stages that differ read as mixed") { v.stagePopup.titleOfSelectedItem == loc("inspector.mixed") }
    }
}
