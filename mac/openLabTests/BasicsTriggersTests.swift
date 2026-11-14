// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The rules behind the Basics and Triggers additions, without an engine: the shown colour, the row
/// patterns, the walk past skipped cues, the duck level's text, the trigger switch and Paste Properties.
@MainActor
final class BasicsTriggersRuleTests: XCTestCase {
    private func memo(_ id: String, _ extra: [String: JSONValue] = [:], children: [JSONValue]? = nil) -> JSONValue {
        var raw = TestSnapshots.cueJSON(
            id: id, number: nil, name: id,
            kind: children.map { TestSnapshots.groupKind($0) } ?? .object(["type": .string("memo")]))
        for (k, v) in extra { raw.set(k, v) }
        return raw
    }

    private func model(_ cues: [JSONValue]) throws -> WorkspaceModel {
        WorkspaceModel(
            id: "w", name: "w", settings: .object([:]), patches: [], networkPatches: [],
            cueLists: [CueList(id: "l", name: "Main", cues: try cues.map { try Cue(json: $0) })])
    }

    private static let skipped: [String: JSONValue] = ["armed": .bool(false), "skip_if_disarmed": .bool(true)]

    func testTheSecondColourShowsOnlyOncePlayedAndAnUnknownColourIsNone() throws {
        let cue = try Cue(json: memo("a", ["color": .string("red"), "second_color": .string("light_blue")]))
        XCTAssertEqual(CueKindStyle.shownColour(of: cue, played: false), "red")
        XCTAssertEqual(CueKindStyle.shownColour(of: cue, played: true), "light_blue")
        let none = try Cue(json: memo("b", ["color": .string("red")]))
        XCTAssertEqual(CueKindStyle.shownColour(of: none, played: true), "red", "no second colour: the first stays")
        let odd = try Cue(json: memo("c", ["color": .string("chartreuse"), "second_color": .string("teal")]))
        XCTAssertNil(CueKindStyle.colours[CueKindStyle.shownColour(of: odd, played: true)], "drawn as none")
        XCTAssertEqual(CueKindStyle.basicColours.count, 8)
        XCTAssertEqual(CueKindStyle.extendedColours.count, 12)
        for key in InspectorViewController.colours where key != "none" {
            XCTAssertNotNil(CueKindStyle.colours[key], key)
            XCTAssertNotEqual(loc("cue.colour.\(key)"), "cue.colour.\(key)", "\(key) has a name")
        }
    }

    func testADisarmedRowIsStripedAndASkippedOneHasItsOwnPatternAndTheBoxDoesNothingArmed() throws {
        XCTAssertNil(RowPattern.of(try Cue(json: memo("a"))))
        XCTAssertNil(RowPattern.of(try Cue(json: memo("b", ["skip_if_disarmed": .bool(true)]))), "armed: no effect")
        XCTAssertEqual(RowPattern.of(try Cue(json: memo("c", ["armed": .bool(false)]))), .disarmed)
        XCTAssertEqual(RowPattern.of(try Cue(json: memo("d", Self.skipped))), .skipped)
    }

    /// The same walk as the engine's: along the level, then out of the group, never into a skipped one.
    func testTheCueStandingByPastASkippedCueIsTheNextOneThereOutwardsThroughGroups() throws {
        let m = try model([
            memo("a"),
            memo("g", children: [memo("g1"), memo("g2", Self.skipped), memo("g3", Self.skipped)]),
            memo("b", Self.skipped), memo("c"), memo("z", Self.skipped),
        ])
        XCTAssertEqual(m.firstPresent(from: "a")?.id, "a", "a cue that is there is itself")
        XCTAssertEqual(m.firstPresent(from: "g2")?.id, "c", "out of the group, past the skipped b")
        XCTAssertEqual(m.firstPresent(from: "b")?.id, "c")
        XCTAssertNil(m.firstPresent(from: "z"), "nothing left: nothing stands by")
        XCTAssertNil(m.firstPresent(from: "gone"))
    }

    func testTheDuckLevelReadsSignedNumbersAndInfAndHoldsTheRange() {
        XCTAssertEqual(OthersLevel.parse("-12"), -12)
        XCTAssertEqual(OthersLevel.parse("6"), 6, "a number without a sign boosts")
        XCTAssertEqual(OthersLevel.parse("+6 dB"), 6)
        XCTAssertEqual(OthersLevel.parse("-3,5"), -3.5)
        XCTAssertEqual(OthersLevel.parse("-inf"), LevelText.silence)
        XCTAssertEqual(OthersLevel.parse("40"), 12, "held at the most a boost can be")
        XCTAssertEqual(OthersLevel.parse("-500"), LevelText.silence)
        XCTAssertNil(OthersLevel.parse("loud"))
        XCTAssertNil(OthersLevel.parse("nan"))
        XCTAssertEqual(OthersLevel.format(-12), "-12")
        XCTAssertEqual(OthersLevel.format(-120), "-inf")
        XCTAssertEqual(OthersLevel.format(2.5), "2.5")
        XCTAssertTrue(OthersLevel.isBoost("+1"))
        XCTAssertFalse(OthersLevel.isBoost("0"))
        XCTAssertFalse(OthersLevel.isBoost("x"), "unreadable text keeps the duck wording")
    }

    /// A typed value of a fade-out or duck keeps the rest of that cue's own setting, and a bad
    /// value is refused before it is sent.
    func testATypedOthersValueKeepsTheCuesOwnSettingAndBadTextIsRefused() throws {
        let cue = try Cue(
            json: memo(
                "a",
                [
                    "fade_and_stop_others": .object([
                        "enabled": .bool(false), "scope": .string("all"), "duration": .double(5),
                    ]),
                    "duck_others": .object(["enabled": .bool(true), "db": .double(-6), "duration": .double(2)]),
                ]))
        let fade = InspectorEdit.othersCommand(.fadeOthersTime, text: "3", cue: cue)
        XCTAssertEqual(
            try fade.get()?.json["property"],
            CueProperty.fadeAndStopOthers(enabled: false, scope: "all", duration: 3).json)
        let db = InspectorEdit.othersCommand(.duckDb, text: "+4", cue: cue)
        XCTAssertEqual(try db.get()?.json["property"], CueProperty.duckOthers(enabled: true, db: 4, duration: 2).json)
        XCTAssertEqual(try InspectorEdit.othersCommand(.duckTime, text: "2", cue: cue).get(), nil, "unchanged")
        XCTAssertThrowsError(try InspectorEdit.othersCommand(.duckDb, text: "loud", cue: cue).get())
        XCTAssertThrowsError(try InspectorEdit.othersCommand(.duckTime, text: "25:00:00", cue: cue).get())
        // A show without the keys has the defaults: off, peers, 1 s, -12 dB.
        let bare = try Cue(json: memo("b"))
        XCTAssertEqual(InspectorEdit.fadeOthers(bare).scope, "peers")
        XCTAssertEqual(InspectorEdit.duckOthers(bare).db, -12)
    }

    /// A switched-off hotkey takes no key press and clashes with no cue, but the key map may still
    /// not take it (the engine refuses a cue's key there either way).
    func testASwitchedOffHotkeyIsNotACueKeyButStillACueKeyForTheKeyMap() throws {
        let k = KeyChord("k", []).hotkeyJSON ?? .null
        let m = try model([
            memo("on", ["triggers": .object(["hotkey": k, "hotkey_enabled": .bool(true)])]),
            memo("off", ["triggers": .object(["hotkey": k, "hotkey_enabled": .bool(false)])]),
            memo("old", ["triggers": .object(["hotkey": KeyChord("j", []).hotkeyJSON ?? .null])]),
        ])
        XCTAssertEqual(m.cueHotkeys.map(\.cue), ["on", "old"], "a show from before the switch has it on")
        XCTAssertEqual(m.storedCueHotkeys.map(\.cue), ["on", "off", "old"])
    }

    func testPastePropertiesCarriesTheNewBasicsAndTriggers() throws {
        let source = try Cue(
            json: memo(
                "s",
                [
                    "second_color": .string("pink"), "auto_load": .bool(true), "skip_if_disarmed": .bool(true),
                    "fade_and_stop_others": .object([
                        "enabled": .bool(true), "scope": .string("list"), "duration": .double(4),
                    ]),
                    "duck_others": .object(["enabled": .bool(true), "db": .double(3), "duration": .double(0.5)]),
                ]))
        let target = try Cue(json: memo("t"))
        let offered = PasteProperty.offered(from: source, to: [target])
        XCTAssertTrue(offered[0].properties.contains(.secondColour))
        XCTAssertTrue(offered[0].properties.contains(.autoLoad))
        XCTAssertTrue(offered[0].properties.contains(.skipIfDisarmed))
        XCTAssertEqual(offered[1].category, .triggers)
        XCTAssertTrue(offered[1].properties.contains(.fadeAndStopOthers))
        XCTAssertTrue(offered[1].properties.contains(.duckOthers))
        let sent = PasteProperty.commands(
            [.secondColour, .autoLoad, .skipIfDisarmed, .fadeAndStopOthers, .duckOthers], from: source, to: [target])
        XCTAssertEqual(
            sent.compactMap { $0.json["property"] },
            [
                CueProperty.secondColor("pink").json, CueProperty.autoLoad(true).json,
                CueProperty.skipIfDisarmed(true).json,
                CueProperty.fadeAndStopOthers(enabled: true, scope: "list", duration: 4).json,
                CueProperty.duckOthers(enabled: true, db: 3, duration: 0.5).json,
            ])
        for p in [PasteProperty.secondColour, .autoLoad, .skipIfDisarmed, .fadeAndStopOthers, .duckOthers] {
            XCTAssertNotEqual(loc(p.titleKey), p.titleKey)
        }
    }
}

/// The Basics and Triggers flows of #633, each done as the operator does them in a workspace window
/// on the real engine; what is checked is what the window draws (rows, marks, the masthead, the
/// controls) and what is heard (a cue's meter). Nothing here needs the window to be frontmost: clicks
/// go to the controls, keys to the app's key handling.
extension PlayheadFlowTests {
    private var inspector: InspectorViewController { wc.inspector }

    // MARK: Doing what the operator does

    private func memos(_ count: Int) async throws -> [ModelID] {
        var ids: [ModelID] = []
        for _ in 0..<count {
            let made = await wc.editor.add(.memo, after: ids.last.map { [$0] })
            ids.append(try XCTUnwrap(made, "memo added"))
        }
        await assertEventually("the rows are drawn") { ids.allSatisfy { self.wc.cueList.node($0) != nil } }
        return ids
    }

    /// Audio cues playing `seconds` of a tone, added after the last row.
    private func tones(_ count: Int, seconds: Double = 30, parent: ModelID? = nil) async throws -> [ModelID] {
        let dir = try XCTUnwrap(try tempFiles(["x.txt"]).first).deletingLastPathComponent()
        var urls: [URL] = []
        for i in 0..<count {
            let url = dir.appendingPathComponent("tone-\(UUID().uuidString)-\(i).wav")
            try writeWAV(url, seconds: seconds)
            urls.append(url)
        }
        let index = parent.flatMap { store.model?.cue($0)?.children.count } ?? (wc.cueList.roots.count)
        let ids = await wc.editor.addFiles(urls, parent: parent, index: index)
        XCTAssertEqual(ids.count, count)
        // The probes are reported as device time moves.
        await assertEventually("probed") {
            try? await self.play(0.01)
            return ids.allSatisfy { self.store.mediaDurations[$0]?.file != nil }
        }
        return ids
    }

    private func select(_ ids: [ModelID]) async {
        wc.cueList.selectAsUser(ids)
        await assertEventually("the inspector shows them") { self.inspector.selection == ids }
    }

    private func tick(_ check: NSButton, _ on: Bool) {
        XCTAssertTrue(check.isEnabled, "the box can be clicked")
        if (check.state == .on) != on { check.performClick(nil) }
    }

    private func pick(_ popup: NSPopUpButton, _ key: String) throws {
        let index = try XCTUnwrap(popup.itemArray.firstIndex { ($0.representedObject as? String) == key })
        popup.selectItem(at: index)
        XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
    }

    /// Types `text` in `field` through its field editor and presses Return (`enter`).
    private func typeIn(_ field: NSTextField, _ text: String, enter: Bool = true) throws {
        XCTAssertTrue(field.isEnabled)
        XCTAssertFalse(field.isHiddenOrHasHiddenAncestor || field.window == nil, "the field is on screen")
        // A field still being typed in keeps its editor; making it first responder again would end that.
        if field.currentEditor() == nil { XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(field)) }
        // A click selects the field's text, which gives it the field editor (as a click on it does).
        if field.currentEditor() == nil { field.selectText(nil) }
        let editor = try XCTUnwrap(field.currentEditor() as? NSTextView)
        editor.selectAll(nil)
        editor.insertText(text, replacementRange: editor.selectedRange())
        if enter { editor.doCommand(by: #selector(NSResponder.insertNewline(_:))) }
    }

    /// Space as the key monitor takes it, then a tenth of a second.
    private func go() async throws {
        let before = goes()
        XCTAssertNil(app.handleKeyDown(try key(" ", code: 49)), "Space is GO")
        await assertEventually("GO was sent") { self.goes() == before + 1 }
        await sync()
        try await play(0.1)
    }

    private func raw(_ id: ModelID, _ key: String) -> JSONValue? { store.model?.cue(id)?.raw[key] }

    // MARK: What the window shows

    private func rowView(_ id: ModelID) throws -> CueRowView {
        let ov = wc.cueList.outlineView
        let row = ov.row(forItem: try XCTUnwrap(wc.cueList.node(id)))
        return try XCTUnwrap(ov.rowView(atRow: row, makeIfNecessary: true) as? CueRowView)
    }

    private func tint(_ id: ModelID) throws -> NSColor? { try rowView(id).tint }

    private func colour(_ key: String) -> NSColor? { CueKindStyle.colours[key]?.tint }

    /// The label of status image `index` (0: playhead, 1: state) drawn in `id`'s row.
    private func mark(_ id: ModelID, _ index: Int) -> String? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier("status"))
        guard let cell = ov.view(atColumn: col, row: ov.row(forItem: node), makeIfNecessary: true) as? CueCell,
            !cell.images[index].isHidden
        else { return nil }
        return cell.images[index].accessibilityLabel()
    }

    private func state(_ id: ModelID) -> String? { mark(id, 1) }

    /// The loudest level of `id`'s first channel over three tenths of a second, read from the meter
    /// its Levels tab draws (meters are kept only for a meter on screen). The cues and the tab shown
    /// before are shown again after.
    private func peak(_ id: ModelID) async throws -> Float {
        let shown = inspector.selection
        let tab = inspector.tabView.selectedTabViewItem?.identifier as? String
        await select([id])
        inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        store.meters.reset()
        var loudest: Float = 0
        for _ in 0..<3 {
            try await play(0.1)
            store.meters.tick(0)
            loudest = max(loudest, store.meters.level(.cue(id), channel: 0))
        }
        if !shown.isEmpty, shown != [id] {
            await select(shown)
            if let tab { inspector.tabView.selectTabViewItem(withIdentifier: tab) }
        }
        return loudest
    }

    // MARK: Flows

    func testAutoLoadIsTickedOnCue2GoOnCue1ShowsCue2Loaded() async throws {
        let cues = try await tones(3)
        await select([cues[1]])
        tick(inspector.autoLoadCheck, true)
        await assertEventually("cue 2 auto-loads") { self.raw(cues[1], "auto_load") == .bool(true) }
        click(cues[0])
        await assertEventually { self.store.playheads[self.list] == cues[0] }
        XCTAssertNil(state(cues[1]), "not loaded before")
        try await go()
        await assertEventually("cue 2's row shows the loaded mark") { self.state(cues[1]) == loc("cue.state.loaded") }
        XCTAssertEqual(state(cues[0]), loc("cue.state.running"))
        XCTAssertNil(state(cues[2]), "cue 3 is not auto-loaded")
    }

    func testWorkspaceSettingsNewCuesAutoLoadTicksANewCueAndLeavesExistingCues() async throws {
        let before = try await memos(1)
        let settings = wc.settingsWindow
        await wc.showSettings().value
        await assertEventually("the box shows the setting") { settings.autoLoadCheck.isEnabled }
        XCTAssertEqual(settings.autoLoadCheck.state, .off)
        settings.autoLoadCheck.performClick(nil)
        await assertEventually("the workspace takes it") {
            self.store.model?.settings["new_cues_auto_load"] == .bool(true)
        }
        XCTAssertEqual(settings.autoLoadCheck.state, .on)
        let made = await wc.editor.add(.memo, after: before)
        let new = try XCTUnwrap(made)
        await select([new])
        await assertEventually("the new cue's box is ticked") { self.inspector.autoLoadCheck.state == .on }
        await select(before)
        XCTAssertEqual(inspector.autoLoadCheck.state, .off, "the cue made before keeps its own")
        settings.window?.close()
    }

    /// A, B (disarmed, skipped), C in an auto-continue chain: GO runs A then C; B's row has the skip
    /// pattern, a plain disarmed row the stripe.
    func testADisarmedCueWithSkipIfDisarmedInAnAutoContinueChainIsPassedOverAndHasItsOwnPattern() async throws {
        let cues = try await tones(4, seconds: 5)
        guard case .success = await store.send(command: .setProperty(cues[0], .continueMode("auto_continue"))),
            case .success = await store.send(command: .setProperty(cues[1], .continueMode("auto_continue")))
        else { return XCTFail("chain") }
        await select([cues[1]])
        tick(inspector.armedCheck, false)
        tick(inspector.skipCheck, true)
        await select([cues[3]])
        tick(inspector.armedCheck, false)
        await assertEventually("B is skipped") {
            self.raw(cues[1], "skip_if_disarmed") == .bool(true) && self.raw(cues[3], "armed") == .bool(false)
        }
        XCTAssertEqual(try rowView(cues[1]).pattern, .skipped)
        XCTAssertEqual(try rowView(cues[3]).pattern, .disarmed, "a disarmed row is striped, not skipped")
        XCTAssertNil(try rowView(cues[0]).pattern)
        click(cues[0])
        await assertEventually { self.store.playheads[self.list] == cues[0] }
        try await go()
        try await play(0.2)
        await assertEventually("A and C run") {
            self.state(cues[0]) == loc("cue.state.running") && self.state(cues[2]) == loc("cue.state.running")
        }
        XCTAssertNil(state(cues[1]), "B does nothing at all")
        XCTAssertFalse(store.played.contains(cues[1]), "a skipped cue is never started")
    }

    func testTheSameCueArmedTheSkipBoxHasNoEffect() async throws {
        let cues = try await tones(3, seconds: 5)
        guard case .success = await store.send(command: .setProperty(cues[0], .continueMode("auto_continue")))
        else { return XCTFail("chain") }
        await select([cues[1]])
        tick(inspector.skipCheck, true)
        await assertEventually { self.raw(cues[1], "skip_if_disarmed") == .bool(true) }
        XCTAssertNil(try rowView(cues[1]).pattern, "armed: drawn as any armed cue")
        click(cues[0])
        await assertEventually { self.store.playheads[self.list] == cues[0] }
        try await go()
        await assertEventually("A's continue starts B") { self.state(cues[1]) == loc("cue.state.running") }
        XCTAssertNil(state(cues[2]))
    }

    func testTheColourControlOffersTheTwentyColoursAsSwatchesAndAPickTintsTheRow() async throws {
        let cues = try await memos(1)
        await select(cues)
        let items = inspector.colourPopup.itemArray.filter { !$0.isHidden }
        let keys = items.map { $0.representedObject as? String }
        XCTAssertEqual(keys.first, "none")
        let groups = keys.dropFirst().split(separator: nil).map { $0.compactMap { $0 } }
        XCTAssertEqual(groups, [CueKindStyle.basicColours, CueKindStyle.extendedColours], "eight basic, twelve more")
        XCTAssertTrue(items.filter { !$0.isSeparatorItem }.allSatisfy { $0.image != nil }, "each has its swatch")
        XCTAssertNil(try tint(cues[0]))
        for key in ["magenta", "navy"] {
            try pick(inspector.colourPopup, key)
            await assertEventually("the row takes \(key)") { (try? self.tint(cues[0])) == self.colour(key) }
            XCTAssertEqual(inspector.colourPopup.selectedItem?.representedObject as? String, key)
        }
    }

    /// Red with a second colour green: the row turns green once the cue has been started, red again
    /// after Reset All, after a Reset cue on it, and after the show is closed and opened again.
    func testASecondColourIsUnlockedAndPickedTheRowShowsItAfterTheFirstStartAndResetsBringBackTheFirst()
        async throws
    {
        let cues = try await memos(2)
        let cue = cues[0]
        await select([cue])
        try pick(inspector.colourPopup, "red")
        XCTAssertFalse(inspector.secondColourPopup.isEnabled, "locked: no second colour")
        tick(inspector.secondColourCheck, true)
        XCTAssertTrue(inspector.secondColourPopup.isEnabled, "unlocked")
        try pick(inspector.secondColourPopup, "green")
        await assertEventually { self.raw(cue, "second_color")?.stringValue == "green" }
        await select([cues[1]])  // the colour is drawn on a row that is not highlighted
        XCTAssertEqual(try tint(cue), colour("red"), "not started yet")

        func start() async throws {
            click(cue)
            await assertEventually { self.store.playheads[self.list] == cue }
            try await go()
            await select([cues[1]])
            await assertEventually("started: the second colour") { (try? self.tint(cue)) == self.colour("green") }
        }
        try await start()
        guard case .success = await store.transport(.resetAll) else { return XCTFail("reset all") }
        await assertEventually("Reset All: the first colour") { (try? self.tint(cue)) == self.colour("red") }

        try await start()
        await select([cue])
        let made = await wc.editor.add(.reset, after: [cue])
        let reset = try XCTUnwrap(made)
        await assertEventually { self.store.model?.cue(reset)?.raw["kind"]?["target"]?.stringValue == cue.raw }
        guard case .success = await store.transport(.start(cue: reset)) else { return XCTFail("reset cue") }
        try await play(0.1)
        await select([cues[1]])
        await assertEventually("a Reset cue on it: the first colour") { (try? self.tint(cue)) == self.colour("red") }

        try await start()
        let path = engine.folder.appendingPathComponent("second-colour-\(UUID().uuidString).openlab")
        guard case .success = await store.client.send({ .request(id: $0, .saveAs(path: path.path)) }) else {
            return XCTFail("save")
        }
        // Close and open again as File > Close and Open do; the file has just been saved, so a save
        // prompt (never a modal here) is answered Don't Save.
        let alerts = ScriptedAlerts()
        alerts.saveChoices = [.dontSave]
        app.alerts = alerts
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed)
        XCTAssertNil(app.windows[doc])
        await app.openWorkspace(url: path)
        XCTAssertEqual(alerts.errors, [])
        let reopened = try XCTUnwrap(app.windows.keys.first, "the show is open again")
        doc = reopened
        await assertEventually("the reopened list is drawn") { self.wc.cueList.node(cue) != nil }
        XCTAssertEqual(try tint(cue), colour("red"), "reopened: the first colour")
    }

    func testEachTriggerHasAnEnableBoxAnUntickedHotkeyKeepsItsKeyAndDoesNotFire() async throws {
        let cues = try await tones(3, seconds: 5)
        // Ticked first, then the key typed (QLab 5, manual p. 128): the key fires.
        await select([cues[2]])
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        XCTAssertTrue(inspector.hotkeyEnableCheck.isEnabled, "usable before there is a key")
        tick(inspector.hotkeyEnableCheck, false)
        await assertEventually { self.raw(cues[2], "triggers")?["hotkey_enabled"] == .bool(false) }
        tick(inspector.hotkeyEnableCheck, true)
        await assertEventually { self.raw(cues[2], "triggers")?["hotkey_enabled"] == .bool(true) }
        XCTAssertTrue(inspector.record(KeyChord("j", [])))
        await assertEventually("J is set") { self.store.model?.cueHotkeys.map(\.cue) == [cues[2]] }
        XCTAssertEqual(inspector.hotkeyEnableCheck.state, .on)
        XCTAssertNil(app.handleKeyDown(try key("j", code: 38)), "J is a cue key")
        try await play(0.2)
        await assertEventually("J started the cue") { self.state(cues[2]) == loc("cue.state.running") }
        // Unticked first, then the key typed: kept, faint, and it does not fire.
        await select([cues[0]])
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        XCTAssertEqual(inspector.hotkeyEnableCheck.state, .on, "a new cue's trigger is on")
        tick(inspector.hotkeyEnableCheck, false)
        await assertEventually("switched off") { self.raw(cues[0], "triggers")?["hotkey_enabled"] == .bool(false) }
        XCTAssertTrue(inspector.record(KeyChord("k", [])))
        await assertEventually("K is kept") { self.raw(cues[0], "triggers")?["hotkey"] != .null }
        XCTAssertEqual(raw(cues[0], "triggers")?["hotkey_enabled"], .bool(false), "the key keeps the box's switch")
        XCTAssertEqual(inspector.hotkeyField.stringValue, "K", "the key is kept and shown")
        XCTAssertEqual(inspector.hotkeyField.textColor, .disabledControlTextColor, "drawn faint")
        XCTAssertEqual(inspector.hotkeyEnableCheck.state, .off)
        // K is not a cue key now: the app does not take it and nothing starts.
        let sentBefore = wire.sent.count
        XCTAssertNotNil(app.handleKeyDown(try key("k", code: 40)), "the key is not taken")
        try await play(0.2)
        XCTAssertFalse(wire.sent.dropFirst(sentBefore).contains { if case .hotkey = $0 { true } else { false } })
        XCTAssertNil(state(cues[0]), "the cue did not start")
        // Nor does it clash: another cue may take K, and then the first cannot be switched back on.
        await select([cues[1]])
        XCTAssertTrue(inspector.record(KeyChord("k", [])), "no clash with a key that is off")
        await assertEventually { self.store.model?.cueHotkeys.map(\.cue) == [cues[1], cues[2]] }
        await select([cues[0]])
        inspector.hotkeyEnableCheck.performClick(nil)
        XCTAssertEqual(inspector.messageLabel.stringValue, loc("inspector.hotkey.otherCue"))
        XCTAssertEqual(inspector.hotkeyEnableCheck.state, .off, "the box is put back")
        await sync()
        XCTAssertEqual(raw(cues[0], "triggers")?["hotkey_enabled"], .bool(false))
    }

    /// A and C in one group, B in another, A and B playing; C with "fade and stop others" over 2 s on
    /// its peers: GO on C fades A out and stops it, and B plays on. With "its list" B goes too.
    func testFadeAndStopOthersTickedWith2SAndPeersFadesTheOtherCuesAtItsLevelAndCuesInOtherGroupsPlayOn()
        async throws
    {
        var groups: [ModelID] = []
        for _ in 0..<2 {
            // Nothing selected: a new group made with cues selected would wrap them.
            let made = await wc.editor.add(.group, after: [])
            groups.append(try XCTUnwrap(made))
        }
        await assertEventually { groups.allSatisfy { self.wc.cueList.node($0) != nil } }
        let ac = try await tones(2, parent: groups[0])
        let inner = try await tones(1, parent: groups[1])
        for g in groups { wc.cueList.outlineView.expandItem(wc.cueList.node(g)) }  // their rows are drawn
        let (a, b, c) = (ac[0], inner[0], ac[1])
        for id in [a, b] {
            guard case .success = await store.transport(.start(cue: id)) else { return XCTFail("start") }
        }
        try await play(0.3)
        await assertEventually("A and B play") {
            self.state(a) == loc("cue.state.running") && self.state(b) == loc("cue.state.running")
        }
        await select([c])
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        XCTAssertFalse(inspector.fadeOthersTimeField.isEnabled, "off: its time is drawn off")
        tick(inspector.fadeOthersCheck, true)
        await assertEventually { InspectorEdit.fadeOthers(self.store.model!.cue(c)!).enabled }
        try typeIn(inspector.fadeOthersTimeField, "2")
        await assertEventually("2 s, peers") {
            let f = InspectorEdit.fadeOthers(self.store.model!.cue(c)!)
            return f.duration == 2 && f.scope == "peers"
        }
        let loud = try await peak(a)
        XCTAssertGreaterThan(loud, 0.01, "A is heard")
        click(c)
        await assertEventually { self.store.playheads[self.list] == c }
        try await go()
        try await play(0.9)
        let halfway = try await peak(a)
        XCTAssertLessThan(halfway, loud * 0.9, "A fades")
        XCTAssertGreaterThan(halfway, 0.0001, "over the 2 s, not at once")
        try await play(1.2)
        await assertEventually("A stopped") { self.state(a) == nil }
        XCTAssertEqual(state(b), loc("cue.state.running"), "B in the other group plays on")
        // Its list: B goes too.
        guard case .success = await store.transport(.stop(cue: c, fadeOut: nil)) else { return XCTFail("stop") }
        try await play(0.1)
        await select([c])  // GO moved the selection on
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        choose(inspector.fadeOthersScopePopup, at: 1)
        await assertEventually { InspectorEdit.fadeOthers(self.store.model!.cue(c)!).scope == "list" }
        guard case .success = await store.transport(.start(cue: c)) else { return XCTFail("start C") }
        try await play(2.3)
        await assertEventually("its list: B stops") { self.state(b) == nil }
        XCTAssertEqual(state(c), loc("cue.state.running"), "never the cue itself")
    }

    private func choose(_ popup: NSPopUpButton, at index: Int) {
        popup.selectItem(at: index)
        XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
    }

    /// A plays; D ducks the others by -12 dB over 1 s: A is a quarter as loud while D runs and comes
    /// back when D ends. A positive number reads "boost" as soon as it is typed; with a pre-wait the
    /// duck waits for it.
    func testDuckOthersTickedWithMinus12DbOver1SLowersTheOtherAudioCuesWhileItRunsAndBoostIsLabelled()
        async throws
    {
        let cues = try await tones(1)
        let ducker = try await tones(1, seconds: 3)
        let (a, d) = (cues[0], ducker[0])
        guard case .success = await store.transport(.start(cue: a)) else { return XCTFail("start A") }
        try await play(0.3)
        let full = try await peak(a)
        XCTAssertGreaterThan(full, 0.01)
        await select([d])
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        XCTAssertEqual(inspector.duckDbField.stringValue, "-12", "the default")
        XCTAssertEqual(inspector.duckCheck.title, loc("inspector.duckOthers"))
        tick(inspector.duckCheck, true)
        await assertEventually { InspectorEdit.duckOthers(self.store.model!.cue(d)!).enabled }
        guard case .success = await store.transport(.start(cue: d)) else { return XCTFail("start D") }
        try await play(1.2)
        let ducked = try await peak(a)
        XCTAssertEqual(Double(ducked / full), 0.25, accuracy: 0.05, "12 dB down")
        try await play(3)  // D ends; the others come back over the same second
        await assertEventually("D ended") { self.state(d) == nil }
        try await play(1.2)
        let back = try await peak(a)
        XCTAssertEqual(Double(back / full), 1, accuracy: 0.05, "back to where it was")

        // A positive number: the label says boost while it is typed, and the boost is kept.
        try typeIn(inspector.duckDbField, "+6", enter: false)
        XCTAssertEqual(inspector.duckCheck.title, loc("inspector.boostOthers"), "relabelled before it is committed")
        try typeIn(inspector.duckDbField, "+6")
        await assertEventually { InspectorEdit.duckOthers(self.store.model!.cue(d)!).db == 6 }
        XCTAssertEqual(inspector.duckCheck.title, loc("inspector.boostOthers"))
        try typeIn(inspector.duckDbField, "-12")
        await assertEventually { InspectorEdit.duckOthers(self.store.model!.cue(d)!).db == -12 }
        XCTAssertEqual(inspector.duckCheck.title, loc("inspector.duckOthers"))

        // With a pre-wait the duck starts after it.
        inspector.tabView.selectTabViewItem(withIdentifier: "basics")
        try typeIn(inspector.preWaitField, "1")
        await assertEventually { self.raw(d, "pre_wait")?.doubleValue == 1 }
        guard case .success = await store.transport(.start(cue: d)) else { return XCTFail("start D again") }
        try await play(0.3)
        let waiting = try await peak(a)
        XCTAssertEqual(Double(waiting / full), 1, accuracy: 0.05, "nothing during the pre-wait")
        try await play(1.6)
        let after = try await peak(a)
        XCTAssertEqual(Double(after / full), 0.25, accuracy: 0.05, "ducked after the pre-wait")
    }

    func testPanicDuringADuckFadesEverythingAndNoDuckedLevelIsLeftOnTheNextRun() async throws {
        let cues = try await tones(1)
        let ducker = try await tones(1)
        let (a, d) = (cues[0], ducker[0])
        await select([d])
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        tick(inspector.duckCheck, true)
        await assertEventually { InspectorEdit.duckOthers(self.store.model!.cue(d)!).enabled }
        guard case .success = await store.transport(.start(cue: a)) else { return XCTFail("start A") }
        try await play(0.3)
        let full = try await peak(a)
        guard case .success = await store.transport(.start(cue: d)) else { return XCTFail("start D") }
        try await play(1.2)
        // Esc, as the key monitor takes it.
        _ = app.handleKeyDown(try key("\u{1B}", code: 53))
        await assertEventually("everything stops") {
            try? await self.play(0.5)
            return self.state(a) == nil && self.state(d) == nil
        }
        guard case .success = await store.transport(.start(cue: a)) else { return XCTFail("start A again") }
        try await play(0.3)
        let again = try await peak(a)
        XCTAssertEqual(Double(again / full), 1, accuracy: 0.05, "no duck left behind")
    }

    /// The Triggers tab holds the hotkey, its box and the others' controls: no OSC address field.
    func testThePerCueOscAddressFieldIsGoneFromTheTriggersTab() async throws {
        let cues = try await memos(1)
        await select(cues)
        let tab = try XCTUnwrap(inspector.tabItems["triggers"]?.view)
        func fields(_ v: NSView) -> [NSTextField] {
            ((v as? NSTextField).map { $0.isEditable || $0 is HotkeyRecorder ? [$0] : [] } ?? [])
                + v.subviews.flatMap(fields)
        }
        XCTAssertEqual(
            Set(fields(tab).map { $0.accessibilityIdentifier() }),
            [
                AX.inspectorHotkey, "inspector-fade-others-time", "inspector-duck-others-db",
                "inspector-duck-others-time",
            ])
    }

    /// The playhead on a skipped cue: its row keeps the playhead mark, the masthead names the next cue
    /// there, and GO starts that one.
    func testThePlayheadRestsOnASkippedCueTheMastheadShowsTheNextCueThereAndGoStartsIt() async throws {
        let cues = try await tones(3, seconds: 5)
        await select([cues[1]])
        tick(inspector.armedCheck, false)
        tick(inspector.skipCheck, true)
        await assertEventually { self.raw(cues[1], "skip_if_disarmed") == .bool(true) }
        click(cues[1])
        await assertEventually("the playhead is on the skipped cue") { self.store.playheads[self.list] == cues[1] }
        XCTAssertEqual(mark(cues[1], 0), loc("cue.playhead"), "the mark stays where the engine has it")
        let next = try XCTUnwrap(store.model?.cue(cues[2]))
        XCTAssertEqual(
            wc.masthead.standbyField.stringValue,
            [next.number, store.model?.listName(next)].compactMap { $0 }.joined(separator: " \u{00B7} "),
            "the masthead names the cue GO starts")
        try await go()
        await assertEventually("GO started the next cue there") { self.state(cues[2]) == loc("cue.state.running") }
        XCTAssertNil(state(cues[1]))
    }

    /// Several cues: a box sets each one, and a scope or time changes only that part of each cue's
    /// own setting.
    func testSeveralCuesTheBoxesAndOthersControlsSetEachCuesOwnValue() async throws {
        let cues = try await memos(2)
        for (id, time) in zip(cues, [1.0, 4.0]) {
            guard
                case .success = await store.send(
                    command: .setProperty(
                        id, .fadeAndStopOthers(enabled: id == cues[1], scope: "peers", duration: time)))
            else { return XCTFail("setup") }
        }
        await select(cues)
        XCTAssertEqual(inspector.fadeOthersCheck.state, .mixed)
        XCTAssertEqual(inspector.fadeOthersTimeField.placeholderString, loc("inspector.mixed"))
        XCTAssertEqual(inspector.autoLoadCheck.state, .off)
        tick(inspector.autoLoadCheck, true)
        tick(inspector.skipCheck, true)
        inspector.tabView.selectTabViewItem(withIdentifier: "triggers")
        choose(inspector.fadeOthersScopePopup, at: 2)
        await assertEventually("each cue took the box and the scope") {
            cues.allSatisfy {
                self.raw($0, "auto_load") == .bool(true) && self.raw($0, "skip_if_disarmed") == .bool(true)
                    && InspectorEdit.fadeOthers(self.store.model!.cue($0)!).scope == "all"
            }
        }
        let after = cues.compactMap { store.model?.cue($0) }.map(InspectorEdit.fadeOthers)
        XCTAssertEqual(after.map(\.duration), [1, 4], "each keeps its own time")
        XCTAssertEqual(after.map(\.enabled), [false, true], "and its own box")
        XCTAssertFalse(inspector.hotkeyEnableCheck.isEnabled, "a hotkey names one cue")
    }
}
