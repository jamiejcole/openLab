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
    /// V2 is marked (`*`) as the display with the menu bar for the stage host too.
    static let displays = "V1:1920x1080,V2:1280x720*"
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

    /// A workspace of keep-black (or not) stages, each with one region on the route of its own id.
    private func model(_ stages: [(String, Bool)]) -> WorkspaceModel {
        WorkspaceModel(
            id: "w", name: "W", settings: .object([:]), patches: [], networkPatches: [],
            routes: stages.map { ModelObject(id: ModelID($0.0), raw: .object(["id": .string($0.0)])) },
            stages: stages.map {
                ModelObject(
                    id: ModelID($0.0),
                    raw: .object([
                        "keep_black": .bool($0.1),
                        "regions": .array([.object(["id": .string($0.0), "route": .string($0.0)])]),
                    ]))
            },
            cueLists: [])
    }

    /// What the engine says of each stage's route (its display choice and state) and of the stage.
    private func outputs(_ mappings: [(String, String?, String?)], panicked: Set<String> = []) -> VideoOutputs {
        VideoOutputs(
            displays: [], video: true,
            routes: mappings.map { stage, display, state in
                RouteEntry(
                    route: ModelID(stage),
                    mapping: display.map { DisplayChoice(display: $0, name: $0, width: 1920, height: 1080) },
                    state: state.map { RouteState(state: $0, display: display, reason: nil) })
            },
            stages: mappings.map { StageEntry(stage: ModelID($0.0), panicked: panicked.contains($0.0)) })
    }

    /// A keep-black stage shown on a connected display is held black, the display with the menu bar
    /// included (#708), whatever the stage host last said of it (it may be gone, failed or not yet
    /// started).
    func testAKeepBlackStageOnAConnectedDisplayIsHeldBlackTheMenuBarsIncluded() {
        let displays = Self.virtual + [BackstopDisplay(id: "V3", frame: .zero, hasMenuBar: false)]
        let m = model([("a", true), ("b", false), ("c", true), ("d", true), ("e", true)])
        let o = outputs([
            ("a", "V1", "failed"), ("b", "V3", "shown"), ("c", "V2", "shown"), ("d", nil, nil), ("e", "GONE", nil),
        ])
        XCTAssertEqual(
            BackstopPlan.covers(workspaces: [(m, o)], displays: displays), ["V1": Self.v1Frame, "V2": Self.v2Frame])
        let gone = outputs([("a", "V1", nil)])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(m, gone)], displays: displays), ["V1": Self.v1Frame])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(nil, o)], displays: displays), [:], "no model yet")
        // Taken down by the operator's Esc: held nowhere, the other stage still held.
        XCTAssertEqual(
            BackstopPlan.covers(workspaces: [(m, o)], displays: displays, dismissed: ["c"]), ["V1": Self.v1Frame])
    }

    /// A keep-black stage whose black a panic took down holds nothing until a cue plays on it again
    /// (QLab 5 manual p. 227).
    func testAStageAPanicTookDownIsNotHeldBlack() {
        let m = model([("a", true)])
        let up = outputs([("a", "V1", "shown")])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(m, up)], displays: Self.virtual), ["V1": Self.v1Frame])
        let down = outputs([("a", "V1", "hidden")], panicked: ["a"])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(m, down)], displays: Self.virtual), [:])
    }

    /// #813 G4: a keep-black stage split over two routes holds black on each of their displays;
    /// a region on no route holds none.
    func testAStageOverTwoRoutesIsHeldBlackOnBothDisplays() {
        let stage = ModelObject(
            id: "s",
            raw: .object([
                "keep_black": .bool(true),
                "regions": .array([
                    .object(["id": .string("A"), "route": .string("r1")]),
                    .object(["id": .string("B"), "route": .string("r2")]),
                    .object(["id": .string("C")]),
                ]),
            ]))
        let m = WorkspaceModel(
            id: "w", name: "W", settings: .object([:]), patches: [], networkPatches: [],
            routes: [ModelObject(id: "r1", raw: .object([:])), ModelObject(id: "r2", raw: .object([:]))],
            stages: [stage], cueLists: [])
        let o = VideoOutputs(
            displays: [], video: true,
            routes: [
                RouteEntry(
                    route: "r1", mapping: DisplayChoice(display: "V1", name: "V1", width: 1, height: 1), state: nil),
                RouteEntry(
                    route: "r2", mapping: DisplayChoice(display: "V2", name: "V2", width: 1, height: 1), state: nil),
            ],
            stages: [StageEntry(stage: "s")])
        XCTAssertEqual(
            BackstopPlan.covers(workspaces: [(m, o)], displays: Self.virtual),
            ["V1": Self.v1Frame, "V2": Self.v2Frame])
        XCTAssertEqual(BackstopPlan.displays(of: stage, model: m, outputs: o), ["V1", "V2"])
    }

    /// #813 V1b: keep-black ticked again on a stage whose black a panic took down while it was off
    /// holds nothing: the engine says its black is down whatever its keep-black, so the app never
    /// puts a black up only to take it down when the engine's next word comes (a flash).
    func testKeepBlackTickedAfterAPanicWhileOffHoldsNoBlack() {
        let ticked = model([("a", true)])
        let afterPanic = outputs([("a", "V1", "hidden")], panicked: ["a"])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(ticked, afterPanic)], displays: Self.virtual), [:])
        let armed = outputs([("a", "V1", "hidden")])
        XCTAssertEqual(
            BackstopPlan.covers(workspaces: [(ticked, armed)], displays: Self.virtual), ["V1": Self.v1Frame],
            "with its black up, ticked again: black at once")
        let unticked = model([("a", false)])
        XCTAssertEqual(BackstopPlan.covers(workspaces: [(unticked, armed)], displays: Self.virtual), [:])
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

    /// #795 S1: the pointer is hidden over a display a stage covers (shown there: its picture or its
    /// black) or the app holds black, never over another, nor over the display with the menu bar
    /// while the stage there stands back under the app's windows.
    func testThePointerIsHiddenOverEveryDisplayAStageCoversNotOverTheAppStandingBack() {
        let displays =
            Self.virtual + [
                BackstopDisplay(id: "V3", frame: NSRect(x: 0, y: 0, width: 10, height: 10), hasMenuBar: false)
            ]
        let o = outputs([("a", "V1", "shown"), ("b", "V2", "shown"), ("c", "V3", "hidden")])
        XCTAssertEqual(
            BlackBackstop.pointerFrames(displays: displays, backstops: [], outputs: [o], standingBack: false),
            [Self.v1Frame, Self.v2Frame])
        XCTAssertEqual(
            BlackBackstop.pointerFrames(displays: displays, backstops: [], outputs: [o], standingBack: true),
            [Self.v1Frame], "the menu bar's display while the app's question is up")
        XCTAssertEqual(
            BlackBackstop.pointerFrames(displays: displays, backstops: ["V3"], outputs: [], standingBack: false),
            [displays[2].frame], "the app's black between stage hosts")
        XCTAssertEqual(
            BlackBackstop.pointerFrames(displays: displays, backstops: [], outputs: [], standingBack: false), [])
    }

    /// #795 S1: the pointer over a covered display is hidden while the app is active and shown as soon
    /// as it leaves it or the app is not; every hide is matched by one unhide.
    func testThePointerHidesOnlyWhileOverACoveredDisplayWithTheAppActive() {
        let pointer = StagePointer()
        var calls: [String] = []
        var at = NSPoint(x: Self.v1Frame.midX, y: Self.v1Frame.midY)
        var active = true
        pointer.hide = { calls.append("hide") }
        pointer.unhide = { calls.append("unhide") }
        pointer.location = { at }
        pointer.isActive = { active }
        pointer.worksInBackground = { false }
        pointer.frames = [Self.v1Frame]
        XCTAssertEqual(calls, ["hide"], "over the stage, the app active")
        pointer.update()
        XCTAssertEqual(calls, ["hide"], "hidden once")
        at = NSPoint(x: Self.v2Frame.midX, y: Self.v2Frame.midY)
        pointer.update()
        XCTAssertEqual(calls, ["hide", "unhide"], "off the stage")
        at = NSPoint(x: Self.v1Frame.midX, y: Self.v1Frame.midY)
        active = false
        pointer.update()
        XCTAssertEqual(calls, ["hide", "unhide"], "another app active: shown")
        active = true
        pointer.update()
        pointer.frames = []
        XCTAssertEqual(calls, ["hide", "unhide", "hide", "unhide"], "the stage gone: shown")
        XCTAssertFalse(pointer.hidden)
    }

    /// #795 S1: no window asks for mouse-moved events and the app's own events never reach a global
    /// monitor, so the place is read on a timer while the app is active over covered displays: from a
    /// stage onto the app's window on another display shows the pointer; back onto the stage hides
    /// it; the app resigning, no display covered or the pointer going away shows it and stops the timer.
    func testThePointerFollowsItsPlaceOnATimerWhileTheAppIsActiveAndIsNeverLeftHidden() {
        var hides = 0
        var unhides = 0
        var at = NSPoint(x: Self.v1Frame.midX, y: Self.v1Frame.midY)
        var active = true
        var tick: (@MainActor () -> Void)?
        func make() -> StagePointer {
            let pointer = StagePointer()
            pointer.hide = { hides += 1 }
            pointer.unhide = { unhides += 1 }
            pointer.location = { at }
            pointer.isActive = { active }
            pointer.worksInBackground = { false }
            pointer.startPolling = { fire in
                tick = fire
                return { tick = nil }
            }
            return pointer
        }
        var pointer: StagePointer? = make()
        XCTAssertEqual(pointer?.isPolling, false, "nothing covered")
        pointer?.frames = [Self.v1Frame]
        XCTAssertEqual(pointer?.hidden, true, "over the stage, active: hidden")
        XCTAssertEqual(pointer?.isPolling, true)
        at = NSPoint(x: Self.v2Frame.midX, y: Self.v2Frame.midY)  // onto the app's window, no event
        tick?()
        XCTAssertEqual(pointer?.hidden, false, "the poll shows it over the workspace window")
        at = NSPoint(x: Self.v1Frame.midX, y: Self.v1Frame.midY)
        tick?()
        XCTAssertEqual(pointer?.hidden, true, "back onto the stage")
        pointer?.update(appActive: false)
        XCTAssertEqual([pointer?.hidden, pointer?.isPolling], [false, false], "resigned: shown, not polled")
        pointer?.update()
        XCTAssertEqual([pointer?.hidden, pointer?.isPolling], [true, true])
        pointer?.frames = []
        XCTAssertEqual([pointer?.hidden, pointer?.isPolling], [false, false], "no display covered")
        pointer?.frames = [Self.v1Frame]
        XCTAssertEqual(pointer?.hidden, true)
        XCTAssertEqual(hides - unhides, 1, "one hide not yet matched")
        pointer = nil
        XCTAssertEqual(hides, unhides, "gone: the pointer is shown again")
        XCTAssertNil(tick, "and the timer stopped")
    }

    /// #812 S1 (reopened): with another app active (Finder in front) the pointer over a showing stage
    /// is still hidden, and shown when it leaves; the timer runs the whole time a display is covered.
    /// This Mac lets a process set the cursor from the background, which was measured by hand: the
    /// pointer shot over a stage with Finder active showed no arrow once hidden this way, and
    /// `NSCursor.hide` alone showed it.
    func testThePointerOverAStageIsHiddenWhileAnotherAppIsActive() {
        XCTAssertTrue(BackgroundCursor.enabled, "this system lets the app hide the pointer from the background")
        var calls: [String] = []
        var at = NSPoint(x: Self.v1Frame.midX, y: Self.v1Frame.midY)
        let pointer = StagePointer()
        pointer.hide = { calls.append("hide") }
        pointer.unhide = { calls.append("unhide") }
        pointer.location = { at }
        pointer.isActive = { false }  // Finder is the active app
        pointer.worksInBackground = { true }
        var tick: (@MainActor () -> Void)?
        pointer.startPolling = { fire in
            tick = fire
            return { tick = nil }
        }
        pointer.frames = [Self.v1Frame]
        XCTAssertEqual(calls, ["hide"], "over the stage with another app active")
        XCTAssertTrue(pointer.isPolling, "its place is read while the app is in the background")
        at = NSPoint(x: Self.v2Frame.midX, y: Self.v2Frame.midY)
        tick?()
        XCTAssertEqual(calls, ["hide", "unhide"], "off the stage: shown")
        at = NSPoint(x: Self.v1Frame.midX, y: Self.v1Frame.midY)
        pointer.update(appActive: false)
        XCTAssertEqual(calls, ["hide", "unhide", "hide"], "resigning active does not show it over the stage")
        pointer.frames = []
        XCTAssertEqual(calls.last, "unhide")
        XCTAssertFalse(pointer.isPolling)
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

    /// The app on a real engine with two virtual displays, a new workspace made (its one stage, Stage 1,
    /// on V1) and a second stage added on V2, both keeping black, their black down until a picture
    /// plays; its store and stage ids.
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
        // A new workspace's route is on the main display (V2, with the menu bar here); these tests
        // put Stage 1 on V1 and a second stage on V2.
        await pushed("the workspace and its routes' displays arrived") {
            guard let store = session.stores[doc], store.model?.stages.count == 1 else { return false }
            return store.videoOutputs.routes.compactMap(\.mapping?.display) == ["V2"]
        }
        let store = try XCTUnwrap(session.stores[doc])
        let first = try XCTUnwrap(store.model?.stages.first?.id)
        guard case .success = await store.setStageDisplay(first, display: "V1") else {
            throw TestEngineError.refused("first stage on V1")
        }
        await pushed("Stage 1 is on V1") { store.videoOutputs.routes.compactMap(\.mapping?.display) == ["V1"] }
        let second = ModelID(UUID().uuidString.lowercased())
        guard
            case .success = await store.addStageOnItsOwnRoute(stage: second, name: "Stage 2", width: 1280, height: 720),
            case .success = await store.setStageDisplay(second, display: "V2")
        else { throw TestEngineError.refused("second stage") }
        await pushed("the second stage is on V2") {
            store.videoOutputs.routes.compactMap(\.mapping?.display) == ["V1", "V2"]
        }
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

    /// A picture shown on `stage` (the test card): its black is up from then on.
    private func played(_ store: WorkspaceStore, _ stage: ModelID) async {
        let result = await store.client.send { .request(id: $0, .stageTestCard(stage: stage, frame: 1)) }
        guard case .success = result else { return XCTFail("test card refused: \(result)") }
    }

    private func keepBlack(_ store: WorkspaceStore, _ stage: ModelID, _ on: Bool) async {
        let result = await store.send(
            command: .setStage(stage, .object(["type": .string("keep_black"), "value": .bool(on)])))
        guard case .success = result else { return XCTFail("keep-black refused: \(result)") }
    }

    private func state(_ store: WorkspaceStore, _ stage: ModelID) -> String? {
        store.stageState(stage)
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
        // #795: a click on it never reaches an app behind; the first one counts too.
        XCTAssertFalse(window.ignoresMouseEvents, file: file, line: line)
        XCTAssertFalse(window.canHide, "Hide openLab leaves it up", file: file, line: line)
        XCTAssertTrue(window.isExcludedFromWindowsMenu, file: file, line: line)
        XCTAssertTrue(window.styleMask == .borderless, file: file, line: line)
        XCTAssertEqual(
            window.collectionBehavior, [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary],
            file: file, line: line)
        let view = try XCTUnwrap(window.contentView, file: file, line: line)
        XCTAssertTrue(view.acceptsFirstMouse(for: nil), file: file, line: line)
        XCTAssertTrue(view.hitTest(NSPoint(x: view.bounds.midX, y: view.bounds.midY)) === view, file: file, line: line)
        let rep = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds), file: file, line: line)
        view.cacheDisplay(in: view.bounds, to: rep)
        for (x, y) in [(0, 0), (rep.pixelsWide / 2, rep.pixelsHigh / 2), (rep.pixelsWide - 1, rep.pixelsHigh - 1)] {
            let c = try XCTUnwrap(rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB), file: file, line: line)
            XCTAssertEqual(
                [c.redComponent, c.greenComponent, c.blueComponent, c.alphaComponent], [0, 0, 0, 1],
                "black at \(x),\(y)", file: file, line: line)
        }
    }

    /// Kills the stage host and waits until a new one draws `stage` on `display` black again,
    /// checking the backstop on `display` stayed up the whole time.
    private func killStageHostAndWaitForBlack(
        _ store: WorkspaceStore, _ stage: ModelID, on display: String, file: StaticString = #filePath,
        line: UInt = #line
    ) async throws {
        let window = try XCTUnwrap(app.backstop.windows[display], file: file, line: line)
        let hosts = stageHostPids(of: engine.pid)
        XCTAssertEqual(hosts.count, 1, "one stage host runs", file: file, line: line)
        let old = try XCTUnwrap(hosts.first, file: file, line: line)
        XCTAssertEqual(kill(old, SIGKILL), 0, file: file, line: line)
        var uncovered = false
        let black: [UInt8] = [0, 0, 0, 255]
        let drawnAgain = await eventually {
            if self.app.backstop.windows[display] !== window || !window.isVisible { uncovered = true }
            try? await self.engine.advance(seconds: 0.11)
            let now = stageHostPids(of: self.engine.pid)
            guard now.count == 1, now != [old], self.state(store, stage) == "shown",
                case .success(let ok) = await self.engine.request(
                    .displayProbe(display: display, points: [[0.5, 0.5]])),
                case .stageProbe(_, let colours)? = ok.data
            else { return false }
            return colours == [black]
        }
        XCTAssertTrue(drawnAgain, "a new stage host draws the stage black again", file: file, line: line)
        XCTAssertFalse(uncovered, "the backstop stayed up the whole time", file: file, line: line)
        XCTAssertTrue(app.backstop.windows[display] === window, file: file, line: line)
    }

    /// Flow 1: a stage keeping black, its stage host killed: the output display shows black,
    /// never the desktop, until the stage is drawn again.
    func testAStageWithKeepBlackOnItsStageHostKilledShowsBlackNeverTheDesktopUntilTheStageIsDrawnAgain()
        async throws
    {
        let (store, stage1, _) = try await start()
        await played(store, stage1)
        await pushed("the stage is up") { self.state(store, stage1) == "shown" && self.v1 != nil }
        try assertHoldsV1Black()
        try await killStageHostAndWaitForBlack(store, stage1, on: "V1")
        try assertHoldsV1Black()
    }

    /// #708 flow: a new workspace, nothing played yet → no display is covered, the app's black
    /// included; its black comes with the first picture played on a stage.
    func testANewWorkspaceWithNothingPlayedCoversNoDisplayUntilItsFirstPicture() async throws {
        let (store, stage1, stage2) = try await start()
        XCTAssertEqual(store.model?.stages.map { $0.raw["keep_black"] }, [.bool(true), .bool(true)])
        await pushed("both stages said hidden") {
            self.state(store, stage1) == "hidden" && self.state(store, stage2) == "hidden"
        }
        await settle()
        XCTAssertTrue(app.backstop.windows.isEmpty, "nothing covered before the first picture")
        await played(store, stage2)
        await pushed("the first picture brings the black") { self.app.backstop.windows["V2"] != nil }
        XCTAssertEqual(Array(app.backstop.windows.keys), ["V2"])
    }

    /// #708 flow: a stage host that crashes or is restarting while a stage is black → the display
    /// stays black (the app's backstop), on the display with the menu bar too, and Esc still takes
    /// it away, with no beep; the app keeps the key window and focus throughout.
    func testAStageHostCrashingWhileTheMenuBarsDisplayIsBlackKeepsItBlackAndEscTakesItAway() async throws {
        let (store, _, stage2) = try await start()
        let wc = try XCTUnwrap(app.controllers[ObjectIdentifier(store)])
        let workspaceWindow = try XCTUnwrap(wc.window)
        let key = NSApp.keyWindow
        let responder = workspaceWindow.firstResponder
        var beeps = 0
        app.beep = { beeps += 1 }
        await played(store, stage2)
        await pushed("the menu bar's display held black") { self.app.backstop.windows["V2"] != nil }
        let window = try XCTUnwrap(app.backstop.windows["V2"])
        XCTAssertEqual(window.frame, Self.v2Frame)
        XCTAssertFalse(window.canBecomeKey || window.canBecomeMain || window.isKeyWindow)
        XCTAssertFalse(window.ignoresMouseEvents, "a click stays with it (#795)")
        try await killStageHostAndWaitForBlack(store, stage2, on: "V2")
        XCTAssertTrue(NSApp.keyWindow === key, "the key window is unchanged")
        XCTAssertTrue(workspaceWindow.firstResponder === responder, "the focus is unchanged")
        // Esc: the app's black goes at once, and the engine takes the stage's down.
        _ = await app.panicAll()
        XCTAssertTrue(app.backstop.windows.isEmpty, "gone at once")
        XCTAssertFalse(window.isVisible)
        await pushed("the engine took the stage's black down") {
            store.videoOutputs.stages.first { $0.stage == stage2 }?.panicked == true
        }
        XCTAssertTrue(app.backstop.windows.isEmpty)
        XCTAssertEqual(beeps, 0, "no beep")
        // The next picture brings the black back.
        await played(store, stage2)
        await pushed("black again after the next picture") { self.app.backstop.windows["V2"] != nil }
    }

    /// #842 V2: Workspace Settings > Video > Output Routing, Guides ticked on an output so they
    /// cover its display, then Esc: the guides go off the display and the box is unticked, as Esc
    /// gets the operator out of a stage elsewhere.
    func testV2EscTurnsOffAGuidesTickedOnAnOutput() async throws {
        let (store, _, _) = try await start()
        let settings = WorkspaceSettingsWindowController(store: store)
        addTeardownBlock { @MainActor in settings.close() }
        await settings.present().value
        let routing = settings.videoRoutingPane
        await pushed("the routes are listed") {
            await routing.refresh()
            return routing.guideChecks.count == 2
        }
        let probe = [[0.5, 0.001]]
        func colour(_ display: String) async -> [UInt8]? {
            guard case .success(let ok) = await engine.request(.displayProbe(display: display, points: probe)),
                case .stageProbe(_, let colours)? = ok.data
            else { return nil }
            return colours.first
        }
        let before = await colour("V1")
        routing.guideChecks[0].performClick(nil)
        await pushed("the guides cover V1") {
            let now = await colour("V1")
            return now != nil && now != before && routing.guideChecks[0].state == .on
        }
        app.beep = {}
        _ = await app.panicAll()
        await pushed("Esc took the guides off V1 and unticked the box") {
            await routing.refresh()
            return await colour("V1") == before && routing.guideChecks[0].state == .off
        }
        XCTAssertEqual(store.videoOutputs.routes.map(\.guides), [false, false])
    }

    /// #842 V2, the user's path: two outputs' Guides ticked, then a real Esc key event through the
    /// app's key handling, first with the settings window key and then with the workspace window
    /// key. The boxes are unticked from what the engine pushes, with no refresh asked for.
    func testV2EscKeyFromEitherWindowUntickesBothGuidesBoxes() async throws {
        let (store, _, _) = try await start()
        let wc = WorkspaceWindowController(store: store)
        let settings = WorkspaceSettingsWindowController(store: store)
        addTeardownBlock { @MainActor in
            settings.close()
            wc.close()
        }
        wc.showWindow(nil)
        await settings.present().value
        let routing = settings.videoRoutingPane
        await pushed("the routes are listed") {
            await routing.refresh()
            return routing.guideChecks.count == 2
        }
        app.beep = {}
        func esc(in window: NSWindow?) throws {
            let event = try XCTUnwrap(
                NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: [],
                    timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: window?.windowNumber ?? 0, context: nil, characters: "\u{1B}",
                    charactersIgnoringModifiers: "\u{1B}", isARepeat: false, keyCode: 53))
            _ = app.handleKeyDown(event)
        }
        for window in [settings.window, wc.window] {
            for check in routing.guideChecks { check.performClick(nil) }
            await pushed("both boxes ticked and held by the engine") {
                routing.guideChecks.allSatisfy { $0.state == .on } && store.videoOutputs.routes.allSatisfy(\.guides)
            }
            window?.makeKeyAndOrderFront(nil)
            try esc(in: window)
            await pushed("Esc unticked both boxes") {
                routing.guideChecks.allSatisfy { $0.state == .off }
                    && store.videoOutputs.routes.allSatisfy { !$0.guides }
            }
        }
    }

    /// #708: Esc takes the app's black away even with no engine to hear it (one relaunching).
    func testEscTakesTheAppsBlackAwayWithoutAnEngine() async throws {
        let (store, stage1, _) = try await start()
        await played(store, stage1)
        await pushed { self.v1 != nil }
        // The engine killed (no supervisor here relaunches it): the workspace keeps what it last heard.
        XCTAssertEqual(kill(engine.pid, SIGKILL), 0)
        await assertEventually("the engine gone") { self.app.session.connectionState != .connected }
        XCTAssertNotNil(v1, "kept while the engine is gone")
        app.beep = {}
        _ = await app.panicAll()
        XCTAssertNil(v1, "Esc took it away")
    }

    /// #708: Esc with the engine hung takes the app's black off the display with the menu bar at once,
    /// although the engine (stopped) never answers the panic, and it is not put back while the panic
    /// waits and fails.
    func testEscWithAHungEngineTakesTheAppsBlackAwayAtOnce() async throws {
        let (store, _, stage2) = try await start()
        await played(store, stage2)
        await pushed("the menu bar's display held black") { self.app.backstop.windows["V2"] != nil }
        let pid = engine.pid
        XCTAssertEqual(kill(pid, SIGSTOP), 0)
        defer { kill(pid, SIGCONT) }
        app.beep = {}
        let esc = Task { await self.app.panicAll() }
        await assertEventually("Esc took the app's black down") { self.app.backstop.windows.isEmpty }
        let answer = await esc.value
        if case .success = answer { XCTFail("a stopped engine answered") }
        XCTAssertTrue(app.backstop.windows.isEmpty, "not put back while the engine is hung")
    }

    /// #708 flow: a hung engine while a stage covers the only display: the app's black stands back by
    /// itself (no Esc) and the operator reaches the app. The engine is stopped; the app's watch asks it
    /// each second (its clock moved by hand here): an ask still unanswered at the next stands the
    /// black on the display with the menu bar back under the app's windows. The engine answering
    /// again puts it back over everything. (The stage host's own window: stages.rs.)
    func testAHungEngineWhileAStageCoversTheOnlyDisplayTheAppsBlackStandsBackByItself() async throws {
        let watch = ManualScheduler()
        let (store, _, stage2) = try await start()
        app.engineWatchScheduler = watch
        await played(store, stage2)
        await pushed("the menu bar's display held black") { self.app.backstop.windows["V2"] != nil }
        let window = try XCTUnwrap(app.backstop.windows["V2"])
        XCTAssertEqual(window.level, BlackBackstop.level)
        let pid = engine.pid
        XCTAssertEqual(kill(pid, SIGSTOP), 0)
        var stopped = true
        defer { if stopped { kill(pid, SIGCONT) } }
        for _ in 0..<2 {
            await assertEventually { await watch.waitingCount == 1 }
            await watch.fire()
        }
        await assertEventually("stood back by itself") { window.level == BlackBackstop.standingBackLevel }
        XCTAssertTrue(window.isVisible && app.backstop.windows["V2"] === window, "black, under the app")
        XCTAssertLessThan(window.level.rawValue, NSWindow.Level.normal.rawValue)
        XCTAssertEqual(kill(pid, SIGCONT), 0)
        stopped = false
        var back = false
        for _ in 0..<20 where !back {
            await assertEventually { await watch.waitingCount == 1 }
            await watch.fire()
            back = await eventually { window.level == BlackBackstop.level }
        }
        XCTAssertTrue(back, "the engine answers again: over everything again")
    }

    /// #795 S1: a stage that keeps no black, playing on the only display, hides the pointer there; the
    /// engine hung, the stage host stands back after its silence limit, so the app takes the
    /// pointer back over its windows at the same time, with no backstop of its own up. The engine
    /// answering again hides it again.
    func testAHungEngineWhileAKeepBlackOffStageShowsTheOnlyDisplayTheAppGetsThePointerBack() async throws {
        let watch = ManualScheduler()
        let (store, _, stage2) = try await start()
        app.engineWatchScheduler = watch
        await keepBlack(store, stage2, false)
        await played(store, stage2)
        await pushed("the picture shown, no black of the app's") {
            self.state(store, stage2) == "shown" && self.app.backstop.windows["V2"] == nil
        }
        XCTAssertTrue(app.backstop.pointer.frames.contains(Self.v2Frame), "hidden over the picture")
        let pid = engine.pid
        XCTAssertEqual(kill(pid, SIGSTOP), 0)
        var stopped = true
        defer { if stopped { kill(pid, SIGCONT) } }
        for _ in 0..<2 {
            await assertEventually { await watch.waitingCount == 1 }
            await watch.fire()
        }
        await assertEventually("the pointer is the app's over its display") {
            !self.app.backstop.pointer.frames.contains(Self.v2Frame)
        }
        XCTAssertEqual(kill(pid, SIGCONT), 0)
        stopped = false
        var back = false
        for _ in 0..<20 where !back {
            await assertEventually { await watch.waitingCount == 1 }
            await watch.fire()
            back = await eventually { self.app.backstop.pointer.frames.contains(Self.v2Frame) }
        }
        XCTAssertTrue(back, "the engine answers again: hidden over the picture again")
    }

    /// The menu bar's display covered: a stage of the workspace shows a picture there (the test card)
    /// and the app holds black under it; and a cue that keeps running (a long pre-wait).
    private func coveredWhileACuePlays() async throws -> (WorkspaceStore, ModelID) {
        let (store, _, stage2) = try await start()
        await played(store, stage2)
        await pushed("the menu bar's display covered") { self.app.backstop.windows["V2"] != nil }
        let list = try XCTUnwrap(store.model?.cueLists.first?.id)
        let cue = ModelID("00000000-0000-0000-0000-0000000007a1")
        let memo: JSONValue = .object([
            "id": .string(cue.raw), "number": .null, "name": .string("Wait"), "notes": .string(""),
            "color": .string("none"), "second_color": .string("none"), "flagged": .bool(false),
            "armed": .bool(true), "auto_load": .bool(false), "skip_if_disarmed": .bool(false),
            "pre_wait": .double(600), "post_wait": .double(0), "continue_mode": .string("do_not_continue"),
            "triggers": .object(["hotkey": .null, "hotkey_enabled": .bool(true)]),
            "second_trigger": .string("nothing"), "second_trigger_on_release": .bool(false),
            "fade_and_stop_others": .object([
                "enabled": .bool(false), "scope": .string("peers"), "duration": .double(1),
            ]),
            "duck_others": .object(["enabled": .bool(false), "db": .double(-12), "duration": .double(1)]),
            "kind": .object(["type": .string("memo")]),
        ])
        guard case .success = await store.send(command: .addCue(memo, list: list, parent: nil, index: 0)),
            case .success = await store.transport(.start(cue: cue))
        else { throw TestEngineError.refused("a playing cue") }
        await pushed("the cue plays") { store.cueStates[cue]?.state.isActive == true }
        return (store, cue)
    }

    /// The app's black on the display with the menu bar, whether it stands back now.
    private var menuBarBlackStandsBack: Bool? {
        app.backstop.windows["V2"].map { $0.level == BlackBackstop.standingBackLevel }
    }

    /// #708 flow: Cmd-Q while a cue plays on the only display, then Cancel → the show plays on and the
    /// stage covers again. While the question is up, the stage and the app's black stand back under
    /// the app's windows (the question can be seen); nothing is panicked or stopped.
    func testCmdQWhileACuePlaysOnTheOnlyDisplayThenCancelTheShowPlaysOnAndTheStageCoversAgain() async throws {
        let (store, cue) = try await coveredWhileACuePlays()
        let alerts = ScriptedAlerts()
        app.alerts = alerts
        alerts.quitPlaying = false
        var whenAsked: (asking: Bool, back: Bool?)?
        alerts.onAsk = { whenAsked = (StandBack.shared.isAsking, self.menuBarBlackStandsBack) }
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit, "cancelled")
        XCTAssertEqual(alerts.log, ["quitPlaying"])
        XCTAssertEqual(whenAsked?.asking, true, "the question stood the stage back")
        XCTAssertEqual(whenAsked?.back, true, "the app's black under the app's windows while asked")
        XCTAssertFalse(StandBack.shared.isAsking)
        XCTAssertEqual(menuBarBlackStandsBack, false, "over everything again")
        await settle()
        XCTAssertEqual(store.cueStates[cue]?.state.isActive, true, "the show plays on")
        // The engine heard it: its stage host stood the stage back for the question, then covered.
        let said = await eventuallyLogged([standsBack, coversAgain])
        XCTAssertTrue(said, "the stage host stood back and covered again, in that order")
        XCTAssertEqual(store.videoOutputs.stages.first { $0.stage == stage2Of(store) }?.panicked, false, "no panic")
    }

    private func stage2Of(_ store: WorkspaceStore) -> ModelID? { store.model?.stages.last?.id }

    /// #708 flow: a save that fails while a stage covers the only display → the alert is visible over
    /// the picture (the stage and the app's black stand back while it is up) and answering it brings
    /// the cover back.
    func testASaveThatFailsWhileAStageCoversTheOnlyDisplayShowsItsAlertOverThePictureAndAnsweringBringsTheCoverBack()
        async throws
    {
        let (store, cue) = try await coveredWhileACuePlays()
        let alerts = ScriptedAlerts()
        app.alerts = alerts
        app.pickSaveURL = { _, _ in SaveTarget(url: URL(fileURLWithPath: "/nonexistent-openlab-folder/show.openlab")) }
        var whenAsked: (asking: Bool, back: Bool?)?
        alerts.onAsk = { whenAsked = (StandBack.shared.isAsking, self.menuBarBlackStandsBack) }
        let saved = await app.saveWorkspace(store)
        XCTAssertFalse(saved)
        XCTAssertEqual(alerts.errors.count, 1, "\(alerts.errors)")
        XCTAssertEqual(whenAsked?.asking, true)
        XCTAssertEqual(whenAsked?.back, true)
        XCTAssertFalse(StandBack.shared.isAsking)
        XCTAssertEqual(menuBarBlackStandsBack, false, "the cover is back")
        XCTAssertEqual(store.cueStates[cue]?.state.isActive, true)
        let said = await eventuallyLogged([standsBack, coversAgain])
        XCTAssertTrue(said, "the stage host stood back for the alert and covered again")
    }

    private let standsBack = "stands back for the app's question"
    private let coversAgain = "covers its display again"

    /// Whether the engine's show log holds lines containing `texts`, in that order, waiting for them
    /// (the stepped engine moved on between looks).
    private func eventuallyLogged(_ texts: [String]) async -> Bool {
        await eventually {
            try? await self.engine.advance(seconds: 0.05)
            guard case .success(let ok) = await self.engine.request(.logTail(max: nil)),
                case .logTail(let entries, _, _)? = ok.data
            else { return false }
            var rest = texts[...]
            for entry in entries where rest.first.map(entry.message.contains) == true { rest = rest.dropFirst() }
            return rest.isEmpty
        }
    }

    /// Flow 3: the backstop covers every display its stage is on, the one with the menu bar too
    /// (#708), never takes the focus, and goes when the stage is removed, keep-black is turned
    /// off, or the workspace closes.
    func testTheBackstopCoversItsStagesDisplayNeverTakesFocusAndGoesWhenItsStageGoes() async throws {
        let (store, stage1, stage2) = try await start()
        let wc = try XCTUnwrap(app.controllers[ObjectIdentifier(store)])
        let workspaceWindow = try XCTUnwrap(wc.window)
        let key = NSApp.keyWindow
        let responder = workspaceWindow.firstResponder

        await played(store, stage2)
        await pushed { self.state(store, stage2) == "shown" && self.app.backstop.windows["V2"] != nil }
        await played(store, stage1)
        await pushed { self.v1 != nil }
        try assertHoldsV1Black()
        XCTAssertEqual(Set(app.backstop.windows.keys), ["V1", "V2"])
        // #795 S2/S3: a click on it stays with it and brings the app forward; the app's own
        // handler is wired.
        XCTAssertNotNil(app.backstop.onClick)
        var clicks = 0
        let wired = app.backstop.onClick
        app.backstop.onClick = { clicks += 1 }
        let clickView = try XCTUnwrap(v1?.contentView)
        let down = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: NSPoint(x: 10, y: 10), modifierFlags: [], timestamp: 0,
                windowNumber: v1?.windowNumber ?? 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        clickView.mouseDown(with: down)
        clickView.rightMouseDown(with: down)
        XCTAssertEqual(clicks, 2)
        app.backstop.onClick = wired
        XCTAssertFalse(app.backstop.pointer.frames.isEmpty, "the pointer is hidden over the covered displays")
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
        // A stage put back has had no picture played on it since: its black is down.
        await played(store, stage1)
        await pushed("the stage back on its display, a picture played: up again") { self.v1 != nil }
        window = try XCTUnwrap(v1)
        let doc = try XCTUnwrap(app.session.doc(of: store))
        guard case .success = await app.session.close(doc: doc, discardChanges: true) else { return XCTFail("close") }
        await pushed("the workspace closed: gone") { self.app.backstop.windows.isEmpty }
        XCTAssertFalse(window.isVisible)
    }

    /// The stage moved to another display, its display unplugged, or the menu bar moved onto it:
    /// the backstop follows its stage's display, over that display's frame, and goes with it.
    func testTheBackstopFollowsItsStagesDisplayAndThisMacsDisplays() async throws {
        let (store, stage1, _) = try await start()
        await played(store, stage1)
        await pushed { self.v1 != nil }

        _ = await store.setStageDisplay(stage1, display: "V2")
        await pushed("moved to the menu bar's display: covered there") {
            Array(self.app.backstop.windows.keys) == ["V2"]
        }
        _ = await store.setStageDisplay(stage1, display: "V1")
        await pushed { self.v1 != nil && self.app.backstop.windows["V2"] == nil }

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
        XCTAssertEqual(v1?.frame, moved, "the menu bar moved onto V1: still covered")
    }

    /// The system moves a window whose display went away onto another display, maybe the
    /// operator's, perhaps after the app has read the displays: the second read a moment later puts
    /// the backstop back over its own display, or takes it down if that display is gone.
    func testABackstopTheSystemMovedAfterADisplayChangeGoesBackOrGoes() async throws {
        let (store, stage1, _) = try await start()
        await played(store, stage1)
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
        await assertEventually("V1 gone: the backstop goes, never over another display") {
            self.app.backstop.windows.isEmpty
        }
        XCTAssertFalse(window.isVisible)
    }

    /// Two workspaces with a stage each on V1: one backstop while either keeps black, gone when the
    /// last turns it off.
    func testTwoWorkspacesOnOneDisplayShareOneBackstop() async throws {
        let (first, a, _) = try await start()
        let (second, b, _) = try await newWorkspace()
        await played(first, a)
        await played(second, b)
        // Both blacks up, not just the first: until the second's is, the first alone holds V1.
        await pushed("both stages' black is up") {
            [(first, a), (second, b)].allSatisfy { store, stage in
                store.videoOutputs.stages.first { $0.stage == stage }?.panicked == false
            } && self.v1 != nil
        }
        let window = try XCTUnwrap(v1)
        await keepBlack(first, a, false)
        await pushed { first.model?.stages.first?.raw["keep_black"] == .bool(false) }
        await settle()
        XCTAssertTrue(v1 === window && window.isVisible, "the other workspace still keeps it black")
        await keepBlack(second, b, false)
        await pushed { self.v1 == nil }
    }

    /// #642 flow: Panic takes every stage to black and closes its window: a keep-black stage's
    /// backstop goes too, so its display shows what it shows with keep-black off. (The stage's own
    /// window going is the engine's: video_cues.rs; the test card used here stays drawn.)
    func testAPanicTakesAKeepBlackStagesBackstopDown() async throws {
        let (store, stage1, _) = try await start()
        await played(store, stage1)
        await pushed("the stage is up") { self.state(store, stage1) == "shown" && self.v1 != nil }
        let window = try XCTUnwrap(v1)
        guard case .success = await store.panic() else { return XCTFail("panic") }
        await pushed("the panic took the backstop down") { self.v1 == nil }
        XCTAssertFalse(window.isVisible)
        XCTAssertEqual(store.videoOutputs.stages.first { $0.stage == stage1 }?.panicked, true)
    }
}
