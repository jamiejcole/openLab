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

/// What changed, for views that update rows instead of reloading (the cue list). Sent after the
/// store's own properties hold the new values.
enum StoreChange: Sendable {
    /// Everything was replaced (welcome or snapshot).
    case reset
    /// A `model_changed` that applied, exactly as the engine sent it.
    case model(upserts: [Upsert], deletes: [Delete])
    /// Cues whose runtime display changed; `playheads` are the cues a playhead left or reached.
    case runtime(cues: Set<ModelID>, playheads: Set<ModelID>)
    /// Cues whose progress changed (running cues, and cues that stopped running).
    case progress(Set<ModelID>)
    /// The workspace warnings were replaced, or a runtime warning arrived.
    case notices
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
    /// How many `log` messages and notes have been added in all (the entries are bounded, this is
    /// not): entry `i` of `logEntries` is number `logCount - logEntries.count + i`.
    private(set) var logCount = 0
    @Published private(set) var connectionState: ConnectionState = .idle

    let changes = PassthroughSubject<StoreChange, Never>()

    private(set) var revision: UInt64 = 0
    private(set) var runtimeSeq: UInt64 = 0
    let client: EngineClient
    private var stateSub: AnyCancellable?

    /// Cues looked at while applying model changes; tests compare it across list sizes to show an
    /// edit in place does not scan the list.
    static var cueVisits = 0

    static let maxRuntimeWarnings = 100
    static let maxLogEntries = 500

    /// `ownsDelegate: false` when something else (the session on a one-workspace engine) owns the
    /// client and forwards its messages to `receive`.
    init(client: EngineClient, ownsDelegate: Bool = true) {
        self.client = client
        if ownsDelegate { client.delegate = self }
        connectionState = client.state
        stateSub = client.$state.sink { [weak self] state in
            self?.connectionState = state
            if state != .connected { self?.clearPreviewing() }
        }
    }

    /// A dropped connection leaves nothing known to be previewing (the next snapshot says what is).
    private func clearPreviewing() {
        guard !previewing.isEmpty else { return }
        let old = previewing
        previewing = []
        changes.send(.runtime(cues: old, playheads: []))
    }

    // MARK: Intents (all go through the client)

    @discardableResult
    func transport(_ action: TransportAction) async -> Result<OkBody, EngineError> {
        await client.send { .transport(id: $0, action) }
    }

    func go(list: ModelID) async -> Result<OkBody, EngineError> { await transport(.go(list: list)) }
    func panic() async -> Result<OkBody, EngineError> { await transport(.panic) }
    func goFrom(list: ModelID, cue: ModelID) async -> Result<OkBody, EngineError> {
        await transport(.goFrom(list: list, cue: cue))
    }
    func stopAll() async -> Result<OkBody, EngineError> { await transport(.stopAll) }
    func hardStopAll() async -> Result<OkBody, EngineError> { await transport(.hardStopAll) }
    func panicCue(_ cue: ModelID) async -> Result<OkBody, EngineError> { await transport(.panicCue(cue: cue)) }

    func send(command: Command) async -> Result<OkBody, EngineError> {
        await client.send { .command(id: $0, command) }
    }

    func setProperty(_ cue: ModelID, _ property: CueProperty) async -> Result<OkBody, EngineError> {
        await send(command: .setProperty(cue, property))
    }

    /// nil clears the number.
    func setNumber(_ cue: ModelID, _ number: String?) async -> Result<OkBody, EngineError> {
        await send(command: .setNumber(cue, number))
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

    /// The pre-show check (SM-06): one report of what would keep the show from running.
    func preshowCheck() async -> Result<PreshowReport, EngineError> {
        switch await client.send({ .request(id: $0, .preshowCheck) }) {
        case .success(let ok):
            if case .preshowCheck(let report)? = ok.data { return .success(report) }
            return .failure(.encoding("preshow_check result has no preshow_check data"))
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
        case .progress(let items, _):
            let old = progress
            progress = Dictionary(items.map { ($0.cue, $0) }, uniquingKeysWith: { $1 })
            let touched = Set(old.keys).union(progress.keys)
            if !touched.isEmpty { changes.send(.progress(touched)) }
        case .warnings(let w):
            warnings = w
            changes.send(.notices)
        case .log(let level, let text):
            logCount += 1
            logEntries.append(LogEntry(level: level, message: text))
            if logEntries.count > Self.maxLogEntries { logEntries.removeFirst(logEntries.count - Self.maxLogEntries) }
        case .status(let s):
            status = s
        case .meters, .logEntry, .result, .error, .provisional, .unknown:
            // Meters and live log records have no consumer yet (the log window reads
            // `log_tail`); the rest belong to the client or session.
            break
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
        // A new connection's health is unknown until its first `status`.
        status = nil
        changes.send(.reset)
    }

    private func applyModelChange(revision rev: UInt64, upserts: [Upsert], deletes: [Delete]) -> ApplyOutcome {
        if rev <= revision { return .ignored }
        guard rev == revision + 1, var m = model else { return .gap }
        for d in deletes {
            switch d {
            case .cue(let id): Self.removeCue(id, fromLists: &m.cueLists)
            case .cueList(let id): m.cueLists.removeAll { $0.id == id }
            case .patch(let id): m.patches.removeAll { $0.id == id }
            case .networkPatch(let id): m.networkPatches.removeAll { $0.id == id }
            case .unknown: return .gap
            }
        }
        for u in upserts {
            switch u {
            case .cue(let list, let parent, let index, let cue):
                guard let li = m.cueLists.firstIndex(where: { $0.id == list }) else { return .gap }
                // An edit in place (the usual one) is found where the engine says it goes: no scan.
                if Self.replace(cue, under: parent, at: index, in: &m.cueLists[li].cues) { continue }
                Self.removeCue(cue.id, fromLists: &m.cueLists)
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
        changes.send(.model(upserts: upserts, deletes: deletes))
        return .applied
    }

    /// Removes the cue with `id` (ids are unique, so the first hit is the one); a scan that looks
    /// into groups that have children only.
    @discardableResult
    private static func removeCue(_ id: ModelID, from cues: inout [Cue]) -> Bool {
        if let i = cues.firstIndex(where: {
            cueVisits += 1
            return $0.id == id
        }) {
            cues.remove(at: i)
            return true
        }
        for i in cues.indices where !cues[i].children.isEmpty {
            if removeCue(id, from: &cues[i].children) { return true }
        }
        return false
    }

    private static func removeCue(_ id: ModelID, fromLists lists: inout [CueList]) {
        for i in lists.indices where removeCue(id, from: &lists[i].cues) { return }
    }

    /// Runs `body` on the children of the cue `parent`, wherever it is.
    private static func withChildren(of parent: ModelID, in cues: inout [Cue], _ body: (inout [Cue]) -> Void) -> Bool {
        for i in cues.indices where cues[i].id == parent || !cues[i].children.isEmpty {
            cueVisits += 1
            if cues[i].id == parent {
                body(&cues[i].children)
                return true
            }
            if withChildren(of: parent, in: &cues[i].children, body) { return true }
        }
        return false
    }

    /// Puts `cue` over the cue already at `index` under `parent` when that one has the same id.
    private static func replace(_ cue: Cue, under parent: ModelID?, at index: Int, in cues: inout [Cue]) -> Bool {
        func swap(_ list: inout [Cue]) -> Bool {
            cueVisits += 1
            guard list.indices.contains(index), list[index].id == cue.id else { return false }
            list[index] = cue
            return true
        }
        guard let parent else { return swap(&cues) }
        var done = false
        _ = withChildren(of: parent, in: &cues) { done = swap(&$0) }
        return done
    }

    private static func insert(_ cue: Cue, under parent: ModelID, at index: Int, in cues: inout [Cue]) -> Bool {
        withChildren(of: parent, in: &cues) { $0.insert(cue, at: min(index, $0.count)) }
    }

    private func applyRuntime(seq: UInt64, changes: [RuntimeChange]) -> ApplyOutcome {
        if seq <= runtimeSeq { return .ignored }
        guard seq == runtimeSeq + 1 else { return .gap }
        runtimeSeq = seq
        var cues = Set<ModelID>()
        var heads = Set<ModelID>()
        var progressEnded = Set<ModelID>()
        var noticed = false
        for change in changes {
            switch change {
            case .cueState(let cue, let state, let reason):
                cueStates[cue] = CueRunState(state: state, reason: reason)
                cues.insert(cue)
                if state != .running && state != .fading, progress[cue] != nil {
                    progress[cue] = nil
                    progressEnded.insert(cue)
                }
            case .problems(let cue, let list):
                problems[cue] = list.isEmpty ? nil : list
                cues.insert(cue)
            case .groupWarning(let group, let on):
                if on { groupWarnings.insert(group) } else { groupWarnings.remove(group) }
                cues.insert(group)
            case .playhead(let list, let cue):
                if let old = playheads[list] { heads.insert(old) }
                if let cue { heads.insert(cue) }
                playheads[list] = cue
            case .preview(let cue, let active):
                if active { previewing.insert(cue) } else { previewing.remove(cue) }
                cues.insert(cue)
            case .warning(let w):
                runtimeWarnings.append(w)
                noticed = true
                if runtimeWarnings.count > Self.maxRuntimeWarnings {
                    runtimeWarnings.removeFirst(runtimeWarnings.count - Self.maxRuntimeWarnings)
                }
            case .unknown(let type):
                engineLog.notice("ignoring unknown runtime change \(type, privacy: .public)")
            }
        }
        if !cues.isEmpty || !heads.isEmpty { self.changes.send(.runtime(cues: cues, playheads: heads)) }
        if !progressEnded.isEmpty { self.changes.send(.progress(progressEnded)) }
        if noticed { self.changes.send(.notices) }
        return .applied
    }
}
