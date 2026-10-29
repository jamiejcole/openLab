// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// M5.11c (TRG-01): the key-map commands encode as the engine's golden fixtures, and the
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
