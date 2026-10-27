// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// An in-process engine for tests and for UI tests (`-OpenLabEngine fake`). It speaks the same
/// JSON as the real one, so the whole stack above the socket runs unchanged. It answers `ok` to
/// commands and transport (it does not execute them) and can be scripted to push any frame, drop
/// connections, refuse auth or hold results.
///
/// `.multi` follows the M4.15 design (several workspaces, unbound app connection, `close`);
/// `.single` behaves like today's engine (`new` / `open` replace the one workspace in place).
@MainActor
final class FakeEngine: EngineTransport {
    enum Mode { case single, multi }

    let mode: Mode
    /// The token the fake accepts in `hello`; the info provider must hand the same one out.
    var token = "fake-token"
    var info: EngineConnectionInfo { EngineConnectionInfo(port: 1, token: token, pid: 0) }

    // Scripting.
    var connectFailure = false
    /// When set, `hello` is answered with this `error` and the connection closes.
    var refuseAuth: ErrorCode?
    var holdResults = false
    /// `sendAndFlush` never returns (a stalled socket), in tests of the quit path.
    var holdFlush = false
    /// Answers every request with an `ok` that does not decode (a changed server, in tests).
    var garbleResults = false
    enum WelcomeFault { case none, silent, garbled }
    /// `silent`: the `welcome` is never sent; `garbled`: it is sent but does not decode.
    var welcomeFault = WelcomeFault.none

    // Observation.
    private(set) var received: [ClientMessage] = []
    /// The same messages with the doc of the connection that sent them (nil: the app connection).
    private(set) var receivedFrom: [(doc: ModelID?, message: ClientMessage)] = []
    private(set) var connectCount = 0
    private(set) var connections: [FakeConnection] = []

    private(set) var docs: [ModelID: JSONValue] = [:]
    private var docOrder: [ModelID] = []
    private var nextDoc = 1
    private var held: [(FakeConnection, JSONValue)] = []

    /// A one-cue-list workspace; `snapshot` (the object under `welcome.snapshot` in the fixtures)
    /// replaces it for the first document.
    init(mode: Mode = .multi, snapshot: JSONValue? = nil) {
        self.mode = mode
        _ = addDoc(snapshot ?? Self.bundledSnapshot() ?? Self.emptySnapshot(name: "Untitled"), path: nil)
    }

    /// `welcome.json` from the app bundle (copied from the protocol fixtures at build time).
    static func bundledSnapshot() -> JSONValue? {
        guard let url = Bundle.main.url(forResource: "welcome", withExtension: "json"),
            let text = try? String(contentsOf: url, encoding: .utf8)
        else { return nil }
        return JSONValue.parse(text)?["snapshot"]
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

    // MARK: Scripting API

    /// Delivers a raw frame to the connections bound to `doc`, or to every connection when nil.
    func push(_ text: String, doc: ModelID? = nil) {
        for c in connections where c.welcomed && (doc == nil || c.doc == doc) { c.deliver(text) }
    }

    func dropConnections() { for c in connections { c.close() } }

    func releaseResults(reversed: Bool = false) {
        let all = reversed ? held.reversed() : held
        held = []
        for (c, v) in all { c.deliver(text(v)) }
    }

    var openDocs: [ModelID] { docOrder }

    func connections(of doc: ModelID) -> [FakeConnection] { connections.filter { $0.doc == doc } }

    /// How many `request` messages of this kind the engine has received.
    func requestCount(_ matches: (Request) -> Bool) -> Int {
        received.filter { if case .request(_, let r) = $0 { return matches(r) } else { return false } }.count
    }

    // MARK: EngineTransport

    func connect(info: EngineConnectionInfo) async throws -> any EngineConnection {
        connectCount += 1
        if connectFailure { throw URLError(.cannotConnectToHost) }
        let c = FakeConnection(engine: self)
        connections.append(c)
        return c
    }

    fileprivate func remove(_ c: FakeConnection) { connections.removeAll { $0 === c } }

    // MARK: Server side

    func addDoc(_ snapshot: JSONValue, path: String?) -> ModelID {
        let doc = ModelID("doc-\(nextDoc)")
        nextDoc += 1
        var s = snapshot
        if let path {
            s["document"]?.set("path", .string(path))
        }
        docs[doc] = s
        docOrder.append(doc)
        return doc
    }

    private func text(_ v: JSONValue) -> String { (try? v.text()) ?? "{}" }

    private func workspacesList() -> JSONValue {
        .array(
            docOrder.map { doc in
                let s = docs[doc] ?? .null
                return .object([
                    "doc": .string(doc.raw), "workspace_id": s["workspace"]?["id"] ?? .null,
                    "name": s["workspace"]?["name"] ?? .null, "path": s["document"]?["path"] ?? .null,
                    "dirty": s["document"]?["dirty"] ?? .bool(false),
                ])
            })
    }

    private func broadcastWorkspaces() {
        let msg = text(.object(["type": .string("workspaces"), "workspaces": workspacesList()]))
        for c in connections where c.welcomed && c.doc == nil && mode == .multi { c.deliver(msg) }
    }

    private func result(
        _ c: FakeConnection, _ id: UInt64, ok: JSONValue = .object([:]), error: (ErrorCode, String)? = nil
    ) {
        var v: JSONValue = .object([
            "type": .string("result"), "id": id <= UInt64(Int64.max) ? .int(Int64(id)) : .uint(id),
        ])
        if let error {
            v.set("error", .object(["code": .string(error.0.raw), "message": .string(error.1)]))
        } else {
            v.set("ok", ok)
        }
        if garbleResults { v.set("ok", .string("garbled")) }
        if holdResults { held.append((c, v)) } else { c.deliver(text(v)) }
    }

    private func snapshotMessage(_ doc: ModelID) -> String {
        text(.object(["type": .string("snapshot"), "snapshot": docs[doc] ?? .null]))
    }

    private func fail(_ c: FakeConnection, _ code: ErrorCode, _ message: String, close: Bool) {
        c.deliver(
            text(
                .object([
                    "type": .string("error"), "code": .string(code.raw), "message": .string(message),
                    "protocol_version": .int(Int64(protocolVersion)),
                ])))
        if close { c.close() }
    }

    fileprivate func handle(_ raw: String, from c: FakeConnection) {
        guard let message = try? JSONDecoder().decode(ClientMessage.self, from: Data(raw.utf8)) else {
            return fail(c, .badRequest, "unreadable frame", close: false)
        }
        received.append(message)
        receivedFrom.append((c.doc, message))
        guard c.welcomed else {
            guard case .hello(let h) = message else { return fail(c, .badToken, "hello first", close: true) }
            return handleHello(h, from: c)
        }
        switch message {
        case .hello: fail(c, .badRequest, "second hello", close: false)
        case .transport(let id, let action):
            result(c, id)
            if let doc = c.doc { fakeEffect(of: action, doc: doc) }
        case .command(let id, _), .undo(let id), .redo(let id): result(c, id)
        case .request(let id, let r): handleRequest(id, r, from: c)
        case .goodbye: c.close()
        case .provisional(.close(let id, let doc, let discard)): handleClose(id, doc: doc, discard: discard, from: c)
        }
    }

    /// The transport effects the fake performs, so UI tests can see them land. GO starts the demo
    /// show's loaded cue (...21); pause, resume, stop and panic move that cue's state. Documents
    /// without that cue are left alone.
    private func fakeEffect(of action: TransportAction, doc: ModelID) {
        let cue = ModelID("00000000-0000-0000-0000-000000000021")
        switch action {
        case .go: fakeState(.running, cue, doc: doc)
        case .pause(cue): fakeState(.paused, cue, doc: doc)
        case .resume(cue): fakeState(.running, cue, doc: doc)
        case .stop(cue, _), .hardStop(cue), .panicCue(cue): fakeState(.idle, cue, doc: doc)
        default: break
        }
    }

    private func fakeState(_ state: WireCueState, _ cue: ModelID, doc: ModelID) {
        guard var s = docs[doc], case .int(let q)? = s["runtime_seq"], "\(s)".contains(cue.raw) else { return }
        s.set("runtime_seq", .int(q + 1))
        docs[doc] = s
        push(
            #"{"type":"runtime","seq":\#(q + 1),"changes":[{"type":"cue_state","cue":"\#(cue.raw)","state":"\#(state.rawValue)"}]}"#,
            doc: doc)
    }

    private func handleHello(_ h: Hello, from c: FakeConnection) {
        if let code = refuseAuth { return fail(c, code, "refused", close: true) }
        if h.protocolVersion != protocolVersion { return fail(c, .protocolVersionMismatch, "version", close: true) }
        if h.token != token { return fail(c, .badToken, "token", close: true) }
        c.welcomed = true
        switch welcomeFault {
        case .none: break
        case .silent: return
        case .garbled: return c.deliver(#"{"type":"welcome","protocol_version":"x","snapshot":7}"#)
        }
        if mode == .multi, h.workspace == nil {
            c.deliver(
                text(
                    .object([
                        "type": .string("welcome"), "protocol_version": .int(Int64(protocolVersion)),
                        "workspaces": workspacesList(),
                    ])))
            return
        }
        let doc = (mode == .single ? docOrder.first : h.workspace).flatMap { docs[$0] != nil ? $0 : nil }
        guard let doc else { return fail(c, .noWorkspace, "no such workspace", close: true) }
        c.doc = doc
        c.deliver(
            text(
                .object([
                    "type": .string("welcome"), "protocol_version": .int(Int64(protocolVersion)),
                    "snapshot": docs[doc] ?? .null,
                ])))
    }

    private func handleRequest(_ id: UInt64, _ r: Request, from c: FakeConnection) {
        switch r {
        case .snapshot:
            guard let doc = c.doc else { return result(c, id, error: (.noWorkspace, "unbound")) }
            c.deliver(snapshotMessage(doc))
            result(c, id)
        case .new(let discard):
            open(
                id, snapshot: Self.emptySnapshot(name: "Untitled \(nextDoc)", workspaceID: workspaceID(nextDoc)),
                path: nil, discard: discard, from: c)
        case .open(let path, let discard, _):
            if mode == .multi,
                let existing = docOrder.first(where: { docs[$0]?["document"]?["path"]?.stringValue == path })
            {
                return result(c, id, error: (.alreadyOpen, "already open as \(existing)"))
            }
            let name = ((path as NSString).lastPathComponent as NSString).deletingPathExtension
            open(
                id, snapshot: Self.emptySnapshot(name: name, workspaceID: workspaceID(nextDoc)), path: path,
                discard: discard, from: c)
        case .openRecovery(_, _, _, let discard):
            open(
                id, snapshot: Self.emptySnapshot(name: "Recovered", workspaceID: workspaceID(nextDoc)), path: nil,
                discard: discard, from: c)
        case .save, .saveAs:
            guard let doc = c.doc else { return result(c, id, error: (.noWorkspace, "unbound")) }
            if case .saveAs(let path) = r { docs[doc]?["document"]?.set("path", .string(path)) }
            docs[doc]?["document"]?.set("dirty", .bool(false))
            push(text(.object(["type": .string("document"), "document": docs[doc]?["document"] ?? .null])), doc: doc)
            result(c, id)
        case .revert: result(c, id)
        case .scanRecovery:
            result(c, id, ok: .object(["data": .object(["type": .string("recovery_entries"), "entries": .array([])])]))
        case .relink:
            result(
                c, id,
                ok: .object([
                    "data": .object([
                        "type": .string("relink"), "relinked": .int(0), "ambiguous": .array([]),
                        "unmatched": .array([]),
                    ])
                ]))
        case .logTail:
            result(
                c, id,
                ok: .object(["data": .object(["type": .string("log_tail"), "entries": .array([]), "dropped": .int(0)])])
            )
        case .listDevices:
            let dev: JSONValue = .object(["name": .string("Fake Output"), "output_channels": .int(2)])
            result(c, id, ok: .object(["data": .object(["type": .string("devices"), "devices": .array([dev])])]))
        }
    }

    private func workspaceID(_ n: Int) -> String { String(format: "00000000-0000-0000-0000-%012x", n) }

    /// `new` / `open` / `open_recovery`: a new document (multi) or an in-place replacement (single).
    private func open(_ id: UInt64, snapshot: JSONValue, path: String?, discard: Bool, from c: FakeConnection) {
        if mode == .multi {
            let doc = addDoc(snapshot, path: path)
            broadcastWorkspaces()
            result(c, id, ok: .object(["data": .object(["type": .string("opened"), "doc": .string(doc.raw)])]))
            return
        }
        guard let doc = docOrder.first else { return result(c, id, error: (.internal, "no document")) }
        if docs[doc]?["document"]?["dirty"] == .bool(true) && !discard {
            return result(c, id, error: (.unsavedChanges, "unsaved changes"))
        }
        // Numbering never restarts: the replacement is a later revision.
        var s = snapshot
        if case .int(let r)? = docs[doc]?["revision"] { s.set("revision", .int(r + 1)) }
        if case .int(let q)? = docs[doc]?["runtime_seq"] { s.set("runtime_seq", .int(q)) }
        if let path { s["document"]?.set("path", .string(path)) }
        docs[doc] = s
        let msg = snapshotMessage(doc)
        for conn in connections where conn.welcomed { conn.deliver(msg) }
        result(c, id)
    }

    private func handleClose(_ id: UInt64, doc: ModelID?, discard: Bool, from c: FakeConnection) {
        guard mode == .multi else { return fail(c, .unknownMessage, "close", close: false) }
        guard let target = doc ?? c.doc, docs[target] != nil else {
            return result(c, id, error: (.noWorkspace, "no such workspace"))
        }
        if docs[target]?["document"]?["dirty"] == .bool(true) && !discard {
            return result(c, id, error: (.unsavedChanges, "unsaved changes"))
        }
        result(c, id)
        docs[target] = nil
        docOrder.removeAll { $0 == target }
        let closed = text(.object(["type": .string("closed"), "doc": .string(target.raw)]))
        for conn in connections(of: target) {
            conn.deliver(closed)
            conn.close()
        }
        broadcastWorkspaces()
    }

    /// Marks a document dirty or renames it, as an edit would, and tells the app connections.
    func setDocument(_ doc: ModelID, dirty: Bool) {
        docs[doc]?["document"]?.set("dirty", .bool(dirty))
        broadcastWorkspaces()
    }
}

@MainActor
final class FakeConnection: EngineConnection {
    private weak var engine: FakeEngine?
    fileprivate(set) var welcomed = false
    /// The workspace this connection is bound to; nil for an app connection.
    fileprivate(set) var doc: ModelID?
    private(set) var isClosed = false
    private var queue: [String] = []
    private var waiter: CheckedContinuation<String, Error>?

    fileprivate init(engine: FakeEngine) { self.engine = engine }

    func deliver(_ text: String) {
        guard !isClosed else { return }
        if let w = waiter {
            waiter = nil
            w.resume(returning: text)
        } else {
            queue.append(text)
        }
    }

    func send(_ text: String) {
        guard !isClosed else { return }
        engine?.handle(text, from: self)
    }

    func sendAndFlush(_ text: String) async {
        send(text)
        if engine?.holdFlush == true { await withCheckedContinuation { (_: CheckedContinuation<Void, Never>) in } }
    }

    func receive() async throws -> String {
        if !queue.isEmpty { return queue.removeFirst() }
        if isClosed { throw URLError(.networkConnectionLost) }
        return try await withCheckedThrowingContinuation { waiter = $0 }
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        engine?.remove(self)
        // Frames queued before the close (an `error` or `closed`) are still delivered first.
        if queue.isEmpty, let w = waiter {
            waiter = nil
            w.resume(throwing: URLError(.networkConnectionLost))
        }
    }
}
