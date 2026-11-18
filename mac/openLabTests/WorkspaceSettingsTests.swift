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
        addTeardownBlock { @MainActor in sc.close() }
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
        guard case .success = await engine.request(.loseDevice()) else { return XCTFail("lose_device") }
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
        XCTAssertEqual(
            sc.patchDevicePopups.first?.itemTitles,
            [
                loc("settings.audio.noDevice"), "", String(format: loc("settings.audio.systemOutput"), device), "",
                device,
            ])
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

    /// #666 checklist: with the second virtual device plugged in, it is picked in a patch's row → a
    /// cue on that patch is heard on the second device and not on the first. A second patch, left on
    /// the first device, meters what that device plays.
    func testTheSecondDevicePickedForAPatchPlaysItsCueThereAndNotOnTheFirst() async throws {
        let second = TestEngine.deviceNames[1]
        guard case .success = await engine.request(.restoreDevice(device: TestEngine.deviceIDs[1])) else {
            return XCTFail("plug in the second device")
        }
        try await engine.advance(seconds: 2)  // a device-list read sees it
        let (rig, sc) = try await open(edit: sounding())
        sc.patchAddButton.performClick(nil)
        await assertEventually { sc.patchNameFields.count == 2 }
        let other = try XCTUnwrap(rig.store.model?.patches.first { $0.id != patch }?.id)
        await assertEventually("the new patch is on the first device") {
            sc.patchDevicePopups.last?.titleOfSelectedItem == self.device
        }
        let popup = try XCTUnwrap(sc.patchDevicePopups.first)
        XCTAssertEqual(
            popup.itemArray.filter { !$0.isSeparatorItem }.map(\.title),
            [
                loc("settings.audio.noDevice"), String(format: loc("settings.audio.systemOutput"), device), device,
                second,
            ])

        try pick(second, in: popup)
        await assertEventually("the engine took it") { sc.model.patches.first?.mapping?.device == second }
        await assertEventually("the row shows the second device") {
            sc.patchDevicePopups.first?.titleOfSelectedItem == second
        }
        let stored = try await mapping(rig)
        XCTAssertEqual(stored, PatchMapping(device: second, channels: [0, 1]))
        try await rig.advance(0.5)  // the second device opens
        await assertEventually("no problem on the row") { sc.patchProblemLabels.first?.stringValue == "" }

        guard case .success = await rig.store.transport(.start(cue: intro)) else { return XCTFail("start") }
        try await rig.advance(0.5)
        rig.wire.resetDevicePeaks()
        for _ in 0..<5 { try await rig.advance(0.1) }
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)
        let onSecond = rig.wire.devicePeaks[patch] ?? []
        XCTAssertGreaterThan(onSecond.max() ?? 0, 0.05, "heard on the second device")
        XCTAssertEqual(rig.wire.devicePeaks[other], [0, 0], "nothing on the first device")
    }

    /// #685 checklist: the device pop-up of a patch lists a system-output entry first (after none,
    /// as QLab 5 lists it), named with the Mac's current output device; choosing it plays the
    /// patch's cues on that device, and the row and the patch editor show the choice.
    func testTheDevicePopUpListsASystemOutputEntryFirstAndChoosingItPlaysThePatchsCuesOnTheMacsCurrentOutputDevice()
        async throws
    {
        let (rig, sc) = try await open(edit: sounding())
        let system = String(format: loc("settings.audio.systemOutput"), device)
        let popup = try XCTUnwrap(sc.patchDevicePopups.first)
        let titles = popup.itemArray.filter { !$0.isSeparatorItem }.map(\.title)
        XCTAssertEqual(titles, [loc("settings.audio.noDevice"), system, device])
        XCTAssertEqual(popup.itemArray.filter { !$0.isSeparatorItem && $0.isEnabled }.count, 3)

        try pick(system, in: popup)
        await assertEventually("the engine took it") { sc.model.patches.first?.mapping?.system == true }
        await assertEventually("the row shows the system output") {
            sc.patchDevicePopups.first?.titleOfSelectedItem == system
        }
        let stored = try await mapping(rig)
        XCTAssertEqual(stored, PatchMapping.systemOutput(channels: [0, 1]))
        XCTAssertEqual(stored?.device, "")
        XCTAssertEqual(sc.patchProblemLabels.first?.stringValue, "")
        let editor = try await edit(in: sc)
        XCTAssertEqual(editor.devicePopup.titleOfSelectedItem, system)
        // The routing matrix is the device's: the system output is that device now.
        XCTAssertEqual(editor.matrix.deviceOutputs, 2)

        let meter = MeterView()
        meter.bank = rig.store.meters
        meter.source = .output(patch)
        let shipped = MeterView.defaultWindowIsCovered
        MeterView.defaultWindowIsCovered = { _, _ in false }
        addTeardownBlock { @MainActor in MeterView.defaultWindowIsCovered = shipped }
        sc.window?.contentView?.addSubview(meter)
        guard case .success = await rig.store.transport(.start(cue: intro)) else { return XCTFail("start") }
        let heard = try await peak(rig, channel: 0)
        XCTAssertGreaterThan(heard, 0.05, "the cue is heard on the Mac's output device")
        meter.removeFromSuperview()

        // Picking it again changes nothing; picking the device by name leaves the system output.
        func sets() -> Int {
            requests(rig).filter { if case .setPatchMapping = $0 { return true } else { return false } }.count
        }
        let sent = sets()
        try pick(system, in: sc.patchDevicePopups[0])
        try await rig.advance(0.2)
        XCTAssertEqual(sets(), sent, "the same choice again sends nothing")
        try pick(device, in: sc.patchDevicePopups[0])
        await assertEventually { sc.model.patches.first?.mapping?.system == false }
        XCTAssertEqual(sc.patchDevicePopups.first?.titleOfSelectedItem, device)
        let named = try await mapping(rig)
        XCTAssertEqual(named, PatchMapping(device: device, channels: [0, 1]))

        // With no output device on the Mac any more, the choice says so and the row why.
        try pick(system, in: sc.patchDevicePopups[0])
        await assertEventually { sc.model.patches.first?.mapping?.system == true }
        try await loseDevice(rig)
        await assertEventually("the row says the Mac has no output") {
            sc.patchDevicePopups.first?.titleOfSelectedItem == loc("settings.audio.systemOutputNone")
                && sc.patchProblemLabels.first?.stringValue == loc("settings.audio.systemOutputMissing")
        }
    }

    /// A device with fewer outputs than the patch's cue outputs is picked anyway: cue outputs 1 and
    /// 2 go to its two outputs and the third has none of its own (as QLab routes it); no problem is
    /// shown and the patch's cues play.
    func testADeviceWithFewerOutputsThanThePatchTakesItsFirstOutputs() async throws {
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
        await sc.model.setMapping(row, device: device)
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped, PatchMapping(device: device, channels: [0, 1]))
        XCTAssertNil(sc.model.message)
        let again = try XCTUnwrap(sc.model.patches.first)
        XCTAssertNil(sc.model.problem(of: again), "no problem shown")
    }

    /// Checklist (ui-qa D1): a show whose patch has 8 cue outputs, opened where the device has 2
    /// and nothing is stored for it, opens on that device: no problem in the editor, and GO plays
    /// the patch's first two cue outputs.
    func testAnEightOutputPatchOnATwoOutputDeviceOpensOnTheDeviceAndPlays() async throws {
        let (rig, sc) = try await open { show in
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                guard var patches = ws["patches"]?.arrayValue, let first = patches.first?["outputs"]?.arrayValue?.first
                else { return XCTFail("the demo show has no patch") }
                patches[0].set("outputs", .array(Array(repeating: first, count: 8)))
                ws.set("patches", .array(patches))
            }
            try self.sounding()(show)
        }
        let editor = try await edit(in: sc)
        await assertEventually { editor.devicePopup.titleOfSelectedItem == self.device }
        XCTAssertEqual(editor.problemLabel.stringValue, "")
        XCTAssertEqual(editor.matrix.rowLabels.count, 8)
        XCTAssertEqual(drawnLevels(editor)[0], [0, nil])
        XCTAssertEqual(drawnLevels(editor)[2], [nil, nil], "cue output 3 has no device output")
        guard case .success = await rig.store.transport(.start(cue: intro)) else { return XCTFail("start") }
        try await rig.advance(1)
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)
        let one = try await peak(rig, channel: 0)
        let two = try await peak(rig, channel: 1)
        XCTAssertGreaterThan(one, 0.01, "cue output 1 is heard")
        XCTAssertGreaterThan(two, 0.01, "cue output 2 is heard")
    }

    func testPickingTheDeviceAPatchAlreadyUsesKeepsItsRouting() async throws {
        let (rig, sc) = try await open()
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: device, channels: [0, 0])
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
        XCTAssertEqual(drawnLevels(editor), [[0, nil], [nil, 0]])
        XCTAssertEqual(editor.outputFields.map(\.stringValue), ["0.0", "0.0"])
        // Device outputs: one row per output of the device, with its level, mute and what reaches it.
        XCTAssertEqual(
            editor.deviceOutputLabels.map(\.stringValue),
            [String(format: loc("patchEditor.feeds"), "1"), String(format: loc("patchEditor.feeds"), "2")])
        XCTAssertEqual(editor.deviceFields.map(\.stringValue), ["0.0", "0.0"])
        XCTAssertEqual(editor.deviceMutes.map(\.state), [.off, .off])
        XCTAssertEqual(editor.deviceFaders.count, 2)
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
        // The routing on this machine follows: the kept outputs where they were, the new ones (the
        // device has only two) with no device output of their own (Q515).
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped, PatchMapping(device: device, channels: [0, 1]))
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

    /// 128 cue outputs (QLab's most): every row is in the editor, each with its meter, and the
    /// matrix scrolls.
    func testA128OutputPatchShowsEveryRowWithItsMeter() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        await sc.model.setOutputCount(patch, 128)
        await editor.editing?.value
        editor.render()
        XCTAssertEqual(editor.outputFields.count, 128)
        XCTAssertEqual(editor.matrix.rowLabels.count, 128)
        XCTAssertEqual(editor.meter.fixedRows, 128, "a meter bar beside every row")
        XCTAssertEqual(editor.meter.frame.maxY, editor.matrix.meterRect(rows: 128).maxY - PatchRoutingView.headerH)
        XCTAssertGreaterThan(editor.matrix.frame.height, PatchRoutingView.rowH * 128)
        // Each level box is inside the matrix, on its own row.
        let last = try XCTUnwrap(editor.outputFields.last)
        XCTAssertTrue(editor.matrixRows.bounds.contains(last.frame))

        // Scrolled to the last row, the header still says what each column is, and the last row's
        // label pane is in view beside its boxes.
        let window = try XCTUnwrap(editor.window)
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        window.contentView?.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(editor.matrix.enclosingScrollView)
        editor.matrix.scroll(NSPoint(x: 0, y: editor.matrix.frame.height))
        scroll.reflectScrolledClipView(scroll.contentView)
        window.contentView?.layoutSubtreeIfNeeded()
        let visible = scroll.convert(scroll.contentView.frame, to: nil)
        let header = editor.matrixHeader.convert(editor.matrixHeader.bounds, to: nil)
        // The header covers the top edge of what shows, and its labels are inside it.
        XCTAssertGreaterThanOrEqual(header.maxY, visible.maxY - 0.5, "header \(header) covers the top of \(visible)")
        XCTAssertGreaterThanOrEqual(header.minY, visible.minY, "header \(header) inside \(visible)")
        XCTAssertLessThanOrEqual(header.minY, visible.maxY - PatchRoutingView.headerH + 0.5, "the labels show")
        XCTAssertTrue(visible.intersects(last.convert(last.bounds, to: nil)), "the last row is in view")
        // The device's two outputs take cue outputs 1 and 2; the rest have none of their own (Q515).
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped?.channels, [0, 1])
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

    /// A warning already there when the window opens is drawn before the table has its width: once
    /// laid out, its row is as tall as its text, with no blank band under it.
    func testAWarningThereWhenTheWindowOpensLeavesNoBlankBandUnderIt() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        try await loseDevice(rig)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        await sc.present().value
        sc.tabs.selectTabViewItem(withIdentifier: "audio")
        let warning = String(format: loc("settings.audio.problem.deviceMissing"), device)
        await assertEventually { sc.patchProblemLabels.first?.stringValue == warning }
        XCTAssertEqual(sc.model.patches.count, 1, "the warning's row is the table's last")
        let line = sc.patchProblemLabels[0]
        await assertEventually("the row ends where its text does") {
            sc.window?.contentView?.layoutSubtreeIfNeeded()
            let needs =
                line.cell?.cellSize(
                    forBounds: NSRect(x: 0, y: 0, width: line.frame.width, height: 10_000)
                ).height ?? 0
            let below =
                line.convert(line.bounds, to: nil).minY
                - sc.patchGrid.convert(sc.patchGrid.bounds, to: nil).minY
            return abs(line.frame.height - needs) < 2 && abs(below) < 2
        }
        sc.close()
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
        // The line is as tall as its text at the table's width: no blank band under it.
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        let line = sc.patchProblemLabels[0]
        let needs = try XCTUnwrap(line.cell).cellSize(
            forBounds: NSRect(x: 0, y: 0, width: line.frame.width, height: 10_000)
        ).height
        XCTAssertEqual(line.frame.height, needs, accuracy: 2, "no blank band under the warning")
        XCTAssertEqual(sc.model.patches.count, 1, "the warning's row is the table's last")
        let lineInWindow = line.convert(line.bounds, to: nil)
        let gridInWindow = sc.patchGrid.convert(sc.patchGrid.bounds, to: nil)
        XCTAssertEqual(lineInWindow.minY, gridInWindow.minY, accuracy: 2, "the row ends where its text does")
        await assertEventually("the device is shown as not connected once the list has lost it") {
            sc.patchDevicePopups.first?.titleOfSelectedItem
                == String(format: loc("settings.audio.notConnected"), self.device)
        }
        await assertEventually { editor.problemLabel.stringValue == warning }
        XCTAssertFalse(editor.problemLabel.isHidden)
        // The status window's warnings list has one entry for the device.
        let deviceTitle = String(format: loc("warnings.device"), device)
        let rows = WarningRows.make(store: rig.store)
        XCTAssertEqual(rows.filter { $0.title == deviceTitle }.count, 1, "\(rows.map(\.title))")

        // Back again: the warning goes from all three.
        guard case .success = await engine.request(.restoreDevice()) else { return XCTFail("restore_device") }
        try await rig.advance(2)
        await assertEventually { sc.patchProblemLabels.first?.stringValue == "" }
        await assertEventually { editor.problemLabel.stringValue == "" }
        await assertEventually {
            !WarningRows.make(store: rig.store).contains { $0.title == deviceTitle }
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
            format: loc("settings.audio.patchInUse"), String(format: loc("settings.audio.newPatchName"), 2), "1 Intro")
        await assertEventually { sc.messageLabel.stringValue == expected }
        XCTAssertEqual(commands(rig).count, sent, "nothing sent")
        XCTAssertEqual(rig.store.model?.patches.count, 2)
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(table.convert(table.bounds, to: nil), before, "the table keeps its place and size")

        // Long patch and cue names: cut in the middle, and the whole sentence is drawn.
        let long = String(repeating: "Front of house main pair ", count: 4)
        await sc.model.renamePatch(second, to: long)
        guard case .success = await rig.store.send(command: .setProperty(intro, .name(long))) else {
            return XCTFail("rename cue")
        }
        await settle()
        sc.patchRemoveButtons[1].performClick(nil)
        await assertEventually { sc.messageLabel.stringValue.contains(WorkspaceSettingsModel.middleCut(long)) }
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        let cell = try XCTUnwrap(sc.messageLabel.cell)
        let needs = cell.cellSize(
            forBounds: NSRect(x: 0, y: 0, width: sc.messageLabel.frame.width, height: 10_000)
        ).height
        XCTAssertLessThanOrEqual(needs, sc.messageLabel.frame.height + 1, "the sentence is not cut")
        XCTAssertTrue(sc.messageLabel.stringValue.hasSuffix(String(loc("settings.audio.patchInUse").suffix(20))))
        XCTAssertEqual(table.convert(table.bounds, to: nil), before, "the table still keeps its place")
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
    /// output 2 is quiet until cue output 1 is sent there too.
    func testRoutingChangedUnderAPlayingCueIsHeardAtOnceAndTheEditorMetersMove() async throws {
        let (rig, _, editor) = try await playing(sounding())
        editor.meter.updateLive()
        XCTAssertTrue(editor.meter.isOnScreen, "the editor's meter is live")
        XCTAssertEqual(editor.meter.source, .output(patch))
        try type("-inf", into: editor.outputFields[1])
        await assertEventually { self.patchLevels(rig).outputs == [0, -120] }
        let quiet = try await peak(rig, channel: 1)
        XCTAssertEqual(quiet, 0, accuracy: 0.0001, "device output 2 is silent")

        // Row 1's box under device output 2, typed in.
        try typeCrosspoint("0", row: 0, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig) == [[0, 1, 0]] }
        await assertEventually { self.drawnLevels(editor) == [[0, 0], [nil, 0]] }
        let heard = try await peak(rig, channel: 1)
        XCTAssertGreaterThan(heard, 0.05, "device output 2 now carries cue output 1")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running, "the cue plays on")
        // The editor's meter rows are the cue outputs themselves: row 1 moves, row 2 (silenced in
        // the patch) stays dark though device output 2 now carries cue output 1.
        XCTAssertTrue(editor.meter.bank === rig.store.meters)
        XCTAssertGreaterThan(rig.store.meters.visibleCount, 0, "the editor's meter is registered")
        let row1 = try await rowPeak(rig, row: 0)
        let row2 = try await rowPeak(rig, row: 1)
        XCTAssertGreaterThan(row1, 0.05, "row 1 moves")
        XCTAssertEqual(row2, 0, accuracy: 0.0001, "row 2 stays dark")
        XCTAssertEqual(
            editor.deviceOutputLabels.map(\.stringValue),
            [String(format: loc("patchEditor.feeds"), "1"), String(format: loc("patchEditor.feeds"), "1, 2")])
    }

    // MARK: Routing matrix, device outputs and the editor's foot (AUD-07)

    /// Checklist: Patch editor, Routing → a cue output is sent to two device outputs at different
    /// levels → a cue on that patch is heard on both at those levels. Cue output 2 is silenced, so
    /// each device output carries cue output 1 only.
    func testPatchEditorRoutingACueOutputSentToTwoDeviceOutputsAtDifferentLevelsIsHeardOnBothAtThoseLevels()
        async throws
    {
        let (rig, _, editor) = try await playing(sounding())
        try type("-inf", into: editor.outputFields[1])
        await assertEventually { self.patchLevels(rig).outputs == [0, -120] }
        let full = try await peak(rig, channel: 0)
        XCTAssertGreaterThan(full, 0.05, "cue output 1 is heard on device output 1")

        try typeCrosspoint("-6", row: 0, column: 0, in: editor)
        await assertEventually { self.crosspoints(rig) == [[0, 0, -6]] }
        try typeCrosspoint("-12", row: 0, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig) == [[0, 0, -6], [0, 1, -12]] }
        // Drawn as typed: each box its own level.
        await assertEventually { self.drawnLevels(editor) == [[-6, -12], [nil, 0]] }
        XCTAssertTrue(editor.crosspointField.isHidden, "the typing box closed")
        let one = try await peak(rig, channel: 0)
        let two = try await peak(rig, channel: 1)
        XCTAssertEqual(Double(one / full), pow(10, -6.0 / 20), accuracy: 0.05, "device output 1 at -6 dB")
        XCTAssertEqual(Double(two / full), pow(10, -12.0 / 20), accuracy: 0.05, "device output 2 at -12 dB")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)
        XCTAssertEqual(
            editor.matrix.accessibilityValue() as? String, "1→1 -6.0, 1→2 -12.0, 2→2 0.0", "what is drawn")
    }

    /// Checklist: a crosspoint's level is typed while a cue plays → the change is heard at once, with
    /// no click (the ramp is proven sample by sample on the engine). Typing it empty stops it; a
    /// stray Space changes nothing; Esc drops what was typed.
    func testACrosspointsLevelTypedWhileACuePlaysIsHeardAtOnceWithNoClick() async throws {
        let (rig, _, editor) = try await playing(sounding())
        let full = try await peak(rig, channel: 1)
        try typeCrosspoint("-20", row: 1, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig) == [[1, 1, -20]] }
        let lowered = try await peak(rig, channel: 1)
        XCTAssertEqual(Double(lowered / full), pow(10, -20.0 / 20), accuracy: 0.02, "heard at once at -20 dB")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running, "the cue plays on, not restarted")

        // A stray Space (the GO key) in the box: nothing is sent.
        let sent = commands(rig).count
        try typeCrosspoint(" ", row: 1, column: 1, in: editor)
        await settle()
        XCTAssertEqual(commands(rig).count, sent, "white space is no change")
        // Esc while typing: the box closes, nothing is sent.
        try typeCrosspointThenEscape("-3", row: 1, column: 1, in: editor)
        XCTAssertEqual(commands(rig).count, sent, "Esc sends nothing")
        // Emptied: cue output 2 no longer reaches device output 2.
        try typeCrosspoint("", row: 1, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig) == [[1, 1, -120]] }
        await assertEventually { self.drawnLevels(editor) == [[0, nil], [nil, nil]] }
        let off = try await peak(rig, channel: 1)
        XCTAssertEqual(off, 0, accuracy: 0.0001)
        // Typed 0 where the mapping sends it: back to the mapping, the list empty again.
        try typeCrosspoint("0", row: 1, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig).isEmpty }
    }

    /// ui-qa: the patch editor's row meter shows its cue output wherever the crosspoints send it.
    /// Cue output 1 taken off its mapped device output (1→1 at -inf) and sent to device output 2
    /// lights row 1 though device output 1 is silent; cue output 2, silenced in the patch, keeps
    /// row 2 dark though device output 2 is sounding.
    func testARowMeterFollowsItsCueOutputNotTheDeviceOutputItIsMappedTo() async throws {
        let (rig, _, editor) = try await playing(sounding())
        try type("-inf", into: editor.outputFields[1])
        try typeCrosspoint("", row: 0, column: 0, in: editor)
        try typeCrosspoint("0", row: 0, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig) == [[0, 0, -120], [0, 1, 0]] }
        await assertEventually { self.patchLevels(rig).outputs == [0, -120] }
        let deviceOne = try await peak(rig, channel: 0)
        let deviceTwo = try await peak(rig, channel: 1)
        XCTAssertEqual(deviceOne, 0, accuracy: 0.0001, "device output 1 hears nothing")
        XCTAssertGreaterThan(deviceTwo, 0.05, "device output 2 carries cue output 1")
        let row1 = try await rowPeak(rig, row: 0)
        let row2 = try await rowPeak(rig, row: 1)
        XCTAssertGreaterThan(row1, 0.05, "row 1 lights")
        XCTAssertEqual(row2, 0, accuracy: 0.0001, "row 2 stays dark")
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)
    }

    /// A show whose patch routes cue output 1 to device output `to` (from 1) and, with `muted`, has
    /// cue output 2 muted.
    private func routedFar(to: Int, muted: Bool = false) -> (URL) throws -> Void {
        { url in
            try editShow(url.appendingPathComponent("Demo.openlab")) { ws in
                guard var patches = ws["patches"]?.arrayValue, !patches.isEmpty else { return }
                patches[0].set(
                    "crosspoints",
                    .array([.object(["output": .int(0), "device_output": .int(Int64(to - 1)), "db": .double(0)])]))
                if muted, var outputs = patches[0]["outputs"]?.arrayValue, outputs.count > 1 {
                    outputs[1].set("mute", .bool(true))
                    patches[0].set("outputs", .array(outputs))
                }
                ws.set("patches", .array(patches))
            }
        }
    }

    /// ui-qa D2: the routing grid scrolled sideways: the column titles stay in their corner over
    /// the left columns, and the device-output numbers pass under it, never over the names, levels
    /// or meters.
    func testScrollingTheRoutingGridSidewaysKeepsTheColumnTitlesInTheirCorner() async throws {
        let (_, sc) = try await open(edit: routedFar(to: 40))
        let editor = try await edit(in: sc)
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        await assertEventually { editor.matrix.deviceOutputs == 40 }
        let scroll = try XCTUnwrap(editor.matrix.enclosingScrollView)
        editor.window?.contentView?.layoutSubtreeIfNeeded()
        editor.matrix.scroll(NSPoint(x: 900, y: 0))
        scroll.tile()
        XCTAssertGreaterThan(scroll.contentView.bounds.minX, 0, "scrolled sideways")
        let clip = scroll.contentView.frame
        let corner = editor.matrixCorner.frame
        XCTAssertEqual(corner.minX, clip.minX)
        if scroll.isFlipped {
            XCTAssertEqual(corner.minY, clip.minY, "on the clip's top edge")
        } else {
            XCTAssertEqual(corner.maxY, clip.maxY, "on the clip's top edge")
        }
        XCTAssertEqual(corner.width, PatchRoutingView.crossX, "as wide as the left columns")
        XCTAssertEqual(corner.height, PatchRoutingView.headerH)
        // Over everything else in the scroll view: the floating panes' container too.
        XCTAssertTrue(scroll.subviews.last === editor.matrixCorner, "\(scroll.subviews)")
        XCTAssertTrue(editor.matrixCorner.isOpaque, "what passes under it is hidden")
    }

    /// ui-qa D3, D6: an editor left open while its device is lost says the device is not connected
    /// (not that it has no outputs), and once the device is back shows it as it is again, with no
    /// stale note, by itself.
    func testAnOpenEditorFollowsItsNamedDeviceLostAndBack() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        try await status(rig)
        try await loseDevice(rig)
        await assertEventually("the editor shows the device gone") {
            editor.devicePopup.titleOfSelectedItem == String(format: loc("settings.audio.notConnected"), self.device)
        }
        await assertEventually { editor.routingNote.stringValue.contains(loc("patchEditor.notConnectedNote")) }
        XCTAssertFalse(editor.routingNote.stringValue.contains("0 outputs"))
        guard case .success = await engine.request(.restoreDevice()) else { return XCTFail("restore_device") }
        try await rig.advance(6)
        await assertEventually("the editor shows the device back") {
            editor.devicePopup.titleOfSelectedItem == self.device && editor.matrix.usable == 2
                && !editor.routingNote.stringValue.contains(loc("patchEditor.notConnectedNote"))
                && editor.deviceOutputLabels.allSatisfy { $0.textColor == .secondaryLabelColor }
        }
    }

    /// ui-qa D4: a cue output's mute shows on its row (its button on, its level red) and is set
    /// and cleared there; Unmute All clears it too.
    func testACueOutputsMuteShowsOnItsRowAndIsToggledThere() async throws {
        let (rig, sc) = try await open(edit: routedFar(to: 2, muted: true))
        let editor = try await edit(in: sc)
        await assertEventually { editor.outputMutes.count == 2 }
        XCTAssertEqual(editor.outputMutes.map(\.state), [.off, .on])
        XCTAssertEqual(editor.outputFields[1].textColor, .systemRed)
        XCTAssertEqual(editor.outputFields[1].stringValue, "0.0", "muting keeps the level")
        XCTAssertTrue(editor.matrixRows.bounds.contains(editor.outputMutes[0].frame), "on its row")
        editor.outputMutes[0].performClick(nil)
        let mutes = {
            (rig.store.model?.patches.first?.raw["outputs"]?.arrayValue ?? []).map { o -> Bool in
                if case .bool(true) = o["mute"] { return true }
                return false
            }
        }
        await assertEventually { mutes() == [true, true] }
        await assertEventually { editor.outputMutes.map(\.state) == [.on, .on] }
        editor.unmuteButton.performClick(nil)
        await assertEventually { mutes() == [false, false] }
        await assertEventually { editor.outputMutes.map(\.state) == [.off, .off] }
        XCTAssertEqual(editor.outputFields[1].textColor, .controlTextColor)
    }

    /// Opens a crosspoint's box, types and presses Esc.
    private func typeCrosspointThenEscape(_ text: String, row: Int, column: Int, in editor: PatchEditorWindowController)
        throws
    {
        let window = try XCTUnwrap(editor.window)
        let box = editor.matrix.boxRect(row: row, column: column)
        try click(at: NSPoint(x: box.midX, y: box.midY), in: editor.matrix, window: window, count: 2)
        let fieldEditor = try XCTUnwrap(editor.crosspointField.currentEditor())
        fieldEditor.string = text
        editor.crosspointField.delegate?.controlTextDidChange?(
            Notification(name: NSControl.textDidChangeNotification, object: editor.crosspointField))
        fieldEditor.doCommand(by: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertTrue(editor.crosspointField.isHidden, "Esc closes the box")
    }

    /// Checklist: Device Outputs → each device output has its own level and mute; lowering one
    /// lowers everything sent to it. Cue output 1 is also sent to device output 2, so device
    /// output 2 carries both cue outputs; lowering it lowers both and leaves device output 1 alone.
    func testDeviceOutputsEachDeviceOutputHasItsOwnLevelAndMuteAndLoweringOneLowersEverythingSentToIt()
        async throws
    {
        let (rig, _, editor) = try await playing(sounding())
        try typeCrosspoint("0", row: 0, column: 1, in: editor)
        await assertEventually { self.crosspoints(rig) == [[0, 1, 0]] }
        let one = try await peak(rig, channel: 0)
        let both = try await peak(rig, channel: 1)
        editor.tabs.selectTabViewItem(withIdentifier: "deviceOutputs")
        XCTAssertEqual(
            editor.deviceOutputLabels.map(\.stringValue),
            [String(format: loc("patchEditor.feeds"), "1"), String(format: loc("patchEditor.feeds"), "1, 2")])

        try type("-12", into: editor.deviceFields[1])
        await assertEventually { self.deviceOutputs(rig) == [[1, -12, 0]] }
        await assertEventually { editor.deviceFields[1].stringValue == "-12.0" }
        XCTAssertEqual(editor.deviceFaders[1].doubleValue, -12, "the fader follows")
        // Heard through the meters, which run while the routing tab (with the editor's meter) shows.
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        let lowered = try await peak(rig, channel: 1)
        XCTAssertEqual(Double(lowered / both), pow(10, -12.0 / 20), accuracy: 0.02, "all of it, 12 dB down")
        let untouched = try await peak(rig, channel: 0)
        XCTAssertEqual(untouched, one, accuracy: one * 0.02, "device output 1 is not changed")

        // Mute: silent; the level is kept and the matrix says so over its column.
        editor.tabs.selectTabViewItem(withIdentifier: "deviceOutputs")
        editor.deviceMutes[1].performClick(nil)
        await assertEventually { self.deviceOutputs(rig) == [[1, -12, 1]] }
        await assertEventually { editor.matrix.routing.deviceOutput(1).mute }
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        let muted = try await peak(rig, channel: 1)
        XCTAssertEqual(muted, 0, accuracy: 0.0001)
        XCTAssertEqual(editor.deviceFields[1].stringValue, "-12.0", "muting keeps the level")
        XCTAssertTrue(editor.unmuteButton.isEnabled)
        editor.deviceMutes[1].performClick(nil)
        await assertEventually { self.deviceOutputs(rig) == [[1, -12, 0]] }
        XCTAssertEqual(rig.store.cueStates[intro]?.state, .running)
    }

    /// Checklist: the editor's buttons to set routing from another patch or to defaults, and to
    /// clear mutes. A second patch is given the first's routing (main, cue output levels and mutes,
    /// crosspoints, device outputs) in one undo step; Unmute All clears every mute; Defaults puts
    /// the first patch back as a new one routes.
    func testTheEditorsButtonsSetRoutingFromAnotherPatchOrToDefaultsAndClearMutes() async throws {
        let (rig, sc) = try await open()
        let main = try await edit(in: sc)
        try type("-3", into: main.mainField)
        try typeCrosspoint("-6", row: 0, column: 1, in: main)
        main.tabs.selectTabViewItem(withIdentifier: "deviceOutputs")
        main.deviceMutes[0].performClick(nil)
        await assertEventually {
            self.crosspoints(rig) == [[0, 1, -6]] && self.deviceOutputs(rig) == [[0, 0, 1]]
                && self.patchLevels(rig).main == -3
        }
        let added0 = await sc.model.addPatch()
        let added = try XCTUnwrap(added0)
        await assertEventually { sc.patchEditButtons.count == 2 }
        main.close()
        let other = try await edit(row: 2, in: sc)
        let titles = other.copyPopup.itemTitles
        XCTAssertEqual(
            titles.filter { !$0.isEmpty },
            [
                loc("patchEditor.copyRouting"), loc("patchEditor.copyDefaults"),
                String(format: loc("patchEditor.copyFrom"), "Main"),
            ])
        XCTAssertFalse(other.unmuteButton.isEnabled, "nothing muted")
        try pick(String(format: loc("patchEditor.copyFrom"), "Main"), in: other.copyPopup)
        await assertEventually {
            self.crosspoints(rig, of: added) == [[0, 1, -6]] && self.deviceOutputs(rig, of: added) == [[0, 0, 1]]
        }
        let copied = rig.store.model?.patches.first { $0.id == added }
        XCTAssertEqual(copied?.raw["master_db"]?.doubleValue, -3)
        await assertEventually { self.drawnLevels(other) == [[0, -6], [nil, 0]] }
        await assertEventually { other.unmuteButton.isEnabled }

        // Unmute All: one step, every mute off, levels kept.
        other.unmuteButton.performClick(nil)
        await assertEventually { self.deviceOutputs(rig, of: added).isEmpty }
        await assertEventually { !other.unmuteButton.isEnabled }
        // Undone: the mute is back; undone again: the copy is gone in one step.
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually { self.deviceOutputs(rig, of: added) == [[0, 0, 1]] }
        guard case .success = await rig.store.undo() else { return XCTFail("undo") }
        await assertEventually {
            self.crosspoints(rig, of: added).isEmpty && self.deviceOutputs(rig, of: added).isEmpty
                && rig.store.model?.patches.first { $0.id == added }?.raw["master_db"]?.doubleValue == 0
        }

        // Defaults on the first patch: no crosspoints, no device-output settings, main at 0 dB.
        other.close()
        let again = try await edit(in: sc)
        try pick(loc("patchEditor.copyDefaults"), in: again.copyPopup)
        await assertEventually {
            self.crosspoints(rig).isEmpty && self.deviceOutputs(rig).isEmpty && self.patchLevels(rig).main == 0
        }
        await assertEventually { self.drawnLevels(again) == [[0, nil], [nil, 0]] }
    }

    /// Checklist: a show saved with such routing reopens with it; on a machine with fewer device
    /// outputs the missing ones are warned about, not lost. The show routes cue output 1 to device
    /// output 4 and sets device output 5; the virtual device has 2.
    func testAShowSavedWithRoutingReopensWithItAndOnFewerDeviceOutputsTheMissingOnesAreWarnedNotLost()
        async throws
    {
        let routed: (URL) throws -> Void = { url in
            try editShow(url.appendingPathComponent("Demo.openlab")) { ws in
                guard var patches = ws["patches"]?.arrayValue, !patches.isEmpty else { return }
                patches[0].set(
                    "crosspoints",
                    .array([
                        .object(["output": .int(0), "device_output": .int(1), "db": .double(-6)]),
                        .object(["output": .int(0), "device_output": .int(3), "db": .double(-3)]),
                    ]))
                patches[0].set(
                    "device_outputs",
                    .array([.object(["device_output": .int(4), "level_db": .double(-10), "mute": .bool(true)])]))
                ws.set("patches", .array(patches))
            }
        }
        let (rig, sc) = try await open(edit: routed)
        let editor = try await edit(in: sc)
        // Kept and drawn: five columns, the three the device lacks in red, a note saying so.
        await assertEventually { editor.matrix.deviceOutputs == 5 && editor.matrix.usable == 2 }
        XCTAssertEqual(editor.matrix.routing.level(0, 3), -3)
        XCTAssertEqual(editor.matrix.routing.level(0, 1), -6)
        XCTAssertTrue(
            editor.routingNote.stringValue.contains(String(format: loc("patchEditor.beyond"), 2, 5)),
            editor.routingNote.stringValue)
        XCTAssertEqual(editor.deviceFaders.count, 5)
        XCTAssertEqual(editor.deviceMutes[4].state, .on)
        XCTAssertEqual(editor.deviceFields[4].stringValue, "-10.0")
        // Warned in the status window, naming the device and the patch (Q510).
        try await status(rig)
        await assertEventually {
            WarningRows.make(store: rig.store).contains {
                $0.title == String(format: loc("warnings.device"), self.device)
                    && $0.detail == String(format: loc("warnings.patchRoutingBeyond"), "Main")
            }
        }

        guard case .success = await rig.store.save() else { return XCTFail("save") }
        let path = try XCTUnwrap(rig.store.document?.path)
        let saved = try XCTUnwrap(JSONValue.parse(try String(contentsOf: URL(fileURLWithPath: path), encoding: .utf8)))
        let patch = try XCTUnwrap(saved["workspace"]?["patches"]?.arrayValue?.first)
        XCTAssertEqual(patch["crosspoints"]?.arrayValue?.count, 2, "not lost")
        XCTAssertEqual(patch["device_outputs"]?.arrayValue?.count, 1, "not lost")
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
        addTeardownBlock { @MainActor in again.close() }
        await again.present().value
        let editorAgain = try await edit(in: again)
        await assertEventually { editorAgain.matrix.routing.level(0, 3) == -3 && editorAgain.matrix.deviceOutputs == 5 }
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

    /// ui-qa D1: an open patch editor follows the device being lost and coming back by itself,
    /// also with the settings window closed: its pop-up, its problem line and its routing say
    /// what the device is now, never what it was one change earlier.
    func testAnOpenEditorFollowsItsDeviceLostAndBackWithTheSettingsWindowClosed() async throws {
        let (rig, sc) = try await open()
        let system = String(format: loc("settings.audio.systemOutput"), device)
        try pick(system, in: try XCTUnwrap(sc.patchDevicePopups.first))
        await assertEventually { sc.model.patches.first?.mapping?.system == true }
        let editor = try await edit(in: sc)
        XCTAssertEqual(editor.devicePopup.titleOfSelectedItem, system)
        // The settings window closed by its title bar's button: the editor stays open.
        sc.window?.close()
        XCTAssertNotNil(editor.window?.isVisible == true || !sc.presentsEditors ? editor : nil)
        try await loseDevice(rig)
        await assertEventually("the editor shows the device gone") {
            editor.devicePopup.titleOfSelectedItem == loc("settings.audio.systemOutputNone")
                && editor.problemLabel.stringValue == loc("settings.audio.systemOutputMissing")
                && editor.matrix.usable == 0
        }
        guard case .success = await engine.request(.restoreDevice()) else { return XCTFail("restore_device") }
        try await rig.advance(1.5)
        await assertEventually("the editor shows the device back") {
            editor.devicePopup.titleOfSelectedItem == system && editor.problemLabel.stringValue.isEmpty
                && editor.matrix.usable == 2
        }
    }

    /// ui-qa D2: an editor opened for the first time while its device is away shows its device
    /// pop-up's whole title at the editor's default width.
    func testAnEditorFirstOpenedWhileItsDeviceIsAwayShowsTheWholeDeviceTitle() async throws {
        let (rig, sc) = try await open()
        let system = String(format: loc("settings.audio.systemOutput"), device)
        try pick(system, in: try XCTUnwrap(sc.patchDevicePopups.first))
        await assertEventually { sc.model.patches.first?.mapping?.system == true }
        try await loseDevice(rig)
        await assertEventually { sc.model.devices.isEmpty }
        let editor = try await edit(in: sc)
        let popup = editor.devicePopup
        editor.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertEqual(popup.titleOfSelectedItem, loc("settings.audio.systemOutputNone"))
        XCTAssertLessThanOrEqual(
            try XCTUnwrap(popup.cell).cellSize.width, popup.bounds.width + 0.5, "the title is cut")
    }

    /// Without a device the matrix takes no clicks and says why.
    func testWithNoDeviceTheMatrixIsOffAndSaysWhy() async throws {
        let (rig, sc) = try await open()
        let editor = try await edit(in: sc)
        try pick(loc("settings.audio.noDevice"), in: editor.devicePopup)
        await editor.editing?.value
        await assertEventually { !editor.matrix.isEnabled }
        XCTAssertTrue(editor.routingNote.stringValue.contains(loc("patchEditor.noDevice")))
        XCTAssertEqual(editor.matrix.deviceOutputs, 0, "no device: no device outputs")
        XCTAssertEqual(editor.matrix.routing.channels, nil)
        let sent = rig.wire.sent.count
        let box = editor.matrix.cellRect(row: 0, column: 0)
        try click(
            at: NSPoint(x: box.midX, y: box.midY), in: editor.matrix, window: try XCTUnwrap(editor.window), count: 2)
        await editor.editing?.value
        XCTAssertTrue(editor.crosspointField.isHidden, "no box opens for typing")
        XCTAssertEqual(rig.wire.sent.count, sent, "nothing sent")
        // Picked again in the editor, it is routed to the first outputs.
        try pick(device, in: editor.devicePopup)
        await editor.editing?.value
        await assertEventually { self.drawnLevels(editor) == [[0, nil], [nil, 0]] }
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
    /// not be key or frontmost; `count` 2 is the second click of a double-click.
    private func click(at point: NSPoint, in view: NSView, window: NSWindow, count: Int = 1) throws {
        let at = view.convert(point, to: nil)
        for type in [NSEvent.EventType.leftMouseDown, .leftMouseUp] {
            let event = try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: at, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                    context: nil, eventNumber: 0, clickCount: count, pressure: 1))
            if type == .leftMouseDown { view.mouseDown(with: event) } else { view.mouseUp(with: event) }
        }
    }

    /// Double-clicks the routing box of cue output `row` and device output `column` (from 0) and
    /// types `text` in it, ended as Return ends it.
    private func typeCrosspoint(_ text: String, row: Int, column: Int, in editor: PatchEditorWindowController) throws {
        editor.tabs.selectTabViewItem(withIdentifier: "routing")
        let window = try XCTUnwrap(editor.window)
        let box = editor.matrix.boxRect(row: row, column: column)
        try click(at: NSPoint(x: box.midX, y: box.midY), in: editor.matrix, window: window)
        try click(at: NSPoint(x: box.midX, y: box.midY), in: editor.matrix, window: window, count: 2)
        XCTAssertFalse(editor.crosspointField.isHidden, "the box opened for typing")
        XCTAssertTrue(editor.matrix.bounds.contains(editor.crosspointField.frame), "the box is on the matrix")
        // The box has the keyboard already: typed into as it is, then ended as Return ends it.
        let fieldEditor = try XCTUnwrap(editor.crosspointField.currentEditor(), "the box is being edited")
        fieldEditor.string = text
        editor.crosspointField.delegate?.controlTextDidChange?(
            Notification(name: NSControl.textDidChangeNotification, object: editor.crosspointField))
        window.makeFirstResponder(nil)
    }

    /// The level each box of the editor's matrix draws, row by row (nil: not routed there).
    private func drawnLevels(_ editor: PatchEditorWindowController) -> [[Double?]] {
        editor.matrix.rowLabels.indices.map { o in
            (0..<editor.matrix.deviceOutputs).map { d in editor.matrix.routing.level(o, d) }
        }
    }

    /// The crosspoints the engine holds for the patch, as `[output, device output, dB]`.
    private func crosspoints(_ rig: Rig, of id: ModelID? = nil) -> [[Double]] {
        let p = rig.store.model?.patches.first { $0.id == (id ?? patch) }
        return (p?.raw["crosspoints"]?.arrayValue ?? []).map { c in
            ["output", "device_output", "db"].map { c[$0]?.doubleValue ?? .nan }
        }
    }

    /// The device-output entries the engine holds for the patch, as `[device output, dB, muted]`.
    private func deviceOutputs(_ rig: Rig, of id: ModelID? = nil) -> [[Double]] {
        let p = rig.store.model?.patches.first { $0.id == (id ?? patch) }
        return (p?.raw["device_outputs"]?.arrayValue ?? []).map { e in
            var muted = 0.0
            if case .bool(true) = e["mute"] { muted = 1 }
            return [e["device_output"]?.doubleValue ?? .nan, e["level_db"]?.doubleValue ?? .nan, muted]
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
        rig.wire.resetDevicePeaks()
        for _ in 0..<5 { try await rig.advance(0.1) }
        let peaks = rig.wire.devicePeaks[patch] ?? []
        return peaks.indices.contains(channel) ? peaks[channel] : 0
    }

    /// The loudest the patch editor's meter drew for cue output `row` (from 0) over the next half
    /// second of device time, read as each report arrives.
    private func rowPeak(_ rig: Rig, row: Int) async throws -> Float {
        try await rig.advance(0.5)
        rig.store.meters.reset()
        var loudest: Float = 0
        for _ in 0..<5 {
            try await rig.advance(0.1)
            rig.store.meters.tick(0)
            loudest = max(loudest, rig.store.meters.level(.output(patch), channel: row))
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

/// AUD-07: the patch editor's routing logic, with no engine: what a box shows, what typing in it
/// sends, and what copying, defaults and Unmute All send.
@MainActor
final class PatchRoutingTests: XCTestCase {
    private let id = ModelID("00000000-0000-0000-0000-000000000001")

    private func routing(channels: [Int]?, crosspoints: [(Int, Int, Double)] = [], outputs: Int = 2)
        -> PatchRouting
    {
        var r = PatchRouting()
        r.outputs = Array(repeating: PatchRouting.CueOutput(), count: outputs)
        r.channels = channels
        for (o, d, db) in crosspoints { r.crosspoints[.init(output: o, deviceOutput: d)] = db }
        return r
    }

    func testABoxShowsTheCrosspointElseTheMappingsZeroElseNothing() {
        let r = routing(channels: [0, 1], crosspoints: [(0, 0, -120), (0, 2, -6)])
        XCTAssertNil(r.level(0, 0), "a silent crosspoint overrides the mapping")
        XCTAssertEqual(r.level(0, 2), -6)
        XCTAssertEqual(r.level(1, 1), 0, "the mapping's 0 dB")
        XCTAssertNil(r.level(1, 0))
        XCTAssertEqual(r.namedDeviceOutputs, 3)
        XCTAssertEqual(r.feeders(of: 1), [1])
        XCTAssertNil(routing(channels: nil).level(0, 0), "no device: nothing routed")
    }

    /// Only routing that sends something names a device output: a silent crosspoint, or one of a cue
    /// output the patch no longer has, draws no column past the device and no note (the engine
    /// raises no warning for them either).
    func testOnlyRoutedCrosspointsOfCueOutputsThePatchHasNameDeviceOutputs() {
        XCTAssertEqual(routing(channels: [0, 1], crosspoints: [(0, 5, -120)]).namedDeviceOutputs, 2)
        XCTAssertEqual(routing(channels: [0, 1], crosspoints: [(2, 5, 0)]).namedDeviceOutputs, 2)
        XCTAssertEqual(routing(channels: [0, 1], crosspoints: [(1, 5, -6)]).namedDeviceOutputs, 6)
        var muted = routing(channels: [0, 1])
        muted.deviceOutputs[3] = .init(level: 0, mute: true)
        XCTAssertEqual(muted.namedDeviceOutputs, 4, "a device output's own setting names it")
    }

    func testTypingSendsOnlyWhatChangesAndKeepsTheListShort() {
        let r = routing(channels: [0, 1], crosspoints: [(0, 2, -6)])
        XCTAssertEqual(r.typed(-6, output: 0, deviceOutput: 2), nil, "the same level: nothing")
        XCTAssertEqual(r.typed(-3, output: 0, deviceOutput: 2), .some(-3))
        XCTAssertEqual(
            r.typed(-120, output: 0, deviceOutput: 2), .some(nil), "emptied off the mapping: out of the list")
        XCTAssertEqual(r.typed(0, output: 0, deviceOutput: 0), nil, "the mapping's own level: nothing")
        XCTAssertEqual(r.typed(-120, output: 0, deviceOutput: 0), .some(-120), "the mapped pair silenced")
        XCTAssertEqual(r.typed(-200, output: 1, deviceOutput: 3), nil, "silence where nothing goes: nothing")
        let listed = routing(channels: [0, 1], crosspoints: [(0, 0, -6)])
        XCTAssertEqual(listed.typed(0, output: 0, deviceOutput: 0), .some(nil), "back to the mapping")
    }

    /// Copying gives the target the routing the source has, whichever device outputs each machine
    /// maps their cue outputs to; a cue output only the target has keeps its routing.
    func testCopyingReproducesTheSourcesRoutingOverTheTargetsMapping() {
        let source = routing(channels: [2, 3], crosspoints: [(0, 0, -6)])
        let target = routing(channels: [0, 1], crosspoints: [(2, 5, -1)], outputs: 3)
        let copied = target.crosspoints(copying: source)
        var after = target
        after.crosspoints = copied
        for o in 0..<2 {
            for d in 0..<6 { XCTAssertEqual(after.level(o, d), source.level(o, d), "\(o)→\(d)") }
        }
        XCTAssertEqual(after.level(2, 5), -1, "kept")
    }

    func testCopyDefaultsAndUnmuteAreOneStepEachAndNothingWhenNothingChanges() throws {
        var r = routing(channels: [0, 1])
        let json: [JSONValue] = [.object(["name": .string("L")]), .object([:])]
        XCTAssertNil(r.defaultsCommands(id, outputsJSON: json), "already the defaults")
        XCTAssertNil(r.unmuteCommands(id, outputsJSON: json), "nothing muted")
        r.master = -3
        r.outputs[1].mute = true
        r.deviceOutputs[1] = .init(level: -6, mute: true)
        r.crosspoints[.init(output: 0, deviceOutput: 1)] = -6
        let unmute = try XCTUnwrap(r.unmuteCommands(id, outputsJSON: json)).json
        XCTAssertEqual(unmute["type"]?.stringValue, "transaction")
        let parts = unmute["commands"]?.arrayValue ?? []
        XCTAssertEqual(parts.map { $0["property"]?["type"]?.stringValue }, ["outputs", "device_outputs"])
        XCTAssertEqual(parts[0]["property"]?["value"]?.arrayValue?.first?["name"]?.stringValue, "L", "names stay")
        XCTAssertEqual(
            parts[1]["property"]?["value"],
            .array([.object(["device_output": .int(1), "level_db": .double(-6), "mute": .bool(false)])]))
        let defaults = try XCTUnwrap(r.defaultsCommands(id, outputsJSON: json)).json
        XCTAssertEqual(
            defaults["commands"]?.arrayValue?.map { $0["property"]?["type"]?.stringValue },
            ["master_db", "outputs", "crosspoints", "device_outputs"])
    }
}
