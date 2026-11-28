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
                .setProperty(id, .triggers(hotkey: hk, enabled: true)),
                #"{"type":"triggers","value":{"hotkey":{"key":"k","modifiers":\#(mods)},"hotkey_enabled":true}}"#
            ),
            (
                .setProperty(id, .triggers(hotkey: nil, enabled: false)),
                #"{"type":"triggers","value":{"hotkey":null,"hotkey_enabled":false}}"#
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

    /// A hotkey edit keeps the trigger's switch as the cue has it (on when the show has none).
    func testAHotkeyEditKeepsTheTriggerSwitch() throws {
        var raw = try XCTUnwrap(NewCueKind.memo.cueJSON(id: id, patch: nil))
        raw.set("triggers", .object(["hotkey": .null, "hotkey_enabled": .bool(false)]))
        let off = try Cue(json: raw)
        let hk = KeyChord("k").hotkeyJSON
        XCTAssertEqual(
            try shape(InspectorEdit.setTriggers(off, hotkey: .some(hk)))?["property"]?["value"]?["hotkey_enabled"],
            .bool(false))
        raw.set("triggers", .object(["hotkey": .null]))
        let old = try Cue(json: raw)
        XCTAssertEqual(
            try shape(InspectorEdit.setTriggers(old, hotkey: .some(hk)))?["property"]?["value"]?["hotkey_enabled"],
            .bool(true))
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
        XCTAssertEqual(validate(KeyChord("p")), .duplicate(of: .pauseResumeSelected), "the model's key")
        XCTAssertEqual(validate(KeyChord("l")), .duplicate(of: .loadSelected))
        XCTAssertEqual(validate(KeyChord("v")), .duplicate(of: .previewSelected), "Preview Selected is a model key")
        XCTAssertEqual(validate(KeyChord("s")), .duplicate(of: .panicSelected))
        XCTAssertEqual(validate(KeyChord("n")), .editKey, "N and Q edit the highlighted cue's number and name")
        XCTAssertEqual(validate(KeyChord("q")), .editKey)
        XCTAssertEqual(validate(KeyChord("t")), .editKey, "and T edits the target")
        for key in ["e", "d", "w", "c", "f"] {
            XCTAssertEqual(validate(KeyChord(key)), .editKey, "\(key) is a cue-editing key")
        }
        XCTAssertNil(validate(KeyChord("n", .shift)), "only the plain keys")
        XCTAssertEqual(HotkeyRules.messageKey(.editKey), "inspector.hotkey.editKey")
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
        XCTAssertNil(validateAgainstMenuBar("j", []), "a plain J is still a cue's to take")
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
        // The inspector reads where the patches play when it loads; that is not an edit under test.
        await assertEventually("the inspector's first read of where the patches play") { self.vc.patchReading == nil }
    }

    override func tearDown() async throws {
        window?.close()
        (window, vc, rig) = (nil, nil, nil)
    }

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
            vc.numberField, vc.nameField, vc.preWaitField, vc.postWaitField, vc.hotkeyField,
            vc.hotkeyClear, vc.secondTriggerPopup, vc.onReleaseCheck, vc.colourPopup, vc.continuePopup,
            vc.flaggedCheck, vc.armedCheck, vc.secondColourCheck, vc.secondColourPopup, vc.autoLoadCheck,
            vc.skipCheck, vc.hotkeyEnableCheck, vc.fadeOthersCheck, vc.fadeOthersScopePopup,
            vc.fadeOthersTimeField, vc.duckCheck, vc.duckDbField, vc.duckTimeField,
        ]
        XCTAssertTrue(controls.allSatisfy { !$0.isEnabled })
        XCTAssertFalse(vc.notesView.isEditable)
        XCTAssertFalse(vc.hotkeyField.acceptsFirstResponder, "the recorder cannot take focus")

        XCTAssertFalse(vc.record(KeyChord("k", [.command, .shift])))
        vc.clearHotkey()
        vc.nameField.stringValue = "Changed"
        vc.commit(.name)
        vc.colourPopup.selectItem(at: InspectorViewController.colourItems.firstIndex(of: "red") ?? -1)
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
                loc("inspector.tab.basics"), loc("inspector.tab.triggers"), loc("inspector.tab.io"),
                loc("inspector.tab.timeLoops"), loc("inspector.tab.levels"),
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

    /// A cue hotkey on a key a command owns is refused with the transport message, and which keys
    /// the commands own is what the model says: the remapped key, not the old default.
    func testRecorderRefusesTheRemappedSelectedCueKeysNotTheDefaults() async throws {
        try await rig.store.setHotkeys(#"{"load_selected":\#(AppControllerTests.hotkey("k"))}"#)
        XCTAssertFalse(vc.record(KeyChord("k")))
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.hotkey.transport"))
        XCTAssertFalse(vc.record(KeyChord("p")), "Pause/Resume Selected still has P")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.hotkey.transport"))
        XCTAssertEqual(triggers()?["hotkey"], .null, "nothing was stored")
        XCTAssertTrue(vc.record(KeyChord("l")), "L is no longer Load Selected")
        await assertEventually { vc.hotkeyField.stringValue == "L" }
        XCTAssertEqual(triggers()?["hotkey"], KeyChord("l").hotkeyJSON, "the engine stored it")
    }

    /// The first edit is refused by the engine (the cue it names exists only in the store, as when
    /// the store is behind); the cue is then added for real with its hotkey trigger off, and the
    /// next edit must start from the engine's cue: the refused key is not sent again (the next
    /// recorded key switches the trigger on, as any recorded key does).
    func testRefusedTriggersAreNotSentAgainWithTheNextEdit() async throws {
        let id: ModelID = "00000000-0000-0000-0000-0000000000ee"
        var cue = try XCTUnwrap(NewCueKind.memo.cueJSON(id: id, patch: nil))
        cue.set("name", .string("Late"))
        let upsert = try CueListHarness.upsertJSON(cue, index: 0)
        let frame =
            #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#
        XCTAssertEqual(rig.store.receive(try ServerMessage.decode(frame)), .applied)
        vc.setSelection([id])
        XCTAssertTrue(vc.record(KeyChord("b")))
        await assertEventually { self.vc.messageLabel.stringValue.contains("does not exist") }
        // Now the engine has the cue, its hotkey trigger off.
        var real = cue
        real.set("triggers", .object(["hotkey": .null, "hotkey_enabled": .bool(false)]))
        let added = await rig.store.send(
            command: .addCue(real, list: TestSnapshots.mainList, parent: nil, index: 0))
        guard case .success = added else { return XCTFail("\(added)") }
        // The store was given a revision ahead of the engine's above, so the add's own
        // `model_changed` reads as old and is dropped: the engine's copy comes by snapshot.
        rig.client.requestSnapshot()
        await assertEventually { self.triggers(id)?["hotkey_enabled"] == .bool(false) }
        XCTAssertTrue(vc.record(KeyChord("a")))
        rig.client.requestSnapshot()  // the engine's own copy, not the one the store was given
        await assertEventually { self.triggers(id)?["hotkey"] == KeyChord("a").hotkeyJSON }
        XCTAssertEqual(triggers(id)?["hotkey_enabled"], .bool(true), "a recorded key switches the trigger on")
    }

    /// What a second trigger does sits under its question, and whether it comes on the key's release
    /// sits right under that, apart from the hotkey (the owner's layout).
    func testTheReleaseBoxSitsUnderTheSecondTriggerMenuAndItsQuestion() throws {
        let stack = try XCTUnwrap(vc.onReleaseCheck.superview as? NSStackView)
        let row = try XCTUnwrap(stack.arrangedSubviews.first { vc.secondTriggerPopup.isDescendant(of: $0) })
        let views = stack.arrangedSubviews
        let rowIndex = views.firstIndex(of: row)
        XCTAssertEqual(views.firstIndex(of: vc.onReleaseCheck), rowIndex.map { $0 + 1 }, "right under the menu")
        XCTAssertTrue(vc.secondTriggerQuestion.isDescendant(of: row), "the question is on the menu's row")
        XCTAssertEqual(vc.secondTriggerQuestion.stringValue, loc("inspector.secondTrigger"))
        XCTAssertTrue(
            vc.secondTriggerQuestion.stringValue.contains("second time while playing"),
            vc.secondTriggerQuestion.stringValue)
        XCTAssertFalse(vc.hotkeyField.isDescendant(of: stack), "the hotkey is in the other column")
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

    /// Reached with Tab, the recorder lets Tab and Shift-Tab move on (the keyboard is not caught in
    /// it, #784 flow 8); clicked, Tab is recorded like any key.
    func testTabPassesThroughTheRecorderReachedByKeyboardAndIsRecordedAfterAClick() throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 300, height: 100), styleMask: [.titled], backing: .buffered,
            defer: true)
        let recorder = HotkeyRecorder()
        recorder.isEditable = false  // as in the Triggers tab
        recorder.isSelectable = false
        let before = NSTextField()
        let after = NSTextField()
        for (i, v) in [before, recorder, after].enumerated() {
            v.frame = NSRect(x: 10 + i * 90, y: 40, width: 80, height: 22)
            window.contentView?.addSubview(v)
        }
        before.nextKeyView = recorder
        recorder.nextKeyView = after
        after.nextKeyView = before
        var got: [KeyChord] = []
        recorder.onRecord = { if let c = KeyChord(event: $0) { got.append(c) } }
        func tab(_ flags: NSEvent.ModifierFlags) throws -> NSEvent {
            try XCTUnwrap(
                NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0,
                    context: nil, characters: "\t", charactersIgnoringModifiers: "\t", isARepeat: false,
                    keyCode: HotkeyRecorder.tabKeyCode))
        }
        XCTAssertTrue(window.makeFirstResponder(recorder), "reached by keyboard")
        recorder.keyDown(with: try tab([]))
        XCTAssertEqual(got, [], "Tab moved on, nothing recorded")
        func focused(_ field: NSTextField) -> Bool {
            window.firstResponder === field || (window.firstResponder as? NSText)?.delegate === field
        }
        XCTAssertTrue(focused(after), "the focus went on to the next field")
        XCTAssertTrue(window.makeFirstResponder(recorder))
        recorder.keyDown(with: try tab([.shift]))
        XCTAssertEqual(got, [])
        XCTAssertTrue(focused(before), "Shift-Tab went back to the field before")
        let click = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0,
                context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        recorder.mouseDown(with: click)
        recorder.keyDown(with: try tab([]))
        XCTAssertEqual(got.count, 1, "clicked, Tab is recorded")
    }

    // MARK: Layout

    /// The Triggers tab at `width`, laid out.
    private func triggersLaidOut(width: CGFloat, height: CGFloat = 300) -> NSView {
        window.setContentSize(NSSize(width: width, height: height))
        vc.tabView.selectTabViewItem(withIdentifier: "triggers")
        vc.view.frame = NSRect(x: 0, y: 0, width: width, height: height)
        vc.view.layoutSubtreeIfNeeded()
        vc.view.layoutSubtreeIfNeeded()
        return vc.tabView.selectedTabViewItem?.view ?? vc.view
    }

    private func frame(_ v: NSView, in root: NSView) -> NSRect { root.convert(v.bounds, from: v) }

    private func sameLine(_ a: NSView, _ b: NSView, in root: NSView) -> Bool {
        abs(frame(a, in: root).midY - frame(b, in: root).midY) < 12
    }

    /// The Triggers tab as QLab 5 lays it out: the label at the left of its row, the box and field on
    /// the line after it, fade-and-stop with its scope and time on one line, the second-trigger
    /// question beside its menu with the on-release box right under it, a divider between columns.
    func testWideTriggersTabHasLabelsAtTheLeftAndOneLinePerTrigger() throws {
        let root = triggersLaidOut(width: 1200)
        let columns = try XCTUnwrap(root as? TriggersColumns)
        XCTAssertFalse(columns.compact)
        XCTAssertTrue(sameLine(vc.hotkeyEnableCheck, vc.hotkeyField, in: root))
        XCTAssertTrue(sameLine(vc.wallClockCheck, vc.wallClockField, in: root))
        XCTAssertTrue(sameLine(vc.wallClockField, vc.wallClockDaysPopup, in: root))
        XCTAssertEqual(frame(vc.hotkeyEnableCheck, in: root).minX, frame(vc.wallClockCheck, in: root).minX, accuracy: 1)
        XCTAssertGreaterThan(frame(vc.hotkeyEnableCheck, in: root).minX, 40, "a label is left of the box")
        XCTAssertTrue(sameLine(vc.fadeOthersCheck, vc.fadeOthersScopePopup, in: root))
        XCTAssertTrue(sameLine(vc.fadeOthersCheck, vc.fadeOthersTimeField, in: root))
        XCTAssertTrue(sameLine(vc.duckDbField, vc.duckTimeField, in: root))
        XCTAssertTrue(sameLine(vc.secondTriggerPopup, vc.secondTriggerQuestion, in: root))
        let menu = frame(vc.secondTriggerPopup, in: root)
        let release = frame(vc.onReleaseCheck, in: root)
        XCTAssertLessThanOrEqual(release.maxY, menu.minY + 1, "the on-release box is under the menu")
        XCTAssertLessThan(menu.minY - release.maxY, 12, "right under it")
        let divider = try XCTUnwrap(columns.arrangedSubviews.first { $0 is SeparatorLine })
        XCTAssertLessThan(frame(vc.wallClockDaysPopup, in: root).maxX, frame(divider, in: root).minX)
        XCTAssertGreaterThan(frame(vc.fadeOthersCheck, in: root).minX, frame(divider, in: root).maxX)
    }

    /// At the least window's width the lines wrap, and every control is drawn inside the tab, none over
    /// another.
    func testNarrowTriggersTabWrapsAndKeepsEveryControlInsideWithoutOverlap() throws {
        let root = triggersLaidOut(width: 780, height: 240)
        let columns = try XCTUnwrap(root as? TriggersColumns)
        XCTAssertTrue(columns.compact, "tab \(vc.tabView.frame.width) wide \(columns.fittingSize.width)")
        XCTAssertFalse(sameLine(vc.fadeOthersCheck, vc.fadeOthersTimeField, in: root), "the time wraps under")
        XCTAssertFalse(sameLine(vc.wallClockField, vc.wallClockDaysPopup, in: root), "the days wrap under")
        XCTAssertFalse(sameLine(vc.secondTriggerPopup, vc.secondTriggerQuestion, in: root))
        let all: [NSView] = [
            vc.hotkeyEnableCheck, vc.hotkeyField, vc.hotkeyClear, vc.wallClockCheck, vc.wallClockField,
            vc.wallClockFormatPopup, vc.wallClockDaysPopup, vc.fadeOthersCheck, vc.fadeOthersScopePopup,
            vc.fadeOthersTimeField, vc.duckCheck, vc.duckDbField, vc.duckTimeField, vc.secondTriggerQuestion,
            vc.secondTriggerPopup, vc.onReleaseCheck,
        ]
        let frames = all.map { frame($0, in: root) }
        for (v, f) in zip(all, frames) {
            XCTAssertGreaterThanOrEqual(f.minX, 0, "\(v) inside on the left")
            XCTAssertLessThanOrEqual(f.maxX, root.bounds.width, "\(v) inside on the right")
        }
        for i in frames.indices {
            for j in frames.indices where j > i {
                let both = frames[i].intersection(frames[j])
                XCTAssertTrue(both.isNull || both.isEmpty, "\(all[i]) overlaps \(all[j])")
            }
        }
        XCTAssertFalse(try XCTUnwrap(triggersLaidOut(width: 1200) as? TriggersColumns).compact, "wide again")
    }

    /// #784: the keyboard goes from the fade-out time field on through the duck fields and round to the
    /// time-of-day field.
    func testTabReachesTheTimeOfDayFieldFromTheFadeOutTimeField() throws {
        _ = triggersLaidOut(width: 1200)
        for c in [vc.fadeOthersTimeField, vc.duckDbField, vc.duckTimeField] { c.isEnabled = true }
        XCTAssertTrue(window.makeFirstResponder(vc.fadeOthersTimeField))
        let fields: [NSView] = [vc.wallClockField, vc.fadeOthersTimeField, vc.duckDbField, vc.duckTimeField]
        var reached: [NSView] = []
        var current: NSView = vc.fadeOthersTimeField
        for _ in 0..<20 {
            guard let next = current.nextValidKeyView else { break }
            if fields.contains(next) { reached.append(next) }
            if reached.count == 3 { break }
            current = next
        }
        XCTAssertEqual(
            reached.map { ObjectIdentifier($0) },
            [vc.duckDbField, vc.duckTimeField, vc.wallClockField].map { ObjectIdentifier($0) },
            "after the fade time: the duck level, the duck time, then round to the time of day")
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
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord("x").hotkeyJSON, enabled: true)))
        await assertEventually { self.first.model?.cue(self.intro)?.raw["triggers"]?["hotkey"] != .null }
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        engine?.stop()
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
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: chord.hotkeyJSON, enabled: true)))
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
            command: .setProperty(intro, .triggers(hotkey: KeyChord("q", [.command]).hotkeyJSON, enabled: true)))
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
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: chord.hotkeyJSON, enabled: true)))
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == chord } == true }
        let message = String(format: loc("hotkeys.warn.menuShadowed"), chord.display)
        await assertEventually { self.first.logEntries.contains { $0.level == .warn && $0.message == message } }
        _ = app.frontKeys
        _ = app.keys(for: first)
        XCTAssertEqual(first.logEntries.filter { $0.message == message }.count, 1)
    }

    private let doorSlam: ModelID = "00000000-0000-0000-0000-000000000021"
    private let rain: ModelID = "00000000-0000-0000-0000-000000000022"

    private func chordJSON(_ key: String) throws -> String { try (KeyChord(key).hotkeyJSON ?? .null).text() }

    /// Preview Selected remapped (issue flow): with the key moved to Y, Y previews the selected cue and V
    /// is free, so a cue whose hotkey is V starts from it, in the app and in the engine.
    func testPreviewSelectedFollowsItsRemappedKeyAndFreesV() async throws {
        keyDoc = demoDoc
        try await first.setHotkeys(#"{"preview_selected":\#(try chordJSON("y"))}"#)
        let wc = try XCTUnwrap(app.windows[demoDoc])
        wc.inspector.setSelection([doorSlam])
        XCTAssertTrue(wc.inspector.record(KeyChord("v")), "the freed chord is accepted as a cue hotkey")
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == KeyChord("v") } == true }
        wc.cueList.select([intro])
        XCTAssertEqual(app.frontKeys.chord(for: .previewSelected), KeyChord("y"))
        XCTAssertNil(app.frontKeys.command(for: KeyChord("v")), "V is free")
        XCTAssertNil(app.handleKeyDown(try event("y", code: 16)), "the key is taken")
        try await run()
        await assertEventually("the selected cue previews") { self.first.previewing.contains(self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("y", code: 16)))
        try await run()
        // A second press leaves it playing (QLab check V3); the preview is stopped by hand.
        XCTAssertTrue(first.previewing.contains(intro), "a second press does not stop it")
        _ = await first.transport(.stopPreview(cue: intro))
        try await run()
        await assertEventually("stopped") { !self.first.previewing.contains(self.intro) }
        // V now starts the cue that has it, and previews nothing.
        XCTAssertNil(app.handleKeyDown(try event("v", code: 9)))
        try await run()
        await assertEventually("the cue hotkey starts its cue") { self.playing(self.first, self.doorSlam) }
        XCTAssertFalse(first.previewing.contains(intro))
        let warned = first.logEntries.contains { $0.level == .warn && $0.message.contains("key map") }
        XCTAssertFalse(warned, "no shadowing warning for a free key")
    }

    /// Panic Selected remapped (issue flow): K panics the selected running cue (it fades out), and S is free
    /// for a cue hotkey.
    func testPanicSelectedFollowsItsRemappedKeyAndFreesS() async throws {
        keyDoc = demoDoc
        try await first.setHotkeys(#"{"panic_selected":\#(try chordJSON("k"))}"#)
        let wc = try XCTUnwrap(app.windows[demoDoc])
        wc.inspector.setSelection([rain])
        XCTAssertTrue(wc.inspector.record(KeyChord("s")), "the freed chord is accepted as a cue hotkey")
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == KeyChord("s") } == true }
        _ = await first.transport(.start(cue: intro))
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }
        wc.cueList.select([intro])
        XCTAssertEqual(app.frontKeys.chord(for: .panicSelected), KeyChord("k"))
        XCTAssertNil(app.handleKeyDown(try event("k", code: 40)), "the key is taken")
        try await run()
        await assertEventually("the selected cue fades out") { self.first.cueStates[self.intro]?.state == .stopping }
        try await engine.advance(seconds: 1)  // past the panic fade
        await assertEventually("and is stopped") { self.first.cueStates[self.intro]?.state.isActive != true }
        XCTAssertFalse(playing(first, rain))
        XCTAssertNil(app.handleKeyDown(try event("s", code: 1)))
        try await run()
        await assertEventually("S starts the cue that has it") { self.playing(self.first, self.rain) }
        XCTAssertNotEqual(first.cueStates[rain]?.state, .stopping, "and does not panic it")
    }

    /// A remapped selected-cue key typed in a text field is text, not a command.
    func testARemappedPreviewKeyTypedInATextFieldPreviewsNothing() async throws {
        keyDoc = demoDoc
        try await first.setHotkeys(#"{"preview_selected":\#(try chordJSON("y"))}"#)
        let wc = try XCTUnwrap(app.windows[demoDoc])
        wc.cueList.select([intro])
        app.isTextEditing = { true }
        let e = try event("y", code: 16)
        XCTAssertTrue(app.handleKeyDown(e) === e, "left to the field")
        try await run()
        XCTAssertFalse(first.previewing.contains(intro))
    }

    /// The keys of the default map are model keys: the recorder refuses V and S, and a cue a loaded show
    /// gave one of them is the shadowed one. The press previews or panics the selected cue, the other
    /// cue stays idle, and the show log says so once each.
    func testACueHotkeyOnTheDefaultPreviewAndPanicKeysIsShadowedAndWarned() async throws {
        keyDoc = demoDoc
        let wc = try XCTUnwrap(app.windows[demoDoc])
        wc.inspector.setSelection([doorSlam])
        XCTAssertFalse(wc.inspector.record(KeyChord("v")), "the recorder refuses a workspace key")
        _ = await first.transport(.start(cue: intro))
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }
        for (n, key) in ["v", "s"].enumerated() {
            let shadowed = n == 0 ? doorSlam : rain
            _ = await first.send(
                command: .setProperty(shadowed, .triggers(hotkey: KeyChord(key).hotkeyJSON, enabled: true)))
            await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == KeyChord(key) } == true }
            await assertEventually {
                _ = self.app.frontKeys
                return self.first.logEntries.filter { $0.message.contains("key map") }.count == n + 1
            }
            XCTAssertEqual(first.logEntries.filter { $0.message.contains("key map") }.count, n + 1, "once each")
        }
        wc.cueList.select([intro])
        XCTAssertNil(app.handleKeyDown(try event("v", code: 9)))
        try await run()
        await assertEventually("V previews the selected cue") { self.first.previewing.contains(self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("s", code: 1)))
        try await run()
        await assertEventually("S panics the selected cue") { self.first.cueStates[self.intro]?.state == .stopping }
        for cue in [doorSlam, rain] {
            XCTAssertFalse(first.cueStates[cue]?.state.isActive ?? false, "the shadowed cue stays idle")
        }
    }

    /// The Transport menu holds the key map of the workspace in front when it was last refreshed. A
    /// cue hotkey in another workspace on that chord still fires when that other workspace is in front,
    /// even though the stale menu item would be greyed there (nothing plays): the greyed-key swallow
    /// leaves Transport items alone.
    func testACueHotkeyOnAnotherWorkspacesTransportKeyFiresDespiteTheStaleMenu() async throws {
        let saved = NSApp.mainMenu
        NSApp.mainMenu = MainMenu.make().bar
        defer { NSApp.mainMenu = saved }
        let delegate = AppDelegate()
        delegate.appController = app
        app.isDisabledMenuChord = { delegate.menuItemIsOff(for: $0) }
        // The delegate and the app hold each other through this closure: let go of it.
        defer { app.isDisabledMenuChord = nil }
        let cmdK = KeyChord("k", [.command])
        _ = await first.send(command: .setWorkspaceKey(.pauseAll, cmdK.hotkeyJSON))
        _ = await other.send(command: .setProperty(second, .triggers(hotkey: cmdK.hotkeyJSON, enabled: true)))
        await assertEventually { self.other.model?.cueHotkeys.contains { $0.chord == cmdK } == true }
        keyDoc = demoDoc
        let transport = try XCTUnwrap(
            NSApp.mainMenu?.items.first { $0.submenu?.title == loc("menu.transport") }?.submenu)
        await assertEventually("the Demo's Pause All key is on the menu") {
            delegate.menuNeedsUpdate(transport)
            return transport.items.contains { $0.keyEquivalent == "k" }
        }
        XCTAssertTrue(delegate.menuItemIsOff(for: cmdK) == false, "a Transport item is never matched")
        keyDoc = secondDoc
        XCTAssertNil(app.handleKeyDown(try event("k", .command, code: 40)))
        try await run()
        await assertEventually("the second workspace's cue fired") { self.playing(self.other, self.second) }
    }

    /// Old files keep V and S: the demo show is a version 7 file, and in it plain V previews the selected
    /// cue (a second press leaves it playing) and plain S panics it, with no setup.
    func testOldFilesKeepVAndS() async throws {
        keyDoc = demoDoc
        let wc = try XCTUnwrap(app.windows[demoDoc])
        wc.cueList.select([intro])
        XCTAssertNil(app.handleKeyDown(try event("v", code: 9)))
        try await run()
        await assertEventually("V previews") { self.first.previewing.contains(self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("v", code: 9)))
        try await run()
        XCTAssertTrue(first.previewing.contains(intro), "V again leaves the preview playing")
        _ = await first.transport(.stopPreview(cue: intro))
        try await run()
        await assertEventually("stopped") { self.first.previewing.isEmpty }
        _ = await first.transport(.start(cue: intro))
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("s", code: 1)))
        try await run()
        await assertEventually("S panics it") { self.first.cueStates[self.intro]?.state == .stopping }
    }

    func testTransportKeyWinsOverACueHotkeyOnTheSameChord() async throws {
        keyDoc = demoDoc
        _ = await first.send(command: .setProperty(intro, .triggers(hotkey: KeyChord(" ").hotkeyJSON, enabled: true)))
        await assertEventually { self.first.model?.cueHotkeys.contains { $0.chord == KeyChord(" ") } == true }
        XCTAssertEqual(app.frontKeys.chord(for: .go), KeyChord(" "), "GO keeps its key")
        await assertEventually {
            self.first.logEntries.contains { $0.level == .warn && $0.message.contains("key map") }
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
        // The workspace status window is a panel like the settings window: with it key, the
        // workspace's keys do nothing (the key goes on to the window), and nothing belongs to
        // a front workspace.
        let status = try XCTUnwrap(app.windows[demoDoc]).statusWindow
        let statusWindow = try XCTUnwrap(status.window)
        app.keyWindow = { statusWindow }
        XCTAssertNil(app.frontController, "the status window is not its workspace's window")
        let blocked = try event("x")
        XCTAssertTrue(app.handleKeyDown(blocked) === blocked, "no cue hotkey with the status window key")
        let wc = try XCTUnwrap(app.windows[demoDoc])
        _ = wc.window?.makeFirstResponder(wc.cueList.outlineView)
        let q = try event("q")
        XCTAssertTrue(app.handleKeyDown(q) === q, "not taken as an edit key")
        XCTAssertEqual(wc.cueList.outlineView.editedRow, -1, "no cell editor opened behind the status window")
        XCTAssertFalse(wc.window?.firstResponder is NSText, "the list keeps the focus")
        await settle()
        let still = await hotkeys()
        XCTAssertEqual(still, 0)
        // With the workspace window key and the status window only open, its keys work.
        let workspaceWindow = try XCTUnwrap(wc.window)
        app.keyWindow = { workspaceWindow }
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { await self.hotkeys() == 1 }
        app.handleKeyUp(try event("x", type: .keyUp))
        app.keyWindow = { nil }  // app active, no key window: still fires
        XCTAssertNil(app.handleKeyDown(try event("x")))
        await assertEventually { await self.hotkeys() == 3 }
    }

    /// With the workspace status window the key window, Esc panics the running cue and leaves the
    /// window open, and Space fires no GO; with the workspace window key, Space does.
    func testWithTheStatusWindowKeyEscPanicsTheRunningCueAndSpaceFiresNoGo() async throws {
        let wc = try XCTUnwrap(app.windows[demoDoc])
        keyDoc = demoDoc
        XCTAssertNil(app.handleKeyDown(try event(" ", code: 49)), "GO, the workspace window key")
        await assertEventually { await self.triggers("transport go") == 1 }
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }

        await wc.showWorkspaceStatus().value
        let panel = try XCTUnwrap(wc.existingStatusWindow?.window)
        app.keyWindow = { panel }
        let space = try event(" ", code: 49)
        XCTAssertTrue(app.handleKeyDown(space) === space, "left to the status window")

        let esc = try event("\u{1B}", code: 53)
        XCTAssertTrue(app.handleKeyDown(esc) === esc)
        panel.sendEvent(esc)  // as the app does with the key the monitor gave back
        await assertEventually { await self.triggers("panic") == 1 }
        try await engine.advance(seconds: 1)  // past the show's 0.5 s panic fade
        await assertEventually("Esc panicked Intro") { !self.playing(self.first, self.intro) }
        XCTAssertTrue(panel.isVisible, "Esc left the status window open")
        let goes = await triggers("transport go")
        XCTAssertEqual(goes, 1, "Space with the status window key fired no GO")
    }

    /// With the workspace status window the key window, the workspace's transport keys (pause, pause
    /// all, preview, panic the selected cue) and Save act on nothing: the keys go on to the window, the
    /// running cue plays on, nothing is previewed, and the Save menu item is off and saves nothing.
    /// With the workspace window key again, each of them acts. (GO and Esc: the test above.)
    func testWithTheStatusWindowKeyTransportKeysAndSaveDoNothingAndAllWorkAgainWithTheWorkspaceKey() async throws {
        let wc = try XCTUnwrap(app.windows[demoDoc])
        let delegate = AppDelegate()
        delegate.appController = app
        let save = try XCTUnwrap(
            MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items).first {
                $0.action == #selector(AppDelegate.saveWorkspace(_:))
            })
        keyDoc = demoDoc
        let cues = try XCTUnwrap(first.model?.cueLists.first?.cues.map(\.id))
        let other = try XCTUnwrap(cues.first { $0 != intro })
        // Something to save, and Intro playing and selected.
        guard case .success = await first.send(command: .setProperty(other, .name("Renamed"))) else {
            return XCTFail("rename")
        }
        await assertEventually("the workspace is unsaved") { self.first.document?.dirty == true }
        wc.cueList.selectAsUser([intro])
        _ = await first.transport(.start(cue: intro))
        try await run()
        await assertEventually { self.playing(self.first, self.intro) }

        await wc.showWorkspaceStatus().value
        let panel = try XCTUnwrap(wc.existingStatusWindow?.window)
        addTeardownBlock { @MainActor in panel.close() }
        app.keyWindow = { panel }
        // Each key is given back to the window (not taken as a command), as the settings window's are.
        for (chars, code) in [("p", 35), ("[", 33), ("s", 1), ("v", 9)] as [(String, UInt16)] {
            let e = try event(chars, code: code)
            XCTAssertTrue(app.handleKeyDown(e) === e, "\(chars) is left to the status window")
        }
        XCTAssertFalse(app.canSaveFront, "no front workspace")
        XCTAssertFalse(delegate.validateMenuItem(save), "Save is off with the status window key")
        delegate.saveWorkspace(nil)
        try await engine.advance(seconds: 1)
        await settle()
        XCTAssertEqual(first.cueStates[intro]?.state, .running, "neither paused nor panicked")
        XCTAssertTrue(first.previewing.isEmpty, "nothing previewed")
        XCTAssertEqual(first.document?.dirty, true, "Save saved nothing")

        // The workspace window key again: all of them work.
        let workspaceWindow = try XCTUnwrap(wc.window)
        app.keyWindow = { workspaceWindow }
        XCTAssertTrue(app.canSaveFront)
        XCTAssertTrue(delegate.validateMenuItem(save))
        XCTAssertNil(app.handleKeyDown(try event("p", code: 35)), "Pause/Resume Selected")
        await assertEventually("paused") { self.first.cueStates[self.intro]?.state == .paused }
        XCTAssertNil(app.handleKeyDown(try event("p", code: 35)))
        await assertEventually("resumed") { self.playing(self.first, self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("[", code: 33)), "Pause All")
        await assertEventually("all paused") { self.first.cueStates[self.intro]?.state == .paused }
        XCTAssertNil(app.handleKeyDown(try event("]", code: 30)), "Resume All")
        await assertEventually("all resumed") { self.playing(self.first, self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("s", code: 1)), "Panic Selected")
        try await engine.advance(seconds: 1)
        await assertEventually("Intro panicked") { !self.playing(self.first, self.intro) }
        XCTAssertNil(app.handleKeyDown(try event("v", code: 9)), "Preview Selected")
        await assertEventually("previewing") { self.first.previewing.contains(self.intro) }
        delegate.saveWorkspace(nil)
        await assertEventually("Save saved") { self.first.document?.dirty == false }
    }

    func testEscStillPanicsWhateverTheCueHotkeys() async throws {
        _ = await first.send(
            command: .setProperty(intro, .triggers(hotkey: KeyChord(KeyChord.escape).hotkeyJSON, enabled: true)))
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
