// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// Finds or starts the engine and notices when it is gone (docs/control.md "Process
/// lifecycle"). No UI: it says what happened on `alarms` and `ready`.
///
/// Show safety: an engine this app run has talked to is never signalled. Ending an engine that
/// is alive but does not answer (SIGTERM, then SIGKILL) happens only at launch, before any
/// connection, after `probeAttempts` failed attempts. Later a dropped connection is retried for
/// as long as the engine's pid lives; only once it is gone is it an alarm and a new engine.
@MainActor
final class EngineSupervisor {
    enum Alarm: Equatable, Sendable {
        /// The engine this app was connected to is gone. `crashed`: it left `engine.json` behind
        /// (a graceful exit removes it).
        case engineStopped(crashed: Bool)
        /// No engine could be started (or several in a row died at once).
        case cannotStart(String)
    }

    let alarms = PassthroughSubject<Alarm, Never>()
    /// The app connection reached an engine run (the first one, or a new one after an alarm).
    let ready = PassthroughSubject<Void, Never>()
    /// An unclean exit was seen (a stale `engine.json` at launch, a crash, an engine ended by
    /// signal): the next engine's autosaves are worth offering. Cleared by whoever offers them.
    var recoveryPending = false

    /// Failed connection attempts to a live but silent engine before it is ended, at launch.
    var probeAttempts = 3
    /// How long an engine gets to exit after SIGTERM (its shutdown is bounded under 20 s on a
    /// hung disk), and after SIGKILL.
    var terminateWait = 25.0
    var killWait = 5.0
    var pollInterval = 0.5
    /// At most this many engine starts in `spawnWindow` seconds (a crash loop is an alarm).
    var maxSpawns = 3
    var spawnWindow = 60.0

    private let processes: EngineProcesses
    private let session: EngineSession
    private let scheduler: Scheduler
    private let now: @MainActor () -> Double
    private var sub: AnyCancellable?

    private var everConnected = false
    /// The engine of the last connection that reached `welcome`.
    private var lastRun: EngineConnectionInfo?
    /// The pids whose end was already handled (alarmed or ended by us).
    private var handled: Set<Int32> = []
    private var spawned: Int32?
    private var spawnTimes: [Double] = []
    private var silentAttempts = 0
    private var terminating = false
    private var spawnLimitReported = false
    /// The app is quitting: no engine is started any more.
    private var quitting = false

    init(
        processes: EngineProcesses, session: EngineSession, scheduler: Scheduler = SystemScheduler(),
        now: @escaping @MainActor () -> Double = { ProcessInfo.processInfo.systemUptime }
    ) {
        self.processes = processes
        self.session = session
        self.scheduler = scheduler
        self.now = now
    }

    /// At launch: start an engine if none runs (or the file is stale), then connect.
    func start() {
        guard sub == nil else { return }
        sub = session.$connectionState.sink { [weak self] state in
            MainActor.assumeIsolated { self?.observe(state) }
        }
        switch processes.readInfo() {
        case .failure(.missing):
            engineLog.notice("no engine running: starting one")
            spawnIfAllowed()
        case .failure(.unreadable(let why)):
            engineLog.error("engine.json unreadable (\(why, privacy: .public)): starting an engine")
            spawnIfAllowed()
        case .success(let info):
            if processes.isEngine(info.pid) {
                engineLog.notice("attaching to the engine with pid \(info.pid, privacy: .public)")
            } else {
                // The last engine did not exit cleanly (its file is left): offer its autosaves.
                engineLog.notice("stale engine.json (pid \(info.pid, privacy: .public) is gone): starting an engine")
                recoveryPending = true
                spawnIfAllowed()
            }
        }
        session.start()
    }

    // MARK: Connection state

    private func observe(_ state: ConnectionState) {
        switch state {
        case .connected:
            let run = session.appClient.connectedRun
            let newRun = run?.token != lastRun?.token
            everConnected = true
            lastRun = run
            silentAttempts = 0
            spawnLimitReported = false
            if newRun { ready.send() }
        case .retrying(let failure):
            evaluate(failure)
        case .failed(.versionMismatch(let v)):
            let detail = String(format: loc("engine.alarm.version"), v.map(String.init) ?? "?")
            alarms.send(.cannotStart(detail))
        case .idle, .connecting, .failed:
            break
        }
    }

    /// Called on each failed connection attempt: decides whether to wait, end a silent engine
    /// (launch only) or start a new one.
    private func evaluate(_ failure: ConnectionFailure) {
        guard !session.isShuttingDown, !terminating, !quitting else { return }
        let info = try? processes.readInfo().get()
        let filed = info.flatMap { processes.isEngine($0.pid) ? $0 : nil }

        guard everConnected, let run = lastRun else {
            // Launch: nothing is bound yet.
            if let live = filed {
                if live.pid == spawned { return }  // still starting
                // Only silence counts (refused, dropped, no welcome): an engine that answers with
                // an error is talking, and is never ended for it.
                switch failure {
                case .transport, .lost: break
                default: return
                }
                silentAttempts += 1
                if silentAttempts >= probeAttempts, !handled.contains(live.pid) {
                    Task { await terminateThenSpawn(live.pid) }
                }
                return
            }
            if let spawned, processes.isEngine(spawned) { return }  // starting: its file comes next
            spawnIfAllowed()
            return
        }

        // After a connection: an engine that is alive is only being reconnected to.
        if processes.isEngine(run.pid) { return }
        if !handled.contains(run.pid) {
            handled.insert(run.pid)
            let crashed = info?.pid == run.pid
            recoveryPending = true
            session.engineRunEnded()
            engineLog.error(
                "engine pid \(run.pid, privacy: .public) is gone (\(crashed ? "crashed" : "exited", privacy: .public)): starting a new one"
            )
            alarms.send(.engineStopped(crashed: crashed))
        }
        if filed != nil { return }  // another engine is up: the client attaches to it
        if let spawned, processes.isEngine(spawned) { return }
        spawnIfAllowed()
    }

    private func spawnIfAllowed() {
        guard !quitting else { return }
        let t = now()
        spawnTimes = spawnTimes.filter { t - $0 < spawnWindow }
        guard spawnTimes.count < maxSpawns else {
            if !spawnLimitReported {
                spawnLimitReported = true
                engineLog.error("the engine was started \(self.maxSpawns, privacy: .public) times in a minute: waiting")
                alarms.send(.cannotStart(loc("engine.alarm.crashLoop")))
            }
            return
        }
        spawnTimes.append(t)
        do {
            let pid = try processes.spawnEngine()
            spawned = pid
            engineLog.notice("started the engine, pid \(pid, privacy: .public)")
        } catch {
            engineLog.error("could not start the engine: \(String(describing: error), privacy: .public)")
            alarms.send(.cannotStart(Self.describe(error)))
        }
    }

    // MARK: Quit

    /// The engine this app run started that no connection has reached yet (it is still coming
    /// up), if it is alive. Nothing plays on it: no workspace was opened there.
    var engineNotReached: Int32? {
        guard let spawned, spawned != lastRun?.pid, processes.isEngine(spawned) else { return nil }
        return spawned
    }

    /// The quit is decided: start no other engine, and end the one this run started and never
    /// reached (it would otherwise run on with the audio device open and no app, since only an
    /// app connection that ends makes it relaunch the app). An engine this run talked to is told
    /// by `goodbye`, never signalled; one it did not start is never touched.
    func appWillQuit() {
        quitting = true
        guard let pid = engineNotReached else { return }
        handled.insert(pid)
        engineLog.notice("quitting before the engine pid \(pid, privacy: .public) was reached: sending SIGTERM")
        processes.signal(pid, .terminate)
    }

    /// Launch only: the engine named in `engine.json` is alive but never answered. A second
    /// engine refuses to start while it runs, so it is ended first.
    private func terminateThenSpawn(_ pid: Int32) async {
        guard !everConnected, !terminating else { return }
        terminating = true
        defer { terminating = false }
        handled.insert(pid)
        engineLog.error("engine pid \(pid, privacy: .public) does not answer: sending SIGTERM")
        processes.signal(pid, .terminate)
        if await !waitForExit(pid, seconds: terminateWait) {
            engineLog.error("engine pid \(pid, privacy: .public) did not exit: sending SIGKILL")
            processes.signal(pid, .kill)
            if await !waitForExit(pid, seconds: killWait) {
                engineLog.error("engine pid \(pid, privacy: .public) survived SIGKILL")
                alarms.send(.cannotStart(loc("engine.alarm.unkillable")))
                return
            }
        }
        recoveryPending = true
        guard !everConnected, !session.isShuttingDown else { return }
        spawnIfAllowed()
    }

    private func waitForExit(_ pid: Int32, seconds: Double) async -> Bool {
        let polls = max(1, Int((seconds / pollInterval).rounded(.up)))
        for _ in 0..<polls {
            if !processes.isEngine(pid) { return true }
            do { try await scheduler.sleep(seconds: pollInterval) } catch { return false }
        }
        return !processes.isEngine(pid)
    }

    private static func describe(_ error: Error) -> String {
        switch error as? EngineSpawnError {
        case .missingExecutable(let path)?: return String(format: loc("engine.alarm.missing"), path)
        case .spawnFailed(let code)?: return String(format: loc("engine.alarm.spawnFailed"), String(code))
        case nil: return loc("error.detail.generic")
        }
    }
}
