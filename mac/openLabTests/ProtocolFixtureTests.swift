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

    func testSetPatchMappingSetsAndClears() throws {
        let set = try JSONDecoder().decode(
            ClientMessage.self, from: Data(try Fixtures.text("client", "request_set_patch_mapping").utf8))
        guard case .request(_, .setPatchMapping(let patch, let mapping)) = set else { return XCTFail("set") }
        XCTAssertEqual(patch, "00000000-0000-0000-0000-000000000070")
        XCTAssertEqual(mapping, PatchMapping(device: "Scarlett 18i20", channels: [2, 3]))
        let clear = try JSONDecoder().decode(
            ClientMessage.self, from: Data(try Fixtures.text("client", "request_clear_patch_mapping").utf8))
        guard case .request(_, .setPatchMapping(_, let cleared)) = clear else { return XCTFail("clear") }
        XCTAssertNil(cleared)
        // A clear is sent as an explicit null, as the fixture has it.
        XCTAssertTrue(try clear.text().contains("\"mapping\":null"))
    }

    /// M5.4d: the snapshot's `media_durations` and the `media_duration` runtime change.
    func testMediaDurationsDecode() throws {
        guard case .snapshot(let snap) = try ServerMessage.decode(try Fixtures.text("server", "snapshot")) else {
            return XCTFail("snapshot")
        }
        XCTAssertEqual(
            snap.mediaDurations,
            [CueMediaDuration(cue: "00000000-0000-0000-0000-00000000000a", file: 62.5, trimmed: 60, duration: 120)])
        guard
            case .runtime(let seq, let changes) = try ServerMessage.decode(
                try Fixtures.text("server", "runtime_media_duration"))
        else { return XCTFail("runtime") }
        XCTAssertEqual(seq, 44)
        let decoded: [CueMediaDuration] = changes.compactMap {
            if case .mediaDuration(let d) = $0 { return d }
            return nil
        }
        XCTAssertEqual(
            decoded,
            [
                CueMediaDuration(
                    cue: "00000000-0000-0000-0000-00000000000a", file: 62.5, trimmed: 60.25, duration: nil),
                CueMediaDuration(cue: "00000000-0000-0000-0000-00000000000b", file: nil, trimmed: nil, duration: nil),
            ])
        // A snapshot without the field (none known) still decodes.
        var bare = try Fixtures.json("server", "snapshot")
        bare["snapshot"]?["media_durations"] = nil
        guard case .snapshot(let old) = try ServerMessage.decode(try bare.text()) else { return XCTFail("bare") }
        XCTAssertNil(old.mediaDurations)
    }

    /// The fake engine sends a `media_duration` change in sequence, which the store applies.
    @MainActor
    func testFakeEnginePushesMediaDurationsInSequence() async throws {
        let rig = try Rig()
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let seq = rig.store.runtimeSeq
        let cue: ModelID = "00000000-0000-0000-0000-00000000000a"
        rig.engine.pushMediaDuration(cue, file: nil, trimmed: nil, duration: nil, doc: rig.engine.docs.keys.first!)
        await assertEventually { rig.store.runtimeSeq == seq + 1 }
        XCTAssertEqual(rig.engine.docs.values.first?["media_durations"]?.arrayValue?.count, 0)
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
            case .relink(let n, let amb, let un, let truncated, let skipped)? = ok.data
        else { return XCTFail("relink") }
        XCTAssertEqual(ok.revision, 9)
        XCTAssertEqual(n, 3)
        XCTAssertEqual(amb.count, 1)
        XCTAssertEqual(un.count, 1)
        XCTAssertTrue(truncated)
        XCTAssertEqual(skipped, 2)
    }

    func testProgressWaitsDecodeAndDefaultToNone() throws {
        let text = try Fixtures.text("server", "progress_waits")
        guard case .progress(let items, let waits) = try ServerMessage.decode(text) else {
            return XCTFail("progress_waits")
        }
        XCTAssertEqual(items.first?.remaining, 4.0)
        XCTAssertEqual(waits.map(\.wait), [.postWait, .preWait])
        XCTAssertEqual(waits.first?.cue, items.first?.cue)
        XCTAssertEqual(waits.last?.elapsed, 0.5)
        XCTAssertEqual(waits.last?.remaining, 1.5)
        // An engine before M4.2e sends no `waits`.
        guard case .progress(_, let none) = try ServerMessage.decode(try Fixtures.text("server", "progress")) else {
            return XCTFail("progress")
        }
        XCTAssertEqual(none, [])
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
