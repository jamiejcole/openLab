// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The timing and loops tab of an Audio cue (AUD-11, AUD-12, AUD-14): the waveform drawing model,
/// the trim handles and fields, the play count, and how peaks are fetched, on the real engine
/// (which reads the file for `waveform` and applies the `kind` edits). The wire is recorded to check
/// what the tab sends and when.
@MainActor
final class TimeLoopsTests: XCTestCase {
    /// Intro: an Audio cue of a 62.5 s stereo file, untrimmed, played once (the demo show with a
    /// longer a.wav written for these tests).
    static let intro = TestSnapshots.intro

    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!
    private var panel: TimeLoopsPanel { vc.timeLoops }
    private var wave: WaveformView { vc.timeLoops.waveform }

    /// The tests of pure drawing and editing logic: they need no engine (and run in engine-free runs).
    private static let engineFree = [
        "testDrawingStacksChannelsAndSpansMinToMax", "testDrawingFollowsTheZoom",
        "testALiveResizeAsksForPeaksOnceWhenItEnds", "testSliceChangesApplyToTheCurrentKind",
        "testTheDrawnFadeShapeIsCoresCurve", "testViewportZoomKeepsTheAnchorAndStaysInTheFile",
        "testViewportPanStopsAtTheFileEnds",
        "testAReadStillOutForAnotherCueDoesNotLandOnTheCueShown", "testAnotherCuesReadIsNotWaitedFor",
    ]

    override func setUp() async throws {
        if Self.engineFree.contains(where: { name.contains($0) }) { return }
        rig = try await Rig.open("Demo") { show in
            try writeWAV(show.appendingPathComponent("media/a.wav"), seconds: 62.5)
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue,
                    var kind = cues.first?["kind"]
                else { return XCTFail("the demo show has no first cue") }
                kind.set("last_slice_play_count", .int(1))
                cues[0].set("kind", kind)
                lists[0].set("cues", .array(cues))
                ws.set("cue_lists", .array(lists))
            }
        }
        vc = InspectorViewController(store: rig.store)
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
    }

    override func tearDown() async throws {
        window?.orderOut(nil)
        rig?.client.stop()
    }

    /// Selects Intro and shows its tab, the waveform at least 625 points wide (10 points a second or more).
    private func showIntro() async {
        vc.setSelection([Self.intro])
        vc.tabView.selectTabViewItem(withIdentifier: "timeLoops")
        vc.view.layoutSubtreeIfNeeded()
        if wave.bounds.width < 625 { wave.setFrameSize(NSSize(width: 625, height: 120)) }
        await panel.loading?.value
        await assertEventually("the peaks came") { self.wave.peaks != nil }
    }

    private var kinds: [JSONValue] {
        rig.wire.sent.compactMap {
            guard case .command(_, let c, _) = $0, let cmd = JSONValue.parse((try? c.json.text()) ?? ""),
                cmd["property"]?["type"]?.stringValue == "kind_fields"
            else { return nil }
            return normalised(cmd["property"]?["value"])
        }
    }

    private var waveformRequests: Int {
        rig.wire.requestCount { if case .waveform = $0 { return true } else { return false } }
    }

    private func type(_ field: NSTextField, _ text: String) {
        field.stringValue = text
        panel.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
    }

    private func fire(_ control: NSControl) { _ = control.target?.perform(control.action, with: control) }

    // MARK: Drawing model

    /// AUD-14: each channel gets a lane of its own, top to bottom; a column spans the bucket's
    /// highest to lowest sample about the lane's middle, and silence is a one-point hairline.
    func testDrawingStacksChannelsAndSpansMinToMax() {
        let peaks = WaveformPeaks(
            cue: Self.intro, duration: 4, start: 0, end: 4,
            channels: [
                .init(min: [-32767, 0, -16384, 0], max: [32767, 0, 16384, 0]),
                .init(min: [0, 0, 0, -32767], max: [0, 0, 0, 0]),
            ])
        let d = WaveformDrawing(
            peaks: peaks, scale: WaveformScale(start: 0, end: 4, width: 4),
            in: CGRect(x: 0, y: 0, width: 4, height: 200))
        XCTAssertEqual(
            d.lanes.map(\.rect),
            [CGRect(x: 0, y: 0, width: 4, height: 100), CGRect(x: 0, y: 100, width: 4, height: 100)])
        let left = d.lanes[0].columns
        XCTAssertEqual(left.map(\.x), [0.5, 1.5, 2.5, 3.5])
        // Full scale fills 95 % of the half-lane each way; half scale half that.
        XCTAssertEqual(left[0].top, 50 - 47.5, accuracy: 0.01)
        XCTAssertEqual(left[0].bottom, 50 + 47.5, accuracy: 0.01)
        XCTAssertEqual(left[2].top, 50 - 47.5 * 16384 / 32767, accuracy: 0.01)
        XCTAssertEqual(left[1].bottom - left[1].top, 1, "silence is a hairline")
        XCTAssertEqual(d.lanes[1].columns[3].bottom, 150 + 47.5, accuracy: 0.01, "the second lane is its own")
    }

    /// Zoomed in, each column takes only the buckets under it; columns past the peaks are left out.
    func testDrawingFollowsTheZoom() {
        let peaks = WaveformPeaks(
            cue: Self.intro, duration: 2, start: 0, end: 2,
            channels: [.init(min: [32767, -32767], max: [32767, -32767])])
        let zoomed = WaveformDrawing(
            peaks: peaks, scale: WaveformScale(start: 0, end: 1, width: 2),
            in: CGRect(x: 0, y: 0, width: 2, height: 100))
        XCTAssertEqual(zoomed.lanes[0].columns.map(\.top), [50 - 47.5, 50 - 47.5], "the first bucket only")
        let wide = WaveformDrawing(
            peaks: peaks, scale: WaveformScale(start: 0, end: 4, width: 4),
            in: CGRect(x: 0, y: 0, width: 4, height: 100))
        XCTAssertEqual(wide.lanes[0].columns.count, 2, "nothing is drawn past the file's end")
    }

    // MARK: The tab

    func testTheTabIsThirdForAudioCuesOnly() {
        vc.setSelection([Self.intro])
        XCTAssertEqual(
            vc.tabView.tabViewItems.compactMap { $0.identifier as? String },
            ["basics", "triggers", "timeLoops", "levels"])
        vc.setSelection([TestSnapshots.fade])
        XCTAssertFalse(vc.tabView.tabViewItems.contains { $0.identifier as? String == "timeLoops" })
    }

    /// The peaks are asked for once per file and zoom: back at a zoom already seen, nothing is asked.
    func testPeaksAreFetchedOncePerZoom() async {
        await showIntro()
        let asked = waveformRequests
        XCTAssertGreaterThanOrEqual(asked, 1)
        XCTAssertEqual(wave.peaks?.channels.count, 2)
        XCTAssertEqual(panel.visible.start, 0)
        XCTAssertEqual(panel.visible.end, 62.5)
        panel.zoomIn()
        await panel.loading?.value
        XCTAssertEqual(waveformRequests, asked + 1)
        XCTAssertEqual(panel.visible.end - panel.visible.start, 31.25, accuracy: 1e-9)
        panel.zoomOut()
        XCTAssertEqual(waveformRequests, asked + 1, "the whole file again comes from the cache")
        XCTAssertEqual(wave.peaks?.end, 62.5)
    }

    // MARK: Zoom and pan (AUD-14)

    func testViewportZoomKeepsTheAnchorAndStaysInTheFile() {
        let whole = WaveformViewport(start: 0, end: 60, duration: 60)
        let z = whole.zoomed(by: 0.5, anchor: 0.75)
        XCTAssertEqual(z.start, 22.5, accuracy: 1e-9, "the 45 s point stays three quarters across")
        XCTAssertEqual(z.end, 52.5, accuracy: 1e-9)
        XCTAssertEqual(whole.zoomed(by: 4), whole, "cannot zoom past the whole file")
        let edge = whole.zoomed(by: 0.1, anchor: 1)
        XCTAssertEqual(edge.end, 60, accuracy: 1e-9, "anchored at the right edge, the end stays put")
        let tiny = whole.zoomed(by: 1e-9)
        XCTAssertEqual(tiny.span, WaveformViewport.minSpan, accuracy: 1e-9)
        XCTAssertEqual(whole.zoomed(by: .nan), whole)
        XCTAssertEqual(whole.zoomed(by: -1), whole)
        XCTAssertEqual(WaveformViewport(start: 0, end: 1, duration: 0).zoomed(by: 0.5).span, 0)
    }

    func testViewportPanStopsAtTheFileEnds() {
        let v = WaveformViewport(start: 10, end: 20, duration: 60)
        XCTAssertEqual(v.panned(by: 10).start, 20)
        XCTAssertEqual(v.panned(by: -50).start, 0)
        XCTAssertEqual(v.panned(by: 500).end, 60)
        XCTAssertEqual(v.panned(by: .infinity), v)
        XCTAssertEqual(v.panned(by: 500).span, 10, accuracy: 1e-9)
    }

    /// A panel whose reads are held until the test answers them.
    @MainActor private final class HeldReads {
        var asked: [ModelID] = []
        var held: [CheckedContinuation<Result<WaveformPeaks, EngineError>, Never>] = []
        let panel = TimeLoopsPanel()
        init() {
            panel.loadPeaks = { [unowned self] cue, _, _, _ in
                asked.append(cue)
                return await withCheckedContinuation { held.append($0) }
            }
            panel.isActive = true
        }
        func show(_ cue: ModelID, media: String?, duration: Double?) {
            let kind = media.map { JSONValue.object(["media": .string($0)]) } ?? .object([:])
            panel.show(cue: cue, kind: kind, duration: duration)
        }
        func answer(_ i: Int, cue: ModelID) {
            let json = """
                {"cue":"\(cue)","duration":10,"start":0,"end":10,"channels":[{"min":[-100],"max":[100]}]}
                """
            // swiftlint:disable:next force_try
            let peaks = try! JSONDecoder().decode(WaveformPeaks.self, from: Data(json.utf8))
            held[i].resume(returning: .success(peaks))
        }
    }

    /// #501: a read out for cue A when B (no file yet) is shown must not draw A's waveform on B.
    func testAReadStillOutForAnotherCueDoesNotLandOnTheCueShown() async {
        let reads = HeldReads()
        let a = TestSnapshots.intro
        let b = TestSnapshots.fade
        reads.show(a, media: "a.wav", duration: 10)
        await settle()
        XCTAssertEqual(reads.asked, [a])
        reads.show(b, media: nil, duration: nil)
        reads.answer(0, cue: a)
        await reads.panel.loading?.value
        XCTAssertNil(reads.panel.waveform.peaks)
    }

    /// A's read held (a stalled disk), B shown: B's read is asked at once, and A's late result is dropped.
    func testAnotherCuesReadIsNotWaitedFor() async {
        let reads = HeldReads()
        let a = TestSnapshots.intro
        let b = TestSnapshots.fade
        reads.show(a, media: "a.wav", duration: 10)
        await settle()
        reads.show(b, media: "b.wav", duration: 10)
        await settle()
        XCTAssertEqual(reads.asked, [a, b], "B's read does not wait for A's")
        reads.answer(1, cue: b)
        await reads.panel.loading?.value
        XCTAssertEqual(reads.panel.waveform.peaks?.cue, b)
        reads.answer(0, cue: a)
        await settle()
        XCTAssertEqual(reads.panel.waveform.peaks?.cue, b, "A's late result is dropped")
    }

    /// Waits for the peaks of the visible stretch, including a read a later zoom queued behind one.
    private func peaksSettled() async {
        repeat { await panel.loading?.value } while panel.reloadPending
        await assertEventually("peaks for the visible range") {
            self.wave.peaks.map { $0.start == self.panel.visible.start && $0.end == self.panel.visible.end } ?? false
        }
    }

    private var span: Double { panel.visible.end - panel.visible.start }

    /// A real event's worth of fields at a point of the waveform, as AppKit delivers them to the view.
    private final class WaveEvent: NSEvent {
        var point = NSPoint.zero
        var mag: CGFloat = 0
        var delta = (x: CGFloat(0), y: CGFloat(0))
        var precise = true
        var flags: NSEvent.ModifierFlags = []
        override var locationInWindow: NSPoint { point }
        override var magnification: CGFloat { mag }
        override var scrollingDeltaX: CGFloat { delta.x }
        override var scrollingDeltaY: CGFloat { delta.y }
        override var hasPreciseScrollingDeltas: Bool { precise }
        override var modifierFlags: NSEvent.ModifierFlags { flags }
    }

    private func windowPoint(atSeconds t: Double) -> NSPoint {
        wave.convert(NSPoint(x: wave.scale.x(t), y: WaveformView.rulerHeight + 20), to: nil)
    }

    private func pinch(_ m: CGFloat, at p: NSPoint) {
        let e = WaveEvent()
        e.point = p
        e.mag = m
        wave.magnify(with: e)
    }

    private func scroll(
        dx: CGFloat = 0, dy: CGFloat = 0, precise: Bool, flags: NSEvent.ModifierFlags = [], at p: NSPoint
    ) {
        let e = WaveEvent()
        e.point = p
        e.delta = (dx, dy)
        e.precise = precise
        e.flags = flags
        wave.scrollWheel(with: e)
    }

    private func commandKey(_ chars: String) -> NSEvent? {
        NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: .command, timestamp: 0, windowNumber: window.windowNumber,
            context: nil, characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: 0)
    }

    /// The x of the orange trim handle line nearest `x` in the drawn view, or nil: read from the pixels.
    private func drawnHandle(near x: CGFloat) -> CGFloat? {
        guard let rep = wave.bitmapImageRepForCachingDisplay(in: wave.bounds) else { return nil }
        wave.cacheDisplay(in: wave.bounds, to: rep)
        let scale = CGFloat(rep.pixelsWide) / wave.bounds.width
        let y = Int((WaveformView.rulerHeight + 60) * scale)
        var best: CGFloat?
        for px in 0..<rep.pixelsWide {
            guard let c = rep.colorAt(x: px, y: y)?.usingColorSpace(.sRGB) else { continue }
            // A one-point line between pixels is a blend with the dark wave behind it: red well above blue.
            guard c.redComponent - c.blueComponent > 0.3, c.redComponent > c.greenComponent else { continue }
            let vx = (CGFloat(px) + 0.5) / scale
            if best == nil || abs(vx - x) < abs(best! - x) { best = vx }
        }
        return best
    }

    /// Zoom buttons: two clicks in shrink the span each time with the handles staying on their times
    /// (drawn there), and out returns to the whole file but no further.
    func testZoomButtons() async {
        await showIntro()
        type(panel.startField, "20")
        await assertEventually { self.panel.startTime == 20 }
        type(panel.endField, "50")
        await assertEventually { self.panel.endTime == 50 }
        XCTAssertEqual(span, 62.5, accuracy: 1e-9)
        func handlesDrawnOnTheirTimes() {
            for t in [20.0, 50.0] where t >= panel.visible.start && t <= panel.visible.end {
                let expected = CGFloat((t - panel.visible.start) / span) * wave.bounds.width
                XCTAssertEqual(wave.scale.x(t), expected, accuracy: 0.5)
                XCTAssertEqual(drawnHandle(near: expected) ?? -99, expected, accuracy: 1.5, "handle at \(t) s")
            }
        }
        handlesDrawnOnTheirTimes()
        var spans = [span]
        for _ in 0..<2 {
            fire(panel.zoomInButton)
            await peaksSettled()
            spans.append(span)
            handlesDrawnOnTheirTimes()
        }
        XCTAssertLessThan(spans[1], spans[0])
        XCTAssertLessThan(spans[2], spans[1])
        XCTAssertEqual(panel.startTime, 20)
        XCTAssertEqual(panel.endTime, 50)
        for _ in 0..<4 { fire(panel.zoomOutButton) }
        await peaksSettled()
        XCTAssertEqual(panel.visible.start, 0)
        XCTAssertEqual(panel.visible.end, 62.5, "zoom out stops at the whole file")
        handlesDrawnOnTheirTimes()
    }

    /// Pinch zooms around the pointer: the 45 s point stays under it.
    func testPinchZoomsAroundThePointer() async {
        await showIntro()
        let p = windowPoint(atSeconds: 45)
        let x = wave.convert(p, from: nil).x
        pinch(0.5, at: p)
        XCTAssertLessThan(span, 62.5)
        XCTAssertEqual(wave.scale.seconds(x), 45, accuracy: 0.05, "45 s still under the pointer")
        pinch(0.5, at: p)
        XCTAssertEqual(wave.scale.seconds(x), 45, accuracy: 0.05)
        await peaksSettled()
        pinch(-0.5, at: p)
        pinch(-0.5, at: p)
        pinch(-0.5, at: p)
        await peaksSettled()
        XCTAssertEqual(span, 62.5, accuracy: 1e-9, "pinching in returns to, and stops at, the whole file")
    }

    func testOptionScrollZooms() async {
        await showIntro()
        let p = windowPoint(atSeconds: 45)
        let x = wave.convert(p, from: nil).x
        scroll(dy: 30, precise: true, flags: .option, at: p)
        let zoomedIn = span
        XCTAssertLessThan(zoomedIn, 62.5)
        XCTAssertEqual(wave.scale.seconds(x), 45, accuracy: 0.05)
        scroll(dy: -30, precise: true, flags: .option, at: p)
        XCTAssertGreaterThan(span, zoomedIn)
        let before = span
        final class Spy: NSResponder {
            var scrolls = 0
            override func scrollWheel(with event: NSEvent) { scrolls += 1 }
        }
        let spy = Spy()
        spy.nextResponder = wave.nextResponder
        wave.nextResponder = spy
        scroll(dy: 30, precise: true, at: p)
        XCTAssertEqual(span, before, "without Option a vertical trackpad scroll does not zoom")
        XCTAssertEqual(spy.scrolls, 1, "it goes on to the next responder, as a scroll does")
        await peaksSettled()
    }

    func testMouseWheelZooms() async {
        await showIntro()
        let p = windowPoint(atSeconds: 45)
        scroll(dy: 1, precise: false, at: p)
        let zoomedIn = span
        XCTAssertLessThan(zoomedIn, 62.5)
        scroll(dy: -1, precise: false, at: p)
        XCTAssertGreaterThan(span, zoomedIn)
        await peaksSettled()
    }

    func testKeyboardZoom() async {
        await showIntro()
        window.makeKeyAndOrderFront(nil)
        XCTAssertFalse(window.performKeyEquivalent(with: commandKey("=")!), "ignored while the waveform is not focused")
        XCTAssertEqual(span, 62.5)
        click(atSeconds: 30)
        XCTAssertTrue(window.firstResponder === wave, "a click on the waveform focuses it")
        XCTAssertTrue(window.performKeyEquivalent(with: commandKey("=")!))
        XCTAssertEqual(span, 31.25, accuracy: 1e-9)
        XCTAssertTrue(window.performKeyEquivalent(with: commandKey("-")!))
        XCTAssertEqual(span, 62.5, accuracy: 1e-9)
        await peaksSettled()
    }

    private func mouse(_ type: NSEvent.EventType, atSeconds t: Double) -> NSEvent {
        NSEvent.mouseEvent(
            with: type, location: windowPoint(atSeconds: t), modifierFlags: [], timestamp: 0,
            windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
    }

    private func click(atSeconds t: Double) {
        window.sendEvent(mouse(.leftMouseDown, atSeconds: t))
        window.sendEvent(mouse(.leftMouseUp, atSeconds: t))
    }

    /// A real mouse-down on a trim handle still takes it, and the drag moves the trim.
    func testAHandleStillDragsAfterTheWaveformTakesFocus() async {
        await showIntro()
        window.makeKeyAndOrderFront(nil)
        window.sendEvent(mouse(.leftMouseDown, atSeconds: 0.2))
        XCTAssertTrue(window.firstResponder === wave)
        XCTAssertEqual(wave.dragging, .start)
        window.sendEvent(mouse(.leftMouseDragged, atSeconds: 10))
        window.sendEvent(mouse(.leftMouseUp, atSeconds: 10))
        await assertEventually { self.kinds.last?["start_time"] == .double(10) }
        XCTAssertEqual(panel.startField.stringValue, "00:10.00")
    }

    /// Zoomed to 0-10 s, a horizontal scroll moves along the file to 20-30 s, and stops at both ends.
    func testPanWhenZoomed() async {
        await showIntro()
        panel.zoom(by: 10 / 62.5, anchor: 0)
        await peaksSettled()
        XCTAssertEqual(panel.visible.start, 0, accuracy: 1e-9)
        XCTAssertEqual(panel.visible.end, 10, accuracy: 1e-9)
        let p = windowPoint(atSeconds: 5)
        let width = wave.bounds.width
        // Content follows the fingers: dragging it left moves the view later in the file.
        scroll(dx: -2 * width, precise: true, at: p)
        XCTAssertEqual(panel.visible.start, 20, accuracy: 1e-9)
        XCTAssertEqual(panel.visible.end, 30, accuracy: 1e-9)
        await peaksSettled()
        scroll(dx: -100 * width, precise: true, at: p)
        XCTAssertEqual(panel.visible.end, 62.5, accuracy: 1e-9, "stops at the end of the file")
        XCTAssertEqual(span, 10, accuracy: 1e-9)
        scroll(dx: 100 * width, precise: true, at: p)
        XCTAssertEqual(panel.visible.start, 0, accuracy: 1e-9, "stops at the start of the file")
        // Shift + mouse wheel pans too (up is earlier, as the content follows the wheel).
        scroll(dx: 0, dy: -10, precise: false, flags: .shift, at: p)
        XCTAssertGreaterThan(panel.visible.start, 0)
        XCTAssertEqual(span, 10, accuracy: 1e-9)
        await peaksSettled()
    }

    /// A pinch asks for peaks many times a second: only one read is out at a time.
    func testAPinchReadsPeaksOneAtATime() async {
        await showIntro()
        let before = waveformRequests
        let p = windowPoint(atSeconds: 30)
        for _ in 0..<10 { pinch(0.1, at: p) }
        await peaksSettled()
        XCTAssertLessThanOrEqual(waveformRequests - before, 3)
    }

    // MARK: Trims

    /// AUD-11: dragging the start handle shows the time as it moves and sends one edit when let go.
    func testDraggingTheStartHandleSendsTheStartTime() async {
        await showIntro()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0) + 2))
        wave.drag(toX: wave.scale.x(10))
        XCTAssertEqual(panel.startField.stringValue, "00:10.00")
        XCTAssertTrue(kinds.isEmpty, "nothing is sent mid-drag")
        wave.endDrag()
        await assertEventually { self.kinds.count == 1 }
        XCTAssertEqual(kinds.last?["start_time"], .double(10))
        XCTAssertNil(kinds.last?["end_time"], "the rest of the kind is not sent")
        XCTAssertNil(kinds.last?["media"])
        await assertEventually("the model answers") {
            normalised(self.rig.store.model?.cue(Self.intro)?.raw["kind"]?["start_time"]) == .double(10)
        }
    }

    /// The end handle trims the end; dragged back to the file's end it clears the trim (null).
    func testDraggingTheEndHandleSetsAndClearsTheEndTime() async {
        await showIntro()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(62.5) - 1))
        wave.drag(toX: wave.scale.x(30))
        wave.endDrag()
        await assertEventually { self.kinds.count == 1 }
        XCTAssertEqual(kinds.last?["end_time"], .double(30))
        await assertEventually { self.wave.trimEnd == 30 }
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(30)))
        wave.drag(toX: wave.scale.x(80))
        wave.endDrag()
        await assertEventually { self.kinds.count == 2 }
        XCTAssertEqual(kinds.last?["end_time"], .null)
    }

    /// A handle cannot pass the other (core keeps 0.05 s between them), nor leave the file.
    func testHandlesStayInTheFileAndApart() async {
        await showIntro()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0)))
        wave.drag(toX: wave.scale.x(100))
        XCTAssertEqual(wave.trimStart, 62.45, accuracy: 1e-9)
        wave.drag(toX: -50)
        XCTAssertEqual(wave.trimStart, 0)
        wave.endDrag()
        XCTAssertTrue(kinds.isEmpty, "back where it started: nothing to send")
        XCTAssertFalse(wave.beginDrag(atX: wave.scale.x(30)), "no handle there")
    }

    /// SM-01: in Show Mode the handles do not move and nothing is sent.
    func testShowModeLocksTheHandlesAndFields() async {
        await showIntro()
        vc.isLocked = true
        XCTAssertFalse(wave.beginDrag(atX: wave.scale.x(0)))
        XCTAssertFalse(panel.startField.isEnabled)
        XCTAssertFalse(panel.loopRadio.isEnabled)
        type(panel.startField, "5")
        XCTAssertTrue(kinds.isEmpty)
    }

    /// Typed trims: a time sends it; one that would leave less than 0.05 s is refused with a message.
    func testTypedTrims() async {
        await showIntro()
        type(panel.startField, "5")
        await assertEventually { self.kinds.count == 1 }
        XCTAssertEqual(kinds.last?["start_time"], .double(5))
        await assertEventually { self.panel.startTime == 5 }
        await answered()
        type(panel.endField, "4")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.trim"))
        XCTAssertEqual(panel.endField.stringValue, "01:02.50", "the file's end is shown again")
        type(panel.endField, "1:00")
        await assertEventually { self.kinds.count == 2 }
        XCTAssertEqual(kinds.last?["end_time"], .double(60))
        XCTAssertNil(kinds.last?["start_time"], "only the edited field is sent")
        await assertEventually("built on the previous edit") {
            normalised(self.modelKind)?["start_time"] == .double(5)
                && normalised(self.modelKind)?["end_time"] == .double(60)
        }
        type(panel.startField, "1,5")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.time"))
        XCTAssertEqual(kinds.count, 2)
    }

    // MARK: Loops

    /// AUD-12: Play N times sends the count; Loop until devamped sends 0; Play again restores N.
    func testPlayCountAndInfiniteLoop() async {
        await showIntro()
        XCTAssertEqual(panel.playRadio.state, .on)
        type(panel.countField, "3")
        await assertEventually { self.kinds.count == 1 }
        XCTAssertEqual(kinds.last?["last_slice_play_count"], .double(3))
        type(panel.countField, "0")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.count"))
        panel.loopRadio.state = .on
        fire(panel.loopRadio)
        await assertEventually { self.kinds.count == 2 }
        XCTAssertEqual(kinds.last?["last_slice_play_count"], .double(0))
        await assertEventually { self.panel.loopRadio.state == .on && !self.panel.countField.isEnabled }
        await answered()
        panel.playRadio.state = .on
        fire(panel.playRadio)
        await assertEventually { self.kinds.count == 3 }
        XCTAssertEqual(kinds.last?["last_slice_play_count"], .double(1))
    }

    // MARK: When peaks are asked for

    /// Selecting Audio cues with another tab showing reads no file; showing the tab does.
    func testNoPeaksAreAskedWhileTheTabIsHidden() async {
        vc.setSelection([Self.intro])
        vc.tabView.selectTabViewItem(withIdentifier: "basics")
        vc.setSelection([TestSnapshots.fade])
        vc.setSelection([Self.intro])
        XCTAssertNil(panel.loading)
        XCTAssertEqual(waveformRequests, 0)
        vc.tabView.selectTabViewItem(withIdentifier: "timeLoops")
        await panel.loading?.value
        // Showing the tab may lay the waveform out to a new width, which asks again for that width.
        XCTAssertGreaterThanOrEqual(waveformRequests, 1)
        await assertEventually("the peaks came") { self.wave.peaks != nil }
    }

    /// SM-01: in Show Mode no file is read for a waveform; back in Edit Mode it is.
    func testNoPeaksAreAskedInShowMode() async {
        vc.isLocked = true
        vc.setSelection([Self.intro])
        vc.tabView.selectTabViewItem(withIdentifier: "timeLoops")
        XCTAssertNil(panel.loading)
        XCTAssertEqual(waveformRequests, 0)
        vc.isLocked = false
        await panel.loading?.value
        XCTAssertGreaterThanOrEqual(waveformRequests, 1)
    }

    /// While the window is being resized nothing is asked; once, when the resize ends.
    func testALiveResizeAsksForPeaksOnceWhenItEnds() {
        final class Resizing: WaveformView {
            var live = true
            override var inLiveResize: Bool { live }
        }
        let view = Resizing()
        var asked = 0
        view.onResize = { asked += 1 }
        for w in stride(from: 300, through: 600, by: 50) { view.setFrameSize(NSSize(width: w, height: 100)) }
        XCTAssertEqual(asked, 0)
        XCTAssertEqual(view.scale.width, 600, "the drawing follows meanwhile")
        view.live = false
        view.viewDidEndLiveResize()
        XCTAssertEqual(asked, 1)
        view.viewDidEndLiveResize()
        XCTAssertEqual(asked, 1, "owed once")
    }

    /// A start typed before the file's length is known is refused: nothing bounds it yet.
    func testAStartBeforeTheLengthIsKnownIsRefused() async {
        await showIntro()
        panel.show(cue: Self.intro, kind: rig.store.model?.cue(Self.intro)?.raw["kind"] ?? .null, duration: nil)
        type(panel.startField, "5")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.unknownLength"))
        XCTAssertEqual(panel.startField.stringValue, "00:00.00")
        XCTAssertTrue(kinds.isEmpty)
    }

    /// While it plays, the playhead line is where the cue is in the file; looping, it wraps.
    func testThePlayheadFollowsProgress() async {
        await showIntro()
        type(panel.startField, "10")
        await assertEventually { self.panel.startTime == 10 }
        await answered()
        panel.showProgress(elapsed: 3)
        XCTAssertEqual(wave.playhead, 13)
        panel.loopRadio.state = .on
        fire(panel.loopRadio)
        await assertEventually { self.panel.playCount == 0 }
        await answered()
        panel.showProgress(elapsed: 52.5 + 2)
        XCTAssertEqual(wave.playhead ?? 0, 12, accuracy: 1e-9, "the second pass")
        panel.showProgress(elapsed: nil)
        XCTAssertNil(wave.playhead)
    }

    // MARK: Slices

    private func slices(_ k: JSONValue?) -> [[Double]] {
        (k?["slices"]?.arrayValue ?? []).map { [$0["time"]?.doubleValue ?? -1, $0["play_count"]?.doubleValue ?? -1] }
    }

    private var modelKind: JSONValue? { rig.store.model?.cue(Self.intro)?.raw["kind"] }

    /// Every edit sent from the tab has been answered (the engine has it).
    private func answered() async { await assertEventually("answered") { self.vc.pendingKinds.isEmpty } }

    /// Clicks at `t` and adds a slice there; waits until the model has `count` markers.
    private func addSlice(at t: Double, expect count: Int) async {
        wave.click(atX: wave.scale.x(t))
        XCTAssertTrue(panel.addSliceButton.isEnabled)
        fire(panel.addSliceButton)
        await assertEventually("\(count) markers") { self.panel.sliceMarks.count == count }
        await answered()
    }

    /// AUD-12: a click places the insertion point and the new-slice button puts a marker there,
    /// in time order; the slice before a new marker plays once and the one after keeps its count.
    /// A marker too near a trim or another marker is refused.
    func testAddingSlicesAtTheInsertionPoint() async {
        await showIntro()
        XCTAssertFalse(panel.addSliceButton.isEnabled, "nowhere to add yet")
        XCTAssertFalse(panel.deleteSlicesButton.isEnabled)
        XCTAssertTrue(panel.slicesNote.isHidden)
        await addSlice(at: 10, expect: 1)
        XCTAssertEqual(slices(kinds.last), [[10, 1]])
        panel.editSliceCount(0)
        type(panel.sliceCountField, "3")
        await assertEventually { self.panel.sliceMarks.first?.playCount == 3 }
        await answered()
        await addSlice(at: 5, expect: 2)
        XCTAssertEqual(slices(kinds.last), [[5, 1], [10, 3]], "the split slice's count stays after the marker")
        XCTAssertFalse(panel.slicesNote.isHidden, "the note says each slice has its own count")
        XCTAssertFalse(panel.loopRadio.isEnabled)
        XCTAssertEqual(wave.sliceMarks, [5, 10])
        XCTAssertEqual(wave.sliceCounts, [1, 3, 1])
        let sent = kinds.count
        wave.click(atX: wave.scale.x(5.02))
        fire(panel.addSliceButton)
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.slice"))
        wave.click(atX: wave.scale.x(0))
        fire(panel.addSliceButton)
        XCTAssertEqual(kinds.count, sent, "nothing sent for a refused marker")
    }

    /// AUD-14: a marker dragged sends its new time once, kept 0.05 s from its neighbours; dragged
    /// out of the waveform and let go it is deleted, but brought back in first it is only moved.
    func testDraggingSliceMarkersMovesAndDeletesThem() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        await addSlice(at: 20, expect: 2)
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(20)))
        XCTAssertEqual(wave.dragging, .slice(1))
        wave.drag(toX: wave.scale.x(4))
        XCTAssertEqual(wave.sliceMarks[1], 10.05, accuracy: 1e-9, "stops 0.05 s after the marker before")
        XCTAssertEqual(kinds.count, 2, "nothing is sent mid-drag")
        wave.drag(toX: wave.scale.x(30))
        wave.endDrag()
        await assertEventually { self.slices(self.kinds.last) == [[10, 1], [30, 1]] }
        await assertEventually { self.wave.sliceMarks == [10, 30] }
        // Out of the view and back in: a move.
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(10)))
        wave.drag(toX: wave.scale.x(12), y: -40)
        XCTAssertTrue(wave.removing)
        wave.drag(toX: wave.scale.x(12), y: 40)
        XCTAssertFalse(wave.removing)
        wave.endDrag()
        await assertEventually { self.slices(self.kinds.last) == [[12, 1], [30, 1]] }
        await assertEventually { self.wave.sliceMarks == [12, 30] }
        // Out of the view and let go: gone.
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(30)))
        wave.drag(toX: wave.scale.x(31), y: -40)
        wave.endDrag()
        await assertEventually { self.slices(self.kinds.last) == [[12, 1]] }
    }

    /// Slice edits made one after another before the engine answers each keep the earlier ones.
    func testQuickSliceEditsBuildOnEachOther() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        await addSlice(at: 20, expect: 2)
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(10)))
        wave.drag(toX: wave.scale.x(12))
        wave.endDrag()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(20)))
        wave.drag(toX: wave.scale.x(40))
        wave.endDrag()
        panel.editSliceCount(1)
        type(panel.sliceCountField, "5")
        await assertEventually { self.slices(self.kinds.last) == [[12, 1], [40, 5]] }
        await assertEventually { self.slices(self.modelKind) == [[12, 1], [40, 5]] }
    }

    /// Another client (or OSC, or an undo) adds a marker at `t` while the tab has the cue shown.
    private func addMarkerElsewhere(_ t: Double) async {
        guard var k = modelKind else { return XCTFail("no kind") }
        let marks = (k["slices"]?.arrayValue ?? []) + [.object(["time": .double(t), "play_count": .int(1)])]
        k.set("slices", .array(marks.sorted { ($0["time"]?.doubleValue ?? 0) < ($1["time"]?.doubleValue ?? 0) }))
        _ = await rig.store.send(command: .setProperty(Self.intro, .kind(k)), coalesce: nil)
        await assertEventually("the tab has the other edit") { self.panel.sliceMarks.contains { $0.time == t } }
        await answered()
    }

    /// A marker added elsewhere while a marker is dragged is kept when the drag is let go: the
    /// drag sends only its own move.
    func testAnEditElsewhereDuringADragIsKept() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        await addSlice(at: 20, expect: 2)
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(10)))
        wave.drag(toX: wave.scale.x(12))
        await addMarkerElsewhere(40)
        XCTAssertEqual(wave.sliceMarks, [12, 20], "the drag keeps its own view meanwhile")
        wave.endDrag()
        await assertEventually { self.slices(self.modelKind) == [[12, 1], [20, 1], [40, 1]] }
        XCTAssertEqual(slices(kinds.last), [[12, 1], [20, 1], [40, 1]])
        XCTAssertEqual(wave.sliceMarks, [12, 20, 40])
    }

    /// A drag let go where it began leaves no stale view behind: a new slice added next keeps a
    /// marker added elsewhere during the drag.
    func testANewSliceAfterAnUnchangedDragKeepsAnEditElsewhere() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(10)))
        await addMarkerElsewhere(40)
        wave.endDrag()
        XCTAssertEqual(wave.sliceMarks, [10, 40])
        await addSlice(at: 30, expect: 3)
        XCTAssertEqual(slices(kinds.last), [[10, 1], [30, 1], [40, 1]])
    }

    /// Each slice change applies to the cue it is given, touching only its own marker; a marker
    /// that is gone or a place too near another changes nothing.
    func testSliceChangesApplyToTheCurrentKind() {
        let base = TimeLoopsPanel.apply(
            .addSlice(20), to: TimeLoopsPanel.apply(.addSlice(10), to: TestSnapshots.audioKind("a.wav")))
        XCTAssertEqual(slices(base), [[10, 1], [20, 1]])
        XCTAssertEqual(slices(TimeLoopsPanel.apply(.moveSlice(from: 10, to: 12), to: base)), [[12, 1], [20, 1]])
        XCTAssertEqual(TimeLoopsPanel.apply(.moveSlice(from: 11, to: 12), to: base), base, "no marker there")
        XCTAssertEqual(TimeLoopsPanel.apply(.moveSlice(from: 10, to: 25), to: base), base, "never past a neighbour")
        XCTAssertEqual(TimeLoopsPanel.apply(.addSlice(20.01), to: base), base, "too near")
        XCTAssertEqual(slices(TimeLoopsPanel.apply(.removeSlice(10), to: base)), [[20, 1]])
        XCTAssertEqual(slices(TimeLoopsPanel.apply(.sliceCount(20, 0), to: base)), [[10, 1], [20, 0]])
        XCTAssertEqual(TimeLoopsPanel.apply(.sliceCount(30, 2), to: base), base)
        let faded = TimeLoopsPanel.apply(.fades(fadeIn: 2, fadeOut: nil, curve: "s_curve"), to: base)
        XCTAssertEqual(faded["fade_in"]?["duration"]?.doubleValue, 2)
        XCTAssertEqual(faded["fade_out"]?["duration"]?.doubleValue, 0, "the fade-out is kept")
        XCTAssertEqual(faded["fade_out"]?["curve"], .string("s_curve"))
    }

    /// AUD-12: clearing removes every marker; it is off while there are none.
    func testClearingSlices() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        await addSlice(at: 20, expect: 2)
        XCTAssertTrue(panel.deleteSlicesButton.isEnabled)
        fire(panel.deleteSlicesButton)
        await assertEventually { !self.panel.hasSlices }
        await answered()
        XCTAssertEqual(slices(kinds.last), [])
        XCTAssertFalse(panel.deleteSlicesButton.isEnabled)
        XCTAssertTrue(panel.slicesNote.isHidden)
        XCTAssertTrue(panel.loopRadio.isEnabled)
    }

    /// AUD-12: a double-click on a slice's count edits it in place: a number of plays, a letter
    /// to loop until devamped (the last slice's count is the cue's), and 0 is refused.
    func testEditingEachSlicesPlayCount() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        wave.doubleClick(atX: wave.scale.x(5), y: wave.waveRect.maxY - 2)
        XCTAssertEqual(panel.editingSlice, 0)
        XCTAssertFalse(panel.sliceCountField.isHidden)
        XCTAssertEqual(panel.sliceCountField.stringValue, "1")
        type(panel.sliceCountField, "4")
        await assertEventually { self.slices(self.kinds.last) == [[10, 4]] }
        XCTAssertTrue(panel.sliceCountField.isHidden)
        wave.doubleClick(atX: wave.scale.x(30), y: wave.waveRect.maxY - 2)
        XCTAssertEqual(panel.editingSlice, 1, "the slice after the last marker")
        type(panel.sliceCountField, "loop")
        await assertEventually { normalised(self.kinds.last?["last_slice_play_count"]) == .double(0) }
        await assertEventually { self.wave.sliceCounts == [4, 0] }
        panel.editSliceCount(1)
        XCTAssertEqual(panel.sliceCountField.stringValue, "\u{221E}")
        let sent = kinds.count
        panel.editSliceCount(0)
        type(panel.sliceCountField, "0")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.sliceCount"))
        XCTAssertEqual(kinds.count, sent)
        wave.doubleClick(atX: wave.scale.x(5), y: wave.waveRect.midY)
        XCTAssertNil(panel.editingSlice, "only on the counts along the bottom")
    }

    /// The trims stop 0.05 s short of the slice markers, dragged or typed: core would drop a
    /// marker they passed.
    func testTrimsStopShortOfSliceMarkers() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        type(panel.endField, "10.01")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.trimSlices"))
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(62.5)))
        wave.drag(toX: wave.scale.x(0))
        XCTAssertEqual(wave.trimEnd ?? 0, 10.05, accuracy: 1e-9)
        wave.endDrag()
        await assertEventually { normalised(self.kinds.last?["end_time"]) == .double(10.05) }
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0)))
        wave.drag(toX: wave.scale.x(20))
        XCTAssertEqual(wave.trimStart, 9.95, accuracy: 1e-9)
        wave.endDrag()
        await assertEventually { normalised(self.kinds.last?["start_time"]) == .double(9.95) }
        type(panel.startField, "9.99")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.trimSlices"))
        XCTAssertEqual(slices(modelKind), [[10, 1]], "the marker is kept")
    }

    // MARK: Built-in fades

    /// AUD-10: switching the built-in fades on gives each way a fade of 1 s (a quarter of a short
    /// cue at most); the curve menu sets one curve for both; off sets both lengths to 0.
    func testTheFadeSwitchAndCurve() async {
        await showIntro()
        XCTAssertEqual(panel.fadeCheck.state, .off)
        XCTAssertFalse(panel.fadeCurvePopup.isEnabled)
        XCTAssertFalse(wave.fadesShown)
        panel.fadeCheck.state = .on
        fire(panel.fadeCheck)
        await assertEventually { self.panel.hasFades }
        await answered()
        XCTAssertEqual(kinds.last?["fade_in"], .object(["duration": .double(1), "curve": .string("linear")]))
        XCTAssertEqual(kinds.last?["fade_out"], .object(["duration": .double(1), "curve": .string("linear")]))
        XCTAssertTrue(wave.fadesShown)
        XCTAssertTrue(panel.fadeCurvePopup.isEnabled)
        panel.fadeCurvePopup.selectItem(at: FadeSettingsPanel.curves.firstIndex(of: "equal_power")!)
        fire(panel.fadeCurvePopup)
        await assertEventually { self.panel.fadeCurve == "equal_power" }
        await answered()
        XCTAssertEqual(kinds.last?["fade_out"]?["curve"], .string("equal_power"))
        XCTAssertEqual(wave.fadeCurve, "equal_power")
        panel.fadeCheck.state = .off
        fire(panel.fadeCheck)
        await assertEventually { !self.panel.hasFades }
        await answered()
        XCTAssertEqual(kinds.last?["fade_in"], .object(["duration": .double(0), "curve": .string("equal_power")]))
        XCTAssertEqual(kinds.last?["fade_out"]?["duration"], .double(0))
        XCTAssertFalse(wave.fadesShown)
        // A short cue gets a quarter of its length each way.
        type(panel.endField, "2")
        await assertEventually { self.panel.endTime == 2 }
        await answered()
        panel.fadeCheck.state = .on
        fire(panel.fadeCheck)
        await assertEventually { self.panel.hasFades }
        await answered()
        XCTAssertEqual(kinds.last?["fade_in"]?["duration"], .double(0.5))
    }

    /// AUD-14: the fade handles along the top of the waves drag the fade-in and fade-out lengths,
    /// sent once when let go; neither passes the other.
    func testDraggingTheFadeHandles() async {
        await showIntro()
        panel.fadeCheck.state = .on
        fire(panel.fadeCheck)
        await assertEventually { self.panel.hasFades }
        await answered()
        let top = wave.waveRect.minY + 2
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(1), y: top))
        XCTAssertEqual(wave.dragging, .fadeIn)
        wave.drag(toX: wave.scale.x(3), y: top)
        let sent = kinds.count
        wave.endDrag()
        await assertEventually { self.kinds.count == sent + 1 }
        XCTAssertEqual(kinds.last?["fade_in"]?["duration"], .double(3))
        XCTAssertEqual(kinds.last?["fade_out"]?["duration"], .double(1))
        await assertEventually { self.panel.fadeIn == 3 }
        await answered()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(61.5), y: top))
        XCTAssertEqual(wave.dragging, .fadeOut)
        wave.drag(toX: wave.scale.x(0), y: top)
        XCTAssertEqual(wave.fadeOut, 59.5, accuracy: 1e-9, "stops where the fade-in ends")
        wave.endDrag()
        await assertEventually { self.panel.fadeOut == 59.5 }
        await answered()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0), y: 2), "in the ruler the start trim is taken")
        XCTAssertEqual(wave.dragging, .start)
        wave.endDrag()
    }

    /// A curve chosen right after a fade drag, before the engine answers, keeps the drag.
    func testQuickFadeEditsBuildOnEachOther() async {
        await showIntro()
        panel.fadeCheck.state = .on
        fire(panel.fadeCheck)
        let top = wave.waveRect.minY + 2
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(1), y: top))
        wave.drag(toX: wave.scale.x(2), y: top)
        wave.endDrag()
        panel.fadeCurvePopup.selectItem(at: FadeSettingsPanel.curves.firstIndex(of: "s_curve")!)
        fire(panel.fadeCurvePopup)
        let want: JSONValue = .object(["duration": .double(2), "curve": .string("s_curve")])
        await assertEventually { self.kinds.last?["fade_in"] == want }
        await assertEventually { normalised(self.modelKind?["fade_in"]) == want }
    }

    /// The edits made in the tab produce the timing fields of the golden kind edit that the
    /// engine's real-audio test plays sample by sample (`command_set_audio_timing.json`).
    func testTheTabBuildsTheGoldenTimingKind() async throws {
        await showIntro()
        type(panel.startField, "0.25")
        await assertEventually { self.panel.startTime == 0.25 }
        await answered()
        type(panel.endField, "1.75")
        await assertEventually { self.panel.endTime == 1.75 }
        await answered()
        await addSlice(at: 0.5, expect: 1)
        await addSlice(at: 1, expect: 2)
        panel.editSliceCount(0)
        type(panel.sliceCountField, "3")
        await assertEventually { self.panel.sliceMarks.first?.playCount == 3 }
        await answered()
        panel.editSliceCount(2)
        type(panel.sliceCountField, "2")
        await assertEventually { self.panel.playCount == 2 }
        await answered()
        panel.fadeCheck.state = .on
        fire(panel.fadeCheck)
        await assertEventually { self.panel.fadeIn == 0.375 }
        await answered()
        let top = wave.waveRect.minY + 2
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(0.625), y: top))
        wave.drag(toX: wave.scale.x(0.35), y: top)
        wave.endDrag()
        await assertEventually { self.panel.fadeIn == 0.1 }
        await answered()
        XCTAssertTrue(wave.beginDrag(atX: wave.scale.x(1.375), y: top))
        wave.drag(toX: wave.scale.x(1.55), y: top)
        wave.endDrag()
        await assertEventually { self.panel.fadeOut == 0.2 }
        await answered()
        panel.fadeCurvePopup.selectItem(at: FadeSettingsPanel.curves.firstIndex(of: "s_curve")!)
        fire(panel.fadeCurvePopup)
        await assertEventually { self.panel.fadeCurve == "s_curve" }
        await answered()
        let golden = try XCTUnwrap(
            Fixtures.json("client", "command_set_audio_timing")["command"]?["property"]?["value"])
        let sent = try XCTUnwrap(normalised(modelKind))
        for key in ["start_time", "end_time", "slices", "last_slice_play_count", "fade_in", "fade_out"] {
            XCTAssertEqual(sent[key], normalised(golden[key]), key)
        }
    }

    /// SM-01: in Show Mode no slice or fade control acts.
    func testShowModeLocksSlicesAndFades() async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        wave.click(atX: wave.scale.x(20))
        vc.isLocked = true
        for c in [panel.addSliceButton, panel.deleteSlicesButton, panel.fadeCheck, panel.fadeCurvePopup] as [NSControl]
        {
            XCTAssertFalse(c.isEnabled)
        }
        let sent = kinds.count
        panel.addSlice()
        panel.deleteSlices()
        panel.editSliceCount(0)
        XCTAssertNil(panel.editingSlice)
        XCTAssertFalse(wave.beginDrag(atX: wave.scale.x(10)))
        XCTAssertEqual(kinds.count, sent)
    }

    /// The drawn fade shape is core's documented curve: 0 and 1 at the ends, its known midpoints.
    func testTheDrawnFadeShapeIsCoresCurve() {
        for curve in FadeSettingsPanel.curves {
            XCTAssertEqual(WaveformView.shape(curve, 0), 0, accuracy: 1e-12, curve)
            XCTAssertEqual(WaveformView.shape(curve, 1), 1, accuracy: 1e-12, curve)
        }
        XCTAssertEqual(WaveformView.shape("equal_power", 0.5), sin(Double.pi / 4), accuracy: 1e-12)
        XCTAssertEqual(WaveformView.shape("s_curve", 0.25), (1 - cos(Double.pi / 4)) / 2, accuracy: 1e-12)
        XCTAssertLessThan(WaveformView.shape("exponential", 0.5), 0.05)
        XCTAssertGreaterThan(WaveformView.shape("logarithmic", 0.5), 0.95)
    }
}

// MARK: Show-file helpers (also used by the other inspector and window tests)

/// A 16-bit stereo WAV of `seconds` at 8 kHz: a quiet tone, the right channel at half the left.
func writeWAV(_ url: URL, seconds: Double, rate: Int = 8000) throws {
    let frames = Int((seconds * Double(rate)).rounded())
    var d = Data()
    func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    d.append(contentsOf: Array("RIFF".utf8))
    u32(UInt32(36 + frames * 4))
    d.append(contentsOf: Array("WAVEfmt ".utf8))
    u32(16)
    u16(1)
    u16(2)
    u32(UInt32(rate))
    u32(UInt32(rate * 4))
    u16(4)
    u16(16)
    d.append(contentsOf: Array("data".utf8))
    u32(UInt32(frames * 4))
    d.reserveCapacity(d.count + frames * 4)
    for i in 0..<frames {
        let v = Int16(3000 * sin(2 * Double.pi * 440 * Double(i) / Double(rate)))
        u16(UInt16(bitPattern: v))
        u16(UInt16(bitPattern: v / 2))
    }
    try d.write(to: url)
}

/// Rewrites the workspace of the show file at `url`.
func editShow(_ url: URL, _ edit: (inout JSONValue) -> Void) throws {
    var file = try XCTUnwrap(JSONValue.parse(try String(contentsOf: url, encoding: .utf8)))
    var ws = try XCTUnwrap(file["workspace"])
    edit(&ws)
    file.set("workspace", ws)
    try file.text().write(to: url, atomically: true, encoding: .utf8)
}
