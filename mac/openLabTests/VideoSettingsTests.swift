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
        let rig = try await Rig.open("Demo", on: engine) { folder in
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
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        await sc.present().value
        return (rig, sc)
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

    /// Flow 1: a new workspace on a Mac with two displays: the Video page lists one stage per
    /// display, named, with its pixel size and its display.
    func testANewWorkspaceListsOneStagePerDisplayWithItsSizeAndDisplay() async throws {
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
        await settled(sc) { pane.nameFields.count == 2 && pane.displayPopups.count == 2 }
        XCTAssertEqual(pane.nameFields.map(\.stringValue), ["Stage 1", "Stage 2"])
        XCTAssertEqual(
            pane.shown?.map { "\($0.width) × \($0.height)" }, ["1920 × 1080", "1280 × 720"])
        XCTAssertEqual(
            pane.displayPopups.map { $0.titleOfSelectedItem }, ["Virtual V1 (1920 × 1080)", "Virtual V2 (1280 × 720)"])
        XCTAssertTrue(pane.emptyLabel.isHidden)
        XCTAssertEqual(pane.warningLabels.map(\.stringValue), ["", ""], "no warnings: both shown or hidden")
    }

    /// Flow 2: a stage's display pop-up changed: the stage goes to that display at once (its state
    /// says shown on it), and the choice is kept for this machine.
    func testChangingAStagesDisplayPopUpMovesItToThatDisplay() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.displayPopups.count == 1 }
        let popup = pane.displayPopups[0]
        XCTAssertEqual(popup.titleOfSelectedItem, loc("settings.video.displayNone"))
        popup.selectItem(withTitle: "Virtual V2 (1280 × 720)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { pane.shown?.first?.display == "V2" }
        XCTAssertEqual(pane.displayPopups.first?.titleOfSelectedItem, "Virtual V2 (1280 × 720)")
        XCTAssertEqual(rig.store.videoOutputs.stages.first?.mapping?.display, "V2")
    }

    /// Flow 3: New Stage adds a stage with no display; its name is typed in place; it is removed;
    /// each is one undo step.
    func testNewStageRenameInPlaceAndRemoveAreOneUndoStepEach() async throws {
        let (rig, sc) = try await open()
        let pane = sc.videoPane
        await settled(sc) { pane.shown?.isEmpty == true }
        XCTAssertFalse(pane.emptyLabel.isHidden, "an empty table says so")
        pane.addButton.performClick(nil)
        await settled(sc) { pane.nameFields.count == 1 }
        XCTAssertEqual(pane.nameFields[0].stringValue, String(format: loc("settings.video.newName"), 1))
        XCTAssertEqual(pane.displayPopups[0].titleOfSelectedItem, loc("settings.video.displayNone"))
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

    /// Flow 4: the keep-black box: on, the stage on its display stays up black between cues
    /// (shown); off, its window is hidden while nothing plays.
    func testTheKeepBlackBoxShowsBlackOrHidesTheStageBetweenCues() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.displayPopups.count == 1 }
        let popup = pane.displayPopups[0]
        popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
        _ = popup.sendAction(popup.action, to: popup.target)
        await settled(sc) { rig.store.videoOutputs.stages.first?.state?.state == "hidden" }
        pane.blackBoxes[0].performClick(nil)
        await settled(sc) { rig.store.videoOutputs.stages.first?.state?.state == "shown" }
        XCTAssertEqual(pane.blackBoxes[0].state, .on)
        pane.blackBoxes[0].performClick(nil)
        await settled(sc) { rig.store.videoOutputs.stages.first?.state?.state == "hidden" }
    }

    /// Flow 7: a show whose stage has no display on this Mac loads; the stage carries a warning in
    /// its row and in the workspace's Warnings list; choosing a display once clears both.
    func testAStageWithoutADisplayWarnsInItsRowAndTheWarningsUntilOneIsChosen() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.warningLabels.first?.stringValue == loc("settings.video.warn.noDisplay") }
        let warnings = WarningRows.make(store: rig.store).map(\.title)
        XCTAssertTrue(
            warnings.contains(String(format: loc("warnings.stage"), "Cyc", loc("settings.video.warn.noDisplay"))),
            "\(warnings)")
        let popup = pane.displayPopups[0]
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
        func check(_ why: String) {
            let rows = pane.shown ?? []
            let warned = rows.filter { $0.warning != nil }.count
            XCTAssertEqual(pane.holder.subviews, [pane.grid], "\(why): one table")
            XCTAssertEqual(pane.grid.numberOfRows, 1 + rows.count + warned, why)
            let controls: [NSView] =
                pane.nameFields + pane.layerFields + pane.displayPopups + pane.blackBoxes + pane.removeButtons
            XCTAssertEqual(pane.grid.subviews.count, 6 + controls.count + warned, why)
            XCTAssertTrue(controls.allSatisfy { $0.superview === pane.grid }, why)
            XCTAssertEqual(pane.nameFields.map(\.stringValue), rows.map(\.name), why)
        }
        for round in 1...2 {
            await settled(sc) { pane.shown?.count == 2 && pane.warningLabels.allSatisfy { !$0.stringValue.isEmpty } }
            check("round \(round): two stages, each with a warning")
            let popup = pane.displayPopups[0]
            popup.selectItem(withTitle: "Virtual V1 (1920 × 1080)")
            _ = popup.sendAction(popup.action, to: popup.target)
            await settled(sc) { pane.shown?.first?.warning == nil }
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

    /// Flow 8: two stages on the same display: the layer typed for each orders them (the engine
    /// draws the higher on top); a layer outside 0 to 1000 is refused with a line and sends nothing.
    func testTheLayerFieldOrdersStagesOnOneDisplayAndRefusesOutOfRangeValues() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Mary", 1920, 1080), (stageB, "Angelica", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.layerFields.count == 2 }
        let field = pane.layerFields[1]
        field.stringValue = "100"
        pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
        await settled(sc) { pane.shown?.last?.layer == 100 }
        let stage = rig.store.model?.stages.last
        XCTAssertEqual(stage?.raw["layer"]?.doubleValue, 100)
        let bad = pane.layerFields[0]
        bad.stringValue = "1001"
        pane.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: bad))
        XCTAssertEqual(pane.messageLabel.stringValue, loc("settings.video.badLayer"))
        XCTAssertEqual(bad.stringValue, "1")
    }
}
