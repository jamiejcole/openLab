// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// M5.6b (CUE-02, CT-14, CT-20): a Wait or Fade cue's duration and a Load cue's time are typed in
/// the inspector or, for the duration, in the cue list's Duration column (QLab 5, manual pp. 94-95,
/// 124-127, 131-132, 680-681). Bad text is refused with a message and never sent; good text goes
/// to the engine as the `duration` / `load_to_time` properties.
@MainActor
final class DurationEditingTests: XCTestCase {
    typealias H = CueListHarness
    typealias SendLog = CueEditingTests.SendLog

    static let wait: ModelID = "00000000-0000-0000-0000-000000000030"
    static let load: ModelID = "00000000-0000-0000-0000-000000000031"

    static let nested: ModelID = "00000000-0000-0000-0000-000000000040"

    /// A new cue of `kind` as the app adds one, with an id, number, name and kind fields.
    static func newCue(
        _ kind: NewCueKind, id: ModelID, number: String, name: String, _ fields: [String: JSONValue]
    ) throws -> JSONValue {
        var cue = try XCTUnwrap(kind.cueJSON(id: id, patch: nil))
        var k = try XCTUnwrap(cue["kind"])
        for (f, v) in fields { k.set(f, v) }
        cue.set("kind", k)
        cue.set("number", .string(number))
        cue.set("name", .string(name))
        return cue
    }

    static func extraCues() throws -> [JSONValue] {
        [
            try newCue(.wait, id: wait, number: "3", name: "Hold", ["duration": .double(2)]),
            try newCue(
                .load, id: load, number: "4", name: "Cue up",
                ["target": .string(H.intro.raw), "load_to_time": .double(1.5)]),
        ]
    }

    /// The demo show with a 2 s Wait cue and a Load cue (Intro to 1.5 s) after it, with no engine.
    static func snapshot() throws -> JSONValue {
        var s = try H.demoSnapshot()
        var lists = s["workspace"]?["cue_lists"]?.arrayValue ?? []
        lists[0].set("cues", .array((lists[0]["cues"]?.arrayValue ?? []) + (try extraCues())))
        s["workspace"]?.set("cue_lists", .array(lists))
        return s
    }

    /// The same on the real engine: the demo show, the Wait and Load cues added to it by command
    /// (`nestedWait`: and a 3 s Wait 2.4 at the end of the group). `engine`: one of the test's own.
    static func openRig(nestedWait: Bool = false, on engine: TestEngine? = nil) async throws -> Rig {
        let rig = try await Rig.open("Demo", on: engine)
        let list = TestSnapshots.mainList
        let extra = try extraCues()
        var adds: [Command] = [
            .addCue(extra[0], list: list, parent: nil, index: 3), .addCue(extra[1], list: list, parent: nil, index: 4),
        ]
        if nestedWait {
            let w = try newCue(.wait, id: nested, number: "2.4", name: "Nested hold", ["duration": .double(3)])
            adds.append(.addCue(w, list: list, parent: H.group, index: 3))
        }
        let added = await rig.store.send(command: try XCTUnwrap(Command.transaction(adds)))
        guard case .success = added else { throw TestEngineError.refused("\(added)") }
        return rig
    }

    private func cue(_ kind: JSONValue, id: ModelID = "c") -> Cue {
        let json = TestSnapshots.cueJSON(id: id.raw, number: nil, name: "", kind: kind)
        return try! Cue(json: json)
    }

    private func wire(_ c: Command?) -> JSONValue? { c.flatMap { JSONValue.parse((try? $0.json.text()) ?? "") } }

    private func property(_ id: ModelID, _ type: String, _ value: String) -> JSONValue? {
        JSONValue.parse(#"{"type":"set_cue_property","id":"\#(id)","property":{"type":"\#(type)","value":\#(value)}}"#)
    }

    // MARK: Parsing and the pure rules

    /// QLab's own examples (manual pp. 131-132): 100 s shows as 01:40, 65:02.5 as 1:05:02.5.
    func testTimeEntryFollowsQLabsExamples() {
        XCTAssertEqual(CueTime.parse("100"), 100)
        XCTAssertEqual(CueTime.format(100), "01:40.00")
        XCTAssertEqual(CueTime.parse("65:02.5"), 3902.5)
        XCTAssertEqual(CueTime.format(3902.5), "1:05:02.50")
        XCTAssertEqual(CueTime.parse("1:05:02.5"), 3902.5)
        XCTAssertEqual(CueTime.parse("0.0015"), 0.002, "rounded to the millisecond")
    }

    func testDurationCommandForWaitAndFade() throws {
        let wait = cue(.object(["type": .string("wait"), "duration": .double(2)]))
        let fade = cue(.object(["type": .string("fade"), "duration": .double(5), "target": .null]))
        for (c, text, seconds) in [
            (wait, "90", "90"), (wait, "0:02.5", "2.5"), (fade, "0", "0"), (fade, "24:00:00", "86400"),
        ] {
            let made = try InspectorEdit.durationCommand(text: text, cue: c).get()
            XCTAssertEqual(wire(made), property("c", "duration", seconds), text)
        }
        XCTAssertNil(try InspectorEdit.durationCommand(text: "2", cue: wait).get(), "the same duration sends nothing")
        XCTAssertNil(try InspectorEdit.durationCommand(text: "00:05.00", cue: fade).get())
    }

    func testDurationRejects() {
        let wait = cue(.object(["type": .string("wait"), "duration": .double(2)]))
        for text in ["", "soon", "-1", "+1", "1e3", "nan", "inf", "1:60", "24:00:00.001", "99999999999999999999"] {
            XCTAssertEqual(InspectorEdit.durationCommand(text: text, cue: wait), .failure(.invalidTime), text)
        }
        XCTAssertEqual(InspectorEdit.durationCommand(text: "1,5", cue: wait), .failure(.decimalComma))
        for kind in ["memo", "start", "load", "group"] {
            XCTAssertEqual(
                InspectorEdit.durationCommand(text: "1", cue: cue(.object(["type": .string(kind)]))),
                .failure(.noEditableDuration), kind)
        }
        XCTAssertEqual(
            InspectorEdit.durationCommand(text: "1", cue: cue(TestSnapshots.audioKind("a.wav"))),
            .failure(.noEditableDuration), "an Audio cue's duration comes from its file")
    }

    func testLoadTimeCommandAndRejects() async throws {
        let rig = try Rig.offline(Self.snapshot())
        let model = try XCTUnwrap(rig.store.model)
        let load = try XCTUnwrap(model.cue(Self.load))
        let made = try InspectorEdit.command(.loadTime, text: "1:30", cue: load, model: model).get()
        XCTAssertEqual(wire(made), property(Self.load, "load_to_time", "90"))
        XCTAssertNil(try InspectorEdit.command(.loadTime, text: "1.5", cue: load, model: model).get())
        XCTAssertEqual(
            InspectorEdit.command(.loadTime, text: "-10", cue: load, model: model), .failure(.negativeLoadTime))
        XCTAssertEqual(
            InspectorEdit.command(.loadTime, text: " -0:10", cue: load, model: model), .failure(.negativeLoadTime))
        XCTAssertEqual(InspectorEdit.command(.loadTime, text: "-x", cue: load, model: model), .failure(.invalidTime))
        XCTAssertEqual(InspectorEdit.command(.loadTime, text: "2,5", cue: load, model: model), .failure(.decimalComma))
        let wait = try XCTUnwrap(model.cue(Self.wait))
        XCTAssertEqual(InspectorEdit.command(.loadTime, text: "1", cue: wait, model: model), .failure(.noLoadTime))
        XCTAssertEqual(InspectorEdit.display(.loadTime, cue: load, model: model), "00:01.50")
        XCTAssertEqual(InspectorEdit.display(.loadTime, cue: wait, model: model), "")
        XCTAssertEqual(InspectorEdit.display(.duration, cue: wait, model: model), "00:02.00")
        XCTAssertEqual(InspectorEdit.display(.duration, cue: load, model: model), "")
    }

    func testEveryRejectionHasAMessage() {
        let all: [InspectorEdit.Rejection] = [
            .invalidTime, .decimalComma, .duplicateNumber, .unknownTarget, .selfTarget, .notApplicable, .invalidOSC,
            .noEditableDuration, .noLoadTime, .negativeLoadTime,
        ]
        for r in all { XCTAssertNotEqual(loc(r.messageKey), r.messageKey, "\(r) has no string") }
    }
}

/// The inspector's Duration field (Basics) and Load Time tab on the real engine.
@MainActor
final class InspectorDurationTests: XCTestCase {
    private var rig: Rig!
    private var vc: InspectorViewController!
    private var window: NSWindow!
    private var log: CueEditingTests.SendLog!

    override func setUp() async throws {
        rig = try await DurationEditingTests.openRig()
        vc = InspectorViewController(store: rig.store)
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
        log = CueEditingTests.SendLog(rig)
    }

    override func tearDown() async throws { window?.orderOut(nil) }

    private func edit(_ field: InspectorField, _ text: String) {
        vc.field(field)?.stringValue = text
        vc.commit(field)
    }

    private func kind(_ id: ModelID) -> JSONValue? { rig.store.model?.cue(id)?.raw["kind"] }

    private func tabs() -> [String] { vc.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    func testWaitDurationIsEditedInTheBasicsTab() async {
        vc.setSelection([DurationEditingTests.wait])
        XCTAssertTrue(vc.durationField.isEnabled)
        XCTAssertEqual(vc.durationField.stringValue, "00:02.00")
        edit(.duration, "90")
        XCTAssertEqual(vc.durationField.stringValue, "01:30.00", "what will be stored, at once")
        await assertEventually("the engine's echo") {
            self.kind(DurationEditingTests.wait)?["duration"]?.doubleValue == 90
        }
        XCTAssertEqual(vc.durationField.stringValue, "01:30.00")
        XCTAssertEqual(log.sent, 1, "one property sent")
        XCTAssertFalse(tabs().contains("loadTime"))
    }

    func testFadeDurationInBasicsSendsTheDurationAlone() async {
        vc.setSelection([CueListHarness.fade])
        XCTAssertTrue(vc.durationField.isEnabled)
        let before = kind(CueListHarness.fade)
        edit(.duration, "0:03")
        await assertEventually { self.kind(CueListHarness.fade)?["duration"]?.doubleValue == 3 }
        var after = kind(CueListHarness.fade)
        after?.set("duration", before?["duration"] ?? .null)
        XCTAssertEqual(after, before, "the rest of the kind is as it was")
        await assertEventually("the Fade tab follows") { self.vc.fadeSettings.durationField.stringValue == "00:03.00" }
    }

    func testOtherCuesHaveNoDurationToType() async {
        for id in [CueListHarness.intro, DurationEditingTests.load, CueListHarness.group] {
            vc.setSelection([id])
            XCTAssertFalse(vc.durationField.isEnabled, "\(id)")
            XCTAssertEqual(vc.durationField.stringValue, "", "\(id)")
        }
        // Reached anyway (a stale field): refused with a message, nothing sent.
        vc.setSelection([CueListHarness.intro])
        log.reset()
        edit(.duration, "5")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.noDuration"))
        await settle()
        XCTAssertEqual(log.sent, 0)
    }

    func testInvalidDurationIsRefusedWithoutSending() async {
        vc.setSelection([DurationEditingTests.wait])
        log.reset()
        for (text, key) in [
            ("soon", "inspector.invalid.time"), ("-2", "inspector.invalid.time"),
            ("25:00:00", "inspector.invalid.time"),
            ("1,5", "inspector.invalid.decimalComma"),
        ] {
            edit(.duration, text)
            XCTAssertEqual(vc.durationField.stringValue, "00:02.00", text)
            XCTAssertEqual(vc.messageLabel.stringValue, loc(key), text)
        }
        await settle()
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(kind(DurationEditingTests.wait)?["duration"]?.doubleValue, 2)
    }

    func testLoadTimeTabForLoadCuesOnly() async {
        vc.setSelection([DurationEditingTests.load])
        XCTAssertTrue(tabs().contains("loadTime"))
        XCTAssertEqual(vc.loadTimeField.stringValue, "00:01.50")
        edit(.loadTime, "1:30")
        await assertEventually { self.kind(DurationEditingTests.load)?["load_to_time"]?.doubleValue == 90 }
        log.reset()
        edit(.loadTime, "-10")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.negativeLoadTime"))
        XCTAssertEqual(vc.loadTimeField.stringValue, "01:30.00")
        await settle()
        XCTAssertEqual(log.sent, 0, "the negative time was not sent")
        vc.setSelection([DurationEditingTests.wait])
        XCTAssertFalse(tabs().contains("loadTime"))
    }

    func testRefusedDurationRevertsAndWarns() async throws {
        // A Wait cue the store holds and the engine does not: the engine refuses its new duration.
        let ghost = try DurationEditingTests.newCue(
            .wait, id: "00000000-0000-0000-0000-0000000000ee", number: "9", name: "Ghost", ["duration": .double(2)])
        let upsert = try CueListHarness.upsertJSON(ghost, index: 0)
        let frame =
            #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#
        XCTAssertEqual(rig.store.receive(try ServerMessage.decode(frame)), .applied)
        vc.setSelection(["00000000-0000-0000-0000-0000000000ee"])
        edit(.duration, "7")
        await assertEventually { self.vc.messageLabel.stringValue.contains("does not exist") }
        XCTAssertEqual(vc.durationField.stringValue, "00:02.00")
    }

    func testShowModeLocksBothFields() async {
        vc.setSelection([DurationEditingTests.wait])
        vc.isLocked = true
        log.reset()
        XCTAssertFalse(vc.durationField.isEnabled)
        edit(.duration, "9")
        vc.setSelection([DurationEditingTests.load])
        XCTAssertFalse(vc.loadTimeField.isEnabled)
        edit(.loadTime, "9")
        await settle()
        XCTAssertEqual(log.sent, 0, "nothing reaches the engine in Show Mode")
        vc.isLocked = false
        XCTAssertTrue(vc.loadTimeField.isEnabled)
        vc.setSelection([DurationEditingTests.wait])
        XCTAssertTrue(vc.durationField.isEnabled)
    }

    /// The Fade tab's duration goes as `duration`; a curve change sent before its answer is built
    /// from a kind that already has the new duration, so it does not put the old one back.
    func testFadeDurationThenCurveBeforeTheAnswerKeepsBoth() async {
        vc.setSelection([CueListHarness.fade])
        let f = vc.fadeSettings
        log.reset()
        // Both go out in this one turn, before either is answered.
        f.durationField.stringValue = "2.5"
        f.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: f.durationField))
        f.curvePopup.selectItem(at: 1)
        _ = f.curvePopup.target?.perform(f.curvePopup.action, with: f.curvePopup)
        await assertEventually { self.log.sent == 2 }
        await assertEventually("nothing left pending") { self.vc.pendingKinds.isEmpty }
        await assertEventually { self.kind(CueListHarness.fade)?["duration"] == .double(2.5) }
        XCTAssertEqual(kind(CueListHarness.fade)?["curve"], .string("linear"))
    }

    /// #354: a Duration typed in the cue list while an inspector kind edit is unanswered: the next
    /// inspector kind edit builds on both, so it does not put the old duration back. The engine is
    /// stopped (SIGSTOP) so the edits wait unanswered on its socket, then continued.
    func testCueListDurationEditIsSeenByAnInspectorKindEditInFlight() async throws {
        let engine = try await TestEngine.start()
        defer {
            kill(engine.info.pid, SIGCONT)
            engine.stop()
        }
        rig = try await DurationEditingTests.openRig(on: engine)
        vc = InspectorViewController(store: rig.store)
        window.contentView = vc.view
        log = CueEditingTests.SendLog(rig)
        vc.setSelection([CueListHarness.fade])
        let editor = CueEditor(store: rig.store, cueList: CueListViewController(store: rig.store))
        editor.trackPending = { [weak self] command, cue in self?.vc.trackPending(command, cue: cue) ?? {} }
        XCTAssertEqual(kill(engine.info.pid, SIGSTOP), 0)
        let f = vc.fadeSettings
        f.curvePopup.selectItem(at: 1)
        _ = f.curvePopup.target?.perform(f.curvePopup.action, with: f.curvePopup)
        await assertEventually { self.log.sent == 1 }
        let typed = Task { await editor.setDuration(of: CueListHarness.fade, text: "7") }
        await assertEventually { self.log.sent == 2 }
        f.curvePopup.selectItem(at: 0)
        _ = f.curvePopup.target?.perform(f.curvePopup.action, with: f.curvePopup)
        await assertEventually { self.log.sent == 3 }
        XCTAssertFalse(vc.pendingKinds.isEmpty, "unanswered while the engine is stopped")
        XCTAssertEqual(kill(engine.info.pid, SIGCONT), 0)
        _ = await typed.value
        await assertEventually("nothing left pending") { self.vc.pendingKinds.isEmpty }
        await assertEventually("the cue-list edit is kept") {
            self.kind(CueListHarness.fade)?["duration"]?.doubleValue == 7
        }
        XCTAssertNotEqual(kind(CueListHarness.fade)?["curve"], .string("linear"), "the last curve won")
    }
}

/// The cue list's Duration column, edited in place (double-click in the app).
@MainActor
final class CueListDurationTests: XCTestCase {
    typealias H = CueListHarness

    private func harness(nestedWait: Bool = false) async throws -> (H, CueEditor, CueEditingTests.SendLog, NSWindow) {
        let h = try await H(collapsed: !nestedWait, rig: try await DurationEditingTests.openRig(nestedWait: nestedWait))
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 600), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = h.vc.view
        let editor = CueEditor(store: h.store, cueList: h.vc)
        let log = CueEditingTests.SendLog(h.rig)
        editor.notify = { log.notices.append($0) }
        h.vc.onEditDuration = { id, text in Task { await editor.setDuration(of: id, text: text) } }
        return (h, editor, log, window)
    }

    private func endEditing(_ vc: CueListViewController) {
        vc.controlTextDidEndEditing(
            Notification(name: NSControl.textDidEndEditingNotification, object: vc.durationEditor))
    }

    func testOnlyWaitAndFadeCellsOpen() async throws {
        let (h, _, _, window) = try await harness()
        defer { window.orderOut(nil) }
        XCTAssertFalse(h.vc.beginDurationEdit(H.intro), "an audio cue's duration comes from its file")
        XCTAssertFalse(h.vc.beginDurationEdit(DurationEditingTests.load))
        XCTAssertFalse(h.vc.beginDurationEdit(H.group))
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        XCTAssertEqual(h.vc.editingDuration, DurationEditingTests.wait)
        XCTAssertEqual(h.vc.durationEditor.stringValue, "00:02.00")
        XCTAssertTrue(h.vc.durationEditor.superview === h.vc.outlineView)
        let row = h.spy.row(forItem: h.vc.node(DurationEditingTests.wait))
        let column = h.spy.column(withIdentifier: NSUserInterfaceItemIdentifier("duration"))
        XCTAssertEqual(h.vc.durationEditor.frame, h.spy.frameOfCell(atColumn: column, row: row))
    }

    /// #355: the open field is an accessibility child of the outline, with a localized label.
    func testOpenEditorIsExposedToAccessibility() async throws {
        let (h, _, _, window) = try await harness()
        defer { window.orderOut(nil) }
        XCTAssertEqual(h.vc.durationEditor.accessibilityLabel(), loc("cuelist.durationEditor.label"))
        XCTAssertFalse(
            (h.spy.accessibilityChildren() ?? []).contains { $0 as AnyObject === h.vc.durationEditor },
            "closed: not listed")
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        XCTAssertTrue((h.spy.accessibilityChildren() ?? []).contains { $0 as AnyObject === h.vc.durationEditor })
        XCTAssertTrue(h.vc.durationEditor.accessibilityParent() as AnyObject === h.spy)
        h.vc.cancelDurationEdit()
        XCTAssertFalse((h.spy.accessibilityChildren() ?? []).contains { $0 as AnyObject === h.vc.durationEditor })
    }

    /// #357: collapsing the group above, or resizing the column, moves the field with its cell; a cue
    /// collapsed out of sight closes the edit and sends nothing.
    func testEditorFollowsCollapseExpandAndColumnResize() async throws {
        let (h, _, log, window) = try await harness(nestedWait: true)
        defer { window.orderOut(nil) }
        let column = h.spy.column(withIdentifier: NSUserInterfaceItemIdentifier("duration"))
        func cell(_ id: ModelID) -> NSRect {
            h.spy.frameOfCell(atColumn: column, row: h.spy.row(forItem: h.vc.node(id)))
        }
        // A cue after the group moves up when the group collapses.
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        let before = h.vc.durationEditor.frame
        h.spy.collapseItem(h.vc.node(H.group))
        XCTAssertNotEqual(cell(DurationEditingTests.wait), before)
        XCTAssertEqual(h.vc.durationEditor.frame, cell(DurationEditingTests.wait), "follows the collapse")
        h.spy.expandItem(h.vc.node(H.group))
        XCTAssertEqual(h.vc.durationEditor.frame, cell(DurationEditingTests.wait), "follows the expand")
        // A column resize before the Duration column moves it sideways.
        let name = h.spy.tableColumns[h.spy.column(withIdentifier: NSUserInterfaceItemIdentifier("name"))]
        name.width += 40
        XCTAssertEqual(h.vc.durationEditor.frame, cell(DurationEditingTests.wait), "follows the column resize")
        // A cue inside the group: collapsing the group closes the edit; nothing is sent.
        log.reset()
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.nested))
        h.vc.durationEditor.stringValue = "9"
        h.spy.collapseItem(h.vc.node(H.group))
        XCTAssertNil(h.vc.editingDuration)
        XCTAssertNil(h.vc.durationEditor.superview)
        await settle()
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(h.store.model?.cue(DurationEditingTests.nested)?.raw["kind"]?["duration"]?.doubleValue, 3)
    }

    func testCommittingSendsTheDurationCommand() async throws {
        let (h, _, log, window) = try await harness()
        defer { window.orderOut(nil) }
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        h.vc.durationEditor.stringValue = "1:15"
        endEditing(h.vc)
        XCTAssertNil(h.vc.editingDuration)
        XCTAssertNil(h.vc.durationEditor.superview)
        await assertEventually("the engine stored it") {
            h.store.model?.cue(DurationEditingTests.wait)?.raw["kind"]?["duration"]?.doubleValue == 75
        }
        await assertEventually("the cell shows the stored duration") {
            h.cell("duration", DurationEditingTests.wait)?.label.stringValue == "01:15.00"
        }
        XCTAssertTrue(log.notices.isEmpty)
    }

    func testBadTextIsRefusedWithAMessageAndNeverSent() async throws {
        let (h, _, log, window) = try await harness()
        defer { window.orderOut(nil) }
        for (text, key) in [("abc", "inspector.invalid.time"), ("1,5", "inspector.invalid.decimalComma")] {
            XCTAssertTrue(h.vc.beginDurationEdit(H.fade))
            h.vc.durationEditor.stringValue = text
            endEditing(h.vc)
            await assertEventually { log.notices.last == loc(key) }
        }
        // Unchanged text sends nothing and says nothing.
        XCTAssertTrue(h.vc.beginDurationEdit(H.fade))
        endEditing(h.vc)
        await settle()
        XCTAssertEqual(log.notices.count, 2)
        XCTAssertEqual(log.sent, 0)
    }

    func testEscapeCancels() async throws {
        let (h, _, log, window) = try await harness()
        defer { window.orderOut(nil) }
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        h.vc.durationEditor.stringValue = "9"
        let handled = h.vc.control(
            h.vc.durationEditor, textView: NSTextView(), doCommandBy: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertTrue(handled)
        XCTAssertNil(h.vc.editingDuration)
        endEditing(h.vc)  // the end of editing that follows sends nothing
        await settle()
        XCTAssertEqual(log.sent, 0)
    }

    func testRefreshesKeepTheTypedTextAndADeletedCueClosesIt() async throws {
        let (h, _, log, window) = try await harness()
        defer { window.orderOut(nil) }
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        h.vc.durationEditor.stringValue = "4"
        // The column refreshes (the Wait runs, its progress shows) and another client adds a cue: the
        // text stays.
        let other = Rig(engine: h.rig.testEngine, workspace: h.rig.client.workspace)
        let connected = await other.connect()
        XCTAssertTrue(connected)
        _ = await other.store.transport(.start(cue: DurationEditingTests.wait))
        try await h.rig.advance(0.5)
        await assertEventually { h.store.cueStates[DurationEditingTests.wait]?.state == .running }
        let memo = try DurationEditingTests.newCue(
            .memo, id: "00000000-0000-0000-0000-000000000050", number: "0", name: "New", [:])
        _ = await other.store.send(command: .addCue(memo, list: TestSnapshots.mainList, parent: nil, index: 0))
        await assertEventually { h.vc.node("00000000-0000-0000-0000-000000000050") != nil }
        XCTAssertEqual(h.vc.durationEditor.stringValue, "4")
        let row = h.spy.row(forItem: h.vc.node(DurationEditingTests.wait))
        let column = h.spy.column(withIdentifier: NSUserInterfaceItemIdentifier("duration"))
        XCTAssertEqual(h.vc.durationEditor.frame, h.spy.frameOfCell(atColumn: column, row: row), "it follows the row")
        // The other client stops and deletes the cue: the edit closes and nothing is sent.
        _ = await other.store.transport(.hardStop(cue: DurationEditingTests.wait))
        try await h.rig.advance(0.05)
        log.reset()
        _ = await other.store.send(command: .removeCue(DurationEditingTests.wait))
        await assertEventually { h.vc.node(DurationEditingTests.wait) == nil }
        XCTAssertNil(h.vc.editingDuration)
        XCTAssertNil(h.vc.durationEditor.superview)
        await settle()
        XCTAssertEqual(log.sent, 0)
    }

    func testShowModeDropsTheEditAndRefusesNewOnes() async throws {
        let (h, editor, log, window) = try await harness()
        defer { window.orderOut(nil) }
        XCTAssertTrue(h.vc.beginDurationEdit(DurationEditingTests.wait))
        h.vc.durationEditor.stringValue = "9"
        h.vc.isEditLocked = true
        editor.isLocked = { true }
        XCTAssertNil(h.vc.editingDuration)
        XCTAssertFalse(h.vc.beginDurationEdit(DurationEditingTests.wait))
        await editor.setDuration(of: DurationEditingTests.wait, text: "9")
        await settle()
        XCTAssertEqual(log.sent, 0)
    }

    /// Save, Close and Quit commit what is typed in the Duration cell and wait for the answer.
    func testPendingCellEditIsCommittedBeforeSave() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 1000, height: 400)
        XCTAssertTrue(wc.cueList.beginDurationEdit(H.fade))
        wc.cueList.durationEditor.stringValue = "12"
        let ok = await wc.commitPendingEdits()
        XCTAssertTrue(ok)
        XCTAssertEqual(
            rig.store.model?.cue(H.fade)?.raw["kind"]?["duration"]?.doubleValue, 12, "answered before it returns")
        XCTAssertNil(wc.cueList.editingDuration)
        // A bad one stops the save.
        let log = CueEditingTests.SendLog(rig)
        XCTAssertTrue(wc.cueList.beginDurationEdit(H.fade))
        wc.cueList.durationEditor.stringValue = "later"
        let refused = await wc.commitPendingEdits()
        XCTAssertFalse(refused)
        XCTAssertEqual(log.sent, 0)
    }
}
