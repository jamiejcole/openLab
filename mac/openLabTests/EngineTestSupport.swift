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

/// Yields to other main-actor work until `condition` holds. Frames from the real engine arrive on
/// the socket's own schedule, so after a few hundred yields it also waits a millisecond between
/// checks, up to `timeout` seconds: a bound on a hang, never a timing the test relies on (the
/// engine's clock is its stepped virtual device, which moves only on `advance`).
@MainActor
func eventually(timeout: Double = 15, _ condition: @MainActor () async -> Bool) async -> Bool {
    let deadline = ProcessInfo.processInfo.systemUptime + timeout
    var checks = 0
    while true {
        if await condition() { return true }
        checks += 1
        if checks < 300 {
            await Task.yield()
        } else {
            if ProcessInfo.processInfo.systemUptime > deadline { return false }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
    }
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

/// A client + store bound to one document (nil: the app connection) on a real `TestEngine`
/// (`Rig.open`) or on none (`Rig.offline`), with manual reconnect, handshake and request
/// schedulers and counted `engine.json` reads.
@MainActor
final class Rig {
    final class InfoBox {
        var reads = 0
        /// What `engine.json` says, for a rig on the wire stub (`init(port:token:workspace:)`).
        var port: UInt16 = 0
        var token = ""
    }

    /// The real engine this rig is connected to (`Rig.open`; nil for `Rig.offline`).
    private(set) var realEngine: TestEngine?
    let scheduler = ManualScheduler()
    let handshakeScheduler = ManualScheduler()
    /// Request deadlines (M15): fire it to time out what is in flight.
    let requestScheduler = ManualScheduler()
    let box = InfoBox()
    /// What this rig's client sent the engine (`Rig.open`, `init(engine:workspace:)`).
    let wire = WireTap()
    let client: EngineClient
    let store: WorkspaceStore

    /// Bound to `workspace` on `engine` (nil: the app connection); reads its `engine.json` afresh
    /// for every connection, as the app does.
    init(engine real: TestEngine, workspace: ModelID?) {
        realEngine = real
        let box = box
        let url = real.infoURL
        client = EngineClient(
            transport: wire,
            infoProvider: {
                box.reads += 1
                return EngineConnectionInfo.load(from: url)
            },
            scheduler: scheduler, handshakeScheduler: handshakeScheduler, requestScheduler: requestScheduler,
            workspace: workspace)
        store = WorkspaceStore(client: client)
    }

    /// Bound to `workspace` on the wire stub (EngineClientTests' `StubEngine`) at `port`; a test
    /// points `box.port` elsewhere to make the engine go away or come back.
    init(port: UInt16, token: String, workspace: ModelID?) {
        box.port = port
        box.token = token
        let box = box
        client = EngineClient(
            transport: WebSocketTransport(),
            infoProvider: {
                box.reads += 1
                return .success(EngineConnectionInfo(port: box.port, token: box.token, pid: 1))
            },
            scheduler: scheduler, handshakeScheduler: handshakeScheduler, requestScheduler: requestScheduler,
            workspace: workspace)
        store = WorkspaceStore(client: client)
    }

    /// A client that never connects (`Rig.offline`).
    fileprivate init(offlineDoc: ModelID) {
        client = EngineClient(
            transport: WebSocketTransport(), infoProvider: { .failure(.missing) }, scheduler: scheduler,
            handshakeScheduler: handshakeScheduler, requestScheduler: requestScheduler, workspace: offlineDoc)
        store = WorkspaceStore(client: client)
    }

    /// The document this rig is bound to.
    var doc: ModelID { client.workspace ?? "" }

    func connect() async -> Bool {
        client.start()
        return await eventually { self.client.state == .connected }
    }
}

/// The real engine for the unit tests: the app's bundled `openlab-engine` on its stepped virtual
/// audio device (docs/control.md "Virtual device"), in a temporary Application Support folder,
/// ending with the test process (`--watch-stdin`). Device time moves only on `advance`, so what a
/// test sees is the same every run.
///
/// One engine per test class: `shared()` starts it on first use and it is stopped when the class's
/// tests are done. Each test opens fresh copies of the test shows (`mac/TestShows`, see
/// `open(_:)`); the documents a test opened are closed when it ends.
@MainActor
final class TestEngine {
    /// The virtual device's rate: frames per second of device time.
    static let rate = 48_000.0

    let folder: URL
    var support: URL { folder.appendingPathComponent("support", isDirectory: true) }
    var infoURL: URL { support.appendingPathComponent("openLab/engine.json") }
    private(set) var info: EngineConnectionInfo
    private let process: Process
    private let stdin: Pipe
    /// An app connection for the app-scope requests the tests make (`open`, `advance`, `close`).
    let app: EngineClient
    private var openedThisTest: [ModelID] = []

    private static var current: TestEngine?
    private static var observing = false

    private init(folder: URL, process: Process, stdin: Pipe, info: EngineConnectionInfo, app: EngineClient) {
        self.folder = folder
        self.process = process
        self.stdin = stdin
        self.info = info
        self.app = app
    }

    /// The running test class's engine.
    static func shared() async throws -> TestEngine {
        if let current, current.process.isRunning { return current }
        current?.stop()
        if !observing {
            observing = true
            XCTestObservationCenter.shared.addTestObserver(EngineTestObserver())
        }
        let engine = try await start()
        current = engine
        return engine
    }

    /// A new engine of its own (a test that ends or kills it must not share it).
    static func start() async throws -> TestEngine {
        // `make test-mac-unit` (every app PR) runs the engine-free tests only (scripts/paths.sh).
        if ProcessInfo.processInfo.environment["OPENLAB_ENGINE_TESTS"] == "skip" {
            throw XCTSkip("real-engine tests are off in this run (make test-mac-unit)")
        }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(
            "openlab-unit-\(UUID().uuidString)", isDirectory: true)
        let support = folder.appendingPathComponent("support", isDirectory: true)
        try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        let executable = try XCTUnwrap(
            Bundle.main.url(forAuxiliaryExecutable: SystemEngineProcesses.engineName), "the app bundles the engine")
        let process = Process()
        process.executableURL = executable
        process.arguments = TestEngineOptions(clock: .stepped, appSupport: support).engineArguments
        let stdin = Pipe()
        process.standardInput = stdin
        process.standardOutput = FileHandle.nullDevice
        let stderr = support.appendingPathComponent("engine-stderr.log")
        FileManager.default.createFile(atPath: stderr.path, contents: nil)
        process.standardError = try FileHandle(forWritingTo: stderr)
        try process.run()
        let infoURL = support.appendingPathComponent("openLab/engine.json")
        var info: EngineConnectionInfo?
        let deadline = ProcessInfo.processInfo.systemUptime + 20
        while info == nil {
            if case .success(let i) = EngineConnectionInfo.load(from: infoURL), i.pid == process.processIdentifier {
                info = i
            } else if !process.isRunning || ProcessInfo.processInfo.systemUptime > deadline {
                process.terminate()
                let said = (try? String(contentsOf: stderr, encoding: .utf8)) ?? ""
                throw XCTSkip("the test engine did not start: \(said)")
            } else {
                try await Task.sleep(nanoseconds: 2_000_000)
            }
        }
        let found = try XCTUnwrap(info)
        let app = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: infoURL) },
            workspace: nil)
        let engine = TestEngine(folder: folder, process: process, stdin: stdin, info: found, app: app)
        app.start()
        guard await eventually({ app.state == .connected }) else {
            engine.stop()
            throw TestEngineError.notConnected
        }
        return engine
    }

    /// Opens a fresh copy of the test show `name` (`mac/TestShows/<name>/<name>.openlab`), first
    /// changed by `edit` (given the copy's folder) when there is one, and lets the engine take in
    /// its media probes; its doc. Closed again when the test ends.
    func open(_ name: String, edit: ((URL) throws -> Void)? = nil) async throws -> ModelID {
        let copies = folder.appendingPathComponent("shows-\(UUID().uuidString)", isDirectory: true)
        let source = try XCTUnwrap(
            Bundle(for: TestEngine.self).url(forResource: "TestShows", withExtension: nil), "the test shows are bundled"
        )
        try FileManager.default.copyItem(at: source.appendingPathComponent(name), to: copies)
        try edit?(copies)
        let path = copies.appendingPathComponent("\(name).openlab").path
        guard case .success(let ok) = await app.send({ .request(id: $0, .open(path: path)) }),
            case .opened(let doc)? = ok.data
        else { throw TestEngineError.refused("open \(name)") }
        openedThisTest.append(doc)
        try await advance(frames: 0)
        return doc
    }

    /// Moves device time on by `seconds` (whole frames), or only waits for background work (0).
    func advance(seconds: Double) async throws {
        try await advance(frames: UInt64((seconds * Self.rate).rounded()))
    }

    /// An app-scope request on the harness's own app connection (`lose_device`, `list_devices`, ...).
    func request(_ request: Request) async -> Result<OkBody, EngineError> {
        await app.send({ .request(id: $0, request) })
    }

    func advance(frames: UInt64) async throws {
        switch await app.send({ .request(id: $0, .advance(frames: frames)) }) {
        case .success: return
        case .failure(let e): throw TestEngineError.refused("advance: \(e)")
        }
    }

    /// The test is over: its documents are closed, unsaved changes discarded.
    fileprivate func closeTestDocuments() async {
        let docs = openedThisTest
        openedThisTest = []
        for doc in docs {
            _ = await app.send({ .close(id: $0, doc: doc, discardChanges: true) })
        }
    }

    /// Ends the engine as a crash does: SIGKILL, its `engine.json` left behind.
    func kill() {
        if process.isRunning { Darwin.kill(process.processIdentifier, SIGKILL) }
        process.waitUntilExit()
    }

    /// Ends it: stdin closes (`--watch-stdin`) and it shuts down; killed if it has not within 20 s.
    func stop() {
        app.stop()
        try? stdin.fileHandleForWriting.close()
        let deadline = ProcessInfo.processInfo.systemUptime + 20
        while process.isRunning && ProcessInfo.processInfo.systemUptime < deadline { usleep(5_000) }
        if process.isRunning { kill() }
        try? FileManager.default.removeItem(at: folder)
        if Self.current === self { Self.current = nil }
    }

    /// The engine's show log so far (for a failure message).
    var logText: String {
        (try? String(contentsOf: support.appendingPathComponent("openLab/Logs/openlab.log"), encoding: .utf8)) ?? ""
    }

    fileprivate static func testEnded() async { await current?.closeTestDocuments() }
    fileprivate static func classEnded() { current?.stop() }
}

enum TestEngineError: Error {
    case notConnected
    case refused(String)
}

/// Closes each test's documents and stops a class's engine when its tests are done.
private final class EngineTestObserver: NSObject, XCTestObservation {
    func testCaseDidFinish(_ testCase: XCTestCase) {
        MainActor.assumeIsolated {
            let done = XCTestExpectation(description: "documents closed")
            Task { @MainActor in
                await TestEngine.testEnded()
                done.fulfill()
            }
            _ = XCTWaiter().wait(for: [done], timeout: 20)
        }
    }

    func testSuiteDidFinish(_ testSuite: XCTestSuite) {
        MainActor.assumeIsolated { TestEngine.classEnded() }
    }
}

/// Engine runs, one after another, in one Application Support folder: the bundled engine on its
/// stepped virtual device started the way the app starts it (`SystemEngineProcesses`), so a crash
/// (SIGKILL) leaves its `engine.json` and autosaves for the next run. Every run is ended by
/// `endAll`.
@MainActor
final class EngineRuns {
    let folder: URL
    var support: URL { folder.appendingPathComponent("support", isDirectory: true) }
    var infoURL: URL { support.appendingPathComponent("openLab/engine.json") }
    private var runs: [(processes: SystemEngineProcesses, pid: Int32)] = []
    /// An app connection to the current run, for what another client does.
    private(set) var app: EngineClient
    private var bound: [EngineClient] = []

    init() throws {
        if ProcessInfo.processInfo.environment["OPENLAB_ENGINE_TESTS"] == "skip" {
            throw XCTSkip("real-engine tests are off in this run (make test-mac-unit)")
        }
        folder = FileManager.default.temporaryDirectory.appendingPathComponent(
            "openlab-runs-\(UUID().uuidString)", isDirectory: true)
        app = EngineClient(transport: WebSocketTransport(), infoProvider: { .failure(.missing) }, workspace: nil)
        try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
    }

    /// Starts a new run (a new token) and connects `app` to it.
    func start() async throws {
        let processes = SystemEngineProcesses(
            infoURL: infoURL, watchStderr: false,
            arguments: TestEngineOptions(clock: .stepped, appSupport: support).engineArguments, watchStdin: true)
        let pid = try processes.spawnEngine()
        runs.append((processes, pid))
        let found = await eventually {
            if case .success(let i) = processes.readInfo() { return i.pid == pid }
            return false
        }
        guard found else { throw TestEngineError.notConnected }
        app.stop()
        let url = infoURL
        app = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) }, workspace: nil)
        app.start()
        guard await eventually({ self.app.state == .connected }) else { throw TestEngineError.notConnected }
    }

    /// Ends the current run as a crash does: SIGKILL, its files left behind.
    func crash() async throws {
        guard let run = runs.last else { return }
        app.stop()
        for c in bound { c.stop() }
        bound = []
        run.processes.signal(run.pid, .kill)
        guard await eventually({ !run.processes.isEngine(run.pid) }) else {
            throw TestEngineError.refused("the engine did not end")
        }
    }

    func endAll() {
        app.stop()
        for c in bound { c.stop() }
        for run in runs where run.processes.isEngine(run.pid) {
            run.processes.signal(run.pid, .kill)
            let deadline = ProcessInfo.processInfo.systemUptime + 10
            while run.processes.isEngine(run.pid) && ProcessInfo.processInfo.systemUptime < deadline {
                usleep(5_000)
            }
        }
        try? FileManager.default.removeItem(at: folder)
    }

    /// Opens `path` on the current run as another client would; its doc.
    func open(_ path: String, recovery: RecoveryChoice? = nil) async throws -> ModelID {
        guard case .success(let ok) = await app.send({ .request(id: $0, .open(path: path, recovery: recovery)) }),
            case .opened(let doc)? = ok.data
        else { throw TestEngineError.refused("open \(path)") }
        return doc
    }

    /// Renames a cue of `doc` as another client would.
    func rename(_ cue: ModelID, in doc: ModelID, to name: String) async throws {
        let url = infoURL
        let client = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) }, workspace: doc)
        bound.append(client)
        client.start()
        guard await eventually({ client.state == .connected }),
            case .success = await client.send({ .command(id: $0, .setProperty(cue, .name(name))) })
        else { throw TestEngineError.refused("rename") }
    }

    /// Moves the current run's device time on.
    func advance(_ seconds: Double) async throws {
        let frames = UInt64((seconds * TestEngine.rate).rounded())
        guard case .success = await app.send({ .request(id: $0, .advance(frames: frames)) }) else {
            throw TestEngineError.refused("advance")
        }
    }

    /// A copy of the test show `template` as `<name>/<name>.openlab` (its media beside it), the
    /// workspace named `name` with an id of its own; its path.
    func show(_ template: String, named name: String) throws -> String {
        let source = try XCTUnwrap(
            Bundle(for: TestEngine.self).url(forResource: "TestShows", withExtension: nil)?.appendingPathComponent(
                template))
        let dir = folder.appendingPathComponent("shows/\(name)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: dir.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.copyItem(at: source, to: dir)
        let original = dir.appendingPathComponent("\(template).openlab")
        var file = try XCTUnwrap(JSONValue.parse(try String(contentsOf: original, encoding: .utf8)))
        file["workspace"]?.set("name", .string(name))
        file["workspace"]?.set("id", .string(UUID().uuidString.lowercased()))
        try FileManager.default.removeItem(at: original)
        let path = dir.appendingPathComponent("\(name).openlab")
        try file.text().write(to: path, atomically: true, encoding: .utf8)
        return path.path
    }
}

extension Rig {
    /// No engine at all: a store that has received a `welcome` with `snapshot` (default: the golden
    /// welcome the Rust tests write, engine/fixtures/protocol/server/welcome.json), for logic that
    /// only reads and applies what the engine sends. Its client never connects: anything it sends
    /// fails with `notConnected`.
    static func offline(_ snapshot: JSONValue? = nil) throws -> Rig {
        let snap = try snapshot ?? XCTUnwrap(Fixtures.json("server", "welcome")["snapshot"])
        let rig = Rig(offlineDoc: "1")
        let welcome: JSONValue = .object([
            "type": .string("welcome"), "protocol_version": .int(Int64(protocolVersion)), "snapshot": snap,
        ])
        XCTAssertEqual(rig.store.receive(try ServerMessage.decode(try welcome.text())), .applied)
        return rig
    }

    /// A client + store bound to a fresh copy of the test show `show` on the class's real engine
    /// (nil `show`: the app connection), connected, with manual reconnect and request schedulers.
    /// `engine`: one of the test's own (`TestEngine.start()`, for a test that kills or stops it).
    /// `edit` changes the copy (its folder) before it opens.
    static func open(
        _ show: String? = "Demo", on own: TestEngine? = nil, edit: ((URL) throws -> Void)? = nil
    ) async throws -> Rig {
        let engine: TestEngine
        if let own { engine = own } else { engine = try await TestEngine.shared() }
        var doc: ModelID?
        if let show { doc = try await engine.open(show, edit: edit) }
        let rig = Rig(engine: engine, workspace: doc)
        guard await rig.connect() else { throw TestEngineError.notConnected }
        return rig
    }

    var testEngine: TestEngine { realEngine! }

    /// Moves the engine's device time on by `seconds` (0: only waits for background work), then
    /// waits until this client has what the engine sent meanwhile (a request on its own
    /// connection is answered after the broadcasts before it).
    func advance(_ seconds: Double = 0) async throws {
        try await testEngine.advance(seconds: seconds)
        await sync()
    }

    /// Returns once every frame the engine sent this connection before now has been applied.
    func sync() async {
        _ = await client.send({ .request(id: $0, .listPatchMappings) })
    }
}

/// The real engine's connections as the app opens them (every `Rig` on an engine uses one), with
/// what the app sent on each (the order of messages is part of several flows: an edit before a
/// close, everything before `goodbye`) and the wire faults the real engine cannot be asked for: results held or made
/// unreadable, a flush that never returns, the connections dropped. No cue model: every frame the
/// app reads came from the engine.
@MainActor
final class WireTap: EngineTransport {
    private let inner = WebSocketTransport()
    /// Every message the app sent, with the doc of the connection it went on (nil: an app connection).
    private(set) var sentOn: [(doc: ModelID?, message: ClientMessage)] = []
    /// Every message the app sent, in order.
    var sent: [ClientMessage] { sentOn.map(\.message) }
    /// The commands sent, in order.
    var commands: [Command] { sent.compactMap { if case .command(_, let c, _) = $0 { return c } else { return nil } } }
    var holdResults = false
    var garbleResults = false
    var holdFlush = false
    fileprivate var connections: [TapConnection] = []
    fileprivate var held: [(TapConnection, String)] = []

    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection {
        let c = TapConnection(inner: try await inner.connect(info: info), tap: self)
        connections.append(c)
        return c
    }

    fileprivate func record(_ text: String, from c: TapConnection) {
        guard let m = try? JSONDecoder().decode(ClientMessage.self, from: Data(text.utf8)) else { return }
        if case .hello(let h) = m { c.doc = h.workspace }
        sentOn.append((c.doc, m))
    }

    /// Ends every connection as a network drop would; the engine and its documents stay.
    func dropConnections() {
        for c in connections { c.close() }
        connections = []
    }

    /// Delivers the held results, in the order they came or (`reversed`) newest first.
    func releaseResults(reversed: Bool = false) {
        let all = reversed ? held.reversed() : held
        held = []
        for (c, text) in all { c.deliver(text) }
    }

    /// How many results are held (`holdResults`).
    var heldCount: Int { held.count }

    /// Forgets what was sent so far (the test's own setup).
    func forgetSent() { sentOn = [] }

    /// How many `request`s of this kind were sent.
    func requestCount(_ matches: (Request) -> Bool) -> Int {
        sent.filter { if case .request(_, let r) = $0 { return matches(r) } else { return false } }.count
    }
}

@MainActor
final class TapConnection: EngineConnection {
    private let inner: any EngineConnection
    private unowned let tap: WireTap
    fileprivate var doc: ModelID?
    private var queue: [String] = []
    private var failure: Error?
    private var waiter: CheckedContinuation<String, Error>?
    private var reader: Task<Void, Never>?

    fileprivate init(inner: any EngineConnection, tap: WireTap) {
        self.inner = inner
        self.tap = tap
        reader = Task { [weak self] in
            while true {
                do {
                    let text = try await inner.receive()
                    self?.arrived(text)
                } catch {
                    self?.fail(error)
                    return
                }
            }
        }
    }

    private func arrived(_ text: String) {
        guard var v = JSONValue.parse(text), v["type"]?.stringValue == "result" else { return deliver(text) }
        if tap.garbleResults, var o = v.objectValue {
            o["error"] = nil
            o["ok"] = .string("garbled")
            v = .object(o)
            return deliver((try? v.text()) ?? text)
        }
        if tap.holdResults { return tap.held.append((self, text)) }
        deliver(text)
    }

    fileprivate func deliver(_ text: String) {
        guard failure == nil else { return }
        if let w = waiter {
            waiter = nil
            w.resume(returning: text)
        } else {
            queue.append(text)
        }
    }

    private func fail(_ error: Error) {
        guard failure == nil else { return }
        failure = error
        if queue.isEmpty, let w = waiter {
            waiter = nil
            w.resume(throwing: error)
        }
    }

    func send(_ text: String) {
        tap.record(text, from: self)
        inner.send(text)
    }

    func sendAndFlush(_ text: String) async {
        tap.record(text, from: self)
        if tap.holdFlush {
            inner.send(text)
            await withCheckedContinuation { (_: CheckedContinuation<Void, Never>) in }
        }
        await inner.sendAndFlush(text)
    }

    func receive() async throws -> String {
        if !queue.isEmpty { return queue.removeFirst() }
        if let failure { throw failure }
        return try await withCheckedThrowingContinuation { waiter = $0 }
    }

    func close() {
        inner.close()
        fail(URLError(.networkConnectionLost))
    }
}

/// A transport whose first connection reports its close late (as `URLSessionWebSocketTask` does:
/// cancellation reaches `receive` asynchronously). `releaseLate()` lets the old receive throw.
@MainActor
final class LateCloseTransport: EngineTransport {
    let inner: any EngineTransport
    private var gates: [CheckedContinuation<Void, Never>] = []
    private var made = 0

    init(inner: any EngineTransport) { self.inner = inner }

    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection {
        let inner = try await self.inner.connect(info: info)
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

/// Snapshots for the engine-free tests (`Rig.offline`): the test shows' workspaces as the engine
/// sends them (the wire's `workspace` is the show file's), and builders for cues and large lists.
enum TestSnapshots {
    static let mainList: ModelID = "00000000-0000-0000-0000-000000000001"
    // The demo show's cues (mac/TestShows/Demo/Demo.openlab).
    static let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
    static let fade: ModelID = "00000000-0000-0000-0000-00000000000b"
    static let group: ModelID = "00000000-0000-0000-0000-000000000020"
    static let door: ModelID = "00000000-0000-0000-0000-000000000021"
    static let rain: ModelID = "00000000-0000-0000-0000-000000000022"
    static let radio: ModelID = "00000000-0000-0000-0000-000000000023"

    private final class Anchor {}

    /// The test show `name` as a just-opened workspace: nothing running, no problems known yet (the
    /// engine adds those; a test that needs them sends a `runtime` change).
    static func show(_ name: String = "Demo") throws -> JSONValue {
        let url = try XCTUnwrap(
            Bundle(for: Anchor.self).url(forResource: "TestShows", withExtension: nil)?.appendingPathComponent(
                "\(name)/\(name).openlab"))
        let file = try XCTUnwrap(JSONValue.parse(try String(contentsOf: url, encoding: .utf8)))
        var s = emptySnapshot(name: name)
        s.set("workspace", try XCTUnwrap(file["workspace"]))
        return s
    }

    /// A cue as the engine sends it (`kind` is the object under `kind`).
    static func cueJSON(
        id: String, number: String?, name: String, kind: JSONValue, color: String = "none", flagged: Bool = false,
        armed: Bool = true, preWait: Double = 0, postWait: Double = 0, continueMode: String = "do_not_continue"
    ) -> JSONValue {
        .object([
            "id": .string(id), "number": number.map(JSONValue.string) ?? .null, "name": .string(name),
            "notes": .string(""), "color": .string(color), "flagged": .bool(flagged), "armed": .bool(armed),
            "pre_wait": .double(preWait), "post_wait": .double(postWait), "continue_mode": .string(continueMode),
            "kind": kind,
        ])
    }

    /// An audio cue's `kind` with every field the engine sends, so message sizes are realistic.
    static func audioKind(_ media: String) -> JSONValue {
        let fade = JSONValue.object(["duration": .double(0), "curve": .string("linear")])
        let output = JSONValue.object(["db": .double(0), "mute": .bool(false)])
        return .object([
            "type": .string("audio"), "media": .string(media),
            "patch": .string("00000000-0000-0000-0000-000000000001"), "start_time": .double(0), "end_time": .null,
            "fade_in": fade, "fade_out": fade, "slices": .array([]), "last_slice_play_count": .int(1),
            "levels": .object([
                "master_db": .double(0), "outputs": .array([output, output]), "crosspoints": .array([]),
                "gangs": .array([]),
            ]),
            "preload": .bool(false),
        ])
    }

    static func groupKind(_ children: [JSONValue]) -> JSONValue {
        .object([
            "type": .string("group"), "mode": .object(["type": .string("timeline")]), "children": .array(children),
        ])
    }

    static func emptySnapshot(name: String, workspaceID: String = "00000000-0000-0000-0000-000000000001") -> JSONValue {
        JSONValue.parse(
            """
            {"revision":1,"runtime_seq":0,
             "document":{"path":null,"dirty":false,"can_undo":false,"can_redo":false},
             "workspace":{"id":"\(workspaceID)","name":"\(name)","settings":{},"patches":[],"network_patches":[],
               "cue_lists":[{"id":"00000000-0000-0000-0000-000000000001","name":"Main","cues":[]}]},
             "cue_states":[],"problems":[],"group_warnings":[],"playheads":[],"previewing":[],"warnings":[]}
            """) ?? .null
    }

    /// `count` cues in all: flat cues with a group of ten children after every thirty top-level cues.
    static func bigSnapshot(cues count: Int) -> JSONValue {
        var s = emptySnapshot(name: "Big")
        var top: [JSONValue] = []
        var made = 0
        func next(_ name: String) -> JSONValue {
            made += 1
            return cueJSON(
                id: String(format: "00000000-0000-0000-0001-%012d", made), number: "\(made)", name: "\(name) \(made)",
                kind: audioKind("media/\(made).wav"), color: made % 7 == 0 ? "red" : "none", flagged: made % 11 == 0,
                postWait: made % 5 == 0 ? 1.5 : 0)
        }
        while made < count {
            if top.count % 31 == 30 && made + 11 <= count {
                let kids = (0..<10).map { _ in next("Child") }
                made += 1
                top.append(
                    cueJSON(
                        id: String(format: "00000000-0000-0000-0001-%012d", made), number: "\(made)",
                        name: "Group \(made)", kind: groupKind(kids)))
            } else {
                top.append(next("Cue"))
            }
        }
        s["workspace"]?.set(
            "cue_lists", .array([.object(["id": .string(mainList.raw), "name": .string("Main"), "cues": .array(top)])]))
        return s
    }
}
