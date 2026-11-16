// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// A Video cue's sound in the app (VID-06, AUD-06; QLab 5 manual pp. 198, 211-216, 242): each
/// acceptance flow on the real engine with a stage host on a virtual display and the stepped
/// virtual audio device. The operator's action on the window, then what the inspector, the list,
/// the meters and the stage show. Test movies carry sound that says where in the file it is (first
/// channel: a ramp from 0 to 0.5 over each second; second channel: the second's number plus one,
/// over 2048), so the cue's meter gives the sound's position beside the frame on the stage.
extension VideoCueTests {
    static let mainPA: ModelID = "00000000-0000-0000-0000-000000000001"
    static let sidePatch: ModelID = "00000000-0000-0000-0000-000000000002"
    static let stereo = "pcm:2:48000"

    // MARK: Setting up

    /// The Video show with cue 1's movie remade with `sound` (`seconds` long) and, when `patched`,
    /// on the Main PA patch with a new cue's levels. `files` writes further media (the media
    /// folder); `show` edits the workspace before it opens.
    func openSoundShow(
        seconds: Double = 4, sound: String = VideoCueTests.stereo, patched: Bool = true,
        files: ((URL) throws -> Void)? = nil, show: ((inout JSONValue) -> Void)? = nil
    ) async throws {
        try await openVideoShow { folder in
            let media = folder.appendingPathComponent("media", isDirectory: true)
            try writeTestMedia(media.appendingPathComponent("clip.mov"), seconds: seconds, sound: sound)
            try files?(media)
            try editShow(folder.appendingPathComponent("Video.openlab")) { ws in
                if patched { Self.patch(&ws, cue: 0, on: Self.mainPA, outputs: 2) }
                show?(&ws)
            }
        }
        await soon("the movie's sound was probed") { self.store.mediaDurations[Self.movie]?.hasSound == true }
    }

    /// Cue `index` of the show's list put on `patch` with a new cue's levels for `outputs` outputs.
    static func patch(_ ws: inout JSONValue, cue index: Int, on patch: ModelID, outputs: Int) {
        editKind(&ws, cue: index) { kind in
            kind.set("patch", .string(patch.raw))
            let object = ModelObject(
                id: patch,
                raw: .object(["outputs": .array(Array(repeating: .object([:]), count: outputs))]))
            kind.set("levels", NewCueKind.defaultLevels(patch: object))
        }
    }

    static func editKind(_ ws: inout JSONValue, cue index: Int, _ edit: (inout JSONValue) -> Void) {
        guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue,
            cues.indices.contains(index), var kind = cues[index]["kind"]
        else { return XCTFail("the show has no cue \(index)") }
        edit(&kind)
        cues[index].set("kind", kind)
        lists[0].set("cues", .array(cues))
        ws.set("cue_lists", .array(lists))
    }

    /// A second output patch, "Side fills", of one cue output.
    static func addSidePatch(_ ws: inout JSONValue) {
        let output = JSONValue.object([
            "level_db": .double(0), "mute": .bool(false), "polarity_inverted": .bool(false),
        ])
        ws.set(
            "patches",
            .array(
                (ws["patches"]?.arrayValue ?? []) + [
                    .object([
                        "id": .string(sidePatch.raw), "name": .string("Side fills"), "master_db": .double(0),
                        "outputs": .array([output]),
                    ])
                ]))
    }

    // MARK: Doing and seeing

    /// The inspector showing `id` on its tab `tab`.
    @discardableResult
    func show(_ id: ModelID, tab: String) async throws -> InspectorViewController {
        let inspector = wc.inspector
        click(id)
        await soon("the inspector shows the cue") { inspector.cue?.id == id }
        let item = try XCTUnwrap(
            inspector.tabView.tabViewItems.first { $0.identifier as? String == tab },
            "the cue has a \(tab) tab; it has \(tabs())")
        inspector.tabView.selectTabViewItem(item)
        return inspector
    }

    func tabs() -> [String] { wc.inspector.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    func fire(_ control: NSControl) { _ = control.target?.perform(control.action, with: control) }

    /// A pick from a pop-up's menu.
    func choose(_ popup: NSPopUpButton, _ title: String, line: UInt = #line) {
        XCTAssertTrue(popup.isEnabled, "the pop-up is on", line: line)
        XCTAssertTrue(popup.itemTitles.contains(title), "\(popup.itemTitles) has \(title)", line: line)
        popup.selectItem(withTitle: title)
        XCTAssertTrue(popup.sendAction(popup.action, to: popup.target), line: line)
    }

    /// A level typed into the Levels tab's box for `slot`.
    func typeLevel(_ panel: LevelsPanel, _ slot: LevelSet.Slot, _ text: String) {
        panel.select(slot)
        panel.levelField.stringValue = text
        panel.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: panel.levelField))
        panel.controlTextDidEndEditing(
            Notification(name: NSControl.textDidEndEditingNotification, object: panel.levelField))
    }

    func kind(_ id: ModelID) -> JSONValue? { store.model?.cue(id)?.raw["kind"] }
    func levels(_ id: ModelID) -> LevelSet? { kind(id).flatMap { LevelSet(kind: $0) } }

    /// The loudest `source`'s meter channel gets while `seconds` of device time play. The meters
    /// run only while one is on screen: a Levels tab is shown.
    func loudest(
        _ source: MeterSource, channel: Int = 0, over seconds: Double, line: UInt = #line
    ) async throws -> Float {
        XCTAssertGreaterThan(store.meters.visibleCount, 0, "a meter is on screen", line: line)
        // What the meter held before is forgotten first.
        store.meters.reset()
        var most: Float = 0
        for _ in 0..<max(1, Int((seconds * 10).rounded())) {
            try await play(0.1)
            store.meters.tick(0)
            most = max(most, store.meters.level(source, channel: channel))
        }
        return most
    }

    /// Where in its file `cue`'s sound is after a further 0.1 s, read off its meter (the test
    /// track's two channels, at unity straight through). The 0.1 s must not cross a whole second
    /// of the file.
    func soundPosition(_ cue: ModelID, line: UInt = #line) async throws -> Double {
        XCTAssertGreaterThan(store.meters.visibleCount, 0, "a meter is on screen", line: line)
        store.meters.reset()
        try await play(0.1)
        store.meters.tick(0)
        let ramp = Double(store.meters.level(.cue(cue), channel: 0))
        let mark = Double(store.meters.level(.cue(cue), channel: 1))
        return (mark * 2048 - 1).rounded() + ramp * 2
    }

    /// The picture's place in its file by the frame on the stage (25 fps test movies).
    func picturePosition() async -> Double? { await shownFrame().map { Double($0) / 25 } }

    /// Picture and sound of `cue` are at the same place of the file, `near` it. The meter is sent
    /// at most thirty times a second, so the sound read may be that much behind: two frames' room.
    func expectInStep(_ cue: ModelID, near: Double, _ what: String, line: UInt = #line) async throws {
        let sound = try await soundPosition(cue, line: line)
        let picture = await picturePosition()
        XCTAssertEqual(picture ?? -1, near, accuracy: 0.09, "\(what): the picture", line: line)
        XCTAssertEqual(sound, picture ?? -1, accuracy: 0.08, "\(what): the sound beside the picture", line: line)
    }

    /// The rows of the Warnings list about `cue`.
    func warningRows(_ cue: ModelID) -> [WarningRow] { WarningRows.make(store: store).filter { $0.cue == cue } }

    // MARK: Flow 1

    /// A movie with a stereo track dropped into the list: the new Video cue has a Levels tab after
    /// Timing & Loops laid out as an Audio cue's, with a new Audio cue's levels; its I/O tab shows
    /// the sound's format under the video's and the live patch pop-up on the first patch. GO: the
    /// picture is on the stage, the sound on the patch, the meters move. Another patch chosen while
    /// idle: the columns follow it and the next GO sounds there. Narrow window; Show Mode.
    func testAMovieWithSoundGetsALevelsTabAndIsHeardOnItsPatch() async throws {
        var dropped: URL!
        try await openVideoShow { folder in
            dropped = folder.appendingPathComponent("media/sound.mov")
            try writeTestMedia(dropped, seconds: 4, sound: Self.stereo)
            try editShow(folder.appendingPathComponent("Video.openlab")) { Self.addSidePatch(&$0) }
        }
        let plan = try XCTUnwrap(wc.cueList.fileDropPlan([dropped], item: nil, index: 6))
        wc.cueList.onFileDrop?(plan, CueListViewController.skipped([dropped], plan))
        await soon("a cue was made") { self.wc.cueList.roots.count == 7 }
        let id = try XCTUnwrap(wc.cueList.roots.last?.id)
        await soon("its sound was probed") { self.store.mediaDurations[id]?.hasSound == true }
        XCTAssertNil(status(id), "playable: no broken mark")

        // The tabs, and the Levels tab as an Audio cue's.
        let inspector = try await show(id, tab: "levels")
        XCTAssertEqual(tabs(), ["basics", "triggers", "io", "geometry", "timeLoops", "levels"])
        XCTAssertEqual(InspectorTabs.ids(forKind: "audio").last, "levels", "where an Audio cue has it")
        let panel = inspector.audioLevels
        XCTAssertEqual(panel.matrix.inputs, 2, "one row per file channel")
        XCTAssertEqual(panel.matrix.outputCount, 2, "one column per cue output of Main PA")
        let audio = await wc.editor.add(.audio)
        let audioID = try XCTUnwrap(audio)
        await soon { self.kind(audioID) != nil }
        XCTAssertEqual(
            normalised(kind(id)?["levels"]), normalised(kind(audioID)?["levels"]), "a new Audio cue's levels")
        XCTAssertEqual(kind(id)?["patch"]?.stringValue, Self.mainPA.raw, "the workspace's first patch")
        _ = await store.undo()

        // The I/O tab.
        try await show(id, tab: "io")
        let v = inspector.videoIO
        XCTAssertEqual(v.soundRow?.isHidden, false)
        XCTAssertFalse(v.soundLabel.isHidden)
        XCTAssertTrue(v.trackPopup.isHidden, "one track: plain text")
        XCTAssertEqual(
            v.soundLabel.stringValue,
            String(
                format: loc("inspector.io.soundFormatText"), loc("inspector.io.audioCodec.lpcm"),
                String(format: loc("inspector.io.channels.many"), 2), "48"))
        inspector.view.layoutSubtreeIfNeeded()
        let format = v.formatLabel.convert(v.formatLabel.bounds, to: nil)
        let sound = v.soundLabel.convert(v.soundLabel.bounds, to: nil)
        let stage = v.stagePopup.convert(v.stagePopup.bounds, to: nil)
        XCTAssertLessThan(sound.maxY, format.minY + 1, "under the video format")
        XCTAssertGreaterThan(sound.minY, stage.maxY - 1, "above the stage")
        XCTAssertEqual(v.patchRow?.isHidden, false)
        XCTAssertTrue(inspector.outputPatchPopup.isEnabled, "the patch pop-up is live")
        XCTAssertEqual(inspector.outputPatchPopup.titleOfSelectedItem, "Main PA")
        XCTAssertTrue(inspector.editPatchButton.isEnabled)
        let openEditor = inspector.onEditPatch
        var edited: [ModelID] = []
        inspector.onEditPatch = { edited.append($0) }
        inspector.editPatchButton.performClick(nil)
        XCTAssertEqual(edited, [Self.mainPA], "Edit opens the cue's patch")
        inspector.onEditPatch = openEditor
        XCTAssertEqual(
            inspector.outputPatchPopup.itemTitles, ["Main PA", "Side fills", "", loc("inspector.io.patchNone")])

        // GO: the picture on the stage, the sound on the patch, the meters moving.
        try await show(id, tab: "levels")
        try await perform(.startSelected, on: id)
        let cueLevel = try await loudest(.cue(id), over: 1)
        XCTAssertGreaterThan(cueLevel, 0.3, "the cue's meter moves")
        let out = try await loudest(.output(Self.mainPA), over: 0.5)
        XCTAssertGreaterThan(out, 0.05, "the patch's meter moves")
        let frame = await shownFrame()
        XCTAssertEqual(Double(frame ?? 0), 37, accuracy: 3, "the picture is on the stage")
        XCTAssertEqual(status(id), loc("cue.state.running"))
        try await perform(.stopSelected, on: id)
        await soon("stopped") { self.status(id) == nil }

        // Another patch while idle: the Levels columns follow it; the next GO sounds there.
        try await show(id, tab: "io")
        choose(inspector.outputPatchPopup, "Side fills")
        await soon("on the side patch") { self.kind(id)?["patch"]?.stringValue == Self.sidePatch.raw }
        try await show(id, tab: "levels")
        await soon("one column: the side patch's one cue output") { panel.matrix.outputCount == 1 }
        XCTAssertNil(status(id), "playable on it")
        try await perform(.startSelected, on: id)
        let side = try await loudest(.output(Self.sidePatch), over: 1)
        XCTAssertGreaterThan(side, 0.3, "heard on the side patch")
        XCTAssertEqual(store.meters.channelCount(.cue(id)), 1, "the cue's meter has that patch's one output")
        try await perform(.stopSelected, on: id)

        // A narrow window: the tab's board scrolls inside the tab, as an Audio cue's does.
        wc.window?.setContentSize(wc.window?.contentMinSize ?? .zero)
        wc.window?.layoutIfNeeded()
        try await show(id, tab: "levels")
        let tab = inspector.tabView.contentRect
        let scroll = try XCTUnwrap(panel.matrix.enclosingScrollView)
        XCTAssertTrue(
            tab.insetBy(dx: -1, dy: -1).contains(scroll.convert(scroll.bounds, to: inspector.tabView)),
            "the levels stay inside the tab")

        // Show Mode locks it, as an Audio cue's.
        wc.setShowMode(true)
        XCTAssertTrue(panel.isLocked)
        XCTAssertFalse(panel.defaultButton.isEnabled)
        try await show(id, tab: "io")
        XCTAssertFalse(inspector.outputPatchPopup.isEnabled)
        XCTAssertFalse(inspector.editPatchButton.isEnabled)
    }

    // MARK: Flow 2

    /// A movie without sound and a still: no Levels tab, no sound-format or patch row, no patch
    /// problem; GO shows the picture alone. The file replaced by one with sound brings the tab and
    /// rows without reselecting; replaced back they go, the cue's levels kept but shown nowhere.
    func testAMovieWithoutSoundAndAStillHaveNoLevelsTab() async throws {
        var withSound: URL!
        var without: URL!
        try await openVideoShow { folder in
            withSound = folder.appendingPathComponent("media/sound.mov")
            without = folder.appendingPathComponent("media/quiet.mov")
            try writeTestMedia(withSound, seconds: 4, sound: Self.stereo)
            try writeTestMedia(without, seconds: 4)
        }
        // An Audio cue's Levels tab keeps the meters running while the silent cues play.
        let metered = await wc.editor.add(.audio)
        let meterCue = try XCTUnwrap(metered)
        let inspector = try await show(Self.movie, tab: "io")
        let v = inspector.videoIO
        for cue in [Self.movie, Self.still] {
            try await show(cue, tab: "io")
            XCTAssertEqual(tabs(), ["basics", "triggers", "io", "geometry", "timeLoops"], "no Levels tab")
            XCTAssertEqual(v.soundRow?.isHidden, true, "no sound-format line")
            XCTAssertEqual(v.patchRow?.isHidden, true, "no output-patch row")
            XCTAssertNil(status(cue), "no patch problem, though it names no patch")
            XCTAssertTrue(store.problems[cue]?.isEmpty ?? true)
        }

        // GO: the picture alone; it runs and ends as before.
        try await show(meterCue, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        let heard = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(heard, 0, "no meter moves")
        let out = try await loudest(.output(Self.mainPA), over: 0.5)
        XCTAssertEqual(out, 0)
        let frame = await shownFrame()
        XCTAssertEqual(Double(frame ?? 0), 37, accuracy: 3)
        try await play(2.7)
        await soon("ended at its last frame") { self.status(Self.movie) == nil }

        // Replaced by a file with sound, the cue still selected.
        try await show(Self.movie, tab: "io")
        await wc.editor.replaceFile(of: Self.movie, with: withSound)
        await soon("the Levels tab appears") { self.tabs().last == "levels" }
        XCTAssertEqual(inspector.cue?.id, Self.movie)
        XCTAssertEqual(v.soundRow?.isHidden, false)
        XCTAssertEqual(v.patchRow?.isHidden, false)
        XCTAssertFalse(v.soundLabel.stringValue.isEmpty)
        choose(inspector.outputPatchPopup, "Main PA")
        await soon("patched") { self.status(Self.movie) == nil && self.kind(Self.movie)?["patch"] != .null }
        // It had no levels of its own: a new cue's levels on that patch come with it.
        XCTAssertEqual(levels(Self.movie)?.outputs.map(\.db), [0, 0])
        XCTAssertEqual(levels(Self.movie)?.crosspoints.count, 2)
        try await show(Self.movie, tab: "levels")
        typeLevel(inspector.audioLevels, .master, "-7")
        await soon { self.levels(Self.movie)?.master == -7 }

        // Replaced back: they go, and the levels are shown nowhere.
        await wc.editor.replaceFile(of: Self.movie, with: without)
        await soon("the Levels tab goes") { !self.tabs().contains("levels") }
        try await show(Self.movie, tab: "io")
        XCTAssertEqual(v.soundRow?.isHidden, true)
        XCTAssertEqual(v.patchRow?.isHidden, true)
        XCTAssertNil(inspector.audioLevels.meter.source)
        XCTAssertNil(status(Self.movie), "not broken by the patch it still names")
        XCTAssertEqual(levels(Self.movie)?.master, -7, "kept in the show")
        // A Fade aimed at it has no Levels tab either.
        let fade = await wc.editor.add(.fade, after: [Self.movie])
        let fadeID = try XCTUnwrap(fade)
        guard case .success = await store.send(command: .setProperty(fadeID, .target(Self.movie))) else {
            return XCTFail("target")
        }
        click(fadeID)
        await soon("the Fade has no Levels tab") {
            inspector.cue?.id == fadeID && self.tabs() == ["basics", "triggers", "curve"]
        }

        // With sound again: the levels come back.
        await wc.editor.replaceFile(of: Self.movie, with: withSound)
        await soon("the Fade's Levels tab follows its target's sound") { self.tabs().last == "fadeLevels" }
        try await show(Self.movie, tab: "levels")
        XCTAssertEqual(inspector.audioLevels.levels?.master, -7)
    }

    // MARK: Flow 3

    /// A Video cue with sound and its patch set to none is broken with that reason and does not
    /// start; a patch clears it; a patch with no device here breaks it as it breaks an Audio cue,
    /// cleared when the patch has its device again. A silent movie is never broken by patches.
    func testSoundButNowhereToPlayItMarksTheCueBroken() async throws {
        try await openSoundShow()
        let inspector = try await show(Self.movie, tab: "io")
        XCTAssertNil(status(Self.movie))
        choose(inspector.outputPatchPopup, loc("inspector.io.patchNone"))
        await soon("broken: sound and no patch") {
            self.brokenTip(Self.movie)?.contains(loc("problem.no_patch")) == true
        }
        XCTAssertTrue(warningRows(Self.movie).contains { $0.detail.contains(loc("problem.no_patch")) })
        XCTAssertEqual(inspector.outputPatchPopup.titleOfSelectedItem, loc("inspector.io.patchNone"))
        XCTAssertFalse(inspector.editPatchButton.isEnabled, "no patch to edit")
        try await show(Self.movie, tab: "levels")
        XCTAssertFalse(inspector.audioLevels.editPatchButton.isEnabled, "nor from the Levels tab")
        XCTAssertTrue(inspector.audioLevels.defaultButton.isEnabled, "its levels are still edited")
        try await perform(.startSelected, on: Self.movie)
        try await play(0.5)
        XCTAssertEqual(status(Self.movie), loc("cue.state.broken"), "GO does not start it")
        let none = await shownFrame()
        XCTAssertNil(none, "no picture either")
        // GO at the playhead passes it as it passes any broken cue: nothing starts.
        click(Self.movie)
        try await go()
        XCTAssertNotEqual(status(Self.movie), loc("cue.state.running"))
        try await perform(.panic, on: Self.movie)
        try await perform(.panic, on: Self.movie)

        try await show(Self.movie, tab: "io")
        choose(inspector.outputPatchPopup, "Main PA")
        await soon("the mark clears") { self.status(Self.movie) == nil }
        XCTAssertTrue(inspector.audioLevels.editPatchButton.isEnabled)
        XCTAssertEqual(levels(Self.movie)?.outputs.count, 2, "its levels were kept")

        // The patch loses its device on this machine: the Audio cue's device problem.
        guard case .success(let ok) = await store.client.send({ .request(id: $0, .listPatchMappings) }),
            case .patchMappings(let entries)? = ok.data,
            let mapping = entries.first(where: { $0.patch == Self.mainPA })?.mapping
        else { return XCTFail("the patch's mapping") }
        let audio = await wc.editor.add(.audio)
        let audioID = try XCTUnwrap(audio)
        _ = await store.setPatchMapping(Self.mainPA, nil)
        await soon("broken: no device") { self.brokenTip(Self.movie)?.contains("Main PA") == true }
        await soon { self.brokenTip(audioID)?.contains("Main PA") == true }
        // The Audio cue (with no file) says that after its own first reason.
        let line = try XCTUnwrap(brokenTip(audioID)?.split(separator: "\n").last)
        XCTAssertTrue(line.contains("Main PA"))
        XCTAssertTrue(brokenTip(Self.movie)?.hasSuffix(line) == true, "the same device problem an Audio cue shows")
        XCTAssertNil(status(Self.long), "a silent movie is never broken by patches")
        _ = await store.setPatchMapping(Self.mainPA, mapping)
        await soon("cleared when the patch has a device") { self.status(Self.movie) == nil }
        XCTAssertNil(status(Self.long))
    }

    // MARK: Flow 4

    /// A movie with a stereo and a 5.1 track: the sound-format line is a pop-up, one item per
    /// track, on the first. The second picked: the Levels rows follow its channels, GO plays it,
    /// undo returns the first. A stored track the file no longer has plays the first with a warning.
    func testAMovieWithSeveralAudioTracksAPopUpPicksTheTrack() async throws {
        var single: URL!
        try await openSoundShow(
            seconds: 4, sound: "pcm:2:48000+pcm:6:44100",
            files: { media in
                single = media.appendingPathComponent("single.mov")
                try writeTestMedia(single, seconds: 4, sound: Self.stereo)
            })
        let inspector = try await show(Self.movie, tab: "io")
        let v = inspector.videoIO
        let pcm = loc("inspector.io.audioCodec.lpcm")
        func item(_ n: Int, _ channels: Int, _ rate: String) -> String {
            String(
                format: loc("inspector.io.trackItem"), n,
                String(
                    format: loc("inspector.io.soundFormatText"), pcm,
                    String(format: loc("inspector.io.channels.many"), channels), rate))
        }
        XCTAssertFalse(v.trackPopup.isHidden, "several tracks: a pop-up")
        XCTAssertTrue(v.soundLabel.isHidden)
        XCTAssertEqual(v.trackPopup.itemTitles, [item(1, 2, "48"), item(2, 6, "44.1")])
        XCTAssertEqual(v.trackPopup.indexOfSelectedItem, 0, "the first track is chosen")
        XCTAssertEqual(inspector.audioLevels.matrix.inputs, 2)

        choose(v.trackPopup, item(2, 6, "44.1"))
        await soon("stored") { self.kind(Self.movie)?["audio_track"]?.doubleValue == 1 }
        await soon("the Levels rows follow the track") { inspector.audioLevels.matrix.inputs == 6 }
        XCTAssertEqual(v.trackPopup.indexOfSelectedItem, 1)
        _ = await store.undo()
        await soon("undo returns the first") {
            self.kind(Self.movie)?["audio_track"]?.doubleValue == 0 && v.trackPopup.indexOfSelectedItem == 0
                && inspector.audioLevels.matrix.inputs == 2
        }
        _ = await store.redo()
        await soon { inspector.audioLevels.matrix.inputs == 6 && v.trackPopup.indexOfSelectedItem == 1 }

        // The choice is saved with the show: the file holds it, and a copy of the saved show opened
        // afresh plays (and so shows) the second track.
        let saved = await app.saveWorkspace(store)
        XCTAssertTrue(saved)
        let copy = engine.folder.appendingPathComponent("reopened-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.copyItem(at: folder, to: copy)
        let reopened = Rig(
            engine: engine, workspace: try await engine.open(path: copy.appendingPathComponent("Video.openlab").path))
        let connected = await reopened.connect()
        XCTAssertTrue(connected)
        XCTAssertEqual(reopened.store.model?.cue(Self.movie)?.raw["kind"]?["audio_track"]?.doubleValue, 1)
        let probed = await eventually {
            try? await self.play(0)
            await reopened.sync()
            return reopened.store.mediaDurations[Self.movie]?.audioTrack == 1
        }
        XCTAssertTrue(probed, "the reopened show plays its second track")
        XCTAssertEqual(reopened.store.mediaDurations[Self.movie]?.fileChannels, 6)
        reopened.client.stop()

        // GO plays the second track: its sixth channel (a constant 5/64) is in no other track.
        let panel = try await show(Self.movie, tab: "levels").audioLevels
        fire(panel.silentButton)
        typeLevel(panel, .master, "0")
        typeLevel(panel, .output(1), "0")
        typeLevel(panel, .cross(input: 5, output: 1), "0")
        await soon { self.levels(Self.movie)?.crosspoints[.init(input: 5, output: 1)]?.db == 0 }
        try await perform(.startSelected, on: Self.movie)
        // Past the start, where the 44.1 kHz track's first samples ring on the 48 kHz device.
        try await play(0.5)
        let sixth = try await loudest(.cue(Self.movie), channel: 1, over: 0.5)
        XCTAssertEqual(sixth, 5.0 / 64, accuracy: 0.01, "the second track's sixth channel is heard")
        try await perform(.stopSelected, on: Self.movie)
        await soon { self.status(Self.movie) == nil }

        // The file replaced by one with a single track: the stored choice is stale.
        await wc.editor.replaceFile(of: Self.movie, with: single)
        await soon("a warning says the first track plays") {
            self.warningRows(Self.movie).contains {
                $0.detail.contains(loc("warning.notice.audio_track_missing"))
            }
        }
        XCTAssertNil(status(Self.movie), "the cue is not broken")
        try await show(Self.movie, tab: "io")
        XCTAssertFalse(v.trackPopup.isHidden, "the pop-up stays, to put it right")
        let missing = String(format: loc("inspector.io.trackMissing"), 2)
        XCTAssertEqual(v.trackPopup.itemTitles, [item(1, 2, "48"), missing])
        XCTAssertEqual(v.trackPopup.titleOfSelectedItem, missing, "the stored track is shown, named as missing")
        XCTAssertFalse(v.trackNote.isHidden, "a line says which track plays")
        XCTAssertEqual(v.trackNote.stringValue, loc("inspector.io.trackMissingNote"))
        XCTAssertEqual(store.mediaDurations[Self.movie]?.audioTrack, 0, "the first track plays")
        // Picking the missing track's own item changes nothing.
        choose(v.trackPopup, missing)
        await sync()
        XCTAssertEqual(kind(Self.movie)?["audio_track"]?.doubleValue, 1)
        XCTAssertEqual(v.trackPopup.titleOfSelectedItem, missing)
        choose(v.trackPopup, item(1, 2, "48"))
        await soon("the warning goes") { self.warningRows(Self.movie).isEmpty }
        await soon("one track: plain text") { v.trackPopup.isHidden && !v.soundLabel.isHidden }
        XCTAssertTrue(v.trackNote.isHidden)
    }

    // MARK: Flow 5

    /// While the picture plays on inside a loop: the main level typed, an output muted and two
    /// levels ganged are each heard at once, and the picture never restarts or jumps; paused, an
    /// edit is heard on resume. The edits undo, limits apply, Set Default Levels and Paste Cue
    /// Properties (Levels, from an Audio cue and back) work.
    func testLevelsAndMutesChangeTheSoundLiveWhileThePicturePlaysOn() async throws {
        try await openSoundShow(
            seconds: 20,
            show: { ws in
                Self.editKind(&ws, cue: 0) { kind in
                    kind.set("end_time", .double(10))
                    kind.set("last_slice_play_count", .int(0))
                }
            })
        try await perform(.startSelected, on: Self.movie)
        try await play(10.5)
        let panel = try await show(Self.movie, tab: "levels").audioLevels
        XCTAssertEqual(panel.meter.source, .cue(Self.movie), "the tab's meter watches the cue")
        let full = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertGreaterThan(full, 0.4)

        /// The picture went on by the time played (inside the ten-second loop), never back to its start.
        var last = await picturePosition() ?? -1
        func pictureWentOn(_ seconds: Double, _ what: String, line: UInt = #line) async {
            let now = await picturePosition() ?? -1
            let gone = (now - last + 10).truncatingRemainder(dividingBy: 10)
            XCTAssertEqual(gone, seconds, accuracy: 0.09, "\(what): the picture plays on", line: line)
            last = now
        }
        typeLevel(panel, .master, "-20")
        await soon { self.levels(Self.movie)?.master == -20 }
        // The edit reaches the mixer with the next block of sound.
        try await play(0.1)
        let quieter = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(quieter / full, 0.1, accuracy: 0.02, "the main level is heard at once")
        await pictureWentOn(1.1, "main level")

        panel.select(.output(0))
        panel.muteCheck.state = .on
        fire(panel.muteCheck)
        await soon { self.levels(Self.movie)?.outputs.first?.mute == true }
        try await play(0.1)
        let muted = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(muted, 0, "a muted output is silent at once")
        await pictureWentOn(1.1, "mute")
        _ = await store.undo()
        await soon("the mute is undone") { self.levels(Self.movie)?.outputs.first?.mute == false }
        _ = await store.undo()
        await soon("and the main level") { self.levels(Self.movie)?.master == 0 }
        try await play(0.1)
        let again = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(again, full, accuracy: 0.05)

        // Two outputs ganged: one typed moves both.
        for o in 0..<2 {
            panel.select(.output(o))
            panel.gangField.stringValue = "A"
            panel.controlTextDidChange(
                Notification(name: NSControl.textDidChangeNotification, object: panel.gangField))
            panel.controlTextDidEndEditing(
                Notification(name: NSControl.textDidEndEditingNotification, object: panel.gangField))
        }
        await soon("ganged") { self.levels(Self.movie)?.gangs == [[0, 1]] }
        typeLevel(panel, .output(0), "-6")
        await soon("both moved") { self.levels(Self.movie)?.outputs.map(\.db) == [-6, -6] }
        try await play(0.1)
        let ganged = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(ganged / full, 0.5, accuracy: 0.04, "heard at once")
        await pictureWentOn(2.2, "gang")

        // Paused: an edit is heard on resume.
        try await perform(.pauseResumeSelected, on: Self.movie)
        await soon("paused") { self.status(Self.movie) == loc("cue.state.paused") }
        try await play(0.3)
        let held = await picturePosition()
        typeLevel(panel, .master, "-20")
        await soon { self.levels(Self.movie)?.master == -20 }
        let silent = try await loudest(.cue(Self.movie), over: 0.5)
        XCTAssertEqual(silent, 0, "nothing sounds while paused")
        let still = await picturePosition()
        XCTAssertEqual(still, held, "the frame holds")
        try await perform(.pauseResumeSelected, on: Self.movie)
        try await play(0.2)
        let resumed = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(resumed / full, 0.05, accuracy: 0.015, "the level set while paused, on resume")
        try await perform(.stopSelected, on: Self.movie)

        // Set Default Levels.
        fire(panel.defaultButton)
        await soon("a new cue's levels") {
            let l = self.levels(Self.movie)
            return l?.master == 0 && l?.outputs.map(\.db) == [0, 0] && l?.crosspoints.count == 2
        }

        // Paste Cue Properties, Levels: from an Audio cue onto the Video cue, and back.
        wc.presentSheet = { _ in }
        wc.pasteDefaults = UserDefaults(suiteName: "openlab-test-\(UUID().uuidString)") ?? .standard
        wc.editor.pasteboard = NSPasteboard.withUniqueName()
        let audio = await wc.editor.add(.audio)
        let audioID = try XCTUnwrap(audio)
        try await show(audioID, tab: "levels")
        typeLevel(panel, .master, "-9")
        await soon { self.levels(audioID)?.master == -9 }
        func pasteLevels(from source: ModelID, onto cue: ModelID) async throws {
            let copied = await wc.editor.copySelection(of: [source])
            XCTAssertTrue(copied)
            wc.cueList.selectAsUser([cue])
            wc.promptPasteProperties(onto: [cue])
            let sheet = try XCTUnwrap(wc.pasteSheet, "the sheet is up")
            XCTAssertNotNil(sheet.propertyChecks[.levels], "Levels is offered")
            for p in PasteProperty.allCases {
                guard let box = sheet.propertyChecks[p] else { continue }
                if (box.state == .on) != (p == .levels) { box.performClick(nil) }
            }
            sheet.applyButton.performClick(nil)
        }
        try await pasteLevels(from: audioID, onto: Self.movie)
        await soon("the Audio cue's levels are the Video cue's") { self.levels(Self.movie)?.master == -9 }
        try await show(Self.movie, tab: "levels")
        typeLevel(panel, .master, "-3")
        await soon { self.levels(Self.movie)?.master == -3 }
        try await pasteLevels(from: Self.movie, onto: audioID)
        await soon("and back") { self.levels(audioID)?.master == -3 }

        // The workspace's level limits, as for an Audio cue: a lower maximum holds the cue's levels
        // and what is typed over it; a level at or under the minimum is silence.
        let capped = await WorkspaceSettingsModel(store: store).setOutputMax("-6")
        XCTAssertTrue(capped)
        try await show(Self.movie, tab: "levels")
        await soon("the fader stops at the maximum") { panel.fader.maxValue == -6 }
        typeLevel(panel, .master, "+6")
        await soon("a level typed over the maximum is held at it") { self.levels(Self.movie)?.master == -6 }
        await soon("its other levels are held too") { self.levels(Self.movie)?.outputs.map(\.db) == [-6, -6] }
        typeLevel(panel, .master, "-200")
        await soon("under the minimum is silence") {
            (self.levels(Self.movie)?.master ?? 0) <= LevelText.silence
        }
    }

    // MARK: Flow 6

    /// A Fade made on a Video cue with sound has its Levels tab and copies the cue's levels. Run,
    /// it takes the sound to silence over its time while the picture stays at full opacity; a
    /// second Fade takes over from one still running; with "stop target when done" picture and
    /// sound end with the Fade. A Fade aimed at a silent Video cue has no Levels tab.
    func testAFadeCueFadesAVideoCuesSound() async throws {
        try await openSoundShow(seconds: 20)
        click(Self.movie)
        let made = await wc.editor.add(.fade)
        let down = try XCTUnwrap(made)
        await soon("the Fade aims at the Video cue") {
            self.kind(down)?["target"]?.stringValue == Self.movie.raw
        }
        let inspector = try await show(Self.movie, tab: "levels")
        typeLevel(inspector.audioLevels, .master, "-4")
        await soon { self.levels(Self.movie)?.master == -4 }
        try await show(down, tab: "fadeLevels")
        XCTAssertEqual(tabs(), ["basics", "triggers", "curve", "fadeLevels"], "as for an Audio target")
        let fadePanel = inspector.fadeLevels
        XCTAssertTrue(fadePanel.fromTargetButton.isEnabled)
        fire(fadePanel.fromTargetButton)
        await soon("the Video cue's levels are copied") { self.levels(down)?.master == -4 }
        XCTAssertEqual(fadePanel.matrix.inputs, 2, "the target's rows")
        typeLevel(fadePanel, .master, "-inf")
        await soon { (self.levels(down)?.master ?? 0) <= LevelText.silence }
        guard case .success = await store.send(command: .setProperty(down, .duration(5))) else {
            return XCTFail("duration")
        }
        // A second Fade, back up over one second.
        click(Self.movie)
        let second = await wc.editor.add(.fade)
        let up = try XCTUnwrap(second)
        try await show(up, tab: "fadeLevels")
        typeLevel(fadePanel, .master, "-4")
        await soon { self.levels(up)?.master == -4 }
        guard case .success = await store.send(command: .setProperty(up, .duration(1))) else {
            return XCTFail("duration")
        }

        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        let full = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertGreaterThan(full, 0.25)
        try await perform(.startSelected, on: down)
        try await play(1.5)
        let falling = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertLessThan(falling, full * 0.9, "the sound follows the Fade down")
        XCTAssertGreaterThan(falling, 0, "and is not silent yet")
        // The second Fade takes over while the first still runs.
        try await perform(.startSelected, on: up)
        try await play(1.1)
        let back = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(back, full, accuracy: 0.03, "the second Fade brought it back")
        try await play(2)
        let stays = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertEqual(stays, full, accuracy: 0.03, "the first Fade no longer moves the level it lost")

        // The whole fade: silent at five seconds, the picture on at full opacity.
        try await perform(.startSelected, on: down)
        try await play(5.4)
        let faded = try await loudest(.cue(Self.movie), over: 0.5)
        XCTAssertEqual(faded, 0, "silent when the Fade is done")
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "the picture plays on")
        let red = try await red()
        XCTAssertGreaterThan(red, 200, "at full opacity")
        try await perform(.stopSelected, on: Self.movie)
        await soon { self.status(Self.movie) == nil }

        // Stop target when done: picture and sound end when the Fade does.
        try await show(down, tab: "curve")
        inspector.fadeSettings.stopCheck.state = .on
        fire(inspector.fadeSettings.stopCheck)
        await soon { self.kind(down)?["stop_target_when_done"] == .bool(true) }
        try await perform(.startSelected, on: Self.movie)
        try await play(0.5)
        try await perform(.startSelected, on: down)
        try await play(5.2)
        await soon("the Video cue ended with the Fade") { self.status(Self.movie) == nil }
        let gone = await shownFrame()
        XCTAssertNil(gone, "its picture left the stage")

        // A Fade aimed at a silent Video cue: no Levels tab.
        click(Self.long)
        let third = await wc.editor.add(.fade)
        let quiet = try XCTUnwrap(third)
        click(quiet)
        await soon("no Levels tab for a silent target") {
            inspector.cue?.id == quiet && self.kind(quiet)?["target"]?.stringValue == Self.long.raw
                && self.tabs() == ["basics", "triggers", "curve"]
        }
    }

    // MARK: Flow 7

    /// From GO to the end the frame on the stage and the sound on the cue's meter are at the same
    /// place of the file; the cue ends once, picture and sound together, and the Duration cell
    /// shows the movie's length.
    func testPictureAndSoundStayWithinOneFrameOfEachOther() async throws {
        try await openSoundShow(seconds: 6)
        XCTAssertEqual(duration(Self.movie), 6, "the Duration cell shows the movie's length")
        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        try await play(0.4)
        for second in 0..<6 {
            try await expectInStep(Self.movie, near: Double(second) + 0.5, "\(second).5 s in")
            try await play(0.9)
        }
        await soon("the cue ended") { self.status(Self.movie) == nil }
        let after = await shownFrame()
        XCTAssertNil(after, "the last frame left")
        let silent = try await loudest(.cue(Self.movie), over: 0.3)
        XCTAssertEqual(silent, 0, "with the last of the sound")
        XCTAssertNil(store.progress[Self.movie], "it ended once")
        XCTAssertEqual(duration(Self.movie), 6)
    }

    // MARK: Flow 8

    /// Pause holds the frame and the sound; resume, a load to time, a loop's seam and hold at end
    /// each keep picture and sound at the same place.
    func testPauseResumeLoadToTimeAndALoopSeamKeepThemTogether() async throws {
        try await openSoundShow(seconds: 6)
        // (a) Pause and resume.
        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        try await play(1.3)
        try await perform(.pauseResumeSelected, on: Self.movie)
        await soon("paused") { self.status(Self.movie) == loc("cue.state.paused") }
        try await play(0.3)
        let held = await picturePosition()
        let quiet = try await loudest(.cue(Self.movie), over: 0.5)
        XCTAssertEqual(quiet, 0, "the sound stopped")
        let later = await picturePosition()
        XCTAssertEqual(later, held, "the frame holds")
        XCTAssertEqual(held ?? -1, 1.3, accuracy: 0.09)
        try await perform(.pauseResumeSelected, on: Self.movie)
        try await play(0.1)
        try await expectInStep(Self.movie, near: 1.5, "resumed")
        try await perform(.stopSelected, on: Self.movie)
        await soon { self.status(Self.movie) == nil }

        // (b) Load to time, then GO: both from there.
        try await perform(.loadSelected, on: Self.movie)
        XCTAssertNil(wc.applyLoadTime("3", to: Self.movie))
        await sync()
        try await play(0)
        await soon("loaded") { self.status(Self.movie) == loc("cue.state.loaded") }
        try await perform(.startSelected, on: Self.movie)
        let first = await picturePosition()
        XCTAssertEqual(first ?? -1, 3, accuracy: 0.05, "the first frame shown is from three seconds in")
        try await play(0.3)
        try await expectInStep(Self.movie, near: 3.4, "after a load to time")
        try await perform(.stopSelected, on: Self.movie)
        await soon { self.status(Self.movie) == nil }

        // (c) Trims with a play count of three: at each seam both jump to the range's start.
        let panel = try await timeLoopsTab()
        try typeInto(panel.startField, "1")
        try typeInto(panel.endField, "3")
        try typeInto(panel.countField, "3")
        await soon("three passes") { self.duration(Self.movie) == 6 }
        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        try await play(1.3)
        try await expectInStep(Self.movie, near: 2.4, "the first pass")
        try await play(0.8)
        try await expectInStep(Self.movie, near: 1.3, "after the first seam")
        try await play(1.9)
        try await expectInStep(Self.movie, near: 1.3, "after the second seam")
        try await play(1.8)
        await soon("ended after the third pass") { self.status(Self.movie) == nil }

        // (d) Hold at end: the sound has ended, the picture stays, the cue stays active.
        panel.holdCheck.state = .on
        fire(panel.holdCheck)
        await soon { self.movieField("hold_at_end") == .bool(true) }
        try await perform(.startSelected, on: Self.movie)
        try await play(6.3)
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "the cue stays active")
        let last = await shownFrame()
        XCTAssertEqual(Double(last ?? 0), 74, accuracy: 1, "on the range's last frame")
        let ended = try await loudest(.cue(Self.movie), over: 0.5)
        XCTAssertEqual(ended, 0, "no sound restarts")
    }

    // MARK: Flow 9

    /// Esc fades the sound with the picture and both are gone together; Esc twice cuts both; panic
    /// on the selected cue leaves another Video cue alone; a hard stop cuts at once; GO during the
    /// panic fade starts the cue again from its top.
    func testStopAndPanicFadeTheSoundWithThePicture() async throws {
        try await openSoundShow(
            seconds: 20,
            files: { media in
                try writeTestMedia(media.appendingPathComponent(Self.longName), seconds: 20, sound: Self.stereo)
            },
            show: { ws in
                Self.patch(&ws, cue: 4, on: Self.mainPA, outputs: 2)
                Self.editKind(&ws, cue: 4) { $0.set("stage", .string(Self.stage.raw)) }
            })
        await soon { self.store.mediaDurations[Self.long]?.hasSound == true }
        let panicTime = store.model?.settings["panic_duration"]?.doubleValue ?? 0
        XCTAssertGreaterThan(panicTime, 0.5, "the workspace's panic time")

        // Esc: the sound falls with the picture, both gone at the panic time.
        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        let full = try await loudest(.cue(Self.movie), over: 1)
        let bright = try await red()
        try await perform(.panic, on: Self.movie)
        try await play(panicTime / 2 - 0.05)
        let half = try await loudest(.cue(Self.movie), over: 0.1)
        let dim = try await red()
        XCTAssertLessThan(half, full, "the sound is fading")
        XCTAssertGreaterThan(half, 0)
        XCTAssertLessThan(Double(dim), Double(bright) * 0.8, "the picture fades with it")
        XCTAssertGreaterThan(dim, 10)
        try await play(panicTime / 2 + 0.1)
        await soon("idle after the panic time") { self.status(Self.movie) == nil }
        let after = await shownFrame()
        XCTAssertNil(after)
        let silent = try await loudest(.cue(Self.movie), over: 0.3)
        XCTAssertEqual(silent, 0)

        // Esc twice: both cut at once.
        try await perform(.startSelected, on: Self.movie)
        try await play(0.5)
        try await perform(.panic, on: Self.movie)
        try await perform(.panic, on: Self.movie)
        await soon("cut") { self.status(Self.movie) == nil }
        // The cut is carried out with the next block of sound.
        try await play(0.1)
        let cut = await shownFrame()
        XCTAssertNil(cut)
        let cutSound = try await loudest(.cue(Self.movie), over: 0.2)
        XCTAssertEqual(cutSound, 0)

        // Panic on the selected cue only leaves the other playing, picture and sound.
        try await perform(.startSelected, on: Self.movie)
        try await perform(.startSelected, on: Self.long)
        try await play(0.5)
        try await perform(.panicSelected, on: Self.movie)
        try await play(panicTime + 0.2)
        await soon("the selected cue is gone") { self.status(Self.movie) == nil }
        XCTAssertEqual(status(Self.long), loc("cue.state.running"), "the other cue is untouched")
        let other = try await loudest(.cue(Self.long), over: 1)
        XCTAssertEqual(other, full, accuracy: 0.05)
        let frame = await shownFrame()
        XCTAssertNotNil(frame, "its picture is on the stage")

        // A hard stop cuts sound and picture at once.
        try await perform(.stopSelected, on: Self.long)
        await soon("stopped") { self.status(Self.long) == nil }
        try await play(0.1)
        let stopped = await shownFrame()
        XCTAssertNil(stopped)
        let stoppedSound = try await loudest(.cue(Self.long), over: 0.2)
        XCTAssertEqual(stoppedSound, 0)

        // GO during the panic fade: the cue starts again from its top, picture and sound.
        try await perform(.startSelected, on: Self.movie)
        try await play(2)
        try await perform(.panicSelected, on: Self.movie)
        try await play(panicTime / 3)
        try await perform(.startSelected, on: Self.movie)
        try await play(0.3)
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"))
        try await expectInStep(Self.movie, near: 0.4, "restarted during its panic fade")
        try await play(panicTime)
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "the old fade does not end the new run")
    }

    // MARK: Flow 10

    /// The audio device is lost while a Video cue with sound plays: its row shows the paused mark
    /// with its time standing still, its picture leaves the stage and the Warnings list says the
    /// device is lost. The device back: nothing resumes, and GO starts the cue from its top with
    /// picture and sound together.
    func testTheAudioDeviceIsLostMidCue() async throws {
        try await openSoundShow(seconds: 20)
        try await show(Self.movie, tab: "levels")
        try await perform(.startSelected, on: Self.movie)
        try await play(1)
        let before = await shownFrame()
        XCTAssertNotNil(before)
        guard case .success = await engine.request(.loseDevice) else { return XCTFail("lose the device") }
        try await play(0.3)
        await soon("the paused mark") { self.status(Self.movie) == loc("cue.state.paused") }
        let stood = duration(Self.movie)
        try await play(0.5)
        XCTAssertEqual(duration(Self.movie), stood, "its time stands still")
        XCTAssertNotNil(stood)
        let lost = await shownFrame()
        XCTAssertNil(lost, "its picture left the stage")
        await soon("the Warnings list names the lost device") {
            WarningRows.make(store: self.store).contains { $0.detail == loc("warnings.device.device_lost") }
        }
        XCTAssertFalse(wc.footer.warningsButton.isHidden)

        // The device back: nothing resumes by itself.
        guard case .success = await engine.request(.restoreDevice) else { return XCTFail("restore the device") }
        try await play(2.5)
        await soon("the device warning clears") {
            !WarningRows.make(store: self.store).contains { $0.detail == loc("warnings.device.device_lost") }
        }
        XCTAssertNotEqual(status(Self.movie), loc("cue.state.running"), "nothing resumes")
        let quiet = try await loudest(.cue(Self.movie), over: 0.5)
        XCTAssertEqual(quiet, 0)
        let none = await shownFrame()
        XCTAssertNil(none)
        XCTAssertTrue(store.problems[Self.movie]?.isEmpty ?? true, "no patch problem is left")

        // GO starts it from the beginning, in step.
        try await perform(.startSelected, on: Self.movie)
        await soon("running again") { self.status(Self.movie) == loc("cue.state.running") }
        try await play(0.3)
        try await expectInStep(Self.movie, near: 0.4, "started again from the top")
    }

    // MARK: Flow 11

    /// A movie whose audio track cannot be decoded, or has more channels than a cue can route, is
    /// broken from its probe with a reason and does not start; one cut short while it plays goes
    /// broken when the damage is met; in each case another Video cue's picture and sound go on.
    func testADamagedOrUnplayableAudioTrackBreaksOnlyThatCue() async throws {
        /// `data` with `new` written `offset` bytes after the first `marker`.
        func patched(_ data: Data, after marker: String, offset: Int, _ new: [UInt8]) throws -> Data {
            let at = try XCTUnwrap(data.range(of: Data(marker.utf8))).lowerBound + offset
            var out = data
            out.replaceSubrange(at..<at + new.count, with: new)
            return out
        }
        var cut: URL!
        try await openSoundShow(
            seconds: 10,
            files: { media in
                let good = try Data(contentsOf: media.appendingPathComponent("clip.mov"))
                // An unknown sample format, and a channel count over what a cue can route.
                try patched(good, after: "lpcm", offset: 0, Array("zzzz".utf8))
                    .write(to: media.appendingPathComponent("codec.mov"))
                try patched(good, after: "lpcm", offset: 44, [0, 0, 0, 65])
                    .write(to: media.appendingPathComponent("wide.mov"))
                cut = media.appendingPathComponent(Self.longName)
                try good.write(to: cut)
            },
            show: { ws in
                Self.patch(&ws, cue: 4, on: Self.mainPA, outputs: 2)
                Self.editKind(&ws, cue: 4) { $0.set("stage", .string(Self.stage.raw)) }
                // The two stills' cues play the unplayable movies instead.
                Self.editKind(&ws, cue: 1) { $0.set("media", .string("media/codec.mov")) }
                Self.editKind(&ws, cue: 2) { $0.set("media", .string("media/wide.mov")) }
            })
        // (a), (c): broken from the probe, naming the track; GO does not start them.
        for (cue, file) in [(Self.still, "codec.mov"), (Self.timedStill, "wide.mov")] {
            await soon("\(file): broken from its probe") { self.status(cue) == loc("cue.state.broken") }
            let tip = brokenTip(cue) ?? ""
            XCTAssertTrue(tip.contains(file), "the mark's reason names the file: \(tip)")
            let problem = (store.problems[cue] ?? []).first { $0.type == "media_unsupported" }
            XCTAssertEqual(
                problem.map { ProblemText.problem($0, label: { $0.raw }) }.map(tip.contains), true,
                "\(file): cannot be played")
            XCTAssertTrue(
                problem?.json["reason"]?.stringValue?.contains("audio track 1") == true,
                "the engine's reason names the track")
            try await perform(.startSelected, on: cue)
            try await play(0.3)
            XCTAssertEqual(status(cue), loc("cue.state.broken"), "\(file) does not start")
            let none = await shownFrame()
            XCTAssertNil(none, "no silent picture")
        }
        XCTAssertNil(status(Self.movie), "the good cue is playable")
        try await show(Self.movie, tab: "levels")

        // (b): damage met part-way breaks that cue alone.
        await soon { self.store.mediaDurations[Self.long]?.hasSound == true }
        try await perform(.startSelected, on: Self.long)
        try await play(0.5)
        XCTAssertEqual(status(Self.long), loc("cue.state.running"), "it plays to the damage")
        let handle = try FileHandle(forWritingTo: cut)
        try handle.truncate(atOffset: UInt64(try Data(contentsOf: cut).count / 2))
        try handle.close()
        try await perform(.startSelected, on: Self.movie)
        var broke = false
        for _ in 0..<100 where !broke {
            try await play(0.1)
            broke = status(Self.long) == loc("cue.state.broken")
        }
        XCTAssertTrue(broke, "the cut movie's cue breaks")
        XCTAssertFalse(warningRows(Self.long).isEmpty, "with a notice in the Warnings list")
        XCTAssertEqual(status(Self.movie), loc("cue.state.running"), "the other cue plays on")
        let heard = try await loudest(.cue(Self.movie), over: 1)
        XCTAssertGreaterThan(heard, 0.3, "its sound too")
        let frame = await shownFrame()
        XCTAssertNotNil(frame, "and its picture, the stage host still running")
    }

    // MARK: Other paths

    /// Several Video cues: with sound on all of them the inspector has their Levels tab and patch
    /// pop-up, and an edit goes to each; with a silent one among them it has neither. A pasted
    /// Video cue keeps its own patch and levels.
    func testSeveralVideoCuesWithSoundShareTheirLevelsTabAndPatch() async throws {
        try await openSoundShow(
            files: { media in
                try writeTestMedia(media.appendingPathComponent(Self.longName), seconds: 2, sound: Self.stereo)
            },
            show: { ws in
                Self.patch(&ws, cue: 4, on: Self.mainPA, outputs: 2)
                Self.addSidePatch(&ws)
                guard var patches = ws["patches"]?.arrayValue, var outputs = patches.first?["outputs"]?.arrayValue
                else { return XCTFail("the show has a patch") }
                for (n, name) in ["Left", "Right"].enumerated() { outputs[n].set("name", .string(name)) }
                patches[0].set("outputs", .array(outputs))
                ws.set("patches", .array(patches))
            })
        await soon { self.store.mediaDurations[Self.long]?.hasSound == true }
        let inspector = wc.inspector
        wc.cueList.selectAsUser([Self.movie, Self.long])
        await soon("both shown") { self.tabs() == ["basics", "triggers", "io", "levels"] }
        let v = inspector.videoIO
        XCTAssertEqual(v.patchRow?.isHidden, false)
        XCTAssertEqual(v.soundRow?.isHidden, false)
        XCTAssertFalse(v.soundLabel.isHidden)
        XCTAssertTrue(v.trackPopup.isHidden, "a track is picked one cue at a time")
        XCTAssertEqual(inspector.outputPatchPopup.titleOfSelectedItem, "Main PA")
        // One patch on both: its output names over the columns, and Edit (both tabs) opens it.
        XCTAssertEqual(inspector.audioLevels.matrix.outputNames, ["Left", "Right"])
        XCTAssertTrue(inspector.editPatchButton.isEnabled)
        XCTAssertTrue(inspector.audioLevels.editPatchButton.isEnabled)
        let openEditor = inspector.onEditPatch
        var edited: [ModelID] = []
        inspector.onEditPatch = { edited.append($0) }
        inspector.editPatchButton.performClick(nil)
        inspector.audioLevels.editPatchButton.performClick(nil)
        XCTAssertEqual(edited, [Self.mainPA, Self.mainPA])
        inspector.onEditPatch = openEditor
        // Two patches between them: no one patch to name the columns by or to edit.
        guard
            case .success = await store.send(
                command: .setProperty(Self.long, .kindFields(["patch": .string(Self.sidePatch.raw)])))
        else { return XCTFail("patch") }
        await soon("mixed patches") { !inspector.editPatchButton.isEnabled }
        XCTAssertFalse(inspector.audioLevels.editPatchButton.isEnabled)
        XCTAssertEqual(inspector.audioLevels.matrix.outputNames, [])
        _ = await store.undo()
        await soon { inspector.editPatchButton.isEnabled }
        typeLevel(inspector.audioLevels, .master, "-5")
        await soon("the level goes to both") {
            self.levels(Self.movie)?.master == -5 && self.levels(Self.long)?.master == -5
        }
        choose(inspector.outputPatchPopup, "Side fills")
        await soon("the patch goes to both") {
            [Self.movie, Self.long].allSatisfy { self.kind($0)?["patch"]?.stringValue == Self.sidePatch.raw }
        }
        _ = await store.undo()
        await soon("one undo step") {
            [Self.movie, Self.long].allSatisfy { self.kind($0)?["patch"]?.stringValue == Self.mainPA.raw }
        }
        // A silent cue among them: no Levels tab, no patch or sound row.
        wc.cueList.selectAsUser([Self.movie, Self.still])
        await soon("no Levels tab") { self.tabs() == ["basics", "triggers", "io"] }
        XCTAssertEqual(v.patchRow?.isHidden, true)
        XCTAssertEqual(v.soundRow?.isHidden, true)

        // Copy and paste: the copy has the cue's own patch and levels, not a new cue's.
        wc.editor.pasteboard = NSPasteboard.withUniqueName()
        guard
            case .success = await store.send(
                command: .setProperty(Self.movie, .kindFields(["patch": .string(Self.sidePatch.raw)])))
        else { return XCTFail("patch") }
        // A patch a Video cue names is kept, and the reason names the cue.
        let settings = WorkspaceSettingsModel(store: store)
        await soon { self.kind(Self.movie)?["patch"]?.stringValue == Self.sidePatch.raw }
        let removed = await settings.removePatch(Self.sidePatch)
        XCTAssertFalse(removed)
        XCTAssertEqual(
            settings.message, String(format: loc("settings.audio.patchInUse"), "Side fills", "1 clip.mov"))
        let copied = await wc.editor.copySelection(of: [Self.movie])
        XCTAssertTrue(copied)
        let before = wc.cueList.roots.count
        await wc.editor.paste(after: [Self.noStage])
        await soon("pasted") { self.wc.cueList.roots.count == before + 1 }
        let pasted = try XCTUnwrap(wc.cueList.roots.last?.id)
        XCTAssertEqual(kind(pasted)?["patch"]?.stringValue, Self.sidePatch.raw)
        XCTAssertEqual(levels(pasted)?.master, -5)
    }

    /// The Time & Loops timeline of a movie with sound draws the waveform of the audio track the
    /// cue plays (another track picked: another drawing), with the keep-last-frame box; a movie
    /// without sound keeps its plain ruler.
    func testTheTimelineShowsTheWaveformWhenTheFileHasSoundAndARulerWhenItHasNone() async throws {
        try await openSoundShow(sound: "pcm:2:48000+pcm:6:44100")
        let panel = try await timeLoopsTab()
        XCTAssertFalse(panel.silent, "there is sound to draw")
        XCTAssertFalse(panel.holdCheck.isHidden, "a Video cue's keep-last-frame box")
        await soon("the waveform is drawn") { panel.waveform.peaks != nil }
        XCTAssertEqual(panel.waveform.peaks?.channels.count, 2, "the first track's two channels")
        XCTAssertEqual(panel.waveform.duration, 4)
        guard
            case .success = await store.send(
                command: .setProperty(Self.movie, .kindFields(["audio_track": .int(1)])))
        else { return XCTFail("track") }
        await soon("the other track's waveform") { panel.waveform.peaks?.channels.count == 6 }
        // The silent movie: the ruler alone.
        click(Self.long)
        await soon("the silent movie shown") { panel.waveform.duration == 2 }
        XCTAssertTrue(panel.silent)
        XCTAssertNil(panel.waveform.peaks)
        XCTAssertFalse(panel.waveform.isReading)
    }

    /// At the least window size a Video cue's Timing & Loops tab fits as an Audio cue's does: the
    /// window keeps its width, with the sidebar shown too, and the keep-last-frame box is whole
    /// inside the tab.
    func testAVideoCuesTimingTabFitsTheLeastWindowAsAnAudioCuesDoes() async throws {
        try await openSoundShow()
        let audio = await wc.editor.add(.audio)
        let audioID = try XCTUnwrap(audio)
        let window = try XCTUnwrap(wc.window)
        let inspector = wc.inspector
        /// The window's width with `cue`'s Timing & Loops tab shown, from the least size.
        func width(showing cue: ModelID) async throws -> CGFloat {
            try await show(cue, tab: "io")
            window.setFrame(NSRect(origin: .zero, size: window.minSize), display: false)
            window.layoutIfNeeded()
            try await show(cue, tab: "timeLoops")
            window.layoutIfNeeded()
            return window.frame.width
        }
        let least = window.minSize.width
        let audioWidth = try await width(showing: audioID)
        let videoWidth = try await width(showing: Self.movie)
        XCTAssertEqual(audioWidth, least, "an Audio cue's tab fits the least window")
        XCTAssertEqual(videoWidth, least, "and a Video cue's: the window does not grow by itself")
        let hold = inspector.timeLoops.holdCheck
        XCTAssertFalse(hold.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(
            inspector.tabView.contentRect.contains(hold.convert(hold.bounds, to: inspector.tabView)),
            "the keep-last-frame box is whole inside the tab")
        XCTAssertGreaterThanOrEqual(hold.frame.width, hold.intrinsicContentSize.width, "and not cut")
        // With the sidebar shown: no wider than the Audio cue's tab makes the window.
        wc.footer.toggles[.sidebar]?.performClick(nil)
        XCTAssertTrue(wc.isVisible(.sidebar))
        let audioWide = try await width(showing: audioID)
        let videoWide = try await width(showing: Self.movie)
        XCTAssertLessThanOrEqual(videoWide, audioWide, "a Video cue's tab needs no more room than an Audio cue's")
    }
}
