// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// AUD-07, AUD-21, WS-07, PB-03, PB-11: the workspace settings window (the footer gear, File >
/// Workspace Settings) with its output patch table, the patch editor and the panic fade, all through
/// the store against the real engine. Its one device is the virtual one (2 outputs); losing it stands
/// for an unplugged interface. Patch mappings are the engine's own settings, so each test has an
/// engine of its own.
@MainActor
final class WorkspaceSettingsTests: XCTestCase {
    private let patch = ModelID("00000000-0000-0000-0000-000000000001")
    private let device = "openLab Virtual Output"
    var engine: TestEngine!

    override func setUp() async throws {
        engine = try await TestEngine.start()
    }

    override func tearDown() async throws { engine?.stop() }

    /// The settings window of the demo show (or the show `edit` makes of it), read from the engine.
    private func open(edit: ((URL) throws -> Void)? = nil) async throws -> (Rig, WorkspaceSettingsWindowController) {
        let rig = try await Rig.open("Demo", on: engine, edit: edit)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        await sc.present().value
        sc.tabs.selectTabViewItem(withIdentifier: "audio")
        return (rig, sc)
    }

    /// Lets the engine run for a while: a status (about once a second) reports its device.
    private func status(_ rig: Rig) async throws {
        try await rig.advance(1.5)
        await assertEventually("a status came") { rig.store.status != nil }
    }

    /// The device goes away (as an interface unplugged): its cues stop, the device list loses it.
    private func loseDevice(_ rig: Rig) async throws {
        guard case .success = await engine.request(.loseDevice) else { return XCTFail("lose_device") }
        try await rig.advance(1.5)  // the next list read and status see it gone
    }

    private func requests(_ rig: Rig) -> [Request] {
        rig.wire.sent.compactMap { if case .request(_, let r) = $0 { return r } else { return nil } }
    }

    func commands(_ rig: Rig) -> [JSONValue] { rig.wire.commands.map(\.json) }

    /// The engine's mapping of the demo's patch, read back from it.
    private func mapping(_ rig: Rig, of id: ModelID? = nil) async throws -> PatchMapping? {
        guard case .success(let entries) = await rig.store.listPatchMappings() else {
            XCTFail("list_patch_mappings")
            return nil
        }
        return entries.first { $0.patch == (id ?? patch) }?.mapping
    }

    /// Picks `title` in a pop-up as a click would.
    private func pick(_ title: String, in popup: NSPopUpButton) throws {
        let item = try XCTUnwrap(popup.item(withTitle: title), "\(popup.itemTitles)")
        popup.select(item)
        _ = popup.target?.perform(popup.action, with: popup)
    }

    /// The editor opened from row `n`'s Edit button (from 1), its first read of the engine done.
    private func edit(row n: Int = 1, in sc: WorkspaceSettingsWindowController) async throws
        -> PatchEditorWindowController
    {
        await assertEventually("row \(n) is drawn") { sc.patchEditButtons.count >= n }
        guard sc.patchEditButtons.count >= n else { throw TestEngineError.notConnected }
        sc.patchEditButtons[n - 1].performClick(nil)
        let editor = try XCTUnwrap(sc.patchEditor, "Edit opened the patch editor")
        await editor.editing?.value
        return editor
    }

    // MARK: Opening

    func testTheFooterGearOpensTheSettingsWindowAndReadsTheEngine() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertTrue(wc.footer.settingsButton.isEnabled)
        wc.footer.settingsButton.performClick(nil)
        let sc = wc.settingsWindow
        XCTAssertTrue(sc.window?.isVisible == true)
        await assertEventually { sc.patchDevicePopups.first?.itemTitles.contains(self.device) == true }
        sc.close()
    }

    func testTheFileMenuHasWorkspaceSettingsOnCommandComma() {
        let item = MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == loc("menu.file") }?.items
            .first { $0.action == #selector(AppDelegate.showWorkspaceSettings(_:)) }
        XCTAssertEqual(item?.title, loc("menu.file.workspaceSettings"))
        XCTAssertEqual(item?.keyEquivalent, ",")
    }

    // MARK: The patch table

    /// Checklist: Workspace Settings, Audio → a table with one row per output patch: its name (typed
    /// in place), a device pop-up, an Edit button and a remove button; a button below adds a patch;
    /// the level limits sit on the same page.
    func testAudioSettingsShowAPatchTableWithNameDeviceEditRemoveAddAndTheLimits() async throws {
        let (rig, sc) = try await open()
        let audio = try XCTUnwrap(sc.tabs.selectedTabViewItem?.view)
        XCTAssertEqual(sc.patchNameFields.map(\.stringValue), ["Main"])
        XCTAssertEqual(sc.patchDevicePopups.first?.titleOfSelectedItem, device)
        XCTAssertEqual(sc.patchDevicePopups.first?.itemTitles, [loc("settings.audio.noDevice"), device])
        XCTAssertEqual(sc.patchEditButtons.first?.title, loc("settings.audio.edit"))
        XCTAssertFalse(try XCTUnwrap(sc.patchRemoveButtons.first).isEnabled, "the last patch cannot go")
        for control in [sc.patchAddButton, sc.maxLimitField, sc.minLimitField] + sc.patchNameFields as [NSView] {
            XCTAssertTrue(control.isDescendant(of: audio), "\(control) is on the Audio page")
        }

        // A button adds a patch, which gets its own row.
        sc.patchAddButton.performClick(nil)
        await assertEventually { sc.patchNameFields.count == 2 }
        XCTAssertEqual(sc.patchNameFields[1].stringValue, String(format: loc("settings.audio.newPatchName"), 2))
        XCTAssertEqual(rig.store.model?.patches.count, 2)

        // The name is typed in place.
        try type("Surrounds", into: sc.patchNameFields[1])
        await assertEventually { rig.store.model?.patches.last?.raw["name"]?.stringValue == "Surrounds" }
        await assertEventually { sc.patchNameFields.map(\.stringValue) == ["Main", "Surrounds"] }

        // Its remove button removes it.
        XCTAssertTrue(sc.patchRemoveButtons[1].isEnabled)
        sc.patchRemoveButtons[1].performClick(nil)
        await assertEventually { sc.patchNameFields.count == 1 }
        XCTAssertEqual(rig.store.model?.patches.count, 1)
        // The limits are the workspace's.
        XCTAssertEqual(sc.maxLimitField.stringValue, "12")
        XCTAssertEqual(sc.minLimitField.stringValue, "-60")
    }

    /// Checklist: a different device is picked in a row while no cue plays → the next cue on that
    /// patch is heard on that device; no restart notice is shown. The virtual output is the only
    /// device, so the patch starts on none and is put on it.
    func testADevicePickedInARowPlaysTheNextCueThereWithNoRestartNotice() async throws {
        let (rig, sc) = try await open(edit: sounding())
        try pick(loc("settings.audio.noDevice"), in: sc.patchDevicePopups[0])
        await assertEventually { sc.patchProblemLabels.first?.stringValue == loc("settings.audio.problem.unmapped") }
        let before = try await mapping(rig)
        XCTAssertNil(before)

        try pick(device, in: sc.patchDevicePopups[0])
        await assertEventually { sc.patchProblemLabels.first?.stringValue == "" }
        XCTAssertEqual(sc.patchDevicePopups.first?.titleOfSelectedItem, device)
        let after = try await mapping(rig)
        XCTAssertEqual(after, PatchMapping(device: device, channels: [0, 1]))
        await assertEventually { sc.patchProblemLabels.first?.stringValue == "" }
        XCTAssertTrue(try XCTUnwrap(sc.patchGrid.cell(for: sc.patchProblemLabels[0])?.row).isHidden)

        let meter = MeterView()
        meter.bank = rig.store.meters  // a meter on screen, as the editor's: the bank keeps peaks
        meter.source = .output(patch)
        let shipped = MeterView.defaultWindowIsCovered
        MeterView.defaultWindowIsCovered = { _, _ in false }
        addTeardownBlock { @MainActor in MeterView.defaultWindowIsCovered = shipped }
        sc.window?.contentView?.addSubview(meter)
        guard case .success = await rig.store.transport(.start(cue: intro)) else { return XCTFail("start") }
        let heard = try await peak(rig, channel: 0)
        XCTAssertGreaterThan(heard, 0.05, "the next cue is heard on the device")
        XCTAssertFalse(
            texts(sc.window?.contentView).contains { $0.contains(loc("settings.audio.restartNeeded")) },
            "no restart notice")
        meter.removeFromSuperview()
    }

    /// A device whose usable outputs are fewer than the patch's cue outputs is refused before
    /// anything is sent, saying how many it has.
    func testADeviceWithTooFewOutputsIsRefusedBeforeAnythingIsSent() async throws {
        let (rig, sc) = try await open { show in
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                guard var patches = ws["patches"]?.arrayValue, var outputs = patches.first?["outputs"]?.arrayValue,
                    let first = outputs.first
                else { return XCTFail("the demo show has no patch") }
                outputs.append(first)
                patches[0].set("outputs", .array(outputs))
                ws.set("patches", .array(patches))
            }
        }
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: nil)
        let row = try XCTUnwrap(sc.model.patches.first)
        XCTAssertEqual(row.outputCount, 3)
        let before = requests(rig).count
        await sc.model.setMapping(row, device: device)
        XCTAssertEqual(requests(rig).count, before, "nothing is sent for a mapping that cannot fit")
        XCTAssertEqual(sc.model.message, String(format: loc("settings.audio.tooFewChannels"), device, 3, 2))
    }

    func testPickingTheDeviceAPatchAlreadyUsesKeepsItsRouting() async throws {
        let (rig, sc) = try await open()
        await sc.model.route(try XCTUnwrap(sc.model.patches.first), output: 1, to: 0)
        let sentBefore = requests(rig).count
        try pick(device, in: sc.patchDevicePopups[0])
        await settle()
        let kept = try await mapping(rig)
        XCTAssertEqual(kept, PatchMapping(device: device, channels: [0, 0]))
        XCTAssertEqual(requests(rig).count, sentBefore + 1, "the same device again sends nothing (one read back)")
    }

    /// The device was unplugged since the window read the list: the engine refuses the mapping.
    func testARefusedMappingShowsTheEnginesReasonAndKeepsTheOldOne() async throws {
        let (rig, sc) = try await open()
        let before = try await mapping(rig)
        try await loseDevice(rig)
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: device, channels: [1, 0])
        sc.render()
        XCTAssertTrue(sc.messageLabel.stringValue.contains(device), sc.messageLabel.stringValue)
        let after = try await mapping(rig)
        XCTAssertEqual(after, before, "the old mapping stays")
    }

    func testAnUnchangedStatusRebuildsNothing() async throws {
        let (rig, sc) = try await open()
        try await status(rig)
        await settle()
        let drawn = sc.renderCount
        try await rig.advance(3)  // three more statuses, the same device in the same state
        await settle()
        XCTAssertEqual(sc.renderCount, drawn, "same device, same state: nothing to redraw")
        try await loseDevice(rig)
        await assertEventually { sc.renderCount > drawn }
    }

    /// #449: a menu that began tracking while the window was closed (its end never heard) does not
    /// freeze the controls once the window is shown again.
    func testAMenuOpenedWhileTheWindowWasClosedDoesNotFreezeItWhenReopened() async throws {
        let (rig, sc) = try await open()
        try await status(rig)
        await settle()
        sc.close()
        NotificationCenter.default.post(name: NSMenu.didBeginTrackingNotification, object: NSMenu())
        sc.showWindow(nil)
        await settle()
        let drawn = sc.renderCount
        try await loseDevice(rig)
        await assertEventually("the reopened window rebuilds") { sc.renderCount > drawn }
        sc.close()
    }

    /// A popup's menu open on the shown window holds the rebuild until it closes.
    func testAMenuOpenOnTheShownWindowHoldsTheRebuildUntilItCloses() async throws {
        let (rig, sc) = try await open()
        try await status(rig)
        await settle()
        let drawn = sc.renderCount
        let menu = NSMenu()
        NotificationCenter.default.post(name: NSMenu.didBeginTrackingNotification, object: menu)
        try await loseDevice(rig)
        await settle()
        XCTAssertEqual(sc.renderCount, drawn, "no rebuild under an open menu")
        NotificationCenter.default.post(name: NSMenu.didEndTrackingNotification, object: menu)
        await assertEventually("it rebuilds once the menu closes") { sc.renderCount > drawn }
        sc.close()
    }

    // MARK: The patch editor

    /// Checklist: Edit on a row → a patch editor window with the patch name, the cue-output count and
    /// the device at the top, and tabs for cue-output names, the routing matrix (cue outputs down,
    /// device outputs across, a main level) and device outputs.
    func testEditOnARowOpensTheEditorWithNameCountDeviceAndThreeTabs() async throws {
        let (_, sc) = try await open()
        let editor = try await edit(in: sc)
        XCTAssertTrue(editor.window?.isVisible == true)
        XCTAssertEqual(editor.patchID, patch)
        XCTAssertEqual(editor.nameField.stringValue, "Main")
        XCTAssertEqual(editor.countField.stringValue, "2")
        XCTAssertEqual(editor.devicePopup.titleOfSelectedItem, device)
        XCTAssertEqual(
            editor.tabs.tabViewItems.map(\.label),
            [loc("patchEditor.tab.cueOutputs"), loc("patchEditor.tab.routing"), loc("patchEditor.tab.deviceOutputs")])
        editor.window?.contentView?.layoutSubtreeIfNeeded()
        let top = [editor.nameField, editor.countField, editor.devicePopup].map { $0.convert($0.bounds, to: nil) }
        let tabsTop = editor.tabs.convert(editor.tabs.bounds, to: nil).maxY
        XCTAssertTrue(top.allSatisfy { $0.minY >= tabsTop }, "the three controls are above the tabs")

        // Cue outputs tab: a name box per cue output.
        XCTAssertEqual(editor.outputNameFields.count, 2)
        XCTAssertEqual(editor.outputNameFields.map(\.placeholderString), ["1", "2"])
        // Routing: a main level, cue outputs down, device outputs across; each routed to its own number.
        XCTAssertEqual(editor.mainField.stringValue, "0.0")
        XCTAssertEqual(editor.matrix.rowLabels, ["1", "2"])
        XCTAssertEqual(editor.matrix.deviceOutputs, 2)
        XCTAssertEqual(editor.matrix.routes, [0, 1])
        XCTAssertEqual(editor.outputFields.map(\.stringValue), ["0.0", "0.0"])
        // Device outputs: one row per output of the device, with what reaches it.
        XCTAssertEqual(
            editor.deviceOutputLabels.map(\.stringValue),
            [String(format: loc("patchEditor.feeds"), "1"), String(format: loc("patchEditor.feeds"), "2")])
        XCTAssertEqual(editor.problemLabel.stringValue, "")

        // A second Edit brings the same editor forward.
        sc.patchEditButtons[0].performClick(nil)
        XCTAssertTrue(sc.patchEditor === editor)
        editor.close()
    }

    /// Checklist: a cue output is renamed in the editor → that name is drawn under its fader on the
    /// Levels tab of a cue on that patch.
    func testARenamedCueOutputIsDrawnUnderItsFaderOnTheLevelsTab() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        editor.tabs.selectTabViewItem(withIdentifier: "cueOutputs")
        try type("Sub", into: editor.outputNameFields[1])
        await assertEventually { sc.model.outputNames(of: self.patch) == ["", "Sub"] }
        await assertEventually { editor.matrix.rowLabels == ["1", "2  Sub"] }

        let wc = WorkspaceWindowController(store: rig.store)
        wc.inspector.setSelection([intro])
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        await settle()
        let matrix = wc.inspector.audioLevels.matrix
        XCTAssertEqual(matrix.outputCount, 2)
        XCTAssertEqual([matrix.columnLabel(0), matrix.columnLabel(1)], ["1", "Sub"], "what is drawn under the faders")

        // Undone, the number is back.
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually { wc.inspector.audioLevels.matrix.columnLabel(1) == "2" }
        // An over-long name is refused with the engine's reason; the old one stays.
        try type(String(repeating: "x", count: 1025), into: editor.outputNameFields[0])
        await assertEventually { !editor.messageLabel.stringValue.isEmpty }
        await assertEventually { editor.outputNameFields[0].stringValue == "" }
        XCTAssertEqual(sc.model.outputNames(of: patch), ["", ""])
    }

    /// Checklist: the cue-output count is changed in the editor → the Levels tab of a cue on that
    /// patch shows that many outputs.
    func testAChangedCueOutputCountShowsThatManyOutputsOnTheLevelsTab() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        try type("1", into: editor.countField)
        await assertEventually { editor.outputFields.count == 1 }
        await editor.editing?.value
        try type("4", into: editor.countField)
        await editor.editing?.value
        await assertEventually("4 outputs (\(editor.messageLabel.stringValue))") { editor.outputFields.count == 4 }
        XCTAssertEqual(editor.outputNameFields.count, 4)
        XCTAssertEqual(editor.matrix.rowLabels.count, 4)
        // The routing on this machine follows: the kept output where it was, new ones on outputs 2, 1, 2.
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped, PatchMapping(device: device, channels: [0, 1, 0, 1]))
        XCTAssertEqual(editor.problemLabel.stringValue, "", "the patch still plays")

        let wc = WorkspaceWindowController(store: rig.store)
        wc.inspector.setSelection([intro])
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        await settle()
        XCTAssertEqual(wc.inspector.audioLevels.matrix.outputCount, 4)
        XCTAssertEqual(wc.inspector.audioLevels.faders.count, 5, "the main fader and four outputs")
    }

    /// The count changed in the editor, then undone (Edit > Undo): the patch keeps playing on this
    /// machine; its row shows no warning and the next cue on it starts and is heard.
    func testACountChangeThenUndoLeavesThePatchPlaying() async throws {
        let shipped = MeterView.defaultWindowIsCovered
        MeterView.defaultWindowIsCovered = { _, _ in false }
        addTeardownBlock { @MainActor in MeterView.defaultWindowIsCovered = shipped }
        let (rig, sc) = try await open(edit: sounding())
        let editor = try await edit(in: sc)
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        editor.meter.updateLive()
        try type("3", into: editor.countField)
        await editor.editing?.value
        await assertEventually { editor.outputFields.count == 3 }
        let commandsSent = commands(rig).filter { $0["type"]?.stringValue == "set_patch_property" }.count
        XCTAssertEqual(commandsSent, 1, "one request")
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually { editor.outputFields.count == 2 }
        await assertEventually("the row shows no warning") { sc.patchProblemLabels.first?.stringValue == "" }
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped, PatchMapping(device: device, channels: [0, 1]))
        guard case .success = await rig.store.transport(.start(cue: intro)) else { return XCTFail("start") }
        let heard = try await peak(rig, channel: 0)
        XCTAssertGreaterThan(heard, 0.05, "the cue on the patch plays")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)
    }

    /// The patch's own device picked again resends the routing when it does not match the patch's
    /// count (it cannot play); one that matches sends nothing.
    func testPickingTheSameDeviceAgainMendsARoutingOfAnotherLength() async throws {
        let (_, sc) = try await open()
        let row = try XCTUnwrap(sc.model.patches.first)
        let stale = PatchRow(
            id: row.id, name: row.name, outputCount: row.outputCount,
            mapping: PatchMapping(device: device, channels: [0]), resolution: nil)
        await sc.model.setMapping(stale, device: device)
        XCTAssertNil(sc.model.message)
        XCTAssertEqual(sc.model.patches.first?.mapping, PatchMapping(device: device, channels: [0, 1]))
    }

    /// Typed counts outside 1 to 128 are refused before anything is sent; the count stays.
    func testACueOutputCountOutsideOneTo128IsRefused() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        let sent = commands(rig).count
        for bad in ["0", "129", "abc", "-3"] {
            try type(bad, into: editor.countField)
            await editor.editing?.value
            await assertEventually("\(bad): \(editor.countField.stringValue)") { editor.countField.stringValue == "2" }
            XCTAssertEqual(editor.messageLabel.stringValue, loc("patchEditor.countRange"), bad)
        }
        XCTAssertEqual(commands(rig).count, sent, "nothing out of range is sent")
    }

    /// 128 cue outputs (QLab's most): every row is in the editor and the matrix scrolls; meters and a
    /// cue's Levels tab show the first 64, and the editor says so.
    func testA128OutputPatchShowsEveryRowAndSaysWhatIsShownOfIt() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        await sc.model.setOutputCount(patch, 128)
        await editor.editing?.value
        editor.render()
        XCTAssertEqual(editor.outputFields.count, 128)
        XCTAssertEqual(editor.matrix.rowLabels.count, 128)
        XCTAssertEqual(editor.meter.fixedRows, 64)
        XCTAssertGreaterThan(editor.matrix.frame.height, PatchRoutingView.rowH * 128)
        XCTAssertTrue(editor.routingNote.stringValue.contains(loc("patchEditor.metersShown")))
        // Each level box is inside the matrix, on its own row.
        let last = try XCTUnwrap(editor.outputFields.last)
        XCTAssertTrue(editor.matrix.bounds.contains(last.frame))
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped?.channels.count, 128)
    }

    /// Checklist: an Audio cue is selected → the I/O tab's edit button, and a button on the Levels
    /// tab, open the editor for that cue's patch.
    func testTheIOAndLevelsTabButtonsOpenTheEditorForTheCuesPatch() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.inspector.setSelection([intro])
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "io")
        await settle()
        XCTAssertTrue(wc.inspector.editPatchButton.isEnabled)
        wc.inspector.editPatchButton.performClick(nil)
        let editor = try XCTUnwrap(wc.existingSettingsWindow?.patchEditor)
        XCTAssertEqual(editor.patchID, patch)
        XCTAssertTrue(editor.window?.isVisible == true)
        XCTAssertFalse(wc.settingsWindow.window?.isVisible == true, "only the editor opens")
        editor.close()

        wc.inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        await settle()
        let levelsButton = wc.inspector.audioLevels.editPatchButton
        XCTAssertTrue(levelsButton.isEnabled)
        levelsButton.performClick(nil)
        XCTAssertTrue(wc.settingsWindow.patchEditor === editor, "the same patch's editor")
        XCTAssertTrue(editor.window?.isVisible == true)
        editor.close()

        // In Show Mode the buttons are off, as every edit is.
        await wc.requestShowMode(true)
        await settle()
        XCTAssertFalse(wc.inspector.editPatchButton.isEnabled)
        XCTAssertFalse(levelsButton.isEnabled)
        XCTAssertNil(wc.editPatch(patch))
    }

    /// Checklist: a patch whose device is missing → its row and the editor show the warning; the
    /// status window lists it.
    func testAPatchWhoseDeviceIsMissingIsWarnedInItsRowTheEditorAndTheStatusWindow() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        try await loseDevice(rig)
        let warning = String(format: loc("settings.audio.problem.deviceMissing"), device)
        await assertEventually { sc.patchProblemLabels.first?.stringValue == warning }
        XCTAssertFalse(try XCTUnwrap(sc.patchGrid.cell(for: sc.patchProblemLabels[0])?.row).isHidden)
        await assertEventually("the device is shown as not connected once the list has lost it") {
            sc.patchDevicePopups.first?.titleOfSelectedItem
                == String(format: loc("settings.audio.notConnected"), self.device)
        }
        await assertEventually { editor.problemLabel.stringValue == warning }
        XCTAssertFalse(editor.problemLabel.isHidden)
        // The status window's warnings list names the patch and the device.
        let rows = WarningRows.make(store: rig.store)
        XCTAssertTrue(
            rows.contains { $0.title.contains("\"Main\"") && $0.title.contains(device) }, "\(rows.map(\.title))")

        // Back again: the warning goes from all three.
        guard case .success = await engine.request(.restoreDevice) else { return XCTFail("restore_device") }
        try await rig.advance(2)
        await assertEventually { sc.patchProblemLabels.first?.stringValue == "" }
        await assertEventually { editor.problemLabel.stringValue == "" }
        await assertEventually {
            !WarningRows.make(store: rig.store).contains { $0.title.contains("\"Main\"") }
        }
    }

    /// A connected device the engine cannot use (wrong rate, a failed first open): the row and the
    /// editor say why in the engine's words, never "not connected"; one whose first open is still
    /// under way says so.
    func testAConnectedDeviceThatCannotBeUsedIsWarnedWithItsReason() async throws {
        let (_, sc) = try await open()
        let row = try XCTUnwrap(sc.model.patches.first)
        func resolution(_ json: String) throws -> WireVariant {
            try JSONDecoder().decode(WireVariant.self, from: Data(json.utf8))
        }
        let unusable = PatchRow(
            id: row.id, name: row.name, outputCount: 2, mapping: row.mapping,
            resolution: try resolution(
                #"{"type":"device_missing","device":"openLab Virtual Output","reason":"it cannot play at 48000 Hz"}"#))
        XCTAssertEqual(
            sc.model.problem(of: unusable), .deviceUnusable(device: device, reason: "it cannot play at 48000 Hz"))
        XCTAssertEqual(
            sc.model.problemText(try XCTUnwrap(sc.model.problem(of: unusable))),
            String(format: loc("settings.audio.problem.deviceUnusable"), device, "it cannot play at 48000 Hz"))
        let opening = PatchRow(
            id: row.id, name: row.name, outputCount: 2, mapping: row.mapping,
            resolution: try resolution(#"{"type":"device_missing","device":"openLab Virtual Output"}"#))
        XCTAssertEqual(sc.model.problem(of: opening), .deviceOpening(device), "listed, no reason: opening")
    }

    /// Remove on a patch a cue plays through: our own sentence names the patch and the cue by number
    /// and name; nothing is sent, and the table does not move or shrink for the message.
    func testRemovingAPatchACueUsesSaysWhichCueAndKeepsTheTableInPlace() async throws {
        let (rig, sc) = try await open()
        sc.window?.setContentSize(try XCTUnwrap(sc.window?.contentMinSize))
        sc.patchAddButton.performClick(nil)
        await assertEventually { sc.patchRemoveButtons.count == 2 }
        let second = try XCTUnwrap(sc.model.patches.last?.id)
        guard
            case .success = await rig.store.send(
                command: .setProperty(intro, .kindFields(["patch": .string(second.raw)])))
        else { return XCTFail("repatch") }
        await settle()
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        let table = try XCTUnwrap(sc.patchGridHolder.enclosingScrollView)
        let before = table.convert(table.bounds, to: nil)
        let sent = commands(rig).count
        sc.patchRemoveButtons[1].performClick(nil)
        let expected = String(
            format: loc("settings.audio.patchInUse"), String(format: loc("settings.audio.newPatchName"), 2), "1  Intro")
        await assertEventually { sc.messageLabel.stringValue == expected }
        XCTAssertEqual(commands(rig).count, sent, "nothing sent")
        XCTAssertEqual(rig.store.model?.patches.count, 2)
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(table.convert(table.bounds, to: nil), before, "the table keeps its place and size")
    }

    /// A patch set to no device in the table is warned about in its row and the status window.
    func testAPatchSetToNoDeviceIsWarnedInItsRowAndTheStatusWindow() async throws {
        let (rig, sc) = try await open()
        try pick(loc("settings.audio.noDevice"), in: sc.patchDevicePopups[0])
        await assertEventually { sc.patchProblemLabels.first?.stringValue == loc("settings.audio.problem.unmapped") }
        try await rig.advance(0.1)
        await assertEventually {
            WarningRows.make(store: rig.store).contains { $0.title.contains("\"Main\"") }
        }
    }

    /// Checklist: a cue is playing on the patch while the editor's routing is changed → the change is
    /// heard at once and the meters in the editor move. Cue output 2 is silenced first, so device
    /// output 2 is quiet until cue output 1 is sent there.
    func testRoutingChangedUnderAPlayingCueIsHeardAtOnceAndTheEditorMetersMove() async throws {
        let (rig, _, editor) = try await playing(sounding())
        let window = try XCTUnwrap(editor.window)
        editor.meter.updateLive()
        XCTAssertTrue(editor.meter.isOnScreen, "the editor's meter is live")
        XCTAssertEqual(editor.meter.source, .output(patch))
        try type("-inf", into: editor.outputFields[1])
        await assertEventually { self.patchLevels(rig).outputs == [0, -120] }
        let quiet = try await peak(rig, channel: 1)
        XCTAssertEqual(quiet, 0, accuracy: 0.0001, "device output 2 is silent")

        // A click on row 1's box under device output 2.
        let box = editor.matrix.cellRect(row: 0, column: 1)
        try click(at: NSPoint(x: box.midX, y: box.midY), in: editor.matrix, window: window)
        await editor.editing?.value
        XCTAssertEqual(editor.matrix.routes, [1, 1])
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped, PatchMapping(device: device, channels: [1, 1]))
        let heard = try await peak(rig, channel: 1)
        XCTAssertGreaterThan(heard, 0.05, "device output 2 now carries cue output 1")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running, "the cue plays on")
        // What the editor's meter draws for row 2: its own source, read through the bank it is on.
        let source = try XCTUnwrap(editor.meter.source)
        XCTAssertTrue(editor.meter.bank === rig.store.meters)
        XCTAssertGreaterThan(rig.store.meters.visibleCount, 0, "the editor's meter is registered")
        XCTAssertGreaterThan(rig.store.meters.level(source, channel: 1), 0.05, "the editor's meter row moves")
        XCTAssertEqual(
            editor.deviceOutputLabels.map(\.stringValue),
            [loc("patchEditor.unused"), String(format: loc("patchEditor.feeds"), "1, 2")])
    }

    /// Esc while a patch name is typed in place, by the real key path (the app's key monitor, then
    /// the window): the app panics, as Esc does from any window (Q104), and the event goes on to the
    /// field, which drops the typing; the name stays and no rename is sent.
    func testEscInAPatchNameFieldKeepsTheNameAndOnlyPanics() async throws {
        let (rig, sc) = try await open(edit: sounding())
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        let app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { sc.window }
        session.start()
        defer { session.stop() }
        await assertEventually { session.connectionState == .connected }
        guard case .success = await rig.store.transport(.start(cue: intro)) else { return XCTFail("start") }
        try await rig.advance(0.5)
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)

        let window = try XCTUnwrap(sc.window)
        let field = sc.patchNameFields[0]
        window.makeFirstResponder(field)
        let editor = try XCTUnwrap(field.currentEditor())
        editor.string = "Typed"
        (field.delegate as? NSTextFieldDelegate)?.controlTextDidChange?(
            Notification(name: NSControl.textDidChangeNotification, object: field))
        let sent = commands(rig).count
        let esc = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false,
                keyCode: 53))
        let onward = app.handleKeyDown(esc)
        XCTAssertTrue(onward === esc, "Esc goes on to the field")
        window.sendEvent(esc)
        window.makeFirstResponder(nil)
        await settle()
        try await rig.advance(1)
        await assertEventually("the app panicked") { rig.store.cueStates[intro]?.state.isActive == false }
        XCTAssertEqual(rig.store.model?.patches.first?.raw["name"]?.stringValue, "Main")
        await assertEventually { sc.patchNameFields.first?.stringValue == "Main" }
        XCTAssertFalse(
            commands(rig).dropFirst(sent).contains { $0["property"]?["type"]?.stringValue == "name" },
            "no rename sent")
    }

    /// Without a device the matrix takes no clicks and says why.
    func testWithNoDeviceTheMatrixIsOffAndSaysWhy() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        try pick(loc("settings.audio.noDevice"), in: editor.devicePopup)
        await editor.editing?.value
        await assertEventually { !editor.matrix.isEnabled }
        XCTAssertTrue(editor.routingNote.stringValue.contains(loc("patchEditor.noDevice")))
        XCTAssertEqual(editor.matrix.routes, [nil, nil])
        let sent = requests(rig).count
        let box = editor.matrix.cellRect(row: 0, column: 1)
        try click(at: NSPoint(x: box.midX, y: box.midY), in: editor.matrix, window: try XCTUnwrap(editor.window))
        await editor.editing?.value
        XCTAssertEqual(requests(rig).count, sent, "nothing sent")
        // Picked again in the editor, it is routed to the first outputs.
        try pick(device, in: editor.devicePopup)
        await editor.editing?.value
        await assertEventually { editor.matrix.routes == [0, 1] }
    }

    // MARK: Smallest sizes

    /// Checklist: the table and the editor at their smallest window sizes → no control is clipped or
    /// overlaps. Two patches, one with a long name and a missing device, so the warning line shows.
    func testTheTableAndTheEditorAtTheirSmallestSizesClipNothing() async throws {
        let (rig, sc) = try await open()
        await sc.model.addPatch()
        await sc.model.renamePatch(
            try XCTUnwrap(sc.model.patches.last?.id), to: String(repeating: "Long name ", count: 20))
        try await loseDevice(rig)
        await sc.model.refresh()
        sc.lastDrawn = nil
        sc.render()
        let window = try XCTUnwrap(sc.window)
        window.setContentSize(window.contentMinSize)
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(
            window.frame.size,
            window.frameRect(forContentRect: NSRect(origin: .zero, size: window.contentMinSize)).size,
            "the long name did not widen it")
        assertNothingClippedOrOverlapping(in: sc.tabs.selectedTabViewItem?.view, "the Audio page")

        let editor = try await edit(row: 2, in: sc)
        let ew = try XCTUnwrap(editor.window)
        ew.setContentSize(PatchEditorWindowController.minSize)
        for tab in ["cueOutputs", "routing", "deviceOutputs"] {
            editor.tabs.selectTabViewItem(withIdentifier: tab)
            ew.contentView?.layoutSubtreeIfNeeded()
            XCTAssertEqual(ew.contentView?.frame.size, PatchEditorWindowController.minSize, tab)
            assertNothingClippedOrOverlapping(in: ew.contentView, "the editor's \(tab) tab")
        }
        editor.close()
    }

    /// The table at the default width shows a device's whole name; a large window widens the table
    /// (the device column takes the spare width) and keeps the limits' title with its fields; the
    /// rows do not move when the table grows a scroller.
    func testTheTableUsesTheWindowsWidthAndKeepsItsColumnsInPlace() async throws {
        let (_, sc) = try await open()
        let window = try XCTUnwrap(sc.window)
        window.contentView?.layoutSubtreeIfNeeded()
        func fits(_ popup: NSPopUpButton) -> Bool { popup.cell!.cellSize.width <= popup.bounds.width + 0.5 }
        XCTAssertTrue(fits(sc.patchDevicePopups[0]), "the whole device name shows at the default width")
        let narrow = sc.patchDevicePopups[0].bounds.width
        let editX = sc.patchEditButtons[0].convert(sc.patchEditButtons[0].bounds, to: nil).minX
        for _ in 0..<12 { await sc.model.addPatch() }
        await assertEventually { sc.patchEditButtons.count == 13 }
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(
            sc.patchEditButtons[0].convert(sc.patchEditButtons[0].bounds, to: nil).minX, editX, accuracy: 0.5,
            "a scroller does not move the rows")
        window.setContentSize(NSSize(width: 1100, height: 1000))
        window.contentView?.layoutSubtreeIfNeeded()
        XCTAssertGreaterThan(sc.patchDevicePopups[0].bounds.width, narrow + 300, "the device column widens")
        let title = try XCTUnwrap(
            sc.maxLimitField.superview?.superview?.subviews.compactMap { $0 as? NSTextField }.first {
                $0.stringValue == loc("settings.audio.limits")
            })
        let titleX = title.convert(title.bounds, to: nil).minX
        let maxRow = sc.maxLimitField.convert(sc.maxLimitField.bounds, to: nil)
        let minRow = sc.minLimitField.convert(sc.minLimitField.bounds, to: nil)
        XCTAssertLessThan(maxRow.minX - titleX, 60, "the title stays over its fields")
        XCTAssertEqual(maxRow.minX, minRow.minX, accuracy: 0.5, "the two fields line up")
    }

    /// The editor's header (name, cue outputs, device) stays at the top left: at the default size, in
    /// a large window, and after the workspace enters Show Mode with the editor open.
    func testTheEditorsHeaderStaysAtTheTopLeft() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        let editor = try XCTUnwrap(wc.editPatch(patch))
        await editor.editing?.value
        let window = try XCTUnwrap(editor.window)
        func left() -> CGFloat {
            window.contentView?.layoutSubtreeIfNeeded()
            return editor.nameField.convert(editor.nameField.bounds, to: nil).minX
        }
        XCTAssertEqual(left(), 14, accuracy: 1)
        window.setContentSize(NSSize(width: 1400, height: 1000))
        XCTAssertEqual(left(), 14, accuracy: 1, "large window")
        window.setContentSize(PatchEditorWindowController.minSize)
        await wc.requestShowMode(true)
        await settle()
        editor.render()
        XCTAssertEqual(left(), 14, accuracy: 1, "after Show Mode")
        editor.close()
    }

    /// Every visible control under `root` lies inside the visible part of each view that holds it, and
    /// no two controls of one container overlap.
    private func assertNothingClippedOrOverlapping(in root: NSView?, _ what: String) {
        guard let root else { return XCTFail("no \(what)") }
        var controls: [NSView] = []
        func walk(_ v: NSView) {
            if v.isHiddenOrHasHiddenAncestor { return }
            if v is NSControl, !(v is NSScroller), v.frame.width > 0 { controls.append(v) }
            // What a scroll view scrolls is reached by scrolling: its own clip view is not checked.
            if v is NSClipView { return }
            v.subviews.forEach(walk)
        }
        walk(root)
        XCTAssertFalse(controls.isEmpty, what)
        for c in controls {
            var view: NSView? = c.superview
            let frame = c.convert(c.bounds, to: nil)
            while let v = view, v !== root.superview {
                let visible = v.convert(v.visibleRect, to: nil)
                XCTAssertTrue(
                    visible.insetBy(dx: -0.5, dy: -0.5).contains(frame),
                    "\(what): \(Swift.type(of: c)) \(c.accessibilityIdentifier()) \(frame) is cut by \(Swift.type(of: v)) \(visible)"
                )
                view = v.superview
            }
        }
        for (i, a) in controls.enumerated() {
            for b in controls[(i + 1)...] where a.superview === b.superview {
                let fa = a.frame.insetBy(dx: 1, dy: 1)
                XCTAssertFalse(
                    fa.intersects(b.frame.insetBy(dx: 1, dy: 1)),
                    "\(what): \(a.accessibilityIdentifier()) overlaps \(b.accessibilityIdentifier())")
            }
        }
    }

    // MARK: Panic fade

    func testThePanicFadeShowsTheWorkspacesValue() async throws {
        let (_, sc) = try await open()
        XCTAssertEqual(sc.panicField.stringValue, "0.5", "settings.panic_duration of the show")
        XCTAssertEqual(sc.goIntervalField.stringValue, "0")
    }

    func testEditingThePanicFadeSendsTheSettingAndARangeErrorSendsNothing() async throws {
        let (rig, sc) = try await open()
        let ok = await sc.model.setPanicDuration("7.5")
        XCTAssertTrue(ok)
        let sent = try XCTUnwrap(commands(rig).last)
        XCTAssertEqual(sent["type"]?.stringValue, "set_workspace_setting")
        XCTAssertEqual(sent["setting"]?["type"]?.stringValue, "panic_duration")
        XCTAssertEqual(sent["setting"]?["value"]?.doubleValue, 7.5)
        XCTAssertEqual(sc.model.panicDuration, 7.5, "the engine stored it")

        let count = commands(rig).count
        for bad in ["61", "-1", "abc", "", "nan"] {
            let accepted = await sc.model.setPanicDuration(bad)
            XCTAssertFalse(accepted, bad)
        }
        XCTAssertEqual(commands(rig).count, count, "nothing out of range is sent")
        XCTAssertEqual(sc.model.panicDuration, 7.5)
        sc.render()
        XCTAssertEqual(sc.messageLabel.stringValue, String(format: loc("settings.general.outOfRange"), 0.0, 60.0))
    }

    func testTheGoIntervalSendsItsOwnSetting() async throws {
        let (rig, sc) = try await open()
        let ok = await sc.model.setMinGoInterval("1.5")
        XCTAssertTrue(ok)
        XCTAssertEqual(commands(rig).last?["setting"]?["type"]?.stringValue, "min_go_interval")
        XCTAssertEqual(sc.model.minGoInterval, 1.5, "the engine stored it")
        let tooLong = await sc.model.setMinGoInterval("11")
        XCTAssertFalse(tooLong)
    }

    // MARK: Patch levels and volume limits (PB-11)

    private let intro = TestSnapshots.intro
    private let fadeCue: ModelID = "00000000-0000-0000-0000-00000000000b"

    /// The demo show with its Intro cue routed file channels 1 and 2 to outputs 1 and 2 (so it sounds on
    /// both) and, when given, a global master (a file of the old format), Intro's master level and
    /// the Fade cue's master level.
    private func sounding(globalMaster: Double? = nil, introMaster: Double? = nil, fadeMaster: Double? = nil)
        -> (URL) throws -> Void
    {
        let intro = intro
        let fadeCue = fadeCue
        return { url in
            try editShow(url.appendingPathComponent("Demo.openlab")) { ws in
                if let globalMaster, var settings = ws["settings"] {
                    settings.set("master_db", .double(globalMaster))
                    ws.set("settings", settings)
                }
                guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue else {
                    return
                }
                for i in cues.indices {
                    guard var kind = cues[i]["kind"], var levels = kind["levels"] else { continue }
                    if cues[i]["id"]?.stringValue == intro.raw {
                        levels.set(
                            "crosspoints",
                            .array(
                                (0..<2).map {
                                    .object([
                                        "input": .int(Int64($0)), "output": .int(Int64($0)), "db": .double(0),
                                        "mute": .bool(false),
                                    ])
                                }))
                        if let introMaster { levels.set("master_db", .double(introMaster)) }
                    } else if cues[i]["id"]?.stringValue == fadeCue.raw, let fadeMaster {
                        levels.set("master_db", .double(fadeMaster))
                    } else {
                        continue
                    }
                    kind.set("levels", levels)
                    cues[i].set("kind", kind)
                }
                lists[0].set("cues", .array(cues))
                ws.set("cue_lists", .array(lists))
            }
        }
    }

    /// Types `text` into `field` and ends the editing, as a user does with Return.
    func type(_ text: String, into field: NSTextField) throws {
        let window = try XCTUnwrap(field.window)
        window.makeFirstResponder(field)
        let editor = try XCTUnwrap(field.currentEditor(), "the field is being edited")
        editor.string = text
        (field.delegate as? NSTextFieldDelegate)?.controlTextDidChange?(
            Notification(name: NSControl.textDidChangeNotification, object: field))
        window.makeFirstResponder(nil)
    }

    /// A press and release at `point` of `view` (its own coordinates), sent to the window, which need
    /// not be key or frontmost.
    private func click(at point: NSPoint, in view: NSView, window: NSWindow) throws {
        let at = view.convert(point, to: nil)
        for type in [NSEvent.EventType.leftMouseDown, .leftMouseUp] {
            let event = try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: at, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                    context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
            if type == .leftMouseDown { view.mouseDown(with: event) } else { view.mouseUp(with: event) }
        }
    }

    /// The settings window and the patch editor opened from its Edit button, on a show that is playing
    /// Intro through the patch, with the meters on.
    private func playing(_ edit: @escaping (URL) throws -> Void) async throws -> (
        Rig, WorkspaceSettingsWindowController, PatchEditorWindowController
    ) {
        // Whether a window counts as covered is the host's occlusion state, which a headless
        // runner reports late and differently each run.
        let shipped = MeterView.defaultWindowIsCovered
        MeterView.defaultWindowIsCovered = { _, _ in false }
        addTeardownBlock { @MainActor in MeterView.defaultWindowIsCovered = shipped }
        let (rig, sc) = try await open(edit: edit)
        let editor = try await self.edit(in: sc)
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        editor.meter.updateLive()
        guard case .success = await rig.store.transport(.start(cue: intro)) else { throw TestEngineError.notConnected }
        try await rig.advance(1)
        return (rig, sc, editor)
    }

    /// The loudest peak the engine reported for the patch's output `channel` over the next second of
    /// device time, read as each report arrives (the meter's own display timer lets a level fall).
    private func peak(_ rig: Rig, channel: Int) async throws -> Float {
        // A change ramps in over a few blocks: the first half second is let go by.
        try await rig.advance(0.5)
        rig.store.meters.reset()
        var loudest: Float = 0
        for _ in 0..<5 {
            try await rig.advance(0.1)
            rig.store.meters.tick(0)
            loudest = max(loudest, rig.store.meters.level(.output(patch), channel: channel))
        }
        return loudest
    }

    private func patchLevels(_ rig: Rig) -> (main: Double?, outputs: [Double]) {
        let patch = rig.store.model?.patches.first
        return (
            patch?.raw["master_db"]?.doubleValue,
            (patch?.raw["outputs"]?.arrayValue ?? []).compactMap { $0["level_db"]?.doubleValue }
        )
    }

    private func cueMaster(_ rig: Rig, _ id: ModelID) -> Double? {
        rig.store.model?.cue(id)?.raw["kind"]?["levels"]?["master_db"]?.doubleValue
    }

    private func texts(_ view: NSView?) -> [String] {
        guard let view else { return [] }
        var out = view.subviews.flatMap { texts($0) }
        if let field = view as? NSTextField, !field.isHiddenOrHasHiddenAncestor { out.append(field.stringValue) }
        if let button = view as? NSButton { out.append(button.title) }
        if let tabs = view as? NSTabView { out += tabs.tabViewItems.flatMap { [$0.label] + texts($0.view) } }
        return out
    }

    func testTheMainFaderChangesWhatPlays() async throws {
        let (rig, _, editor) = try await playing(sounding())
        let before = try await peak(rig, channel: 0)
        XCTAssertGreaterThan(before, 0.05, "Intro is sounding through the patch")
        try type("-12", into: editor.mainField)
        await assertEventually { self.patchLevels(rig).main == -12 }
        await assertEventually { editor.mainField.stringValue == "-12.0" }
        let after = try await peak(rig, channel: 0)
        XCTAssertEqual(Double(after), Double(before) * pow(10, -12 / 20), accuracy: Double(before) * 0.1)
        XCTAssertEqual(cueMaster(rig, intro), 0, "the cue's own levels are untouched")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running, "the cue stays playing")
    }

    func testACueOutputLevelSilencesThatOutputAndLeavesTheOther() async throws {
        let (rig, _, editor) = try await playing(sounding())
        let before = try await peak(rig, channel: 0)
        try type("-inf", into: editor.outputFields[1])
        await assertEventually { self.patchLevels(rig).outputs == [0, -120] }
        await assertEventually { editor.outputFields[1].stringValue == "-inf" }
        let silent = try await peak(rig, channel: 1)
        let kept = try await peak(rig, channel: 0)
        XCTAssertEqual(silent, 0, accuracy: 0.0001)
        XCTAssertEqual(Double(kept), Double(before), accuracy: Double(before) * 0.1)
    }

    func testTheLevelsAndNamesSurviveSaveAndReopen() async throws {
        let (rig, sc, editor) = try await playing(sounding())
        try type("-6", into: editor.mainField)
        try type("-3", into: editor.outputFields[1])
        editor.tabs.selectTabViewItem(withIdentifier: "cueOutputs")
        try type("Left", into: editor.outputNameFields[0])
        await assertEventually { self.patchLevels(rig).outputs == [0, -3] && self.patchLevels(rig).main == -6 }
        await assertEventually { sc.model.outputNames(of: self.patch) == ["Left", ""] }
        guard case .success = await rig.store.save() else { return XCTFail("save") }
        let path = try XCTUnwrap(rig.store.document?.path)
        let doc = try XCTUnwrap(rig.doc)
        guard case .success = await rig.client.send({ .close(id: $0, doc: doc, discardChanges: false) }) else {
            return XCTFail("close")
        }
        guard case .success(let ok) = await rig.testEngine.request(.open(path: path, recovery: nil)),
            case .opened(let reopenedDoc)? = ok.data
        else { return XCTFail("reopen") }
        let reopened = Rig(engine: rig.testEngine, workspace: reopenedDoc)
        guard await reopened.connect() else { return XCTFail("reconnect") }
        let again = WorkspaceSettingsWindowController(store: reopened.store)
        await again.present().value
        let editorAgain = try await edit(in: again)
        await assertEventually { editorAgain.mainField.stringValue == "-6.0" }
        XCTAssertEqual(editorAgain.outputFields.map(\.stringValue), ["0.0", "-3.0"])
        XCTAssertEqual(editorAgain.outputNameFields.map(\.stringValue), ["Left", ""])
    }

    func testTheWorkspaceLimitsFieldsClampTypedLevelsAndRefuseOutOfRangeLimits() async throws {
        let (rig, sc, editor) = try await playing(sounding())
        XCTAssertEqual(sc.maxLimitField.stringValue, "12")
        XCTAssertEqual(sc.minLimitField.stringValue, "-60")
        let before = try await peak(rig, channel: 0)
        try type("0", into: sc.maxLimitField)
        await assertEventually { sc.model.limits.max == 0 }
        try type("+6", into: editor.mainField)
        await assertEventually { editor.mainField.stringValue == "0.0" }
        XCTAssertEqual(patchLevels(rig).main, 0)
        let after = try await peak(rig, channel: 0)
        XCTAssertLessThanOrEqual(Double(after), Double(before) * 1.1, "nothing louder is rendered")
        // Out of range: refused, the old value stays.
        let sent = commands(rig).count
        try type("-31", into: sc.maxLimitField)
        try type("-39", into: sc.minLimitField)
        try type("-181", into: sc.minLimitField)
        await assertEventually { sc.maxLimitField.stringValue == "0" && sc.minLimitField.stringValue == "-60" }
        XCTAssertEqual(commands(rig).count, sent, "nothing out of range is sent")
        XCTAssertEqual(sc.messageLabel.stringValue, loc("settings.audio.limitOutOfRange"))
        // Inside the range they are taken.
        try type("-180", into: sc.minLimitField)
        await assertEventually { sc.model.limits.min == -180 }
    }

    func testLoweringTheMaximumPullsCueAndFadeLevelsDown() async throws {
        let (rig, sc, _) = try await playing(sounding(introMaster: 6, fadeMaster: 6))
        XCTAssertEqual(cueMaster(rig, intro), 6)
        XCTAssertEqual(cueMaster(rig, fadeCue), 6)
        try type("0", into: sc.maxLimitField)
        await assertEventually { self.cueMaster(rig, self.intro) == 0 }
        XCTAssertEqual(cueMaster(rig, fadeCue), 0, "a Fade aimed above 0 ends at 0")
        // One undo gives it all back.
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually { self.cueMaster(rig, self.intro) == 6 }
        XCTAssertEqual(cueMaster(rig, fadeCue), 6)
        XCTAssertEqual(sc.model.limits.max, 12)
    }

    func testThereIsNoGlobalMaster() async throws {
        let (rig, sc, editor) = try await playing(sounding(globalMaster: -2))
        XCTAssertNil(rig.store.model?.settings["master_db"], "the old file's master is gone from the model")
        let shown = texts(sc.window?.contentView) + texts(editor.window?.contentView)
        XCTAssertFalse(shown.contains { $0.localizedCaseInsensitiveContains("master") }, "\(shown)")
        // It plays as the file's levels say: the tone at its own level, not 2 dB down.
        let level = try await peak(rig, channel: 0)
        XCTAssertEqual(Double(level), 3000.0 / 32768, accuracy: 0.003)
    }

    // MARK: The patch editor during a show

    /// A real key press, as the window receives it.
    private func press(_ chars: String, keyCode: UInt16, in window: NSWindow) throws {
        window.sendEvent(
            try XCTUnwrap(
                NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                    windowNumber: window.windowNumber, context: nil, characters: chars,
                    charactersIgnoringModifiers: chars, isARepeat: false, keyCode: keyCode)))
    }

    func testOpeningThePatchEditorFocusesNoFieldAndSpaceEditsNoLevel() async throws {
        let (rig, _, editor) = try await playing(sounding())
        let window = try XCTUnwrap(editor.window)
        editor.present()
        XCTAssertNil(editor.mainField.currentEditor(), "no caret in the main box")
        XCTAssertNil(editor.nameField.currentEditor(), "nor in the name")
        XCTAssertFalse(window.firstResponder is NSText, "\(String(describing: window.firstResponder))")
        // The GO habit, with nothing focused.
        try press(" ", keyCode: 49, in: window)
        window.makeFirstResponder(nil)
        await settle()
        XCTAssertEqual(editor.mainField.stringValue, "0.0")
        XCTAssertEqual(patchLevels(rig).main, 0, "the patch is not silenced")
        let sounding = try await peak(rig, channel: 0)
        XCTAssertGreaterThan(sounding, 0.05, "and still sounds")
    }

    func testSpaceInAFocusedLevelBoxLeavesTheLevelAlone() async throws {
        let (rig, _, editor) = try await playing(sounding())
        let window = try XCTUnwrap(editor.window)
        // Clicked into, then Space: the text is replaced with a blank, and leaving it is no change.
        window.makeFirstResponder(editor.mainField)
        editor.mainField.currentEditor()?.selectAll(nil)
        try press(" ", keyCode: 49, in: window)
        XCTAssertEqual(editor.mainField.currentEditor()?.string, " ", "the key reached the field")
        let sent = commands(rig).count
        window.makeFirstResponder(nil)
        await assertEventually { editor.mainField.stringValue == "0.0" }
        XCTAssertEqual(commands(rig).count, sent, "a blank sends nothing")
        XCTAssertEqual(patchLevels(rig).main, 0)
        // Text that is not a number is still -inf (QLab 5 manual p. 200).
        try type("x", into: editor.mainField)
        await assertEventually { self.patchLevels(rig).main == -120 }
        await assertEventually { editor.mainField.stringValue == "-inf" }
    }

    /// Emptied is -inf, as in QLab (manual p. 204).
    func testAnEmptiedLevelBoxInThePatchEditorIsSilence() async throws {
        let (rig, _, editor) = try await playing(sounding())
        try type("", into: editor.mainField)
        await assertEventually { self.patchLevels(rig).main == -120 }
        await assertEventually { editor.mainField.stringValue == "-inf" }
    }

    func testALevelTypedBetweenALowMinimumAndSilenceIsSilent() async throws {
        let (rig, sc, editor) = try await playing(sounding())
        try type("-180", into: sc.minLimitField)
        await assertEventually { sc.model.limits.min == -180 }
        try type("-150", into: editor.mainField)
        await assertEventually { self.patchLevels(rig).main == -120 }
        await assertEventually { editor.mainField.stringValue == "-inf" }
        XCTAssertEqual(LevelText.clamp(-150, limits: sc.model.limits), LevelText.silence)
        // A level above silence is a level, however low the minimum.
        try type("-100", into: editor.mainField)
        await assertEventually { self.patchLevels(rig).main == -100 }
    }

    /// Each drag of the main fader is its own undo step.
    func testEachDragOfThePatchFaderIsItsOwnUndoStep() async throws {
        let (rig, _, editor) = try await playing(sounding())
        func drag(to values: [Double]) async {
            editor.mainFader.onGestureBegin?()
            for v in values {
                editor.mainFader.doubleValue = v
                _ = editor.mainFader.target?.perform(editor.mainFader.action, with: editor.mainFader)
            }
            await assertEventually { self.patchLevels(rig).main == values.last }
            editor.mainFader.onGestureEnd?()
        }
        await drag(to: [-1, -2, -3])
        await drag(to: [-7, -8])
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually { self.patchLevels(rig).main == -3 }
        await assertEventually { editor.mainField.stringValue == "-3.0" }
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually { self.patchLevels(rig).main == 0 }
    }

    /// An edit that does not take says so; the old level stays on show.
    func testALevelTheEngineDoesNotTakeSaysSoAndTheOldOneStays() async throws {
        let (rig, _, editor) = try await playing(sounding())
        XCTAssertEqual(editor.messageLabel.stringValue, "")
        rig.client.stop()
        try type("-12", into: editor.mainField)
        await assertEventually { !editor.messageLabel.stringValue.isEmpty }
        XCTAssertTrue(
            editor.messageLabel.stringValue.contains(loc("error.detail.notConnected")),
            editor.messageLabel.stringValue)
        XCTAssertEqual(editor.mainField.stringValue, "0.0")
    }

    func testThePatchEditorClosesWithItsWorkspace() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        await wc.showSettings().value
        let editor = try await edit(in: wc.settingsWindow)
        XCTAssertTrue(editor.window?.isVisible == true)
        wc.window?.close()
        XCTAssertFalse(editor.window?.isVisible == true, "no dead window left behind")
        XCTAssertFalse(wc.settingsWindow.window?.isVisible == true)
    }

    func testClosingTheSettingsControllerClosesThePatchEditor() async throws {
        let (_, sc) = try await open()
        let editor = try await edit(in: sc)
        sc.close()
        XCTAssertFalse(editor.window?.isVisible == true)
    }

    func testARemovedPatchClosesItsEditor() async throws {
        let (_, sc) = try await open()
        await sc.model.addPatch()
        let editor = try await edit(row: 2, in: sc)
        sc.patchRemoveButtons[1].performClick(nil)
        await assertEventually { editor.window?.isVisible != true }
    }

    func testWithoutAnEngineNothingCanBeChanged() async throws {
        let rig = try Rig.offline()
        let sc = WorkspaceSettingsWindowController(store: rig.store)  // never connected
        await sc.present().value
        XCTAssertFalse(sc.model.loaded)
        XCTAssertTrue(sc.patchDevicePopups.allSatisfy { !$0.isEnabled })
        XCTAssertFalse(sc.messageLabel.stringValue.isEmpty)
    }
}
