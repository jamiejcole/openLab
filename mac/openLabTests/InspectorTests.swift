// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
import XCTest

@testable import openLab

final class CueTimeTests: XCTestCase {
    func testFormat() {
        XCTAssertEqual(CueTime.format(0), "00:00.00")
        XCTAssertEqual(CueTime.format(60), "01:00.00")
        XCTAssertEqual(CueTime.format(1.2345), "00:01.235")
        XCTAssertEqual(CueTime.format(3599.9994), "59:59.999")
        XCTAssertEqual(CueTime.format(3600), "1:00:00.00")
        XCTAssertEqual(CueTime.format(86400), "24:00:00.00")
        XCTAssertEqual(CueTime.format(-1), "")
        XCTAssertEqual(CueTime.format(.nan), "")
        XCTAssertEqual(CueTime.format(.infinity), "")
    }

    func testParseAcceptedForms() {
        let cases: [(String, Double)] = [
            ("0", 0), ("5", 5), ("5.5", 5.5), ("90", 90), ("01:00.00", 60), ("1:30", 90), ("1:30.25", 90.25),
            ("00:00.001", 0.001), ("1:00:00", 3600), ("24:00:00.00", 86400), ("  2.5 ", 2.5), ("0.0004", 0),
        ]
        for (text, seconds) in cases { XCTAssertEqual(CueTime.parse(text), seconds, text) }
    }

    func testParseRejects() {
        for text in [
            "", " ", "abc", "-1", "+1", "1e3", "nan", "inf", "1.", ".5", "1..2", "1:60", "1:2:3:4", ":30", "1::30",
            "60:00:00", "1:60:00", "24:00:00.001", "99999999999999999999", "1,5", "٣",
        ] {
            XCTAssertNil(CueTime.parse(text), text)
        }
    }

    func testRoundTrip() {
        for s in [0, 0.001, 1, 59.999, 60, 61.5, 3599.999, 3600, 7322.25, 86400] as [Double] {
            XCTAssertEqual(CueTime.parse(CueTime.format(s)), s, "\(s)")
        }
    }
}

final class EditCommandTests: XCTestCase {
    private func shape(_ c: Command) throws -> JSONValue? { JSONValue.parse(try c.json.text()) }

    /// #255: the group mode on the wire is the engine's own fixture, every variant: the app's encoding
    /// equals it. (The app only encodes this command; it never decodes one.)
    func testGroupModeCommandsMatchTheEnginesFixtures() throws {
        let id: ModelID = "00000000-0000-0000-0000-00000000000c"
        let modes: [(String, String, Bool, Bool)] = [
            ("timeline", "timeline", false, false),
            ("start_first_and_enter", "start_first_and_enter", false, false),
            ("start_first_and_go_to_next", "start_first_and_go_to_next", false, false),
            ("playlist", "playlist", true, false),
            ("start_random", "start_random", false, false),
        ]
        for (file, type, loop, shuffle) in modes {
            let fixture = try Fixtures.json("client", "command_set_group_mode_\(file)")["command"]
            let command = Command.setProperty(id, .groupMode(type: type, loop: loop, shuffle: shuffle))
            XCTAssertEqual(try shape(command), fixture, file)
            let value = try XCTUnwrap(fixture?["property"]?["value"])
            XCTAssertEqual(value["type"], .string(type), "\(file): the fixture holds the variant its name says")
        }
        // A loop/shuffle flag only travels with a playlist.
        let random = try shape(.setProperty(id, .groupMode(type: "start_random", loop: true, shuffle: true)))
        XCTAssertEqual(random, try Fixtures.json("client", "command_set_group_mode_start_random")["command"])
    }

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
            (
                .setProperty(id, .groupMode(type: "playlist", loop: true, shuffle: false)),
                #"{"type":"group_mode","value":{"type":"playlist","loop":true,"shuffle":false}}"#
            ),
            (
                .setProperty(id, .groupMode(type: "start_random", loop: true, shuffle: true)),
                #"{"type":"group_mode","value":{"type":"start_random"}}"#
            ),
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

    /// The Wait/Fade duration and Load time properties encode as the engine's golden fixtures.
    func testDurationAndLoadTimeMatchTheFixtures() throws {
        let id: ModelID = "00000000-0000-0000-0000-00000000000c"
        let cases: [(String, CueProperty)] = [("duration", .duration(12.5)), ("load_to_time", .loadToTime(90.25))]
        for (name, property) in cases {
            let fixture = try Fixtures.json("client", "command_set_cue_\(name)")["command"]
            XCTAssertEqual(try shape(.setProperty(id, property)), fixture, name)
        }
    }
}

/// The inspector against the fake engine: the demo show (Intro #1, a fade cue targeting it, a
/// group of three audio cues).
@MainActor
final class InspectorTests: XCTestCase {
    static let intro = TestSnapshots.intro
    static let fade = TestSnapshots.fade
    static let door = TestSnapshots.door
    static let group = TestSnapshots.group
    static let list = TestSnapshots.mainList
    /// Over the engine's bound on a cue name (1024 bytes): the engine refuses it.
    static let tooLong = String(repeating: "x", count: 1100)

    /// The demo show on the real engine, the inspector's wire recorded (nil for the engine-free tests).
    private var rig: Rig!
    private var store: WorkspaceStore!

    /// What only reads the model or the controls needs no engine (and runs in engine-free runs).
    private static let engineFree = [
        "testEmptyAndMultipleStates", "testShowsTheSelectedCue", "testModeTabIsForGroupsOnly",
        "testSelectionFollowsTheCueList",
        "testUnchangedSelectionMidEditKeepsTextAndMessage", "testDecimalCommaHasItsOwnMessage",
    ]
    private var vc: InspectorViewController!
    private var window: NSWindow!

    override func setUp() async throws {
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
        window?.orderOut(nil)
        rig?.client.stop()
    }

    private var commands: [JSONValue] {
        rig.wire.commands.compactMap { JSONValue.parse((try? $0.json.text()) ?? "") }
    }

    private var intro: Cue? { store.model?.cue(Self.intro) }

    /// Another client of the same workspace (another window, a second Mac), for changes made elsewhere.
    private func elsewhere(_ command: Command) async throws {
        let other = Rig(engine: rig.testEngine, workspace: rig.doc)
        let connected = await other.connect()
        XCTAssertTrue(connected)
        guard case .success = await other.store.send(command: command) else { return XCTFail("\(command)") }
        other.client.stop()
    }

    private func setModeElsewhere(_ mode: JSONValue) async throws {
        let cmd = Command(
            .object([
                "type": .string("set_cue_property"), "id": .string(Self.group.raw),
                "property": .object(["type": .string("group_mode"), "value": mode]),
            ]))
        try await self.elsewhere(cmd)
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
        XCTAssertEqual(vc.preWaitField.stringValue, "00:00.00")
        XCTAssertEqual(vc.continuePopup.indexOfSelectedItem, 0)
        XCTAssertEqual(vc.colourPopup.indexOfSelectedItem, 0)
        XCTAssertEqual(vc.armedCheck.state, .on)
        XCTAssertFalse(vc.targetField.isEditable, "an audio cue's file is chosen, not typed")
        vc.setSelection([Self.fade])
        XCTAssertTrue(vc.targetField.isEnabled)
        XCTAssertEqual(vc.targetField.stringValue, "1", "the target is shown by its number")
        vc.setSelection([TestSnapshots.rain])
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
        await assertEventually("the field shows what was stored") { self.vc.preWaitField.stringValue == "01:30.00" }
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
        // The engine stored each one.
        await assertEventually { self.intro?.number == nil }
        XCTAssertEqual(intro?.name, "Storm")
        XCTAssertEqual(intro?.raw["notes"]?.stringValue, "line 1\nline 2")
        XCTAssertEqual(intro?.raw["pre_wait"]?.doubleValue, 90)
        XCTAssertEqual(intro?.raw["post_wait"]?.doubleValue, 2.5)
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
        await assertEventually("the engine stored them") { self.intro?.raw["armed"] == .bool(false) }
        XCTAssertEqual(intro?.raw["color"]?.stringValue, "green")
        XCTAssertEqual(intro?.raw["continue_mode"]?.stringValue, "auto_continue")
        XCTAssertEqual(intro?.raw["flagged"], .bool(true))
    }

    private func choose(_ popup: NSPopUpButton, _ i: Int) {
        popup.selectItem(at: i)
        _ = popup.target?.perform(popup.action, with: popup)
    }

    private func toggle(_ check: NSButton, _ on: Bool) {
        check.state = on ? .on : .off
        _ = check.target?.perform(check.action, with: check)
    }

    private func groupMode(_ json: String) -> JSONValue? {
        property("group_mode", json, id: Self.group)
    }

    func testModeTabIsForGroupsOnly() {
        let hasMode = { self.vc.tabView.tabViewItems.contains { $0.identifier as? String == "mode" } }
        vc.setSelection([Self.intro])
        XCTAssertFalse(hasMode())
        vc.setSelection([Self.group])
        XCTAssertTrue(hasMode())
        XCTAssertEqual(vc.groupModePopup.indexOfSelectedItem, 0, "the demo group is a timeline")
        vc.setSelection([Self.fade])
        XCTAssertFalse(hasMode())
    }

    func testEachGroupModeSendsItsCommand() async {
        vc.setSelection([Self.group])
        let expected = [
            (1, #"{"type":"playlist","loop":false,"shuffle":false}"#),
            (2, #"{"type":"start_first_and_enter"}"#),
            (3, #"{"type":"start_first_and_go_to_next"}"#),
            (4, #"{"type":"start_random"}"#),
            (0, #"{"type":"timeline"}"#),
        ]
        for (n, (index, mode)) in expected.enumerated() {
            choose(vc.groupModePopup, index)
            await assertEventually { self.commands.count == n + 1 }
            XCTAssertEqual(commands.last, groupMode(mode), mode)
            // The popup alone proves nothing: it shows the choice before the engine has answered. And
            // the next choice must meet the new mode: the old one chosen again changes nothing.
            let type = JSONValue.parse(mode)?["type"]
            await assertEventually("the model answers") {
                self.store.model?.cue(Self.group)?.raw["kind"]?["mode"]?["type"] == type
                    && self.vc.groupModePopup.indexOfSelectedItem == index
            }
        }
    }

    func testLoopAndShuffleOnlyForPlaylistAndSendWholeMode() async {
        vc.setSelection([Self.group])
        XCTAssertFalse(vc.groupLoopCheck.isEnabled)
        XCTAssertFalse(vc.groupShuffleCheck.isEnabled)
        choose(vc.groupModePopup, 1)
        await assertEventually { self.vc.groupLoopCheck.isEnabled && self.vc.groupShuffleCheck.isEnabled }
        toggle(vc.groupLoopCheck, true)
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands.last, groupMode(#"{"type":"playlist","loop":true,"shuffle":false}"#))
        toggle(vc.groupShuffleCheck, true)
        await assertEventually { self.commands.count == 3 }
        XCTAssertEqual(commands.last, groupMode(#"{"type":"playlist","loop":true,"shuffle":true}"#))
        choose(vc.groupModePopup, 0)
        await assertEventually { !self.vc.groupLoopCheck.isEnabled }
        XCTAssertEqual(vc.groupLoopCheck.state, .off)
    }

    func testGroupControlsFollowAModelChangeFromElsewhere() async throws {
        vc.setSelection([Self.group])
        try await setModeElsewhere(
            .object(["type": .string("playlist"), "loop": .bool(true), "shuffle": .bool(true)]))
        await assertEventually { self.vc.groupModePopup.indexOfSelectedItem == 1 }
        XCTAssertEqual(vc.groupLoopCheck.state, .on)
        XCTAssertEqual(vc.groupShuffleCheck.state, .on)
        XCTAssertTrue(vc.groupLoopCheck.isEnabled)
    }

    func testRechoosingPlaylistKeepsLoopAndShuffleAndSendsNothing() async throws {
        vc.setSelection([Self.group])
        try await setModeElsewhere(
            .object(["type": .string("playlist"), "loop": .bool(true), "shuffle": .bool(true)]))
        await assertEventually { self.vc.groupShuffleCheck.state == .on }
        let before = commands.count
        choose(vc.groupModePopup, 1)
        // A command sent after it must be the very next one: the re-choice sent none.
        toggle(vc.groupShuffleCheck, false)
        await assertEventually { self.commands.count == before + 1 }
        XCTAssertEqual(commands.last, groupMode(#"{"type":"playlist","loop":true,"shuffle":false}"#))
        await assertEventually("the engine kept the loop") {
            self.store.model?.cue(Self.group)?.raw["kind"]?["mode"]
                == .object(["type": .string("playlist"), "loop": .bool(true), "shuffle": .bool(false)])
        }
    }

    func testGroupControlsAreLockedInShowMode() async {
        vc.setSelection([Self.group])
        choose(vc.groupModePopup, 1)
        await assertEventually { self.vc.groupLoopCheck.isEnabled }
        vc.isLocked = true
        for c in [vc.groupModePopup as NSControl, vc.groupLoopCheck, vc.groupShuffleCheck] {
            XCTAssertFalse(c.isEnabled)
        }
        choose(vc.groupModePopup, 2)
        await settle()
        XCTAssertEqual(commands.count, 1, "nothing is sent in Show Mode")
        vc.isLocked = false
        XCTAssertTrue(vc.groupModePopup.isEnabled)
        XCTAssertTrue(vc.groupLoopCheck.isEnabled)
    }

    func testTargetByNumber() async {
        vc.setSelection([Self.fade])
        edit(.target, "2.1")
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("target", #""\#(Self.door)""#, id: Self.fade))
        edit(.target, "")
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands.last, property("target", "null", id: Self.fade))
        await assertEventually("the engine cleared it") {
            self.store.model?.cue(Self.fade)?.raw["kind"]?["target"] == .null
        }
    }

    func testInvalidInputRevertsWithoutSending() async {
        vc.setSelection([Self.fade])
        for text in ["abc", "-1", "25:00:00", "1:75", ""] {
            edit(.preWait, text)
            XCTAssertEqual(vc.preWaitField.stringValue, "00:00.00", text)
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
        vc.setSelection([Self.intro])
        edit(.name, Self.tooLong)  // over the engine's bound on a name: refused
        XCTAssertEqual(vc.nameField.stringValue, Self.tooLong)
        await assertEventually { self.vc.nameField.stringValue == "Intro" }
        XCTAssertTrue(vc.messageLabel.stringValue.contains("refused"))
        edit(.notes, String(repeating: "n", count: 70_000))  // over the bound on notes
        await assertEventually { self.vc.notesView.string == "" }
        XCTAssertEqual(intro?.name, "Intro", "the engine changed nothing")
    }

    func testExternalChangeUpdatesFieldsButNotTheOneBeingEdited() async throws {
        vc.setSelection([Self.intro])
        XCTAssertTrue(window.makeFirstResponder(vc.nameField))
        vc.nameField.currentEditor()?.string = "half typed"
        typed(.name, "half typed")
        XCTAssertTrue(vc.isEditing(.name))
        try await elsewhere(
            XCTUnwrap(
                Command.transaction([
                    .setProperty(Self.intro, .name("Renamed elsewhere")),
                    .setProperty(Self.intro, .notes("from another client")),
                    .setProperty(Self.intro, .preWait(2)),
                ])))
        await assertEventually { self.vc.notesView.string == "from another client" }
        XCTAssertEqual(vc.nameField.currentEditor()?.string, "half typed")
        XCTAssertEqual(vc.preWaitField.stringValue, "00:02.00")
        window.makeFirstResponder(nil)  // focus loss commits the typed name
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("name", #""half typed""#))
        await assertEventually("the typed name wins") { self.intro?.name == "half typed" }
    }

    /// A menu command can select another cue while a name is typed (a new cue, Select Cue by number;
    /// a playhead move cannot, `PlayheadFlowTests`): the name goes to the cue it was typed for, and
    /// the inspector shows the other cue's own name.
    func testASelectionTheAppChangesMidEditCommitsTheTypingToTheCueItWasTypedFor() async throws {
        vc.setSelection([Self.intro])
        XCTAssertTrue(window.makeFirstResponder(vc.nameField))
        vc.nameField.currentEditor()?.string = "half typed"
        typed(.name, "half typed")
        vc.setSelection([Self.fade])
        await assertEventually { self.commands.count == 1 }
        XCTAssertEqual(commands.last, property("name", #""half typed""#))
        XCTAssertNil(vc.nameField.currentEditor(), "editing ended")
        let fadeName = try XCTUnwrap(rig.store.model?.cue(Self.fade)?.raw["name"]?.stringValue)
        XCTAssertEqual(vc.nameField.stringValue, fadeName)
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

    /// The button sends its toggle to the window's transport and follows what the engine says.
    func testPreviewButtonFollowsTheRuntimeAndSendsToggles() async throws {
        var sent: [TransportAction] = []
        vc.onTransport = { action in
            sent.append(action)
            Task { _ = await self.store.transport(action) }
        }
        vc.setSelection([Self.intro])
        XCTAssertTrue(vc.previewButton.isEnabled)
        XCTAssertEqual(vc.previewButton.title, loc("inspector.preview"))
        vc.previewButton.performClick(nil)
        XCTAssertEqual(sent, [.preview(cue: Self.intro)])
        try await rig.advance(0.1)
        await assertEventually("the engine previews it") { self.vc.previewButton.title == loc("inspector.stopPreview") }
        vc.previewButton.performClick(nil)
        XCTAssertEqual(sent.last, .stopPreview(cue: Self.intro))
        try await rig.advance(0.1)
        await assertEventually("and stops") { self.vc.previewButton.title == loc("inspector.preview") }
    }

    func testPreviewButtonIsDisabledForOtherKindsAndShowModeKeepsItUsable() throws {
        vc.setSelection([Self.fade])
        XCTAssertFalse(vc.previewButton.isEnabled, "a fade cue cannot be previewed")
        vc.setSelection([])
        XCTAssertFalse(vc.previewButton.isEnabled)
        vc.setSelection([Self.intro])
        vc.isLocked = true
        XCTAssertTrue(vc.previewButton.isEnabled, "a preview edits nothing, so Show Mode leaves it")
    }

    func testSelectionFollowsTheCueList() throws {
        let list = CueListViewController(store: store)
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
        vc.commit(.name)  // a Save's commit: sent, field stays focused
        await assertEventually { self.commands.count == 1 }
        await assertEventually { self.store.model?.cue(Self.intro)?.name == "A" }
        try await elsewhere(.setProperty(Self.intro, .name("B")))
        await assertEventually { self.vc.nameField.stringValue == "B" }
        window.makeFirstResponder(nil)  // clicking away
        await settle()
        XCTAssertEqual(commands.count, 1, "nothing is re-sent for a field the user did not touch")
    }

    func testCueDeletedMidEditDropsTheEdit() async throws {
        vc.setSelection([Self.intro])
        typed(.name, "doomed")
        try await elsewhere(.removeCue(Self.intro))
        await assertEventually { !self.vc.emptyLabel.isHidden }
        vc.commit(.name)
        await settle()
        XCTAssertTrue(commands.isEmpty)
        let ok = await vc.commitPendingEdits()
        XCTAssertTrue(ok)
    }

    /// The refusal of an edit arrives while a newer one is being typed in the same field.
    func testRefusalDoesNotWipeANewerEditOfTheSameField() async {
        vc.setSelection([Self.intro])
        edit(.name, Self.tooLong)
        typed(.name, "second, still typing")  // before the engine's answer (it needs this turn to end)
        await assertEventually { self.vc.messageLabel.stringValue.contains("refused") }
        XCTAssertEqual(vc.nameField.stringValue, "second, still typing", "a dirty field is not reverted")
    }

    func testStaleRefusalDoesNotRevertWhileANewerEditIsInFlight() async {
        vc.setSelection([Self.intro])
        edit(.name, Self.tooLong)  // refused
        edit(.name, "second")  // sent before the first is answered, and accepted
        await assertEventually { self.commands.count == 2 }
        await assertEventually { self.vc.messageLabel.stringValue.contains("refused") }
        await assertEventually { self.intro?.name == "second" }
        await settle()
        XCTAssertEqual(vc.nameField.stringValue, "second", "the newest edit decides what shows")
    }

    func testSecondEditBeforeTheFirstResultSendsBoth() async {
        vc.setSelection([Self.intro])
        edit(.name, "one")
        edit(.name, "two")
        await assertEventually { self.commands.count == 2 }
        XCTAssertEqual(commands[0], property("name", #""one""#))
        XCTAssertEqual(commands[1], property("name", #""two""#))
        let ok = await vc.commitPendingEdits()
        XCTAssertTrue(ok)
        await assertEventually { self.intro?.name == "two" }
    }

    func testPendingEditsAreCommittedAndAwaited() async {
        vc.setSelection([Self.intro])
        typed(.name, "Storm")
        typed(.notes, "n")
        let ok = await vc.commitPendingEdits()
        XCTAssertTrue(ok)
        XCTAssertEqual(commands.count, 2)
        XCTAssertTrue(vc.dirty.isEmpty)
        XCTAssertEqual(intro?.name, "Storm", "committed and answered")
        XCTAssertEqual(intro?.raw["notes"]?.stringValue, "n")
    }

    func testRefusedPendingEditReportsFailure() async {
        vc.setSelection([Self.intro])
        typed(.name, Self.tooLong)
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

/// The Network message line: address, then space-separated typed arguments (CT-30).
final class NetworkMessageTests: XCTestCase {
    private func arg(_ type: String, _ value: JSONValue) -> JSONValue {
        .object(["type": .string(type), "value": value])
    }

    func testParsesEachArgumentType() throws {
        let parsed = try NetworkMessage.parse(#"/x 2 10.5 "two words" \T \F word -3 .5 7."#).get()
        XCTAssertEqual(parsed.address, "/x")
        XCTAssertEqual(
            parsed.args,
            [
                arg("int", .int(2)), arg("float", .double(10.5)), arg("string", .string("two words")),
                arg("bool", .bool(true)), arg("bool", .bool(false)), arg("string", .string("word")),
                arg("int", .int(-3)), arg("float", .double(0.5)), arg("float", .double(7)),
            ])
    }

    func testAddressIsKeptAsTypedEvenWhenBad() throws {
        let parsed = try NetworkMessage.parse("no-slash 1").get()
        XCTAssertEqual(parsed.address, "no-slash")
        XCTAssertEqual(parsed.args, [arg("int", .int(1))])
        XCTAssertEqual(try NetworkMessage.parse("   ").get().address, "")
    }

    func testUnreadableArgumentsAreRejected() {
        XCTAssertEqual(NetworkMessage.parse(#"/x "open"#), .failure(.unterminatedQuote))
        XCTAssertEqual(NetworkMessage.parse(#"/x \I"#), .failure(.unsupportedArgument))
        XCTAssertEqual(NetworkMessage.parse(#"/x 1 \N"#), .failure(.unsupportedArgument))
    }

    func testAnIntegerTooBigForThirtyTwoBitsStaysAString() throws {
        let args = try NetworkMessage.parse("/x 99999999999").get().args
        XCTAssertEqual(args, [arg("string", .string("99999999999"))])
        XCTAssertEqual(NetworkMessage.format(address: "/x", args: args), #"/x "99999999999""#)
    }

    func testSmallAndLargeFloatsReadBackAsTheSameNumber() throws {
        for value in [1e-5, 1.5e-12, 1e-30, 3.0e20, 1.0e30] {
            let sent = Double(Float(value))
            let text = NetworkMessage.format(address: "/x", args: [arg("float", .double(sent))])
            let back = try NetworkMessage.parse(text).get().args
            XCTAssertEqual(back.first?["type"]?.stringValue, "float", text)
            XCTAssertEqual(back.first?["value"]?.doubleValue, sent, text)
        }
    }

    func testAnEmptyAddressWithArgumentsReadsBackAsEmpty() throws {
        let args = [arg("int", .int(5)), arg("string", .string("a"))]
        let text = NetworkMessage.format(address: "", args: args)
        let back = try NetworkMessage.parse(text).get()
        XCTAssertEqual(back.address, "")
        XCTAssertEqual(back.args, args)
    }

    func testFormatReadsBackAsTheSameMessage() throws {
        let args = [
            arg("int", .int(5)), arg("float", .double(5)), arg("float", .double(0.1)),
            arg("string", .string("5")), arg("string", .string("")), arg("string", .string(#"say "hi" \ now"#)),
            arg("string", .string(#"\T"#)), arg("string", .string("plain")), arg("bool", .bool(false)),
        ]
        let text = NetworkMessage.format(address: "/a", args: args)
        let back = try NetworkMessage.parse(text).get()
        XCTAssertEqual(back.address, "/a")
        XCTAssertEqual(NetworkMessage.format(address: back.address, args: back.args), text)
        XCTAssertEqual(back.args.map { $0["type"]?.stringValue }, args.map { $0["type"]?.stringValue })
        XCTAssertEqual(back.args[5]["value"]?.stringValue, #"say "hi" \ now"#)
        XCTAssertEqual(NetworkMessage.format(address: "", args: []), "")
    }
}

/// The Network cue's Settings tab (CT-30; QLab 5, manual pp. 366-367, 371-372) on the real engine: the
/// operator picks a patch, types the message with real key events, clicks Send, and a socket the test
/// owns reads what really goes out. Each test is one flow of the issue's checklist.
@MainActor
final class NetworkInspectorTests: XCTestCase {
    private var app: AppController!
    private var engine: TestEngine!
    private let wire = WireTap()
    private let sessionScheduler = ManualScheduler()
    private var doc: ModelID = "none"
    private var wc: WorkspaceWindowController { app.windows[doc]! }
    private var store: WorkspaceStore { wc.store }
    private var inspector: InspectorViewController { wc.inspector }
    private var list: ModelID { wc.cueList.listID! }
    private var one: UDPListener!
    private var two: UDPListener!
    private var net: ModelID = "none"
    private var memo: ModelID = "none"
    private let patch1: ModelID = "00000000-0000-0000-0000-0000000000b1"
    private let patch2: ModelID = "00000000-0000-0000-0000-0000000000b2"

    override func setUp() async throws {
        engine = try await TestEngine.start()
        one = try UDPListener()
        two = try UDPListener()
        let url = engine.infoURL
        let session = EngineSession(
            transport: wire, infoProvider: { EngineConnectionInfo.load(from: url) }, scheduler: sessionScheduler)
        app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { [unowned self] in app.windows[doc]?.window }
        session.start()
        await assertEventually { session.appClient.state == .connected }
        guard case .success(let new) = await session.newWorkspace() else { return XCTFail("new") }
        doc = new
        await assertEventually {
            self.app.windows[new]?.store.model != nil && self.app.windows[new]?.cueList.listID != nil
        }
        app.isTextEditing = { [unowned self] in (wc.window?.firstResponder as? NSText)?.isEditable == true }
        wc.window?.setFrame(NSRect(x: 0, y: 0, width: 1400, height: 900), display: false)
        wc.window?.makeKeyAndOrderFront(nil)
        // The workspace has the patches "Network 1" and "Network 2", each to one of the test's sockets.
        func patch(_ id: ModelID, _ name: String, _ port: UInt16) -> JSONValue {
            .object([
                "id": .string(id.raw), "name": .string(name), "host": .string("127.0.0.1"),
                "port": .int(Int64(port)), "protocol": .string("udp"),
            ])
        }
        var setup: [Command] = (store.model?.networkPatches ?? []).map {
            Command(.object(["type": .string("remove_network_patch"), "id": .string($0.id.raw)]))
        }
        setup += [
            .addNetworkPatch(patch(patch1, "Network 1", one.port), index: 0),
            .addNetworkPatch(patch(patch2, "Network 2", two.port), index: 1),
        ]
        guard let all = Command.transaction(setup), case .success = await store.send(command: all) else {
            return XCTFail("patches")
        }
        await assertEventually { self.store.model?.networkPatches.count == 2 }
        // A Network cue (what Cmd-8 adds) with a memo after it, the cue selected, the Settings tab shown.
        let addedNet = await wc.editor.add(.network)
        net = try XCTUnwrap(addedNet)
        let addedMemo = await wc.editor.add(.memo)
        memo = try XCTUnwrap(addedMemo)
        await assertEventually { self.store.model?.cue(self.memo) != nil && self.store.model?.cue(self.net) != nil }
        click(net)
        await assertEventually { self.inspector.cue?.id == self.net && self.store.playheads[self.list] == self.net }
        inspector.tabView.selectTabViewItem(withIdentifier: "network")
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        engine?.stop()
    }

    // MARK: Doing what the operator does

    private func key(_ chars: String, code: UInt16 = 0) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0,
                windowNumber: wc.window?.windowNumber ?? 0, context: nil, characters: chars,
                charactersIgnoringModifiers: chars, isARepeat: false, keyCode: chars == " " ? 49 : code))
    }

    /// Types `text` one key at a time through the app's key path and the window to its first responder.
    private func type(_ text: String) throws {
        let window = try XCTUnwrap(wc.window)
        for ch in text {
            if let passed = app.handleKeyDown(try key(String(ch))) { window.sendEvent(passed) }
        }
    }

    private func press(_ chars: String, code: UInt16) throws {
        let window = try XCTUnwrap(wc.window)
        if let passed = app.handleKeyDown(try key(chars, code: code)) { window.sendEvent(passed) }
    }

    private func tab() throws { try press("\t", code: 48) }
    private func returnKey() throws { try press("\r", code: 36) }

    /// A click in the message field, then `text` replacing what is there.
    private func typeMessage(_ text: String) throws {
        let field = inspector.networkMessageField
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(field))
        field.currentEditor()?.selectAll(nil)
        if text.isEmpty { try press("\u{7f}", code: 51) } else { try type(text) }
    }

    private func click(_ id: ModelID) { wc.cueList.selectAsUser([id]) }

    /// The Space key as the key monitor gets it, with the focus on the cue list.
    private func go() throws {
        wc.window?.makeFirstResponder(nil)
        XCTAssertNil(app.handleKeyDown(try key(" ")), "Space is GO")
    }

    /// The next message `listener` got, running the engine's clock on until it comes.
    private func received(_ listener: UDPListener) async -> String? {
        var got: String?
        _ = await eventually {
            try? await self.engine.advance(seconds: 0.1)
            got = got ?? listener.receiveMessage()
            return got != nil
        }
        return got
    }

    /// Runs the clock on for a while and says whether either socket heard anything.
    private func quiet() async throws -> Bool {
        for _ in 0..<5 { try await engine.advance(seconds: 0.1) }
        await settle()
        return one.receiveMessage() == nil && two.receiveMessage() == nil
    }

    // MARK: What the window shows

    private func status(_ id: ModelID, _ index: Int) -> NSImageView? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let row = ov.row(forItem: node)
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier("status"))
        guard row >= 0, col >= 0, let cell = ov.view(atColumn: col, row: row, makeIfNecessary: true) as? CueCell,
            !cell.images[index].isHidden
        else { return nil }
        return cell.images[index]
    }

    private func isBroken(_ id: ModelID) -> Bool { status(id, 1)?.accessibilityLabel() == loc("cue.state.broken") }

    private func targetCell(_ id: ModelID) -> String? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier("target"))
        return (ov.view(atColumn: col, row: ov.row(forItem: node), makeIfNecessary: true) as? CueCell)?.label
            .stringValue
    }

    private var kind: JSONValue? { store.model?.cue(net)?.raw["kind"] }

    private func choosePatch(at index: Int) {
        let popup = inspector.networkPatchPopup
        popup.selectItem(at: index)
        XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
    }

    /// Types `text` in the message field, ends the typing with Tab and waits for the cue to hold it.
    private func setMessageAndWait(_ text: String) async throws {
        try typeMessage(text)
        try tab()
        let parsed = try NetworkMessage.parse(text).get()
        await assertEventually("the cue holds the message") {
            self.kind?["address"]?.stringValue == parsed.address
                && NetworkMessage.format(address: "/", args: self.kind?["args"]?.arrayValue ?? [])
                    == NetworkMessage.format(address: "/", args: parsed.args)
        }
    }

    // MARK: Flows

    func testSettingsTabExists() {
        XCTAssertEqual(
            inspector.tabView.tabViewItems.map(\.label),
            [loc("inspector.tab.basics"), loc("inspector.tab.triggers"), loc("inspector.tab.network")])
        XCTAssertEqual(inspector.tabView.selectedTabViewItem?.identifier as? String, "network")
        XCTAssertEqual(inspector.networkPatchPopup.titleOfSelectedItem, "Network 1")
        XCTAssertEqual(inspector.networkMessageField.stringValue, "")
        XCTAssertEqual(inspector.networkSendButton.title, loc("inspector.network.send"))
        XCTAssertTrue(inspector.networkSendButton.isEnabled)
        XCTAssertTrue(isBroken(net), "a new cue has no message yet")
    }

    func testChooseADestination() async throws {
        let popup = inspector.networkPatchPopup
        XCTAssertEqual(
            popup.itemArray.filter { !$0.isSeparatorItem }.map(\.title),
            ["Network 1", "Network 2", loc("inspector.network.unpatched")])
        XCTAssertEqual(targetCell(net), "", "a Network cue takes no target: its cell is empty")
        try await setMessageAndWait("/lights/go 5")
        choosePatch(at: 1)
        await assertEventually { self.kind?["destination"]?.stringValue == self.patch2.raw }
        XCTAssertEqual(popup.titleOfSelectedItem, "Network 2")
        // GO sends to Network 2's host and port.
        try go()
        let got = await received(two)
        XCTAssertEqual(got, "/lights/go i:5")
        XCTAssertNil(one.receiveMessage(), "nothing went to Network 1")
        // Undo puts Network 1 back.
        await wc.editor.undo()
        await assertEventually { self.kind?["destination"]?.stringValue == self.patch1.raw }
        // GO moved the playhead and the selection with it: select the cue again.
        click(net)
        await assertEventually { self.inspector.cue?.id == self.net }
        await assertEventually { popup.titleOfSelectedItem == "Network 1" }
    }

    func testUnpatchedCueIsBroken() async throws {
        try await setMessageAndWait("/lights/go 5")
        await assertEventually { !self.isBroken(self.net) }
        let popup = inspector.networkPatchPopup
        choosePatch(at: popup.numberOfItems - 1)
        await assertEventually { self.isBroken(self.net) }
        XCTAssertEqual(popup.titleOfSelectedItem, loc("inspector.network.unpatched"))
        XCTAssertEqual(targetCell(net), "")
        let tip = try XCTUnwrap(status(net, 1)?.toolTip)
        XCTAssertTrue(tip.contains(loc("problem.network_patch_missing.none")), tip)
        // GO sends nothing and the playhead moves on.
        try go()
        await assertEventually { self.store.playheads[self.list] == self.memo }
        let silent = try await quiet()
        XCTAssertTrue(silent, "the broken cue sent nothing")
    }

    func testTypeAMessageAndTestSendIt() async throws {
        try typeMessage("/lights/go 5")
        XCTAssertTrue(inspector.dirty.contains(.message))
        try tab()
        await assertEventually("the message is stored") { self.kind?["address"]?.stringValue == "/lights/go" }
        await assertEventually { !self.isBroken(self.net) }
        inspector.networkSendButton.performClick(nil)
        let got = await received(one)
        XCTAssertEqual(got, "/lights/go i:5")
        // Reselecting another cue and back shows the same text.
        click(memo)
        await assertEventually { self.inspector.cue?.id == self.memo }
        click(net)
        await assertEventually { self.inspector.cue?.id == self.net }
        XCTAssertEqual(inspector.networkMessageField.stringValue, "/lights/go 5")
    }

    func testSendCommitsTextStillBeingTyped() async throws {
        try typeMessage("/typed 1")
        XCTAssertTrue(inspector.dirty.contains(.message), "still typing: no Tab, no Return")
        inspector.networkSendButton.performClick(nil)
        let got = await received(one)
        XCTAssertEqual(got, "/typed i:1", "what the field shows is what goes out")
    }

    func testSendEndsTheTypingSoSpaceFiresGo() async throws {
        try typeMessage("/typed 1")
        inspector.networkSendButton.performClick(nil)
        let first = await received(one)
        XCTAssertEqual(first, "/typed i:1")
        XCTAssertFalse(wc.window?.firstResponder is NSText, "the field no longer has the focus")
        XCTAssertNil(app.handleKeyDown(try key(" ")), "Space is GO, not a space in the field")
        let second = await received(one)
        XCTAssertEqual(second, "/typed i:1", "GO sent the cue")
        XCTAssertEqual(kind?["address"]?.stringValue, "/typed", "no space went into the message")
    }

    /// V on a selected Network cue sends its message once and changes no state.
    func testPreviewSelectedKeySendsTheNetworkCueMessageOnce() async throws {
        try await setMessageAndWait("/lights/go 5")
        await assertEventually("the cue is no longer broken") { !self.isBroken(self.net) }
        wc.window?.makeFirstResponder(nil)
        let before = store.cueStates[net]?.state
        XCTAssertEqual(before, .idle)
        XCTAssertNil(app.handleKeyDown(try key("v", code: 9)), "V is Preview Selected")
        let got = await received(one)
        XCTAssertEqual(got, "/lights/go i:5")
        let again = try await quiet()
        XCTAssertTrue(again, "sent once")
        XCTAssertEqual(store.cueStates[net]?.state, before, "no state change")
        XCTAssertTrue(store.previewing.isEmpty)
        XCTAssertEqual(store.playheads[list], net, "the playhead stays")
    }

    func testPreviewSelectedKeyOnADisarmedNetworkCueSendsNothing() async throws {
        try await setMessageAndWait("/lights/go 5")
        inspector.armedCheck.performClick(nil)
        await assertEventually("disarmed") { self.store.model?.cue(self.net)?.raw["armed"] == .bool(false) }
        wc.window?.makeFirstResponder(nil)
        XCTAssertNil(app.handleKeyDown(try key("v", code: 9)))
        let silent = try await quiet()
        XCTAssertTrue(silent, "a disarmed cue sends nothing")
    }

    func testSendFromADisarmedCueSendsNothing() async throws {
        try await setMessageAndWait("/lights/go 5")
        inspector.armedCheck.performClick(nil)
        await assertEventually("disarmed") { self.store.model?.cue(self.net)?.raw["armed"] == .bool(false) }
        inspector.networkSendButton.performClick(nil)
        let silent = try await quiet()
        XCTAssertTrue(silent, "a disarmed cue sends nothing, by hand or by GO")
    }

    func testSendWithNetworkOutputDisabledSendsNothingThenWorksAgain() async throws {
        try await setMessageAndWait("/lights/go 5")
        await assertEventually("a status came") {
            try? await self.engine.advance(seconds: 0.3)
            return self.store.status != nil
        }
        app.showOverrideControls()
        app.overrides.networkSwitch.performClick(nil)
        await assertEventually("network output is off") {
            try? await self.engine.advance(seconds: 0.3)
            return !self.wc.footer.overrideBadge.isHidden
        }
        inspector.networkSendButton.performClick(nil)
        let silent = try await quiet()
        XCTAssertTrue(silent, "Send respects the output override")
        app.overrides.networkSwitch.performClick(nil)
        await assertEventually("network output is back on") {
            try? await self.engine.advance(seconds: 0.3)
            return self.wc.footer.overrideBadge.isHidden
        }
        inspector.networkSendButton.performClick(nil)
        let got = await received(one)
        XCTAssertEqual(got, "/lights/go i:5")
    }

    func testTypedArguments() async throws {
        let text = #"/x 2 10.5 "two words" \T"#
        try await setMessageAndWait(text)
        click(memo)
        await assertEventually { self.inspector.cue?.id == self.memo }
        click(net)
        await assertEventually { self.inspector.cue?.id == self.net }
        XCTAssertEqual(inspector.networkMessageField.stringValue, text)
        inspector.networkSendButton.performClick(nil)
        let got = await received(one)
        XCTAssertEqual(got, "/x i:2 f:10.5 s:two words T")
    }

    func testUnreadableArgumentsAreAVisibleWarningNotACrash() async throws {
        try await setMessageAndWait("/ok 1")
        try typeMessage(#"/ok "unclosed"#)
        try tab()
        XCTAssertEqual(inspector.messageLabel.stringValue, loc("inspector.invalid.networkMessage"))
        XCTAssertEqual(inspector.networkMessageField.stringValue, "/ok 1", "the cue's own text is back")
        XCTAssertEqual(kind?["address"]?.stringValue, "/ok")
    }

    func testInvalidMessageIsAcceptedButBroken() async throws {
        try await setMessageAndWait("/fine 1")
        await assertEventually { !self.isBroken(self.net) }
        try typeMessage("no-slash 1")
        try tab()
        await assertEventually("broken") { self.isBroken(self.net) }
        XCTAssertEqual(inspector.networkMessageField.stringValue, "no-slash 1", "the text stays")
        let tip = try XCTUnwrap(status(net, 1)?.toolTip)
        XCTAssertTrue(tip.contains("The OSC message cannot be sent"), tip)
        inspector.networkSendButton.performClick(nil)
        let silent = try await quiet()
        XCTAssertTrue(silent, "Send does nothing")
        // Fixed: the broken marker clears.
        try typeMessage("/ok")
        try tab()
        await assertEventually("fixed") { !self.isBroken(self.net) && self.kind?["address"]?.stringValue == "/ok" }
        // Over the size bounds: refused, the field goes back, a notice says why.
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(inspector.networkMessageField))
        let huge = "/ok \"" + String(repeating: "a", count: 64 * 1024 + 1) + "\""
        let editor = try XCTUnwrap(inspector.networkMessageField.currentEditor() as? NSTextView)
        editor.selectAll(nil)
        editor.insertText(huge, replacementRange: editor.selectedRange())
        try tab()
        await assertEventually("refused") {
            self.inspector.messageLabel.stringValue.hasPrefix(String(format: loc("inspector.refused"), ""))
                && self.inspector.networkMessageField.stringValue == "/ok"
        }
        XCTAssertEqual(kind?["args"], .array([]))
    }

    func testShowModeLocksTheSettings() async throws {
        let controls: [NSControl] = [
            inspector.networkPatchPopup, inspector.networkMessageField, inspector.networkSendButton,
        ]
        XCTAssertTrue(controls.allSatisfy(\.isEnabled))
        await wc.requestShowMode(true)
        XCTAssertTrue(wc.isShowMode)
        XCTAssertTrue(controls.allSatisfy { !$0.isEnabled })
        await wc.requestShowMode(false)
        XCTAssertTrue(controls.allSatisfy(\.isEnabled), "Edit Mode re-enables them")
    }

    /// The playhead moves while the message is typed: the selection stays put, Space types a space,
    /// and the follow happens when the typing ends.
    func testAPlayheadMoveWhileTypingTheMessageIsHeldUntilTheTypingEnds() async throws {
        try typeMessage("/held")
        let moved = await store.transport(.setPlayhead(list: list, cue: memo))
        guard case .success = moved else { return XCTFail("\(moved)") }
        await assertEventually { self.store.playheads[self.list] == self.memo }
        XCTAssertEqual(inspector.cue?.id, net, "the selection stays while typing")
        XCTAssertEqual(inspector.networkMessageField.stringValue, "/held")
        try type(" 7")  // Space types a space; it does not fire GO
        XCTAssertEqual(inspector.networkMessageField.stringValue, "/held 7")
        try returnKey()
        await assertEventually("the held follow ran") { self.wc.cueList.selectedIDs() == [self.memo] }
        await assertEventually("the text went to the cue it was typed for") {
            self.store.model?.cue(self.net)?.raw["kind"]?["address"]?.stringValue == "/held"
        }
        let silent = try await quiet()
        XCTAssertTrue(silent, "no GO was fired by the typed Space")
    }
}

/// The Target row and drag-to-target (#494, QLab 5 manual pp. 94, 120, 126-127) on the real engine: the
/// "Targets" test show is 1 intro, pad (no number), 3 an unnamed Fade with no target and 4 note (a Memo).
/// A real drag is in `CueTargetUITests`; here a drop is what the table hands over, so every branch runs.
@MainActor
final class CueTargetFlowTests: XCTestCase {
    private static let intro: ModelID = "00000000-0000-0000-0000-000000000101"
    private static let pad: ModelID = "00000000-0000-0000-0000-000000000102"
    private static let fade: ModelID = "00000000-0000-0000-0000-000000000103"
    private static let memo: ModelID = "00000000-0000-0000-0000-000000000104"
    private static let on = NSOutlineViewDropOnItemIndex

    private var rig: Rig!
    private var wc: WorkspaceWindowController!
    private var inspector: InspectorViewController { wc.inspector }
    private var list: ModelID { wc.cueList.listID ?? "" }

    override func setUp() async throws {
        try await open("Targets")
    }

    override func tearDown() async throws {
        wc?.window?.close()
        wc = nil
        rig = nil
    }

    private func open(_ show: String) async throws {
        wc?.window?.close()
        rig = try await Rig.open(show)
        wc = WorkspaceWindowController(store: rig.store)
        wc.window?.makeKeyAndOrderFront(nil)
        await assertEventually { self.wc.cueList.roots.isEmpty == false }
    }

    // MARK: Doing what the operator does

    private func select(_ id: ModelID) async {
        wc.cueList.selectAsUser([id])
        await assertEventually { self.inspector.cue?.id == id }
        inspector.tabView.selectTabViewItem(at: 0)
    }

    /// Clicks in the Target field, replaces what is there with `text`, and presses Return.
    private func typeTarget(_ text: String) throws {
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(inspector.targetField.isEnabled)
        XCTAssertTrue(window.makeFirstResponder(inspector.targetField))
        let editor = try XCTUnwrap(inspector.targetField.currentEditor() as? NSTextView)
        editor.selectAll(nil)
        editor.insertText(text, replacementRange: editor.selectedRange())
        editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
    }

    /// The button is drawn, enabled, and pressed.
    private func click(_ button: NSButton) throws {
        XCTAssertFalse(button.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(button.isEnabled)
        button.performClick(nil)
    }

    private func mouseDown(_ clicks: Int) throws {
        let field = inspector.targetField
        let event = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: field.convert(NSPoint(x: 4, y: 4), to: nil), modifierFlags: [],
                timestamp: 0, windowNumber: wc.window?.windowNumber ?? 0, context: nil, eventNumber: 0,
                clickCount: clicks, pressure: 1))
        // Direct: a press through the window only brings it forward when the app is not active (CI).
        field.mouseDown(with: event)
    }

    /// What dropping `dragged` on (or between) rows does, done.
    @discardableResult
    private func drop(
        _ dragged: [ModelID], on item: ModelID?, index: Int = on, targetColumn: Bool = true,
        file: StaticString = #filePath,
        line: UInt = #line
    ) throws -> CueListViewController.CueDrop {
        let nodes = dragged.compactMap { wc.cueList.node($0) }
        let planned = wc.cueList.cueDrop(
            nodes, item: item.flatMap { wc.cueList.node($0) }, index: index, inTargetColumn: targetColumn)
        let plan = try XCTUnwrap(planned, "the drop is refused", file: file, line: line)
        wc.cueList.perform(plan)
        return plan
    }

    private func add(_ kind: NewCueKind) async throws -> ModelID {
        let added = await wc.editor.add(kind)
        let id = try XCTUnwrap(added)
        await assertEventually { self.wc.cueList.node(id) != nil }
        return id
    }

    // MARK: What the window shows

    private func cell(_ column: String, _ id: ModelID) -> CueCell? {
        let o = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let col = o.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        return o.view(atColumn: col, row: o.row(forItem: node), makeIfNecessary: true) as? CueCell
    }

    private func targetCell(_ id: ModelID) -> String { cell("target", id)?.label.stringValue ?? "no cell" }
    private func nameCell(_ id: ModelID) -> String { cell("name", id)?.label.stringValue ?? "no cell" }
    private var order: [ModelID] { wc.cueList.roots.map(\.id) }
    private var original: [ModelID] { [Self.intro, Self.pad, Self.fade, Self.memo] }
    private func target(_ id: ModelID) -> String? { rig.store.model?.cue(id)?.raw["kind"]?["target"]?.stringValue }

    private func assertNoTargetShown(line: UInt = #line) {
        XCTAssertEqual(inspector.targetField.stringValue, "", line: line)
        XCTAssertTrue(inspector.targetField.isEnabled, line: line)
        XCTAssertEqual(inspector.targetNote.stringValue, loc("inspector.target.none"), line: line)
        XCTAssertFalse(inspector.targetNote.isHidden, line: line)
        XCTAssertFalse(inspector.clearTargetButton.isHidden, line: line)
        XCTAssertFalse(inspector.clearTargetButton.isEnabled, line: line)
        XCTAssertFalse(inspector.jumpTargetButton.isHidden, line: line)
        XCTAssertFalse(inspector.jumpTargetButton.isEnabled, line: line)
        XCTAssertEqual(targetCell(Self.fade), loc("cue.target.missing"), line: line)
    }

    // MARK: The issue's flows

    func testNoTargetShown() async throws {
        await select(Self.fade)
        assertNoTargetShown()
        XCTAssertEqual(targetCell(Self.fade), "?")
        XCTAssertTrue(inspector.chooseButton.isHidden, "a control cue is offered no file chooser")
    }

    func testTypeATargetNumber() async throws {
        await select(Self.fade)
        try typeTarget("1")
        await assertEventually { self.targetCell(Self.fade) == "1" }
        XCTAssertEqual(inspector.targetField.stringValue, "1")
        XCTAssertEqual(inspector.targetNote.stringValue, "intro")
        XCTAssertTrue(inspector.clearTargetButton.isEnabled)
        XCTAssertTrue(inspector.jumpTargetButton.isEnabled)
        XCTAssertEqual(inspector.messageLabel.stringValue, "")
    }

    func testTargetWithoutANumber() async throws {
        await select(Self.fade)
        await wc.editor.setTarget(of: [Self.fade], to: Self.pad)
        await assertEventually { self.targetCell(Self.fade) == "pad" }
        // The field holds a number and the name goes beside it, so there is nothing to type back.
        XCTAssertEqual(inspector.targetField.stringValue, "")
        XCTAssertEqual(inspector.targetNote.stringValue, "pad")
        XCTAssertTrue(inspector.jumpTargetButton.isEnabled)
    }

    func testClearTarget() async throws {
        await select(Self.fade)
        try typeTarget("1")
        await assertEventually { self.targetCell(Self.fade) == "1" }
        try click(inspector.clearTargetButton)
        await assertEventually { self.targetCell(Self.fade) == "?" }
        assertNoTargetShown()
        XCTAssertNil(target(Self.fade))
    }

    func testJumpToTarget() async throws {
        await select(Self.fade)
        try typeTarget("1")
        await assertEventually { self.inspector.jumpTargetButton.isEnabled }
        try click(inspector.jumpTargetButton)
        XCTAssertEqual(wc.cueList.selectedIDs(), [Self.intro], "the target's row is highlighted")
        let row = wc.cueList.outlineView.row(forItem: wc.cueList.node(Self.intro))
        XCTAssertTrue(wc.cueList.outlineView.rows(in: wc.cueList.outlineView.visibleRect).contains(row))
        await assertEventually { self.inspector.cue?.id == Self.intro }
        XCTAssertEqual(inspector.nameField.stringValue, "intro")
        await assertEventually { self.rig.store.playheads[self.list] == Self.intro }
        await assertEventually { self.wc.masthead.standbyField.stringValue.contains("intro") }
        let marker = try XCTUnwrap(cell("status", Self.intro)?.images[0])
        await assertEventually { !marker.isHidden }
    }

    func testFileTargetChooser() async throws {
        let wav = FileManager.default.temporaryDirectory.appendingPathComponent("\(UUID().uuidString).wav")
        XCTAssertTrue(FileManager.default.createFile(atPath: wav.path, contents: Data()))
        addTeardownBlock { try? FileManager.default.removeItem(at: wav) }
        var asked = 0
        var picked: URL?
        wc.pickAudioFile = {
            asked += 1
            return picked
        }
        await select(Self.intro)
        XCTAssertFalse(inspector.chooseButton.isHidden)
        XCTAssertFalse(inspector.targetField.isEditable, "the path is not typed in")
        let before = inspector.targetField.stringValue
        XCTAssertEqual(before, try showFolder() + "/media/a.wav", "the whole path, not the stored one")
        XCTAssertTrue(inspector.targetNote.isHidden)
        XCTAssertTrue(inspector.jumpTargetButton.isHidden, "a file has no cue to go to")
        // One click asks nothing; a cancelled chooser changes nothing.
        try mouseDown(1)
        XCTAssertEqual(asked, 0)
        try mouseDown(2)
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(inspector.targetField.stringValue, before)
        picked = wav
        try mouseDown(2)
        XCTAssertEqual(asked, 2)
        await assertEventually { self.inspector.targetField.stringValue == wav.standardizedFileURL.path }
        // The list's round button opens the same chooser, for the same cue.
        let round = try XCTUnwrap(cell("target", Self.intro)?.chooseButton)
        try click(round)
        XCTAssertEqual(asked, 3)
        // Show Mode offers no chooser.
        wc.setShowMode(true)
        try mouseDown(2)
        XCTAssertEqual(asked, 3)
        XCTAssertFalse(inspector.chooseButton.isEnabled)
    }

    func testNoTargetCueType() async throws {
        await select(Self.memo)
        XCTAssertFalse(inspector.targetField.isEnabled)
        XCTAssertEqual(inspector.targetField.stringValue, "")
        XCTAssertEqual(inspector.targetNote.stringValue, loc("inspector.target.notApplicable"))
        XCTAssertFalse(inspector.targetNote.isHidden)
        XCTAssertTrue(inspector.chooseButton.isHidden, "no chooser is offered")
        XCTAssertTrue(inspector.clearTargetButton.isHidden)
        XCTAssertTrue(inspector.jumpTargetButton.isHidden)
        XCTAssertEqual(targetCell(Self.memo), "", "the list shows nothing either")
    }

    func testDragTheActingCueOntoItsTarget() async throws {
        XCTAssertEqual(try drop([Self.fade], on: Self.intro), .setTarget([Self.fade], Self.intro))
        await assertEventually { self.targetCell(Self.fade) == "1" }
        XCTAssertEqual(order, original)
        XCTAssertEqual(nameCell(Self.fade), "Fade intro", "its default name follows")
    }

    func testDragTheTargetOntoTheActingCue() async throws {
        XCTAssertEqual(try drop([Self.intro], on: Self.fade), .setTarget([Self.fade], Self.intro))
        await assertEventually { self.targetCell(Self.fade) == "1" }
        XCTAssertEqual(order, original)
    }

    func testDropBetweenRowsStillReorders() async throws {
        await wc.editor.setTarget(of: [Self.fade], to: Self.pad)
        await assertEventually { self.targetCell(Self.fade) == "pad" }
        try drop([Self.fade], on: nil, index: 1)
        await assertEventually { self.order == [Self.intro, Self.fade, Self.pad, Self.memo] }
        XCTAssertEqual(target(Self.fade), Self.pad.raw)
        XCTAssertEqual(targetCell(Self.fade), "pad")
    }

    // MARK: The rules around them

    /// The folder of the open show.
    private func showFolder() throws -> String {
        (try XCTUnwrap(rig.store.document?.path) as NSString).deletingLastPathComponent
    }

    /// A file in the show's folder is stored relative to it; the field shows the whole path all the same,
    /// when chosen and when the saved show is opened again.
    func testAFileInTheShowFolderShowsItsWholePathAfterAReopen() async throws {
        let folder = try showFolder()
        let path = try XCTUnwrap(rig.store.document?.path)
        let chosen = URL(fileURLWithPath: folder).appendingPathComponent("media/b.wav")
        try FileManager.default.copyItem(atPath: folder + "/media/a.wav", toPath: chosen.path)
        wc.pickAudioFile = { chosen }
        await select(Self.intro)
        try mouseDown(2)
        await assertEventually { self.inspector.targetField.stringValue == chosen.standardizedFileURL.path }
        guard case .success = await rig.store.save() else { return XCTFail("the save") }
        guard case .success = await rig.client.send({ .close(id: $0, doc: self.rig.doc, discardChanges: false) })
        else { return XCTFail("the close") }
        wc.window?.close()
        let engine = rig.testEngine
        guard case .success(let ok) = await engine.request(.open(path: path)), case .opened(let doc)? = ok.data else {
            return XCTFail("the reopen")
        }
        rig = Rig(engine: engine, workspace: doc)
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        wc = WorkspaceWindowController(store: rig.store)
        wc.window?.makeKeyAndOrderFront(nil)
        await assertEventually { self.wc.cueList.node(Self.intro) != nil }
        await select(Self.intro)
        XCTAssertEqual(inspector.targetField.stringValue, chosen.standardizedFileURL.path)
        XCTAssertEqual(inspector.targetField.toolTip, chosen.standardizedFileURL.path)
        _ = await rig.client.send({ .close(id: $0, doc: doc, discardChanges: true) })
    }

    /// QLab 5 lists a Network cue as taking no target: its Target cell is empty (no patch name, no `?`).
    func testANetworkCuesTargetCellIsEmpty() async throws {
        let net = try await add(.network)
        let cell = try XCTUnwrap(cell("target", net))
        XCTAssertEqual(cell.label.stringValue, "")
        XCTAssertTrue(cell.chooseButton.isHidden)
    }

    /// A Devamp cue made from the toolbar carries every field core requires; one without `devamp_mode`
    /// is a frame the engine cannot read, and the add never got an answer.
    func testANewDevampCueIsAcceptedByTheEngine() async throws {
        let id = try await add(.devamp)
        let mode = wc.cueList.node(id)?.cue.raw["kind"]?["devamp_mode"]?.stringValue
        XCTAssertEqual(mode, "current_slice")
    }

    /// `InspectorEdit.targetRejection` is a copy of core's `can_target`: every cue that takes a target, on
    /// every kind and every Group mode, must be refused exactly where the engine would mark it broken.
    func testTheTargetRulesMatchTheEngines() async throws {
        var targets: [(String, ModelID)] = []
        for kind in NewCueKind.allCases where kind != .group { targets.append((kind.wireType, try await add(kind))) }
        for mode in InspectorViewController.groupModes {
            let group = try await add(.group)
            let set = await rig.store.send(
                command: .setProperty(group, .groupMode(type: mode, loop: false, shuffle: false)))
            guard case .success = set else { return XCTFail("group mode \(mode)") }
            targets.append(("group \(mode)", group))
        }
        let sources = NewCueKind.allCases.filter { InspectorEdit.targetKinds.contains($0.wireType) }
        XCTAssertEqual(Set(sources.map(\.wireType)), InspectorEdit.targetKinds, "every kind that takes a target")
        var refused = 0
        for kind in sources {
            let source = try await add(kind)
            for (name, target) in targets {
                guard case .success = await rig.store.send(command: .setProperty(source, .target(target))) else {
                    return XCTFail("\(kind.wireType) on \(name)")
                }
                try await rig.advance()
                let cue = try XCTUnwrap(rig.store.model?.cue(source))
                let targetCue = try XCTUnwrap(rig.store.model?.cue(target))
                let ours = InspectorEdit.targetRejection(of: cue, target: targetCue) != nil
                let engine = (rig.store.problems[source] ?? []).contains { $0.type == "target_wrong_kind" }
                XCTAssertEqual(ours, engine, "\(kind.wireType) on \(name)")
                if ours { refused += 1 }
            }
        }
        XCTAssertGreaterThan(refused, 0, "the table reached the refused cases")
    }

    /// QLab 5's change log, pp. 5-7: the dragged cue becomes the target of the cue it lands on.
    func testBothCouldTargetEachOtherTheDraggedCueBecomesTheTarget() async throws {
        let start = try await add(.start)
        let stop = try await add(.stop)
        let before = order
        XCTAssertEqual(try drop([start], on: stop), .setTarget([stop], start))
        await assertEventually { self.target(stop) == start.raw }
        XCTAssertNil(target(start))
        XCTAssertEqual(try drop([stop], on: start), .setTarget([start], stop))
        await assertEventually { self.target(start) == stop.raw }
        XCTAssertEqual(target(stop), start.raw, "each is set only by a drop on it")
        XCTAssertEqual(order, before)
    }

    func testAFadeAcceptsOnlyWhatCanBeFaded() async throws {
        let start = try await add(.start)
        // Either way round the Start cue is the one that can act on the other.
        XCTAssertEqual(try drop([Self.fade], on: start), .setTarget([start], Self.fade))
        XCTAssertEqual(try drop([start], on: Self.fade), .setTarget([start], Self.fade))
        // A Memo holds no target and cannot be faded: this is a reorder, the target unchanged.
        XCTAssertEqual(try drop([Self.fade], on: Self.memo), .move([Self.fade], nil, 4))
        await assertEventually { self.order == [Self.intro, Self.pad, Self.memo, Self.fade, start] }
        XCTAssertNil(target(Self.fade))
    }

    /// QLab 5.0's change log: only the Target column sets a target, so a reorder cannot change one by accident.
    func testADropElsewhereOnARowOnlyReorders() async throws {
        XCTAssertEqual(try drop([Self.fade], on: Self.intro, targetColumn: false), .move([Self.fade], nil, 1))
        await assertEventually { self.order == [Self.intro, Self.fade, Self.pad, Self.memo] }
        XCTAssertNil(target(Self.fade))
    }

    func testADropOnItsOwnTargetCellChangesNothing() async throws {
        let log = CueEditingTests.SendLog(rig)
        guard case .move = try drop([Self.fade], on: Self.fade) else {
            return XCTFail("a cue cannot be its own target")
        }
        // The move runs in a task: this waits for it, and an undo would then find a step if it had sent one.
        await settle()
        try await rig.advance()
        // The clock step above is itself one request; a move would be a second.
        XCTAssertEqual(log.sent, 1, "it is already there: nothing but the clock step went to the engine")
        XCTAssertEqual(order, original)
        XCTAssertNil(target(Self.fade))
    }

    func testDraggingAgainDoesNotRetargetOrSendAnything() async throws {
        await wc.editor.setTarget(of: [Self.fade], to: Self.intro)
        await assertEventually { self.target(Self.fade) == Self.intro.raw }
        let log = CueEditingTests.SendLog(rig)
        await wc.editor.setTarget(of: [Self.fade], to: Self.intro)
        XCTAssertEqual(log.sent, 0, "already its target")
    }

    func testSeveralDraggedCuesAllTargetTheOneDroppedOnInOneUndoStep() async throws {
        let start = try await add(.start)
        let stop = try await add(.stop)
        XCTAssertEqual(try drop([start, stop], on: Self.intro), .setTarget([start, stop], Self.intro))
        await assertEventually { self.targetCell(start) == "1" && self.targetCell(stop) == "1" }
        await wc.editor.undo()
        await assertEventually { self.target(start) == nil && self.target(stop) == nil }
    }

    func testADropOnAGroupsTargetCellMakesItTheTargetAndAnywhereElseMovesIn() async throws {
        try await open("Demo")
        let group = TestSnapshots.group
        let stop = try await add(.stop)
        let before = order
        XCTAssertEqual(try drop([stop], on: group), .setTarget([stop], group))
        await assertEventually { self.target(stop) == group.raw }
        XCTAssertEqual(order, before, "not moved into it")
        guard
            case .move(_, group?, _)? = wc.cueList.cueDrop(
                [try XCTUnwrap(wc.cueList.node(stop))], item: wc.cueList.node(group), index: Self.on,
                inTargetColumn: false)
        else { return XCTFail("a drop on the rest of the row goes in") }
    }

    func testATypedNumberThatMatchesNoCueIsRefusedOnScreen() async throws {
        await select(Self.fade)
        let log = CueEditingTests.SendLog(rig)
        try typeTarget("99")
        XCTAssertEqual(inspector.messageLabel.stringValue, loc("inspector.invalid.unknownTarget"))
        assertNoTargetShown()
        XCTAssertEqual(log.sent, 0)
    }

    func testACueCannotTargetItself() async throws {
        await select(Self.fade)
        let log = CueEditingTests.SendLog(rig)
        try typeTarget("3")
        XCTAssertEqual(inspector.messageLabel.stringValue, loc("inspector.invalid.selfTarget"))
        assertNoTargetShown()
        XCTAssertEqual(log.sent, 0)
    }

    func testATypedTargetOfTheWrongKindIsRefusedOnScreen() async throws {
        await select(Self.fade)
        let log = CueEditingTests.SendLog(rig)
        try typeTarget("4")
        XCTAssertEqual(inspector.messageLabel.stringValue, loc("inspector.invalid.wrongTargetKind"))
        assertNoTargetShown()
        XCTAssertEqual(log.sent, 0)
    }

    func testUndoOfATargetChange() async throws {
        try drop([Self.fade], on: Self.intro)
        await assertEventually { self.targetCell(Self.fade) == "1" }
        await select(Self.fade)
        await wc.editor.undo()
        await assertEventually { self.targetCell(Self.fade) == "?" }
        assertNoTargetShown()
        XCTAssertEqual(nameCell(Self.fade), CueKindStyle.kindLabel("fade"))
        await wc.editor.redo()
        await assertEventually { self.targetCell(Self.fade) == "1" }
        XCTAssertEqual(inspector.targetNote.stringValue, "intro")
    }

    func testClearDropsANumberTypedAndNotCommitted() async throws {
        await select(Self.fade)
        try typeTarget("1")
        await assertEventually { self.target(Self.fade) == Self.intro.raw }
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(inspector.targetField))
        let editor = try XCTUnwrap(inspector.targetField.currentEditor() as? NSTextView)
        editor.selectAll(nil)
        editor.insertText("4", replacementRange: editor.selectedRange())
        try click(inspector.clearTargetButton)
        await assertEventually { self.targetCell(Self.fade) == "?" }
        assertNoTargetShown()
        XCTAssertEqual(inspector.messageLabel.stringValue, "", "the dropped text was not checked")
        XCTAssertFalse(inspector.isEditing(.target))
    }

    /// A render from an edit elsewhere must leave a number being typed alone.
    func testAModelChangeLeavesANumberBeingTyped() async throws {
        await select(Self.fade)
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(inspector.targetField))
        let editor = try XCTUnwrap(inspector.targetField.currentEditor() as? NSTextView)
        editor.insertText("1", replacementRange: editor.selectedRange())
        guard case .success = await rig.store.send(command: .setProperty(Self.intro, .name("overture"))) else {
            return XCTFail("the rename")
        }
        await assertEventually { self.nameCell(Self.intro) == "overture" }
        inspector.render()
        XCTAssertNotNil(inspector.targetField.currentEditor(), "still typing")
        XCTAssertEqual(inspector.targetField.currentEditor()?.string, "1")
        editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
        await assertEventually { self.targetCell(Self.fade) == "1" }
        XCTAssertEqual(inspector.targetNote.stringValue, "overture")
    }

    func testATargetThatWasDeletedCanBeClearedButNotJumpedTo() async throws {
        await wc.editor.setTarget(of: [Self.fade], to: Self.pad)
        await assertEventually { self.targetCell(Self.fade) == "pad" }
        await select(Self.pad)
        await wc.editor.deleteSelection()
        await select(Self.fade)
        await assertEventually { self.inspector.targetField.stringValue == loc("inspector.target.missing") }
        XCTAssertEqual(targetCell(Self.fade), "?")
        XCTAssertEqual(inspector.targetNote.stringValue, "")
        XCTAssertFalse(inspector.jumpTargetButton.isEnabled)
        try click(inspector.clearTargetButton)
        await assertEventually { self.inspector.targetField.stringValue == "" }
        assertNoTargetShown()
    }

    func testJumpOpensTheCollapsedGroupsAroundTheTarget() async throws {
        try await open("Demo")
        let fade = TestSnapshots.fade
        let rain = TestSnapshots.rain
        await wc.editor.setTarget(of: [fade], to: rain)
        await assertEventually { self.target(fade) == rain.raw }
        wc.cueList.outlineView.collapseItem(wc.cueList.node(TestSnapshots.group))
        await select(fade)
        try click(inspector.jumpTargetButton)
        XCTAssertEqual(wc.cueList.selectedIDs(), [rain])
        await assertEventually { self.rig.store.playheads[self.list] == rain }
    }

    func testJumpFollowsATargetIntoAnotherList() async throws {
        let second: ModelID = "00000000-0000-0000-0000-0000000000a2"
        let added = await rig.store.send(command: .addCueList(id: second, name: "Second list", index: 1))
        guard case .success = added else { return XCTFail("no second list") }
        let other = ModelID(UUID().uuidString.lowercased())
        let memo = try XCTUnwrap(NewCueKind.memo.cueJSON(id: other, patch: nil, networkPatch: nil))
        guard case .success = await rig.store.send(command: .addCue(memo, list: second, parent: nil, index: 0)) else {
            return XCTFail("no cue in the second list")
        }
        let stop = try await add(.stop)
        guard case .success = await rig.store.send(command: .setProperty(stop, .target(other))) else {
            return XCTFail("no target")
        }
        await select(stop)
        await assertEventually { self.inspector.jumpTargetButton.isEnabled }
        try click(inspector.jumpTargetButton)
        XCTAssertEqual(wc.cueList.listID, second)
        XCTAssertEqual(wc.cueList.selectedIDs(), [other])
    }

    func testShowModeRefusesTargetEdits() async throws {
        await wc.editor.setTarget(of: [Self.fade], to: Self.intro)
        await assertEventually { self.targetCell(Self.fade) == "1" }
        await select(Self.fade)
        wc.setShowMode(true)
        await assertEventually { self.inspector.isLocked }
        XCTAssertFalse(inspector.targetField.isEnabled)
        XCTAssertFalse(inspector.clearTargetButton.isEnabled)
        XCTAssertNil(
            wc.cueList.cueDrop(
                [try XCTUnwrap(wc.cueList.node(Self.pad))], item: wc.cueList.node(Self.fade), index: Self.on,
                inTargetColumn: true))
        let log = CueEditingTests.SendLog(rig)
        await wc.editor.setTarget(of: [Self.fade], to: Self.pad)
        XCTAssertEqual(log.sent, 0, "a drop on its way is refused too")
    }
}
