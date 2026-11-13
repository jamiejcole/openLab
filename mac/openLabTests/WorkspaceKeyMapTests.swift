// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// (TRG-01): the key-map commands encode as the engine's golden fixtures, and the
/// snapshot's `settings.hotkeys` (format v5, with the selected-cue keys) decodes.
final class WorkspaceKeyMapTests: XCTestCase {
    private func key(_ name: String) -> JSONValue {
        .object([
            "key": .string(name),
            "modifiers": .object([
                "shift": .bool(false), "control": .bool(false), "alt": .bool(false), "command": .bool(false),
            ]),
        ])
    }

    func testOneKeyEncodesAsTheFixture() throws {
        let command = Command.setWorkspaceKey(.stopAll, key("f1"))
        let encoded = JSONValue.parse(try command.json.text())
        let fixture = try Fixtures.json("client", "command_set_workspace_setting_hotkey")["command"]
        XCTAssertEqual(encoded, fixture ?? .null)
        // A clear is an explicit null.
        let clear = Command.setWorkspaceKey(.loadSelected, nil).json
        XCTAssertEqual(clear["setting"]?["value"], .null)
        XCTAssertEqual(clear["setting"]?["key"]?.stringValue, "load_selected")
    }

    func testTheWholeMapEncodesAsTheFixture() throws {
        let fixture = try Fixtures.json("client", "command_set_workspace_setting_hotkeys")["command"]
        let map = try XCTUnwrap(fixture?["setting"]?["value"])
        let encoded = JSONValue.parse(try Command.setKeyMap(map).json.text())
        XCTAssertEqual(encoded, fixture ?? .null)
        // The fixture carries every key the Swift side names, and no other.
        guard case .object(let fields) = map else { return XCTFail("not an object") }
        XCTAssertEqual(Set(fields.keys), Set(WorkspaceKey.allCases.map(\.rawValue)))
    }

    func testTheSnapshotDecodesTheSelectedCueKeys() throws {
        guard case .welcome(_, let snap) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome")
        }
        let ws = snap.workspace
        XCTAssertEqual(ws.hotkey(.go)?["key"]?.stringValue, "space")
        XCTAssertEqual(ws.hotkey(.loadSelected)?["key"]?.stringValue, "l")
        XCTAssertEqual(ws.hotkey(.pauseResumeSelected)?["key"]?.stringValue, "p")
        XCTAssertNil(ws.hotkey(.stopAll), "v5: Esc is panic, Stop All has no key")
        XCTAssertNil(ws.hotkey(.startSelected))
        XCTAssertNil(ws.settings["hotkeys"]?["playhead_next"], "v6: no Next / Previous Cue key")
        XCTAssertNil(ws.settings["hotkeys"]?["playhead_previous"])
    }

    func testAMapWithoutTheKeyReadsAsNone() {
        let ws = WorkspaceModel(
            id: ModelID("00000000-0000-0000-0000-000000000001"), name: "Old",
            settings: .object(["hotkeys": .object(["go": key("space")])]), patches: [], networkPatches: [],
            cueLists: [])
        XCTAssertEqual(ws.hotkey(.go)?["key"]?.stringValue, "space")
        XCTAssertNil(ws.hotkey(.resetSelected))
        let none = WorkspaceModel(
            id: ModelID("00000000-0000-0000-0000-000000000001"), name: "Old", settings: .null, patches: [],
            networkPatches: [], cueLists: [])
        XCTAssertNil(none.hotkey(.go))
    }
}

/// The Keys page of the workspace settings (TRG-01, TRG-07), on the demo show and the real engine:
/// a key is typed into an action's field as the operator types it, and the page shows what the
/// engine then holds.
@MainActor
final class KeyMapPageTests: XCTestCase {
    private var rig: Rig!
    private var sc: WorkspaceSettingsWindowController!

    override func setUp() async throws {
        rig = try await Rig.open("Demo")
        sc = WorkspaceSettingsWindowController(store: rig.store)
        sc.menuChords = { [KeyChord("s", .command), KeyChord("z", .command)] }
        await sc.present().value
        sc.tabs.selectTabViewItem(withIdentifier: "keys")
    }

    override func tearDown() async throws {
        sc?.close()
        rig?.client.stop()
    }

    private func key(_ chars: String, code: UInt16 = 0, _ flags: NSEvent.ModifierFlags = []) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
                characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code))
    }

    /// Clicks `key`'s field and presses the chord, as the operator does.
    private func type(
        _ chars: String, code: UInt16 = 0, _ flags: NSEvent.ModifierFlags = [], into action: WorkspaceKey
    ) throws {
        let field = try XCTUnwrap(sc.keyFields[action])
        field.mouseDown(with: try XCTUnwrap(NSEvent.mouseEvent(
            with: .leftMouseDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
            eventNumber: 0, clickCount: 1, pressure: 1)))
        XCTAssertTrue(sc.window?.firstResponder === field, "the field waits for a key")
        XCTAssertEqual(field.placeholderString, loc("settings.keys.press"))
        field.keyDown(with: try key(chars, code: code, flags))
    }

    private func reason(_ key: WorkspaceKey) -> String? {
        guard let label = sc.keyReasons[key], !label.isHidden else { return nil }
        return label.stringValue
    }

    private func stored(_ key: WorkspaceKey) -> String? { rig.store.model?.hotkey(key)?["key"]?.stringValue }

    /// The page holds a column of workspace actions and one of cue-editing actions, each with its key;
    /// a key typed into a field becomes that action's key, and the clear button takes it away.
    func testTheKeyMapPageHasAWorkspaceColumnAndACueEditingColumnTypingAssignsAndClearRemoves() async throws {
        let columns = try XCTUnwrap(
            sc.tabs.tabViewItem(at: sc.tabs.indexOfTabViewItem(withIdentifier: "keys")).view?.subviews.first
                as? NSStackView)
        XCTAssertEqual(columns.arrangedSubviews.count, 2)
        func inColumn(_ index: Int, _ key: WorkspaceKey) -> Bool {
            sc.keyFields[key].map { $0.isDescendant(of: columns.arrangedSubviews[index]) } ?? false
        }
        XCTAssertTrue(WorkspaceKey.workspaceActions.allSatisfy { inColumn(0, $0) })
        XCTAssertTrue(WorkspaceKey.editingActions.allSatisfy { inColumn(1, $0) })
        XCTAssertEqual(WorkspaceKey.editingActions.count, 9)
        let shown = WorkspaceKey.editingActions.map { sc.keyFields[$0]?.stringValue }
        XCTAssertEqual(shown, ["N", "Q", "O", "T", "E", "D", "W", "C", "F"])
        XCTAssertEqual(sc.keyFields[.go]?.stringValue, loc("inspector.hotkey.space"))

        try type("g", code: 5, into: .editNotes)
        await assertEventually("the engine holds G") { self.stored(.editNotes) == "g" }
        await assertEventually("the field shows G") { self.sc.keyFields[.editNotes]?.stringValue == "G" }
        XCTAssertNil(reason(.editNotes))
        XCTAssertTrue(sc.window?.firstResponder !== sc.keyFields[.editNotes], "the field gave up the focus")

        let clear = try XCTUnwrap(sc.keyClearButtons[.editNotes])
        XCTAssertTrue(clear.isEnabled)
        clear.performClick(nil)
        await assertEventually("the engine holds no key") { self.rig.store.model?.hotkey(.editNotes) == nil }
        await assertEventually("the field is empty") { self.sc.keyFields[.editNotes]?.stringValue == "" }
        XCTAssertEqual(sc.keyFields[.editNotes]?.placeholderString, loc("settings.keys.none"))
        XCTAssertFalse(clear.isEnabled, "nothing left to clear")
        XCTAssertEqual(rig.store.model?.settings["hotkeys"]?["edit_notes"], JSONValue.null, "saved as no key")

        // The workspace column works the same way.
        try type("j", code: 38, into: .loadSelected)
        await assertEventually { self.stored(.loadSelected) == "j" }
    }

    /// A key that another action, a cue's hotkey, a menu command or Esc has is not taken: the reason
    /// is beside that field, the field keeps its key and nothing reaches the engine.
    func testAKeyUsedByAnotherActionOrACueHotkeyIsRefusedWithTheReasonBesideTheField() async throws {
        let cue = try XCTUnwrap(rig.store.model?.cueLists.first?.cues.first)
        guard case .success = await rig.store.send(command: .setProperty(cue.id, .triggers(
            hotkey: KeyChord("x").hotkeyJSON, osc: nil)))
        else { return XCTFail("cue hotkey") }
        await assertEventually { self.rig.store.model?.cueHotkeys.count == 1 }
        let sent = rig.wire.commands.count
        let title = WarningRows.cueTitle(
            try XCTUnwrap(rig.store.model?.cue(cue.id)), name: try XCTUnwrap(rig.store.model?.listName(cue)))

        let cases: [(String, UInt16, NSEvent.ModifierFlags, String)] = [
            ("x", 7, [], String(format: loc("settings.keys.refused.cue"), title)),
            (" ", 49, [], String(format: loc("settings.keys.refused.action"), loc("menu.transport.go"))),
            ("d", 2, [], String(format: loc("settings.keys.refused.action"), loc("keymap.action.edit_duration"))),
            ("s", 1, .command, loc("settings.keys.refused.menu")),
            (KeyChord.up, 126, [], loc("settings.keys.refused.reserved")),
        ]
        for (chars, code, flags, text) in cases {
            try type(chars, code: code, flags, into: .editName)
            await assertEventually("the reason for \(chars)") { self.reason(.editName) == text }
            XCTAssertEqual(sc.keyFields[.editName]?.stringValue, "Q", "the field keeps its key")
            XCTAssertNil(reason(.go), "the reason is beside the field typed into only")
        }
        await rig.sync()
        XCTAssertEqual(rig.wire.commands.count, sent, "nothing went to the engine")
        XCTAssertEqual(stored(.editName), "q")

        // A key the workspace column has is refused there too, naming the cue-editing action.
        try type("f", code: 3, into: .pauseAll)
        let flag = String(format: loc("settings.keys.refused.action"), loc("keymap.action.toggle_flag"))
        await assertEventually { self.reason(.pauseAll) == flag }
        XCTAssertNil(reason(.editName), "one reason at a time")

        // A free key is taken and the reason goes.
        try type("k", code: 40, into: .pauseAll)
        await assertEventually { self.stored(.pauseAll) == "k" }
        await assertEventually { self.reason(.pauseAll) == nil }
    }

    /// The engine cannot be reached: the key is not taken, and the field says why.
    func testAKeyTheEngineCannotTakeSaysWhyBesideTheField() async throws {
        let offline = try Rig.offline()
        let page = WorkspaceSettingsWindowController(store: offline.store)
        addTeardownBlock { @MainActor in page.close() }
        XCTAssertEqual(page.keyFields[.editNotes]?.stringValue, "O")
        page.keyFields[.editNotes]?.keyDown(with: try key("g", code: 5))
        await assertEventually("the reason is shown") { page.keyReasons[.editNotes]?.isHidden == false }
        XCTAssertEqual(page.keyReasons[.editNotes]?.stringValue, loc("error.detail.notConnected"))
        XCTAssertEqual(page.keyFields[.editNotes]?.stringValue, "O")
    }
}
