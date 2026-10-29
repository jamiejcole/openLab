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
    private let handshakeScheduler = ManualScheduler()
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
        session = EngineSession(
            transport: engine, infoProvider: { processes.readInfo() }, scheduler: scheduler,
            handshakeScheduler: handshakeScheduler)
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

    /// An engine that accepts the connection but is slow to send its welcome (its control loop
    /// is busy while the audio thread plays) is never ended for it (#268).
    func testALaunchHandshakeTimeoutIsBusyNotSilence() async throws {
        processes.info = .success(engine.info)
        processes.alive = [engine.pid]
        engine.welcomeFault = .silent
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
        // It answers in the end: attached, nothing was lost.
        engine.welcomeFault = .none
        let ok = await pump { self.session.connectionState == .connected }
        XCTAssertTrue(ok)
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

    /// A quit while the engine this app just started is still coming up ends it: no app would
    /// ever connect, so it would run on in the background holding the audio device.
    func testQuitBeforeAStartedEngineIsReachedEndsIt() async throws {
        processes.onSpawn = { [unowned self] in
            processes.alive.insert(engine.pid)
            return engine.pid  // up, but engine.json not written yet
        }
        engine.connectFailure = true
        supervisor.start()
        _ = await pump { self.supervisor.engineNotReached != nil }
        let app = AppController(session: session, supervisor: supervisor, scheduler: ManualScheduler())
        let alerts = ScriptedAlerts()
        app.alerts = alerts
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertTrue(alerts.log.isEmpty, "nothing plays on it: no prompt")
        XCTAssertEqual(processes.signals.map(\.pid), [engine.pid])
        XCTAssertEqual(processes.signals.map(\.signal), [.terminate])
        for _ in 0..<5 { await scheduler.fire() }
        await settle()
        XCTAssertEqual(processes.spawns, 1, "no engine is started after the quit")
    }

    /// The quit's other side: an engine this run reached gets `goodbye`, never a signal.
    func testQuitNeverSignalsAnEngineThisRunReached() async throws {
        spawnsTheFake()
        supervisor.start()
        _ = await pump { self.session.connectionState == .connected }
        let app = AppController(session: session, supervisor: supervisor, scheduler: ManualScheduler())
        let alerts = ScriptedAlerts()
        alerts.saveChoices = [.dontSave]  // the fixture's show has unsaved changes
        app.alerts = alerts
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertTrue(engine.received.contains { $0 == .goodbye(shutdown: true) })
        XCTAssertTrue(processes.signals.isEmpty)
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
        XCTAssertTrue(
            presenter.detail.contains(String(format: loc("engine.alarm.editsLostOne"), "Test")),
            "the dirty show had no newer autosave: its lost edits are listed")
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

    /// An autosave the operator dismissed with the alarm (an untitled workspace's: no file to
    /// reopen it from) is still offered from File > Recover Autosaves… later.
    func testADismissedAutosaveCanStillBeRecoveredFromTheMenu() async throws {
        engine.recoveryEntries = [
            .object([
                "workspace_id": .string("00000000-0000-0000-0000-0000000000ee"), "stale": .bool(false),
            ])
        ]
        processes.crash()
        let offered = await pump { self.presenter.restore != nil }
        XCTAssertTrue(offered)
        presenter.dismiss()

        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log, ["autosave:" + loc("engine.alarm.untitled")])
        await assertEventually { self.app.allWindows.count == 3 }
        // Now a window holds it: nothing more to recover.
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log.last, "info:" + loc("recover.none"))
    }

    func testRecoverAutosavesIsInTheFileMenuAndWaitsForTheEngine() throws {
        let file = MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == loc("menu.file") }
        let item = try XCTUnwrap(file?.items.first { $0.action == #selector(AppDelegate.recoverAutosaves(_:)) })
        let delegate = AppDelegate()  // its engine is never started here
        XCTAssertFalse(delegate.validateMenuItem(item), "nothing to ask while no engine is connected")
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

    /// File > New during a reattach: no waiting window can claim a new workspace, so it gets its
    /// window at once instead of when every detached window is rebound (#262).
    func testANewWorkspaceDuringAReattachGetsItsWindowAtOnce() async throws {
        engine.holdResults = true  // the new engine does not answer the reopens yet
        processes.crash()
        let waiting = await pump { self.app.session.detached.count == 2 && self.app.session.isReattaching }
        XCTAssertTrue(waiting)
        engine.holdResults = false
        await app.newWorkspace()
        let shown = self.app.session.orderedStores.count
        XCTAssertEqual(shown, 1, "the new workspace has its store while the others wait")
        XCTAssertTrue(app.session.isReattaching)
        XCTAssertEqual(app.allWindows.count, 3)
        engine.releaseResults()
        await assertEventually { self.app.windows.count == 3 && !self.app.session.isReattaching }
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

    // MARK: Set-aside autosaves of open workspaces (#332)

    /// Hamlet's workspace id (the fixture's), the same in every engine run of the fake.
    private let hamletID = "00000000-0000-0000-0000-000000000001"

    private func setAside(_ workspace: String, name: String, ms: Int64) -> JSONValue {
        .object([
            "workspace_id": .string(workspace), "show_name": .string(name), "path_hash": .string("00000000000000aa"),
            "modified_ms": .int(ms), "stale": .bool(true),
        ])
    }

    /// #264's case: the engine dies while Hamlet has unsaved edits; its autosave was not newer
    /// than the file, so the reopen set it aside and the window came back from the file. That
    /// autosave may be the only copy of the edits: the alarm offers it although Hamlet is open,
    /// labelled as set aside; once restored it is not offered again.
    func testASetAsideAutosaveOfAReopenedWorkspaceIsOffered() async throws {
        engine.recoveryEntries = [setAside(hamletID, name: "Test", ms: 1_700_000_000_000)]
        processes.crash()
        let offered = await pump { self.presenter.restore != nil }
        XCTAssertTrue(offered)
        XCTAssertTrue(presenter.detail.contains(String(format: loc("engine.alarm.editsLostSetAside"), "Test")))
        XCTAssertFalse(presenter.detail.contains(String(format: loc("engine.alarm.editsLostOne"), "Test")))
        XCTAssertTrue(presenter.detail.contains(String(format: loc("engine.alarm.autosaves"), 1)))
        presenter.restore?()
        await assertEventually { self.app.allWindows.count == 3 }
        XCTAssertEqual(alerts.log, ["setAsideAutosave:Test"])
        XCTAssertEqual(
            engine.requestCount {
                if case .openRecovery(ModelID(self.hamletID), "00000000000000aa", 1_700_000_000_000?) = $0 {
                    return true
                }
                return false
            }, 1)
        // The window it opened in holds it now (with a fresh workspace id: Hamlet is open).
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log.last, "info:" + loc("recover.none"))
    }

    /// Review M1: Hamlet came back from its file and its autosave was set aside; a Save As
    /// elsewhere afterwards (the file server misbehaving) does not put the autosave's edits in
    /// the window, so File > Recover Autosaves… still offers it. A time is not lineage.
    func testASetAsideAutosaveIsStillOfferedAfterASaveAs() async throws {
        engine.recoveryEntries = [setAside(hamletID, name: "Test", ms: 1_700_000_000_000)]
        processes.crash()
        let offered = await pump { self.presenter.restore != nil }
        XCTAssertTrue(offered)
        presenter.dismiss()

        let hamlet = try XCTUnwrap(app.session.orderedStores.first { $0.store.model?.id.raw == hamletID }?.store)
        guard case .success = await hamlet.saveAs(path: "/Users/sam/Desktop/Hamlet.openlab") else {
            return XCTFail("save as")
        }
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log, ["setAsideAutosave:Test"])
    }

    /// A window restored from an autosave holds that autosave (should it be set aside later),
    /// and no other: an older set-aside one of the same workspace is still offered.
    func testOnlyTheAutosaveARestoreLoadedIsNotOfferedAgain() async throws {
        engine.autosaveNewer = ["/Shows/B.openlab"]  // its offer says 1_700_000_000_000
        alerts.recoveryChoice = .restore
        await app.openWorkspace(url: URL(fileURLWithPath: "/Shows/B.openlab"))
        await assertEventually { self.app.windows.count == 3 }
        let b = try XCTUnwrap(app.session.workspaces.first { $0.path == "/Shows/B.openlab" }?.workspaceId)
        engine.recoveryEntries = [
            setAside(b.raw, name: "Restored", ms: 1_700_000_000_000),
            setAside(b.raw, name: "Older", ms: 1_699_999_999_000),
        ]
        await alarm.recoverAutosaves()
        XCTAssertEqual(alerts.log.filter { $0.hasPrefix("setAside") }, ["setAsideAutosave:Older"])
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

    func testACurrentAutosaveIsOfferedOnlyWhenNoWindowHoldsItsWorkspace() {
        let mine = entry(w, ms: 5, stale: false)
        let lost = entry(other, ms: 5, stale: false)
        let window = AutosaveOffer.Window(workspace: w, path: nil, cover: AutosaveCover())
        XCTAssertEqual(AutosaveOffer.offerable([mine, lost], windows: [window]), [lost])
    }

    /// A Finder duplicate shares the workspace id: the current autosave of the other copy is not
    /// held by the window of this one (#359); the window's own copy's is.
    func testACurrentAutosaveOfAnotherCopyOfAnOpenWorkspaceIsOffered() {
        let ownHash = RecoveryPathHash.of("/Shows/Show.openlab")
        let own = entry(w, ms: 5, stale: false, hash: ownHash)
        let copy = entry(w, ms: 6, stale: false, hash: RecoveryPathHash.of("/Shows/Show copy.openlab"))
        let untitled = entry(w, ms: 7, stale: false, hash: nil)
        let window = AutosaveOffer.Window(workspace: w, path: "/Shows/Show.openlab", cover: AutosaveCover())
        XCTAssertEqual(AutosaveOffer.offerable([own, copy, untitled], windows: [window]), [copy])
    }

    /// The hash is the engine's `path_hash` (values pinned in the engine's own test and computed
    /// with the same FNV-1a): spelling-independent, NFC and case-insensitive.
    func testThePathHashMatchesTheEnginesAndIgnoresSpelling() {
        XCTAssertEqual(RecoveryPathHash.of(""), "cbf29ce484222325")
        XCTAssertEqual(RecoveryPathHash.of("/Show/a.openlab"), "1560fec8ba18f71e")
        XCTAssertEqual(RecoveryPathHash.of("/show/./x/../A.OPENLAB"), RecoveryPathHash.of("/Show/a.openlab"))
        XCTAssertEqual(RecoveryPathHash.of("/Users/sam/Shows/Caf\u{e9}.openlab"), "d4bc43e7fe1d07eb")
        XCTAssertEqual(RecoveryPathHash.of("/Users/sam/Shows/Cafe\u{301}.openlab"), "d4bc43e7fe1d07eb")
        XCTAssertEqual(RecoveryPathHash.of("///x"), RecoveryPathHash.of("/x"))
        XCTAssertNotEqual(RecoveryPathHash.of("//srv/x"), RecoveryPathHash.of("/srv/x"))
    }

    /// A restore by `open` covers the exact offer time of the window's own workspace only.
    func testASetAsideAutosaveOfAnOpenWorkspaceIsOfferedUnlessThatAutosaveWasRestored() {
        let stale = entry(w, ms: 1_000, stale: true)
        var cover = AutosaveCover()
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], windows: [.init(workspace: w, path: nil, cover: cover)]), [stale],
            "opened from its file: the file's time says nothing reliable")
        cover.loadOwn(modifiedMs: 2_000)
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], windows: [.init(workspace: w, path: nil, cover: cover)]), [stale],
            "a later autosave was restored: it need not contain this one")
        cover.loadOwn(modifiedMs: 1_000)
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], windows: [.init(workspace: w, path: nil, cover: cover)]), [])
        XCTAssertEqual(
            AutosaveOffer.offerable([stale], windows: [.init(workspace: other, path: nil, cover: cover)]), [stale],
            "another workspace's window covers nothing of this one")
        XCTAssertEqual(
            AutosaveOffer.offerable(
                [entry(w, ms: nil, stale: true)], windows: [.init(workspace: w, path: nil, cover: cover)]),
            [entry(w, ms: nil, stale: true)], "no time: never matched")
    }

    /// `open_recovery` covers that entry exactly (workspace id, path hash, time), whatever the
    /// window's own workspace id.
    func testAWindowOpenedFromAnAutosaveCoversExactlyThatEntry() {
        var cover = AutosaveCover()
        cover.load(entry(w, ms: 1_000, stale: true))
        let windows = [AutosaveOffer.Window(workspace: other, path: nil, cover: cover)]
        XCTAssertEqual(AutosaveOffer.offerable([entry(w, ms: 1_000, stale: true)], windows: windows), [])
        for different in [
            entry(w, ms: 999, stale: true), entry(w, ms: 1_001, stale: true),
            entry(w, ms: 1_000, stale: true, hash: "00000000000000bb"),
        ] {
            XCTAssertEqual(AutosaveOffer.offerable([different], windows: windows), [different])
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
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let program = dir.appendingPathComponent(SystemEngineProcesses.engineName)
        try FileManager.default.copyItem(at: URL(fileURLWithPath: "/bin/sleep"), to: program)
        let child = Process()
        child.executableURL = program
        child.arguments = ["30"]
        try child.run()
        defer { if child.isRunning { child.terminate() } }
        let p = SystemEngineProcesses(executable: nil, bundleID: "x", infoURL: URL(fileURLWithPath: "/nonexistent"))
        XCTAssertTrue(p.isEngine(child.processIdentifier))
        child.terminate()
        child.waitUntilExit()
        XCTAssertFalse(p.isEngine(child.processIdentifier), "gone once it exited")
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
        let program = dir.appendingPathComponent("engine.sh")
        try "#!/bin/sh\necho 'no audio device'\necho 'second line' >&2\necho 'no audio device' >&2\n"
            .write(to: program, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: program.path)
        let log = dir.appendingPathComponent("logs/engine-stderr.log")
        let p = SystemEngineProcesses(
            executable: program, bundleID: "x", infoURL: dir.appendingPathComponent("engine.json"), stderrURL: log,
            watchStderr: false)
        let pid = try p.spawnEngine()
        var status: Int32 = 0
        XCTAssertEqual(waitpid(pid, &status, 0), pid)
        XCTAssertEqual(p.stderrTail.read(), ["second line", "no audio device"], "stdout is not read")
        XCTAssertEqual(p.stderrTail.read(), [], "each line once")
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
                let engine = FakeEngine()
                let session = EngineSession(transport: engine, infoProvider: { .success(engine.info) })
                return (session, EngineSupervisor(processes: FakeEngineProcesses(engine: engine), session: session))
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
