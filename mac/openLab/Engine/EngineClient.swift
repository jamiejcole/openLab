// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// Why a request or the connection failed. Never a crash: every engine refusal is a value.
enum EngineError: Error, Equatable, Sendable {
    case notConnected
    /// The connection dropped before the `result` arrived (the request may or may not have run).
    case disconnected
    case engine(code: ErrorCode, message: String)
    case encoding(String)
    /// A `snapshot` is already out; never pipelined (docs/network.md).
    case snapshotAlreadyPending
    /// No `result` within `ReconnectPolicy.requestTimeout` (the engine may still carry it out).
    case timeout
    /// `open` found an autosave newer than the show file and opened nothing (`recovery_offer`):
    /// ask, then repeat the `open` with a choice.
    case recoveryOffered(path: String, workspaceName: String, modifiedMs: Int64)
}

enum ConnectionFailure: Equatable, Sendable {
    /// No readable `engine.json`: the engine is not running. A normal state.
    case engineNotAvailable
    /// `engine.json` says the engine's shutdown has begun: it is not attached to (it is about to
    /// exit), and its workspaces can no longer be saved through it.
    case engineStopping
    /// The token in `engine.json` was refused (stale file, engine restarted); the file is re-read.
    case badToken
    case versionMismatch(engineVersion: UInt32?)
    case lost
    case engine(code: ErrorCode, message: String)
    case transport(String)
    /// The socket opened but no `welcome` came in time. The engine builds it from a snapshot of
    /// its control loop, so this means busy (maybe playing), not gone: never silence.
    case noWelcome
    /// The engine closed this connection's workspace (`closed`): never reconnect to it.
    case workspaceClosed
    /// `engine.json` names another engine run than the one this workspace connection was bound in:
    /// doc ids restart in a new run, so the old one could name another show. The session rebinds.
    case engineRestarted
}

enum ConnectionState: Equatable, Sendable {
    case idle
    case connecting
    case connected
    /// Waiting to try again (backoff).
    case retrying(ConnectionFailure)
    /// Will not retry on its own (a protocol version mismatch).
    case failed(ConnectionFailure)
}

/// Reconnect delays: doubling from `initial`, capped at `cap`.
struct ReconnectPolicy: Equatable, Sendable {
    var initial = 0.25
    var cap = 5.0
    /// How long a connection may take to deliver its `welcome` before the attempt is abandoned.
    var handshakeTimeout = 10.0
    /// How long a live request (transport, edit, hotkey, panic, a request the control thread
    /// answers at once) may wait for its `result`. Above the engine's 5 s hold of a GO on a file
    /// still being probed. Document requests (new, open, save, close, relink, recovery) have no
    /// deadline here: the engine bounds its reads, and the app bounds saves and closes itself.
    var requestTimeout = 10.0
    /// Live requests timed out in a row, with no `result` between them, that mark the connection
    /// dead (an engine whose socket is open but that no longer answers): it is closed and retried.
    var deadAfterTimeouts = 2
    func delay(attempt: Int) -> Double { min(cap, initial * pow(2, Double(min(attempt, 30)))) }
}

enum ApplyOutcome: Equatable, Sendable {
    case applied
    /// Old news (a revision or seq at or below what the store holds); dropped.
    case ignored
    /// A revision or seq step that is not +1, or a change that does not fit the model: resync.
    case gap
}

@MainActor
protocol EngineClientDelegate: AnyObject {
    /// The revision the delegate holds, to spot a `result` that is ahead of its model.
    var revision: UInt64 { get }
    func engineClient(_ client: EngineClient, didReceive message: ServerMessage) -> ApplyOutcome
}

/// The connection to one engine socket: handshake, request/result matching, gap recovery and
/// reconnect. It owns no model; the delegate (a `WorkspaceStore`, or the `EngineSession`) does.
@MainActor
final class EngineClient: ObservableObject {
    @Published private(set) var state: ConnectionState = .idle
    /// `error` messages and non-result refusals, for the UI to show. Never fatal.
    let engineErrors = PassthroughSubject<ErrorBody, Never>()
    weak var delegate: EngineClientDelegate?

    private let transport: EngineTransport
    private let infoProvider: @MainActor () -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>
    private let scheduler: Scheduler
    private let handshakeScheduler: Scheduler
    private let requestScheduler: Scheduler
    /// Deadline timers of the live requests in flight.
    private var deadlines: [UInt64: Task<Void, Never>] = [:]
    private var timeoutsInARow = 0
    private var handshakeTimedOut = false
    private let policy: ReconnectPolicy
    private let clientName: String
    /// The doc this connection is bound to; nil for the app connection.
    private(set) var workspace: ModelID?
    /// For a bound connection: the token of the engine run its doc id belongs to (nil: any run).
    private(set) var runToken: String?
    /// `engine.json` as read for the connection that last reached `welcome` (kept after a drop:
    /// the supervisor asks whether that engine is still alive).
    private(set) var connectedRun: EngineConnectionInfo?
    private var workspaceClosed = false

    private var runTask: Task<Void, Never>?
    private var connection: (any EngineConnection)?
    private typealias Completion = @MainActor (Result<OkBody, EngineError>) -> Void
    private var pending: [UInt64: Completion] = [:]
    /// The one snapshot-answering request in flight, if any.
    private var snapshotRequest: UInt64?
    /// Per connection, from 1. Settable for tests only (to reach the `u64::MAX` edge).
    var nextRequestID: UInt64 = 1

    init(
        transport: EngineTransport,
        infoProvider: @escaping @MainActor () -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>,
        scheduler: Scheduler = SystemScheduler(), handshakeScheduler: Scheduler = SystemScheduler(),
        requestScheduler: Scheduler = SystemScheduler(), policy: ReconnectPolicy = ReconnectPolicy(),
        clientName: String = "openLab for macOS", workspace: ModelID? = nil, runToken: String? = nil
    ) {
        self.runToken = runToken
        self.handshakeScheduler = handshakeScheduler
        self.requestScheduler = requestScheduler
        self.transport = transport
        self.infoProvider = infoProvider
        self.scheduler = scheduler
        self.policy = policy
        self.clientName = clientName
        self.workspace = workspace
    }

    func start() {
        guard runTask == nil else { return }
        runTask = Task { await run() }
    }

    /// Binds this connection to `doc` of the engine run with `runToken` and connects again (a
    /// window reattached to its reopened show after an engine restart). Requests in flight fail.
    func rebind(workspace doc: ModelID, runToken token: String?) {
        stop()
        workspace = doc
        runToken = token
        start()
    }

    func stop() {
        runTask?.cancel()
        runTask = nil
        connection?.close()
        connection = nil
        failPending(.notConnected)
        state = .idle
    }

    // MARK: Requests

    /// Sends one message built from a fresh request id and waits for its `result`.
    func send(_ make: (UInt64) -> ClientMessage) async -> Result<OkBody, EngineError> {
        await withCheckedContinuation { cont in
            submit(make, completion: { cont.resume(returning: $0) })
        }
    }

    /// Sends every message, in order, before waiting for any `result` (cues the operator acts on
    /// together must not start a round trip apart). One result per message, in the same order.
    func sendAll(_ makes: [(UInt64) -> ClientMessage]) async -> [Result<OkBody, EngineError>] {
        guard !makes.isEmpty else { return [] }
        let batch = Batch(count: makes.count)
        return await withCheckedContinuation { cont in
            batch.cont = cont
            for (i, make) in makes.enumerated() { submit(make, completion: { batch.set(i, $0) }) }
        }
    }

    @MainActor private final class Batch {
        var cont: CheckedContinuation<[Result<OkBody, EngineError>], Never>?
        private var results: [Result<OkBody, EngineError>?]
        private var left: Int
        init(count: Int) {
            results = Array(repeating: nil, count: count)
            left = count
        }
        func set(_ i: Int, _ r: Result<OkBody, EngineError>) {
            guard results.indices.contains(i), results[i] == nil else { return }
            results[i] = r
            left -= 1
            if left == 0 {
                cont?.resume(returning: results.map { $0 ?? .failure(.disconnected) })
                cont = nil
            }
        }
    }

    /// `goodbye`, flushed to the socket. No reply exists; a no-op when not connected.
    func goodbye(shutdown: Bool) async {
        guard state == .connected, let connection, let text = try? ClientMessage.goodbye(shutdown: shutdown).text()
        else { return }
        await connection.sendAndFlush(text)
    }

    /// Asks for a snapshot unless one is already out. Fire and forget.
    func requestSnapshot() {
        submit({ .request(id: $0, .snapshot) }, completion: { _ in })
    }

    private func allocateID() -> UInt64 {
        // u64::MAX is the server's own snapshot request id; a client using it never gets a result.
        if nextRequestID == UInt64.max { nextRequestID = 1 }
        defer { nextRequestID += 1 }
        return nextRequestID
    }

    /// Synchronous so the bookkeeping (pending, snapshot gate) is settled before any suspension.
    private func submit(_ make: (UInt64) -> ClientMessage, completion: @escaping Completion) {
        guard state == .connected, let connection else { return completion(.failure(.notConnected)) }
        let id = allocateID()
        let message = make(id)
        var answersWithSnapshot = false
        if case .request(_, let r) = message { answersWithSnapshot = r.answersWithSnapshot }
        if answersWithSnapshot && snapshotRequest != nil { return completion(.failure(.snapshotAlreadyPending)) }
        let text: String
        do { text = try message.text() } catch { return completion(.failure(.encoding("\(error)"))) }
        pending[id] = completion
        if answersWithSnapshot { snapshotRequest = id }
        if message.hasDeadline {
            deadlines[id] = Task { [weak self, requestScheduler, timeout = policy.requestTimeout] in
                do { try await requestScheduler.sleep(seconds: timeout) } catch { return }
                if !Task.isCancelled { self?.timedOut(id, on: connection) }
            }
        }
        connection.send(text)
    }

    /// A live request got no answer in time: it fails (the caller beeps and logs, as for a lost
    /// connection). Several in a row with no result between mean the engine stopped answering on
    /// an open socket: the connection is closed, which fails the rest and reconnects.
    private func timedOut(_ id: UInt64, on conn: any EngineConnection) {
        deadlines[id] = nil
        guard connection === conn, let completion = pending.removeValue(forKey: id) else { return }
        if snapshotRequest == id { snapshotRequest = nil }
        timeoutsInARow += 1
        engineLog.error("request \(id) got no result in \(self.policy.requestTimeout) s")
        completion(.failure(.timeout))
        if timeoutsInARow >= policy.deadAfterTimeouts {
            engineLog.error("engine stopped answering on an open connection: reconnecting")
            timeoutsInARow = 0
            conn.close()
        }
    }

    private func failPending(_ error: EngineError) {
        let all = pending
        pending = [:]
        snapshotRequest = nil
        for (_, timer) in deadlines { timer.cancel() }
        deadlines = [:]
        for (_, completion) in all { completion(.failure(error)) }
    }

    // MARK: Connection loop

    private func run() async {
        var attempt = 0
        while !Task.isCancelled {
            state = .connecting
            let result = await connectOnce()
            if Task.isCancelled { return }
            if result.reachedWelcome { attempt = 0 }
            switch result.failure {
            case .versionMismatch, .engine(.noWorkspace, _), .workspaceClosed, .engineRestarted:
                // Retrying cannot help: a version mismatch, a workspace the engine closed, or a
                // doc id of an engine run that is gone.
                state = .failed(result.failure)
                return
            default: break
            }
            state = .retrying(result.failure)
            let delay = policy.delay(attempt: attempt)
            attempt += 1
            do { try await scheduler.sleep(seconds: delay) } catch { return }
        }
    }

    private func connectOnce() async -> (failure: ConnectionFailure, reachedWelcome: Bool) {
        let info: EngineConnectionInfo
        switch infoProvider() {
        case .failure: return (.engineNotAvailable, false)
        case .success(let i): info = i
        }
        // An engine that is shutting down is not attached to: it is about to exit.
        if info.stopping { return (.engineStopping, false) }
        if let runToken, workspace != nil, info.token != runToken { return (.engineRestarted, false) }
        let conn: any EngineConnection
        do { conn = try await transport.connect(info: info) } catch {
            return (.transport(error.localizedDescription), false)
        }
        if Task.isCancelled {  // stopped while connecting
            conn.close()
            return (.lost, false)
        }
        connection = conn
        nextRequestID = 1
        timeoutsInARow = 0
        handshakeTimedOut = false
        workspaceClosed = false
        let timer = Task { [handshakeScheduler, timeout = policy.handshakeTimeout] in
            do { try await handshakeScheduler.sleep(seconds: timeout) } catch { return }
            if !Task.isCancelled {
                handshakeTimedOut = true
                conn.close()
            }
        }
        defer {
            timer.cancel()
            conn.close()
            // A stop() + start() may already have installed the next run's connection.
            if connection === conn {
                connection = nil
                failPending(.disconnected)
            }
        }
        // The pid lets the engine tell a slow reconnect from a crash (it relaunches only a gone app).
        let hello = Hello(
            protocolVersion: protocolVersion, token: info.token, clientName: clientName, workspace: workspace,
            pid: UInt32(clamping: ProcessInfo.processInfo.processIdentifier))
        guard let helloText = try? ClientMessage.hello(hello).text() else { return (.transport("hello"), false) }
        conn.send(helloText)

        var welcomed = false
        do {
            while true {
                let text = try await conn.receive()
                if Task.isCancelled { return (.lost, welcomed) }
                let message: ServerMessage
                do { message = try ServerMessage.decode(text) } catch {
                    engineLog.error("undecodable frame: \(error.localizedDescription, privacy: .public)")
                    // Before the welcome the attempt has failed; after it, a `result` must not hang.
                    if !welcomed { return (.transport("undecodable frame before welcome"), false) }
                    failUndecodableResult(text)
                    continue
                }
                if !welcomed {
                    switch message {
                    case .welcome, .appWelcome:
                        welcomed = true
                        timer.cancel()
                        connectedRun = info
                        if workspace != nil, runToken == nil { runToken = info.token }
                        _ = delegate?.engineClient(self, didReceive: message)
                        state = .connected
                    case .error(let code, let detail, let engineVersion):
                        engineErrors.send(ErrorBody(code: code, message: detail))
                        if code == .badToken { return (.badToken, false) }
                        if code == .protocolVersionMismatch {
                            return (.versionMismatch(engineVersion: engineVersion), false)
                        }
                        return (.engine(code: code, message: detail), false)
                    default:
                        engineLog.error("message before welcome dropped")
                    }
                } else {
                    handle(message)
                }
            }
        } catch {
            if workspaceClosed { return (.workspaceClosed, welcomed) }
            return (handshakeTimedOut && !welcomed ? .noWelcome : .lost, welcomed)
        }
    }

    private struct ResultStub: Decodable {
        var type: String
        var id: UInt64
    }

    /// A `result` whose body does not decode still ends its request (and frees the snapshot gate).
    private func failUndecodableResult(_ text: String) {
        guard let stub = try? JSONDecoder().decode(ResultStub.self, from: Data(text.utf8)), stub.type == "result"
        else { return }
        if snapshotRequest == stub.id { snapshotRequest = nil }
        deadlines.removeValue(forKey: stub.id)?.cancel()
        pending.removeValue(forKey: stub.id)?(.failure(.encoding("undecodable result")))
    }

    // MARK: Inbound

    private func handle(_ message: ServerMessage) {
        switch message {
        case .result(let id, let outcome):
            if snapshotRequest == id { snapshotRequest = nil }
            timeoutsInARow = 0
            deadlines.removeValue(forKey: id)?.cancel()
            guard let completion = pending.removeValue(forKey: id) else { return }
            switch outcome {
            case .ok(let body):
                // The model_changed for N always precedes a result with revision N; behind it means
                // the change was dropped (docs/network.md), so resync.
                if let r = body.revision, let delegate, r > delegate.revision { requestSnapshot() }
                completion(.success(body))
            case .error(let e):
                completion(.failure(.engine(code: e.code, message: e.message)))
            }
        case .error(let code, let text, _):
            engineLog.error("engine error \(code.raw, privacy: .public): \(text, privacy: .public)")
            engineErrors.send(ErrorBody(code: code, message: text))
        case .unknown(let type):
            engineLog.notice("ignoring unknown server message type \(type, privacy: .public)")
        case .closed(let doc):
            // Only this connection's own doc: a `closed` naming another must not close a window.
            guard workspace != nil, doc == workspace else {
                engineLog.error("closed for doc \(doc.raw, privacy: .public) on another connection ignored")
                return
            }
            // The engine closes the connection next; the run loop then stops instead of retrying.
            workspaceClosed = true
            _ = delegate?.engineClient(self, didReceive: message)
        default:
            if delegate?.engineClient(self, didReceive: message) == .gap { requestSnapshot() }
        }
    }
}

extension ClientMessage {
    /// Whether `EngineClient` gives up on its `result` after `ReconnectPolicy.requestTimeout`.
    /// Document requests can rightly take long (a slow disk, a big show): they have none.
    var hasDeadline: Bool {
        switch self {
        case .transport, .command, .undo, .redo, .panic, .hotkey: return true
        case .hello, .goodbye, .close: return false
        case .request(_, let r):
            switch r {
            case .new, .open, .openRecovery, .scanRecovery, .save, .saveAs, .relink: return false
            case .listDevices, .preshowCheck, .logTail, .setOutputOverrides, .setShowMode, .setPatchMapping,
                .snapshot:
                return true
            }
        }
    }
}
