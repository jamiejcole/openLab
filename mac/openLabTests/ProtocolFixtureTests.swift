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
        case .logEntry: return "log_entry"
        case .status: return "status"
        case .warnings: return "warnings"
        case .result: return "result"
        case .error: return "error"
        case .provisional(.appWelcome): return "welcome"
        case .provisional(.workspaces): return "workspaces"
        case .provisional(.closed): return "closed"
        case .unknown(_): return "?"
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
        XCTAssertEqual(v, 2)
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
        for type in ["stats_v2", "log_tail", "something_new"] {
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
        let bound = try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c", workspace: "1"))
            .text()
        XCTAssertEqual(JSONValue.parse(bound)?["workspace"], .int(1), "a doc id is a number on the wire")
        // A store key that is not a doc id is never sent as one.
        XCTAssertThrowsError(
            try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c", workspace: "default")).text()
        )
    }

    func testProvisionalMessagesDecode() throws {
        let app = try ServerMessage.decode(
            #"{"type":"welcome","protocol_version":2,"workspaces":[{"doc":4,"workspace_id":"w","name":"A","path":null,"dirty":false}]}"#
        )
        guard case .provisional(.appWelcome(_, let list)) = app else { return XCTFail() }
        XCTAssertEqual(list.first?.doc, "4")
        let closed = try ServerMessage.decode(#"{"type":"closed","doc":4}"#)
        guard case .provisional(.closed(let d)) = closed else { return XCTFail() }
        XCTAssertEqual(d, "4")
        // A doc id is a number: a string one is not a protocol 2 message.
        XCTAssertThrowsError(try ServerMessage.decode(#"{"type":"closed","doc":"4"}"#))
        guard case .result(_, .ok(let ok)) = try ServerMessage.decode(try Fixtures.text("server", "result_ok_opened"))
        else { return XCTFail("opened") }
        XCTAssertEqual(ok.data?.openedDoc, "4")
        let close = ClientMessage.provisional(.close(id: 5, doc: "4", discardChanges: true))
        let again = try JSONDecoder().decode(ClientMessage.self, from: Data(try close.text().utf8))
        XCTAssertEqual(again, close)
    }
}
