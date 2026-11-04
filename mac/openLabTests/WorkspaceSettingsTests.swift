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

    // MARK: Patch faders and volume limits (PB-11)

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
    private func type(_ text: String, into field: NSTextField) throws {
        let window = try XCTUnwrap(field.window)
        window.makeFirstResponder(field)
        let editor = try XCTUnwrap(field.currentEditor(), "the field is being edited")
        editor.string = text
        (field.delegate as? NSTextFieldDelegate)?.controlTextDidChange?(
            Notification(name: NSControl.textDidChangeNotification, object: field))
        window.makeFirstResponder(nil)
    }

    /// The settings window and the patch editor opened from its Edit button, on a show that is playing
    /// Intro through the patch, with the meters on.
    private func playing(_ edit: @escaping (URL) throws -> Void) async throws -> (
        Rig, WorkspaceSettingsWindowController, PatchEditorWindowController
    ) {
        let (rig, sc) = try await open(edit: edit)
        sc.tabs.selectTabViewItem(withIdentifier: "audio")
        sc.patchMeter.updateLive()
        sc.patchEditButton.performClick(nil)
        let editor = try XCTUnwrap(sc.patchEditor, "the Edit button opened the patch editor")
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

    func testMainAndOutputFadersInTheRoutingTab() async throws {
        let (_, sc) = try await open()
        sc.patchEditButton.performClick(nil)
        let editor = try XCTUnwrap(sc.patchEditor)
        XCTAssertTrue(editor.window?.isVisible == true)
        XCTAssertEqual(editor.tabs.selectedTabViewItem?.label, loc("patchEditor.tab.routing"))
        // One main fader, then one per output of the stereo patch, each with its dB box, all at 0 dB.
        XCTAssertEqual(editor.outputFaders.count, 2)
        editor.window?.contentView?.layoutSubtreeIfNeeded()
        let faders = [editor.mainFader] + editor.outputFaders
        let fields = [editor.mainField] + editor.outputFields
        let xs = faders.map { $0.convert($0.bounds, to: nil).minX }
        XCTAssertEqual(xs, xs.sorted(), "the main fader comes first")
        XCTAssertEqual(Set(xs).count, 3, "three faders side by side")
        for (fader, field) in zip(faders, fields) {
            XCTAssertEqual(fader.doubleValue, 0)
            XCTAssertEqual(field.stringValue, "0.0")
            XCTAssertLessThan(field.convert(field.bounds, to: nil).maxY, fader.convert(fader.bounds, to: nil).maxY)
        }
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

    func testAnOutputFaderSilencesThatOutputAndLeavesTheOther() async throws {
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

    func testTheFaderLevelsSurviveSaveAndReopen() async throws {
        let (rig, _, editor) = try await playing(sounding())
        try type("-6", into: editor.mainField)
        try type("-3", into: editor.outputFields[1])
        await assertEventually { self.patchLevels(rig).outputs == [0, -3] && self.patchLevels(rig).main == -6 }
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
        let sc = WorkspaceSettingsWindowController(store: reopened.store)
        await sc.present().value
        sc.patchEditButton.performClick(nil)
        let again = try XCTUnwrap(sc.patchEditor)
        await assertEventually { again.mainField.stringValue == "-6.0" }
        XCTAssertEqual(again.outputFields.map(\.stringValue), ["0.0", "-3.0"])
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
        func texts(_ view: NSView?) -> [String] {
            guard let view else { return [] }
            var out = view.subviews.flatMap { texts($0) }
            if let field = view as? NSTextField { out.append(field.stringValue) }
            if let button = view as? NSButton { out.append(button.title) }
            if let tabs = view as? NSTabView { out += tabs.tabViewItems.flatMap { [$0.label] + texts($0.view) } }
            return out
        }
        let shown = texts(sc.window?.contentView) + texts(editor.window?.contentView)
        XCTAssertFalse(shown.contains { $0.localizedCaseInsensitiveContains("master") }, "\(shown)")
        // It plays as the file's levels say: the tone at its own level, not 2 dB down.
        let level = try await peak(rig, channel: 0)
        XCTAssertEqual(Double(level), 3000.0 / 32768, accuracy: 0.003)
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
