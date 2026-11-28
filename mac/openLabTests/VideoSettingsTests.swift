// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// VID-01, VID-12 (#642): the Video page of the workspace settings against the real engine and
/// its real stage host, drawn offscreen on two virtual displays (V1 1920 x 1080, V2 1280 x 720), so
/// no window opens on this Mac's displays. Each test has an engine of its own: the display choices
/// are the engine's machine settings.
@MainActor
final class VideoSettingsTests: XCTestCase {
    static let displays = "V1:1920x1080,V2:1280x720"
    var engine: TestEngine!

    override func setUp() async throws {
        engine = try await TestEngine.start(displays: Self.displays)
    }

    override func tearDown() async throws { engine?.stop() }

    /// The Demo show with `stages` added (each `[id, name, width, height]`), its settings window
    /// open with the stage lists read.
    private func open(stages: [(String, String, Int, Int)] = []) async throws
        -> (Rig, WorkspaceSettingsWindowController)
    {
        let rig = try await openShow(stages: stages, on: engine)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        return (rig, sc)
    }

    /// The Demo show with `stages` added, opened on `engine` (no settings window).
    private func openShow(stages: [(String, String, Int, Int)], on engine: TestEngine) async throws -> Rig {
        try await Rig.open("Demo", on: engine) { folder in
            guard !stages.isEmpty else { return }
            let file = folder.appendingPathComponent("Demo.openlab")
            var root = try XCTUnwrap(
                JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
            var ws = try XCTUnwrap(root["workspace"] as? [String: Any])
            // Each stage has one region covering it, on an output route of its own id and size.
            ws["routes"] = stages.enumerated().map { n, s in
                ["id": s.0, "name": "Output \(n + 1)", "width": s.2, "height": s.3] as [String: Any]
            }
            ws["stages"] = stages.map { s in
                [
                    "id": s.0, "name": s.1, "width": s.2, "height": s.3, "keep_black": false, "layer": 1,
                    "regions": [["id": s.0, "route": s.0, "x": 0, "y": 0, "width": s.2, "height": s.3]],
                ] as [String: Any]
            }
            root["workspace"] = ws
            try JSONSerialization.data(withJSONObject: root).write(to: file)
        }
    }

    /// Waits until the page shows what the engine holds (the lists are read and pushes applied).
    private func settled(_ sc: WorkspaceSettingsWindowController, _ ok: @escaping @MainActor () -> Bool) async {
        await assertEventually("the Video page caught up") {
            await sc.videoPane.pending?.value
            await sc.videoPane.refresh()
            return ok()
        }
    }

    private let stageA = "00000000-0000-0000-0000-00000000a001"
    private let stageB = "00000000-0000-0000-0000-00000000a002"

    /// The Video category is split into Video Outputs (the stages), Output Routing (the display
    /// each is shown on) and Output Devices (the displays), as QLab 5's are, with no Video Inputs;
    /// the page bar lists the three and the stages page has no display choice.
    func testTheVideoCategoryHasStagesRoutingAndDevicePages() async throws {
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let video = try XCTUnwrap(SettingsCategory.allCases.firstIndex(of: .video))
        sc.categoryTable.selectRowIndexes(IndexSet(integer: video), byExtendingSelection: false)
        XCTAssertEqual(sc.shownPage, .video, "a category shows its first page")
        XCTAssertFalse(sc.pageBar.isHidden)
        XCTAssertEqual(
            (0..<sc.pageBar.segmentCount).compactMap { sc.pageBar.label(forSegment: $0) },
            ["Video Outputs", "Output Routing", "Output Devices"])
        XCTAssertEqual(SettingsCategory.video.pages, [.video, .videoRouting, .videoDevices])
        await settled(sc) { sc.videoPane.nameFields.count == 1 }
        XCTAssertFalse(sc.videoPane.cols.contains(.display), "the stages page chooses no display")
        XCTAssertEqual(sc.videoRoutingPane.cols, [.handle, .name, .size, .display, .layout, .guides, .edit, .remove])
        XCTAssertEqual(sc.videoPane.cols, [.handle, .name, .size, .devices, .edit, .monitor, .remove])
        XCTAssertEqual(sc.videoDevicesPane.cols, [.device, .name, .resolution, .rate])
        sc.pageBar.selectedSegment = 1
        _ = sc.pageBar.target?.perform(sc.pageBar.action, with: sc.pageBar)
        XCTAssertEqual(sc.shownPage, .videoRouting)
    }

    /// The Output Devices page is a table of the Mac's displays, one row each: "Device N:", the
    /// display's name, its size in pixels and its refresh rate, with no stage column.
    func testTheDevicesPageListsEachDisplayWithItsNameResolutionAndRate() async throws {
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let devices = sc.videoDevicesPane
        await settled(sc) { devices.deviceLabels.count == 2 }
        XCTAssertEqual(
            devices.deviceLabels.map { $0.map(\.stringValue) },
            [
                ["Device 1:", "Virtual V1", "1920 × 1080", "60 fps"],
                ["Device 2:", "Virtual V2", "1280 × 720", "60 fps"],
            ])
        XCTAssertTrue(devices.emptyLabel.isHidden)
        XCTAssertEqual(
            devices.cols.map { loc("settings.video.col.\($0.key)") }, ["", "Name", "Resolution", "Refresh rate"])
    }

    /// A display's refresh rate reads as the system gave it (120 and 240 on a laptop and a gaming
    /// monitor), and nothing where the stage host did not know it.
    func testDeviceRowsShowTheRefreshRateTheDisplayReports() {
        let rows = [
            StageDisplay(id: "A", name: "Built-in Retina Display", width: 3024, height: 1964, fps: 120),
            StageDisplay(id: "B", name: "GO27Q24", width: 2560, height: 1440, fps: 240),
            StageDisplay(id: "C", name: "Old", width: 1024, height: 768),
        ]
        XCTAssertEqual(
            rows.enumerated().map { VideoSettingsPane.deviceTexts(number: $0.offset + 1, display: $0.element) },
            [
                ["Device 1:", "Built-in Retina Display", "3024 × 1964", "120 fps"],
                ["Device 2:", "GO27Q24", "2560 × 1440", "240 fps"],
                ["Device 3:", "Old", "1024 × 768", ""],
            ])
    }

    /// The monitor button says what it does: its title and tooltip name a monitor window and a live
    /// copy of the stage, not a bare "Watch".
    func testTheMonitorButtonSaysItOpensALiveCopyOfTheStage() async throws {
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.monitorButtons.count == 1 }
        let button = pane.monitorButtons[0]
        XCTAssertEqual(button.title, "Monitor")
        XCTAssertTrue(try XCTUnwrap(button.toolTip).contains("live copy"), "\(button.toolTip ?? "")")
        XCTAssertTrue(try XCTUnwrap(button.accessibilityLabel()).contains("monitor window"))
    }

    /// Flow 2: a stage's display pop-up changed: the stage goes to that display at once (its state
    /// says shown on it), and the choice is kept for this machine.
    func testChangingAStagesDisplayPopUpMovesItToThatDisplay() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.displayPopups.count == 1 }
        let popup = routing.displayPopups[0]
        XCTAssertEqual(popup.titleOfSelectedItem, loc("settings.video.displayNone"))
        popup.selectItem(withTitle: "Virtual V2 (1280 × 720)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { routing.shown?.first?.display == "V2" }
        XCTAssertEqual(routing.displayPopups.first?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        XCTAssertEqual(rig.store.videoOutputs.routes.first?.mapping?.display, "V2")
    }

    /// Flow 3: New Stage adds a stage (with no output route in the workspace, it reaches no
    /// display); its name is typed in place; it is removed; each is one undo step.
    func testNewStageRenameInPlaceAndRemoveAreOneUndoStepEach() async throws {
        let (rig, sc) = try await open()
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.shown?.isEmpty == true }
        XCTAssertFalse(pane.emptyLabel.isHidden, "an empty table says so")
        choose(loc("settings.video.addEmpty"), in: pane.addPopup)
        await settled(sc) { pane.nameFields.count == 1 }
        XCTAssertEqual(pane.nameFields[0].stringValue, String(format: loc("settings.video.newName"), 1))
        XCTAssertEqual(routing.shown, [], "no output route")
        XCTAssertEqual(routing.emptyLabel.isHidden, false)
        let field = pane.nameFields[0]
        field.stringValue = "Cyc"
        pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
        await settled(sc) { pane.shown?.first?.name == "Cyc" }
        pane.removeButtons[0].performClick(nil)
        await settled(sc) { pane.shown?.isEmpty == true }
        // Undo: back, renamed; then the name as it was; then no stage.
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.first?.name == "Cyc" }
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.first?.name == String(format: loc("settings.video.newName"), 1) }
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.isEmpty == true }
    }

    /// Flow 4: the stage editor's keep-black box sets the stage's keep-black; its black comes only
    /// with the first picture played on it (#708; black between cues itself: the engine's
    /// video_cues.rs).
    func testTheKeepBlackBoxSetsTheStagesBlackWhichComesOnlyWithItsFirstPicture() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let editor = sc.openStageEditor(ModelID(stageA))
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.displayPopups.count == 1 }
        let popup = routing.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { rig.store.videoOutputs.routes.first?.state?.state == "hidden" }
        editor.blackCheck.performClick(nil)
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(true) }
        await editor.editing?.value
        XCTAssertEqual(editor.blackCheck.state, .on)
        // #708: no picture has played on the stage since the show opened, so its black is not up yet:
        // ticking the box never covers a display.
        await settled(sc) { rig.store.videoOutputs.stages.first?.panicked == true }
        XCTAssertEqual(rig.store.videoOutputs.routes.first?.state?.state, "hidden")
        editor.blackCheck.performClick(nil)
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(false) }
        XCTAssertEqual(rig.store.videoOutputs.routes.first?.state?.state, "hidden")
    }

    /// What `stage`'s monitor window shows at a content size of `width` x `height` pixels, read back
    /// from the stage host: the test card's frame and the colours at `points` (none: no window).
    private func monitorShows(
        _ rig: Rig, _ stage: String, _ width: UInt32, _ height: UInt32, _ points: [[Double]]
    ) async -> (UInt16?, [[UInt8]])? {
        guard
            case .success(let ok) = await rig.store.client.send({
                .request(
                    id: $0, .monitorProbe(stage: ModelID(stage), width: width, height: height, points: points))
            }),
            case .stageProbe(let frame, let colours)? = ok.data
        else { return nil }
        return (frame, colours)
    }

    /// #648 flow 1 (the settings page's button): the watch button beside a stage → a window shows
    /// that stage scaled to fit, black bars where the shapes differ. The window is the stage host's
    /// (drawn offscreen here on virtual displays), read back as the operator would see it: the
    /// stage's test card in a square window, black above and below it. The page itself is
    /// unchanged and says nothing.
    func testTheMonitorButtonBesideAStageInSettingsOpensAWindowShowingTheStageScaledToFitWithBlackBars()
        async throws
    {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.monitorButtons.count == 1 }
        let button = pane.monitorButtons[0]
        XCTAssertEqual(button.title, loc("settings.video.monitor"))
        XCTAssertTrue(button.isEnabled)
        _ = await rig.store.client.send { .request(id: $0, .stageTestCard(stage: ModelID(self.stageA), frame: 77)) }
        let closed = await monitorShows(rig, stageA, 400, 400, [[0.5, 0.5]])
        XCTAssertEqual(closed?.1, [], "no window before the click")
        button.performClick(nil)
        await settled(sc) { rig.store.videoOutputs.stages.first?.monitor == true }
        // A square window: the 16:9 stage across its width, its four quadrants inside, bars above
        // and below.
        let inside = [[0.25, 0.21875 + 0.3 * 0.5625], [0.75, 0.21875 + 0.8 * 0.5625]]
        let shown = await monitorShows(rig, stageA, 400, 400, inside + [[0.5, 0.1], [0.5, 0.9]])
        XCTAssertEqual(shown?.0, 77, "the stage's picture")
        XCTAssertEqual(shown?.1, [[255, 0, 0, 255], [255, 255, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255]])
        XCTAssertEqual(pane.messageLabel.stringValue, "")
        // Clicked again on an open window: brought to the front, still open.
        button.performClick(nil)
        await settled(sc) { pane.pending == nil || rig.store.videoOutputs.stages.first?.monitor == true }
        XCTAssertEqual(rig.store.videoOutputs.stages.first?.monitor, true)
        XCTAssertEqual(pane.messageLabel.stringValue, "")
    }

    /// Flow 7: a show whose stage has no display on this Mac loads; the stage carries a warning in
    /// its row and in the workspace's Warnings list; choosing a display once clears both.
    func testAStageWithoutADisplayWarnsInItsRowAndTheWarningsUntilOneIsChosen() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.warningLabels.first?.stringValue == loc("settings.video.warn.noDisplay") }
        let warnings = WarningRows.make(store: rig.store)
        XCTAssertTrue(
            warnings.contains {
                $0.title == String(format: loc("warnings.stage"), "Output 1")
                    && $0.detail == loc("warnings.stage.noDisplay")
            }, "\(warnings)")
        let popup = routing.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { routing.shown?.first?.warning == nil }
        XCTAssertFalse(WarningRows.make(store: rig.store).contains { $0.id == "v:\(self.stageA)" })
    }

    /// The table redrawn through every row shape (plain, with a warning line, added, removed,
    /// reordered), twice over: no AppKit exception (a grid row with a merged warning cell cannot
    /// be removed), and the table holds exactly the controls of the rows shown.
    func testTheTableRedrawsThroughEveryRowShapeWithoutLeftovers() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080), (stageB, "Angelica", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        func check(_ why: String) {
            let rows = pane.shown ?? []
            let routes = routing.shown ?? []
            let warned = routes.filter { $0.warning != nil }.count
            let stagesWarned = rows.filter { $0.warning != nil }.count
            XCTAssertEqual(pane.holder.subviews.filter { $0 is NSGridView }, [pane.grid], "\(why): one table")
            XCTAssertEqual(pane.grid.numberOfRows, rows.count + stagesWarned, why)
            let controls: [NSView] =
                pane.handles + pane.sizeLabels + pane.deviceCells + pane.editButtons + pane.monitorButtons
                + pane.removeButtons
            // Per row its seven cells (the name with its icon is one; the titles are not in the
            // rows' grid), and a line per warning.
            XCTAssertEqual(pane.grid.subviews.count, 7 * rows.count + stagesWarned, why)
            XCTAssertTrue(controls.allSatisfy { $0.superview === pane.grid }, why)
            XCTAssertEqual(pane.nameFields.map(\.stringValue), rows.map(\.name), why)
            // The routing page: its row's eight cells, and a line for each warning.
            XCTAssertEqual(routing.holder.subviews.filter { $0 is NSGridView }, [routing.grid], "\(why): one table")
            XCTAssertEqual(routing.grid.numberOfRows, routes.count + warned, why)
            XCTAssertEqual(routing.grid.subviews.count, 8 * routes.count + warned, why)
            XCTAssertTrue(routing.displayPopups.allSatisfy { $0.superview === routing.grid }, why)
        }
        for round in 1...2 {
            await settled(sc) { pane.shown?.count == 2 && routing.warningLabels.allSatisfy { !$0.stringValue.isEmpty } }
            check("round \(round): two stages, each with a warning")
            let popup = routing.displayPopups[0]
            popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
            _ = popup.sendAction(popup.action, to: popup.target)
            await settled(sc) { routing.shown?.first?.warning == nil }
            check("round \(round): a warning line gone")
            choose(loc("settings.video.addEmpty"), in: pane.addPopup)
            await settled(sc) { pane.shown?.count == 3 }
            check("round \(round): a stage added")
            _ = await rig.store.send(
                command: Command(
                    .object(["type": .string("move_stage"), "id": .string(stageA), "index": .int(2)])))
            await settled(sc) { pane.shown?.last?.id.raw == self.stageA }
            check("round \(round): reordered")
            pane.removeButtons[1].performClick(nil)
            await settled(sc) { pane.shown?.count == 2 }
            check("round \(round): a stage removed")
            // Back to the start: the stage removed and the added one undone, the display unchosen.
            _ = await rig.store.undo()
            _ = await rig.store.undo()
            _ = await rig.store.undo()
            _ = await rig.store.setRouteDisplay(ModelID(stageA), display: nil)
            await sc.videoPane.refresh()
        }
    }

    /// A push that comes while a stage's name is typed in is drawn at once, and the name being
    /// typed stays as typed, caret and all, uncommitted; ending the typing commits it.
    func testWhatChangesWhileANameIsTypedIsDrawnAtOnceAndTheTypingGoesOn() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.warningLabels.first?.stringValue == loc("settings.video.warn.noDisplay") }
        try startTyping("Cyclo", in: pane.nameFields[0], sc)
        let caret = NSRange(location: 2, length: 2)
        pane.nameFields[0].currentEditor()?.selectedRange = caret
        guard case .success = await rig.store.setRouteDisplay(ModelID(stageA), display: "V1") else {
            return XCTFail("mapping")
        }
        await caughtUp(rig, sc, "drawn while the name is typed") { routing.warningLabels.first?.stringValue == "" }
        let field = pane.nameFields[0]
        XCTAssertEqual(field.currentEditor()?.string, "Cyclo", "still typing, as typed")
        XCTAssertEqual(field.currentEditor()?.selectedRange, caret, "the selection where it was")
        XCTAssertEqual(rig.store.model?.stages.first?.raw["name"]?.stringValue, "Cyc", "nothing committed")
        XCTAssertTrue(try XCTUnwrap(sc.window).makeFirstResponder(nil))
        await caughtUp(rig, sc, "the name is committed") { pane.shown?.first?.name == "Cyclo" }
    }

    /// A stage or route name typed and left there is sent when the window goes (Done), and the next
    /// opening sends nothing.
    func testANameTypedOnAVideoPageIsSentWhenTheWindowGoes() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        await settled(sc) { sc.videoPane.nameFields.count == 1 && sc.videoRoutingPane.nameFields.count == 1 }
        for (page, pane) in [(SettingsPage.video, sc.videoPane), (.videoRouting, sc.videoRoutingPane)] {
            sc.show(page: page)
            let field = try XCTUnwrap(pane.nameFields.first)
            XCTAssertTrue(try XCTUnwrap(sc.window).makeFirstResponder(field))
            field.currentEditor()?.string = "Typed"
            sc.doneButton.performClick(nil)
            await assertEventually("\(page): sent") {
                let items = page == .video ? rig.store.model?.stages : rig.store.model?.routes
                return items?.first?.raw["name"]?.stringValue == "Typed"
            }
            await pane.pending?.value
            await settle()
            XCTAssertFalse(sc.window?.isVisible == true, "\(page): closed")
            let sent = rig.wire.commands.count
            await sc.present().value
            await pane.pending?.value
            XCTAssertEqual(rig.wire.commands.count, sent, "\(page): the opening sends nothing")
        }
    }

    /// A size typed in a stage editor is committed as its window goes, not at its next opening.
    func testASizeTypedInTheStageEditorIsSentWhenItsWindowGoes() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        await settled(sc) { sc.videoPane.nameFields.count == 1 }
        let editor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(editor.window)
        window.makeFirstResponder(editor.layerField)
        editor.layerField.currentEditor()?.string = "5"
        window.performClose(nil)
        await assertEventually("sent") { rig.store.model?.stages.first?.raw["layer"]?.doubleValue == 5 }
        let sent = rig.wire.commands.count
        sc.openStageEditor(ModelID(stageA))
        await settle()
        XCTAssertEqual(rig.wire.commands.count, sent, "the opening sends nothing")
    }

    /// Starts typing `text` in `field` of the shown Video page (not committed).
    private func startTyping(_ text: String, in field: NSTextField, _ sc: WorkspaceSettingsWindowController) throws {
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        XCTAssertTrue(try XCTUnwrap(sc.window).makeFirstResponder(field))
        try XCTUnwrap(field.currentEditor()).string = text
    }

    /// Waits for the page's edits and the engine's answers, moving the stepped engine on (its
    /// pushes wait for its clock), without asking the engine again: what is drawn must come by
    /// itself.
    private func caughtUp(
        _ rig: Rig, _ sc: WorkspaceSettingsWindowController, _ why: String,
        _ ok: @escaping @MainActor () -> Bool
    ) async {
        await assertEventually(why) {
            await sc.videoPane.pending?.value
            try? await rig.advance(0.2)
            return ok()
        }
    }

    /// While a stage's name is typed in, New Stage and a remove are drawn at once: the typed name
    /// is committed first, and the rows are the workspace's, never the ones before; a stage added
    /// by another client while a name is typed is drawn at once too.
    func testAddAndRemoveWhileANameIsTypedAreDrawnAtOnce() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080), (stageB, "Angelica", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 2 }
        try startTyping("Cyc", in: pane.nameFields[0], sc)
        pane.removeButtons[1].performClick(nil)
        await caughtUp(rig, sc, "the remove is drawn") { pane.nameFields.map(\.stringValue) == ["Cyc"] }
        XCTAssertEqual(rig.store.model?.stages.map { $0.raw["name"]?.stringValue }, ["Cyc"])
        try startTyping("Cyc 2", in: pane.nameFields[0], sc)
        choose(loc("settings.video.addEmpty"), in: pane.addPopup)
        await caughtUp(rig, sc, "the add is drawn") { pane.nameFields.count == 2 }
        XCTAssertEqual(pane.nameFields.first?.stringValue, "Cyc 2")
        // Another client adds a stage while a name is typed: drawn at once, the typing committed.
        try startTyping("Cyc 3", in: pane.nameFields[0], sc)
        _ = await rig.store.send(
            command: .addStage(id: ModelID(stageB), name: "Elsewhere", width: 640, height: 480, index: 2))
        await caughtUp(rig, sc, "the other client's add is drawn") { pane.nameFields.count == 3 }
        // The new stage is "Stage N", one past the highest such number in use (none here).
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Cyc 3", "Stage 1", "Elsewhere"])
        XCTAssertEqual(pane.messageLabel.stringValue, "")
    }

    /// A layer typed out of range in the stage editor and then left by clicking the keep-black
    /// box: the box's edit is made, and the layer's reason stays shown (the box's edit does not
    /// clear it).
    func testABadLayerLeftByClickingTheBoxKeepsItsReason() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080)])
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertTrue(try XCTUnwrap(editor.window).makeFirstResponder(editor.layerField))
        try XCTUnwrap(editor.layerField.currentEditor()).string = "1500"
        editor.blackCheck.performClick(nil)
        await editor.editing?.value
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(true) }
        XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(editor.layerField.stringValue, "1")
    }

    /// A name the engine refuses, left by clicking the stage's Monitor button: the button's
    /// request is made, and the engine's reason for the name is shown when it comes (the click
    /// does not outrun it).
    func testANameTheEngineRefusesLeftByClickingMonitorShowsWhy() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 1 }
        try startTyping(String(repeating: "n", count: 1025), in: pane.nameFields[0], sc)
        pane.monitorButtons[0].performClick(nil)
        await caughtUp(rig, sc, "the monitor is opened") { rig.store.videoOutputs.stages.first?.monitor == true }
        XCTAssertFalse(pane.messageLabel.stringValue.isEmpty, "the engine's reason for the name")
        XCTAssertNotEqual(pane.messageLabel.stringValue, loc("settings.video.badName"))
        XCTAssertEqual(pane.nameFields[0].stringValue, "Mary")
        XCTAssertEqual(rig.store.model?.stages.first?.raw["name"]?.stringValue, "Mary")
    }

    /// A remove of a stage already gone says so in words, never with the stage's internal id.
    func testRemovingAStageAlreadyGoneSaysSoWithoutAnId() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.removeButtons.count == 1 }
        // Twice before the page is drawn again: the second finds the stage gone.
        let remove = pane.removeButtons[0]
        remove.performClick(nil)
        remove.performClick(nil)
        await caughtUp(rig, sc, "both answered") { pane.nameFields.isEmpty }
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.gone"))
        XCTAssertFalse(pane.messageLabel.stringValue.contains("0000"))
    }

    /// Every refused value says why in the same place and goes back: a layer out of range (also
    /// right after another edit that was accepted), a layer that is not a number, an empty name,
    /// and a name the engine refuses.
    func testEveryRefusedValueSaysWhyAndGoesBack() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080), (stageB, "Angelica", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 2 }
        let editor = sc.openStageEditor(ModelID(stageA))
        func type(_ text: String, into field: NSTextField) {
            field.stringValue = text
            let note = Notification(name: NSControl.textDidEndEditingNotification, object: field)
            if field.window === editor.window {
                editor.controlTextDidEndEditing(note)
            } else {
                pane.controlTextDidEndEditing(note)
            }
        }
        // An accepted edit still on its way does not wipe the refusal typed after it.
        type("0", into: editor.layerField)
        type("1001", into: editor.layerField)
        await editor.editing?.value
        await settled(sc) { rig.store.model?.stages.first?.raw["layer"]?.doubleValue == 0 }
        XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(editor.layerField.stringValue, loc("stageEditor.layer.bottom"))
        type("abc", into: editor.layerField)
        XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(editor.layerField.stringValue, loc("stageEditor.layer.bottom"))
        type("", into: pane.nameFields[1])
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badName"))
        XCTAssertEqual(pane.nameFields[1].stringValue, "Angelica")
        // A name over the length limit: the engine's reason, and the name as it was.
        type(String(repeating: "n", count: 1025), into: pane.nameFields[1])
        await caughtUp(rig, sc, "the engine's refusal is shown") {
            !pane.messageLabel.stringValue.isEmpty && pane.messageLabel.stringValue != loc("settings.video.badName")
        }
        XCTAssertEqual(pane.nameFields[1].stringValue, "Angelica")
        XCTAssertEqual(rig.store.model?.stages.last?.raw["name"]?.stringValue, "Angelica")
    }

    /// The column titles stay above the rows however far they scroll, lined up with them; the
    /// display column takes the room there is, so a display's whole name and "not connected"
    /// read at the window's first size, and a wide window widens the display and name columns.
    func testTheTitlesStayAndTheDisplayColumnTakesTheRoom() async throws {
        let (rig, sc) = try await open(
            stages: (1...12).map { n in
                (String(format: "00000000-0000-0000-0000-0000000a%04d", n), "Stage \(n)", 1920, 1080)
            })
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.nameFields.count == 12 }
        let window = try XCTUnwrap(sc.window)
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        XCTAssertFalse(pane.header.isDescendant(of: pane.scroll), "the titles do not scroll")
        // Scrolled to the end: the titles are still where they were.
        window.contentView?.layoutSubtreeIfNeeded()
        let titlesAt = pane.header.convert(pane.header.bounds, to: nil)
        pane.scroll.contentView.scroll(to: NSPoint(x: 0, y: pane.holder.frame.height))
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(pane.header.convert(pane.header.bounds, to: nil), titlesAt)
        // A display chosen, at the window's first size: its choice reads whole.
        let first = try XCTUnwrap(pane.shown?.first?.id)
        let chosen = await rig.store.setStageDisplay(first, display: "V1")
        guard case .success = chosen else { return XCTFail("\(chosen)") }
        await caughtUp(rig, sc, "the choice is drawn \(rig.store.videoOutputs)") {
            routing.shown?.first?.display == "V1"
        }
        window.contentView?.layoutSubtreeIfNeeded()
        for c in 0..<pane.cols.count {
            XCTAssertEqual(pane.header.column(at: c).width, pane.grid.column(at: c).width, "column \(c)")
        }
        // The routing page's display choice reads whole, and its titles line up with its columns.
        sc.tabs.selectTabViewItem(withIdentifier: "videoRouting")
        window.contentView?.layoutSubtreeIfNeeded()
        for popup in routing.displayPopups + routing.layoutPopups {
            XCTAssertLessThanOrEqual(
                popup.intrinsicContentSize.width, popup.frame.width, popup.titleOfSelectedItem ?? "")
        }
        for c in 0..<routing.cols.count {
            XCTAssertEqual(routing.header.column(at: c).width, routing.grid.column(at: c).width, "column \(c)")
        }
        // At the window's least size too, with overlay scroll bars and with always-shown (legacy)
        // ones, whose 17 pt scroller takes room from the page.
        let styles: [(NSScroller.Style, String)] = [(.overlay, "overlay"), (.legacy, "legacy")]
        for (style, name) in styles {
            routing.scroll.scrollerStyle = style
            window.setContentSize(window.contentMinSize)
            window.contentView?.layoutSubtreeIfNeeded()
            for popup in routing.displayPopups + routing.layoutPopups {
                XCTAssertLessThanOrEqual(
                    popup.intrinsicContentSize.width, popup.frame.width,
                    "least size, \(name) scroll bars: \(popup.titleOfSelectedItem ?? "")")
            }
        }
        routing.scroll.scrollerStyle = NSScroller.preferredScrollerStyle
        window.setContentSize(window.contentMinSize)
        window.contentView?.layoutSubtreeIfNeeded()
        let least = routing.displayWidth
        window.setContentSize(NSSize(width: 1400, height: 1000))
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertGreaterThanOrEqual(routing.displayWidth, least)
        XCTAssertEqual(routing.displayWidth, VideoSettingsPane.mostDisplayWidth)
        XCTAssertGreaterThan(routing.nameWidth, VideoSettingsPane.Col.name.width)
        for c in 0..<routing.cols.count {
            XCTAssertEqual(routing.header.column(at: c).width, routing.grid.column(at: c).width, "column \(c)")
        }
    }

    /// A long stage name is cut with an ellipsis in its field, and a long output route name in the
    /// Warnings list, whose reason is a short line of its own.
    func testALongStageNameIsShortenedAndTheReasonStays() async throws {
        let long = "A very long stage name for the upstage cyclorama projector left half"
        let (rig, sc) = try await open(stages: [(stageA, long, 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        let renamed = await rig.store.send(
            command: Command(
                .object([
                    "type": .string("set_route_property"), "id": .string(stageA),
                    "property": .object(["type": .string("name"), "value": .string(long)]),
                ])))
        guard case .success = renamed else { return XCTFail("\(renamed)") }
        await settled(sc) {
            pane.nameFields.count == 1 && routing.shown?.first?.warning != nil && routing.shown?.first?.name == long
        }
        XCTAssertEqual(pane.nameFields[0].lineBreakMode, .byTruncatingTail)
        let row = try XCTUnwrap(WarningRows.make(store: rig.store).first { $0.id == "v:\(stageA)" })
        XCTAssertEqual(row.detail, loc("warnings.stage.noDisplay"), "the reason whole, on a line of its own")
        XCTAssertTrue(row.title.contains("…"), row.title)
        XCTAssertFalse(row.title.contains(long), row.title)
    }

    /// Flow 7 without the settings window: a show whose stage has no display on this Mac is
    /// opened and its main window alone shows the stage's warning (the Warnings list and the
    /// footer's count), from what the engine sends on connecting; and again on a new engine (an
    /// engine relaunched under the app: a fresh connection and store).
    func testAStageWithoutADisplayWarnsInTheMainWindowWithoutTheSettingsWindow() async throws {
        // The warning names the stage's output route.
        let warning = String(format: loc("warnings.stage"), "Output 1")
        var engines = [engine!]
        for round in 1...2 {
            let rig = try await openShow(stages: [(stageA, "Cyc", 1920, 1080)], on: engines[round - 1])
            let wc = WorkspaceWindowController(store: rig.store)
            wc.window?.contentView?.layoutSubtreeIfNeeded()
            await assertEventually("round \(round): the stage's warning is listed") {
                // The stepped engine's clock moves only when the test moves it.
                try? await rig.advance(0.2)
                return WarningRows.make(store: rig.store).contains {
                    $0.title == warning && $0.detail == loc("warnings.stage.noDisplay")
                }
            }
            let rows = WarningRows.make(store: rig.store).count
            XCTAssertEqual(wc.footer.warningsButton.title, "\(rows)", "round \(round)")
            XCTAssertFalse(wc.footer.warningsButton.isHiddenOrHasHiddenAncestor, "round \(round)")
            wc.close()
            if round == 1 {
                engine.stop()
                engine = try await TestEngine.start(displays: Self.displays)
                engines.append(engine)
            }
        }
    }

    /// Flow 8: two stages on the same display: the layer typed for each orders them, as the
    /// display shows (each stage carries its own test card frame, read back from the display);
    /// swapping the layers swaps the picture; a layer outside 0 to 1000 is refused with a line and
    /// sends nothing.
    func testTheLayerFieldOrdersStagesOnOneDisplayAndRefusesOutOfRangeValues() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080), (stageB, "Angelica", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.nameFields.count == 2 && routing.displayPopups.count == 2 }
        let editors = [sc.openStageEditor(ModelID(stageA)), sc.openStageEditor(ModelID(stageB))]
        for (i, stage) in [stageA, stageB].enumerated() {
            let popup = routing.displayPopups[i]
            popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
            _ = popup.sendAction(popup.action, to: popup.target)
            await settled(sc) { routing.shown?[i].display == "V1" }
            editors[i].blackCheck.performClick(nil)
            await settled(sc) { rig.store.model?.stages[i].raw["keep_black"] == .bool(true) }
            let frame: UInt16 = i == 0 ? 111 : 222
            guard
                case .success = await rig.client.send({
                    .request(id: $0, .stageTestCard(stage: ModelID(stage), frame: frame))
                })
            else { return XCTFail("test card") }
        }
        func onTop() async -> UInt16? {
            guard case .success(let ok) = await engine.request(.displayProbe(display: "V1", points: [[0.5, 0.5]])),
                case .stageProbe(let frame, _)? = ok.data
            else { return nil }
            return frame
        }
        func type(_ layer: String, into row: Int) {
            let field = editors[row].layerField
            field.stringValue = layer
            editors[row].controlTextDidEndEditing(
                Notification(name: NSControl.textDidEndEditingNotification, object: field))
        }
        func layer(_ row: Int) -> Double? { rig.store.model?.stages[row].raw["layer"]?.doubleValue }
        // Angelica raised to layer 100: Angelica's picture is on top.
        type("100", into: 1)
        await settled(sc) { layer(1) == 100 }
        await assertEventually("Angelica on top") { await onTop() == 222 }
        // The layers swapped: Mary's picture is on top.
        type("200", into: 0)
        await settled(sc) { layer(0) == 200 }
        await assertEventually("Mary on top") { await onTop() == 111 }
        // Out of range: refused in the editor, nothing sent, the picture as it was.
        type("1001", into: 1)
        XCTAssertEqual(editors[1].messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(editors[1].layerField.stringValue, "100")
        let onTopNow = await onTop()
        XCTAssertEqual(onTopNow, 111)
    }

    // MARK: A stage's pixel size (#674)

    /// Types `text` into `field` and leaves it, as the operator's Tab or Return does.
    private func type(_ text: String, into field: NSTextField, _ pane: VideoSettingsPane) {
        field.stringValue = text
        pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
    }

    /// As `type(_:into:_:)`, in a stage editor's field.
    private func type(_ text: String, into field: NSTextField, _ editor: StageEditorWindowController) {
        field.stringValue = text
        editor.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
    }

    /// Picks `title` from `popup` (a pull-down's item too), as the operator's choice in its menu.
    private func choose(_ title: String, in popup: NSPopUpButton, line: UInt = #line) {
        guard let item = popup.menu?.items.first(where: { $0.title == title && !$0.isSeparatorItem }) else {
            return XCTFail("no item \"\(title)\" in \(popup.itemTitles)", line: line)
        }
        popup.select(item)
        _ = popup.sendAction(popup.action, to: popup.target)
    }

    /// The colours display `display` shows at `points`.
    private func displayColours(_ display: String, _ points: [[Double]]) async -> [[UInt8]] {
        guard case .success(let ok) = await engine.request(.displayProbe(display: display, points: points)),
            case .stageProbe(_, let colours)? = ok.data
        else { return [] }
        return colours
    }

    /// Opens `editor`'s resize dialog (off screen) and types `width` (and `height` when given) with the
    /// shape box set to `keepShape`, regions handled by `mode`; OK when `ok`.
    @discardableResult
    private func resize(
        _ editor: StageEditorWindowController, width: String, height: String? = nil, keepShape: Bool = false,
        mode: ResizeStageDialog.Mode = .scale, ok: Bool = true
    ) throws -> ResizeStageDialog {
        editor.resizeClicked()
        let dialog = try XCTUnwrap(editor.resizeDialog, "the dialog is up")
        dialog.keepShapeCheck.state = keepShape ? .on : .off
        dialog.modePopup.selectItem(at: mode == .scale ? 0 : 1)
        dialog.widthField.stringValue = width
        dialog.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: dialog.widthField))
        if let height {
            dialog.heightField.stringValue = height
            dialog.controlTextDidChange(
                Notification(name: NSControl.textDidChangeNotification, object: dialog.heightField))
        }
        if ok { dialog.okClicked() }
        return dialog
    }

    /// #674 flow 1, #813 G6 and #860 B7: a stage's size is changed from its editor's resize dialog,
    /// regions scaled: its row and the editor's size text show the new size, its region follows it
    /// (still the whole stage) and still fills its route on V1, and one undo puts both back.
    func testStageEditor_resizeScalesRegionsOneUndo() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.sizeLabels.count == 1 && routing.displayPopups.count == 1 }
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertEqual(editor.sizeLabel.stringValue, "1920 × 1080")
        let popup = routing.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { routing.shown?.first?.display == "V1" }
        guard
            case .success = await rig.client.send({ .request(id: $0, .stageTestCard(stage: ModelID(stageA), frame: 7)) }
            )
        else { return XCTFail("test card") }
        let red: [UInt8] = [255, 0, 0, 255]
        let black: [UInt8] = [0, 0, 0, 255]
        // Near V1's left edge, then a little further in: the card's red quadrant both, the
        // stage filling the display.
        let points: [[Double]] = [[0.05, 0.3], [0.3, 0.3]]
        await assertEventually("the card fills V1") { await self.displayColours("V1", points) == [red, red] }
        // The shape kept: the height follows the width typed; unticked it does not.
        let kept = try resize(editor, width: "960", keepShape: true, ok: false)
        XCTAssertEqual(kept.heightField.stringValue, "540", "the height follows with the shape kept")
        kept.cancelButton.performClick(nil)
        XCTAssertNil(editor.resizeDialog, "Cancel takes it down")
        await editor.editing?.value
        XCTAssertEqual(rig.store.document?.canUndo, false, "Cancel sent nothing")
        try resize(editor, width: "1080")
        await settled(sc) { pane.shown?.first?.width == 1080 }
        await editor.editing?.value
        editor.render()
        XCTAssertEqual(pane.sizeLabels[0].stringValue, "1080 × 1080")
        XCTAssertEqual(editor.sizeLabel.stringValue, "1080 × 1080")
        XCTAssertEqual(editor.messageLabel.stringValue, "")
        XCTAssertEqual(editor.regions.map(\.bounds), [RegionBounds(x: 0, y: 0, width: 1080, height: 1080)])
        XCTAssertEqual(editor.canvas.regionFrames, [editor.canvas.stageFrame], "the region still covers the stage")
        XCTAssertEqual(rig.store.document?.undoName, "resize_stage")
        await assertEventually("the square stage's region still fills V1") {
            await self.displayColours("V1", points + [[0.5, 0.05]]) == [red, red, black]
        }
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.first?.width == 1920 }
        editor.render()
        XCTAssertEqual(editor.sizeLabel.stringValue, "1920 × 1080")
        XCTAssertEqual(editor.regions.map(\.bounds), [RegionBounds(x: 0, y: 0, width: 1920, height: 1080)])
        await assertEventually("back across V1") { await self.displayColours("V1", points) == [red, red] }
    }

    /// #674 flow 2 and #860 B6: the resize dialog has the width and height, the shape box ticked,
    /// the regions' choice and a picture; a size of 0, above the limit, or not a number keeps it up
    /// with the reason and nothing is sent; Cancel changes nothing; the largest size is taken.
    func testStageEditor_resizeDialogControls() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.sizeLabels.count == 1 }
        let editor = sc.openStageEditor(ModelID(stageA))
        editor.resizeClicked()
        let dialog = try XCTUnwrap(editor.resizeDialog)
        let window = try XCTUnwrap(dialog.window)
        XCTAssertNil(window.sheetParent, "a window of its own, not a sheet")
        XCTAssertEqual(window.title, loc("resize.title"))
        XCTAssertEqual([dialog.widthField.stringValue, dialog.heightField.stringValue], ["1920", "1080"])
        XCTAssertEqual(dialog.keepShapeCheck.state, .on, "the shape is kept by default")
        XCTAssertEqual(dialog.modePopup.itemTitles, [loc("resize.mode.scale"), loc("resize.mode.cropOrPad")])
        XCTAssertEqual(dialog.preview.newSize, NSSize(width: 1920, height: 1080))
        XCTAssertEqual([dialog.cancelButton.title, dialog.okButton.title], ["Cancel", "OK"])
        dialog.cancelButton.performClick(nil)
        XCTAssertNil(editor.resizeDialog)
        for (text, width) in [("0", true), ("16385", false), ("abc", true), ("-5", false), ("", true)] {
            let d = try resize(editor, width: width ? text : "1920", height: width ? "1080" : text)
            XCTAssertEqual(d.messageLabel.stringValue, loc("settings.video.badSize"), text)
            XCTAssertTrue(editor.resizeDialog === d, "\(text): the dialog stays up")
            d.cancelButton.performClick(nil)
        }
        await editor.editing?.value
        XCTAssertEqual(rig.store.model?.stages.first?.raw["width"]?.doubleValue, 1920)
        XCTAssertEqual(rig.store.document?.canUndo, false, "nothing was sent")
        try resize(editor, width: "1920", height: "16384")
        await settled(sc) { pane.shown?.first?.height == 16384 }
        await editor.editing?.value
        XCTAssertEqual(editor.messageLabel.stringValue, "")
    }

    /// #860 B8: a stage 1920 x 1080 with region A on its left half and B partly past its right
    /// edge once shrunk; resized to 1280 x 720 keeping the regions' size (crop or pad): each keeps
    /// its pixel size and its place about the centre, cut at the new edges; one undo restores both.
    func testStageEditor_resizeCropPadKeepsRegions() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920,
            regions: [(regionA, route1, 0, 960), (regionB, nil, 1500, 400)])
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { editor.regionRows.count == 2 }
        let before = editor.regions
        try resize(editor, width: "1280", height: "720", mode: .cropOrPad)
        await editorSettled(sc, editor) { editor.sizeLabel.stringValue == "1280 × 720" }
        // The stage loses 320 a side and 180 above and below: A (0..960 x 0..1080) becomes
        // -320..640 x -180..900, cut to 0..640 x 0..720; B (1500..1900) becomes 1180..1580, cut to 1180..1280.
        XCTAssertEqual(
            editor.regions.map(\.bounds),
            [RegionBounds(x: 0, y: 0, width: 640, height: 720), RegionBounds(x: 1180, y: 0, width: 100, height: 720)])
        XCTAssertEqual(editor.regions.map(\.route), [ModelID(route1), nil], "routes kept")
        XCTAssertEqual(rig.store.document?.undoName, "resize_stage")
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.sizeLabel.stringValue == "1920 × 1080" }
        XCTAssertEqual(editor.regions, before, "one undo restores the size and the regions exactly")
    }

    /// Crop or pad never drops a region: one left wholly outside the smaller stage comes in to the
    /// nearest edge with its size; a larger stage pads round each region, which keeps its place
    /// about the centre.
    func testCropOrPadClampsARegionWhollyOutsideAndPadsAroundTheCentre() {
        let a = RegionRow(id: "a", route: nil, bounds: RegionBounds(x: 1800, y: 0, width: 100, height: 100))
        let b = RegionRow(id: "b", route: nil, bounds: RegionBounds(x: 0, y: 980, width: 100, height: 100))
        let shrunk = ResizeStageDialog.cropOrPad([a, b], from: (1920, 1080), to: (1000, 600))
        XCTAssertEqual(
            shrunk.map(\.bounds),
            [RegionBounds(x: 900, y: 0, width: 100, height: 100), RegionBounds(x: 0, y: 500, width: 100, height: 100)])
        let grown = ResizeStageDialog.cropOrPad([a], from: (1920, 1080), to: (3840, 2160))
        XCTAssertEqual(grown.map(\.bounds), [RegionBounds(x: 2760, y: 540, width: 100, height: 100)])
    }

    /// A layer typed in the stage editor and committed with Return, then undone: the field shows the
    /// layer the undo put back, and a click on another control sends no layer (the undone one stays).
    func testALayerUndoneAfterReturnIsShownAndAClickElsewhereNeverSendsItAgain() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080), (stageB, "Side", 1280, 720)])
        let pane = sc.videoPane
        await settled(sc) { pane.sizeLabels.count == 2 }
        let stageEditor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(stageEditor.window)
        XCTAssertTrue(window.makeFirstResponder(stageEditor.layerField))
        try XCTUnwrap(stageEditor.layerField.currentEditor()).string = "400"
        let editor = try XCTUnwrap(stageEditor.layerField.currentEditor() as? NSTextView)
        editor.insertNewline(nil)
        await settled(sc) { rig.store.model?.stages.first?.raw["layer"]?.doubleValue == 400 }
        await stageEditor.editing?.value
        _ = await rig.store.undo()
        await settled(sc) { rig.store.model?.stages.first?.raw["layer"]?.doubleValue == 1 }
        await assertEventually("the undone layer is shown") { stageEditor.layerField.stringValue == "1" }
        XCTAssertNotEqual(stageEditor.layerField.currentEditor()?.string, "400")
        stageEditor.blackCheck.performClick(nil)
        await stageEditor.editing?.value
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(true) }
        XCTAssertEqual(rig.store.model?.stages.first?.raw["layer"]?.doubleValue, 1, "no layer sent again")
        XCTAssertEqual(stageEditor.layerField.stringValue, "1")
        XCTAssertEqual(stageEditor.messageLabel.stringValue, "")
    }

    /// In a wide window the name column takes the room the display column leaves, so a long name
    /// reads whole.
    func testALongNameShowsWholeInAWideWindow() async throws {
        let long = "Upstage cyclorama projector for the second act"
        let (_, sc) = try await open(stages: [(stageA, long, 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 1 }
        let window = try XCTUnwrap(sc.window)
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        window.setContentSize(NSSize(width: 1600, height: 1000))
        window.contentView?.layoutSubtreeIfNeeded()
        window.contentView?.layoutSubtreeIfNeeded()
        let field = pane.nameFields[0]
        XCTAssertLessThanOrEqual(
            try XCTUnwrap(field.cell).cellSize(forBounds: NSRect(x: 0, y: 0, width: 10_000, height: 100)).width,
            field.frame.width, "the whole name fits")
        XCTAssertEqual(pane.header.column(at: 0).width, pane.grid.column(at: 0).width)
        let right = pane.grid.convert(pane.grid.bounds, to: pane.scroll.contentView).maxX
        XCTAssertGreaterThan(right, pane.scroll.contentView.bounds.width - 40, "the table takes the page's width")
    }

    // MARK: Output routes and stage regions (#813)

    private let route1 = "00000000-0000-0000-0000-00000000b001"
    private let route2 = "00000000-0000-0000-0000-00000000b002"
    private let regionA = "00000000-0000-0000-0000-00000000c001"
    private let regionB = "00000000-0000-0000-0000-00000000c002"
    private let black: [UInt8] = [0, 0, 0, 255]
    private let blue: [UInt8] = [0, 0, 255, 255]
    private let yellow: [UInt8] = [255, 255, 0, 255]

    /// The Demo show with `routes` (`[id, name, width, height]`) and one stage "Stage 1" (id
    /// `stageA`) of `width` x 1080 holding `regions` (`[id, route, x, width]`, each the stage's
    /// whole height), its settings window open; each route in `displays` shown on that display here.
    private func openRouted(
        routes: [(String, String, Int, Int)], width: Int, regions: [(String, String?, Int, Int)],
        displays: [(String, String)] = []
    ) async throws -> (Rig, WorkspaceSettingsWindowController) {
        let stage = stageA
        let rig = try await Rig.open("Demo", on: engine) { folder in
            let file = folder.appendingPathComponent("Demo.openlab")
            var root = try XCTUnwrap(
                JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
            var ws = try XCTUnwrap(root["workspace"] as? [String: Any])
            ws["routes"] = routes.map { ["id": $0.0, "name": $0.1, "width": $0.2, "height": $0.3] as [String: Any] }
            let shapes = regions.map { r -> [String: Any] in
                var d: [String: Any] = ["id": r.0, "x": r.2, "y": 0, "width": r.3, "height": 1080]
                if let route = r.1 { d["route"] = route }
                return d
            }
            ws["stages"] = [
                [
                    "id": stage, "name": "Stage 1", "width": width, "height": 1080, "keep_black": false,
                    "layer": 1, "regions": shapes,
                ] as [String: Any]
            ]
            root["workspace"] = ws
            try JSONSerialization.data(withJSONObject: root).write(to: file)
        }
        if !displays.isEmpty { await displaysListed() }
        for (route, display) in displays {
            guard case .success = await rig.store.setRouteDisplay(ModelID(route), display: display) else {
                XCTFail("display \(display) for \(route)")
                break
            }
        }
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        return (rig, sc)
    }

    /// Returns once the stage host, started with the engine, has listed both virtual displays: a
    /// display is chosen only once it is listed.
    private func displaysListed() async {
        await assertEventually("the stage host listed its displays") {
            guard case .success(let ok) = await self.engine.request(.listDisplays),
                case .displays(let list, _)? = ok.data
            else { return false }
            return list.count == 2
        }
    }

    /// The G4 workspace: Stage 1 at 3840 x 1080, region A (its left half) on Output 1 shown on V1
    /// and region B (its right half) on Output 2 shown on V2; the stage shows its test card.
    private func openG4() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080), (route2, "Output 2", 1280, 720)], width: 3840,
            regions: [(regionA, route1, 0, 1920), (regionB, route2, 1920, 1920)],
            displays: [(route1, "V1"), (route2, "V2")])
        guard
            case .success = await rig.client.send({
                .request(id: $0, .stageTestCard(stage: ModelID(self.stageA), frame: 5))
            })
        else {
            XCTFail("test card")
            return (rig, sc)
        }
        return (rig, sc)
    }

    /// What V1 and V2 show low in their middle: the card's lower-left (blue) quadrant on V1 and
    /// its lower-right (yellow) on V2 while each shows its half of the G4 stage; black where a
    /// display shows nothing.
    private func lowerMiddles() async -> [[UInt8]] {
        let v1 = await displayColours("V1", [[0.5, 0.8]])
        let v2 = await displayColours("V2", [[0.5, 0.8]])
        return v1 + v2
    }

    /// Waits until the stage editor shows what the engine holds.
    private func editorSettled(
        _ sc: WorkspaceSettingsWindowController, _ editor: StageEditorWindowController,
        _ why: String = "the stage editor caught up", _ ok: @escaping @MainActor () -> Bool
    ) async {
        await assertEventually(why) {
            await editor.editing?.value
            await sc.videoPane.pending?.value
            await sc.videoRoutingPane.pending?.value
            await sc.videoPane.refresh()
            editor.render()
            return ok()
        }
    }

    /// A press of the mouse at `at` (window coordinates) in `editor`'s window.
    private func press(_ editor: StageEditorWindowController, at: NSPoint = .zero) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: at, modifierFlags: [], timestamp: 0,
                windowNumber: editor.window?.windowNumber ?? 0, context: nil, eventNumber: 0, clickCount: 1,
                pressure: 1))
    }

    /// The titles of a pop-up's items, separators left out.
    private func titles(_ popup: NSPopUpButton) -> [String] {
        (popup.menu?.items ?? []).filter { !$0.isSeparatorItem }.map(\.title)
    }

    /// #813 N1: a Mac with a main display V1 and a second display V2, a new workspace → Output
    /// Routing lists one route, Output 1, sized as V1, on V1, filling it, not turned; Video Outputs
    /// lists Stage 1, the same size, its displays column naming V1; its editor shows one region A
    /// covering the whole stage, sent to Output 1.
    func testN1ANewWorkspaceHasOutput1OnTheMainDisplayAndStage1FillingIt() async throws {
        await assertEventually("the stage host listed its displays") {
            guard case .success(let ok) = await self.engine.request(.listDisplays),
                case .displays(let list, _)? = ok.data
            else { return false }
            return list.count == 2
        }
        guard case .success(let ok) = await engine.request(.new), case .opened(let doc)? = ok.data else {
            return XCTFail("new")
        }
        let rig = Rig(engine: engine, workspace: doc)
        guard await rig.connect() else { return XCTFail("connect") }
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) {
            pane.nameFields.count == 1 && routing.displayPopups.count == 1 && !pane.deviceCells.isEmpty
        }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1"])
        XCTAssertEqual(routing.sizeLabels.map(\.stringValue), ["1920 × 1080"])
        XCTAssertEqual(routing.displayPopups.map { $0.titleOfSelectedItem }, ["Virtual V1 (1920 × 1080)"])
        XCTAssertEqual(routing.layoutPopups.map { $0.title }, [loc("settings.video.layout.full")])
        XCTAssertEqual(routing.shown?.map(\.scaling), ["fill"])
        XCTAssertEqual(routing.warningLabels.map(\.stringValue), [""], "no warning: shown")
        XCTAssertEqual(rig.store.model?.routes.first?.raw["rotation"]?.doubleValue ?? 0, 0, "not turned")
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1"])
        XCTAssertEqual(pane.sizeLabels.map(\.stringValue), ["1920 × 1080"])
        XCTAssertEqual(pane.deviceCells.map(\.stringValue), ["Virtual V1"])
        XCTAssertTrue(pane.emptyLabel.isHidden)
        let stage = try XCTUnwrap(rig.store.model?.stages.first?.id)
        pane.editButtons[0].performClick(nil)
        let editor = try XCTUnwrap(sc.stageEditors[stage])
        XCTAssertEqual(editor.regionChips.map(\.letter), ["A"])
        XCTAssertEqual(editor.regionRoutePopups.map { $0.titleOfSelectedItem }, ["Output 1 (Virtual V1)"])
        XCTAssertEqual(editor.canvas.regionFrames, [editor.canvas.stageFrame], "A covers the whole stage")
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A → Output 1"])
        XCTAssertEqual(
            [editor.regionWidthField, editor.regionHeightField, editor.lowerLeftXField, editor.lowerLeftYField]
                .map(\.stringValue), ["1920", "1080", "-960", "-540"])
    }

    /// #813 N2 (the app's part): openLab with no video output on this Mac (no displays listed), a
    /// new workspace → Output 1 (1920 x 1080) and Stage 1 with its region exist as in N1; the
    /// route's row says it has no display on this Mac, and the stage's row that it is shown on none.
    func testN2ANewWorkspaceWithNoDisplaysSaysItsRouteHasNoDisplayHere() async throws {
        engine.stop()
        engine = try await TestEngine.start()
        guard case .success(let ok) = await engine.request(.new), case .opened(let doc)? = ok.data else {
            return XCTFail("new")
        }
        let rig = Rig(engine: engine, workspace: doc)
        guard await rig.connect() else { return XCTFail("connect") }
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.shown?.first?.warning != nil && pane.shown?.count == 1 }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1"])
        XCTAssertEqual(routing.sizeLabels.map(\.stringValue), ["1920 × 1080"])
        XCTAssertEqual(routing.displayPopups.map { $0.titleOfSelectedItem }, [loc("settings.video.displayNone")])
        XCTAssertEqual(routing.warningLabels.map(\.stringValue), [loc("settings.video.warn.noDisplay")])
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1"])
        XCTAssertEqual(
            pane.deviceCells.map(\.stringValue), [String(format: loc("video.route.noDisplayNamed"), "Output 1")])
        XCTAssertEqual(pane.warningLabels.map(\.stringValue), [loc("settings.video.warn.stageNoDisplay")])
        let editor = sc.openStageEditor(try XCTUnwrap(rig.store.model?.stages.first?.id))
        XCTAssertEqual(editor.regionRoutePopups.map { $0.titleOfSelectedItem }, ["Output 1 (no display)"])
        XCTAssertEqual(editor.canvas.regionFrames, [editor.canvas.stageFrame])
    }

    /// #813 R1: two displays, the N1 workspace → the add-route pop-up offers each display and a
    /// route on none; choosing V2 adds "Output 2", sized as V2, on V2, filling it, not turned.
    /// Undo removes it; redo brings it back, still on V2.
    func testR1ChoosingADisplayFromTheAddRoutePopUpAddsOutput2OnItAndUndoRedoKeepIt() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920, regions: [(regionA, route1, 0, 1920)],
            displays: [(route1, "V1")])
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.nameFields.count == 1 }
        XCTAssertEqual(
            titles(routing.addPopup),
            [
                loc("settings.video.newRoute.key"), loc("settings.video.addRouteWith"), "   Virtual V1 (1920 × 1080)",
                "   Virtual V2 (1280 × 720)", loc("settings.video.addRouteNone"),
            ])
        choose("   Virtual V2 (1280 × 720)", in: routing.addPopup)
        await settled(sc) { routing.shown?.count == 2 && routing.shown?.last?.display == "V2" }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1", "Output 2"])
        XCTAssertEqual(routing.sizeLabels.last?.stringValue, "1280 × 720")
        XCTAssertEqual(routing.displayPopups.last?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        XCTAssertEqual(routing.shown?.last?.scaling, "fill")
        XCTAssertEqual(rig.store.model?.routes.last?.raw["rotation"]?.doubleValue ?? 0, 0)
        XCTAssertEqual(rig.store.document?.undoName, "add_route")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.count == 1 }
        _ = await rig.store.redo()
        await settled(sc) { routing.shown?.count == 2 && routing.shown?.last?.display == "V2" }
        XCTAssertEqual(routing.displayPopups.last?.titleOfSelectedItem, "Virtual V2 (1280 × 720)", "still on V2")
        // With no display: 1920 x 1080 on none.
        choose(loc("settings.video.addRouteNone"), in: routing.addPopup)
        await settled(sc) { routing.shown?.count == 3 }
        XCTAssertEqual(routing.nameFields.last?.stringValue, "Output 3")
        XCTAssertEqual(routing.sizeLabels.last?.stringValue, "1920 × 1080")
        XCTAssertEqual(routing.displayPopups.last?.titleOfSelectedItem, loc("settings.video.displayNone"))
    }

    /// #813 R2: route rows shown → a new name typed in Output 2's name cell and Return: the row and
    /// every region route pop-up of the stage editor show it. An empty name is refused and the old
    /// name comes back. Two routes may share a name.
    func testR2ARouteRenamedInPlaceShowsInTheRowAndTheRegionPopUpsAndAnEmptyNameIsRefused() async throws {
        let (rig, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.nameFields.count == 2 }
        let editor = sc.openStageEditor(ModelID(stageA))
        type("Projector", into: routing.nameFields[1], routing)
        await settled(sc) { routing.shown?.last?.name == "Projector" }
        XCTAssertEqual(routing.nameFields[1].stringValue, "Projector")
        await editorSettled(sc, editor) {
            editor.regionRoutePopups.last?.titleOfSelectedItem == "Projector (Virtual V2)"
        }
        XCTAssertEqual(
            titles(editor.regionRoutePopups[0]),
            [loc("stageEditor.noRoute"), "Output 1 (Virtual V1)", "Projector (Virtual V2)"])
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A → Output 1", "B → Projector"])
        XCTAssertEqual(rig.store.document?.undoName, "rename_route")
        // Empty: refused, the old name back.
        type("  ", into: routing.nameFields[1], routing)
        XCTAssertEqual(routing.messageLabel.stringValue, loc("settings.video.badRouteName"))
        XCTAssertEqual(routing.nameFields[1].stringValue, "Projector")
        // The same name as Output 1: taken.
        type("Output 1", into: routing.nameFields[1], routing)
        await settled(sc) { routing.shown?.last?.name == "Output 1" }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1", "Output 1"])
        XCTAssertEqual(routing.messageLabel.stringValue, "")
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).last == "Output 1 (Virtual V2)" }
        XCTAssertEqual(
            titles(editor.regionRoutePopups[0]),
            [loc("stageEditor.noRoute"), "Output 1 (Virtual V1)", "Output 1 (Virtual V2)"])
    }

    /// #813 R3: two routes → Output 2 dragged by its handle above Output 1: the rows swap, the
    /// region route pop-ups list the routes in the new order, and Undo puts them back.
    func testR3DraggingOutput2AboveOutput1SwapsTheRowsAndThePopUpsAndUndoPutsThemBack() async throws {
        let (rig, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        sc.tabs.selectTabViewItem(withIdentifier: "videoRouting")
        await settled(sc) { routing.handles.count == 2 }
        let window = try XCTUnwrap(sc.window)
        window.contentView?.layoutSubtreeIfNeeded()
        let editor = sc.openStageEditor(ModelID(stageA))
        let handle = routing.handles[1]
        let from = handle.convert(NSPoint(x: handle.bounds.midX, y: handle.bounds.midY), to: nil)
        XCTAssertTrue(window.contentView?.hitTest(from) === handle, "a press there lands on the handle")
        let top = routing.nameFields[0].convert(routing.nameFields[0].bounds, to: routing.holder)
        let to = routing.holder.convert(NSPoint(x: 40, y: top.minY + 1), to: nil)
        func event(_ type: NSEvent.EventType, _ at: NSPoint) throws -> NSEvent {
            try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: at, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                    context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        }
        handle.mouseDown(with: try event(.leftMouseDown, from))
        handle.mouseDragged(with: try event(.leftMouseDragged, to))
        XCTAssertFalse(routing.dropLine.isHidden, "the line shows where it goes")
        XCTAssertLessThanOrEqual(routing.dropLine.frame.maxY, top.minY + 1, "above Output 1")
        handle.mouseUp(with: try event(.leftMouseUp, to))
        XCTAssertTrue(routing.dropLine.isHidden)
        await settled(sc) { routing.shown?.first?.name == "Output 2" }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 2", "Output 1"])
        XCTAssertEqual(rig.store.document?.undoName, "move_route")
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).dropFirst().first == "Output 2 (Virtual V2)" }
        XCTAssertEqual(
            titles(editor.regionRoutePopups[0]),
            [loc("stageEditor.noRoute"), "Output 2 (Virtual V2)", "Output 1 (Virtual V1)"])
        XCTAssertEqual(
            Array(titles(editor.newRegionPopup).dropFirst()),
            ["On Output 2 (Virtual V2)", "On Output 1 (Virtual V1)", loc("stageEditor.newRegionNone")])
        // Let go where it already is: nothing is sent.
        routing.move(from: 0, toGap: 1)
        await routing.pending?.value
        XCTAssertEqual(rig.store.document?.undoName, "move_route")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.first?.name == "Output 1" }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1", "Output 2"])
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).dropFirst().first == "Output 1 (Virtual V1)" }
    }

    /// A route removed while its editor is up (by another client or an undo): OK is refused with
    /// the route named as gone, never by its id.
    func testTheRouteEditorOfARouteRemovedMeanwhileSaysItIsGone() async throws {
        let (rig, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.editButtons.count == 2 }
        routing.editButtons[1].performClick(nil)
        let editor = try XCTUnwrap(sc.routeEditor)
        let gone = editor.routeID
        guard case .success = await rig.store.send(command: .removeRoute(gone)) else { return XCTFail("remove") }
        await settled(sc) { rig.store.model?.routes.contains { $0.id == gone } == false }
        editor.widthField.stringValue = "1000"
        editor.okButton.performClick(nil)
        await editor.saving?.value
        XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.routeGone"))
        XCTAssertFalse(editor.messageLabel.stringValue.contains(gone.raw), "no id shown")
    }

    /// #813 R8 (the app's part): a route whose display this Mac does not have now → its row names
    /// the display and marks it not connected, with a warning, and the stage's displays column
    /// names it the same way.
    func testR8ARoutesDisplayNotConnectedIsNamedAndMarkedInItsRowWithAWarning() async throws {
        let settings: [String: Any] = [
            "settings_version": 1,
            "workspaces": [
                "00000000-0000-0000-0000-0000000000d1": [
                    "patches": [String: Any](),
                    "routes": [
                        stageA: ["display": "GONE", "name": "Projector", "width": 1920, "height": 1080]
                    ],
                ]
            ],
        ]
        engine.stop()
        engine = try await TestEngine.start(
            displays: Self.displays, settings: try JSONSerialization.data(withJSONObject: settings))
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.shown?.first?.warning == loc("settings.video.warn.missing") }
        XCTAssertEqual(routing.displayPopups.first?.titleOfSelectedItem, "Projector (not connected)")
        XCTAssertEqual(routing.warningLabels.map(\.stringValue), [loc("settings.video.warn.missing")])
        XCTAssertEqual(pane.deviceCells.map(\.stringValue), ["Projector (not connected)"])
    }

    /// #813 R9 (the app's part): G4 → Output 2 removed: its row goes; in the stage editor region B
    /// has no route (its pop-up says so, the canvas draws it dashed) and V2 shows nothing while V1
    /// keeps its half. Undo brings Output 2 back on V2 with region B on it.
    func testR9RemovingARouteLeavesItsRegionWithNoneDashedAndUndoPutsItBackOnItsDisplay() async throws {
        let (rig, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.removeButtons.count == 2 }
        let editor = sc.openStageEditor(ModelID(stageA))
        await assertEventually("both halves shown") { await self.lowerMiddles() == [self.blue, self.yellow] }
        routing.removeButtons[1].performClick(nil)
        await settled(sc) { routing.shown?.count == 1 }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1"])
        XCTAssertEqual(rig.store.document?.undoName, "remove_route")
        await editorSettled(sc, editor) {
            editor.regionRoutePopups.last?.titleOfSelectedItem == loc("stageEditor.noRoute")
        }
        XCTAssertNil(editor.canvas.regions[1].route, "B drawn dashed")
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label)[1], "B", "the letter alone")
        await assertEventually("V2 freed, V1 plays on") { await self.lowerMiddles() == [self.blue, self.black] }
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.count == 2 && routing.shown?.last?.display == "V2" }
        XCTAssertEqual(routing.displayPopups.last?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        await editorSettled(sc, editor) { editor.regionRoutePopups.last?.titleOfSelectedItem == "Output 2 (Virtual V2)" }
        XCTAssertNotNil(editor.canvas.regions[1].route)
        await assertEventually("both halves back") { await self.lowerMiddles() == [self.blue, self.yellow] }
    }

    /// A region lies inside a stage only when every pixel of it does; the test never overflows, so
    /// a typed size of any value is judged, not trapped on.
    func testRegionBoundsInsideAtItsEdges() {
        let stage = (3840, 1080)
        func inside(_ x: Int, _ y: Int, _ w: Int, _ h: Int) -> Bool {
            RegionBounds(x: x, y: y, width: w, height: h).inside(width: stage.0, height: stage.1)
        }
        XCTAssertTrue(inside(0, 0, 3840, 1080), "an exact fit")
        XCTAssertTrue(inside(3839, 1079, 1, 1), "the last pixel")
        XCTAssertFalse(inside(0, 0, 3841, 1080), "one past the width")
        XCTAssertFalse(inside(0, 0, 3840, 1081), "one past the height")
        XCTAssertFalse(inside(1, 0, 3840, 1080), "moved one right")
        XCTAssertFalse(inside(0, 1, 3840, 1080), "moved one down")
        XCTAssertFalse(inside(-1, 0, 100, 100), "left of the stage")
        XCTAssertFalse(inside(0, -1, 100, 100), "above the stage")
        XCTAssertFalse(inside(0, 0, 0, 10), "no width")
        XCTAssertFalse(inside(0, 0, 10, 0), "no height")
        XCTAssertFalse(inside(1920, 0, Int.max, 1080), "Int.max wide, which a sum would overflow")
        XCTAssertFalse(inside(0, 540, 1920, Int.max), "Int.max high")
        XCTAssertFalse(inside(Int.max, 0, 1, 1), "at Int.max")
    }

    /// The new-region pop-up keeps its menu through a redraw that changes none of its items (a
    /// region selected, an edit answered), so a pick made while it is open is not dropped; a route
    /// renamed does change it.
    func testTheNewRegionMenuIsKeptUnlessItsItemsChange() async throws {
        let (rig, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        let menu = try XCTUnwrap(editor.newRegionPopup.menu)
        let titles = editor.newRegionPopup.itemTitles
        // A redraw for the selection moving, and for an edit.
        editor.regionChips[1].mouseDown(with: try press(editor))
        XCTAssertEqual(editor.canvas.selected, 1)
        type("960", into: editor.regionWidthField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.width == 960 }
        XCTAssertTrue(editor.newRegionPopup.menu === menu, "the same menu after the selection and an edit")
        // A route renamed: the items follow it.
        let renamed = await rig.store.send(
            command: .setRoute(ModelID(route1), .object(["type": .string("name"), "value": .string("Front")])))
        guard case .success = renamed else { return XCTFail("\(renamed)") }
        await editorSettled(sc, editor) { editor.newRegionPopup.itemTitles != titles }
        XCTAssertTrue(
            editor.newRegionPopup.itemTitles.contains(String(format: loc("stageEditor.newRegionOn"), "Front (Virtual V1)")))
        // The new menu works: a region on the renamed route.
        choose(String(format: loc("stageEditor.newRegionOn"), "Front (Virtual V1)"), in: editor.newRegionPopup)
        await editorSettled(sc, editor) { editor.regionChips.count == 3 }
        XCTAssertEqual(editor.regions.last?.route, ModelID(route1))
    }

    /// #813 G5 (the app's part): G4 → region B's route pop-up set to none: the canvas draws B
    /// dashed, V2 shows nothing while V1 keeps its half, and Stage 1's displays column names V1
    /// only. Undo puts B back on V2.
    func testG5RegionBSetToNoRouteIsDashedFreesV2AndUndoPutsItBack() async throws {
        let (rig, sc) = try await openG4()
        let pane = sc.videoPane
        let editor = sc.openStageEditor(ModelID(stageA))
        await settled(sc) { pane.deviceCells.first?.stringValue == "Virtual V1, Virtual V2" }
        await assertEventually("both halves shown") { await self.lowerMiddles() == [self.blue, self.yellow] }
        choose(loc("stageEditor.noRoute"), in: editor.regionRoutePopups[1])
        await editorSettled(sc, editor) { editor.canvas.regions[1].route == nil }
        XCTAssertEqual(editor.regionRoutePopups[1].titleOfSelectedItem, loc("stageEditor.noRoute"))
        XCTAssertEqual(rig.store.document?.undoName, "region_route")
        await settled(sc) { pane.deviceCells.first?.stringValue == "Virtual V1" }
        await assertEventually("V2 freed, V1 plays on") { await self.lowerMiddles() == [self.blue, self.black] }
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.canvas.regions[1].route == "Output 2" }
        await settled(sc) { pane.deviceCells.first?.stringValue == "Virtual V1, Virtual V2" }
        await assertEventually("B back on V2") { await self.lowerMiddles() == [self.blue, self.yellow] }
    }

    /// #813 G7 (the app's part): a region removed with its button leaves the list and the canvas;
    /// undo brings it back on its route.
    func testG7RemovingARegionTakesItOffTheListAndCanvasAndUndoBringsItBack() async throws {
        let (rig, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        editor.regionRemoveButtons[1].performClick(nil)
        await editorSettled(sc, editor) { editor.regionChips.count == 1 }
        XCTAssertEqual(editor.canvas.regions.count, 1)
        XCTAssertEqual(rig.store.document?.undoName, "remove_region")
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.regionChips.count == 2 }
        XCTAssertEqual(editor.regionRoutePopups[1].titleOfSelectedItem, "Output 2 (Virtual V2)")
    }

    /// A new stage or route is "Stage N" or "Output N", one past the highest number in use; a
    /// typed name whose number the engine would not read, or the largest it reads, never
    /// overflows.
    func testNewNamesTakeTheNextNumberAndNeverOverflow() {
        let routes = loc("settings.video.routeName")
        XCTAssertEqual(VideoSettingsPane.nextName(routes, after: []), "Output 1")
        XCTAssertEqual(VideoSettingsPane.nextName(routes, after: ["Output 1", "Output 4", "Cyc"]), "Output 5")
        XCTAssertEqual(VideoSettingsPane.nextName(routes, after: ["Output 99999999999999999999"]), "Output 1")
        XCTAssertEqual(VideoSettingsPane.nextName(routes, after: ["Output 4294967295"]), "Output 4294967295")
        XCTAssertEqual(VideoSettingsPane.nextName(routes, after: ["Output -3", "Output 2"]), "Output 3")
        XCTAssertEqual(VideoSettingsPane.nextName(loc("settings.video.newName"), after: ["Stage 1"]), "Stage 2")
    }

    /// The Output Routing page's button opens this Mac's Displays settings.
    func testTheDisplaysButtonOpensTheMacsDisplaySettings() async throws {
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        var urls: [URL] = []
        sc.videoRoutingPane.openURL = { urls.append($0) }
        sc.videoRoutingPane.displaysButton.performClick(nil)
        XCTAssertEqual(urls, [VideoSettingsPane.displaysSettings].compactMap { $0 })
        XCTAssertEqual(urls.first?.scheme, "x-apple.systempreferences")
    }

    /// #842 V4: the Displays Settings button is on the Output Devices page too, in the top row
    /// beside the caption (as on Output Routing), and opens the same settings; the Video Outputs
    /// page has none.
    func testV4TheDisplaysSettingsButtonIsOnBothOutputRoutingAndOutputDevices() async throws {
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let devices = sc.videoDevicesPane
        let routing = sc.videoRoutingPane
        for pane in [routing, devices] {
            XCTAssertEqual(pane.displaysButton.title, loc("settings.video.displaysButton"))
            XCTAssertNotNil(pane.displaysButton.superview, "the button is in the page")
            XCTAssertTrue(pane.displaysButton.isDescendant(of: pane.view))
        }
        XCTAssertFalse(sc.videoPane.displaysButton.isDescendant(of: sc.videoPane.view))
        var urls: [URL] = []
        devices.openURL = { urls.append($0) }
        devices.displaysButton.performClick(nil)
        XCTAssertEqual(urls, [VideoSettingsPane.displaysSettings].compactMap { $0 })
    }
}

// MARK: Output route guides (#821; QLab 5 manual pp. 230, 232-233)

extension VideoSettingsTests {
    /// Output 1's and Output 2's default guide colours (given as they were created).
    static let firstGuide: [UInt8] = [0x00, 0xc8, 0xff, 255]
    static let secondGuide: [UInt8] = [0xff, 0x3c, 0xc8, 255]
    static let orange: [UInt8] = [0xff, 0x82, 0x00, 255]

    /// On a 16:9 route filling its display: the top outline, the centre lines' crossing, and a
    /// point clear of every line.
    static let guidePoints: [[Double]] = [[0.5, 0.001], [0.5, 0.5], [0.3, 0.75]]

    /// G4's routes and stage (Output 1 on V1, Output 2 on V2, each a half of Stage 1), nothing
    /// playing and keep-black off: both displays show nothing.
    private func openGuided() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080), (route2, "Output 2", 1280, 720)], width: 3840,
            regions: [(regionA, route1, 0, 1920), (regionB, route2, 1920, 1920)],
            displays: [(route1, "V1"), (route2, "V2")])
        await settled(sc) { sc.videoRoutingPane.guideChecks.count == 2 }
        addTeardownBlock { @MainActor in NSColorPanel.shared.close() }
        return (rig, sc)
    }

    /// Clicks route `n`'s guides box (from 0), as the operator does, and waits for the row to show
    /// what the engine then holds.
    private func clickGuides(_ sc: WorkspaceSettingsWindowController, _ n: Int, line: UInt = #line) async {
        let routing = sc.videoRoutingPane
        let want = routing.guideChecks[n].state != .on
        routing.guideChecks[n].performClick(nil)
        await settled(sc) { routing.shown?[n].guides == want && routing.guideChecks[n].state == (want ? .on : .off) }
    }

    /// Picks `colour` for route `n` as the operator does: a click on its swatch opens the colour
    /// panel on the colour it shows, and the panel's pick goes to the route.
    private func pickGuideColour(
        _ sc: WorkspaceSettingsWindowController, _ n: Int, _ colour: [UInt8], line: UInt = #line
    ) async {
        let routing = sc.videoRoutingPane
        let shown = routing.guideSwatches[n].colour.flatMap(GuideSwatch.hex)
        routing.guideSwatches[n].performClick(nil)
        let panel = NSColorPanel.shared
        XCTAssertTrue(panel.isVisible, "the colour panel is up", line: line)
        XCTAssertEqual(GuideSwatch.hex(panel.color), shown, "on the swatch's colour", line: line)
        panel.color = NSColor(
            srgbRed: CGFloat(colour[0]) / 255, green: CGFloat(colour[1]) / 255, blue: CGFloat(colour[2]) / 255,
            alpha: 1)
        routing.guideColourPicked(panel)
        let hex = String(format: "#%02x%02x%02x", colour[0], colour[1], colour[2])
        await settled(sc) { routing.shown?[n].guideColour == hex }
    }

    /// The colour route `n`'s swatch draws.
    private func swatch(_ sc: WorkspaceSettingsWindowController, _ n: Int) -> [UInt8]? {
        guard let c = sc.videoRoutingPane.guideSwatches[n].colour?.usingColorSpace(.sRGB) else { return nil }
        return [c.redComponent, c.greenComponent, c.blueComponent].map { UInt8(($0 * 255).rounded()) } + [255]
    }

    /// Waits until `display` shows `colours` at the guide points.
    private func showsGuides(
        _ display: String, _ colours: [[UInt8]], _ why: String, line: UInt = #line
    ) async {
        await assertEventually(why, line: line) { await self.displayColours(display, Self.guidePoints) == colours }
    }

    /// #821 GD1: Output 1 on V1, nothing playing → its guides box ticked: V1 shows the outline at
    /// its edge and the centre lines in the colour its swatch shows (black between), and V2 is
    /// untouched; unticked: V1 shows nothing again (its window down: the desktop).
    func testGD1TickingOutput1sGuidesDrawsThemOnItsDisplayOnlyAndUntickingClearsIt() async throws {
        let (rig, sc) = try await openGuided()
        let routing = sc.videoRoutingPane
        XCTAssertEqual(routing.guideChecks.map(\.state), [.off, .off])
        XCTAssertEqual(routing.guideChecks.first?.title, loc("settings.video.guides.show"))
        await showsGuides("V1", [black, black, black], "nothing on V1")
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "Output 1's guides on V1")
        XCTAssertEqual(swatch(sc, 0), Self.firstGuide, "the swatch is the colour drawn")
        await showsGuides("V2", [black, black, black], "V2 untouched")
        XCTAssertEqual(rig.store.videoOutputs.route(ModelID(route1))?.state?.state, "shown")
        await clickGuides(sc, 0)
        await showsGuides("V1", [black, black, black], "cleared")
        await assertEventually("V1's window down") {
            await sc.videoRoutingPane.refresh()
            return rig.store.videoOutputs.route(ModelID(self.route1))?.state?.state == "hidden"
        }
        XCTAssertEqual(routing.messageLabel.stringValue, "")
    }

    /// #821 GD2: Output 2's box alone → only V2 draws guides; Output 1's too → each display its
    /// own in its own colour; Output 2's off → V2 clears and V1 keeps its guides.
    func testGD2EachRoutesGuidesComeAndGoOnItsOwnDisplayInItsOwnColour() async throws {
        let (_, sc) = try await openGuided()
        await clickGuides(sc, 1)
        await showsGuides("V2", [Self.secondGuide, Self.secondGuide, black], "Output 2's on V2")
        await showsGuides("V1", [black, black, black], "V1 untouched")
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "Output 1's on V1")
        await showsGuides("V2", [Self.secondGuide, Self.secondGuide, black], "V2 keeps its own")
        XCTAssertNotEqual(swatch(sc, 0), swatch(sc, 1))
        await clickGuides(sc, 1)
        await showsGuides("V2", [black, black, black], "V2 clears")
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "V1 keeps its guides")
    }

    /// What the name's part of V1 shows: a grid over the top left quarter of a route filling it.
    private func namePattern() async -> [[UInt8]] {
        var grid: [[Double]] = []
        for i in 0..<32 {
            for j in 0..<6 {
                let x: Double = 0.12 + 0.26 * Double(i) / 31
                let y: Double = 0.225 + 0.05 * Double(j) / 5
                grid.append([x, y])
            }
        }
        // A probe reads at most 64 points.
        var colours: [[UInt8]] = []
        for start in stride(from: 0, to: grid.count, by: 64) {
            colours += await displayColours("V1", Array(grid[start..<min(start + 64, grid.count)]))
        }
        return colours
    }

    /// #821 GD3: Output 1 renamed in place while its guides show → the name drawn on V1 changes
    /// at once (its pixels are another pattern), the guides staying up.
    func testGD3RenamingARouteWhileItsGuidesShowChangesTheNameDrawn() async throws {
        let (_, sc) = try await openGuided()
        let routing = sc.videoRoutingPane
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "guides up")
        let before = await namePattern()
        XCTAssertTrue(before.contains(Self.firstGuide), "the name is drawn")
        type("Projector Left", into: routing.nameFields[0], routing)
        await settled(sc) { routing.shown?.first?.name == "Projector Left" }
        await assertEventually("the new name drawn") {
            let now = await self.namePattern()
            return now != before && now.contains(Self.firstGuide)
        }
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "the guides stay")
    }

    /// #821 GD4: Output 1 (1920 x 1080) on a 1280 x 1024 display with guides on: filling it, the
    /// outline's top and bottom lie on the display's edges and its sides are cut off; fitted, the
    /// sides lie on the edges and black bars above and below; turned a quarter in the route editor,
    /// the outline turns with it (its long sides now up the display's left and right).
    func testGD4GuidesShowTheRoutesWholeRasterAsItsScalingAndRotationPlaceIt() async throws {
        engine.stop()
        engine = try await TestEngine.start(displays: "V1:1920x1080,V2:1280x1024")
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920, regions: [(regionA, route1, 0, 1920)],
            displays: [(route1, "V2")])
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.guideChecks.count == 1 }
        await clickGuides(sc, 0)
        let edges: [[Double]] = [[0.3, 0.001], [0.3, 0.999], [0.001, 0.3], [0.999, 0.3]]
        let c = Self.firstGuide
        await assertEventually("fill: top and bottom, sides cut off") {
            await self.displayColours("V2", edges) == [c, c, self.black, self.black]
        }
        routing.editButtons[0].performClick(nil)
        let scalingEditor = try XCTUnwrap(sc.routeEditor)
        scalingEditor.scalingPopup.selectItem(withTitle: loc("settings.video.scaling.fit"))
        scalingEditor.okButton.performClick(nil)
        await scalingEditor.saving?.value
        await settled(sc) { routing.shown?.first?.scaling == "fit" }
        // Fitted: 1280 x 720 in the middle, black bars of 152 rows above and below.
        await assertEventually("fit: the sides on the edges, bars above and below") {
            await self.displayColours("V2", edges + [[0.3, 0.149], [0.3, 0.05]])
                == [self.black, self.black, c, c, c, self.black]
        }
        routing.editButtons[0].performClick(nil)
        let editor = try XCTUnwrap(sc.routeEditor)
        editor.rotationPopup.selectItem(withTitle: "90°")
        editor.okButton.performClick(nil)
        await editor.saving?.value
        await settled(sc) { rig.store.model?.routes.first?.raw["rotation"]?.doubleValue == 90 }
        // Turned and fitted: 576 x 1024 standing in the middle, its outline from x 0.275 to 0.725.
        await assertEventually("the outline turned with the route") {
            await self.displayColours("V2", [[0.276, 0.3], [0.724, 0.3], [0.5, 0.001], [0.1, 0.3]])
                == [c, c, c, self.black]
        }
        XCTAssertEqual(routing.guideChecks.first?.state, .on, "still on")
    }

    /// #821 GD7: the swatch opens the colour panel on its colour; a colour picked there redraws
    /// the guides in it at once and the swatch shows it. Output 1's and 2's defaults differ. A
    /// colour picked while the box is unticked draws nothing, and is the colour when it is ticked.
    func testGD7AColourPickedFromTheSwatchRedrawsTheGuidesAndIsUsedWhenTickedLater() async throws {
        let (_, sc) = try await openGuided()
        XCTAssertEqual(swatch(sc, 0), Self.firstGuide)
        XCTAssertEqual(swatch(sc, 1), Self.secondGuide)
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "in its default colour")
        await pickGuideColour(sc, 0, Self.orange)
        await showsGuides("V1", [Self.orange, Self.orange, black], "redrawn in the colour picked")
        XCTAssertEqual(swatch(sc, 0), Self.orange)
        // Picked while unticked: nothing drawn, then used when ticked.
        let green: [UInt8] = [0x20, 0xe0, 0x40, 255]
        await pickGuideColour(sc, 1, green)
        XCTAssertEqual(swatch(sc, 1), green)
        await showsGuides("V2", [black, black, black], "nothing drawn while unticked")
        await clickGuides(sc, 1)
        await showsGuides("V2", [green, green, black], "drawn in the colour picked before")
    }

    /// The colour panel outlives the settings window: a colour picked there after the window
    /// closed and its pages went changes nothing and crashes nothing.
    func testAColourPickedAfterTheSettingsWindowClosedGoesNowhere() async throws {
        let (rig, sc) = try await openGuided()
        weak var gone: VideoSettingsPane?
        do {
            let page = VideoSettingsPane(store: rig.store, section: .routing)
            gone = page
            await page.refresh()
            page.guideSwatches[0].performClick(nil)
            XCTAssertTrue(GuideColourPick.shared.pane === page)
        }
        await assertEventually("the page went") { gone == nil }
        sc.close()
        let panel = NSColorPanel.shared
        panel.color = NSColor(srgbRed: 1, green: 0, blue: 0, alpha: 1)
        XCTAssertTrue(NSApp.sendAction(#selector(GuideColourPick.picked(_:)), to: GuideColourPick.shared, from: panel))
        await rig.sync()
        XCTAssertEqual(
            rig.store.model?.routes.first?.raw["guide_colour"].flatMap { v -> String? in
                if case .string(let hex) = v { return hex } else { return nil }
            }, "#00c8ff", "nothing changed: still the colour it was given")
    }

    /// #821 GD8: Output 1 renamed (an undo step), then its guides ticked and a colour picked →
    /// neither is an undo step: Edit > Undo still names the rename, and undoing it leaves the
    /// guides showing in the colour picked.
    func testGD8TickingGuidesAndPickingAColourAreNoUndoSteps() async throws {
        let (rig, sc) = try await openGuided()
        let routing = sc.videoRoutingPane
        type("Cyc", into: routing.nameFields[0], routing)
        await settled(sc) { routing.shown?.first?.name == "Cyc" }
        XCTAssertEqual(rig.store.document?.undoName, "rename_route")
        await clickGuides(sc, 0)
        await pickGuideColour(sc, 0, Self.orange)
        await showsGuides("V1", [Self.orange, Self.orange, black], "guides in the colour picked")
        await rig.sync()
        XCTAssertEqual(rig.store.document?.undoName, "rename_route", "Undo still names the rename")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.first?.name == "Output 1" }
        XCTAssertEqual(routing.guideChecks.first?.state, .on)
        XCTAssertEqual(swatch(sc, 0), Self.orange)
        await showsGuides("V1", [Self.orange, Self.orange, black], "the guides stay as they were")
    }

    /// #821 GD9: guides ticked on Output 1 with a colour picked, saved, closed and opened again
    /// → no display draws guides and the box is unticked; the colour was saved with the show and
    /// is the colour when the box is ticked again.
    func testGD9AReopenedShowHasItsGuideColourButNoGuidesShowing() async throws {
        let (rig, sc) = try await openGuided()
        await clickGuides(sc, 0)
        await pickGuideColour(sc, 0, Self.orange)
        await showsGuides("V1", [Self.orange, Self.orange, black], "guides up")
        let path = try XCTUnwrap(rig.store.document?.path)
        guard case .success = await rig.store.save() else { return XCTFail("save") }
        XCTAssertTrue(try String(contentsOfFile: path, encoding: .utf8).contains("\"guide_colour\": \"#ff8200\""))
        sc.close()
        guard case .success = await rig.testEngine.app.send({ .close(id: $0, doc: rig.doc, discardChanges: false) })
        else { return XCTFail("close") }
        await showsGuides("V1", [black, black, black], "gone with the workspace")
        let doc = try await engine.open(path: path)
        let again = Rig(engine: engine, workspace: doc)
        guard await again.connect() else { return XCTFail("connect") }
        let reopened = WorkspaceSettingsWindowController(store: again.store)
        addTeardownBlock { @MainActor in reopened.close() }
        await reopened.present().value
        await settled(reopened) { reopened.videoRoutingPane.guideChecks.count == 2 }
        XCTAssertEqual(reopened.videoRoutingPane.guideChecks.first?.state, .off)
        XCTAssertEqual(swatch(reopened, 0), Self.orange, "the colour travelled with the show")
        await showsGuides("V1", [black, black, black], "no guides")
        await clickGuides(reopened, 0)
        await showsGuides("V1", [Self.orange, Self.orange, black], "drawn in the saved colour")
    }

    /// #821 GD10: guides on Output 1 → the route editor's new size and scaling redraw them for the
    /// new route at once, still on; Cancel changes nothing. Output 1 removed: its guides go with
    /// it, and the undo brings it back with them off.
    func testGD10RouteEditsRedrawTheGuidesAndARemovedRouteComesBackWithThemOff() async throws {
        let (rig, sc) = try await openGuided()
        let routing = sc.videoRoutingPane
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "guides up")
        let beside: [[Double]] = [[0.1, 0.5], [0.5, 0.001]]
        let c = Self.firstGuide
        await assertEventually { await self.displayColours("V1", beside) == [c, c] }
        // Cancel: nothing changes.
        routing.editButtons[0].performClick(nil)
        var editor = try XCTUnwrap(sc.routeEditor)
        editor.widthField.stringValue = "1080"
        editor.cancelButton.performClick(nil)
        await assertEventually("the sheet is down") { sc.routeEditor == nil }
        await assertEventually { await self.displayColours("V1", beside) == [c, c] }
        // A square route, fitted: its sides move in, the centre line no longer reaches x 0.1.
        routing.editButtons[0].performClick(nil)
        editor = try XCTUnwrap(sc.routeEditor)
        editor.widthField.stringValue = "1080"
        editor.heightField.stringValue = "1080"
        editor.scalingPopup.selectItem(withTitle: loc("settings.video.scaling.fit"))
        editor.okButton.performClick(nil)
        await editor.saving?.value
        await settled(sc) { routing.shown?.first?.width == 1080 }
        await assertEventually("redrawn for the square route") {
            await self.displayColours("V1", beside + [[0.2193, 0.3]]) == [self.black, c, c]
        }
        XCTAssertEqual(routing.guideChecks.first?.state, .on, "still on")
        // Removed: the guides go; the undo brings the route back without them.
        routing.removeButtons[0].performClick(nil)
        await settled(sc) { routing.shown?.count == 1 }
        await showsGuides("V1", [black, black, black], "gone with the route")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.count == 2 }
        XCTAssertEqual(routing.guideChecks.first?.state, .off, "back with its guides off")
        await showsGuides("V1", [black, black, black], "none drawn")
        // Change its display and press OK: the guides, ticked again, draw on V2 in Output 1's own
        // colour and V1 shows none; the box stays ticked.
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "back on V1")
        routing.editButtons[0].performClick(nil)
        editor = try XCTUnwrap(sc.routeEditor)
        choose("Virtual V2 (1280 × 720)", in: editor.displayPopup)
        editor.okButton.performClick(nil)
        await editor.saving?.value
        await settled(sc) { routing.shown?.first?.display == "V2" }
        await showsGuides("V2", [Self.firstGuide, Self.firstGuide, black], "now on V2")
        await showsGuides("V1", [black, black, black], "none left on V1")
        XCTAssertEqual(routing.guideChecks.first?.state, .on, "the box stays ticked")
    }

    /// #821 GD2 (a route's colour is its own): removing Output 1 leaves Output 2's swatch and the
    /// colour its guides are drawn in as they were, and the undo brings Output 1 back in its own.
    func testGD2RemovingOutput1LeavesOutput2sSwatchAndGuideColourAsTheyWere() async throws {
        let (rig, sc) = try await openGuided()
        let routing = sc.videoRoutingPane
        XCTAssertEqual([swatch(sc, 0), swatch(sc, 1)], [Self.firstGuide, Self.secondGuide])
        routing.removeButtons[0].performClick(nil)
        await settled(sc) { routing.shown?.count == 1 && routing.guideSwatches.count == 1 }
        XCTAssertEqual(swatch(sc, 0), Self.secondGuide, "Output 2 keeps its colour")
        await clickGuides(sc, 0)
        await showsGuides("V2", [Self.secondGuide, Self.secondGuide, black], "drawn in Output 2's colour")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.count == 2 && routing.guideSwatches.count == 2 }
        await assertEventually("Output 1 back in its own colour") {
            [self.swatch(sc, 0), self.swatch(sc, 1)] == [Self.firstGuide, Self.secondGuide]
        }
    }

    /// A pop-up that is open while another client removes the route above it is torn down with its
    /// row, but AppKit still delivers the choice from it: the choice goes to the route the pop-up
    /// was drawn for, never to the one that now sits at its old place in the list.
    func testAChoiceFromARemovedRowsPopUpGoesToItsOwnRoute() async throws {
        let (rig, sc) = try await openTwoStages()
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.shown?.count == 3 && routing.layoutPopups.count == 3 }
        let route3 = "00000000-0000-0000-0000-00000000b003"
        // Output 2's pop-ups, held as an open menu would hold them.
        let display = routing.displayPopups[1]
        let layout = routing.layoutPopups[1]
        XCTAssertEqual(routing.shown?.first { $0.id.raw == route2 }?.display, "V2")
        // Another client removes Output 1: Output 3 now sits where Output 2 did.
        guard case .success = await rig.store.send(command: .removeRoute(ModelID(route1))) else {
            return XCTFail("remove")
        }
        await settled(sc) { routing.shown?.map(\.id.raw) == [self.route2, route3] }
        // The choices are made from the old pop-ups.
        display.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = display.sendAction(display.action, to: display.target)
        await settled(sc) { routing.shown?.first?.display == "V1" }
        try pickLayout(layout, "2 wide", part: 2)
        await settled(sc) { routing.shown?.first?.partial?.index == 1 }
        let after = routing.shown ?? []
        XCTAssertEqual(after.map(\.id.raw), [route2, route3])
        XCTAssertNotEqual(after[1].display, "V1", "Output 3 keeps its own display")
        XCTAssertNil(after[1].partial, "Output 3 keeps its own layout")
    }

    /// The stage editor's region route pop-up, open while another client removes the region above
    /// it, delivers its choice to the region it was drawn for, not the one now at its old place.
    func testAChoiceFromARemovedRegionsRoutePopUpGoesToItsOwnRegion() async throws {
        let (rig, sc) = try await openTwoStages()
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { editor.regionRoutePopups.count == 3 }
        let regionC = editor.regions[2].id
        let popup = editor.regionRoutePopups[1]
        guard case .success = await rig.store.send(command: .removeRegion(stage: ModelID(stageA), ModelID(regionA)))
        else { return XCTFail("remove") }
        await editorSettled(sc, editor) { editor.regions.map(\.id.raw) == [self.regionB, regionC.raw] }
        // Region B's pop-up, from before: Output 1.
        popup.selectItem(withTitle: "Output 1 (Virtual V1)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await editorSettled(sc, editor) { editor.regions.first?.route == ModelID(self.route1) }
        XCTAssertNil(editor.regions[1].route, "region C keeps its own route (none)")
    }

    /// Text typed in a region field (the caret still in it) belongs to the region it was drawn for:
    /// when that region goes (an undo of its adding, or another client removing it) and the
    /// selection falls back to another region, committing the field changes no region.
    func testTextTypedForARegionThatGoesIsNotAppliedToTheOneSelectedAfterIt() async throws {
        for byUndo in [true, false] {
            let (rig, sc) = try await openG4()
            let editor = sc.openStageEditor(ModelID(stageA))
            choose(loc("stageEditor.newRegionNone"), in: editor.newRegionPopup)
            await editorSettled(sc, editor) { editor.regionChips.count == 3 && editor.selected == editor.regions[2].id }
            let added = editor.regions[2].id
            let before = editor.regions.map(\.bounds)
            XCTAssertTrue(editor.window?.makeFirstResponder(editor.regionWidthField) == true)
            editor.regionWidthField.currentEditor()?.string = "500"
            if byUndo {
                _ = await rig.store.undo()
            } else {
                _ = await rig.store.send(command: .removeRegion(stage: ModelID(stageA), added))
            }
            await editorSettled(sc, editor) { editor.regionChips.count == 2 }
            _ = editor.window?.makeFirstResponder(nil)
            await editor.editing?.value
            XCTAssertEqual(editor.regions.map(\.bounds), Array(before.prefix(2)), "undo: \(byUndo)")
            XCTAssertEqual(editor.regionWidthField.stringValue, "1920", "the text typed for the gone region is dropped")
        }
    }

    /// A region field typed in and left by clicking another region reaches the region it was typed
    /// for, not the one clicked.
    func testTextTypedInARegionFieldReachesThatRegionWhenAnotherIsClicked() async throws {
        let (_, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { editor.regionChips.count == 2 }
        XCTAssertEqual(editor.selected, editor.regions[0].id)
        XCTAssertTrue(editor.window?.makeFirstResponder(editor.regionWidthField) == true)
        editor.regionWidthField.currentEditor()?.string = "500"
        editor.regionChips[1].mouseDown(with: try press(editor))
        await editorSettled(sc, editor) { editor.regions[0].bounds.width == 500 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 1920, y: 0, width: 1920, height: 1080))
        XCTAssertEqual(editor.selected, editor.regions[1].id)
    }

    /// A route dragged by its handle is the route the handle was drawn for: when another client
    /// removes a route above it during the drag, the drop moves that route, not the one that has
    /// taken its place in the list.
    func testADroppedHandleMovesItsOwnRouteWhenARowAboveItGoesDuringTheDrag() async throws {
        let (rig, sc) = try await openTwoStages()
        let routing = sc.videoRoutingPane
        sc.tabs.selectTabViewItem(withIdentifier: "videoRouting")
        await settled(sc) { routing.handles.count == 3 }
        let route3 = "00000000-0000-0000-0000-00000000b003"
        let handle = routing.handles[1]  // Output 2
        guard case .success = await rig.store.send(command: .removeRoute(ModelID(route1))) else {
            return XCTFail("remove")
        }
        await settled(sc) { routing.shown?.map(\.id.raw) == [self.route2, route3] }
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        // Let go below the last row.
        let below = routing.holder.convert(NSPoint(x: 40, y: routing.holder.bounds.maxY), to: nil)
        handle.onDrop?(handle, below)
        await settled(sc) { routing.shown?.map(\.id.raw) == [route3, self.route2] }
    }

    /// The region list takes the views of a row it drops out of the window, and a remove button held
    /// across a redraw (a region above it went) removes the region it was drawn for. (A real held
    /// press then fires no action, the rebuilt list having taken the button out of the window; the
    /// click here is the action as AppKit would still deliver it.)
    func testARemoveButtonFromBeforeARedrawRemovesItsOwnRegionAndNoOldViewsStay() async throws {
        let (rig, sc) = try await openTwoStages()
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { editor.regionRemoveButtons.count == 3 }
        let regionC = editor.regions[2].id
        let held = editor.regionRemoveButtons[1]  // region B
        let removed = await rig.store.send(command: .removeRegion(stage: ModelID(stageA), ModelID(regionA)))
        guard case .success = removed else { return XCTFail("remove") }
        await editorSettled(sc, editor) { editor.regionChips.count == 2 }
        XCTAssertEqual(
            editor.regionList.arrangedSubviews.count, editor.regionRows.count, "no views of dropped rows are left")
        XCTAssertNil(held.superview, "the old button is out of the window")
        held.performClick(nil)
        await editorSettled(sc, editor) { editor.regionChips.count == 1 }
        XCTAssertEqual(editor.regions.map(\.id), [regionC], "region B went, C stayed")
    }

    /// The GD11 workspace: Stage 1 (G4) with regions A on Output 1 (V1), B on Output 2 (V2) and C on
    /// none; Stage 2 with one region on Output 3 (no display), used by no other stage.
    private func openTwoStages() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let route3 = "00000000-0000-0000-0000-00000000b003"
        let (stage1, stage2) = (stageA, stageB)
        let (rA, rB) = (regionA, regionB)
        let (o1, o2) = (route1, route2)
        let rig = try await Rig.open("Demo", on: engine) { folder in
            let file = folder.appendingPathComponent("Demo.openlab")
            var root = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
            var ws = try XCTUnwrap(root["workspace"] as? [String: Any])
            ws["routes"] = [(o1, "Output 1"), (o2, "Output 2"), (route3, "Output 3")].map {
                ["id": $0.0, "name": $0.1, "width": 1920, "height": 1080] as [String: Any]
            }
            let region = { (id: String, route: String?, x: Int) -> [String: Any] in
                var d: [String: Any] = ["id": id, "x": x, "y": 0, "width": 1280, "height": 1080]
                if let route { d["route"] = route }
                return d
            }
            ws["stages"] = [
                [
                    "id": stage1, "name": "Stage 1", "width": 3840, "height": 1080, "keep_black": false,
                    "regions": [
                        region(rA, o1, 0), region(rB, o2, 1280),
                        region("00000000-0000-0000-0000-00000000c003", nil, 2560),
                    ],
                ] as [String: Any],
                [
                    "id": stage2, "name": "Stage 2", "width": 1920, "height": 1080, "keep_black": false,
                    "regions": [
                        [
                            "id": "00000000-0000-0000-0000-00000000c004", "route": route3, "x": 0, "y": 0,
                            "width": 1920, "height": 1080,
                        ]
                    ],
                ] as [String: Any],
            ]
            root["workspace"] = ws
            try JSONSerialization.data(withJSONObject: root).write(to: file)
        }
        await displaysListed()
        for (route, display) in [(route1, "V1"), (route2, "V2")] {
            guard case .success = await rig.store.setRouteDisplay(ModelID(route), display: display) else {
                XCTFail("display \(display) for \(route)")
                break
            }
        }
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        await settled(sc) { sc.videoRoutingPane.guideChecks.count == 3 }
        return (rig, sc)
    }

    /// #821 GD11: Stage 1 in its editor (regions on Output 1 and Output 2, one on none) → its guides
    /// box ticked: V1 and V2 draw their routes' guides, and both routes' boxes on Output Routing
    /// are ticked; Output 3, used only by Stage 2, is untouched; nothing is refused. Unticked:
    /// both clear.
    func testGD11TheStageEditorsGuidesBoxShowsTheGuidesOfEveryRouteTheStageUses() async throws {
        let (_, sc) = try await openTwoStages()
        let routing = sc.videoRoutingPane
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { editor.guidesCheck.isEnabled }
        XCTAssertEqual(editor.guidesCheck.state, .off)
        editor.guidesCheck.performClick(nil)
        await editorSettled(sc, editor) { editor.guidesCheck.state == .on }
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "Output 1's on V1")
        await showsGuides("V2", [Self.secondGuide, Self.secondGuide, black], "Output 2's on V2")
        await settled(sc) { routing.guideChecks.map(\.state) == [.on, .on, .off] }
        XCTAssertEqual(editor.messageLabel.stringValue, "")
        editor.guidesCheck.performClick(nil)
        await editorSettled(sc, editor) { editor.guidesCheck.state == .off }
        await showsGuides("V1", [black, black, black], "V1 clears")
        await showsGuides("V2", [black, black, black], "V2 clears")
        await settled(sc) { routing.guideChecks.map(\.state) == [.off, .off, .off] }
    }

    /// #821 GD12: Output 1's box ticked by hand on Output Routing, Output 2's not, Stage 1 using
    /// both → the stage's box is mixed; a click ticks Output 2 and leaves Output 1 (all on); a
    /// click again clears both.
    func testGD12AStagesGuidesBoxIsMixedWhenSomeOfItsRoutesShowThem() async throws {
        let (_, sc) = try await openTwoStages()
        let routing = sc.videoRoutingPane
        let editor = sc.openStageEditor(ModelID(stageA))
        await clickGuides(sc, 0)
        await editorSettled(sc, editor) { editor.guidesCheck.state == .mixed }
        editor.guidesCheck.performClick(nil)
        await editorSettled(sc, editor) { editor.guidesCheck.state == .on }
        await settled(sc) { routing.guideChecks.map(\.state) == [.on, .on, .off] }
        await showsGuides("V2", [Self.secondGuide, Self.secondGuide, black], "Output 2's on V2")
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "Output 1's stay")
        editor.guidesCheck.performClick(nil)
        await editorSettled(sc, editor) { editor.guidesCheck.state == .off }
        await settled(sc) { routing.guideChecks.map(\.state) == [.off, .off, .off] }
        await showsGuides("V1", [black, black, black], "all cleared")
    }

    /// #821 GD13 (the app's part; the display coming back is the engine's test of the same name):
    /// a route whose display this Mac lacks now → its box ticks with no alert or message, its row
    /// still says the display is not connected, and nothing is drawn anywhere.
    func testGD13GuidesTickedOnARouteWhoseDisplayIsMissingTickWithoutAnAlert() async throws {
        let settings: [String: Any] = [
            "settings_version": 1,
            "workspaces": [
                "00000000-0000-0000-0000-0000000000d1": [
                    "patches": [String: Any](),
                    "routes": [stageA: ["display": "GONE", "name": "Projector", "width": 1920, "height": 1080]],
                ]
            ],
        ]
        engine.stop()
        engine = try await TestEngine.start(
            displays: Self.displays, settings: try JSONSerialization.data(withJSONObject: settings))
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.shown?.first?.warning == loc("settings.video.warn.missing") }
        await clickGuides(sc, 0)
        XCTAssertEqual(routing.guideChecks.first?.state, .on)
        XCTAssertEqual(routing.warningLabels.map(\.stringValue), [loc("settings.video.warn.missing")])
        XCTAssertEqual(routing.messageLabel.stringValue, "")
        XCTAssertNil(sc.window?.attachedSheet)
        await showsGuides("V1", [black, black, black], "nothing drawn on V1")
        await showsGuides("V2", [black, black, black], "nor on V2")
    }

    /// #821 GD15: guides showing on V1 and the workspace closed → V1 shows nothing again; the
    /// workspace opened again starts with none.
    func testGD15ClosingTheWorkspaceTakesItsGuidesOffTheDisplay() async throws {
        let (rig, sc) = try await openGuided()
        await clickGuides(sc, 0)
        await showsGuides("V1", [Self.firstGuide, Self.firstGuide, black], "guides up")
        let path = try XCTUnwrap(rig.store.document?.path)
        sc.close()
        guard case .success = await rig.testEngine.app.send({ .close(id: $0, doc: rig.doc, discardChanges: true) })
        else { return XCTFail("close") }
        await showsGuides("V1", [black, black, black], "gone with the workspace")
        _ = try await engine.open(path: path)
        await showsGuides("V1", [black, black, black], "opened again: none")
    }
}

// MARK: Route layout (#842 V3; QLab 5 manual pp. 232-234: a route can use one part of a display)

extension VideoSettingsTests {
    /// The leaf "Part `part`" of the layout `name` ("2 wide") in a route's layout pop-up.
    private func layoutLeaf(_ popup: NSPopUpButton, _ name: String, part: Int) throws -> NSMenuItem {
        let group = try XCTUnwrap(
            popup.menu?.items.first { $0.title == name && $0.hasSubmenu }, "no layout \"\(name)\"")
        return try XCTUnwrap(
            group.submenu?.items.first { $0.title == String(format: loc("settings.video.layout.part"), part) })
    }

    /// Picks the layout `name`, part `part`, as the operator does from the pop-up's menu.
    func pickLayout(_ popup: NSPopUpButton, _ name: String, part: Int) throws {
        let item = try layoutLeaf(popup, name, part: part)
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(item.action), to: item.target, from: item))
    }

    private func openOneRouteWithTheCard() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920, regions: [(regionA, route1, 0, 1920)],
            displays: [(route1, "V1")])
        guard
            case .success = await rig.client.send({
                .request(id: $0, .stageTestCard(stage: ModelID(self.stageA), frame: 5))
            })
        else {
            XCTFail("test card")
            return (rig, sc)
        }
        await settled(sc) { sc.videoRoutingPane.layoutPopups.count == 1 }
        return (rig, sc)
    }

    /// V3: Output Routing → each route's row has a Layout column where Scaling was, its pop-up
    /// reading Full and offering the layouts QLab 5 does (2, 3 and 4 wide and high, and the grids by
    /// their rows, each with its parts); the scaling is in the route's editor.
    func testV3TheRoutingTableHasALayoutColumnWhereScalingWasWithQLabsChoices() async throws {
        let (_, sc) = try await openOneRouteWithTheCard()
        let routing = sc.videoRoutingPane
        XCTAssertFalse(routing.cols.contains { $0.key == "scaling" }, "no Scaling column")
        let at = try XCTUnwrap(routing.cols.firstIndex(of: .layout))
        XCTAssertEqual(routing.cols[at - 1], .display, "beside the display, where Scaling was")
        let title = routing.header.cell(atColumnIndex: at, rowIndex: 0).contentView as? NSTextField
        XCTAssertEqual(title?.stringValue, loc("settings.video.col.layout"))
        let popup = try XCTUnwrap(routing.layoutPopups.first)
        XCTAssertTrue(popup.isEnabled)
        XCTAssertEqual(popup.title, loc("settings.video.layout.full"))
        let items = popup.menu?.items ?? []
        XCTAssertEqual(
            items.dropFirst().filter { !$0.isSeparatorItem }.map(\.title),
            [
                "Full", "Horizontal", "2 wide", "3 wide", "4 wide", "Vertical", "2 high", "3 high", "4 high", "2 rows",
                "2 × 2", "3 × 2", "4 × 2", "3 rows", "2 × 3", "3 × 3", "4 × 3", "4 rows", "2 × 4", "3 × 4", "4 × 4",
            ])
        let grid = try XCTUnwrap(items.first { $0.title == "3 × 2" }?.submenu)
        XCTAssertEqual(grid.items.map(\.title), (1...6).map { "Part \($0)" })
        XCTAssertEqual(items.first { $0.title == "Horizontal" }?.isEnabled, false, "a heading")
        // The scaling is in the editor, as QLab keeps it.
        routing.editButtons[0].performClick(nil)
        XCTAssertEqual(sc.routeEditor?.scalingPopup.itemTitles.contains(loc("settings.video.scaling.stretch")), true)
    }

    /// V3: Output 1 on V1 showing the card → pick "2 wide", part 2: the pop-up reads that, the
    /// card fills the right half of V1 and the left half is black; part 1: the other way round;
    /// Full: the whole display again. The choice is this Mac's, not an undo step.
    func testV3PickingAPartShowsTheRouteOnThatPartOfItsDisplayOnly() async throws {
        let (rig, sc) = try await openOneRouteWithTheCard()
        let routing = sc.videoRoutingPane
        let popup = try XCTUnwrap(routing.layoutPopups.first)
        let before = await displayColours("V1", [[0.25, 0.8], [0.75, 0.8]])
        XCTAssertEqual(before, [[0, 0, 255, 255], [255, 255, 0, 255]], "the card across the whole display")
        let undoName = rig.store.document?.undoName
        try pickLayout(popup, "2 wide", part: 2)
        await settled(sc) { routing.shown?.first?.partial == DisplayPart(columns: 2, rows: 1, index: 1) }
        XCTAssertEqual(routing.layoutPopups.first?.title, "2 wide · 2")
        await assertEventually("the card on the right half of V1, black on the left") {
            await self.displayColours("V1", [[0.25, 0.8], [0.625, 0.8], [0.875, 0.8], [0.49, 0.5]])
                == [self.black, [0, 0, 255, 255], [255, 255, 0, 255], self.black]
        }
        XCTAssertEqual(rig.store.document?.undoName, undoName, "no undo step")
        let tick = try layoutLeaf(try XCTUnwrap(routing.layoutPopups.first), "2 wide", part: 2)
        XCTAssertEqual(tick.state, .on, "the chosen part is ticked")
        try pickLayout(try XCTUnwrap(routing.layoutPopups.first), "2 wide", part: 1)
        await settled(sc) { routing.shown?.first?.partial?.index == 0 }
        await assertEventually("the card on the left half") {
            await self.displayColours("V1", [[0.125, 0.8], [0.375, 0.8], [0.75, 0.8]])
                == [[0, 0, 255, 255], [255, 255, 0, 255], self.black]
        }
        let full = try XCTUnwrap(routing.layoutPopups.first?.menu?.items.first { $0.title == "Full" })
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(full.action), to: full.target, from: full))
        await settled(sc) { routing.shown?.first?.partial == nil }
        XCTAssertEqual(routing.layoutPopups.first?.title, loc("settings.video.layout.full"))
        await assertEventually("the whole display again") {
            await self.displayColours("V1", [[0.25, 0.8], [0.75, 0.8]]) == before
        }
    }

    /// V3: the part is kept with this Mac's display choice: choosing another display puts the
    /// route on the whole of it, and a route with no display has nothing to lay out (its pop-up is
    /// off).
    func testV3AnotherDisplayTakesTheWholeOfItAndNoDisplayLeavesTheLayoutOff() async throws {
        let (rig, sc) = try await openOneRouteWithTheCard()
        let routing = sc.videoRoutingPane
        try pickLayout(try XCTUnwrap(routing.layoutPopups.first), "3 wide", part: 3)
        await settled(sc) { routing.shown?.first?.partial?.index == 2 }
        choose("Virtual V2 (1280 × 720)", in: routing.displayPopups[0])
        await settled(sc) { routing.shown?.first?.display == "V2" && routing.shown?.first?.partial == nil }
        XCTAssertEqual(routing.layoutPopups.first?.title, loc("settings.video.layout.full"))
        choose(loc("settings.video.displayNone"), in: routing.displayPopups[0])
        await settled(sc) { routing.shown?.first?.display == nil }
        XCTAssertEqual(routing.layoutPopups.first?.isEnabled, false)
        XCTAssertEqual(rig.store.videoOutputs.routes.first?.mapping?.partial, nil)
    }

    /// V3: a route on a part of a display → its editor's Set from Device takes the part's size
    /// (QLab 5 manual p. 234), and the device size line says it.
    func testV3SetFromDeviceTakesThePartsSize() async throws {
        let (_, sc) = try await openOneRouteWithTheCard()
        let routing = sc.videoRoutingPane
        try pickLayout(try XCTUnwrap(routing.layoutPopups.first), "2 high", part: 2)
        await settled(sc) { routing.shown?.first?.partial == DisplayPart(columns: 1, rows: 2, index: 1) }
        routing.editButtons[0].performClick(nil)
        let editor = try XCTUnwrap(sc.routeEditor)
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "1920 × 540")
        editor.fromDisplayButton.performClick(nil)
        XCTAssertEqual(editor.widthField.stringValue, "1920")
        XCTAssertEqual(editor.heightField.stringValue, "540")
    }
}

// MARK: #860: Video settings as QLab 5's (UI map `*.demo` surfaces), one test per flow

extension VideoSettingsTests {
    private var stageC: String { "00000000-0000-0000-0000-00000000a003" }

    /// Three stages "Stage 1" to "Stage 3", each on an output route of its own; Stage 1's route on V1.
    private func openThree() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let (rig, sc) = try await open(stages: [
            (stageA, "Stage 1", 1920, 1080), (stageB, "Stage 2", 1920, 1080), (stageC, "Stage 3", 1280, 720),
        ])
        await displaysListed()
        _ = await rig.store.setRouteDisplay(ModelID(stageA), display: "V1")
        await settled(sc) { sc.videoPane.deviceCells.first?.stringValue == "Virtual V1" }
        return (rig, sc)
    }

    /// The frame of `view` in `window`'s content view.
    private func frame(_ view: NSView, in window: NSWindow) -> NSRect {
        view.convert(view.bounds, to: window.contentView)
    }

    /// The view under `view` whose accessibility identifier is `id`.
    private func descendant(of view: NSView, id: String) -> NSView? {
        if view.accessibilityIdentifier() == id { return view }
        for sub in view.subviews {
            if let found = descendant(of: sub, id: id) { return found }
        }
        return nil
    }

    /// A1: three stages → each row has a handle, a stage icon, the name in an editable field, the
    /// size as "W × H", where it goes, Edit, Monitor and remove at the right, in that order; the
    /// titles above name the name, resolution and destinations columns.
    func testVideoOutputs_rowLayoutHasAllControls() async throws {
        let (_, sc) = try await openThree()
        let pane = sc.videoPane
        let window = try XCTUnwrap(sc.window)
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(pane.cols, [.handle, .name, .size, .devices, .edit, .monitor, .remove])
        XCTAssertEqual(
            pane.cols.map { loc("settings.video.col.\($0.key)") }, ["", "Name", "Resolution", "Destinations", "", "", ""])
        XCTAssertEqual(pane.handles.count, 3)
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1", "Stage 2", "Stage 3"])
        XCTAssertTrue(pane.nameFields.allSatisfy(\.isEditable))
        XCTAssertEqual(pane.sizeLabels.map(\.stringValue), ["1920 × 1080", "1920 × 1080", "1280 × 720"])
        XCTAssertEqual(pane.deviceCells.first?.stringValue, "Virtual V1")
        XCTAssertEqual(pane.editButtons.map(\.title), Array(repeating: loc("settings.video.edit"), count: 3))
        XCTAssertEqual(pane.monitorButtons.map(\.title), Array(repeating: loc("settings.video.monitor"), count: 3))
        XCTAssertEqual(pane.removeButtons.count, 3)
        for i in 0..<3 {
            let icon = try XCTUnwrap(descendant(of: pane.view, id: "settings-video-icon-\(i + 1)") as? NSImageView)
            XCTAssertNotNil(icon.image, "a stage icon")
            let row: [NSView] = [
                pane.handles[i], icon, pane.nameFields[i], pane.sizeLabels[i], pane.deviceCells[i],
                pane.editButtons[i], pane.monitorButtons[i], pane.removeButtons[i],
            ]
            let xs = row.map { frame($0, in: window).minX }
            XCTAssertEqual(xs, xs.sorted(), "row \(i + 1) left to right")
            XCTAssertEqual(
                frame(pane.handles[i], in: window).midY, frame(pane.nameFields[i], in: window).midY, accuracy: 2)
        }
    }

    /// A2: a stage none of whose regions goes to a route says it goes nowhere in its destinations
    /// column (no warning line); one whose region goes to a route names that route's display; one
    /// on a route with no display here names the route as having none and warns.
    func testVideoOutputs_noRouteShowsNoDestinations() async throws {
        let (rig, sc) = try await openThree()
        let pane = sc.videoPane
        guard
            case .success = await rig.store.send(
                command: .setRegionRoute(stage: ModelID(stageB), ModelID(stageB), route: nil))
        else { return XCTFail("route none") }
        await settled(sc) {
            pane.deviceCells.count == 3 && pane.deviceCells[1].stringValue == loc("settings.video.noDestinations")
        }
        XCTAssertEqual(pane.deviceCells[0].stringValue, "Virtual V1")
        XCTAssertEqual(pane.deviceCells[2].stringValue, String(format: loc("video.route.noDisplayNamed"), "Output 3"))
        XCTAssertEqual(
            pane.warningLabels.map(\.stringValue), ["", "", loc("settings.video.warn.stageNoDisplay")],
            "only the stage whose routes show nowhere here warns")
    }

    /// A5: the add-stage pop-up → its title names Command-N; an empty stage, then under a heading a
    /// stage on each route named with its display; nothing for partial or multiple outputs.
    func testVideoOutputs_addPopupItems() async throws {
        let (_, sc) = try await openThree()
        let pane = sc.videoPane
        await settled(sc) { self.titles(pane.addPopup).count == 6 }
        XCTAssertEqual(
            titles(pane.addPopup),
            [
                loc("settings.video.add.key"), loc("settings.video.addEmpty"), loc("settings.video.addOnRouteHeader"),
                "   Output 1 (Virtual V1)", "   Output 2 (no display)", "   Output 3 (no display)",
            ])
        XCTAssertTrue(pane.addPopup.title.hasSuffix("⌘N"), pane.addPopup.title)
        let heading = pane.addPopup.menu?.items.first { $0.title == loc("settings.video.addOnRouteHeader") }
        XCTAssertEqual(heading?.isEnabled, false, "the heading is not an item to pick")
    }

    /// A6: the empty stage picked → a row "Stage 4", 1920 x 1080, going nowhere, selected, and its
    /// editor opens by itself with no region, nothing selected and a dash in every region field.
    func testVideoOutputs_emptyStageOpensEditor() async throws {
        let (rig, sc) = try await openThree()
        let pane = sc.videoPane
        choose(loc("settings.video.addEmpty"), in: pane.addPopup)
        await settled(sc) { pane.shown?.count == 4 }
        XCTAssertEqual(pane.nameFields.last?.stringValue, "Stage 4")
        XCTAssertEqual(pane.sizeLabels.last?.stringValue, "1920 × 1080")
        XCTAssertEqual(pane.deviceCells.last?.stringValue, loc("settings.video.noDestinations"))
        let id = try XCTUnwrap(pane.shown?.last?.id)
        XCTAssertEqual(pane.selectedRow, id, "the new row is selected")
        XCTAssertFalse(pane.selectionBox.isHidden)
        let editor = try XCTUnwrap(sc.stageEditors[id], "its editor opened by itself")
        XCTAssertTrue(sc.stageEditor === editor)
        XCTAssertEqual(editor.window?.isVisible, true)
        await editorSettled(sc, editor) { editor.window?.title == String(format: loc("stageEditor.title"), "Stage 4") }
        XCTAssertEqual(editor.regionRows.count, 0)
        XCTAssertEqual(editor.regionHeading.stringValue, loc("stageEditor.noRegion"))
        for f in [editor.regionWidthField, editor.regionHeightField, editor.lowerLeftXField, editor.lowerLeftYField] {
            XCTAssertEqual(f.stringValue, StageEditorWindowController.dash)
            XCTAssertFalse(f.isEnabled)
        }
        XCTAssertEqual(
            [editor.upperRightXLabel.stringValue, editor.upperRightYLabel.stringValue],
            [StageEditorWindowController.dash, StageEditorWindowController.dash])
        XCTAssertEqual(rig.store.model?.stages.last?.raw["layer"]?.doubleValue, 1000, "a new stage is on top")
        XCTAssertEqual(rig.store.document?.undoName, "add_stage")
    }

    /// A7: the stage on Output 1 picked → a row sized as Output 1, selected, naming its display; no
    /// editor opens; its editor opened later shows one region A on Output 1 covering the stage.
    func testVideoOutputs_stageOnRouteDoesNotOpenEditor() async throws {
        let (_, sc) = try await openThree()
        let pane = sc.videoPane
        choose("   Output 1 (Virtual V1)", in: pane.addPopup)
        await settled(sc) { pane.shown?.count == 4 && pane.deviceCells.last?.stringValue == "Virtual V1" }
        let id = try XCTUnwrap(pane.shown?.last?.id)
        XCTAssertEqual(pane.sizeLabels.last?.stringValue, "1920 × 1080")
        XCTAssertEqual(pane.selectedRow, id)
        XCTAssertNil(sc.stageEditors[id], "no editor opens")
        let editor = sc.openStageEditor(id)
        await editorSettled(sc, editor) { editor.regionRows.count == 1 }
        XCTAssertEqual(editor.regionLetters.map(\.stringValue), ["A"])
        XCTAssertEqual(editor.regionRoutePopups[0].titleOfSelectedItem, "Output 1 (Virtual V1)")
        XCTAssertEqual(editor.regions[0].bounds, RegionBounds(x: 0, y: 0, width: 1920, height: 1080))
    }

    /// A8: Command-N on Video Outputs (nothing being typed) opens its add pop-up; on Output Routing
    /// that page's; on another page it goes on to the menu.
    func testVideoOutputs_commandNOpensAddPopup() async throws {
        let (_, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        var opened: [NSPopUpButton] = []
        sc.videoPane.openAddMenu = { opened.append($0) }
        sc.videoRoutingPane.openAddMenu = { opened.append($0) }
        let window = try XCTUnwrap(sc.window)
        let key = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: .command, timestamp: 0,
                windowNumber: window.windowNumber, context: nil, characters: "n", charactersIgnoringModifiers: "n",
                isARepeat: false, keyCode: 45))
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        XCTAssertTrue(window.contentView?.performKeyEquivalent(with: key) == true)
        XCTAssertTrue(opened.last === sc.videoPane.addPopup)
        sc.tabs.selectTabViewItem(withIdentifier: "videoRouting")
        XCTAssertTrue(window.contentView?.performKeyEquivalent(with: key) == true)
        XCTAssertTrue(opened.last === sc.videoRoutingPane.addPopup)
        XCTAssertTrue(sc.videoRoutingPane.addPopup.title.hasSuffix("⌘N"))
        sc.tabs.selectTabViewItem(withIdentifier: "general")
        XCTAssertFalse(window.contentView?.performKeyEquivalent(with: key) == true)
        XCTAssertEqual(opened.count, 2)
    }

    /// A9: a row's Edit → that stage's editor window opens; a second Edit brings the same window
    /// forward (no second one); Monitor opens the stage's monitor window.
    func testVideoOutputs_editAndMonitorButtons() async throws {
        let (rig, sc) = try await openThree()
        let pane = sc.videoPane
        pane.editButtons[1].performClick(nil)
        let editor = try XCTUnwrap(sc.stageEditors[ModelID(stageB)])
        XCTAssertEqual(editor.window?.isVisible, true)
        XCTAssertEqual(editor.window?.title, String(format: loc("stageEditor.title"), "Stage 2"))
        XCTAssertEqual(pane.selectedRow, ModelID(stageB))
        pane.editButtons[1].performClick(nil)
        XCTAssertTrue(sc.stageEditors[ModelID(stageB)] === editor, "the same window")
        XCTAssertEqual(sc.stageEditors.count, 1)
        pane.monitorButtons[1].performClick(nil)
        await settled(sc) {
            rig.store.videoOutputs.stages.first { $0.stage == ModelID(self.stageB) }?.monitor == true
        }
    }

    /// A11: Show Mode on → the list, the add pop-up, a reorder, a rename and a remove all work.
    func testVideoOutputs_editableInShowMode() async throws {
        let (rig, sc) = try await openThree()
        let pane = sc.videoPane
        _ = await rig.store.setShowMode(true).value
        await settled(sc) { rig.store.showMode }
        XCTAssertTrue(pane.addPopup.isEnabled)
        XCTAssertTrue(pane.nameFields.allSatisfy(\.isEditable))
        XCTAssertTrue(pane.handles.allSatisfy(\.isEnabled))
        choose("   Output 2 (no display)", in: pane.addPopup)
        await settled(sc) { pane.shown?.count == 4 }
        pane.move(from: 3, toGap: 0)
        await settled(sc) { pane.shown?.first?.name == "Stage 4" }
        type("Front", into: pane.nameFields[0], pane)
        await settled(sc) { pane.shown?.first?.name == "Front" }
        pane.removeButtons[0].performClick(nil)
        await settled(sc) { pane.shown?.count == 3 }
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1", "Stage 2", "Stage 3"])
        XCTAssertEqual(pane.messageLabel.stringValue, "")
    }

    /// A12: the settings window at its least size → the name and destinations columns give way;
    /// Edit, Monitor and remove stay whole inside the table, where a click reaches them.
    func testVideoOutputs_narrowWindowKeepsButtons() async throws {
        let (_, sc) = try await openThree()
        let pane = sc.videoPane
        let window = try XCTUnwrap(sc.window)
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        window.setContentSize(NSSize(width: 1300, height: 800))
        window.contentView?.layoutSubtreeIfNeeded()
        pane.fitColumns()
        let wideName = pane.nameWidth
        window.setContentSize(window.contentMinSize)
        window.contentView?.layoutSubtreeIfNeeded()
        pane.fitColumns()
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertLessThan(pane.nameWidth, wideName, "the name gave way")
        let visible = pane.scroll.contentView.bounds
        for button in pane.editButtons + pane.monitorButtons + pane.removeButtons {
            let f = button.convert(button.bounds, to: pane.scroll.contentView)
            XCTAssertTrue(visible.contains(f), "\(button.accessibilityIdentifier()) \(f) in \(visible)")
            let mid = button.convert(NSPoint(x: button.bounds.midX, y: button.bounds.midY), to: nil)
            XCTAssertTrue(window.contentView?.hitTest(mid)?.isDescendant(of: button) == true, "a click reaches it")
        }
    }

    // MARK: B. The stage editor

    /// B1: a stage's editor → a window of its own (not a sheet) titled for the stage, with the
    /// name, layer, keep-black box, size text and its button at the top; the region table, the
    /// region box, the new-region pop-up and the canvas below; the guides box and the scale pop-up
    /// at the foot.
    func testStageEditor_opensAsWindowWithAllAreas() async throws {
        let (_, sc) = try await openG4()
        await settled(sc) { sc.videoPane.editButtons.count == 1 }
        sc.videoPane.editButtons[0].performClick(nil)
        let editor = try XCTUnwrap(sc.stageEditors[ModelID(stageA)])
        let window = try XCTUnwrap(editor.window)
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertTrue(window.isVisible)
        XCTAssertNil(window.sheetParent, "not a sheet")
        XCTAssertTrue(window.styleMask.contains(.resizable))
        XCTAssertEqual(window.title, String(format: loc("stageEditor.title"), "Stage 1"))
        func y(_ v: NSView) -> CGFloat { frame(v, in: window).midY }
        // Top to bottom (AppKit counts y upwards).
        let column: [NSView] = [
            editor.nameField, editor.sizeLabel, editor.regionList, editor.newRegionPopup, editor.canvasScroll,
            editor.guidesCheck,
        ]
        let ys = column.map(y)
        XCTAssertEqual(ys, ys.sorted(by: >), "name, size, regions, new region, canvas, foot")
        XCTAssertEqual(y(editor.layerField), y(editor.nameField), accuracy: 3)
        XCTAssertEqual(y(editor.blackCheck), y(editor.nameField), accuracy: 3)
        XCTAssertEqual(y(editor.resizeButton), y(editor.sizeLabel), accuracy: 3)
        XCTAssertEqual(y(editor.scalePopup), y(editor.guidesCheck), accuracy: 3)
        XCTAssertGreaterThan(frame(editor.scalePopup, in: window).minX, frame(editor.guidesCheck, in: window).maxX)
        XCTAssertGreaterThan(
            frame(editor.regionWidthField, in: window).minX, frame(editor.regionList, in: window).maxX,
            "the region box beside the table")
    }

    /// B3: a new stage's layer reads the top's word; the stepper down gives 999, 998; 1000 typed
    /// shows the top's word again; 0 typed or stepped to shows the bottom's word; a non-number or
    /// one above 1000 is refused and the old value stays.
    func testStageEditor_layerFieldWordsAndStepper() async throws {
        let (rig, sc) = try await openThree()
        choose(loc("settings.video.addEmpty"), in: sc.videoPane.addPopup)
        await settled(sc) { sc.videoPane.shown?.count == 4 }
        let editor = try XCTUnwrap(sc.stageEditor)
        func layer() -> Double? { rig.store.model?.stages.last?.raw["layer"]?.doubleValue }
        await editorSettled(sc, editor) { editor.layerField.stringValue == loc("stageEditor.layer.top") }
        XCTAssertEqual(editor.layerStepper.integerValue, 1000)
        for expected in [999, 998] {
            editor.layerStepper.integerValue -= 1
            _ = editor.layerStepper.sendAction(editor.layerStepper.action, to: editor.layerStepper.target)
            await editorSettled(sc, editor) { layer() == Double(expected) }
            XCTAssertEqual(editor.layerField.stringValue, String(expected))
        }
        type("1000", into: editor.layerField, editor)
        await editorSettled(sc, editor) { layer() == 1000 }
        XCTAssertEqual(editor.layerField.stringValue, loc("stageEditor.layer.top"))
        type("0", into: editor.layerField, editor)
        await editorSettled(sc, editor) { layer() == 0 }
        XCTAssertEqual(editor.layerField.stringValue, loc("stageEditor.layer.bottom"))
        type("1", into: editor.layerField, editor)
        await editorSettled(sc, editor) { layer() == 1 }
        editor.layerStepper.integerValue = 0
        _ = editor.layerStepper.sendAction(editor.layerStepper.action, to: editor.layerStepper.target)
        await editorSettled(sc, editor) { layer() == 0 }
        XCTAssertEqual(editor.layerField.stringValue, loc("stageEditor.layer.bottom"))
        for bad in ["abc", "1001", "-1", "2.5"] {
            type(bad, into: editor.layerField, editor)
            XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badLayer"), bad)
            XCTAssertEqual(editor.layerField.stringValue, loc("stageEditor.layer.bottom"), "\(bad): the old value")
        }
        await editor.editing?.value
        XCTAssertEqual(layer(), 0)
        // The words typed are taken too.
        type(loc("stageEditor.layer.top"), into: editor.layerField, editor)
        await editorSettled(sc, editor) { layer() == 1000 }
    }

    /// B4: a new stage's keep-black box is ticked; unticking it sets the stage's keep-black off at
    /// once (one undo step), ticking it puts it back (the black itself: Q819, engine video tests).
    func testStageEditor_keepBlackBox() async throws {
        let (rig, sc) = try await openThree()
        choose(loc("settings.video.addEmpty"), in: sc.videoPane.addPopup)
        await settled(sc) { sc.videoPane.shown?.count == 4 }
        let editor = try XCTUnwrap(sc.stageEditor)
        await editorSettled(sc, editor) { editor.blackCheck.state == .on }
        editor.blackCheck.performClick(nil)
        await editorSettled(sc, editor) { rig.store.model?.stages.last?.raw["keep_black"] == .bool(false) }
        XCTAssertEqual(editor.blackCheck.state, .off)
        XCTAssertEqual(rig.store.document?.undoName, "stage_keep_black")
        editor.blackCheck.performClick(nil)
        await editorSettled(sc, editor) { rig.store.model?.stages.last?.raw["keep_black"] == .bool(true) }
        XCTAssertEqual(editor.blackCheck.state, .on)
    }

    /// B5: the size is plain text with a button beside it: it cannot be typed in or selected, and a
    /// click on it does nothing.
    func testStageEditor_sizeIsReadOnlyText() async throws {
        let (rig, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(editor.window)
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(editor.sizeLabel.stringValue, "3840 × 1080")
        XCTAssertFalse(editor.sizeLabel.isEditable)
        XCTAssertFalse(editor.sizeLabel.isSelectable)
        XCTAssertEqual(editor.resizeButton.title, loc("stageEditor.resize"))
        XCTAssertLessThan(frame(editor.sizeLabel, in: window).maxX, frame(editor.resizeButton, in: window).minX)
        editor.sizeLabel.mouseDown(with: try press(editor))
        XCTAssertFalse(window.firstResponder is NSText, "no caret")
        XCTAssertNil(editor.resizeDialog)
        XCTAssertEqual(rig.store.document?.canUndo, false)
    }

    /// B9: the region table: each row has its letter, colour dot, a thumbnail with its region lit,
    /// its route pop-up and a remove button; a click on a row selects its region, the row drawn
    /// selected and the canvas drawing it darker.
    func testStageEditor_regionTableRows() async throws {
        let (_, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { editor.regionRows.count == 2 }
        editor.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(editor.regionLetters.map(\.stringValue), ["A", "B"])
        XCTAssertEqual(editor.regionChips.map(\.colour), [RegionColour.of(0), RegionColour.of(1)])
        XCTAssertEqual(editor.regionThumbnails.map(\.colour), editor.regionChips.map(\.colour))
        XCTAssertEqual(
            editor.regionThumbnails.map(\.bounds_), editor.regions.map(\.bounds), "each thumbnail lights its region")
        let thumb = editor.regionThumbnails[1]
        XCTAssertEqual(thumb.regionFrame.minX, thumb.stageFrame.midX, accuracy: 0.5, "B: the right half")
        XCTAssertEqual(editor.regionRoutePopups.count, 2)
        XCTAssertEqual(editor.regionRemoveButtons.count, 2)
        XCTAssertEqual(editor.canvas.selected, 0)
        XCTAssertEqual(editor.regionRows.map(\.isSelected), [true, false])
        editor.regionRows[1].mouseDown(with: try press(editor))
        XCTAssertEqual(editor.canvas.selected, 1, "the canvas draws B darker")
        XCTAssertEqual(editor.regionRows.map(\.isSelected), [false, true])
        XCTAssertEqual(editor.regionHeading.stringValue, String(format: loc("stageEditor.regionHeading"), "B"))
        // The thumbnail is part of its row.
        editor.regionThumbnails[0].mouseDown(with: try press(editor))
        XCTAssertEqual(editor.canvas.selected, 0)
    }

    /// B10: a region's route pop-up lists none, a separator, then every route named with its
    /// display and its rotation, the current one chosen; picking another moves the canvas label to
    /// it and undo moves it back.
    func testStageEditor_routePopupNamesAndPick() async throws {
        let (rig, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        guard
            case .success = await rig.store.send(
                command: .setRoute(ModelID(route2), .object(["type": .string("rotation"), "value": .int(90)])))
        else { return XCTFail("rotate") }
        await editorSettled(sc, editor) {
            self.titles(editor.regionRoutePopups[0]).last == "Output 2 (Virtual V2, 90°)"
        }
        let popup = editor.regionRoutePopups[0]
        XCTAssertEqual(
            titles(popup), [loc("stageEditor.noRoute"), "Output 1 (Virtual V1)", "Output 2 (Virtual V2, 90°)"])
        XCTAssertTrue(popup.menu?.items[1].isSeparatorItem == true, "a separator after none")
        XCTAssertEqual(popup.titleOfSelectedItem, "Output 1 (Virtual V1)")
        XCTAssertEqual(popup.selectedItem?.state, .on, "the current one ticked")
        choose("Output 2 (Virtual V2, 90°)", in: popup)
        await editorSettled(sc, editor) { editor.canvas.regions[0].route == "Output 2" }
        XCTAssertEqual(StageCanvasView.label(editor.canvas.regions[0]), "A → Output 2")
        XCTAssertEqual(rig.store.document?.undoName, "region_route")
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.canvas.regions[0].route == "Output 1" }
        XCTAssertEqual(editor.regionRoutePopups[0].titleOfSelectedItem, "Output 1 (Virtual V1)")
    }

    /// B11 and #813 G2: the new-region pop-up → a route picked: region B covers the whole stage on
    /// that route, selected; undo removes it. On a stage with no route a region with none can still
    /// be added.
    func testStageEditor_newRegionCoversStage() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080), (route2, "Output 2", 1280, 720)], width: 1920,
            regions: [(regionA, route1, 0, 1920)], displays: [(route1, "V1"), (route2, "V2")])
        let editor = sc.openStageEditor(ModelID(stageA))
        await editorSettled(sc, editor) { self.titles(editor.newRegionPopup).count == 4 }
        XCTAssertEqual(
            titles(editor.newRegionPopup),
            [
                loc("stageEditor.newRegion"), "On Output 1 (Virtual V1)", "On Output 2 (Virtual V2)",
                loc("stageEditor.newRegionNone"),
            ])
        choose("On Output 2 (Virtual V2)", in: editor.newRegionPopup)
        await editorSettled(sc, editor) { editor.regionRows.count == 2 }
        XCTAssertEqual(editor.regionLetters.map(\.stringValue), ["A", "B"])
        XCTAssertEqual(editor.regionRoutePopups.last?.titleOfSelectedItem, "Output 2 (Virtual V2)")
        XCTAssertEqual(editor.regions.last?.bounds, RegionBounds(x: 0, y: 0, width: 1920, height: 1080))
        XCTAssertEqual(editor.canvas.selected, 1, "B selected")
        XCTAssertEqual(rig.store.document?.undoName, "add_region")
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.regionRows.count == 1 }
        // A stage with no route at all still takes a region on none.
        for id in [route1, route2] { _ = await rig.store.send(command: .removeRoute(ModelID(id))) }
        await editorSettled(sc, editor) { self.titles(editor.newRegionPopup).count == 2 }
        XCTAssertEqual(titles(editor.newRegionPopup), [loc("stageEditor.newRegion"), loc("stageEditor.newRegionNone")])
        choose(loc("stageEditor.newRegionNone"), in: editor.newRegionPopup)
        await editorSettled(sc, editor) { editor.regionRows.count == 2 }
        XCTAssertNil(editor.regions.last?.route)
    }

    /// B12: region B (the whole of a 1920 x 1080 stage) selected → its box shows its size with the
    /// lock between, its lower left from the stage's centre (y up) and its upper right, read only.
    func testStageEditor_regionBoxValues() async throws {
        let (_, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920,
            regions: [(regionA, route1, 0, 960), (regionB, route1, 0, 1920)])
        let editor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(editor.window)
        editor.regionRows[1].mouseDown(with: try press(editor))
        XCTAssertEqual(editor.regionHeading.stringValue, String(format: loc("stageEditor.regionHeading"), "B"))
        XCTAssertEqual(
            [editor.regionWidthField, editor.regionHeightField, editor.lowerLeftXField, editor.lowerLeftYField]
                .map(\.stringValue), ["1920", "1080", "-960", "-540"])
        XCTAssertEqual([editor.upperRightXLabel.stringValue, editor.upperRightYLabel.stringValue], ["960", "540"])
        XCTAssertFalse(editor.upperRightXLabel.isEditable)
        window.contentView?.layoutSubtreeIfNeeded()
        let lock = frame(editor.aspectLock, in: window)
        XCTAssertGreaterThan(lock.minX, frame(editor.regionWidthField, in: window).maxX, "the lock between")
        XCTAssertLessThan(lock.maxX, frame(editor.regionHeightField, in: window).minX)
        XCTAssertNotNil(editor.aspectLock.image)
    }

    /// B13: the lock off, region B (the whole stage) → width 960: B keeps its lower-left corner
    /// (-960, -540), its upper right becomes 0, 540 and the canvas hatches its overlap with A;
    /// height 540 then gives an upper right of 0, 0. One undo per field.
    func testStageEditor_typingSizeKeepsLowerLeft() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920,
            regions: [(regionA, route1, 0, 1920), (regionB, route1, 0, 1920)])
        let editor = sc.openStageEditor(ModelID(stageA))
        editor.regionRows[1].mouseDown(with: try press(editor))
        XCTAssertEqual(editor.aspectLock.state, .off, "off at first")
        type("960", into: editor.regionWidthField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.width == 960 }
        XCTAssertEqual([editor.lowerLeftXField.stringValue, editor.lowerLeftYField.stringValue], ["-960", "-540"])
        XCTAssertEqual([editor.upperRightXLabel.stringValue, editor.upperRightYLabel.stringValue], ["0", "540"])
        XCTAssertEqual(editor.canvas.overlaps, [editor.canvas.regionFrames[1]], "B over A is hatched")
        XCTAssertEqual(rig.store.document?.undoName, "resize_region")
        type("540", into: editor.regionHeightField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.height == 540 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 0, y: 540, width: 960, height: 540))
        XCTAssertEqual([editor.lowerLeftXField.stringValue, editor.lowerLeftYField.stringValue], ["-960", "-540"])
        XCTAssertEqual([editor.upperRightXLabel.stringValue, editor.upperRightYLabel.stringValue], ["0", "0"])
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.regions[1].bounds.height == 1080 }
        XCTAssertEqual(editor.regions[1].bounds.width, 960, "one field undone")
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.regions[1].bounds.width == 1920 }
    }

    /// B14: the lock on, a 1920 x 1080 region → width 960 sets the height to 540 too, one step; the
    /// lock is the window's own (not saved, no undo step).
    func testStageEditor_aspectLockLinksSize() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920, regions: [(regionA, route1, 0, 1920)])
        let editor = sc.openStageEditor(ModelID(stageA))
        editor.aspectLock.performClick(nil)
        XCTAssertEqual(editor.aspectLock.state, .on)
        XCTAssertEqual(rig.store.document?.canUndo, false, "the lock is no edit")
        type("960", into: editor.regionWidthField, editor)
        await editorSettled(sc, editor) { editor.regions[0].bounds.width == 960 }
        XCTAssertEqual(editor.regions[0].bounds, RegionBounds(x: 0, y: 540, width: 960, height: 540))
        XCTAssertEqual(editor.regionHeightField.stringValue, "540")
        type("270", into: editor.regionHeightField, editor)
        await editorSettled(sc, editor) { editor.regions[0].bounds.height == 270 }
        XCTAssertEqual(editor.regions[0].bounds.width, 480)
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.regions[0].bounds.width == 960 }
        XCTAssertEqual(editor.regions[0].bounds.height, 540, "width and height were one step")
    }

    /// B15 and #813 G3: a corner or size that would put the region past the stage's edge is refused
    /// with a message and the field shows the old value; the region is unchanged and nothing sent.
    func testStageEditor_cornerOutsideStageRefused() async throws {
        let (rig, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        editor.regionRows[1].mouseDown(with: try press(editor))
        let outside = String(format: loc("stageEditor.outside"), 3840, 1080)
        let b = RegionBounds(x: 1920, y: 0, width: 1920, height: 1080)
        for (field, text) in [
            (editor.lowerLeftXField, "1000"), (editor.lowerLeftXField, "-1921"), (editor.lowerLeftYField, "1"),
            (editor.regionWidthField, "4000"), (editor.regionHeightField, "1081"),
            (editor.regionWidthField, "9223372036854775807"),
        ] {
            let before = field.stringValue
            type(text, into: field, editor)
            XCTAssertEqual(editor.messageLabel.stringValue, outside, text)
            XCTAssertEqual(field.stringValue, before, "\(text): the old value")
            XCTAssertEqual(editor.regions[1].bounds, b)
        }
        type("abc", into: editor.lowerLeftXField, editor)
        XCTAssertEqual(editor.messageLabel.stringValue, loc("stageEditor.badNumber"))
        type("0", into: editor.regionWidthField, editor)
        XCTAssertEqual(editor.messageLabel.stringValue, loc("stageEditor.badNumber"), "0 is no size")
        await editor.editing?.value
        XCTAssertEqual(rig.store.document?.canUndo, false, "nothing was sent")
        // Inside is taken, and the message goes.
        type("-1920", into: editor.lowerLeftXField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.x == 0 }
        XCTAssertEqual(editor.messageLabel.stringValue, "")
        XCTAssertEqual(rig.store.document?.undoName, "move_region")
    }

    /// B16: no region selected (its last region removed) → every region field a dash and off, the
    /// box's heading saying none is chosen.
    func testStageEditor_noSelectionShowsDashes() async throws {
        let (_, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920, regions: [(regionA, route1, 0, 1920)])
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertEqual(editor.regionHeading.stringValue, String(format: loc("stageEditor.regionHeading"), "A"))
        XCTAssertTrue(editor.regionWidthField.isEnabled)
        editor.regionRemoveButtons[0].performClick(nil)
        await editorSettled(sc, editor) { editor.regionRows.isEmpty }
        XCTAssertEqual(editor.regionHeading.stringValue, loc("stageEditor.noRegion"))
        for f in [editor.regionWidthField, editor.regionHeightField, editor.lowerLeftXField, editor.lowerLeftYField] {
            XCTAssertEqual(f.stringValue, StageEditorWindowController.dash)
            XCTAssertFalse(f.isEnabled)
        }
        XCTAssertFalse(editor.aspectLock.isEnabled)
        XCTAssertEqual(editor.upperRightXLabel.stringValue, StageEditorWindowController.dash)
        XCTAssertNil(editor.canvas.selected)
    }

    /// B17: the canvas: the stage outline with its name and size in the middle; each routed region
    /// in its colour labelled "A → Output 1"; the selected one darker; a click on a region selects
    /// it and its row.
    func testStageEditor_canvasDrawsRegionsAndLabels() async throws {
        let (_, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(editor.window)
        window.contentView?.layoutSubtreeIfNeeded()
        editor.layoutCanvas()
        XCTAssertEqual(editor.canvas.stageLabel, "Stage 1\n3840 × 1080")
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A → Output 1", "B → Output 2"])
        XCTAssertEqual(editor.canvas.regions.map(\.colour), editor.regionChips.map(\.colour))
        let stage = editor.canvas.stageFrame
        let frames = editor.canvas.regionFrames
        XCTAssertEqual(frames[0].minX, stage.minX, accuracy: 0.5)
        XCTAssertEqual(frames[0].width, stage.width / 2, accuracy: 0.5)
        XCTAssertEqual(frames[1].maxX, stage.maxX, accuracy: 0.5)
        XCTAssertTrue(editor.canvas.overlaps.isEmpty, "side by side: nothing hatched")
        XCTAssertEqual(editor.canvas.selected, 0)
        let atB = editor.canvas.convert(NSPoint(x: frames[1].midX, y: frames[1].midY), to: nil)
        XCTAssertTrue(window.contentView?.hitTest(atB) === editor.canvas)
        editor.canvas.mouseDown(with: try press(editor, at: atB))
        XCTAssertEqual(editor.canvas.selected, 1)
        XCTAssertEqual(editor.regionRows.map(\.isSelected), [false, true], "its row too")
        XCTAssertEqual(editor.lowerLeftXField.stringValue, "0", "B's lower left: the stage's centre")
    }

    /// B18: a region with no route is drawn lighter with a dashed outline, labelled by its letter
    /// alone.
    func testStageEditor_noRouteRegionDashedLetterOnly() async throws {
        let (_, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 3840,
            regions: [(regionA, route1, 0, 1920), (regionB, nil, 1920, 1920)])
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertNil(editor.canvas.regions[1].route, "drawn dashed and lighter")
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A → Output 1", "B"])
        XCTAssertEqual(editor.regionRoutePopups[1].titleOfSelectedItem, loc("stageEditor.noRoute"))
    }

    /// B19: the scale pop-up at the foot lists fit, then percentages up to 400; 50 draws the
    /// stage at half its pixels, scrolled when it is larger than the pane; fit fills the pane again.
    func testStageEditor_scalePopupFitAndPercent() async throws {
        let (_, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 3840, regions: [(regionA, route1, 0, 3840)])
        let editor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(editor.window)
        window.contentView?.layoutSubtreeIfNeeded()
        editor.layoutCanvas()
        XCTAssertEqual(editor.scalePopup.itemTitles.first, loc("stageEditor.scale.fit"))
        XCTAssertEqual(editor.scalePopup.itemTitles.last, "400%")
        XCTAssertEqual(editor.scalePopup.titleOfSelectedItem, loc("stageEditor.scale.fit"))
        let room = editor.canvasScroll.contentView.bounds.size
        XCTAssertEqual(editor.canvas.frame.size, room, "fit fills the pane")
        choose("50%", in: editor.scalePopup)
        XCTAssertEqual(editor.canvas.stageFrame.width, 1920, accuracy: 0.5, "half of 3840 pixels")
        XCTAssertEqual(editor.canvas.stageFrame.height, 540, accuracy: 0.5)
        XCTAssertGreaterThan(editor.canvas.frame.width, editor.canvasScroll.contentView.bounds.width, "it scrolls")
        choose(loc("stageEditor.scale.fit"), in: editor.scalePopup)
        XCTAssertEqual(editor.canvas.frame.size, editor.canvasScroll.contentView.bounds.size)
        XCTAssertLessThanOrEqual(editor.canvas.stageFrame.width, room.width)
    }

    /// B20: the guides box at the foot is off and unticked while no region has a route; one routed
    /// region turns it on, and a click shows that route's guides as its row's box does.
    func testStageEditor_guidesBoxDisabledWithoutRoutes() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 1920, regions: [(regionA, nil, 0, 1920)],
            displays: [(route1, "V1")])
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertFalse(editor.guidesCheck.isEnabled)
        XCTAssertEqual(editor.guidesCheck.state, .off)
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).last == "Output 1 (Virtual V1)" }
        choose("Output 1 (Virtual V1)", in: editor.regionRoutePopups[0])
        await editorSettled(sc, editor) { editor.guidesCheck.isEnabled }
        editor.guidesCheck.performClick(nil)
        await editorSettled(sc, editor) { rig.store.videoOutputs.route(ModelID(self.route1))?.guides == true }
        XCTAssertEqual(editor.guidesCheck.state, .on)
        await settled(sc) { sc.videoRoutingPane.guideChecks.first?.state == .on }
    }

    /// B22: a stage's editor open, the stage removed on Video Outputs → the editor closes; undo
    /// brings the stage back without opening it again.
    func testStageEditor_closesWhenStageRemoved() async throws {
        let (rig, sc) = try await openThree()
        let pane = sc.videoPane
        pane.editButtons[1].performClick(nil)
        let editor = try XCTUnwrap(sc.stageEditors[ModelID(stageB)])
        XCTAssertEqual(editor.window?.isVisible, true)
        pane.removeButtons[1].performClick(nil)
        await settled(sc) { pane.shown?.count == 2 }
        await assertEventually("the editor went") { editor.window?.isVisible == false }
        XCTAssertNil(sc.stageEditors[ModelID(stageB)])
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.count == 3 }
        await settle()
        XCTAssertEqual(editor.window?.isVisible, false, "the undo does not reopen it")
        XCTAssertNil(sc.stageEditors[ModelID(stageB)])
    }

    /// B23: Show Mode on → the editor opens and edits as in Edit mode.
    func testStageEditor_editableInShowMode() async throws {
        let (rig, sc) = try await openG4()
        _ = await rig.store.setShowMode(true).value
        await settled(sc) { rig.store.showMode }
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertEqual(editor.window?.isVisible, true)
        for c in [editor.nameField, editor.layerField, editor.blackCheck, editor.resizeButton, editor.newRegionPopup]
            as [NSControl]
        {
            XCTAssertTrue(c.isEnabled, "\(c.accessibilityIdentifier())")
        }
        type("Cyc", into: editor.nameField, editor)
        await editorSettled(sc, editor) { editor.window?.title == String(format: loc("stageEditor.title"), "Cyc") }
        type("1000", into: editor.regionWidthField, editor)
        await editorSettled(sc, editor) { editor.regions[0].bounds.width == 1000 }
        try resize(editor, width: "1920", height: "1080")
        await editorSettled(sc, editor) { editor.sizeLabel.stringValue == "1920 × 1080" }
    }

    /// B24: the editor at its least size: the region box, the canvas and the foot stay inside the
    /// window, no control is cut, and the canvas fits the stage under "fit".
    func testStageEditor_narrowWindowNoClipping() async throws {
        let (_, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(editor.window)
        window.setContentSize(StageEditorWindowController.minSize)
        window.contentView?.layoutSubtreeIfNeeded()
        editor.layoutCanvas()
        let content = try XCTUnwrap(window.contentView).bounds
        let controls: [NSView] =
            [
                editor.nameField, editor.layerField, editor.layerStepper, editor.blackCheck, editor.sizeLabel,
                editor.resizeButton, editor.regionWidthField, editor.aspectLock, editor.regionHeightField,
                editor.lowerLeftXField, editor.lowerLeftYField, editor.upperRightYLabel, editor.newRegionPopup,
                editor.canvasScroll, editor.guidesCheck, editor.scalePopup,
            ] + editor.regionRoutePopups + editor.regionRemoveButtons
        for v in controls {
            let f = frame(v, in: window)
            XCTAssertTrue(content.contains(f), "\(v.accessibilityIdentifier()) \(f) inside \(content)")
            if let c = v as? NSControl, !(c is NSPopUpButton) {
                XCTAssertGreaterThanOrEqual(
                    f.width + 0.5, c.fittingSize.width, "\(v.accessibilityIdentifier()) not cut")
            }
        }
        XCTAssertGreaterThan(frame(editor.scalePopup, in: window).minX, frame(editor.guidesCheck, in: window).maxX)
        XCTAssertTrue(editor.canvas.bounds.contains(editor.canvas.stageFrame), "the stage fits its canvas")
        XCTAssertGreaterThan(editor.canvas.stageFrame.width, 100)
    }

    // MARK: C. The route editor

    /// Opens Output Routing row `n`'s editor (off screen).
    private func routeEditor(_ sc: WorkspaceSettingsWindowController, _ n: Int = 0) throws -> RouteEditorController {
        sc.videoRoutingPane.editButtons[n].performClick(nil)
        return try XCTUnwrap(sc.routeEditor)
    }

    /// C1: a route row's Edit → a dialog window of its own (not a sheet on the settings window),
    /// titled for editing the route: name, size with the take-the-display's-size button, rotation,
    /// scaling with one line under it, the display and its size, Cancel and OK.
    func testRouteEditor_opensAsModalWindow() async throws {
        let (_, sc) = try await openG4()
        await settled(sc) { sc.videoRoutingPane.editButtons.count == 2 }
        let editor = try routeEditor(sc)
        let window = try XCTUnwrap(editor.window)
        XCTAssertNil(window.sheetParent, "not a sheet")
        XCTAssertNil(sc.window?.attachedSheet)
        XCTAssertTrue(window.styleMask.contains(.titled))
        XCTAssertEqual(window.title, loc("routeEditor.title"))
        XCTAssertEqual(editor.nameField.stringValue, "Output 1")
        XCTAssertEqual([editor.widthField.stringValue, editor.heightField.stringValue], ["1920", "1080"])
        XCTAssertEqual(editor.fromDisplayButton.title, loc("routeEditor.fromDisplay"))
        XCTAssertEqual(editor.rotationPopup.itemTitles, ["0°", "90°", "180°", "270°"])
        XCTAssertEqual(editor.scalingNote.stringValue, loc("routeEditor.scaling.note.fill"))
        XCTAssertEqual(editor.displayPopup.titleOfSelectedItem, "Virtual V1 (1920 × 1080)")
        XCTAssertFalse(editor.displaySizeLabel.isEditable)
        XCTAssertEqual([editor.cancelButton.title, editor.okButton.title], ["Cancel", "OK"])
        XCTAssertTrue(sc.owns(window), "Edit > Undo acts on this workspace from it")
        editor.cancelButton.performClick(nil)
        XCTAssertNil(sc.routeEditor)
    }

    /// C2: a 3840 x 2160 display chosen → the take-the-display's-size button makes the size 3840 x
    /// 2160; Cancel leaves the route as it was.
    func testRouteEditor_setSizeFromDisplay() async throws {
        engine.stop()
        engine = try await TestEngine.start(displays: "V1:1920x1080,V2:3840x2160")
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        await displaysListed()
        await settled(sc) { sc.videoRoutingPane.editButtons.count == 1 }
        let editor = try routeEditor(sc)
        choose("Virtual V2 (3840 × 2160)", in: editor.displayPopup)
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "3840 × 2160")
        editor.fromDisplayButton.performClick(nil)
        XCTAssertEqual([editor.widthField.stringValue, editor.heightField.stringValue], ["3840", "2160"])
        editor.cancelButton.performClick(nil)
        await settle()
        XCTAssertEqual(rig.store.model?.routes.first?.raw["width"]?.doubleValue, 1920)
        XCTAssertNil(rig.store.videoOutputs.route(ModelID(stageA))?.mapping, "no display chosen")
        XCTAssertEqual(rig.store.document?.canUndo, false)
    }

    /// C3: rotation 0 and 1920 x 1080 → 90 makes the size 1080 x 1920; 270 from 90 leaves it; 0 from
    /// 270 swaps it back; 180 from 0 leaves it.
    func testRouteEditor_rotation90Swaps() async throws {
        let (_, sc) = try await openG4()
        await settled(sc) { sc.videoRoutingPane.editButtons.count == 2 }
        let editor = try routeEditor(sc)
        func size() -> [String] { [editor.widthField.stringValue, editor.heightField.stringValue] }
        for (degrees, expected) in [
            ("90°", ["1080", "1920"]), ("270°", ["1080", "1920"]), ("0°", ["1920", "1080"]),
            ("180°", ["1920", "1080"]), ("270°", ["1080", "1920"]), ("180°", ["1920", "1080"]),
        ] {
            choose(degrees, in: editor.rotationPopup)
            XCTAssertEqual(size(), expected, degrees)
        }
        editor.cancelButton.performClick(nil)
    }

    /// C4: OK on a turned route → its stage's regions are as they were (sizes and places), and the
    /// stage editor names the route with its rotation.
    func testRouteEditor_rotationDoesNotChangeRegions() async throws {
        let (rig, sc) = try await openG4()
        await settled(sc) { sc.videoRoutingPane.editButtons.count == 2 }
        let stageEditor = sc.openStageEditor(ModelID(stageA))
        let before = stageEditor.regions
        let editor = try routeEditor(sc)
        choose("90°", in: editor.rotationPopup)
        editor.okButton.performClick(nil)
        await editor.saving?.value
        await editorSettled(sc, stageEditor) {
            stageEditor.regionRoutePopups[0].titleOfSelectedItem == "Output 1 (Virtual V1, 90°)"
        }
        XCTAssertEqual(stageEditor.regions, before, "the regions are unchanged")
        XCTAssertEqual(rig.store.model?.routes.first?.raw["width"]?.doubleValue, 1080)
        XCTAssertEqual(rig.store.model?.routes.first?.raw["rotation"]?.doubleValue, 90)
    }

    /// C5: the scaling pop-up lists centre, fit, stretch and fill (fill for a new route); each
    /// choice has its own line under it, our own words.
    func testRouteEditor_scalingPopupAndExplanation() async throws {
        let (_, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        choose(loc("settings.video.addRouteNone"), in: routing.addPopup)
        await settled(sc) { routing.editButtons.count == 3 }
        let editor = try routeEditor(sc, 2)
        XCTAssertEqual(
            editor.scalingPopup.itemTitles,
            ["centre", "fit", "stretch", "fill"].map { loc("settings.video.scaling.\($0)") })
        XCTAssertEqual(editor.scalingPopup.titleOfSelectedItem, loc("settings.video.scaling.fill"), "a new route fills")
        var lines: Set<String> = []
        for s in ["centre", "fit", "stretch", "fill"] {
            choose(loc("settings.video.scaling.\(s)"), in: editor.scalingPopup)
            XCTAssertEqual(editor.scalingNote.stringValue, loc("routeEditor.scaling.note.\(s)"))
            lines.insert(editor.scalingNote.stringValue)
        }
        XCTAssertEqual(lines.count, 4, "a line of its own each")
        editor.cancelButton.performClick(nil)
    }

    /// C6: the display pop-up and its size, read only: a display picked shows its size; none shows
    /// none chosen and the route's own size.
    func testRouteEditor_displayPopupAndReadOnlySize() async throws {
        let (_, sc) = try await openG4()
        await settled(sc) { sc.videoRoutingPane.editButtons.count == 2 }
        let editor = try routeEditor(sc, 1)
        XCTAssertEqual(
            editor.displayPopup.itemTitles,
            [loc("settings.video.displayNone"), "Virtual V1 (1920 × 1080)", "Virtual V2 (1280 × 720)"])
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "1280 × 720")
        choose("Virtual V1 (1920 × 1080)", in: editor.displayPopup)
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "1920 × 1080")
        choose(loc("settings.video.displayNone"), in: editor.displayPopup)
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "1280 × 720", "the route's own size")
        XCTAssertFalse(editor.fromDisplayButton.isEnabled)
        XCTAssertFalse(editor.displaySizeLabel.isEditable)
        editor.cancelButton.performClick(nil)
    }

    /// C7: OK with the name, size, rotation and scaling changed → one undo step; the display picked
    /// is this Mac's and no undo step; Cancel (and Esc, its key) applies nothing.
    func testRouteEditor_okOneUndoCancelNothing() async throws {
        let (rig, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.editButtons.count == 2 }
        let cancelled = try routeEditor(sc)
        XCTAssertEqual(cancelled.cancelButton.keyEquivalent, "\u{1b}", "Esc is Cancel")
        cancelled.nameField.stringValue = "Nope"
        cancelled.cancelButton.performClick(nil)
        await settle()
        XCTAssertEqual(rig.store.model?.routes.first?.raw["name"]?.stringValue, "Output 1")
        XCTAssertEqual(rig.store.document?.canUndo, false, "Cancel sent nothing")
        let editor = try routeEditor(sc)
        editor.nameField.stringValue = "Front"
        choose("90°", in: editor.rotationPopup)
        editor.widthField.stringValue = "1000"
        choose(loc("settings.video.scaling.fit"), in: editor.scalingPopup)
        choose("Virtual V2 (1280 × 720)", in: editor.displayPopup)
        editor.okButton.performClick(nil)
        await editor.saving?.value
        await settled(sc) { routing.shown?.first?.name == "Front" && routing.shown?.first?.display == "V2" }
        XCTAssertNil(sc.routeEditor, "OK takes it down")
        let raw = try XCTUnwrap(rig.store.model?.routes.first?.raw)
        XCTAssertEqual(raw["width"]?.doubleValue, 1000)
        XCTAssertEqual(raw["height"]?.doubleValue, 1920)
        XCTAssertEqual(raw["rotation"]?.doubleValue, 90)
        XCTAssertEqual(raw["scaling"]?.stringValue, "fit")
        XCTAssertEqual(rig.store.document?.undoName, "edit_route")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.first?.name == "Output 1" }
        let back = try XCTUnwrap(rig.store.model?.routes.first?.raw)
        XCTAssertEqual(
            [back["width"]?.doubleValue, back["height"]?.doubleValue, back["rotation"]?.doubleValue], [1920, 1080, 0])
        XCTAssertEqual(back["scaling"]?.stringValue, "fill")
        XCTAssertEqual(rig.store.document?.canUndo, false, "one step for all four")
        XCTAssertEqual(routing.shown?.first?.display, "V2", "the display is this Mac's, not undone")
    }

    /// C8: an empty name, or a size outside 1 to 30000 → the dialog stays up saying why; OK does
    /// not take it down.
    func testRouteEditor_invalidInputKeepsDialog() async throws {
        let (rig, sc) = try await openG4()
        await settled(sc) { sc.videoRoutingPane.editButtons.count == 2 }
        let editor = try routeEditor(sc)
        editor.nameField.stringValue = "  "
        editor.okButton.performClick(nil)
        XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badRouteName"))
        XCTAssertTrue(sc.routeEditor === editor)
        editor.nameField.stringValue = "Output 1"
        for (w, h) in [("0", "1080"), ("1920", "30001"), ("wide", "1080"), ("-1", "1")] {
            editor.widthField.stringValue = w
            editor.heightField.stringValue = h
            editor.okButton.performClick(nil)
            XCTAssertEqual(editor.messageLabel.stringValue, loc("routeEditor.badSize"), "\(w) x \(h)")
            XCTAssertTrue(sc.routeEditor === editor, "still up")
        }
        await settle()
        XCTAssertEqual(rig.store.document?.canUndo, false, "nothing sent")
        editor.cancelButton.performClick(nil)
    }

    /// C9: Show Mode on → the route editor opens and OK applies.
    func testRouteEditor_worksInShowMode() async throws {
        let (rig, sc) = try await openG4()
        _ = await rig.store.setShowMode(true).value
        await settled(sc) { rig.store.showMode && sc.videoRoutingPane.editButtons.count == 2 }
        XCTAssertTrue(sc.videoRoutingPane.editButtons[0].isEnabled)
        let editor = try routeEditor(sc)
        editor.nameField.stringValue = "Show"
        editor.okButton.performClick(nil)
        await editor.saving?.value
        await settled(sc) { sc.videoRoutingPane.shown?.first?.name == "Show" }
        XCTAssertNil(sc.routeEditor)
    }

    // MARK: E. Removing a route

    /// E1: Stage 1 with regions A and B on Output 1 → Output 1 removed (no question): the stage's
    /// row says it goes nowhere; its open editor shows both route pop-ups at none, the regions
    /// dashed and labelled by letter only, the guides box off.
    func testRoutes_removeLeavesRegionsWithNoRoute() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 3840,
            regions: [(regionA, route1, 0, 1920), (regionB, route1, 1920, 1920)], displays: [(route1, "V1")])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        let editor = sc.openStageEditor(ModelID(stageA))
        await settled(sc) { pane.deviceCells.first?.stringValue == "Virtual V1" && routing.removeButtons.count == 1 }
        XCTAssertTrue(editor.guidesCheck.isEnabled)
        routing.removeButtons[0].performClick(nil)
        await settled(sc) { routing.shown?.isEmpty == true }
        XCTAssertEqual(pane.deviceCells.first?.stringValue, loc("settings.video.noDestinations"))
        await editorSettled(sc, editor) {
            editor.regionRoutePopups.allSatisfy { $0.titleOfSelectedItem == loc("stageEditor.noRoute") }
        }
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A", "B"])
        XCTAssertTrue(editor.canvas.regions.allSatisfy { $0.route == nil }, "dashed")
        XCTAssertFalse(editor.guidesCheck.isEnabled)
        XCTAssertEqual(rig.store.document?.undoName, "remove_route")
    }

    /// E2: undo after the removal → the route is back on its display and both regions on it.
    func testRoutes_undoRestoresRegions() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080)], width: 3840,
            regions: [(regionA, route1, 0, 1920), (regionB, route1, 1920, 1920)], displays: [(route1, "V1")])
        let routing = sc.videoRoutingPane
        let editor = sc.openStageEditor(ModelID(stageA))
        await settled(sc) { routing.removeButtons.count == 1 }
        routing.removeButtons[0].performClick(nil)
        await settled(sc) { routing.shown?.isEmpty == true }
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.first?.display == "V1" }
        XCTAssertEqual(routing.displayPopups.first?.titleOfSelectedItem, "Virtual V1 (1920 × 1080)")
        await editorSettled(sc, editor) {
            editor.regionRoutePopups.allSatisfy { $0.titleOfSelectedItem == "Output 1 (Virtual V1)" }
        }
        XCTAssertEqual(editor.regions.map(\.route), [ModelID(route1), ModelID(route1)])
        XCTAssertEqual(sc.videoPane.deviceCells.first?.stringValue, "Virtual V1")
    }
}
