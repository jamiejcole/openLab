// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Network
import XCTest

@testable import openLab

/// The client against the real engine where the engine's answer is the point (the handshake, a
/// refused token, a closed workspace), and against `StubEngine` (a scripted socket with no cue
/// model) for the wire's failure cases the real engine never produces on purpose: results held,
/// reordered or garbled, frames pushed out of turn, a silent or garbled welcome, a dropped socket.
@MainActor
final class EngineClientTests: XCTestCase {
    private let list: ModelID = "00000000-0000-0000-0000-000000000001"

    /// A client bound to the stub's workspace "1" (nil: the app connection), with manual schedulers.
    private func stubRig(workspace: ModelID? = "1") async throws -> (StubEngine, Rig) {
        let stub = try await StubEngine.start()
        let rig = Rig(port: stub.port, token: stub.token, workspace: workspace)
        addTeardownBlock { @MainActor in
            rig.client.stop()
            stub.stop()
        }
        return (stub, rig)
    }

    private func ids(_ stub: StubEngine) -> [UInt64] {
        stub.received.compactMap {
            switch $0 {
            case .command(let id, _, _), .transport(let id, _), .undo(let id), .redo(let id), .request(let id, _):
                return id
            default: return nil
            }
        }
    }

    // MARK: Against the real engine

    /// The engine accepts the hello (token, protocol version, workspace, pid: #168, it checks it)
    /// and the welcome becomes the store's model.
    func testHandshakeSendsHelloAndMirrorsWelcome() async throws {
        let engine = try await TestEngine.shared()
        let doc = try await engine.open("Demo")
        let rig = Rig(engine: engine, workspace: doc)
        XCTAssertEqual(rig.client.state, .idle)
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        XCTAssertEqual(rig.client.runToken, engine.info.token, "the doc id is pinned to this engine run")
        XCTAssertEqual(rig.client.workspace, doc)
        XCTAssertEqual(rig.store.model?.name, "Demo")
        XCTAssertEqual(rig.store.model?.cue(TestSnapshots.intro)?.name, "Intro")
        XCTAssertEqual(rig.store.connectionState, .connected)
    }

    func testBoundClientSendsItsWorkspaceInHello() async throws {
        let rig = try await Rig.open("Second")
        XCTAssertEqual(rig.store.model?.name, "Second", "the engine sent the bound workspace's snapshot")
        XCTAssertEqual(rig.client.state, .connected)
    }

    func testBadTokenSurfacesAndTheTokenIsReReadOnRetry() async throws {
        let engine = try await TestEngine.shared()
        let doc = try await engine.open("Demo")
        var token = "stale"
        var reads = 0
        let scheduler = ManualScheduler()
        let port = engine.info.port
        let client = EngineClient(
            transport: WebSocketTransport(),
            infoProvider: {
                reads += 1
                return .success(EngineConnectionInfo(port: port, token: token, pid: 1))
            }, scheduler: scheduler, workspace: doc)
        addTeardownBlock { @MainActor in client.stop() }
        var seen: [ErrorBody] = []
        let sub = client.engineErrors.sink { seen.append($0) }
        defer { sub.cancel() }
        client.start()
        await assertEventually { client.state == .retrying(.badToken) }
        XCTAssertEqual(seen.first?.code, .badToken, "the engine refused the stale token")
        // The engine restarted: engine.json now holds the new token.
        token = engine.info.token
        let before = reads
        await assertEventually { await scheduler.waitingCount == 1 }
        await scheduler.fire()
        await assertEventually { client.state == .connected }
        XCTAssertGreaterThan(reads, before)
    }

    func testMissingOrGarbledEngineJSONIsAnEngineNotAvailableState() async throws {
        let engine = try await TestEngine.shared()
        let missing = try tempFile(nil)
        let scheduler = ManualScheduler()
        let client = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: missing) },
            scheduler: scheduler, workspace: nil)
        addTeardownBlock { @MainActor in client.stop() }
        client.start()
        await assertEventually { client.state == .retrying(.engineNotAvailable) }

        // Garbled is the same state, not a crash.
        try "{{{".write(to: missing, atomically: true, encoding: .utf8)
        await scheduler.fire()
        await assertEventually { await scheduler.requested.count == 2 }
        XCTAssertEqual(client.state, .retrying(.engineNotAvailable))

        // Then the engine writes it and the next attempt connects.
        try FileManager.default.removeItem(at: missing)
        try FileManager.default.copyItem(at: engine.infoURL, to: missing)
        await scheduler.fire()
        await assertEventually { client.state == .connected }
    }

    /// The snapshot gate opens again whatever the answer: the real engine's (an app connection's
    /// `snapshot` is answered `ok`) and an error (the stub refuses it with no_workspace).
    func testSnapshotRequestAnsweredWithErrorFreesTheGate() async throws {
        let rig = try await Rig.open(nil)
        for _ in 0..<2 {
            let r = await rig.client.send { .request(id: $0, .snapshot) }
            XCTAssertEqual(r.isSuccess, true, "\(r)")
        }
        let (stub, unbound) = try await stubRig(workspace: nil)
        stub.refuseSnapshot = .noWorkspace
        _ = await unbound.connect()
        for _ in 0..<2 {
            let r = await unbound.client.send { .request(id: $0, .snapshot) }
            guard case .failure(.engine(let code, _)) = r else { return XCTFail("\(r)") }
            XCTAssertEqual(code, .noWorkspace, "refused each time, never held back as already pending")
        }
        XCTAssertEqual(stub.requestCount { $0 == .snapshot }, 2)
    }

    func testBoundClientOfAClosedWorkspaceStopsReconnecting() async throws {
        let engine = try await TestEngine.shared()
        let rig = Rig(engine: engine, workspace: "99")
        rig.client.start()
        await assertEventually {
            if case .failed(.engine(code: .noWorkspace, _)) = rig.client.state { return true }
            return false
        }
        await settle()
        let asked = await rig.scheduler.requested
        XCTAssertTrue(asked.isEmpty, "no retry for a workspace the engine does not have")
        rig.client.stop()
    }

    func testStopThenStartKeepsTheNewConnectionWhenTheOldOneClosesLate() async throws {
        let engine = try await TestEngine.shared()
        let transport = LateCloseTransport(inner: WebSocketTransport())
        let url = engine.infoURL
        let client = EngineClient(
            transport: transport, infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler(), handshakeScheduler: ManualScheduler(), workspace: nil)
        addTeardownBlock { @MainActor in client.stop() }
        client.start()
        await assertEventually { client.state == .connected }
        client.stop()
        client.start()
        await assertEventually { client.state == .connected }
        transport.releaseLate()  // the first run's receive now throws and its cleanup runs
        await settle()
        XCTAssertEqual(client.state, .connected)
        let result = await client.send { .request(id: $0, .listDevices) }
        XCTAssertEqual(result.isSuccess, true, "the new connection must survive the old run's cleanup")
    }

    // MARK: Request ids and results (stub)

    func testRequestIDsStartAtOneAndNeverUseU64Max() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        _ = await rig.store.undo()
        _ = await rig.store.undo()
        XCTAssertEqual(ids(stub), [1, 2])

        rig.client.nextRequestID = UInt64.max - 1
        for _ in 0..<3 { _ = await rig.store.redo() }
        XCTAssertEqual(Array(ids(stub).suffix(3)), [UInt64.max - 1, 1, 2])
        XCTAssertFalse(ids(stub).contains(UInt64.max))
    }

    func testRequestIDsRestartOnEachConnection() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        _ = await rig.store.undo()
        stub.dropConnections()
        await assertEventually { await rig.scheduler.waitingCount == 1 }
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        _ = await rig.store.undo()
        XCTAssertEqual(ids(stub), [1, 1])
    }

    func testResultsAreMatchedByIDOutOfOrder() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        let devices = Task { await rig.store.listDevices() }
        let recovery = Task { await rig.client.send { .request(id: $0, .scanRecovery) } }
        await assertEventually { stub.received.count == 3 }
        stub.releaseResults(reversed: true)
        guard case .success(let d) = await devices.value, case .devices? = d.data else { return XCTFail("devices") }
        guard case .success(let r) = await recovery.value, case .recoveryEntries? = r.data else {
            return XCTFail("recovery")
        }
    }

    func testPendingRequestsFailOnDisconnect() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        let pending = Task { await rig.store.undo() }
        await assertEventually { stub.received.count == 2 }
        stub.dropConnections()
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
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
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
        stub.holdResults = false
        await assertEventually {
            await rig.scheduler.fire()
            return rig.client.state == .connected
        }
    }

    /// A result in between resets the count: a slow engine is not a dead one.
    func testAResultBetweenTimeoutsKeepsTheConnection() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        let go = Task { await rig.store.stopAll() }
        await assertEventually { await rig.requestScheduler.waitingCount == 1 }
        await rig.requestScheduler.fire()
        _ = await go.value
        stub.releaseResults()  // the late answer
        stub.holdResults = false
        let undo = await rig.store.undo()
        if case .failure(let e) = undo { XCTFail("\(e)") }
        stub.holdResults = true
        let again = Task { await rig.store.stopAll() }
        // The answered undo's timer is cancelled but still parked in the manual scheduler.
        await assertEventually { await rig.requestScheduler.waitingCount == 2 }
        await rig.requestScheduler.fire()
        let result = await again.value
        XCTAssertEqual(result, .failure(.timeout))
        await settle()
        XCTAssertEqual(rig.client.state, .connected)
    }

    func testUndecodableResultFailsItsRequestAndFreesTheSnapshotGate() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.garbleResults = true
        let first = await rig.client.send { .request(id: $0, .snapshot) }
        guard case .failure(.encoding) = first else { return XCTFail("\(first)") }
        stub.garbleResults = false
        let second = await rig.client.send { .request(id: $0, .snapshot) }
        XCTAssertEqual(second.isSuccess, true, "the gate must not stay set")
        XCTAssertEqual(rig.client.state, .connected)
    }

    // MARK: Snapshots and gaps (stub)

    func testAtMostOneSnapshotRequestIsOutstanding() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        for _ in 0..<3 { rig.client.requestSnapshot() }
        await assertEventually { stub.requestCount { $0 == .snapshot } == 1 }
        // A second snapshot request is held back too.
        let blocked = await rig.client.send { .request(id: $0, .snapshot) }
        XCTAssertEqual(blocked, .failure(.snapshotAlreadyPending))
        // Anything else still goes through.
        let undo = Task { await rig.store.undo() }
        await assertEventually { stub.received.count == 3 }  // hello, snapshot, undo
        XCTAssertEqual(stub.requestCount { $0 == .snapshot }, 1)
        stub.releaseResults()
        _ = await undo.value
        // Once the result is in, the next one is allowed.
        rig.client.requestSnapshot()
        await assertEventually { stub.requestCount { $0 == .snapshot } == 2 }
    }

    func testRevisionGapRequestsExactlyOneSnapshot() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        var gap = try Fixtures.json("server", "model_changed")
        gap.set("revision", .int(9))  // the store is at 7
        stub.holdResults = true
        stub.push(try gap.text())
        stub.push(try gap.text())
        await assertEventually { stub.requestCount { $0 == .snapshot } >= 1 }
        try await stub.roundTrip(rig.client)
        XCTAssertEqual(stub.requestCount { $0 == .snapshot }, 1)
        XCTAssertEqual(rig.store.revision, 7, "a gapped change must not be applied")
        // The snapshot replaced the state with the engine's.
        XCTAssertEqual(rig.store.model?.name, "Test")
    }

    func testSeqGapRequestsExactlyOneSnapshot() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        var gap = try Fixtures.json("server", "runtime")
        gap.set("seq", .int(44))  // the store is at 42
        stub.holdResults = true
        stub.push(try gap.text())
        stub.push(try gap.text())
        await assertEventually { stub.requestCount { $0 == .snapshot } >= 1 }
        try await stub.roundTrip(rig.client)
        XCTAssertEqual(stub.requestCount { $0 == .snapshot }, 1)
        XCTAssertEqual(rig.store.runtimeSeq, 42)
    }

    func testGapWhileASnapshotIsOutDoesNotSendAnother() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        var g1 = try Fixtures.json("server", "model_changed")
        g1.set("revision", .int(20))
        var g2 = try Fixtures.json("server", "runtime")
        g2.set("seq", .int(90))
        stub.push(try g1.text())
        stub.push(try g2.text())
        await assertEventually { stub.requestCount { $0 == .snapshot } >= 1 }
        try await stub.roundTrip(rig.client)
        XCTAssertEqual(stub.requestCount { $0 == .snapshot }, 1)
    }

    func testResultAheadOfTheModelRequestsASnapshot() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        let undo = Task { await rig.store.undo() }
        await assertEventually { stub.received.count == 2 }
        stub.push(#"{"type":"result","id":1,"ok":{"revision":99}}"#)
        let result = await undo.value
        guard case .success = result else { return XCTFail("\(result)") }
        await assertEventually { stub.requestCount { $0 == .snapshot } == 1 }
    }

    func testDropWhileASnapshotIsOutAllowsExactlyOneOnTheNewConnection() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.holdResults = true
        rig.client.requestSnapshot()
        await assertEventually { stub.requestCount { $0 == .snapshot } == 1 }
        stub.dropConnections()
        await assertEventually { rig.client.state == .retrying(.lost) }
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        rig.client.requestSnapshot()
        rig.client.requestSnapshot()
        await assertEventually { stub.requestCount { $0 == .snapshot } == 2 }
        stub.holdResults = false
        _ = await rig.store.undo()
        XCTAssertEqual(stub.requestCount { $0 == .snapshot }, 2, "one before the drop, one after")
    }

    // MARK: Frames (stub)

    func testUnknownAndMalformedFramesAreDroppedAndTheConnectionKept() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        stub.push(#"{"type":"status","uptime":3}"#)
        stub.push(#"{"type":"log_tail","lines":[]}"#)
        stub.push("this is not json")
        stub.push(#"{"type":"document"}"#)  // known type, malformed
        stub.push(try Fixtures.text("server", "document"))
        await assertEventually { rig.store.document?.path == nil }
        XCTAssertEqual(rig.client.state, .connected)
        XCTAssertEqual(stub.connectCount, 1)
    }

    func testEngineErrorAfterWelcomeIsPublishedNotFatal() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        var seen: [ErrorBody] = []
        let sub = rig.client.engineErrors.sink { seen.append($0) }
        stub.push(try Fixtures.text("server", "error_bad_request"))
        await assertEventually { !seen.isEmpty }
        XCTAssertEqual(seen.first?.code, .badRequest)
        XCTAssertEqual(rig.client.state, .connected)
        sub.cancel()
    }

    // MARK: Handshake failures and reconnecting (stub)

    func testVersionMismatchIsTerminal() async throws {
        let (stub, rig) = try await stubRig()
        stub.refuseAuth = .protocolVersionMismatch
        rig.client.start()
        await assertEventually { rig.client.state == .failed(.versionMismatch(engineVersion: protocolVersion)) }
        await settle()
        let waiting = await rig.scheduler.requested
        XCTAssertTrue(waiting.isEmpty, "no retry after a version mismatch")
        XCTAssertEqual(stub.connectCount, 1)
    }

    func testReconnectBackoffDoublesCapsAndResetsAfterAWelcome() async throws {
        let (stub, rig) = try await stubRig()
        // No engine listening: engine.json names a port nobody answers on.
        rig.box.port = try StubEngine.closedPort()
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

        // The engine comes back on a new port.
        rig.box.port = stub.port
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }

        // A welcome resets the backoff.
        stub.dropConnections()
        await assertEventually { await rig.scheduler.requested.count == 10 }
        let all = await rig.scheduler.requested
        XCTAssertEqual(all.last, 0.25)
    }

    func testReconnectTakesTheNewWelcomeAsTheFullState() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        rig.store.receive(
            try ServerMessage.decode(
                #"{"type":"runtime","seq":43,"changes":[{"type":"cue_state","cue":"x","state":"running"}]}"#))
        stub.dropConnections()
        await assertEventually { rig.client.state == .retrying(.lost) }
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        XCTAssertEqual(rig.store.revision, 7)
        XCTAssertEqual(rig.store.runtimeSeq, 42)
        XCTAssertNil(rig.store.cueStates["x"], "what the old connection had is gone")
    }

    func testStopReturnsToIdleAndStopsConnecting() async throws {
        let (stub, rig) = try await stubRig()
        _ = await rig.connect()
        rig.client.stop()
        XCTAssertEqual(rig.client.state, .idle)
        await settle()
        XCTAssertEqual(stub.connectCount, 1)
        let r = await rig.store.undo()
        XCTAssertEqual(r, .failure(.notConnected))
    }

    func testUndecodableWelcomeFailsTheAttemptAndRetries() async throws {
        let (stub, rig) = try await stubRig()
        stub.welcomeFault = .garbled
        rig.client.start()
        await assertEventually { rig.client.state == .retrying(.transport("undecodable frame before welcome")) }
        stub.welcomeFault = .none
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
    }

    func testMissingWelcomeTimesOutAndRetries() async throws {
        let (stub, rig) = try await stubRig()
        stub.welcomeFault = .silent
        rig.client.start()
        await assertEventually { await rig.handshakeScheduler.waitingCount == 1 }
        let asked = await rig.handshakeScheduler.requested
        XCTAssertEqual(asked, [ReconnectPolicy().handshakeTimeout])
        await rig.handshakeScheduler.fire()
        await assertEventually { rig.client.state == .retrying(.noWelcome) }
        stub.welcomeFault = .none
        await rig.scheduler.fire()
        await assertEventually { rig.client.state == .connected }
        // The first attempt's timer was cancelled by the welcome and must not drop this connection.
        await rig.handshakeScheduler.fire()
        await settle()
        XCTAssertEqual(rig.client.state, .connected)
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
        XCTAssertEqual(
            EngineConnectionInfo.load(from: try tempFile(#"{"port":51234,"token":"abc","pid":42,"stopping":true}"#)),
            .success(EngineConnectionInfo(port: 51234, token: "abc", pid: 42, stopping: true)))
        XCTAssertEqual(EngineConnectionInfo.load(from: try tempFile(nil)), .failure(.missing))
        for bad in [
            "", "{", "[]", #"{"port":1}"#, #"{"port":0,"token":"a","pid":1}"#, #"{"port":70000,"token":"a","pid":1}"#,
            #"{"port":5,"token":"","pid":1}"#, #"{"port":5,"token":"a","pid":1,"stopping":"yes"}"#,
        ] {
            guard case .failure(.unreadable) = EngineConnectionInfo.load(from: try tempFile(bad)) else {
                return XCTFail("accepted \(bad)")
            }
        }
    }

    func testLaunchArgumentSelectsTheEngine() {
        let app: [String: String] = [:]
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab"], environment: app), .real)
        XCTAssertEqual(
            EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "nonsense"], environment: app), .real)
        XCTAssertEqual(EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine"], environment: app), .real)
        XCTAssertEqual(
            EngineMode.fromLaunchArguments(
                [
                    "openLab", "-OpenLabEngine", "test", "-OpenLabVirtualDevice", "stepped", "-OpenLabAppSupport",
                    "/tmp/s", "-OpenLabOpen", "/a.openlab", "-OpenLabOpen", "/b.openlab",
                ], environment: app),
            .test(
                TestEngineOptions(
                    clock: .stepped, appSupport: URL(fileURLWithPath: "/tmp/s"), open: ["/a.openlab", "/b.openlab"])))
        // Realtime unless told otherwise, and a folder of its own when none is named.
        guard
            case .test(let o) = EngineMode.fromLaunchArguments(["openLab", "-OpenLabEngine", "test"], environment: app)
        else { return XCTFail("not the test engine") }
        XCTAssertEqual(o.clock, .realtime)
        XCTAssertTrue(o.appSupport.path.hasPrefix(FileManager.default.temporaryDirectory.path))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: o.appSupport.path), [])
        // The unit-test host never runs the shipping engine, whatever its arguments.
        guard
            case .test(let host) = EngineMode.fromLaunchArguments(
                ["openLab"], environment: ["XCTestConfigurationFilePath": "/x"])
        else { return XCTFail("the unit-test host runs the shipping engine") }
        XCTAssertEqual(host.clock, .stepped)
    }

    /// The test engine is the bundled one on its virtual device, in its own folder, ending with the
    /// app (`--watch-stdin`), never relaunching it; the shows open on its first start only.
    func testTheTestEngineArgumentsNeverRelaunchTheApp() {
        let o = TestEngineOptions(clock: .stepped, appSupport: URL(fileURLWithPath: "/tmp/s"), open: ["/a.openlab"])
        XCTAssertEqual(o.engineArguments, ["--virtual-device", "stepped", "--app-support", "/tmp/s", "--watch-stdin"])
        XCTAssertEqual(o.openArguments, ["--open", "/a.openlab"])
        XCTAssertFalse((o.engineArguments + o.openArguments).contains("--relaunch-app"))
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

// MARK: - The wire stub

/// A local WebSocket server that speaks the protocol's framing and nothing else: no cue model. It
/// answers `hello` with the golden welcome (workspace "1") or an app welcome, every request with an
/// empty `ok` (`list_devices` and `scan_recovery` with empty lists, `snapshot` with the golden
/// snapshot), and can be scripted to hold, reorder or garble results, push any frame, refuse the
/// hello, send a garbled or no welcome, and drop its connections. For the client's failure paths;
/// everything the engine decides is tested against the real one.
@MainActor
final class StubEngine {
    enum WelcomeFault { case none, silent, garbled }

    let token = "stub-token"
    private(set) var port: UInt16 = 0
    var holdResults = false
    var garbleResults = false
    /// When set, `hello` is answered with this `error` and the connection closes.
    var refuseAuth: ErrorCode?
    var welcomeFault = WelcomeFault.none
    /// What an app connection's welcome lists (`workspaces` entries; empty by default).
    var appWorkspaces: [JSONValue] = []
    /// What `scan_recovery` lists (`recovery_entries` entry objects).
    var recoveryEntries: [JSONValue] = []

    /// A `workspaces` entry for `doc` (the stub binds only doc 1: the golden welcome's).
    static func workspaceEntry(doc: Int64 = 1, dirty: Bool = false) -> JSONValue {
        .object([
            "doc": .int(doc), "workspace_id": .string("00000000-0000-0000-0000-000000000001"),
            "name": .string("Test"), "path": .null, "dirty": .bool(dirty),
        ])
    }
    /// When set, `snapshot` is answered with this error.
    var refuseSnapshot: ErrorCode?
    private(set) var received: [ClientMessage] = []
    private(set) var connectCount = 0

    private let listener: NWListener
    private var connections: [StubConnection] = []
    private var held: [(StubConnection, String)] = []
    private let snapshot: JSONValue

    private init(listener: NWListener, snapshot: JSONValue) {
        self.listener = listener
        self.snapshot = snapshot
    }

    static func start() async throws -> StubEngine {
        let ws = NWProtocolWebSocket.Options()
        ws.autoReplyPing = true
        ws.maximumMessageSize = 64 * 1024 * 1024
        let params = NWParameters.tcp
        params.defaultProtocolStack.applicationProtocols.insert(ws, at: 0)
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: .any)
        let listener = try NWListener(using: params)
        let stub = StubEngine(
            listener: listener, snapshot: try XCTUnwrap(Fixtures.json("server", "welcome")["snapshot"]))
        listener.newConnectionHandler = { conn in
            MainActor.assumeIsolated { stub.accept(conn) }
        }
        listener.start(queue: .main)
        guard await eventually({ listener.port != nil && listener.state == .ready }), let port = listener.port else {
            throw TestEngineError.notConnected
        }
        stub.port = port.rawValue
        return stub
    }

    /// A port with nothing listening on it (bound once, then released).
    static func closedPort() throws -> UInt16 {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        defer { close(fd) }
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_port = 0
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bound = withUnsafeMutablePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { p -> Int32 in
                guard bind(fd, p, len) == 0 else { return -1 }
                return getsockname(fd, p, &len)
            }
        }
        guard bound == 0 else { throw TestEngineError.refused("no free port") }
        return UInt16(bigEndian: addr.sin_port)
    }

    var info: EngineConnectionInfo { EngineConnectionInfo(port: port, token: token, pid: 1) }

    func stop() {
        dropConnections()
        listener.cancel()
    }

    func push(_ text: String) {
        for c in connections where c.welcomed { c.send(text) }
    }

    func dropConnections() {
        let all = connections
        connections = []
        for c in all { c.close() }
    }

    func releaseResults(reversed: Bool = false) {
        let all = reversed ? held.reversed() : held
        held = []
        for (c, text) in all { c.send(text) }
    }

    func requestCount(_ matches: (Request) -> Bool) -> Int {
        received.filter { if case .request(_, let r) = $0 { return matches(r) } else { return false } }.count
    }

    /// Returns once the stub has answered a request sent now, and its answer arrived: every frame
    /// pushed before it has been applied by the client (one socket keeps order).
    func roundTrip(_ client: EngineClient) async throws {
        let wasHolding = holdResults
        holdResults = false
        defer { holdResults = wasHolding }
        guard case .success = await client.send({ .redo(id: $0) }) else {
            throw TestEngineError.refused("round trip")
        }
    }

    private func accept(_ nw: NWConnection) {
        connectCount += 1
        let c = StubConnection(nw: nw, stub: self)
        connections.append(c)
        c.start()
    }

    fileprivate func remove(_ c: StubConnection) { connections.removeAll { $0 === c } }

    private static func text(_ v: JSONValue) -> String { (try? v.text()) ?? "{}" }

    fileprivate func handle(_ raw: String, from c: StubConnection) {
        guard let message = try? JSONDecoder().decode(ClientMessage.self, from: Data(raw.utf8)) else { return }
        received.append(message)
        guard c.welcomed else {
            guard case .hello(let h) = message else { return c.close() }
            return hello(h, from: c)
        }
        switch message {
        case .command(let id, _, _), .transport(let id, _), .undo(let id), .redo(let id), .panic(let id),
            .hotkey(let id, _, _, _), .close(let id, _, _):
            result(c, id)
        case .request(let id, let r):
            switch r {
            case .listDevices:
                result(c, id, data: .object(["type": .string("devices"), "devices": .array([])]))
            case .scanRecovery:
                result(c, id, data: .object(["type": .string("recovery_entries"), "entries": .array(recoveryEntries)]))
            case .snapshot:
                if let refuseSnapshot {
                    return c.send(
                        Self.text(
                            .object([
                                "type": .string("result"), "id": .uint(id),
                                "error": .object(["code": .string(refuseSnapshot.raw), "message": .string("refused")]),
                            ])))
                }
                c.send(Self.text(.object(["type": .string("snapshot"), "snapshot": snapshot])))
                result(c, id)
            default: result(c, id)
            }
        case .goodbye: c.close()
        case .hello: break
        }
    }

    private func result(_ c: StubConnection, _ id: UInt64, data: JSONValue? = nil) {
        var ok: JSONValue = .object([:])
        if let data { ok.set("data", data) }
        if garbleResults { ok = .string("garbled") }
        let text = Self.text(.object(["type": .string("result"), "id": .uint(id), "ok": ok]))
        if holdResults { held.append((c, text)) } else { c.send(text) }
    }

    private func hello(_ h: Hello, from c: StubConnection) {
        func fail(_ code: ErrorCode) {
            c.send(
                Self.text(
                    .object([
                        "type": .string("error"), "code": .string(code.raw), "message": .string("refused"),
                        "protocol_version": .int(Int64(protocolVersion)),
                    ])), thenClose: true)
        }
        if let refuseAuth { return fail(refuseAuth) }
        if h.token != token { return fail(.badToken) }
        c.welcomed = true
        switch welcomeFault {
        case .none: break
        case .silent: return
        case .garbled: return c.send(#"{"type":"welcome","protocol_version":"x","snapshot":7}"#)
        }
        var welcome: [String: JSONValue] = [
            "type": .string("welcome"), "protocol_version": .int(Int64(protocolVersion)),
        ]
        if h.workspace == nil {
            welcome["workspaces"] = .array(appWorkspaces)
        } else if h.workspace == "1" {
            welcome["snapshot"] = snapshot
        } else {
            c.welcomed = false
            return fail(.noWorkspace)
        }
        c.send(Self.text(.object(welcome)))
    }
}

@MainActor
private final class StubConnection {
    let nw: NWConnection
    unowned let stub: StubEngine
    var welcomed = false
    private var closed = false

    init(nw: NWConnection, stub: StubEngine) {
        self.nw = nw
        self.stub = stub
    }

    func start() {
        nw.start(queue: .main)
        receive()
    }

    private func receive() {
        nw.receiveMessage { [weak self] data, _, _, error in
            MainActor.assumeIsolated {
                guard let self, !self.closed else { return }
                if error != nil { return self.close() }
                if let data, let text = String(data: data, encoding: .utf8) { self.stub.handle(text, from: self) }
                if !self.closed { self.receive() }
            }
        }
    }

    /// `thenClose`: closes once the frame is on the wire (as the engine does after a refusal), so
    /// the client reads it before the close.
    func send(_ text: String, thenClose: Bool = false) {
        guard !closed else { return }
        let meta = NWProtocolWebSocket.Metadata(opcode: .text)
        let context = NWConnection.ContentContext(identifier: "frame", metadata: [meta])
        guard thenClose else {
            return nw.send(content: Data(text.utf8), contentContext: context, isComplete: true, completion: .idempotent)
        }
        nw.send(
            content: Data(text.utf8), contentContext: context, isComplete: true,
            completion: .contentProcessed { [weak self] _ in
                MainActor.assumeIsolated { self?.close() }
            })
    }

    func close() {
        guard !closed else { return }
        closed = true
        stub.remove(self)
        nw.cancel()
    }
}
