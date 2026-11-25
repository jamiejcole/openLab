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
        XCTAssertEqual(sc.videoRoutingPane.cols, [.handle, .name, .size, .display, .scaling, .guides, .edit, .remove])
        XCTAssertEqual(sc.videoPane.cols, [.name, .size, .devices, .edit, .monitor, .remove])
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
            XCTAssertEqual(pane.holder.subviews, [pane.grid], "\(why): one table")
            XCTAssertEqual(pane.grid.numberOfRows, rows.count + stagesWarned, why)
            let controls: [NSView] =
                pane.nameFields + pane.sizeLabels + pane.deviceCells + pane.editButtons + pane.monitorButtons
                + pane.removeButtons
            // Per row its controls (the titles are not in the rows' grid), and a line per warning.
            XCTAssertEqual(pane.grid.subviews.count, controls.count + stagesWarned, why)
            XCTAssertTrue(controls.allSatisfy { $0.superview === pane.grid }, why)
            XCTAssertEqual(pane.nameFields.map(\.stringValue), rows.map(\.name), why)
            // The routing page: its row's eight cells, and a line for each warning.
            XCTAssertEqual(routing.holder.subviews, [routing.grid, routing.dropLine], "\(why): one table")
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
        XCTAssertEqual(editor.layerField.stringValue, "0")
        type("abc", into: editor.layerField)
        XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(editor.layerField.stringValue, "0")
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
        for popup in routing.displayPopups + routing.scalingPopups {
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
            for popup in routing.displayPopups + routing.scalingPopups {
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

    /// #674 flow 1 and #813 G6: a stage's size is changed in its editor: its row shows the new size,
    /// its region follows it (half the width: still the whole stage) and still fills its route on
    /// V1, and one undo puts both back.
    func testG6AStagesSizeChangedInItsEditorScalesItsRegionsAndOneUndoRestoresBoth() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.sizeLabels.count == 1 && routing.displayPopups.count == 1 }
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertEqual(editor.widthField.stringValue, "1920")
        XCTAssertEqual(editor.heightField.stringValue, "1080")
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
        type("1080", into: editor.widthField, editor)
        await settled(sc) { pane.shown?.first?.width == 1080 }
        await editor.editing?.value
        XCTAssertEqual(pane.sizeLabels[0].stringValue, "1080 × 1080")
        XCTAssertEqual(editor.widthField.stringValue, "1080")
        XCTAssertEqual(editor.heightField.stringValue, "1080")
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
        XCTAssertEqual(editor.widthField.stringValue, "1920")
        XCTAssertEqual(editor.regions.map(\.bounds), [RegionBounds(x: 0, y: 0, width: 1920, height: 1080)])
        await assertEventually("back across V1") { await self.displayColours("V1", points) == [red, red] }
    }

    /// #674 flow 2: a size of 0, above the limit, or not a number is refused with the reason shown
    /// where every other refused value's is, and the old size stays (nothing is sent).
    func testASizeOfZeroAboveTheLimitOrNotANumberIsRefusedWithTheReasonAndTheOldSizeStays() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.sizeLabels.count == 1 }
        let editor = sc.openStageEditor(ModelID(stageA))
        for (text, width) in [("0", true), ("16385", false), ("abc", true), ("-5", false), ("", true)] {
            let field = width ? editor.widthField : editor.heightField
            type(text, into: field, editor)
            XCTAssertEqual(editor.messageLabel.stringValue, loc("settings.video.badSize"), text)
            XCTAssertEqual(field.stringValue, width ? "1920" : "1080", "\(text): the old size stays")
        }
        await editor.editing?.value
        XCTAssertEqual(rig.store.model?.stages.first?.raw["width"]?.doubleValue, 1920)
        XCTAssertEqual(rig.store.model?.stages.first?.raw["height"]?.doubleValue, 1080)
        XCTAssertEqual(rig.store.document?.canUndo, false, "nothing was sent")
        // The largest size is taken, and a good edit clears the reason.
        type("16384", into: editor.heightField, editor)
        await settled(sc) { pane.shown?.first?.height == 16384 }
        await editor.editing?.value
        XCTAssertEqual(editor.messageLabel.stringValue, "")
        XCTAssertEqual(editor.heightField.stringValue, "16384")
    }

    /// A size typed in the stage editor and committed with Return, then undone: the field shows the
    /// size the undo put back, and a click on another control sends no size (the undone one stays).
    func testASizeUndoneAfterReturnIsShownAndAClickElsewhereNeverSendsItAgain() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080), (stageB, "Side", 1280, 720)])
        let pane = sc.videoPane
        await settled(sc) { pane.sizeLabels.count == 2 }
        let stageEditor = sc.openStageEditor(ModelID(stageA))
        let window = try XCTUnwrap(stageEditor.window)
        XCTAssertTrue(window.makeFirstResponder(stageEditor.heightField))
        try XCTUnwrap(stageEditor.heightField.currentEditor()).string = "400"
        let editor = try XCTUnwrap(stageEditor.heightField.currentEditor() as? NSTextView)
        editor.insertNewline(nil)
        await settled(sc) { pane.shown?.first?.height == 400 }
        await stageEditor.editing?.value
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.first?.height == 1080 }
        await assertEventually("the undone size is shown") { stageEditor.heightField.stringValue == "1080" }
        XCTAssertNotEqual(stageEditor.heightField.currentEditor()?.string, "400")
        stageEditor.blackCheck.performClick(nil)
        await stageEditor.editing?.value
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(true) }
        XCTAssertEqual(rig.store.model?.stages.first?.raw["height"]?.doubleValue, 1080, "no size sent again")
        XCTAssertEqual(stageEditor.heightField.stringValue, "1080")
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
        XCTAssertEqual(routing.scalingPopups.map { $0.titleOfSelectedItem }, [loc("settings.video.scaling.fill")])
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
        XCTAssertEqual(editor.regionRoutePopups.map { $0.titleOfSelectedItem }, ["Output 1"])
        XCTAssertEqual(editor.canvas.regionFrames, [editor.canvas.stageFrame], "A covers the whole stage")
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A — Output 1"])
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
        XCTAssertEqual(pane.deviceCells.map(\.stringValue), [""])
        XCTAssertEqual(pane.warningLabels.map(\.stringValue), [loc("settings.video.warn.stageNoDisplay")])
        let editor = sc.openStageEditor(try XCTUnwrap(rig.store.model?.stages.first?.id))
        XCTAssertEqual(editor.regionRoutePopups.map { $0.titleOfSelectedItem }, ["Output 1"])
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
                loc("settings.video.newRoute"), loc("settings.video.addRouteWith"), "   Virtual V1 (1920 × 1080)",
                "   Virtual V2 (1280 × 720)", loc("settings.video.addRouteNone"),
            ])
        choose("   Virtual V2 (1280 × 720)", in: routing.addPopup)
        await settled(sc) { routing.shown?.count == 2 && routing.shown?.last?.display == "V2" }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1", "Output 2"])
        XCTAssertEqual(routing.sizeLabels.last?.stringValue, "1280 × 720")
        XCTAssertEqual(routing.displayPopups.last?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        XCTAssertEqual(routing.scalingPopups.last?.titleOfSelectedItem, loc("settings.video.scaling.fill"))
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
        await editorSettled(sc, editor) { editor.regionRoutePopups.last?.titleOfSelectedItem == "Projector" }
        XCTAssertEqual(titles(editor.regionRoutePopups[0]), ["Output 1", "Projector", loc("stageEditor.noRoute")])
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A — Output 1", "B — Projector"])
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
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).count == 3 }
        XCTAssertEqual(titles(editor.regionRoutePopups[0]), ["Output 1", "Output 1", loc("stageEditor.noRoute")])
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
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).first == "Output 2" }
        XCTAssertEqual(titles(editor.regionRoutePopups[0]), ["Output 2", "Output 1", loc("stageEditor.noRoute")])
        XCTAssertEqual(
            Array(titles(editor.newRegionPopup).dropFirst()),
            ["On Output 2", "On Output 1", loc("stageEditor.newRegionNone")])
        // Let go where it already is: nothing is sent.
        routing.moveRoute(from: 0, toGap: 1)
        await routing.pending?.value
        XCTAssertEqual(rig.store.document?.undoName, "move_route")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.first?.name == "Output 1" }
        XCTAssertEqual(routing.nameFields.map(\.stringValue), ["Output 1", "Output 2"])
        await editorSettled(sc, editor) { self.titles(editor.regionRoutePopups[0]).first == "Output 1" }
    }

    /// #813 R4: Output 1's Edit → a sheet with the name, the size and a button that takes the
    /// display's size, the rotation (0, 90, 180, 270), the scaling with a line saying what it does,
    /// the display and its size. Size and scaling changed, then OK: the row shows the new size and
    /// one Undo reverts both. The same then Cancel: nothing changes. 0 or above 30000 is refused.
    func testR4TheRouteEditorChangesSizeAndScalingInOneUndoStepCancelChangesNothing() async throws {
        let (rig, sc) = try await openG4()
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.editButtons.count == 2 }
        routing.editButtons[0].performClick(nil)
        let editor = try XCTUnwrap(sc.routeEditor)
        await assertEventually("the sheet is up") { sc.window?.attachedSheet === editor.window }
        XCTAssertEqual(editor.nameField.stringValue, "Output 1")
        XCTAssertEqual([editor.widthField.stringValue, editor.heightField.stringValue], ["1920", "1080"])
        XCTAssertEqual(editor.rotationPopup.itemTitles, ["0°", "90°", "180°", "270°"])
        XCTAssertEqual(editor.rotationPopup.titleOfSelectedItem, "0°")
        XCTAssertEqual(
            editor.scalingPopup.itemTitles,
            ["centre", "fit", "stretch", "fill"].map { loc("settings.video.scaling.\($0)") })
        XCTAssertEqual(editor.scalingPopup.titleOfSelectedItem, loc("settings.video.scaling.fill"))
        XCTAssertEqual(editor.scalingNote.stringValue, loc("routeEditor.scaling.note.fill"))
        XCTAssertEqual(editor.displayPopup.titleOfSelectedItem, "Virtual V1 (1920 × 1080)")
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "1920 × 1080")
        // The scaling's line follows the choice; the display's size follows the display.
        editor.scalingPopup.selectItem(withTitle: loc("settings.video.scaling.centre"))
        _ = editor.scalingPopup.sendAction(editor.scalingPopup.action, to: editor.scalingPopup.target)
        XCTAssertEqual(editor.scalingNote.stringValue, loc("routeEditor.scaling.note.centre"))
        editor.displayPopup.selectItem(withTitle: "Virtual V2 (1280 × 720)")
        _ = editor.displayPopup.sendAction(editor.displayPopup.action, to: editor.displayPopup.target)
        XCTAssertEqual(editor.displaySizeLabel.stringValue, "1280 × 720")
        editor.fromDisplayButton.performClick(nil)
        XCTAssertEqual([editor.widthField.stringValue, editor.heightField.stringValue], ["1280", "720"])
        // Cancel: nothing changes.
        editor.cancelButton.performClick(nil)
        await assertEventually("the sheet is down") { sc.window?.attachedSheet == nil && sc.routeEditor == nil }
        XCTAssertEqual(rig.store.model?.routes.first?.raw["width"]?.doubleValue, 1920)
        XCTAssertEqual(rig.store.model?.routes.first?.raw["scaling"]?.stringValue, "fill")
        XCTAssertEqual(rig.store.videoOutputs.route(ModelID(route1))?.mapping?.display, "V1")
        // Again: 0, above 30000 and not a number are refused in the sheet, which stays up.
        routing.editButtons[0].performClick(nil)
        let again = try XCTUnwrap(sc.routeEditor)
        for (w, h) in [("0", "1080"), ("1920", "30001"), ("wide", "1080")] {
            again.widthField.stringValue = w
            again.heightField.stringValue = h
            again.okButton.performClick(nil)
            XCTAssertEqual(again.messageLabel.stringValue, loc("routeEditor.badSize"), "\(w) x \(h)")
            XCTAssertTrue(sc.routeEditor === again, "still up")
        }
        again.widthField.stringValue = "1280"
        again.heightField.stringValue = "720"
        again.scalingPopup.selectItem(withTitle: loc("settings.video.scaling.fit"))
        again.okButton.performClick(nil)
        await again.saving?.value
        await settled(sc) { routing.shown?.first?.width == 1280 }
        XCTAssertNil(sc.routeEditor, "OK takes the sheet down")
        XCTAssertEqual(routing.sizeLabels.first?.stringValue, "1280 × 720")
        XCTAssertEqual(routing.scalingPopups.first?.titleOfSelectedItem, loc("settings.video.scaling.fit"))
        XCTAssertEqual(rig.store.document?.undoName, "edit_route")
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.first?.width == 1920 }
        XCTAssertEqual(routing.scalingPopups.first?.titleOfSelectedItem, loc("settings.video.scaling.fill"), "both")
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
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label)[1], "B — \(loc("stageEditor.noRoute"))")
        await assertEventually("V2 freed, V1 plays on") { await self.lowerMiddles() == [self.blue, self.black] }
        _ = await rig.store.undo()
        await settled(sc) { routing.shown?.count == 2 && routing.shown?.last?.display == "V2" }
        XCTAssertEqual(routing.displayPopups.last?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        await editorSettled(sc, editor) { editor.regionRoutePopups.last?.titleOfSelectedItem == "Output 2" }
        XCTAssertNotNil(editor.canvas.regions[1].route)
        await assertEventually("both halves back") { await self.lowerMiddles() == [self.blue, self.yellow] }
    }

    /// #813 S1: the N1 workspace → the empty stage from the add-stage pop-up: "Stage 2" at 1920 x
    /// 1080 with no region; its displays column is empty and its row says it is shown on no
    /// display; its monitor window shows its picture. Undo removes it.
    func testS1AnEmptyStageHasNoRegionNoDisplayAndAMonitorAndUndoRemovesIt() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Stage 1", 1920, 1080)])
        let pane = sc.videoPane
        _ = await rig.store.setRouteDisplay(ModelID(stageA), display: "V1")
        await settled(sc) { pane.deviceCells.map(\.stringValue) == ["Virtual V1"] }
        XCTAssertEqual(
            titles(pane.addPopup), [loc("settings.video.add"), loc("settings.video.addEmpty"), "On Output 1"])
        choose(loc("settings.video.addEmpty"), in: pane.addPopup)
        await settled(sc) { pane.shown?.count == 2 }
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1", "Stage 2"])
        XCTAssertEqual(pane.sizeLabels.last?.stringValue, "1920 × 1080")
        XCTAssertEqual(pane.deviceCells.last?.stringValue, "")
        XCTAssertEqual(pane.warningLabels.last?.stringValue, loc("settings.video.warn.stageNoDisplay"))
        XCTAssertEqual(pane.warningLabels.first?.stringValue, "", "Stage 1 is on V1")
        let added = try XCTUnwrap(pane.shown?.last?.id)
        XCTAssertEqual(rig.store.model?.stages.last?.raw["regions"]?.arrayValue ?? [], [], "no region")
        XCTAssertEqual(rig.store.document?.undoName, "add_stage")
        // Its monitor shows it.
        pane.monitorButtons[1].performClick(nil)
        await settled(sc) { rig.store.videoOutputs.stages.last?.monitor == true }
        _ = await rig.client.send { .request(id: $0, .stageTestCard(stage: added, frame: 9)) }
        let monitor = await monitorShows(rig, added.raw, 400, 300, [[0.5, 0.5]])
        XCTAssertEqual(monitor?.0, 9)
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.count == 1 }
    }

    /// #813 S2: two routes → "On Output 2" from the add-stage pop-up: Stage 2 sized as Output 2,
    /// one region covering it sent to Output 2, its displays column naming V2. Undo and redo are
    /// one step each.
    func testS2AStageOnARouteIsSizedAsItWithOneRegionOnItAndIsOneUndoStep() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080), (route2, "Output 2", 1280, 720)], width: 1920,
            regions: [(regionA, route1, 0, 1920)], displays: [(route1, "V1"), (route2, "V2")])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 1 && pane.deviceCells.first?.stringValue == "Virtual V1" }
        choose("On Output 2", in: pane.addPopup)
        await settled(sc) { pane.shown?.count == 2 }
        XCTAssertEqual(pane.nameFields.last?.stringValue, "Stage 2")
        XCTAssertEqual(pane.sizeLabels.last?.stringValue, "1280 × 720")
        XCTAssertEqual(pane.deviceCells.last?.stringValue, "Virtual V2")
        let stage = try XCTUnwrap(rig.store.model?.stages.last)
        XCTAssertEqual(RegionRow.of(stage).map(\.route), [ModelID(route2)])
        XCTAssertEqual(RegionRow.of(stage).map(\.bounds), [RegionBounds(x: 0, y: 0, width: 1280, height: 720)])
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.count == 1 }
        XCTAssertEqual(rig.store.document?.canUndo, false, "one step")
        _ = await rig.store.redo()
        await settled(sc) { pane.shown?.count == 2 }
        XCTAssertEqual(pane.deviceCells.last?.stringValue, "Virtual V2")
        XCTAssertEqual(rig.store.document?.canRedo, false, "one step")
    }

    /// #813 S4: stage rows → each shows its name (typed in place), its size, the displays it
    /// reaches through its regions' routes (comma-separated, in route order) and Edit, Monitor and
    /// remove buttons; keep-black and layer are in the stage editor, not on the row.
    func testS4AStageRowShowsNameSizeDisplaysInRouteOrderAndEditMonitorRemove() async throws {
        let (_, sc) = try await openG4()
        let pane = sc.videoPane
        await settled(sc) { pane.deviceCells.first?.stringValue == "Virtual V1, Virtual V2" }
        XCTAssertTrue(pane.nameFields[0].isEditable)
        XCTAssertEqual(pane.nameFields[0].stringValue, "Stage 1")
        XCTAssertEqual(pane.sizeLabels[0].stringValue, "3840 × 1080")
        XCTAssertEqual(pane.editButtons[0].title, loc("settings.video.edit"))
        XCTAssertEqual(pane.monitorButtons[0].title, loc("settings.video.monitor"))
        XCTAssertEqual(pane.removeButtons.count, 1)
        XCTAssertEqual(
            pane.cols.map { loc("settings.video.col.\($0.key)") }, ["Name", "Resolution", "Displays", "", "", ""])
        XCTAssertTrue(pane.warningLabels.allSatisfy(\.stringValue.isEmpty))
        // The routes swapped: the displays follow the route order.
        _ = await sc.videoPane.store.send(command: .moveRoute(ModelID(route2), to: 0))
        await settled(sc) { pane.deviceCells.first?.stringValue == "Virtual V2, Virtual V1" }
    }

    /// #813 G1: Stage 1's Edit → a window for Stage 1: name, layer, keep-black and size at the top;
    /// the region list (letter, colour, route pop-up, remove); the canvas with each region drawn in
    /// its colour, labelled with its letter and route. A click on a region in the canvas or the
    /// list selects it; the selected one is drawn darker.
    func testG1TheStageEditorShowsItsAttributesRegionListAndCanvasAndAClickSelects() async throws {
        let (_, sc) = try await openG4()
        let pane = sc.videoPane
        await settled(sc) { pane.editButtons.count == 1 }
        pane.editButtons[0].performClick(nil)
        let editor = try XCTUnwrap(sc.stageEditors[ModelID(stageA)])
        let window = try XCTUnwrap(editor.window)
        XCTAssertTrue(window.isVisible)
        XCTAssertEqual(window.title, String(format: loc("stageEditor.title"), "Stage 1"))
        XCTAssertEqual(editor.nameField.stringValue, "Stage 1")
        XCTAssertEqual(editor.layerField.stringValue, "1")
        XCTAssertEqual(editor.blackCheck.state, .off)
        XCTAssertEqual([editor.widthField.stringValue, editor.heightField.stringValue], ["3840", "1080"])
        XCTAssertEqual(editor.regionChips.map(\.letter), ["A", "B"])
        XCTAssertNotEqual(editor.regionChips[0].colour, editor.regionChips[1].colour)
        XCTAssertEqual(editor.regionRoutePopups.map { $0.titleOfSelectedItem }, ["Output 1", "Output 2"])
        XCTAssertEqual(editor.regionRemoveButtons.count, 2)
        XCTAssertEqual(editor.canvas.regions.map(StageCanvasView.label), ["A — Output 1", "B — Output 2"])
        XCTAssertEqual(editor.canvas.regions.map(\.colour), editor.regionChips.map(\.colour))
        window.contentView?.layoutSubtreeIfNeeded()
        let stage = editor.canvas.stageFrame
        let frames = editor.canvas.regionFrames
        XCTAssertEqual(frames[0].minX, stage.minX, accuracy: 0.5)
        XCTAssertEqual(frames[0].width, stage.width / 2, accuracy: 0.5)
        XCTAssertEqual(frames[1].maxX, stage.maxX, accuracy: 0.5)
        XCTAssertEqual(editor.canvas.selected, 0, "A selected first")
        XCTAssertTrue(editor.regionChips[0].isSelected)
        // A click on B in the canvas.
        let atB = editor.canvas.convert(NSPoint(x: frames[1].midX, y: frames[1].midY), to: nil)
        XCTAssertTrue(window.contentView?.hitTest(atB) === editor.canvas)
        let click = try press(editor, at: atB)
        editor.canvas.mouseDown(with: click)
        XCTAssertEqual(editor.canvas.selected, 1)
        XCTAssertEqual([editor.regionChips[0].isSelected, editor.regionChips[1].isSelected], [false, true])
        XCTAssertEqual(editor.lowerLeftXField.stringValue, "0", "B's lower left: the stage's centre")
        // A click on A in the list.
        editor.regionChips[0].mouseDown(with: click)
        XCTAssertEqual(editor.canvas.selected, 0)
        XCTAssertEqual(editor.lowerLeftXField.stringValue, "-1920")
    }

    /// #813 G2: the stage editor open → "On Output 2" from the new-region pop-up: region B appears
    /// covering the whole stage, sent to Output 2 and selected; its fields show its size and its
    /// lower-left corner from the stage's centre. Undo removes it.
    func testG2ANewRegionOnARouteCoversTheStageIsSelectedAndUndoRemovesIt() async throws {
        let (rig, sc) = try await openRouted(
            routes: [(route1, "Output 1", 1920, 1080), (route2, "Output 2", 1280, 720)], width: 1920,
            regions: [(regionA, route1, 0, 1920)], displays: [(route1, "V1"), (route2, "V2")])
        let editor = sc.openStageEditor(ModelID(stageA))
        XCTAssertEqual(
            titles(editor.newRegionPopup),
            [loc("stageEditor.newRegion"), "On Output 1", "On Output 2", loc("stageEditor.newRegionNone")])
        choose("On Output 2", in: editor.newRegionPopup)
        await editorSettled(sc, editor) { editor.regionChips.count == 2 }
        XCTAssertEqual(editor.regionChips.map(\.letter), ["A", "B"])
        XCTAssertEqual(editor.regionRoutePopups.last?.titleOfSelectedItem, "Output 2")
        XCTAssertEqual(editor.regions.last?.bounds, RegionBounds(x: 0, y: 0, width: 1920, height: 1080))
        XCTAssertEqual(editor.canvas.selected, 1, "B selected")
        XCTAssertTrue(editor.regionChips[1].isSelected)
        XCTAssertEqual(
            [editor.regionWidthField, editor.regionHeightField, editor.lowerLeftXField, editor.lowerLeftYField]
                .map(\.stringValue), ["1920", "1080", "-960", "-540"])
        XCTAssertEqual(editor.upperRightLabel.stringValue, "960, 540")
        XCTAssertEqual(rig.store.document?.undoName, "add_region")
        _ = await rig.store.undo()
        await editorSettled(sc, editor) { editor.regionChips.count == 1 }
        XCTAssertEqual(editor.canvas.selected, 0)
    }

    /// #813 G3: region B selected → a width, height or lower-left corner typed: the canvas redraws
    /// at once, one undo step per field (the corner stays put for a size). A value that would put
    /// any part of the region outside the stage is refused, and the field shows the old value.
    func testG3TypedRegionSizeAndCornerRedrawOneStepEachAndOutsideTheStageIsRefused() async throws {
        let (rig, sc) = try await openG4()
        let editor = sc.openStageEditor(ModelID(stageA))
        editor.regionChips[1].mouseDown(with: try press(editor))
        XCTAssertEqual(editor.canvas.selected, 1)
        let stage = editor.canvas.stageFrame
        // A size no stage can hold is a number, refused as outside the stage, with no sum overflowing
        // (with B at x 1920 a width of Int.max once overflowed and crashed the app).
        let outside = String(format: loc("stageEditor.outside"), 3840, 1080)
        for field in [editor.regionWidthField, editor.regionHeightField] {
            for text in ["9223372036854775807", "20000"] {
                type(text, into: field, editor)
                XCTAssertEqual(editor.messageLabel.stringValue, outside, "\(text)")
                XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 1920, y: 0, width: 1920, height: 1080))
            }
        }
        type("960", into: editor.regionWidthField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.width == 960 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 1920, y: 0, width: 960, height: 1080))
        XCTAssertEqual(editor.canvas.regionFrames[1].width, stage.width / 4, accuracy: 0.5)
        XCTAssertEqual(rig.store.document?.undoName, "resize_region")
        // The height: the lower edge stays.
        type("540", into: editor.regionHeightField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.height == 540 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 1920, y: 540, width: 960, height: 540))
        XCTAssertEqual(editor.lowerLeftYField.stringValue, "-540")
        // The corner: the size stays.
        type("-1920", into: editor.lowerLeftXField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.x == 0 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 0, y: 540, width: 960, height: 540))
        XCTAssertEqual(editor.canvas.regionFrames[1].minX, stage.minX, accuracy: 0.5)
        XCTAssertEqual(rig.store.document?.undoName, "move_region")
        type("0", into: editor.lowerLeftYField, editor)
        await editorSettled(sc, editor) { editor.regions[1].bounds.y == 0 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 0, y: 0, width: 960, height: 540))
        // Outside the stage: refused, the old value back, nothing sent.
        for (field, text) in [
            (editor.regionWidthField, "4000"), (editor.regionHeightField, "1081"), (editor.lowerLeftXField, "1000"),
            (editor.lowerLeftYField, "100"), (editor.lowerLeftXField, "-1921"), (editor.regionWidthField, "0"),
        ] {
            let before = field.stringValue
            type(text, into: field, editor)
            XCTAssertFalse(editor.messageLabel.stringValue.isEmpty, text)
            XCTAssertEqual(field.stringValue, before, "\(text): the old value")
        }
        XCTAssertEqual(editor.messageLabel.stringValue, loc("stageEditor.badNumber"), "0 is no size")
        type("1000", into: editor.lowerLeftXField, editor)
        XCTAssertEqual(editor.messageLabel.stringValue, String(format: loc("stageEditor.outside"), 3840, 1080))
        await editor.editing?.value
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 0, y: 0, width: 960, height: 540))
        // One undo per field, back to where B was.
        for _ in 0..<4 { _ = await rig.store.undo() }
        await editorSettled(sc, editor) { editor.regions[1].bounds.x == 1920 }
        XCTAssertEqual(editor.regions[1].bounds, RegionBounds(x: 1920, y: 0, width: 1920, height: 1080))
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
        XCTAssertEqual(editor.regionRoutePopups[1].titleOfSelectedItem, "Output 2")
    }

    /// Command-N on the Output Routing or Video Outputs page opens that page's add pop-up (QLab 5
    /// manual pp. 226, 233), not New Workspace; on another page it goes on to the menu.
    func testCommandNOnAVideoPageOpensItsAddPopUp() async throws {
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
        sc.tabs.selectTabViewItem(withIdentifier: "videoRouting")
        XCTAssertTrue(window.contentView?.performKeyEquivalent(with: key) == true)
        XCTAssertTrue(opened.last === sc.videoRoutingPane.addPopup)
        sc.tabs.selectTabViewItem(withIdentifier: "video")
        XCTAssertTrue(window.contentView?.performKeyEquivalent(with: key) == true)
        XCTAssertTrue(opened.last === sc.videoPane.addPopup)
        sc.tabs.selectTabViewItem(withIdentifier: "general")
        XCTAssertFalse(window.contentView?.performKeyEquivalent(with: key) == true)
        XCTAssertEqual(opened.count, 2)
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
        choose(loc("settings.video.scaling.fit"), in: routing.scalingPopups[0])
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
        guard case .success = await rig.client.send({ .close(id: $0, doc: rig.doc, discardChanges: false) })
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
        guard case .success = await rig.client.send({ .close(id: $0, doc: rig.doc, discardChanges: true) })
        else { return XCTFail("close") }
        await showsGuides("V1", [black, black, black], "gone with the workspace")
        _ = try await engine.open(path: path)
        await showsGuides("V1", [black, black, black], "opened again: none")
    }
}
