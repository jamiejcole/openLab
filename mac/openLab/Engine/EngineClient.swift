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
    /// A `snapshot` (or `new` / `open` / `revert`) is already out; never pipelined (docs/network.md).
    case snapshotAlreadyPending
    /// The engine in use does not have this feature (the multi-workspace calls on a one-workspace engine).
    case unsupported
}

enum ConnectionFailure: Equatable, Sendable {
    /// No readable `engine.json`: the engine is not running. A normal state.
    case engineNotAvailable
    /// The token in `engine.json` was refused (stale file, engine restarted); the file is re-read.
    case badToken
    case versionMismatch(engineVersion: UInt32?)
    case lost
    case engine(code: ErrorCode, message: String)
    case transport(String)
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
    private let policy: ReconnectPolicy
    private let clientName: String
    private let workspace: ModelID?

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
        scheduler: Scheduler = SystemScheduler(), policy: ReconnectPolicy = ReconnectPolicy(),
        clientName: String = "openLab for macOS", workspace: ModelID? = nil
    ) {
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
        connection.send(text)
    }

    private func failPending(_ error: EngineError) {
        let all = pending
        pending = [:]
        snapshotRequest = nil
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
            if case .versionMismatch = result.failure {
                state = .failed(result.failure)
                return
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
        let conn: any EngineConnection
        do { conn = try await transport.connect(info: info) } catch {
            return (.transport(error.localizedDescription), false)
        }
        connection = conn
        nextRequestID = 1
        defer {
            conn.close()
            connection = nil
            failPending(.disconnected)
        }
        let hello = Hello(
            protocolVersion: protocolVersion, token: info.token, clientName: clientName, workspace: workspace)
        guard let helloText = try? ClientMessage.hello(hello).text() else { return (.transport("hello"), false) }
        conn.send(helloText)

        var welcomed = false
        do {
            while true {
                let text = try await conn.receive()
                if Task.isCancelled { return (.lost, welcomed) }
                let message: ServerMessage
                do { message = try ServerMessage.decode(text) } catch {
                    engineLog.error("dropped undecodable frame: \(error.localizedDescription, privacy: .public)")
                    continue
                }
                if !welcomed {
                    switch message {
                    case .welcome, .provisional(.appWelcome):
                        welcomed = true
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
            return (.lost, welcomed)
        }
    }

    // MARK: Inbound

    private func handle(_ message: ServerMessage) {
        switch message {
        case .result(let id, let outcome):
            if snapshotRequest == id { snapshotRequest = nil }
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
        default:
            if delegate?.engineClient(self, didReceive: message) == .gap { requestSnapshot() }
        }
    }
}
