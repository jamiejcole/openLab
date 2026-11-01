// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Add, delete, reorder, renumber, undo and redo (M5.10 part 1) on the real engine: where cues go
/// once the engine has made the edit, what the app refuses before sending, what the engine refuses,
/// and that Show Mode blocks every path.
@MainActor
final class CueEditingTests: XCTestCase {
    typealias H = CueListHarness

    /// The demo show open on the real engine, its list expanded, an editor on it.
    private func harness(_ show: String = "Demo") async throws -> (H, CueEditor, SendLog) {
        try await Self.harness(show)
    }

    static func harness(_ show: String = "Demo") async throws -> (H, CueEditor, SendLog) {
        let h = try await H(collapsed: false, rig: try await Rig.open(show))
        let editor = CueEditor(store: h.store, cueList: h.vc)
        let log = SendLog(h.rig)
        editor.notify = { log.notices.append($0) }
        return (h, editor, log)
    }

    /// What the client sent the engine after a mark (every command, transport, undo, redo and
    /// request takes a request id), and the notices the editor showed.
    @MainActor final class SendLog {
        let rig: Rig
        var notices: [String] = []
        private var mark: UInt64
        init(_ rig: Rig) {
            self.rig = rig
            mark = rig.client.nextRequestID
        }
        var sent: Int { Int(rig.client.nextRequestID - mark) }
        func reset() { mark = rig.client.nextRequestID }
    }

    /// Starts `cue` on the engine and waits until the store shows it in `state` (default running).
    static func start(_ h: H, _ cue: ModelID, until state: WireCueState = .running) async throws {
        guard case .success = await h.store.transport(.start(cue: cue)) else { return XCTFail("start") }
        try await h.rig.advance(0.05)
        await assertEventually("\(cue) is \(state)") { h.store.cueStates[cue]?.state == state }
    }

    /// Makes the store hold a cue the engine never had (as if its copy were behind the engine's): an
    /// edit that names it is one the engine refuses. A snapshot puts the store right again.
    static func phantom(_ h: H, _ id: ModelID = "00000000-0000-0000-0000-0000000000ee", index: Int) throws {
        try h.change(upserts: [try H.upsertJSON(h.cue(id.raw, "Ghost"), index: index)])
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
        let added = await editor.add(.memo)
        let new = try XCTUnwrap(added)
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(h.vc.node(H.group)?.children.map(\.id), [H.door, new, H.rain, H.radio])
        XCTAssertEqual(h.vc.node(new)?.cue.kindType, "memo")
        XCTAssertNotNil(h.vc.node(new)?.cue.number, "the engine numbered it (auto_number)")
        XCTAssertTrue(log.notices.isEmpty)
    }

    func testAddWithAGroupSelectedPutsTheCueInsideItLast() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.group])
        let added = await editor.add(.memo)
        let new = try XCTUnwrap(added)
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
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(shape(h.vc.roots, new: new), "N[0a 21] 0b 20[22 23]")
        XCTAssertEqual(h.vc.node(new)?.cue.kindType, "group")
        XCTAssertTrue(log.notices.isEmpty)
        // One undo step takes the whole of it back.
        await editor.undo()
        await assertEventually { shape(h.vc.roots, new: new) == before }
        XCTAssertNil(h.vc.node(new))
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
        try await Self.start(h, H.intro)
        h.vc.select([H.intro, H.fade])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        XCTAssertTrue(log.notices.isEmpty)
        await assertEventually { h.vc.node(new)?.children.map(\.id) == [H.intro, H.fade] }
        // No stop or restart went with the edit: Intro plays on inside the new group.
        try await h.rig.advance(0.1)
        XCTAssertEqual(h.store.cueStates[H.intro]?.state, .running)
    }

    func testNewGroupWithOneCueSelectedIsAnEmptyGroupAfterIt() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.intro])
        let added = await editor.add(.group)
        let new = try XCTUnwrap(added)
        await assertEventually { h.vc.selectedIDs() == [new] }
        XCTAssertEqual(shape(h.vc.roots, new: new), "0a N[] 0b 20[21 22 23]")
    }

    func testNewGroupRefusedByTheEngineLeavesTheListAndSaysWhy() async throws {
        let (h, editor, log) = try await harness()
        // The store holds a cue the engine does not have; grouping it is refused, whole.
        let ghost: ModelID = "00000000-0000-0000-0000-0000000000ee"
        try Self.phantom(h, ghost, index: 1)
        h.vc.select([H.intro, ghost])
        let new = await editor.add(.group)
        XCTAssertNil(new)
        XCTAssertEqual(log.notices.count, 1)
        XCTAssertTrue(log.notices[0].contains("does not exist"), log.notices[0])
        // The engine's list is as it was: nothing of the group was made.
        h.rig.client.requestSnapshot()
        await assertEventually { shape(h.vc.roots, new: nil) == "0a 0b 20[21 22 23]" }
    }

    /// The app refuses this itself (no engine is involved): an Audio cue needs a patch to play to.
    func testAddAudioWithoutAPatchIsRefusedWithAMessage() async throws {
        var snapshot = try H.demoSnapshot()
        snapshot["workspace"]?.set("patches", .array([]))
        let h = try await H(snapshot: snapshot)
        let editor = CueEditor(store: h.store, cueList: h.vc)
        var notices: [String] = []
        editor.notify = { notices.append($0) }
        let id = await editor.add(.audio)
        XCTAssertNil(id)
        XCTAssertEqual(notices, [loc("edit.error.noPatch")], "refused before anything is sent")
    }

    func testAddNetworkCueUsesTheFirstNetworkPatchAndIsSelected() async throws {
        let (h, editor, log) = try await harness()
        let desk = try XCTUnwrap(h.store.model?.networkPatches.first)
        let added = await editor.add(.network)
        let id = try XCTUnwrap(added)
        await assertEventually { h.vc.selectedIDs() == [id] }
        let kind = try XCTUnwrap(h.vc.node(id)?.cue.raw["kind"])
        XCTAssertEqual(kind["type"]?.stringValue, "network")
        XCTAssertEqual(kind["destination"]?.stringValue, desk.id.raw)
        XCTAssertEqual(kind["address"]?.stringValue, "")
        XCTAssertEqual(kind["args"], .array([]))
        XCTAssertEqual(h.store.model?.networkPatches.count, 1, "an existing destination needs no new patch")
        XCTAssertTrue(log.notices.isEmpty)
    }

    func testAddNetworkCueWithNoNetworkPatchAddsADefaultOneInTheSameStep() async throws {
        let (h, editor, log) = try await harness("Second")
        // The empty show's one destination is removed first.
        let desk = try XCTUnwrap(h.store.model?.networkPatches.first)
        let removed = await h.store.send(
            command: Command(.object(["type": .string("remove_network_patch"), "id": .string(desk.id.raw)])))
        guard case .success = removed else { return XCTFail("\(removed)") }
        await assertEventually { h.store.model?.networkPatches.isEmpty == true }
        let added = await editor.add(.network)
        let id = try XCTUnwrap(added)
        await assertEventually { h.vc.node(id) != nil }
        XCTAssertTrue(log.notices.isEmpty)
        let patch = try XCTUnwrap(h.store.model?.networkPatches.first)
        XCTAssertEqual(h.vc.node(id)?.cue.raw["kind"]?["destination"]?.stringValue, patch.id.raw)
        // What the engine stored is the engine's golden message, which its tests play: all but the
        // app's random ids.
        let golden = try XCTUnwrap(try Fixtures.json("client", "command_add_new_network_cue")["command"])
        let want = try XCTUnwrap(golden["commands"]?.arrayValue)
        var stored = patch.raw
        stored.set("id", try XCTUnwrap(want[0]["patch"]?["id"]))
        XCTAssertEqual(stored, want[0]["patch"])
        XCTAssertEqual(patch.raw["name"]?.stringValue, loc("edit.networkPatch.defaultName"))
        var kind = try XCTUnwrap(h.vc.node(id)?.cue.raw["kind"])
        kind.set("destination", try XCTUnwrap(want[0]["patch"]?["id"]))
        XCTAssertEqual(kind, want[1]["cue"]?["kind"])
        // One command, so one undo step: undo takes the cue and the patch back.
        await editor.undo()
        await assertEventually { h.vc.node(id) == nil && h.store.model?.networkPatches.isEmpty == true }
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
        let before = shape(h.vc.roots, new: nil)
        h.vc.select([H.group, H.door, H.fade])
        await editor.deleteSelection()
        await assertEventually { shape(h.vc.roots, new: nil) == "0a" }
        XCTAssertTrue(log.notices.isEmpty, "the child of the deleted group is not deleted again (refused)")
        // One transaction: one undo brings all of it back.
        await editor.undo()
        await assertEventually { shape(h.vc.roots, new: nil) == before }
    }

    func testDeleteOfASingleCueIsPlainRemoveAndSelectionMovesOn() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.fade])
        await editor.deleteSelection()
        await assertEventually { h.vc.node(H.fade) == nil && h.vc.selectedIDs() == [H.group] }
    }

    func testDeleteRefusedForRunningCuePausedCueAndGroupWithARunningChild() async throws {
        for state in [WireCueState.running, .paused, .stopping, .fading, .preWaiting] {
            let (h, editor, log) = try await harness()
            switch state {
            case .paused:
                try await Self.start(h, H.rain)
                _ = await h.store.transport(.pause(cue: H.rain))
                try await h.rig.advance(0.05)
            case .fading:
                // The demo's Fade cue, aimed at Rain, moving its levels.
                _ = await h.store.send(command: .setProperty(H.fade, .target(H.rain)))
                try await Self.start(h, H.rain)
                _ = await h.store.transport(.start(cue: H.fade))
                try await h.rig.advance(0.05)
            case .stopping:
                try await Self.start(h, H.rain)
                _ = await h.store.transport(.stop(cue: H.rain, fadeOut: FadeSpec(duration: 5, curve: "linear")))
                try await h.rig.advance(0.05)
            case .preWaiting:
                _ = await h.store.send(command: .setProperty(H.rain, .preWait(5)))
                _ = await h.store.transport(.start(cue: H.rain))
                try await h.rig.advance(0.05)
            default:
                try await Self.start(h, H.rain)
            }
            await assertEventually("Rain is \(state)") { h.store.cueStates[H.rain]?.state == state }
            log.reset()
            h.vc.select([H.rain])
            await editor.deleteSelection()
            XCTAssertEqual(log.sent, 0, "\(state): nothing is sent")
            XCTAssertEqual(log.notices, [loc("edit.error.deleteBusy")], "\(state)")
            h.vc.select([H.group])
            await editor.deleteSelection()
            XCTAssertEqual(log.sent, 0, "group with a \(state) child")
            XCTAssertEqual(log.notices.count, 2)
            XCTAssertNotNil(h.vc.node(H.rain))
        }
    }

    func testDeleteRefusedWhilePreviewing() async throws {
        let (h, editor, log) = try await harness()
        _ = await h.store.transport(.preview(cue: H.intro))
        try await h.rig.advance(0.05)
        await assertEventually { h.store.previewing.contains(H.intro) }
        log.reset()
        h.vc.select([H.intro])
        await editor.deleteSelection()
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(log.notices.count, 1)
        XCTAssertNotNil(h.vc.node(H.intro))
    }

    func testDeleteOfIdleAndLoadedCuesIsAllowed() async throws {
        let (h, editor, log) = try await harness()
        _ = await h.store.transport(.load(cue: H.door, at: 0))
        try await h.rig.advance(0.05)
        await assertEventually { h.store.cueStates[H.door]?.state == .loaded }
        h.vc.select([H.door])
        await editor.deleteSelection()
        await assertEventually { h.vc.node(H.door) == nil }
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
        let before = shape(h.vc.roots, new: nil)
        await editor.move([H.door, H.rain], to: nil, at: 0)
        await assertEventually { shape(h.vc.roots, new: nil) == "21 22 0a 0b 20[23]" }
        await editor.undo()
        await assertEventually { shape(h.vc.roots, new: nil) == before }
        // Dropping the group into itself is refused before anything is sent.
        log.reset()
        await editor.move([H.group], to: H.radio, at: 0)
        await editor.move([H.group], to: H.group, at: 0)
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(shape(h.vc.roots, new: nil), before)
    }

    func testMoveSelectionByOneKeepsBlockedCuesInPlace() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door, H.rain])
        await editor.moveSelection(by: -1)  // Door is first already: only Rain stays behind it
        XCTAssertEqual(log.sent, 0, "a selection already at the top does not move")
        h.vc.select([H.rain, H.radio])
        await editor.moveSelection(by: -1)
        await assertEventually { h.vc.node(H.group)?.children.map(\.id) == [H.rain, H.radio, H.door] }
    }

    // MARK: Renumber, undo, redo

    func testRenumberSendsTheSelectionAndRefusesBadValues() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro, H.fade])
        await editor.renumberSelection(start: 10, increment: 5)
        await assertEventually { h.vc.node(H.intro)?.cue.number == "10" && h.vc.node(H.fade)?.cue.number == "15" }
        log.reset()
        for (s, i) in [(-1.0, 1.0), (1.0, 0.0), (1.0, -1.0), (.nan, 1.0), (1.0, .infinity)] {
            await editor.renumberSelection(start: s, increment: i)
        }
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(log.notices.count, 5)
        XCTAssertEqual(h.vc.node(H.intro)?.cue.number, "10")
    }

    func testUndoAndRedoAreEngineMessagesAndFollowDocumentState() async throws {
        let (h, editor, _) = try await harness()
        XCTAssertEqual(editor.canUndo, h.store.document?.canUndo, "enabled from the engine's document state")
        XCTAssertEqual(editor.canRedo, h.store.document?.canRedo)
        XCTAssertFalse(editor.canUndo, "a show just opened has nothing to undo")
        h.vc.select([H.fade])
        await editor.deleteSelection()
        await assertEventually { editor.canUndo && h.vc.node(H.fade) == nil }
        XCTAssertFalse(editor.canRedo)
        await editor.undo()
        await assertEventually { editor.canRedo && h.vc.node(H.fade) != nil }
        XCTAssertFalse(editor.canUndo)
        await editor.redo()
        await assertEventually { h.vc.node(H.fade) == nil && editor.canUndo && !editor.canRedo }
    }

    func testAnEngineRefusalIsShown() async throws {
        let (h, editor, log) = try await harness()
        let ghost: ModelID = "00000000-0000-0000-0000-0000000000ee"
        try Self.phantom(h, ghost, index: 1)
        h.vc.select([ghost])
        await editor.deleteSelection()
        XCTAssertEqual(log.notices.count, 1)
        XCTAssertTrue(log.notices[0].contains("does not exist"), log.notices[0])
    }

    // MARK: Show Mode

    func testShowModeBlocksEveryEditPath() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        let ids = wc.cueList.roots.map(\.id)
        let first = try XCTUnwrap(ids.first)
        wc.cueList.select([first])
        wc.setShowMode(true)
        await assertEventually { rig.store.document?.showMode == true }
        let log = SendLog(rig)
        let revision = rig.store.revision
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
        XCTAssertEqual(log.sent, 0, "nothing reached the engine")
        await rig.sync()
        XCTAssertEqual(rig.store.revision, revision, "and the engine changed nothing")
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
