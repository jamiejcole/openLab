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

/// A client + store wired to a `FakeEngine`, with a manual scheduler and counted info reads.
@MainActor
final class Rig {
    final class InfoBox {
        var reads = 0
        var port: UInt16 = 1
        var token = ""
    }

    let engine: FakeEngine!
    /// The real engine this rig is connected to (`Rig.open`).
    private(set) var realEngine: TestEngine?
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

    /// Bound to `workspace` on `engine` (nil: the app connection); reads its `engine.json` afresh
    /// for every connection, as the app does.
    init(engine real: TestEngine, workspace: ModelID?) {
        engine = nil
        realEngine = real
        let box = box
        let url = real.infoURL
        client = EngineClient(
            transport: WebSocketTransport(),
            infoProvider: {
                box.reads += 1
                return EngineConnectionInfo.load(from: url)
            },
            scheduler: scheduler, handshakeScheduler: handshakeScheduler, requestScheduler: requestScheduler,
            workspace: workspace)
        store = WorkspaceStore(client: client)
    }

    /// A client that never connects (`Rig.offline`).
    fileprivate init(offlineDoc: ModelID) {
        engine = nil
        client = EngineClient(
            transport: WebSocketTransport(), infoProvider: { .failure(.missing) }, scheduler: scheduler,
            handshakeScheduler: handshakeScheduler, requestScheduler: requestScheduler, workspace: offlineDoc)
        store = WorkspaceStore(client: client)
    }

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

    /// Opens a fresh copy of the test show `name` (`mac/TestShows/<name>/<name>.openlab`) and lets
    /// the engine take in its media probes; its doc. Closed again when the test ends.
    func open(_ name: String) async throws -> ModelID {
        let copies = folder.appendingPathComponent("shows-\(UUID().uuidString)", isDirectory: true)
        let source = try XCTUnwrap(
            Bundle(for: TestEngine.self).url(forResource: "TestShows", withExtension: nil), "the test shows are bundled")
        try FileManager.default.copyItem(at: source.appendingPathComponent(name), to: copies)
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
    static func open(_ show: String? = "Demo") async throws -> Rig {
        let engine = try await TestEngine.shared()
        var doc: ModelID?
        if let show { doc = try await engine.open(show) }
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
