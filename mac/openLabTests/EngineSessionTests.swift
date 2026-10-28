// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import XCTest

@testable import openLab

@MainActor
final class EngineSessionTests: XCTestCase {
    private let scheduler = ManualScheduler()

    private func session() throws -> (EngineSession, FakeEngine) {
        let engine = FakeEngine(snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        // Read afresh on every attempt, as engine.json is: a new engine run has another token.
        let s = EngineSession(transport: engine, infoProvider: { .success(engine.info) }, scheduler: scheduler)
        return (s, engine)
    }

    private func started() async throws -> (EngineSession, FakeEngine) {
        let (s, e) = try session()
        s.start()
        let ok = await eventually {
            s.connectionState == .connected && !s.workspaces.isEmpty && s.stores.values.allSatisfy { $0.model != nil }
        }
        XCTAssertTrue(ok)
        return (s, e)
    }

    /// Fires the reconnect backoff until `condition` holds (no real time).
    private func pump(_ condition: @MainActor () -> Bool) async -> Bool {
        for _ in 0..<200 {
            if condition() { return true }
            await scheduler.fire()
            for _ in 0..<20 { await Task.yield() }
        }
        return condition()
    }

    private func requests(_ engine: FakeEngine, _ match: (Request) -> Bool) -> Int {
        engine.requestCount(match)
    }

    // MARK: Workspaces

    func testListsWorkspacesAndGivesEachItsOwnStore() async throws {
        let (s, engine) = try await started()
        XCTAssertEqual(s.workspaces.map(\.doc), ["1"])
        XCTAssertEqual(s.workspaces.first?.name, "Test")
        XCTAssertEqual(s.stores["1"]?.model?.name, "Test")
        // The app connection is unbound; the store's is bound to its doc.
        guard case .hello(let appHello)? = engine.received.first else { return XCTFail() }
        XCTAssertNil(appHello.workspace)
        XCTAssertEqual(engine.connections(of: "1").count, 1)
    }

    func testNewAndOpenProduceSeparateStoresThatDoNotLeakIntoEachOther() async throws {
        let (s, engine) = try await started()
        guard case .success(let second) = await s.newWorkspace() else { return XCTFail("new") }
        guard case .success(let third) = await s.open(path: "/Shows/Hamlet.openlab") else { return XCTFail("open") }
        XCTAssertEqual([second, third], ["2", "3"])
        await assertEventually { s.stores.count == 3 && s.stores.values.allSatisfy { $0.model != nil } }
        XCTAssertEqual(s.workspaces.count, 3)
        XCTAssertEqual(s.stores[third]?.model?.name, "Hamlet")
        XCTAssertEqual(s.workspaces.last?.path, "/Shows/Hamlet.openlab")
        XCTAssertTrue(s.stores["1"] !== s.stores[second])
        XCTAssertNotEqual(s.stores["1"]?.model?.id, s.stores[second]?.model?.id)

        // A frame pushed to one doc changes only that doc's store.
        engine.push(try Fixtures.text("server", "document"), doc: second)
        await assertEventually { s.stores[second]?.document?.path == nil && s.stores[second]?.document?.dirty == false }
        XCTAssertEqual(s.stores["1"]?.document?.dirty, true)

        // Intents go through the right connection.
        let before = engine.received.count
        _ = await s.stores[third]?.go(list: "00000000-0000-0000-0000-000000000001")
        XCTAssertEqual(engine.received.count, before + 1)
    }

    func testStoresAreInOpenOrderNotDocIdStringOrder() async throws {
        let (s, engine) = try await started()
        for _ in 0..<10 { _ = await s.newWorkspace() }
        await assertEventually { s.stores.count == 11 }
        XCTAssertEqual(s.order.map(\.raw), (1...11).map(String.init), "\"10\" comes after \"9\"")
        XCTAssertEqual(s.orderedStores.map(\.doc), s.order)
        // A list naming a doc twice (a broken peer) still gives one store and one place.
        let one = #"{"doc":3,"workspace_id":"w","name":"x","path":null,"dirty":false}"#
        engine.push(#"{"type":"workspaces","workspaces":[\#(one),\#(one)]}"#, doc: nil)
        await assertEventually { s.order == ["3"] }
        XCTAssertEqual(s.stores.count, 1)
    }

    func testOpeningAnOpenPathIsAlreadyOpen() async throws {
        let (s, _) = try await started()
        _ = await s.open(path: "/Shows/A.openlab")
        let again = await s.open(path: "/Shows/A.openlab")
        guard case .failure(.engine(let code, _)) = again else { return XCTFail("\(again)") }
        XCTAssertEqual(code, .alreadyOpen)
        XCTAssertEqual(s.workspaces.count, 2)
    }

    func testOpenWithANewerAutosaveOpensNothingAndSaysSo() async throws {
        let (s, engine) = try await started()
        engine.autosaveNewer = ["/Shows/A.openlab"]
        let offered = await s.open(path: "/Shows/A.openlab")
        guard case .failure(.recoveryOffered(let path, let name, _)) = offered else { return XCTFail("\(offered)") }
        XCTAssertEqual(path, "/Shows/A.openlab")
        XCTAssertEqual(name, "A")
        XCTAssertEqual(s.stores.count, 1)
        guard case .success(let doc) = await s.open(path: "/Shows/A.openlab", recovery: .restore) else {
            return XCTFail()
        }
        await assertEventually { s.stores[doc]?.document?.dirty == true }
    }

    func testCloseRemovesTheStoreAndClosesItsConnection() async throws {
        let (s, engine) = try await started()
        guard case .success(let doc) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores[doc]?.model != nil }
        let closed = await s.close(doc: doc)
        guard case .success = closed else { return XCTFail("\(closed)") }
        await assertEventually { s.stores[doc] == nil && s.workspaces.count == 1 }
        XCTAssertTrue(engine.connections(of: doc).isEmpty)
        XCTAssertNotNil(s.stores["1"])
    }

    func testClosedOnTheStoresOwnConnectionRemovesItAndNeverReconnects() async throws {
        let (s, engine) = try await started()
        guard case .success(let doc) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores[doc]?.connectionState == .connected }
        let client = try XCTUnwrap(s.stores[doc]?.client)
        // Only the bound connection hears it (the app connection's list has not changed yet).
        engine.push(#"{"type":"closed","doc":\#(doc.raw)}"#, doc: doc)
        for c in engine.connections(of: doc) { c.close() }
        await assertEventually { s.stores[doc] == nil }
        XCTAssertFalse(s.order.contains(doc))
        let connects = engine.connectCount
        await scheduler.fire()
        await settle()
        XCTAssertEqual(engine.connectCount, connects, "no reconnect to a closed doc")
        XCTAssertEqual(client.state, .idle)
    }

    func testClosedForAnotherDocOnABoundConnectionIsIgnored() async throws {
        let (s, engine) = try await started()
        engine.push(#"{"type":"closed","doc":42}"#, doc: "1")
        await settle()
        XCTAssertNotNil(s.stores["1"])
        XCTAssertEqual(s.stores["1"]?.connectionState, .connected)
    }

    func testCloseOfADirtyWorkspaceIsRefusedUnlessDiscarded() async throws {
        let (s, _) = try await started()  // doc 1 is dirty in the fixture
        let refused = await s.close(doc: "1")
        guard case .failure(.engine(let code, _)) = refused else { return XCTFail("\(refused)") }
        XCTAssertEqual(code, .unsavedChanges)
        XCTAssertNotNil(s.stores["1"])
        guard case .success = await s.close(doc: "1", discardChanges: true) else { return XCTFail() }
        await assertEventually { s.stores.isEmpty && s.workspaces.isEmpty }
    }

    func testAppConnectionSeesDirtyFlagChanges() async throws {
        let (s, engine) = try await started()
        engine.setDocument("1", dirty: false)
        await assertEventually { s.workspaces.first?.dirty == false }
    }

    func testPanicAllSendsTheAppScopePanic() async throws {
        let (s, engine) = try await started()
        guard case .success = await s.panicAll() else { return XCTFail() }
        guard case .panic? = engine.receivedFrom.last?.message else { return XCTFail("\(engine.received)") }
        XCTAssertNil(engine.receivedFrom.last?.doc ?? nil, "on the app connection")
    }

    func testHotkeyIsOneAppScopeMessage() async throws {
        let (s, engine) = try await started()
        let mods = HotkeyModifiers(shift: true, control: false, alt: false, command: false)
        guard case .success = await s.hotkey(key: "a", modifiers: mods, up: true) else { return XCTFail() }
        guard case .hotkey(_, "a", mods, true)? = engine.received.last else { return XCTFail() }
    }

    func testScanRecoveryReturnsEntries() async throws {
        let (s, engine) = try await started()
        guard case .success(let none) = await s.scanRecovery() else { return XCTFail() }
        XCTAssertTrue(none.isEmpty)
        engine.recoveryEntries = [.object(["workspace_id": .string("w"), "stale": .bool(false)])]
        guard case .success(let one) = await s.scanRecovery() else { return XCTFail() }
        XCTAssertEqual(one.map(\.workspaceId), ["w"])
    }

    func testStopClearsEverything() async throws {
        let (s, _) = try await started()
        s.stop()
        XCTAssertTrue(s.stores.isEmpty)
        XCTAssertTrue(s.order.isEmpty)
        XCTAssertTrue(s.workspaces.isEmpty)
        XCTAssertEqual(s.connectionState, .idle)
    }

    func testFakeEngineDefaultSnapshotDecodes() async throws {
        let engine = FakeEngine()
        let s = EngineSession(transport: engine, infoProvider: { .success(engine.info) }, scheduler: ManualScheduler())
        s.start()
        // "Test" is the workspace name in the bundled welcome.json fixture.
        await assertEventually { s.stores["1"]?.model?.name == "Test" }
    }

    // MARK: Reconnect and reattach (M5.2)

    func testDroppedConnectionToTheSameRunReconnectsWithoutReopening() async throws {
        let (s, engine) = try await started()
        let store = try XCTUnwrap(s.stores["1"])
        engine.dropConnections()
        await assertEventually { s.connectionState != .connected }
        XCTAssertNotNil(s.stores["1"]?.model, "the store stays mirrored")
        let ok = await pump { s.connectionState == .connected && store.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertTrue(s.stores["1"] === store)
        XCTAssertEqual(requests(engine) { if case .open = $0 { return true } else { return false } }, 0)
    }

    /// A new engine run (another token): every window's show is reopened by its file, the store
    /// objects survive under new doc ids, and none is made twice.
    func testNewEngineRunReopensEveryFileAndKeepsTheStores() async throws {
        let (s, engine) = try await started()
        _ = await s.open(path: "/Shows/A.openlab")
        _ = await s.open(path: "/Shows/B.openlab")
        await assertEventually { s.stores.count == 3 && s.stores.values.allSatisfy { $0.model != nil } }
        let before = s.orderedStores.map(\.store)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }

        engine.restart()
        // Another client opened a show first in the new run: it takes doc id 1.
        _ = engine.addDoc(FakeEngine.emptySnapshot(name: "Other"), path: "/Shows/Other.openlab")
        let ok = await pump { events.contains { if case .reattached = $0 { return true } else { return false } } }
        XCTAssertTrue(ok, "\(events)")
        await assertEventually { s.stores.values.allSatisfy { $0.connectionState == .connected && $0.model != nil } }

        XCTAssertEqual(events.first, .newEngineRun)
        // Hamlet had unsaved changes and no autosave newer than its file: they are lost, and said so.
        XCTAssertEqual(events.last, .reattached(failures: [], restored: [], editsLost: ["Test"]))
        XCTAssertTrue(
            before[0].logEntries.contains { $0 == LogEntry(level: .warn, message: loc("engine.reattach.editsLost")) })
        // The same three stores, plus one for the other client's show; none made twice.
        let now = Set(s.orderedStores.map { ObjectIdentifier($0.store) })
        XCTAssertTrue(now.isSuperset(of: before.map(ObjectIdentifier.init)))
        XCTAssertEqual(s.orderedStores.count, 4, "no store made twice")
        // Each store shows its own show under its new id, never what its old id names now.
        XCTAssertTrue(s.stores["2"] === before[0])
        XCTAssertTrue(s.stores["3"] === before[1])
        XCTAssertTrue(s.stores["4"] === before[2])
        XCTAssertEqual(s.orderedStores.map(\.doc), ["1", "2", "3", "4"], "the new run's open order")
        XCTAssertEqual(s.orderedStores.map { $0.store.model?.name }, ["Other", "Hamlet", "A", "B"])
        XCTAssertEqual(
            engine.receivedFrom.compactMap {
                if case .request(_, .open(let path, _)) = $0.message, engine.runs == 2 { return path }
                return nil
            }.suffix(3),
            ["/Users/sam/Shows/Hamlet.openlab", "/Shows/A.openlab", "/Shows/B.openlab"])
    }

    func testAnOldDocIdIsNeverUsedInANewRun() async throws {
        let (s, engine) = try await started()
        let store = try XCTUnwrap(s.stores["1"])
        engine.openFailures = ["/Users/sam/Shows/Hamlet.openlab": .notFound]
        engine.restart()
        _ = engine.addDoc(FakeEngine.emptySnapshot(name: "Other"), path: "/Shows/Other.openlab")  // doc 1 again
        let helloBefore = engine.received.count
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        _ = await pump { events.contains { if case .reattached = $0 { return true } else { return false } } }
        await assertEventually { s.stores["1"]?.connectionState == .connected }
        // The old store never said hello to doc 1 of the new run.
        let bound = engine.received.dropFirst(helloBefore).compactMap { m -> ModelID? in
            if case .hello(let h) = m { return h.workspace }
            return nil
        }
        XCTAssertEqual(bound, ["1"], "only the new store for Other binds to doc 1")
        XCTAssertNotEqual(store.model?.name, "Other")
        XCTAssertNil(s.doc(of: store), "its show could not be reopened: it left the session")
    }

    func testANewerAutosaveIsRestoredForADirtyWindowAndIgnoredForACleanOne() async throws {
        let (s, engine) = try await started()  // doc 1 (Hamlet) is dirty
        _ = await s.open(path: "/Shows/A.openlab")  // clean
        await assertEventually { s.stores.count == 2 }
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        engine.restart()
        engine.autosaveNewer = ["/Users/sam/Shows/Hamlet.openlab", "/Shows/A.openlab"]
        _ = await pump { events.contains { if case .reattached = $0 { return true } else { return false } } }
        XCTAssertEqual(events.last, .reattached(failures: [], restored: ["Test"]))
        let choices = engine.received.compactMap { m -> String? in
            if case .request(_, .open(let path, let choice?)) = m { return "\(path):\(choice.rawValue)" }
            return nil
        }
        XCTAssertEqual(choices, ["/Users/sam/Shows/Hamlet.openlab:restore", "/Shows/A.openlab:ignore"])
        let hamlet = try XCTUnwrap(s.orderedStores.first?.store)
        await assertEventually { hamlet.logEntries.contains { $0.message == loc("engine.reattach.restored") } }
    }

    func testAnUntitledWindowComesBackFromItsAutosaveOrFails() async throws {
        let (s, engine) = try await started()
        guard case .success(let clean) = await s.newWorkspace() else { return XCTFail() }
        guard case .success(let dirty) = await s.newWorkspace() else { return XCTFail() }
        guard case .success(let lost) = await s.newWorkspace() else { return XCTFail() }
        for doc in [dirty, lost] {
            engine.push(
                #"{"type":"document","document":{"path":null,"dirty":true,"can_undo":true,"can_redo":false}}"#, doc: doc
            )
        }
        await assertEventually { s.stores[dirty]?.document?.dirty == true && s.stores[lost]?.document?.dirty == true }
        let dirtyID = try XCTUnwrap(s.stores[dirty]?.model?.id)
        let lostName = try XCTUnwrap(s.stores[lost]?.model?.name)
        _ = clean
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        engine.restart()
        engine.recoveryEntries = [.object(["workspace_id": .string(dirtyID.raw), "stale": .bool(false)])]
        _ = await pump { events.contains { if case .reattached = $0 { return true } else { return false } } }
        guard case .reattached(let failures, let restored, _)? = events.last else { return XCTFail() }
        XCTAssertEqual(failures.map(\.name), [lostName])
        XCTAssertEqual(restored.count, 1)
        XCTAssertEqual(s.stores.count, 3, "Hamlet, the clean one (new) and the recovered one")
        XCTAssertEqual(
            requests(engine) { if case .openRecovery(dirtyID, _, _) = $0 { return true } else { return false } }, 1)
    }

    func testAReattachCutShortResumesAndTakesWhatIsAlreadyOpen() async throws {
        let (s, engine) = try await started()
        _ = await s.open(path: "/Shows/A.openlab")
        await assertEventually { s.stores.count == 2 }
        engine.restart()
        // The new engine already has Hamlet open (the first reopen landed, then the link dropped).
        _ = engine.addDoc(FakeEngine.emptySnapshot(name: "Test"), path: "/Users/sam/Shows/Hamlet.openlab")
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        _ = await pump { events.contains { if case .reattached = $0 { return true } else { return false } } }
        XCTAssertEqual(events.last, .reattached(failures: [], restored: []))
        XCTAssertEqual(s.stores.count, 2, "already_open bound the window to the open doc, no second store")
        XCTAssertEqual(s.orderedStores.map(\.doc), ["1", "2"])
    }
}
