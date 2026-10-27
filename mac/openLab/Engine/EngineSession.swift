// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// The app-level engine session: the unbound connection, the list of open workspaces, and one
/// `EngineClient` + `WorkspaceStore` per workspace. No UI. The multi-workspace wire names are
/// provisional (ProvisionalProtocol.swift).
///
/// Against today's engine (one workspace; `new` / `open` replace it) the unbound connection is
/// also the workspace connection: the session then holds a single store under `singleDoc` and
/// `close` is unsupported. Against the multi-workspace engine each listed workspace gets its own
/// bound connection.
@MainActor
final class EngineSession: ObservableObject, EngineClientDelegate {
    enum Mode: Equatable { case unknown, single, multi }

    /// The doc id the session gives the one workspace of a one-workspace engine.
    static let singleDoc = ModelID("default")

    @Published private(set) var mode: Mode = .unknown
    @Published private(set) var workspaces: [WorkspaceInfo] = []
    @Published private(set) var stores: [ModelID: WorkspaceStore] = [:]
    @Published private(set) var connectionState: ConnectionState = .idle

    let appClient: EngineClient
    private let transport: EngineTransport
    private let infoProvider: @MainActor () -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>
    private let scheduler: Scheduler
    private let clientName: String
    private var stateSub: AnyCancellable?
    private var singleStore: WorkspaceStore?

    init(
        transport: EngineTransport,
        infoProvider: @escaping @MainActor () -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>,
        scheduler: Scheduler = SystemScheduler(), clientName: String = "openLab for macOS"
    ) {
        self.transport = transport
        self.infoProvider = infoProvider
        self.scheduler = scheduler
        self.clientName = clientName
        appClient = EngineClient(
            transport: transport, infoProvider: infoProvider, scheduler: scheduler, clientName: clientName)
        appClient.delegate = self
        stateSub = appClient.$state.sink { [weak self] in self?.connectionState = $0 }
    }

    func start() { appClient.start() }

    func stop() {
        appClient.stop()
        for s in stores.values where s !== singleStore { s.client.stop() }
        stores = [:]
        singleStore = nil
        workspaces = []
        mode = .unknown
    }

    // MARK: Calls

    func newWorkspace(discardChanges: Bool = false) async -> Result<ModelID, EngineError> {
        await opened(appClient.send { .request(id: $0, .new(discardChanges: discardChanges)) })
    }

    func open(path: String, discardChanges: Bool = false, recovery: RecoveryChoice? = nil) async -> Result<
        ModelID, EngineError
    > {
        await opened(
            appClient.send { .request(id: $0, .open(path: path, discardChanges: discardChanges, recovery: recovery)) })
    }

    func openRecovery(_ entry: RecoveryEntryInfo, discardChanges: Bool = false) async -> Result<ModelID, EngineError> {
        await opened(
            appClient.send {
                .request(
                    id: $0,
                    .openRecovery(
                        workspaceId: entry.workspaceId, pathHash: entry.pathHash, modifiedMs: entry.modifiedMs,
                        discardChanges: discardChanges))
            })
    }

    func scanRecovery() async -> Result<[RecoveryEntryInfo], EngineError> {
        switch await appClient.send({ .request(id: $0, .scanRecovery) }) {
        case .success(let ok):
            if case .recoveryEntries(let e)? = ok.data { return .success(e) }
            return .success([])
        case .failure(let e): return .failure(e)
        }
    }

    /// Closes a workspace on the engine (multi-workspace engines only).
    func close(doc: ModelID, discardChanges: Bool = false) async -> Result<Void, EngineError> {
        guard mode == .multi else { return .failure(.unsupported) }
        let result = await appClient.send { .provisional(.close(id: $0, doc: doc, discardChanges: discardChanges)) }
        return result.map { _ in () }
    }

    /// App-scope panic: the ordinary `transport: panic`, which a multi-workspace engine applies to
    /// every open workspace.
    func panicAll() async -> Result<OkBody, EngineError> {
        await appClient.send { .transport(id: $0, .panic) }
    }

    /// A `new` / `open` result: the new doc on a multi-workspace engine, the one doc otherwise.
    private func opened(_ result: Result<OkBody, EngineError>) -> Result<ModelID, EngineError> {
        result.map { ok in
            guard let doc = ok.data?.openedDoc else { return Self.singleDoc }
            ensureStore(doc)
            return doc
        }
    }

    // MARK: Delegate for the unbound connection

    /// No workspace on this connection means no revision to be behind (never triggers a resync).
    var revision: UInt64 { singleStore?.revision ?? .max }

    func engineClient(_ client: EngineClient, didReceive message: ServerMessage) -> ApplyOutcome {
        switch message {
        case .provisional(.appWelcome(_, let list)):
            mode = .multi
            dropSingleStore()
            setWorkspaces(list)
            return .applied
        case .provisional(.workspaces(let list)):
            setWorkspaces(list)
            return .applied
        case .provisional(.closed(let doc)):
            removeStore(doc)
            return .applied
        case .welcome:
            mode = .single
            let store = singleStore ?? makeSingleStore()
            let outcome = store.receive(message)
            refreshSingleInfo()
            return outcome
        default:
            guard let store = singleStore else { return .ignored }
            let outcome = store.receive(message)
            refreshSingleInfo()
            return outcome
        }
    }

    private func makeSingleStore() -> WorkspaceStore {
        let store = WorkspaceStore(client: appClient, ownsDelegate: false)
        singleStore = store
        stores[Self.singleDoc] = store
        return store
    }

    private func dropSingleStore() {
        if singleStore != nil { stores[Self.singleDoc] = nil }
        singleStore = nil
    }

    private func refreshSingleInfo() {
        guard let model = singleStore?.model, let doc = singleStore?.document else { return }
        workspaces = [
            WorkspaceInfo(
                doc: Self.singleDoc, workspaceId: model.id, name: model.name, path: doc.path, dirty: doc.dirty)
        ]
    }

    private func setWorkspaces(_ list: [WorkspaceInfo]) {
        workspaces = list
        for info in list { ensureStore(info.doc) }
        for doc in stores.keys where !list.contains(where: { $0.doc == doc }) { removeStore(doc) }
    }

    private func ensureStore(_ doc: ModelID) {
        guard mode == .multi, stores[doc] == nil else { return }
        let client = EngineClient(
            transport: transport, infoProvider: infoProvider, scheduler: scheduler, clientName: clientName,
            workspace: doc)
        stores[doc] = WorkspaceStore(client: client)
        client.start()
    }

    private func removeStore(_ doc: ModelID) {
        guard mode == .multi, let store = stores.removeValue(forKey: doc) else { return }
        store.client.stop()
        workspaces.removeAll { $0.doc == doc }
    }
}
