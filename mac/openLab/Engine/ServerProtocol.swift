// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

// Engine -> client messages (docs/protocol.md section 4; Rust: openlab-core::protocol::server).
// Decode only. Adding a message is one case here plus one fixture; an unknown `type` decodes to
// `.unknown` so a newer engine cannot kill the connection.

/// `MAX_SERVER_FRAME_BYTES`: the largest frame the engine sends (a whole snapshot).
let maxServerFrameBytes = 32 * 1024 * 1024
/// 2: connections bind to one of several open workspaces (M4.15f).
let protocolVersion: UInt32 = 3

/// A tagged object the UI shows but does not branch on yet (warnings, problems, resolutions).
/// Kept as data so a new variant is displayed, not fatal.
struct WireVariant: Hashable, Sendable, Decodable {
    let type: String
    let json: JSONValue
    init(from decoder: Decoder) throws {
        json = try JSONValue(from: decoder)
        guard let t = json["type"]?.stringValue else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "no type"))
        }
        type = t
    }
}
typealias WireWarning = WireVariant
typealias WireProblem = WireVariant

enum WireCueState: String, Hashable, Sendable, Codable, CaseIterable {
    case idle, loaded, running, paused, fading, stopping, broken
    case preWaiting = "pre_waiting"
    case postWaiting = "post_waiting"

    /// A live cue: the Active Cues panel lists it and closing or quitting asks first. QLab shows a
    /// cue as active while it plays or is paused; waiting and stopping cues are live too.
    var isActive: Bool {
        switch self {
        case .running, .paused, .fading, .preWaiting, .postWaiting, .stopping: return true
        case .idle, .loaded, .broken: return false
        }
    }
}

enum WireBrokenReason: String, Hashable, Sendable, Codable, CaseIterable {
    case voiceRejected = "voice_rejected"
    case streamFailed = "stream_failed"
    case deviceLost = "device_lost"
}

enum RuntimeChange: Sendable, Decodable {
    case cueState(cue: ModelID, state: WireCueState, reason: WireBrokenReason?)
    case problems(cue: ModelID, problems: [WireProblem])
    case groupWarning(group: ModelID, warning: Bool)
    case playhead(list: ModelID, cue: ModelID?)
    case preview(cue: ModelID, active: Bool)
    case warning(WireWarning)
    /// Every field nil: the cue's lengths are not known now (drop what is held).
    case mediaDuration(CueMediaDuration)
    case unknown(String)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch try c.decode(String.self, forKey: "type") {
        case "cue_state":
            self = .cueState(
                cue: try c.decode(ModelID.self, forKey: "cue"), state: try c.decode(WireCueState.self, forKey: "state"),
                reason: try c.decodeIfPresent(WireBrokenReason.self, forKey: "reason"))
        case "problems":
            self = .problems(
                cue: try c.decode(ModelID.self, forKey: "cue"),
                problems: try c.decode([WireProblem].self, forKey: "problems"))
        case "group_warning":
            self = .groupWarning(
                group: try c.decode(ModelID.self, forKey: "group"), warning: try c.decode(Bool.self, forKey: "warning"))
        case "playhead":
            self = .playhead(
                list: try c.decode(ModelID.self, forKey: "list"),
                cue: try c.decodeIfPresent(ModelID.self, forKey: "cue"))
        case "preview":
            self = .preview(
                cue: try c.decode(ModelID.self, forKey: "cue"), active: try c.decode(Bool.self, forKey: "active"))
        case "warning":
            self = .warning(try c.decode(WireWarning.self, forKey: "warning"))
        case "media_duration":
            self = .mediaDuration(try CueMediaDuration(from: decoder))
        case let other:
            self = .unknown(other)
        }
    }
}

struct CueStateEntry: Sendable, Decodable {
    let cue: ModelID
    let state: WireCueState
    let reason: WireBrokenReason?
}

struct CueProblems: Sendable, Decodable {
    let cue: ModelID
    let problems: [WireProblem]
}

struct PlayheadEntry: Sendable, Decodable {
    let list: ModelID
    let cue: ModelID?
}

struct Snapshot: Sendable, Decodable {
    var revision: UInt64
    var runtimeSeq: UInt64
    var document: DocumentState
    var workspace: WorkspaceModel
    var cueStates: [CueStateEntry]
    var problems: [CueProblems]
    var groupWarnings: [ModelID]
    var playheads: [PlayheadEntry]
    var previewing: [ModelID]
    var warnings: [WireWarning]
    /// Absent (none known): nil.
    var mediaDurations: [CueMediaDuration]?
    enum CodingKeys: String, CodingKey {
        case revision, document, workspace, problems, playheads, previewing, warnings
        case runtimeSeq = "runtime_seq"
        case cueStates = "cue_states"
        case groupWarnings = "group_warnings"
        case mediaDurations = "media_durations"
    }
}

struct ProgressItem: Hashable, Sendable, Decodable {
    let cue: ModelID
    let elapsed: Double
    let remaining: Double?
}

/// A pre-wait or post-wait counting down (M4.2e), in seconds; it always has an end.
struct WaitProgress: Hashable, Sendable, Decodable {
    enum Kind: String, Hashable, Sendable, Decodable {
        case preWait = "pre_wait"
        case postWait = "post_wait"
    }
    let cue: ModelID
    let wait: Kind
    let elapsed: Double
    let remaining: Double
}

struct OutputMeter: Sendable, Decodable {
    let patch: ModelID
    let peaks: [Float]
}

struct CueMeter: Sendable, Decodable {
    let cue: ModelID
    let peaks: [Float]
}

enum LogLevel: String, Sendable, Decodable { case error, warn, info, debug }

/// An engine error code. A string so a code added later still decodes.
struct ErrorCode: Hashable, Sendable, Codable, ExpressibleByStringLiteral, CustomStringConvertible {
    let raw: String
    init(stringLiteral value: String) { raw = value }
    init(from decoder: Decoder) throws { raw = try decoder.singleValueContainer().decode(String.self) }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(raw)
    }
    var description: String { raw }

    static let protocolVersionMismatch: ErrorCode = "protocol_version_mismatch"
    static let badToken: ErrorCode = "bad_token"
    static let badRequest: ErrorCode = "bad_request"
    static let unknownMessage: ErrorCode = "unknown_message"
    static let commandRefused: ErrorCode = "command_refused"
    static let notFound: ErrorCode = "not_found"
    static let noWorkspace: ErrorCode = "no_workspace"
    static let unsavedChanges: ErrorCode = "unsaved_changes"
    static let io: ErrorCode = "io"
    static let badShowFile: ErrorCode = "bad_show_file"
    static let unsupported: ErrorCode = "unsupported"
    static let `internal`: ErrorCode = "internal"
}

struct ErrorBody: Hashable, Sendable, Decodable {
    let code: ErrorCode
    let message: String
}

struct DeviceEntry: Hashable, Sendable, Decodable {
    let name: String
    let outputChannels: Int
    enum CodingKeys: String, CodingKey {
        case name
        case outputChannels = "output_channels"
    }
}

/// What a show-log record is about. A string so a kind added later still decodes.
struct LogKind: Hashable, Sendable, Decodable, ExpressibleByStringLiteral {
    let raw: String
    init(stringLiteral value: String) { raw = value }
    init(from decoder: Decoder) throws { raw = try decoder.singleValueContainer().decode(String.self) }
    static let go: LogKind = "go"
    static let transport: LogKind = "transport"
    static let trigger: LogKind = "trigger"
    static let cueStart: LogKind = "cue_start"
    static let cueStop: LogKind = "cue_stop"
    static let cueFail: LogKind = "cue_fail"
    static let device: LogKind = "device"
    static let document: LogKind = "document"
    static let engine: LogKind = "engine"
}

/// One show-log record (DX-02), as in `log_tail` and the engine's log file.
struct ShowLogEntry: Hashable, Sendable, Decodable {
    let timeMs: Int64
    let showTime: Double?
    let level: LogLevel
    let kind: LogKind
    let message: String
    let cue: ModelID?
    let source: String?
    enum CodingKeys: String, CodingKey {
        case level, kind, message, cue, source
        case timeMs = "time_ms"
        case showTime = "show_time"
    }
}

/// The output device's state. A string so a state added later still decodes.
struct DeviceState: Hashable, Sendable, Decodable, ExpressibleByStringLiteral {
    let raw: String
    init(stringLiteral value: String) { raw = value }
    init(from decoder: Decoder) throws { raw = try decoder.singleValueContainer().decode(String.self) }
    static let ok: DeviceState = "ok"
    static let lost: DeviceState = "lost"
    static let failed: DeviceState = "failed"
    static let absent: DeviceState = "absent"
}

struct DiskStatus: Hashable, Sendable, Decodable {
    let streams: UInt32
    let underrunFrames: UInt64
    let healthy: Bool
    enum CodingKeys: String, CodingKey {
        case streams, healthy
        case underrunFrames = "underrun_frames"
    }
}

struct DeviceStatus: Hashable, Sendable, Decodable {
    let name: String?
    let state: DeviceState
    let sampleRate: UInt32
    let blockFrames: UInt32?
    let channels: UInt32
    let latencyMs: Double?
    enum CodingKeys: String, CodingKey {
        case name, state, channels
        case sampleRate = "sample_rate"
        case blockFrames = "block_frames"
        case latencyMs = "latency_ms"
    }
}

/// The `status` message body (DX-03).
struct SystemStatus: Hashable, Sendable, Decodable {
    let cpuPercent: Double?
    let engineLoad: Double?
    let xruns: UInt64
    let disk: DiskStatus
    let device: DeviceStatus
    let logDropped: UInt64
    /// The newest show-log record's `seq`; above the last `log_entry` received means a push was
    /// lost. 0 when absent (an engine from before the field).
    let logSeq: UInt64
    /// PB-10: the network output override is on, so Network cues send nothing.
    let networkOutputDisabled: Bool
    enum CodingKeys: String, CodingKey {
        case xruns, disk, device
        case cpuPercent = "cpu_percent"
        case engineLoad = "engine_load"
        case logDropped = "log_dropped"
        case logSeq = "log_seq"
        case networkOutputDisabled = "network_output_disabled"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        cpuPercent = try c.decodeIfPresent(Double.self, forKey: .cpuPercent)
        engineLoad = try c.decodeIfPresent(Double.self, forKey: .engineLoad)
        xruns = try c.decode(UInt64.self, forKey: .xruns)
        disk = try c.decode(DiskStatus.self, forKey: .disk)
        device = try c.decode(DeviceStatus.self, forKey: .device)
        logDropped = try c.decode(UInt64.self, forKey: .logDropped)
        logSeq = try c.decodeIfPresent(UInt64.self, forKey: .logSeq) ?? 0
        networkOutputDisabled = try c.decodeIfPresent(Bool.self, forKey: .networkOutputDisabled) ?? false
    }
}

struct AmbiguousMatch: Hashable, Sendable, Decodable {
    let cue: ModelID
    let candidates: [String]
}

struct RecoveryEntryInfo: Hashable, Sendable, Decodable {
    let workspaceId: ModelID
    let pathHash: String?
    let showName: String?
    let modifiedMs: Int64?
    let stale: Bool
    enum CodingKeys: String, CodingKey {
        case stale
        case workspaceId = "workspace_id"
        case pathHash = "path_hash"
        case showName = "show_name"
        case modifiedMs = "modified_ms"
    }
}

/// The pre-show check (SM-06, NFR-07); Rust: `PreshowReport`. Every list is sorted for display.
struct PreshowSummary: Hashable, Sendable, Decodable {
    let brokenCues: UInt64
    /// Missing or unsupported files: causes of broken cues, not counted in `total`.
    let media: UInt64
    /// Patches with no device that some cue uses (spare patches are listed but not counted); not in `total`.
    let patches: UInt64
    /// Destinations with status `missing`; `networkUnchecked` ones are neither a problem nor known fine.
    let network: UInt64
    let networkUnchecked: UInt64
    /// `system` entries that are problems (`preload_over_budget` is information and not counted).
    let system: UInt64
    /// `brokenCues + system`: the problem rows a list shows.
    let total: UInt64
    enum CodingKeys: String, CodingKey {
        case media, patches, network, system, total
        case brokenCues = "broken_cues"
        case networkUnchecked = "network_unchecked"
    }
}

struct PreshowCue: Hashable, Sendable, Decodable {
    let cue: ModelID
    let list: ModelID
    let number: String?
    let name: String
    let problems: [WireProblem]
}

/// `missing` or `unsupported`. A string so a status added later still decodes.
struct PreshowStatus: Hashable, Sendable, Decodable, ExpressibleByStringLiteral {
    let raw: String
    init(stringLiteral value: String) { raw = value }
    init(from decoder: Decoder) throws { raw = try decoder.singleValueContainer().decode(String.self) }
    static let missing: PreshowStatus = "missing"
    static let unsupported: PreshowStatus = "unsupported"
    /// Network destinations only: the destination exists, reachability is not probed.
    static let notChecked: PreshowStatus = "not_checked"
}

struct PreshowMedia: Hashable, Sendable, Decodable {
    let path: String
    let status: PreshowStatus
    let reason: String?
    let cues: [ModelID]
}

struct PreshowPatch: Hashable, Sendable, Decodable {
    let patch: ModelID
    /// Nil when the workspace has no such patch.
    let name: String?
    let device: String?
    let mapped: Bool
    let resolution: WireVariant?
    let cues: [ModelID]
}

struct PreshowTarget: Hashable, Sendable, Decodable {
    let patch: ModelID
    let name: String?
    let host: String?
    let port: UInt16?
    let `protocol`: String?
    let status: PreshowStatus
    let cues: [ModelID]
}

struct PreshowReport: Hashable, Sendable, Decodable {
    let ok: Bool
    let summary: PreshowSummary
    let brokenCues: [PreshowCue]
    let media: [PreshowMedia]
    let patches: [PreshowPatch]
    let network: [PreshowTarget]
    let system: [WireWarning]
    enum CodingKeys: String, CodingKey {
        case ok, summary, media, patches, network, system
        case brokenCues = "broken_cues"
    }
}

enum ResultData: Sendable, Equatable, Decodable {
    case devices([DeviceEntry])
    case relink(
        relinked: Int, ambiguous: [AmbiguousMatch], unmatched: [ModelID], truncated: Bool, skipped: Int)
    case recoveryOffer(path: String, workspaceName: String, modifiedMs: Int64)
    case recoveryEntries([RecoveryEntryInfo])
    /// `seq`: the number of the newest record when it was answered (0: none, or an older engine).
    case logTail(entries: [ShowLogEntry], dropped: UInt64, seq: UInt64)
    case preshowCheck(PreshowReport)
    /// The doc a `new` / `open` / `open_recovery` opened.
    case opened(doc: ModelID)
    /// A data type this client does not know; the raw object is kept.
    case other(type: String, json: JSONValue)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let type = try c.decode(String.self, forKey: "type")
        switch type {
        case "devices": self = .devices(try c.decode([DeviceEntry].self, forKey: "devices"))
        case "relink":
            self = .relink(
                relinked: try c.decode(Int.self, forKey: "relinked"),
                ambiguous: try c.decode([AmbiguousMatch].self, forKey: "ambiguous"),
                unmatched: try c.decode([ModelID].self, forKey: "unmatched"),
                truncated: try c.decode(Bool.self, forKey: "truncated"),
                skipped: try c.decode(Int.self, forKey: "skipped"))
        case "recovery_offer":
            self = .recoveryOffer(
                path: try c.decode(String.self, forKey: "path"),
                workspaceName: try c.decode(String.self, forKey: "workspace_name"),
                modifiedMs: try c.decode(Int64.self, forKey: "modified_ms"))
        case "recovery_entries": self = .recoveryEntries(try c.decode([RecoveryEntryInfo].self, forKey: "entries"))
        case "log_tail":
            self = .logTail(
                entries: try c.decode([ShowLogEntry].self, forKey: "entries"),
                dropped: try c.decode(UInt64.self, forKey: "dropped"),
                seq: try c.decodeIfPresent(UInt64.self, forKey: "seq") ?? 0)
        case "preshow_check": self = .preshowCheck(try PreshowReport(from: decoder))
        case "opened": self = .opened(doc: ModelID(doc: try c.decode(UInt64.self, forKey: "doc")))
        default: self = .other(type: type, json: try JSONValue(from: decoder))
        }
    }
}

/// One open workspace as the app connection lists it.
struct WorkspaceInfo: Hashable, Sendable, Decodable {
    let doc: ModelID
    let workspaceId: ModelID
    let name: String
    let path: String?
    let dirty: Bool
    /// The hash the engine names this doc's autosave by (a `recovery_entries` entry's
    /// `path_hash`); nil for a doc never saved, or from an engine that does not send it.
    let pathHash: String?
    enum CodingKeys: String, CodingKey {
        case doc, name, path, dirty
        case workspaceId = "workspace_id"
        case pathHash = "path_hash"
    }

    init(doc: ModelID, workspaceId: ModelID, name: String, path: String?, dirty: Bool, pathHash: String? = nil) {
        self.doc = doc
        self.workspaceId = workspaceId
        self.name = name
        self.path = path
        self.dirty = dirty
        self.pathHash = pathHash
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        doc = ModelID(doc: try c.decode(UInt64.self, forKey: .doc))
        workspaceId = try c.decode(ModelID.self, forKey: .workspaceId)
        name = try c.decode(String.self, forKey: .name)
        path = try c.decodeIfPresent(String.self, forKey: .path)
        dirty = try c.decode(Bool.self, forKey: .dirty)
        pathHash = try c.decodeIfPresent(String.self, forKey: .pathHash)
    }
}

extension ResultData {
    /// The doc a `new` / `open` / `open_recovery` opened; nil for any other result.
    var openedDoc: ModelID? {
        if case .opened(let doc) = self { return doc }
        return nil
    }
}

extension ErrorCode {
    /// `open` of a path that is already open.
    static let alreadyOpen: ErrorCode = "already_open"
}

struct OkBody: Sendable, Equatable, Decodable {
    var revision: UInt64?
    var data: ResultData?
}

enum Outcome: Sendable {
    case ok(OkBody)
    case error(ErrorBody)
}

enum ServerMessage: Sendable, Decodable {
    case welcome(protocolVersion: UInt32, snapshot: Snapshot)
    case snapshot(Snapshot)
    case modelChanged(revision: UInt64, upserts: [Upsert], deletes: [Delete])
    case document(DocumentState)
    case runtime(seq: UInt64, changes: [RuntimeChange])
    /// Actions in progress, and pre-wait / post-wait countdowns (`waits`, absent: none).
    case progress([ProgressItem], waits: [WaitProgress])
    case meters(outputs: [OutputMeter], cues: [CueMeter])
    case log(LogLevel, String)
    /// One new show-log record (M4.7c). Lossy and rate-limited: a gap in `seq` means records to
    /// read with `log_tail`.
    case logEntry(seq: UInt64, ShowLogEntry)
    case status(SystemStatus)
    case warnings([WireWarning])
    case result(id: UInt64, Outcome)
    case error(code: ErrorCode, message: String, protocolVersion: UInt32)
    /// `welcome` on the app (unbound) connection: the open workspaces, in open order.
    case appWelcome(protocolVersion: UInt32, workspaces: [WorkspaceInfo])
    /// The list changed (a workspace opened, closed, renamed, saved or became dirty).
    case workspaces([WorkspaceInfo])
    /// The engine closed this connection's workspace; it closes the connection next.
    case closed(doc: ModelID)
    /// A `type` this client does not know: logged and dropped by the client.
    case unknown(type: String)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        let type = try c.decode(String.self, forKey: "type")
        func d<T: Decodable>(_ key: Key) throws -> T { try c.decode(T.self, forKey: key) }
        switch type {
        case "welcome":
            if c.contains("snapshot") {
                self = .welcome(protocolVersion: try d("protocol_version"), snapshot: try d("snapshot"))
            } else {
                self = .appWelcome(protocolVersion: try d("protocol_version"), workspaces: try d("workspaces"))
            }
        case "snapshot": self = .snapshot(try d("snapshot"))
        case "model_changed":
            self = .modelChanged(revision: try d("revision"), upserts: try d("upserts"), deletes: try d("deletes"))
        case "document": self = .document(try d("document"))
        case "runtime": self = .runtime(seq: try d("seq"), changes: try d("changes"))
        case "progress":
            self = .progress(try d("items"), waits: try c.decodeIfPresent([WaitProgress].self, forKey: "waits") ?? [])
        case "meters": self = .meters(outputs: try d("outputs"), cues: try d("cues"))
        case "log": self = .log(try d("level"), try d("message"))
        case "log_entry": self = .logEntry(seq: try d("seq"), try d("entry"))
        case "status": self = .status(try d("status"))
        case "warnings": self = .warnings(try d("warnings"))
        case "result":
            let id: UInt64 = try d("id")
            if c.contains("ok") {
                self = .result(id: id, .ok(try d("ok")))
            } else {
                self = .result(id: id, .error(try d("error")))
            }
        case "error":
            self = .error(code: try d("code"), message: try d("message"), protocolVersion: try d("protocol_version"))
        case "workspaces": self = .workspaces(try d("workspaces"))
        case "closed": self = .closed(doc: ModelID(doc: try d("doc")))
        default: self = .unknown(type: type)
        }
    }

    /// Decodes one text frame. Throws only for a frame that is not a protocol message at all;
    /// the caller logs and drops it.
    static func decode(_ text: String) throws -> ServerMessage {
        try JSONDecoder().decode(ServerMessage.self, from: Data(text.utf8))
    }
}
