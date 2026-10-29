// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import XCTest

@testable import openLab

@MainActor
final class EngineClientTests: XCTestCase {
    private let list: ModelID = "00000000-0000-0000-0000-000000000001"

    private func ids(_ rig: Rig) -> [UInt64] {
        rig.engine.received.compactMap {
            switch $0 {
            case .command(let id, _, _), .transport(let id, _), .undo(let id), .redo(let id), .request(let id, _):
                return id
            default: return nil
            }
        }
    }

    func testHandshakeSendsHelloAndMirrorsWelcome() async throws {
        let rig = try Rig()
        XCTAssertEqual(rig.client.state, .idle)
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        guard case .hello(let h)? = rig.engine.received.first else { return XCTFail("no hello") }
        XCTAssertEqual(h.protocolVersion, protocolVersion)
        XCTAssertEqual(h.token, rig.engine.token)
        XCTAssertEqual(h.workspace, "1")
        XCTAssertEqual(rig.client.runToken, rig.engine.token, "the doc id is pinned to this engine run")
        XCTAssertEqual(rig.store.model?.name, "Test")
        XCTAssertEqual(rig.store.revision, 7)
        XCTAssertEqual(rig.store.connectionState, .connected)
    }

    func testRequestIDsStartAtOneAndNeverUseU64Max() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        _ = await rig.store.undo()
        _ = await rig.store.undo()
        XCTAssertEqual(ids(rig), [1, 2])

        rig.client.nextRequestID = UInt64.max - 1
        for _ in 0..<3 { _ = await rig.store.redo() }
        XCTAssertEqual(Array(ids(rig).suffix(3)), [UInt64.max - 1, 1, 2])
        XCTAssertFalse(ids(rig).contains(UInt64.max))
    }

    func testRequestIDsRestartOnEachConnection() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        _ = await rig.store.undo()
        rig.engine.dropConnections()
        _ = await eventually { await rig.scheduler.waitingCount == 1 }
        await rig.scheduler.fire()
        _ = await eventually { rig.client.state == .connected }
        _ = await rig.store.undo()
        XCTAssertEqual(ids(rig), [1, 1])
    }

    func testResultsAreMatchedByIDOutOfOrder() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        let devices = Task { await rig.store.listDevices() }
        let recovery = Task { await rig.client.send { .request(id: $0, .scanRecovery) } }
        _ = await eventually { rig.engine.received.count == 3 }
        rig.engine.releaseResults(reversed: true)
        guard case .success(let d) = await devices.value, case .devices? = d.data else { return XCTFail("devices") }
        guard case .success(let r) = await recovery.value, case .recoveryEntries? = r.data else {
            return XCTFail("recovery")
        }
    }

    func testPendingRequestsFailOnDisconnect() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        let pending = Task { await rig.store.undo() }
        _ = await eventually { rig.engine.received.count == 2 }
        rig.engine.dropConnections()
        let result = await pending.value
        XCTAssertEqual(result, .failure(.disconnected))
        // And while down, a request fails at once rather than queueing.
        let later = await rig.store.undo()
        XCTAssertEqual(later, .failure(.notConnected))
    }

    /// M15: an engine that keeps its socket open but stops answering must not leave GO waiting
    /// for ever. A live request fails with `timeout`; a document request (a slow save) has no
    /// deadline; two timeouts in a row close the connection, which then reconnects.
    func testALiveRequestTimesOutAndRepeatedTimeoutsReconnect() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        let go = Task { await rig.store.go(list: "00000000-0000-0000-0000-000000000001") }
        let save = Task { await rig.store.save() }
        await assertEventually { await rig.requestScheduler.waitingCount == 1 }  // GO only
        await rig.requestScheduler.fire()
        let first = await go.value
        XCTAssertEqual(first, .failure(.timeout))
        XCTAssertEqual(rig.client.state, .connected, "one timeout is not a dead engine")

        let stop = Task { await rig.store.stopAll() }
        await assertEventually { await rig.requestScheduler.waitingCount == 1 }
        await rig.requestScheduler.fire()
        let second = await stop.value
        XCTAssertEqual(second, .failure(.timeout))
        // The second in a row closes the connection: the save in flight fails and it reconnects.
        let saved = await save.value
        XCTAssertEqual(saved, .failure(.disconnected))
        await assertEventually { rig.client.state != .connected }
        rig.engine.holdResults = false
        await assertEventually {
            await rig.scheduler.fire()
            return rig.client.state == .connected
        }
    }

    /// A result in between resets the count: a slow engine is not a dead one.
    func testAResultBetweenTimeoutsKeepsTheConnection() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        let go = Task { await rig.store.stopAll() }
        await assertEventually { await rig.requestScheduler.waitingCount == 1 }
        await rig.requestScheduler.fire()
        _ = await go.value
        rig.engine.releaseResults()  // the late answer
        rig.engine.holdResults = false
        let undo = await rig.store.undo()
        if case .failure(let e) = undo { XCTFail("\(e)") }
        rig.engine.holdResults = true
        let again = Task { await rig.store.stopAll() }
        // The answered undo's timer is cancelled but still parked in the manual scheduler.
        await assertEventually { await rig.requestScheduler.waitingCount == 2 }
        await rig.requestScheduler.fire()
        let result = await again.value
        XCTAssertEqual(result, .failure(.timeout))
        await settle()
        XCTAssertEqual(rig.client.state, .connected)
    }

    func testAtMostOneSnapshotRequestIsOutstanding() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        for _ in 0..<3 { rig.client.requestSnapshot() }
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 1)
        // A second snapshot request is held back too.
        let blocked = await rig.client.send { .request(id: $0, .snapshot) }
        XCTAssertEqual(blocked, .failure(.snapshotAlreadyPending))
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 1)
        // Anything else still goes through.
        let undo = Task { await rig.store.undo() }
        _ = await eventually { rig.engine.received.count == 3 }  // hello, snapshot, undo
        rig.engine.releaseResults()
        _ = await undo.value
        // Once the result is in, the next one is allowed.
        rig.client.requestSnapshot()
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 2)
    }

    func testRevisionGapRequestsExactlyOneSnapshot() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        var gap = try Fixtures.json("server", "model_changed")
        gap.set("revision", .int(9))  // the store is at 7
        rig.engine.holdResults = true
        rig.engine.push(try gap.text())
        rig.engine.push(try gap.text())
        await assertEventually { rig.engine.requestCount { $0 == .snapshot } >= 1 }
        await settle()
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 1)
        XCTAssertEqual(rig.store.revision, 7, "a gapped change must not be applied")
        // The snapshot replaced the state with the engine's.
        XCTAssertEqual(rig.store.model?.name, "Test")
    }

    func testSeqGapRequestsExactlyOneSnapshot() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        var gap = try Fixtures.json("server", "runtime")
        gap.set("seq", .int(44))  // the store is at 42
        rig.engine.push(try gap.text())
        await assertEventually { rig.engine.requestCount { $0 == .snapshot } >= 1 }
        await settle()
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 1)
        XCTAssertEqual(rig.store.runtimeSeq, 42)
    }

    func testGapWhileASnapshotIsOutDoesNotSendAnother() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        var g1 = try Fixtures.json("server", "model_changed")
        g1.set("revision", .int(20))
        var g2 = try Fixtures.json("server", "runtime")
        g2.set("seq", .int(90))
        rig.engine.push(try g1.text())
        rig.engine.push(try g2.text())
        await settle()
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 1)
    }

    func testResultAheadOfTheModelRequestsASnapshot() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        let undo = Task { await rig.store.undo() }
        _ = await eventually { rig.engine.received.count == 2 }
        rig.engine.push(#"{"type":"result","id":1,"ok":{"revision":99}}"#)
        let result = await undo.value
        guard case .success = result else { return XCTFail("\(result)") }
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 1)
    }

    func testUnknownAndMalformedFramesAreDroppedAndTheConnectionKept() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.push(#"{"type":"status","uptime":3}"#)
        rig.engine.push(#"{"type":"log_tail","lines":[]}"#)
        rig.engine.push("this is not json")
        rig.engine.push(#"{"type":"document"}"#)  // known type, malformed
        rig.engine.push(try Fixtures.text("server", "document"))
        await assertEventually { rig.store.document?.path == nil }
        XCTAssertEqual(rig.client.state, .connected)
        XCTAssertEqual(rig.engine.connectCount, 1)
    }

    func testEngineErrorAfterWelcomeIsPublishedNotFatal() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        var seen: [ErrorBody] = []
        let sub = rig.client.engineErrors.sink { seen.append($0) }
        rig.engine.push(try Fixtures.text("server", "error_bad_request"))
        await assertEventually { !seen.isEmpty }
        XCTAssertEqual(seen.first?.code, .badRequest)
        XCTAssertEqual(rig.client.state, .connected)
        sub.cancel()
    }

    func testBadTokenSurfacesAndTheTokenIsReReadOnRetry() async throws {
        let rig = try Rig()
        var seen: [ErrorBody] = []
        let sub = rig.client.engineErrors.sink { seen.append($0) }
        rig.box.token = "stale"
        rig.client.start()
        await assertEventually { rig.client.state == .retrying(.badToken) }
        XCTAssertEqual(seen.first?.code, .badToken)
        // The engine restarted: engine.json now holds the new token.
        rig.box.token = rig.engine.token
        let reads = rig.box.reads
        await assertEventually { await rig.scheduler.waitingCount == 1 }
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        XCTAssertGreaterThan(rig.box.reads, reads)
        sub.cancel()
    }

    func testVersionMismatchIsTerminal() async throws {
        let rig = try Rig()
        rig.engine.refuseAuth = .protocolVersionMismatch
        rig.client.start()
        await assertEventually { rig.client.state == .failed(.versionMismatch(engineVersion: protocolVersion)) }
        await settle()
        let waiting = await rig.scheduler.requested
        XCTAssertTrue(waiting.isEmpty, "no retry after a version mismatch")
        XCTAssertEqual(rig.engine.connectCount, 1)
    }

    func testReconnectBackoffDoublesCapsAndResetsAfterAWelcome() async throws {
        let rig = try Rig()
        rig.engine.connectFailure = true
        rig.client.start()
        var expected: [Double] = []
        for step in 0..<8 {
            await assertEventually("step \(step)") { await rig.scheduler.waitingCount == 1 }
            await rig.scheduler.fire()
            expected.append([0.25, 0.5, 1, 2, 4, 5, 5, 5][step])
            _ = await eventually { await rig.scheduler.requested.count == expected.count + 1 }
        }
        let requested = await rig.scheduler.requested
        XCTAssertEqual(Array(requested.prefix(8)), expected)
        XCTAssertGreaterThanOrEqual(rig.box.reads, 8, "connection info is read on every attempt")

        // The engine comes back on a new port with a new token.
        rig.engine.connectFailure = false
        rig.box.token = rig.engine.token
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }

        // A welcome resets the backoff.
        rig.engine.dropConnections()
        await assertEventually { await rig.scheduler.requested.count == 10 }
        let all = await rig.scheduler.requested
        XCTAssertEqual(all.last, 0.25)
    }

    func testReconnectTakesTheNewWelcomeAsTheFullState() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.dropConnections()
        await assertEventually { rig.client.state == .retrying(.lost) }
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        XCTAssertEqual(rig.store.revision, 7)
    }

    func testStopReturnsToIdleAndStopsConnecting() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.client.stop()
        XCTAssertEqual(rig.client.state, .idle)
        await settle()
        XCTAssertEqual(rig.engine.connectCount, 1)
        let r = await rig.store.undo()
        XCTAssertEqual(r, .failure(.notConnected))
    }

    func testBoundClientSendsItsWorkspaceInHello() async throws {
        let rig = try Rig(workspace: "1")
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        guard case .hello(let h)? = rig.engine.received.first else { return XCTFail() }
        XCTAssertEqual(h.workspace, "1")
        XCTAssertEqual(rig.store.model?.name, "Test")
    }

    // MARK: Connection info

    private func tempFile(_ contents: String?) throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("engine.json")
        if let contents { try contents.write(to: url, atomically: true, encoding: .utf8) }
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return url
    }

    func testEngineJSONParsing() throws {
        XCTAssertEqual(
            EngineConnectionInfo.load(from: try tempFile(#"{"port":51234,"token":"abc","pid":42,"extra":1}"#)),
            .success(EngineConnectionInfo(port: 51234, token: "abc", pid: 42)))
        XCTAssertEqual(EngineConnectionInfo.load(from: try tempFile(nil)), .failure(.missing))
        for bad in [
            "", "{", "[]", #"{"port":1}"#, #"{"port":0,"token":"a","pid":1}"#, #"{"port":70000,"token":"a","pid":1}"#,
            #"{"port":5,"token":"","pid":1}"#,
        ] {
            guard case .failure(.unreadable) = EngineConnectionInfo.load(from: try tempFile(bad)) else {
                return XCTFail("accepted \(bad)")
            }
        }
    }

    func testMissingOrGarbledEngineJSONIsAnEngineNotAvailableState() async throws {
        let missing = try tempFile(nil)
        let engine = FakeEngine()
        let scheduler = ManualScheduler()
        let client = EngineClient(
            transport: engine, infoProvider: { EngineConnectionInfo.load(from: missing) }, scheduler: scheduler)
        client.start()
        await assertEventually { client.state == .retrying(.engineNotAvailable) }
        XCTAssertEqual(engine.connectCount, 0)

        // Garbled is the same state, not a crash.
        try "{{{".write(to: missing, atomically: true, encoding: .utf8)
        await scheduler.fire()
        await assertEventually { await scheduler.requested.count == 2 }
        XCTAssertEqual(client.state, .retrying(.engineNotAvailable))

        // Then the engine writes it and the next attempt connects.
        try #"{"port":1,"token":"\#(engine.token)","pid":1}"#.write(to: missing, atomically: true, encoding: .utf8)
        await scheduler.fire()
        await assertEventually { client.state == .connected }
        client.stop()
    }

    func testLaunchArgumentSelectsTheEngine() {
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab"]), .real)
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "fake"]), .fake(crashAfter: nil))
        XCTAssertEqual(
            EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "fake", "-OpenLabFakeCrashAfter", "2"]),
            .fake(crashAfter: 2))
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "fake-single"]), .real)
        XCTAssertEqual(
            EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "fake", "-OpenLabFakeEmpty"]),
            .fake(crashAfter: nil, empty: true))
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "nonsense"]), .real)
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine"]), .real)
    }

    // MARK: Review fixes

    func testStopThenStartKeepsTheNewConnectionWhenTheOldOneClosesLate() async throws {
        let engine = FakeEngine(snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        let transport = LateCloseTransport(engine: engine)
        let token = engine.token
        let client = EngineClient(
            transport: transport, infoProvider: { .success(EngineConnectionInfo(port: 1, token: token, pid: 1)) },
            scheduler: ManualScheduler(), handshakeScheduler: ManualScheduler())
        client.start()
        await assertEventually { client.state == .connected }
        client.stop()
        client.start()
        await assertEventually { client.state == .connected }
        transport.releaseLate()  // the first run's receive now throws and its cleanup runs
        await settle()
        XCTAssertEqual(client.state, .connected)
        let result = await client.send { .undo(id: $0) }
        XCTAssertEqual(result.isSuccess, true, "the new connection must survive the old run's cleanup")
    }

    func testUndecodableResultFailsItsRequestAndFreesTheSnapshotGate() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.garbleResults = true
        let first = await rig.client.send { .request(id: $0, .snapshot) }
        guard case .failure(.encoding) = first else { return XCTFail("\(first)") }
        rig.engine.garbleResults = false
        let second = await rig.client.send { .request(id: $0, .snapshot) }
        XCTAssertEqual(second.isSuccess, true, "the gate must not stay set")
        XCTAssertEqual(rig.client.state, .connected)
    }

    func testUndecodableWelcomeFailsTheAttemptAndRetries() async throws {
        let rig = try Rig()
        rig.engine.welcomeFault = .garbled
        rig.client.start()
        await assertEventually { rig.client.state == .retrying(.transport("undecodable frame before welcome")) }
        rig.engine.welcomeFault = .none
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
    }

    func testMissingWelcomeTimesOutAndRetries() async throws {
        let rig = try Rig()
        rig.engine.welcomeFault = .silent
        rig.client.start()
        await assertEventually { await rig.handshakeScheduler.waitingCount == 1 }
        let asked = await rig.handshakeScheduler.requested
        XCTAssertEqual(asked, [ReconnectPolicy().handshakeTimeout])
        await rig.handshakeScheduler.fire()
        await assertEventually { rig.client.state == .retrying(.transport("no welcome")) }
        rig.engine.welcomeFault = .none
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        // The first attempt's timer was cancelled by the welcome and must not drop this connection.
        await rig.handshakeScheduler.fire()
        await settle()
        XCTAssertEqual(rig.client.state, .connected)
    }

    func testSnapshotRequestAnsweredWithErrorFreesTheGate() async throws {
        let rig = try Rig(workspace: nil)  // unbound: `snapshot` is refused with no_workspace
        _ = await rig.connect()
        for _ in 0..<2 {
            let r = await rig.client.send { .request(id: $0, .snapshot) }
            XCTAssertEqual(r, .failure(.engine(code: .noWorkspace, message: "unbound")))
        }
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 2)
    }

    func testDropWhileASnapshotIsOutAllowsExactlyOneOnTheNewConnection() async throws {
        let rig = try Rig()
        _ = await rig.connect()
        rig.engine.holdResults = true
        rig.client.requestSnapshot()
        rig.engine.dropConnections()
        await assertEventually { rig.client.state == .retrying(.lost) }
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        rig.client.requestSnapshot()
        rig.client.requestSnapshot()
        XCTAssertEqual(rig.engine.requestCount { $0 == .snapshot }, 2, "one before the drop, one after")
    }

    func testBoundClientOfAClosedWorkspaceStopsReconnecting() async throws {
        let rig = try Rig(workspace: "99")
        rig.client.start()
        let failed = ConnectionState.failed(.engine(code: .noWorkspace, message: "no such workspace"))
        await assertEventually { rig.client.state == failed }
        await settle()
        let asked = await rig.scheduler.requested
        XCTAssertTrue(asked.isEmpty)
        XCTAssertEqual(rig.engine.connectCount, 1)
    }

    func testWebSocketTaskAllowsLargeFrames() throws {
        let task = try WebSocketTransport().makeTask(info: EngineConnectionInfo(port: 1, token: "t", pid: 1))
        XCTAssertEqual(task.maximumMessageSize, maxServerFrameBytes)
        XCTAssertGreaterThanOrEqual(task.maximumMessageSize, 32 * 1024 * 1024)
    }
}

extension Result {
    fileprivate var isSuccess: Bool { if case .success = self { return true } else { return false } }
}
