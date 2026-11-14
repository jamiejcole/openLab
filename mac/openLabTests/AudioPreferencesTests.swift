// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// UI-01, AUD-21: the Audio tab of the application preferences, against the real engine through the
/// app's own connection: each output device with its buffer size, and the clock device kept for the
/// next start. Machine settings belong to the engine, so each test has an engine of its own.
@MainActor
final class AudioPreferencesTests: XCTestCase {
    private let device = "openLab Virtual Output"
    var engine: TestEngine!

    override func setUp() async throws {
        engine = try await TestEngine.start()
    }

    override func tearDown() async throws { engine?.stop() }

    /// The preferences window on the Audio tab, its first read of the engine done.
    private func open() async throws -> PreferencesWindowController {
        let suite = "openlab-prefs-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        let prefs = PreferencesWindowController(
            preferences: LaunchPreferences(defaults: defaults), audio: AudioPreferencesModel(client: engine.app))
        prefs.tabs.selectTabViewItem(withIdentifier: "audio")
        await prefs.refreshing?.value
        return prefs
    }

    private func pick(_ title: String, in popup: NSPopUpButton) throws {
        let item = try XCTUnwrap(popup.item(withTitle: title), "\(popup.itemTitles)")
        popup.select(item)
        _ = popup.target?.perform(popup.action, with: popup)
    }

    private func listed() async throws -> (devices: [DeviceEntry], selected: String?) {
        guard case .success(let ok) = await engine.request(.listDevices), case .devices(let d, let s)? = ok.data else {
            XCTFail("list_devices")
            return ([], nil)
        }
        return (d, s)
    }

    /// Checklist: application preferences have an audio buffer-size choice; changing it does not need
    /// a relaunch. The device plays a cue meanwhile, which plays on at the new size.
    func testTheBufferSizeChoiceTakesEffectWithoutARelaunch() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        guard case .success = await rig.store.transport(.start(cue: TestSnapshots.intro)) else {
            return XCTFail("start")
        }
        try await rig.advance(1.5)
        await assertEventually { rig.store.status?.outputs.first?.blockFrames == 512 }
        let pid = engine.pid
        let prefs = try await open()
        XCTAssertEqual(prefs.bufferPopups.count, 1)
        XCTAssertEqual(prefs.bufferPopups[0].titleOfSelectedItem, "512")
        XCTAssertEqual(
            prefs.bufferPopups[0].itemTitles, AudioPreferencesModel.bufferSizes.map(String.init), "16 to 1024")

        try pick("256", in: prefs.bufferPopups[0])
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.bufferPopups[0].titleOfSelectedItem, "256")
        let now = try await listed()
        XCTAssertEqual(now.devices.first?.bufferFrames, 256, "kept for this machine")
        try await rig.advance(1.5)
        await assertEventually("the device plays at the new size") {
            rig.store.status?.outputs.first?.blockFrames == 256
        }
        XCTAssertEqual(rig.store.status?.outputs.first?.state, .up)
        XCTAssertEqual(engine.pid, pid, "the same engine: nothing was relaunched")
        XCTAssertEqual(rig.store.cueStates[TestSnapshots.intro]?.state, .running, "the cue plays on")
        XCTAssertTrue(prefs.audioMessage.isHidden)
    }

    /// Drawn again, the device table holds only what the engine lists now: the rows of an earlier
    /// drawing are gone, an unplugged device's with them.
    func testReloadingDrawsOnlyTheListedDevices() async throws {
        let prefs = try await open()
        prefs.reload()
        await prefs.refreshing?.value
        prefs.reload()
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.deviceGrid.numberOfRows, 1)
        XCTAssertEqual(prefs.deviceGrid.subviews.count, prefs.deviceGrid.numberOfRows * 2, "one name, one pop-up")
        guard case .success = await engine.request(.loseDevice) else { return XCTFail("lose_device") }
        try await engine.advance(seconds: 1.5)
        prefs.reload()
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.deviceGrid.numberOfRows, 0)
        XCTAssertTrue(prefs.deviceGrid.subviews.isEmpty, "the unplugged device's row is gone")
    }

    func testTheDeviceListComesFromTheEngine() async throws {
        let prefs = try await open()
        XCTAssertEqual(prefs.clockPopup.itemTitles, [loc("settings.audio.systemDefault"), device])
        XCTAssertEqual(prefs.clockPopup.indexOfSelectedItem, 0, "none chosen: the system default")
        XCTAssertTrue(prefs.clockNote.isHidden)
    }

    func testChoosingTheClockDeviceKeepsItForTheNextStart() async throws {
        let prefs = try await open()
        try pick(device, in: prefs.clockPopup)
        await prefs.refreshing?.value
        let kept = try await listed()
        XCTAssertEqual(kept.selected, device, "the engine keeps it for its next start")
        // It is what plays now: nothing to restart for.
        XCTAssertTrue(prefs.clockNote.isHidden)

        try pick(loc("settings.audio.systemDefault"), in: prefs.clockPopup)
        await prefs.refreshing?.value
        let cleared = try await listed()
        XCTAssertNil(cleared.selected)
    }

    func testAChosenClockDeviceThatIsNoLongerListedShowsAsNotAvailable() async throws {
        let prefs = try await open()
        try pick(device, in: prefs.clockPopup)
        await prefs.refreshing?.value
        guard case .success = await engine.request(.loseDevice) else { return XCTFail("lose_device") }
        try await engine.advance(seconds: 1.5)
        prefs.reload()
        await prefs.refreshing?.value
        XCTAssertEqual(
            prefs.clockPopup.titleOfSelectedItem, String(format: loc("settings.audio.notAvailable"), device))
        XCTAssertEqual(prefs.clockNote.stringValue, String(format: loc("settings.audio.chosenMissing"), device))
        XCTAssertFalse(prefs.clockNote.isHidden)
        XCTAssertTrue(prefs.bufferPopups.isEmpty, "an unplugged device has no buffer to set")
    }

    func testARefusedChoiceShowsTheEnginesReason() async throws {
        let prefs = try await open()
        let model = try XCTUnwrap(prefs.audio)
        await model.chooseDevice("Scarlett")  // no such device: the engine refuses it
        prefs.renderAudio()
        let kept = try await listed()
        XCTAssertNil(kept.selected)
        XCTAssertTrue(prefs.audioMessage.stringValue.contains("Scarlett"), prefs.audioMessage.stringValue)
        XCTAssertFalse(prefs.audioMessage.isHidden)
    }

    func testWithoutAnEngineNothingCanBeChanged() async throws {
        engine.stop()
        let prefs = try await open()
        XCTAssertFalse(prefs.clockPopup.isEnabled)
        XCTAssertFalse(prefs.audioMessage.stringValue.isEmpty)
    }

    /// The tab at the window's least size: the long device names are cut, nothing is clipped.
    func testTheAudioTabAtTheLeastSizeKeepsItsControlsInside() async throws {
        let prefs = try await open()
        let window = try XCTUnwrap(prefs.window)
        window.setContentSize(window.contentMinSize)
        window.contentView?.layoutSubtreeIfNeeded()
        let page = try XCTUnwrap(prefs.tabs.selectedTabViewItem?.view)
        let inside = page.convert(page.bounds, to: nil)
        for control in [prefs.clockPopup] + prefs.bufferPopups as [NSView] {
            XCTAssertTrue(inside.contains(control.convert(control.bounds, to: nil)), "\(control)")
        }
        // The device list fills the tab's width and the height left below the headings.
        let list = try XCTUnwrap(prefs.deviceGrid.enclosingScrollView)
        let frame = list.convert(list.bounds, to: nil)
        XCTAssertEqual(frame.width, inside.width - 32, accuracy: 1)
        XCTAssertLessThan(frame.minY - inside.minY, 60, "the list reaches near the tab's bottom")
    }
}
