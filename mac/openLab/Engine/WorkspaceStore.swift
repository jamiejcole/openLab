// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

struct CueRunState: Hashable, Sendable {
    var state: WireCueState
    var reason: WireBrokenReason?
}

struct LogEntry: Hashable, Sendable {
    var level: LogLevel
    var message: String
}

/// The UI's mirror of one open workspace: the model plus the engine's runtime state, applied from
/// the protocol and published with Combine. View controllers read this and call its intent
/// methods; they never see the socket (ARCHITECTURE section 9).
@MainActor
final class WorkspaceStore: ObservableObject, EngineClientDelegate {
    @Published private(set) var model: WorkspaceModel?
    @Published private(set) var document: DocumentState?
    @Published private(set) var cueStates: [ModelID: CueRunState] = [:]
    @Published private(set) var problems: [ModelID: [WireProblem]] = [:]
    @Published private(set) var groupWarnings: Set<ModelID> = []
    @Published private(set) var playheads: [ModelID: ModelID] = [:]
    @Published private(set) var previewing: Set<ModelID> = []
    @Published private(set) var progress: [ModelID: ProgressItem] = [:]
    /// Workspace-level notices in force (replaced by each `warnings` message).
    @Published private(set) var warnings: [WireWarning] = []
    /// Warnings that arrived as runtime changes, newest last.
    @Published private(set) var runtimeWarnings: [WireWarning] = []
    /// The latest `status` (system health, DX-03); nil until the first one arrives.
    @Published private(set) var status: SystemStatus?
    @Published private(set) var logEntries: [LogEntry] = []
    @Published private(set) var connectionState: ConnectionState = .idle

    private(set) var revision: UInt64 = 0
    private(set) var runtimeSeq: UInt64 = 0
    let client: EngineClient
    private var stateSub: AnyCancellable?

    static let maxRuntimeWarnings = 100
    static let maxLogEntries = 500

    /// `ownsDelegate: false` when something else (the session on a one-workspace engine) owns the
    /// client and forwards its messages to `receive`.
    init(client: EngineClient, ownsDelegate: Bool = true) {
        self.client = client
        if ownsDelegate { client.delegate = self }
        connectionState = client.state
        stateSub = client.$state.sink { [weak self] in self?.connectionState = $0 }
    }

    // MARK: Intents (all go through the client)

    @discardableResult
    func transport(_ action: TransportAction) async -> Result<OkBody, EngineError> {
        await client.send { .transport(id: $0, action) }
    }

    func go(list: ModelID) async -> Result<OkBody, EngineError> { await transport(.go(list: list)) }
    func panic() async -> Result<OkBody, EngineError> { await transport(.panic) }

    func send(command: Command) async -> Result<OkBody, EngineError> {
        await client.send { .command(id: $0, command) }
    }

    func undo() async -> Result<OkBody, EngineError> { await client.send { .undo(id: $0) } }
    func redo() async -> Result<OkBody, EngineError> { await client.send { .redo(id: $0) } }
    func save() async -> Result<OkBody, EngineError> { await client.send { .request(id: $0, .save) } }
    func saveAs(path: String) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .saveAs(path: path)) }
    }
    func relink(folder: String) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .relink(folder: folder)) }
    }
    /// The newest show-log entries the engine holds (oldest first); `max` nil: all of them.
    func logTail(max: UInt32? = nil) async -> Result<(entries: [ShowLogEntry], dropped: UInt64), EngineError> {
        switch await client.send({ .request(id: $0, .logTail(max: max)) }) {
        case .success(let ok):
            if case .logTail(let entries, let dropped)? = ok.data { return .success((entries, dropped)) }
            return .failure(.encoding("log_tail result has no log_tail data"))
        case .failure(let e): return .failure(e)
        }
    }

    func listDevices() async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .listDevices) }
    }

    // MARK: Applying server messages

    func engineClient(_ client: EngineClient, didReceive message: ServerMessage) -> ApplyOutcome {
        receive(message)
    }

    /// Applies one server message. `.gap` means the caller must request a snapshot.
    @discardableResult
    func receive(_ message: ServerMessage) -> ApplyOutcome {
        switch message {
        case .welcome(_, let snapshot), .snapshot(let snapshot):
            replace(with: snapshot)
        case .modelChanged(let rev, let upserts, let deletes):
            return applyModelChange(revision: rev, upserts: upserts, deletes: deletes)
        case .runtime(let seq, let changes):
            return applyRuntime(seq: seq, changes: changes)
        case .document(let d):
            document = d
        case .progress(let items):
            progress = Dictionary(items.map { ($0.cue, $0) }, uniquingKeysWith: { $1 })
        case .warnings(let w):
            warnings = w
        case .log(let level, let text):
            logEntries.append(LogEntry(level: level, message: text))
            if logEntries.count > Self.maxLogEntries { logEntries.removeFirst(logEntries.count - Self.maxLogEntries) }
        case .status(let s):
            status = s
        case .meters, .result, .error, .provisional, .unknown:
            break  // meters have no consumer yet; the rest belong to the client or session.
        }
        return .applied
    }

    private func replace(with s: Snapshot) {
        model = s.workspace
        document = s.document
        revision = s.revision
        runtimeSeq = s.runtimeSeq
        cueStates = Dictionary(
            s.cueStates.map { ($0.cue, CueRunState(state: $0.state, reason: $0.reason)) }, uniquingKeysWith: { $1 })
        problems = Dictionary(s.problems.map { ($0.cue, $0.problems) }, uniquingKeysWith: { $1 })
        groupWarnings = Set(s.groupWarnings)
        playheads = Dictionary(s.playheads.compactMap { e in e.cue.map { (e.list, $0) } }, uniquingKeysWith: { $1 })
        previewing = Set(s.previewing)
        warnings = s.warnings
        progress = [:]
        runtimeWarnings = []
    }

    private func applyModelChange(revision rev: UInt64, upserts: [Upsert], deletes: [Delete]) -> ApplyOutcome {
        if rev <= revision { return .ignored }
        guard rev == revision + 1, var m = model else { return .gap }
        for d in deletes {
            switch d {
            case .cue(let id): for i in m.cueLists.indices { Self.removeCue(id, from: &m.cueLists[i].cues) }
            case .cueList(let id): m.cueLists.removeAll { $0.id == id }
            case .patch(let id): m.patches.removeAll { $0.id == id }
            case .networkPatch(let id): m.networkPatches.removeAll { $0.id == id }
            case .unknown: return .gap
            }
        }
        for u in upserts {
            switch u {
            case .cue(let list, let parent, let index, let cue):
                for i in m.cueLists.indices { Self.removeCue(cue.id, from: &m.cueLists[i].cues) }
                guard let li = m.cueLists.firstIndex(where: { $0.id == list }) else { return .gap }
                if let parent {
                    guard Self.insert(cue, under: parent, at: index, in: &m.cueLists[li].cues) else { return .gap }
                } else {
                    m.cueLists[li].cues.insert(cue, at: min(index, m.cueLists[li].cues.count))
                }
            case .cueList(let id, let name, let index):
                var list = CueList(id: id, name: name, cues: [])
                if let old = m.cueLists.firstIndex(where: { $0.id == id }) {
                    list.cues = m.cueLists.remove(at: old).cues
                }
                m.cueLists.insert(list, at: min(index, m.cueLists.count))
            case .patch(let index, let patch):
                m.patches.removeAll { $0.id == patch.id }
                m.patches.insert(patch, at: min(index, m.patches.count))
            case .networkPatch(let index, let patch):
                m.networkPatches.removeAll { $0.id == patch.id }
                m.networkPatches.insert(patch, at: min(index, m.networkPatches.count))
            case .settings(let name, let settings):
                m.name = name
                m.settings = settings
            case .unknown: return .gap
            }
        }
        model = m
        revision = rev
        return .applied
    }

    private static func removeCue(_ id: ModelID, from cues: inout [Cue]) {
        cues.removeAll { $0.id == id }
        for i in cues.indices { removeCue(id, from: &cues[i].children) }
    }

    private static func insert(_ cue: Cue, under parent: ModelID, at index: Int, in cues: inout [Cue]) -> Bool {
        for i in cues.indices {
            if cues[i].id == parent {
                cues[i].children.insert(cue, at: min(index, cues[i].children.count))
                return true
            }
            if insert(cue, under: parent, at: index, in: &cues[i].children) { return true }
        }
        return false
    }

    private func applyRuntime(seq: UInt64, changes: [RuntimeChange]) -> ApplyOutcome {
        if seq <= runtimeSeq { return .ignored }
        guard seq == runtimeSeq + 1 else { return .gap }
        runtimeSeq = seq
        for change in changes {
            switch change {
            case .cueState(let cue, let state, let reason):
                cueStates[cue] = CueRunState(state: state, reason: reason)
                if state != .running && state != .fading { progress[cue] = nil }
            case .problems(let cue, let list): problems[cue] = list.isEmpty ? nil : list
            case .groupWarning(let group, let on):
                if on { groupWarnings.insert(group) } else { groupWarnings.remove(group) }
            case .playhead(let list, let cue): playheads[list] = cue
            case .preview(let cue, let active):
                if active { previewing.insert(cue) } else { previewing.remove(cue) }
            case .warning(let w):
                runtimeWarnings.append(w)
                if runtimeWarnings.count > Self.maxRuntimeWarnings {
                    runtimeWarnings.removeFirst(runtimeWarnings.count - Self.maxRuntimeWarnings)
                }
            case .unknown(let type):
                engineLog.notice("ignoring unknown runtime change \(type, privacy: .public)")
            }
        }
        return .applied
    }
}
