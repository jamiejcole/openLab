// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

final class InspectorTimeTests: XCTestCase {
    func testFormat() {
        XCTAssertEqual(InspectorTime.format(0), "00:00.000")
        XCTAssertEqual(InspectorTime.format(60), "01:00.000")
        XCTAssertEqual(InspectorTime.format(1.2345), "00:01.235")
        XCTAssertEqual(InspectorTime.format(3599.9994), "59:59.999")
        XCTAssertEqual(InspectorTime.format(3600), "1:00:00.000")
        XCTAssertEqual(InspectorTime.format(86400), "24:00:00.000")
        XCTAssertEqual(InspectorTime.format(-1), "")
        XCTAssertEqual(InspectorTime.format(.nan), "")
        XCTAssertEqual(InspectorTime.format(.infinity), "")
    }

    func testParseAcceptedForms() {
        let cases: [(String, Double)] = [
            ("0", 0), ("5", 5), ("5.5", 5.5), ("90", 90), ("01:00.000", 60), ("1:30", 90), ("1:30.25", 90.25),
            ("00:00.001", 0.001), ("1:00:00", 3600), ("24:00:00.000", 86400), ("  2.5 ", 2.5), ("0.0004", 0),
        ]
        for (text, seconds) in cases { XCTAssertEqual(InspectorTime.parse(text), seconds, text) }
    }

    func testParseRejects() {
        for text in [
            "", " ", "abc", "-1", "+1", "1e3", "nan", "inf", "1.", ".5", "1..2", "1:60", "1:2:3:4", ":30", "1::30",
            "60:00:00", "1:60:00", "24:00:00.001", "99999999999999999999", "1,5", "٣",
        ] {
            XCTAssertNil(InspectorTime.parse(text), text)
        }
    }

    func testRoundTrip() {
        for s in [0, 0.001, 1, 59.999, 60, 61.5, 3599.999, 3600, 7322.25, 86400] as [Double] {
            XCTAssertEqual(InspectorTime.parse(InspectorTime.format(s)), s, "\(s)")
        }
    }
}

final class EditCommandTests: XCTestCase {
    private func shape(_ c: Command) throws -> JSONValue? { JSONValue.parse(try c.json.text()) }

    func testCommandShapesMatchCore() throws {
        let id: ModelID = "00000000-0000-0000-0000-00000000000a"
        // The fixture core's tests write (engine/fixtures/protocol/client/command.json).
        let fixture = try Fixtures.json("client", "command")["command"]
        XCTAssertEqual(try shape(.setProperty("00000000-0000-0000-0000-00000000000a", .name("Storm"))), fixture)
        let cases: [(Command, String)] = [
            (.setProperty(id, .notes("n")), #"{"type":"notes","value":"n"}"#),
            (.setProperty(id, .color("blue")), #"{"type":"color","value":"blue"}"#),
            (.setProperty(id, .flagged(true)), #"{"type":"flagged","value":true}"#),
            (.setProperty(id, .armed(false)), #"{"type":"armed","value":false}"#),
            (.setProperty(id, .preWait(1.5)), #"{"type":"pre_wait","value":1.5}"#),
            (.setProperty(id, .postWait(60)), #"{"type":"post_wait","value":60}"#),
            (.setProperty(id, .continueMode("auto_follow")), #"{"type":"continue_mode","value":"auto_follow"}"#),
            (.setProperty(id, .target("t")), #"{"type":"target","value":"t"}"#),
            (.setProperty(id, .target(nil)), #"{"type":"target","value":null}"#),
        ]
        for (command, property) in cases {
            let expected = JSONValue.parse(
                #"{"type":"set_cue_property","id":"\#(id)","property":\#(property)}"#)
            XCTAssertEqual(try shape(command), expected, property)
        }
        XCTAssertEqual(
            try shape(.setNumber(id, "7")),
            JSONValue.parse(#"{"type":"set_cue_numbers","numbers":[{"id":"\#(id)","number":"7"}]}"#))
        XCTAssertEqual(
            try shape(.setNumber(id, nil)),
            JSONValue.parse(#"{"type":"set_cue_numbers","numbers":[{"id":"\#(id)","number":null}]}"#))
    }
}

/// The inspector against the fake engine: the demo show (Intro #1, a fade cue targeting it, a
/// group of three audio cues).
@MainActor
final class InspectorTests: XCTestCase {
    static let intro = CueListHarness.intro
    static let fade = CueListHarness.fade
    static let door = CueListHarness.door

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

    private var commands: [JSONValue] {
        rig.engine.received.compactMap {
            if case .command(_, let c, _) = $0 { return JSONValue.parse((try? c.json.text()) ?? "") }
            return nil
        }
    }

    private func property(_ type: String, _ value: String, id: ModelID = intro) -> JSONValue? {
        JSONValue.parse(
            #"{"type":"set_cue_property","id":"\#(id)","property":{"type":"\#(type)","value":\#(value)}}"#)
    }

    private func edit(_ field: InspectorField, _ text: String) {
        if field == .notes {
            vc.notesView.string = text
        } else {
            vc.field(field)?.stringValue = text
        }
        vc.commit(field)
    }

    func testEmptyAndMultipleStates() {
        vc.setSelection([])
        XCTAssertFalse(vc.emptyLabel.isHidden)
        XCTAssertEqual(vc.emptyLabel.stringValue, loc("inspector.empty"))
        XCTAssertTrue(vc.tabView.isHidden)
        vc.setSelection([Self.intro, Self.fade])
        XCTAssertEqual(vc.emptyLabel.stringValue, loc("inspector.multiple"))
        XCTAssertTrue(vc.tabView.isHidden)
        vc.setSelection(["00000000-0000-0000-0000-0000000000ff"])  // no such cue
        XCTAssertFalse(vc.emptyLabel.isHidden)
        vc.setSelection([Self.intro])
        XCTAssertTrue(vc.emptyLabel.isHidden)
        XCTAssertFalse(vc.tabView.isHidden)
    }

    func testShowsTheSelectedCue() {
        vc.setSelection([Self.intro])
        XCTAssertEqual(vc.numberField.stringValue, "1")
        XCTAssertEqual(vc.nameField.stringValue, "Intro")
        XCTAssertEqual(vc.preWaitField.stringValue, "00:00.000")
        XCTAssertEqual(vc.continuePopup.indexOfSelectedItem, 0)
        XCTAssertEqual(vc.colourPopup.indexOfSelectedItem, 0)
        XCTAssertEqual(vc.armedCheck.state, .on)
        XCTAssertFalse(vc.targetField.isEnabled, "an audio cue has no cue target")
        vc.setSelection([Self.fade])
        XCTAssertTrue(vc.targetField.isEnabled)
        XCTAssertEqual(vc.targetField.stringValue, "1", "the target is shown by its number")
        vc.setSelection([CueListHarness.rain])
        XCTAssertEqual(vc.colourPopup.indexOfSelectedItem, 4)
        XCTAssertEqual(vc.flaggedCheck.state, .on)
        XCTAssertEqual(vc.continuePopup.indexOfSelectedItem, 2)
        XCTAssertFalse(vc.postWaitField.isEnabled, "auto-follow ignores the post-wait")
    }

    func testEachFieldSendsItsCommand() async {
        vc.setSelection([Self.intro])
        edit(.name, "Storm")
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("name", #""Storm""#))
        edit(.notes, "line 1\nline 2")
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands.last, property("notes", #""line 1\nline 2""#))
        edit(.preWait, "90")
        await assertEventually { self.commands.count == 3 }
        XCTAssertEqual(commands.last, property("pre_wait", "90"))
        await assertEventually("the field shows what was stored") { self.vc.preWaitField.stringValue == "01:30.000" }
        edit(.postWait, "0:02.5")
        await assertEventually { self.commands.count == 4 }
        XCTAssertEqual(commands.last, property("post_wait", "2.5"))
        edit(.number, " 12 ")
        await assertEventually { self.commands.count == 5 }
        XCTAssertEqual(
            commands.last,
            JSONValue.parse(
                #"{"type":"set_cue_numbers","numbers":[{"id":"\#(Self.intro)","number":"12"}]}"#))
        edit(.number, "")
        await assertEventually { self.commands.count == 6 }
        XCTAssertEqual(
            commands.last,
            JSONValue.parse(
                #"{"type":"set_cue_numbers","numbers":[{"id":"\#(Self.intro)","number":null}]}"#))
    }

    func testPopupsAndChecksSendTheirCommands() async {
        vc.setSelection([Self.intro])
        vc.colourPopup.selectItem(at: 3)
        _ = vc.colourPopup.target?.perform(vc.colourPopup.action, with: vc.colourPopup)
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("color", #""green""#))
        vc.continuePopup.selectItem(at: 1)
        _ = vc.continuePopup.target?.perform(vc.continuePopup.action, with: vc.continuePopup)
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands.last, property("continue_mode", #""auto_continue""#))
        vc.flaggedCheck.state = .on
        _ = vc.flaggedCheck.target?.perform(vc.flaggedCheck.action, with: vc.flaggedCheck)
        await assertEventually { self.commands.count == 3 }
        XCTAssertEqual(commands.last, property("flagged", "true"))
        vc.armedCheck.state = .off
        _ = vc.armedCheck.target?.perform(vc.armedCheck.action, with: vc.armedCheck)
        await assertEventually { self.commands.count == 4 }
        XCTAssertEqual(commands.last, property("armed", "false"))
    }

    func testTargetByNumber() async {
        vc.setSelection([Self.fade])
        edit(.target, "2.1")
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("target", #""\#(Self.door)""#, id: Self.fade))
        edit(.target, "")
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands.last, property("target", "null", id: Self.fade))
    }

    func testInvalidInputRevertsWithoutSending() async {
        vc.setSelection([Self.fade])
        for text in ["abc", "-1", "25:00:00", "1:75", ""] {
            edit(.preWait, text)
            XCTAssertEqual(vc.preWaitField.stringValue, "00:00.000", text)
            XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.time"), text)
        }
        edit(.target, "99")
        XCTAssertEqual(vc.targetField.stringValue, "1")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.unknownTarget"))
        edit(.target, "")  // the fade has a target; clearing is valid, so this one sends
        await assertEventually { self.commands.count == 1 }
        vc.setSelection([Self.fade])
        edit(.number, "1")  // Intro's number
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.duplicateNumber"))
        XCTAssertEqual(vc.numberField.stringValue, "")
        vc.setSelection([Self.intro])
        edit(.target, "2.1")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.notApplicable"))
        await settle()
        XCTAssertEqual(commands.count, 1, "only the valid clear was sent")
    }

    func testUnchangedTextSendsNothing() async {
        vc.setSelection([Self.intro])
        edit(.name, "Intro")
        edit(.preWait, "0")
        edit(.number, "1")
        await settle()
        XCTAssertTrue(commands.isEmpty)
    }

    func testRefusedCommandRevertsAndWarns() async {
        rig.engine.refuseCommands = true
        vc.setSelection([Self.intro])
        edit(.name, "Storm")
        XCTAssertEqual(vc.nameField.stringValue, "Storm")
        await assertEventually { self.vc.nameField.stringValue == "Intro" }
        XCTAssertTrue(vc.messageLabel.stringValue.contains("refused"))
        edit(.preWait, "5")
        await assertEventually { self.vc.preWaitField.stringValue == "00:00.000" }
    }

    func testExternalChangeUpdatesFieldsButNotTheOneBeingEdited() async throws {
        vc.setSelection([Self.intro])
        XCTAssertTrue(window.makeFirstResponder(vc.nameField))
        vc.nameField.currentEditor()?.string = "half typed"
        typed(.name, "half typed")
        XCTAssertTrue(vc.isEditing(.name))
        var cue = try XCTUnwrap(rig.store.model?.cue(Self.intro)).raw
        cue.set("name", .string("Renamed elsewhere"))
        cue.set("notes", .string("from another client"))
        cue.set("pre_wait", .double(2))
        let upsert =
            #"{"type":"cue","list":"\#(CueListHarness.list)","parent":null,"index":0,"cue":\#(try cue.text())}"#
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#
                )), .applied)
        XCTAssertEqual(vc.nameField.currentEditor()?.string, "half typed")
        XCTAssertEqual(vc.notesView.string, "from another client")
        XCTAssertEqual(vc.preWaitField.stringValue, "00:02.000")
        window.makeFirstResponder(nil)  // focus loss commits the typed name
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("name", #""half typed""#))
    }

    func testEscCancelsTheEdit() async {
        vc.setSelection([Self.intro])
        XCTAssertTrue(window.makeFirstResponder(vc.nameField))
        vc.nameField.currentEditor()?.string = "never sent"
        XCTAssertTrue(
            vc.control(vc.nameField, textView: NSTextView(), doCommandBy: #selector(NSResponder.cancelOperation(_:))))
        await settle()
        XCTAssertEqual(vc.nameField.stringValue, "Intro")
        XCTAssertTrue(commands.isEmpty)
        XCTAssertNil(vc.nameField.currentEditor(), "editing ended")
    }

    func testSelectionFollowsTheCueList() throws {
        let list = CueListViewController(store: rig.store)
        list.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        var seen: [[ModelID]] = []
        list.onSelectionChange = { seen.append($0) }
        list.select([Self.intro])
        XCTAssertTrue(seen.isEmpty, "a programmatic restore is not the user's choice")
        let row = list.outlineView.row(forItem: list.node(Self.fade))
        list.outlineView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        XCTAssertEqual(seen.last, [Self.fade])
    }

    private func typed(_ f: InspectorField, _ text: String) {
        if f == .notes {
            vc.notesView.string = text
            vc.textDidChange(Notification(name: NSText.didChangeNotification, object: vc.notesView))
        } else if let tf = vc.field(f) {
            tf.stringValue = text
            vc.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: tf))
        }
    }

    private func upsertName(_ name: String, id: ModelID = intro) throws {
        var cue = try XCTUnwrap(rig.store.model?.cue(id)).raw
        cue.set("name", .string(name))
        let upsert =
            #"{"type":"cue","list":"\#(CueListHarness.list)","parent":null,"index":0,"cue":\#(try cue.text())}"#
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#
                )), .applied)
    }

    func testUnchangedSelectionMidEditKeepsTextAndMessage() {
        vc.setSelection([Self.intro])
        typed(.name, "half")
        vc.messageLabel.stringValue = "keep me"
        vc.setSelection([Self.intro])
        XCTAssertEqual(vc.nameField.stringValue, "half")
        XCTAssertEqual(vc.messageLabel.stringValue, "keep me")
        XCTAssertTrue(vc.isEditing(.name))
    }

    func testFocusedButUntouchedFieldTakesExternalChange() async throws {
        vc.setSelection([Self.intro])
        XCTAssertTrue(window.makeFirstResponder(vc.nameField))
        typed(.name, "A")
        vc.commit(.name)  // Return: sent, field stays focused
        await assertEventually { self.commands.count == 1 }
        await assertEventually { self.rig.store.model?.cue(Self.intro)?.name == "A" }
        try upsertName("B")
        XCTAssertEqual(vc.nameField.stringValue, "B")
        window.makeFirstResponder(nil)  // clicking away
        await settle()
        XCTAssertEqual(commands.count, 1, "nothing is re-sent for a field the user did not touch")
    }

    func testCueDeletedMidEditDropsTheEdit() async throws {
        vc.setSelection([Self.intro])
        typed(.name, "doomed")
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(rig.store.revision + 1),"upserts":[],"deletes":[{"type":"cue","id":"\#(Self.intro)"}]}"#
                )), .applied)
        XCTAssertFalse(vc.emptyLabel.isHidden)
        vc.commit(.name)
        await settle()
        XCTAssertTrue(commands.isEmpty)
        let ok = await vc.commitPendingEdits()
        XCTAssertTrue(ok)
    }

    func testRefusalDoesNotWipeANewerEditOfTheSameField() async {
        rig.engine.holdResults = true
        rig.engine.refuseCommands = true
        vc.setSelection([Self.intro])
        edit(.name, "first")
        await assertEventually { self.commands.count == 1 }
        typed(.name, "second, still typing")
        rig.engine.releaseResults()
        await assertEventually { self.vc.messageLabel.stringValue.contains("refused") }
        XCTAssertEqual(vc.nameField.stringValue, "second, still typing", "a dirty field is not reverted")
    }

    func testStaleRefusalDoesNotRevertWhileANewerEditIsInFlight() async {
        rig.engine.holdResults = true
        rig.engine.refuseCommands = true
        vc.setSelection([Self.intro])
        edit(.name, "first")
        await assertEventually { self.commands.count == 1 }
        rig.engine.refuseCommands = false  // the newer edit is accepted
        edit(.name, "second")
        await assertEventually { self.commands.count == 2 }
        rig.engine.releaseResults()
        await assertEventually { self.vc.messageLabel.stringValue.contains("refused") }
        await settle()
        XCTAssertEqual(vc.nameField.stringValue, "second", "the newest edit decides what shows")
    }

    func testSecondEditBeforeTheFirstResultSendsBoth() async {
        rig.engine.holdResults = true
        vc.setSelection([Self.intro])
        edit(.name, "one")
        edit(.name, "two")
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands[0], property("name", #""one""#))
        XCTAssertEqual(commands[1], property("name", #""two""#))
        rig.engine.holdResults = false
        rig.engine.releaseResults()
        let ok = await vc.commitPendingEdits()
        XCTAssertTrue(ok)
    }

    func testPendingEditsAreCommittedAndAwaited() async {
        vc.setSelection([Self.intro])
        typed(.name, "Storm")
        typed(.notes, "n")
        let ok = await vc.commitPendingEdits()
        XCTAssertTrue(ok)
        XCTAssertEqual(commands.count, 2)
        XCTAssertTrue(vc.dirty.isEmpty)
    }

    func testRefusedPendingEditReportsFailure() async {
        rig.engine.refuseCommands = true
        vc.setSelection([Self.intro])
        typed(.name, "Storm")
        let ok = await vc.commitPendingEdits()
        XCTAssertFalse(ok)
    }

    func testDecimalCommaHasItsOwnMessage() {
        vc.setSelection([Self.intro])
        edit(.preWait, "1,5")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.decimalComma"))
        edit(.preWait, "1,x")
        XCTAssertEqual(vc.messageLabel.stringValue, loc("inspector.invalid.time"))
    }
}
