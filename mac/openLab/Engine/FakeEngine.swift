// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// An in-process engine for tests and for UI tests (`-OpenLabEngine fake`). It speaks the same
/// JSON as the real one, so the whole stack above the socket runs unchanged. It answers `ok` to
/// commands and transport (it does not execute them) and can be scripted to push any frame, drop
/// connections, refuse auth or hold results.
/// Protocol 2: several workspaces, the unbound app connection, `close`. `restart()` plays a new
/// engine run (new token and pid, no doc open, doc ids from the start again).
@MainActor
final class FakeEngine: EngineTransport {
    /// The token the fake accepts in `hello`; the info provider must hand the same one out.
    var token = "fake-token"
    /// The fake's process id as `engine.json` would name it (changes with each `restart`).
    private(set) var pid: Int32 = 100
    private(set) var runs = 1
    var info: EngineConnectionInfo { EngineConnectionInfo(port: 1, token: token, pid: pid) }

    // Scripting.
    var connectFailure = false
    /// When set, `hello` is answered with this `error` and the connection closes.
    var refuseAuth: ErrorCode?
    var holdResults = false
    /// `sendAndFlush` never returns (a stalled socket), in tests of the quit path.
    var holdFlush = false
    /// Answers every request with an `ok` that does not decode (a changed server, in tests).
    var garbleResults = false
    /// Answers every `command` with `command_refused` and changes nothing.
    var refuseCommands = false
    /// The records `log_tail` answers with (show-log JSON objects), and the `relink` result data.
    var logTailEntries: [JSONValue] = []
    /// The `seq` `log_tail` answers with (the newest record's number; 0: unnumbered); `max` keeps the newest.
    var logTailSeq: Int64 = 0
    var relinkData: JSONValue = .object([
        "type": .string("relink"), "relinked": .int(0), "ambiguous": .array([]), "unmatched": .array([]),
        "truncated": .bool(false), "skipped": .int(0),
    ])

    private static let allClearReport: JSONValue = .object([
        "type": .string("preshow_check"), "ok": .bool(true),
        "summary": .object([
            "broken_cues": .int(0), "media": .int(0), "patches": .int(0), "network": .int(0),
            "network_unchecked": .int(0), "system": .int(0), "total": .int(0),
        ]),
        "broken_cues": .array([]), "media": .array([]), "patches": .array([]),
        "network": .array([]), "system": .array([]),
    ])

    /// One cue with a missing file and one lost device: `total` is 2 (the cue and the system entry).
    /// The cue is the demo show's Radio (2.3, inside the collapsed group Act One).
    private static let sampleReportWithProblems: JSONValue = {
        let cue = "00000000-0000-0000-0000-000000000023"
        let list = demoListID
        let path = "media/o.wav"
        return .object([
            "type": .string("preshow_check"), "ok": .bool(false),
            "summary": .object([
                "broken_cues": .int(1), "media": .int(1), "patches": .int(0), "network": .int(0),
                "network_unchecked": .int(0), "system": .int(1), "total": .int(2),
            ]),
            "broken_cues": .array([
                .object([
                    "cue": .string(cue), "list": .string(list), "number": .string("2.3"), "name": .string("Radio"),
                    "problems": .array([.object(["type": .string("media_missing"), "media": .string(path)])]),
                ])
            ]),
            "media": .array([
                .object(["path": .string(path), "status": .string("missing"), "cues": .array([.string(cue)])])
            ]),
            "patches": .array([]), "network": .array([]),
            "system": .array([
                .object([
                    "type": .string("notice"), "code": .string("device_lost"),
                    "message": .string("audio device \"Fake Output\" is lost"),
                ])
            ]),
        ])
    }()

    /// `preshow_check` on one of these docs answers a sample report with problems (a broken cue,
    /// its missing file, a lost device) instead of the all-clear; other docs answer the all-clear.
    var preshowProblemDocs: Set<ModelID> = []
    /// `scan_recovery` lists these (`recovery_entries` entry objects).
    var recoveryEntries: [JSONValue] = []
    /// `workspaces` lists a doc saved at one of these paths with this `path_hash`; at any other
    /// path with none, as an engine before the field did.
    var pathHashes: [String: String] = [:]
    /// `open` of one of these paths without a `recovery` choice answers `recovery_offer`.
    var autosaveNewer: Set<String> = []
    /// `open` of one of these paths fails with this code.
    var openFailures: [String: ErrorCode] = [:]
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
    init(snapshot: JSONValue? = nil) {
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

    /// A new engine run (after a crash or a restart): every connection drops, nothing is open, a
    /// new token and pid, and doc ids start again (so an old id may name another show).
    func restart() {
        dropConnections()
        runs += 1
        token = "fake-token-\(runs)"
        pid += 1
        docs = [:]
        docOrder = []
        nextDoc = 1
        undoStacks = [:]
        redoStacks = [:]
        held = []
    }

    /// Sends a `media_duration` runtime change for `cue` (M5.4d; all nil: not known now) and keeps
    /// it in the doc's snapshot, as the engine does when a probe answers or a file goes missing.
    func pushMediaDuration(
        _ cue: ModelID, file: Double?, trimmed: Double?, duration: Double?, doc: ModelID = ModelID(doc: 1)
    ) {
        guard var s = docs[doc], case .int(let q)? = s["runtime_seq"] else { return }
        var fields: [String: JSONValue] = ["cue": .string(cue.raw)]
        if let file { fields["file"] = .double(file) }
        if let trimmed { fields["trimmed"] = .double(trimmed) }
        if let duration { fields["duration"] = .double(duration) }
        var list = (s["media_durations"]?.arrayValue ?? []).filter { $0["cue"]?.stringValue != cue.raw }
        if fields.count > 1 { list.append(.object(fields)) }
        s.set("media_durations", .array(list))
        s.set("runtime_seq", .int(q + 1))
        docs[doc] = s
        fields["type"] = .string("media_duration")
        let message = JSONValue.object([
            "type": .string("runtime"), "seq": .int(q + 1), "changes": .array([.object(fields)]),
        ])
        push(text(message), doc: doc)
    }

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
        let doc = ModelID(doc: UInt64(nextDoc))
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

    /// A doc id as the engine writes it: a number.
    private static func docJSON(_ doc: ModelID) -> JSONValue { doc.docNumber.map { .uint($0) } ?? .null }

    private func workspacesList() -> JSONValue {
        .array(
            docOrder.map { doc in
                let s = docs[doc] ?? .null
                var entry: [String: JSONValue] = [
                    "doc": Self.docJSON(doc), "workspace_id": s["workspace"]?["id"] ?? .null,
                    "name": s["workspace"]?["name"] ?? .null, "path": s["document"]?["path"] ?? .null,
                    "dirty": s["document"]?["dirty"] ?? .bool(false),
                ]
                if let hash = s["document"]?["path"]?.stringValue.flatMap({ pathHashes[$0] }) {
                    entry["path_hash"] = .string(hash)
                }
                return .object(entry)
            })
    }

    private func broadcastWorkspaces() {
        let msg = text(.object(["type": .string("workspaces"), "workspaces": workspacesList()]))
        for c in connections where c.welcomed && c.doc == nil { c.deliver(msg) }
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
        case .command(let id, let command, let coalesce):
            if refuseCommands { return result(c, id, error: (.commandRefused, "refused by the fake")) }
            if let doc = c.doc {
                recordingHistory(
                    doc, structural: Self.structural.contains(command.json["type"]?.stringValue ?? ""),
                    coalesce: coalesce
                ) {
                    fakeEdit(doc, command)
                }
            }
            result(c, id)
        case .undo(let id):
            if let doc = c.doc { lastCoalesce[doc] = nil }
            if let doc = c.doc { fakeHistory(doc, undo: true) }
            result(c, id)
        case .redo(let id):
            if let doc = c.doc { lastCoalesce[doc] = nil }
            if let doc = c.doc { fakeHistory(doc, undo: false) }
            result(c, id)
        case .request(let id, let r): handleRequest(id, r, from: c)
        case .goodbye: c.close()
        case .close(let id, let doc, let discard): handleClose(id, doc: doc, discard: discard, from: c)
        case .panic(let id): result(c, id)
        case .hotkey(let id, let key, let modifiers, let up):
            result(c, id)
            if !up { startHotkeyCues(key: key, modifiers: modifiers) }
        }
    }

    /// The transport effects the fake performs, so UI tests can see them land. GO starts the demo
    /// show's loaded cue (...21); pause, resume, stop and panic move that cue's state. Documents
    /// without that cue are left alone.
    private func fakeEffect(of action: TransportAction, doc: ModelID) {
        let cue = ModelID("00000000-0000-0000-0000-000000000021")
        switch action {
        case .go: fakeState(.running, cue, doc: doc)
        case .start(let started): fakeState(.running, started, doc: doc)
        case .pause(cue): fakeState(.paused, cue, doc: doc)
        case .resume(cue): fakeState(.running, cue, doc: doc)
        case .stop(cue, _), .hardStop(cue), .panicCue(cue): fakeState(.idle, cue, doc: doc)
        case .preview(let c): fakePreview(c, active: true, doc: doc)
        case .stopPreview(let c?): fakePreview(c, active: false, doc: doc)
        case .batch(let items): for item in items { fakeEffect(of: item, doc: doc) }
        default: break
        }
    }

    private func fakePreview(_ cue: ModelID, active: Bool, doc: ModelID) {
        guard var s = docs[doc], case .int(let q)? = s["runtime_seq"], "\(s)".contains(cue.raw) else { return }
        s.set("runtime_seq", .int(q + 1))
        docs[doc] = s
        push(
            #"{"type":"runtime","seq":\#(q + 1),"changes":[{"type":"preview","cue":"\#(cue.raw)","active":\#(active)}]}"#,
            doc: doc)
    }

    private func fakeState(_ state: WireCueState, _ cue: ModelID, doc: ModelID) {
        guard var s = docs[doc], case .int(let q)? = s["runtime_seq"], "\(s)".contains(cue.raw) else { return }
        s.set("runtime_seq", .int(q + 1))
        docs[doc] = s
        push(
            #"{"type":"runtime","seq":\#(q + 1),"changes":[{"type":"cue_state","cue":"\#(cue.raw)","state":"\#(state.rawValue)"}]}"#,
            doc: doc)
    }

    /// The edits the fake performs so UI tests can see them land: `set_cue_property` (top-level
    /// fields, `target`) and `set_cue_numbers`. Anything else is only recorded. The workspace
    /// changes and a `model_changed` goes out, as the engine does before its `result`.
    private func fakeEdit(_ doc: ModelID, _ command: Command) {
        guard var s = docs[doc], case .int(let rev)? = s["revision"],
            var lists = s["workspace"]?["cue_lists"]?.arrayValue
        else { return }
        if Self.structural.contains(command.json["type"]?.stringValue ?? "") {
            // Adds, removes and moves are answered with a whole `snapshot` (the fake keeps no diff).
            guard Self.applyStructural(command.json, to: &lists) else { return }
            s["workspace"]?.set("cue_lists", .array(lists))
            s.set("revision", .int(rev + 1))
            docs[doc] = s
            return
        }
        var edits: [(String, (inout JSONValue) -> Void)] = []
        switch command.json["type"]?.stringValue {
        case "set_cue_property":
            guard let id = command.json["id"]?.stringValue, let p = command.json["property"],
                let type = p["type"]?.stringValue, let value = p["value"]
            else { return }
            edits.append(
                (
                    id,
                    { cue in
                        switch type {
                        case "target": cue["kind"]?.set("target", value)
                        case "group_mode": cue["kind"]?.set("mode", value)
                        case "media", "duration", "load_to_time": cue["kind"]?.set(type, value)
                        default: cue.set(type, value)
                        }
                    }
                ))
        case "set_cue_numbers":
            for n in command.json["numbers"]?.arrayValue ?? [] {
                guard let id = n["id"]?.stringValue, let number = n["number"] else { return }
                edits.append((id, { $0.set("number", number) }))
            }
        default: return
        }
        var upserts: [JSONValue] = []
        for (id, edit) in edits {
            for li in lists.indices {
                var cues = lists[li]["cues"]?.arrayValue ?? []
                guard let hit = Self.edit(id, in: &cues, parent: nil, apply: edit) else { continue }
                lists[li].set("cues", .array(cues))
                upserts.append(
                    .object([
                        "type": .string("cue"), "list": lists[li]["id"] ?? .null,
                        "parent": hit.parent.map { .string($0) } ?? .null, "index": .int(Int64(hit.index)),
                        "cue": hit.cue,
                    ]))
            }
        }
        guard !upserts.isEmpty else { return }
        s["workspace"]?.set("cue_lists", .array(lists))
        s.set("revision", .int(rev + 1))
        docs[doc] = s
        push(
            text(
                .object([
                    "type": .string("model_changed"), "revision": .int(rev + 1), "upserts": .array(upserts),
                    "deletes": .array([]),
                ])), doc: doc)
    }

    // MARK: Undo and redo (the fake keeps whole cue-list states)

    private var undoStacks: [ModelID: [JSONValue]] = [:]
    private var redoStacks: [ModelID: [JSONValue]] = [:]

    /// The coalesce key of the last recorded edit: an edit with the same key joins its undo step
    /// (docs/commands.md "Coalescing"). Any other edit, undo or redo ends the gesture.
    private var lastCoalesce: [ModelID: String] = [:]

    /// Runs an edit; if it changed the revision, keeps the state before it for undo and, for the
    /// structural ones, sends the new state as a `snapshot`.
    private func recordingHistory(_ doc: ModelID, structural: Bool, coalesce: String?, _ edit: () -> Void) {
        let before = docs[doc]?["workspace"]?["cue_lists"] ?? .null
        let rev = docs[doc]?["revision"]
        edit()
        guard docs[doc]?["revision"] != rev else { return }  // a no-op edit leaves the gesture open
        let joins = coalesce != nil && lastCoalesce[doc] == coalesce && !(undoStacks[doc] ?? []).isEmpty
        lastCoalesce[doc] = coalesce
        if !joins { undoStacks[doc, default: []].append(before) }
        redoStacks[doc] = []
        publishHistory(doc, snapshot: structural)
    }

    private func publishHistory(_ doc: ModelID, snapshot: Bool) {
        let old = docs[doc]?["document"]
        docs[doc]?["document"]?.set("dirty", .bool(true))  // an edit, undo or redo leaves unsaved changes
        docs[doc]?["document"]?.set("can_undo", .bool(!(undoStacks[doc] ?? []).isEmpty))
        docs[doc]?["document"]?.set("can_redo", .bool(!(redoStacks[doc] ?? []).isEmpty))
        if snapshot {
            push(snapshotMessage(doc), doc: doc)
        } else if let now = docs[doc]?["document"],
            now["can_undo"] != old?["can_undo"] || now["can_redo"] != old?["can_redo"]
        {
            // Only a change in what can be undone is announced, as the engine does for a plain edit.
            push(text(.object(["type": .string("document"), "document": now])), doc: doc)
        }
    }

    private func fakeHistory(_ doc: ModelID, undo: Bool) {
        guard var s = docs[doc], case .int(let rev)? = s["revision"],
            let target = undo ? undoStacks[doc]?.popLast() : redoStacks[doc]?.popLast()
        else { return }
        let now = s["workspace"]?["cue_lists"] ?? .null
        if undo { redoStacks[doc, default: []].append(now) } else { undoStacks[doc, default: []].append(now) }
        s["workspace"]?.set("cue_lists", target)
        s.set("revision", .int(rev + 1))
        docs[doc] = s
        publishHistory(doc, snapshot: true)
    }

    private static let structural: Set<String> = [
        "add_cue", "paste", "paste_clipboard", "remove_cue", "move_cue", "reparent_cue", "transaction", "renumber",
    ]

    /// Applies one structural command (or a transaction of them) to `lists`; false when it cannot.
    private static func applyStructural(_ c: JSONValue, to lists: inout [JSONValue]) -> Bool {
        switch c["type"]?.stringValue {
        case "transaction":
            var copy = lists
            for inner in c["commands"]?.arrayValue ?? [] where !applyStructural(inner, to: &copy) { return false }
            lists = copy
            return true
        case "add_cue":
            guard let cue = c["cue"], let at = c["at"] else { return false }
            return insert(cue, at: at, into: &lists)
        case "paste", "paste_clipboard":
            guard var cues = c["cues"]?.arrayValue, var at = c["at"] else { return false }
            // The real engine runs `prepare_paste`; the fake only gives the copies fresh ids.
            if c["type"]?.stringValue == "paste_clipboard" { cues = cues.map(withFreshIDs) }
            guard case .int(let start)? = at["index"] else { return false }
            var copy = lists
            for (n, cue) in cues.enumerated() {
                at.set("index", .int(start + Int64(n)))
                guard insert(cue, at: at, into: &copy) else { return false }
            }
            lists = copy
            return true
        case "remove_cue":
            guard let id = c["id"]?.stringValue else { return false }
            return take(id, from: &lists) != nil
        case "move_cue", "reparent_cue":
            guard let id = c["id"]?.stringValue, let (cue, place) = take(id, from: &lists) else { return false }
            if c["type"]?.stringValue == "reparent_cue", let to = c["to"] { return insert(cue, at: to, into: &lists) }
            var at = place  // move_cue stays in the cue's own parent
            at.set("index", c["index"] ?? .int(0))
            return insert(cue, at: at, into: &lists)
        default: return false
        }
    }

    private static func withFreshIDs(_ cue: JSONValue) -> JSONValue {
        var cue = cue
        cue.set("id", .string(UUID().uuidString.lowercased()))
        if var kind = cue["kind"], let kids = kind["children"]?.arrayValue {
            kind.set("children", .array(kids.map(withFreshIDs)))
            cue.set("kind", kind)
        }
        return cue
    }

    private static func children(_ cue: JSONValue) -> [JSONValue] { cue["kind"]?["children"]?.arrayValue ?? [] }

    /// The engine's app-scope matching, so UI tests see a hotkey land: every doc's cues whose
    /// `triggers.hotkey` is exactly this key and these modifiers go to running.
    private func startHotkeyCues(key: String, modifiers: HotkeyModifiers) {
        let mods: JSONValue = .object([
            "shift": .bool(modifiers.shift), "control": .bool(modifiers.control), "alt": .bool(modifiers.alt),
            "command": .bool(modifiers.command),
        ])
        func walk(_ cues: [JSONValue], doc: ModelID) {
            for cue in cues {
                if let hk = cue["triggers"]?["hotkey"], hk["key"]?.stringValue == key, hk["modifiers"] == mods,
                    let id = cue["id"]?.stringValue
                {
                    fakeState(.running, ModelID(id), doc: doc)
                }
                walk(Self.children(cue), doc: doc)
            }
        }
        for doc in docOrder {
            for list in docs[doc]?["workspace"]?["cue_lists"]?.arrayValue ?? [] {
                walk(list["cues"]?.arrayValue ?? [], doc: doc)
            }
        }
    }

    /// Removes the cue; also where it was, as a position object without an index.
    private static func take(_ id: String, from lists: inout [JSONValue]) -> (JSONValue, JSONValue)? {
        for li in lists.indices {
            var cues = lists[li]["cues"]?.arrayValue ?? []
            if let found = take(id, from: &cues, parent: nil, list: lists[li]["id"] ?? .null) {
                lists[li].set("cues", .array(cues))
                return found
            }
        }
        return nil
    }

    private static func take(
        _ id: String, from cues: inout [JSONValue], parent: String?, list: JSONValue
    ) -> (JSONValue, JSONValue)? {
        if let i = cues.firstIndex(where: { $0["id"]?.stringValue == id }) {
            return (cues.remove(at: i), .object(["list": list, "parent": parent.map { .string($0) } ?? .null]))
        }
        for i in cues.indices {
            var kids = children(cues[i])
            if kids.isEmpty { continue }
            if let found = take(id, from: &kids, parent: cues[i]["id"]?.stringValue, list: list) {
                cues[i]["kind"]?.set("children", .array(kids))
                return found
            }
        }
        return nil
    }

    private static func insert(_ cue: JSONValue, at: JSONValue, into lists: inout [JSONValue]) -> Bool {
        guard let li = lists.firstIndex(where: { $0["id"] == at["list"] }), case .int(let n)? = at["index"] else {
            return false
        }
        var cues = lists[li]["cues"]?.arrayValue ?? []
        guard insert(cue, parent: at["parent"]?.stringValue, index: Int(n), into: &cues) else { return false }
        lists[li].set("cues", .array(cues))
        return true
    }

    private static func insert(_ cue: JSONValue, parent: String?, index: Int, into cues: inout [JSONValue]) -> Bool {
        guard let parent else {
            cues.insert(cue, at: min(max(index, 0), cues.count))
            return true
        }
        for i in cues.indices {
            if cues[i]["id"]?.stringValue == parent {
                var kids = children(cues[i])
                kids.insert(cue, at: min(max(index, 0), kids.count))
                cues[i]["kind"]?.set("children", .array(kids))
                return true
            }
            var kids = children(cues[i])
            if !kids.isEmpty, insert(cue, parent: parent, index: index, into: &kids) {
                cues[i]["kind"]?.set("children", .array(kids))
                return true
            }
        }
        return false
    }

    /// Applies `edit` to the cue `id` anywhere in `cues`; the edited cue (with its children, as the
    /// engine sends it) and where it sits.
    private static func edit(
        _ id: String, in cues: inout [JSONValue], parent: String?, apply edit: (inout JSONValue) -> Void
    ) -> (cue: JSONValue, parent: String?, index: Int)? {
        for i in cues.indices {
            if cues[i]["id"]?.stringValue == id {
                edit(&cues[i])
                return (cues[i], parent, i)
            }
            if var kids = cues[i]["kind"]?["children"]?.arrayValue,
                let hit = Self.edit(id, in: &kids, parent: cues[i]["id"]?.stringValue, apply: edit)
            {
                cues[i]["kind"]?.set("children", .array(kids))
                return hit
            }
        }
        return nil
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
        if h.workspace == nil {
            c.deliver(
                text(
                    .object([
                        "type": .string("welcome"), "protocol_version": .int(Int64(protocolVersion)),
                        "workspaces": workspacesList(),
                    ])))
            return
        }
        let doc = h.workspace.flatMap { docs[$0] != nil ? $0 : nil }
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
        case .new:
            open(
                id, snapshot: Self.emptySnapshot(name: "Untitled \(nextDoc)", workspaceID: workspaceID(nextDoc)),
                path: nil, from: c)
        case .open(let path, let recovery):
            if let existing = docOrder.first(where: {
                docs[$0]?["document"]?["path"]?.stringValue.map { EngineSession.samePath($0, path) } ?? false
            }) {
                return result(c, id, error: (.alreadyOpen, "already open as \(existing)"))
            }
            if let code = openFailures[path] { return result(c, id, error: (code, "cannot open \(path)")) }
            let name = ((path as NSString).lastPathComponent as NSString).deletingPathExtension
            if autosaveNewer.contains(path), recovery == nil {
                let offer: JSONValue = .object([
                    "type": .string("recovery_offer"), "path": .string(path), "workspace_name": .string(name),
                    "modified_ms": .int(1_700_000_000_000),
                ])
                return result(c, id, ok: .object(["data": offer]))
            }
            var snapshot = Self.emptySnapshot(name: name, workspaceID: workspaceID(nextDoc))
            if recovery == .restore { snapshot["document"]?.set("dirty", .bool(true)) }
            open(id, snapshot: snapshot, path: path, from: c)
        case .openRecovery(let workspaceId, _, _):
            var snapshot = Self.emptySnapshot(name: "Recovered", workspaceID: workspaceId.raw)
            snapshot["document"]?.set("dirty", .bool(true))
            open(id, snapshot: snapshot, path: nil, from: c)
        case .save, .saveAs:
            guard let doc = c.doc else { return result(c, id, error: (.noWorkspace, "unbound")) }
            if case .saveAs(let path) = r { docs[doc]?["document"]?.set("path", .string(path)) }
            docs[doc]?["document"]?.set("dirty", .bool(false))
            push(text(.object(["type": .string("document"), "document": docs[doc]?["document"] ?? .null])), doc: doc)
            result(c, id)
        case .setOutputOverrides: result(c, id)
        case .setPatchMapping: result(c, id)
        case .setShowMode(let on):
            guard let doc = c.doc else { return result(c, id, error: (.noWorkspace, "unbound")) }
            docs[doc]?["document"]?.set("show_mode", .bool(on))
            push(text(.object(["type": .string("document"), "document": docs[doc]?["document"] ?? .null])), doc: doc)
            result(c, id)
        case .scanRecovery:
            result(
                c, id,
                ok: .object([
                    "data": .object(["type": .string("recovery_entries"), "entries": .array(recoveryEntries)])
                ]))
        case .relink:
            result(
                c, id,
                ok: .object(["data": relinkData]))
        case .logTail(let max):
            let sent = max.map { Array(logTailEntries.suffix(Int($0))) } ?? logTailEntries
            result(
                c, id,
                ok: .object([
                    "data": .object([
                        "type": .string("log_tail"), "entries": .array(sent), "dropped": .int(0),
                        "seq": .int(logTailSeq),
                    ])
                ]))
        case .preshowCheck:
            let problems = c.doc.map(preshowProblemDocs.contains) ?? false
            result(c, id, ok: .object(["data": problems ? Self.sampleReportWithProblems : Self.allClearReport]))
        case .listDevices:
            let dev: JSONValue = .object(["name": .string("Fake Output"), "output_channels": .int(2)])
            result(c, id, ok: .object(["data": .object(["type": .string("devices"), "devices": .array([dev])])]))
        }
    }

    private func workspaceID(_ n: Int) -> String { String(format: "00000000-0000-0000-0000-%012x", n) }

    /// `new` / `open` / `open_recovery`: another document; the result goes out before the list, as
    /// the engine does (control/docs.rs publishes the workspaces after answering).
    private func open(_ id: UInt64, snapshot: JSONValue, path: String?, from c: FakeConnection) {
        let doc = addDoc(snapshot, path: path)
        result(c, id, ok: .object(["data": .object(["type": .string("opened"), "doc": Self.docJSON(doc)])]))
        broadcastWorkspaces()
    }

    private func handleClose(_ id: UInt64, doc: ModelID?, discard: Bool, from c: FakeConnection) {
        guard let target = doc ?? c.doc, docs[target] != nil else {
            return result(c, id, error: (.noWorkspace, "no such workspace"))
        }
        if docs[target]?["document"]?["dirty"] == .bool(true) && !discard {
            return result(c, id, error: (.unsavedChanges, "unsaved changes"))
        }
        result(c, id)
        docs[target] = nil
        docOrder.removeAll { $0 == target }
        let closed = text(.object(["type": .string("closed"), "doc": Self.docJSON(target)]))
        for conn in connections(of: target) {
            conn.deliver(closed)
            conn.close()
        }
        broadcastWorkspaces()
    }

    /// Marks a document dirty or renames it, as an edit would, and tells the app connections.
    /// Before any connection: no workspace open (an engine just started).
    func closeAll() {
        docs = [:]
        docOrder = []
    }

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
