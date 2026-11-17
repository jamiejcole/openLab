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
        "testALiveResizeAsksForPeaksOnceWhenItEnds",
        "testTheRulersFirstLabelIsNotUnderTheStartHandle", "testSliceChangesApplyToTheCurrentKind",
        "testTheDrawnFadeShapeIsCoresCurve", "testViewportZoomKeepsTheAnchorAndStaysInTheFile",
        "testViewportPanStopsAtTheFileEnds",
        "testAReadStillOutForAnotherCueDoesNotLandOnTheCueShown",
        "testOneReadIsOutAtATimeAndTheLatestAskIsMade", "testARefusedReadIsAskedAgain",
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
        window?.close()
        rig?.client.stop()
        (window, vc, rig) = (nil, nil, nil)
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

    /// The ruler's first label starts beside the start handle's triangle, wherever the handle is, and a
    /// label that would run under the end handle is left out.
    func testTheRulersFirstLabelIsNotUnderTheStartHandle() {
        let wave = WaveformView(frame: NSRect(x: 0, y: 0, width: 600, height: 120))
        wave.duration = 60
        wave.scale = WaveformScale(start: 0, end: 60, width: 600)
        let width: (String) -> CGFloat = { _ in 42 }
        let first = wave.rulerMarks(labelWidth: width).first
        XCTAssertEqual(first?.text, "00:00.00")
        XCTAssertGreaterThanOrEqual(first?.labelX ?? 0, wave.scale.x(0) + WaveformView.handleWidth, "beside the handle")
        wave.trimStart = 10
        let moved = wave.rulerMarks(labelWidth: width).first { $0.text == "00:10.00" }
        XCTAssertGreaterThanOrEqual(moved?.labelX ?? 0, wave.scale.x(10) + WaveformView.handleWidth)
        let before = wave.rulerMarks(labelWidth: width).first { $0.text == "00:00.00" }
        XCTAssertEqual(before?.labelX ?? 0, 2, "a label clear of the handle stays where it was")
        wave.trimEnd = 22
        let marks = wave.rulerMarks(labelWidth: width)
        XCTAssertNil(marks.first { $0.text == "00:20.00" }?.labelX, "no label under the end handle's triangle")
        XCTAssertNotNil(marks.first { $0.text == "00:20.00" }, "its tick stays")
    }

    // MARK: The tab

    func testTheTabFollowsIOForAudioCuesOnly() {
        vc.setSelection([Self.intro])
        XCTAssertEqual(
            vc.tabView.tabViewItems.compactMap { $0.identifier as? String },
            ["basics", "triggers", "io", "timeLoops", "levels"])
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
        func fail(_ i: Int, _ error: EngineError) { held[i].resume(returning: .failure(error)) }
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

    /// A's read held (a stalled disk), B shown: B waits for it, and only then is asked.
    func testOneReadIsOutAtATimeAndTheLatestAskIsMade() async {
        let reads = HeldReads()
        let (a, b, c) = (TestSnapshots.intro, TestSnapshots.fade, TestSnapshots.door)
        reads.show(a, media: "a.wav", duration: 10)
        reads.show(b, media: "b.wav", duration: 10)
        reads.show(c, media: "c.wav", duration: 10)
        await settle()
        XCTAssertEqual(reads.asked, [a], "one read out for the panel")
        reads.answer(0, cue: a)
        await reads.panel.loading?.value
        await assertEventually { reads.asked == [a, c] }
        XCTAssertNil(reads.panel.waveform.peaks, "A's result is not drawn on C")
        reads.answer(1, cue: c)
        await reads.panel.loading?.value
        XCTAssertEqual(reads.panel.waveform.peaks?.cue, c)
    }

    /// The engine refuses a read when too many wait: the cue still shown is asked again, not marked unavailable.
    func testARefusedReadIsAskedAgain() async {
        let reads = HeldReads()
        let a = TestSnapshots.intro
        reads.show(a, media: "a.wav", duration: 10)
        await settle()
        reads.fail(0, .engine(code: .commandRefused, message: "too many waveforms are waiting to be read"))
        await reads.panel.loading?.value
        await assertEventually { reads.asked == [a, a] }
        reads.answer(1, cue: a)
        await reads.panel.loading?.value
        XCTAssertEqual(reads.panel.waveform.peaks?.cue, a)
        XCTAssertNotEqual(reads.panel.waveform.message, loc("timeLoops.unavailable"))
    }

    /// Arrowing through ten Audio cues leaves the last one's waveform drawn, never "unavailable", with
    /// no more than one read out at a time (the engine queues only a few).
    func testArrowingThroughAudioCuesDrawsTheLastWaveform() async throws {
        let ids = (0..<10).map { String(format: "00000000-0000-0000-0000-0000000002%02d", $0) }
        let many = try await Rig.open("Demo") { show in
            try writeWAV(show.appendingPathComponent("media/a.wav"), seconds: 20)
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue,
                    let first = cues.first
                else { return XCTFail("the demo show has no first cue") }
                for (n, id) in ids.enumerated() {
                    var cue = first
                    cue.set("id", .string(id))
                    cue.set("number", .string("A\(n)"))
                    cues.append(cue)
                }
                lists[0].set("cues", .array(cues))
                ws.set("cue_lists", .array(lists))
            }
        }
        defer { many.client.stop() }
        let inspector = InspectorViewController(store: many.store)
        let box = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        box.isReleasedWhenClosed = false
        box.contentView = inspector.view
        defer { box.orderOut(nil) }
        inspector.setSelection([ModelID(ids[0])])
        inspector.tabView.selectTabViewItem(withIdentifier: "timeLoops")
        inspector.view.layoutSubtreeIfNeeded()
        let panel = inspector.timeLoops
        if panel.waveform.bounds.width < 625 { panel.waveform.setFrameSize(NSSize(width: 625, height: 120)) }
        await assertEventually("durations known") {
            ids.allSatisfy { many.store.mediaDurations[ModelID($0)]?.file != nil }
        }
        var out = 0
        var most = 0
        let real = panel.loadPeaks
        panel.loadPeaks = { cue, a, b, n in
            out += 1
            most = max(most, out)
            let r = await real?(cue, a, b, n) ?? .failure(.notConnected)
            out -= 1
            return r
        }
        for id in ids {
            inspector.setSelection([ModelID(id)])
            await Task.yield()
        }
        let last = ModelID(ids[9])
        await assertEventually("the last cue's waveform is drawn") { panel.waveform.peaks?.cue == last }
        XCTAssertNotEqual(panel.waveform.message, loc("timeLoops.unavailable"))
        XCTAssertLessThanOrEqual(most, 1)
    }

    /// Waits for the peaks of the visible stretch, including a read a later zoom queued behind one.
    private func peaksSettled() async {
        repeat { await panel.loading?.value } while panel.reloadPending
        await assertEventually("peaks for the visible range") {
            self.wave.peaks.map { $0.start == self.panel.visible.start && $0.end == self.panel.visible.end } ?? false
        }
    }

    private var span: Double { panel.visible.end - panel.visible.start }

    private func windowPoint(atSeconds t: Double) -> NSPoint {
        wave.convert(NSPoint(x: wave.scale.x(t), y: WaveformView.rulerHeight + 20), to: nil)
    }

    private func pinch(_ m: CGFloat, at p: NSPoint) {
        let e = GestureEvent()
        e.point = p
        e.mag = m
        wave.magnify(with: e)
    }

    private func scroll(
        dx: CGFloat = 0, dy: CGFloat = 0, precise: Bool, flags: NSEvent.ModifierFlags = [], at p: NSPoint
    ) {
        let e = GestureEvent()
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
        guard let rep = try? renderView(wave) else { return nil }
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

    /// Hands `event` to the view under it, as the window does for a window that is key. The test
    /// host is not the active app, so no window here is key and `sendEvent` would take a first
    /// click for activation only (the test below that sends one that way).
    private func deliver(_ event: NSEvent) {
        guard let content = window.contentView,
            let target = content.hitTest(content.convert(event.locationInWindow, from: nil))
        else { return XCTFail("nothing under the pointer") }
        switch event.type {
        case .leftMouseDown: target.mouseDown(with: event)
        case .leftMouseDragged: target.mouseDragged(with: event)
        default: target.mouseUp(with: event)
        }
    }

    private func mouse(_ type: NSEvent.EventType, atSeconds t: Double) -> NSEvent {
        NSEvent.mouseEvent(
            with: type, location: windowPoint(atSeconds: t), modifierFlags: [], timestamp: 0,
            windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
    }

    private func click(atSeconds t: Double) {
        deliver(mouse(.leftMouseDown, atSeconds: t))
        deliver(mouse(.leftMouseUp, atSeconds: t))
    }

    /// A click that only brings the window forward moves no handle and places no insertion point.
    func testAClickThatBringsTheWindowForwardMovesNoHandle() async {
        await showIntro()
        XCTAssertFalse(window.isKeyWindow, "the test host is not active, so this click only activates")
        window.sendEvent(mouse(.leftMouseDown, atSeconds: 0.2))
        window.sendEvent(mouse(.leftMouseDragged, atSeconds: 10))
        window.sendEvent(mouse(.leftMouseUp, atSeconds: 10))
        XCTAssertNil(wave.dragging)
        XCTAssertEqual(panel.startField.stringValue, "00:00.00")
        XCTAssertTrue(kinds.isEmpty)
        XCTAssertNil(wave.insertion)
    }

    /// A real mouse-down on a trim handle still takes it, and the drag moves the trim.
    func testAHandleStillDragsAfterTheWaveformTakesFocus() async {
        await showIntro()
        deliver(mouse(.leftMouseDown, atSeconds: 0.2))
        XCTAssertTrue(window.firstResponder === wave)
        XCTAssertEqual(wave.dragging, .start)
        deliver(mouse(.leftMouseDragged, atSeconds: 10))
        deliver(mouse(.leftMouseUp, atSeconds: 10))
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

    /// Zoomed in and scrolled to either end of the file while playing, each line just past a visible
    /// edge (a playhead, a slice marker, a trim: 0.15 s outside, so its x falls on the Start/End
    /// fields or the controls beside the waveform) is not drawn outside the waveform: with the one
    /// line set, every pixel outside the waveform renders as it does with no line.
    func testNoLineIsDrawnOutsideTheWaveformWhenZoomedScrolledAndPlaying() async throws {
        await showIntro()
        type(panel.startField, "20")
        await assertEventually { self.panel.startTime == 20 }
        type(panel.endField, "50")
        await assertEventually { self.panel.endTime == 50 }
        await answered()
        panel.showProgress(position: 23)  // playing
        let width = wave.bounds.width
        for _ in 0..<5 { pinch(0.5, at: windowPoint(atSeconds: 22)) }
        await peaksSettled()
        let host: NSView = vc.view
        let waveFrame = { host.convert(self.wave.bounds, from: self.wave) }
        let duration = try XCTUnwrap(wave.duration)
        // The lines as the panel set them, put back after each case.
        let (start, end, marks, head) = (wave.trimStart, wave.trimEnd, wave.sliceMarks, wave.playhead)
        func baseline() { (wave.trimStart, wave.trimEnd, wave.sliceMarks, wave.playhead) = (0, duration, [], nil) }
        defer { (wave.trimStart, wave.trimEnd, wave.sliceMarks, wave.playhead) = (start, end, marks, head) }

        /// Pixels outside the waveform that differ between two renders of the whole inspector.
        func stray(_ a: NSBitmapImageRep, _ b: NSBitmapImageRep) -> Int {
            let frame = waveFrame()
            let scale = CGFloat(a.pixelsWide) / host.bounds.width
            var count = 0
            for py in 0..<a.pixelsHigh {
                for px in 0..<a.pixelsWide {
                    let down = (CGFloat(py) + 0.5) / scale
                    let at = NSPoint(
                        x: (CGFloat(px) + 0.5) / scale, y: host.isFlipped ? down : host.bounds.height - down)
                    if frame.contains(at) { continue }
                    if a.colorAt(x: px, y: py) != b.colorAt(x: px, y: py) { count += 1 }
                }
            }
            return count
        }

        for direction: CGFloat in [-1, 1] {
            scroll(dx: direction * 100 * width, precise: true, at: windowPoint(atSeconds: panel.visible.start + 1))
            await peaksSettled()
            let (lo, hi) = panel.visible
            let sides: [(String, Double)] = [("left", lo - 0.15), ("right", hi + 0.15)]
            baseline()
            let empty = try renderView(host)
            for (side, t) in sides where t >= 0 && t <= duration {
                let cases: [(String, () -> Void)] = [
                    ("playhead", { self.wave.playhead = t }),
                    ("slice marker", { self.wave.sliceMarks = [t] }),
                    ("start trim", { self.wave.trimStart = t }),
                    ("end trim", { self.wave.trimEnd = t }),
                ]
                for (name, apply) in cases {
                    baseline()
                    apply()
                    // The line's x falls between the waveform and the edge of what is rendered.
                    let x = host.convert(NSPoint(x: wave.scale.x(t), y: 0), from: wave).x
                    XCTAssertTrue(
                        x >= 0 && x <= host.bounds.width
                            && !waveFrame().insetBy(dx: -0.5, dy: 0).contains(NSPoint(x: x, y: waveFrame().midY)),
                        "the \(name) \(side) of the view is drawn off the rendered area at x \(x)")
                    XCTAssertEqual(
                        stray(try renderView(host), empty), 0,
                        "the \(name) \(side) of the visible span, scrolled \(direction), is drawn outside the waveform")
                }
            }
        }
    }

    /// With slices (the left column then also carries a note), its Start, End, play count and Loop
    /// all sit inside the inspector, under the tab strip, level with the waveform.
    func testTheLeftColumnSitsInsideTheInspector() async throws {
        await showIntro()
        await addSlice(at: 30, expect: 1)
        XCTAssertFalse(panel.slicesNote.isHidden)
        window.setContentSize(NSSize(width: 900, height: 190))  // a short inspector
        vc.view.layoutSubtreeIfNeeded()
        let controls: [(String, NSView)] = [
            ("Start", panel.startField), ("End", panel.endField), ("Play count", panel.countField),
            ("Loop", panel.loopRadio),
        ]
        for (name, control) in controls {
            let frame = control.convert(control.bounds, to: vc.view)
            XCTAssertTrue(vc.view.bounds.contains(frame), "\(name) at \(frame) is outside \(vc.view.bounds)")
        }
        let top = panel.startField.convert(panel.startField.bounds, to: vc.view).maxY
        let waveTop = wave.convert(wave.bounds, to: vc.view).maxY
        XCTAssertEqual(top, waveTop, accuracy: 12, "Start is level with the top of the waveform")
        // One place for the column, not several the layout may pick between: with more than one, Start
        // was now and then laid out above the inspector after a resize. With the note, and without it.
        let column = try XCTUnwrap(panel.startField.superview?.superview)
        XCTAssertFalse(column.hasAmbiguousLayout, "the left column has one layout with the slices note")
        panel.deleteSlicesButton.performClick(nil)
        await assertEventually { self.panel.slicesNote.isHidden }
        vc.view.layoutSubtreeIfNeeded()
        XCTAssertFalse(column.hasAmbiguousLayout, "the left column has one layout without the note")
        let frame = panel.startField.convert(panel.startField.bounds, to: vc.view)
        XCTAssertTrue(vc.view.bounds.contains(frame), "Start at \(frame) is inside \(vc.view.bounds)")
    }

    /// With the view at the file's start and end the two trim handles sit on the waveform's edges
    /// and are drawn whole there: the outermost pixel columns hold the orange line, not half of it.
    func testTrimHandlesAtTheEdgesAreDrawnWhole() async throws {
        await showIntro()
        XCTAssertEqual(panel.startTime, 0)
        XCTAssertEqual(wave.scale.x(wave.endTime), wave.bounds.width, accuracy: 0.5)
        let rep = try renderView(wave)
        let y = Int((WaveformView.rulerHeight + 60) * CGFloat(rep.pixelsHigh) / wave.bounds.height)
        for px in [0, 1, rep.pixelsWide - 2, rep.pixelsWide - 1] {
            let c = try XCTUnwrap(rep.colorAt(x: px, y: y)?.usingColorSpace(.sRGB))
            XCTAssertGreaterThan(c.redComponent - c.blueComponent, 0.5, "pixel column \(px) of the edge handle")
        }
    }

    /// Slice 1 (0 to 10 s) of the 62.5 s file plays three times, then the rest once; the cue is
    /// started on the real engine.
    private func startLoopingSlice(count: Int = 3) async {
        await showIntro()
        await addSlice(at: 10, expect: 1)
        panel.editSliceCount(0)
        type(panel.sliceCountField, count == 0 ? "\u{221E}" : String(count))
        await assertEventually { self.panel.sliceMarks.first?.playCount == SlicePlay(integerLiteral: count) }
        await answered()
        // The click that placed the marker loaded the cue there: a click at the start loads it from
        // the beginning again.
        wave.click(atX: wave.scale.x(0))
        await assertEventually("the load at the start went out first") {
            self.rig.wire.sent.contains {
                if case .transport(_, .seek(_, 0)) = $0 { return true } else { return false }
            }
        }
        _ = await rig.store.transport(.start(cue: Self.intro))
    }

    /// Plays on `seconds` and returns where the line is drawn (a position in the file).
    private func playhead(after seconds: Double) async throws -> Double? {
        try await rig.advance(seconds)
        await assertEventually("progress shown") { self.rig.store.progress[Self.intro] != nil }
        return wave.playhead
    }

    /// A sliced cue with a looping slice, played past the end of that slice: the playhead returns
    /// to the slice's start each pass and stays within it, as the engine reports the file position.
    func testALoopingSlicesPlayheadReturnsToItsStartEachPass() async throws {
        await startLoopingSlice()
        var seen: [Double] = []
        for _ in 0..<27 {
            guard let p = try await playhead(after: 1) else { return XCTFail("no playhead while playing") }
            seen.append(p)
            XCTAssertLessThanOrEqual(p, 10.2, "stays within the slice while it loops: \(seen)")
        }
        let wraps = zip(seen, seen.dropFirst()).filter { $1 < $0 }.count
        XCTAssertEqual(wraps, 2, "returned to the slice start on each new pass: \(seen)")
        let later = try await playhead(after: 5)
        XCTAssertEqual(later ?? 0, 10 + 2, accuracy: 1.0, "on into the rest of the file")
    }

    /// A cue stopped draws no playhead line.
    func testAStoppedCueDrawsNoPlayhead() async throws {
        await startLoopingSlice()
        let playing = try await playhead(after: 4)
        XCTAssertNotNil(playing)
        _ = await rig.store.transport(.hardStop(cue: Self.intro))
        try await rig.advance(1)
        await assertEventually("progress ended") { self.rig.store.progress[Self.intro] == nil }
        XCTAssertNil(wave.playhead, "the line is gone once the cue stopped")
    }

    /// Esc (the engine's panic) while a slice loops: the cue fades out, and once the fade has
    /// ended the Time & Loops waveform draws no playhead line.
    func testAfterEscTheWaveformDrawsNoPlayhead() async throws {
        await startLoopingSlice()
        let playing = try await playhead(after: 4)
        XCTAssertNotNil(playing)
        _ = await rig.store.transport(.panic)
        try await rig.advance(0.1)
        try await rig.advance(10)
        await assertEventually("progress ended") { self.rig.store.progress[Self.intro] == nil }
        XCTAssertNil(wave.playhead, "no line once the panic fade is over")
    }

    /// Devamped while looping slice 1: the playhead has stayed within that slice, then moves on past
    /// its end into the next slice and keeps rising.
    func testDevampingMovesThePlayheadToTheNextSlice() async throws {
        await startLoopingSlice(count: 0)
        let devampID: ModelID = "00000000-0000-0000-0000-0000000000d1"
        var devamp = try XCTUnwrap(NewCueKind.devamp.cueJSON(id: devampID, patch: nil))
        var kind = try XCTUnwrap(devamp["kind"])
        kind.set("target", .string(Self.intro.raw))
        kind.set("devamp_mode", .string("current_slice"))
        devamp.set("kind", kind)
        let added = await rig.store.send(
            command: .addCue(devamp, list: TestSnapshots.mainList, parent: nil, index: 1))
        guard case .success = added else { return XCTFail("\(added)") }
        for _ in 0..<12 {
            let p = try await playhead(after: 1)
            XCTAssertLessThanOrEqual(p ?? 99, 10.2, "looping inside slice 1 before the devamp")
        }
        _ = await rig.store.transport(.start(cue: devampID))
        var seen: [Double] = []
        for _ in 0..<12 {
            if let p = try await playhead(after: 1) { seen.append(p) }
        }
        XCTAssertGreaterThan(seen.last ?? 0, 10.5, "moved on past slice 1: \(seen)")
        let moved = try XCTUnwrap(seen.firstIndex { $0 > 10.2 })
        let rest = Array(seen[moved...])
        XCTAssertEqual(rest, rest.sorted(), "rises once it has moved on: \(seen)")
    }

    /// The line is drawn where the engine says it is.
    func testThePlayheadIsDrawnAtTheReportedPosition() async {
        await showIntro()
        panel.showProgress(position: 13)
        XCTAssertEqual(wave.playhead, 13)
        panel.showProgress(position: nil)
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
        // A skipped slice and one looping until devamped are written apart and read back apart.
        let skipped = TimeLoopsPanel.apply(
            .playCount(.endless), to: TimeLoopsPanel.apply(.sliceCount(10, .skip), to: base))
        XCTAssertEqual(skipped["slices"]?.arrayValue?.map { $0["play_count"] }, [.string("skip"), .int(1)])
        XCTAssertEqual(skipped["last_slice_play_count"], .int(0))
        XCTAssertEqual(TimeLoopsPanel.marks(of: skipped).map(\.playCount), [.skip, .times(1)])
        XCTAssertEqual(SlicePlay(json: skipped["last_slice_play_count"]), .endless)
        for bad in [JSONValue.string("loop"), .int(-1), .double(1.5), .null, .bool(true)] {
            XCTAssertNil(SlicePlay(json: bad), "\(bad)")
        }
        XCTAssertEqual(
            ["3", "0", "x", "\u{221E}"].map(SlicePlay.init(typed:)), [.times(3), .skip, .endless, .endless])
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
        // 0 skips the slice: stored apart from the loop's 0, and offered back as 0.
        panel.editSliceCount(0)
        type(panel.sliceCountField, "0")
        await assertEventually { self.kinds.last?["slices"]?.arrayValue?.first?["play_count"] == .string("skip") }
        await assertEventually { self.wave.sliceCounts == [.skip, .endless] }
        panel.editSliceCount(0)
        XCTAssertEqual(panel.sliceCountField.stringValue, "0")
        await answered()
        let sent = kinds.count
        for bad in ["-1", "1.5", "1000001", "?", ""] {
            panel.editSliceCount(0)
            vc.messageLabel.stringValue = ""
            type(panel.sliceCountField, bad)
            XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.sliceCount"), bad)
        }
        XCTAssertEqual(kinds.count, sent, "a value that is no count changes nothing")
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

/// A 16-bit WAV of `seconds` at 8 kHz with `channels` channels (stereo by default): a quiet tone, each channel at half the one before.
func writeWAV(_ url: URL, seconds: Double, rate: Int = 8000, channels: Int = 2) throws {
    let frames = Int((seconds * Double(rate)).rounded())
    var d = Data()
    func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    d.append(contentsOf: Array("RIFF".utf8))
    u32(UInt32(36 + frames * 2 * channels))
    d.append(contentsOf: Array("WAVEfmt ".utf8))
    u32(16)
    u16(1)
    u16(UInt16(channels))
    u32(UInt32(rate))
    u32(UInt32(rate * 2 * channels))
    u16(UInt16(2 * channels))
    u16(16)
    d.append(contentsOf: Array("data".utf8))
    u32(UInt32(frames * 2 * channels))
    d.reserveCapacity(d.count + frames * 2 * channels)
    for i in 0..<frames {
        let v = Int16(3000 * sin(2 * Double.pi * 440 * Double(i) / Double(rate)))
        for c in 0..<channels { u16(UInt16(bitPattern: v / Int16(1 << min(c, 14)))) }
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

// MARK: Clicks and sections in the waveform

/// A click in the Time & Loops waveform moves where an Audio cue plays, and a drag selects a section
/// (#711's flows), done as the operator does them in a workspace window on the real engine: real
/// mouse events on the waveform view, keys through the app's key handling. What is checked is what
/// the window shows: the position line, the shaded section, the row's state mark, the play line and
/// the cue's times as the engine reports them, the trims and slice markers.
extension PlayheadFlowTests {
    private var loops: TimeLoopsPanel { wc.inspector.timeLoops }
    private var waveView: WaveformView { loops.waveform }

    /// Audio cues of a `seconds` tone each, added after the last row; probed unless `probed` is false.
    private func waveTones(_ count: Int, seconds: Double = 30, probed: Bool = true) async throws -> [ModelID] {
        let dir = try XCTUnwrap(try tempFiles(["x.txt"]).first).deletingLastPathComponent()
        var urls: [URL] = []
        for i in 0..<count {
            let url = dir.appendingPathComponent("wave-\(UUID().uuidString)-\(i).wav")
            try writeWAV(url, seconds: seconds)
            urls.append(url)
        }
        let ids = await wc.editor.addFiles(urls, parent: nil, index: wc.cueList.roots.count)
        XCTAssertEqual(ids.count, count)
        if probed {
            await assertEventually("probed") {
                try? await self.play(0.01)
                return ids.allSatisfy { self.store.mediaDurations[$0]?.file != nil }
            }
        }
        return ids
    }

    /// `id` selected and its Time & Loops tab shown; with `drawn`, once its waveform is drawn.
    private func showWaveform(_ id: ModelID, drawn: Bool = true) async {
        wc.cueList.selectAsUser([id])
        await assertEventually("the inspector shows it") { self.wc.inspector.selection == [id] }
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "timeLoops")
        wc.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertGreaterThan(waveView.bounds.width, 300, "the waveform is laid out")
        if drawn { await waveformDrawn() }
    }

    private func waveformDrawn() async {
        repeat { await loops.loading?.value } while loops.reloadPending
        await assertEventually("the waveform is drawn") {
            self.waveView.peaks.map { $0.start == self.loops.visible.start && $0.end == self.loops.visible.end }
                ?? false
        }
    }

    private func waveX(_ t: Double) -> CGFloat { waveView.scale.x(t) }

    private func waveMouse(_ type: NSEvent.EventType, x: CGFloat, y: CGFloat? = nil, clicks: Int = 1) -> NSEvent {
        let p = waveView.convert(NSPoint(x: x, y: y ?? WaveformView.rulerHeight + 30), to: nil)
        return NSEvent.mouseEvent(
            with: type, location: p, modifierFlags: [], timestamp: 0, windowNumber: wc.window?.windowNumber ?? 0,
            context: nil, eventNumber: 0, clickCount: clicks, pressure: 1)!
    }

    /// A press at `x` (view points; `y` from the view's top) handed to what the window has under it,
    /// which must be the waveform.
    private func pressWave(x: CGFloat, y: CGFloat? = nil, clicks: Int = 1) {
        let e = waveMouse(.leftMouseDown, x: x, y: y, clicks: clicks)
        guard let content = wc.window?.contentView,
            let target = content.hitTest(content.convert(e.locationInWindow, from: nil))
        else { return XCTFail("nothing under the pointer") }
        XCTAssertTrue(target === waveView, "the press lands on the waveform, not on \(target)")
        target.mouseDown(with: e)
    }

    private func clickWave(at t: Double) {
        pressWave(x: waveX(t))
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: waveX(t)))
    }

    /// A press at `fromX`, moved through each of `xs` and let go at the last (view points): the
    /// moves and the release go to the waveform, as AppKit sends a drag to the view it began on.
    private func dragWave(fromX: CGFloat, through xs: [CGFloat], y: CGFloat? = nil) {
        pressWave(x: fromX, y: y)
        for x in xs { waveView.mouseDragged(with: waveMouse(.leftMouseDragged, x: x, y: y)) }
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: xs.last ?? fromX, y: y))
    }

    private func dragWave(from a: Double, to b: Double) {
        dragWave(fromX: waveX(a), through: [waveX((a + b) / 2), waveX(b)])
    }

    /// A key as the app's key monitor gets it; what it passes on goes to the window.
    private func waveKey(_ chars: String, code: UInt16, _ flags: NSEvent.ModifierFlags = []) throws {
        let e = try key(chars, code: code, flags, windowNumber: wc.window?.windowNumber ?? 0)
        if let passed = app.handleKeyDown(e) { wc.window?.sendEvent(passed) }
    }

    private func shiftI() throws { try waveKey("I", code: 34, .shift) }
    private func shiftO() throws { try waveKey("O", code: 31, .shift) }
    private func keyM() throws { try waveKey("m", code: 46) }

    /// Space (GO) from `id`'s waveform, then a tenth of a second. GO moves the selection on, so `id` is
    /// selected again, as the operator does to see its waveform.
    private func goFromWaveform(_ id: ModelID) async throws {
        let before = goes()
        try waveKey(" ", code: 49)
        await assertEventually("GO was sent") { self.goes() == before + 1 }
        await sync()
        try await play(0.1)
        await showWaveform(id)
    }

    /// The positions the app asked cues to play from, in order.
    private var seeks: [Double] {
        wire.sent.compactMap { if case .transport(_, .seek(_, let p)) = $0 { return p } else { return nil } }
    }

    /// The `count`th seek has gone out and the engine has run it.
    private func seekRun(_ count: Int) async {
        await assertEventually("the seek went out") { self.seeks.count == count }
        await sync()
    }

    /// The state mark drawn in `id`'s row, as its label; nil when it shows none.
    private func rowMark(_ id: ModelID) -> String? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier("status"))
        guard let cell = ov.view(atColumn: col, row: ov.row(forItem: node), makeIfNecessary: true) as? CueCell,
            !cell.images[1].isHidden
        else { return nil }
        return cell.images[1].accessibilityLabel()
    }

    /// The engine reports `id` playing within `tolerance` of `expected` seconds into its file, and the
    /// play line is drawn there.
    private func playsAt(
        _ id: ModelID, _ expected: Double, tolerance: Double = 0.3, file: StaticString = #filePath,
        line: UInt = #line
    ) async {
        await assertEventually("plays at \(expected) s", file: file, line: line) {
            guard let p = self.store.progress[id]?.position else { return false }
            return abs(p - expected) <= tolerance
        }
        XCTAssertEqual(
            waveView.playhead, store.progress[id]?.position, "the play line is drawn there", file: file, line: line)
    }

    /// The columns (view points) of the drawn waveform, along the top of the waves, whose colour
    /// `matches`.
    private func drawnColumns(_ matches: (NSColor) -> Bool) throws -> [CGFloat] {
        let rep = try renderView(waveView)
        let s = CGFloat(rep.pixelsWide) / waveView.bounds.width
        let y = Int((WaveformView.rulerHeight + 4) * s)
        return (0..<rep.pixelsWide).compactMap { px in
            guard let c = rep.colorAt(x: px, y: y)?.usingColorSpace(.sRGB), matches(c) else { return nil }
            return (CGFloat(px) + 0.5) / s
        }
    }

    /// The white position line is drawn at `x`.
    private func lineDrawn(at x: CGFloat) throws -> Bool {
        try drawnColumns { min($0.redComponent, $0.greenComponent, $0.blueComponent) > 0.4 }.contains {
            abs($0 - x) <= 1
        }
    }

    /// The span of the waves drawn with the section's light wash, or nil.
    private func shadedSpan() throws -> ClosedRange<CGFloat>? {
        // The wash over the dark waves is a pale blue-grey: blue above red, unlike a line's edge.
        let shaded = try drawnColumns {
            (0.1..<0.4).contains($0.redComponent) && $0.blueComponent < 0.4 && $0.blueComponent > $0.redComponent
        }
        guard let a = shaded.min(), let b = shaded.max() else { return nil }
        return a...b
    }

    /// The trims typed in their fields.
    private func typeWaveTrims(start: String?, end: String?) async {
        for (field, text) in [(loops.startField, start), (loops.endField, end)] {
            guard let text else { continue }
            field.stringValue = text
            loops.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
        }
        await assertEventually("the trims are set") { self.wc.inspector.pendingKinds.isEmpty }
    }

    /// Slice `index`'s play count typed over its drawn number after a double-click there.
    private func typeSliceCount(_ index: Int, _ text: String) async {
        let r = waveView.countRect(index)
        pressWave(x: r.midX, y: r.midY, clicks: 1)
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: r.midX, y: r.midY))
        pressWave(x: r.midX, y: r.midY, clicks: 2)
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: r.midX, y: r.midY, clicks: 2))
        XCTAssertFalse(loops.sliceCountField.isHidden, "the count opens for typing")
        loops.sliceCountField.stringValue = text
        loops.controlTextDidEndEditing(
            Notification(name: NSControl.textDidEndEditingNotification, object: loops.sliceCountField))
        await assertEventually("the count is taken") { self.wc.inspector.pendingKinds.isEmpty }
    }

    private var runningMark: String { loc("cue.state.running") }

    // MARK: Flows

    func testIdleCueClickShowsTheLineAndTheCueLoadedAndGoPlaysFromThere() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        XCTAssertNil(rowMark(id), "idle")
        clickWave(at: 12)
        XCTAssertEqual(waveView.insertion ?? -1, 12, accuracy: 0.05, "the line is where the click was")
        XCTAssertTrue(try lineDrawn(at: waveX(12)), "and drawn there")
        await seekRun(1)
        XCTAssertEqual(seeks.last ?? -1, 12, accuracy: 0.05)
        await assertEventually("the row shows the cue loaded") { self.rowMark(id) == loc("cue.state.loaded") }
        XCTAssertTrue(wc.window?.firstResponder === waveView, "the waveform has the focus; GO still works")
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(0.4)
        await playsAt(id, 12.5)
        XCTAssertEqual(rowMark(id), runningMark)
    }

    func testACueLoadedToATimeByALoadCueShowsThePositionLineThere() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        XCTAssertNil(waveView.playhead, "nothing is loaded yet")
        let load: ModelID = "00000000-0000-0000-0000-0000000000d3"
        var cue = try XCTUnwrap(NewCueKind.load.cueJSON(id: load, patch: nil))
        var kind = try XCTUnwrap(cue["kind"])
        kind.set("target", .string(id.raw))
        kind.set("load_to_time", .double(7))
        cue.set("kind", kind)
        let lists = try XCTUnwrap(store.model?.cueLists.first)
        let added = await store.send(command: .addCue(cue, list: lists.id, parent: nil, index: lists.cues.count))
        guard case .success = added else { return XCTFail("add the Load cue") }
        await assertEventually("the Load cue is in") { self.store.model?.cue(load) != nil }
        await app.perform(.startSelected, on: wc, cues: [load])
        await assertEventually("the cue is loaded") { self.rowMark(id) == loc("cue.state.loaded") }
        await assertEventually("the position line is at the loaded time") {
            try? await self.play(0.01)
            return abs((self.waveView.playhead ?? -1) - 7) < 0.05
        }
        XCTAssertTrue(try lineDrawn(at: waveX(7)) || waveView.playhead != nil, "and drawn there")
        try await goFromWaveform(id)
        try await play(0.4)
        await playsAt(id, 7.4)
    }

    func testPlayingOrPreviewingCueClickJumpsThereAndPlaysOnWithItsTimesFromThere() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(1.9)
        await playsAt(id, 2)
        clickWave(at: 20)
        await seekRun(1)
        XCTAssertEqual(rowMark(id), runningMark, "no stop")
        try await play(0.5)
        await playsAt(id, 20.5)
        XCTAssertEqual(rowMark(id), runningMark)
        let times = try XCTUnwrap(store.progress[id])
        XCTAssertEqual(times.elapsed, 20.5, accuracy: 0.3, "the elapsed time goes on from the clicked point")
        XCTAssertEqual(times.remaining ?? -1, 9.5, accuracy: 0.3)
        try await play(1)
        await playsAt(id, 21.5)
        // A preview moved by a click plays on: it is not ended.
        _ = await store.transport(.hardStop(cue: id))
        try await play(0.2)
        await assertEventually("stopped") { self.rowMark(id) == nil }
        try waveKey("v", code: 9)
        await assertEventually("previewing") { self.store.previewing.contains(id) }
        try await play(1)
        clickWave(at: 15)
        await seekRun(2)
        try await play(1)
        XCTAssertTrue(store.previewing.contains(id), "the preview goes on")
    }

    func testPausedCueClickMovesThePlayLineAndResumePlaysFromThere() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(1.9)
        try waveKey("p", code: 35)
        await assertEventually("paused") { self.rowMark(id) == loc("cue.state.paused") }
        clickWave(at: 20)
        await seekRun(1)
        // Progress goes out as device time moves; the virtual clock stands still until told.
        try await play(0.1)
        await playsAt(id, 20, tolerance: 0.05)
        try await play(1)
        await playsAt(id, 20, tolerance: 0.05)
        XCTAssertEqual(rowMark(id), loc("cue.state.paused"), "it stays paused")
        try waveKey("p", code: 35)
        try await play(0.5)
        await playsAt(id, 20.5)
        XCTAssertEqual(rowMark(id), runningMark)
    }

    func testLoopingSliceClickInALaterSliceOrInsideTheLoopPlaysFromThereAndTheLoopStillLoops() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        clickWave(at: 10)
        try keyM()
        await assertEventually("a marker at 10 s") { self.waveView.sliceMarks == [10] }
        await typeSliceCount(0, "\u{221E}")
        XCTAssertEqual(loops.sliceMarks.first?.playCount, 0, "slice 1 loops until devamped")
        clickWave(at: 2)
        await seekRun(2)
        try await goFromWaveform(id)
        try await play(0.9)
        await playsAt(id, 3)
        clickWave(at: 4)
        await seekRun(3)
        try await play(3)
        await playsAt(id, 7)
        try await play(5)
        await playsAt(id, 2, tolerance: 0.4)
        try await play(10)
        await playsAt(id, 2, tolerance: 0.4)
        clickWave(at: 15)
        await seekRun(4)
        try await play(1)
        await playsAt(id, 16)
        XCTAssertEqual(rowMark(id), runningMark)
    }

    func testSlicedCueClickInsideACountThreeSlicePlaysThatPassThenTwoMore() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        clickWave(at: 10)
        try keyM()
        await assertEventually("a marker at 10 s") { self.waveView.sliceMarks == [10] }
        await typeSliceCount(0, "3")
        clickWave(at: 0.5)
        await seekRun(2)
        try await goFromWaveform(id)
        try await play(0.9)
        clickWave(at: 6)
        await seekRun(3)
        try await play(3)
        await playsAt(id, 9, tolerance: 0.4)
        try await play(2)
        await playsAt(id, 1, tolerance: 0.4)
        try await play(10)
        await playsAt(id, 1, tolerance: 0.4)
        try await play(10)
        await playsAt(id, 11, tolerance: 0.4)
    }

    func testClickInTheDimmedPartsPlacesTheLineThereAndLoadsOrJumpsToTheNearestTrim() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        await typeWaveTrims(start: "5", end: "20")
        clickWave(at: 2)
        XCTAssertEqual(waveView.insertion ?? -1, 2, accuracy: 0.05, "the line goes where clicked")
        XCTAssertTrue(try lineDrawn(at: waveX(2)))
        await seekRun(1)
        await assertEventually("loaded") { self.rowMark(id) == loc("cue.state.loaded") }
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(0.4)
        await playsAt(id, 5.5)
        clickWave(at: 25)
        XCTAssertEqual(waveView.insertion ?? -1, 25, accuracy: 0.05)
        await seekRun(2)
        try await play(0.5)
        await assertEventually("nothing is left past the end time: the cue ends") { self.rowMark(id) == nil }
    }

    func testPreWaitClickEndsTheWaitAndPostWaitClickMovesOnlyTheSound() async throws {
        let ids = try await waveTones(3)
        let (waiting, posting, next) = (ids[0], ids[1], ids[2])
        _ = await store.setProperty(waiting, .preWait(5))
        _ = await store.setProperty(posting, .postWait(3))
        _ = await store.setProperty(posting, .continueMode("auto_continue"))
        await sync()
        await showWaveform(waiting)
        _ = await store.transport(.start(cue: waiting))
        try await play(1)
        await assertEventually("pre-waiting") { self.rowMark(waiting) == loc("cue.state.preWaiting") }
        clickWave(at: 12)
        await seekRun(1)
        await assertEventually("the pre-wait ended: it plays") { self.rowMark(waiting) == self.runningMark }
        try await play(0.5)
        await playsAt(waiting, 12.5)

        await showWaveform(posting)
        _ = await store.transport(.start(cue: posting))
        try await play(1)
        clickWave(at: 20)
        await seekRun(2)
        try await play(0.5)
        await playsAt(posting, 20.5)
        try await play(1.2)
        XCTAssertNil(rowMark(next), "the post-wait still counts from the start: not yet at 2.7 s")
        try await play(0.5)
        await assertEventually("the auto-continue fired at 3 s") { self.rowMark(next) == self.runningMark }
        XCTAssertLessThan(store.progress[next]?.elapsed ?? 99, 0.5, "started once, just now")
        XCTAssertEqual(rowMark(posting), runningMark, "its sound plays on from the clicked point")
    }

    func testZoomedAndScrolledClickOrDragUsesTheTimeUnderThePointerAndTheViewStays() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        loops.zoom(by: 1.0 / 3, anchor: 0)
        loops.pan(bySeconds: 10)
        await waveformDrawn()
        let shown = loops.visible
        XCTAssertEqual(shown.start, 10, accuracy: 1e-9)
        XCTAssertEqual(shown.end, 20, accuracy: 1e-9)
        clickWave(at: 15)
        XCTAssertEqual(waveView.insertion ?? -1, 15, accuracy: 0.02)
        await seekRun(1)
        XCTAssertEqual(seeks.last ?? -1, 15, accuracy: 0.02)
        await assertEventually("loaded") { self.rowMark(id) == loc("cue.state.loaded") }
        XCTAssertTrue(loops.visible == shown, "no scroll or zoom")
        dragWave(from: 12, to: 14)
        let section = try XCTUnwrap(waveView.selection)
        XCTAssertEqual(section.lowerBound, 12, accuracy: 0.02)
        XCTAssertEqual(section.upperBound, 14, accuracy: 0.02)
        XCTAssertTrue(loops.visible == shown, "no scroll or zoom")
    }

    func testPressOnAHandleOrACountAndDragMovesItAndNotThePlaybackNorASection() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        dragWave(fromX: waveX(0) + 1, through: [waveX(1.5), waveX(3)])
        await assertEventually("the start moved") { self.loops.startTime == 3 }
        dragWave(fromX: waveX(30) - 1, through: [waveX(27), waveX(25)])
        await assertEventually("the end moved") { self.loops.endTime == 25 }
        XCTAssertEqual(seeks, [], "nothing moved the playback")
        XCTAssertNil(waveView.selection)
        XCTAssertNil(waveView.insertion)
        XCTAssertNil(rowMark(id), "not loaded")
        clickWave(at: 10)
        try keyM()
        await assertEventually("a marker at 10 s") { self.waveView.sliceMarks == [10] }
        await seekRun(1)
        let afterMarker = seeks.count
        dragWave(fromX: waveX(10), through: [waveX(11), waveX(12)])
        await assertEventually("the marker moved") { self.loops.sliceMarks.map(\.time) == [12] }
        loops.fadeCheck.performClick(nil)
        await assertEventually("fades on") { self.loops.fadeIn == 1 }
        let band = waveView.waveRect.minY + 2
        dragWave(fromX: waveX(4), through: [waveX(5), waveX(6)], y: band)
        await assertEventually("the fade-in moved") { self.loops.fadeIn == 3 }
        let count = waveView.countRect(0)
        dragWave(fromX: count.midX, through: [count.midX + 40], y: count.midY)
        XCTAssertEqual(seeks.count, afterMarker, "no handle, marker, fade or count press moved the playback")
        XCTAssertNil(waveView.selection, "nor selected a section")
        await typeSliceCount(0, "2")
        XCTAssertEqual(loops.sliceMarks.first?.playCount, 2, "the count is edited as before")
        XCTAssertEqual(seeks.count, afterMarker)
    }

    func testDragAcrossTheWaveformShadesTheSectionAndDoesNotMoveThePlayback() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(1.9)
        dragWave(from: 20, to: 10)
        let section = try XCTUnwrap(waveView.selection, "dragged right to left")
        XCTAssertEqual(section.lowerBound, 10, accuracy: 0.05)
        XCTAssertEqual(section.upperBound, 20, accuracy: 0.05)
        let shaded = try XCTUnwrap(try shadedSpan(), "the section is shaded")
        XCTAssertEqual(shaded.lowerBound, waveX(10), accuracy: 1.5)
        XCTAssertEqual(shaded.upperBound, waveX(20), accuracy: 1.5)
        XCTAssertEqual(seeks, [], "a drag sends no seek")
        try await play(0.5)
        await playsAt(id, 2.5)
        dragWave(from: 4, to: 8)
        XCTAssertEqual(waveView.selection?.lowerBound ?? -1, 4, accuracy: 0.05, "left to right")
        XCTAssertEqual(waveView.selection?.upperBound ?? -1, 8, accuracy: 0.05)

        // Zoomed to the first 10 s, a drag past the right edge scrolls on and stops at the file's end.
        loops.zoom(by: 1.0 / 3, anchor: 0)
        await waveformDrawn()
        let width = waveView.bounds.width
        // Well past the edge, each move scrolls at most a quarter of the view.
        dragWave(fromX: waveX(5), through: Array(repeating: width * 1.5, count: 30))
        XCTAssertEqual(loops.visible.end, 30, accuracy: 1e-9, "scrolled to the end of the file and stopped")
        XCTAssertEqual(loops.visible.end - loops.visible.start, 10, accuracy: 1e-9, "not zoomed")
        XCTAssertEqual(waveView.selection?.lowerBound ?? -1, 5, accuracy: 0.05)
        XCTAssertEqual(waveView.selection?.upperBound ?? -1, 30, accuracy: 0.001, "to the file's end")
        await waveformDrawn()
        dragWave(fromX: waveX(25), through: Array(repeating: -width / 2, count: 30))
        XCTAssertEqual(loops.visible.start, 0, accuracy: 1e-9, "back to the start and stopped")
        XCTAssertEqual(waveView.selection?.lowerBound ?? -1, 0, accuracy: 0.001)
        XCTAssertEqual(waveView.selection?.upperBound ?? -1, 25, accuracy: 0.05)
        XCTAssertEqual(seeks, [])
        await waveformDrawn()
    }

    func testADragShorterThanFiftyMillisecondsIsAPlainClick() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        dragWave(fromX: waveX(12), through: [waveX(12.02), waveX(12.03)])
        XCTAssertNil(waveView.selection, "too short for a section")
        XCTAssertEqual(waveView.insertion ?? -1, 12, accuracy: 0.03)
        await seekRun(1)
        await assertEventually("loaded") { self.rowMark(id) == loc("cue.state.loaded") }
    }

    func testWithASectionShiftIAndShiftOCropTheCueToItEachOneUndoStep() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        dragWave(from: 8, to: 16)
        XCTAssertNotNil(waveView.selection)
        try shiftI()
        await assertEventually("the start is the section's left edge") { self.loops.startTime == 8 }
        try shiftO()
        await assertEventually("the end is its right edge") { self.loops.endTime == 16 }
        XCTAssertEqual(loops.startField.stringValue, CueTime.format(8))
        XCTAssertEqual(loops.endField.stringValue, CueTime.format(16))
        XCTAssertEqual(seeks, [], "the keys move no playback")
        XCTAssertNil(rowMark(id))
        await wc.editor.undo()
        await assertEventually("one undo takes the end back") { self.loops.endTime == nil }
        XCTAssertEqual(loops.startTime, 8)
        await wc.editor.undo()
        await assertEventually("the next the start") { self.loops.startTime == 0 }
        // No section: the keys act at the clicked point.
        clickWave(at: 5)
        XCTAssertNil(waveView.selection)
        try shiftI()
        await assertEventually { self.loops.startTime == 5 }
        clickWave(at: 25)
        try shiftO()
        await assertEventually { self.loops.endTime == 25 }
    }

    func testWithASectionAddSliceMarksBothEdgesButNotAnEdgeNearATrimOrMarker() async throws {
        await showWaveform(try await waveTones(1)[0])
        dragWave(from: 8, to: 16)
        try keyM()
        await assertEventually("markers on both edges") { self.loops.sliceMarks.map(\.time) == [8, 16] }
        XCTAssertEqual(loops.sliceMarks.map(\.playCount), [1, 1], "the section between them plays once")
        XCTAssertEqual(waveView.sliceMarks, [8, 16], "drawn")
        await wc.editor.undo()
        await assertEventually("one undo step for both") { self.loops.sliceMarks.isEmpty }
        await wc.editor.redo()
        await assertEventually { self.loops.sliceMarks.count == 2 }
        // An edge within 0.05 s of a marker or of a trim gets none; the button does what M does.
        dragWave(from: 16.03, to: 24)
        XCTAssertTrue(loops.addSliceButton.isEnabled)
        loops.addSliceButton.performClick(nil)
        await assertEventually("only the far edge") { self.loops.sliceMarks.map(\.time) == [8, 16, 24] }
        dragWave(from: 27, to: 30)
        try keyM()
        await assertEventually("not at the end trim") { self.loops.sliceMarks.map(\.time) == [8, 16, 24, 27] }
        XCTAssertEqual(seeks, [])
    }

    func testASectionIsClearedByAClickByAnotherCueOrByANewFileAndNeverCarriesOver() async throws {
        let ids = try await waveTones(2)
        await showWaveform(ids[0])
        dragWave(from: 8, to: 16)
        XCTAssertNotNil(try shadedSpan())
        clickWave(at: 20)
        XCTAssertNil(waveView.selection, "a click elsewhere clears it")
        XCTAssertNil(try shadedSpan())
        XCTAssertEqual(waveView.insertion ?? -1, 20, accuracy: 0.05, "and places the line")
        dragWave(from: 8, to: 16)
        await showWaveform(ids[1])
        XCTAssertNil(waveView.selection, "not on the other cue")
        XCTAssertNil(waveView.insertion)
        await showWaveform(ids[0])
        XCTAssertNil(waveView.selection, "gone when coming back")
        dragWave(from: 8, to: 16)
        let other = try XCTUnwrap(try tempFiles(["x.txt"]).first).deletingLastPathComponent()
            .appendingPathComponent("other.wav")
        try writeWAV(other, seconds: 20)
        _ = await store.setProperty(ids[0], .media(other.path))
        await assertEventually("the new file is shown") {
            try? await self.play(0.01)
            return self.store.mediaDurations[ids[0]]?.file.map { abs($0 - 20) < 0.01 } ?? false
        }
        XCTAssertNil(waveView.selection, "a new file clears it")
    }

    func testFileStillBeingReadTakesNoClickAndABrokenCueGetsOnlyTheLine() async throws {
        let id = try await waveTones(1, probed: false)[0]
        await showWaveform(id, drawn: false)
        XCTAssertEqual(waveView.message, loc("timeLoops.notRead"), "it says the file is being read")
        clickWave(at: 0)
        dragWave(fromX: 40, through: [80, 120])
        XCTAssertNil(waveView.insertion, "a click does nothing")
        XCTAssertNil(waveView.selection, "nor a drag")
        XCTAssertEqual(seeks, [])

        await assertEventually("probed") {
            try? await self.play(0.01)
            return self.store.mediaDurations[id]?.file != nil
        }
        await waveformDrawn()
        guard case .success = await engine.request(.loseDevice()) else { return XCTFail("lose_device") }
        try await play(0.5)
        await assertEventually("broken: no device") { self.rowMark(id) == loc("cue.state.broken") }
        clickWave(at: 12)
        XCTAssertEqual(waveView.insertion ?? -1, 12, accuracy: 0.05, "the line goes there for the keys")
        try await play(0.5)
        XCTAssertEqual(seeks, [], "nothing is asked to load or sound")
        XCTAssertEqual(rowMark(id), loc("cue.state.broken"))
    }

    func testSeveralCuesHaveNoTimeLoopsTabAndShowModeTakesNoClick() async throws {
        let ids = try await waveTones(2)
        wc.cueList.selectAsUser(ids)
        await assertEventually { self.wc.inspector.selection == ids }
        XCTAssertNil(
            wc.inspector.tabView.tabViewItems.first { ($0.identifier as? String) == "timeLoops" }, "no Time & Loops")
        await showWaveform(ids[0])
        wc.setShowMode(true)
        XCTAssertFalse(wc.isVisible(.inspector), "Show Mode hides the inspector")
        waveView.mouseDown(with: waveMouse(.leftMouseDown, x: waveX(12)))
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: waveX(12)))
        waveView.keyDown(with: try key("I", code: 34, .shift, windowNumber: wc.window?.windowNumber ?? 0))
        XCTAssertNil(waveView.insertion)
        XCTAssertEqual(seeks, [])
        XCTAssertEqual(loops.startTime, 0)
        XCTAssertNil(rowMark(ids[0]), "nothing moved or loaded")
    }

    /// A press whose release is lost (Show Mode, a tab switch) moves nothing.
    func testAPressWhoseReleaseIsLostMovesNothing() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        pressWave(x: waveX(12))
        wc.inspector.endGestures()
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: waveX(12)))
        XCTAssertNil(waveView.insertion)
        XCTAssertEqual(seeks, [])
        XCTAssertNil(rowMark(id))
    }

    /// A cue whose file is missing never says it is being read: it says the file is missing, in a
    /// colour that reads on the waves' background in light and dark appearance, and a click there
    /// moves nothing.
    func testAMissingFileSaysSoAndNeverThatItIsBeingRead() async throws {
        let id = try await waveTones(1)[0]
        let set = await store.setProperty(id, .media("/nonexistent-\(UUID().uuidString)/gone.wav"))
        guard case .success = set else { return XCTFail("media: \(set)") }
        await assertEventually("broken: the file is missing") {
            try? await self.play(0.1)
            return self.rowMark(id) == loc("cue.state.broken")
        }
        await showWaveform(id, drawn: false)
        for _ in 0..<6 {
            XCTAssertEqual(waveView.message, loc("timeLoops.missing"))
            try await play(0.5)
        }
        XCTAssertNil(waveView.peaks)
        clickWave(at: 0.5)
        XCTAssertEqual(seeks, [], "nothing to load")
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            try XCTUnwrap(NSAppearance(named: name)).performAsCurrentDrawingAppearance {
                guard let fg = WaveformView.messageColour.usingColorSpace(.sRGB),
                    let bg = WaveformView.waveBackground.usingColorSpace(.sRGB)
                else { return XCTFail("colours") }
                func luminance(_ c: NSColor) -> CGFloat {
                    0.2126 * c.redComponent + 0.7152 * c.greenComponent + 0.0722 * c.blueComponent
                }
                let ratio = (luminance(fg) + 0.05) / (luminance(bg) + 0.05)
                XCTAssertGreaterThan(ratio, 7, "the message reads on the waves in \(name.rawValue)")
            }
        }
    }

    /// With a section shown, the line where the cue is loaded stays drawn beside it.
    func testTheLoadedPositionStaysDrawnBesideASection() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        clickWave(at: 12)
        await seekRun(1)
        dragWave(from: 18, to: 26)
        XCTAssertNotNil(waveView.selection)
        XCTAssertEqual(waveView.insertion ?? -1, 12, accuracy: 0.05)
        XCTAssertTrue(try lineDrawn(at: waveX(12)), "the loaded position is still drawn")
        let shaded = try XCTUnwrap(try shadedSpan())
        XCTAssertEqual(shaded.lowerBound, waveX(18), accuracy: 1.5)
        await assertEventually("loaded") { self.rowMark(id) == loc("cue.state.loaded") }
    }

    /// A press held while another cue comes into the tab (not by a click in the list) sends that
    /// cue nothing when it is let go.
    func testAPressHeldWhileAnotherCueIsShownMovesNeitherCue() async throws {
        let ids = try await waveTones(2)
        await showWaveform(ids[0])
        pressWave(x: waveX(12))
        wc.inspector.setSelection([ids[1]])
        await assertEventually("the other cue is shown") { self.wc.inspector.selection == [ids[1]] }
        waveView.mouseUp(with: waveMouse(.leftMouseUp, x: waveX(12)))
        XCTAssertNil(waveView.insertion)
        try await play(0.2)
        XCTAssertEqual(seeks, [])
        XCTAssertNil(rowMark(ids[1]))
    }

    /// A cue looping until devamped, clicked past its end time: the loop goes on from its start
    /// (the click lands at the end of a pass), the cue never stops.
    func testALoopingCueClickedPastItsEndTimeLoopsOnFromItsStart() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        await typeWaveTrims(start: nil, end: "20")
        loops.loopRadio.performClick(nil)
        await assertEventually("loops until devamped") { self.loops.playCount == 0 }
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(1.9)
        await playsAt(id, 2)
        clickWave(at: 25)
        XCTAssertEqual(waveView.insertion ?? -1, 25, accuracy: 0.05, "the line goes where clicked")
        await seekRun(1)
        try await play(0.5)
        await playsAt(id, 0.5, tolerance: 0.4)
        XCTAssertEqual(rowMark(id), runningMark, "still playing")
        try await play(1)
        await playsAt(id, 1.5, tolerance: 0.4)
        XCTAssertEqual(rowMark(id), runningMark)
    }

    /// With a section, the position line and the play line all shown, the position line is drawn
    /// bright and the section's edges faint, so the line is told apart by its look.
    func testThePositionLineStandsOutFromASectionsEdges() async throws {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        clickWave(at: 12)
        await seekRun(1)
        dragWave(from: 18, to: 26)
        XCTAssertGreaterThanOrEqual(
            WaveformView.positionLine.alphaComponent - WaveformView.sectionEdge.alphaComponent, 0.5)
        XCTAssertTrue(try lineDrawn(at: waveX(12)), "the position line is drawn bright")
        XCTAssertFalse(try lineDrawn(at: waveX(18)), "the section's edge is not")
        XCTAssertFalse(try lineDrawn(at: waveX(26)))
        _ = id
    }

    /// A slice marker added at the position line is drawn over it: its green line stays seen.
    func testASliceMarkerAtThePositionLineIsDrawnOverIt() async throws {
        await showWaveform(try await waveTones(1)[0])
        clickWave(at: 10)
        try keyM()
        await assertEventually("a marker at 10 s") { self.waveView.sliceMarks == [10] }
        XCTAssertEqual(waveView.insertion ?? -1, 10, accuracy: 0.001, "the line is still there")
        let green = try drawnColumns { $0.greenComponent > $0.redComponent + 0.2 && $0.greenComponent > 0.3 }
        XCTAssertTrue(green.contains { abs($0 - self.waveX(10)) <= 1 }, "the marker's green shows")
        XCTAssertFalse(try lineDrawn(at: waveX(10)), "not the white line over it")
    }

    /// A 20 s file sliced at 5 s, ending at 15 s, its last slice played three times (35 s in all),
    /// clicked past its end time while in its first slice: the times shown stay within the cue's
    /// length, elapsed and remaining adding up to it.
    func testAClickPastTheEndTimeOfACountedLastSliceKeepsTheTimesWithinTheCue() async throws {
        let id = try await waveTones(1, seconds: 20)[0]
        await showWaveform(id)
        await typeWaveTrims(start: nil, end: "15")
        clickWave(at: 5)
        try keyM()
        await assertEventually("a marker at 5 s") { self.waveView.sliceMarks == [5] }
        await typeSliceCount(1, "3")
        XCTAssertEqual(loops.playCount, 3)
        // Away from the start handle: loaded near the start.
        clickWave(at: 0.5)
        await seekRun(2)
        await assertEventually { self.store.playheads[self.list] == id }
        try await goFromWaveform(id)
        try await play(0.9)
        clickWave(at: 18)
        await seekRun(3)
        for _ in 0..<8 {
            try await play(0.5)
            let times = try XCTUnwrap(store.progress[id])
            let remaining = try XCTUnwrap(times.remaining, "a counted cue has an end")
            XCTAssertLessThanOrEqual(times.elapsed, 35.05, "never past the cue's length")
            XCTAssertEqual(times.elapsed + remaining, 35, accuracy: 0.1, "elapsed and remaining add up to it")
        }
        XCTAssertEqual(rowMark(id), runningMark)
    }
}

// MARK: Skipped and endless slices

/// A slice's play count of 0 skips it and a letter loops it without end, as the operator sets them
/// on the waveform in a workspace window on the real engine. What is checked is what the window
/// shows: the count drawn on the slice, the dark tint over a skipped one, the cue's time and state
/// in its row, the Warnings list, and where the play line is as the engine plays.
extension PlayheadFlowTests {
    /// A 30 s tone cut at 10 s and 20 s into three slices, each played once, shown in its tab.
    private func threeSlices() async throws -> ModelID {
        let id = try await waveTones(1)[0]
        await showWaveform(id)
        loops.onChange?(.addSlices([10, 20]))
        await assertEventually("two markers") { self.waveView.sliceMarks == [10, 20] }
        XCTAssertEqual(waveView.sliceCounts, [1, 1, 1])
        await assertEventually { self.store.playheads[self.list] == id }
        return id
    }

    /// How bright the top of the waves is drawn at file time `t` (0 black to 1 white).
    private func brightness(at t: Double) throws -> CGFloat {
        let rep = try renderView(waveView)
        let s = CGFloat(rep.pixelsWide) / waveView.bounds.width
        let c = try XCTUnwrap(
            rep.colorAt(x: Int(waveX(t) * s), y: Int((WaveformView.rulerHeight + 4) * s))?.usingColorSpace(.sRGB))
        return (c.redComponent + c.greenComponent + c.blueComponent) / 3
    }

    /// The reasons the Warnings list gives for `id`.
    private func warned(_ id: ModelID) -> [String] {
        WarningRows.make(store: store).filter { $0.cue == id }.map(\.detail)
    }

    func testACountOfZeroSkipsTheSlice() async throws {
        let id = try await threeSlices()
        XCTAssertEqual(cell(id, "duration"), CueTime.running(30))
        let plain = try brightness(at: 15)
        await typeSliceCount(1, "0")
        // Its count reads 0 and it is drawn darker than its neighbours, over its whole span.
        XCTAssertEqual(waveView.sliceCounts, [1, .skip, 1])
        XCTAssertEqual(SlicePlay.skip.label, "0")
        let dark = try XCTUnwrap(waveView.skippedRects().first)
        XCTAssertEqual(waveView.skippedRects().count, 1)
        XCTAssertEqual(dark.minX, waveX(10), accuracy: 0.5)
        XCTAssertEqual(dark.maxX, waveX(20), accuracy: 0.5)
        XCTAssertLessThan(try brightness(at: 15), plain - 0.02, "the skipped slice is darker than it was")
        XCTAssertLessThan(try brightness(at: 15), try brightness(at: 5) - 0.02, "and than the slice before it")
        XCTAssertEqual(try brightness(at: 25), try brightness(at: 5), accuracy: 0.01, "its neighbours are not")
        // The cue's time in the list is the first plus the third slice.
        await assertEventually("20 s in the list") { self.cell(id, "duration") == CueTime.running(20) }
        XCTAssertNil(rowMark(id), "idle, not broken")
        // Playing: the play line goes from the end of the first slice to the start of the third.
        try await goFromWaveform(id)
        try await play(9.4)
        await playsAt(id, 9.5)
        XCTAssertEqual(waveView.skippedRects(), [dark], "drawn the same while it plays")
        try await play(1)
        await playsAt(id, 20.5)
        let times = try XCTUnwrap(store.progress[id])
        XCTAssertEqual(times.elapsed, 10.5, accuracy: 0.3, "the skipped slice takes no time")
        XCTAssertEqual(times.remaining ?? -1, 9.5, accuracy: 0.3)
        // Paused: the same picture, the line standing in the third slice.
        try waveKey("p", code: 35)
        await assertEventually("paused") { self.rowMark(id) == loc("cue.state.paused") }
        try await play(0.5)
        await playsAt(id, 20.5, tolerance: 0.4)
        XCTAssertEqual(waveView.skippedRects(), [dark])
        XCTAssertEqual(waveView.sliceCounts, [1, .skip, 1])
        try waveKey("p", code: 35)
        try await play(9.8)
        await assertEventually("it ends after 20 s of sound") { self.rowMark(id) == nil }
    }

    func testALetterLoopsTheSliceWithoutEnd() async throws {
        let id = try await threeSlices()
        await typeSliceCount(0, "x")
        XCTAssertEqual(waveView.sliceCounts, [.endless, 1, 1])
        XCTAssertEqual(SlicePlay.endless.label, "\u{221E}", "the endless mark is drawn")
        XCTAssertEqual(waveView.skippedRects(), [], "a looping slice is not dark")
        // The list shows one pass, as for any cue that loops without end.
        await assertEventually("one pass in the list") { self.cell(id, "duration") == CueTime.running(30) }
        try await goFromWaveform(id)
        try await play(11.9)
        await playsAt(id, 2, tolerance: 0.4)
        try await play(10)
        await playsAt(id, 2, tolerance: 0.4)
        XCTAssertEqual(rowMark(id), runningMark, "it loops until devamped or stopped")
        XCTAssertNil(store.progress[id]?.remaining, "no end to count down to")
        _ = await store.transport(.hardStop(cue: id))
        try await play(0.2)
        await assertEventually("stopped") { self.rowMark(id) == nil }
        // A number again: it plays that many times.
        await showWaveform(id)
        await typeSliceCount(0, "2")
        XCTAssertEqual(waveView.sliceCounts, [2, 1, 1])
        await assertEventually("40 s in the list") { self.cell(id, "duration") == CueTime.running(40) }
        // Neither a number below 0 nor a sign is a count: refused, the slice as it was.
        await typeSliceCount(0, "-1")
        XCTAssertEqual(waveView.sliceCounts, [2, 1, 1])
        await typeSliceCount(0, "?")
        XCTAssertEqual(waveView.sliceCounts, [2, 1, 1])
    }

    func testTheFirstAndTheLastSliceCanBeSkipped() async throws {
        let id = try await threeSlices()
        await typeSliceCount(0, "0")
        await typeSliceCount(2, "0")
        XCTAssertEqual(waveView.sliceCounts, [.skip, 1, .skip])
        XCTAssertEqual(waveView.skippedRects().count, 2)
        await assertEventually("10 s in the list") { self.cell(id, "duration") == CueTime.running(10) }
        // GO starts at the second slice's start.
        try await goFromWaveform(id)
        try await play(0.4)
        await playsAt(id, 10.5)
        XCTAssertEqual(store.progress[id]?.elapsed ?? -1, 0.5, accuracy: 0.3)
        // And the cue ends at the last slice's start.
        try await play(9.0)
        await playsAt(id, 19.5, tolerance: 0.4)
        try await play(0.8)
        await assertEventually("ended where the last slice starts") { self.rowMark(id) == nil }
        XCTAssertNil(store.progress[id])
    }

    func testEverySliceZeroBreaksTheCue() async throws {
        let id = try await threeSlices()
        let next = try await waveTones(1)[0]
        await showWaveform(id)
        for i in 0..<3 { await typeSliceCount(i, "0") }
        XCTAssertEqual(waveView.sliceCounts, [.skip, .skip, .skip])
        await assertEventually("broken in the list") { self.rowMark(id) == loc("cue.state.broken") }
        XCTAssertEqual(warned(id), [loc("problem.nothing_plays")], "the Warnings list says no part of the file plays")
        // GO does not start it, and the playhead moves on as for any broken cue.
        await assertEventually { self.store.playheads[self.list] == id }
        let before = goes()
        try waveKey(" ", code: 49)
        await assertEventually("GO was sent") { self.goes() == before + 1 }
        try await play(0.5)
        XCTAssertEqual(rowMark(id), loc("cue.state.broken"), "it did not start")
        XCTAssertNil(store.progress[id])
        await assertEventually("the playhead moved on") { self.store.playheads[self.list] == next }
        // One count above 0 clears it.
        await showWaveform(id)
        await typeSliceCount(1, "1")
        await assertEventually("mended") { self.rowMark(id) == nil }
        XCTAssertEqual(warned(id), [])
        await assertEventually("10 s in the list") { self.cell(id, "duration") == CueTime.running(10) }
    }

    func testChangesWhileTheCuePlays() async throws {
        let id = try await threeSlices()
        try await goFromWaveform(id)
        try await play(1.9)
        await playsAt(id, 2)
        let plain = try brightness(at: 15)
        // The count and the tint are the new ones at once; the cue plays on as it started.
        await typeSliceCount(1, "0")
        XCTAssertEqual(waveView.sliceCounts, [1, .skip, 1])
        XCTAssertEqual(waveView.skippedRects().count, 1)
        XCTAssertLessThan(try brightness(at: 15), plain - 0.02, "the tint is drawn while it plays")
        XCTAssertEqual(rowMark(id), runningMark)
        XCTAssertEqual(seeks, [], "the edit moved nothing")
        try await play(10)
        await playsAt(id, 12, tolerance: 0.4)
        XCTAssertEqual(store.progress[id]?.elapsed ?? -1, 12, accuracy: 0.4, "the slice now dark is still heard")
        // Every count 0 while it sounds: still running, not broken, until it has stopped.
        await typeSliceCount(0, "0")
        await typeSliceCount(2, "0")
        XCTAssertEqual(waveView.sliceCounts, [.skip, .skip, .skip])
        XCTAssertEqual(waveView.skippedRects().count, 3)
        try await play(1)
        await playsAt(id, 13, tolerance: 0.4)
        XCTAssertEqual(rowMark(id), runningMark, "not broken while it sounds")
        _ = await store.transport(.hardStop(cue: id))
        try await play(0.2)
        await assertEventually("broken once stopped") { self.rowMark(id) == loc("cue.state.broken") }
        XCTAssertEqual(warned(id), [loc("problem.nothing_plays")])
        // The next start plays the new plan: only the last slice.
        await showWaveform(id)
        await typeSliceCount(2, "1")
        await assertEventually("mended") { self.rowMark(id) == nil }
        _ = await store.transport(.start(cue: id))
        try await play(0.5)
        await showWaveform(id)
        await playsAt(id, 20.5, tolerance: 0.4)
    }

    /// Undo of a count set to 0 puts back the count and the drawing; redo skips the slice again.
    func testUndoRestoresASkippedSlicesCountAndDrawing() async throws {
        let id = try await threeSlices()
        await typeSliceCount(0, "3")
        await typeSliceCount(0, "0")
        XCTAssertEqual(waveView.sliceCounts, [.skip, 1, 1])
        XCTAssertEqual(waveView.skippedRects().count, 1)
        await wc.editor.undo()
        await assertEventually("the count is back") { self.waveView.sliceCounts == [3, 1, 1] }
        XCTAssertEqual(waveView.skippedRects(), [])
        await assertEventually("50 s in the list") { self.cell(id, "duration") == CueTime.running(50) }
        await wc.editor.redo()
        await assertEventually("skipped again") { self.waveView.sliceCounts == [.skip, 1, 1] }
        XCTAssertEqual(waveView.skippedRects().count, 1)
    }

    func testAClickInsideASkippedSliceLoadsToTheStartOfTheNextSliceThatPlays() async throws {
        let id = try await threeSlices()
        await typeSliceCount(1, "0")
        clickWave(at: 14)
        XCTAssertEqual(waveView.insertion ?? -1, 14, accuracy: 0.05, "the line goes where clicked")
        await seekRun(1)
        await assertEventually("the row shows the cue loaded") { self.rowMark(id) == loc("cue.state.loaded") }
        try await goFromWaveform(id)
        try await play(0.4)
        await playsAt(id, 20.5)
        XCTAssertEqual(store.progress[id]?.elapsed ?? -1, 10.5, accuracy: 0.3, "the first slice counts as played")
        // Playing in the first slice, a click in the dark slice jumps to the third: it is never heard.
        clickWave(at: 2)
        await seekRun(2)
        try await play(1)
        await playsAt(id, 3, tolerance: 0.4)
        clickWave(at: 17)
        await seekRun(3)
        try await play(1)
        await playsAt(id, 21, tolerance: 0.4)
        XCTAssertEqual(rowMark(id), runningMark)
    }
}
