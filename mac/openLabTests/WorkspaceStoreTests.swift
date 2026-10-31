// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class WorkspaceStoreTests: XCTestCase {
    private let list: ModelID = "00000000-0000-0000-0000-000000000001"
    private let cueA: ModelID = "00000000-0000-0000-0000-00000000000a"

    /// No engine: the store as the golden welcome left it (for what it does with what it receives).
    private func connectedRig() async throws -> Rig { try Rig.offline() }

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

    func testEditInPlaceKeepsPositionAndIsAllOrNothing() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        let group =
            #"{"type":"group","mode":{"type":"timeline"},"children":[{"id":"k1","name":"kid","kind":{"type":"stop","target":null}},{"id":"k2","name":"kid2","kind":{"type":"stop","target":null}}]}"#
        s.receive(
            try change(8, upserts: [cueUpsert("c1"), cueUpsert("c2", index: 1), cueUpsert("g", kind: group, index: 2)]))
        let top = { s.model?.cueLists[0].cues.map(\.id.raw) ?? [] }
        let before = s.model?.cueLists
        // Same id at the same index: replaced where it stands, children too.
        let renamed =
            #"{"type":"cue","list":"\#(list)","parent":null,"index":1,"cue":{"id":"c2","name":"renamed","kind":{"type":"stop","target":null}}}"#
        let kid =
            #"{"type":"cue","list":"\#(list)","parent":"g","index":1,"cue":{"id":"k2","name":"kid two","kind":{"type":"stop","target":null}}}"#
        s.receive(try change(9, upserts: [renamed, kid]))
        XCTAssertEqual(Array(top().prefix(3)), ["c1", "c2", "g"])
        XCTAssertEqual(s.model?.cue("c2")?.name, "renamed")
        XCTAssertEqual(s.model?.cue("g")?.children.map(\.name), ["kid", "kid two"])
        XCTAssertEqual(top().filter { $0 == "c2" }.count, 1)
        // A good in-place edit followed by one that does not fit: nothing applies.
        let after = s.model?.cueLists
        XCTAssertNotEqual(before, after)
        let renamedAgain = renamed.replacingOccurrences(of: "renamed", with: "again")
        let orphan = cueUpsert("z", parent: "missing")
        XCTAssertEqual(s.receive(try change(10, upserts: [renamedAgain, orphan])), .gap)
        XCTAssertEqual(s.model?.cueLists, after)
        XCTAssertEqual(s.revision, 9)
    }

    func testEditAtAnotherIndexMovesTheCue() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        s.receive(try change(8, upserts: [cueUpsert("c1"), cueUpsert("c2", index: 1), cueUpsert("c3", index: 2)]))
        s.receive(try change(9, upserts: [cueUpsert("c1", index: 2)]))
        XCTAssertEqual(Array((s.model?.cueLists[0].cues.map(\.id.raw) ?? []).prefix(3)), ["c2", "c3", "c1"])
    }

    func testChangesAreSentOnlyForWhatApplied() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        var seen: [StoreChange] = []
        let sub = s.changes.sink { seen.append($0) }
        defer { sub.cancel() }
        XCTAssertEqual(s.receive(try change(7)), .ignored)
        XCTAssertEqual(s.receive(try change(20)), .gap)
        XCTAssertEqual(s.receive(try change(8, upserts: [cueUpsert("bad", parent: "missing")])), .gap)
        XCTAssertTrue(seen.isEmpty)
        s.receive(try change(8, upserts: [cueUpsert("c1")], deletes: [#"{"type":"cue","id":"never-held"}"#]))
        guard case .model(let ups, let dels)? = seen.first, seen.count == 1 else { return XCTFail("\(seen)") }
        XCTAssertEqual(ups.count, 1)
        XCTAssertEqual(dels.count, 1)
    }

    func testRuntimeChangesNameTheCuesTheyTouch() async throws {
        let rig = try await connectedRig()
        let s = rig.store
        var seen: [StoreChange] = []
        let sub = s.changes.sink { seen.append($0) }
        defer { sub.cancel() }
        s.receive(
            try msg(
                #"""
                {"type":"runtime","seq":43,"changes":[
                 {"type":"cue_state","cue":"a","state":"running"},
                 {"type":"problems","cue":"b","problems":[]},
                 {"type":"playhead","list":"\#(list)","cue":"new"}]}
                """#))
        guard case .runtime(let cues, let heads)? = seen.first else { return XCTFail("\(seen)") }
        XCTAssertEqual(cues, ["a", "b"])
        // The playhead was on cue ...0b; both the cue it left and the one it reached are named.
        XCTAssertEqual(heads, ["00000000-0000-0000-0000-00000000000b", "new"])
        seen = []
        s.receive(try msg(#"{"type":"progress","items":[{"cue":"a","elapsed":1.0,"remaining":null}]}"#))
        guard case .progress(let ids)? = seen.first else { return XCTFail("\(seen)") }
        XCTAssertEqual(ids, ["a"])
        // A cue that stops running reports its progress ending.
        seen = []
        s.receive(try msg(#"{"type":"runtime","seq":44,"changes":[{"type":"cue_state","cue":"a","state":"idle"}]}"#))
        XCTAssertEqual(seen.count, 2)
        guard case .progress(let ended)? = seen.last else { return XCTFail("\(seen)") }
        XCTAssertEqual(ended, ["a"])
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
        let rig = try await Rig.open()
        // The real engine's log: it started, then opened the show.
        guard case .success(let tail) = await rig.store.logTail(max: 50) else { return XCTFail("log_tail") }
        XCTAssertTrue(tail.entries.contains { $0.message.hasPrefix("workspace opened") }, "\(tail.entries)")
        XCTAssertEqual(tail.dropped, 0)
        guard case .success(let one) = await rig.store.logTail(max: 1) else { return XCTFail("log_tail 1") }
        XCTAssertEqual(one.entries.count, 1, "max is sent and kept to")
    }

    func testLogTailFixtureDecodes() throws {
        guard case .result(_, .ok(let ok)) = try msg(try Fixtures.text("server", "result_log_tail")),
            case .logTail(let entries, let dropped, let seq)? = ok.data
        else { return XCTFail("fixture") }
        XCTAssertEqual(entries.count, 9)
        XCTAssertEqual(entries.first?.kind, .go)
        XCTAssertNil(entries[7].showTime)
        XCTAssertEqual(dropped, 2)
        XCTAssertEqual(seq, 1009, "the newest record number is kept (gap detection)")
    }

    func testPreshowCheckReturnsTheReportAndFixturesDecode() async throws {
        // An empty show passes the engine's check.
        let empty = try await Rig.open("Second")
        guard case .success(let clean) = await empty.store.preshowCheck() else { return XCTFail("clean") }
        XCTAssertTrue(clean.ok)
        XCTAssertEqual(clean.summary.total, 0)
        // The demo show's Radio has no file.
        let demo = try await Rig.open()
        guard case .success(let bad) = await demo.store.preshowCheck() else { return XCTFail("demo") }
        XCTAssertFalse(bad.ok)
        XCTAssertEqual(bad.brokenCues.map(\.name), ["Radio"])
        XCTAssertEqual(bad.brokenCues.first?.problems.first?.type, "media_missing")
        XCTAssertEqual(bad.media.filter { $0.status == .missing }.map(\.path), ["media/o.wav"])
    }

    func testPreshowCheckFixturesDecode() throws {
        guard case .result(_, .ok(let ok)) = try msg(try Fixtures.text("server", "result_preshow_check")),
            case .preshowCheck(let r)? = ok.data
        else { return XCTFail("fixture") }
        XCTAssertFalse(r.ok)
        XCTAssertEqual(r.summary.brokenCues, 2)
        XCTAssertEqual(r.summary.networkUnchecked, 1)
        XCTAssertEqual(r.summary.total, 3)
        XCTAssertEqual(r.brokenCues.map(\.name), ["Storm", ""])
        XCTAssertEqual(r.brokenCues[0].number, "3")
        XCTAssertNil(r.brokenCues[1].number)
        XCTAssertEqual(r.brokenCues[0].problems.first?.type, "media_missing")
        XCTAssertEqual(r.media.map(\.status), [.unsupported, .missing])
        XCTAssertEqual(r.media[0].reason, "unsupported codec")
        XCTAssertEqual(r.patches[0].resolution?.type, "device_missing")
        XCTAssertTrue(r.patches[0].mapped)
        XCTAssertNil(r.patches[1].name)
        XCTAssertNil(r.patches[1].resolution)
        XCTAssertEqual(r.network.map(\.status), [.notChecked, .missing])
        XCTAssertEqual(r.network[0].port, 8000)
        XCTAssertEqual(r.network[0].protocol, "udp")
        XCTAssertEqual(r.system.first?.type, "notice")

        guard case .result(_, .ok(let okOK)) = try msg(try Fixtures.text("server", "result_preshow_check_ok")),
            case .preshowCheck(let empty)? = okOK.data
        else { return XCTFail("ok fixture") }
        XCTAssertTrue(empty.ok)
        XCTAssertTrue(empty.brokenCues.isEmpty)
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
        let rig = try await Rig.open()
        // State the engine never sent: the snapshot it sends when asked takes it away.
        rig.store.receive(
            try msg(
                #"{"type":"runtime","seq":\#(rig.store.runtimeSeq + 1),"changes":[{"type":"cue_state","cue":"x","state":"running"}]}"#
            ))
        XCTAssertNotNil(rig.store.cueStates["x"])
        rig.client.requestSnapshot()
        await assertEventually { rig.store.cueStates["x"] == nil }
        XCTAssertEqual(rig.store.model?.name, "Demo")
    }

    // MARK: Intents

    func testIntentsSendTheRightMessagesAndReturnTheResult() async throws {
        let rig = try await Rig.open()
        let s = rig.store
        guard case .success = await s.go(list: list) else { return XCTFail("go") }
        try await rig.advance(0.1)
        await assertEventually { s.cueStates[cueA]?.state == .running }
        // The fixture command renames Intro.
        let cmd = Command(try Fixtures.json("client", "command")["command"] ?? .null)
        guard case .success = await s.send(command: cmd) else { return XCTFail("command") }
        XCTAssertEqual(s.model?.cue(cueA)?.name, "Storm", "its model_changed came before the result")
        await assertEventually { s.document?.dirty == true }
        guard case .success = await s.save() else { return XCTFail("save") }
        await assertEventually { s.document?.dirty == false }
        let path = try XCTUnwrap(s.document?.path)
        XCTAssertTrue(try String(contentsOfFile: path, encoding: .utf8).contains("Storm"), "saved to the file")
        guard case .success = await s.panic() else { return XCTFail("panic") }
        try await rig.advance(1)  // past the show's 0.5 s panic fade
        await assertEventually { s.cueStates[cueA] == nil || s.cueStates[cueA]?.state == .idle }
    }

    func testOscApiTransportIntentsSendTheirActions() async throws {
        let rig = try await Rig.open()
        let s = rig.store
        func playing() -> Bool { s.cueStates[cueA]?.state == .running }
        func start() async throws {
            guard case .success = await s.goFrom(list: list, cue: cueA) else { return XCTFail("go_from") }
            try await rig.advance(0.1)
            await assertEventually("go_from starts the cue") { playing() }
        }
        try await start()
        guard case .success = await s.stopAll() else { return XCTFail("stop_all") }
        try await rig.advance(0.1)
        await assertEventually("stop_all stops it") { !playing() }
        try await start()
        guard case .success = await s.hardStopAll() else { return XCTFail("hard_stop_all") }
        try await rig.advance(0.1)
        await assertEventually("hard_stop_all stops it") { !playing() }
        try await start()
        guard case .success = await s.panicCue(cueA) else { return XCTFail("panic_cue") }
        try await rig.advance(1)  // past the show's 0.5 s panic fade
        await assertEventually("panic_cue fades it out and stops it") { !playing() }
    }

    func testEngineRefusalComesBackAsATypedError() async throws {
        let rig = try await Rig.open()
        // A rename of a cue the workspace does not have: the engine refuses it.
        let result = await rig.store.send(command: .setProperty("00000000-0000-0000-0000-0000000000ff", .name("x")))
        guard case .failure(.engine(let code, _)) = result else { return XCTFail("\(result)") }
        XCTAssertEqual(code, .commandRefused)
    }
}
