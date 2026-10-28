// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Reconnect backoff without real time: `sleep` suspends until a test calls `fire()`.
actor ManualScheduler: Scheduler {
    private(set) var requested: [Double] = []
    private var waiters: [CheckedContinuation<Void, Error>] = []

    func sleep(seconds: Double) async throws {
        requested.append(seconds)
        try await withCheckedThrowingContinuation { waiters.append($0) }
    }

    var waitingCount: Int { waiters.count }

    func fire() {
        let all = waiters
        waiters = []
        for w in all { w.resume() }
    }
}

/// The golden protocol files written by the Rust tests (engine/fixtures/protocol).
enum Fixtures {
    private final class Anchor {}
    static let dir: URL =
        Bundle(for: Anchor.self).resourceURL?.appendingPathComponent("protocol")
        ?? URL(fileURLWithPath: "/nonexistent")

    static func files(_ direction: String) throws -> [URL] {
        try FileManager.default.contentsOfDirectory(
            at: dir.appendingPathComponent(direction), includingPropertiesForKeys: nil
        ).filter { $0.pathExtension == "json" }.sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    static func text(_ direction: String, _ name: String) throws -> String {
        try String(contentsOf: dir.appendingPathComponent("\(direction)/\(name).json"), encoding: .utf8)
    }

    static func json(_ direction: String, _ name: String) throws -> JSONValue {
        try XCTUnwrap(JSONValue.parse(try text(direction, name)))
    }
}

/// Yields to other main-actor work until `condition` holds (no sleeping, no wall clock).
@MainActor
func eventually(_ condition: @MainActor () async -> Bool) async -> Bool {
    for _ in 0..<5000 {
        if await condition() { return true }
        await Task.yield()
    }
    return false
}

@MainActor
func assertEventually(
    _ message: String = "never became true", file: StaticString = #filePath, line: UInt = #line,
    _ condition: @MainActor () async -> Bool
) async {
    let ok = await eventually(condition)
    XCTAssertTrue(ok, message, file: file, line: line)
}

/// Lets queued work run; for asserting that something did NOT happen.
@MainActor
func settle() async {
    for _ in 0..<200 { await Task.yield() }
}

/// A client + store wired to a `FakeEngine`, with a manual scheduler and counted info reads.
@MainActor
final class Rig {
    final class InfoBox {
        var reads = 0
        var port: UInt16 = 1
        var token = ""
    }

    let engine: FakeEngine
    let scheduler = ManualScheduler()
    let handshakeScheduler = ManualScheduler()
    /// Request deadlines (M15): fire it to time out what is in flight.
    let requestScheduler = ManualScheduler()
    let box = InfoBox()
    let client: EngineClient
    let store: WorkspaceStore

    /// Bound to the fake's first doc ("1") unless `workspace` says otherwise (nil: the app connection).
    init(workspace: ModelID? = "1", snapshot: JSONValue? = nil) throws {
        let snap = try snapshot ?? Fixtures.json("server", "welcome")["snapshot"]
        engine = FakeEngine(snapshot: snap)
        box.token = engine.token
        let box = box
        client = EngineClient(
            transport: engine,
            infoProvider: {
                box.reads += 1
                return .success(EngineConnectionInfo(port: box.port, token: box.token, pid: 1))
            },
            scheduler: scheduler, handshakeScheduler: handshakeScheduler, requestScheduler: requestScheduler,
            workspace: workspace)
        store = WorkspaceStore(client: client)
    }

    func connect() async -> Bool {
        client.start()
        return await eventually { self.client.state == .connected }
    }
}

/// A transport whose first connection reports its close late (as `URLSessionWebSocketTask` does:
/// cancellation reaches `receive` asynchronously). `releaseLate()` lets the old receive throw.
@MainActor
final class LateCloseTransport: EngineTransport {
    let engine: FakeEngine
    private var gates: [CheckedContinuation<Void, Never>] = []
    private var made = 0

    init(engine: FakeEngine) { self.engine = engine }

    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection {
        let inner = try await engine.connect(info: info)
        made += 1
        return LateConnection(inner: inner, late: made == 1, transport: self)
    }

    fileprivate func wait() async { await withCheckedContinuation { gates.append($0) } }

    func releaseLate() {
        let all = gates
        gates = []
        for g in all { g.resume() }
    }
}

@MainActor
private final class LateConnection: EngineConnection {
    let inner: any EngineConnection
    let late: Bool
    unowned let transport: LateCloseTransport
    init(inner: any EngineConnection, late: Bool, transport: LateCloseTransport) {
        self.inner = inner
        self.late = late
        self.transport = transport
    }
    func send(_ text: String) { inner.send(text) }
    func close() { inner.close() }
    func receive() async throws -> String {
        do { return try await inner.receive() } catch {
            if late { await transport.wait() }
            throw error
        }
    }
}
