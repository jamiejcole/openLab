// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The timing and loops tab of an Audio cue (AUD-11, AUD-12, AUD-14): the waveform drawing model,
/// the trim handles and fields, the play count, and how peaks are fetched, against the fake engine
/// (which answers `waveform` from the cue's `media_durations` entry and applies `kind` edits).
@MainActor
final class TimeLoopsTests: XCTestCase {
    /// Intro: an Audio cue of a 62.5 s file, untrimmed, played once (the welcome fixture).
    static let intro = CueListHarness.intro

    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!
    private var panel: TimeLoopsPanel { vc.timeLoops }
    private var wave: WaveformView { vc.timeLoops.waveform }

    override func setUp() async throws {
        let base = try Fixtures.json("server", "welcome")["snapshot"] ?? .null
        rig = try Rig(snapshot: FakeEngine.demoSnapshot(base: base))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        vc = InspectorViewController(store: rig.store)
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
    }

    override func tearDown() async throws { window.orderOut(nil) }

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
        rig.engine.received.compactMap {
            guard case .command(_, let c, _) = $0, let cmd = JSONValue.parse((try? c.json.text()) ?? ""),
                cmd["property"]?["type"]?.stringValue == "kind"
            else { return nil }
            return normalised(cmd["property"]?["value"])
        }
    }

    private var waveformRequests: Int {
        rig.engine.requestCount { if case .waveform = $0 { return true } else { return false } }
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
        vc.setSelection([CueListHarness.fade])
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
        XCTAssertEqual(kinds.last?["end_time"], .null, "the rest of the kind is untouched")
        XCTAssertEqual(kinds.last?["media"], .string("media/a.wav"))
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
        type(panel.endField, "4")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("timeLoops.invalid.trim"))
        XCTAssertEqual(panel.endField.stringValue, "01:02.50", "the file's end is shown again")
        type(panel.endField, "1:00")
        await assertEventually { self.kinds.count == 2 }
        XCTAssertEqual(kinds.last?["end_time"], .double(60))
        XCTAssertEqual(kinds.last?["start_time"], .double(5), "built on the previous edit")
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
        vc.setSelection([CueListHarness.fade])
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
        panel.showProgress(elapsed: 3)
        XCTAssertEqual(wave.playhead, 13)
        panel.loopRadio.state = .on
        fire(panel.loopRadio)
        await assertEventually { self.panel.playCount == 0 }
        panel.showProgress(elapsed: 52.5 + 2)
        XCTAssertEqual(wave.playhead ?? 0, 12, accuracy: 1e-9, "the second pass")
        panel.showProgress(elapsed: nil)
        XCTAssertNil(wave.playhead)
    }
}
