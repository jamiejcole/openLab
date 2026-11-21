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
        guard case .success = await engine.request(.loseDevice()) else { return XCTFail("lose_device") }
        try await engine.advance(seconds: 1.5)
        prefs.reload()
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.deviceGrid.numberOfRows, 0)
        XCTAssertTrue(prefs.deviceGrid.subviews.isEmpty, "the unplugged device's row is gone")
    }

    /// Flow (#752): a device with a long name shows whole when the window has room, is cut at its end only at
    /// the least width, and the buffer-size pop-up stays in line to its right with nothing overlapping.
    /// Unit test (openLabTests), the real window laid out; the long name is set on the model, as the host's
    /// engine lists no such device.
    func testALongDeviceNameShowsWholeOrIsCutAtItsEndAndThePopUpStaysInLine() async throws {
        let prefs = try await open()
        let model = try XCTUnwrap(prefs.audio)
        let long = "openLab Virtual Output With A Long Interface Name 2"
        let json = """
            [{"name":"\(long)","output_channels":2,"buffer_frames":512,"primary":true},
             {"name":"Short","output_channels":2,"buffer_frames":256}]
            """
        model.devices = try JSONDecoder().decode([DeviceEntry].self, from: Data(json.utf8))
        prefs.renderAudio()
        let window = try XCTUnwrap(prefs.window)
        func layout(width: CGFloat) throws -> (name: NSTextField, popup: NSPopUpButton, list: NSView) {
            window.setContentSize(NSSize(width: width, height: 380))
            window.contentView?.layoutSubtreeIfNeeded()
            prefs.deviceGrid.layoutSubtreeIfNeeded()
            let stack = try XCTUnwrap(prefs.deviceGrid.cell(atColumnIndex: 0, rowIndex: 0).contentView as? NSStackView)
            let name = try XCTUnwrap(stack.arrangedSubviews.first as? NSTextField)
            return (name, prefs.bufferPopups[0], try XCTUnwrap(prefs.deviceGrid.superview))
        }
        for width in [CGFloat(700), PreferencesWindowController.Layout.leastSize.width] {
            let (name, popup, list) = try layout(width: width)
            let n = name.convert(name.bounds, to: list)
            let p = popup.convert(popup.bounds, to: list)
            XCTAssertEqual(name.stringValue, long, "the name is never changed")
            XCTAssertEqual(name.lineBreakMode, .byTruncatingTail, "a cut is at the end")
            XCTAssertLessThanOrEqual(n.maxX, p.minX, "the name ends before the pop-up at \(width)")
            XCTAssertLessThanOrEqual(p.maxX, list.bounds.maxX, "the pop-up is inside the list at \(width)")
            XCTAssertGreaterThanOrEqual(n.minX, list.bounds.minX)
            if width == 700 {
                XCTAssertGreaterThanOrEqual(n.width, name.attributedStringValue.size().width, "whole at 700")
            }
            let short = try XCTUnwrap(prefs.deviceGrid.cell(atColumnIndex: 1, rowIndex: 1).contentView)
            XCTAssertEqual(
                short.convert(short.bounds, to: list).maxX, p.maxX, accuracy: 0.5, "the pop-ups line up")
        }
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
        guard case .success = await engine.request(.loseDevice()) else { return XCTFail("lose_device") }
        try await engine.advance(seconds: 1.5)
        prefs.reload()
        await prefs.refreshing?.value
        XCTAssertEqual(
            prefs.clockPopup.titleOfSelectedItem, String(format: loc("settings.audio.notAvailable"), device))
        XCTAssertEqual(prefs.clockNote.stringValue, String(format: loc("settings.audio.chosenMissing"), device))
        XCTAssertFalse(prefs.clockNote.isHidden)
        XCTAssertTrue(prefs.bufferPopups.isEmpty, "an unplugged device has no buffer to set")
        XCTAssertEqual(prefs.deviceGrid.numberOfRows, 1, "the device set earlier is still listed")
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
        // The connection is being retried, so the window says openLab is starting rather than failing.
        XCTAssertEqual(prefs.deviceListNote.stringValue, loc("prefs.engine.starting"))
        XCTAssertFalse(prefs.deviceListNote.isHidden)
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
        XCTAssertEqual(frame.width, inside.width - 40, accuracy: 1)
        XCTAssertGreaterThanOrEqual(frame.height, 80, "the list keeps room for a row")
    }

    // MARK: Flows (#773)

    private func detail(_ prefs: PreferencesWindowController, row: Int) throws -> String {
        let stack = try XCTUnwrap(prefs.deviceGrid.cell(atColumnIndex: 0, rowIndex: row).contentView as? NSStackView)
        return try XCTUnwrap(stack.arrangedSubviews.last as? NSTextField).stringValue
    }

    private func name(_ prefs: PreferencesWindowController, row: Int) throws -> String {
        let stack = try XCTUnwrap(prefs.deviceGrid.cell(atColumnIndex: 0, rowIndex: row).contentView as? NSStackView)
        return try XCTUnwrap(stack.arrangedSubviews.first as? NSTextField).stringValue
    }

    private func secondDevice() async throws {
        if case .failure(let e) = await engine.request(.restoreDevice(device: TestEngine.deviceIDs[1])) {
            return XCTFail("restore_device: \(e)")
        }
        try await engine.advance(seconds: 2)
    }

    /// Flow "Audio: devices with channel counts; choose a different clock device".
    func testChoosingAnotherClockDeviceMarksItAndAPlayingCuePlaysOn() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        try await secondDevice()
        guard case .success = await rig.store.transport(.start(cue: TestSnapshots.intro)) else {
            return XCTFail("start")
        }
        try await rig.advance(1)
        let pid = engine.pid
        let prefs = try await open()
        XCTAssertEqual(prefs.deviceGrid.numberOfRows, 2)
        XCTAssertEqual(
            try detail(prefs, row: 0),
            String(format: loc("prefs.audio.outputs"), 2) + " · " + loc("prefs.audio.clockNow"))
        XCTAssertEqual(
            try detail(prefs, row: 1), String(format: loc("prefs.audio.outputs"), 2), "channels, no clock mark")

        try pick(TestEngine.deviceNames[1], in: prefs.clockPopup)
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.clockPopup.titleOfSelectedItem, TestEngine.deviceNames[1])
        XCTAssertTrue(
            try detail(prefs, row: 1).hasSuffix(loc("prefs.audio.clockNext")), "the list marks the new clock device")
        XCTAssertEqual(
            prefs.clockNote.stringValue, loc("settings.audio.restartNeeded"), "and says when it takes effect")
        try await rig.advance(1)
        XCTAssertEqual(rig.store.cueStates[TestSnapshots.intro]?.state, .running, "the cue plays on")
        XCTAssertEqual(engine.pid, pid)
        XCTAssertTrue(prefs.audioMessage.isHidden)
    }

    /// Flow "Audio: change one device's buffer size", including that it persists across a relaunch.
    func testOneDevicesBufferSizeChangesAloneAndIsKeptAcrossARelaunch() async throws {
        try await secondDevice()
        let prefs = try await open()
        XCTAssertEqual(prefs.bufferPopups.map { $0.titleOfSelectedItem }, ["512", "512"])
        try pick("128", in: prefs.bufferPopups[1])
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.bufferPopups.map { $0.titleOfSelectedItem }, ["512", "128"], "only that device changed")
        let now = try await listed()
        XCTAssertEqual(now.devices.map(\.bufferFrames), [512, 128])

        let saved = await engine.savedSettings(containing: ": 128")
        let kept = try XCTUnwrap(saved, "the engine wrote the size to this machine's settings")
        engine.stop()
        engine = try await TestEngine.start(settings: kept)
        try await secondDevice()
        let again = try await open()
        XCTAssertEqual(again.bufferPopups.map { $0.titleOfSelectedItem }, ["512", "128"], "kept for the next launch")
    }

    /// Flow "An output device that was set earlier is unplugged".
    func testAnUnpluggedClockDeviceStaysListedAsMissingAndTheOthersStayEditable() async throws {
        try await secondDevice()
        let prefs = try await open()
        try pick(TestEngine.deviceNames[1], in: prefs.clockPopup)
        await prefs.refreshing?.value
        guard case .success = await engine.request(.loseDevice(device: TestEngine.deviceIDs[1])) else {
            return XCTFail("lose_device")
        }
        try await engine.advance(seconds: 2)
        prefs.reload()
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.deviceGrid.numberOfRows, 2, "the missing device is a row, not dropped")
        XCTAssertEqual(try name(prefs, row: 1), TestEngine.deviceNames[1])
        XCTAssertEqual(try detail(prefs, row: 1), loc("prefs.audio.missing"))
        let missing = try XCTUnwrap(prefs.deviceGrid.cell(atColumnIndex: 1, rowIndex: 1).contentView as? NSPopUpButton)
        XCTAssertFalse(missing.isEnabled, "nothing to set on a device that is not there")
        XCTAssertEqual(prefs.bufferPopups.count, 1)
        XCTAssertTrue(prefs.bufferPopups[0].isEnabled)
        try pick("64", in: prefs.bufferPopups[0])
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.bufferPopups[0].titleOfSelectedItem, "64")
        XCTAssertTrue(prefs.audioMessage.isHidden)
    }

    /// Flow "Engine still starting": the window opens, the engine's controls wait, then fill in.
    func testTheWindowOpensWhileTheEngineStartsAndFillsInWithoutReopening() async throws {
        let infoURL = engine.infoURL
        let client = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: infoURL) },
            workspace: nil)
        let suite = "openlab-prefs-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        let prefs = PreferencesWindowController(
            preferences: LaunchPreferences(defaults: defaults), audio: AudioPreferencesModel(client: client))
        await prefs.refreshing?.value
        XCTAssertFalse(prefs.clockPopup.isEnabled)
        XCTAssertEqual(prefs.clockPopup.titleOfSelectedItem, loc("prefs.engine.waiting"))
        XCTAssertFalse(prefs.backupField.isEnabled)
        XCTAssertEqual(prefs.deviceListNote.stringValue, loc("prefs.engine.starting"))
        XCTAssertTrue(prefs.audioMessage.isHidden, "starting is not an error")
        XCTAssertTrue(prefs.launchPopup.isEnabled, "the launch choice needs no engine")

        client.start()
        await assertEventually("the controls fill in") { prefs.clockPopup.isEnabled && prefs.bufferPopups.count == 1 }
        await prefs.refreshing?.value
        XCTAssertTrue(prefs.deviceListNote.isHidden)
        XCTAssertTrue(prefs.backupField.isEnabled)
        XCTAssertFalse(prefs.backupField.stringValue.isEmpty)
        client.stop()
    }

    /// Flow "Show Mode on, or a cue playing": read and change; a change applies, the cue plays on.
    func testSettingsAreReadableAndEditableInShowModeWithACuePlaying() async throws {
        let rig = try await Rig.open("Demo", on: engine)
        guard case .success = await rig.store.setShowMode(true).value else { return XCTFail("show mode") }
        guard case .success = await rig.store.transport(.start(cue: TestSnapshots.intro)) else {
            return XCTFail("start")
        }
        try await rig.advance(1)
        let prefs = try await open()
        XCTAssertTrue(prefs.clockPopup.isEnabled)
        XCTAssertEqual(prefs.bufferPopups.count, 1)
        XCTAssertTrue(prefs.backupField.isEnabled)
        try pick("1024", in: prefs.bufferPopups[0])
        await prefs.refreshing?.value
        XCTAssertEqual(prefs.bufferPopups[0].titleOfSelectedItem, "1024")
        XCTAssertTrue(prefs.audioMessage.isHidden, "applied without a refusal")
        try await rig.advance(1)
        XCTAssertEqual(rig.store.cueStates[TestSnapshots.intro]?.state, .running)
    }

    /// Flow "Narrow window size, light and dark": at the least size on each tab no text is clipped,
    /// every control has an accessibility label, and the two tabs' controls start at the same place.
    func testBothTabsAtTheLeastSizeInLightAndDarkClipNothingAndEveryControlIsLabelled() async throws {
        let prefs = try await open()
        let window = try XCTUnwrap(prefs.window)
        var controlLeft: [CGFloat] = []
        for appearance in [NSAppearance.Name.aqua, .darkAqua] {
            window.appearance = NSAppearance(named: appearance)
            window.setContentSize(window.contentMinSize)
            controlLeft = []
            for tab in ["general", "audio"] {
                prefs.tabs.selectTabViewItem(withIdentifier: tab)
                await prefs.refreshing?.value
                let page = try XCTUnwrap(prefs.tabs.selectedTabViewItem?.view)
                window.contentView?.layoutSubtreeIfNeeded()
                page.layoutSubtreeIfNeeded()
                let inside = page.convert(page.bounds, to: nil)
                func walk(_ v: NSView) {
                    if v.isHidden { return }
                    let frame = v.convert(v.bounds, to: nil)
                    if v is NSControl || v is NSScrollView {
                        XCTAssertTrue(inside.contains(frame), "\(tab) \(appearance.rawValue): \(v) leaves the page")
                    }
                    if let f = v as? NSTextField, !f.stringValue.isEmpty, f.lineBreakMode != .byTruncatingTail,
                        !f.isEditable
                    {
                        let need = f.attributedStringValue.boundingRect(
                            with: NSSize(width: f.bounds.width, height: .greatestFiniteMagnitude),
                            options: [.usesLineFragmentOrigin]
                        ).height
                        XCTAssertLessThanOrEqual(need, f.bounds.height + 1, "\(tab): \"\(f.stringValue)\" is clipped")
                    }
                    if v is NSPopUpButton || (v is NSTextField && (v as! NSTextField).isEditable) {
                        XCTAssertFalse(
                            (v.accessibilityLabel() ?? "").isEmpty, "\(tab): a control has no accessibility label")
                    }
                    for s in v.subviews { walk(s) }
                }
                walk(page)
                let first = tab == "general" ? prefs.launchPopup : prefs.clockPopup
                controlLeft.append(first.convert(first.bounds, to: nil).minX)
            }
            XCTAssertEqual(controlLeft[0], controlLeft[1], accuracy: 0.5, "the tabs' controls line up")
        }
    }
}
