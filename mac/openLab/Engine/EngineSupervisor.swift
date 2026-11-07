// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// Finds or starts the engine and notices when it is gone. No UI: it says what happened on `alarms` and `ready`.
///
/// Show safety: an engine this app run has talked to is never signalled. Ending an engine that
/// is alive but does not answer (SIGTERM, then SIGKILL) happens only at launch, before any
/// connection, after `probeAttempts` refused or reset attempts (a slow `welcome` is busy, not silence). Later a dropped connection is retried for
/// as long as the engine's pid lives; only once it is gone is it an alarm and a new engine.
/// The one other engine ended is a spawned one no connection has reached (nothing is bound to it,
/// so nothing plays on it) that missed its start deadline: it wrote no `engine.json` in time, or
/// wrote it and did not answer in time. The deadline doubles each time that happens, and starts
/// again from `startDeadline` once a connection is made.
@MainActor
final class EngineSupervisor {
    enum Alarm: Equatable, Sendable {
        /// The engine this app was connected to is gone. `crashed`: it left `engine.json` behind
        /// (a graceful exit removes it).
        case engineStopped(crashed: Bool)
        /// No engine could be started (or several in a row died at once).
        case cannotStart(String)
        /// A live engine this app did not spawn accepts connections but sends no `welcome`,
        /// `probeAttempts` times in a row at launch. It is not signalled (its audio may be
        /// playing): the operator is told. Taken down when an engine answers.
        case notResponding
        /// The engine this app spawned wrote its file and failed `probeAttempts` attempts. No
        /// connection has reached it, so nothing plays on it: it is ended at its deadline
        /// (`startTimedOut`) unless it answers first. Taken down when an engine answers.
        case newEngineNotAnswering
        /// The engine this app spawned wrote no `engine.json` in time, or wrote it and did not
        /// answer in time: it is being ended and replaced (within the spawn limit). Taken down
        /// when an engine answers.
        case startTimedOut
        /// An engine whose `engine.json` says it is stopping has not exited within `stoppingWait`.
        /// It is not signalled; a new engine starts once it has gone.
        case stillStopping
    }

    let alarms = PassthroughSubject<Alarm, Never>()
    /// The app connection reached an engine run (the first one, or a new one after an alarm).
    let ready = PassthroughSubject<Void, Never>()
    /// An unclean exit was seen (a stale `engine.json` at launch, a crash, an engine ended by
    /// signal): the next engine's autosaves are worth offering. Cleared by whoever offers them.
    var recoveryPending = false

    /// Failed connection attempts before something is done about a live engine: one not spawned
    /// here is ended at launch if silent, or alarmed if busy; one spawned here is alarmed.
    var probeAttempts = 3
    /// How long an engine gets to exit after SIGTERM (its shutdown is bounded under 20 s on a
    /// hung disk), and after SIGKILL.
    var terminateWait = 25.0
    var killWait = 5.0
    /// How long an engine that says it is stopping gets to exit before the operator is told (the
    /// engine forces its own exit 24 s after its shutdown began).
    var stoppingWait = 25.0
    var pollInterval = 0.5
    /// At most this many engine starts in `spawnWindow` seconds (a crash loop is an alarm).
    var maxSpawns = 3
    var spawnWindow = 60.0
    /// How long an engine this app spawned gets to write `engine.json`, and then again to answer
    /// once the file is there. Generous: a cold start on a 2014 Mac mini with a hard disk (first
    /// exec checked by the system, audio devices enumerated) takes tens of seconds. It applies only
    /// until a connection reaches the engine, and doubles for each consecutive engine that missed
    /// it (reset by a connection), so a start of any finite length eventually runs.
    var startDeadline = 90.0

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
    /// The engine this run spawned, until a connection reaches it: from then on it is an engine
    /// like any other, and nothing here may end it.
    private var spawned: Int32?
    private var spawnedAt = 0.0
    /// When `spawned` was first found in `engine.json` and failed to answer.
    private var unansweredSince: Double?
    /// The attempts `spawned` has failed since its file appeared.
    private var unansweredAttempts = 0
    /// Consecutive spawned engines that missed their deadline.
    private var startTimeouts = 0
    private var spawnTimes: [Double] = []
    private var silentAttempts = 0
    private var busyAttempts = 0
    private var terminating = false
    private var spawnLimitReported = false
    /// The app is quitting: no engine is started any more.
    private var quitting = false
    /// The pid of a stopping engine being waited for before another is started.
    private var awaitingStop: Int32?

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
            if processes.isEngine(info.pid), info.stopping {
                awaitStop(info.pid)
            } else if processes.isEngine(info.pid) {
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
            if run?.pid == spawned { spawned = nil }
            silentAttempts = 0
            busyAttempts = 0
            startTimeouts = 0
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
                if live.stopping { return awaitStop(live.pid) }
                if live.pid == spawned { return spawnedEngineDidNotAnswer(live.pid, failure) }
                // Only silence counts (refused or reset connections): an engine that answers with
                // an error is talking, and one that accepts the socket but is slow to send its
                // `welcome` is busy (its control loop may be stalled while the audio thread plays),
                // so neither is ever ended for it.
                switch failure {
                case .transport, .lost: break
                case .noWelcome:
                    engineLog.error(
                        "engine pid \(live.pid, privacy: .public) accepted the connection but sent no welcome: busy, waiting"
                    )
                    busyAttempts += 1
                    if busyAttempts == probeAttempts { alarms.send(.notResponding) }
                    return
                default: return
                }
                silentAttempts += 1
                if silentAttempts >= probeAttempts, !handled.contains(live.pid) {
                    Task { await terminateThenSpawn(live.pid) }
                }
                return
            }
            if stillStarting() { return }  // its file comes next
            spawnIfAllowed()
            return
        }

        // After a connection: an engine that is alive is only being reconnected to.
        if processes.isEngine(run.pid) { return }
        if !handled.contains(run.pid) {
            handled.insert(run.pid)
            // A file left behind is a crash, unless it says the shutdown had begun.
            let crashed = info?.pid == run.pid && info?.stopping != true
            recoveryPending = true
            session.engineRunEnded()
            engineLog.error(
                "engine pid \(run.pid, privacy: .public) is gone (\(crashed ? "crashed" : "exited", privacy: .public)): starting a new one"
            )
            alarms.send(.engineStopped(crashed: crashed))
        }
        if let live = filed {  // another engine is up: the client attaches to it once it is not stopping
            if live.stopping {
                awaitStop(live.pid)
            } else if live.pid == spawned {
                spawnedEngineDidNotAnswer(live.pid, failure)
            }
            return
        }
        if stillStarting() { return }
        spawnIfAllowed()
    }

    /// True while the engine this run spawned is alive and has not written `engine.json` (it comes
    /// next). Past the start deadline that engine is ended and replaced, and this still answers
    /// true: the replacement is that task's.
    private func stillStarting() -> Bool {
        guard let pid = spawned, processes.isEngine(pid) else { return false }
        _ = endIfStartMissed(pid, since: spawnedAt, doing: "wrote no engine.json")
        return true
    }

    /// The engine this run spawned wrote its file and this attempt did not reach it. A valid start
    /// is never refused (it listens before it writes the file) and its control loop starts right
    /// after the file, so the alarm after `probeAttempts` is no false one: refused, or about 30 s
    /// of no `welcome`. No connection has reached this engine, so nothing plays on it: unlike any
    /// other engine it is ended, once the start deadline has passed since the file appeared.
    private func spawnedEngineDidNotAnswer(_ pid: Int32, _ failure: ConnectionFailure) {
        let since = unansweredSince ?? now()
        unansweredSince = since
        if endIfStartMissed(pid, since: since, doing: "did not answer") { return }
        switch failure {
        // `engine`: the engine's own snapshot wait can end before the handshake's, with `internal`.
        case .transport, .lost, .noWelcome, .engine, .badToken:
            unansweredAttempts += 1
            guard unansweredAttempts == probeAttempts else { return }
            engineLog.error("engine pid \(pid, privacy: .public) wrote engine.json but does not answer")
            alarms.send(.newEngineNotAnswering)
        default: break
        }
    }

    /// Past the start deadline counted from `since`, alarms, ends and replaces the unreached engine
    /// `pid` (that pid only, within the spawn limit). Answers whether it did.
    private func endIfStartMissed(_ pid: Int32, since: Double, doing what: String) -> Bool {
        let deadline = startDeadline * Double(1 << min(startTimeouts, 10))
        guard now() - since >= deadline, !handled.contains(pid) else { return false }
        handled.insert(pid)
        startTimeouts += 1
        engineLog.error(
            "engine pid \(pid, privacy: .public) \(what, privacy: .public) within \(deadline, privacy: .public) s: ending it"
        )
        alarms.send(.startTimedOut)
        Task { await terminateThenSpawn(pid, silent: false) }
        return true
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
            spawnedAt = t
            unansweredSince = nil
            unansweredAttempts = 0
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
        guard let spawned, processes.isEngine(spawned) else { return nil }
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

    /// The engine `pid` is alive but never answered (`silent`: not spawned here, named in
    /// `engine.json`, launch only), or was spawned here and missed its start deadline. A second
    /// engine refuses to start while it runs, so it is ended first.
    private func terminateThenSpawn(_ pid: Int32, silent: Bool = true) async {
        guard !silent || !everConnected, !terminating else { return }
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
        if silent { recoveryPending = true }  // an engine never reached has nothing to recover
        guard !silent || !everConnected, !session.isShuttingDown, !quitting else { return }
        spawnIfAllowed()
    }

    /// An engine whose shutdown has begun is never attached to and never signalled: wait for it
    /// to exit, then start the next engine (a start before that would be refused by its lock).
    private func awaitStop(_ pid: Int32) {
        guard awaitingStop == nil, !quitting else { return }
        awaitingStop = pid
        engineLog.notice("engine pid \(pid, privacy: .public) is stopping: waiting for it to exit")
        Task { await waitForStopThenSpawn(pid) }
    }

    private func waitForStopThenSpawn(_ pid: Int32) async {
        if await !waitForExit(pid, seconds: stoppingWait) {
            engineLog.error(
                "engine pid \(pid, privacy: .public) is still stopping after \(self.stoppingWait, privacy: .public) s: waiting"
            )
            if !quitting { alarms.send(.stillStopping) }
            while processes.isEngine(pid), !quitting {
                do { try await scheduler.sleep(seconds: pollInterval) } catch {
                    awaitingStop = nil
                    return
                }
            }
        }
        awaitingStop = nil
        guard !quitting, !session.isShuttingDown else { return }
        engineLog.notice("engine pid \(pid, privacy: .public) has exited")
        // Its shutdown wrote the recovery files of its unsaved workspaces, or was forced out at its
        // deadline before it could (a forced exit removes engine.json too, so the file says
        // nothing): offer the autosaves either way. With none, the offer shows nothing (#388).
        recoveryPending = true
        if let info = try? processes.readInfo().get(), processes.isEngine(info.pid) {
            // Another engine is up (started by someone else): attach to it, or wait for it too.
            if info.stopping { awaitStop(info.pid) }
            return
        }
        if let spawned, spawned != pid, processes.isEngine(spawned) { return }
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
