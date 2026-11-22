// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The Levels tab (Audio cue) and the Fade tab (Fade cue) against the fake engine, which applies
/// `kind` edits, sends the `model_changed`, and merges edits that share a `coalesce` key.
/// Every number as a double: the wire's `-6` and `-6.0` are one value, but `JSONValue` keeps them apart.
func normalised(_ v: JSONValue?) -> JSONValue? {
    switch v {
    case .int(let i)?: return .double(Double(i))
    case .uint(let i)?: return .double(Double(i))
    case .array(let a)?: return .array(a.compactMap { normalised($0) })
    case .object(let o)?: return .object(o.compactMapValues { normalised($0) })
    default: return v
    }
}

@MainActor
final class InspectorLevelsTests: XCTestCase {
    static let intro = TestSnapshots.intro
    static let fade = TestSnapshots.fade

    /// The demo show on the real engine, the wire recorded (nil for the engine-free tests).
    private var rig: Rig!
    private var store: WorkspaceStore!
    private var vc: InspectorViewController!
    private var window: NSWindow!

    /// What only reads the model or the controls needs no engine (and runs in engine-free runs).
    private static let engineFree = [
        "testLevelsTabIsForAudioCuesAndFadeTabForFadeCues", "testMatrixShowsTheCueRouting", "testMatrixHitTesting",
        "testFadeTabShowsTheFadeSettings", "testSixtyFourOutputMatrixStaysResponsive",
        "testADragDoesNotEditARowTheMatrixNoLongerHas", "testLongOutputNamesAreCutInTheMiddleAndStayApart",
    ]

    override func setUp() async throws {
        // A host that is the active app (after another suite) reports a window not yet ordered
        // front as covered when a meter joins it, and its on-screen windows as covered: the
        // meters these tests show are made with the check that believes the window.
        let shipped = MeterView.defaultWindowIsCovered
        MeterView.defaultWindowIsCovered = { _, _ in false }
        addTeardownBlock { @MainActor in MeterView.defaultWindowIsCovered = shipped }
        if Self.engineFree.contains(where: { name.contains($0) }) {
            store = try Rig.offline(TestSnapshots.show()).store
        } else {
            rig = try await Rig.open()
            store = rig.store
        }
        vc = InspectorViewController(store: store)
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
    }

    override func tearDown() async throws {
        window?.close()
        rig?.client.stop()
        (window, vc, rig, store) = (nil, nil, nil, nil)
    }

    private var sent: [(command: JSONValue, coalesce: String?)] {
        rig.wire.sent.compactMap {
            if case .command(_, let c, let key) = $0 {
                return (JSONValue.parse((try? c.json.text()) ?? "") ?? .null, key)
            }
            return nil
        }
    }

    private func kindOf(_ command: JSONValue?) -> JSONValue? {
        guard command?["type"]?.stringValue == "set_cue_property",
            command?["property"]?["type"]?.stringValue == "kind_fields"
        else { return nil }
        return normalised(command?["property"]?["value"])
    }

    private var lastKind: JSONValue? { kindOf(sent.last?.command) }
    private var lastKindFields: [String: JSONValue]? { lastKind?.objectValue }

    private func modelKind(_ id: ModelID) -> JSONValue? { normalised(store.model?.cue(id)?.raw["kind"]) }

    private func tabs() -> [String] { vc.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    private func endEditing(_ field: NSTextField, _ owner: NSTextFieldDelegate) {
        owner.controlTextDidChange?(Notification(name: NSControl.textDidChangeNotification, object: field))
        owner.controlTextDidEndEditing?(Notification(name: NSControl.textDidEndEditingNotification, object: field))
    }

    private func type(_ panel: LevelsPanel, _ text: String) {
        panel.levelField.stringValue = text
        endEditing(panel.levelField, panel)
    }

    private func fire(_ control: NSControl) { _ = control.target?.perform(control.action, with: control) }

    private func drag(_ panel: LevelsPanel, to values: [Double]) {
        panel.beginGesture()
        for v in values {
            panel.fader.doubleValue = v
            fire(panel.fader)
        }
        panel.endGesture()
    }

    // MARK: Tabs

    func testLevelsTabIsForAudioCuesAndFadeTabForFadeCues() {
        vc.setSelection([Self.intro])
        XCTAssertEqual(tabs(), ["basics", "triggers", "io", "timeLoops", "levels"])
        vc.setSelection([Self.fade])
        XCTAssertEqual(tabs(), ["basics", "triggers", "curve", "fadeLevels"])
        vc.setSelection([TestSnapshots.group])
        XCTAssertEqual(tabs(), ["basics", "triggers", "mode"])
    }

    func testMatrixShowsTheCueRouting() {
        vc.setSelection([Self.intro])
        XCTAssertEqual(vc.audioLevels.matrix.outputCount, 2)
        XCTAssertEqual(vc.audioLevels.matrix.inputs, InspectorViewController.defaultInputs)
        XCTAssertEqual(vc.audioLevels.levelField.stringValue, "0.0", "the master is selected")
        XCTAssertEqual(vc.audioLevels.selection, .master)
    }

    // MARK: Matrix rows follow the file (AUD-06)

    /// Replaces the setUp rig with the demo show opened after `edit`, which can rewrite its files.
    private func reopen(_ edit: @escaping (URL) throws -> Void) async throws {
        rig.client.stop()
        rig = try await Rig.open("Demo", edit: edit)
        store = rig.store
        vc = InspectorViewController(store: store)
        window.contentView = vc.view
    }

    /// Selects the cue and brings its levels tab on screen (a tab that is not shown draws nothing and
    /// has no field editor).
    private func showCue(_ id: ModelID) {
        vc.setSelection([id])
        let tab = store.model?.cue(id)?.kindType == "fade" ? "fadeLevels" : "levels"
        vc.tabView.selectTabViewItem(withIdentifier: tab)
        vc.view.layoutSubtreeIfNeeded()
    }

    private func media(_ show: URL, _ name: String) -> URL { show.appendingPathComponent("media/\(name)") }

    /// Draws the matrix into a bitmap and checks the cell of each file-channel row is painted.
    /// Reads what is on screen, not what the view believes.
    private func renderedInputRows(_ m: LevelMatrixView) throws -> Int {
        let scale: CGFloat = 1
        let rep = try XCTUnwrap(
            NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: Int(m.bounds.width), pixelsHigh: Int(m.bounds.height),
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                bytesPerRow: 0, bitsPerPixel: 0))
        let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: rep))
        context.cgContext.setFillColor(NSColor(white: 0.5, alpha: 1).cgColor)
        context.cgContext.fill(CGRect(x: 0, y: 0, width: rep.pixelsWide, height: rep.pixelsHigh))
        context.cgContext.scaleBy(x: scale, y: scale)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        m.draw(m.bounds)  // a view in a window that is not on screen cannot display itself
        NSGraphicsContext.restoreGraphicsState()
        // The bitmap is not flipped, the view is: view y counts from the top, pixel rows from the top
        // of the drawn image, which is the view's bottom.
        func isDrawn(_ slot: LevelSet.Slot) -> Bool {
            let r = m.rect(of: slot)
            let (x, y) = (Int(r.minX) + 6, rep.pixelsHigh - 1 - Int(r.minY + 6))
            guard x >= 0, y >= 0, x < rep.pixelsWide, y < rep.pixelsHigh,
                let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB)
            else { return false }
            return abs(c.redComponent + c.greenComponent + c.blueComponent - 1.5) > 0.03  // not the grey behind it
        }
        var rows = 0
        while rows <= LevelSet.maxInputs, isDrawn(.cross(input: rows, output: 0)) { rows += 1 }
        return rows
    }

    /// The rows the matrix shows: drawn, and as tall as that many rows (the frame is what scrolls).
    private func assertRows(
        _ n: Int, _ m: LevelMatrixView, file: StaticString = #filePath, line: UInt = #line
    ) throws {
        XCTAssertEqual(try renderedInputRows(m), n, "rows drawn", file: file, line: line)
        // The captions over the crosspoint section take a band only when there are rows under them.
        let captions = n > 0 ? LevelMatrixView.captionH : 0
        XCTAssertEqual(
            m.frame.height, LevelMatrixView.headerH + captions + LevelMatrixView.cellH * CGFloat(n + 1), "frame",
            file: file, line: line)
    }

    private func relink(_ cue: ModelID, to path: String) async throws {
        guard case .success = await store.send(command: .setProperty(cue, .media(path))) else {
            return XCTFail("the relink was refused")
        }
    }

    func testAMultiChannelFileGivesTheMatrixOneRowPerChannel() async throws {
        try await reopen { try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6) }
        showCue(Self.intro)
        await assertEventually { self.vc.audioLevels.matrix.inputs == 6 }
        try assertRows(6, vc.audioLevels.matrix)
        XCTAssertEqual(vc.audioLevels.matrix.outputCount, 2, "the columns still come from the cue's outputs")
    }

    /// An Audio cue's I/O tab shows its file with Choose and the format the engine's probe read
    /// (the Video cue's rows), above the output patch; a file that is not there says it is not read.
    func testAnAudioCuesIOTabShowsItsFileAndTheFilesFormat() async throws {
        try await reopen { try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 1) }
        showCue(Self.intro)
        vc.tabView.selectTabViewItem(withIdentifier: "io")
        await assertEventually { self.store.mediaDurations[Self.intro]?.audioFile != nil }
        let io = vc.videoIO
        let format = try XCTUnwrap(store.mediaDurations[Self.intro]?.audioFile)
        XCTAssertEqual(format.codec, "pcm")
        XCTAssertEqual(format.channels, 1)
        await assertEventually { io.soundLabel.stringValue.hasPrefix("PCM, 1 channel, ") }
        XCTAssertTrue(io.fileLabel.stringValue.hasSuffix("a.wav"), io.fileLabel.stringValue)
        XCTAssertFalse(io.soundLabel.isHidden)
        XCTAssertTrue(io.trackPopup.isHidden)
        XCTAssertTrue(io.chooseButton.isEnabled)
        XCTAssertEqual(io.videoRows.map(\.isHidden), [true, true], "no picture format or stage")
        XCTAssertEqual(io.patchRow?.isHidden, false)
        vc.view.layoutSubtreeIfNeeded()
        XCTAssertGreaterThan(
            io.fileLabel.convert(io.fileLabel.bounds, to: nil).minY,
            io.soundLabel.convert(io.soundLabel.bounds, to: nil).maxY - 1, "the file above the format")

        showCue(TestSnapshots.radio)
        await rig.sync()
        XCTAssertNil(store.mediaDurations[TestSnapshots.radio], "its file is missing")
        XCTAssertEqual(io.soundLabel.stringValue, loc("inspector.io.formatUnknown"))
        XCTAssertEqual(io.fileLabel.textColor, .labelColor, "the path still shows")
    }

    func testAMonoFileGivesTheMatrixOneRow() async throws {
        try await reopen { try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 1) }
        showCue(Self.intro)
        await assertEventually { self.vc.audioLevels.matrix.inputs == 1 }
        try assertRows(1, vc.audioLevels.matrix)
    }

    func testAMissingFileKeepsTheStereoRowsAndNeverCrashes() async throws {
        showCue(TestSnapshots.radio)
        await rig.sync()
        XCTAssertNil(store.mediaDurations[TestSnapshots.radio], "its file is missing")
        try assertRows(InspectorViewController.defaultInputs, vc.audioLevels.matrix)
    }

    func testPointingTheCueAtAnotherFileChangesTheRows() async throws {
        try await reopen {
            try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6)
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 1)
        }
        showCue(Self.intro)
        await assertEventually { self.vc.audioLevels.matrix.inputs == 6 }
        try await relink(Self.intro, to: "media/d.wav")
        await assertEventually { self.vc.audioLevels.matrix.inputs == 1 }
        try assertRows(1, vc.audioLevels.matrix)
        // The file going away leaves the rows for an unknown file, not the old file's six or one.
        try await relink(Self.intro, to: "media/gone.wav")
        await assertEventually { self.vc.audioLevels.matrix.inputs == InspectorViewController.defaultInputs }
        try assertRows(InspectorViewController.defaultInputs, vc.audioLevels.matrix)
    }

    func testAFadeShowsTheRowsOfTheCueItFades() async throws {
        try await reopen { try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6) }
        showCue(Self.fade)
        await assertEventually { self.vc.fadeLevels.matrix.inputs == 6 }
        try assertRows(6, vc.fadeLevels.matrix)
    }

    func testAFadeFollowsItsTargetWhenTheTargetsFileChangesAfterItWasSelected() async throws {
        try await reopen {
            try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6)
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 3)
        }
        showCue(Self.fade)
        await assertEventually { self.vc.fadeLevels.matrix.inputs == 6 }
        try await relink(Self.intro, to: "media/d.wav")
        await assertEventually { self.vc.fadeLevels.matrix.inputs == 3 }
        try assertRows(3, vc.fadeLevels.matrix)
    }

    func testAFadeOfAGroupShowsTheMostChannelsOfTheCuesInsideIt() async throws {
        try await reopen {
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 3)
            try writeWAV(self.media($0, "r.wav"), seconds: 1, channels: 5)
            try editShow($0.appendingPathComponent("Demo.openlab")) { ws in
                guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue,
                    var kind = cues[1]["kind"]
                else { return XCTFail("the demo show has no fade") }
                kind.set("target", .string(TestSnapshots.group.raw))
                cues[1].set("kind", kind)
                lists[0].set("cues", .array(cues))
                ws.set("cue_lists", .array(lists))
            }
        }
        showCue(Self.fade)
        await assertEventually { self.vc.fadeLevels.matrix.inputs == 5 }
        try assertRows(5, vc.fadeLevels.matrix)
    }

    func testRowsHoldDuringAGestureAndFollowTheFileWhenItEnds() async throws {
        try await reopen {
            try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6)
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 1)
        }
        showCue(Self.intro)
        await assertEventually { self.vc.audioLevels.matrix.inputs == 6 }
        vc.audioLevels.beginGesture()
        try await relink(Self.intro, to: "media/d.wav")
        await assertEventually { self.store.mediaDurations[Self.intro]?.fileChannels == 1 }
        await rig.sync()
        XCTAssertEqual(vc.audioLevels.matrix.inputs, 6, "the grid does not move under a drag")
        vc.audioLevels.endGesture()
        await assertEventually { self.vc.audioLevels.matrix.inputs == 1 }
    }

    /// Another client sets the cue's master to `db`; returns once this app's model has it.
    private func peerSetsMaster(_ db: Double) async throws {
        let peer = Rig(engine: rig.testEngine, workspace: rig.doc)
        guard await peer.connect() else { return XCTFail("the peer did not connect") }
        defer { peer.client.stop() }
        var levels = try XCTUnwrap(peer.store.model?.cue(Self.intro)?.raw["kind"]?["levels"])
        levels.set("master_db", .double(db))
        guard case .success = await peer.store.send(command: .setProperty(Self.intro, .kindFields(["levels": levels])))
        else { return XCTFail("the peer's edit was refused") }
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(db) }
    }

    private func typeInto(_ field: NSTextField, _ text: String) throws {
        // A level is typed in its box, which a double-click (or Return) opens.
        for p in [vc.audioLevels, vc.fadeLevels] where field === p.levelField { p.beginTyping(p.selection) }
        let editor = try XCTUnwrap(field.currentEditor(), "the field is being edited")
        editor.string = text
        // What the field editor posts as each character goes in.
        (field.delegate as? NSTextFieldDelegate)?.controlTextDidChange?(
            Notification(name: NSControl.textDidChangeNotification, object: field))
    }

    func testAPeersChangeDuringTypingIsNotWrittenBackByTheCommit() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        p.select(.output(1))
        try typeInto(p.levelField, "-6")
        try await peerSetsMaster(-20)
        XCTAssertEqual(p.levels?.master, -20, "the panel took the peer's level while the text was held")
        window.makeFirstResponder(nil)
        await assertEventually { self.sent.contains { self.kindOf($0.command) != nil } }
        let levels = lastKind?["levels"]
        XCTAssertEqual(levels?["outputs"]?.arrayValue?[1]["db"], .double(-6))
        XCTAssertNotEqual(levels?["master_db"], .double(0), "the commit did not put the master back to 0")
        // The commit's change reaches the model in a broadcast, which a reply can overtake: wait for it.
        await assertEventually("the commit reached the model") {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[1]["db"] == .double(-6)
        }
        XCTAssertEqual(modelKind(Self.intro)?["levels"]?["master_db"], .double(-20), "the peer's level survives")
    }

    func testLeavingAFieldWithoutTypingTakesAPeersChangeAndWritesNothing() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        p.beginTyping(p.selection)  // the box opens for typing
        XCTAssertNotNil(p.levelField.currentEditor())
        try await peerSetsMaster(-20)
        await assertEventually { p.levelField.stringValue == LevelText.format(-20) }
        window.makeFirstResponder(nil)
        await rig.sync()
        XCTAssertFalse(sent.contains { kindOf($0.command) != nil }, "nothing was typed, nothing is sent")
        XCTAssertEqual(modelKind(Self.intro)?["levels"]?["master_db"], .double(-20))
        // The next fader move starts from the peer's level, not a stale 0.
        drag(p, to: [-10])
        await assertEventually { self.sent.contains { self.kindOf($0.command) != nil } }
        XCTAssertEqual(lastKind?["levels"]?["master_db"], .double(-10))
        XCTAssertEqual(lastKind?["levels"]?["outputs"]?.arrayValue?.count, 2)
    }

    /// A key the field editor turns into a command (Return, Esc), through the panel's delegate as
    /// the real editor does.
    private func press(_ selector: Selector, in field: NSTextField) throws {
        let editor = try XCTUnwrap(field.currentEditor() as? NSTextView, "the field is being edited")
        let handled = (field.delegate as? NSTextFieldDelegate)?.control?(
            field, textView: editor, doCommandBy: selector)
        XCTAssertEqual(handled, true)
    }

    private var escape: Selector { #selector(NSResponder.cancelOperation(_:)) }
    private var returnKey: Selector { #selector(NSResponder.insertNewline(_:)) }

    func testRowsFollowTheFileAfterAReturnCommit() async throws {
        try await reopen {
            try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6)
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 1)
        }
        showCue(Self.intro)
        let p = vc.audioLevels
        await assertEventually { p.matrix.inputs == 6 }
        p.select(.cross(input: 1, output: 0))
        try typeInto(p.levelField, "-6")
        try press(returnKey, in: p.levelField)
        await assertEventually { self.sent.contains { self.kindOf($0.command) != nil } }
        XCTAssertEqual(lastKind?["levels"]?["crosspoints"]?.arrayValue?.first?["db"], .double(-6))
        try await relink(Self.intro, to: "media/d.wav")
        await assertEventually { p.matrix.inputs == 1 }
        XCTAssertEqual(p.selection, .master)
    }

    /// Types "-6" in the master field and presses Esc: the text is dropped and typing is over.
    private func typeAndEscape(_ p: LevelsPanel) throws {
        try typeInto(p.levelField, "-6")
        try press(escape, in: p.levelField)
        XCTAssertEqual(p.levelField.stringValue, LevelText.format(p.levels?.master ?? 0), "the typed text is gone")
    }

    func testAfterEscTheFaderAndTheFieldAgreeAndLeavingTheFieldWritesNothingMore() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        try typeAndEscape(p)
        drag(p, to: [-10])
        await assertEventually { self.sent.contains { self.kindOf($0.command) != nil } }
        XCTAssertEqual(p.levelField.stringValue, LevelText.format(-10), "the field follows the fader")
        let before = sent.count
        p.beginTyping(p.selection)  // the box opens for typing
        window.makeFirstResponder(nil)
        await rig.sync()
        XCTAssertEqual(sent.count, before, "clicking in and out with nothing typed sends nothing")
        // The drag's own broadcast may still be on its way: a sync doesn't wait for broadcasts.
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-10) }
    }

    func testADragStartedWhileTypingCommitsTheTextFirstAndLeavingTheFieldWritesNothingOverIt() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        try typeInto(p.levelField, "-6")
        drag(p, to: [-10])
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-10) }
        XCTAssertEqual(p.levelField.stringValue, LevelText.format(-10), "the field follows the fader")
        let before = sent.count
        window.makeFirstResponder(nil)
        await rig.sync()
        XCTAssertEqual(sent.count, before, "leaving the field writes nothing over the drag")
        XCTAssertEqual(modelKind(Self.intro)?["levels"]?["master_db"], .double(-10))
    }

    func testAfterEscAPeersChangeIsTakenAndNotWrittenBack() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        try typeAndEscape(p)
        try await peerSetsMaster(-20)
        await assertEventually { p.levelField.stringValue == LevelText.format(-20) }
        p.beginTyping(p.selection)  // the box opens for typing
        window.makeFirstResponder(nil)
        await rig.sync()
        XCTAssertFalse(sent.contains { kindOf($0.command) != nil })
        XCTAssertEqual(modelKind(Self.intro)?["levels"]?["master_db"], .double(-20))
    }

    func testAfterEscTheRowsFollowTheFile() async throws {
        try await reopen {
            try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6)
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 1)
        }
        showCue(Self.intro)
        let p = vc.audioLevels
        await assertEventually { p.matrix.inputs == 6 }
        try typeAndEscape(p)
        try await relink(Self.intro, to: "media/d.wav")
        await assertEventually { p.matrix.inputs == 1 }
    }

    func testLeavingAFieldWhoseLevelDoesNotRoundTripThroughItsTextWritesNothing() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        try await peerSetsMaster(-6.25)
        await assertEventually { p.levels?.master == -6.25 }
        XCTAssertEqual(p.levelField.stringValue, LevelText.format(-6.25), "shown to one decimal")
        p.beginTyping(p.selection)  // the box opens for typing
        window.makeFirstResponder(nil)
        await rig.sync()
        XCTAssertFalse(sent.contains { kindOf($0.command) != nil }, "nothing was typed, nothing is sent")
        XCTAssertEqual(modelKind(Self.intro)?["levels"]?["master_db"], .double(-6.25))
    }

    func testTheModelsLevelsReplaceAnEditThatIsTakenBack() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        let before = try XCTUnwrap(p.levels)
        try typeInto(p.levelField, "-6")
        try press(returnKey, in: p.levelField)
        XCTAssertEqual(p.levels?.master, -6)
        // What the inspector does when the engine refuses the edit: show the model's levels again.
        p.show(before, inputs: p.matrix.inputs, outputs: p.matrix.outputCount)
        XCTAssertEqual(p.levels?.master, before.master)
        XCTAssertEqual(p.levelField.stringValue, LevelText.format(before.master ?? 0))
    }

    func testATypedLevelStaysOnItsCrosspointWhenTheFileLosesThatRow() async throws {
        try await reopen {
            try writeWAV(self.media($0, "a.wav"), seconds: 1, channels: 6)
            try writeWAV(self.media($0, "d.wav"), seconds: 1, channels: 1)
        }
        showCue(Self.intro)
        let p = vc.audioLevels
        await assertEventually { p.matrix.inputs == 6 }
        p.select(.cross(input: 1, output: 0))
        try typeInto(p.levelField, "-6")
        try await relink(Self.intro, to: "media/d.wav")
        await assertEventually { self.store.mediaDurations[Self.intro]?.fileChannels == 1 }
        await rig.sync()
        XCTAssertEqual(p.selection, .cross(input: 1, output: 0), "the selection did not move under the text")
        XCTAssertEqual(p.matrix.inputs, 6, "the grid waits for the end of the edit")
        window.makeFirstResponder(nil)
        await assertEventually { self.sent.contains { self.kindOf($0.command) != nil } }
        let levels = lastKind?["levels"]
        XCTAssertNotEqual(levels?["master_db"], .double(-6), "the master did not take the text")
        XCTAssertEqual(levels?["crosspoints"]?.arrayValue?.first?["db"], .double(-6))
        await assertEventually { p.matrix.inputs == 1 }
        XCTAssertEqual(p.selection, .master)
    }

    func testACrosspointBeyondTheFilesChannelsSurvivesAnEdit() async throws {
        try await reopen { show in
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue,
                    var kind = cues[0]["kind"], var levels = kind["levels"]
                else { return XCTFail("the demo show has no first cue") }
                levels.set(
                    "crosspoints",
                    .array([.object(["input": .int(4), "output": .int(0), "db": .double(-3), "mute": .bool(false)])]))
                kind.set("levels", levels)
                cues[0].set("kind", kind)
                lists[0].set("cues", .array(cues))
                ws.set("cue_lists", .array(lists))
            }
        }
        showCue(Self.intro)
        await assertEventually { self.store.mediaDurations[Self.intro]?.fileChannels == 2 }
        let p = vc.audioLevels
        XCTAssertEqual(p.matrix.inputs, 2, "the saved route to channel 5 has no row in a stereo file")
        type(p, "-6")
        await assertEventually { self.sent.contains { self.kindOf($0.command) != nil } }
        XCTAssertEqual(lastKind?["levels"]?["master_db"], .double(-6))
        let points = lastKind?["levels"]?["crosspoints"]?.arrayValue ?? []
        XCTAssertEqual(points.first?["input"], .double(4), "the hidden route is kept")
        XCTAssertEqual(points.first?["db"], .double(-3))
    }

    /// Long names that differ only at the end stay apart under the faders: cut in the middle, each
    /// within its column.
    func testLongOutputNamesAreCutInTheMiddleAndStayApart() {
        let m = LevelMatrixView()
        m.outputNames = ["Surround L", "Surround R", "C"]
        let a = m.columnLabel(0)
        let b = m.columnLabel(1)
        XCTAssertNotEqual(a, b)
        XCTAssertTrue(a.hasSuffix("L") && b.hasSuffix("R"), "\(a) \(b)")
        for label in [a, b] {
            let width = (label as NSString).size(withAttributes: [.font: LevelMatrixView.labelFont]).width
            XCTAssertLessThanOrEqual(width, LevelMatrixView.cellW - 4, label)
        }
        XCTAssertEqual(m.columnLabel(2), "C")
        XCTAssertEqual(m.columnLabel(3), "4", "no name: the number")
    }

    // MARK: Cue outputs above 64 (a patch has up to 128)

    /// Reopens the demo show with cue `index`'s `levels` changed by `edit`, and its patch given as
    /// many cue outputs as those levels name (the Levels tab shows the patch's count).
    /// `patchOutputs`: the patch is given that many cue outputs, `patchCrosspoints` its routing to
    /// device outputs; `tone`: the cue's file is a tone that long.
    private func reopenWithLevels(
        ofCue index: Int, patchOutputs: Int? = nil, patchCrosspoints: [JSONValue] = [], tone: Double? = nil,
        _ edit: @escaping (inout JSONValue) -> Void
    ) async throws {
        try await reopen { show in
            if let tone { try writeWAV(show.appendingPathComponent("media/a.wav"), seconds: tone) }
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue,
                    cues.indices.contains(index), var kind = cues[index]["kind"]
                else { return XCTFail("the demo show has no cue \(index)") }
                var levels = kind["levels"] ?? .object([:])
                edit(&levels)
                kind.set("levels", levels)
                cues[index].set("kind", kind)
                lists[0].set("cues", .array(cues))
                ws.set("cue_lists", .array(lists))
                let wanted = levels["outputs"]?.arrayValue?.count ?? 0
                if var patches = ws["patches"]?.arrayValue, let outputs = patches.first?["outputs"]?.arrayValue,
                    let first = outputs.first, wanted > outputs.count
                {
                    patches[0].set("outputs", .array(Array(repeating: first, count: wanted)))
                    ws.set("patches", .array(patches))
                }
                if let patchOutputs, var patches = ws["patches"]?.arrayValue,
                    let first = patches.first?["outputs"]?.arrayValue?.first
                {
                    patches[0].set("outputs", .array(Array(repeating: first, count: patchOutputs)))
                    patches[0].set("crosspoints", .array(patchCrosspoints))
                    ws.set("patches", .array(patches))
                }
            }
        }
    }

    private static func cross(_ input: Int, _ output: Int, _ db: Double) -> JSONValue {
        .object(["input": .int(Int64(input)), "output": .int(Int64(output)), "db": .double(db), "mute": .bool(false)])
    }

    /// The crosspoint of `levels` from `input` to `output`, if listed.
    private func crosspoint(_ levels: JSONValue?, _ input: Double, _ output: Double) -> JSONValue? {
        levels?["crosspoints"]?.arrayValue?.first { $0["input"] == .double(input) && $0["output"] == .double(output) }
    }

    /// A cue routed to cue output 101 keeps that routing, its level there and its gang when a level
    /// on a low output is edited in the inspector, and undo puts back exactly what it had.
    func testRoutingToACueOutputAbove64SurvivesAnInspectorEditAndUndo() async throws {
        try await reopenWithLevels(ofCue: 0) { levels in
            levels.set(
                "outputs", .array(Array(repeating: .object(["db": .double(-1), "mute": .bool(false)]), count: 101)))
            levels.set(
                "crosspoints", .array([Self.cross(0, 0, 0), Self.cross(1, 1, 0), Self.cross(0, 100, -3)]))
            levels.set("gangs", .array([.array([.int(99), .int(100)])]))
        }
        showCue(Self.intro)
        let before = try XCTUnwrap(modelKind(Self.intro)?["levels"])
        XCTAssertEqual(crosspoint(before, 0, 100)?["db"], .double(-3), "the engine keeps the route to output 101")
        let p = vc.audioLevels
        XCTAssertEqual(p.matrix.outputCount, 101, "the tab shows every output of the patch")
        p.select(.output(1))
        type(p, "-3.5")
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[1]["db"] == .double(-3.5)
        }
        let after = modelKind(Self.intro)?["levels"]
        XCTAssertEqual(crosspoint(after, 0, 100)?["db"], .double(-3), "the route above 64 survives the edit")
        XCTAssertEqual(after?["crosspoints"]?.arrayValue?.count, 3)
        XCTAssertEqual(after?["outputs"]?.arrayValue?.count, 101)
        XCTAssertEqual(after?["outputs"]?.arrayValue?[100]["db"], .double(-1))
        XCTAssertEqual(after?["gangs"], .array([.array([.double(99), .double(100)])]))
        _ = await store.undo()
        await assertEventually("undo restores the levels exactly") { self.modelKind(Self.intro)?["levels"] == before }
    }

    /// A Fade's levels for cue output 101 and its crosspoint there survive an edit of its main
    /// level in the Fade tab, and undo restores them exactly.
    func testAFadesLevelsAbove64SurviveAnInspectorEditAndUndo() async throws {
        try await reopenWithLevels(ofCue: 1) { levels in
            var outputDb = Array(repeating: JSONValue.null, count: 101)
            outputDb[100] = .double(-10)
            levels.set("output_db", .array(outputDb))
            levels.set(
                "crosspoints",
                .array([.object(["input": .int(0), "output": .int(100), "db": .double(-6)])]))
        }
        showCue(Self.fade)
        let before = try XCTUnwrap(modelKind(Self.fade)?["levels"])
        XCTAssertEqual(before["output_db"]?.arrayValue?.count, 101, "the engine keeps the level for output 101")
        let p = vc.fadeLevels
        p.select(.master)
        type(p, "-6")
        await assertEventually { self.modelKind(Self.fade)?["levels"]?["master_db"] == .double(-6) }
        let after = modelKind(Self.fade)?["levels"]
        XCTAssertEqual(after?["output_db"]?.arrayValue?.count, 101)
        XCTAssertEqual(after?["output_db"]?.arrayValue?[100], .double(-10))
        XCTAssertEqual(crosspoint(after, 0, 100)?["db"], .double(-6))
        _ = await store.undo()
        await assertEventually("undo restores the levels exactly") { self.modelKind(Self.fade)?["levels"] == before }
    }

    /// "Set defaults" on a cue of 100 outputs is taken by the engine: every output at 0 dB, file
    /// channel n to output n for the 64 channels a file can have.
    func testSetDefaultsOnACueOf100OutputsChangesItsLevels() async throws {
        try await reopenWithLevels(ofCue: 0) { levels in
            levels.set(
                "outputs", .array(Array(repeating: .object(["db": .double(-6), "mute": .bool(false)]), count: 100)))
            levels.set("crosspoints", .array([]))
        }
        showCue(Self.intro)
        let p = vc.audioLevels
        fire(p.defaultButton)
        await assertEventually("the engine takes the edit") {
            self.modelKind(Self.intro)?["levels"]?["crosspoints"]?.arrayValue?.count == 64
        }
        let levels = modelKind(Self.intro)?["levels"]
        XCTAssertEqual(levels?["outputs"]?.arrayValue?.count, 100)
        XCTAssertEqual(levels?["outputs"]?.arrayValue?[99]["db"], .double(0))
        XCTAssertEqual(crosspoint(levels, 63, 63)?["db"], .double(0))
        XCTAssertNil(crosspoint(levels, 64, 64))
    }

    // MARK: 128 cue outputs (AUD-02, AUD-19)

    /// The patch's cue output 100 also goes to device output 1, so what that output plays is heard
    /// there (the device's first output carries nothing else when only output 100 is routed).
    private static let output100ToDevice1 = JSONValue.object([
        "output": .int(99), "device_output": .int(0), "db": .double(0),
    ])
    private static let patch: ModelID = "00000000-0000-0000-0000-000000000001"

    /// Every one of 128 outputs at `db`, unmuted.
    private static func outputs128(_ db: Double = 0) -> JSONValue {
        .array(Array(repeating: .object(["db": .double(db), "mute": .bool(false)]), count: 128))
    }

    /// The window on screen with its meters running (a meter registers only while shown).
    private func showMeters(_ p: LevelsPanel) {
        window.orderFront(nil)
        p.meter.updateLive()
        XCTAssertGreaterThan(store.meters.visibleCount, 0, "the Levels tab's meter is on screen")
    }

    /// Moves the show on by `seconds` of device time.
    private func play(_ seconds: Double) async throws { try await rig.advance(seconds) }

    /// The loudest the cue's meter channel `channel` and the device's first output get over three
    /// tenths of a second, after a tenth to let an edit reach the sound.
    private func heard(_ cue: ModelID, channel: Int) async throws -> (meter: Float, device: Float) {
        try await play(0.1)
        store.meters.reset()
        rig.wire.resetDevicePeaks()
        var meter: Float = 0
        for _ in 0..<3 {
            try await play(0.1)
            store.meters.tick(0)
            meter = max(meter, store.meters.level(.cue(cue), channel: channel))
        }
        return (meter, rig.wire.devicePeaks[Self.patch]?.first ?? 0)
    }

    /// Types `text` in the box of `slot` as a double-click on it does: the field opens over the box.
    private func typeInBox(_ p: LevelsPanel, _ slot: LevelSet.Slot, _ text: String) {
        p.beginTyping(slot)
        XCTAssertFalse(p.levelField.isHidden, "the box opens for typing")
        XCTAssertEqual(
            p.levelField.frame, p.board.convert(p.matrix.rect(of: slot), from: p.matrix).insetBy(dx: 1, dy: 0))
        type(p, text)
    }

    /// What of `view` (in its own coordinates) shows inside the panel's scroll view.
    private func shown(_ p: LevelsPanel, _ rect: NSRect, of view: NSView) -> NSRect {
        let clip = p.scroll.contentView
        return clip.convert(rect, from: view).intersection(clip.bounds)
    }

    /// The panel's board and everything on it lie inside its scroll view, which lies inside the tab.
    private func assertInsideTheTab(_ p: LevelsPanel, line: UInt = #line) throws {
        let tab = try XCTUnwrap(p.scroll.superview)
        XCTAssertTrue(tab.bounds.contains(p.scroll.frame), "the levels scroll view lies inside the tab", line: line)
        XCTAssertTrue(p.scroll.documentView === p.board, line: line)
        XCTAssertTrue(p.board.bounds.contains(p.matrix.frame), "the matrix lies on the board", line: line)
        for (n, f) in p.faders.enumerated() {
            XCTAssertTrue(p.board.bounds.contains(f.frame), "fader \(n) lies on the board", line: line)
        }
        if p.meter.superview != nil {
            XCTAssertTrue(p.board.bounds.contains(p.meter.frame), "the meter lies on the board", line: line)
        }
    }

    /// Checklist: a cue on a 128-output patch is selected → the Levels tab shows all 128 outputs
    /// inside its horizontal scroll, each with its value box and fader; scrolling stays smooth.
    func testACueOnA128OutputPatchShowsAll128OutputsInsideTheLevelsTabsScroll() async throws {
        // The cue's own levels name two outputs, as a cue made before its patch grew does.
        try await reopenWithLevels(ofCue: 0, patchOutputs: 128, tone: 6) { _ in }
        showCue(Self.intro)
        let p = vc.audioLevels
        vc.view.layoutSubtreeIfNeeded()
        XCTAssertEqual(p.matrix.outputCount, 128)
        XCTAssertEqual(p.faders.count, 129, "the main fader and one per output")
        // Playing, so its meters are drawn (a cue not playing has no meter bars).
        showMeters(p)
        guard case .success = await store.transport(.start(cue: Self.intro)) else { return XCTFail("start") }
        try await play(0.2)
        store.meters.tick(0)
        XCTAssertEqual(store.meters.channelCount(.cue(Self.intro)), 128)
        XCTAssertEqual(p.meter.columns?.count, 128, "a meter beside each output's fader")
        let m = p.matrix
        XCTAssertEqual(m.frame.width, LevelMatrixView.headW + 128 * LevelMatrixView.cellW)
        XCTAssertEqual(m.slot(at: NSPoint(x: m.bounds.maxX - 1, y: 1)), .output(127))
        XCTAssertEqual(m.columnLabel(127), "128", "an unnamed output shows its number")
        try assertInsideTheTab(p)
        // Each box has its fader above it, in its own column.
        for o in [0, 63, 64, 99, 127] {
            let box = p.board.convert(m.rect(of: .output(o)), from: m)
            XCTAssertEqual(p.faders[o + 1].frame.midX, box.midX, "fader \(o + 1) is over its box")
            XCTAssertLessThan(p.faders[o + 1].frame.maxY, box.minY)
        }
        // The boxes are wider than the tab: it scrolls sideways to the last, then back.
        let clip = p.scroll.contentView
        XCTAssertLessThan(clip.bounds.width, p.board.frame.width, "the outputs are wider than the tab")
        XCTAssertTrue(p.scroll.hasHorizontalScroller)
        let views = p.board.subviews
        let last = m.rect(of: .output(127))
        m.scrollToVisible(last)
        XCTAssertEqual(shown(p, last, of: m).size, last.size, "output 128's box is in view")
        XCTAssertEqual(shown(p, m.rect(of: .master), of: m).width, 0, "the main box has scrolled out")
        _ = try renderView(vc.view)
        // Smooth: scrolling and drawing make no views, and a draw paints only the meters in view.
        XCTAssertTrue(p.board.subviews.elementsEqual(views, by: ===), "no view is made or dropped by scrolling")
        #if DEBUG
            let inView = Int(clip.bounds.width / LevelMatrixView.cellW) + 2
            XCTAssertGreaterThan(p.meter.drawnColumns, 0, "the meters in view are drawn")
            XCTAssertLessThanOrEqual(p.meter.drawnColumns, inView, "only the meters in view are drawn")
        #endif
        m.scrollToVisible(m.rect(of: .master))
        XCTAssertEqual(shown(p, m.rect(of: .master), of: m).size, m.rect(of: .master).size, "back to the main box")
        // The model shown again (an edit elsewhere): the same faders, not new ones.
        type(p, "-1")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-1) }
        XCTAssertTrue(p.board.subviews.elementsEqual(views, by: ===), "showing the cue again keeps its views")
        _ = try renderView(vc.view)
    }

    /// Checklist: a level on output 100 is typed → that output's level changes in the engine and is
    /// heard; one undo restores it.
    func testALevelTypedOnOutput100ChangesInTheEngineIsHeardAndOneUndoRestoresIt() async throws {
        try await reopenWithLevels(ofCue: 0, patchOutputs: 128, patchCrosspoints: [Self.output100ToDevice1], tone: 6) {
            $0.set("outputs", Self.outputs128())
            $0.set("crosspoints", .array([Self.cross(0, 99, 0)]))
        }
        showCue(Self.intro)
        let p = vc.audioLevels
        showMeters(p)
        guard case .success = await store.transport(.start(cue: Self.intro)) else { return XCTFail("start") }
        let before = try await heard(Self.intro, channel: 99)
        XCTAssertGreaterThan(before.meter, 0.01, "output 100's meter shows the tone")
        XCTAssertGreaterThan(before.device, 0.01, "output 100 is heard on the device")

        typeInBox(p, .output(99), "-20")
        await assertEventually("the engine has the level") {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[99]["db"] == .double(-20)
        }
        XCTAssertTrue(p.levelField.isHidden, "the box closes")
        XCTAssertEqual(p.levelField.stringValue, "-20.0", "the box shows the level")
        XCTAssertEqual(p.faders[100].doubleValue, -20, "output 100's fader is at the level")
        XCTAssertEqual(p.matrix.levels?.db(.output(99)), -20)
        let quieter = try await heard(Self.intro, channel: 99)
        XCTAssertEqual(Double(quieter.meter / before.meter), 0.1, accuracy: 0.02, "20 dB down on the meter")
        XCTAssertEqual(Double(quieter.device / before.device), 0.1, accuracy: 0.02, "20 dB down where it is heard")

        guard case .success = await store.undo() else { return XCTFail("undo") }
        await assertEventually("one undo restores it") {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[99]["db"] == .double(0)
        }
        await assertEventually { p.levelField.stringValue == "0.0" }
        XCTAssertEqual(p.faders[100].doubleValue, 0)
        let restored = try await heard(Self.intro, channel: 99)
        XCTAssertEqual(Double(restored.device / before.device), 1, accuracy: 0.05, "heard as before")
    }

    /// Checklist: a Fade cue targeting that cue → its Levels tab shows and edits all 128 outputs the
    /// same way.
    func testAFadeTargetingACueOnA128OutputPatchShowsAndEditsAll128Outputs() async throws {
        // The target's levels list two outputs (made before its patch grew); its file channel 1
        // goes to output 100, which the patch also sends to device output 1.
        try await reopenWithLevels(
            ofCue: 0, patchOutputs: 128, patchCrosspoints: [Self.output100ToDevice1], tone: 6
        ) { $0.set("crosspoints", .array([Self.cross(0, 99, 0)])) }
        showCue(Self.fade)
        // A short Fade that leaves its target playing and moves nothing else (the demo's takes the
        // main level to silence), so what it does on output 100 is heard.
        let nothing = JSONValue.object(["master_db": .null, "output_db": .array([]), "crosspoints": .array([])])
        let setUp: [CueProperty] = [
            .duration(0.5), .kindFields(["stop_target_when_done": .bool(false), "levels": nothing]),
        ]
        for edit in setUp {
            guard case .success = await store.send(command: .setProperty(Self.fade, edit)) else {
                return XCTFail("\(edit)")
            }
        }
        let p = vc.fadeLevels
        vc.view.layoutSubtreeIfNeeded()
        XCTAssertEqual(p.matrix.outputCount, 128, "as many outputs as the target's patch")
        XCTAssertEqual(p.faders.count, 129)
        XCTAssertNil(p.meter.superview, "a Fade's levels have no meter")
        try assertInsideTheTab(p)
        let before = try XCTUnwrap(modelKind(Self.fade)?["levels"])
        XCTAssertFalse(p.matrix.levels?.isActive(.output(99)) ?? true, "the Fade leaves output 100 alone")

        typeInBox(p, .output(99), "-20")
        await assertEventually("the engine has the level") {
            self.modelKind(Self.fade)?["levels"]?["output_db"]?.arrayValue?.count == 100
        }
        let outputDb = modelKind(Self.fade)?["levels"]?["output_db"]?.arrayValue
        XCTAssertEqual(outputDb?[99], .double(-20))
        XCTAssertEqual(outputDb?.dropLast().allSatisfy { $0 == .null }, true, "the others are left alone")
        XCTAssertEqual(p.levelField.stringValue, "-20.0")
        XCTAssertTrue(p.matrix.levels?.isActive(.output(99)) ?? false, "output 100 now fades")
        XCTAssertEqual(p.faders[100].doubleValue, -20)
        // Heard: output 100 of the playing target is silent until the Fade brings it up.
        guard case .success = await store.transport(.start(cue: Self.intro)) else { return XCTFail("start") }
        let silent = try await heard(Self.intro, channel: 99)
        XCTAssertEqual(silent.device, 0, accuracy: 0.0001, "output 100 is silent before the Fade")
        guard case .success = await store.transport(.start(cue: Self.fade)) else { return XCTFail("fade") }
        try await play(0.1)
        XCTAssertEqual(store.cueStates[Self.intro]?.state, .fading, "the Fade moves the target")
        try await play(0.5)
        let faded = try await heard(Self.intro, channel: 99)
        XCTAssertGreaterThan(faded.device, 0.001, "the Fade brings output 100 up, and it is heard")
        // The crosspoint of output 128 is typed as well, at the far end of the scroll.
        typeInBox(p, .cross(input: 0, output: 127), "-6")
        await assertEventually {
            self.crosspoint(self.modelKind(Self.fade)?["levels"], 0, 127)?["db"] == .double(-6)
        }

        _ = await store.undo()
        _ = await store.undo()
        await assertEventually("undo restores the levels") { self.modelKind(Self.fade)?["levels"] == before }
        XCTAssertFalse(p.matrix.levels?.isActive(.output(99)) ?? true)
    }

    /// Checklist: the cue plays → its meters show every output it has, including those above 64.
    func testACuePlayingOn128OutputsMetersEveryOutputIncludingThoseAbove64() async throws {
        try await reopenWithLevels(ofCue: 0, patchOutputs: 128, tone: 6) {
            $0.set("outputs", Self.outputs128())
            $0.set("crosspoints", .array((0..<128).map { Self.cross(0, $0, 0) }))
        }
        showCue(Self.intro)
        let p = vc.audioLevels
        showMeters(p)
        guard case .success = await store.transport(.start(cue: Self.intro)) else { return XCTFail("start") }
        try await play(0.2)
        store.meters.tick(0)
        XCTAssertEqual(store.meters.channelCount(.cue(Self.intro)), 128, "a peak for every output")
        for o in [0, 63, 64, 99, 127] {
            XCTAssertGreaterThan(store.meters.level(.cue(Self.intro), channel: o), 0.01, "output \(o + 1) is metered")
        }
        // Output 100's bar, beside its fader, is drawn lit once scrolled into view.
        let m = p.matrix
        m.scrollToVisible(m.rect(of: .output(99)))
        let c = try XCTUnwrap(p.meter.columns)
        let bar = NSRect(x: c.first + 99 * c.step, y: 0, width: c.width, height: p.meter.bounds.height)
        XCTAssertEqual(shown(p, bar, of: p.meter).size, bar.size, "output 100's meter is in view")
        let fader = p.faders[100].frame
        XCTAssertEqual(p.board.convert(bar, from: p.meter).minX, fader.maxX, "the bar is just right of its fader")
        let column = p.board.convert(m.rect(of: .output(99)), from: m)
        let drawn = p.board.convert(bar, from: p.meter)
        XCTAssertTrue(drawn.minX >= column.minX && drawn.maxX <= column.maxX, "the bar is in output 100's column")
        let rep = try renderView(vc.view)
        XCTAssertGreaterThan(
            litPixels(rep, vc.view.convert(bar, from: p.meter), in: vc.view), 20, "output 100's bar is lit")
        // A silent output's bar is dark.
        typeInBox(p, .output(98), "-inf")
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[98]["db"] == .double(-120)
        }
        try await play(0.2)
        store.meters.reset()  // what was shown before the edit is forgotten: only new peaks count
        try await play(0.3)
        store.meters.tick(0)
        XCTAssertEqual(store.meters.level(.cue(Self.intro), channel: 98), 0, "a silenced output shows nothing")
        let dark = NSRect(x: c.first + 98 * c.step, y: 0, width: c.width, height: p.meter.bounds.height)
        let after = try renderView(vc.view)
        XCTAssertEqual(
            litPixels(after, vc.view.convert(dark, from: p.meter), in: vc.view), 0, "output 99's bar is dark")
        XCTAssertGreaterThan(litPixels(after, vc.view.convert(bar, from: p.meter), in: vc.view), 20)
    }

    /// The Mute item of `slot`'s right-click menu is ticked.
    private func muteTicked(_ p: LevelsPanel, _ slot: LevelSet.Slot) throws -> Bool {
        let box = p.matrix.rect(of: slot)
        let at = p.matrix.convert(NSPoint(x: box.midX, y: box.midY), to: nil)
        let event = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .rightMouseDown, location: at, modifierFlags: [], timestamp: 0,
                windowNumber: p.matrix.window?.windowNumber ?? 0, context: nil, eventNumber: 0, clickCount: 1,
                pressure: 1))
        return try XCTUnwrap(p.matrix.menu(for: event)?.items.first).state == .on
    }

    /// Mute chosen on output 100, which the cue's levels do not list yet (its patch grew after the
    /// cue was made): the menu shows it ticked because the mute is kept, so a level typed there
    /// later is not heard until it is unmuted; undo takes each step back (#758).
    func testMuteOnAnOutputTheCuesLevelsDoNotListIsKeptAndSilencesIt() async throws {
        try await reopenWithLevels(
            ofCue: 0, patchOutputs: 128, patchCrosspoints: [Self.output100ToDevice1], tone: 6
        ) { $0.set("crosspoints", .array([Self.cross(0, 99, 0)])) }
        showCue(Self.intro)
        let p = vc.audioLevels
        let before = try XCTUnwrap(modelKind(Self.intro)?["levels"])
        func output100() -> JSONValue? {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.dropFirst(99).first
        }
        XCTAssertTrue(p.muteFromMenu(.output(99)), "output 100 has a Mute")
        await assertEventually("the mute is kept") { output100()?["mute"] == .bool(true) }
        XCTAssertEqual(output100()?["db"], .double(-120), "still silent")
        XCTAssertTrue(try muteTicked(p, .output(99)))
        XCTAssertEqual(p.matrix.levels?.isMuted(.output(99)), true, "the box is drawn muted")
        guard case .success = await store.transport(.start(cue: Self.intro)) else { return XCTFail("start") }
        typeInBox(p, .output(99), "0")
        await assertEventually { output100()?["db"] == .double(0) }
        XCTAssertEqual(output100()?["mute"], .bool(true), "typing a level keeps the mute")
        XCTAssertTrue(try muteTicked(p, .output(99)))
        let muted = try await heard(Self.intro, channel: 99)
        XCTAssertEqual(muted.device, 0, accuracy: 0.0001, "a muted output is not heard")
        XCTAssertTrue(p.muteFromMenu(.output(99)))
        await assertEventually { output100()?["mute"] == .bool(false) }
        XCTAssertFalse(try muteTicked(p, .output(99)))
        let open = try await heard(Self.intro, channel: 99)
        XCTAssertGreaterThan(open.device, 0.01, "unmuted, it is heard")
        for _ in 0..<3 { _ = await store.undo() }
        await assertEventually("undo restores the levels") { self.modelKind(Self.intro)?["levels"] == before }
        XCTAssertFalse(try muteTicked(p, .output(99)))
    }

    /// Strongly coloured pixels of `rep` (a render of `view`) inside `rect`, in `view`'s coordinates.
    private func litPixels(_ rep: NSBitmapImageRep, _ rect: NSRect, in view: NSView) -> Int {
        let scale = CGFloat(rep.pixelsWide) / view.bounds.width
        let top = view.isFlipped ? rect.minY : view.bounds.height - rect.maxY
        var lit = 0
        for py in Int(top * scale)..<Int((top + rect.height) * scale) {
            for px in Int(rect.minX * scale)..<Int(rect.maxX * scale) {
                guard let c = rep.colorAt(x: px, y: py)?.usingColorSpace(.sRGB) else { continue }
                let hi = max(c.redComponent, c.greenComponent, c.blueComponent)
                if hi - min(c.redComponent, c.greenComponent, c.blueComponent) > 0.4 { lit += 1 }
            }
        }
        return lit
    }

    /// Checklist: a narrow window → nothing is drawn outside the tab; the first columns stay
    /// reachable.
    func testANarrowWindowDrawsNothingOutsideTheTabAndTheFirstColumnsStayReachable() async throws {
        try await reopenWithLevels(ofCue: 0, patchOutputs: 128) { _ in }
        window.setContentSize(NSSize(width: 420, height: 260))
        showCue(Self.intro)
        let p = vc.audioLevels
        vc.view.layoutSubtreeIfNeeded()
        try assertInsideTheTab(p)
        let tab = try XCTUnwrap(p.scroll.superview)
        XCTAssertTrue(tab.bounds.contains(p.sideScroll.frame), "the column of buttons lies inside the tab")
        XCTAssertGreaterThan(p.scroll.contentView.bounds.width, LevelMatrixView.headW, "some boxes show")
        // The last output chosen, the tab scrolls to it; arrows walk back to the main level, and the
        // first columns come back into view.
        let m = p.matrix
        p.select(.output(127))
        m.scrollToVisible(m.rect(of: .output(127)))
        XCTAssertEqual(shown(p, m.rect(of: .output(0)), of: m).width, 0, "output 1 has scrolled out")
        window.makeFirstResponder(m)
        let left = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, characters: "\u{F702}", charactersIgnoringModifiers: "\u{F702}", isARepeat: false,
                keyCode: 123))
        for _ in 0..<128 { m.keyDown(with: left) }
        XCTAssertEqual(p.selection, .master)
        XCTAssertEqual(shown(p, m.rect(of: .master), of: m).size, m.rect(of: .master).size, "the main box is in view")
        XCTAssertEqual(
            shown(p, m.rect(of: .output(0)), of: m).width, m.rect(of: .output(0)).width, "output 1 is in view")
        // Typed in, the first box takes its level.
        typeInBox(p, .output(0), "-3")
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.first?["db"] == .double(-3)
        }
        try assertInsideTheTab(p)
        _ = try renderView(vc.view)
    }

    // MARK: Audio levels

    func testTypedMasterOutputAndCrosspointLevelsSendTheCueKind() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        type(p, "-6")
        await assertEventually { self.sent.count == 1 }
        XCTAssertEqual(lastKind?["levels"]?["master_db"], .double(-6))
        XCTAssertNil(lastKind?["media"], "the rest of the kind is not sent")
        XCTAssertNil(sent.last?.coalesce, "a typed value is a one-off edit")

        p.select(.output(1))
        type(p, "-3.5")
        await assertEventually { self.sent.count == 2 }
        XCTAssertEqual(lastKind?["levels"]?["outputs"]?.arrayValue?[1]["db"], .double(-3.5))
        XCTAssertEqual(lastKind?["levels"]?["master_db"], .double(-6), "an edit starts from the previous one")

        p.select(.cross(input: 1, output: 0))
        type(p, "-12")
        await assertEventually { self.sent.count == 3 }
        XCTAssertEqual(
            lastKind?["levels"]?["crosspoints"]?.arrayValue,
            [.object(["input": .double(1), "output": .double(0), "db": .double(-12), "mute": .bool(false)])])
        await assertEventually("the model answers") {
            self.modelKind(Self.intro)?["levels"]?["crosspoints"]?.arrayValue?.count == 1
        }
        XCTAssertEqual(p.levelField.stringValue, "-12.0")
        XCTAssertEqual(p.selection, .cross(input: 1, output: 0))
    }

    func testMuteKeepsTheLevel() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        window.layoutIfNeeded()
        XCTAssertFalse(p.muteFromMenu(.master), "the cue master has no mute")
        p.select(.cross(input: 0, output: 0))
        XCTAssertFalse(p.muteFromMenu(.cross(input: 0, output: 0)), "a route that is not listed is silent already")
        type(p, "0")
        await assertEventually { self.sent.count == 1 }
        XCTAssertTrue(p.muteFromMenu(.cross(input: 0, output: 0)))
        await assertEventually { self.sent.count == 2 }
        XCTAssertEqual(lastKind?["levels"]?["crosspoints"]?.arrayValue?.first?["mute"], .bool(true))
        XCTAssertEqual(lastKind?["levels"]?["crosspoints"]?.arrayValue?.first?["db"], .double(0))
        XCTAssertTrue(p.muteFromMenu(.output(0)))
        await assertEventually { self.sent.count == 3 }
        XCTAssertEqual(lastKind?["levels"]?["outputs"]?.arrayValue?.first?["mute"], .bool(true))
    }

    func testMinusInfinity() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        p.select(.output(0))
        type(p, "-inf")
        await assertEventually { self.sent.count == 1 }
        XCTAssertEqual(lastKind?["levels"]?["outputs"]?.arrayValue?.first?["db"], .double(-120))
        await assertEventually { p.levelField.stringValue == "-inf" }
        XCTAssertEqual(p.fader.doubleValue, LevelText.floorDb, "the fader rests at the bottom")
        type(p, "-70")
        await settle()
        XCTAssertEqual(sent.count, 1, "below the limit is -inf again: nothing to send")
        drag(p, to: [-30, LevelText.floorDb])
        await assertEventually { self.sent.count == 3 }
        XCTAssertEqual(
            lastKind?["levels"]?["outputs"]?.arrayValue?.first?["db"], .double(-120), "a fader at the bottom is -inf")
        p.select(.cross(input: 0, output: 0))
        type(p, "-6")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["crosspoints"]?.arrayValue?.count == 1 }
        type(p, "-inf")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["crosspoints"]?.arrayValue?.isEmpty == true }
    }

    /// The strip and the matrix live in the workspace's limits, not the default ones.
    func testTheLevelsFollowTheWorkspaceVolumeLimits() async throws {
        let settings = WorkspaceSettingsModel(store: store)
        let lowered = await settings.setOutputMin("-80")
        XCTAssertTrue(lowered)
        await assertEventually { self.store.model?.settings["output_min_db"]?.doubleValue == -80 }
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        p.select(.output(0))
        type(p, "-70")
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.first?["db"] == .double(-70)
        }
        await assertEventually { p.levelField.stringValue == "-70.0" }
        XCTAssertEqual(p.fader.minValue, -80, "the fader goes down to the minimum")
        XCTAssertEqual(p.fader.doubleValue, -70, "and shows the level")
        XCTAssertEqual(p.matrix.limits.min, -80, "as does the matrix")
        drag(p, to: [-75, -80])
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.first?["db"] == .double(-120)
        }
        // A maximum below 0 dB stops the fader there.
        let capped = await settings.setOutputMax("-6")
        XCTAssertTrue(capped)
        await assertEventually { p.fader.maxValue == -6 }
        type(p, "+6")
        await assertEventually { p.levelField.stringValue == "-6.0" }
    }

    /// Space in a playing cue's level box (the GO habit) clears the text; leaving it changes nothing.
    func testABlankLevelBoxIsNoChangeOnAPlayingCue() async throws {
        guard case .success = await store.transport(.start(cue: Self.intro)) else { return XCTFail("start") }
        // The cue's state reaches the store as its own message after the start's result.
        await assertEventually("the cue is running") { self.store.cueStates[Self.intro]?.state == .running }
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        p.select(.master)
        vc.tabView.selectTabViewItem(withIdentifier: "levels")
        window.makeKeyAndOrderFront(nil)
        p.beginTyping(p.selection)  // the box opens for typing
        let editor = try XCTUnwrap(p.levelField.currentEditor())
        editor.selectAll(nil)
        window.sendEvent(
            try XCTUnwrap(
                NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                    windowNumber: window.windowNumber, context: nil, characters: " ",
                    charactersIgnoringModifiers: " ", isARepeat: false, keyCode: 49)))
        XCTAssertEqual(p.levelField.currentEditor()?.string, " ", "the key reached the box")
        let before = sent.count
        window.makeFirstResponder(nil)
        await settle()
        XCTAssertEqual(sent.count, before, "a blank sends nothing")
        XCTAssertEqual(modelKind(Self.intro)?["levels"]?["master_db"], .double(0))
        XCTAssertEqual(p.levelField.stringValue, "0.0", "the box shows the level again")
        XCTAssertEqual(store.cueStates[Self.intro]?.state, .running)
    }

    /// Emptied is -inf, as in QLab (manual p. 204).
    func testAnEmptiedLevelBoxIsSilence() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        p.select(.master)
        type(p, "")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-120) }
        await assertEventually { p.levelField.stringValue == "-inf" }
    }

    func testFaderDragIsOneUndoStep() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        drag(p, to: [-1, -2, -3, -4, -5])
        await assertEventually { self.sent.count == 5 }
        let keys = Set(sent.map { $0.coalesce })
        XCTAssertEqual(keys.count, 1, "one key for the whole gesture")
        let key = try XCTUnwrap(keys.first ?? nil)
        XCTAssertTrue(
            key.contains(InspectorViewController.launchToken),
            "a counter alone repeats across clients and launches: \(key)")
        XCTAssertTrue(sent.allSatisfy { kindOf($0.command) != nil })
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-5) }

        drag(p, to: [-8])
        await assertEventually { self.sent.count == 6 }
        XCTAssertNotEqual(sent[5].coalesce, sent[0].coalesce, "a new gesture has its own key")

        // The engine merges edits with one key: two undo steps, not six.
        _ = await store.undo()
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-5) }
        _ = await store.undo()
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(0) }
        // Its own message, after the model's.
        await assertEventually { self.store.document?.canUndo == false }
    }

    func testMatrixDragIsOneGesture() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        p.select(.cross(input: 0, output: 1))
        for (n, db) in [-20.0, -10, -5].enumerated() {
            p.matrix.onDrag?(.cross(input: 0, output: 1), db, n == 0)
        }
        p.matrix.onDragEnd?()
        await assertEventually { self.sent.count == 3 }
        XCTAssertEqual(Set(sent.map { $0.coalesce }).count, 1)
        XCTAssertNotNil(sent.first?.coalesce)
    }

    func testADragDoesNotEditARowTheMatrixNoLongerHas() throws {
        vc.setSelection([Self.intro])
        let m = vc.audioLevels.matrix
        m.resize(inputs: 6, outputs: 2)
        var edited = false
        m.onDrag = { _, _, _ in edited = true }
        func event(_ type: NSEvent.EventType, _ p: NSPoint) throws -> NSEvent {
            try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: m.convert(p, to: nil), modifierFlags: [], timestamp: 0,
                    windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        }
        let cell = m.rect(of: .cross(input: 4, output: 0))
        m.mouseDown(with: try event(.leftMouseDown, NSPoint(x: cell.midX, y: cell.midY)))
        m.resize(inputs: 1, outputs: 2)
        m.mouseDragged(with: try event(.leftMouseDragged, NSPoint(x: cell.midX, y: cell.midY - 30)))
        XCTAssertFalse(edited, "the row was removed under the mouse")
    }

    // MARK: Faders, value boxes, curve and fade mode

    func testEachOutputHasAFaderOverItsValueBoxAndMovingOneSetsThatOutput() async throws {
        showCue(Self.intro)
        window.layoutIfNeeded()
        let p = vc.audioLevels
        let outputs = p.matrix.outputCount
        XCTAssertGreaterThanOrEqual(outputs, 2)
        XCTAssertEqual(p.faders.count, outputs + 1, "the main level and one per cue output")
        for (n, f) in p.faders.enumerated() {
            let box = p.board.convert(p.matrix.rect(of: n == 0 ? .master : .output(n - 1)), from: p.matrix)
            XCTAssertTrue(f.isVertical)
            XCTAssertEqual(f.frame.midX, box.midX, accuracy: 0.5, "fader \(n) is over its box")
            XCTAssertLessThanOrEqual(f.frame.maxY, box.minY, "and above it")
            XCTAssertGreaterThanOrEqual(f.frame.height, LevelsPanel.minFaderHeight)
            XCTAssertFalse(f.isHiddenOrHasHiddenAncestor)
        }
        let second = p.faders[2]
        second.onGestureBegin?()
        second.doubleValue = -12
        fire(second)
        second.onGestureEnd?()
        XCTAssertEqual(p.selection, .output(1), "the level it moves is the chosen one")
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[1]["db"] == .double(-12)
        }
        XCTAssertEqual(
            modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[0]["db"], .double(0), "the others stay")
    }

    func testAValueBoxIsTypedInAfterADoubleClickAndTheFieldGoesWhenTypingEnds() async throws {
        showCue(Self.intro)
        window.layoutIfNeeded()
        let p = vc.audioLevels
        XCTAssertTrue(p.levelField.isHidden, "no field until a box is opened")
        let box = p.matrix.rect(of: .output(0))
        let doubleClick = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: p.matrix.convert(NSPoint(x: box.midX, y: box.midY), to: nil),
                modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber, context: nil, eventNumber: 0,
                clickCount: 2, pressure: 1))
        p.matrix.mouseDown(with: doubleClick)
        XCTAssertFalse(p.levelField.isHidden)
        XCTAssertEqual(p.levelField.frame.midX, p.board.convert(box, from: p.matrix).midX, accuracy: 1, "over that box")
        XCTAssertTrue(window.firstResponder === p.levelField.currentEditor())
        try typeInto(p.levelField, "-9")
        try press(returnKey, in: p.levelField)
        XCTAssertTrue(p.levelField.isHidden)
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?[0]["db"] == .double(-9)
        }
    }

    func testSetDefaultLevelsPutsANewCuesLevelsBack() async throws {
        showCue(Self.intro)
        let p = vc.audioLevels
        fire(p.silentButton)
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(LevelText.silence) }
        fire(p.defaultButton)
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(0) }
        let levels = modelKind(Self.intro)?["levels"]
        XCTAssertEqual(levels?["outputs"]?.arrayValue?.map { $0["db"] }, [.double(0), .double(0)])
        let routes = (levels?["crosspoints"]?.arrayValue ?? []).map {
            [$0["input"]?.doubleValue, $0["output"]?.doubleValue, $0["db"]?.doubleValue]
        }
        XCTAssertEqual(routes, [[0, 0, 0], [1, 1, 0]], "file channel n to cue output n")
    }

    func testAFadesModeIsChosenOnItsLevelsTabAndStored() async throws {
        showCue(Self.fade)
        window.layoutIfNeeded()
        let popup = vc.fadeSettings.modePopup
        XCTAssertFalse(popup.isHiddenOrHasHiddenAncestor, "on the Levels tab")
        XCTAssertEqual(popup.titleOfSelectedItem, loc("fade.mode.absolute"), "absolute until chosen otherwise")
        popup.selectItem(at: 1)
        fire(popup)
        await assertEventually { self.modelKind(Self.fade)?["mode"] == .string("relative") }
        vc.setSelection([Self.intro])
        showCue(Self.fade)
        XCTAssertEqual(popup.titleOfSelectedItem, loc("fade.mode.relative"))
    }

    func testTheCurveTabDrawsTheChosenCurveRisingAndFalling() async throws {
        vc.setSelection([Self.fade])
        vc.tabView.selectTabViewItem(withIdentifier: "curve")
        window.layoutIfNeeded()
        let s = vc.fadeSettings
        XCTAssertFalse(s.risingView.isHiddenOrHasHiddenAncestor)
        XCTAssertGreaterThan(s.risingView.frame.width, 100)
        XCTAssertEqual(s.risingView.frame.width, s.fallingView.frame.width, accuracy: 1)
        let i = try XCTUnwrap(FadeSettingsPanel.curves.firstIndex(of: "equal_power"))
        s.curvePopup.selectItem(at: i)
        fire(s.curvePopup)
        await assertEventually { self.modelKind(Self.fade)?["curve"] == .string("equal_power") }
        await assertEventually { s.risingView.curve == "equal_power" }
        XCTAssertEqual(s.risingView.duration, modelKind(Self.fade)?["duration"]?.doubleValue)
        // The engine's shapes: half way an equal-power fade is at sin(45 deg); falling mirrors rising.
        XCTAssertEqual(s.risingView.value(at: 0.5), sin(Double.pi / 4), accuracy: 1e-9)
        XCTAssertEqual(s.fallingView.value(at: 0.25), s.risingView.value(at: 0.75), accuracy: 1e-9)
        XCTAssertEqual(FadeCurveView.shape("linear", 0.3), 0.3, accuracy: 1e-12)
        XCTAssertEqual(FadeCurveView.shape("s_curve", 0.5), 0.5, accuracy: 1e-12)
        XCTAssertEqual(FadeCurveView.shape("exponential", 0.5), expm1(log(1000) / 2) / 999, accuracy: 1e-9)
        XCTAssertEqual(
            FadeCurveView.shape("logarithmic", 0.5), 1 - FadeCurveView.shape("exponential", 0.5), accuracy: 1e-9)
        // And it is drawn: a yellow line from the bottom left to the top right of the rising graph.
        let rep = try renderView(s.risingView)
        func yellow(_ x: Int, _ y: Int) -> Bool {
            guard let c = rep.colorAt(x: x, y: y) else { return false }
            return c.redComponent > 0.8 && c.greenComponent > 0.6 && c.blueComponent < 0.4
        }
        let left = (0..<rep.pixelsHigh).filter { yellow(3, $0) }
        let right = (0..<rep.pixelsHigh).filter { yellow(rep.pixelsWide - 4, $0) }
        XCTAssertGreaterThan(try XCTUnwrap(left.min()), rep.pixelsHigh * 3 / 4, "low at the start")
        XCTAssertLessThan(try XCTUnwrap(right.max()), rep.pixelsHigh / 4, "high at the end")
    }

    func testMatrixHitTesting() {
        vc.setSelection([Self.intro])
        let m = vc.audioLevels.matrix
        // The value boxes are the top row, their names under them, then the crosspoint rows.
        let row1 = LevelMatrixView.crossTop + LevelMatrixView.cellH * 0.5
        XCTAssertEqual(m.slot(at: NSPoint(x: LevelMatrixView.headW + 5, y: row1)), .cross(input: 0, output: 0))
        XCTAssertEqual(m.slot(at: NSPoint(x: 5, y: 5)), .master)
        XCTAssertEqual(m.slot(at: NSPoint(x: LevelMatrixView.headW + LevelMatrixView.cellW + 5, y: 5)), .output(1))
        XCTAssertNil(m.slot(at: NSPoint(x: 5, y: row1)), "a row label")
        XCTAssertNil(m.slot(at: NSPoint(x: 5000, y: 5)))
        XCTAssertNil(m.slot(at: NSPoint(x: LevelMatrixView.headW + 5, y: LevelMatrixView.cellH + 5)), "the names")
        let caption = LevelMatrixView.cellH + LevelMatrixView.headerH + LevelMatrixView.captionH / 2
        XCTAssertNil(m.slot(at: NSPoint(x: LevelMatrixView.headW + 5, y: caption)), "the captions over the grid")
    }

    func testGangedOutputsMoveTogether() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        for o in 0..<2 {
            p.assignGang(o == 0 ? "A" : "a", to: .output(o))
            await assertEventually { self.sent.count == o + 1 }
        }
        XCTAssertEqual(lastKind?["levels"]?["gangs"], .array([.array([.double(0), .double(1)])]))
        type(p, "-6")
        await assertEventually { self.sent.count == 3 }
        let outputs = lastKind?["levels"]?["outputs"]?.arrayValue
        XCTAssertEqual(outputs?.map { $0["db"] }, [.double(-6), .double(-6)])
    }

    func testSetAllSilent() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        fire(p.silentButton)
        await assertEventually { self.sent.count == 1 }
        let levels = lastKind?["levels"]
        XCTAssertEqual(levels?["master_db"], .double(-120))
        XCTAssertEqual(levels?["outputs"]?.arrayValue?.map { $0["db"] }, [.double(-120), .double(-120)])
    }

    func testModelChangeFromElsewhereShowsInTheMatrix() async throws {
        vc.setSelection([Self.intro])
        let cue = try XCTUnwrap(store.model?.cue(Self.intro)).raw
        var set = try XCTUnwrap(LevelSet(kind: cue["kind"] ?? .null))
        set.set(.master, db: -9)
        set.set(.cross(input: 1, output: 1), db: -4)
        let levels = try XCTUnwrap(set.json(into: cue["kind"] ?? .null)["levels"])
        // Another client of the same workspace (another window, a second Mac) sets them.
        let other = Rig(engine: rig.testEngine, workspace: rig.doc)
        let connected = await other.connect()
        XCTAssertTrue(connected)
        guard case .success = await other.store.send(command: .setProperty(Self.intro, .kindFields(["levels": levels])))
        else { return XCTFail("the other client's edit") }
        await assertEventually { self.vc.audioLevels.levelField.stringValue == "-9.0" }
        XCTAssertEqual(vc.audioLevels.levels?.db(.cross(input: 1, output: 1)), -4)
        other.client.stop()
    }

    func testLockedInShowMode() async throws {
        vc.setSelection([Self.intro])
        vc.isLocked = true
        let p = vc.audioLevels
        for c in [p.levelField, p.fader, p.gangsButton, p.clearMutesButton, p.silentButton] as [NSControl] {
            XCTAssertFalse(c.isEnabled)
        }
        window.layoutIfNeeded()
        XCTAssertFalse(p.muteFromMenu(.output(0)), "no mute menu in Show Mode")
        type(p, "-6")
        drag(p, to: [-3])
        p.matrix.onDrag?(.master, -3, true)
        fire(p.silentButton)
        await settle()
        XCTAssertTrue(sent.isEmpty, "nothing reaches the engine in Show Mode")
        XCTAssertEqual(p.levelField.stringValue, "0.0", "the shown level is the model's")
        vc.isLocked = false
        XCTAssertTrue(p.levelField.isEnabled)
    }

    // MARK: Fade cue

    func testFadeTabShowsTheFadeSettings() {
        vc.setSelection([Self.fade])
        let f = vc.fadeSettings
        XCTAssertEqual(f.durationField.stringValue, "00:05.000")
        XCTAssertEqual(f.curvePopup.indexOfSelectedItem, FadeSettingsPanel.curves.firstIndex(of: "equal_power"))
        XCTAssertEqual(f.stopCheck.state, .on)
        XCTAssertEqual(vc.fadeLevels.matrix.outputCount, 2, "the target Intro has two cue outputs")
        XCTAssertTrue(vc.fadeLevels.canSetFromTarget)
    }

    func testFadeDurationCurveAndStopSendTheKind() async throws {
        vc.setSelection([Self.fade])
        let f = vc.fadeSettings
        f.durationField.stringValue = "0:02.5"
        endEditing(f.durationField, f)
        await assertEventually { self.sent.count == 1 }
        // The duration has its own property: the rest of the kind is not resent.
        XCTAssertEqual(sent.last?.command["property"]?["type"], .string("duration"))
        XCTAssertEqual(sent.last?.command["property"]?["value"], .double(2.5))
        XCTAssertEqual(f.durationField.stringValue, "00:02.500")

        for (i, curve) in FadeSettingsPanel.curves.enumerated() {
            f.curvePopup.selectItem(at: i)
            fire(f.curvePopup)
            await assertEventually { self.lastKind?["curve"] == .string(curve) }
        }
        f.stopCheck.state = .off
        fire(f.stopCheck)
        await assertEventually { self.lastKind?["stop_target_when_done"] == .bool(false) }
        await assertEventually("the model answers") {
            self.modelKind(Self.fade)?["stop_target_when_done"] == .bool(false)
        }
        XCTAssertEqual(modelKind(Self.fade)?["duration"], .double(2.5), "no edit lost the one before it")
    }

    /// An edit made before the engine answered the one before it keeps that one (the answer, arriving
    /// late, also must not undo it on screen).
    func testQuickEditsOfOneKindKeepEachOther() async throws {
        vc.setSelection([Self.fade])
        let f = vc.fadeSettings
        f.durationField.stringValue = "2.5"
        endEditing(f.durationField, f)
        f.curvePopup.selectItem(at: 1)
        fire(f.curvePopup)
        type(vc.fadeLevels, "-20")  // the master, with the two edits before it unanswered
        await assertEventually { self.sent.count == 3 }
        XCTAssertEqual(Set(lastKind?.objectValue?.keys.map { $0 } ?? []), ["levels"])
        XCTAssertEqual(lastKind?["levels"]?["master_db"], .double(-20))
        await assertEventually("all answered") { self.modelKind(Self.fade)?["levels"]?["master_db"] == .double(-20) }
        XCTAssertEqual(modelKind(Self.fade)?["duration"], .double(2.5))
        XCTAssertEqual(modelKind(Self.fade)?["curve"], .string("linear"))
        XCTAssertEqual(vc.fadeSettings.curvePopup.indexOfSelectedItem, 1)
        await assertEventually("nothing left pending") { self.vc.pendingKinds.isEmpty }
    }

    /// #457: another client changes the cue's kind and the engine has done it before this app's copy
    /// has heard (nothing is awaited between the two). The curve edit names only the curve, so the
    /// other client's field survives; sending the whole kind put the old value back.
    func testAKindEditFromAnotherClientBeforeTheAnswerIsKept() async throws {
        vc.setSelection([Self.fade])
        let other = Rig(engine: rig.testEngine, workspace: rig.doc)
        let connected = await other.connect()
        XCTAssertTrue(connected)
        // Sent, not awaited: the engine applies it before this app's copy has heard of it.
        let edit = Task {
            await other.store.send(
                command: .setProperty(Self.fade, .kindFields(["stop_target_when_done": .bool(false)])))
        }
        XCTAssertEqual(modelKind(Self.fade)?["stop_target_when_done"], .bool(true), "this app has not heard yet")
        let f = vc.fadeSettings
        f.curvePopup.selectItem(at: 1)
        fire(f.curvePopup)
        await assertEventually { self.sent.count == 1 }
        XCTAssertEqual(Set(lastKindFields?.keys.map { $0 } ?? []), ["curve"], "only the edited field is sent")
        guard case .success = await edit.value else { return XCTFail("the other client's edit") }
        await assertEventually("both edits are in the model") {
            let kind = self.modelKind(Self.fade)
            return kind?["curve"] == .string(FadeSettingsPanel.curves[1])
                && kind?["stop_target_when_done"] == .bool(false)
        }
        await assertEventually("nothing left pending") { self.vc.pendingKinds.isEmpty }
        XCTAssertEqual(f.stopCheck.state, .off, "the panel shows the other client's edit, not the stale overlay")
        other.client.stop()
    }

    func testFadeInvalidDurationIsRefusedLocally() async throws {
        vc.setSelection([Self.fade])
        let f = vc.fadeSettings
        f.durationField.stringValue = "soon"
        endEditing(f.durationField, f)
        await settle()
        XCTAssertTrue(sent.isEmpty)
        XCTAssertEqual(f.durationField.stringValue, "00:05.000")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.time"))
    }

    func testFadeTargetLevelsActivateAndDeactivate() async throws {
        vc.setSelection([Self.fade])
        let p = vc.fadeLevels
        p.select(.output(1))
        XCTAssertEqual(p.activeCheck.state, .off, "the fixture fades the master only")
        type(p, "-20")
        await assertEventually { self.sent.count == 1 }
        XCTAssertEqual(lastKind?["levels"]?["output_db"], .array([.null, .double(-20)]))
        XCTAssertEqual(p.activeCheck.state, .on)

        p.select(.cross(input: 0, output: 1))
        p.activeCheck.state = .on
        fire(p.activeCheck)
        await assertEventually { self.sent.count == 2 }
        XCTAssertEqual(
            lastKind?["levels"]?["crosspoints"]?.arrayValue,
            [.object(["input": .double(0), "output": .double(1), "db": .double(-120)])], "activated at -inf, as in QLab"
        )

        p.select(.master)
        p.activeCheck.state = .off
        fire(p.activeCheck)
        await assertEventually { self.sent.count == 3 }
        XCTAssertEqual(lastKind?["levels"]?["master_db"], .null)
        XCTAssertNil(lastKind?["duration"], "a levels edit does not send the fade's settings")
        XCTAssertEqual(modelKind(Self.fade)?["duration"], .double(5), "and they stay")
    }

    func testFadeSetLevelsFromTarget() async throws {
        vc.setSelection([Self.intro])
        type(vc.audioLevels, "-4")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-4) }
        vc.setSelection([Self.fade])
        let p = vc.fadeLevels
        fire(p.fromTargetButton)
        await assertEventually { self.sent.count == 2 }
        let levels = lastKind?["levels"]
        XCTAssertEqual(levels?["master_db"], .double(-4))
        XCTAssertEqual(levels?["output_db"], .array([.double(0), .double(0)]))
    }

    func testFadeControlsLockedInShowMode() async throws {
        vc.setSelection([Self.fade])
        vc.isLocked = true
        let f = vc.fadeSettings
        for c in [f.durationField, f.curvePopup, f.stopCheck] as [NSControl] { XCTAssertFalse(c.isEnabled) }
        f.curvePopup.selectItem(at: 1)
        fire(f.curvePopup)
        type(vc.fadeLevels, "-3")
        await settle()
        XCTAssertTrue(sent.isEmpty)
        XCTAssertFalse(vc.fadeLevels.activeCheck.isEnabled)
    }

    /// Levels tab, faders: they run from the workspace's minimum to its maximum with 0 dB part-way up, and
    /// a level above 0 dB shows at its own place.
    func testLevelsFadersReachTheWorkspaceMaximumWithZeroDbPartWay() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        XCTAssertEqual(p.fader.maxValue, store.volumeLimits.max)
        XCTAssertGreaterThan(p.fader.maxValue, 0, "above 0 dB")
        XCTAssertEqual(p.fader.doubleValue, 0, "0 dB is part-way, not at the top")
        XCTAssertLessThan(p.fader.doubleValue, p.fader.maxValue)
        type(p, "+6")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(6) }
        XCTAssertEqual(p.levelField.stringValue, "6.0")
        XCTAssertEqual(p.fader.doubleValue, 6, "at its own place")
        XCTAssertEqual(p.levels?.master, 6, "showing the fader did not change the level")
        drag(p, to: [store.volumeLimits.max])
        await assertEventually {
            self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(self.store.volumeLimits.max)
        }
    }

    /// Levels tab: a button clears every mute, a button opens the cue's patch editor, an off crosspoint is
    /// drawn blank, and there is no block for a separately selected level.
    func testLevelsTabClearsMutesOpensThePatchEditorDrawsOffCrosspointsBlankAndHasNoSelectedLevelBlock()
        async throws
    {
        showCue(Self.intro)
        window.layoutIfNeeded()
        let p = vc.audioLevels
        // No block for the chosen level: no name of it, no mute box, no gang box in the tab.
        var texts: [String] = []
        var boxes: [String] = []
        func walk(_ v: NSView) {
            if let t = v as? NSTextField, !t.isHidden { texts.append(t.stringValue) }
            if let b = v as? NSButton, !b.isHidden { boxes.append(b.title) }
            v.subviews.forEach(walk)
        }
        walk(p.view)
        XCTAssertFalse(texts.contains(loc("levels.master")), "no name of the chosen level: \(texts)")
        XCTAssertFalse(boxes.contains(loc("levels.mute")), "no mute box: \(boxes)")
        XCTAssertEqual(
            boxes.filter { !$0.isEmpty }.sorted(),
            [
                loc("levels.clearMutes"), loc("levels.editPatch"), loc("levels.gangs"), loc("levels.setDefault"),
                loc("levels.setSilent"),
            ]
            .sorted(), "the column is buttons only")
        // Clear Mutes: off with nothing muted; on after two mutes; one click un-mutes both.
        XCTAssertFalse(p.clearMutesButton.isEnabled)
        XCTAssertTrue(p.muteFromMenu(.output(0)))
        XCTAssertTrue(p.muteFromMenu(.output(1)))
        await assertEventually("both muted") {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.allSatisfy { $0["mute"] == .bool(true) }
                == true
        }
        XCTAssertTrue(p.clearMutesButton.isEnabled)
        p.clearMutesButton.performClick(nil)
        await assertEventually("no mute is left") {
            self.modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.allSatisfy { $0["mute"] == .bool(false) }
                == true
        }
        XCTAssertEqual(
            modelKind(Self.intro)?["levels"]?["outputs"]?.arrayValue?.map { $0["db"] }, [.double(0), .double(0)],
            "the levels are kept")
        await assertEventually { !p.clearMutesButton.isEnabled }
        // Edit Patch opens the cue's patch.
        XCTAssertTrue(p.editPatchButton.isEnabled)
        var opened: [ModelID] = []
        vc.onEditPatch = { opened.append($0) }
        p.editPatchButton.performClick(nil)
        XCTAssertEqual(opened.count, 1, "the patch editor is asked for the cue's patch")
        // An off crosspoint is an empty box; a routed one shows its level.
        p.select(.cross(input: 0, output: 0))
        type(p, "-6")
        await assertEventually("the route is made") {
            self.modelKind(Self.intro)?["levels"]?["crosspoints"]?.arrayValue?.contains { $0["db"] == .double(-6) }
                == true
        }
        await assertEventually { p.levels?.db(.cross(input: 0, output: 0)) == -6 }
        let off = p.matrix.rect(of: .cross(input: 0, output: 1))
        let on = p.matrix.rect(of: .cross(input: 0, output: 0))
        let rep = try drawn(p.matrix)
        XCTAssertEqual(inkedPixels(rep, off.insetBy(dx: 4, dy: 4)), 0, "an off crosspoint is blank")
        XCTAssertGreaterThan(inkedPixels(rep, on.insetBy(dx: 4, dy: 4)), 0, "a routed one shows its level")
    }

    /// In a light window an off crosspoint is a light empty box and a fader knob has body against its
    /// track; with no channel sent, the meter draws no bar at all.
    func testLevelsTabReadsInALightWindowAndDrawsNoEmptyMeterBar() async throws {
        showCue(Self.intro)
        window.layoutIfNeeded()
        let p = vc.audioLevels
        func sum(_ c: NSColor) -> CGFloat { c.redComponent + c.greenComponent + c.blueComponent }
        let off = p.matrix.rect(of: .cross(input: 0, output: 1)).insetBy(dx: 4, dy: 4)
        func boxFill(_ name: NSAppearance.Name) throws -> CGFloat {
            p.matrix.appearance = NSAppearance(named: name)
            var rep: NSBitmapImageRep?
            p.matrix.effectiveAppearance.performAsCurrentDrawingAppearance { rep = try? self.drawn(p.matrix) }
            let r = try XCTUnwrap(rep)
            let c = try XCTUnwrap(
                r.colorAt(x: Int(off.minX), y: r.pixelsHigh - 1 - Int(off.minY))?.usingColorSpace(.sRGB))
            return sum(c)
        }
        // The window's own background shows through the translucent box: compare against the dark box.
        XCTAssertGreaterThan(try boxFill(.aqua), try boxFill(.darkAqua) + 0.9, "light box is light, dark box dark")
        XCTAssertGreaterThan(try boxFill(.aqua), 1.5, "a light well in a light window")
        // The knob: not near-white on a light window.
        let fader = try XCTUnwrap(p.faders.first)
        fader.appearance = NSAppearance(named: .aqua)
        let rep = try renderView(fader, background: .white)
        let knob = fader.cell.map { ($0 as? NSSliderCell)?.knobRect(flipped: fader.isFlipped) } ?? nil
        let k = try XCTUnwrap(knob)
        let c = try XCTUnwrap(
            rep.colorAt(x: Int(k.midX * 2), y: Int((fader.bounds.height - k.midY) * 2))?.usingColorSpace(.sRGB))
        XCTAssertLessThan(sum(c), 2.4, "the knob has body against a white window: \(c)")
        // No channels sent: nothing drawn.
        let meter = MeterView()
        meter.frame = NSRect(x: 0, y: 0, width: 100, height: 22)
        let mrep = try renderView(meter)
        XCTAssertEqual(mrep.colorAt(x: 10, y: 10)?.alphaComponent ?? 1, 0, accuracy: 0.001, "no empty bar")
    }

    private func drawn(_ m: LevelMatrixView) throws -> NSBitmapImageRep {
        let rep = try XCTUnwrap(
            NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: Int(m.bounds.width), pixelsHigh: Int(m.bounds.height),
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                bytesPerRow: 0, bitsPerPixel: 0))
        let context = try XCTUnwrap(NSGraphicsContext(bitmapImageRep: rep))
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        m.draw(m.bounds)
        NSGraphicsContext.restoreGraphicsState()
        return rep
    }

    /// Pixels in `rect` (view coordinates) that differ from the box's own fill: the digits.
    private func inkedPixels(_ rep: NSBitmapImageRep, _ rect: NSRect) -> Int {
        func sum(_ c: NSColor) -> CGFloat { c.redComponent + c.greenComponent + c.blueComponent }
        guard let fill = rep.colorAt(x: Int(rect.minX), y: rep.pixelsHigh - 1 - Int(rect.minY))?.usingColorSpace(.sRGB)
        else { return 0 }
        var n = 0
        // The bitmap is not flipped and the view is: pixel rows run from the view's bottom.
        for vy in Int(rect.minY)..<Int(rect.maxY) {
            let y = rep.pixelsHigh - 1 - vy
            for x in Int(rect.minX)..<Int(rect.maxX) {
                if let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB), abs(sum(c) - sum(fill)) > 0.3 { n += 1 }
            }
        }
        return n
    }

    // MARK: A full 64-output patch

    func testSixtyFourOutputMatrixStaysResponsive() throws {
        var kind = TestSnapshots.audioKind("big.wav")
        let output = JSONValue.object(["db": .double(-6), "mute": .bool(false)])
        let points = (0..<64).flatMap { i in
            (0..<64).map { o in
                JSONValue.object([
                    "input": .int(Int64(i)), "output": .int(Int64(o)), "db": .double(-3), "mute": .bool(false),
                ])
            }
        }
        kind["levels"] = .object([
            "master_db": .double(0), "outputs": .array(Array(repeating: output, count: 64)),
            "crosspoints": .array(points), "gangs": .array([]),
        ])
        let set = try XCTUnwrap(LevelSet(kind: kind))
        XCTAssertEqual(set.crosspoints.count, 4096)
        let panel = LevelsPanel(isFade: false)
        panel.view.frame = NSRect(x: 0, y: 0, width: 800, height: 300)
        panel.show(set, inputs: 64, outputs: 64)
        panel.view.layoutSubtreeIfNeeded()
        let m = panel.matrix
        // A whole redraw: the cells are drawn text, not 4,096 views.
        _ = try renderView(m)
        _ = set.json(into: kind)
        XCTAssertEqual(m.subviews.count, 0, "no per-cell views")
        XCTAssertEqual(m.outputCount, 64)
        XCTAssertEqual(m.inputs, 64)
        XCTAssertEqual(m.slot(at: NSPoint(x: m.bounds.maxX - 1, y: m.bounds.maxY - 1)), .cross(input: 63, output: 63))
    }
}

/// #445: the Basics tab at the inspector's real height (240 pt) and at minimum and typical widths.
@MainActor
final class InspectorBasicsLayoutTests: XCTestCase {
    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!

    override func setUp() async throws {
        rig = try Rig.offline(TestSnapshots.show())
        vc = InspectorViewController(store: rig.store)
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
    }

    override func tearDown() async throws {
        window?.close()
        (window, vc, rig) = (nil, nil, nil)
    }

    private func laidOut(_ cue: ModelID, width: CGFloat) {
        vc.setSelection([cue])
        window.setContentSize(NSSize(width: width, height: 240))
        vc.view.layoutSubtreeIfNeeded()
        vc.tabView.selectedTabViewItem?.view?.layoutSubtreeIfNeeded()
    }

    private func frame(_ v: NSView) -> NSRect { vc.view.convert(v.bounds, from: v) }

    func testNumberAndTargetKeepUsableWidthWithoutOverlap() {
        for width in [CGFloat(520), 900] {
            laidOut(TestSnapshots.intro, width: width)
            XCTAssertEqual(window.contentView?.frame.width, width, "the Basics view must not force the window wider")
            XCTAssertLessThanOrEqual(vc.view.fittingSize.width, 520, "minimum width")
            XCTAssertFalse(vc.tabView.selectedTabViewItem?.view?.hasAmbiguousLayout ?? true)
            let fields: [(String, NSView)] = [
                ("number", vc.numberField), ("duration", vc.durationField), ("preWait", vc.preWaitField),
                ("postWait", vc.postWaitField), ("name", vc.nameField), ("target", vc.targetField),
                ("choose", vc.chooseButton),
            ]
            for (name, v) in fields {
                let f = frame(v)
                XCTAssertGreaterThanOrEqual(f.width, name == "target" ? 90 : 60, "\(name) at \(width)")
                XCTAssertGreaterThanOrEqual(f.height, 14, "\(name) height at \(width)")
                XCTAssertTrue(vc.view.bounds.insetBy(dx: -1, dy: -1).contains(f), "\(name) clipped at \(width)")
            }
            for (i, a) in fields.enumerated() {
                for b in fields[(i + 1)...] {
                    XCTAssertFalse(frame(a.1).intersects(frame(b.1)), "\(a.0) overlaps \(b.0) at \(width)")
                }
            }
            XCTAssertLessThanOrEqual(frame(vc.targetField).maxX, frame(vc.chooseButton).minX, "\(width)")
            XCTAssertEqual(vc.targetField.lineBreakMode, .byTruncatingMiddle)
        }
    }

    func testTargetTooltipCarriesTheFullPath() {
        laidOut(TestSnapshots.intro, width: 900)
        vc.targetField.stringValue = "/Volumes/Show/Act 1/Sound/intro.wav"
        vc.render()
        XCTAssertEqual(vc.targetField.toolTip, vc.targetField.stringValue)
    }
}

/// Tab order by cue kind, the Devamp Settings tab and the Target's clear button (#393).
@MainActor
final class InspectorTabOrderTests: XCTestCase {
    static let devamp: ModelID = "00000000-0000-0000-0000-000000000040"
    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!

    override func setUp() async throws {
        rig = try await Rig.open("Demo") { show in
            try editShow(show.appendingPathComponent("Demo.openlab")) { ws in
                var extra = TestSnapshots.cueJSON(
                    id: Self.devamp.raw, number: "9", name: "Out of loop",
                    kind: .object([
                        "type": .string("devamp"), "target": .string(TestSnapshots.intro.raw),
                        "start_next_cue": .bool(false), "stop_target_when_slice_ends": .bool(false),
                        "devamp_mode": .string("current_slice"),
                    ]))
                extra.set("triggers", .object(["hotkey": .null, "osc": .null]))
                extra.set("second_trigger", .string("nothing"))
                extra.set("second_trigger_on_release", .bool(false))
                guard var lists = ws["cue_lists"]?.arrayValue else { return }
                lists[0].set("cues", .array((lists[0]["cues"]?.arrayValue ?? []) + [extra]))
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

    private func tabs() -> [String] { vc.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    private var sent: [JSONValue] { rig.wire.commands.compactMap { JSONValue.parse((try? $0.json.text()) ?? "") } }

    private func kind(_ id: ModelID) -> JSONValue? { normalised(rig.store.model?.cue(id)?.raw["kind"]) }

    /// Every kind's tabs are Basics and Triggers, then its own, in the table's order.
    func testTabsFollowTheTableForEveryKind() {
        for kind in CueKindStyle.kinds.keys.sorted() {
            let expected = InspectorTabs.ids(forKind: kind)
            XCTAssertEqual(Array(expected.prefix(2)), ["basics", "triggers"], kind)
            let cue = rig.store.model?.cueLists.flatMap { $0.cues }.first { $0.kindType == kind }
            guard let cue else { continue }
            vc.setSelection([cue.id])
            XCTAssertEqual(tabs(), expected, kind)
        }
        XCTAssertEqual(InspectorTabs.ids(forKind: "fade"), ["basics", "triggers", "curve", "fadeLevels"])
        XCTAssertEqual(InspectorTabs.ids(forKind: "devamp"), ["basics", "triggers", "devamp"])
    }

    func testChangingKindKeepsTheSelectedCommonTab() {
        vc.setSelection([TestSnapshots.intro])
        vc.tabView.selectTabViewItem(at: 1)
        vc.setSelection([TestSnapshots.fade])
        XCTAssertEqual(vc.tabView.selectedTabViewItem?.identifier as? String, "triggers")
    }

    func testDevampSettingsEditTheKind() async {
        vc.setSelection([Self.devamp])
        XCTAssertEqual(vc.devampStartNextCheck.state, .off)
        vc.devampStartNextCheck.performClick(nil)
        await assertEventually { self.sent.count == 1 }
        vc.devampModePopup.selectItem(at: 1)
        vc.devampChanged(vc.devampModePopup)
        await assertEventually { self.sent.count == 2 }
        XCTAssertEqual(sent.last?["property"]?["type"], .string("kind_fields"))
        XCTAssertEqual(normalised(sent.first?["property"]?["value"]), .object(["start_next_cue": .bool(true)]))
        let kind = normalised(sent.last?["property"]?["value"])
        XCTAssertEqual(kind, .object(["devamp_mode": .string("looping_cue")]), "only the changed field is sent")
        await assertEventually("the engine stored it") {
            self.kind(Self.devamp)?["devamp_mode"] == .string("looping_cue")
        }
        XCTAssertEqual(self.kind(Self.devamp)?["start_next_cue"], .bool(true))
        XCTAssertEqual(self.kind(Self.devamp)?["target"], .string(TestSnapshots.intro.raw))
    }

    func testShowModeLocksTheDevampTabAndClearButton() async {
        vc.setSelection([Self.devamp])
        vc.isLocked = true
        for c in [vc.devampStartNextCheck, vc.devampStopCheck, vc.devampModePopup, vc.clearTargetButton] as [NSControl]
        {
            XCTAssertFalse(c.isEnabled)
        }
        vc.devampStartNextCheck.state = .on
        vc.devampChanged(vc.devampStartNextCheck)
        vc.clearTargetClicked()
        await settle()
        XCTAssertTrue(sent.isEmpty, "locked edits send nothing")
    }

    func testClearTargetOnAControlCueSendsNoTarget() async {
        vc.setSelection([Self.devamp])
        XCTAssertTrue(vc.clearTargetButton.isEnabled)
        vc.clearTargetClicked()
        await assertEventually { !self.sent.isEmpty }
        XCTAssertEqual(sent.last?["property"]?["type"], .string("target"))
        XCTAssertEqual(sent.last?["property"]?["value"], .null)
        await assertEventually { self.vc.clearTargetButton.isEnabled == false }
    }

    func testClearTargetOnAnAudioCueDropsTheFile() async {
        vc.setSelection([TestSnapshots.intro])
        XCTAssertTrue(vc.clearTargetButton.isEnabled)
        vc.clearTargetClicked()
        await assertEventually { !self.sent.isEmpty }
        XCTAssertEqual(sent.last?["property"]?["type"], .string("media"))
        XCTAssertEqual(sent.last?["property"]?["value"], .null)
        await assertEventually("the engine dropped the file") { self.kind(TestSnapshots.intro)?["media"] == .null }
    }

    /// Labels sit left of the fields, in one column, and still fit 520 x 240 and 900 wide.
    func testBasicsLabelsSitLeftOfTheirFieldsInOneColumn() {
        for width in [CGFloat(520), 900] {
            vc.setSelection([TestSnapshots.intro])
            window.setContentSize(NSSize(width: width, height: 240))
            vc.view.layoutSubtreeIfNeeded()
            vc.tabView.selectedTabViewItem?.view?.layoutSubtreeIfNeeded()
            func frame(_ v: NSView) -> NSRect { vc.view.convert(v.bounds, from: v) }
            let column = [vc.numberField, vc.durationField, vc.preWaitField, vc.postWaitField, vc.continuePopup]
            for (a, b) in zip(column, column.dropFirst()) {
                XCTAssertLessThan(frame(a).midY - frame(b).midY, 40, "one column, top to bottom at \(width)")
                XCTAssertGreaterThan(frame(a).midY, frame(b).midY, "\(width)")
                XCTAssertEqual(frame(a).minX, frame(b).minX, accuracy: 0.5, "fields line up at \(width)")
            }
            for f in column {
                XCTAssertTrue(vc.view.bounds.insetBy(dx: -1, dy: -1).contains(frame(f)), "clipped at \(width)")
                let label = f.superview?.subviews.first as? NSTextField
                XCTAssertNotNil(label)
                if let label {
                    XCTAssertLessThanOrEqual(frame(label).maxX, frame(f).minX, "label left of field at \(width)")
                    XCTAssertEqual(frame(label).midY, frame(f).midY, accuracy: 8, "same row at \(width)")
                }
            }
            XCTAssertTrue(
                vc.view.bounds.insetBy(dx: -1, dy: -1).contains(frame(vc.clearTargetButton)),
                "clear clipped at \(width)")
        }
    }
}
