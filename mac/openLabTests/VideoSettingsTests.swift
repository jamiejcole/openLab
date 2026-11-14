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
            XCTAssertEqual(pane.grid.numberOfRows, rows.count + warned, why)
            let controls: [NSView] =
                pane.nameFields + pane.layerFields + pane.displayPopups + pane.blackBoxes + pane.removeButtons
            // Per row its controls and its pixel-size label; the warning lines (the titles are not
            // in the rows' grid).
            XCTAssertEqual(pane.grid.subviews.count, controls.count + rows.count + warned, why)
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

    /// A push that comes while a stage's name is typed in is drawn at once, and the name being
    /// typed stays as typed, caret and all, uncommitted; ending the typing commits it.
    func testWhatChangesWhileANameIsTypedIsDrawnAtOnceAndTheTypingGoesOn() async throws {
        let (rig, sc) = try await open(stages: [(stageA, "Cyc", 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.warningLabels.first?.stringValue == loc("settings.video.warn.noDisplay") }
        try startTyping("Cyclo", in: pane.nameFields[0], sc)
        guard case .success = await rig.store.setStageMapping(ModelID(stageA), display: "V1") else {
            return XCTFail("mapping")
        }
        await caughtUp(rig, sc, "drawn while the name is typed") { pane.warningLabels.first?.stringValue == "" }
        let field = pane.nameFields[0]
        XCTAssertEqual(field.currentEditor()?.string, "Cyclo", "still typing, as typed")
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
        for popup in pane.displayPopups {
            XCTAssertLessThanOrEqual(
                popup.intrinsicContentSize.width, popup.frame.width, popup.titleOfSelectedItem ?? "")
        }
        for c in 0..<6 { XCTAssertEqual(pane.header.column(at: c).width, pane.grid.column(at: c).width, "column \(c)") }
        let least = pane.displayWidth
        window.setContentSize(NSSize(width: 1400, height: 1000))
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertGreaterThan(pane.displayWidth, least)
        XCTAssertEqual(pane.displayWidth, VideoSettingsPane.mostDisplayWidth)
        XCTAssertGreaterThan(pane.nameWidth, VideoSettingsPane.columns[0])
        for c in 0..<6 { XCTAssertEqual(pane.header.column(at: c).width, pane.grid.column(at: c).width, "column \(c)") }
    }

    /// A long stage name is cut with an ellipsis in its field and in the Warnings list, where the
    /// reason after it stays whole.
    func testALongStageNameIsShortenedAndTheReasonStays() async throws {
        let long = "A very long stage name for the upstage cyclorama projector left half"
        let (rig, sc) = try await open(stages: [(stageA, long, 1920, 1080)])
        let pane = sc.videoPane
        await settled(sc) { pane.nameFields.count == 1 && pane.shown?.first?.warning != nil }
        XCTAssertEqual(pane.nameFields[0].lineBreakMode, .byTruncatingTail)
        let row = try XCTUnwrap(WarningRows.make(store: rig.store).first { $0.id == "v:\(stageA)" })
        XCTAssertTrue(row.title.hasSuffix(loc("settings.video.warn.noDisplay")), row.title)
        XCTAssertTrue(row.title.contains("…"), row.title)
        XCTAssertFalse(row.title.contains(long), row.title)
    }

    /// Flow 7 without the settings window: a show whose stage has no display on this Mac is
    /// opened and its main window alone shows the stage's warning (the Warnings list and the
    /// footer's count), from what the engine sends on connecting; and again on a new engine (an
    /// engine relaunched under the app: a fresh connection and store).
    func testAStageWithoutADisplayWarnsInTheMainWindowWithoutTheSettingsWindow() async throws {
        let warning = String(format: loc("warnings.stage"), "Cyc", loc("settings.video.warn.noDisplay"))
        var engines = [engine!]
        for round in 1...2 {
            let rig = try await openShow(stages: [(stageA, "Cyc", 1920, 1080)], on: engines[round - 1])
            let wc = WorkspaceWindowController(store: rig.store)
            wc.window?.contentView?.layoutSubtreeIfNeeded()
            await assertEventually("round \(round): the stage's warning is listed") {
                // The stepped engine's clock moves only when the test moves it.
                try? await rig.advance(0.2)
                return WarningRows.make(store: rig.store).contains { $0.title == warning }
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
        await settled(sc) { pane.layerFields.count == 2 }
        for (i, stage) in [stageA, stageB].enumerated() {
            let popup = pane.displayPopups[i]
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
}
