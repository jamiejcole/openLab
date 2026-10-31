// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

// Client -> engine messages (docs/protocol.md section 3; Rust: openlab-core::protocol::client).
// Encode and decode, so the fixture tests can round-trip every sample. Adding a transport action
// or request is one case, one line in `wire`, one line in `init(from:)` and one fixture.

/// One encodable field of a tagged message. Optional values encode as `null` unless dropped.
private typealias Fields = [(String, any Encodable)]

private func encodeTagged(_ tag: String, _ fields: Fields, to encoder: Encoder) throws {
    var c = encoder.container(keyedBy: Key.self)
    try c.encode(tag, forKey: "type")
    for (k, v) in fields { try c.encode(v, forKey: Key(k)) }
}

/// A model edit: core's `Command` (docs/commands.md). Opaque until the editing tasks give it
/// types; it round-trips exactly.
struct Command: Hashable, Sendable, Codable {
    var json: JSONValue
    init(_ json: JSONValue) { self.json = json }
    init(from decoder: Decoder) throws { json = try JSONValue(from: decoder) }
    func encode(to encoder: Encoder) throws { try json.encode(to: encoder) }
}

enum TransportAction: Hashable, Sendable, Codable {
    case go(list: ModelID)
    case panic
    case pauseAll
    case resumeAll
    case setPlayhead(list: ModelID, cue: ModelID?)
    case setPlayheadByNumber(list: ModelID, number: String)
    case playheadNext(list: ModelID)
    case playheadPrevious(list: ModelID)
    case start(cue: ModelID)
    case stop(cue: ModelID, fadeOut: FadeSpec?)
    case hardStop(cue: ModelID)
    case pause(cue: ModelID)
    case resume(cue: ModelID)
    case load(cue: ModelID, at: Double)
    case reset(cue: ModelID)
    case triggerReleased(cue: ModelID)
    case preview(cue: ModelID)
    case stopPreview(cue: ModelID?)
    case goFrom(list: ModelID, cue: ModelID)
    case stopAll
    case hardStopAll
    case panicCue(cue: ModelID)
    /// Per-cue actions the engine carries out together, at one scheduling time (Start Selected
    /// and the like, NFR-21): 1 to `maxBatch`, no cue twice.
    indirect case batch(actions: [TransportAction])

    /// The engine's `MAX_BATCH_ACTIONS`: a longer batch is refused whole.
    static let maxBatch = 4096

    private var wire: (String, Fields) {
        switch self {
        case .go(let l): return ("go", [("list", l)])
        case .panic: return ("panic", [])
        case .pauseAll: return ("pause_all", [])
        case .resumeAll: return ("resume_all", [])
        case .setPlayhead(let l, let c): return ("set_playhead", [("list", l), ("cue", c)])
        case .setPlayheadByNumber(let l, let n): return ("set_playhead_by_number", [("list", l), ("number", n)])
        case .playheadNext(let l): return ("playhead_next", [("list", l)])
        case .playheadPrevious(let l): return ("playhead_previous", [("list", l)])
        case .start(let c): return ("start", [("cue", c)])
        case .stop(let c, let f): return ("stop", [("cue", c), ("fade_out", f)])
        case .hardStop(let c): return ("hard_stop", [("cue", c)])
        case .pause(let c): return ("pause", [("cue", c)])
        case .resume(let c): return ("resume", [("cue", c)])
        case .load(let c, let at): return ("load", [("cue", c), ("at", at)])
        case .reset(let c): return ("reset", [("cue", c)])
        case .triggerReleased(let c): return ("trigger_released", [("cue", c)])
        case .preview(let c): return ("preview", [("cue", c)])
        case .stopPreview(let c): return ("stop_preview", [("cue", c)])
        case .goFrom(let l, let c): return ("go_from", [("list", l), ("cue", c)])
        case .stopAll: return ("stop_all", [])
        case .hardStopAll: return ("hard_stop_all", [])
        case .panicCue(let c): return ("panic_cue", [("cue", c)])
        case .batch(let a): return ("batch", [("actions", a)])
        }
    }

    func encode(to encoder: Encoder) throws { try encodeTagged(wire.0, wire.1, to: encoder) }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        func list() throws -> ModelID { try c.decode(ModelID.self, forKey: "list") }
        func cue() throws -> ModelID { try c.decode(ModelID.self, forKey: "cue") }
        switch try c.decode(String.self, forKey: "type") {
        case "go": self = .go(list: try list())
        case "panic": self = .panic
        case "pause_all": self = .pauseAll
        case "resume_all": self = .resumeAll
        case "set_playhead":
            self = .setPlayhead(list: try list(), cue: try c.decodeIfPresent(ModelID.self, forKey: "cue"))
        case "set_playhead_by_number":
            self = .setPlayheadByNumber(list: try list(), number: try c.decode(String.self, forKey: "number"))
        case "playhead_next": self = .playheadNext(list: try list())
        case "playhead_previous": self = .playheadPrevious(list: try list())
        case "start": self = .start(cue: try cue())
        case "stop": self = .stop(cue: try cue(), fadeOut: try c.decodeIfPresent(FadeSpec.self, forKey: "fade_out"))
        case "hard_stop": self = .hardStop(cue: try cue())
        case "pause": self = .pause(cue: try cue())
        case "resume": self = .resume(cue: try cue())
        case "load": self = .load(cue: try cue(), at: try c.decode(Double.self, forKey: "at"))
        case "reset": self = .reset(cue: try cue())
        case "trigger_released": self = .triggerReleased(cue: try cue())
        case "preview": self = .preview(cue: try cue())
        case "stop_preview": self = .stopPreview(cue: try c.decodeIfPresent(ModelID.self, forKey: "cue"))
        case "go_from": self = .goFrom(list: try list(), cue: try cue())
        case "stop_all": self = .stopAll
        case "hard_stop_all": self = .hardStopAll
        case "panic_cue": self = .panicCue(cue: try cue())
        case "batch": self = .batch(actions: try c.decode([TransportAction].self, forKey: "actions"))
        case let other:
            throw DecodingError.dataCorrupted(
                .init(codingPath: c.codingPath, debugDescription: "transport action \(other)"))
        }
    }
}

/// Where one patch plays on this machine (WS-07): the device's name as `list_devices` gives it and
/// a 0-based device channel for each output of the patch, in order.
struct PatchMapping: Hashable, Sendable, Codable {
    var device: String
    var channels: [UInt32]
}

enum RecoveryChoice: String, Hashable, Sendable, Codable { case restore, ignore }

/// `new`, `open` and `open_recovery` open another workspace beside the ones open (protocol 2): the
/// result carries `opened { doc }` and a connection bound to that doc gets its snapshot.
enum Request: Hashable, Sendable, Codable {
    case new
    case open(path: String, recovery: RecoveryChoice? = nil)
    case openRecovery(workspaceId: ModelID, pathHash: String? = nil, modifiedMs: Int64? = nil)
    case scanRecovery
    case save
    case saveAs(path: String)
    case relink(folder: String)
    case listDevices
    case preshowCheck
    case logTail(max: UInt32? = nil)
    /// PB-10: Network cues still run but send nothing. Engine-wide, in memory only.
    case setOutputOverrides(networkDisabled: Bool)
    /// SM-01/SM-07: the engine keeps Show Mode per workspace and holds off sleep while one is in it.
    case setShowMode(on: Bool)
    /// WS-07: sets, or with nil clears, where the workspace's patch plays on this machine. Kept in
    /// the engine's machine settings, not the show file. A clear travels as an explicit `null`.
    case setPatchMapping(patch: ModelID, mapping: PatchMapping?)
    /// AUD-14: an Audio cue's file peaks over file seconds `start..<end` in `buckets` parts (1 to 8192).
    case waveform(cue: ModelID, start: Double, end: Double, buckets: UInt32)
    case snapshot

    /// Replies with a `snapshot` (the `workspaces` list on the app connection), so the engine
    /// holds this connection's broadcasts until the `result` (docs/network.md "Ordering and gaps").
    var answersWithSnapshot: Bool {
        switch self {
        case .snapshot: return true
        default: return false
        }
    }

    private var wire: (String, Fields) {
        func opt(_ f: inout Fields, _ k: String, _ v: (any Encodable)?) { if let v { f.append((k, v)) } }
        var f: Fields = []
        switch self {
        case .new: return ("new", [])
        case .open(let p, let r):
            f.append(("path", p))
            opt(&f, "recovery", r)
            return ("open", f)
        case .openRecovery(let w, let h, let m):
            f.append(("workspace_id", w))
            opt(&f, "path_hash", h)
            opt(&f, "modified_ms", m)
            return ("open_recovery", f)
        case .scanRecovery: return ("scan_recovery", [])
        case .save: return ("save", [])
        case .saveAs(let p): return ("save_as", [("path", p)])
        case .relink(let folder): return ("relink", [("folder", folder)])
        case .listDevices: return ("list_devices", [])
        case .preshowCheck: return ("preshow_check", [])
        case .logTail(let max):
            opt(&f, "max", max)
            return ("log_tail", f)
        case .setOutputOverrides(let disabled): return ("set_output_overrides", [("network_disabled", disabled)])
        case .setShowMode(let on): return ("set_show_mode", [("on", on)])
        case .setPatchMapping(let patch, let mapping):
            return ("set_patch_mapping", [("patch", patch), ("mapping", mapping as PatchMapping?)])
        case .snapshot: return ("snapshot", [])
        case .waveform(let cue, let start, let end, let buckets):
            return ("waveform", [("cue", cue), ("start", start), ("end", end), ("buckets", buckets)])
        }
    }

    func encode(to encoder: Encoder) throws { try encodeTagged(wire.0, wire.1, to: encoder) }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch try c.decode(String.self, forKey: "type") {
        case "new": self = .new
        case "open":
            self = .open(
                path: try c.decode(String.self, forKey: "path"),
                recovery: try c.decodeIfPresent(RecoveryChoice.self, forKey: "recovery"))
        case "open_recovery":
            self = .openRecovery(
                workspaceId: try c.decode(ModelID.self, forKey: "workspace_id"),
                pathHash: try c.decodeIfPresent(String.self, forKey: "path_hash"),
                modifiedMs: try c.decodeIfPresent(Int64.self, forKey: "modified_ms"))
        case "scan_recovery": self = .scanRecovery
        case "save": self = .save
        case "save_as": self = .saveAs(path: try c.decode(String.self, forKey: "path"))
        case "relink": self = .relink(folder: try c.decode(String.self, forKey: "folder"))
        case "list_devices": self = .listDevices
        case "preshow_check": self = .preshowCheck
        case "log_tail": self = .logTail(max: try c.decodeIfPresent(UInt32.self, forKey: "max"))
        case "set_output_overrides":
            self = .setOutputOverrides(networkDisabled: try c.decode(Bool.self, forKey: "network_disabled"))
        case "set_show_mode": self = .setShowMode(on: try c.decode(Bool.self, forKey: "on"))
        case "set_patch_mapping":
            self = .setPatchMapping(
                patch: try c.decode(ModelID.self, forKey: "patch"),
                mapping: try c.decodeIfPresent(PatchMapping.self, forKey: "mapping"))
        case "snapshot": self = .snapshot
        case "waveform":
            self = .waveform(
                cue: try c.decode(ModelID.self, forKey: "cue"), start: try c.decode(Double.self, forKey: "start"),
                end: try c.decode(Double.self, forKey: "end"), buckets: try c.decode(UInt32.self, forKey: "buckets"))
        case let other:
            throw DecodingError.dataCorrupted(.init(codingPath: c.codingPath, debugDescription: "request \(other)"))
        }
    }
}

// A doc id is a JSON number on the wire (the engine's `DocId`, u64); the app keys stores and
// windows by it as a `ModelID` holding its decimal digits (`ModelID(doc:)`, `docNumber`).
extension ModelID {
    /// A doc id as the engine sends it.
    init(doc: UInt64) { self.init(String(doc)) }

    /// The wire form of a doc id; nil when this is not one.
    var docNumber: UInt64? { UInt64(raw) }

    /// Encodes this doc id as the engine's number, or fails (a doc id is never anything else).
    func encodeDoc<K: CodingKey>(in c: inout KeyedEncodingContainer<K>, forKey key: K) throws {
        guard let n = docNumber else {
            throw EncodingError.invalidValue(
                raw, .init(codingPath: c.codingPath + [key], debugDescription: "not a doc id"))
        }
        try c.encode(n, forKey: key)
    }
}

/// `hello`. `workspace` binds the connection to one open workspace; absent (and omitted from the
/// JSON) it is the unbound app connection. `pid` is this process: while it runs, an engine that
/// lost the app connection waits for it to reconnect instead of relaunching the app (#168).
struct Hello: Hashable, Sendable, Codable {
    var protocolVersion: UInt32
    var token: String
    var clientName: String
    var workspace: ModelID?
    var pid: UInt32?

    enum CodingKeys: String, CodingKey {
        case type, token, workspace, pid
        case protocolVersion = "protocol_version"
        case clientName = "client_name"
    }

    init(
        protocolVersion: UInt32, token: String, clientName: String, workspace: ModelID? = nil, pid: UInt32? = nil
    ) {
        self.protocolVersion = protocolVersion
        self.token = token
        self.clientName = clientName
        self.workspace = workspace
        self.pid = pid
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try c.decode(UInt32.self, forKey: .protocolVersion)
        token = try c.decode(String.self, forKey: .token)
        clientName = try c.decode(String.self, forKey: .clientName)
        workspace = try c.decodeIfPresent(UInt64.self, forKey: .workspace).map(ModelID.init(doc:))
        pid = try c.decodeIfPresent(UInt32.self, forKey: .pid)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode("hello", forKey: .type)
        try c.encode(protocolVersion, forKey: .protocolVersion)
        try c.encode(token, forKey: .token)
        try c.encode(clientName, forKey: .clientName)
        try workspace?.encodeDoc(in: &c, forKey: .workspace)
        try c.encodeIfPresent(pid, forKey: .pid)
    }
}

/// The modifier keys of a `hotkey` message (all four always on the wire).
struct HotkeyModifiers: Hashable, Sendable, Codable {
    var shift = false
    var control = false
    /// Option.
    var alt = false
    var command = false
}

enum ClientMessage: Hashable, Sendable, Codable {
    case hello(Hello)
    /// `coalesce`: edits sent with the same key join one undo step (docs/commands.md "Coalescing");
    /// absent on the wire when nil.
    case command(id: UInt64, Command, coalesce: String? = nil)
    case transport(id: UInt64, TransportAction)
    case undo(id: UInt64)
    case redo(id: UInt64)
    case request(id: UInt64, Request)
    case goodbye(shutdown: Bool)
    /// Close a workspace: the bound one, or `doc` from any connection. Refused with
    /// `unsaved_changes` unless `discardChanges`.
    case close(id: UInt64, doc: ModelID?, discardChanges: Bool)
    /// The app-scope Panic (Esc): every open workspace, on any connection.
    case panic(id: UInt64)
    /// A cue hotkey went down (`up` false) or up, app-scope: the engine matches it against every
    /// open workspace's cue hotkeys and starts those cues (or sends them their release).
    case hotkey(id: UInt64, key: String, modifiers: HotkeyModifiers, up: Bool)

    func encode(to encoder: Encoder) throws {
        switch self {
        case .hello(let h): try h.encode(to: encoder)
        case .command(let id, let cmd, let coalesce):
            var fields: Fields = [("id", id), ("command", cmd)]
            if let coalesce { fields.append(("coalesce", coalesce)) }
            try encodeTagged("command", fields, to: encoder)
        case .transport(let id, let a): try encodeTagged("transport", [("id", id), ("action", a)], to: encoder)
        case .undo(let id): try encodeTagged("undo", [("id", id)], to: encoder)
        case .redo(let id): try encodeTagged("redo", [("id", id)], to: encoder)
        case .request(let id, let r): try encodeTagged("request", [("id", id), ("request", r)], to: encoder)
        case .goodbye(let s): try encodeTagged("goodbye", [("shutdown", s)], to: encoder)
        case .close(let id, let doc, let discard):
            var c = encoder.container(keyedBy: Key.self)
            try c.encode("close", forKey: "type")
            try c.encode(id, forKey: "id")
            try doc?.encodeDoc(in: &c, forKey: "doc")
            if discard { try c.encode(true, forKey: "discard_changes") }
        case .panic(let id): try encodeTagged("panic", [("id", id)], to: encoder)
        case .hotkey(let id, let key, let modifiers, let up):
            try encodeTagged(
                "hotkey", [("id", id), ("key", key), ("modifiers", modifiers), ("phase", up ? "up" : "down")],
                to: encoder)
        }
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        func id() throws -> UInt64 { try c.decode(UInt64.self, forKey: "id") }
        switch try c.decode(String.self, forKey: "type") {
        case "hello": self = .hello(try Hello(from: decoder))
        case "command":
            self = .command(
                id: try id(), try c.decode(Command.self, forKey: "command"),
                coalesce: try c.decodeIfPresent(String.self, forKey: "coalesce"))
        case "transport": self = .transport(id: try id(), try c.decode(TransportAction.self, forKey: "action"))
        case "undo": self = .undo(id: try id())
        case "redo": self = .redo(id: try id())
        case "request": self = .request(id: try id(), try c.decode(Request.self, forKey: "request"))
        case "goodbye": self = .goodbye(shutdown: try c.decodeIfPresent(Bool.self, forKey: "shutdown") ?? false)
        case "close":
            self = .close(
                id: try id(), doc: try c.decodeIfPresent(UInt64.self, forKey: "doc").map(ModelID.init(doc:)),
                discardChanges: try c.decodeIfPresent(Bool.self, forKey: "discard_changes") ?? false)
        case "panic": self = .panic(id: try id())
        case "hotkey":
            self = .hotkey(
                id: try id(), key: try c.decode(String.self, forKey: "key"),
                modifiers: try c.decodeIfPresent(HotkeyModifiers.self, forKey: "modifiers") ?? HotkeyModifiers(),
                up: try c.decodeIfPresent(String.self, forKey: "phase") == "up")
        case let other:
            throw DecodingError.dataCorrupted(
                .init(codingPath: c.codingPath, debugDescription: "client message \(other)"))
        }
    }

    func text() throws -> String {
        let enc = JSONEncoder()
        enc.outputFormatting = [.withoutEscapingSlashes]
        return String(decoding: try enc.encode(self), as: UTF8.self)
    }
}
