// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// A window that could not be reattached after an engine restart (its show is gone, unreadable,
/// or an untitled workspace with no autosave).
struct ReattachFailure: Equatable, Sendable {
    var name: String
    var reason: String
}

enum SessionEvent: Equatable, Sendable {
    /// The app connection reached an engine run other than the one the windows were bound in.
    case newEngineRun
    /// Every window waiting for the new engine run was reopened, or failed (its window closed).
    /// `restored`: the workspaces whose newer autosave was opened. `editsLost`: windows that had
    /// unsaved changes and were reopened from their file (no autosave newer than it was left).
    case reattached(
        failures: [ReattachFailure], restored: [String], editsLost: [String] = [], editsLostWorkspaces: [ModelID] = [])
}

/// The app-level engine session: the unbound app connection, the list of open workspaces, and one
/// `EngineClient` + `WorkspaceStore` per workspace, in open order. No UI. Protocol 2 only: an
/// engine of another version refuses the `hello`.
///
/// Doc ids belong to one engine run: when the app connection reaches a new run (another token),
/// every store is detached and rebound by reopening its show (`open`; an untitled one from its
/// autosave), never by its old doc id. The store, and so its window, survives.
@MainActor
final class EngineSession: ObservableObject, EngineClientDelegate {
    @Published private(set) var workspaces: [WorkspaceInfo] = []
    @Published private(set) var connectionState: ConnectionState = .idle
    private(set) var stores: [ModelID: WorkspaceStore] = [:]
    /// The docs of `stores` in open order (the `workspaces` list order; a doc not listed yet last).
    private(set) var order: [ModelID] = []
    /// Stores waiting to be rebound in a new engine run, in window order, with what their window
    /// showed when the old run ended.
    private(set) var detached: [(info: WorkspaceInfo, store: WorkspaceStore)] = []
    /// Sent after `stores`, `order` or `detached` changed.
    let storesChanged = PassthroughSubject<Void, Never>()
    let events = PassthroughSubject<SessionEvent, Never>()

    let appClient: EngineClient
    private let transport: EngineTransport
    private let infoProvider: @MainActor () -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>
    private let scheduler: Scheduler
    private let clientName: String
    private var stateSub: AnyCancellable?
    /// The engine run the stores' doc ids belong to.
    private(set) var runToken: String?
    private var reattachTask: Task<Void, Never>?

    init(
        transport: EngineTransport,
        infoProvider: @escaping @MainActor () -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>,
        scheduler: Scheduler = SystemScheduler(), handshakeScheduler: Scheduler = SystemScheduler(),
        clientName: String = "openLab for macOS"
    ) {
        self.transport = transport
        self.infoProvider = infoProvider
        self.scheduler = scheduler
        self.clientName = clientName
        appClient = EngineClient(
            transport: transport, infoProvider: infoProvider, scheduler: scheduler,
            handshakeScheduler: handshakeScheduler, clientName: clientName)
        appClient.delegate = self
        stateSub = appClient.$state.sink { [weak self] in self?.connectionState = $0 }
    }

    func start() { appClient.start() }

    func stop() {
        reattachTask?.cancel()
        reattachTask = nil
        appClient.stop()
        for s in stores.values { s.client.stop() }
        for d in detached { d.store.client.stop() }
        stores = [:]
        order = []
        detached = []
        workspaces = []
        pendingCovers = [:]
        offeredAutosaves = [:]
        storesChanged.send()
    }

    /// The stores in open order.
    var orderedStores: [(doc: ModelID, store: WorkspaceStore)] {
        order.compactMap { doc in stores[doc].map { (doc, $0) } }
    }

    func doc(of store: WorkspaceStore) -> ModelID? {
        stores.first { $0.value === store }?.key
    }

    // MARK: Calls

    func newWorkspace() async -> Result<ModelID, EngineError> {
        await opened(appClient.send { .request(id: $0, .new) })
    }

    func open(path: String, recovery: RecoveryChoice? = nil) async -> Result<ModelID, EngineError> {
        let result = await opened(
            appClient.send { .request(id: $0, .open(path: path, recovery: recovery)) }, path: path)
        if recovery == .restore, case .success(let doc) = result, let ms = offeredAutosaves[path] {
            cover(doc) { $0.loadOwn(modifiedMs: ms) }
        }
        // Keyed by the path asked for: the caller repeats it with `restore`.
        if case .failure(.recoveryOffered(_, _, let ms)) = result { offeredAutosaves[path] = ms }
        if recovery != nil { offeredAutosaves[path] = nil }
        return result
    }

    func openRecovery(_ entry: RecoveryEntryInfo) async -> Result<ModelID, EngineError> {
        let result = await opened(sendOpenRecovery(entry))
        if case .success(let doc) = result { cover(doc) { $0.load(entry) } }
        return result
    }

    /// The autosave time of each path's last `recovery_offer`: a `restore` opens that autosave.
    private var offeredAutosaves: [String: Int64] = [:]
    /// Covers for docs whose store is not made yet (one opened while windows are being reattached).
    private var pendingCovers: [ModelID: AutosaveCover] = [:]

    private func cover(_ doc: ModelID, _ update: (inout AutosaveCover) -> Void) {
        if let store = stores[doc] {
            update(&store.autosaveCover)
        } else {
            update(&pendingCovers[doc, default: AutosaveCover()])
        }
    }

    func scanRecovery() async -> Result<[RecoveryEntryInfo], EngineError> {
        switch await appClient.send({ .request(id: $0, .scanRecovery) }) {
        case .success(let ok):
            if case .recoveryEntries(let e)? = ok.data { return .success(e) }
            return .success([])
        case .failure(let e): return .failure(e)
        }
    }

    /// Closes a workspace on the engine.
    func close(doc: ModelID, discardChanges: Bool = false) async -> Result<Void, EngineError> {
        let result = await appClient.send { .close(id: $0, doc: doc, discardChanges: discardChanges) }
        return result.map { _ in () }
    }

    /// App-scope panic (Esc): every open workspace.
    func panicAll() async -> Result<OkBody, EngineError> {
        await appClient.send { .panic(id: $0) }
    }

    /// App-scope cue hotkey: the engine matches it in every open workspace.
    func hotkey(key: String, modifiers: HotkeyModifiers, up: Bool) async -> Result<OkBody, EngineError> {
        await appClient.send { .hotkey(id: $0, key: key, modifiers: modifiers, up: up) }
    }

    /// Set once `shutdown` began: the engine's end that follows is not a crash.
    private(set) var isShuttingDown = false

    /// Windows waiting to be rebound in a new engine run, or a reattach under way.
    var isReattaching: Bool { reattachTask != nil || !detached.isEmpty }

    /// Quit: tells the engine to exit (it writes recovery for every dirty doc), then drops the connections.
    func shutdown() async {
        isShuttingDown = true
        await appClient.goodbye(shutdown: true)
        stop()
    }

    private func sendOpenRecovery(_ entry: RecoveryEntryInfo) async -> Result<OkBody, EngineError> {
        await appClient.send {
            .request(
                id: $0,
                .openRecovery(workspaceId: entry.workspaceId, pathHash: entry.pathHash, modifiedMs: entry.modifiedMs))
        }
    }

    /// A `new` / `open` result: the new doc, or the autosave offer that opened nothing.
    private func opened(_ result: Result<OkBody, EngineError>, path: String? = nil) -> Result<ModelID, EngineError> {
        result.flatMap { ok in
            switch ok.data {
            case .opened(let doc)?:
                ensureStore(doc, openedPath: path)
                return .success(doc)
            case .recoveryOffer(let path, let name, let modified)?:
                return .failure(.recoveryOffered(path: path, workspaceName: name, modifiedMs: modified))
            default:
                return .failure(.encoding("open result has no opened doc"))
            }
        }
    }

    // MARK: Delegate for the app connection

    /// No workspace on this connection means no revision to be behind (never triggers a resync).
    var revision: UInt64 { .max }

    func engineClient(_ client: EngineClient, didReceive message: ServerMessage) -> ApplyOutcome {
        switch message {
        case .appWelcome(_, let list):
            let token = client.connectedRun?.token
            if runToken != nil, token != runToken, !stores.isEmpty || !detached.isEmpty {
                detachAll()
            }
            runToken = token
            setWorkspaces(list)
            if !detached.isEmpty { startReattach() }
            return .applied
        case .workspaces(let list):
            setWorkspaces(list)
            return .applied
        case .closed(let doc):
            removeStore(doc)
            return .applied
        case .welcome:
            // A doc's welcome on the app connection: not a protocol 2 engine (it refuses at hello).
            engineLog.error("doc welcome on the app connection ignored")
            return .ignored
        default:
            return .ignored
        }
    }

    private func setWorkspaces(_ list: [WorkspaceInfo]) {
        workspaces = list
        // While windows wait to be rebound, a listed doc may be one of theirs being reopened: only
        // the reattach decides (it adds the rest when done), so no window is ever made twice.
        if detached.isEmpty && reattachTask == nil {
            for info in list where stores[info.doc] == nil { makeStore(info.doc) }
        }
        for doc in stores.keys where !list.contains(where: { $0.doc == doc }) { dropStore(doc) }
        var seen = Set<ModelID>()
        let listed = list.map(\.doc).filter { stores[$0] != nil && seen.insert($0).inserted }
        order = listed + order.filter { !listed.contains($0) && stores[$0] != nil }
        storesChanged.send()
    }

    /// A doc the operator just opened gets its store (and so its window) at once, unless a
    /// window waiting to be rebound will claim it: the reattach takes a doc open at that window's
    /// path (`alreadyOpen`), so a second store would leave that window failed. `openedPath` is the
    /// path asked for, since the engine sends the `result` before the `workspaces` list that names
    /// the doc. A new or other file's doc is nobody's.
    private func ensureStore(_ doc: ModelID, openedPath: String?) {
        guard stores[doc] == nil else { return }
        if let openedPath, !detached.isEmpty || reattachTask != nil,
            detached.contains(where: { $0.info.path.map { Self.samePath($0, openedPath) } ?? false })
        {
            return
        }
        makeStore(doc)
        storesChanged.send()
    }

    /// The engine's path equality, close enough: NFC and case-insensitive. A false "same" only
    /// defers a store to the end of the reattach.
    static func samePath(_ a: String, _ b: String) -> Bool {
        a.precomposedStringWithCanonicalMapping.lowercased() == b.precomposedStringWithCanonicalMapping.lowercased()
    }

    private func makeStore(_ doc: ModelID) {
        let client = EngineClient(
            transport: transport, infoProvider: infoProvider, scheduler: scheduler, clientName: clientName,
            workspace: doc, runToken: runToken)
        let store = WorkspaceStore(client: client)
        if let cover = pendingCovers.removeValue(forKey: doc) { store.autosaveCover.merge(cover) }
        attach(store, as: doc)
        client.start()
    }

    private func attach(_ store: WorkspaceStore, as doc: ModelID) {
        stores[doc] = store
        if !order.contains(doc) { order.append(doc) }
        store.onClosed = { [weak self, weak store] in
            guard let self, let store, let doc = self.doc(of: store) else { return }
            self.removeStore(doc)
        }
    }

    private func removeStore(_ doc: ModelID) {
        guard stores[doc] != nil else { return }
        dropStore(doc)
        workspaces.removeAll { $0.doc == doc }
        storesChanged.send()
    }

    private func dropStore(_ doc: ModelID) {
        guard let store = stores.removeValue(forKey: doc) else { return }
        order.removeAll { $0 == doc }
        store.client.stop()
    }

    // MARK: Reattach after an engine restart

    /// The engine the stores were bound in is gone (its pid ended): every cue it played stopped.
    func engineRunEnded() {
        for s in stores.values { s.clearRuntime() }
    }

    /// Every store leaves its doc id (it means nothing in the new run) and waits to be rebound.
    private func detachAll() {
        reattachTask?.cancel()
        reattachTask = nil
        for (doc, store) in orderedStores {
            store.client.stop()
            store.clearRuntime()
            detached.append((info(of: store, doc: doc), store))
        }
        stores = [:]
        order = []
        pendingCovers = [:]  // doc ids of the old run
        events.send(.newEngineRun)
        storesChanged.send()
    }

    /// What the window showed: the store's own document state is the newest.
    private func info(of store: WorkspaceStore, doc: ModelID) -> WorkspaceInfo {
        let listed = workspaces.first { $0.doc == doc }
        return WorkspaceInfo(
            doc: doc, workspaceId: store.model?.id ?? listed?.workspaceId ?? ModelID(""),
            name: store.model?.name ?? listed?.name ?? "",
            path: store.document != nil ? store.document?.path : listed?.path,
            dirty: store.document?.dirty ?? listed?.dirty ?? false)
    }

    private func startReattach() {
        guard reattachTask == nil else { return }
        reattachTask = Task { [weak self] in await self?.reattach() }
    }

    /// Where a rebound window's show came from.
    private enum Source {
        case file, autosave
        /// The saved file, although the window had unsaved changes: they are gone.
        case fileEditsLost
    }

    private enum Rebind {
        /// `cover`: the autosave it was reopened from, if known.
        case bound(ModelID, Source, cover: AutosaveCover = AutosaveCover())
        case failed(String)
        /// The app connection dropped: try again at the next `welcome`.
        case interrupted
    }

    private var reattachFailures: [ReattachFailure] = []
    private var reattachRestored: [String] = []
    private var reattachEditsLost: [String] = []
    private var reattachEditsLostIDs: [ModelID] = []

    /// Reopens one window's show at a time (the engine takes one document request at a time).
    private func reattach() async {
        var recovery: [RecoveryEntryInfo]?
        while let next = detached.first, !Task.isCancelled {
            let outcome = await rebind(next.info, recovery: &recovery)
            guard !Task.isCancelled, detached.first?.store === next.store else { return }
            switch outcome {
            case .interrupted:
                reattachTask = nil
                return
            case .bound(let doc, let source, let cover):
                detached.removeFirst()
                next.store.autosaveCover.merge(cover)
                attach(next.store, as: doc)
                next.store.client.rebind(workspace: doc, runToken: runToken)
                switch source {
                case .file:
                    _ = next.store.receive(.log(.info, loc("engine.reattach.reopened")))
                case .autosave:
                    _ = next.store.receive(.log(.warn, loc("engine.reattach.restored")))
                    reattachRestored.append(next.info.name)
                case .fileEditsLost:
                    _ = next.store.receive(.log(.warn, loc("engine.reattach.editsLost")))
                    reattachEditsLost.append(next.info.name)
                    reattachEditsLostIDs.append(next.info.workspaceId)
                    engineLog.error(
                        "reopened \(next.info.name, privacy: .public) from its file: its unsaved edits had no autosave")
                }
            case .failed(let reason):
                detached.removeFirst()
                next.store.client.stop()
                reattachFailures.append(ReattachFailure(name: next.info.name, reason: reason))
                engineLog.error("could not reattach \(next.info.name, privacy: .public): \(reason, privacy: .public)")
            }
            storesChanged.send()
        }
        guard !Task.isCancelled else { return }
        reattachTask = nil
        setWorkspaces(workspaces)  // the docs no window claimed (opened by another client) get theirs
        let failures = reattachFailures
        let restored = reattachRestored
        let editsLost = reattachEditsLost
        let editsLostIDs = reattachEditsLostIDs
        reattachFailures = []
        reattachRestored = []
        reattachEditsLost = []
        reattachEditsLostIDs = []
        events.send(
            .reattached(
                failures: failures, restored: restored, editsLost: editsLost, editsLostWorkspaces: editsLostIDs))
    }

    private func rebind(_ info: WorkspaceInfo, recovery: inout [RecoveryEntryInfo]?) async -> Rebind {
        if let path = info.path {
            // With no autosave newer than the file, a window with unsaved changes loses them.
            var source: Source = info.dirty ? .fileEditsLost : .file
            var cover = AutosaveCover()
            var result = await openForRebind(.open(path: path, recovery: nil))
            if case .success(let ok) = result, case .recoveryOffer(_, _, let modified)? = ok.data {
                // An autosave newer than the file: the window showed it if it had unsaved changes
                // (the engine stopped before they were saved); otherwise it showed the saved file.
                source = info.dirty ? .autosave : .file
                if info.dirty { cover.loadOwn(modifiedMs: modified) }
                result = await openForRebind(.open(path: path, recovery: info.dirty ? .restore : .ignore))
            }
            switch result {
            case .success(let ok):
                if let doc = ok.data?.openedDoc, stores[doc] == nil { return .bound(doc, source, cover: cover) }
                return .failed(loc("engine.reattach.noDoc"))
            case .failure(.engine(.alreadyOpen, let message)):
                // Opened in this run already (a reattach cut short, or another client): take it.
                // A restored autosave opens dirty, the saved file clean: a window with unsaved
                // changes bound to a clean doc lost them (#337).
                if let listed = workspaces.first(where: { $0.path.map { Self.samePath($0, path) } ?? false }),
                    stores[listed.doc] == nil
                {
                    let source: Source = !info.dirty ? .file : listed.dirty ? .autosave : .fileEditsLost
                    return .bound(listed.doc, source)
                }
                return .failed(message)
            case .failure(.notConnected), .failure(.disconnected): return .interrupted
            case .failure(let e): return .failed(Self.describe(e))
            }
        }
        // Untitled: only its autosave holds it (a clean one is an empty new workspace).
        if recovery == nil {
            switch await scanRecovery() {
            case .success(let entries): recovery = entries
            case .failure(.notConnected), .failure(.disconnected): return .interrupted
            case .failure: recovery = []
            }
        }
        let request: Request
        let entry = recovery?.first { $0.workspaceId == info.workspaceId }
        if let entry {
            request = .openRecovery(
                workspaceId: entry.workspaceId, pathHash: entry.pathHash, modifiedMs: entry.modifiedMs)
        } else if !info.dirty {
            request = .new
        } else {
            return .failed(loc("engine.reattach.noAutosave"))
        }
        switch await openForRebind(request) {
        case .success(let ok):
            if let doc = ok.data?.openedDoc, stores[doc] == nil {
                var cover = AutosaveCover()
                if let entry { cover.load(entry) }
                return .bound(doc, request == .new ? .file : .autosave, cover: cover)
            }
            return .failed(loc("engine.reattach.noDoc"))
        case .failure(.notConnected), .failure(.disconnected): return .interrupted
        case .failure(let e): return .failed(Self.describe(e))
        }
    }

    /// One document request; `command_refused` (another client's request is being handled) is
    /// tried again a few times.
    private func openForRebind(_ request: Request) async -> Result<OkBody, EngineError> {
        var result = await appClient.send { .request(id: $0, request) }
        for _ in 0..<5 {
            guard case .failure(.engine(.commandRefused, _)) = result else { break }
            do { try await scheduler.sleep(seconds: 0.5) } catch { break }
            result = await appClient.send { .request(id: $0, request) }
        }
        return result
    }

    static func describe(_ e: EngineError) -> String {
        switch e {
        case .engine(_, let message) where !message.isEmpty: return message
        case .notConnected, .disconnected: return loc("error.detail.notConnected")
        case .timeout: return loc("error.detail.timeout")
        default: return loc("error.detail.generic")
        }
    }
}
