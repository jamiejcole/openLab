// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The Network cue properties (M4.6a) encode exactly as the engine's golden fixtures.
final class NetworkCuePropertyTests: XCTestCase {
    private let cue: ModelID = "00000000-0000-0000-0000-00000000000c"

    private func fixture(_ name: String) throws -> JSONValue {
        try Fixtures.json("client", "command_set_network_cue_\(name)")["command"] ?? .null
    }

    private func args() -> [JSONValue] {
        func arg(_ type: String, _ value: JSONValue) -> JSONValue {
            .object(["type": .string(type), "value": value])
        }
        return [
            arg("int", .int(3)),
            arg("float", .double(0.5)),
            arg("string", .string("go")),
            arg("bool", .bool(true)),
        ]
    }

    func testPropertiesMatchTheFixtures() throws {
        let cases: [(String, CueProperty)] = [
            ("destination", .destination("00000000-0000-0000-0000-000000000004")),
            ("address", .address("/cue/1/start")),
            ("args", .args(args())),
        ]
        for (name, property) in cases {
            let encoded = JSONValue.parse(try Command.setProperty(cue, property).json.text())
            XCTAssertEqual(encoded, try fixture(name), name)
        }
    }

    func testFixtureArgumentsDecodeToTheirTypes() throws {
        let command = try fixture("args")
        guard case .array(let items)? = command["property"]?["value"] else {
            return XCTFail("args is not an array")
        }
        XCTAssertEqual(items, args())
    }
}
