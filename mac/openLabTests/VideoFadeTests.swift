// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// A Fade cue aimed at a Video cue (VID-05, CT-10; QLab 5 manual pp. 241-247), each flow of the
/// issue's checklist on the real engine with a stage host on a virtual display: the operator's
/// action in the inspector or the transport, then what the inspector shows and what the stage
/// shows (the test card's red quadrant read back: its red is the picture's opacity over black).
extension VideoCueTests {
    var fg: FadeGeometryPanel { wc.inspector.fadeGeometry }

    /// A new Fade aimed at `target` (made from it selected, as the toolbar makes one), `seconds`
    /// long, its Geometry tab shown.
    func fade(on target: ModelID, seconds: Double) async throws -> ModelID {
        click(target)
        let made = await wc.editor.add(.fade)
        let id = try XCTUnwrap(made)
        await soon("the Fade aims at its target") { self.kind(id)?["target"]?.stringValue == target.raw }
        guard case .success = await store.send(command: .setProperty(id, .duration(seconds))) else {
            XCTFail("the engine refused the Fade's duration")
            throw CancellationError()
        }
        try await show(id, tab: "fadeGeometry")
        return id
    }

    /// Ticks `check` (or unticks it) as a click on it does.
    func tick(_ check: NSButton, _ on: Bool = true) {
        check.state = on ? .on : .off
        fire(check)
    }

    func fadeGeometry(_ id: ModelID) -> FadeGeometryValues { FadeGeometryValues(kind: kind(id)) }

    /// The red the stage shows in the card's red quadrant, as a fraction of `full`.
    func opacity(of full: UInt8) async throws -> Double { Double(try await red()) / Double(full) }

    /// The stage shows the picture at `want` of the opacity whose red was `full`.
    func expectOpacity(
        _ full: UInt8, _ want: Double, _ within: Double, _ what: String = "", line: UInt = #line
    ) async throws {
        let got = try await opacity(of: full)
        XCTAssertEqual(got, want, accuracy: within, what, line: line)
    }

    // MARK: Flow 1

    /// A Fade whose target is a Video cue shows Curve, Levels (its target has sound) and Geometry;
    /// aimed at a silent Video cue, Curve and Geometry; aimed at an Audio cue or a Group, no
    /// Geometry tab.
    func testAFadeWhoseTargetIsAVideoCueShowsCurveLevelsAndGeometryTabsAndNotTheAudioOnlyOnes() async throws {
        try await openSoundShow(seconds: 4)
        let loud = try await fade(on: Self.movie, seconds: 1)
        XCTAssertEqual(tabs(), ["basics", "triggers", "curve", "fadeLevels", "fadeGeometry"])
        XCTAssertEqual(wc.inspector.tabView.selectedTabViewItem?.label, loc("inspector.tab.geometry"))
        let quiet = try await fade(on: Self.still, seconds: 1)
        XCTAssertEqual(tabs(), ["basics", "triggers", "curve", "fadeGeometry"], "a still has no sound")
        // Retargeted at a Fade's own kind of target that is not a Video cue: no Geometry tab.
        guard case .success = await store.send(command: .setProperty(quiet, .target(loud))) else {
            return XCTFail("retarget")
        }
        await soon("a Fade aimed at a Fade shows no Geometry tab") { !self.tabs().contains("fadeGeometry") }
        // No target yet: every tab a Fade can have, as QLab shows it.
        guard case .success = await store.send(command: .setProperty(quiet, .target(nil))) else {
            return XCTFail("clear target")
        }
        await soon("a Fade with no target has its Geometry tab") { self.tabs().contains("fadeGeometry") }
    }

    // MARK: Flow 2

    /// The Geometry tab has a tick box beside opacity, position, scale and rotation, none ticked
    /// for a new Fade; a value typed while unticked is kept but not faded, and only the ticked
    /// values move: run, the picture's opacity fades while it stays in place.
    func testGeometryTabATickBoxBesideEachValueAndOnlyTickedValuesAreFaded() async throws {
        try await openVideoShow()
        let id = try await fade(on: Self.still, seconds: 0.5)
        XCTAssertEqual(
            fg.checks.map(\.title),
            ["opacity", "position", "scale", "rotation"].map {
                loc("inspector.geometry.\($0)")
            })
        XCTAssertTrue(fg.checks.allSatisfy { $0.state == .off && $0.isEnabled }, "none ticked for a new Fade")
        for v in fg.checks + fg.numberFields as [NSView] {
            XCTAssertFalse(v.isHiddenOrHasHiddenAncestor || v.frame.isEmpty, "\(v.accessibilityIdentifier())")
        }
        try typeInto(fg.xField, "700")
        await soon("the position is kept, unticked") { self.fadeGeometry(id).translation.x == 700 }
        XCTAssertFalse(fadeGeometry(id).fadeTranslation)
        XCTAssertEqual(fg.xField.textColor, .secondaryLabelColor, "an unticked value is drawn dim")
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "50")
        await soon("opacity 50 % ticked") { self.fadeGeometry(id).fadeOpacity && self.fadeGeometry(id).opacity == 0.5 }
        XCTAssertEqual(fg.opacityField.stringValue, "50")
        XCTAssertEqual(fg.opacityField.textColor, .controlTextColor)
        await soon("a ticked value is something to fade") { self.store.problems[id]?.isEmpty ?? true }

        try await perform(.startSelected, on: Self.still)
        try await play(0.3)
        let full = try await red()
        XCTAssertGreaterThan(full, 200)
        try await perform(.startSelected, on: id)
        try await play(0.7)
        let shown = await colours(Self.stage, [[0.25, 0.4], [0.75, 0.9]])
        XCTAssertEqual(Double(shown.first?.first ?? 0) / Double(full), 0.5, accuracy: 0.05, "half opacity")
        XCTAssertGreaterThan(shown.last?.first ?? 0, 90, "the yellow quadrant is still there: not moved")
    }

    // MARK: Flow 3

    /// A Fade with opacity 0 ticked, 3 s, the S-curve chosen → GO while the video plays → the
    /// picture fades to nothing over 3 s along that curve (half way at half time, a quarter in
    /// still above 80 %); the movie plays on; the target reads as fading, then running.
    func testAFadeWithOpacity0Ticked3sGoWhileTheVideoPlaysFadesThePictureToNothingAlongItsCurve() async throws {
        try await openVideoShow()
        let id = try await fade(on: Self.movie, seconds: 3)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "0")
        await soon { self.fadeGeometry(id).fadeOpacity && self.fadeGeometry(id).opacity == 0 }
        try await show(id, tab: "curve")
        choose(wc.inspector.fadeSettings.curvePopup, FadeSettingsPanel.curves.firstIndex(of: "s_curve") ?? 0)
        await soon("the S-curve chosen") { self.kind(id)?["curve"]?.stringValue == "s_curve" }

        try await perform(.startSelected, on: Self.movie)
        try await play(0.3)
        let full = try await red()
        let before = await shownFrame()
        try await perform(.startSelected, on: id)
        try await play(0.75)
        try await expectOpacity(full, 0.85, 0.05, "S-curve, a quarter in")
        XCTAssertEqual(status(Self.movie), loc("cue.state.fading"))
        try await play(0.75)
        try await expectOpacity(full, 0.5, 0.05, "half way")
        try await play(1.6)
        let red6673 = try await red()
        XCTAssertEqual(red6673, 0, "faded to nothing")
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "the movie plays on")
        XCTAssertNotNil(before)
    }

    // MARK: Flow 4

    /// "Stop target when done" → the video cue stops at the end of the fade and its picture
    /// leaves the stage.
    func testStopTargetWhenDoneStopsTheVideoAtTheEndOfTheFadeAndItsPictureLeavesTheStage() async throws {
        try await openVideoShow()
        let id = try await fade(on: Self.still, seconds: 1)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "40")
        tick(fg.stopCheck)
        await soon("stop target ticked") { self.kind(id)?["stop_target_when_done"] == .bool(true) }
        XCTAssertEqual(fg.stopCheck.state, .on)
        try await perform(.startSelected, on: Self.still)
        try await play(0.2)
        try await perform(.startSelected, on: id)
        try await play(0.5)
        XCTAssertEqual(status(Self.still), loc("cue.state.fading"))
        try await play(0.7)
        await soon("the Video cue stopped with the Fade") { self.status(Self.still) == nil }
        let gone = await shownFrame()
        XCTAssertNil(gone)
        let red7873 = try await red()
        XCTAssertEqual(red7873, 0, "its picture left the stage")
    }

    // MARK: Flow 5

    /// Absolute and relative apply to geometry as to levels: relative opacity 50 % halves where
    /// the picture is each time it runs; an absolute Fade then puts it at its value whatever it was.
    func testAbsoluteAndRelativeModeApplyToGeometryAsToLevels() async throws {
        try await openVideoShow()
        let half = try await fade(on: Self.still, seconds: 0.2)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "50")
        choose(fg.modePopup, 1)
        await soon("relative") { self.kind(half)?["mode"]?.stringValue == "relative" }
        XCTAssertEqual(fg.modePopup.titleOfSelectedItem, loc("fade.mode.relative"))
        let back = try await fade(on: Self.still, seconds: 0.2)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "100")
        await soon { self.fadeGeometry(back).fadeOpacity }
        XCTAssertEqual(fg.modePopup.titleOfSelectedItem, loc("fade.mode.absolute"))

        try await perform(.startSelected, on: Self.still)
        try await play(0.2)
        let full = try await red()
        try await perform(.startSelected, on: half)
        try await play(0.4)
        try await expectOpacity(full, 0.5, 0.04)
        try await perform(.startSelected, on: half)
        try await play(0.4)
        try await expectOpacity(full, 0.25, 0.04, "half of where it was")
        try await perform(.startSelected, on: back)
        try await play(0.4)
        try await expectOpacity(full, 1, 0.02, "absolute: at its value")
    }

    // MARK: Flow 6

    /// Two Fades on the same value of one cue → the later one takes it over from where the first
    /// had it, as with levels; the first, still running, no longer moves it.
    func testTwoFadesOnTheSameValueOfOneCueTheLaterOneTakesItOver() async throws {
        try await openVideoShow()
        let down = try await fade(on: Self.still, seconds: 4)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "0")
        let up = try await fade(on: Self.still, seconds: 1)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "100")
        await soon { self.fadeGeometry(down).fadeOpacity && self.fadeGeometry(up).fadeOpacity }

        try await perform(.startSelected, on: Self.still)
        try await play(0.2)
        let full = try await red()
        try await perform(.startSelected, on: down)
        try await play(1)
        try await expectOpacity(full, 0.75, 0.04)
        try await perform(.startSelected, on: up)
        try await play(0.5)
        try await expectOpacity(full, 0.875, 0.04, "on its way up from where it was")
        try await play(1)
        try await expectOpacity(full, 1, 0.02)
        XCTAssertNotNil(status(down), "the first still runs")
        try await play(2)
        try await expectOpacity(full, 1, 0.02, "and moves the opacity no more")
    }

    // MARK: Flow 7

    /// A Fade started while its target is not running does nothing and raises no warning: no
    /// footer notice, nothing added to the Warnings count; it runs its time and the picture
    /// started after it is untouched.
    func testAFadeStartedWhileItsTargetIsNotRunningDoesNothingAndRaisesNoWarning() async throws {
        try await openVideoShow()
        let id = try await fade(on: Self.still, seconds: 0.5)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "0")
        await soon { self.fadeGeometry(id).fadeOpacity }
        let warningsBefore = wc.footer.warningsButton.title
        let runtime = store.runtimeWarnings.count
        try await perform(.startSelected, on: id)
        XCTAssertEqual(status(id), loc("cue.state.running"), "it runs its time")
        try await play(0.7)
        XCTAssertNil(status(id))
        XCTAssertEqual(store.runtimeWarnings.count, runtime, "no warning listed")
        XCTAssertEqual(wc.footer.warningsButton.title, warningsBefore, "the Warnings count is unchanged")
        XCTAssertEqual(wc.footer.noticeLabel.stringValue, "", "no notice")
        try await perform(.startSelected, on: Self.still)
        try await play(0.2)
        let red12038 = try await red()
        XCTAssertGreaterThan(red12038, 200, "the picture is untouched")
    }

    // MARK: Flow 8

    /// Pause and resume mid-fade → the fade holds and continues; panic mid-fade → picture and
    /// sound fade out from where they are.
    func testPauseAndResumeMidFadeHoldAndContinueAndPanicFadesPictureAndSoundOut() async throws {
        try await openSoundShow(
            seconds: 20,
            show: { ws in
                var settings = ws["settings"] ?? .object([:])
                settings.set("panic_duration", .double(1))
                ws.set("settings", settings)
            })
        let id = try await fade(on: Self.movie, seconds: 2)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "0")
        await soon { self.fadeGeometry(id).fadeOpacity }
        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        // The test track's level changes through its file: the loudest over a second is its top.
        let sound = try await loudest(.cue(Self.movie), over: 1)
        let full = try await red()
        try await perform(.startSelected, on: id)
        try await play(0.5)
        try await expectOpacity(full, 0.75, 0.05, "a quarter way down")
        await app.perform(.pauseAll, on: wc, cues: [])
        try await play(1)
        try await expectOpacity(full, 0.75, 0.05, "held while paused")
        await app.perform(.resumeAll, on: wc, cues: [])
        try await play(0.5)
        try await expectOpacity(full, 0.5, 0.05, "carries on from where it held")
        // Panic: from where they are, over the workspace's panic time (1 s).
        await app.perform(.panic, on: wc, cues: [])
        try await play(0.4)
        let fading = try await loudest(.cue(Self.movie), over: 0.1)
        XCTAssertLessThan(fading, sound * 0.7, "the sound on its way out")
        XCTAssertGreaterThan(fading, 0, "and not cut")
        let leaving = try await opacity(of: full)
        XCTAssertLessThan(leaving, 0.4, "the picture on its way out from half")
        XCTAssertGreaterThan(leaving, 0.1)
        try await play(0.7)
        await soon("the Video cue ended") { self.status(Self.movie) == nil }
        let red13866 = try await red()
        XCTAssertEqual(red13866, 0)
    }

    // MARK: Flow 9

    /// The Fade can be previewed, as an audio Fade can: Preview on its Geometry tab runs it on its
    /// playing target now, and the playhead stays where it is.
    func testTheFadeCanBePreviewedAsAnAudioFadeCan() async throws {
        try await openVideoShow()
        let id = try await fade(on: Self.still, seconds: 0.5)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "0")
        await soon { self.fadeGeometry(id).fadeOpacity }
        try await perform(.startSelected, on: Self.still)
        try await play(0.2)
        try await show(id, tab: "fadeGeometry")
        let playhead = store.playheads
        XCTAssertTrue(fg.previewButton.isEnabled)
        press(fg.previewButton)
        await sync()
        try await play(0.7)
        let red14699 = try await red()
        XCTAssertEqual(red14699, 0, "the Fade ran on its target")
        XCTAssertEqual(store.playheads, playhead, "the playhead did not move")
    }

    // MARK: Broken, whole percents, a target filling its stage

    /// A Fade with only picture values ticked is something to fade for a Video target only:
    /// aimed at an Audio cue it is broken for nothing to fade (its row shows the mark and the
    /// reason), and aimed back at the Video cue it is not. Opacity is typed in whole percents.
    /// While the target fills its stage the tab says only its opacity changes.
    func testAFadeWithOnlyPictureValuesIsBrokenOnAnAudioTargetAndNotOnAVideoTarget() async throws {
        try await openVideoShow()
        let id = try await fade(on: Self.still, seconds: 1)
        tick(fg.opacityCheck)
        try typeInto(fg.opacityField, "33.7")
        await soon("whole percents") { self.fadeGeometry(id).fadeOpacity && self.fadeGeometry(id).opacity == 0.34 }
        await soon { self.fg.opacityField.stringValue == "34" }
        await soon("not broken on the Video cue") { self.status(id) == nil }
        XCTAssertFalse(fg.fillsStageNote.isHiddenOrHasHiddenAncestor, "the still fills its stage: said so")
        XCTAssertFalse(fg.fillsStageNote.frame.isEmpty)

        let sound = await wc.editor.add(.audio)
        let audio = try XCTUnwrap(sound)
        guard case .success = await store.send(command: .setProperty(id, .target(audio))) else {
            return XCTFail("retarget")
        }
        await soon("broken on an Audio cue") { self.status(id) == loc("cue.state.broken") }
        XCTAssertTrue(brokenTip(id)?.contains(loc("problem.nothing_to_fade")) == true, "\(brokenTip(id) ?? "")")
        guard case .success = await store.send(command: .setProperty(id, .target(Self.still))) else {
            return XCTFail("retarget")
        }
        await soon("not broken on the Video cue again") { self.status(id) == nil }

        // Custom placement: the note goes.
        guard
            case .success = await store.send(
                command: .setProperty(Self.still, .kindFields(["fill_stage": .bool(false)])))
        else { return XCTFail("custom") }
        click(id)
        await soon("no note for a custom target") { self.fg.fillsStageNote.isHidden }
    }
}
