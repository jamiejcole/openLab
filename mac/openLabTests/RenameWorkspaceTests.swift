// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// `RenameWorkspace` (M1.20) encodes exactly as the engine's golden fixture.
final class RenameWorkspaceTests: XCTestCase {
    func testEncodesAsTheFixture() throws {
        let encoded = JSONValue.parse(try Command.renameWorkspace("Hamlet").json.text())
        XCTAssertEqual(encoded, try Fixtures.json("client", "command_rename_workspace")["command"] ?? .null)
    }

    func testTheFixtureRoundTripsAsAClientMessage() throws {
        let text = try Fixtures.text("client", "command_rename_workspace")
        let message = try JSONDecoder().decode(ClientMessage.self, from: Data(text.utf8))
        guard case .command(_, let command, _) = message else { return XCTFail("not a command") }
        XCTAssertEqual(command, Command.renameWorkspace("Hamlet"))
    }
}
