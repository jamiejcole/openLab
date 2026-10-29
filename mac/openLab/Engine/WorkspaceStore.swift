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
    /// Cues whose pre-wait or post-wait countdown changed, began or ended.
    case waits(Set<ModelID>)
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
    /// Pre-wait and post-wait countdowns in progress (`progress.waits`), by cue.
    @Published private(set) var waits: [ModelID: [WaitProgress.Kind: WaitProgress]] = [:]
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
    /// The newest show-log record number heard (`log_entry`, `log_tail`); 0: none on this connection.
    private(set) var logSeq: UInt64 = 0
    /// A `log_entry` arrived more than one past the last: records were not pushed, so a live view
    /// reads `log_tail` again (docs/protocol.md "Show log entries and status").
    let logGaps = PassthroughSubject<UInt64, Never>()
    /// A new connection (`welcome`): the engine may be a new run whose records count from 1 again.
    let logRuns = PassthroughSubject<Void, Never>()
    /// Every `log_entry` heard, in arrival order: an open log window appends those above the tail's `seq`.
    let logPushes = PassthroughSubject<(seq: UInt64, entry: ShowLogEntry), Never>()
    @Published private(set) var connectionState: ConnectionState = .idle
    /// This workspace's Show Mode (SM-01), as its window shows it. The engine is told so it can
    /// hold off sleep (SM-07); its `document.show_mode` is read back and wins when it changes.
    @Published private(set) var showMode = false
    /// `showMode` is not known to have reached the engine (a send failed or is under way): the
    /// next snapshot is told it rather than taken from it.
    private var showModeUnsent = false

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

    /// The engine closed this workspace (`closed` on its connection): its window goes at once.
    var onClosed: (() -> Void)?

    init(client: EngineClient) {
        self.client = client
        client.delegate = self
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

    /// Several actions sent together (Start Selected and the like); one result each, in order.
    func transport(_ actions: [TransportAction]) async -> [Result<OkBody, EngineError>] {
        await client.sendAll(actions.map { action in { .transport(id: $0, action) } })
    }

    func go(list: ModelID) async -> Result<OkBody, EngineError> { await transport(.go(list: list)) }
    func panic() async -> Result<OkBody, EngineError> { await transport(.panic) }
    func goFrom(list: ModelID, cue: ModelID) async -> Result<OkBody, EngineError> {
        await transport(.goFrom(list: list, cue: cue))
    }
    func stopAll() async -> Result<OkBody, EngineError> { await transport(.stopAll) }
    func hardStopAll() async -> Result<OkBody, EngineError> { await transport(.hardStopAll) }
    func panicCue(_ cue: ModelID) async -> Result<OkBody, EngineError> { await transport(.panicCue(cue: cue)) }

    /// `coalesce`: edits sent with the same key join one undo step (a fader drag; docs/commands.md).
    func send(command: Command, coalesce: String? = nil) async -> Result<OkBody, EngineError> {
        await client.send { .command(id: $0, command, coalesce: coalesce) }
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
    func logTail(max: UInt32? = nil) async -> Result<
        (entries: [ShowLogEntry], dropped: UInt64, seq: UInt64), EngineError
    > {
        switch await client.send({ .request(id: $0, .logTail(max: max)) }) {
        case .success(let ok):
            if case .logTail(let entries, let dropped, let seq)? = ok.data {
                logSeq = Swift.max(logSeq, seq)
                return .success((entries, dropped, seq))
            }
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

    /// Enters or leaves Show Mode at once (the UI locks before the engine answers) and tells the
    /// engine. A failure is logged and stays owed: the next snapshot (a reconnect) sends it again.
    /// The task's value is the engine's answer (tests wait on it).
    @discardableResult
    func setShowMode(_ on: Bool) -> Task<Result<OkBody, EngineError>, Never> {
        showMode = on
        showModeUnsent = true
        return Task { await sendShowMode() }
    }

    private func sendShowMode() async -> Result<OkBody, EngineError> {
        let on = showMode
        showModeUnsent = true
        let result = await client.send { .request(id: $0, .setShowMode(on: on)) }
        // A newer choice made meanwhile owns the flag.
        guard showMode == on else { return result }
        switch result {
        case .success: showModeUnsent = false
        case .failure(let e):
            _ = receive(.log(.warn, String(format: loc("showMode.error.engine"), EngineSession.describe(e))))
        }
        return result
    }

    /// After a snapshot (connect, reconnect, reattach): a mode the engine has not got (it restarted,
    /// or a send failed) is sent again; otherwise the engine's mode is taken (the app restarted
    /// while the engine kept the workspace in Show Mode).
    private func reconcileShowMode(engine: Bool) {
        if showModeUnsent || (showMode && !engine) {
            if showMode != engine { Task { _ = await sendShowMode() } } else { showModeUnsent = false }
        } else if showMode != engine {
            showMode = engine
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
        case .welcome(_, let snapshot):
            // A new connection may be a new engine run, whose records count from 1 again; a same-run
            // resync (`snapshot`) keeps the number, or the first entry after it could not show a gap.
            logSeq = 0
            logRuns.send()
            replace(with: snapshot)
        case .snapshot(let snapshot):
            replace(with: snapshot)
        case .modelChanged(let rev, let upserts, let deletes):
            return applyModelChange(revision: rev, upserts: upserts, deletes: deletes)
        case .runtime(let seq, let changes):
            return applyRuntime(seq: seq, changes: changes)
        case .document(let d):
            let engineChanged = d.showMode != (document?.showMode ?? false)
            document = d
            // Another client (or the engine) changed it: the engine's word is the mode now.
            if engineChanged, d.showMode != showMode, !showModeUnsent { showMode = d.showMode }
        case .progress(let items, let w):
            let old = progress
            let oldWaits = waits
            progress = Dictionary(items.map { ($0.cue, $0) }, uniquingKeysWith: { $1 })
            var next: [ModelID: [WaitProgress.Kind: WaitProgress]] = [:]
            for wait in w { next[wait.cue, default: [:]][wait.wait] = wait }
            waits = next
            let touched = Set(old.keys).union(progress.keys)
            if !touched.isEmpty { changes.send(.progress(touched)) }
            let waited = Set(oldWaits.keys).union(next.keys)
            if !waited.isEmpty { changes.send(.waits(waited)) }
        case .warnings(let w):
            warnings = w
            changes.send(.notices)
        case .log(let level, let text):
            logCount += 1
            logEntries.append(LogEntry(level: level, message: text))
            if logEntries.count > Self.maxLogEntries { logEntries.removeFirst(logEntries.count - Self.maxLogEntries) }
        case .status(let s):
            status = s
            // The hub drops a push for a slow client without telling it: the newest record's number
            // in `status` is how a trailing loss is noticed (docs/protocol.md "Show log entries and status").
            if s.logSeq > logSeq { logGaps.send(s.logSeq) }
        case .closed(let doc):
            if doc == client.workspace { onClosed?() }
        case .logEntry(let seq, let entry):
            // A live view appends the record; its number tells whether any were lost.
            logPushes.send((seq, entry))
            if logSeq > 0, seq > logSeq, seq - logSeq > 1 { logGaps.send(seq) }  // no `+ 1`: a hostile u64::MAX
            logSeq = Swift.max(logSeq, seq)
        case .meters, .result, .error, .appWelcome, .workspaces, .unknown:
            // Meters have no consumer yet; the rest belong to the client or session.
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
        waits = [:]
        runtimeWarnings = []
        // A new connection's health is unknown until its first `status`.
        status = nil
        reconcileShowMode(engine: s.document.showMode)
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
        var waitsEnded = Set<ModelID>()
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
                // Stop, Reset and Panic end the countdowns with the run (no last `progress` comes).
                if !state.isActive, waits[cue] != nil {
                    waits[cue] = nil
                    waitsEnded.insert(cue)
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
            case .mediaDuration:
                // Decoded for the Duration column (#112), which keeps them; nothing shows them yet.
                break
            case .unknown(let type):
                engineLog.notice("ignoring unknown runtime change \(type, privacy: .public)")
            }
        }
        if !cues.isEmpty || !heads.isEmpty { self.changes.send(.runtime(cues: cues, playheads: heads)) }
        if !progressEnded.isEmpty { self.changes.send(.progress(progressEnded)) }
        if !waitsEnded.isEmpty { self.changes.send(.waits(waitsEnded)) }
        if noticed { self.changes.send(.notices) }
        return .applied
    }
}
