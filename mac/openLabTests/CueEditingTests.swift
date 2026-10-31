// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Add, delete, reorder, renumber, undo and redo (M5.10 part 1): the commands sent, where cues
/// go, what is refused and that Show Mode blocks every path.
@MainActor
final class CueEditingTests: XCTestCase {
    typealias H = CueListHarness

    private func harness() async throws -> (H, CueEditor, SendLog) {
        let h = try await H(collapsed: false)
        let editor = CueEditor(store: h.store, cueList: h.vc)
        let log = SendLog(h.rig.engine)
        editor.notify = { log.notices.append($0) }
        return (h, editor, log)
    }

    /// Commands the fake engine received after a mark.
    @MainActor final class SendLog {
        let engine: FakeEngine
        var notices: [String] = []
        private var mark = 0
        init(_ engine: FakeEngine) {
            self.engine = engine
            mark = engine.received.count
        }
        var messages: [ClientMessage] { Array(engine.received.dropFirst(mark)) }
        var commands: [JSONValue] {
            messages.compactMap { if case .command(_, let c, _) = $0 { return c.json } else { return nil } }
        }
        /// Everything but transport and the Show Mode switch itself.
        var edits: Int {
            messages.filter {
                switch $0 {
                case .transport, .request(_, .setShowMode): return false
                default: return true
                }
            }.count
        }
    }

    /// What goes over the wire: encoded and parsed back, so 1.0 and 1 compare as the engine sees them.
    private func wire(_ c: Command) throws -> JSONValue? { JSONValue.parse(try c.json.text()) }

    // MARK: Command shapes

    func testCommandShapesMatchCore() throws {
        let fixture = try Fixtures.json("client", "command_add_cue")["command"]?["commands"]?.arrayValue ?? []
        let cue = try XCTUnwrap(fixture[0]["cue"])
        XCTAssertEqual(
            Command.addCue(cue, list: "00000000-0000-0000-0000-000000000001", parent: nil, index: 0).json, fixture[0])
        XCTAssertEqual(Command.removeCue("00000000-0000-0000-0000-00000000000b").json, fixture[1])
        let id: ModelID = "a"
        let list: ModelID = "l"
        let cases: [(Command, String)] = [
            (.moveCue(id, index: 2), #"{"type":"move_cue","id":"a","index":2}"#),
            (
                .reparentCue(id, list: list, parent: "g", index: 1),
                #"{"type":"reparent_cue","id":"a","to":{"list":"l","parent":"g","index":1}}"#
            ),
            (
                .renumber([id], start: 1, increment: 0.5),
                #"{"type":"renumber","ids":["a"],"start":1.0,"increment":0.5}"#
            ),
        ]
        for (command, text) in cases { XCTAssertEqual(try wire(command), JSONValue.parse(text), text) }
        XCTAssertEqual(
            Command.transaction([.removeCue("a"), .removeCue("b")])?.json,
            JSONValue.parse(
                #"{"type":"transaction","commands":[{"type":"remove_cue","id":"a"},{"type":"remove_cue","id":"b"}]}"#))
        XCTAssertNil(Command.transaction([]))
    }

    func testEveryKindHasCoreShapedJSON() throws {
        let patch = try XCTUnwrap(
            try Fixtures.json("server", "welcome")["snapshot"]?["workspace"]?["patches"]?.arrayValue?.first)
        let model = try JSONDecoder().decode(ModelObject.self, from: Data(try patch.text().utf8))
        let desk = try XCTUnwrap(
            try Fixtures.json("server", "welcome")["snapshot"]?["workspace"]?["network_patches"]?.arrayValue?.first)
        let network = try JSONDecoder().decode(ModelObject.self, from: Data(try desk.text().utf8))
        for kind in NewCueKind.allCases {
            let cue = try XCTUnwrap(kind.cueJSON(id: "x", patch: model, networkPatch: network), "\(kind)")
            XCTAssertEqual(cue["kind"]?["type"]?.stringValue, kind.wireType)
            // The same top-level fields the fixture cue has.
            let fixture = try Fixtures.json("client", "command_add_cue")["command"]?["commands"]?.arrayValue?[0]["cue"]
            guard case .object(let keys) = cue, case .object(let want)? = fixture else { return XCTFail() }
            XCTAssertEqual(Set(keys.keys), Set(want.keys))
        }
        XCTAssertNil(NewCueKind.audio.cueJSON(id: "x", patch: nil), "an audio cue needs a patch")
        XCTAssertEqual(
            NewCueKind.audio.kindJSON(patch: model)?["levels"]?["outputs"]?.arrayValue?.count, 2,
            "one cue output per patch output")
        XCTAssertEqual(NewCueKind.goto.wireType, "go_to")
        XCTAssertEqual(CueToolbarView.kinds.flatMap { $0 }.compactMap { $0 }.count, NewCueKind.allCases.count)
    }

    /// A dropped file's `AddCue` (built as `addFiles` builds it) is the engine's golden fixture, which the
    /// engine's real-audio tests play and measure: file channel n on cue output n at 0 dB, so it is heard.
    func testNewAudioCueEncodesAsTheGoldenFixtureTheEnginePlays() throws {
        let patch = try XCTUnwrap(
            try Fixtures.json("server", "welcome")["snapshot"]?["workspace"]?["patches"]?.arrayValue?.first)
        let model = try JSONDecoder().decode(ModelObject.self, from: Data(try patch.text().utf8))
        let fixture = try XCTUnwrap(Fixtures.json("client", "command_add_new_audio_cue")["command"])
        var cue = try XCTUnwrap(NewCueKind.audio.cueJSON(id: "00000000-0000-0000-0000-00000000000d", patch: model))
        var kind = try XCTUnwrap(cue["kind"])
        kind.set("media", .string("/Volumes/Show/thunder.wav"))
        cue.set("kind", kind)
        let sent = Command.addCue(cue, list: "00000000-0000-0000-0000-000000000001", parent: nil, index: 0).json
        XCTAssertEqual(normalised(sent), normalised(fixture))
    }

    // MARK: Insertion

    func testInsertionPosition() async throws {
        let h = try await H(collapsed: false)
        let n = { (id: ModelID) in h.vc.node(id)! }
        let roots = h.vc.roots
        XCTAssertEqual(CueEditor.insertion(selected: [], roots: roots).index, 3)
        XCTAssertNil(CueEditor.insertion(selected: [], roots: roots).parent)
        let afterIntro = CueEditor.insertion(selected: [n(H.intro)], roots: roots)
        XCTAssertEqual(afterIntro.index, 1)
        XCTAssertNil(afterIntro.parent)
        // A selected group takes the cue in, as its last child.
        XCTAssertEqual(CueEditor.insertion(selected: [n(H.group)], roots: roots).index, 3)
        XCTAssertEqual(CueEditor.insertion(selected: [n(H.group)], roots: roots).parent, H.group)
        // Inside a group: after the last selected child.
        let inside = CueEditor.insertion(selected: [n(H.door), n(H.rain)], roots: roots)
        XCTAssertEqual(inside.parent, H.group)
        XCTAssertEqual(inside.index, 2)
    }

    func testAddSendsAddCueAfterTheSelectionAndSelectsIt() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door])
        let id = await editor.add(.memo)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "add_cue")
        XCTAssertEqual(sent["at"]?["parent"]?.stringValue, H.group.raw)
        XCTAssertEqual(sent["at"]?["index"], .int(1))
        XCTAssertEqual(sent["at"]?["list"]?.stringValue, H.list)
        XCTAssertEqual(sent["auto_number"], .bool(true))
        XCTAssertEqual(sent["cue"]?["kind"]?["type"]?.stringValue, "memo")
        XCTAssertEqual(sent["cue"]?["id"]?.stringValue, id?.raw)
        let new = try XCTUnwrap(id)
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(h.vc.node(H.group)?.children.map(\.id), [H.door, new, H.rain, H.radio])
    }

    func testAddWithAGroupSelectedPutsTheCueInsideItLast() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.group])
        let added = await editor.add(.memo)
        let new = try XCTUnwrap(added)
        XCTAssertEqual(log.commands.first?["at"]?["parent"]?.stringValue, H.group.raw)
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(h.vc.node(H.group)?.children.map(\.id), [H.door, H.rain, H.radio, new])
    }

    // MARK: Group from the selection

    /// The cue list's shape, e.g. "0a 20[21 22] 0b", from the demo ids' last two characters; the
    /// new cue shows as "N".
    private func shape(_ nodes: [CueNode], new: ModelID?) -> String {
        nodes.map { n in
            let name = n.id == new ? "N" : String(n.id.raw.suffix(2))
            return n.isGroup ? "\(name)[\(shape(n.children, new: new))]" : name
        }.joined(separator: " ")
    }

    func testNewGroupTakesInScatteredSelectedCuesInListOrderAsOneUndoStep() async throws {
        let (h, editor, log) = try await harness()
        let before = shape(h.vc.roots, new: nil)
        XCTAssertEqual(before, "0a 0b 20[21 22 23]")
        // Picked bottom first and from different parents: they still go in list order, at the
        // first one's place.
        h.vc.select([H.door, H.intro])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        XCTAssertEqual(log.commands.count, 1, "one undo step")
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "transaction")
        let steps = sent["commands"]?.arrayValue ?? []
        XCTAssertEqual(steps.map { $0["type"]?.stringValue }, ["add_cue", "reparent_cue", "reparent_cue"])
        XCTAssertEqual(steps.first?["at"]?["parent"], .null)
        XCTAssertEqual(steps.first?["at"]?["index"], .int(0))
        XCTAssertEqual(steps.first?["cue"]?["kind"]?["type"]?.stringValue, "group")
        XCTAssertEqual(steps.dropFirst().map { $0["id"]?.stringValue }, [H.intro.raw, H.door.raw])
        XCTAssertEqual(steps.dropFirst().map { $0["to"]?["parent"]?.stringValue }, [new.raw, new.raw])
        XCTAssertEqual(steps.dropFirst().map { $0["to"]?["index"] }, [.int(0), .int(1)])
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(shape(h.vc.roots, new: new), "N[0a 21] 0b 20[22 23]")
        XCTAssertTrue(log.notices.isEmpty)
        await editor.undo()
        await assertEventually { shape(h.vc.roots, new: new) == before }
    }

    func testNewGroupTakesInContiguousCuesInsideAGroupWhereTheFirstWas() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.door, H.rain])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(shape(h.vc.roots, new: new), "0a 0b 20[N[21 22] 23]")
    }

    func testNewGroupWithAGroupAndItsOwnChildSelectedTakesTheGroupWhole() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.group, H.rain])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(shape(h.vc.roots, new: new), "0a 0b N[20[21 22 23]]")
    }

    func testNewGroupAroundARunningCueSendsOnlyTheEditAndIsNotRefused() async throws {
        let (h, editor, log) = try await harness()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.intro)","state":"running"}"#)
        h.vc.select([H.intro, H.fade])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        XCTAssertTrue(log.notices.isEmpty)
        // No stop, start or other transport: the one edit, and the playhead following the selection.
        let others = log.messages.filter {
            switch $0 {
            case .command, .transport(_, .setPlayhead): return false
            default: return true
            }
        }
        XCTAssertEqual(others, [])
        XCTAssertEqual(log.commands.map { $0["type"]?.stringValue }, ["transaction"])
        await assertEventually { h.vc.node(new)?.children.map(\.id) == [H.intro, H.fade] }
    }

    func testNewGroupWithOneCueSelectedIsAnEmptyGroupAfterIt() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        XCTAssertEqual(log.commands.first?["type"]?.stringValue, "add_cue")
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(shape(h.vc.roots, new: new), "0a N[] 0b 20[21 22 23]")
    }

    func testNewGroupRefusedByTheEngineLeavesTheListAndSaysWhy() async throws {
        let (h, editor, log) = try await harness()
        h.rig.engine.refuseCommands = true
        h.vc.select([H.intro, H.fade])
        let new = await editor.add(.group)
        XCTAssertNil(new)
        XCTAssertEqual(log.notices.count, 1)
        XCTAssertTrue(log.notices[0].contains("refused by the fake"), log.notices[0])
        XCTAssertEqual(shape(h.vc.roots, new: nil), "0a 0b 20[21 22 23]")
    }

    func testAddAudioWithoutAPatchIsRefusedWithAMessage() async throws {
        var snapshot = FakeEngine.demoSnapshot(base: try Fixtures.json("server", "welcome")["snapshot"])
        snapshot["workspace"]?.set("patches", .array([]))
        let h = try await H(snapshot: snapshot)
        let editor = CueEditor(store: h.store, cueList: h.vc)
        let log = SendLog(h.rig.engine)
        editor.notify = { log.notices.append($0) }
        let id = await editor.add(.audio)
        XCTAssertNil(id)
        XCTAssertEqual(log.notices, [loc("edit.error.noPatch")])
        XCTAssertTrue(log.commands.isEmpty)
    }

    func testAddNetworkCueUsesTheFirstNetworkPatchAndIsSelected() async throws {
        let (h, editor, log) = try await harness()
        let desk = try XCTUnwrap(h.store.model?.networkPatches.first)
        let added = await editor.add(.network)
        let id = try XCTUnwrap(added)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(log.commands.count, 1, "an existing destination needs no new patch")
        XCTAssertEqual(sent["type"]?.stringValue, "add_cue")
        XCTAssertEqual(sent["cue"]?["kind"]?["type"]?.stringValue, "network")
        XCTAssertEqual(sent["cue"]?["kind"]?["destination"]?.stringValue, desk.id.raw)
        XCTAssertEqual(sent["cue"]?["kind"]?["address"]?.stringValue, "")
        XCTAssertEqual(sent["cue"]?["kind"]?["args"], .array([]))
        await assertEventually { h.vc.selectedIDs() == [id] }
        XCTAssertEqual(h.vc.node(id)?.cue.kindType, "network")
        XCTAssertTrue(log.notices.isEmpty)
    }

    func testAddNetworkCueWithNoNetworkPatchAddsADefaultOneInTheSameStep() async throws {
        var snapshot = FakeEngine.demoSnapshot(base: try Fixtures.json("server", "welcome")["snapshot"])
        snapshot["workspace"]?.set("network_patches", .array([]))
        let h = try await H(snapshot: snapshot)
        let editor = CueEditor(store: h.store, cueList: h.vc)
        let log = SendLog(h.rig.engine)
        editor.notify = { log.notices.append($0) }
        let added = await editor.add(.network)
        let id = try XCTUnwrap(added)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(log.commands.count, 1, "one command, so one undo step")
        XCTAssertEqual(sent["type"]?.stringValue, "transaction")
        let inner = try XCTUnwrap(sent["commands"]?.arrayValue)
        XCTAssertEqual(inner.map { $0["type"]?.stringValue }, ["add_network_patch", "add_cue"])
        let patch = try XCTUnwrap(inner[0]["patch"])
        XCTAssertEqual(patch["host"]?.stringValue, "127.0.0.1")
        XCTAssertEqual(patch["port"], .int(53000))
        XCTAssertEqual(patch["protocol"]?.stringValue, "udp")
        XCTAssertEqual(patch["name"]?.stringValue, loc("edit.networkPatch.defaultName"))
        XCTAssertEqual(inner[1]["cue"]?["kind"]?["destination"], patch["id"])
        XCTAssertEqual(inner[1]["cue"]?["id"]?.stringValue, id.raw)
        await assertEventually { h.vc.node(id) != nil }
        XCTAssertTrue(log.notices.isEmpty)

        // Byte for byte the engine's golden message, which its tests play: only the ids are the
        // app's random ones, so they are set to the fixture's.
        let golden = try XCTUnwrap(try Fixtures.json("client", "command_add_new_network_cue")["command"])
        let want = try XCTUnwrap(golden["commands"]?.arrayValue)
        let patchID = try XCTUnwrap(want[0]["patch"]?["id"])
        var steps = try XCTUnwrap(sent["commands"]?.arrayValue)
        var patchStep = steps[0]
        var patch = try XCTUnwrap(patchStep["patch"])
        patch.set("id", patchID)
        patchStep.set("patch", patch)
        var cueStep = steps[1]
        var cue = try XCTUnwrap(cueStep["cue"])
        var kind = try XCTUnwrap(cue["kind"])
        kind.set("destination", patchID)
        cue.set("kind", kind)
        cue.set("id", try XCTUnwrap(want[1]["cue"]?["id"]))
        cueStep.set("cue", cue)
        steps = [patchStep, cueStep]
        var sentCommand = sent
        sentCommand.set("commands", .array(steps))
        XCTAssertEqual(JSONValue.parse(try sentCommand.text()), golden)
    }

    func testNetworkIsOnTheToolbarAndTheCuesMenuAtCmd8() throws {
        XCTAssertTrue(CueToolbarView.kinds.flatMap { $0 }.contains(.network))
        let cues = MainMenu.make().cues
        let entry = try XCTUnwrap(cues.items.first { ($0.representedObject as? String) == "network" })
        XCTAssertEqual(entry.keyEquivalent, "8")
        XCTAssertEqual(entry.keyEquivalentModifierMask, [.command])
        XCTAssertEqual(entry.action, #selector(AppDelegate.newCue(_:)))
        XCTAssertEqual(cues.items.filter { $0.keyEquivalent == "8" }.count, 1)
    }

    // MARK: Delete

    func testDeleteSendsOneTransactionAndSkipsChildrenOfADeletedGroup() async throws {
        let (h, editor, log) = try await harness()
        try h.runtime(#"{"type":"cue_state","cue":"\#(H.rain)","state":"idle"}"#)  // the demo's paused child
        h.vc.select([H.group, H.door, H.fade])
        await editor.deleteSelection()
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "transaction")
        let ids = sent["commands"]?.arrayValue?.map { $0["id"]?.stringValue }
        XCTAssertEqual(ids, [H.fade.raw, H.group.raw])
        XCTAssertEqual(log.commands.count, 1)
    }

    func testDeleteOfASingleCueIsPlainRemoveAndSelectionMovesOn() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.fade])
        await editor.deleteSelection()
        XCTAssertEqual(log.commands.first, Command.removeCue(H.fade).json)
        await assertEventually { h.vc.selectedIDs() == [H.group] }
    }

    func testDeleteRefusedForRunningCuePausedCueAndGroupWithARunningChild() async throws {
        for state in ["running", "paused", "fading", "pre_waiting"] {
            let (h, editor, log) = try await harness()
            try h.runtime(#"{"type":"cue_state","cue":"\#(H.rain)","state":"\#(state)"}"#)
            h.vc.select([H.rain])
            await editor.deleteSelection()
            XCTAssertTrue(log.commands.isEmpty, state)
            XCTAssertEqual(log.notices, [loc("edit.error.deleteBusy")], state)
            h.vc.select([H.group])
            await editor.deleteSelection()
            XCTAssertTrue(log.commands.isEmpty, "group with a \(state) child")
            XCTAssertEqual(log.notices.count, 2)
        }
    }

    func testDeleteRefusedWhilePreviewing() async throws {
        let (h, editor, log) = try await harness()
        try h.runtime(#"{"type":"preview","cue":"\#(H.intro)","active":true}"#)
        h.vc.select([H.intro])
        await editor.deleteSelection()
        XCTAssertTrue(log.commands.isEmpty)
        XCTAssertEqual(log.notices.count, 1)
    }

    func testDeleteOfIdleAndLoadedCuesIsAllowed() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door])  // loaded in the demo show
        await editor.deleteSelection()
        XCTAssertEqual(log.commands.count, 1)
        XCTAssertTrue(log.notices.isEmpty)
    }

    // MARK: Reorder

    func testDropValidationRefusesOwnDescendants() async throws {
        let h = try await H(collapsed: false)
        let n = { (id: ModelID) in h.vc.node(id)! }
        XCTAssertFalse(CueEditor.canDrop([n(H.group)], into: n(H.group)), "into itself")
        XCTAssertFalse(CueEditor.canDrop([n(H.group)], into: n(H.door)), "into a child")
        XCTAssertFalse(CueEditor.canDrop([n(H.intro), n(H.group)], into: n(H.rain)))
        XCTAssertTrue(CueEditor.canDrop([n(H.door)], into: n(H.group)))
        XCTAssertTrue(CueEditor.canDrop([n(H.group)], into: nil))
        XCTAssertFalse(CueEditor.canDrop([], into: nil))
    }

    func testMoveCommandsWithinAndBetweenGroups() async throws {
        let h = try await H(collapsed: false)
        let n = { (id: ModelID) in h.vc.node(id)! }
        let list = ModelID(H.list)
        // Intro to the end of the top level: after the others have shifted, index 2.
        XCTAssertEqual(
            CueEditor.moveCommands(list: list, dragged: [n(H.intro)], parent: nil, index: 3, roots: h.vc.roots)
                .map(\.json), [Command.moveCue(H.intro, index: 2).json])
        // Fade into the group before Rain.
        XCTAssertEqual(
            CueEditor.moveCommands(list: list, dragged: [n(H.fade)], parent: n(H.group), index: 1, roots: h.vc.roots)
                .map(\.json), [Command.reparentCue(H.fade, list: list, parent: H.group, index: 1).json])
        // Rain out of the group to the top, before Intro.
        XCTAssertEqual(
            CueEditor.moveCommands(list: list, dragged: [n(H.rain)], parent: nil, index: 0, roots: h.vc.roots)
                .map(\.json), [Command.reparentCue(H.rain, list: list, parent: nil, index: 0).json])
        // Two cues dragged together keep their order and land together.
        let two = CueEditor.moveCommands(
            list: list, dragged: [n(H.door), n(H.radio)], parent: n(H.group), index: 2, roots: h.vc.roots
        ).map(\.json)
        XCTAssertEqual(two, [Command.moveCue(H.door, index: 2).json, Command.moveCue(H.radio, index: 2).json])
    }

    func testMoveSendsOneUndoStep() async throws {
        let (h, editor, log) = try await harness()
        await editor.move([H.door, H.rain], to: nil, at: 0)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "transaction")
        XCTAssertEqual(sent["commands"]?.arrayValue?.count, 2)
        // Dropping the group into itself is refused before anything is sent.
        let before = log.commands.count
        await editor.move([H.group], to: H.radio, at: 0)
        await editor.move([H.group], to: H.group, at: 0)
        XCTAssertEqual(log.commands.count, before)
        _ = h
    }

    func testMoveSelectionByOneKeepsBlockedCuesInPlace() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door, H.rain])
        await editor.moveSelection(by: -1)  // Door is first already: only Rain stays behind it
        XCTAssertTrue(log.commands.isEmpty, "a selection already at the top does not move")
        h.vc.select([H.rain, H.radio])
        await editor.moveSelection(by: -1)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(
            sent["commands"]?.arrayValue?.map { $0["id"]?.stringValue }, [H.rain.raw, H.radio.raw])
        XCTAssertEqual(sent["commands"]?.arrayValue?.map { $0["index"] }, [.int(0), .int(1)])
    }

    // MARK: Renumber, undo, redo

    func testRenumberSendsTheSelectionAndRefusesBadValues() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro, H.fade])
        await editor.renumberSelection(start: 10, increment: 5)
        XCTAssertEqual(
            log.commands.first,
            try wire(.renumber([H.intro, H.fade], start: 10, increment: 5)))
        for (s, i) in [(-1.0, 1.0), (1.0, 0.0), (1.0, -1.0), (.nan, 1.0), (1.0, .infinity)] {
            await editor.renumberSelection(start: s, increment: i)
        }
        XCTAssertEqual(log.commands.count, 1)
        XCTAssertEqual(log.notices.count, 5)
    }

    func testUndoAndRedoAreEngineMessagesAndFollowDocumentState() async throws {
        let (h, editor, log) = try await harness()
        XCTAssertEqual(editor.canUndo, h.store.document?.canUndo, "enabled from the engine's document state")
        XCTAssertEqual(editor.canRedo, h.store.document?.canRedo)
        await editor.undo()
        await editor.redo()
        XCTAssertEqual(
            log.messages.map { if case .undo = $0 { "undo" } else if case .redo = $0 { "redo" } else { "?" } },
            ["undo", "redo"])
        h.vc.select([H.fade])
        await editor.deleteSelection()
        await assertEventually { editor.canUndo && h.vc.node(H.fade) == nil }
        XCTAssertFalse(editor.canRedo)
        await editor.undo()
        await assertEventually { editor.canRedo && h.vc.node(H.fade) != nil }
        XCTAssertFalse(editor.canUndo)
        await editor.redo()
        await assertEventually { h.vc.node(H.fade) == nil }
    }

    func testAnEngineRefusalIsShown() async throws {
        let (h, editor, log) = try await harness()
        h.rig.engine.refuseCommands = true
        h.vc.select([H.fade])
        await editor.deleteSelection()
        XCTAssertEqual(log.notices.count, 1)
        XCTAssertTrue(log.notices[0].contains("refused by the fake"), log.notices[0])
    }

    // MARK: Show Mode

    func testShowModeBlocksEveryEditPath() async throws {
        let rig = try Rig()
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        let ids = wc.cueList.roots.map(\.id)
        let first = try XCTUnwrap(ids.first)
        wc.cueList.select([first])
        let log = SendLog(rig.engine)
        wc.setShowMode(true)
        let e = wc.editor
        XCTAssertTrue(wc.cueList.isEditLocked)
        XCTAssertTrue(wc.cueToolbar.segmentedControls.allSatisfy { !$0.isEnabled })
        for kind in NewCueKind.allCases { await e.add(kind) }
        wc.cueList.select(Array(ids.prefix(2)))
        await e.add(.group)
        wc.cueList.select([first])
        await e.deleteSelection()
        await e.moveSelection(by: 1)
        await e.move([first], to: nil, at: 0)
        await e.renumberSelection(start: 1, increment: 1)
        await e.undo()
        await e.redo()
        wc.cueToolbar.onAdd?(.memo)
        XCTAssertFalse(e.canUndo)
        XCTAssertFalse(e.canRedo)
        XCTAssertNil(
            wc.cueList.outlineView(
                wc.cueList.outlineView, pasteboardWriterForItem: try XCTUnwrap(wc.cueList.node(first))))
        for _ in 0..<50 { await Task.yield() }  // let any stray Task run
        XCTAssertEqual(log.edits, 0, "nothing reached the engine")
        wc.setShowMode(false)
        XCTAssertFalse(wc.cueList.isEditLocked)
        XCTAssertNotNil(
            wc.cueList.outlineView(
                wc.cueList.outlineView, pasteboardWriterForItem: try XCTUnwrap(wc.cueList.node(first))))
    }

    func testTextUndoNeverFallsThroughToTheWorkspace() {
        XCTAssertNil(CueEditor.textUndo(nil, redo: false), "no text editing: the workspace undoes")
        let text = NSTextView()  // not in a window, so its undo stack is empty
        XCTAssertEqual(CueEditor.textUndo(text, redo: false), false)
        XCTAssertEqual(CueEditor.textUndo(text, redo: true), false)
    }

    func testToolbarButtonsAskForTheirKind() throws {
        let toolbar = CueToolbarView()
        var asked: [NewCueKind] = []
        toolbar.onAdd = { asked.append($0) }
        toolbar.add(group: 0, segment: 0)
        toolbar.add(group: 1, segment: 1)
        toolbar.add(group: 2, segment: 0)
        toolbar.add(group: 4, segment: 1)  // Target: the model has no such cue
        XCTAssertEqual(asked, [.audio, .memo, .network])
        toolbar.isLocked = true
        toolbar.add(group: 0, segment: 0)  // even if a control were live, a locked toolbar adds nothing
        XCTAssertEqual(asked, [.audio, .memo, .network])
        XCTAssertFalse(toolbar.segmentedControls[4].isEnabled(forSegment: 1))
    }

    func testCuesMenuKeysFollowQLab() {
        let cues = MainMenu.make().cues
        let keys = Dictionary(
            uniqueKeysWithValues: cues.items.compactMap { i in
                (i.representedObject as? String).map { ($0, i.keyEquivalent) }
            })
        XCTAssertEqual(keys["group"], "0")
        XCTAssertEqual(keys["audio"], "1")
        XCTAssertEqual(keys["fade"], "7")
        XCTAssertEqual(keys["network"], "8")
        // Types QLab gives no menu key (or that openLab alone has) stay unbound.
        for kind in NewCueKind.allCases where ![.group, .audio, .fade, .network].contains(kind) {
            XCTAssertEqual(keys[kind.rawValue], "", "\(kind)")
        }
        let edit = MainMenu.make().edit
        let delete = edit.items.first { $0.action == #selector(AppDelegate.deleteCues(_:)) }
        XCTAssertEqual(delete?.keyEquivalent, "\u{8}")
        XCTAssertEqual(delete?.keyEquivalentModifierMask, [.command])
    }
}
