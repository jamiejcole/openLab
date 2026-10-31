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
        case .appWelcome: return "welcome"
        case .workspaces: return "workspaces"
        case .closed: return "closed"
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

    /// M5.11g: a `batch` transport decodes to its items in order.
    func testBatchTransportDecodesItsItemsInOrder() throws {
        let m = try JSONDecoder().decode(
            ClientMessage.self, from: Data(try Fixtures.text("client", "transport_batch").utf8))
        guard case .transport(_, .batch(let items)) = m else { return XCTFail("batch") }
        XCTAssertEqual(
            items,
            [
                .start(cue: "00000000-0000-0000-0000-00000000000a"),
                .start(cue: "00000000-0000-0000-0000-00000000000b"),
                .pause(cue: "00000000-0000-0000-0000-00000000000c"),
                .resume(cue: "00000000-0000-0000-0000-00000000000d"),
            ])
    }

    func testOutputDeviceAndPatchMappingMessagesDecodeFromTheirFixtures() throws {
        guard
            case .request(_, .setOutputDevice(let device)) = try JSONDecoder().decode(
                ClientMessage.self, from: Data(try Fixtures.text("client", "request_set_output_device").utf8))
        else { return XCTFail("set_output_device") }
        XCTAssertEqual(device, "Scarlett 2i2")
        let devicesText = try Fixtures.text("server", "result_devices")
        guard case .result(_, .ok(let devices)) = try ServerMessage.decode(devicesText),
            case .devices(let list, let selected)? = devices.data
        else { return XCTFail("devices") }
        XCTAssertEqual(list.map(\.name), ["Scarlett 2i2"])
        XCTAssertEqual(selected, "Scarlett 2i2")
        let mappingsText = try Fixtures.text("server", "result_patch_mappings")
        guard case .result(_, .ok(let ok)) = try ServerMessage.decode(mappingsText),
            case .patchMappings(let entries)? = ok.data
        else { return XCTFail("patch_mappings") }
        XCTAssertEqual(entries.count, 2)
        XCTAssertEqual(entries[0].mapping, PatchMapping(device: "Scarlett 2i2", channels: [0, 1]))
        XCTAssertNil(entries[1].mapping)
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

    /// AUD-14: the `waveform` request is sent as the fixture has it and its answer decodes.
    func testWaveformRequestAndResultMatchTheFixtures() throws {
        let sent = ClientMessage.request(
            id: 56, .waveform(cue: "00000000-0000-0000-0000-00000000000a", start: 0, end: 62.5, buckets: 800))
        let golden = try JSONDecoder().decode(
            ClientMessage.self, from: Data(try Fixtures.text("client", "request_waveform").utf8))
        XCTAssertEqual(sent, golden)
        guard case .result(_, .ok(let ok)) = try ServerMessage.decode(try Fixtures.text("server", "result_waveform")),
            case .waveform(let w)? = ok.data
        else { return XCTFail("waveform") }
        XCTAssertEqual(w.cue, "00000000-0000-0000-0000-00000000000a")
        XCTAssertEqual([w.duration, w.start, w.end], [62.5, 1.5, 2.0])
        XCTAssertEqual(
            w.channels,
            [
                WaveformPeaks.Channel(min: [-32767, -120, 0], max: [32767, 4000, 0]),
                WaveformPeaks.Channel(min: [-16384, -1, 0], max: [16384, 1, 0]),
            ])
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
        XCTAssertEqual(v, protocolVersion)
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

    /// SM-07: the engine's Show Mode reaches the app from its own fixture; absent reads as false.
    func testDocumentShowModeDecodesFromTheEngineFixture() throws {
        guard case .document(let on) = try ServerMessage.decode(try Fixtures.text("server", "document_show_mode"))
        else {
            return XCTFail("document_show_mode")
        }
        XCTAssertTrue(on.showMode)
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome")
        }
        XCTAssertFalse(snapshot.document.showMode)
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
        XCTAssertFalse(plain.contains("pid"), "no pid: omitted, as the engine expects")
        let withPID = try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c", pid: 812)).text()
        XCTAssertEqual(JSONValue.parse(withPID)?["pid"], .int(812))
        let bound = try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c", workspace: "1"))
            .text()
        XCTAssertEqual(JSONValue.parse(bound)?["workspace"], .int(1), "a doc id is a number on the wire")
        // A store key that is not a doc id is never sent as one.
        XCTAssertThrowsError(
            try ClientMessage.hello(Hello(protocolVersion: 1, token: "t", clientName: "c", workspace: "default")).text()
        )
    }

    func testWorkspaceMessagesDecode() throws {
        let app = try ServerMessage.decode(
            #"{"type":"welcome","protocol_version":3,"workspaces":[{"doc":4,"workspace_id":"w","name":"A","path":null,"dirty":false}]}"#
        )
        guard case .appWelcome(_, let list) = app else { return XCTFail() }
        XCTAssertEqual(list.first?.doc, "4")
        XCTAssertNil(list.first?.pathHash, "an engine before path_hash: no hash")
        guard case .workspaces(let listed) = try ServerMessage.decode(try Fixtures.text("server", "workspaces"))
        else { return XCTFail("workspaces") }
        XCTAssertEqual(listed.map(\.pathHash), ["0123456789abcdef", nil])
        let closed = try ServerMessage.decode(#"{"type":"closed","doc":4}"#)
        guard case .closed(let d) = closed else { return XCTFail() }
        XCTAssertEqual(d, "4")
        // A doc id is a number: a string one is not a protocol 2 message.
        XCTAssertThrowsError(try ServerMessage.decode(#"{"type":"closed","doc":"4"}"#))
        guard case .result(_, .ok(let ok)) = try ServerMessage.decode(try Fixtures.text("server", "result_ok_opened"))
        else { return XCTFail("opened") }
        XCTAssertEqual(ok.data?.openedDoc, "4")
        let close = ClientMessage.close(id: 5, doc: "4", discardChanges: true)
        let again = try JSONDecoder().decode(ClientMessage.self, from: Data(try close.text().utf8))
        XCTAssertEqual(again, close)
    }
}
