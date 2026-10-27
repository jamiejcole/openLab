// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// NFR-01: the Swift codec against the Rust golden files. The tests iterate the directories, so a
/// fixture added on the Rust side without a Swift type fails here.
final class ProtocolFixtureTests: XCTestCase {
    private func serverTypeName(_ m: ServerMessage) -> String {
        switch m {
        case .welcome: return "welcome"
        case .snapshot: return "snapshot"
        case .modelChanged: return "model_changed"
        case .document: return "document"
        case .runtime: return "runtime"
        case .progress: return "progress"
        case .meters: return "meters"
        case .log: return "log"
        case .warnings: return "warnings"
        case .result: return "result"
        case .error: return "error"
        case .provisional, .unknown(_): return "?"
        }
    }

    func testEveryServerFixtureDecodes() throws {
        let files = try Fixtures.files("server")
        XCTAssertGreaterThan(files.count, 50)
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            let message = try ServerMessage.decode(text)
            let expected = try XCTUnwrap(JSONValue.parse(text)?["type"]?.stringValue)
            // A fixture whose type has no Swift case decodes to .unknown / "?": that is the failure.
            XCTAssertEqual(serverTypeName(message), expected, file.lastPathComponent)
        }
    }

    func testEveryClientFixtureRoundTrips() throws {
        let files = try Fixtures.files("client")
        XCTAssertGreaterThan(files.count, 30)
        for file in files {
            let data = try Data(contentsOf: file)
            let message = try JSONDecoder().decode(ClientMessage.self, from: data)
            let encoded = try JSONEncoder().encode(message)
            let a = try JSONSerialization.jsonObject(with: data) as? NSDictionary
            let b = try JSONSerialization.jsonObject(with: encoded) as? NSDictionary
            XCTAssertEqual(a, b, file.lastPathComponent)
            // And the frame the client really sends decodes back to the same message.
            let again = try JSONDecoder().decode(ClientMessage.self, from: Data(try message.text().utf8))
            XCTAssertEqual(again, message, file.lastPathComponent)
        }
    }

    func testDecodedValuesAreRight() throws {
        guard case .welcome(let v, let snap) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome")
        }
        XCTAssertEqual(v, 1)
        XCTAssertEqual(snap.revision, 7)
        XCTAssertEqual(snap.runtimeSeq, 42)
        XCTAssertEqual(snap.workspace.name, "Test")
        XCTAssertEqual(snap.workspace.cueLists.first?.cues.first?.name, "Intro")
        XCTAssertEqual(snap.cueStates.first?.state, .running)
        XCTAssertEqual(snap.problems.first?.problems.first?.type, "target_missing")
        XCTAssertEqual(snap.document.path, "/Users/sam/Shows/Hamlet.openlab")

        guard
            case .result(let id, .error(let e)) = try ServerMessage.decode(try Fixtures.text("server", "result_error"))
        else { return XCTFail("result_error") }
        XCTAssertEqual(id, 2)
        XCTAssertEqual(e.code, .commandRefused)

        guard case .result(_, .ok(let ok)) = try ServerMessage.decode(try Fixtures.text("server", "result_relink")),
            case .relink(let n, let amb, let un)? = ok.data
        else { return XCTFail("relink") }
        XCTAssertEqual(ok.revision, 9)
        XCTAssertEqual(n, 3)
        XCTAssertEqual(amb.count, 1)
        XCTAssertEqual(un.count, 1)
    }

    func testUnknownFieldsAreIgnored() throws {
        var welcome = try Fixtures.json("server", "welcome")
        welcome.set("future_field", .int(1))
        welcome["snapshot"]?.set("future_field", .string("x"))
        welcome["snapshot"]?["document"]?.set("future_field", .bool(true))
        XCTAssertNoThrow(try ServerMessage.decode(try welcome.text()))

        var hello = try Fixtures.json("client", "hello")
        hello.set("future_field", .null)
        XCTAssertNoThrow(try JSONDecoder().decode(ClientMessage.self, from: Data(try hello.text().utf8)))
    }

    func testUnknownServerTypeDecodesToUnknownNotAnError() throws {
        for type in ["status", "log_tail", "something_new"] {
            let m = try ServerMessage.decode(#"{"type":"\#(type)","anything":[1,2]}"#)
            guard case .unknown(let t) = m else { return XCTFail("\(type)") }
            XCTAssertEqual(t, type)
        }
    }

    func testUnknownRuntimeChangeAndNestedVariantsDoNotFailTheMessage() throws {
        let m = try ServerMessage.decode(
            #"{"type":"runtime","seq":1,"changes":[{"type":"from_the_future","x":1},{"type":"warning","warning":{"type":"new_warning"}}]}"#
        )
        guard case .runtime(_, let changes) = m else { return XCTFail() }
        XCTAssertEqual(changes.count, 2)
    }

    func testNonJSONFrameThrows() {
        XCTAssertThrowsError(try ServerMessage.decode("not json"))
        XCTAssertThrowsError(try ServerMessage.decode(#"{"no_type":1}"#))
    }

    func testHelloOmitsWorkspaceWhenNilAndSendsItWhenBound() throws {
        let plain = try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c")).text()
        XCTAssertFalse(plain.contains("workspace"))
        let bound = try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c", workspace: "doc-1"))
            .text()
        XCTAssertEqual(JSONValue.parse(bound)?["workspace"]?.stringValue, "doc-1")
    }

    func testProvisionalMessagesDecode() throws {
        let app = try ServerMessage.decode(
            #"{"type":"welcome","protocol_version":1,"workspaces":[{"doc":"d1","workspace_id":"w","name":"A","path":null,"dirty":false}]}"#
        )
        guard case .provisional(.appWelcome(_, let list)) = app else { return XCTFail() }
        XCTAssertEqual(list.first?.doc, "d1")
        let closed = try ServerMessage.decode(#"{"type":"closed","doc":"d1"}"#)
        guard case .provisional(.closed(let d)) = closed else { return XCTFail() }
        XCTAssertEqual(d, "d1")
        let close = ClientMessage.provisional(.close(id: 5, doc: "d1", discardChanges: true))
        let again = try JSONDecoder().decode(ClientMessage.self, from: Data(try close.text().utf8))
        XCTAssertEqual(again, close)
    }
}
