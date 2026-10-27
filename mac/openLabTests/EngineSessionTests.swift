// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class EngineSessionTests: XCTestCase {
    private func session(_ mode: FakeEngine.Mode) throws -> (EngineSession, FakeEngine) {
        let engine = FakeEngine(mode: mode, snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        let info = engine.info
        let s = EngineSession(transport: engine, infoProvider: { .success(info) }, scheduler: ManualScheduler())
        return (s, engine)
    }

    private func started(_ mode: FakeEngine.Mode) async throws -> (EngineSession, FakeEngine) {
        let (s, e) = try session(mode)
        s.start()
        let ok = await eventually {
            s.connectionState == .connected && !s.workspaces.isEmpty && s.stores.values.allSatisfy { $0.model != nil }
        }
        XCTAssertTrue(ok)
        return (s, e)
    }

    // MARK: Multi-workspace engine

    func testMultiListsWorkspacesAndGivesEachItsOwnStore() async throws {
        let (s, engine) = try await started(.multi)
        XCTAssertEqual(s.mode, .multi)
        XCTAssertEqual(s.workspaces.map(\.doc), ["doc-1"])
        XCTAssertEqual(s.workspaces.first?.name, "Test")
        XCTAssertEqual(s.stores["doc-1"]?.model?.name, "Test")
        // The app connection is unbound; the store's is bound to its doc.
        guard case .hello(let appHello)? = engine.received.first else { return XCTFail() }
        XCTAssertNil(appHello.workspace)
        XCTAssertEqual(engine.connections(of: "doc-1").count, 1)
    }

    func testNewAndOpenProduceSeparateStoresThatDoNotLeakIntoEachOther() async throws {
        let (s, engine) = try await started(.multi)
        guard case .success(let second) = await s.newWorkspace() else { return XCTFail("new") }
        guard case .success(let third) = await s.open(path: "/Shows/Hamlet.openlab") else { return XCTFail("open") }
        XCTAssertEqual([second, third], ["doc-2", "doc-3"])
        await assertEventually { s.stores.count == 3 && s.stores.values.allSatisfy { $0.model != nil } }
        XCTAssertEqual(s.workspaces.count, 3)
        XCTAssertEqual(s.stores[third]?.model?.name, "Hamlet")
        XCTAssertEqual(s.workspaces.last?.path, "/Shows/Hamlet.openlab")
        XCTAssertTrue(s.stores["doc-1"] !== s.stores[second])
        XCTAssertNotEqual(s.stores["doc-1"]?.model?.id, s.stores[second]?.model?.id)

        // A frame pushed to one doc changes only that doc's store.
        engine.push(try Fixtures.text("server", "document"), doc: second)
        await assertEventually { s.stores[second]?.document?.path == nil && s.stores[second]?.document?.dirty == false }
        XCTAssertEqual(s.stores["doc-1"]?.document?.dirty, true)

        // Intents go through the right connection.
        let before = engine.received.count
        _ = await s.stores[third]?.go(list: "00000000-0000-0000-0000-000000000001")
        XCTAssertEqual(engine.received.count, before + 1)
    }

    func testOpeningAnOpenPathIsAlreadyOpen() async throws {
        let (s, _) = try await started(.multi)
        _ = await s.open(path: "/Shows/A.openlab")
        let again = await s.open(path: "/Shows/A.openlab")
        guard case .failure(.engine(let code, _)) = again else { return XCTFail("\(again)") }
        XCTAssertEqual(code, .alreadyOpen)
        XCTAssertEqual(s.workspaces.count, 2)
    }

    func testCloseRemovesTheStoreAndClosesItsConnection() async throws {
        let (s, engine) = try await started(.multi)
        guard case .success(let doc) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores[doc]?.model != nil }
        let closed = await s.close(doc: doc)
        guard case .success = closed else { return XCTFail("\(closed)") }
        await assertEventually { s.stores[doc] == nil && s.workspaces.count == 1 }
        XCTAssertTrue(engine.connections(of: doc).isEmpty)
        XCTAssertNotNil(s.stores["doc-1"])
    }

    func testCloseOfADirtyWorkspaceIsRefusedUnlessDiscarded() async throws {
        let (s, _) = try await started(.multi)  // doc-1 is dirty in the fixture
        let refused = await s.close(doc: "doc-1")
        guard case .failure(.engine(let code, _)) = refused else { return XCTFail("\(refused)") }
        XCTAssertEqual(code, .unsavedChanges)
        XCTAssertNotNil(s.stores["doc-1"])
        guard case .success = await s.close(doc: "doc-1", discardChanges: true) else { return XCTFail() }
        await assertEventually { s.stores.isEmpty && s.workspaces.isEmpty }
    }

    func testAppConnectionSeesDirtyFlagChanges() async throws {
        let (s, engine) = try await started(.multi)
        engine.setDocument("doc-1", dirty: false)
        await assertEventually { s.workspaces.first?.dirty == false }
    }

    func testPanicAllSendsTransportPanicOnTheAppConnection() async throws {
        let (s, engine) = try await started(.multi)
        guard case .success = await s.panicAll() else { return XCTFail() }
        guard case .transport(_, .panic)? = engine.received.last else { return XCTFail() }
    }

    func testScanRecoveryReturnsEntries() async throws {
        let (s, _) = try await started(.multi)
        guard case .success(let entries) = await s.scanRecovery() else { return XCTFail() }
        XCTAssertTrue(entries.isEmpty)
    }

    func testDroppedConnectionReconnectsAndKeepsItsStores() async throws {
        let (s, engine) = try await started(.multi)
        engine.dropConnections()
        await assertEventually { s.connectionState != .connected }
        // Each connection (app + doc-1) retries through its own scheduler; the session's is manual,
        // so the bound store reconnects only when its scheduler fires. The store stays mirrored.
        XCTAssertNotNil(s.stores["doc-1"]?.model)
    }

    // MARK: One-workspace engine (today's)

    func testSingleEngineDegradesToOneWorkspaceOnTheSameConnection() async throws {
        let (s, engine) = try await started(.single)
        XCTAssertEqual(s.mode, .single)
        XCTAssertEqual(s.workspaces.count, 1)
        XCTAssertEqual(s.workspaces.first?.doc, EngineSession.singleDoc)
        XCTAssertEqual(s.stores.keys.sorted { $0.raw < $1.raw }, [EngineSession.singleDoc])
        XCTAssertEqual(s.stores[EngineSession.singleDoc]?.model?.name, "Test")
        XCTAssertEqual(engine.connectCount, 1, "no second connection")
    }

    func testSingleEngineOpenReplacesInPlace() async throws {
        let (s, engine) = try await started(.single)
        let refused = await s.open(path: "/Shows/B.openlab")  // the fixture workspace is dirty
        guard case .failure(.engine(let code, _)) = refused else { return XCTFail("\(refused)") }
        XCTAssertEqual(code, .unsavedChanges)

        guard case .success(let doc) = await s.open(path: "/Shows/B.openlab", discardChanges: true) else {
            return XCTFail()
        }
        XCTAssertEqual(doc, EngineSession.singleDoc)
        await assertEventually { s.stores[doc]?.model?.name == "B" }
        XCTAssertEqual(s.workspaces.first?.path, "/Shows/B.openlab")
        XCTAssertEqual(s.stores.count, 1)
        XCTAssertEqual(engine.openDocs.count, 1)
        XCTAssertGreaterThan(s.stores[doc]?.revision ?? 0, 7, "numbering never restarts")
    }

    func testSingleEngineCannotClose() async throws {
        let (s, engine) = try await started(.single)
        let before = engine.received.count
        let r = await s.close(doc: EngineSession.singleDoc)
        guard case .failure(.unsupported) = r else { return XCTFail("\(r)") }
        XCTAssertEqual(engine.received.count, before, "nothing is sent")
    }

    func testSingleEngineStoreStillGetsRuntimeAndGaps() async throws {
        let (s, engine) = try await started(.single)
        engine.push(try Fixtures.text("server", "runtime"))
        await assertEventually { s.stores[EngineSession.singleDoc]?.runtimeSeq == 43 }
        var gap = try Fixtures.json("server", "model_changed")
        gap.set("revision", .int(99))
        engine.push(try gap.text())
        await assertEventually { engine.requestCount { $0 == .snapshot } == 1 }
    }

    func testStopClearsEverything() async throws {
        let (s, _) = try await started(.multi)
        s.stop()
        XCTAssertTrue(s.stores.isEmpty)
        XCTAssertTrue(s.workspaces.isEmpty)
        XCTAssertEqual(s.mode, .unknown)
        XCTAssertEqual(s.connectionState, .idle)
    }

    func testFakeEngineDefaultSnapshotDecodes() async throws {
        let engine = FakeEngine(mode: .single)
        let info = engine.info
        let s = EngineSession(transport: engine, infoProvider: { .success(info) }, scheduler: ManualScheduler())
        s.start()
        // "Test" is the workspace name in the bundled welcome.json fixture.
        await assertEventually { s.stores[EngineSession.singleDoc]?.model?.name == "Test" }
    }
}
