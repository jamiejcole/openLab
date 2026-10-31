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
    static let intro = CueListHarness.intro
    static let fade = CueListHarness.fade

    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!

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

    private var sent: [(command: JSONValue, coalesce: String?)] {
        rig.engine.received.compactMap {
            if case .command(_, let c, let key) = $0 {
                return (JSONValue.parse((try? c.json.text()) ?? "") ?? .null, key)
            }
            return nil
        }
    }

    private func kindOf(_ command: JSONValue?) -> JSONValue? {
        guard command?["type"]?.stringValue == "set_cue_property", command?["property"]?["type"]?.stringValue == "kind_fields"
        else { return nil }
        return normalised(command?["property"]?["value"])
    }

    private var lastKind: JSONValue? { kindOf(sent.last?.command) }
    private var lastKindFields: [String: JSONValue]? { lastKind?.objectValue }

    private func modelKind(_ id: ModelID) -> JSONValue? { normalised(rig.store.model?.cue(id)?.raw["kind"]) }

    private func tabs() -> [String] { vc.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    private func endEditing(_ field: NSTextField, _ owner: NSTextFieldDelegate) {
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
        XCTAssertEqual(tabs(), ["basics", "triggers", "timeLoops", "levels"])
        vc.setSelection([Self.fade])
        XCTAssertEqual(tabs(), ["basics", "triggers", "fade"])
        vc.setSelection([CueListHarness.group])
        XCTAssertEqual(tabs(), ["basics", "mode", "triggers"])
    }

    func testMatrixShowsTheCueRouting() {
        vc.setSelection([Self.intro])
        XCTAssertEqual(vc.audioLevels.matrix.outputCount, 2)
        XCTAssertEqual(vc.audioLevels.matrix.inputs, InspectorViewController.defaultInputs)
        XCTAssertEqual(vc.audioLevels.levelField.stringValue, "0.0", "the master is selected")
        XCTAssertEqual(vc.audioLevels.cellLabel.stringValue, loc("levels.master"))
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
        XCTAssertEqual(p.cellLabel.stringValue, String(format: loc("levels.cross"), 2, 1))
    }

    func testMuteKeepsTheLevel() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        p.select(.master)
        XCTAssertFalse(p.muteCheck.isEnabled, "the cue master has no mute")
        p.select(.cross(input: 0, output: 0))
        XCTAssertFalse(p.muteCheck.isEnabled, "a route that is not listed is silent already")
        type(p, "0")
        await assertEventually { self.sent.count == 1 }
        XCTAssertTrue(p.muteCheck.isEnabled)
        p.muteCheck.state = .on
        fire(p.muteCheck)
        await assertEventually { self.sent.count == 2 }
        XCTAssertEqual(lastKind?["levels"]?["crosspoints"]?.arrayValue?.first?["mute"], .bool(true))
        XCTAssertEqual(lastKind?["levels"]?["crosspoints"]?.arrayValue?.first?["db"], .double(0))
        p.select(.output(0))
        p.muteCheck.state = .on
        fire(p.muteCheck)
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

        // The fake merges edits with one key as the engine does: two undo steps, not six.
        _ = await rig.store.undo()
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(-5) }
        _ = await rig.store.undo()
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(0) }
        XCTAssertEqual(rig.store.document?.canUndo, false)
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

    func testMatrixHitTesting() {
        vc.setSelection([Self.intro])
        let m = vc.audioLevels.matrix
        let row1 = LevelMatrixView.headerH + LevelMatrixView.cellH * 1.5
        XCTAssertEqual(m.slot(at: NSPoint(x: LevelMatrixView.headW + 5, y: row1)), .cross(input: 0, output: 0))
        XCTAssertEqual(m.slot(at: NSPoint(x: 5, y: LevelMatrixView.headerH + 5)), .master)
        XCTAssertEqual(
            m.slot(at: NSPoint(x: LevelMatrixView.headW + LevelMatrixView.cellW + 5, y: LevelMatrixView.headerH + 5)),
            .output(1))
        XCTAssertNil(m.slot(at: NSPoint(x: 5, y: row1)), "a row label")
        XCTAssertNil(m.slot(at: NSPoint(x: 5000, y: 5)))
        XCTAssertNil(m.slot(at: NSPoint(x: LevelMatrixView.headW + 5, y: 5)), "the header")
    }

    func testGangedOutputsMoveTogether() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        for o in 0..<2 {
            p.select(.output(o))
            p.gangField.stringValue = o == 0 ? "A" : "a"
            endEditing(p.gangField, p)
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

    func testModelChangeFromElsewhereShowsInTheMatrix() throws {
        vc.setSelection([Self.intro])
        var cue = try XCTUnwrap(rig.store.model?.cue(Self.intro)).raw
        var set = try XCTUnwrap(LevelSet(kind: cue["kind"] ?? .null))
        set.set(.master, db: -9)
        set.set(.cross(input: 1, output: 1), db: -4)
        cue.set("kind", set.json(into: cue["kind"] ?? .null))
        let upsert =
            #"{"type":"cue","list":"\#(CueListHarness.list)","parent":null,"index":0,"cue":\#(try cue.text())}"#
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#
                )), .applied)
        XCTAssertEqual(vc.audioLevels.levelField.stringValue, "-9.0")
        XCTAssertEqual(vc.audioLevels.levels?.db(.cross(input: 1, output: 1)), -4)
    }

    func testRefusedEditPutsTheModelBack() async throws {
        vc.setSelection([Self.intro])
        rig.engine.refuseCommands = true
        type(vc.audioLevels, "-6")
        await assertEventually { self.vc.audioLevels.levels?.master == 0 }
        XCTAssertEqual(vc.audioLevels.levelField.stringValue, "0.0")
        XCTAssertFalse(vc.messageLabel.stringValue.isEmpty)
    }

    func testLockedInShowMode() async throws {
        vc.setSelection([Self.intro])
        vc.isLocked = true
        let p = vc.audioLevels
        for c in [p.levelField, p.fader, p.muteCheck, p.gangField, p.silentButton] as [NSControl] {
            XCTAssertFalse(c.isEnabled)
        }
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
        XCTAssertEqual(f.durationField.stringValue, "00:05.00")
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
        // The duration has its own property (M5.6b): the rest of the kind is not resent.
        XCTAssertEqual(sent.last?.command["property"]?["type"], .string("duration"))
        XCTAssertEqual(sent.last?.command["property"]?["value"], .double(2.5))
        XCTAssertEqual(f.durationField.stringValue, "00:02.50")

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
        let other = try await rig.engine.connect(info: EngineConnectionInfo(port: 1, token: rig.engine.token, pid: 2))
        other.send(
            #"{"type":"hello","protocol_version":\#(protocolVersion),"token":"\#(rig.engine.token)","client_name":"other","workspace":1,"pid":2}"#
        )
        other.send(
            #"{"type":"command","id":1,"command":{"type":"set_cue_property","id":"\#(Self.fade.raw)","property":{"type":"kind_fields","value":{"stop_target_when_done":false}}}}"#
        )
        XCTAssertEqual(modelKind(Self.fade)?["stop_target_when_done"], .bool(true), "this app has not heard yet")
        let f = vc.fadeSettings
        f.curvePopup.selectItem(at: 1)
        fire(f.curvePopup)
        await assertEventually { self.sent.count == 2 }
        XCTAssertEqual(Set(lastKindFields?.keys.map { $0 } ?? []), ["curve"], "only the edited field is sent")
        await assertEventually("both edits are in the model") {
            let kind = self.modelKind(Self.fade)
            return kind?["curve"] == .string(FadeSettingsPanel.curves[1]) && kind?["stop_target_when_done"] == .bool(false)
        }
    }

    func testFadeInvalidDurationIsRefusedLocally() async throws {
        vc.setSelection([Self.fade])
        let f = vc.fadeSettings
        f.durationField.stringValue = "soon"
        endEditing(f.durationField, f)
        await settle()
        XCTAssertTrue(sent.isEmpty)
        XCTAssertEqual(f.durationField.stringValue, "00:05.00")
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

    /// The fader stops at 0 dB; a typed level above it is kept and shows pinned.
    func testFaderTopIsZeroDbAndATypedLevelAboveItIsKept() async throws {
        vc.setSelection([Self.intro])
        let p = vc.audioLevels
        XCTAssertEqual(p.fader.maxValue, 0)
        type(p, "+6")
        await assertEventually { self.modelKind(Self.intro)?["levels"]?["master_db"] == .double(6) }
        XCTAssertEqual(p.levelField.stringValue, "6.0")
        XCTAssertEqual(p.fader.doubleValue, 0, "pinned at the top")
        XCTAssertEqual(p.levels?.master, 6, "showing the fader did not change the level")
    }

    // MARK: A full 64-output patch

    func testSixtyFourOutputMatrixStaysResponsive() throws {
        var kind = FakeEngine.audioKind("big.wav")
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
        let whole = try XCTUnwrap(m.bitmapImageRepForCachingDisplay(in: m.bounds))
        m.cacheDisplay(in: m.bounds, to: whole)
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

    private func laidOut(_ cue: ModelID, width: CGFloat) {
        vc.setSelection([cue])
        window.setContentSize(NSSize(width: width, height: 240))
        vc.view.layoutSubtreeIfNeeded()
        vc.tabView.selectedTabViewItem?.view?.layoutSubtreeIfNeeded()
    }

    private func frame(_ v: NSView) -> NSRect { vc.view.convert(v.bounds, from: v) }

    func testNumberAndTargetKeepUsableWidthWithoutOverlap() {
        for width in [CGFloat(520), 900] {
            laidOut(CueListHarness.intro, width: width)
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
        laidOut(CueListHarness.intro, width: 900)
        vc.targetField.stringValue = "/Volumes/Show/Act 1/Sound/intro.wav"
        vc.render()
        XCTAssertEqual(vc.targetField.toolTip, vc.targetField.stringValue)
    }
}
