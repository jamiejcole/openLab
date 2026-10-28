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
        XCTAssertEqual(validate(KeyChord(" ")), .duplicate(of: .go))
        XCTAssertEqual(validate(KeyChord("[")), .duplicate(of: .pauseAll))
        XCTAssertNil(validate(KeyChord("p")), "a fixed app default (L, P, V) yields to a cue hotkey")
        XCTAssertEqual(validate(KeyChord("\u{7F}")), .unsupported)
        XCTAssertEqual(validate(KeyChord("q", [.command]), menu: [KeyChord("q", [.command])]), .menuKey)
    }

    func testAllowedKeys() {
        XCTAssertNil(validate(KeyChord("a")))
        XCTAssertNil(validate(KeyChord(KeyChord.left)), "QLab allows left and right")
        XCTAssertNil(validate(KeyChord(KeyChord.down, [.command])))
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

/// The inspector's Triggers tab against the fake engine.
@MainActor
final class TriggersTabTests: XCTestCase {
    private let intro = CueListHarness.intro
    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!

    override func setUp() async throws {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
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

    private var properties: [JSONValue] {
        rig.engine.received.compactMap {
            if case .command(_, let c, _) = $0 { return JSONValue.parse((try? c.json.text()) ?? "")?["property"] }
            return nil
        }
    }

    // MARK: Show Mode (SM-01)

    func testLockedInspectorBlocksEveryEditPathAndUnlockRestores() async {
        let before = rig.engine.received.count
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
        XCTAssertEqual(rig.engine.received.count, before, "nothing reached the engine")

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
            [loc("inspector.tab.basics"), loc("inspector.tab.triggers"), loc("inspector.tab.levels")])
    }

    func testRecordingAHotkeySendsTriggersAndShowsItAfterTheEngineAnswers() async {
        XCTAssertTrue(vc.record(KeyChord("k", [.command, .shift])))
        await assertEventually { vc.hotkeyField.stringValue == "⇧⌘K" }
        XCTAssertEqual(properties.last?["type"]?.stringValue, "triggers")
        XCTAssertEqual(properties.last?["value"]?["hotkey"]?["key"]?.stringValue, "k")
        XCTAssertTrue(vc.hotkeyClear.isEnabled)
        XCTAssertTrue(vc.onReleaseCheck.isEnabled)
    }

    func testRefusedKeysSendNothingAndShowALocalisedMessage() async {
        let cases: [(KeyChord, String)] = [
            (KeyChord(KeyChord.escape), "inspector.hotkey.reserved"),
            (KeyChord(KeyChord.up), "inspector.hotkey.reserved"),
            (KeyChord(" "), "inspector.hotkey.transport"),
            (KeyChord("s", [.command]), "inspector.hotkey.menu"),
            (KeyChord("\u{7F}"), "inspector.hotkey.unsupported"),
        ]
        for (chord, key) in cases {
            XCTAssertFalse(vc.record(chord))
            XCTAssertEqual(vc.messageLabel.stringValue, loc(key))
            XCTAssertNotEqual(loc(key), key, "string exists")
        }
        await settle()
        XCTAssertTrue(properties.isEmpty)
    }

    func testAnotherCuesHotkeyIsRefused() async {
        XCTAssertTrue(vc.record(KeyChord("a")))
        await assertEventually { vc.hotkeyField.stringValue == "A" }
        vc.setSelection([CueListHarness.fade])
        let sent = properties.count
        XCTAssertFalse(vc.record(KeyChord("a")))
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.hotkey.otherCue"))
        XCTAssertEqual(properties.count, sent)
    }

    func testClearKeepsTheOSCAddressAndOSCEditKeepsTheHotkey() async {
        vc.oscField.stringValue = "/cue/one"
        vc.commit(.osc)
        await assertEventually { vc.oscField.stringValue == "/cue/one" && properties.count == 1 }
        XCTAssertEqual(properties.last?["value"]?["osc"]?.stringValue, "/cue/one")
        XCTAssertTrue(vc.record(KeyChord("a")))
        await assertEventually { properties.count == 2 }
        XCTAssertEqual(properties.last?["value"]?["osc"]?.stringValue, "/cue/one", "OSC kept")
        await assertEventually { vc.hotkeyClear.isEnabled }
        vc.clearHotkey()
        await assertEventually { properties.count == 3 && !vc.hotkeyClear.isEnabled }
        XCTAssertEqual(properties.last?["value"]?["hotkey"], .null)
        XCTAssertEqual(properties.last?["value"]?["osc"]?.stringValue, "/cue/one")
        vc.oscField.stringValue = ""
        vc.commit(.osc)
        await assertEventually { properties.count == 4 }
        XCTAssertEqual(properties.last?["value"]?["osc"], .null)
    }

    func testRefusedTriggersAreNotSentAgainWithTheNextEdit() async {
        rig.engine.refuseCommands = true
        vc.oscField.stringValue = "/refused"
        vc.commit(.osc)
        await assertEventually { properties.count == 1 && vc.oscField.stringValue == "" }
        rig.engine.refuseCommands = false
        XCTAssertTrue(vc.record(KeyChord("a")))
        await assertEventually { properties.count == 2 }
        XCTAssertEqual(properties.last?["value"]?["osc"], .null, "the refused address is gone")
        XCTAssertEqual(properties.last?["value"]?["hotkey"]?["key"]?.stringValue, "a")
    }

    func testBadOSCAddressRevertsWithoutSending() async {
        vc.oscField.stringValue = "no slash"
        XCTAssertFalse(vc.commit(.osc))
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.osc"))
        XCTAssertEqual(vc.oscField.stringValue, "")
        await settle()
        XCTAssertTrue(properties.isEmpty)
    }

    func testSecondTriggerAndOnReleaseSend() async {
        vc.record(KeyChord("a"))
        await assertEventually { vc.onReleaseCheck.isEnabled }
        vc.secondTriggerPopup.selectItem(at: 2)
        _ = vc.secondTriggerPopup.target?.perform(vc.secondTriggerPopup.action, with: vc.secondTriggerPopup)
        vc.onReleaseCheck.state = .on
        _ = vc.onReleaseCheck.target?.perform(vc.onReleaseCheck.action, with: vc.onReleaseCheck)
        await assertEventually { properties.count == 3 }
        XCTAssertEqual(properties[1]["type"]?.stringValue, "second_trigger")
        XCTAssertEqual(properties[1]["value"]?.stringValue, "stop")
        XCTAssertEqual(properties[2]["type"]?.stringValue, "second_trigger_on_release")
        XCTAssertEqual(properties[2]["value"], .bool(true))
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

/// Cue hotkeys through the app controller with two open workspaces.
@MainActor
final class CueHotkeyFiringTests: XCTestCase {
    private var app: AppController!
    private var engine: FakeEngine!
    private var keyDoc: ModelID?
    private let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
    private let second: ModelID = "00000000-0000-0000-0000-0000000000a2"
    private var secondDoc: ModelID = "none"

    override func setUp() async throws {
        engine = FakeEngine(snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        let info = engine.info
        let session = EngineSession(transport: engine, infoProvider: { .success(info) }, scheduler: ManualScheduler())
        app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { [unowned self] in keyDoc.flatMap { app.windows[$0]?.window } }
        app.menuChords = { [KeyChord("q", [.command])] }
        session.start()
        await assertEventually { app.windows.count == 1 && session.stores.values.allSatisfy { $0.model != nil } }
        guard case .success(let doc) = await session.newWorkspace() else { return XCTFail("new") }
        secondDoc = doc
        await assertEventually { app.session.stores[doc]?.model != nil }
        // A cue in the second workspace with the same hotkey as Intro ("x") and release on.
        let store = try XCTUnwrap(app.session.stores[doc])
        let list = try XCTUnwrap(store.model?.cueLists.first?.id)
        var cue = FakeEngine.cueJSON(
            id: second.raw, number: "9", name: "Second",
            kind: .object(["type": .string("wait"), "duration": .double(1)]))
        cue.set("triggers", .object(["hotkey": KeyChord("x").hotkeyJSON ?? .null, "osc": .null]))
        cue.set("second_trigger_on_release", .bool(true))
        let upsert = JSONValue.object([
            "type": .string("cue"), "list": .string(list.raw), "parent": .null, "index": .int(0), "cue": cue,
        ])
        engine.push(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[\#(try upsert.text())],"deletes":[]}"#,
            doc: doc)
        await assertEventually { store.model?.cue(second) != nil }
        let first = try XCTUnwrap(app.session.stores["1"])
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord("x").hotkeyJSON, osc: nil)))
        await assertEventually { first.model?.cue(intro)?.raw["triggers"]?["hotkey"] != .null }
    }

    override func tearDown() async throws {
        app.closeAllWindows()
        app = nil
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

    private func sent(_ action: TransportAction) -> [ModelID?] {
        engine.receivedFrom.compactMap {
            if case .transport(_, let a) = $0.message, a == action { return $0.doc }
            return nil
        }
    }

    /// The `hotkey` messages the engine received: key, modifiers, up, and the connection (nil:
    /// the app's).
    private func hotkeys() -> [(key: String, mods: HotkeyModifiers, up: Bool, doc: ModelID?)] {
        engine.receivedFrom.compactMap {
            if case .hotkey(_, let key, let mods, let up) = $0.message { return (key, mods, up, $0.doc) }
            return nil
        }
    }

    private var downs: Int { hotkeys().filter { !$0.up }.count }
    private var ups: Int { hotkeys().filter(\.up).count }

    private var anyStart: Bool {
        engine.receivedFrom.contains {
            if case .transport(_, .start) = $0.message { return true }
            return false
        }
    }

    func testKeySendsOneAppScopeHotkeyEvenWithoutAKeyWindow() async throws {
        keyDoc = nil  // no workspace window is key (launcher or alert in front)
        XCTAssertNil(app.handleKeyDown(try event("x")), "consumed")
        await assertEventually { self.downs == 1 }
        await settle()
        let sent = hotkeys()
        XCTAssertEqual(sent.count, 1, "one message for both workspaces, not one per workspace")
        XCTAssertEqual(sent.first?.key, "x")
        XCTAssertEqual(sent.first?.mods, HotkeyModifiers())
        XCTAssertNil(sent.first?.doc ?? nil, "on the app connection")
        XCTAssertFalse(anyStart, "the engine does the matching: no per-workspace start")
    }

    func testModifiersGoOnTheWire() async throws {
        let first = try XCTUnwrap(app.session.stores["1"])
        let chord = KeyChord("k", [.shift, .control])
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: chord.hotkeyJSON, osc: nil)))
        await assertEventually { first.model?.cueHotkeys.contains { $0.chord == chord } == true }
        XCTAssertNil(app.handleKeyDown(try event("k", [.shift, .control], code: 40)))
        await assertEventually { self.downs == 1 }
        XCTAssertEqual(hotkeys().first?.mods, HotkeyModifiers(shift: true, control: true, alt: false, command: false))
    }

    func testKeyWithNoMatchIsNotConsumed() async throws {
        let e = try event("y")
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        XCTAssertTrue(hotkeys().isEmpty)
    }

    func testHeldKeyFiresOnce() async throws {
        XCTAssertNil(app.handleKeyDown(try event("x")))
        XCTAssertNil(app.handleKeyDown(try event("x", repeats: true)), "still ours, nothing sent")
        await settle()
        XCTAssertEqual(downs, 1)
        // A fresh press with no key-up seen (lost while a modal ran) still fires.
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { self.downs == 2 }
    }

    func testKeyUpSendsTheUpOnce() async throws {
        _ = app.handleKeyDown(try event("x"))
        await assertEventually { self.downs == 1 }
        app.handleKeyUp(try event("x", type: .keyUp))
        await assertEventually { self.ups == 1 }
        XCTAssertEqual(hotkeys().last?.key, "x")
        // A second key-up has nothing left to release.
        app.handleKeyUp(try event("x", type: .keyUp))
        await settle()
        XCTAssertEqual(ups, 1)
        // A key-up of a key that fired nothing sends nothing.
        app.handleKeyUp(try event("z", type: .keyUp, code: 9))
        await settle()
        XCTAssertEqual(ups, 1)
    }

    func testAnUndeliverableHotkeyBeepsAndIsLogged() async throws {
        var beeps = 0
        app.beep = { beeps += 1 }
        engine.connectFailure = true
        engine.dropConnections()  // nothing connected: the hotkey cannot be delivered
        await assertEventually { self.app.session.connectionState != .connected }
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { beeps == 1 }
        let first = try XCTUnwrap(app.session.stores["1"])
        XCTAssertTrue(first.logEntries.contains { $0.level == .error })
    }

    func testTypingInATextFieldKeepsTheKey() async throws {
        app.isTextEditing = { true }
        let e = try event("x")
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        XCTAssertTrue(hotkeys().isEmpty)
    }

    func testRecorderHasTheKey() async throws {
        app.isRecordingKey = { true }
        let e = try event("x")
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        XCTAssertTrue(hotkeys().isEmpty)
    }

    func testAMenuKeyIsNeverTakenByACue() async throws {
        let first = try XCTUnwrap(app.session.stores["1"])
        _ = await first.send(
            command: .setProperty(intro, .triggers(hotkey: KeyChord("q", [.command]).hotkeyJSON, osc: nil)))
        await assertEventually { first.model?.cueHotkeys.contains { $0.chord == KeyChord("q", [.command]) } == true }
        let e = try event("q", [.command])
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        XCTAssertTrue(hotkeys().isEmpty)
    }

    func testFixedDefaultKeysYieldToACueHotkey() async throws {
        keyDoc = "1"
        let first = try XCTUnwrap(app.session.stores["1"])
        for key in ["v", "l"] {
            _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord(key).hotkeyJSON, osc: nil)))
            await assertEventually { first.model?.cueHotkeys.contains { $0.chord == KeyChord(key) } == true }
            XCTAssertNil(app.handleKeyDown(try event(key, code: key == "v" ? 9 : 37)), "\(key) fires the cue")
            await assertEventually { self.hotkeys().contains { $0.key == key && !$0.up } }
            XCTAssertNil(app.frontKeys.chord(for: key == "v" ? .previewSelected : .loadSelected))
            XCTAssertTrue(
                first.logEntries.contains { $0.level == .warn && $0.message.contains("yields to a cue hotkey") })
        }
        XCTAssertEqual(first.logEntries.filter { $0.message.contains("yields to a cue hotkey") }.count, 2, "once each")
        _ = app.frontKeys
        XCTAssertEqual(first.logEntries.filter { $0.message.contains("yields to a cue hotkey") }.count, 2)
    }

    func testTransportKeyWinsOverACueHotkeyOnTheSameChord() async throws {
        keyDoc = "1"
        let first = try XCTUnwrap(app.session.stores["1"])
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord(" ").hotkeyJSON, osc: nil)))
        await assertEventually { first.model?.cueHotkeys.contains { $0.chord == KeyChord(" ") } == true }
        XCTAssertEqual(app.frontKeys.chord(for: .go), KeyChord(" "), "GO keeps its key")
        await assertEventually {
            first.logEntries.contains { $0.level == .warn && $0.message.contains("transport key") }
        }
        XCTAssertEqual(first.logEntries.filter { $0.level == .warn }.count, 1)
        _ = app.frontKeys
        XCTAssertEqual(first.logEntries.filter { $0.level == .warn }.count, 1, "once")
        XCTAssertNil(app.handleKeyDown(try event(" ", code: 49)))
        await assertEventually { !sent(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty }
        XCTAssertTrue(hotkeys().isEmpty, "the cue does not fire")
        // With no front controller the transport chord still must not start the cue.
        keyDoc = nil
        let e = try event(" ", code: 49)
        XCTAssertTrue(app.handleKeyDown(e) === e)
        await settle()
        XCTAssertTrue(hotkeys().isEmpty)
    }

    func testResignActiveForgetsHeldKeysSoAReleaseIsSentOnce() async throws {
        _ = app.handleKeyDown(try event("x"))
        await assertEventually { self.downs == 1 }
        NotificationCenter.default.post(name: NSApplication.didResignActiveNotification, object: nil)
        _ = app.handleKeyDown(try event("x"))
        await assertEventually { self.downs == 2 }
        app.handleKeyUp(try event("x", type: .keyUp))
        await assertEventually { self.ups == 1 }
        await settle()
        XCTAssertEqual(ups, 1)
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
        XCTAssertTrue(hotkeys().isEmpty)
        // The Log window is a monitoring window: hotkeys fire with it key.
        let log = NSWindow(contentRect: .zero, styleMask: [.titled], backing: .buffered, defer: false)
        log.isReleasedWhenClosed = false
        app.keyWindow = { log }
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { self.downs == 1 }
        app.handleKeyUp(try event("x", type: .keyUp))
        app.keyWindow = { nil }  // app active, no key window: still fires
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { self.downs == 2 }
    }

    func testEscStillPanicsWhateverTheCueHotkeys() async throws {
        let first = try XCTUnwrap(app.session.stores["1"])
        _ = await first.send(
            command: .setProperty(intro, .triggers(hotkey: KeyChord(KeyChord.escape).hotkeyJSON, osc: nil)))
        await assertEventually { first.model?.cue(intro)?.raw["triggers"]?["hotkey"] != .null }
        let esc = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
                characters: "\u{1B}", charactersIgnoringModifiers: "\u{1B}", isARepeat: false, keyCode: 53))
        XCTAssertTrue(app.handleKeyDown(esc) === esc)
        await assertEventually {
            self.engine.receivedFrom.contains {
                if case .panic = $0.message { return $0.doc == nil }
                return false
            }
        }
        XCTAssertTrue(hotkeys().isEmpty)
    }
}
