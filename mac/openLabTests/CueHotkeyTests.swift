// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

final class TriggerCommandTests: XCTestCase {
    private func shape(_ c: Command) throws -> JSONValue? { JSONValue.parse(try c.json.text()) }
    private let id: ModelID = "00000000-0000-0000-0000-00000000000a"

    func testTriggerCommandsMatchCore() throws {
        let hk = KeyChord("k", [.command, .shift]).hotkeyJSON
        let mods = #"{"shift":true,"control":false,"alt":false,"command":true}"#
        let cases: [(Command, String)] = [
            (
                .setProperty(id, .triggers(hotkey: hk, osc: "/go/1")),
                #"{"type":"triggers","value":{"hotkey":{"key":"k","modifiers":\#(mods)},"osc":"/go/1"}}"#
            ),
            (
                .setProperty(id, .triggers(hotkey: nil, osc: nil)),
                #"{"type":"triggers","value":{"hotkey":null,"osc":null}}"#
            ),
            (.setProperty(id, .secondTrigger("hard_stop")), #"{"type":"second_trigger","value":"hard_stop"}"#),
            (.setProperty(id, .secondTriggerOnRelease(true)), #"{"type":"second_trigger_on_release","value":true}"#),
        ]
        for (command, property) in cases {
            XCTAssertEqual(
                try shape(command),
                JSONValue.parse(#"{"type":"set_cue_property","id":"\#(id)","property":\#(property)}"#), property)
        }
    }

    func testHotkeyJSONRoundTripsThroughTheShowFileForm() {
        let chords = [
            KeyChord("a"), KeyChord(" "), KeyChord("["), KeyChord(KeyChord.left), KeyChord(KeyChord.down, [.command]),
            KeyChord("\r", [.option]), KeyChord(String(utf16CodeUnits: [unichar(NSF5FunctionKey)], count: 1)),
        ]
        for chord in chords {
            XCTAssertEqual(chord.hotkeyJSON.flatMap { KeyChord(hotkey: $0) }, chord, chord.display)
        }
        XCTAssertEqual(KeyChord(" ").hotkeyJSON?["key"]?.stringValue, "space")
        XCTAssertEqual(
            KeyChord(String(utf16CodeUnits: [unichar(NSF5FunctionKey)], count: 1)).hotkeyJSON?["key"]?.stringValue, "f5"
        )
        XCTAssertNil(KeyChord("\u{7F}").hotkeyJSON, "Delete has no name")
        XCTAssertEqual(KeyChord("k", [.command, .shift]).display, "⇧⌘K")
    }

    func testOSCAddressValidation() {
        for ok in ["/go", "/show/cue-1", "/a/b_c.d"] { XCTAssertTrue(InspectorEdit.isValidOSCAddress(ok), ok) }
        for bad in [
            "", "/", "go", "/a b", "/a*", "/a?", "/{x}", "/é", "/a\n", "/a#1", "/" + String(repeating: "a", count: 300),
        ] {
            XCTAssertFalse(InspectorEdit.isValidOSCAddress(bad), bad)
        }
    }
}

/// Hotkey rules against the demo show; the transport keys are the defaults.
final class HotkeyRulesTests: XCTestCase {
    private var model: WorkspaceModel!
    private let intro: ModelID = "00000000-0000-0000-0000-00000000000a"

    override func setUpWithError() throws {
        let snap = try Fixtures.json("server", "welcome")["snapshot"]
        model = try JSONDecoder().decode(WorkspaceModel.self, from: Data(try XCTUnwrap(snap?["workspace"]).text().utf8))
    }

    private func validate(_ c: KeyChord, menu: Set<KeyChord> = []) -> KeyBindError? {
        HotkeyRules.validate(c, for: intro, in: model, keys: TransportKeys(settings: model.settings), menuChords: menu)
    }

    func testRefusals() {
        XCTAssertEqual(validate(KeyChord(KeyChord.escape)), .reserved)
        XCTAssertEqual(validate(KeyChord(KeyChord.up)), .reserved)
        XCTAssertEqual(validate(KeyChord(KeyChord.down)), .reserved)
        XCTAssertEqual(validate(KeyChord(KeyChord.down, .command)), .reserved, "any modifiers")
        XCTAssertEqual(validate(KeyChord(KeyChord.up, [.control, .option])), .reserved)
        XCTAssertEqual(validate(KeyChord(KeyChord.escape, .shift)), .reserved)
        XCTAssertEqual(validate(KeyChord(" ")), .duplicate(of: .go))
        XCTAssertEqual(validate(KeyChord("[")), .duplicate(of: .pauseAll))
        XCTAssertNil(validate(KeyChord("p")), "a fixed app default (L, P, V) yields to a cue hotkey")
        XCTAssertEqual(validate(KeyChord("\u{7F}")), .unsupported)
        XCTAssertEqual(validate(KeyChord("q", [.command]), menu: [KeyChord("q", [.command])]), .menuKey)
    }

    /// The real menu bar's chords: an upper-case key equivalent (AppKit adds Shift) must collide with the
    /// Shift chord a cue hotkey is recorded as (#454).
    @MainActor
    private func validateAgainstMenuBar(_ key: String, _ modifiers: NSEvent.ModifierFlags) -> KeyBindError? {
        let saved = NSApp.mainMenu
        NSApp.mainMenu = MainMenu.make().bar
        defer { NSApp.mainMenu = saved }
        return validate(KeyChord(key, modifiers), menu: HotkeyRules.currentMenuChords())
    }

    /// Find's keys (ED-09) are menu keys, so a cue cannot take them (and a loaded one never fires on them).
    @MainActor
    func testFindKeysAreRefusedAsCueHotkeys() {
        XCTAssertEqual(validateAgainstMenuBar("f", [.command]), .menuKey)
        XCTAssertEqual(validateAgainstMenuBar("g", [.command]), .menuKey)
        XCTAssertEqual(validateAgainstMenuBar("g", [.command, .shift]), .menuKey)
        XCTAssertEqual(validateAgainstMenuBar("8", [.command]), .menuKey, "New Network Cue")
        XCTAssertNil(validateAgainstMenuBar("f", []), "a plain F is still a cue's to take")
    }

    @MainActor
    func testRedoShiftCmdZIsRefused() {
        XCTAssertEqual(validateAgainstMenuBar("z", [.command, .shift]), .menuKey)
    }

    @MainActor
    func testSaveAsShiftCmdSIsRefused() {
        XCTAssertEqual(validateAgainstMenuBar("s", [.command, .shift]), .menuKey)
    }

    @MainActor
    func testPreShowShiftCmdWIsRefused() {
        XCTAssertEqual(validateAgainstMenuBar("w", [.command, .shift]), .menuKey)
    }

    func testAllowedKeys() {
        XCTAssertNil(validate(KeyChord("a")))
        XCTAssertNil(validate(KeyChord(KeyChord.left)), "QLab allows left and right")
        XCTAssertNil(validate(KeyChord(" ", [.shift])))
    }

    func testAnotherCuesHotkeyIsRefusedButTheCuesOwnIsNot() throws {
        var snap = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let hk = try XCTUnwrap(KeyChord("a").hotkeyJSON)
        var lists = try XCTUnwrap(snap["workspace"]?["cue_lists"]?.arrayValue)
        var cues = try XCTUnwrap(lists[0]["cues"]?.arrayValue)
        cues[1]["triggers"]?.set("hotkey", hk)
        lists[0].set("cues", .array(cues))
        snap["workspace"]?.set("cue_lists", .array(lists))
        model = try JSONDecoder().decode(WorkspaceModel.self, from: Data(try XCTUnwrap(snap["workspace"]).text().utf8))
        XCTAssertEqual(validate(KeyChord("a")), .cueHotkey)
        XCTAssertNil(
            HotkeyRules.validate(
                KeyChord("a"), for: "00000000-0000-0000-0000-00000000000b", in: model,
                keys: TransportKeys(settings: nil), menuChords: []))
        XCTAssertEqual(model.cueHotkeys.count, 1)
    }
}

/// The inspector's Triggers tab on the real engine: what is recorded is what the engine stores.
@MainActor
final class TriggersTabTests: XCTestCase {
    private let intro = CueListHarness.intro
    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!

    override func setUp() async throws {
        rig = try await Rig.open()
        vc = InspectorViewController(store: rig.store)
        vc.menuChords = { [KeyChord("s", [.command])] }
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
        vc.setSelection([intro])
    }

    override func tearDown() async throws { window.orderOut(nil) }

    /// The cue's triggers as the engine sent them.
    private func triggers(_ cue: ModelID? = nil) -> JSONValue? { rig.store.model?.cue(cue ?? intro)?.raw["triggers"] }
    private func field(_ cue: ModelID? = nil, _ name: String) -> JSONValue? {
        rig.store.model?.cue(cue ?? intro)?.raw[name]
    }

    // MARK: Show Mode (SM-01)

    func testLockedInspectorBlocksEveryEditPathAndUnlockRestores() async {
        let log = CueEditingTests.SendLog(rig)
        let before = rig.store.model?.cue(intro)
        vc.isLocked = true
        let controls: [NSControl] = [
            vc.numberField, vc.nameField, vc.preWaitField, vc.postWaitField, vc.oscField, vc.hotkeyField,
            vc.hotkeyClear, vc.secondTriggerPopup, vc.onReleaseCheck, vc.colourPopup, vc.continuePopup,
            vc.flaggedCheck, vc.armedCheck,
        ]
        XCTAssertTrue(controls.allSatisfy { !$0.isEnabled })
        XCTAssertFalse(vc.notesView.isEditable)
        XCTAssertFalse(vc.hotkeyField.acceptsFirstResponder, "the recorder cannot take focus")

        XCTAssertFalse(vc.record(KeyChord("k", [.command, .shift])))
        vc.clearHotkey()
        vc.nameField.stringValue = "Changed"
        vc.commit(.name)
        vc.colourPopup.selectItem(at: 2)
        _ = vc.colourPopup.target?.perform(vc.colourPopup.action, with: vc.colourPopup)
        vc.flaggedCheck.state = .on
        _ = vc.flaggedCheck.target?.perform(vc.flaggedCheck.action, with: vc.flaggedCheck)
        await settle()
        XCTAssertEqual(log.sent, 0, "nothing reached the engine")
        await rig.sync()
        XCTAssertEqual(rig.store.model?.cue(intro), before)

        // A re-render while locked keeps the controls locked.
        vc.render(force: Set(InspectorField.allCases))
        XCTAssertFalse(vc.nameField.isEnabled)

        vc.isLocked = false
        XCTAssertTrue(vc.nameField.isEnabled)
        XCTAssertTrue(vc.notesView.isEditable)
        XCTAssertFalse(vc.hotkeyClear.isEnabled, "no hotkey yet: Clear stays off as in Edit Mode")
        XCTAssertTrue(vc.record(KeyChord("k", [.command, .shift])))
        await assertEventually { vc.hotkeyField.stringValue == "⇧⌘K" }
    }

    func testTabExistsAfterBasics() {
        XCTAssertEqual(
            vc.tabView.tabViewItems.map(\.label),
            [
                loc("inspector.tab.basics"), loc("inspector.tab.triggers"), loc("inspector.tab.timeLoops"),
                loc("inspector.tab.levels"),
            ])
    }

    func testRecordingAHotkeySendsTriggersAndShowsItAfterTheEngineAnswers() async {
        XCTAssertTrue(vc.record(KeyChord("k", [.command, .shift])))
        await assertEventually { vc.hotkeyField.stringValue == "⇧⌘K" }
        XCTAssertEqual(triggers()?["hotkey"], KeyChord("k", [.command, .shift]).hotkeyJSON, "the engine stored it")
        XCTAssertTrue(vc.hotkeyClear.isEnabled)
        XCTAssertTrue(vc.onReleaseCheck.isEnabled)
    }

    func testRefusedKeysSendNothingAndShowALocalisedMessage() async {
        let log = CueEditingTests.SendLog(rig)
        let cases: [(KeyChord, String)] = [
            (KeyChord(KeyChord.escape), "inspector.hotkey.reserved"),
            (KeyChord(KeyChord.up), "inspector.hotkey.reserved"),
            (KeyChord(KeyChord.down, [.command, .shift]), "inspector.hotkey.reserved"),
            (KeyChord(" "), "inspector.hotkey.transport"),
            (KeyChord("s", [.command]), "inspector.hotkey.menu"),
            (KeyChord("\u{7F}"), "inspector.hotkey.unsupported"),
        ]
        let shown = vc.hotkeyField.stringValue
        for (chord, key) in cases {
            XCTAssertFalse(vc.record(chord))
            XCTAssertEqual(vc.hotkeyField.stringValue, shown, "a refused key does not appear in the field")
            XCTAssertEqual(vc.messageLabel.stringValue, loc(key))
            XCTAssertNotEqual(loc(key), key, "string exists")
        }
        await settle()
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(triggers()?["hotkey"], .null)
    }

    func testAnotherCuesHotkeyIsRefused() async {
        XCTAssertTrue(vc.record(KeyChord("a")))
        await assertEventually { vc.hotkeyField.stringValue == "A" }
        vc.setSelection([CueListHarness.fade])
        let log = CueEditingTests.SendLog(rig)
        XCTAssertFalse(vc.record(KeyChord("a")))
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.hotkey.otherCue"))
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(triggers(CueListHarness.fade)?["hotkey"], .null)
    }

    func testClearKeepsTheOSCAddressAndOSCEditKeepsTheHotkey() async {
        vc.oscField.stringValue = "/cue/one"
        vc.commit(.osc)
        await assertEventually { self.triggers()?["osc"]?.stringValue == "/cue/one" }
        XCTAssertEqual(vc.oscField.stringValue, "/cue/one")
        XCTAssertTrue(vc.record(KeyChord("a")))
        await assertEventually { self.triggers()?["hotkey"] == KeyChord("a").hotkeyJSON }
        XCTAssertEqual(triggers()?["osc"]?.stringValue, "/cue/one", "OSC kept")
        await assertEventually { vc.hotkeyClear.isEnabled }
        vc.clearHotkey()
        await assertEventually { self.triggers()?["hotkey"] == .null && !vc.hotkeyClear.isEnabled }
        XCTAssertEqual(triggers()?["osc"]?.stringValue, "/cue/one")
        vc.oscField.stringValue = ""
        vc.commit(.osc)
        await assertEventually { self.triggers()?["osc"] == .null }
    }

    /// The first edit is refused by the engine (the cue it names exists only in the store, as when
    /// the store is behind); the cue is then added for real, and the next edit must not carry the
    /// refused address along.
    func testRefusedTriggersAreNotSentAgainWithTheNextEdit() async throws {
        let id: ModelID = "00000000-0000-0000-0000-0000000000ee"
        var cue = try XCTUnwrap(NewCueKind.memo.cueJSON(id: id, patch: nil))
        cue.set("name", .string("Late"))
        let upsert = try CueListHarness.upsertJSON(cue, index: 0)
        let frame =
            #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#
        XCTAssertEqual(rig.store.receive(try ServerMessage.decode(frame)), .applied)
        vc.setSelection([id])
        vc.oscField.stringValue = "/refused"
        vc.commit(.osc)
        await assertEventually { self.vc.messageLabel.stringValue.contains("does not exist") }
        await assertEventually { vc.oscField.stringValue == "" }
        // Now the engine has the cue (the store already shows it exactly so).
        let added = await rig.store.send(
            command: .addCue(cue, list: TestSnapshots.mainList, parent: nil, index: 0))
        guard case .success = added else { return XCTFail("\(added)") }
        XCTAssertTrue(vc.record(KeyChord("a")))
        rig.client.requestSnapshot()  // the engine's own copy, not the one the store was given
        await assertEventually { self.triggers(id)?["hotkey"] == KeyChord("a").hotkeyJSON }
        XCTAssertEqual(triggers(id)?["osc"], .null, "the refused address is gone")
    }

    func testBadOSCAddressRevertsWithoutSending() async {
        let log = CueEditingTests.SendLog(rig)
        vc.oscField.stringValue = "no slash"
        XCTAssertFalse(vc.commit(.osc))
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.osc"))
        XCTAssertEqual(vc.oscField.stringValue, "")
        await settle()
        XCTAssertEqual(log.sent, 0)
    }

    func testSecondTriggerAndOnReleaseSend() async {
        vc.record(KeyChord("a"))
        await assertEventually { vc.onReleaseCheck.isEnabled }
        vc.secondTriggerPopup.selectItem(at: 2)
        _ = vc.secondTriggerPopup.target?.perform(vc.secondTriggerPopup.action, with: vc.secondTriggerPopup)
        vc.onReleaseCheck.state = .on
        _ = vc.onReleaseCheck.target?.perform(vc.onReleaseCheck.action, with: vc.onReleaseCheck)
        await assertEventually {
            self.field(nil, "second_trigger")?.stringValue == "stop"
                && self.field(nil, "second_trigger_on_release") == .bool(true)
        }
    }

    func testRecorderTakesTheKeyEventAndEscLeavesWithoutRecording() throws {
        let key = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [.command], timestamp: 0, windowNumber: 0,
                context: nil, characters: "k", charactersIgnoringModifiers: "k", isARepeat: false, keyCode: 40))
        var got: [KeyChord] = []
        vc.hotkeyField.onRecord = { if let c = KeyChord(event: $0) { got.append(c) } }
        vc.hotkeyField.keyDown(with: key)
        XCTAssertEqual(got, [KeyChord("k", [.command])])
        let esc = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
                characters: "\u{1B}", charactersIgnoringModifiers: "\u{1B}", isARepeat: false, keyCode: 53))
        vc.hotkeyField.keyDown(with: esc)
        XCTAssertEqual(got.count, 1)
    }
}

/// Cue hotkeys through the app controller with two open workspaces, on an engine of the test's own:
/// the demo show (Intro has hotkey X) and a new workspace whose 1 s Wait "Second" has X too, stopped
/// by the key's release (second trigger Stop, on release). What the engine received is read from its
/// show log, where each hotkey message is a `hotkey` trigger (down and up alike).
@MainActor
final class CueHotkeyFiringTests: XCTestCase {
    private var app: AppController!
    private var engine: TestEngine!
    private var keyDoc: ModelID?
    private let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
    private let second: ModelID = "00000000-0000-0000-0000-0000000000a2"
    private var demoDoc: ModelID = "none"
    private var secondDoc: ModelID = "none"
    private var first: WorkspaceStore { app.session.stores[demoDoc]! }
    private var other: WorkspaceStore { app.session.stores[secondDoc]! }

    override func setUp() async throws {
        engine = try await TestEngine.start()
        demoDoc = try await engine.open("Demo")
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { [unowned self] in keyDoc.flatMap { app.windows[$0]?.window } }
        app.menuChords = { [KeyChord("q", [.command])] }
        session.start()
        await assertEventually { self.app.windows.count == 1 && session.stores.values.allSatisfy { $0.model != nil } }
        guard case .success(let doc) = await session.newWorkspace() else { return XCTFail("new") }
        secondDoc = doc
        await assertEventually { self.app.session.stores[doc]?.model != nil }
        // A cue in the second workspace with the same hotkey as Intro ("x"), stopped by its release.
        let store = try XCTUnwrap(app.session.stores[doc])
        let list = try XCTUnwrap(store.model?.cueLists.first?.id)
        var cue = try XCTUnwrap(NewCueKind.wait.cueJSON(id: second, patch: nil))
        cue.set("name", .string("Second"))
        cue.set("number", .string("9"))
        cue.set("triggers", .object(["hotkey": KeyChord("x").hotkeyJSON ?? .null, "osc": .null]))
        cue.set("second_trigger", .string("stop"))
        cue.set("second_trigger_on_release", .bool(true))
        guard case .success = await store.send(command: .addCue(cue, list: list, parent: nil, index: 0)) else {
            return XCTFail("add")
        }
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord("x").hotkeyJSON, osc: nil)))
        await assertEventually { self.first.model?.cue(self.intro)?.raw["triggers"]?["hotkey"] != .null }
    }

    override func tearDown() async throws {
        app.closeAllWindows()
        app = nil
        engine.stop()
    }

    private func event(
        _ chars: String, _ flags: NSEvent.ModifierFlags = [], type: NSEvent.EventType = .keyDown, repeats: Bool = false,
        code: UInt16 = 7
    ) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: type, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
                characters: chars, charactersIgnoringModifiers: chars, isARepeat: repeats, keyCode: code))
    }

    /// The show log's trigger records whose message is `message` (`hotkey`, `transport go`, `panic`):
    /// what the engine received, down and up alike for a hotkey.
    private func triggers(_ message: String) async -> Int {
        guard case .success(let ok) = await engine.request(.logTail(max: nil)),
            case .logTail(let entries, _, _)? = ok.data
        else { return -1 }
        return entries.filter { $0.kind == "trigger" && $0.message == message }.count
    }

    private func hotkeys() async -> Int { await triggers("hotkey") }

    /// Lets the engine run what it received (the stepped device moves only when told).
    private func run() async throws { try await engine.advance(seconds: 0.05) }

    private func playing(_ store: WorkspaceStore, _ cue: ModelID) -> Bool { store.cueStates[cue]?.state == .running }

    func testKeySendsOneAppScopeHotkeyEvenWithoutAKeyWindow() async throws {
        keyDoc = nil  // no workspace window is key (launcher or alert in front)
        XCTAssertNil(app.handleKeyDown(try event("x")), "consumed")
        await assertEventually { await self.hotkeys() == 1 }
        try await run()
        // The engine did the matching, in every workspace, from one message on the app connection.
        await assertEventually { self.playing(self.first, self.intro) && self.playing(self.other, self.second) }
        await settle()
        let sent = await hotkeys()
        XCTAssertEqual(sent, 1, "one message for both workspaces, not one per workspace")
        let starts = await triggers("transport start")
        XCTAssertEqual(starts, 0, "no per-workspace start")
    }

    func testModifiersGoOnTheWire() async throws {
        let chord = KeyChord("k", [.shift, .control])
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: chord.hotkeyJSON, osc: nil)))
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == chord } == true }
        XCTAssertNil(app.handleKeyDown(try event("k", [.shift, .control], code: 40)))
        await assertEventually { await self.hotkeys() == 1 }
        try await run()
        await assertEventually("the engine matched the chord with its modifiers") {
            self.playing(self.first, self.intro)
        }
    }

    func testKeyWithNoMatchIsNotConsumed() async throws {
        let e = try event("y")
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        let n = await hotkeys()
        XCTAssertEqual(n, 0)
    }

    func testHeldKeyFiresOnce() async throws {
        XCTAssertNil(app.handleKeyDown(try event("x")))
        XCTAssertNil(app.handleKeyDown(try event("x", repeats: true)), "still ours, nothing sent")
        await assertEventually { await self.hotkeys() == 1 }
        await settle()
        let once = await hotkeys()
        XCTAssertEqual(once, 1)
        // A fresh press with no key-up seen (lost while a modal ran) still fires.
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { await self.hotkeys() == 2 }
    }

    func testKeyUpSendsTheUpOnce() async throws {
        _ = app.handleKeyDown(try event("x"))
        await assertEventually { await self.hotkeys() == 1 }
        try await run()
        await assertEventually { self.playing(self.other, self.second) }
        app.handleKeyUp(try event("x", type: .keyUp))
        await assertEventually { await self.hotkeys() == 2 }
        try await run()
        await assertEventually("the release stops Second (its second trigger)") {
            !self.playing(self.other, self.second)
        }
        // A second key-up has nothing left to release.
        app.handleKeyUp(try event("x", type: .keyUp))
        // A key-up of a key that fired nothing sends nothing.
        app.handleKeyUp(try event("z", type: .keyUp, code: 9))
        await settle()
        let n = await hotkeys()
        XCTAssertEqual(n, 2)
    }

    func testAnUndeliverableHotkeyBeepsAndIsLogged() async throws {
        var beeps = 0
        app.beep = { beeps += 1 }
        engine.kill()  // nothing connected: the hotkey cannot be delivered
        await assertEventually { self.app.session.connectionState != .connected }
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { beeps == 1 }
        XCTAssertTrue(first.logEntries.contains { $0.level == .error })
    }

    func testTypingInATextFieldKeepsTheKey() async throws {
        app.isTextEditing = { true }
        let e = try event("x")
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        let n = await hotkeys()
        XCTAssertEqual(n, 0)
    }

    func testRecorderHasTheKey() async throws {
        app.isRecordingKey = { true }
        let e = try event("x")
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        let n = await hotkeys()
        XCTAssertEqual(n, 0)
    }

    func testAMenuKeyIsNeverTakenByACue() async throws {
        _ = await first.send(
            command: .setProperty(intro, .triggers(hotkey: KeyChord("q", [.command]).hotkeyJSON, osc: nil)))
        await assertEventually {
            self.first.model?.cueHotkeys.contains { $0.chord == KeyChord("q", [.command]) } == true
        }
        let e = try event("q", [.command])
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        let n = await hotkeys()
        XCTAssertEqual(n, 0)
    }

    /// A cue hotkey a menu command shadows never fires, so the show log says so (once).
    func testACueHotkeyOnAMenuKeyIsWarnedAboutInTheLog() async throws {
        let chord = KeyChord("q", [.command])
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: chord.hotkeyJSON, osc: nil)))
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == chord } == true }
        let message = String(format: loc("hotkeys.warn.menuShadowed"), chord.display)
        await assertEventually { self.first.logEntries.contains { $0.level == .warn && $0.message == message } }
        _ = app.frontKeys
        _ = app.keys(for: first)
        XCTAssertEqual(first.logEntries.filter { $0.message == message }.count, 1)
    }

    func testFixedDefaultKeysYieldToACueHotkey() async throws {
        keyDoc = demoDoc
        for (n, key) in ["v", "l"].enumerated() {
            _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord(key).hotkeyJSON, osc: nil)))
            await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == KeyChord(key) } == true }
            XCTAssertNil(app.handleKeyDown(try event(key, code: key == "v" ? 9 : 37)), "\(key) fires the cue")
            await assertEventually { await self.hotkeys() == n + 1 }
            XCTAssertNil(app.frontKeys.chord(for: key == "v" ? .previewSelected : .loadSelected))
            XCTAssertTrue(
                first.logEntries.contains { $0.level == .warn && $0.message.contains("the cue keeps it") })
        }
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }
        XCTAssertEqual(first.logEntries.filter { $0.message.contains("the cue keeps it") }.count, 2, "once each")
        _ = app.frontKeys
        XCTAssertEqual(first.logEntries.filter { $0.message.contains("the cue keeps it") }.count, 2)
    }

    func testTransportKeyWinsOverACueHotkeyOnTheSameChord() async throws {
        keyDoc = demoDoc
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord(" ").hotkeyJSON, osc: nil)))
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == KeyChord(" ") } == true }
        XCTAssertEqual(app.frontKeys.chord(for: .go), KeyChord(" "), "GO keeps its key")
        await assertEventually {
            self.first.logEntries.contains { $0.level == .warn && $0.message.contains("transport key") }
        }
        XCTAssertEqual(first.logEntries.filter { $0.level == .warn }.count, 1)
        _ = app.frontKeys
        XCTAssertEqual(first.logEntries.filter { $0.level == .warn }.count, 1, "once")
        XCTAssertNil(app.handleKeyDown(try event(" ", code: 49)))
        await assertEventually { await self.triggers("transport go") == 1 }
        let fired = await hotkeys()
        XCTAssertEqual(fired, 0, "the cue does not fire")
        // With no front controller the transport chord still must not start the cue.
        keyDoc = nil
        let e = try event(" ", code: 49)
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        let after = await hotkeys()
        XCTAssertEqual(after, 0)
    }

    func testResignActiveForgetsHeldKeysSoAReleaseIsSentOnce() async throws {
        _ = app.handleKeyDown(try event("x"))
        await assertEventually { await self.hotkeys() == 1 }
        NotificationCenter.default.post(name: NSApplication.didResignActiveNotification, object: nil)
        _ = app.handleKeyDown(try event("x"))
        await assertEventually { await self.hotkeys() == 2 }
        app.handleKeyUp(try event("x", type: .keyUp))
        await assertEventually { await self.hotkeys() == 3 }
        await settle()
        let n = await hotkeys()
        XCTAssertEqual(n, 3, "one release")
    }

    func testCueHotkeysStayOutOfModalsAndNonWorkspaceWindows() async throws {
        app.isModalUp = { true }
        let e = try event("x")
        XCTAssertTrue(app.handleKeyDown(e) === e, "alert or panel running")
        app.isModalUp = { false }
        let panel = NSPanel(contentRect: .zero, styleMask: [.titled], backing: .buffered, defer: false)
        panel.isReleasedWhenClosed = false
        app.keyWindow = { panel }  // a file panel
        XCTAssertTrue(app.handleKeyDown(e) === e)
        app.keyWindow = { [unowned self] in app.launcher.window }
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        let none = await hotkeys()
        XCTAssertEqual(none, 0)
        // The Log window is a monitoring window: hotkeys fire with it key.
        let log = NSWindow(contentRect: .zero, styleMask: [.titled], backing: .buffered, defer: false)
        log.isReleasedWhenClosed = false
        app.keyWindow = { log }
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { await self.hotkeys() == 1 }
        app.handleKeyUp(try event("x", type: .keyUp))
        app.keyWindow = { nil }  // app active, no key window: still fires
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { await self.hotkeys() == 3 }
    }

    func testEscStillPanicsWhateverTheCueHotkeys() async throws {
        _ = await first.send(
            command: .setProperty(intro, .triggers(hotkey: KeyChord(KeyChord.escape).hotkeyJSON, osc: nil)))
        await assertEventually { self.first.model?.cue(self.intro)?.raw["triggers"]?["hotkey"] != .null }
        _ = await first.transport(.start(cue: intro))
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }
        let esc = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
                characters: "\u{1B}", charactersIgnoringModifiers: "\u{1B}", isARepeat: false, keyCode: 53))
        XCTAssertTrue(app.handleKeyDown(esc) === esc)
        await assertEventually { await self.triggers("panic") == 1 }
        try await engine.advance(seconds: 1)  // past the show's 0.5 s panic fade
        await assertEventually("Esc panicked Intro") { !self.playing(self.first, self.intro) }
        let n = await hotkeys()
        XCTAssertEqual(n, 0)
    }
}
