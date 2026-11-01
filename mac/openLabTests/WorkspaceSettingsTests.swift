// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// AUD-07, WS-07, PB-03: the workspace settings window (the footer gear, File > Workspace
/// Settings): the audio output device, the patch editor and the panic fade, all through the store
/// against the real engine. Its one device is the virtual one (2 outputs); losing it stands for an
/// unplugged interface. The device choice and patch mappings are the engine's own settings, so each
/// test has an engine of its own.
@MainActor
final class WorkspaceSettingsTests: XCTestCase {
    private let patch = ModelID("00000000-0000-0000-0000-000000000001")
    private let device = "openLab Virtual Output"
    private var engine: TestEngine!

    override func setUp() async throws {
        engine = try await TestEngine.start()
    }

    override func tearDown() async throws { engine?.stop() }

    /// The settings window of the demo show (or the show `edit` makes of it), read from the engine.
    private func open(edit: ((URL) throws -> Void)? = nil) async throws -> (Rig, WorkspaceSettingsWindowController) {
        let rig = try await Rig.open("Demo", on: engine, edit: edit)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        await sc.present().value
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

    private func commands(_ rig: Rig) -> [JSONValue] { rig.wire.commands.map(\.json) }

    /// The engine's mapping of the demo's patch, read back from it.
    private func mapping(_ rig: Rig) async throws -> PatchMapping? {
        guard case .success(let entries) = await rig.store.listPatchMappings() else {
            XCTFail("list_patch_mappings")
            return nil
        }
        return entries.first { $0.patch == patch }?.mapping
    }

    /// The device the engine keeps for its next start, read back from it.
    private func selected(_ rig: Rig) async throws -> String? {
        guard case .success(let ok) = await rig.store.listDevices(), case .devices(_, let selected)? = ok.data else {
            XCTFail("list_devices")
            return nil
        }
        return selected
    }

    // MARK: Meters

    func testTheAudioTabShowsTheShownPatchsPeaksAndOtherTabsIdle() async throws {
        let (rig, sc) = try await open()
        sc.tabs.selectTabViewItem(withIdentifier: "audio")
        sc.patchMeter.updateLive()
        XCTAssertNotNil(sc.patchMeter.source, "the shown patch is metered")
        XCTAssertEqual(rig.store.meters.visibleCount, 1)
        sc.tabs.selectTabViewItem(withIdentifier: "general")
        XCTAssertEqual(rig.store.meters.visibleCount, 0)
        sc.close()
    }

    // MARK: Opening

    func testTheFooterGearOpensTheSettingsWindowAndReadsTheEngine() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertTrue(wc.footer.settingsButton.isEnabled)
        wc.footer.settingsButton.performClick(nil)
        let sc = wc.settingsWindow
        XCTAssertTrue(sc.window?.isVisible == true)
        await assertEventually { sc.devicePopup.itemTitles.contains(self.device) }
        sc.close()
    }

    func testTheFileMenuHasWorkspaceSettingsOnCommandComma() {
        let item = MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == loc("menu.file") }?.items
            .first { $0.action == #selector(AppDelegate.showWorkspaceSettings(_:)) }
        XCTAssertEqual(item?.title, loc("menu.file.workspaceSettings"))
        XCTAssertEqual(item?.keyEquivalent, ",")
    }

    // MARK: Output device

    func testTheDeviceListComesFromTheEngine() async throws {
        let (_, sc) = try await open()
        XCTAssertEqual(sc.devicePopup.itemTitles, [loc("settings.audio.systemDefault"), device])
        XCTAssertEqual(sc.devicePopup.indexOfSelectedItem, 0, "none chosen: the system default")
    }

    func testChoosingADeviceSendsItAndKeepsItForTheNextStart() async throws {
        let (rig, sc) = try await open()
        try await status(rig)
        await assertEventually { sc.model.playing?.name == self.device }
        sc.devicePopup.selectItem(withTitle: device)
        _ = sc.devicePopup.target?.perform(sc.devicePopup.action, with: sc.devicePopup)
        await assertEventually { sc.model.chosenDevice == self.device }
        XCTAssertTrue(requests(rig).contains(.setOutputDevice(device: device)))
        let kept = try await selected(rig)
        XCTAssertEqual(kept, device, "the engine keeps it for its next start")
        sc.render()
        // It is what plays now: nothing to restart for.
        XCTAssertNotEqual(sc.deviceNoteLabel.stringValue, loc("settings.audio.restartNeeded"))

        sc.devicePopup.selectItem(at: 0)
        _ = sc.devicePopup.target?.perform(sc.devicePopup.action, with: sc.devicePopup)
        await assertEventually { sc.model.chosenDevice == nil }
        XCTAssertTrue(requests(rig).contains(.setOutputDevice(device: nil)))
        let cleared = try await selected(rig)
        XCTAssertNil(cleared)
    }

    func testAChosenDeviceThatIsNoLongerListedShowsAsNotAvailable() async throws {
        let (rig, sc) = try await open()
        await sc.model.chooseDevice(device)
        try await loseDevice(rig)
        await sc.model.refresh()
        sc.render()
        XCTAssertTrue(sc.model.chosenDeviceMissing)
        XCTAssertEqual(
            sc.devicePopup.titleOfSelectedItem, String(format: loc("settings.audio.notAvailable"), device))
        // The device was also the one playing: its problem is listed too.
        XCTAssertTrue(
            sc.deviceNoteLabel.stringValue.contains(String(format: loc("settings.audio.chosenMissing"), device)),
            sc.deviceNoteLabel.stringValue)
    }

    func testALostPlayingDeviceIsShownInTheWindow() async throws {
        let (rig, sc) = try await open()
        try await status(rig)
        try await loseDevice(rig)
        await assertEventually { sc.model.deviceProblem }
        sc.render()
        XCTAssertTrue(sc.deviceStatusLabel.stringValue.contains(device), sc.deviceStatusLabel.stringValue)
        XCTAssertTrue(sc.deviceNoteLabel.stringValue.contains(loc("settings.audio.deviceProblem")))
        XCTAssertFalse(sc.deviceNoteLabel.isHidden)
    }

    func testARefusedDeviceChoiceShowsTheEnginesReason() async throws {
        let (rig, sc) = try await open()
        await sc.model.chooseDevice("Scarlett")  // no such device: the engine refuses it
        sc.render()
        let kept = try await selected(rig)
        XCTAssertNil(kept)
        XCTAssertTrue(sc.messageLabel.stringValue.contains("Scarlett"), sc.messageLabel.stringValue)
    }

    func testAnUnchangedStatusRebuildsNothing() async throws {
        let (rig, sc) = try await open()
        try await status(rig)
        await assertEventually { sc.model.playing?.name == self.device }
        await settle()
        let drawn = sc.renderCount
        try await rig.advance(3)  // three more statuses, the same device in the same state
        await settle()
        XCTAssertEqual(sc.renderCount, drawn, "same device, same state: nothing to redraw")
        try await loseDevice(rig)
        await assertEventually { sc.renderCount > drawn }
    }

    // MARK: Patch editor

    /// The engine put the patch on the device's first outputs when the show opened; cleared, the
    /// patch is flagged unmapped.
    func testThePatchEditorListsThePatchAndFlagsAnUnmappedOne() async throws {
        let (_, sc) = try await open()
        XCTAssertEqual(sc.patchPopup.itemTitles, ["Main"])
        XCTAssertEqual(sc.patchDevicePopup.titleOfSelectedItem, device)
        XCTAssertEqual(sc.patchProblemLabel.stringValue, "")
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: nil)
        sc.render()
        XCTAssertEqual(sc.patchDevicePopup.titleOfSelectedItem, loc("settings.audio.noDevice"))
        XCTAssertEqual(sc.patchProblemLabel.stringValue, loc("settings.audio.problem.unmapped"))
        XCTAssertTrue(sc.channelPopups.isEmpty)
    }

    func testChoosingAPatchDeviceMapsItsOutputsToTheFirstChannels() async throws {
        let (rig, sc) = try await open()
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: nil)
        try await status(rig)
        await assertEventually { sc.model.playing?.name == self.device }
        sc.render()
        sc.patchDevicePopup.selectItem(withTitle: device)
        _ = sc.patchDevicePopup.target?.perform(sc.patchDevicePopup.action, with: sc.patchDevicePopup)
        await assertEventually { sc.channelPopups.count == 2 }
        let mapped = try await mapping(rig)
        XCTAssertEqual(mapped, PatchMapping(device: device, channels: [0, 1]))
        XCTAssertEqual(sc.patchProblemLabel.stringValue, "")
        XCTAssertEqual(sc.channelPopups.map { $0.selectedItem?.tag }, [0, 1])
        XCTAssertEqual(sc.channelPopups[0].itemTitles, ["1", "2"])
    }

    func testPickingTheDeviceAPatchAlreadyUsesKeepsItsRouting() async throws {
        let (rig, sc) = try await open()
        await sc.model.setChannel(try XCTUnwrap(sc.model.patches.first), output: 1, channel: 0)
        let sentBefore = requests(rig).count
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: device)
        let kept = try await mapping(rig)
        XCTAssertEqual(kept, PatchMapping(device: device, channels: [0, 0]))
        XCTAssertEqual(requests(rig).count, sentBefore + 1, "the same device again sends nothing (one read back)")
    }

    func testChangingAnOutputsChannelSendsTheWholeMapping() async throws {
        let (rig, sc) = try await open()
        sc.render()
        let mapped = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setChannel(mapped, output: 0, channel: 1)
        XCTAssertTrue(
            requests(rig).contains(
                .setPatchMapping(patch: patch, mapping: PatchMapping(device: device, channels: [1, 1]))))
        let now = try await mapping(rig)
        XCTAssertEqual(now, PatchMapping(device: device, channels: [1, 1]))
        sc.render()
        XCTAssertEqual(sc.channelPopups.map { $0.selectedItem?.tag }, [1, 1])
    }

    func testClearingAPatchSendsAnExplicitNone() async throws {
        let (rig, sc) = try await open()
        let before = try await mapping(rig)
        XCTAssertNotNil(before)
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: nil)
        let after = try await mapping(rig)
        XCTAssertNil(after)
        XCTAssertTrue(requests(rig).contains(.setPatchMapping(patch: patch, mapping: nil)))
        sc.render()
        XCTAssertEqual(sc.patchProblemLabel.stringValue, loc("settings.audio.problem.unmapped"))
    }

    /// A three-output patch cannot go on the two-output device: refused here, nothing sent.
    func testADeviceWithTooFewChannelsIsRefusedBeforeAnythingIsSent() async throws {
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
        XCTAssertEqual(sc.model.message, String(format: loc("settings.audio.tooFewChannels"), device, 3))
    }

    func testAMappingToAnAbsentDeviceIsFlaggedWithItsName() async throws {
        let (rig, sc) = try await open()
        try await loseDevice(rig)  // unplugged
        await sc.model.refresh()
        sc.render()
        XCTAssertEqual(sc.model.problem(of: try XCTUnwrap(sc.model.patches.first)), .deviceMissing(device))
        XCTAssertEqual(
            sc.patchProblemLabel.stringValue, String(format: loc("settings.audio.problem.deviceMissing"), device))
        XCTAssertEqual(
            sc.patchDevicePopup.titleOfSelectedItem, String(format: loc("settings.audio.notAvailable"), device))
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

    func testWithoutAnEngineNothingCanBeChanged() async throws {
        let rig = try Rig.offline()
        let sc = WorkspaceSettingsWindowController(store: rig.store)  // never connected
        await sc.present().value
        XCTAssertFalse(sc.model.loaded)
        XCTAssertFalse(sc.devicePopup.isEnabled)
        XCTAssertFalse(sc.messageLabel.stringValue.isEmpty)
    }
}

extension NSPopUpButton {
    fileprivate var itemTitles: [String] { itemArray.map(\.title) }
}
