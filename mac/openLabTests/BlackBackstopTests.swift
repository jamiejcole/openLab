// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// VID-12: the app's black backstop on the output displays of keep-black stages, which the audience
/// sees while no stage host draws there. The real engine and its stage host run on two virtual
/// displays; the backstop is told where they are (off every screen, so no window covers this Mac's
/// displays), V2 playing the display with the menu bar. The engine killed and relaunched by the app:
/// EngineCrashFlowTests.
@MainActor
final class BlackBackstopTests: XCTestCase {
    static let displays = "V1:1920x1080,V2:1280x720"
    static let v1Frame = NSRect(x: -40000, y: -40000, width: 1920, height: 1080)
    static let v2Frame = NSRect(x: -36000, y: -40000, width: 1280, height: 720)
    static let virtual = [
        BackstopDisplay(id: "V1", frame: v1Frame, hasMenuBar: false),
        BackstopDisplay(id: "V2", frame: v2Frame, hasMenuBar: true),
    ]

    private var engine: TestEngine!
    private var app: AppController!
    /// The app's waits (the backstop's second read of the displays).
    private let scheduler = ManualScheduler()

    override func tearDown() async throws {
        app?.closeAllWindows()
        app?.session.stop()
        if let app { XCTAssertTrue(app.backstop.windows.isEmpty, "no workspace, no backstop") }
        app = nil
        engine?.stop()
    }

    // MARK: Which displays are held black

    private func model(_ stages: [(String, Bool)]) -> WorkspaceModel {
        WorkspaceModel(
            id: "w", name: "W", settings: .object([:]), patches: [], networkPatches: [],
            stages: stages.map { ModelObject(id: ModelID($0.0), raw: .object(["keep_black": .bool($0.1)])) },
            cueLists: [])
    }

    private func outputs(_ mappings: [(String, String?, String?)]) -> VideoOutputs {
        VideoOutputs(
            displays: [], video: true,
            stages: mappings.map { stage, display, state in
                StageMappingEntry(
                    stage: ModelID(stage),
                    mapping: display.map { StageMapping(display: $0, name: $0, width: 1920, height: 1080) },
                    state: state.map { StageState(state: $0, display: display, reason: nil) })
            })
    }

    /// Only a keep-black stage shown on a connected display without the menu bar is held black,
    /// whatever the stage host last said of it (it may be gone, failed or not yet started).
    func testOnlyAKeepBlackStageOnAConnectedOutputDisplayIsHeldBlack() {
        let displays = Self.virtual + [BackstopDisplay(id: "V3", frame: .zero, hasMenuBar: false)]
        let m = model([("a", true), ("b", false), ("c", true), ("d", true), ("e", true)])
        let o = outputs([
            ("a", "V1", "failed"), ("b", "V3", "shown"), ("c", "V2", "shown"), ("d", nil, nil), ("e", "GONE", nil),
        ])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(m, o)], displays: displays), ["V1": Self.v1Frame])
        let gone = outputs([("a", "V1", nil)])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(m, gone)], displays: displays), ["V1": Self.v1Frame])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(nil, o)], displays: displays), [:], "no model yet")
    }

    /// Two workspaces whose stages share a display hold one backstop there while either keeps black.
    func testTwoWorkspacesSharingADisplayHoldItBlackWhileEitherKeepsBlack() {
        let o = outputs([("a", "V1", "shown")])
        let on = model([("a", true)])
        let off = model([("a", false)])
        XCTAssertEqual(
            BackstopPlan.covers(workspaces: [(on, o), (on, o)], displays: Self.virtual), ["V1": Self.v1Frame])
        XCTAssertEqual(
            BackstopPlan.covers(workspaces: [(off, o), (on, o)], displays: Self.virtual), ["V1": Self.v1Frame])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(off, o), (off, o)], displays: Self.virtual), [:])
    }

    /// This Mac's displays as the backstop reads them: ColorSync's UUIDs (the stage host's display
    /// ids), each screen's frame, and the menu bar on the first screen only.
    func testThisMacsDisplaysAreNamedAsTheStageHostNamesThem() throws {
        let screens = NSScreen.screens
        guard !screens.isEmpty else { throw XCTSkip("no screen on this machine") }
        let displays = BlackBackstop.systemDisplays()
        XCTAssertEqual(displays.map(\.frame), screens.map(\.frame))
        XCTAssertEqual(displays.map(\.hasMenuBar), screens.indices.map { $0 == 0 })
        for d in displays {
            XCTAssertNotNil(UUID(uuidString: d.id), d.id)
            XCTAssertEqual(d.id, d.id.uppercased(), "CFUUID text, as the stage host writes it")
        }
    }

    // MARK: The app on the real engine

    /// The app on a real engine with two virtual displays, a new workspace made (one stage per
    /// display: Stage 1 on V1, Stage 2 on V2, keep-black off); its store and stage ids.
    private func start() async throws -> (WorkspaceStore, ModelID, ModelID) {
        engine = try await TestEngine.start(displays: Self.displays)
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        app = AppController(session: session, scheduler: scheduler)
        app.alerts = ScriptedAlerts()
        app.recentURLs = { [] }
        app.backstop.readDisplays = { Self.virtual }
        app.backstop.displaysChanged()
        session.start()
        await assertEventually("the app reached the engine") { session.connectionState == .connected }
        await assertEventually("the stage host listed its displays") {
            guard case .success(let ok) = await self.engine.request(.listDisplays),
                case .displays(let list, _)? = ok.data
            else { return false }
            return list.count == 2
        }
        return try await newWorkspace()
    }

    private func newWorkspace() async throws -> (WorkspaceStore, ModelID, ModelID) {
        guard case .success(let doc) = await app.session.newWorkspace() else { throw TestEngineError.refused("new") }
        let session = app.session
        await pushed("the workspace and its stages' displays arrived") {
            guard let store = session.stores[doc], let stages = store.model?.stages, stages.count == 2 else {
                return false
            }
            return store.videoOutputs.stages.compactMap(\.mapping?.display) == ["V1", "V2"]
        }
        let store = try XCTUnwrap(session.stores[doc])
        let stages = try XCTUnwrap(store.model?.stages.map(\.id))
        return (store, stages[0], stages[1])
    }

    /// Waits for `condition`, moving the engine's stepped clock on between looks: `video_outputs`
    /// pushes go at most every 100 ms of it.
    private func pushed(
        _ message: String = "never became true", file: StaticString = #filePath, line: UInt = #line,
        _ condition: @MainActor () async -> Bool
    ) async {
        let ok = await eventually {
            if await condition() { return true }
            try? await self.engine.advance(seconds: 0.11)
            return await condition()
        }
        XCTAssertTrue(ok, message, file: file, line: line)
    }

    private func keepBlack(_ store: WorkspaceStore, _ stage: ModelID, _ on: Bool) async {
        let result = await store.send(
            command: .setStage(stage, .object(["type": .string("keep_black"), "value": .bool(on)])))
        guard case .success = result else { return XCTFail("keep-black refused: \(result)") }
    }

    private func state(_ store: WorkspaceStore, _ stage: ModelID) -> String? {
        store.videoOutputs.stages.first { $0.stage == stage }?.state?.state
    }

    private var v1: NSWindow? { app.backstop.windows["V1"] }

    /// What the operator and audience get from the backstop on V1: up over V1's whole frame, black,
    /// under the stage host's windows and over everything else, never key or main, clicks through.
    private func assertHoldsV1Black(file: StaticString = #filePath, line: UInt = #line) throws {
        let window = try XCTUnwrap(v1, "a backstop on V1", file: file, line: line)
        XCTAssertTrue(window.isVisible, file: file, line: line)
        XCTAssertEqual(window.frame, Self.v1Frame, file: file, line: line)
        XCTAssertEqual(window.level.rawValue, NSWindow.Level.screenSaver.rawValue - 1, file: file, line: line)
        XCTAssertFalse(window.canBecomeKey || window.canBecomeMain || window.isKeyWindow, file: file, line: line)
        XCTAssertTrue(window.ignoresMouseEvents, file: file, line: line)
        XCTAssertFalse(window.canHide, "Hide openLab leaves it up", file: file, line: line)
        XCTAssertTrue(window.isExcludedFromWindowsMenu, file: file, line: line)
        XCTAssertTrue(window.styleMask == .borderless, file: file, line: line)
        XCTAssertEqual(
            window.collectionBehavior, [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary],
            file: file, line: line)
        let view = try XCTUnwrap(window.contentView, file: file, line: line)
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds), file: file, line: line)
        view.cacheDisplay(in: view.bounds, to: rep)
        for (x, y) in [(0, 0), (rep.pixelsWide / 2, rep.pixelsHigh / 2), (rep.pixelsWide - 1, rep.pixelsHigh - 1)] {
            let c = try XCTUnwrap(rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB), file: file, line: line)
            XCTAssertEqual(
                [c.redComponent, c.greenComponent, c.blueComponent, c.alphaComponent], [0, 0, 0, 1],
                "black at \(x),\(y)", file: file, line: line)
        }
    }

    /// Flow 1: a stage with keep-black on, its stage host killed: the output display shows black,
    /// never the desktop, until the stage is drawn again.
    func testAStageWithKeepBlackOnItsStageHostKilledShowsBlackNeverTheDesktopUntilTheStageIsDrawnAgain()
        async throws
    {
        let (store, stage1, _) = try await start()
        await keepBlack(store, stage1, true)
        await pushed("the stage is up black") { self.state(store, stage1) == "shown" && self.v1 != nil }
        try assertHoldsV1Black()
        let window = try XCTUnwrap(v1)
        let hosts = stageHostPids(of: engine.pid)
        XCTAssertEqual(hosts.count, 1, "one stage host runs")
        let old = try XCTUnwrap(hosts.first)
        XCTAssertEqual(kill(old, SIGKILL), 0)
        var uncovered = false
        let black: [UInt8] = [0, 0, 0, 255]
        let drawnAgain = await eventually {
            if self.v1 !== window || !window.isVisible { uncovered = true }
            try? await self.engine.advance(seconds: 0.11)
            let now = stageHostPids(of: self.engine.pid)
            guard now.count == 1, now != [old], self.state(store, stage1) == "shown",
                case .success(let ok) = await self.engine.request(.displayProbe(display: "V1", points: [[0.5, 0.5]])),
                case .stageProbe(_, let colours)? = ok.data
            else { return false }
            return colours == [black]
        }
        XCTAssertTrue(drawnAgain, "a new stage host draws the stage black again")
        XCTAssertFalse(uncovered, "the backstop stayed up the whole time")
        XCTAssertTrue(v1 === window)
        try assertHoldsV1Black()
    }

    /// Flow 3: the backstop never covers the operator's display (the one with the menu bar) or takes
    /// the focus, and goes when the stage is removed, keep-black is turned off, or the workspace closes.
    func testTheBackstopNeverCoversTheOperatorsDisplayOrTakesFocusAndGoesWhenItsStageGoes() async throws {
        let (store, stage1, stage2) = try await start()
        let wc = try XCTUnwrap(app.controllers[ObjectIdentifier(store)])
        let workspaceWindow = try XCTUnwrap(wc.window)
        let key = NSApp.keyWindow
        let responder = workspaceWindow.firstResponder

        await keepBlack(store, stage2, true)
        await pushed { self.state(store, stage2) == "shown" }
        await settle()
        XCTAssertTrue(app.backstop.windows.isEmpty, "the menu bar's display is the operator's: never covered")

        await keepBlack(store, stage1, true)
        await pushed { self.v1 != nil }
        try assertHoldsV1Black()
        XCTAssertEqual(Array(app.backstop.windows.keys), ["V1"])
        XCTAssertTrue(NSApp.keyWindow === key, "the key window is unchanged")
        XCTAssertTrue(workspaceWindow.firstResponder === responder, "the focus is unchanged")

        var window = try XCTUnwrap(v1)
        await keepBlack(store, stage1, false)
        await pushed("keep-black off: gone") { self.v1 == nil }
        XCTAssertFalse(window.isVisible)

        await keepBlack(store, stage1, true)
        await pushed { self.v1 != nil }
        window = try XCTUnwrap(v1)
        guard case .success = await store.send(command: .removeStage(stage1)) else { return XCTFail("remove") }
        await pushed("the stage removed: gone") { self.v1 == nil }
        XCTAssertFalse(window.isVisible)

        guard case .success = await store.undo() else { return XCTFail("undo") }
        await pushed("the stage back on its display: up again") { self.v1 != nil }
        window = try XCTUnwrap(v1)
        let doc = try XCTUnwrap(app.session.doc(of: store))
        guard case .success = await app.session.close(doc: doc, discardChanges: true) else { return XCTFail("close") }
        await pushed("the workspace closed: gone") { self.v1 == nil }
        XCTAssertFalse(window.isVisible)
    }

    /// The stage moved to the operator's display, its display unplugged, or the menu bar moved onto
    /// it: the backstop goes; back on an output display, it comes back over that display's frame.
    func testTheBackstopFollowsItsStagesDisplayAndThisMacsDisplays() async throws {
        let (store, stage1, _) = try await start()
        await keepBlack(store, stage1, true)
        await pushed { self.v1 != nil }

        _ = await store.setStageMapping(stage1, display: "V2")
        await pushed("moved to the operator's display: gone") { self.app.backstop.windows.isEmpty }
        _ = await store.setStageMapping(stage1, display: "V1")
        await pushed { self.v1 != nil }

        app.backstop.readDisplays = { [Self.virtual[1]] }
        app.backstop.displaysChanged()
        XCTAssertTrue(app.backstop.windows.isEmpty, "V1 unplugged: gone")
        let moved = NSRect(x: -30000, y: -40000, width: 1920, height: 1080)
        app.backstop.readDisplays = { [BackstopDisplay(id: "V1", frame: moved, hasMenuBar: false), Self.virtual[1]] }
        app.backstop.displaysChanged()
        XCTAssertEqual(v1?.frame, moved, "plugged back in elsewhere: over its new frame")
        XCTAssertEqual(v1?.isVisible, true)
        app.backstop.readDisplays = {
            [
                BackstopDisplay(id: "V1", frame: moved, hasMenuBar: true),
                BackstopDisplay(id: "V2", frame: Self.v2Frame, hasMenuBar: false),
            ]
        }
        app.backstop.displaysChanged()
        XCTAssertTrue(app.backstop.windows.isEmpty, "the menu bar moved onto V1: gone")
    }

    /// The system moves a window whose display went away onto another display, maybe the
    /// operator's, perhaps after the app has read the displays: the second read a moment later puts
    /// the backstop back over its own display, or takes it down if that display is gone.
    func testABackstopTheSystemMovedAfterADisplayChangeGoesBackOrGoes() async throws {
        let (store, stage1, _) = try await start()
        await keepBlack(store, stage1, true)
        await pushed { self.v1 != nil }
        let window = try XCTUnwrap(v1)
        NotificationCenter.default.post(name: NSApplication.didChangeScreenParametersNotification, object: NSApp)
        window.setFrame(Self.v2Frame, display: false)
        await assertEventually { await self.scheduler.waitingCount == 1 }
        await scheduler.fire()
        await assertEventually("back over V1") { window.frame == Self.v1Frame }
        XCTAssertTrue(v1 === window && window.isVisible)

        NotificationCenter.default.post(name: NSApplication.didChangeScreenParametersNotification, object: NSApp)
        app.backstop.readDisplays = { [Self.virtual[1]] }
        window.setFrame(Self.v2Frame, display: false)
        await assertEventually { await self.scheduler.waitingCount == 1 }
        await scheduler.fire()
        await assertEventually("V1 gone: the backstop goes, never over the operator's display") {
            self.app.backstop.windows.isEmpty
        }
        XCTAssertFalse(window.isVisible)
    }

    /// Two workspaces with a stage each on V1: one backstop while either keeps black, gone when the
    /// last turns it off.
    func testTwoWorkspacesOnOneDisplayShareOneBackstop() async throws {
        let (first, a, _) = try await start()
        let (second, b, _) = try await newWorkspace()
        await keepBlack(first, a, true)
        await keepBlack(second, b, true)
        await pushed { self.v1 != nil }
        let window = try XCTUnwrap(v1)
        await keepBlack(first, a, false)
        await pushed { first.model?.stages.first?.raw["keep_black"] == .bool(false) }
        await settle()
        XCTAssertTrue(v1 === window && window.isVisible, "the other workspace still keeps it black")
        await keepBlack(second, b, false)
        await pushed { self.v1 == nil }
    }
}
