// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
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
func eventually(
    timeout: Double = 15, now: @MainActor () -> Double = { ProcessInfo.processInfo.systemUptime },
    _ condition: @MainActor () async -> Bool
) async -> Bool {
    let deadline = now() + timeout
    var checks = 0
    while true {
        if await condition() { return true }
        checks += 1
        if checks < 300 {
            await Task.yield()
        } else {
            if now() > deadline {
                // A main-thread stall can carry the clock past the deadline before queued
                // main-actor work (an engine message) has run; give it a turn, then look once more.
                for _ in 0..<50 { await Task.yield() }
                try? await Task.sleep(nanoseconds: 20_000_000)
                for _ in 0..<50 { await Task.yield() }
                return await condition()
            }
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
/// audio device, in a temporary Application Support folder,
/// ending with the test process (`--watch-stdin`). Device time moves only on `advance`, so what a
/// test sees is the same every run.
///
/// One engine per test class: `shared()` starts it on first use and it is stopped when the class's
/// tests are done. Each test opens fresh copies of the test shows (`mac/TestShows`, see
/// `open(_:)`); the documents a test opened are closed when it ends.
///
/// A test that needs an engine of its own (`start`) gets a new one every time, never one another
/// test used. Starting one is the engine's own start-up, which the test would wait for: so while a
/// test runs, the next engine of the same kind starts beside it (`spares`), ready for the next
/// `start`, and an engine let go with `stopInBackground` ends off the main thread.
@MainActor
final class TestEngine {
    /// The virtual device's rate: frames per second of device time.
    static let rate = 48_000.0
    /// The engine's virtual output devices, as `list_devices` gives them: the first is listed from
    /// the start and is the system's output; the second only once plugged in (`restoreDevice`).
    static let deviceIDs = ["openlab:virtual", "openlab:virtual-2"]
    static let deviceNames = ["openLab Virtual Output", "openLab Virtual Output 2"]

    let folder: URL
    var support: URL { folder.appendingPathComponent("support", isDirectory: true) }
    var infoURL: URL { support.appendingPathComponent("openLab/engine.json") }
    private(set) var info: EngineConnectionInfo
    private let process: Process
    private let stdin: Pipe
    /// The engine's process id (a test checks that it ended).
    var pid: Int32 { process.processIdentifier }
    /// An app connection for the app-scope requests the tests make (`open`, `advance`, `close`).
    let app: EngineClient
    private var openedThisTest: [ModelID] = []

    private static var current: TestEngine?
    /// An engine started ahead for the next `start` of the same `displays` ("" for none) with no
    /// settings: never used before it is handed out. Ended when the class's tests are done.
    private static var spares: [String: Task<TestEngine, Error>] = [:]
    /// Engines being ended off the main thread (`stopInBackground`).
    private static let ending = DispatchGroup()

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
        try skipIfOff()
        // One per class: an engine started ahead for it would only be ended unused.
        let engine = try await launch(displays: nil, settings: nil)
        current = engine
        return engine
    }

    /// A new engine of its own (a test that ends or kills it must not share it). `displays`
    /// (`ID:WxH,...`) gives it a stage host on virtual displays, drawn offscreen; `settings` is
    /// this machine's `settings.json` as it finds it at start.
    static func start(displays: String? = nil, settings: Data? = nil) async throws -> TestEngine {
        try skipIfOff()
        guard settings == nil else { return try await launch(displays: displays, settings: settings) }
        let kind = displays ?? ""
        let spare = spares.removeValue(forKey: kind)
        spares[kind] = Task { try await launch(displays: displays, settings: nil) }
        if let ready = try? await spare?.value {
            if ready.process.isRunning && ready.app.state == .connected { return ready }
            ready.stop()
        }
        return try await launch(displays: displays, settings: nil)
    }

    /// `make test-mac-unit` (every app PR) runs the engine-free tests only (scripts/paths.sh).
    private static func skipIfOff() throws {
        if ProcessInfo.processInfo.environment["OPENLAB_ENGINE_TESTS"] == "skip" {
            throw XCTSkip("real-engine tests are off in this run (make test-mac-unit)")
        }
    }

    /// Ends the engines started ahead (`spares`): the class that would have used them is done.
    fileprivate static func endSpares() async {
        let waiting = spares.values
        spares = [:]
        for spare in waiting { (try? await spare.value)?.stopInBackground() }
    }

    private static func launch(displays: String?, settings: Data?) async throws -> TestEngine {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(
            "openlab-unit-\(UUID().uuidString)", isDirectory: true)
        let support = folder.appendingPathComponent("support", isDirectory: true)
        try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        if let settings {
            let dir = support.appendingPathComponent("openLab", isDirectory: true)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try settings.write(to: dir.appendingPathComponent("settings.json"))
        }
        let executable = try XCTUnwrap(
            Bundle.main.url(forAuxiliaryExecutable: SystemEngineProcesses.engineName), "the app bundles the engine")
        let process = Process()
        process.executableURL = executable
        process.arguments =
            TestEngineOptions(clock: .stepped, appSupport: support, displays: displays).engineArguments
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
                throw TestEngineError.refused("the test engine did not start: \(said)")
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
        let source = try XCTUnwrap(Self.testShows, "the test shows are bundled")
        try FileManager.default.copyItem(at: source.appendingPathComponent(name), to: copies)
        try edit?(copies)
        return try await open(path: copies.appendingPathComponent("\(name).openlab").path)
    }

    /// The bundled test shows' folder (`mac/TestShows`).
    static var testShows: URL? { Bundle(for: TestEngine.self).url(forResource: "TestShows", withExtension: nil) }

    /// Opens the show file at `path` where it is, and lets the engine take in its media probes; its
    /// doc. Closed (unsaved changes discarded) when the test ends.
    func open(path: String) async throws -> ModelID {
        let result = await app.send({ .request(id: $0, .open(path: path)) })
        guard case .success(let ok) = result, case .opened(let doc)? = ok.data else {
            throw TestEngineError.refused("open \(path): \(result)")
        }
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
        letGo().finish()
    }

    /// Ends it as `stop` does, the wait for the process to exit and the removal of its folder done
    /// off the main thread: for a test that is over and does not look at the engine again.
    func stopInBackground() {
        let end = letGo()
        let group = Self.ending
        group.enter()
        DispatchQueue.global().async {
            end.finish()
            group.leave()
        }
    }

    /// Lets the engine go: the harness's connection closes, and stdin, so the engine shuts down.
    private func letGo() -> EngineEnd {
        app.stop()
        try? stdin.fileHandleForWriting.close()
        if Self.current === self { Self.current = nil }
        return EngineEnd(process: process, folder: folder)
    }

    /// The test bundle is done: every engine started ahead is ended, and every engine being ended is
    /// waited for, so no engine or folder outlives the run.
    fileprivate static func bundleEnded() async {
        await endSpares()
        let group = ending
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            DispatchQueue.global().async {
                _ = group.wait(timeout: .now() + 30)
                done.resume()
            }
        }
    }

    /// The engine's show log so far (for a failure message).
    var logText: String {
        (try? String(contentsOf: support.appendingPathComponent("openLab/Logs/openlab.log"), encoding: .utf8)) ?? ""
    }

    fileprivate static func testEnded() async { await current?.closeTestDocuments() }
    fileprivate static func classEnded() async {
        current?.stop()
        await endSpares()
    }
}

/// An engine process let go: waited for (killed if it has not ended within 20 s), then its folder
/// removed. Touched by one thread at a time.
private final class EngineEnd: @unchecked Sendable {
    private let process: Process
    private let folder: URL

    init(process: Process, folder: URL) {
        self.process = process
        self.folder = folder
    }

    func finish() {
        let deadline = ProcessInfo.processInfo.systemUptime + 20
        while process.isRunning && ProcessInfo.processInfo.systemUptime < deadline { usleep(5_000) }
        if process.isRunning {
            Darwin.kill(process.processIdentifier, SIGKILL)
            process.waitUntilExit()
        }
        try? FileManager.default.removeItem(at: folder)
    }
}

enum TestEngineError: Error {
    case notConnected
    case refused(String)
}

/// Runs between tests, for every test of the bundle (it is the bundle's principal class, so XCTest
/// makes it before the first test): closes each test's documents, stops a class's engine when its
/// tests are done, and fails when a test left a window alive. XCTest calls it on the main thread only.
///
/// A window that outlives its test keeps its window-server surface, so leaked windows piled up
/// over a test process (about 250 and 2.7 GB in the second one, #716) until the test machine's
/// window server stalled. XCTest keeps every test case object to the end of the run, so a window
/// in a test's property is kept unless its tearDown lets it go.
@objc(OpenLabTestObserver)
final class EngineTestObserver: NSObject, XCTestObservation, @unchecked Sendable {
    /// The windows that were there when the running test started.
    private var windowsBefore: Set<ObjectIdentifier> = []
    /// Windows finished tests left alive, not yet given time to go (`checkWindows`).
    @MainActor private var suspects: [Suspect] = []
    /// Leaks found after a test: a finished test's run takes no more failures, so they fail the
    /// next test to start, naming the test that left the window.
    private var leaks: [String] = []

    override init() {
        super.init()
        // Window open and close animations hold their window until they end, and in a test host
        // that is not on screen they can hold it for many tests: a closed window would look leaked.
        UserDefaults.standard.register(defaults: ["NSAutomaticWindowAnimationsEnabled": false])
        XCTestObservationCenter.shared.addTestObserver(self)
    }

    func testCaseWillStart(_ testCase: XCTestCase) {
        for leak in leaks { testCase.record(XCTIssue(type: .assertionFailure, compactDescription: leak)) }
        leaks = []
        MainActor.assumeIsolated { windowsBefore = Set(NSApp.windows.map(ObjectIdentifier.init)) }
    }

    func testCaseDidFinish(_ testCase: XCTestCase) {
        let name = testCase.name
        MainActor.assumeIsolated {
            let done = XCTestExpectation(description: "documents closed")
            Task { @MainActor in
                await TestEngine.testEnded()
                done.fulfill()
            }
            _ = XCTWaiter().wait(for: [done], timeout: 20)
            checkWindows(after: name)
        }
    }

    func testSuiteDidFinish(_ testSuite: XCTestSuite) {
        MainActor.assumeIsolated { waitOnMain("the class's engines ended") { await TestEngine.classEnded() } }
    }

    func testBundleDidFinish(_ testBundle: Bundle) {
        MainActor.assumeIsolated { waitOnMain("every engine ended") { await TestEngine.bundleEnded() } }
        TestMediaCache.removeAll()
    }

    /// Runs `work` on the main actor and waits for it there, as XCTest's own observer calls must.
    @MainActor private func waitOnMain(_ what: String, _ work: @escaping @MainActor () async -> Void) {
        let done = XCTestExpectation(description: what)
        Task { @MainActor in
            await work()
            done.fulfill()
        }
        _ = XCTWaiter().wait(for: [done], timeout: 40)
    }

    /// A window a test opened that is still alive two tests after it finished is a leak. A window still
    /// open is closed first (a test may leave one up; closed, nothing holds it), so what fails is a
    /// window something still holds. A reply still on its way or a task still finishing can hold a
    /// window for a moment after its test; a leak holds it for good, so it is still there later.
    @MainActor private func checkWindows(after test: String) {
        autoreleasepool {
            for window in NSApp.windows where isNew(window) && window.isVisible { window.close() }
            forgetLastEvent()
        }
        for _ in 0..<3 { RunLoop.current.run(mode: .default, before: Date()) }
        autoreleasepool {
            for window in NSApp.windows where isNew(window) { suspects.append(Suspect(test: test, window: window)) }
        }
        suspects.removeAll { $0.window == nil }
        for suspect in suspects { suspect.testsSince += 1 }
        for suspect in suspects where suspect.testsSince > 2 {
            let what = suspect.window.map { "\(type(of: $0)) \"\($0.title)\"" } ?? "a window"
            leaks.append("\(suspect.test) left \(what) alive: it is still there two tests later")
        }
        suspects.removeAll { $0.testsSince > 2 }
    }

    /// A window a finished test left alive, held weakly.
    @MainActor private final class Suspect {
        let test: String
        weak var window: NSWindow?
        var testsSince = 0
        init(test: String, window: NSWindow) {
            self.test = test
            self.window = window
        }
    }

    /// The app keeps the last event it took from its queue (`currentEvent`), and that event keeps its
    /// window, until the next one: a test that sent a key to a window would see it held. An event of
    /// no window takes its place.
    @MainActor private func forgetLastEvent() {
        guard
            let marker = NSEvent.otherEvent(
                with: .applicationDefined, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0,
                context: nil, subtype: 0, data1: 0, data2: 0)
        else { return }
        NSApp.postEvent(marker, atStart: true)
        _ = NSApp.nextEvent(matching: .applicationDefined, until: Date(), inMode: .default, dequeue: true)
    }

    /// Opened during the running test, by the app, the tests or AppKit itself.
    @MainActor private func isNew(_ window: NSWindow) -> Bool {
        !windowsBefore.contains(ObjectIdentifier(window)) && isAppWindow(window)
    }
}

/// A window of the app, the tests or AppKit itself. Windows of other system frameworks (text input
/// and its completion list) are made once for the process when a field first takes the keyboard.
@MainActor
func isAppWindow(_ window: NSWindow) -> Bool {
    let owner = Bundle(for: type(of: window))
    return owner == Bundle.main || owner == Bundle(for: EngineTestObserver.self) || owner == Bundle(for: NSWindow.self)
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

    /// The autosaves of `names` are written (`waitForAutosaves`); fails the test if they never are.
    func autosaved(_ names: String..., file: StaticString = #filePath, line: UInt = #line) async {
        let ok = await waitForAutosaves(Set(names), on: app)
        XCTAssertTrue(ok, "autosaves of \(names) written", file: file, line: line)
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
        let source = try XCTUnwrap(TestEngine.testShows?.appendingPathComponent(template))
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

/// Waits until the engine's autosaves of the workspaces named `names` are on disk (`scan_recovery`
/// lists them). An autosave falls due on `advance` but is written on the engine's persist thread,
/// so a SIGKILL right after the advance can beat it.
@MainActor
func waitForAutosaves(_ names: Set<String>, on client: EngineClient) async -> Bool {
    await eventually {
        if case .success(let ok) = await client.send({ .request(id: $0, .scanRecovery) }),
            case .recoveryEntries(let entries)? = ok.data, names.isSubset(of: Set(entries.compactMap(\.showName)))
        {
            return true
        }
        _ = await client.send({ .request(id: $0, .advance(frames: 0)) })
        return false
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

    /// Returns once the engine has answered a request sent now: replies sent before it are applied. A
    /// broadcast can be overtaken by a reply (`hub::distribute`), so wait for what it brings.
    func sync() async {
        _ = await client.send({ .request(id: $0, .listPatchMappings) })
    }
}

/// The real engine's connections as the app opens them (every `Rig` on an engine uses one), with
/// what the app sent on each (the order of messages is part of several flows: an edit before a
/// close, everything before `goodbye`) and the wire faults the real engine cannot be asked for: results or
/// `workspaces` lists held, results made unreadable, a flush that never returns, the connections dropped. No cue
/// model: every frame the app reads came from the engine.
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
    /// The engine's `workspaces` lists wait too, for a test that puts its own list ahead of them.
    var holdLists = false
    /// Results are unreadable when they reach the app (a held one when it is released).
    var garbleResults = false
    var holdFlush = false
    /// Flushes held by `holdFlush`, ended by `releaseResults` (one never ended would keep its
    /// caller, and all it holds, for the rest of the run).
    fileprivate var heldFlushes: [CheckedContinuation<Void, Never>] = []
    /// Hold `runtime` broadcasts (the engine's playhead moves) until `releaseResults`.
    var holdRuntime = false
    /// Hold `model_changed` broadcasts until `releaseResults`: the app's model lags behind the engine's.
    var holdModel = false
    /// Hold `document` broadcasts until `releaseResults`: a store learns its path after the save's reply.
    var holdDocument = false
    /// Hold everything a workspace connection receives, its `welcome` (which carries the snapshot) first and
    /// in order, until `releaseResults`: a big show's snapshot is slow to reach the app. Clear it before releasing.
    var holdSnapshots = false
    fileprivate var connections: [TapConnection] = []
    /// Every message the app sent, with the connection it went on (kept by `forgetSent`): a result is
    /// matched to its request by both, as each connection numbers its own.
    private var sentBy: [(TapConnection, ClientMessage)] = []
    fileprivate var held: [(TapConnection, String)] = []
    /// The loudest peak each patch's mapped device outputs reported since `resetDevicePeaks`, from
    /// the engine's `meters` (`peaks`: one per cue output, at the device output it is mapped to):
    /// what is heard there, which the app's meters (the cue outputs themselves) do not show.
    private(set) var devicePeaks: [ModelID: [Float]] = [:]

    func resetDevicePeaks() { devicePeaks = [:] }

    fileprivate func noteMeters(_ text: String) {
        guard let v = JSONValue.parse(text), v["type"]?.stringValue == "meters" else { return }
        for m in v["outputs"]?.arrayValue ?? [] {
            guard let patch = m["patch"]?.stringValue else { continue }
            let peaks = (m["peaks"]?.arrayValue ?? []).map { Float($0.doubleValue ?? 0) }
            var kept = devicePeaks[ModelID(patch)] ?? []
            for (i, p) in peaks.enumerated() {
                if i < kept.count { kept[i] = max(kept[i], p) } else { kept.append(p) }
            }
            devicePeaks[ModelID(patch)] = kept
        }
    }

    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection {
        let c = TapConnection(inner: try await inner.connect(info: info), tap: self)
        connections.append(c)
        return c
    }

    fileprivate func record(_ text: String, from c: TapConnection) {
        guard let m = try? JSONDecoder().decode(ClientMessage.self, from: Data(text.utf8)) else { return }
        if case .hello(let h) = m { c.doc = h.workspace }
        sentOn.append((c.doc, m))
        sentBy.append((c, m))
    }

    /// Ends every connection as a network drop would; the engine and its documents stay.
    func dropConnections() {
        for c in connections { c.close() }
        connections = []
    }

    /// Delivers what is held, in the order it came or (`reversed`) newest first, and ends held flushes.
    func releaseResults(reversed: Bool = false) {
        let all = reversed ? held.reversed() : held
        held = []
        for (c, text) in all { c.deliver(garbled(text)) }
        let flushes = heldFlushes
        heldFlushes = []
        for flush in flushes { flush.resume() }
    }

    /// `text`, unreadable if it is a result and results are to be.
    fileprivate func garbled(_ text: String) -> String {
        guard garbleResults, let v = JSONValue.parse(text), v["type"]?.stringValue == "result",
            var o = v.objectValue
        else { return text }
        o["error"] = nil
        o["ok"] = .string("garbled")
        return (try? JSONValue.object(o).text()) ?? text
    }

    /// How many results are held (`holdResults`).
    var heldCount: Int { held.count }

    /// How many held results answer a message the app sent that `matches`, on the connection it went
    /// on: how many of those requests are pending, whatever else is held beside them (the answer to a
    /// read another window started just before the hold, say).
    func heldResults(of matches: (ClientMessage) -> Bool) -> Int {
        let asked = sentBy.compactMap { c, m in matches(m) ? m.resultID.map { (c, Double($0)) } : nil }
        return held.filter { c, text in
            guard let v = JSONValue.parse(text), v["type"]?.stringValue == "result", let id = v["id"]?.doubleValue
            else { return false }
            return asked.contains { $0.0 === c && $0.1 == id }
        }.count
    }

    /// Forgets what was sent so far (the test's own setup).
    func forgetSent() { sentOn = [] }

    /// How many `request`s of this kind were sent.
    func requestCount(_ matches: (Request) -> Bool) -> Int {
        sent.filter { if case .request(_, let r) = $0 { return matches(r) } else { return false } }.count
    }
}

extension ClientMessage {
    /// A request that opens or makes a document (what a reattach sends for each window).
    var opensADoc: Bool {
        switch self {
        case .request(_, .open), .request(_, .openRecovery), .request(_, .new): return true
        default: return false
        }
    }

    /// The id of the `result` that answers it (nil: none does).
    var resultID: UInt64? {
        switch self {
        case .command(let id, _, _), .transport(let id, _), .undo(let id), .redo(let id), .request(let id, _),
            .close(let id, _, _), .panic(let id), .hotkey(let id, _, _, _):
            return id
        case .hello, .goodbye: return nil
        }
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
        if tap.holdSnapshots && doc != nil { return tap.held.append((self, text)) }
        let type = JSONValue.parse(text)?["type"]?.stringValue
        if type == "meters" { tap.noteMeters(text) }
        if type == "runtime" && tap.holdRuntime { return tap.held.append((self, text)) }
        if type == "model_changed" && tap.holdModel { return tap.held.append((self, text)) }
        if type == "document" && tap.holdDocument { return tap.held.append((self, text)) }
        if type == "result" ? tap.holdResults : (type == "workspaces" && tap.holdLists) {
            return tap.held.append((self, text))
        }
        deliver(tap.garbled(text))
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
            await withCheckedContinuation { tap.heldFlushes.append($0) }
            return
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

extension WorkspaceStore {
    /// Merges `hotkeys` (a JSON object of key-map fields) into the workspace's key map through the
    /// engine (`set_key_map`) and waits until this store's model shows it.
    func setHotkeys(_ hotkeys: String) async throws {
        var map = try XCTUnwrap(model?.settings["hotkeys"])
        for (key, value) in try XCTUnwrap(JSONValue.parse(hotkeys)?.objectValue) { map.set(key, value) }
        let result = await send(command: .setKeyMap(map))
        guard case .success = result else { return XCTFail("set_key_map: \(result)") }
        await assertEventually { self.model?.settings["hotkeys"] == map }
    }
}

extension Icon {
    /// Which of openLab's own drawings `image` shows, nil for any other image (a symbol, the text
    /// stand-in): read from the name the lookup tags each drawing's image with, not from pixels.
    static func drawing(shownBy image: NSImage?) -> Icon? {
        guard let name = IconLookup.drawingName(of: image) else { return nil }
        return allCases.first { $0.drawing == name }
    }
}

/// `view` and everything in it drawn into a bitmap of `scale` pixels per point, over `background`
/// when given. The view tree is drawn straight into the bitmap: `cacheDisplay` skips layer-backed
/// subviews, and a window never ordered on screen has nothing to read back. Each view's `draw`
/// runs with the clip AppKit gives it on screen, so a view that does not clip to its bounds paints
/// over its neighbours here as it does there. Rendering the window's frame view gives the window.
@MainActor
func renderView(_ view: NSView, scale: CGFloat = 2, background: NSColor? = nil) throws -> NSBitmapImageRep {
    let rep = try XCTUnwrap(
        NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: Int((view.bounds.width * scale).rounded()),
            pixelsHigh: Int((view.bounds.height * scale).rounded()), bitsPerSample: 8, samplesPerPixel: 4,
            hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
        "a bitmap for \(view.bounds.size)")
    let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: rep))
    if let background {
        context.cgContext.setFillColor(background.cgColor)
        context.cgContext.fill(CGRect(x: 0, y: 0, width: rep.pixelsWide, height: rep.pixelsHigh))
    }
    context.cgContext.scaleBy(x: scale, y: scale)
    view.displayIgnoringOpacity(view.bounds, in: context)
    redrawTabStrips(in: view, rep: rep, context: context, scale: scale)
    return rep
}

/// On screen a tab view shows its tabs through its layer; forced to draw (above) it paints a blank
/// white bar where they go, over the tab buttons' own drawing. Each bar is filled with the colour
/// beside it and the tab buttons are drawn again on top.
@MainActor
private func redrawTabStrips(in root: NSView, rep: NSBitmapImageRep, context: NSGraphicsContext, scale: CGFloat) {
    var strips: [NSView] = []
    func find(_ v: NSView) {
        guard !v.isHidden else { return }
        if v is NSTabView {
            strips += v.subviews.filter { $0 is NSSegmentedControl && !$0.isHidden }
        }
        v.subviews.forEach(find)
    }
    find(root)
    for control in strips {
        let r = control.convert(control.bounds, to: root)
        // The colour just left of the bar, in bitmap pixels (rows run top-down).
        let x = Int((r.minX - 4) * scale)
        let y = root.isFlipped ? Int(r.midY * scale) : rep.pixelsHigh - Int(r.midY * scale)
        guard x >= 0, y >= 0, y < rep.pixelsHigh, let beside = rep.colorAt(x: x, y: y) else { continue }
        let cg = context.cgContext
        cg.saveGState()
        cg.setFillColor(beside.cgColor)
        cg.fill(r)
        cg.translateBy(x: r.minX, y: r.minY)
        control.displayIgnoringOpacity(control.bounds, in: context)
        cg.restoreGState()
    }
}

/// A real event's worth of fields at a point, as AppKit delivers a scroll or pinch to a view
/// (NSEvent has no public initializer for those).
final class GestureEvent: NSEvent {
    var point = NSPoint.zero
    var mag: CGFloat = 0
    var delta = (x: CGFloat(0), y: CGFloat(0))
    var precise = true
    var flags: NSEvent.ModifierFlags = []
    override var locationInWindow: NSPoint { point }
    override var magnification: CGFloat { mag }
    override var scrollingDeltaX: CGFloat { delta.x }
    override var scrollingDeltaY: CGFloat { delta.y }
    override var hasPreciseScrollingDeltas: Bool { precise }
    override var modifierFlags: NSEvent.ModifierFlags { flags }
    // One wheel step outside any trackpad gesture: AppKit's tables and scroll views read these, and
    // NSEvent's own getters assert on an event the window server did not make.
    override var phase: NSEvent.Phase { [] }
    override var momentumPhase: NSEvent.Phase { [] }
}

/// The texts shown in a sheet (an alert's message).
@MainActor
func sheetText(_ sheet: NSWindow) -> String {
    var found: [String] = []
    func walk(_ view: NSView) {
        if let field = view as? NSTextField { found.append(field.stringValue) }
        view.subviews.forEach(walk)
    }
    if let content = sheet.contentView { walk(content) }
    return found.joined(separator: " ")
}

/// Clicks the button titled `title` in `sheet`, as the operator would; false when it has none.
@MainActor
func pressButton(_ title: String, in sheet: NSWindow) -> Bool {
    var pressed = false
    func walk(_ view: NSView) {
        if let button = view as? NSButton, button.title == title, !pressed {
            button.performClick(nil)
            pressed = true
        }
        view.subviews.forEach(walk)
    }
    if let content = sheet.contentView { walk(content) }
    return pressed
}

/// A UDP socket the test owns on the loopback interface: a Network cue's destination, so what the
/// engine really sends (or does not send) is what the test reads.
final class UDPListener {
    let fd: Int32
    let port: UInt16

    init() throws {
        let fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else { throw TestEngineError.refused("socket") }
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_port = 0
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) }
        }
        guard bound == 0, named == 0, fcntl(fd, F_SETFL, O_NONBLOCK) == 0 else {
            close(fd)
            throw TestEngineError.refused("bind")
        }
        self.fd = fd
        port = UInt16(bigEndian: addr.sin_port)
    }

    deinit { close(fd) }

    /// The next datagram that has arrived, nil when none has.
    func receive() -> Data? {
        var buffer = [UInt8](repeating: 0, count: 65536)
        let n = recv(fd, &buffer, buffer.count, 0)
        return n > 0 ? Data(buffer.prefix(n)) : nil
    }

    /// The next OSC message that has arrived, as "address arg arg" with typed arguments (`i:5`,
    /// `f:10.5`, `s:text`, `T`, `F`); nil when none has.
    func receiveMessage() -> String? {
        guard let data = receive() else { return nil }
        let bytes = Array(data)
        var at = 0
        func string() -> String {
            let start = at
            while at < bytes.count, bytes[at] != 0 { at += 1 }
            let text = String(decoding: bytes[start..<min(at, bytes.count)], as: UTF8.self)
            at = (at / 4 + 1) * 4
            return text
        }
        func word() -> UInt32 {
            defer { at += 4 }
            guard at + 4 <= bytes.count else { return 0 }
            return bytes[at..<at + 4].reduce(0) { $0 << 8 | UInt32($1) }
        }
        var parts = [string()]
        for tag in string().dropFirst() {
            switch tag {
            case "i": parts.append("i:\(Int32(bitPattern: word()))")
            case "f": parts.append("f:\(Float(bitPattern: word()))")
            case "s": parts.append("s:\(string())")
            case "T": parts.append("T")
            case "F": parts.append("F")
            default: parts.append("?\(tag)")
            }
        }
        return parts.joined(separator: " ")
    }
}

/// A test movie or still made by the bundled stage host (test-support builds: `--make-test-media`):
/// every frame is the test card for its own number (288 x 160), so a stage probe reads which frame
/// is shown. `codec` is `h264`, `hevc` or `prores` (a movie of `frames` at `fps`) or `png`, `jpeg`,
/// `tiff` (a still). Nothing is checked in; a codec this Mac cannot encode fails the test.
///
/// Each kind of file is made once per test process (`TestMediaCache`) and copied from there: making
/// a movie takes the stage host most of a second, and the Video tests make four per test.
func makeTestMedia(_ url: URL, codec: String, frames: Int = 0, fps: Int = 25) throws {
    let fm = FileManager.default
    try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try? fm.removeItem(at: url)
    let made = try TestMediaCache.file(codec: codec, frames: frames, fps: fps, ext: url.pathExtension) {
        try runMakeTestMedia($0, codec: codec, frames: frames, fps: fps)
    }
    try fm.copyItem(at: made, to: url)
    // As if written now: a copy keeps the made file's date, and the engine tells a changed file by it.
    try fm.setAttributes([.modificationDate: Date()], ofItemAtPath: url.path)
}

/// The test media made so far in this test process, in a folder of its own that is removed when
/// the test bundle ends.
enum TestMediaCache {
    static let folder = FileManager.default.temporaryDirectory.appendingPathComponent(
        "openlab-test-media-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)

    /// The file made for these arguments, made by `make` (given where to write it) the first time.
    static func file(codec: String, frames: Int, fps: Int, ext: String, make: (URL) throws -> Void) throws -> URL {
        let name = "\(codec)-\(frames)-\(fps)".map { $0.isLetter || $0.isNumber || $0 == "-" ? $0 : "_" }
        let url = folder.appendingPathComponent(String(name)).appendingPathExtension(ext)
        if FileManager.default.fileExists(atPath: url.path) { return url }
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        // Made under a temporary name and moved in whole: a make that fails leaves nothing to reuse.
        let partial = folder.appendingPathComponent("partial-\(UUID().uuidString)").appendingPathExtension(ext)
        defer { try? FileManager.default.removeItem(at: partial) }
        try make(partial)
        try FileManager.default.moveItem(at: partial, to: url)
        return url
    }

    static func removeAll() { try? FileManager.default.removeItem(at: folder) }
}

private func runMakeTestMedia(_ url: URL, codec: String, frames: Int, fps: Int) throws {
    let host = Bundle.main.bundleURL.appendingPathComponent(
        "Contents/Helpers/openLab Stage.app/Contents/MacOS/openlab-stage")
    let process = Process()
    process.executableURL = host
    process.arguments = ["--make-test-media", codec, url.path] + (frames > 0 ? [String(frames), String(fps)] : [])
    let errors = Pipe()
    process.standardError = errors
    process.standardOutput = FileHandle.nullDevice
    try process.run()
    let said = String(decoding: errors.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
        throw TestEngineError.refused("--make-test-media \(codec) \(url.lastPathComponent): \(said)")
    }
}

/// A media file at `url` of `seconds`, its kind by its extension: a WAV tone, a movie at 25 fps
/// (`.mov` H.264, `.mp4` HEVC) or a still (`.png`, `.jpg`, `.tiff`; `seconds` unused). `sound`
/// gives a movie audio tracks: one per `+`-joined part (`pcm:2:48000`, `aac:2:44100+pcm:6:48000`).
func writeTestMedia(_ url: URL, seconds: Double, sound: String? = nil) throws {
    let frames = max(1, Int((seconds * 25).rounded()))
    let tracks = sound.map { "+\($0)" } ?? ""
    switch url.pathExtension.lowercased() {
    case "mov": try makeTestMedia(url, codec: "h264" + tracks, frames: frames)
    case "mp4": try makeTestMedia(url, codec: "hevc" + tracks, frames: frames)
    case "png": try makeTestMedia(url, codec: "png")
    case "jpg", "jpeg": try makeTestMedia(url, codec: "jpeg")
    case "tif", "tiff": try makeTestMedia(url, codec: "tiff")
    default: try writeWAV(url, seconds: seconds)
    }
}

/// The stage hosts `engine` runs now (its children named `openlab-stage`; a picture probe is one
/// too while it runs).
func stageHostPids(of engine: Int32) -> [Int32] {
    var pids = [Int32](repeating: 0, count: 256)
    let count = proc_listchildpids(engine, &pids, Int32(pids.count * MemoryLayout<Int32>.size))
    guard count > 0 else { return [] }
    return pids.prefix(Int(count)).filter { pid in
        var name = [CChar](repeating: 0, count: 256)
        guard proc_name(pid, &name, UInt32(name.count)) > 0 else { return false }
        return String(cString: name) == "openlab-stage"
    }
}
