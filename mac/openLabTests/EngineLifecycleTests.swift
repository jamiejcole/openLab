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

@MainActor
final class EngineSupervisorTests: XCTestCase {
    private let scheduler = ManualScheduler()
    private var engine: FakeEngine!
    private var processes: ScriptedProcesses!
    private var session: EngineSession!
    private var supervisor: EngineSupervisor!
    private var alarms: [EngineSupervisor.Alarm] = []
    private var subs: [AnyCancellable] = []
    private var clock = 0.0

    override func setUp() async throws {
        engine = FakeEngine(snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        processes = ScriptedProcesses()
        let processes = processes!
        session = EngineSession(transport: engine, infoProvider: { processes.readInfo() }, scheduler: scheduler)
        supervisor = EngineSupervisor(
            processes: processes, session: session, scheduler: ImmediateScheduler(), now: { [unowned self] in clock })
        subs.append(supervisor.alarms.sink { [unowned self] in alarms.append($0) })
    }

    override func tearDown() async throws {
        session.stop()
        subs = []
    }

    /// The engine as a process: up when spawned, writing its file.
    private func spawnsTheFake() {
        processes.onSpawn = { [unowned self] in
            processes.alive.insert(engine.pid)
            processes.info = .success(engine.info)
            return engine.pid
        }
    }

    private func pump(_ condition: @MainActor () -> Bool) async -> Bool {
        for _ in 0..<300 {
            if condition() { return true }
            await scheduler.fire()
            for _ in 0..<20 { await Task.yield() }
        }
        return condition()
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
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.spawns, 0)
        XCTAssertTrue(processes.signals.isEmpty)
    }

    func testLiveSilentEngineAtLaunchIsTerminatedThenReplaced() async throws {
        let silent: Int32 = 777
        processes.info = .success(EngineConnectionInfo(port: 9, token: engine.token, pid: silent))
        processes.alive = [silent]
        engine.connectFailure = true  // the engine named in the file does not answer
        processes.onSpawn = { [unowned self] in
            engine.connectFailure = false
            processes.alive.insert(engine.pid)
            processes.info = .success(engine.info)
            return engine.pid
        }
        supervisor.start()
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
        XCTAssertEqual(processes.signals.map(\.pid), [silent])
        XCTAssertEqual(processes.signals.map(\.signal), [.terminate])
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertTrue(alarms.isEmpty, "nothing was bound: no alarm")
    }

    func testASilentEngineThatIgnoresSigtermGetsSigkill() async throws {
        let silent: Int32 = 778
        processes.info = .success(EngineConnectionInfo(port: 9, token: "t", pid: silent))
        processes.alive = [silent]
        processes.diesOn = ["kill"]
        engine.connectFailure = true
        supervisor.start()
        _ = await pump { self.processes.spawns == 1 }
        XCTAssertEqual(processes.signals.map(\.signal), [.terminate, .kill])
        XCTAssertEqual(processes.spawns, 1)
    }

    func testAnEngineThatAnswersWithAnErrorAtLaunchIsNotEnded() async throws {
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        engine.refuseAuth = .internal  // it answers, with a refusal
        supervisor.start()
        for _ in 0..<10 {
            await scheduler.fire()
            for _ in 0..<20 { await Task.yield() }
        }
        XCTAssertTrue(processes.signals.isEmpty, "only silence ends an engine")
        XCTAssertEqual(processes.spawns, 0)
    }

    func testAnEngineThisRunTalkedToIsNeverSignalled() async throws {
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        // Alive but silent from now on (a hung engine): retried, never ended, no alarm.
        engine.connectFailure = true
        engine.dropConnections()
        for _ in 0..<20 {
            await scheduler.fire()
            for _ in 0..<20 { await Task.yield() }
        }
        XCTAssertNotEqual(session.connectionState, .connected)
        XCTAssertTrue(processes.signals.isEmpty)
        XCTAssertEqual(processes.spawns, 0)
        XCTAssertTrue(alarms.isEmpty)
    }

    func testAGracefulExitNotAskedForIsAnAlarmButNotACrash() async throws {
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        // Someone else stopped the engine: it removed its file.
        processes.alive = []
        processes.info = .failure(.missing)
        engine.connectFailure = true
        engine.dropConnections()
        _ = await pump { !self.alarms.isEmpty }
        XCTAssertEqual(alarms, [.engineStopped(crashed: false)])
        XCTAssertEqual(processes.spawns, 1, "a new engine is started for the open windows")
    }

    func testACrashLoopIsCappedAndAlarmed() async throws {
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        processes.alive = []  // crashed; every new engine dies at once (onSpawn: nothing comes up)
        engine.connectFailure = true
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

    func testLaunchAfterAnUncleanExitOffersTheAutosavesWithoutAModal() async throws {
        processes.info = .success(EngineConnectionInfo(port: 9, token: "old", pid: 4242))  // stale
        spawnsTheFake()
        engine.recoveryEntries = [
            .object(["workspace_id": .string("w-lost"), "show_name": .string("Lost"), "stale": .bool(false)])
        ]
        let presenter = RecordingPresenter()
        let alerts = ScriptedAlerts()
        let controller = EngineAlarmController(
            session: session, supervisor: supervisor, presenter: presenter, alerts: { alerts })
        supervisor.start()
        let offered = await pump { presenter.restore != nil }
        XCTAssertTrue(offered)
        XCTAssertEqual(presenter.shown.first?.title, loc("engine.alarm.unclean"))
        XCTAssertTrue(alerts.log.isEmpty, "no modal by itself")
        XCTAssertFalse(supervisor.recoveryPending, "offered once")
        let entries = try await session.scanRecovery().get()
        await controller.restore(entries)
        XCTAssertEqual(alerts.log, ["autosave:Lost"])
        await assertEventually { self.session.stores.count == 2 }
    }

    func testAShutdownIsNotACrash() async throws {
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        await session.shutdown()
        XCTAssertTrue(engine.received.contains { $0 == .goodbye(shutdown: true) })
        processes.alive = []
        processes.info = .failure(.missing)
        for _ in 0..<5 { await scheduler.fire() }
        await settle()
        XCTAssertTrue(alarms.isEmpty)
        XCTAssertEqual(processes.spawns, 0)
    }
}

/// The whole path through the app: crash, alarm, new engine, windows reattached, recovery offer.
@MainActor
final class EngineCrashFlowTests: XCTestCase {
    private let scheduler = ManualScheduler()
    private var engine: FakeEngine!
    private var processes: FakeEngineProcesses!
    private var app: AppController!
    private var alarm: EngineAlarmController!
    private var presenter: RecordingPresenter!
    private var alerts: ScriptedAlerts!
    private var supervisor: EngineSupervisor!

    override func setUp() async throws {
        engine = FakeEngine(snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        processes = FakeEngineProcesses(engine: engine)
        let processes = processes!
        let session = EngineSession(transport: engine, infoProvider: { processes.readInfo() }, scheduler: scheduler)
        supervisor = EngineSupervisor(processes: processes, session: session, scheduler: ImmediateScheduler())
        app = AppController(session: session, scheduler: ManualScheduler())
        alerts = ScriptedAlerts()
        app.alerts = alerts
        app.recentURLs = { [] }
        presenter = RecordingPresenter()
        alarm = EngineAlarmController(
            session: session, supervisor: supervisor, presenter: presenter, alerts: { [unowned self] in alerts })
        supervisor.start()
        await assertEventually { session.connectionState == .connected }
        _ = await session.open(path: "/Shows/A.openlab")
        await assertEventually {
            self.app.allWindows.count == 2 && session.stores.values.allSatisfy { $0.model != nil }
        }
    }

    override func tearDown() async throws {
        app.closeAllWindows()
        app.session.stop()
    }

    private func pump(_ condition: @MainActor () -> Bool) async -> Bool {
        for _ in 0..<300 {
            if condition() { return true }
            await scheduler.fire()
            for _ in 0..<20 { await Task.yield() }
        }
        return condition()
    }

    func testEngineCrashAlarmsStartsANewEngineReattachesAndOffersRecovery() async throws {
        let windows = app.allWindows.map(ObjectIdentifier.init)
        let oldPid = engine.pid
        // An autosave no window holds (another workspace that was open in the crashed run).
        engine.recoveryEntries = [
            .object([
                "workspace_id": .string("00000000-0000-0000-0000-0000000000ff"), "show_name": .string("Lost"),
                "modified_ms": .int(1_700_000_000_000), "stale": .bool(false),
            ])
        ]
        processes.crash()

        let back = await pump { self.presenter.restore != nil }
        XCTAssertTrue(back, "alarm, new engine, reattach and offer")
        XCTAssertEqual(presenter.shown.first?.title, loc("engine.alarm.crashed"))
        XCTAssertEqual(processes.spawns, 1)
        XCTAssertNotEqual(engine.pid, oldPid)
        // The same windows, bound to the reopened shows; none made twice.
        XCTAssertEqual(app.allWindows.map(ObjectIdentifier.init), windows)
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertEqual(Set(app.session.stores.values.compactMap { $0.model?.name }), ["Hamlet", "A"])
        XCTAssertTrue(presenter.detail.contains(loc("engine.alarm.back")))
        XCTAssertTrue(presenter.detail.contains(String(format: loc("engine.alarm.autosaves"), 1)))
        XCTAssertTrue(presenter.isVisible)

        // Nothing modal ran by itself: the operator opens the prompts with the button.
        XCTAssertTrue(alerts.log.isEmpty)
        presenter.restore?()
        await assertEventually { self.app.allWindows.count == 3 }
        XCTAssertEqual(alerts.log, ["autosave:Lost"])
        XCTAssertEqual(
            engine.requestCount {
                if case .openRecovery("00000000-0000-0000-0000-0000000000ff", _, _) = $0 { return true }
                return false
            }, 1)
    }

    /// SM-07 after a crash: the new engine run opened the show afresh (not in Show Mode); the
    /// window still is, so the engine is told again and keeps the Mac awake.
    func testShowModeIsRestoredAfterTheEngineRestarts() async throws {
        let wc = try XCTUnwrap(app.windows[ModelID(doc: 1)])
        wc.setShowMode(true)
        await assertEventually { wc.store.document?.showMode == true }
        processes.crash()
        let back = await pump {
            self.app.windows.values.contains { $0 === wc } && wc.store.document?.showMode == true
        }
        XCTAssertTrue(back, "reattached and Show Mode sent again")
        XCTAssertTrue(wc.isShowMode)
        let doc = try XCTUnwrap(app.session.doc(of: wc.store))
        XCTAssertEqual(engine.docs[doc]?["document"]?["show_mode"], .bool(true))
    }

    /// The crashed run's cues stopped with it: a window waiting to be reopened shows none playing.
    func testAWindowWaitingForItsShowShowsNoCuePlaying() async throws {
        let store = try XCTUnwrap(app.session.orderedStores.first?.store)
        XCTAssertTrue(store.cueStates.values.contains { $0.state.isActive }, "the fixture has live cues")
        engine.holdResults = true  // the new engine does not answer the reopen yet
        processes.crash()
        let alarmed = await pump { !self.presenter.shown.isEmpty }
        XCTAssertTrue(alarmed)
        XCTAssertFalse(store.cueStates.values.contains { $0.state.isActive })
        XCTAssertTrue(store.progress.isEmpty)
        let waiting = await pump { self.app.session.detached.count == 2 }
        XCTAssertTrue(waiting)
        XCTAssertFalse(store.cueStates.values.contains { $0.state.isActive })
        engine.holdResults = false
        engine.releaseResults()
        await assertEventually { !self.app.session.isReattaching }
    }

    func testTransportStillWorksWhileTheAlarmIsUp() async throws {
        processes.crash()
        let back = await pump { self.presenter.detail.contains(loc("engine.alarm.back")) }
        XCTAssertTrue(back)
        await assertEventually { self.app.session.stores.values.allSatisfy { $0.connectionState == .connected } }
        XCTAssertTrue(presenter.isVisible)
        let store = try XCTUnwrap(app.session.orderedStores.first?.store)
        if case .failure(let e) = await store.transport(.pauseAll) { XCTFail("\(e)") }
        if case .failure(let e) = await app.panicAll() { XCTFail("\(e)") }
    }

    func testOpeningAShowWithANewerAutosaveAsksFirst() async throws {
        engine.autosaveNewer = ["/Shows/B.openlab", "/Shows/C.openlab"]
        alerts.recoveryChoice = nil  // Cancel: nothing opens, no error
        await app.openWorkspace(url: URL(fileURLWithPath: "/Shows/B.openlab"))
        XCTAssertEqual(alerts.log, ["recoveryOffer:B"])
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(app.windows.count, 2)
        alerts.recoveryChoice = .restore
        await app.openWorkspace(url: URL(fileURLWithPath: "/Shows/C.openlab"))
        await assertEventually { self.app.windows.count == 3 }
        XCTAssertEqual(
            engine.requestCount {
                if case .open("/Shows/C.openlab", .restore?) = $0 { return true }
                return false
            }, 1)
    }

    func testAWindowWaitingForItsShowCannotBeClosed() async throws {
        let store = try XCTUnwrap(app.session.orderedStores.first?.store)
        var beeps = 0
        app.beep = { beeps += 1 }
        engine.holdResults = true  // the new engine does not answer the reopen yet
        processes.crash()
        let waiting = await pump { self.app.session.detached.count == 2 && self.app.session.isReattaching }
        XCTAssertTrue(waiting)
        XCTAssertNil(app.session.doc(of: store))
        let closed = await app.closeWorkspace(store)
        XCTAssertFalse(closed, "it has no doc to close yet")
        XCTAssertEqual(beeps, 1)
        XCTAssertEqual(app.allWindows.count, 2, "the windows stay while they wait")
        engine.holdResults = false
        engine.releaseResults()
        await assertEventually { self.app.windows.count == 2 && !self.app.session.isReattaching }
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
