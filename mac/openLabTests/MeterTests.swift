// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Peak meters (AUD-19, NFR-26): pushes fold into the bank, a tick at the display rate draws them,
/// and nothing runs while no meter is on screen. The display tick is driven by hand.
@MainActor
final class MeterTests: XCTestCase {
    static let intro = CueListHarness.intro

    /// Hands the bank's tick handler to the test and counts timers started and stopped.
    @MainActor final class FakeTicker {
        var handler: (@MainActor (Double) -> Void)?
        var started = 0
        var stopped = 0
        var running: Bool { handler != nil }
        func start(_ h: @escaping @MainActor (Double) -> Void) -> () -> Void {
            started += 1
            handler = h
            return { [self] in
                stopped += 1
                handler = nil
            }
        }
        func fire(_ dt: Double = 1.0 / 60) { handler?(dt) }
    }

    private var ticker: FakeTicker!
    private var bank: MeterBank!
    private var window: NSWindow!
    private var view: MeterView!

    override func setUp() async throws {
        ticker = FakeTicker()
        let t = ticker!
        bank = MeterBank(startTicker: { t.start($0) })
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 300, height: 100), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        view = MeterView(frame: NSRect(x: 0, y: 0, width: 120, height: 22))
        view.source = .cue(Self.intro)
        view.bank = bank
        window.contentView?.addSubview(view)
        window.orderFront(nil)
        view.updateLive()
    }

    override func tearDown() async throws {
        view.removeFromSuperview()
        window.orderOut(nil)
    }

    private func push(_ peaks: [Float], cue: ModelID = intro) {
        bank.ingest(outputs: [], cues: [CueMeter(cue: cue, peaks: peaks)])
    }

    // MARK: Push to state

    func testStoreFoldsAMetersPushIntoTheBank() throws {
        let rig = try Rig()
        view.bank = rig.store.meters
        XCTAssertEqual(rig.store.meters.visibleCount, 1)
        let msg = try ServerMessage.decode(
            #"{"type":"meters","outputs":[{"patch":"p1","peaks":[0.25]}],"cues":[{"cue":"\#(Self.intro)","peaks":[0.5,0.125]}]}"#
        )
        rig.store.receive(msg)
        rig.store.meters.tick(1.0 / 60)
        XCTAssertEqual(rig.store.meters.channelCount(.cue(Self.intro)), 2)
        XCTAssertEqual(rig.store.meters.level(.cue(Self.intro), channel: 0), 0.5)
        XCTAssertEqual(rig.store.meters.level(.cue(Self.intro), channel: 1), 0.125)
        XCTAssertEqual(rig.store.meters.level(.output("p1"), channel: 0), 0.25)
    }

    // MARK: Coalescing

    func testManyPushesAreOneRedrawPerTick() {
        let before = view.invalidations
        for i in 1...50 { push([Float(i) / 100, 0.1]) }
        XCTAssertEqual(view.invalidations, before, "a push does not invalidate the view")
        ticker.fire()
        XCTAssertEqual(view.invalidations, before + 1, "fifty pushes, one redraw")
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0.5, "the tick shows the loudest peak since the last")
        let afterFirst = view.invalidations
        ticker.fire(0)  // nothing new and no time passed: no redraw
        XCTAssertEqual(view.invalidations, afterFirst)
    }

    func testAViewOnlyRedrawsForItsOwnSource() {
        let before = view.invalidations
        push([0.9], cue: "other")
        ticker.fire()
        XCTAssertEqual(view.invalidations, before)
    }

    // MARK: Fall

    func testLevelFallsTwentyDbPerSecondAndEndsAtSilence() {
        push([1])
        ticker.fire(0)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 1)
        ticker.fire(0.5)  // 10 dB down
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0.3162, accuracy: 0.001)
        push([0.5])  // a louder peak than the fallen level takes over at once
        ticker.fire(0.01)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0.5, accuracy: 0.001)
        for _ in 0..<5 { ticker.fire(1) }
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0, "below -80 dB is silence")
    }

    func testIdleSourcesAreForgotten() {
        push([0.5])
        for _ in 0..<(MeterBank.idleTicks + 60 * 5) { ticker.fire(1.0 / 60) }
        XCTAssertEqual(bank.sourceCount, 0)
    }

    func testFillMapsSixtyDbRange() {
        XCTAssertEqual(MeterView.fill(0), 0)
        XCTAssertEqual(MeterView.fill(1), 1)
        XCTAssertEqual(MeterView.fill(0.1), 2.0 / 3, accuracy: 0.001)
        XCTAssertEqual(MeterView.fill(1e-6), 0)
    }

    // MARK: Hostile input

    func testBadPeaksAndHugeCountsAreBounded() {
        push([.nan, .infinity, -1, 0.5])
        push(Array(repeating: 0.1, count: 10_000))
        ticker.fire(0)
        XCTAssertEqual(bank.channelCount(.cue(Self.intro)), MeterBank.maxChannels)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0.1, accuracy: 1e-6)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 3), 0.5, accuracy: 1e-6)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 99), 0)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: -1), 0)
        for i in 0..<(MeterBank.maxSources + 50) { push([0.1], cue: ModelID("c\(i)")) }
        XCTAssertLessThanOrEqual(bank.sourceCount, MeterBank.maxSources)
    }

    // MARK: Hidden does nothing

    func testNothingRunsWhileTheViewIsHidden() {
        XCTAssertTrue(ticker.running)
        view.isHidden = true
        XCTAssertEqual(bank.visibleCount, 0)
        XCTAssertFalse(ticker.running, "the timer stops with the last visible meter")
        push([0.7])
        XCTAssertEqual(bank.sourceCount, 0, "a push with nothing on screen is dropped")
        view.isHidden = false
        XCTAssertTrue(ticker.running)
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0, "no stale level on return")
    }

    func testNothingRunsOnceTheWindowClosesOrTheViewLeaves() {
        window.orderOut(nil)
        view.updateLive()
        XCTAssertEqual(bank.visibleCount, 0)
        XCTAssertFalse(ticker.running)
        window.orderFront(nil)
        view.updateLive()
        XCTAssertTrue(ticker.running)
        view.removeFromSuperview()
        XCTAssertEqual(bank.visibleCount, 0)
        XCTAssertEqual(ticker.started, ticker.stopped)
    }

    func testOneTimerServesAllViews() {
        let second = MeterView(frame: .zero)
        second.bank = bank
        window.contentView?.addSubview(second)
        XCTAssertEqual(bank.visibleCount, 2)
        XCTAssertEqual(ticker.started, 1)
        second.removeFromSuperview()
        XCTAssertTrue(ticker.running)
    }

    func testResetClearsLevels() {
        push([0.9])
        ticker.fire(0)
        bank.reset()
        XCTAssertEqual(bank.level(.cue(Self.intro), channel: 0), 0)
    }

    // MARK: Inspector and the rest of the window

    private func inspector() async throws -> (Rig, InspectorViewController) {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let vc = InspectorViewController(store: rig.store)
        window.contentView = vc.view
        window.orderFront(nil)
        return (rig, vc)
    }

    func testLevelsTabMeterWatchesTheSelectedCueOnlyWhileTheTabShows() async throws {
        let (rig, vc) = try await inspector()
        vc.setSelection([Self.intro])
        let tab = vc.tabView.tabViewItems.firstIndex { ($0.identifier as? String) == "levels" }
        XCTAssertNotNil(tab)
        XCTAssertEqual(vc.audioLevels.meter.source, .cue(Self.intro))
        XCTAssertTrue(vc.audioLevels.meter.bank === rig.store.meters)

        vc.tabView.selectTabViewItem(at: tab ?? 0)
        XCTAssertEqual(rig.store.meters.visibleCount, 1, "the Levels tab shows its meter")
        vc.tabView.selectTabViewItem(at: 0)
        XCTAssertEqual(rig.store.meters.visibleCount, 0, "another tab: the meter is off screen and idle")
        XCTAssertFalse(rig.store.meters.isTicking)
    }

    func testFadeLevelsHaveNoMeterAndNonAudioCuesWatchNothing() async throws {
        let (_, vc) = try await inspector()
        XCTAssertNil(vc.fadeLevels.view.subviews.flatMap { $0.subviews }.first { $0 is MeterView })
        vc.setSelection([CueListHarness.group])
        XCTAssertNil(vc.audioLevels.meter.source)
    }

    func testActiveCuesAndCueListHaveNoMeters() async throws {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        let rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let wc = WorkspaceWindowController(store: rig.store)
        func meters(in v: NSView) -> Int {
            (v is MeterView ? 1 : 0) + v.subviews.reduce(0) { $0 + meters(in: $1) }
        }
        XCTAssertEqual(meters(in: wc.sidebar.view), 0)
        XCTAssertEqual(meters(in: wc.cueList.view), 0)
    }

    /// A closed window keeps its views, so the bank must let go of them at close: neither it nor
    /// the view's observers may keep the window and its views alive (AppKit itself keeps a window
    /// object around in the test host, so this checks the bank, not deallocation).
    func testClosingAWindowReleasesItsMeters() {
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 100, height: 50), styleMask: [.titled], backing: .buffered,
            defer: false)
        w.isReleasedWhenClosed = false
        let v = MeterView(frame: NSRect(x: 0, y: 0, width: 50, height: 20))
        v.bank = bank
        w.contentView?.addSubview(v)
        w.orderFront(nil)
        v.updateLive()
        XCTAssertEqual(bank.visibleCount, 2)
        w.close()
        XCTAssertEqual(bank.visibleCount, 1, "closing the window takes its meter off the bank")
    }

    func testChangingTheSourceRedrawsTheView() {
        let before = view.invalidations
        view.source = .cue("other")
        XCTAssertGreaterThan(view.invalidations, before, "bars of the old cue must not stay frozen")
    }
}
