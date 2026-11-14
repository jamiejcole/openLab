// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The Video cue's stage property encodes exactly as the engine's golden fixtures, and a picture
/// that failed mid-show decodes as its own broken reason.
final class VideoCuePropertyTests: XCTestCase {
    private let cue: ModelID = "00000000-0000-0000-0000-00000000000f"

    func testStagePropertyMatchesTheFixtures() throws {
        let cases: [(String, CueProperty)] = [
            ("command_set_cue_stage", .stage("00000000-0000-0000-0000-000000000002")),
            ("command_set_cue_stage_none", .stage(nil)),
        ]
        for (name, property) in cases {
            let encoded = JSONValue.parse(try Command.setProperty(cue, property).json.text())
            XCTAssertEqual(encoded, try Fixtures.json("client", name)["command"] ?? .null, name)
        }
    }

    func testDecodeFailedIsABrokenReason() throws {
        let text = try Fixtures.text("server", "runtime_state_broken_decode_failed")
        guard case .runtime(_, let changes) = try ServerMessage.decode(text),
            case .cueState(_, .broken, let reason)? = changes.first
        else { return XCTFail("not a broken cue state") }
        XCTAssertEqual(reason, .decodeFailed)
    }
}
