// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import XCTest

@testable import openLab

/// Waits return at once: the supervisor's SIGTERM / SIGKILL grace periods take no real time.
struct ImmediateScheduler: openLab.Scheduler {
    func sleep(seconds: Double) async throws { await Task.yield() }
}

/// `engine.json` and the process table, scripted.
@MainActor
final class ScriptedProcesses: EngineProcesses {
    var info: Result<EngineConnectionInfo, EngineConnectionInfo.LoadError> = .failure(.missing)
    var alive: Set<Int32> = []
    /// Which signals end a process.
    var diesOn: Set<String> = ["terminate", "kill"]
    private(set) var signals: [(pid: Int32, signal: EngineSignal)] = []
    private(set) var spawns = 0
    var spawnError: EngineSpawnError?
    /// What a spawn does (default: nothing comes up).
    var onSpawn: (() -> Int32)?

    func readInfo() -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError> { info }
    func isEngine(_ pid: Int32) -> Bool { alive.contains(pid) }

    func signal(_ pid: Int32, _ signal: EngineSignal) {
        signals.append((pid, signal))
        if diesOn.contains(signal == .terminate ? "terminate" : "kill") { alive.remove(pid) }
    }

    func spawnEngine() throws -> Int32 {
        spawns += 1
        if let spawnError { throw spawnError }
        return onSpawn?() ?? 0
    }
}

@MainActor
final class RecordingPresenter: EngineAlarmPresenting {
    private(set) var shown: [(title: String, message: String)] = []
    private(set) var detail = ""
    private(set) var restore: (() -> Void)?
    private(set) var isVisible = false

    func show(title: String, message: String) {
        shown.append((title, message))
        isVisible = true
    }
    func setDetail(_ text: String) { detail = text }
    func setRestoreAction(_ action: (() -> Void)?) { restore = action }
    func dismiss() { isVisible = false }
}

/// The supervisor's policy: the process table and engine.json are scripted (`ScriptedProcesses`),
/// the engine's socket is the wire stub (no cue model; EngineClientTests.swift). The real engine's
/// crash, respawn and reattach run in EngineCrashFlowTests.
@MainActor
final class EngineSupervisorTests: XCTestCase {
    private let scheduler = ManualScheduler()
    private let handshakeScheduler = ManualScheduler()
    private var engine: StubEngine!
    /// The pid the scripted process table gives the stub's engine.
    private let enginePid: Int32 = 100
    private var engineInfo: EngineConnectionInfo {
        EngineConnectionInfo(port: engine.port, token: engine.token, pid: enginePid)
    }
    /// engine.json naming an engine whose port nobody answers on.
    private var deadPort: UInt16 = 0
    private var processes: ScriptedProcesses!
    private var session: EngineSession!
    private var supervisor: EngineSupervisor!
    private var alarms: [EngineSupervisor.Alarm] = []
    private var subs: [AnyCancellable] = []
    private var clock = 0.0

    override func setUp() async throws {
        engine = try await StubEngine.start()
        deadPort = try StubEngine.closedPort()
        processes = ScriptedProcesses()
        let processes = processes!
        session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { processes.readInfo() }, scheduler: scheduler,
            handshakeScheduler: handshakeScheduler)
        supervisor = EngineSupervisor(
            processes: processes, session: session, scheduler: ImmediateScheduler(), now: { [unowned self] in clock })
        subs.append(supervisor.alarms.sink { [unowned self] in alarms.append($0) })
    }

    override func tearDown() async throws {
        session.stop()
        engine.stop()
        subs = []
    }

    /// The engine as a process: up when spawned, writing its file.
    private func spawnsTheFake() {
        processes.onSpawn = { [unowned self] in
            processes.alive.insert(enginePid)
            processes.info = .success(engineInfo)
            return enginePid
        }
    }

    /// Fires the reconnect backoff until `condition` holds (the stub answers on its socket).
    private func pump(_ condition: @MainActor () -> Bool) async -> Bool {
        await eventually {
            if condition() { return true }
            await self.scheduler.fire()
            return condition()
        }
    }

    /// Fires the backoff `times` times, each attempt run until it waits on a scheduler again (or,
    /// when nothing retries any more, for at most a second: a bound, not a timing relied on).
    private func spin(_ times: Int, also other: ManualScheduler? = nil) async {
        for _ in 0..<times {
            await scheduler.fire()
            await other?.fire()
            _ = await eventually(timeout: 1) {
                let waits = await self.scheduler.waitingCount
                let others = await other?.waitingCount ?? 0
                return waits + others > 0
            }
        }
    }

    func testNoEngineFileStartsAnEngine() async throws {
        spawnsTheFake()
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertFalse(supervisor.recoveryPending, "a clean start offers nothing")
    }

    func testStaleEngineFileStartsAnEngineAndOffersRecovery() async throws {
        processes.info = .success(EngineConnectionInfo(port: 9, token: "old", pid: 4242))  // pid is gone
        spawnsTheFake()
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertTrue(processes.signals.isEmpty, "a dead pid is never signalled")
        XCTAssertTrue(supervisor.recoveryPending, "the last engine did not exit cleanly")
    }

    func testUnreadableEngineFileStartsAnEngine() async throws {
        processes.info = .failure(.unreadable("garbage"))
        spawnsTheFake()
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 1)
    }

    func testLiveEngineThatAnswersIsAttachedWithoutASpawn() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 0)
        XCTAssertTrue(processes.signals.isEmpty)
    }

    func testLiveSilentEngineAtLaunchIsTerminatedThenReplaced() async throws {
        let silent: Int32 = 777
        // The engine named in the file does not answer.
        processes.info = .success(EngineConnectionInfo(port: deadPort, token: engine.token, pid: silent))
        processes.alive = [silent]
        processes.onSpawn = { [unowned self] in
            processes.alive.insert(enginePid)
            processes.info = .success(engineInfo)
            return enginePid
        }
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.signals.map(\.pid), [silent])
        XCTAssertEqual(processes.signals.map(\.signal), [.terminate])
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertTrue(alarms.isEmpty, "nothing was bound: no alarm")
    }

    /// An engine that accepts the connection but is slow to send its welcome (its control loop
    /// is busy while the audio thread plays) is never ended for it (#268).
    func testALaunchHandshakeTimeoutIsBusyNotSilence() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        engine.welcomeFault = .silent
        let presenter = RecordingPresenter()
        let alerts = ScriptedAlerts()
        let controller = EngineAlarmController(
            session: session, supervisor: supervisor, presenter: presenter, alerts: { alerts })
        supervisor.start()
        var timeouts = 0
        for _ in 0..<(supervisor.probeAttempts * 3) {
            let waiting = await eventually {
                await self.scheduler.fire()  // the reconnect backoff
                return await self.handshakeScheduler.waitingCount > 0
            }
            XCTAssertTrue(waiting)
            await handshakeScheduler.fire()
            timeouts += 1
        }
        XCTAssertGreaterThan(timeouts, supervisor.probeAttempts)
        XCTAssertTrue(processes.signals.isEmpty, "a slow welcome never ends the engine")
        XCTAssertEqual(processes.spawns, 0)
        XCTAssertEqual(alarms, [.notResponding], "the operator is told once, the engine is left alone")
        XCTAssertTrue(presenter.isVisible, "the alarm is up")
        XCTAssertEqual(presenter.shown.last?.title, loc("engine.alarm.notResponding"))
        // It answers in the end: attached, nothing was lost, and the alarm (which says to quit the
        // engine) goes away.
        engine.welcomeFault = .none
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        await assertEventually { !presenter.isVisible }
        _ = controller
    }

    /// An engine whose `engine.json` says its shutdown has begun is not attached to and never
    /// signalled: the app waits for it to exit, then starts the next engine. No alarm.
    func testAStoppingEngineIsWaitedForThenReplacedWithoutASignalOrAlarm() async throws {
        let stopping: Int32 = 779
        processes.info = .success(
            EngineConnectionInfo(port: engine.port, token: engine.token, pid: stopping, stopping: true))
        processes.alive = [stopping]
        spawnsTheFake()
        let waits = ManualScheduler()
        let supervisor = EngineSupervisor(processes: processes, session: session, scheduler: waits, now: { 0 })
        var alarms: [EngineSupervisor.Alarm] = []
        subs.append(supervisor.alarms.sink { alarms.append($0) })
        supervisor.start()
        await spin(10, also: waits)
        XCTAssertEqual(engine.connectCount, 0, "a stopping engine is not attached to")
        XCTAssertEqual(processes.spawns, 0, "its lock would refuse a second engine")
        // It exits and removes its file.
        processes.alive = []
        processes.info = .failure(.missing)
        await waits.fire()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertTrue(processes.signals.isEmpty)
        XCTAssertTrue(alarms.isEmpty)
        XCTAssertTrue(supervisor.recoveryPending, "its shutdown autosaved whatever was unsaved")
    }

    /// One still stopping past its own exit deadline is reported, still never signalled, and
    /// replaced once it has gone; the alarm goes away when the new engine answers.
    func testAnEngineStillStoppingPastItsDeadlineIsAlarmedButNeverSignalled() async throws {
        let stopping: Int32 = 780
        processes.info = .success(
            EngineConnectionInfo(port: engine.port, token: engine.token, pid: stopping, stopping: true))
        processes.alive = [stopping]
        spawnsTheFake()
        let presenter = RecordingPresenter()
        let alerts = ScriptedAlerts()
        let controller = EngineAlarmController(
            session: session, supervisor: supervisor, presenter: presenter, alerts: { alerts })
        supervisor.start()
        let alarmed = await pump { self.alarms == [.stillStopping] }
        XCTAssertTrue(alarmed)
        XCTAssertEqual(presenter.shown.last?.title, loc("engine.alarm.stillStopping"))
        XCTAssertTrue(processes.signals.isEmpty)
        XCTAssertEqual(processes.spawns, 0)
        // It is forced out at its deadline, which removes its file too: its final writes may not
        // have finished, so the autosaves are still offered (#388).
        processes.alive = []
        processes.info = .failure(.missing)
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertTrue(processes.signals.isEmpty)
        await assertEventually { !presenter.isVisible }
        await assertEventually { self.engine.requestCount { $0 == .scanRecovery } == 1 }
        _ = controller
    }

    /// The engine this app was connected to began its shutdown and left its file (killed, or its
    /// removal blocked on a hung disk): it stopped, it did not crash.
    func testAnEngineThatLeftAStoppingFileStoppedRatherThanCrashed() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        var left = engineInfo
        left.stopping = true
        processes.info = .success(left)
        processes.alive = []
        engine.dropConnections()
        _ = await pump { !self.alarms.isEmpty }
        XCTAssertEqual(alarms, [.engineStopped(crashed: false)])
        XCTAssertTrue(supervisor.recoveryPending)
    }

    func testASilentEngineThatIgnoresSigtermGetsSigkill() async throws {
        let silent: Int32 = 778
        processes.info = .success(EngineConnectionInfo(port: deadPort, token: "t", pid: silent))
        processes.alive = [silent]
        processes.diesOn = ["kill"]
        supervisor.start()
        _ = await pump { self.processes.spawns == 1 }
        XCTAssertEqual(processes.signals.map(\.signal), [.terminate, .kill])
        XCTAssertEqual(processes.spawns, 1)
    }

    func testAnEngineThatAnswersWithAnErrorAtLaunchIsNotEnded() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        engine.refuseAuth = .internal  // it answers, with a refusal
        supervisor.start()
        await spin(10)
        XCTAssertTrue(processes.signals.isEmpty, "only silence ends an engine")
        XCTAssertEqual(processes.spawns, 0)
    }

    func testAnEngineThisRunTalkedToIsNeverSignalled() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        // Alive but silent from now on (a hung engine): retried, never ended, no alarm.
        processes.info = .success(EngineConnectionInfo(port: deadPort, token: engine.token, pid: enginePid))
        engine.dropConnections()
        await spin(20)
        XCTAssertNotEqual(session.connectionState, .connected)
        XCTAssertTrue(processes.signals.isEmpty)
        XCTAssertEqual(processes.spawns, 0)
        XCTAssertTrue(alarms.isEmpty)
    }

    func testAGracefulExitNotAskedForIsAnAlarmButNotACrash() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        // Someone else stopped the engine: it removed its file.
        processes.alive = []
        processes.info = .failure(.missing)
        engine.dropConnections()
        _ = await pump { !self.alarms.isEmpty }
        XCTAssertEqual(alarms, [.engineStopped(crashed: false)])
        XCTAssertEqual(processes.spawns, 1, "a new engine is started for the open windows")
    }

    func testACrashLoopIsCappedAndAlarmed() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        processes.alive = []  // crashed; every new engine dies at once (onSpawn: nothing comes up)
        processes.info = .success(EngineConnectionInfo(port: deadPort, token: engine.token, pid: enginePid))
        engine.dropConnections()
        _ = await pump { self.alarms.count == 2 }
        XCTAssertEqual(alarms, [.engineStopped(crashed: true), .cannotStart(loc("engine.alarm.crashLoop"))])
        XCTAssertEqual(processes.spawns, 3)
        // A minute later it tries again.
        clock += 61
        _ = await pump { self.processes.spawns == 4 }
        XCTAssertEqual(processes.spawns, 4)
    }

    func testAMissingEngineProgramIsAnAlarm() async throws {
        processes.spawnError = .missingExecutable("/x/openlab-engine")
        supervisor.start()
        XCTAssertEqual(alarms, [.cannotStart(String(format: loc("engine.alarm.missing"), "/x/openlab-engine"))])
    }

    /// A quit while the engine this app just started is still coming up ends it: no app would
    /// ever connect, so it would run on in the background holding the audio device.
    func testQuitBeforeAStartedEngineIsReachedEndsIt() async throws {
        processes.onSpawn = { [unowned self] in
            processes.alive.insert(enginePid)
            return enginePid  // up, but engine.json not written yet
        }
        supervisor.start()
        _ = await pump { self.supervisor.engineNotReached != nil }
        let app = AppController(session: session, supervisor: supervisor, scheduler: ManualScheduler())
        let alerts = ScriptedAlerts()
        app.alerts = alerts
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertTrue(alerts.log.isEmpty, "nothing plays on it: no prompt")
        XCTAssertEqual(processes.signals.map(\.pid), [enginePid])
        XCTAssertEqual(processes.signals.map(\.signal), [.terminate])
        await spin(5)
        XCTAssertEqual(processes.spawns, 1, "no engine is started after the quit")
    }

    /// The quit's other side: an engine this run reached gets `goodbye`, never a signal.
    func testQuitNeverSignalsAnEngineThisRunReached() async throws {
        engine.appWorkspaces = [StubEngine.workspaceEntry(doc: 1, dirty: true)]
        spawnsTheFake()
        supervisor.start()
        _ = await pump { self.session.stores["1"]?.document?.dirty == true }
        let app = AppController(session: session, supervisor: supervisor, scheduler: ManualScheduler())
        let alerts = ScriptedAlerts()
        alerts.saveChoices = [.dontSave]  // the stub's workspace has unsaved changes
        app.alerts = alerts
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertEqual(alerts.log.filter { $0.hasPrefix("unsaved") }.count, 1, "asked about the unsaved show")
        await assertEventually { self.engine.received.contains { $0 == .goodbye(shutdown: true) } }
        XCTAssertTrue(processes.signals.isEmpty)
    }

    func testAShutdownIsNotACrash() async throws {
        processes.info = .success(engineInfo)
        processes.alive = [enginePid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        await session.shutdown()
        await assertEventually { self.engine.received.contains { $0 == .goodbye(shutdown: true) } }
        processes.alive = []
        processes.info = .failure(.missing)
        await spin(5)
        XCTAssertTrue(alarms.isEmpty)
        XCTAssertEqual(processes.spawns, 0)
    }
}

/// The whole path through the app on the real engine: the supervisor starts the bundled engine
/// the way the app does (`SystemEngineProcesses`, here on the stepped virtual device in a folder
/// of the test's own), the engine is killed (SIGKILL, as a crash ends it), and the alarm, the new
/// engine, the reattached windows and the recovery offers follow.
@MainActor
final class EngineCrashFlowTests: XCTestCase {
    private let scheduler = ManualScheduler()
    /// The app's connections (`holdResults`: the engine's answers wait until released).
    private let tap = WireTap()
    private var runs: EngineRuns!
    private var processes: SystemEngineProcesses!
    private var app: AppController!
    private var alarm: EngineAlarmController!
    private var presenter: RecordingPresenter!
    private var alerts: ScriptedAlerts!
    private var supervisor: EngineSupervisor!
    private var spawned: [Int32] = []
    private let intro = TestSnapshots.intro

    override func setUp() async throws {
        runs = try EngineRuns()
    }

    /// The app with Hamlet (the demo show, a cue playing and an unsaved edit) and A (an empty show)
    /// open. `before`: what happens in the folder before the app starts its engine.
    private func launch() async throws {
        let support = runs.support
        processes = SystemEngineProcesses(
            infoURL: runs.infoURL, watchStderr: false,
            arguments: TestEngineOptions(clock: .stepped, appSupport: support).engineArguments, watchStdin: true)
        let processes = processes!
        let session = EngineSession(transport: tap, infoProvider: { processes.readInfo() }, scheduler: scheduler)
        supervisor = EngineSupervisor(processes: processes, session: session, scheduler: ImmediateScheduler())
        app = AppController(session: session, scheduler: ManualScheduler())
        alerts = ScriptedAlerts()
        app.alerts = alerts
        app.recentURLs = { [] }
        presenter = RecordingPresenter()
        alarm = EngineAlarmController(
            session: session, supervisor: supervisor, presenter: presenter, alerts: { [unowned self] in alerts })
        supervisor.start()
        // engine.json appears once the engine is up: the reconnect backoff is fired until then.
        let up = await pump { session.connectionState == .connected }
        XCTAssertTrue(up, "the supervisor started the engine and the app reached it")
    }

    private func openShows() async throws {
        let session = app.session
        _ = await session.open(path: try runs.show("Demo", named: "Hamlet"))
        _ = await session.open(path: try runs.show("Second", named: "A"))
        await assertEventually {
            self.app.allWindows.count == 2 && session.stores.values.allSatisfy { $0.model != nil }
                && session.stores.values.allSatisfy { $0.connectionState == .connected }
        }
        let hamlet = try XCTUnwrap(hamletStore)
        _ = await hamlet.go(list: TestSnapshots.mainList)
        try await advance(0.1)
        await assertEventually { hamlet.cueStates[self.intro]?.state == .running }
        _ = await hamlet.send(command: .setProperty(intro, .name("Unsaved")))
        await assertEventually { hamlet.document?.dirty == true }
    }

    private var hamletStore: WorkspaceStore? {
        app.session.orderedStores.first { $0.store.model?.name == "Hamlet" }?.store
    }

    private func advance(_ seconds: Double) async throws {
        let frames = UInt64((seconds * TestEngine.rate).rounded())
        guard case .success = await app.session.appClient.send({ .request(id: $0, .advance(frames: frames)) }) else {
            throw TestEngineError.refused("advance")
        }
    }

    /// Kills the running engine (SIGKILL): its engine.json and autosaves stay behind.
    private func crash() async throws {
        guard case .success(let info) = processes.readInfo() else { return XCTFail("no engine.json") }
        spawned.append(info.pid)
        processes.signal(info.pid, .kill)
        await assertEventually { !self.processes.isEngine(info.pid) }
    }

    override func tearDown() async throws {
        tap.holdResults = false
        tap.releaseResults()
        app?.closeAllWindows()
        app?.session.stop()
        if let processes, case .success(let info) = processes.readInfo(), processes.isEngine(info.pid) {
            processes.signal(info.pid, .kill)
            _ = await eventually { !processes.isEngine(info.pid) }
        }
        runs?.endAll()
    }

    private func pump(_ condition: @MainActor () -> Bool) async -> Bool {
        await eventually {
            if condition() { return true }
            await self.scheduler.fire()
            return condition()
        }
    }

    func testEngineCrashAlarmsStartsANewEngineReattachesAndOffersRecovery() async throws {
        try await launch()
        try await openShows()
        let windows = app.allWindows.map(ObjectIdentifier.init)
        guard case .success(let oldInfo) = processes.readInfo() else { return XCTFail() }
        try await crash()

        let back = await pump { self.presenter.detail.contains(loc("engine.alarm.back")) }
        XCTAssertTrue(back, "alarm, new engine and reattach")
        XCTAssertEqual(presenter.shown.first?.title, loc("engine.alarm.crashed"))
        guard case .success(let newInfo) = processes.readInfo() else { return XCTFail() }
        XCTAssertNotEqual(newInfo.pid, oldInfo.pid, "a new engine was started")
        XCTAssertNotEqual(newInfo.token, oldInfo.token)
        // The same windows, bound to the reopened shows; none made twice.
        XCTAssertEqual(app.allWindows.map(ObjectIdentifier.init), windows)
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertEqual(Set(app.session.stores.values.compactMap { $0.model?.name }), ["Hamlet", "A"])
        XCTAssertTrue(
            presenter.detail.contains(String(format: loc("engine.alarm.editsLostOne"), "Hamlet")),
            "the dirty show had no newer autosave: its lost edits are listed")
        await assertEventually("Hamlet is its file again") { self.hamletStore?.model?.cue(self.intro)?.name == "Intro" }
        XCTAssertTrue(presenter.isVisible)
        // Nothing modal ran by itself.
        XCTAssertTrue(alerts.log.isEmpty)
    }

    /// An unclean exit before this launch (the last engine was killed with an untitled workspace
    /// autosaved): the new engine's autosave is offered by the alarm, not by a modal, and restored
    /// from it on the operator's word.
    func testLaunchAfterAnUncleanExitOffersTheAutosavesWithoutAModal() async throws {
        try await runs.start()
        guard case .success(let made) = await runs.app.send({ .request(id: $0, .new) }),
            case .opened(let doc)? = made.data
        else { return XCTFail("new") }
        let url = runs.infoURL
        let bound = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) }, workspace: doc)
        bound.start()
        await assertEventually { bound.state == .connected }
        _ = await bound.send({ .command(id: $0, .renameWorkspace("Lost")) })
        try await runs.advance(31)  // past the 30 s autosave interval
        bound.stop()
        try await runs.crash()

        try await launch()
        XCTAssertTrue(app.session.stores.isEmpty)
        let offered = await pump { self.presenter.restore != nil }
        XCTAssertTrue(offered)
        XCTAssertEqual(presenter.shown.first?.title, loc("engine.alarm.unclean"))
        XCTAssertTrue(alerts.log.isEmpty, "no modal by itself")
        XCTAssertFalse(supervisor.recoveryPending, "offered once")
        presenter.restore?()
        await assertEventually { self.alerts.log == ["autosave:Lost"] }
        await assertEventually { self.app.session.stores.values.contains { $0.model?.name == "Lost" } }
    }

    /// An autosave the operator dismissed with the alarm (an untitled workspace's that no window
    /// holds) is still offered from File > Recover Autosaves… later.
    func testADismissedAutosaveCanStillBeRecoveredFromTheMenu() async throws {
        try await runs.start()
        guard case .success(let made) = await runs.app.send({ .request(id: $0, .new) }),
            case .opened(let doc)? = made.data
        else { return XCTFail("new") }
        let url = runs.infoURL
        let bound = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) }, workspace: doc)
        bound.start()
        await assertEventually { bound.state == .connected }
        _ = await bound.send({ .command(id: $0, .renameWorkspace("Untitled one")) })
        try await runs.advance(31)
        bound.stop()
        try await runs.crash()

        try await launch()
        let offered = await pump { self.presenter.restore != nil }
        XCTAssertTrue(offered)
        presenter.dismiss()
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log, ["autosave:Untitled one"])
        await assertEventually { self.app.allWindows.count == 1 }
        // Now a window holds it: nothing more to recover.
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log.last, "info:" + loc("recover.none"))
    }

    /// The alarm controller is built on `app.gatedAlerts`, as the app delegate does: in Show Mode
    /// "nothing to recover" is a log record, not an alert (NFR-10).
    func testRecoverNoticesAreQuietInShowMode() async throws {
        try await launch()
        try await openShows()
        let quiet = EngineAlarmController(
            session: app.session, supervisor: supervisor, presenter: presenter,
            alerts: { [unowned self] in app.gatedAlerts })
        let wc = try XCTUnwrap(app.allWindows.first)
        wc.setShowMode(true)
        await quiet.recoverAutosaves()
        XCTAssertTrue(alerts.log.isEmpty, "\(alerts.log)")
        XCTAssertEqual(wc.store.logEntries.last?.message, loc("recover.none"))
    }

    func testRecoverAutosavesIsInTheFileMenuAndWaitsForTheEngine() throws {
        let file = MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == loc("menu.file") }
        let item = try XCTUnwrap(file?.items.first { $0.action == #selector(AppDelegate.recoverAutosaves(_:)) })
        let delegate = AppDelegate()  // its engine is never started here
        XCTAssertFalse(delegate.validateMenuItem(item), "nothing to ask while no engine is connected")
    }

    /// SM-07 after a crash: the new engine run opened the show afresh (not in Show Mode); the
    /// window still is, so the engine is told again.
    func testShowModeIsRestoredAfterTheEngineRestarts() async throws {
        try await launch()
        try await openShows()
        let wc = try XCTUnwrap(app.allWindows.first { $0.store === hamletStore })
        wc.setShowMode(true)
        await assertEventually { wc.store.document?.showMode == true }
        try await crash()
        // The old run's state holds until the drop is seen: wait for the new run's reattach.
        let back = await pump {
            self.presenter.detail.contains(loc("engine.alarm.back"))
                && self.app.windows.values.contains { $0 === wc } && wc.store.connectionState == .connected
                && wc.store.document?.showMode == true
        }
        XCTAssertTrue(back, "reattached and Show Mode sent again")
        XCTAssertTrue(wc.isShowMode)
        // The new engine says so itself: a fresh connection to the doc sees Show Mode.
        let doc = try XCTUnwrap(app.session.doc(of: wc.store))
        let url = runs.infoURL
        let check = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) }, workspace: doc)
        let mirror = WorkspaceStore(client: check)
        check.start()
        await assertEventually { mirror.document?.showMode == true }
        check.stop()
    }

    /// The crashed run's cues stopped with it: a window waiting to be reopened shows none playing.
    func testAWindowWaitingForItsShowShowsNoCuePlaying() async throws {
        try await launch()
        try await openShows()
        let store = try XCTUnwrap(hamletStore)
        XCTAssertTrue(store.cueStates.values.contains { $0.state.isActive }, "Intro is playing")
        try await crash()
        let alarmed = await pump { !self.presenter.shown.isEmpty }
        XCTAssertTrue(alarmed)
        XCTAssertFalse(store.cueStates.values.contains { $0.state.isActive })
        XCTAssertTrue(store.progress.isEmpty)
        await assertEventually { !self.app.session.isReattaching && store.connectionState == .connected }
        XCTAssertFalse(store.cueStates.values.contains { $0.state.isActive }, "nothing plays in the new run")
    }

    /// Runs the new engine until it has answered the first reopen (the answer held by `tap`): moved
    /// on over a connection of its own, since the app's connections' answers are held.
    private func reopenAnswered() async throws {
        let url = runs.infoURL
        let direct = EngineClient(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) }, workspace: nil)
        direct.start()
        defer { direct.stop() }
        let ok = await eventually {
            guard direct.state == .connected else { return false }
            _ = await direct.send({ .request(id: $0, .advance(frames: 480)) })
            return self.tap.heldCount >= 1
        }
        XCTAssertTrue(ok, "the new engine answered the first reopen")
    }

    /// File > New during a reattach: no waiting window can claim a new workspace, so it gets its
    /// window at once instead of when every detached window is rebound (#262).
    func testANewWorkspaceDuringAReattachGetsItsWindowAtOnce() async throws {
        try await launch()
        try await openShows()
        tap.holdResults = true  // the new engine's answers to the reopens are held
        try await crash()
        let waiting = await pump { self.app.session.detached.count == 2 && self.app.session.isReattaching }
        XCTAssertTrue(waiting)
        try await reopenAnswered()
        tap.holdResults = false
        await app.newWorkspace()
        XCTAssertEqual(app.session.orderedStores.count, 1, "the new workspace has its store while the others wait")
        XCTAssertTrue(app.session.isReattaching)
        XCTAssertEqual(app.allWindows.count, 3)
        tap.releaseResults()
        let done = await pump { self.app.windows.count == 3 && !self.app.session.isReattaching }
        XCTAssertTrue(done)
    }

    /// A window whose show is not reopened yet has no doc to close: Close beeps and it stays.
    func testAWindowWaitingForItsShowCannotBeClosed() async throws {
        try await launch()
        try await openShows()
        let store = try XCTUnwrap(hamletStore)
        var beeps = 0
        app.beep = { beeps += 1 }
        tap.holdResults = true  // the new engine's answers to the reopens are held
        try await crash()
        let waiting = await pump { self.app.session.detached.count == 2 && self.app.session.isReattaching }
        XCTAssertTrue(waiting)
        XCTAssertNil(app.session.doc(of: store))
        let closed = await app.closeWorkspace(store)
        XCTAssertFalse(closed, "it has no doc to close yet")
        XCTAssertEqual(beeps, 1)
        XCTAssertEqual(app.allWindows.count, 2, "the windows stay while they wait")
        tap.holdResults = false
        tap.releaseResults()
        let done = await pump { self.app.windows.count == 2 && !self.app.session.isReattaching }
        XCTAssertTrue(done)
    }

    func testTransportStillWorksWhileTheAlarmIsUp() async throws {
        try await launch()
        try await openShows()
        try await crash()
        let back = await pump { self.presenter.detail.contains(loc("engine.alarm.back")) }
        XCTAssertTrue(back)
        await assertEventually { self.app.session.stores.values.allSatisfy { $0.connectionState == .connected } }
        XCTAssertTrue(presenter.isVisible)
        let store = try XCTUnwrap(hamletStore)
        guard case .success = await store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await advance(0.1)
        await assertEventually { store.cueStates[self.intro]?.state == .running }
        if case .failure(let e) = await app.panicAll() { XCTFail("\(e)") }
        try await advance(1)  // past the 0.5 s panic fade
        await assertEventually { store.cueStates[self.intro]?.state != .running }
    }

    /// A show whose autosave is newer than its file (left by a killed engine) asks first; Cancel
    /// opens nothing, Restore opens the autosave.
    func testOpeningAShowWithANewerAutosaveAsksFirst() async throws {
        try await runs.start()
        let b = try runs.show("Demo", named: "B")
        let c = try runs.show("Demo", named: "C")
        for path in [b, c] {
            let doc = try await runs.open(path)
            try await runs.rename(intro, in: doc, to: "Autosaved")
        }
        try await runs.advance(31)
        try await runs.crash()

        try await launch()
        alerts.recoveryChoice = nil  // Cancel: nothing opens, no error
        await app.openWorkspace(url: URL(fileURLWithPath: b))
        XCTAssertEqual(alerts.log.filter { $0.hasPrefix("recoveryOffer") }, ["recoveryOffer:B"])
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(app.windows.count, 0)
        alerts.recoveryChoice = .restore
        await app.openWorkspace(url: URL(fileURLWithPath: c))
        await assertEventually { self.app.windows.count == 1 }
        let store = try XCTUnwrap(app.session.orderedStores.first?.store)
        await assertEventually { store.model?.cue(self.intro)?.name == "Autosaved" }
        XCTAssertEqual(store.document?.dirty, true, "the autosave was opened")
    }

    /// #264: the engine dies while Hamlet has unsaved edits whose autosave is not newer than the
    /// file (the file changed since): the reopen sets it aside and the window comes back from the
    /// file. That autosave may be the only copy of the edits: the alarm offers it although Hamlet
    /// is open, labelled as set aside; once restored it is not offered again.
    func testASetAsideAutosaveOfAReopenedWorkspaceIsOffered() async throws {
        try await launch()
        try await openShows()
        let path = try XCTUnwrap(app.session.workspaces.first { $0.name == "Hamlet" }?.path)
        try await advance(31)  // Hamlet's edit is autosaved
        try await crash()
        try FileManager.default.setAttributes(
            [.modificationDate: Date().addingTimeInterval(3600)], ofItemAtPath: path)
        let offered = await pump { self.presenter.restore != nil }
        XCTAssertTrue(offered)
        XCTAssertTrue(presenter.detail.contains(String(format: loc("engine.alarm.editsLostSetAside"), "Hamlet")))
        XCTAssertFalse(presenter.detail.contains(String(format: loc("engine.alarm.editsLostOne"), "Hamlet")))
        XCTAssertEqual(hamletStore?.model?.cue(intro)?.name, "Intro", "the window came back from the file")
        presenter.restore?()
        await assertEventually { self.app.allWindows.count == 3 }
        XCTAssertEqual(alerts.log, ["setAsideAutosave:Hamlet"])
        await assertEventually {
            self.app.session.stores.values.contains { $0.model?.cue(self.intro)?.name == "Unsaved" }
        }
        // The window it opened in holds it now: nothing more to recover.
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log.last, "info:" + loc("recover.none"))
    }
}

/// Which autosaves are offered (#332), on the scan data alone.
final class AutosaveOfferTests: XCTestCase {
    private let w = ModelID("00000000-0000-0000-0000-0000000000a1")
    private let other = ModelID("00000000-0000-0000-0000-0000000000b2")

    private func entry(_ id: ModelID, ms: Int64?, stale: Bool, hash: String? = "00000000000000aa")
        -> RecoveryEntryInfo
    {
        RecoveryEntryInfo(workspaceId: id, pathHash: hash, showName: nil, modifiedMs: ms, stale: stale)
    }

    private func holder(_ id: ModelID, _ hash: String? = "00000000000000aa") -> AutosaveOffer.Holder {
        AutosaveOffer.Holder(workspace: id, pathHash: hash)
    }

    func testACurrentAutosaveIsOfferedOnlyWhenNoWindowHoldsItsWorkspace() {
        let mine = entry(w, ms: 5, stale: false)
        let lost = entry(other, ms: 5, stale: false)
        let window = AutosaveOffer.Window(workspace: w, cover: AutosaveCover())
        XCTAssertEqual(AutosaveOffer.offerable([mine, lost], held: [holder(w)], windows: [window]), [lost])
    }

    /// #359: a copy of the open show (same workspace id, other path) left a current autosave; it
    /// is nobody's, so it is offered, and the open copy's own is not.
    func testAnotherCopysCurrentAutosaveIsOfferedWhileItsWorkspaceIsOpen() {
        let mine = entry(w, ms: 5, stale: false)
        let copy = entry(w, ms: 6, stale: false, hash: "00000000000000bb")
        let window = AutosaveOffer.Window(workspace: w, cover: AutosaveCover())
        XCTAssertEqual(AutosaveOffer.offerable([mine, copy], held: [holder(w)], windows: [window]), [copy])
    }

    /// Without a hash on either side (an untitled window, an engine that does not send it, an
    /// untitled workspace's autosave) the workspace id alone decides, as before.
    func testWithoutAHashOnEitherSideTheWorkspaceIdDecides() {
        let copy = entry(w, ms: 6, stale: false, hash: "00000000000000bb")
        let untitled = entry(w, ms: 7, stale: false, hash: nil)
        let window = AutosaveOffer.Window(workspace: w, cover: AutosaveCover())
        XCTAssertEqual(AutosaveOffer.offerable([copy, untitled], held: [holder(w, nil)], windows: [window]), [])
        XCTAssertEqual(AutosaveOffer.offerable([untitled], held: [holder(w)], windows: [window]), [])
        XCTAssertEqual(AutosaveOffer.offerable([untitled], held: [holder(other, nil)], windows: []), [untitled])
    }

    /// #385: a window known to be untitled (restored from a copy's autosave, it kept that
    /// workspace id) holds only its own hashless autosave, not the saved copies'.
    func testAnUntitledWindowHoldsOnlyTheHashlessAutosave() {
        let copy = entry(w, ms: 6, stale: false, hash: "00000000000000bb")
        let own = entry(w, ms: 7, stale: false, hash: nil)
        let held: Set = [AutosaveOffer.Holder(workspace: w, pathHash: nil, untitled: true)]
        let window = AutosaveOffer.Window(workspace: w, cover: AutosaveCover())
        XCTAssertEqual(AutosaveOffer.offerable([copy, own], held: held, windows: [window]), [copy])
    }

    /// Another copy's autosave restored into a window (which got a fresh workspace id) is that
    /// window's: not offered again.
    func testACurrentAutosaveLoadedIntoAWindowIsNotOfferedAgain() {
        let copy = entry(w, ms: 6, stale: false, hash: "00000000000000bb")
        var cover = AutosaveCover()
        cover.load(copy)
        let windows = [
            AutosaveOffer.Window(workspace: w, cover: AutosaveCover()),
            AutosaveOffer.Window(workspace: other, cover: cover),
        ]
        XCTAssertEqual(
            AutosaveOffer.offerable([copy], held: [holder(w), holder(other, nil)], windows: windows), [])
    }

    /// A restore by `open` covers the exact offer time of the window's own workspace only.
    func testASetAsideAutosaveOfAnOpenWorkspaceIsOfferedUnlessThatAutosaveWasRestored() {
        let stale = entry(w, ms: 1_000, stale: true)
        var cover = AutosaveCover()
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], held: [holder(w)], windows: [.init(workspace: w, cover: cover)]), [stale],
            "opened from its file: the file's time says nothing reliable")
        cover.loadOwn(modifiedMs: 2_000)
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], held: [holder(w)], windows: [.init(workspace: w, cover: cover)]), [stale],
            "a later autosave was restored: it need not contain this one")
        cover.loadOwn(modifiedMs: 1_000)
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], held: [holder(w)], windows: [.init(workspace: w, cover: cover)]), [])
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], held: [holder(w)], windows: [.init(workspace: other, cover: cover)]),
            [stale],
            "another workspace's window covers nothing of this one")
        XCTAssertEqual(
            AutosaveOffer.offerable(
                [entry(w, ms: nil, stale: true)], held: [holder(w)], windows: [.init(workspace: w, cover: cover)]),
            [entry(w, ms: nil, stale: true)], "no time: never matched")
    }

    /// `open_recovery` covers that entry exactly (workspace id, path hash, time), whatever the
    /// window's own workspace id.
    func testAWindowOpenedFromAnAutosaveCoversExactlyThatEntry() {
        var cover = AutosaveCover()
        cover.load(entry(w, ms: 1_000, stale: true))
        let windows = [AutosaveOffer.Window(workspace: other, cover: cover)]
        XCTAssertEqual(
            AutosaveOffer.offerable([entry(w, ms: 1_000, stale: true)], held: [holder(w)], windows: windows), [])
        for different in [
            entry(w, ms: 999, stale: true), entry(w, ms: 1_001, stale: true),
            entry(w, ms: 1_000, stale: true, hash: "00000000000000bb"),
        ] {
            XCTAssertEqual(AutosaveOffer.offerable([different], held: [holder(w)], windows: windows), [different])
        }
    }
}

final class SystemEngineProcessesTests: XCTestCase {
    @MainActor
    func testOnlyALiveOpenLabEngineIsAnEngine() {
        let p = SystemEngineProcesses(executable: nil, bundleID: "x", infoURL: URL(fileURLWithPath: "/nonexistent"))
        XCTAssertFalse(p.isEngine(0), "a process group, never a pid")
        XCTAssertFalse(p.isEngine(-1), "every process, never a pid")
        XCTAssertFalse(p.isEngine(1), "launchd")
        XCTAssertFalse(p.isEngine(getpid()), "a live process that is not the engine (pid reuse)")
        XCTAssertFalse(p.isEngine(Int32.max), "no such process")
        // Signalling something that is not an engine does nothing.
        p.signal(getpid(), .terminate)
    }

    /// A live process whose program is named `openlab-engine` (a copy of sleep) reads as an engine,
    /// and once it has exited it does not.
    @MainActor
    func testALiveProcessNamedOpenLabEngineIsAnEngine() throws {
        let sleep = try Data(contentsOf: URL(fileURLWithPath: "/bin/sleep"))
        let child = Process()
        child.executableURL = try Self.keptProgram(SystemEngineProcesses.engineName, sleep)
        child.arguments = ["30"]
        try child.run()
        defer { if child.isRunning { child.terminate() } }
        let p = SystemEngineProcesses(executable: nil, bundleID: "x", infoURL: URL(fileURLWithPath: "/nonexistent"))
        XCTAssertTrue(p.isEngine(child.processIdentifier))
        child.terminate()
        child.waitUntilExit()
        XCTAssertFalse(p.isEngine(child.processIdentifier), "gone once it exited")
    }

    /// A test program at a fixed path, made once and never removed (#484). The first exec of a
    /// new executable is held by the kernel until Gatekeeper (syspolicyd) has checked it, and
    /// checks run one at a time. A program deleted while it is checked has crashed syspolicyd
    /// (macOS 26.7: `SecStaticCode` reading a vanished Mach-O), and an exec queued behind that
    /// check was then never let run, so a child that should have exited at once hung the test
    /// run. A program kept at one path is checked once, and never vanishes under the check.
    private static func keptProgram(_ name: String, _ contents: Data) throws -> URL {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("openLabTests-programs", isDirectory: true)
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(name)
        if fm.isExecutableFile(atPath: url.path), fm.contents(atPath: url.path) == contents { return url }
        // Renamed into place: a copy another test run is executing keeps its own file.
        let temp = dir.appendingPathComponent(".\(name).\(UUID().uuidString)")
        try contents.write(to: temp)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: temp.path)
        guard rename(temp.path, url.path) == 0 else {
            let code = POSIXErrorCode(rawValue: errno) ?? .EIO
            try? fm.removeItem(at: temp)
            throw POSIXError(code)
        }
        return url
    }

    /// Reaps the child `pid`, waiting at most `timeout` seconds. The blocking `waitpid` runs on its
    /// own thread so a child that never ends fails this test (with its process state) instead of
    /// hanging the whole test run; such a child is killed so the thread is freed. The status, or nil.
    @MainActor @discardableResult
    private func reap(
        _ pid: pid_t, timeout: TimeInterval = 10, _ what: String = "", file: StaticString = #filePath,
        line: UInt = #line
    ) -> Int32? {
        final class Box: @unchecked Sendable {
            var result: pid_t = 0
            var status: Int32 = 0
            var error: Int32 = 0
        }
        guard pid > 0 else {  // 0 or -1 would wait for, and kill, a whole process group
            XCTFail("no child to reap (pid \(pid)) \(what)", file: file, line: line)
            return nil
        }
        let box = Box()
        // A semaphore, not an expectation: it is waited on twice (before and after the kill).
        let reaped = DispatchSemaphore(value: 0)
        Thread.detachNewThread {
            var status: Int32 = 0
            var r: pid_t
            repeat { r = waitpid(pid, &status, 0) } while r < 0 && errno == EINTR
            box.result = r
            box.status = status
            box.error = r < 0 ? errno : 0
            reaped.signal()
        }
        if reaped.wait(timeout: .now() + timeout) == .timedOut {
            let state = Self.processState(pid)
            kill(pid, SIGKILL)
            let after = reaped.wait(timeout: .now() + 10) == .timedOut ? "; not reaped 10 s after SIGKILL" : ""
            XCTFail(
                "child \(pid) \(what) did not exit within \(timeout) s; it was \(state)\(after)", file: file, line: line
            )
            return nil
        }
        guard box.result == pid else {
            XCTFail("waitpid(\(pid)) \(what) returned \(box.result), errno \(box.error)", file: file, line: line)
            return nil
        }
        return box.status
    }

    /// What the kernel says `pid` is doing: its status (run, sleep, stop, zombie), flags and program.
    @MainActor
    private static func processState(_ pid: pid_t) -> String {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return "gone (errno \(errno))" }
        let name = SystemEngineProcesses.executableName(of: pid) ?? "?"
        return "status \(info.pbi_status) flags 0x\(String(info.pbi_flags, radix: 16)) pgid \(info.pbi_pgid) "
            + "ppid \(info.pbi_ppid) program \(name)"
    }

    private func scratchDirectory() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return dir
    }

    /// What a starting engine prints on stderr reaches the app (#263): the spawned program's
    /// stderr is the log file, and the tail reads exactly what was written since the spawn.
    @MainActor
    func testWhatTheEnginePrintsOnStderrIsReadBack() throws {
        let dir = try scratchDirectory()
        let program = try Self.keptProgram(
            "stderr-lines.sh",
            Data("#!/bin/sh\necho 'no audio device'\necho 'second line' >&2\necho 'no audio device' >&2\n".utf8))
        let log = dir.appendingPathComponent("logs/engine-stderr.log")
        let p = SystemEngineProcesses(
            executable: program, bundleID: "x", infoURL: dir.appendingPathComponent("engine.json"), stderrURL: log,
            watchStderr: false)
        XCTAssertEqual(reap(try p.spawnEngine()), 0, "it ran and exited 0")
        XCTAssertEqual(p.stderrTail.read(), ["second line", "no audio device"], "stdout is not read")
        XCTAssertEqual(p.stderrTail.read(), [], "each line once")
    }

    /// A stderr log the app cannot open (read-only, a directory) costs the engine its log, never
    /// its start: the engine is spawned with /dev/null instead. The program is `true` (a system
    /// program, which Gatekeeper never holds: see `keptProgram`), so it runs and exits 0.
    @MainActor
    func testAnUnusableStderrLogDoesNotStopTheEngineStarting() throws {
        let dir = try scratchDirectory()
        let program = URL(fileURLWithPath: "/usr/bin/true")
        let readOnly = dir.appendingPathComponent("readonly.log")
        try Data("old\n".utf8).write(to: readOnly)
        try FileManager.default.setAttributes([.posixPermissions: 0o400], ofItemAtPath: readOnly.path)
        let folder = dir.appendingPathComponent("folder.log")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        for log in [readOnly, folder] {
            let p = SystemEngineProcesses(
                executable: program, bundleID: "x", infoURL: dir.appendingPathComponent("engine.json"),
                stderrURL: log, watchStderr: false)
            XCTAssertEqual(reap(try p.spawnEngine(), log.lastPathComponent), 0, log.lastPathComponent)
        }
    }

    /// A child that never exits fails the wait within its bound, saying what the child was
    /// doing, and is killed and reaped, instead of hanging every later test (#484: a blocking
    /// `waitpid` held the whole mac test run for an hour).
    @MainActor
    func testAChildThatNeverExitsFailsTheWaitInsteadOfHangingTheRun() throws {
        var pid: pid_t = 0
        let argv: [UnsafeMutablePointer<CChar>?] = [strdup("/bin/sleep"), strdup("600"), nil]
        defer { for a in argv { free(a) } }
        XCTAssertEqual(posix_spawn(&pid, "/bin/sleep", nil, nil, argv, nil), 0)
        let options = XCTExpectedFailure.Options()
        // Only the bound's own failure, and only once the killed child was reaped.
        options.issueMatcher = {
            $0.compactDescription.contains("did not exit within") && !$0.compactDescription.contains("not reaped")
        }
        XCTExpectFailure("the child is still sleeping when the wait ends", options: options) {
            XCTAssertNil(reap(pid, timeout: 0.5))
        }
        let alive = kill(pid, 0)
        let error = errno
        XCTAssertEqual(alive, -1, "killed and reaped: no such process")
        XCTAssertEqual(error, ESRCH)
    }

    private final class Lines: @unchecked Sendable {
        private let lock = NSLock()
        private var all: [String] = []
        func add(_ line: String) {
            lock.lock()
            all.append(line)
            lock.unlock()
        }
        var value: [String] {
            lock.lock()
            defer { lock.unlock() }
            return all
        }
    }

    /// The watcher logs lines appended to the file, a respawn's watcher replaces the old one, and
    /// the old one closes its descriptor and says nothing more.
    func testTheStderrWatcherLogsAppendedLinesAndIsReplacedOnRespawn() throws {
        let dir = try scratchDirectory()
        let url = dir.appendingPathComponent("e.log")
        let tail = EngineStderrTail(url: url)
        XCTAssertTrue(tail.prepare())
        let handle = try FileHandle(forWritingTo: url)
        defer { try? handle.close() }

        let first = Lines()
        let seen = expectation(description: "first line")
        let w1 = EngineStderrWatcher(tail: tail) { line in
            first.add(line)
            seen.fulfill()
        }
        let closed = expectation(description: "first watcher's descriptor closed")
        w1.onClosed = { closed.fulfill() }
        w1.start()
        handle.seekToEndOfFile()
        handle.write(Data("boom\n".utf8))
        wait(for: [seen], timeout: 10)
        XCTAssertEqual(first.value, ["boom"])

        let second = Lines()
        let seen2 = expectation(description: "second watcher's line")
        let w2 = EngineStderrWatcher(tail: tail) { line in
            second.add(line)
            seen2.fulfill()
        }
        w1.stop()
        wait(for: [closed], timeout: 10)
        w2.start()
        handle.write(Data("again\n".utf8))
        wait(for: [seen2], timeout: 10)
        w2.stop()
        XCTAssertEqual(second.value, ["again"])
        XCTAssertEqual(first.value, ["boom"], "a stopped watcher logs nothing")
    }

    func testTheStderrTailIsBounded() throws {
        let dir = try scratchDirectory()
        let url = dir.appendingPathComponent("e.log")
        let tail = EngineStderrTail(url: url)
        XCTAssertTrue(tail.prepare())
        let handle = try FileHandle(forWritingTo: url)
        func write(_ text: String) {
            handle.seekToEndOfFile()
            handle.write(Data(text.utf8))
        }

        write("par")
        XCTAssertEqual(tail.read(), [], "a line is complete at its newline")
        write("tial\n" + String(repeating: "x", count: 5000) + "\n")
        let lines = tail.read()
        XCTAssertEqual(lines.first, "partial")
        XCTAssertEqual(lines.last?.count, EngineStderrTail.lineLimit + 3, "a long line is cut")

        // A flood larger than one read keeps the newest part; a budget caps the lines returned.
        write(String(repeating: "noise\n", count: 100_000))
        let flood = tail.read()
        XCTAssertLessThanOrEqual(flood.count, EngineStderrTail.lineBudget + 1)
        XCTAssertEqual(flood.last, "(further engine stderr output is not logged)")
        write("more\n")
        XCTAssertEqual(tail.read(), [], "nothing is logged past the budget")

        // The file emptied under the reader (a new run's prepare) restarts it at the top.
        handle.truncateFile(atOffset: 0)
        let fresh = EngineStderrTail(url: url)
        XCTAssertTrue(fresh.prepare())
        write("again\n")
        XCTAssertEqual(fresh.read(), ["again"])
        try handle.close()
        // Over the limit at the start of a run: emptied, so the file cannot grow without end.
        try Data(count: Int(EngineStderrTail.fileLimit) + 1).write(to: url)
        XCTAssertTrue(fresh.prepare())
        XCTAssertEqual((try FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.intValue, 0)
    }

    @MainActor
    func testAMissingProgramIsAnErrorNotACrash() {
        let missing = URL(fileURLWithPath: "/nonexistent/openlab-engine")
        let p = SystemEngineProcesses(executable: missing, bundleID: "x", infoURL: URL(fileURLWithPath: "/nonexistent"))
        XCTAssertThrowsError(try p.spawnEngine()) { error in
            XCTAssertEqual(error as? EngineSpawnError, .missingExecutable(missing.path))
        }
        guard case .failure(.missing) = p.readInfo() else { return XCTFail() }
    }
}

final class SingleInstanceTests: XCTestCase {
    /// A second copy hands over to the one already running; two started at once agree on one.
    func testOnlyTheFirstLaunchedCopyStays() {
        let early = SingleInstance.Instance(pid: 900, launched: Date(timeIntervalSince1970: 100))
        let late = SingleInstance.Instance(pid: 300, launched: Date(timeIntervalSince1970: 200))
        XCTAssertEqual(SingleInstance.handOver(me: late, others: [early, late]), early)
        XCTAssertNil(SingleInstance.handOver(me: early, others: [early, late]))
        XCTAssertNil(SingleInstance.handOver(me: early, others: [early]), "alone: it runs")
        // Same or unknown launch date: the lower pid stays, as both copies see it.
        let a = SingleInstance.Instance(pid: 10, launched: nil)
        let b = SingleInstance.Instance(pid: 20, launched: Date(timeIntervalSince1970: 1))
        XCTAssertEqual(SingleInstance.handOver(me: b, others: [a, b]), a)
        XCTAssertNil(SingleInstance.handOver(me: a, others: [a, b]))
    }
}

/// The launch path of a second copy, with the system pieces injected (#338, #336).
@MainActor
final class AppLaunchTests: XCTestCase {
    private var log: [String] = []

    private final class FirstCopy: RunningCopy {
        let processIdentifier: pid_t = 4242
        let record: (String) -> Void
        init(record: @escaping (String) -> Void) { self.record = record }
        func activate(options: NSApplication.ActivationOptions) -> Bool {
            record(options.contains(.activateIgnoringOtherApps) ? "activate(ignoringOtherApps)" : "activate")
            return true
        }
        @available(macOS 14, *)
        func activate(from application: NSRunningApplication, options: NSApplication.ActivationOptions) -> Bool {
            record(application == NSRunningApplication.current ? "activate(fromThisCopy)" : "activate(fromOther)")
            return true
        }
    }

    private func launch(first: RunningCopy?, cooperative: Bool) -> AppLaunch {
        AppLaunch(
            runningFirst: { first },
            makeEngine: { [unowned self] in
                log.append("engine")
                // No engine: nothing is listening and nothing is spawned (only what is built counts).
                let processes = ScriptedProcesses()
                let session = EngineSession(
                    transport: WebSocketTransport(), infoProvider: { .failure(.missing) },
                    scheduler: ManualScheduler())
                return (session, EngineSupervisor(processes: processes, session: session))
            },
            installMenu: { [unowned self] _ in log.append("menu") },
            activation: HandOverActivation(cooperative: cooperative) { [unowned self] copy in
                log.append("yield(\(copy.processIdentifier))")
            },
            terminate: { [unowned self] in log.append("terminate") })
    }

    private func runLaunch(_ delegate: AppDelegate) {
        delegate.applicationWillFinishLaunching(Notification(name: NSApplication.willFinishLaunchingNotification))
        delegate.applicationDidFinishLaunching(Notification(name: NSApplication.didFinishLaunchingNotification))
    }

    /// The second copy builds no menu and no engine session, fronts the first and exits; the
    /// delegate hooks that come after never reach the session either.
    func testASecondCopyHandsOverWithoutTouchingTheEngine() {
        let delegate = AppDelegate(
            launch: launch(first: FirstCopy { [unowned self] in log.append($0) }, cooperative: true))
        delegate.applicationWillFinishLaunching(Notification(name: NSApplication.willFinishLaunchingNotification))
        XCTAssertTrue(delegate.isSecondInstance)
        XCTAssertEqual(log, [], "nothing until it is the active app")
        delegate.applicationDidFinishLaunching(Notification(name: NSApplication.didFinishLaunchingNotification))
        XCTAssertEqual(log, ["yield(4242)", "activate(fromThisCopy)", "terminate"])
        XCTAssertFalse(delegate.applicationShouldHandleReopen(NSApp, hasVisibleWindows: false))
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApp), .terminateNow)
        XCTAssertEqual(log, ["yield(4242)", "activate(fromThisCopy)", "terminate"], "no menu, no engine, no session")
    }

    /// macOS 14+ activation is cooperative: the exiting copy yields, then asks for the first's
    /// activation from itself (`activate(from:options:)`, as NSRunningApplication.h says). Before
    /// 14 the first is activated over every other app.
    func testTheHandOverActivationFitsTheSystem() {
        let first = FirstCopy { [unowned self] in log.append($0) }
        runLaunch(AppDelegate(launch: launch(first: first, cooperative: true)))
        XCTAssertEqual(log, ["yield(4242)", "activate(fromThisCopy)", "terminate"])
        log = []
        runLaunch(AppDelegate(launch: launch(first: first, cooperative: false)))
        XCTAssertEqual(log, ["activate(ignoringOtherApps)", "terminate"])
        if #available(macOS 14, *) {
            XCTAssertTrue(HandOverActivation.live.cooperative)
        } else {
            XCTAssertFalse(HandOverActivation.live.cooperative)
        }
    }

    func testTheFirstCopyBuildsItsMenuAndTheTestHostIsNeverASecondCopy() {
        let delegate = AppDelegate(launch: launch(first: nil, cooperative: true))
        delegate.applicationWillFinishLaunching(Notification(name: NSApplication.willFinishLaunchingNotification))
        XCTAssertFalse(delegate.isSecondInstance)
        XCTAssertEqual(log, ["menu"])
        XCTAssertNil(AppLaunch.live.runningFirst(), "the unit-test host shares no engine with anyone")
    }
}

/// #368: a click anywhere on the engine alarm must leave the workspace window key, so GO and the
/// transport keys keep working while the engine is down.
@MainActor
final class EngineAlarmPanelTests: XCTestCase {
    func testNothingInTheAlarmMakesItKey() throws {
        let alarm = EngineAlarmPanel()
        alarm.show(title: "Engine stopped", message: "The engine stopped.")
        alarm.setDetail("detail")
        alarm.setRestoreAction {}
        defer { alarm.dismiss() }
        let panel = try XCTUnwrap(alarm.panel)
        XCTAssertTrue(panel.becomesKeyOnlyIfNeeded)
        func walk(_ v: NSView) -> [NSView] { [v] + v.subviews.flatMap(walk) }
        let views = walk(try XCTUnwrap(panel.contentView))
        XCTAssertTrue(views.contains { ($0 as? NSTextField)?.stringValue == "The engine stopped." })
        for v in views { XCTAssertFalse(v.needsPanelToBecomeKey, "\(type(of: v))") }
    }
}
