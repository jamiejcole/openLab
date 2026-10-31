// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// AUD-07, WS-07, PB-03: the workspace settings window (the footer gear, File > Workspace
/// Settings): the audio output device, the patch editor and the panic fade, all through the store
/// against the fake engine.
@MainActor
final class WorkspaceSettingsTests: XCTestCase {
    private let patch = ModelID("00000000-0000-0000-0000-000000000001")

    private func open(
        devices: [(name: String, channels: Int)] = [("Built-in", 2), ("Scarlett", 4)]
    ) async throws -> (Rig, WorkspaceSettingsWindowController) {
        let rig = try Rig()
        rig.engine.fakeDevices = devices
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        await sc.present().value
        return (rig, sc)
    }

    /// The `status` fixture with the playing device renamed and in `state`.
    private func pushStatus(_ rig: Rig, device: String, state: String = "ok") throws {
        let text = try Fixtures.text("server", "status")
            .replacingOccurrences(of: "\"Scarlett 2i2\"", with: "\"\(device)\"")
            .replacingOccurrences(of: "\"state\": \"ok\"", with: "\"state\": \"\(state)\"")
        rig.engine.push(text)
    }

    private func requests(_ rig: Rig) -> [Request] {
        rig.engine.received.compactMap { if case .request(_, let r) = $0 { return r } else { return nil } }
    }

    private func commands(_ rig: Rig) -> [JSONValue] {
        rig.engine.received.compactMap { if case .command(_, let c, _) = $0 { return c.json } else { return nil } }
    }

    // MARK: Opening

    func testTheFooterGearOpensTheSettingsWindowAndReadsTheEngine() async throws {
        let rig = try Rig()
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertTrue(wc.footer.settingsButton.isEnabled)
        wc.footer.settingsButton.performClick(nil)
        let sc = wc.settingsWindow
        XCTAssertTrue(sc.window?.isVisible == true)
        await assertEventually { sc.devicePopup.itemTitles.contains("Fake Output") }
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
        XCTAssertEqual(sc.devicePopup.itemTitles, [loc("settings.audio.systemDefault"), "Built-in", "Scarlett"])
        XCTAssertEqual(sc.devicePopup.indexOfSelectedItem, 0, "none chosen: the system default")
    }

    func testChoosingADeviceSendsItAndKeepsItForTheNextStart() async throws {
        let (rig, sc) = try await open()
        try pushStatus(rig, device: "Built-in")
        await assertEventually { sc.model.playing?.name == "Built-in" }
        sc.devicePopup.selectItem(withTitle: "Scarlett")
        _ = sc.devicePopup.target?.perform(sc.devicePopup.action, with: sc.devicePopup)
        await assertEventually { rig.engine.selectedOutputDevice == "Scarlett" }
        XCTAssertTrue(requests(rig).contains(.setOutputDevice(device: "Scarlett")))
        await assertEventually { sc.model.chosenDevice == "Scarlett" }
        sc.render()
        // It is not what plays now: the window says it opens at the next start.
        XCTAssertEqual(sc.deviceNoteLabel.stringValue, loc("settings.audio.restartNeeded"))

        sc.devicePopup.selectItem(at: 0)
        _ = sc.devicePopup.target?.perform(sc.devicePopup.action, with: sc.devicePopup)
        await assertEventually { rig.engine.selectedOutputDevice == nil && sc.model.chosenDevice == nil }
        XCTAssertTrue(requests(rig).contains(.setOutputDevice(device: nil)))
    }

    func testAChosenDeviceThatIsNoLongerListedShowsAsNotAvailable() async throws {
        let (rig, sc) = try await open()
        await sc.model.chooseDevice("Scarlett")
        rig.engine.fakeDevices = [("Built-in", 2)]
        await sc.model.refresh()
        sc.render()
        XCTAssertTrue(sc.model.chosenDeviceMissing)
        XCTAssertEqual(
            sc.devicePopup.titleOfSelectedItem, String(format: loc("settings.audio.notAvailable"), "Scarlett"))
        XCTAssertEqual(sc.deviceNoteLabel.stringValue, String(format: loc("settings.audio.chosenMissing"), "Scarlett"))
    }

    func testALostPlayingDeviceIsShownInTheWindow() async throws {
        let (rig, sc) = try await open()
        try pushStatus(rig, device: "Scarlett", state: "lost")
        await assertEventually { sc.model.deviceProblem }
        sc.render()
        XCTAssertTrue(sc.deviceStatusLabel.stringValue.contains("Scarlett"))
        XCTAssertTrue(sc.deviceNoteLabel.stringValue.contains(loc("settings.audio.deviceProblem")))
        XCTAssertFalse(sc.deviceNoteLabel.isHidden)
    }

    func testARefusedDeviceChoiceShowsTheEnginesReason() async throws {
        let (rig, sc) = try await open()
        rig.engine.fakeDevices = [("Built-in", 2)]  // Scarlett was unplugged since the list was read
        await sc.model.chooseDevice("Scarlett")
        sc.render()
        XCTAssertNil(rig.engine.selectedOutputDevice)
        XCTAssertTrue(sc.messageLabel.stringValue.contains("Scarlett"), sc.messageLabel.stringValue)
    }

    // MARK: Patch editor

    func testThePatchEditorListsThePatchAndFlagsAnUnmappedOne() async throws {
        let (_, sc) = try await open()
        XCTAssertEqual(sc.patchPopup.itemTitles, ["Main"])
        XCTAssertEqual(sc.patchDevicePopup.titleOfSelectedItem, loc("settings.audio.noDevice"))
        XCTAssertEqual(sc.patchProblemLabel.stringValue, loc("settings.audio.problem.unmapped"))
        XCTAssertTrue(sc.channelPopups.isEmpty)
    }

    func testChoosingAPatchDeviceMapsItsOutputsToTheFirstChannels() async throws {
        let (rig, sc) = try await open()
        try pushStatus(rig, device: "Scarlett")
        await assertEventually { sc.model.playing?.name == "Scarlett" }
        sc.render()
        sc.patchDevicePopup.selectItem(withTitle: "Scarlett")
        _ = sc.patchDevicePopup.target?.perform(sc.patchDevicePopup.action, with: sc.patchDevicePopup)
        await assertEventually { rig.engine.patchMappings[self.patch] != nil }
        XCTAssertEqual(rig.engine.patchMappings[patch], PatchMapping(device: "Scarlett", channels: [0, 1]))
        await assertEventually { sc.channelPopups.count == 2 }
        XCTAssertEqual(sc.patchProblemLabel.stringValue, "")
        XCTAssertEqual(sc.channelPopups.map { $0.selectedItem?.tag }, [0, 1])
        XCTAssertEqual(sc.channelPopups[0].itemTitles, ["1", "2", "3", "4"])
    }

    func testChangingAnOutputsChannelSendsTheWholeMapping() async throws {
        let (rig, sc) = try await open()
        let row = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setMapping(row, device: "Scarlett")
        sc.render()
        let mapped = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setChannel(mapped, output: 1, channel: 3)
        XCTAssertEqual(rig.engine.patchMappings[patch], PatchMapping(device: "Scarlett", channels: [0, 3]))
        sc.render()
        XCTAssertEqual(sc.channelPopups.map { $0.selectedItem?.tag }, [0, 3])
    }

    func testClearingAPatchSendsAnExplicitNone() async throws {
        let (rig, sc) = try await open()
        let row = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setMapping(row, device: "Built-in")
        XCTAssertNotNil(rig.engine.patchMappings[patch])
        let mapped = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setMapping(mapped, device: nil)
        XCTAssertNil(rig.engine.patchMappings[patch])
        XCTAssertTrue(requests(rig).contains(.setPatchMapping(patch: patch, mapping: nil)))
        sc.render()
        XCTAssertEqual(sc.patchProblemLabel.stringValue, loc("settings.audio.problem.unmapped"))
    }

    func testADeviceWithTooFewChannelsIsRefusedBeforeAnythingIsSent() async throws {
        let (rig, sc) = try await open(devices: [("Mono", 1)])
        let row = try XCTUnwrap(sc.model.patches.first)
        let before = requests(rig).count
        await sc.model.setMapping(row, device: "Mono")
        XCTAssertEqual(requests(rig).count, before, "nothing is sent for a mapping that cannot fit")
        XCTAssertFalse(requests(rig).contains { if case .setPatchMapping = $0 { return true } else { return false } })
        XCTAssertEqual(sc.model.message, String(format: loc("settings.audio.tooFewChannels"), "Mono", 2))
    }

    func testAMappingToAnAbsentDeviceIsFlaggedWithItsName() async throws {
        let (rig, sc) = try await open()
        let row = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setMapping(row, device: "Scarlett")
        rig.engine.fakeDevices = [("Built-in", 2)]  // unplugged
        await sc.model.refresh()
        sc.render()
        XCTAssertEqual(
            sc.model.problem(of: try XCTUnwrap(sc.model.patches.first)), .deviceMissing("Scarlett"))
        XCTAssertEqual(
            sc.patchProblemLabel.stringValue, String(format: loc("settings.audio.problem.deviceMissing"), "Scarlett"))
        XCTAssertEqual(
            sc.patchDevicePopup.titleOfSelectedItem, String(format: loc("settings.audio.notAvailable"), "Scarlett"))
    }

    func testARefusedMappingShowsTheEnginesReasonAndKeepsTheOldOne() async throws {
        let (rig, sc) = try await open()
        let row = try XCTUnwrap(sc.model.patches.first)
        await sc.model.setMapping(row, device: "Built-in")
        rig.engine.fakeDevices = [("Scarlett", 4)]
        await sc.model.setMapping(try XCTUnwrap(sc.model.patches.first), device: "Built-in", channels: [0, 1])
        sc.render()
        XCTAssertTrue(sc.messageLabel.stringValue.contains("Built-in"), sc.messageLabel.stringValue)
    }

    // MARK: Panic fade

    func testThePanicFadeShowsTheWorkspacesValue() async throws {
        let (_, sc) = try await open()
        XCTAssertEqual(sc.panicField.stringValue, "5", "settings.panic_duration of the snapshot")
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

        let count = commands(rig).count
        for bad in ["61", "-1", "abc", "", "nan"] {
            let accepted = await sc.model.setPanicDuration(bad)
            XCTAssertFalse(accepted, bad)
        }
        XCTAssertEqual(commands(rig).count, count, "nothing out of range is sent")
        sc.render()
        XCTAssertEqual(sc.messageLabel.stringValue, String(format: loc("settings.general.outOfRange"), 0.0, 60.0))
    }

    func testTheGoIntervalSendsItsOwnSetting() async throws {
        let (rig, sc) = try await open()
        let ok = await sc.model.setMinGoInterval("1.5")
        XCTAssertTrue(ok)
        XCTAssertEqual(commands(rig).last?["setting"]?["type"]?.stringValue, "min_go_interval")
        let tooLong = await sc.model.setMinGoInterval("11")
        XCTAssertFalse(tooLong)
    }

    func testWithoutAnEngineNothingCanBeChanged() async throws {
        let rig = try Rig()
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
