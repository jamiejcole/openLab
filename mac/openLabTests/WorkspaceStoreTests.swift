// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class WorkspaceStoreTests: XCTestCase {
    private let list: ModelID = "00000000-0000-0000-0000-000000000001"
    private let cueA: ModelID = "00000000-0000-0000-0000-00000000000a"

    private func connectedRig() async throws -> Rig {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        return rig
    }

    private func msg(_ json: String) throws -> ServerMessage { try ServerMessage.decode(json) }

    private func cueUpsert(
        _ id: String, kind: String = #"{"type":"stop","target":null}"#, parent: String? = nil, index: Int = 0
    ) -> String {
        """
        {"type":"cue","list":"\(list)","parent":\(parent.map { "\"\($0)\"" } ?? "null"),"index":\(index),
         "cue":{"id":"\(id)","number":null,"name":"n-\(id)","kind":\(kind)}}
        """
    }

    private func change(_ rev: Int, upserts: [String] = [], deletes: [String] = []) throws -> ServerMessage {
        try msg(
            #"{"type":"model_changed","revision":\#(rev),"upserts":[\#(upserts.joined(separator: ","))],"deletes":[\#(deletes.joined(separator: ","))]}"#
        )
    }

    func testWelcomeBuildsModelAndRuntimeState() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        XCTAssertEqual(s.model?.cueLists.count, 1)
        XCTAssertEqual(s.model?.cue(cueA)?.number, "1")
        XCTAssertEqual(s.model?.patches.count, 1)
        XCTAssertEqual(s.cueStates[cueA]?.state, .running)
        XCTAssertEqual(s.problems.count, 1)
        XCTAssertEqual(s.groupWarnings.count, 1)
        XCTAssertEqual(s.playheads[list], "00000000-0000-0000-0000-00000000000b")
        XCTAssertEqual(s.previewing, [cueA])
        XCTAssertEqual(s.warnings.count, 1)
        XCTAssertEqual(s.document?.dirty, true)
        XCTAssertEqual(s.runtimeSeq, 42)
    }

    func testSnapshotReplacesEverything() async throws {
        let rig = try await connectedRig()
        rig.store.receive(
            try msg(#"{"type":"runtime","seq":43,"changes":[{"type":"cue_state","cue":"x","state":"running"}]}"#))
        XCTAssertNotNil(rig.store.cueStates["x"])
        var snap = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        snap.set("revision", .int(30))
        snap.set("runtime_seq", .int(5))
        snap["workspace"]?.set("name", .string("Other"))
        snap.set("cue_states", .array([]))
        let outcome = rig.store.receive(try msg(#"{"type":"snapshot","snapshot":\#(try snap.text())}"#))
        XCTAssertEqual(outcome, .applied)
        XCTAssertEqual(rig.store.model?.name, "Other")
        XCTAssertEqual(rig.store.revision, 30)
        XCTAssertEqual(rig.store.runtimeSeq, 5)
        XCTAssertTrue(rig.store.cueStates.isEmpty)
    }

    func testModelChangedFixtureUpsertsAreApplied() async throws {
        let rig = try await connectedRig()
        let outcome = rig.store.receive(try msg(try Fixtures.text("server", "model_changed")))
        XCTAssertEqual(outcome, .applied)
        XCTAssertEqual(rig.store.revision, 8)
        XCTAssertEqual(rig.store.model?.name, "Hamlet")
        XCTAssertEqual(rig.store.model?.cue(cueA)?.name, "Intro")
        XCTAssertEqual(rig.store.model?.cueLists.count, 1)
    }

    func testDeleteAndMoveCues() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        // Add two cues, then move the second above the first (delete + upsert in one message).
        s.receive(try change(8, upserts: [cueUpsert("c1"), cueUpsert("c2", index: 1)]))
        let ids = { s.model?.cueLists[0].cues.map(\.id.raw) ?? [] }
        XCTAssertEqual(ids().prefix(2), ["c1", "c2"])
        s.receive(try change(9, upserts: [cueUpsert("c2", index: 0)]))
        XCTAssertEqual(ids().first, "c2", "an upsert of a held cue moves it")
        XCTAssertEqual(ids().filter { $0 == "c2" }.count, 1)
        s.receive(try change(10, deletes: [#"{"type":"cue","id":"c2"}"#, #"{"type":"cue","id":"never-held"}"#]))
        XCTAssertFalse(ids().contains("c2"))
        XCTAssertEqual(s.revision, 10)
    }

    func testGroupSubtreeUpsertAndDelete() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        let group = cueUpsert(
            "g",
            kind:
                #"{"type":"group","mode":{"type":"timeline"},"children":[{"id":"k1","name":"kid","kind":{"type":"stop","target":null}}]}"#
        )
        s.receive(try change(8, upserts: [group]))
        XCTAssertEqual(s.model?.cue("g")?.children.map(\.id.raw), ["k1"])
        XCTAssertNil(s.model?.cue("g")?.raw["kind"]?["children"], "children live in the tree, not the raw object")
        XCTAssertNotNil(s.model?.cue("k1"))
        s.receive(try change(9, upserts: [cueUpsert("k2", parent: "g", index: 0)]))
        XCTAssertEqual(s.model?.cue("g")?.children.map(\.id.raw), ["k2", "k1"])
        s.receive(try change(10, deletes: [#"{"type":"cue","id":"g"}"#]))
        XCTAssertNil(s.model?.cue("k1"))
        XCTAssertNil(s.model?.cue("g"))
    }

    func testRevisionStepsOtherThanPlusOne() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        XCTAssertEqual(s.receive(try change(7)), .ignored)
        XCTAssertEqual(s.receive(try change(3)), .ignored)
        XCTAssertEqual(s.receive(try change(9)), .gap)
        XCTAssertEqual(s.revision, 7)
    }

    func testChangeThatDoesNotFitTheModelIsAGapAndLeavesTheModelAlone() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        let before = s.model?.cueLists
        let orphan = #"{"type":"cue","list":"nope","parent":null,"index":0,"cue":{"id":"z","kind":{"type":"stop"}}}"#
        XCTAssertEqual(s.receive(try change(8, upserts: [cueUpsert("ok"), orphan])), .gap)
        XCTAssertEqual(s.model?.cueLists, before, "all or nothing")
        XCTAssertEqual(s.revision, 7)
        let badParent = cueUpsert("p", parent: "missing")
        XCTAssertEqual(s.receive(try change(8, upserts: [badParent])), .gap)
        XCTAssertEqual(s.receive(try change(8, upserts: [#"{"type":"future_kind"}"#])), .gap)
    }

    func testRuntimeFixtureIsApplied() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        XCTAssertEqual(s.receive(try msg(try Fixtures.text("server", "runtime"))), .applied)
        XCTAssertEqual(s.runtimeSeq, 43)
        XCTAssertEqual(s.cueStates[cueA]?.state, .broken)
        XCTAssertEqual(s.cueStates[cueA]?.reason, .deviceLost)
        XCTAssertEqual(s.problems[cueA]?.count, 3)
        XCTAssertEqual(s.receive(try msg(try Fixtures.text("server", "runtime"))), .ignored)
    }

    func testRuntimeChangesPreviewGroupPlayheadAndWarnings() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        s.receive(
            try msg(
                #"""
                {"type":"runtime","seq":43,"changes":[
                 {"type":"preview","cue":"p","active":true},
                 {"type":"group_warning","group":"g","warning":true},
                 {"type":"playhead","list":"L","cue":"c"},
                 {"type":"problems","cue":"c","problems":[{"type":"no_media"}]},
                 {"type":"warning","warning":{"type":"go_ignored","list":"L","reason":"no_cue"}}]}
                """#))
        XCTAssertTrue(s.previewing.contains("p"))
        XCTAssertTrue(s.groupWarnings.contains("g"))
        XCTAssertEqual(s.playheads["L"], "c")
        XCTAssertEqual(s.problems["c"]?.first?.type, "no_media")
        XCTAssertEqual(s.runtimeWarnings.count, 1)
        s.receive(
            try msg(
                #"""
                {"type":"runtime","seq":44,"changes":[
                 {"type":"preview","cue":"p","active":false},{"type":"group_warning","group":"g","warning":false},
                 {"type":"playhead","list":"L","cue":null},{"type":"problems","cue":"c","problems":[]}]}
                """#))
        XCTAssertFalse(s.previewing.contains("p"))
        XCTAssertFalse(s.groupWarnings.contains("g"))
        XCTAssertNil(s.playheads["L"])
        XCTAssertNil(s.problems["c"])
    }

    func testRuntimeWarningsAndLogAreBounded() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        for i in 0..<(WorkspaceStore.maxLogEntries + 20) {
            s.receive(.log(.info, "line \(i)"))
        }
        XCTAssertEqual(s.logEntries.count, WorkspaceStore.maxLogEntries)
        XCTAssertEqual(s.logEntries.last?.message, "line \(WorkspaceStore.maxLogEntries + 19)")
    }

    func testStatusKeepsTheLatestOne() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        XCTAssertNil(s.status)
        s.receive(try msg(try Fixtures.text("server", "status")))
        XCTAssertEqual(s.status?.cpuPercent, 7.5)
        XCTAssertEqual(s.status?.device.state, .ok)
        s.receive(try msg(try Fixtures.text("server", "status_device_lost")))
        XCTAssertEqual(s.status?.device.state, .lost)
        XCTAssertNil(s.status?.device.name)
        XCTAssertEqual(s.status?.disk.healthy, false)
    }

    func testLogTailIntentSendsMaxAndReturnsEntries() async throws {
        let rig = try await connectedRig()
        let result = await rig.store.logTail(max: 50)
        guard case .success(let tail) = result else { return XCTFail("\(result)") }
        XCTAssertEqual(tail.entries, [])
        XCTAssertEqual(tail.dropped, 0)
        XCTAssertTrue(rig.engine.received.contains { $0.isLogTail(max: 50) })
        // The fixture's result decodes to entries.
        guard case .result(_, .ok(let ok)) = try msg(try Fixtures.text("server", "result_log_tail")),
            case .logTail(let entries, let dropped)? = ok.data
        else { return XCTFail("fixture") }
        XCTAssertEqual(entries.count, 9)
        XCTAssertEqual(entries.first?.kind, .go)
        XCTAssertNil(entries[7].showTime)
        XCTAssertEqual(dropped, 2)
    }

    func testProgressDocumentAndWarnings() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        s.receive(try msg(try Fixtures.text("server", "progress")))
        XCTAssertEqual(s.progress[cueA]?.elapsed, 1.5)
        XCTAssertEqual(s.progress[cueA]?.remaining, 58.5)
        XCTAssertNil(s.progress["00000000-0000-0000-0000-00000000000b"]?.remaining)
        // A cue that stops running drops its progress.
        s.receive(
            try msg(#"{"type":"runtime","seq":43,"changes":[{"type":"cue_state","cue":"\#(cueA)","state":"idle"}]}"#))
        XCTAssertNil(s.progress[cueA])
        s.receive(try msg(try Fixtures.text("server", "document")))
        XCTAssertEqual(s.document?.dirty, false)
        s.receive(try msg(try Fixtures.text("server", "warnings")))
        XCTAssertEqual(s.warnings.isEmpty, false)
        s.receive(try msg(#"{"type":"warnings","warnings":[]}"#))
        XCTAssertTrue(s.warnings.isEmpty, "warnings replace the list; empty clears it")
    }

    func testSnapshotArrivingThroughTheClientReplacesStoreState() async throws {
        let rig = try await connectedRig()
        var snap = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        snap["workspace"]?.set("name", .string("Pushed"))
        rig.engine.push(#"{"type":"snapshot","snapshot":\#(try snap.text())}"#)
        await assertEventually { rig.store.model?.name == "Pushed" }
    }

    // MARK: Intents

    func testIntentsSendTheRightMessagesAndReturnTheResult() async throws {
        let rig = try await connectedRig()
        let go = await rig.store.go(list: list)
        guard case .success = go else { return XCTFail("\(go)") }
        let cmd = Command(try Fixtures.json("client", "command")["command"] ?? .null)
        let sent = await rig.store.send(command: cmd)
        guard case .success = sent else { return XCTFail("\(sent)") }
        _ = await rig.store.save()
        _ = await rig.store.panic()

        let kinds = rig.engine.received.dropFirst().map { m -> String in
            switch m {
            case .transport(_, .go(let l)): return "go \(l)"
            case .transport(_, .panic): return "panic"
            case .command(_, let c): return "command \(c == cmd)"
            case .request(_, .save): return "save"
            default: return "other"
            }
        }
        XCTAssertEqual(kinds, ["go \(list)", "command true", "save", "panic"])
    }

    func testEngineRefusalComesBackAsATypedError() async throws {
        let rig = try await connectedRig()
        // A fake that refuses: swap the next result for an error by sending the real thing by hand.
        rig.engine.holdResults = true
        let task = Task { await rig.store.send(command: Command(.object([:]))) }
        _ = await eventually { rig.engine.received.count == 2 }
        rig.engine.push(
            try Fixtures.text("server", "result_error").replacingOccurrences(of: "\"id\": 2", with: "\"id\": 1"))
        let result = await task.value
        guard case .failure(.engine(let code, _)) = result else { return XCTFail("\(result)") }
        XCTAssertEqual(code, .commandRefused)
    }
}

extension ClientMessage {
    fileprivate func isLogTail(max: UInt32?) -> Bool {
        if case .request(_, .logTail(let m)) = self { return m == max }
        return false
    }
}
