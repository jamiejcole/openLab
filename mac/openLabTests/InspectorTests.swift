// SPDX-License-Identifier: GPL-3.0-or-later

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

    /// The Wait/Fade duration and Load time properties (M5.6b) encode as the engine's golden fixtures.
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
        XCTAssertFalse(vc.targetField.isEnabled, "an audio cue has no cue target")
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
            await assertEventually("the model answers") { self.vc.groupModePopup.indexOfSelectedItem == index }
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
        vc.commit(.name)  // Return: sent, field stays focused
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
