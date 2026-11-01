// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import XCTest

@testable import openLab

/// The session against the real engine: workspaces opened, edited, closed and, across a crash
/// (SIGKILL) and a new engine run in the same Application Support folder, reopened from their files
/// or their autosaves. The stub (EngineClientTests.swift) covers only frames the engine never sends.
@MainActor
final class EngineSessionTests: XCTestCase {
    private let scheduler = ManualScheduler()
    /// The session's connections (`holdResults`: the engine's answers wait until released).
    private let tap = WireTap()
    private let intro = TestSnapshots.intro
    private let list = TestSnapshots.mainList

    /// A session on a new engine run with copies of `shows` open (each a show `name` made from the
    /// test show `template`), connected, every store mirrored.
    private func started(_ shows: [(name: String, template: String)] = [("Demo", "Demo")]) async throws -> (
        EngineSession, EngineRuns
    ) {
        let runs = try EngineRuns()
        addTeardownBlock { @MainActor in runs.endAll() }
        try await runs.start()
        for show in shows { _ = try await runs.open(try runs.show(show.template, named: show.name)) }
        try await runs.advance(0)  // the media probes are taken in (missing files known)
        let s = session(runs)
        s.start()
        let ok = await eventually {
            s.connectionState == .connected && s.workspaces.count == shows.count
                && s.stores.values.allSatisfy { $0.model != nil && $0.connectionState == .connected }
        }
        XCTAssertTrue(ok)
        return (s, runs)
    }

    private func session(_ runs: EngineRuns) -> EngineSession {
        let url = runs.infoURL
        let s = EngineSession(
            transport: tap, infoProvider: { EngineConnectionInfo.load(from: url) }, scheduler: scheduler)
        addTeardownBlock { @MainActor in s.stop() }
        return s
    }

    /// Fires the reconnect backoff until `condition` holds (the engine answers on its socket).
    private func pump(_ condition: @MainActor () -> Bool) async -> Bool {
        await eventually {
            if condition() { return true }
            await scheduler.fire()
            return condition()
        }
    }

    private func reattached(_ events: [SessionEvent]) -> Bool {
        events.contains { if case .reattached = $0 { return true } else { return false } }
    }

    private func store(named name: String, _ s: EngineSession) -> WorkspaceStore? {
        s.orderedStores.first { $0.store.model?.name == name }?.store
    }

    /// Renames Intro: the workspace has unsaved changes.
    private func dirty(_ store: WorkspaceStore?, _ name: String = "Renamed") async throws {
        let store = try XCTUnwrap(store)
        guard case .success = await store.send(command: .setProperty(intro, .name(name))) else {
            return XCTFail("rename")
        }
        await assertEventually { store.document?.dirty == true }
    }

    // MARK: Workspaces

    func testListsWorkspacesAndGivesEachItsOwnStore() async throws {
        let (s, _) = try await started()
        XCTAssertEqual(s.workspaces.map(\.name), ["Demo"])
        let doc = try XCTUnwrap(s.workspaces.first?.doc)
        XCTAssertEqual(s.stores[doc]?.model?.name, "Demo")
        XCTAssertEqual(s.stores.count, 1)
        // The app connection (unbound) and the store's (bound to its doc) were both let in.
        XCTAssertEqual(s.connectionState, .connected)
        XCTAssertEqual(s.stores[doc]?.client.workspace, doc)
    }

    /// The pre-show check is per workspace (#352): the demo show's missing file is not the new one's.
    func testThePreshowCheckIsPerWorkspace() async throws {
        let (s, _) = try await started()
        let demo = try XCTUnwrap(s.workspaces.first?.doc)
        guard case .success(let second) = await s.newWorkspace() else { return XCTFail("new") }
        await assertEventually { s.stores[second]?.model != nil }
        guard case .success(let withProblems)? = await s.stores[demo]?.preshowCheck(),
            case .success(let clear)? = await s.stores[second]?.preshowCheck()
        else { return XCTFail("preshow_check") }
        XCTAssertFalse(withProblems.ok)
        XCTAssertTrue(clear.ok)
    }

    func testNewAndOpenProduceSeparateStoresThatDoNotLeakIntoEachOther() async throws {
        let (s, runs) = try await started()
        let first = try XCTUnwrap(s.workspaces.first?.doc)
        guard case .success(let second) = await s.newWorkspace() else { return XCTFail("new") }
        let path = try runs.show("Demo", named: "Hamlet")
        guard case .success(let third) = await s.open(path: path) else { return XCTFail("open") }
        XCTAssertEqual(Set([first, second, third]).count, 3)
        await assertEventually { s.stores.count == 3 && s.stores.values.allSatisfy { $0.model != nil } }
        XCTAssertEqual(s.workspaces.count, 3)
        XCTAssertEqual(s.stores[third]?.model?.name, "Hamlet")
        XCTAssertEqual(s.workspaces.last?.path, path)
        XCTAssertTrue(s.stores[first] !== s.stores[second])
        XCTAssertNotEqual(s.stores[first]?.model?.id, s.stores[second]?.model?.id)

        // An edit in one workspace changes only that store.
        try await dirty(s.stores[third], "Hamlet's intro")
        XCTAssertEqual(s.stores[third]?.model?.cue(intro)?.name, "Hamlet's intro")
        XCTAssertEqual(s.stores[first]?.model?.cue(intro)?.name, "Intro")
        XCTAssertEqual(s.stores[first]?.document?.dirty, false)

        // Intents go through the right connection: GO in the third starts its cue, not the first's.
        guard case .success? = await s.stores[third]?.go(list: list) else { return XCTFail("go") }
        try await runs.advance(0.1)
        await assertEventually { s.stores[third]?.cueStates[self.intro]?.state == .running }
        XCTAssertNil(s.stores[first]?.cueStates[intro])
    }

    func testStoresAreInOpenOrderNotDocIdStringOrder() async throws {
        let (s, _) = try await started([])
        // The engine holds at most 8 workspaces: open new ones and close the oldest so the open
        // docs' ids pass 9 (doc ids are numbers; "10" sorts before "9" as a string).
        for n in 0..<12 {
            let made = await s.newWorkspace()
            guard case .success = made else { return XCTFail("new \(n): \(made)") }
            await assertEventually { s.stores.count == min(n + 1, 4) + (n >= 4 ? 1 : 0) }
            if s.stores.count > 4, let oldest = s.order.first {
                guard case .success = await s.close(doc: oldest) else { return XCTFail("close \(oldest)") }
                await assertEventually { s.stores.count == 4 && s.workspaces.count == 4 }
            }
        }
        let numbers = s.order.compactMap { UInt64($0.raw) }
        XCTAssertEqual(numbers.count, 4)
        XCTAssertTrue(numbers.contains(9) && numbers.contains { $0 >= 10 }, "\(s.order)")
        XCTAssertEqual(numbers, numbers.sorted(), "\"10\" comes after \"9\": \(s.order)")
        XCTAssertEqual(s.order, s.workspaces.map(\.doc))
        XCTAssertEqual(s.orderedStores.map(\.doc), s.order)
    }

    /// A list naming a doc twice (a broken peer; the engine never sends one) still gives one store
    /// and one place.
    func testAListNamingADocTwiceGivesOneStore() async throws {
        let stub = try await StubEngine.start()
        addTeardownBlock { @MainActor in stub.stop() }
        let info = stub.info
        let s = EngineSession(
            transport: WebSocketTransport(), infoProvider: { .success(info) }, scheduler: scheduler)
        addTeardownBlock { @MainActor in s.stop() }
        s.start()
        await assertEventually { s.connectionState == .connected }
        let one = #"{"doc":1,"workspace_id":"w","name":"x","path":null,"dirty":false}"#
        stub.push(#"{"type":"workspaces","workspaces":[\#(one),\#(one)]}"#)
        await assertEventually { s.order == ["1"] }
        XCTAssertEqual(s.stores.count, 1)
    }

    func testOpeningAnOpenPathIsAlreadyOpen() async throws {
        let (s, runs) = try await started([])
        let path = try runs.show("Second", named: "A")
        _ = await s.open(path: path)
        let again = await s.open(path: path)
        guard case .failure(.engine(let code, _)) = again else { return XCTFail("\(again)") }
        XCTAssertEqual(code, .alreadyOpen)
        await assertEventually { s.workspaces.count == 1 }
    }

    /// A show whose autosave (left by an engine that crashed) is newer than its file opens nothing
    /// until the operator chooses; restoring opens the autosave.
    func testOpenWithANewerAutosaveOpensNothingAndSaysSo() async throws {
        let runs = try EngineRuns()
        addTeardownBlock { @MainActor in runs.endAll() }
        try await runs.start()
        let path = try runs.show("Demo", named: "A")
        let doc = try await runs.open(path)
        try await runs.rename(intro, in: doc, to: "Unsaved")
        try await runs.advance(31)  // past the show's 30 s autosave interval
        try await runs.crash()
        try await runs.start()

        let s = session(runs)
        s.start()
        await assertEventually { s.connectionState == .connected }
        let offered = await s.open(path: path)
        guard case .failure(.recoveryOffered(let offeredPath, let name, _)) = offered else {
            return XCTFail("\(offered)")
        }
        XCTAssertEqual(offeredPath, path)
        XCTAssertEqual(name, "A")
        XCTAssertEqual(s.stores.count, 0)
        guard case .success(let restored) = await s.open(path: path, recovery: .restore) else { return XCTFail() }
        await assertEventually { s.stores[restored]?.document?.dirty == true }
        XCTAssertEqual(s.stores[restored]?.model?.cue(intro)?.name, "Unsaved", "the autosave was opened")
    }

    func testCloseRemovesTheStoreAndClosesItsConnection() async throws {
        let (s, _) = try await started()
        let first = try XCTUnwrap(s.workspaces.first?.doc)
        guard case .success(let doc) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores[doc]?.model != nil }
        let client = try XCTUnwrap(s.stores[doc]?.client)
        let closed = await s.close(doc: doc)
        guard case .success = closed else { return XCTFail("\(closed)") }
        await assertEventually { s.stores[doc] == nil && s.workspaces.count == 1 }
        await assertEventually { client.state == .idle }
        XCTAssertNotNil(s.stores[first])
    }

    /// Another client closes the workspace: its store goes and never reconnects.
    func testClosedOnTheStoresOwnConnectionRemovesItAndNeverReconnects() async throws {
        let (s, runs) = try await started()
        guard case .success(let doc) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores[doc]?.connectionState == .connected }
        let client = try XCTUnwrap(s.stores[doc]?.client)
        guard case .success = await runs.app.send({ .close(id: $0, doc: doc, discardChanges: true) }) else {
            return XCTFail("close")
        }
        await assertEventually { s.stores[doc] == nil }
        XCTAssertFalse(s.order.contains(doc))
        await scheduler.fire()
        await settle()
        XCTAssertEqual(client.state, .idle, "no reconnect to a closed doc")
    }

    /// A `closed` naming another doc on a bound connection (the engine never sends one) is ignored.
    func testClosedForAnotherDocOnABoundConnectionIsIgnored() async throws {
        let stub = try await StubEngine.start()
        addTeardownBlock { @MainActor in stub.stop() }
        stub.appWorkspaces = [
            .object([
                "doc": .int(1), "workspace_id": .string("w"), "name": .string("Test"), "path": .null,
                "dirty": .bool(false),
            ])
        ]
        let info = stub.info
        let s = EngineSession(transport: WebSocketTransport(), infoProvider: { .success(info) }, scheduler: scheduler)
        addTeardownBlock { @MainActor in s.stop() }
        s.start()
        await assertEventually { s.stores["1"]?.connectionState == .connected }
        stub.push(#"{"type":"closed","doc":42}"#)
        try await stub.roundTrip(try XCTUnwrap(s.stores["1"]?.client))
        XCTAssertNotNil(s.stores["1"])
        XCTAssertEqual(s.stores["1"]?.connectionState, .connected)
    }

    func testCloseOfADirtyWorkspaceIsRefusedUnlessDiscarded() async throws {
        let (s, _) = try await started()
        let doc = try XCTUnwrap(s.workspaces.first?.doc)
        try await dirty(s.stores[doc])
        let refused = await s.close(doc: doc)
        guard case .failure(.engine(let code, _)) = refused else { return XCTFail("\(refused)") }
        XCTAssertEqual(code, .unsavedChanges)
        XCTAssertNotNil(s.stores[doc])
        guard case .success = await s.close(doc: doc, discardChanges: true) else { return XCTFail() }
        await assertEventually { s.stores.isEmpty && s.workspaces.isEmpty }
    }

    func testAppConnectionSeesDirtyFlagChanges() async throws {
        let (s, _) = try await started()
        let doc = try XCTUnwrap(s.workspaces.first?.doc)
        XCTAssertEqual(s.workspaces.first?.dirty, false)
        try await dirty(s.stores[doc])
        await assertEventually { s.workspaces.first?.dirty == true }
        guard case .success? = await s.stores[doc]?.save() else { return XCTFail("save") }
        await assertEventually { s.workspaces.first?.dirty == false }
    }

    func testPanicAllSendsTheAppScopePanic() async throws {
        let (s, runs) = try await started()
        let store = try XCTUnwrap(s.orderedStores.first?.store)
        _ = await store.go(list: list)
        try await runs.advance(0.1)
        await assertEventually { store.cueStates[self.intro]?.state == .running }
        guard case .success = await s.panicAll() else { return XCTFail() }
        try await runs.advance(1)  // past the show's 0.5 s panic fade
        await assertEventually { store.cueStates[self.intro]?.state != .running }
    }

    /// A cue's hotkey, sent once on the app connection: the engine matches it in every workspace.
    func testHotkeyIsOneAppScopeMessage() async throws {
        let (s, runs) = try await started()
        let store = try XCTUnwrap(s.orderedStores.first?.store)
        let none = HotkeyModifiers(shift: false, control: false, alt: false, command: false)
        let key: JSONValue = .object([
            "key": .string("j"),
            "modifiers": .object([
                "shift": .bool(false), "control": .bool(false), "alt": .bool(false), "command": .bool(false),
            ]),
        ])
        guard case .success = await store.send(command: .setProperty(intro, .triggers(hotkey: key, osc: nil))) else {
            return XCTFail("set the hotkey")
        }
        guard case .success = await s.hotkey(key: "j", modifiers: none, up: false) else { return XCTFail() }
        try await runs.advance(0.1)
        await assertEventually { store.cueStates[self.intro]?.state == .running }
    }

    func testScanRecoveryReturnsEntries() async throws {
        let runs = try EngineRuns()
        addTeardownBlock { @MainActor in runs.endAll() }
        try await runs.start()
        let s = session(runs)
        s.start()
        await assertEventually { s.connectionState == .connected }
        guard case .success(let none) = await s.scanRecovery() else { return XCTFail() }
        XCTAssertTrue(none.isEmpty)
        // An untitled workspace with unsaved changes, autosaved, then the engine crashed.
        guard case .success(let doc) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores[doc]?.connectionState == .connected }
        let id = try XCTUnwrap(s.stores[doc]?.model?.id)
        _ = await s.stores[doc]?.send(command: .renameWorkspace("Unsaved"))
        try await runs.advance(31)
        try await runs.crash()
        s.stop()
        try await runs.start()
        let after = session(runs)
        after.start()
        await assertEventually { after.connectionState == .connected }
        guard case .success(let one) = await after.scanRecovery() else { return XCTFail() }
        XCTAssertEqual(one.map(\.workspaceId), [id])
    }

    func testStopClearsEverything() async throws {
        let (s, _) = try await started()
        s.stop()
        XCTAssertTrue(s.stores.isEmpty)
        XCTAssertTrue(s.order.isEmpty)
        XCTAssertTrue(s.workspaces.isEmpty)
        XCTAssertEqual(s.connectionState, .idle)
    }

    // MARK: Reconnect and reattach (M5.2)

    /// A dropped socket to the same engine run (the stub drops it; the engine never does on purpose)
    /// reconnects the same store without reopening anything.
    func testDroppedConnectionToTheSameRunReconnectsWithoutReopening() async throws {
        let stub = try await StubEngine.start()
        addTeardownBlock { @MainActor in stub.stop() }
        stub.appWorkspaces = [
            .object([
                "doc": .int(1), "workspace_id": .string("w"), "name": .string("Test"), "path": .null,
                "dirty": .bool(false),
            ])
        ]
        let info = stub.info
        let s = EngineSession(transport: WebSocketTransport(), infoProvider: { .success(info) }, scheduler: scheduler)
        addTeardownBlock { @MainActor in s.stop() }
        s.start()
        await assertEventually { s.stores["1"]?.model != nil && s.stores["1"]?.connectionState == .connected }
        let store = try XCTUnwrap(s.stores["1"])
        stub.dropConnections()
        await assertEventually { s.connectionState != .connected }
        XCTAssertNotNil(s.stores["1"]?.model, "the store stays mirrored")
        let ok = await pump { s.connectionState == .connected && store.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertTrue(s.stores["1"] === store)
        XCTAssertEqual(stub.requestCount { if case .open = $0 { return true } else { return false } }, 0)
    }

    /// A reopen whose path the new engine run holds under another spelling (case) takes that doc,
    /// as the engine compares paths: the window does not fail.
    func testAReattachTakesADocOpenUnderAnotherSpellingOfThePath() async throws {
        let (s, runs) = try await started([("A", "Second")])
        let path = try XCTUnwrap(s.workspaces.first?.path)
        let store = try XCTUnwrap(s.orderedStores.first?.store)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        try await runs.crash()
        try await runs.start()
        let other = (path as NSString).deletingLastPathComponent + "/a.OPENLAB"
        _ = try await runs.open(other)
        let ok = await pump { self.reattached(events) }
        XCTAssertTrue(ok)
        guard case .reattached(let failures, _, _, _)? = events.last else { return XCTFail() }
        XCTAssertEqual(failures, [])
        XCTAssertEqual(s.stores.count, 1)
        XCTAssertTrue(s.orderedStores.first?.store === store)
    }

    /// A new engine run (another token): every window's show is reopened by its file, the store
    /// objects survive under new doc ids, and none is made twice.
    func testNewEngineRunReopensEveryFileAndKeepsTheStores() async throws {
        let (s, runs) = try await started([("Hamlet", "Demo"), ("A", "Second"), ("B", "Second")])
        let before = s.orderedStores.map(\.store)
        let hamletID = try XCTUnwrap(before.first?.model?.id)
        try await dirty(before.first)  // unsaved, and no autosave yet (no device time passed)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }

        try await runs.crash()
        try await runs.start()
        // Another client opened a show first in the new run: it takes the first doc id.
        _ = try await runs.open(try runs.show("Second", named: "Other"))
        let ok = await pump { self.reattached(events) }
        XCTAssertTrue(ok, "\(events)")
        await assertEventually { s.stores.values.allSatisfy { $0.connectionState == .connected && $0.model != nil } }

        XCTAssertEqual(events.first, .newEngineRun)
        // Hamlet had unsaved changes and no autosave newer than its file: they are lost, and said so.
        XCTAssertEqual(
            events.last,
            .reattached(failures: [], restored: [], editsLost: ["Hamlet"], editsLostWorkspaces: [hamletID]))
        XCTAssertTrue(
            before[0].logEntries.contains { $0 == LogEntry(level: .warn, message: loc("engine.reattach.editsLost")) })
        XCTAssertEqual(before[0].model?.cue(intro)?.name, "Intro", "Hamlet is its file again")
        // The same three stores, plus one for the other client's show; none made twice.
        XCTAssertEqual(s.orderedStores.count, 4, "no store made twice")
        XCTAssertTrue(
            s.orderedStores.dropFirst().map(\.store).elementsEqual(before, by: ===),
            "\(s.orderedStores.map { ($0.doc, $0.store.model?.name) })")
        // Each store shows its own show under its new id, in the new run's open order.
        XCTAssertEqual(s.orderedStores.map { $0.store.model?.name }, ["Other", "Hamlet", "A", "B"])
        XCTAssertEqual(s.orderedStores.map(\.doc), s.workspaces.map(\.doc))
    }

    /// File > Open of a show a waiting window will reopen, while the reattach is under way: the
    /// waiting window takes the doc, no second window or store is made, and none fails.
    func testOpeningAWaitingWindowsShowDuringAReattachLeavesItsWindowTheDoc() async throws {
        let (s, runs) = try await started([("Hamlet", "Demo"), ("A", "Second"), ("B", "Second")])
        let before = s.orderedStores.map(\.store)
        let bPath = try XCTUnwrap(s.workspaces.first { $0.name == "B" }?.path)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }

        tap.holdResults = true  // the reopens' answers are held: the reattach is under way
        try await runs.crash()
        try await runs.start()
        let waiting = await pump { s.detached.count == 3 && s.isReattaching }
        XCTAssertTrue(waiting)
        // The first reopen is read and answered (the answer held), so the engine takes another.
        let answered = await eventually {
            try? await runs.advance(0.01)
            return self.tap.heldCount == 1
        }
        XCTAssertTrue(answered, "the first reopen was answered")
        tap.holdResults = false
        let opened = Task { await s.open(path: bPath) }
        await assertEventually("the open was sent") {
            self.tap.sent.contains { if case .request(_, .open(bPath, _)) = $0 { return true } else { return false } }
        }
        tap.releaseResults()
        let result = await opened.value
        guard case .success = result else {
            return XCTFail("open: \(result) held \(tap.heldCount) sent \(tap.sent.suffix(6))")
        }
        let ok = await pump { self.reattached(events) }
        XCTAssertTrue(ok, "\(events)")
        guard case .reattached(let failures, _, _, _)? = events.last else { return XCTFail("\(events)") }
        XCTAssertEqual(failures, [], "no window failed")
        await assertEventually { s.stores.count == 3 }
        XCTAssertTrue(before.allSatisfy { b in s.stores.values.contains { $0 === b } }, "the same windows")
        await assertEventually { s.workspaces.count == 3 }
    }

    func testAnOldDocIdIsNeverUsedInANewRun() async throws {
        let (s, runs) = try await started([("Hamlet", "Demo")])
        let store = try XCTUnwrap(s.orderedStores.first?.store)
        let oldDoc = try XCTUnwrap(s.orderedStores.first?.doc)
        let path = try XCTUnwrap(s.workspaces.first?.path)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        try await runs.crash()
        try FileManager.default.removeItem(atPath: path)  // Hamlet cannot be reopened
        try await runs.start()
        let otherDoc = try await runs.open(try runs.show("Second", named: "Other"))
        XCTAssertEqual(otherDoc, oldDoc, "the new run gives the old doc id to another show")
        _ = await pump { self.reattached(events) }
        guard case .reattached(let failures, _, _, _)? = events.last else { return XCTFail("\(events)") }
        XCTAssertEqual(failures.map(\.name), ["Hamlet"])
        await assertEventually { s.stores[otherDoc]?.model?.name == "Other" }
        XCTAssertNotEqual(store.model?.name, "Other", "the old store never bound to the reused id")
        XCTAssertFalse(s.stores[otherDoc] === store)
        XCTAssertNil(s.doc(of: store), "its show could not be reopened: it left the session")
    }

    func testANewerAutosaveIsRestoredForADirtyWindow() async throws {
        let (s, runs) = try await started([("Hamlet", "Demo"), ("A", "Second")])
        let hamlet = try XCTUnwrap(store(named: "Hamlet", s))
        try await dirty(hamlet, "Autosaved")
        try await runs.advance(31)  // past the 30 s autosave interval
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        try await runs.crash()
        try await runs.start()
        _ = await pump { self.reattached(events) }
        XCTAssertEqual(events.last, .reattached(failures: [], restored: ["Hamlet"]))
        await assertEventually { hamlet.connectionState == .connected && hamlet.document?.dirty == true }
        XCTAssertEqual(hamlet.model?.cue(intro)?.name, "Autosaved", "the autosave, not the file")
        XCTAssertEqual(store(named: "A", s)?.document?.dirty, false, "the clean one is its file")
        await assertEventually { hamlet.logEntries.contains { $0.message == loc("engine.reattach.restored") } }
    }

    func testAnUntitledWindowComesBackFromItsAutosaveOrFails() async throws {
        let (s, runs) = try await started([])
        guard case .success(let clean) = await s.newWorkspace() else { return XCTFail() }
        guard case .success(let dirty) = await s.newWorkspace() else { return XCTFail() }
        guard case .success(let lost) = await s.newWorkspace() else { return XCTFail() }
        await assertEventually { s.stores.values.allSatisfy { $0.model != nil && $0.connectionState == .connected } }
        _ = clean
        _ = await s.stores[dirty]?.send(command: .renameWorkspace("Kept"))
        try await runs.advance(31)  // "Kept" is autosaved
        _ = await s.stores[lost]?.send(command: .renameWorkspace("Lost"))  // after the autosave: none
        await assertEventually { s.stores[dirty]?.document?.dirty == true && s.stores[lost]?.document?.dirty == true }
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        try await runs.crash()
        try await runs.start()
        _ = await pump { self.reattached(events) }
        guard case .reattached(let failures, let restored, _, _)? = events.last else { return XCTFail("\(events)") }
        XCTAssertEqual(failures.map(\.name), ["Lost"])
        XCTAssertEqual(restored, ["Kept"])
        XCTAssertEqual(s.stores.count, 2, "the clean one (new) and the recovered one")
        XCTAssertNotNil(store(named: "Kept", s))
    }

    func testAReattachCutShortResumesAndTakesWhatIsAlreadyOpen() async throws {
        let (s, runs) = try await started([("Hamlet", "Demo"), ("A", "Second")])
        let hamlet = try XCTUnwrap(store(named: "Hamlet", s))
        let hamletID = try XCTUnwrap(hamlet.model?.id)
        let path = try XCTUnwrap(s.workspaces.first { $0.name == "Hamlet" }?.path)
        try await dirty(hamlet)  // no autosave yet
        try await runs.crash()
        try await runs.start()
        // The new engine already has Hamlet open (the first reopen landed, then the link dropped).
        _ = try await runs.open(path)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        _ = await pump { self.reattached(events) }
        // #337: Hamlet's window had unsaved changes and the doc it took is the clean file.
        XCTAssertEqual(
            events.last,
            .reattached(failures: [], restored: [], editsLost: ["Hamlet"], editsLostWorkspaces: [hamletID]))
        XCTAssertEqual(s.stores.count, 2, "already_open bound the window to the open doc, no second store: \(events)")
        // The engine answers an `open` before it lists the doc it opened.
        await assertEventually("windows in the engine's order") {
            s.orderedStores.map(\.doc) == s.workspaces.map(\.doc)
        }
        await assertEventually { hamlet.logEntries.contains { $0.message == loc("engine.reattach.editsLost") } }
    }

    /// The cut-short reopen had restored the autosave (the doc is dirty): reported as restored.
    func testAReattachCutShortAfterARestoreReportsTheAutosave() async throws {
        let (s, runs) = try await started([("Hamlet", "Demo")])
        let hamlet = try XCTUnwrap(store(named: "Hamlet", s))
        let path = try XCTUnwrap(s.workspaces.first?.path)
        try await dirty(hamlet, "Autosaved")
        try await runs.advance(31)
        try await runs.crash()
        try await runs.start()
        _ = try await runs.open(path, recovery: .restore)
        var events: [SessionEvent] = []
        let sub = s.events.sink { events.append($0) }
        defer { sub.cancel() }
        _ = await pump { self.reattached(events) }
        XCTAssertEqual(events.last, .reattached(failures: [], restored: ["Hamlet"], editsLost: []))
    }
}
