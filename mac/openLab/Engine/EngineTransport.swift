// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation
import os

let engineLog = Logger(subsystem: "dev.openlab.openLab", category: "engine")

/// `<app-support>/openLab/engine.json`: `{port, token, pid, stopping}`, written by the engine
/// (M4.9; the other fields are ignored).
struct EngineConnectionInfo: Equatable, Sendable, Decodable {
    var port: UInt16
    var token: String
    var pid: Int32
    /// The engine's shutdown has begun: do not attach, wait for `pid` to exit (docs/control.md,
    /// Process lifecycle). Absent (an older engine): false.
    var stopping = false

    enum LoadError: Error, Equatable, Sendable {
        /// No file: the engine is not running (or does not write one yet). A normal state.
        case missing
        case unreadable(String)
    }

    static var defaultURL: URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support")
        return base.appendingPathComponent("openLab/engine.json")
    }

    enum CodingKeys: String, CodingKey { case port, token, pid, stopping }

    /// Reads and validates the file. Called afresh on every connection attempt: the engine picks a
    /// new port and token each start.
    static func load(from url: URL = defaultURL) -> Result<EngineConnectionInfo, LoadError> {
        guard let data = try? Data(contentsOf: url) else { return .failure(.missing) }
        guard let info = try? JSONDecoder().decode(EngineConnectionInfo.self, from: data) else {
            return .failure(.unreadable("engine.json is not {port, token, pid}"))
        }
        guard info.port != 0, !info.token.isEmpty else {
            return .failure(.unreadable("engine.json has an empty token or port"))
        }
        return .success(info)
    }
}

extension EngineConnectionInfo {
    // In an extension, so the memberwise init stays.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        port = try c.decode(UInt16.self, forKey: .port)
        token = try c.decode(String.self, forKey: .token)
        pid = try c.decode(Int32.self, forKey: .pid)
        stopping = try c.decodeIfPresent(Bool.self, forKey: .stopping) ?? false
    }
}

/// One live connection to the engine: `WebSocketConnection` (tests wrap it to record or hold frames).
@MainActor
protocol EngineConnection: AnyObject {
    /// Queues one text frame; order is preserved. A failed write surfaces as `receive` throwing.
    func send(_ text: String)
    /// The next text frame. Throws when the connection is closed or has failed.
    func receive() async throws -> String
    func close()
    /// `send`, returning once the frame has been handed to the OS (the app is about to quit).
    func sendAndFlush(_ text: String) async
}

extension EngineConnection {
    func sendAndFlush(_ text: String) async { send(text) }
}

/// Opens connections: `WebSocketTransport` (tests wrap it to record or hold frames).
@MainActor
protocol EngineTransport: AnyObject {
    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection
}

/// Sleeps for reconnect backoff. Injected so tests never wait in real time.
protocol Scheduler: Sendable {
    func sleep(seconds: Double) async throws
}

struct SystemScheduler: Scheduler {
    func sleep(seconds: Double) async throws {
        try await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
    }
}

@MainActor
final class WebSocketTransport: EngineTransport {
    private let session = URLSession(configuration: .ephemeral)

    /// The configured, not yet resumed task (a seam for the max-size test).
    func makeTask(info: EngineConnectionInfo) throws -> URLSessionWebSocketTask {
        guard let url = URL(string: "ws://127.0.0.1:\(info.port)") else { throw URLError(.badURL) }
        let task = session.webSocketTask(with: url)
        // The default is 1 MiB; a large snapshot or group upsert is up to MAX_SERVER_FRAME_BYTES.
        task.maximumMessageSize = maxServerFrameBytes
        return task
    }

    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection {
        let task = try makeTask(info: info)
        task.resume()
        return WebSocketConnection(task: task)
    }
}

@MainActor
final class WebSocketConnection: EngineConnection {
    private let task: URLSessionWebSocketTask

    init(task: URLSessionWebSocketTask) { self.task = task }

    func send(_ text: String) {
        let task = task
        task.send(.string(text)) { error in
            if let error {
                engineLog.error("websocket send failed: \(error.localizedDescription, privacy: .public)")
                task.cancel(with: .abnormalClosure, reason: nil)
            }
        }
    }

    func sendAndFlush(_ text: String) async {
        let task = task
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            task.send(.string(text)) { _ in cont.resume() }
        }
    }

    func receive() async throws -> String {
        switch try await task.receive() {
        case .string(let s): return s
        case .data(let d):
            guard let s = String(data: d, encoding: .utf8) else { throw URLError(.cannotParseResponse) }
            return s
        @unknown default: throw URLError(.cannotParseResponse)
        }
    }

    func close() { task.cancel(with: .goingAway, reason: nil) }
}
