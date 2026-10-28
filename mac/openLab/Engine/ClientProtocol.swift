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
        case let other:
            throw DecodingError.dataCorrupted(
                .init(codingPath: c.codingPath, debugDescription: "transport action \(other)"))
        }
    }
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
        case .snapshot: return ("snapshot", [])
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
        case "snapshot": self = .snapshot
        case let other:
            throw DecodingError.dataCorrupted(.init(codingPath: c.codingPath, debugDescription: "request \(other)"))
        }
    }
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
    /// Multi-workspace messages (ProvisionalProtocol.swift).
    case provisional(ProvisionalClientMessage)

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
        case .provisional(let p): try p.encode(to: encoder)
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
        case let other:
            if let p = try ProvisionalClientMessage(type: other, from: decoder) {
                self = .provisional(p)
            } else {
                throw DecodingError.dataCorrupted(
                    .init(codingPath: c.codingPath, debugDescription: "client message \(other)"))
            }
        }
    }

    func text() throws -> String {
        let enc = JSONEncoder()
        enc.outputFormatting = [.withoutEscapingSlashes]
        return String(decoding: try enc.encode(self), as: UTF8.self)
    }
}
