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
            ws["stages"] = stages.map { s in
                ["id": s.0, "name": s.1, "width": s.2, "height": s.3, "keep_black": false, "layer": 1] as [String: Any]
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

    /// Flow 1: a new workspace on a Mac with two displays: the Video page lists one stage, Stage 1,
    /// with the main display's pixel size, shown on that display.
    func testANewWorkspaceListsOneStageOnTheMainDisplay() async throws {
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
        await sc.present().value
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.nameFields.count == 1 && routing.displayPopups.count == 1 }
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1"])
        XCTAssertEqual(pane.shown?.map { "\($0.width) × \($0.height)" }, ["1920 × 1080"])
        XCTAssertEqual(routing.displayPopups.map { $0.titleOfSelectedItem }, ["Virtual V1 (1920 × 1080)"])
        XCTAssertTrue(pane.emptyLabel.isHidden)
        XCTAssertEqual(routing.warningLabels.map(\.stringValue), [""], "no warning: shown")
    }

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
        XCTAssertEqual(sc.videoRoutingPane.cols, [.name, .size, .display])
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
        await settled(sc) { pane.shown?.first?.display == "V2" }
        XCTAssertEqual(routing.displayPopups.first?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        XCTAssertEqual(rig.store.videoOutputs.stages.first?.mapping?.display, "V2")
    }

    /// Flow 3: New Stage adds a stage with no display; its name is typed in place; it is removed;
    /// each is one undo step.
    func testNewStageRenameInPlaceAndRemoveAreOneUndoStepEach() async throws {
        let (rig, sc) = try await open()
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.shown?.isEmpty == true }
        XCTAssertFalse(pane.emptyLabel.isHidden, "an empty table says so")
        pane.addButton.performClick(nil)
        await settled(sc) { pane.nameFields.count == 1 }
        XCTAssertEqual(pane.nameFields[0].stringValue, String(format: loc("settings.video.newName"), 1))
        XCTAssertEqual(routing.displayPopups[0].titleOfSelectedItem, loc("settings.video.displayNone"))
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

    /// Flow 4: the keep-black box sets the stage's keep-black; its black comes only with the first
    /// picture played on it (#708; black between cues itself: the engine's video_cues.rs).
    func testTheKeepBlackBoxSetsTheStagesBlackWhichComesOnlyWithItsFirstPicture() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { routing.displayPopups.count == 1 }
        let popup = routing.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { rig.store.videoOutputs.stages.first?.state?.state == "hidden" }
        pane.blackBoxes[0].performClick(nil)
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(true) }
        XCTAssertEqual(pane.blackBoxes[0].state, .on)
        // #708: no picture has played on the stage since the show opened, so its black is not up yet:
        // ticking the box never covers a display.
        await settled(sc) { rig.store.videoOutputs.stages.first?.panicked == true }
        XCTAssertEqual(rig.store.videoOutputs.stages.first?.state?.state, "hidden")
        pane.blackBoxes[0].performClick(nil)
        await settled(sc) { rig.store.model?.stages.first?.raw["keep_black"] == .bool(false) }
        XCTAssertEqual(rig.store.videoOutputs.stages.first?.state?.state, "hidden")
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
                $0.title == String(format: loc("warnings.stage"), "Cyc")
                    && $0.detail == loc("warnings.stage.noDisplay")
            }, "\(warnings)")
        let popup = routing.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { pane.shown?.first?.warning == nil }
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
            let warned = rows.filter { $0.warning != nil }.count
            XCTAssertEqual(pane.holder.subviews, [pane.grid], "\(why): one table")
            XCTAssertEqual(pane.grid.numberOfRows, rows.count, why)
            let controls: [NSView] =
                pane.nameFields + pane.layerFields + pane.blackBoxes + pane.monitorButtons + pane.removeButtons
            // Per row its controls and its pixel-size field pair (the titles are not in the rows' grid).
            XCTAssertEqual(pane.grid.subviews.count, controls.count + rows.count, why)
            XCTAssertTrue(controls.allSatisfy { $0.superview === pane.grid }, why)
            XCTAssertEqual(pane.nameFields.map(\.stringValue), rows.map(\.name), why)
            // The routing page: its row's controls and label cells, and a line for each warning.
            XCTAssertEqual(routing.grid.numberOfRows, rows.count + warned, why)
            XCTAssertEqual(routing.grid.subviews.count, 3 * rows.count + warned, why)
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
            pane.addButton.performClick(nil)
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
            _ = await rig.store.setStageMapping(ModelID(stageA), display: nil)
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
        guard case .success = await rig.store.setStageMapping(ModelID(stageA), display: "V1") else {
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
        pane.addButton.performClick(nil)
        await caughtUp(rig, sc, "the add is drawn") { pane.nameFields.count == 2 }
        XCTAssertEqual(pane.nameFields.first?.stringValue, "Cyc 2")
        // Another client adds a stage while a name is typed: drawn at once, the typing committed.
        try startTyping("Cyc 3", in: pane.nameFields[0], sc)
        _ = await rig.store.send(
            command: .addStage(id: ModelID(stageB), name: "Elsewhere", width: 640, height: 480, index: 2))
        await caughtUp(rig, sc, "the other client's add is drawn") { pane.nameFields.count == 3 }
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Cyc 3", "Stage 2", "Elsewhere"])
        XCTAssertEqual(pane.messageLabel.stringValue, "")
    }

    /// A layer typed out of range and then left by clicking the keep-black box: the box's edit is
    /// made, and the layer's reason stays shown (the box's edit does not clear it).
    func testABadLayerLeftByClickingTheBoxKeepsItsReason() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.layerFields.count == 1 }
        try startTyping("1500", in: pane.layerFields[0], sc)
        pane.blackBoxes[0].performClick(nil)
        await caughtUp(rig, sc, "the box's edit is made") { pane.shown?.first?.keepBlack == true }
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(pane.layerFields[0].stringValue, "1")
    }

    /// A name the engine refuses, left by clicking the keep-black box: the box's edit is made,
    /// and the engine's reason for the name is shown when it comes (the click does not outrun it).
    func testANameTheEngineRefusesLeftByClickingTheBoxShowsWhy() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 1 }
        try startTyping(String(repeating: "n", count: 1025), in: pane.nameFields[0], sc)
        pane.blackBoxes[0].performClick(nil)
        await caughtUp(rig, sc, "the box's edit is made") { pane.shown?.first?.keepBlack == true }
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
        await settled(sc) { pane.layerFields.count == 2 }
        func type(_ text: String, into field: NSTextField) {
            field.stringValue = text
            pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
        }
        // An accepted edit still on its way does not wipe the refusal typed after it.
        type("0", into: pane.layerFields[0])
        type("1001", into: pane.layerFields[1])
        await caughtUp(rig, sc, "the accepted layer is drawn") { pane.shown?.first?.layer == 0 }
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(pane.layerFields[1].stringValue, "1")
        type("abc", into: pane.layerFields[1])
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(pane.layerFields[1].stringValue, "1")
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
        let chosen = await rig.store.setStageMapping(first, display: "V1")
        guard case .success = chosen else { return XCTFail("\(chosen)") }
        await caughtUp(rig, sc, "the choice is drawn \(rig.store.videoOutputs)") {
            pane.shown?.first?.display == "V1"
        }
        window.contentView?.layoutSubtreeIfNeeded()
        for c in 0..<pane.cols.count {
            XCTAssertEqual(pane.header.column(at: c).width, pane.grid.column(at: c).width, "column \(c)")
        }
        // The routing page's display choice reads whole, and its titles line up with its columns.
        sc.tabs.selectTabViewItem(withIdentifier: "videoRouting")
        window.contentView?.layoutSubtreeIfNeeded()
        for popup in routing.displayPopups {
            XCTAssertLessThanOrEqual(
                popup.intrinsicContentSize.width, popup.frame.width, popup.titleOfSelectedItem ?? "")
        }
        for c in 0..<routing.cols.count {
            XCTAssertEqual(routing.header.column(at: c).width, routing.grid.column(at: c).width, "column \(c)")
        }
        // At the window's least size too.
        window.setContentSize(window.contentMinSize)
        window.contentView?.layoutSubtreeIfNeeded()
        for popup in routing.displayPopups {
            XCTAssertLessThanOrEqual(
                popup.intrinsicContentSize.width, popup.frame.width, "least size: \(popup.titleOfSelectedItem ?? "")")
        }
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

    /// A long stage name is cut with an ellipsis in its field and in the Warnings list, whose
    /// reason is a short line of its own.
    func testALongStageNameIsShortenedAndTheReasonStays() async throws {
        let long = "A very long stage name for the upstage cyclorama projector left half"
        let (rig, sc) = try await open(stages: [(stageA, long, 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 1 && pane.shown?.first?.warning != nil }
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
        let warning = String(format: loc("warnings.stage"), "Cyc")
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
        await settled(sc) { pane.layerFields.count == 2 }
        for (i, stage) in [stageA, stageB].enumerated() {
            let popup = routing.displayPopups[i]
            popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
            _ = popup.sendAction(popup.action, to: popup.target)
            await settled(sc) { pane.shown?[i].display == "V1" }
            pane.blackBoxes[i].performClick(nil)
            await settled(sc) { pane.shown?[i].keepBlack == true }
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
            let field = pane.layerFields[row]
            field.stringValue = layer
            pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
        }
        // Angelica raised to layer 100: Angelica's picture is on top.
        type("100", into: 1)
        await settled(sc) { pane.shown?.last?.layer == 100 }
        XCTAssertEqual(rig.store.model?.stages.last?.raw["layer"]?.doubleValue, 100)
        await assertEventually("Angelica on top") { await onTop() == 222 }
        // The layers swapped: Mary's picture is on top.
        type("200", into: 0)
        await settled(sc) { pane.shown?.first?.layer == 200 }
        await assertEventually("Mary on top") { await onTop() == 111 }
        // Out of range: refused in the page, nothing sent, the picture as it was.
        type("1001", into: 1)
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(pane.layerFields[1].stringValue, "100")
        let onTopNow = await onTop()
        XCTAssertEqual(onTopNow, 111)
    }

    // MARK: A stage's pixel size (#674)

    /// Types `text` into `field` and leaves it, as the operator's Tab or Return does.
    private func type(_ text: String, into field: NSTextField, _ pane: VideoSettingsPane) {
        field.stringValue = text
        pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
    }

    /// The colours display `display` shows at `points`.
    private func displayColours(_ display: String, _ points: [[Double]]) async -> [[UInt8]] {
        guard case .success(let ok) = await engine.request(.displayProbe(display: display, points: points)),
            case .stageProbe(_, let colours)? = ok.data
        else { return [] }
        return colours
    }

    /// #674 flow 1: a stage's size is changed on the Video page: its row shows the new size, its
    /// picture on the display is drawn at that size (a square stage on the wide V1: black bars at
    /// the sides, the test card between them), and one undo puts it back.
    func testAStagesSizeChangedOnTheVideoPageShowsInItsRowAndIsDrawnAtThatSizeInOneUndoStep() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        let routing = sc.videoRoutingPane
        await settled(sc) { pane.widthFields.count == 1 }
        XCTAssertEqual(pane.widthFields[0].stringValue, "1920")
        XCTAssertEqual(pane.heightFields[0].stringValue, "1080")
        let popup = routing.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { pane.shown?.first?.display == "V1" }
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
        type("1080", into: pane.widthFields[0], pane)
        await settled(sc) { pane.shown?.first?.width == 1080 }
        XCTAssertEqual(pane.widthFields[0].stringValue, "1080")
        XCTAssertEqual(pane.heightFields[0].stringValue, "1080")
        XCTAssertEqual(rig.store.model?.stages.first?.raw["width"]?.doubleValue, 1080)
        XCTAssertEqual(pane.messageLabel.stringValue, "")
        await assertEventually("a square stage in V1's middle") {
            await self.displayColours("V1", points) == [black, red]
        }
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.first?.width == 1920 }
        XCTAssertEqual(pane.widthFields[0].stringValue, "1920")
        await assertEventually("back across V1") { await self.displayColours("V1", points) == [red, red] }
    }

    /// #674 flow 2: a size of 0, above the limit, or not a number is refused with the reason shown
    /// where every other refused value's is, and the old size stays (nothing is sent).
    func testASizeOfZeroAboveTheLimitOrNotANumberIsRefusedWithTheReasonAndTheOldSizeStays() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.widthFields.count == 1 }
        for (text, width) in [("0", true), ("16385", false), ("abc", true), ("-5", false), ("", true)] {
            let field = width ? pane.widthFields[0] : pane.heightFields[0]
            type(text, into: field, pane)
            XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badSize"), text)
            XCTAssertEqual(field.stringValue, width ? "1920" : "1080", "\(text): the old size stays")
        }
        await sc.videoPane.pending?.value
        XCTAssertEqual(rig.store.model?.stages.first?.raw["width"]?.doubleValue, 1920)
        XCTAssertEqual(rig.store.model?.stages.first?.raw["height"]?.doubleValue, 1080)
        XCTAssertEqual(rig.store.document?.canUndo, false, "nothing was sent")
        // The largest size is taken, and a good edit clears the reason.
        type("16384", into: pane.heightFields[0], pane)
        await settled(sc) { pane.shown?.first?.height == 16384 }
        XCTAssertEqual(pane.messageLabel.stringValue, "")
        XCTAssertEqual(pane.heightFields[0].stringValue, "16384")
    }

    /// A size typed and committed with Return, then undone from the workspace window: the field
    /// shows the size the undo put back, and a click on another control of the page sends no size
    /// (the undone one stays).
    func testASizeUndoneAfterReturnIsShownAndAClickElsewhereNeverSendsItAgain() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080), (stageB, "Side", 1280, 720)])
        let pane = sc.videoPane
        await settled(sc) { pane.heightFields.count == 2 }
        try startTyping("400", in: pane.heightFields[0], sc)
        let editor = try XCTUnwrap(pane.heightFields[0].currentEditor() as? NSTextView)
        editor.insertNewline(nil)
        await settled(sc) { pane.shown?.first?.height == 400 }
        _ = await rig.store.undo()
        await settled(sc) { pane.shown?.first?.height == 1080 }
        XCTAssertEqual(pane.heightFields[0].stringValue, "1080", "the undone size is shown")
        XCTAssertNotEqual(pane.heightFields[0].currentEditor()?.string, "400")
        pane.blackBoxes[1].performClick(nil)
        await settled(sc) { pane.shown?.last?.keepBlack == true }
        XCTAssertEqual(rig.store.model?.stages.first?.raw["height"]?.doubleValue, 1080, "no size sent again")
        XCTAssertEqual(pane.heightFields[0].stringValue, "1080")
        XCTAssertEqual(pane.messageLabel.stringValue, "")
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
}
