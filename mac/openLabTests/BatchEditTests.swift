// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The rules of Paste Properties and of a levels edit made on several cues, without an engine.
@MainActor
final class PastePropertiesRuleTests: XCTestCase {
    private func cue(_ id: String, _ kind: JSONValue, _ extra: [String: JSONValue] = [:]) throws -> Cue {
        var raw = TestSnapshots.cueJSON(id: id, number: nil, name: "", kind: kind)
        for (k, v) in extra { raw.set(k, v) }
        return try Cue(json: raw)
    }

    private func audio(_ id: String, master: Double = 0, outputs: Int = 2) throws -> Cue {
        var kind = TestSnapshots.audioKind("a.wav")
        var levels = kind["levels"] ?? .object([:])
        levels.set("master_db", .double(master))
        levels.set(
            "outputs", .array(Array(repeating: .object(["db": .double(0), "mute": .bool(false)]), count: outputs)))
        kind.set("levels", levels)
        return try cue(id, kind)
    }

    private func fade(_ id: String) throws -> Cue {
        try cue(
            id,
            .object([
                "type": .string("fade"), "duration": .double(5), "curve": .string("s_curve"),
                "levels": .object(["master_db": .double(-10), "output_db": .array([]), "crosspoints": .array([])]),
            ]))
    }

    func testCategoriesThatApplyToNoneOfTheCuesAreLeftOut() throws {
        let source = try audio("a")
        XCTAssertEqual(
            PasteProperty.offered(from: source, to: [try fade("f")]).map(\.category), [.basics, .triggers],
            "an Audio cue's output and levels are nothing a Fade can take")
        let both = PasteProperty.offered(from: source, to: [try fade("f"), try audio("b")])
        XCTAssertEqual(both.map(\.category), [.basics, .triggers, .io, .levels])
        XCTAssertFalse(both[0].properties.contains(.duration), "an Audio cue's duration is its file's")
        let fades = PasteProperty.offered(from: try fade("f"), to: [try fade("g")])
        XCTAssertEqual(fades.map(\.category), [.basics, .triggers, .levels, .fade])
        XCTAssertTrue(fades[0].properties.contains(.duration))
    }

    func testCommandsSetEachTickedPropertyOnTheCuesThatCanTakeIt() throws {
        let source = try cue(
            "s", TestSnapshots.audioKind("a.wav"), ["color": .string("red"), "flagged": .bool(true)])
        let commands = PasteProperty.commands(
            [.colour, .levels], from: source, to: [try audio("b", master: -6), try fade("f")])
        let shapes = commands.map { ($0.json["id"]?.stringValue, $0.json["property"]?["type"]?.stringValue) }
        XCTAssertEqual(shapes.map(\.0), ["b", "b", "f"])
        XCTAssertEqual(shapes.map(\.1), ["color", "kind_fields", "color"], "no levels for the Fade")
        XCTAssertTrue(PasteProperty.commands([], from: source, to: [try audio("b")]).isEmpty)
    }

    /// Levels pasted onto a cue with fewer outputs fill the outputs it has: nothing it lacks is written.
    func testPastedLevelsFitTheCuesOwnOutputs() throws {
        var from = try XCTUnwrap(LevelSet(kind: try audio("a", master: -3, outputs: 4).raw["kind"] ?? .null))
        from.set(.output(3), db: -9)
        from.set(.cross(input: 0, output: 3), db: -2)
        from.set(.cross(input: 0, output: 1), db: -4)
        from.gangs = [[1, 3]]
        let to = try XCTUnwrap(LevelSet(kind: try audio("b", outputs: 2).raw["kind"] ?? .null))
        let pasted = from.pasted(onto: to)
        XCTAssertEqual(pasted.master, -3)
        XCTAssertEqual(pasted.outputs.count, 2)
        XCTAssertEqual(Array(pasted.crosspoints.keys), [.init(input: 0, output: 1)])
        XCTAssertEqual(pasted.gangs, [[1]])
    }

    /// A level changed across several cues sets that level on each and leaves the others as each had them.
    func testABatchLevelEditSetsOnlyWhatItChanged() throws {
        let base = try XCTUnwrap(LevelSet(kind: try audio("a").raw["kind"] ?? .null))
        var other = base
        other.set(.output(1), db: -12)
        other.master = -20
        var edited = base
        edited.set(.output(0), db: -3)
        let result = other.taking(.level(.output(0)), from: edited, limits: VolumeLimits())
        XCTAssertEqual(result.db(.output(0)), -3)
        XCTAssertEqual(result.db(.output(1)), -12, "an output the edit did not touch keeps the cue's own")
        XCTAssertEqual(result.master, -20)
        XCTAssertEqual(
            LevelSet.mixedSlots([base, other], inputs: 1, outputs: 2), [.master, .output(1)])
    }

    /// An output the first cue lacks, set on several cues: the shown levels grow to it, and no cue
    /// takes the outputs that growth added (they were silencing every other output of a wider cue).
    func testAnOutputSetOnSeveralCuesWritesThatOutputAlone() throws {
        let narrow = try XCTUnwrap(LevelSet(kind: try audio("a", outputs: 2).raw["kind"] ?? .null))
        let wide = try XCTUnwrap(LevelSet(kind: try audio("b", outputs: 8).raw["kind"] ?? .null))
        var shown = narrow
        shown.set(.output(5), db: -6)
        let result = wide.taking(.level(.output(5)), from: shown, limits: VolumeLimits())
        XCTAssertEqual(result.outputs.map(\.db), [0, 0, 0, 0, 0, -6, 0, 0])
        XCTAssertEqual(narrow.taking(.level(.output(5)), from: shown, limits: VolumeLimits()), narrow)
    }

    /// A ganged output moved on several cues moves each cue's own gang: a cue without that gang has
    /// only that output moved.
    func testAGangedOutputMovesOnlyEachCuesOwnGang() throws {
        var ganged = try XCTUnwrap(LevelSet(kind: try audio("a").raw["kind"] ?? .null))
        ganged.gangs = [[0, 1]]
        ganged.outputs = [LevelSet.Output(db: 0), LevelSet.Output(db: -10)]
        var plain = ganged
        plain.gangs = []
        plain.set(.output(0), db: -3)
        plain.set(.output(1), db: -3)
        var shown = ganged
        shown.set(.output(0), db: -5, drag: true)
        XCTAssertEqual(
            plain.taking(.level(.output(0)), from: shown, limits: VolumeLimits(), drag: true).outputs.map(\.db),
            [-5, -3])
        XCTAssertEqual(
            ganged.taking(.level(.output(0)), from: shown, limits: VolumeLimits(), drag: true).outputs.map(\.db),
            [-5, -15])
    }

    /// What is remembered: the ticks of the properties offered this time, others' earlier ticks kept.
    func testTheTicksAreRememberedPerProperty() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "paste-\(UUID().uuidString)"))
        PastePropertiesSheet.remember([.levels, .colour], offered: [.levels, .colour, .name], in: defaults)
        PastePropertiesSheet.remember([.name], offered: [.name, .colour], in: defaults)
        XCTAssertEqual(PastePropertiesSheet.remembered(defaults), [.name, .levels])
    }
}

/// Several cues edited in the inspector and Paste Properties, in a workspace window on the real engine
/// (the issue's workspace: Audio cue 1 "intro", Audio cue 2, Fade cue 3 fading cue 1).
extension PlayheadFlowTests {
    private var inspector: InspectorViewController { wc.inspector }

    private func raw(_ id: ModelID, _ key: String) -> JSONValue? { store.model?.cue(id)?.raw[key] }

    private func master(_ id: ModelID) -> Double? {
        store.model?.cue(id)?.raw["kind"]?["levels"]?["master_db"]?.doubleValue
    }

    private func tabIDs() -> [String] { inspector.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    /// A workspace to paste in: a private pasteboard and remembered ticks of its own.
    private func pasteWorkspace() async throws {
        try await editWorkspace()
        let board = NSPasteboard.withUniqueName()
        wc.editor.pasteboard = board
        wc.cueList.pasteboard = board
        wc.pasteDefaults = try XCTUnwrap(UserDefaults(suiteName: "paste-\(UUID().uuidString)"))
    }

    private func selectAll(_ ids: [ModelID]) async {
        wc.cueList.selectAsUser(ids)
        await assertEventually("the inspector shows them") { self.inspector.selection == ids }
    }

    private func setLevels(_ id: ModelID, _ change: (inout LevelSet) -> Void) async throws {
        var set = try XCTUnwrap(LevelSet(kind: store.model?.cue(id)?.raw["kind"] ?? .null))
        change(&set)
        let levels = try XCTUnwrap(set.json(into: .object([:]))["levels"])
        guard case .success = await store.send(command: .setProperty(id, .kindFields(["levels": levels]))) else {
            return XCTFail("levels")
        }
    }

    private func send(_ command: Command) async {
        guard case .success = await store.send(command: command) else { return XCTFail("\(command)") }
    }

    /// Types `text` into an inspector field as the operator does (a click into it, the keys, Return).
    private func typeInto(_ field: NSTextField, _ text: String) throws {
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(field))
        try type(text)
        try pressReturn()
    }

    private var delegate: AppDelegate {
        let d = AppDelegate()
        d.appController = app
        return d
    }

    private func pasteItem() throws -> NSMenuItem {
        try XCTUnwrap(
            MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items).first {
                $0.action == #selector(AppDelegate.pasteCueProperties(_:))
            })
    }

    private func rowMenuItem(on id: ModelID) throws -> NSMenuItem {
        let menu = NSMenu()
        wc.cueList.prepareRowMenu(menu, row: wc.cueList.outlineView.row(forItem: wc.cueList.node(id)))
        return try XCTUnwrap(menu.items.first { $0.title == loc("menu.edit.pasteProperties") })
    }

    private var sheet: PastePropertiesSheet? { wc.pasteSheet }

    private func outputs(_ id: ModelID) -> [Double?] {
        LevelSet(kind: store.model?.cue(id)?.raw["kind"] ?? .null)?.outputs.map(\.db) ?? []
    }

    /// Types `text` in level `slot` of the Levels tab, as a double-click on its box and the keys do.
    private func typeLevel(_ slot: LevelSet.Slot, _ text: String) throws {
        inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        let levels = inspector.audioLevels
        levels.select(slot)
        levels.beginTyping(slot)
        try type(text)
        try pressReturn()
    }

    /// Edit > Paste Properties, as the menu bar runs it: validated, then chosen.
    private func chooseFromEditMenu() async throws {
        let item = try pasteItem()
        XCTAssertTrue(delegate.validateMenuItem(item), "the item is on")
        delegate.pasteCueProperties(item)
        await assertEventually("the sheet is up") { self.wc.window?.attachedSheet != nil }
    }

    /// A key pressed while the sheet is the key window, as the app takes it: the key monitor, then the
    /// window's key equivalents, then the window.
    private func pressInSheet(_ chars: String, code: UInt16) throws {
        let window = try XCTUnwrap(wc.window?.attachedSheet)
        app.keyWindow = { window }
        defer { app.keyWindow = { [unowned self] in app.windows[doc]?.window } }
        let event = try key(chars, code: code, windowNumber: window.windowNumber)
        guard let passed = app.handleKeyDown(event) else { return }
        if !window.performKeyEquivalent(with: passed) { window.sendEvent(passed) }
    }

    private func tick(_ property: PasteProperty) throws {
        let box = try XCTUnwrap(sheet?.propertyChecks[property], "\(property) is listed")
        if box.state != .on { box.performClick(nil) }
    }

    // MARK: The issue's flows

    func testTwoAudioCuesAreSelectedTheInspectorShowsTheTabsThatHoldBatchEditableFieldsAndMixedValuesAsMixed()
        async throws
    {
        try await pasteWorkspace()
        await send(.setProperty(cues[1], .preWait(1.5)))
        await send(.setProperty(cues[1], .color("red")))
        await send(.setProperty(cues[0], .flagged(true)))
        try await setLevels(cues[1]) { $0.master = -10 }
        await sync()
        await selectAll([cues[0], cues[1]])
        XCTAssertTrue(inspector.emptyLabel.isHidden, "the inspector stays live")
        XCTAssertEqual(tabIDs(), ["basics", "triggers", "io", "levels"], "Time & Loops is one cue's file")
        XCTAssertEqual(inspector.nameField.stringValue, "")
        XCTAssertEqual(inspector.nameField.placeholderString, loc("inspector.mixed"), "intro and the file name")
        XCTAssertEqual(inspector.preWaitField.stringValue, "")
        XCTAssertEqual(inspector.preWaitField.placeholderString, loc("inspector.mixed"))
        XCTAssertEqual(inspector.postWaitField.stringValue, CueTime.format(0), "the same on both")
        XCTAssertEqual(inspector.colourPopup.titleOfSelectedItem, loc("inspector.mixed"))
        XCTAssertEqual(inspector.continuePopup.titleOfSelectedItem, loc("inspector.continue.do_not_continue"))
        XCTAssertEqual(inspector.flaggedCheck.state, .mixed)
        XCTAssertEqual(inspector.armedCheck.state, .on)
        XCTAssertFalse(inspector.numberField.isEnabled, "a number names one cue")
        XCTAssertFalse(inspector.hotkeyField.isEnabled)
        XCTAssertFalse(inspector.previewButton.isEnabled)
        inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        let levels = inspector.audioLevels
        XCTAssertEqual(levels.matrix.mixed, [.master], "only the main level differs")
        levels.select(.master)
        XCTAssertEqual(levels.levelField.stringValue, "", "a mixed level shows no value")
        levels.select(.output(0))
        XCTAssertEqual(levels.levelField.stringValue, "0.0")
        XCTAssertNil(levels.meter.source, "no one cue's meter")
        // One cue again: everything is that cue's.
        await selectAll([cues[1]])
        XCTAssertEqual(inspector.preWaitField.stringValue, CueTime.format(1.5))
        XCTAssertTrue(inspector.numberField.isEnabled)
        XCTAssertEqual(levels.matrix.mixed, [])
        XCTAssertFalse(inspector.flaggedCheck.allowsMixedState)
    }

    func testAnAudioCueAndAFadeCueAreSelectedOnlyBasicsAndTriggersAreShown() async throws {
        try await pasteWorkspace()
        await selectAll([cues[0], fade])
        XCTAssertEqual(tabIDs(), ["basics", "triggers"])
        XCTAssertFalse(inspector.durationField.isEnabled, "an Audio cue's duration is its file's")
        XCTAssertFalse(inspector.targetField.isEnabled, "a file and a cue target are not one field")
        XCTAssertNil(inspector.kindIcon.image, "two kinds")
        await selectAll([cues[0], cues[1], fade])
        XCTAssertEqual(tabIDs(), ["basics", "triggers"])
    }

    func testAValueTypedInASharedFieldIsTakenByEverySelectedCueInOneUndoStep() async throws {
        try await pasteWorkspace()
        try await setLevels(cues[1]) { $0.set(.output(0), db: -12) }
        await sync()
        await selectAll([cues[0], cues[1]])
        try typeInto(inspector.preWaitField, "2")
        await assertEventually("both wait") {
            [self.cues[0], self.cues[1]].allSatisfy { self.raw($0, "pre_wait")?.doubleValue == 2 }
        }
        XCTAssertEqual(inspector.preWaitField.stringValue, CueTime.format(2))
        await wc.editor.undo()
        await assertEventually("one undo takes both back") {
            [self.cues[0], self.cues[1]].allSatisfy { (self.raw($0, "pre_wait")?.doubleValue ?? 0) == 0 }
        }
        try typeInto(inspector.nameField, "music")
        await assertEventually("both named") {
            [self.cues[0], self.cues[1]].allSatisfy { self.raw($0, "name")?.stringValue == "music" }
        }
        // A click on a mixed tick box sets it on every cue.
        await send(.setProperty(cues[0], .flagged(true)))
        await assertEventually { self.inspector.flaggedCheck.state == .mixed }
        inspector.flaggedCheck.performClick(nil)
        await assertEventually("both flagged") {
            [self.cues[0], self.cues[1]].allSatisfy { self.raw($0, "flagged") == .bool(true) }
        }
        // A level typed sets that level on both; each keeps its other levels.
        inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        let levels = inspector.audioLevels
        levels.select(.master)
        levels.beginTyping(.master)
        try type("6")
        try pressReturn()
        await assertEventually("both at -6") { [self.cues[0], self.cues[1]].allSatisfy { self.master($0) == -6 } }
        let outputs = { (id: ModelID) in LevelSet(kind: self.store.model?.cue(id)?.raw["kind"] ?? .null)?.db(.output(0))
        }
        XCTAssertEqual(outputs(cues[1]), -12, "cue 2's own output level is kept")
        XCTAssertEqual(outputs(cues[0]), 0)
        XCTAssertEqual(levels.matrix.mixed, [.output(0)])
        await wc.editor.undo()
        await assertEventually("one undo") { [self.cues[0], self.cues[1]].allSatisfy { self.master($0) == 0 } }
        XCTAssertEqual(goes(), 0, "Return fired no GO")
    }

    /// Cues with different output counts: an output only the wider cue has is set on it alone, and its
    /// other outputs keep their levels (they were silenced, and heard so at once on a playing cue).
    func testAnOutputLevelSetOnCuesWithDifferentOutputCountsLeavesTheirOtherOutputs() async throws {
        try await pasteWorkspace()
        try await setLevels(cues[1]) { set in
            set.outputs = Array(repeating: LevelSet.Output(db: 0), count: 8)
        }
        await assertEventually("cue 2 has 8 outputs") { self.outputs(self.cues[1]).count == 8 }
        await selectAll([cues[0], cues[1]])
        try typeLevel(.output(5), "6")
        await assertEventually("output 6 of cue 2 is at -6") { self.outputs(self.cues[1])[5] == -6 }
        XCTAssertEqual(outputs(cues[1]), [0, 0, 0, 0, 0, -6, 0, 0], "no other output changed")
        XCTAssertEqual(outputs(cues[0]), [0, 0], "cue 1 has no output 6")
        await wc.editor.undo()
        await assertEventually("one undo") { self.outputs(self.cues[1]) == Array(repeating: 0, count: 8) }
    }

    /// Cues ganged differently: no gang is drawn or can be set, and a ganged output moved moves each
    /// cue's own gang only.
    func testGangsThatDifferAreNotDrawnAndAMovedOutputMovesEachCuesOwnGang() async throws {
        try await pasteWorkspace()
        try await setLevels(cues[0]) { set in
            set.gangs = [[0, 1]]
            set.outputs = [LevelSet.Output(db: 0), LevelSet.Output(db: -10)]
        }
        try await setLevels(cues[1]) { set in
            set.outputs = [LevelSet.Output(db: -3), LevelSet.Output(db: -3)]
        }
        await sync()
        await selectAll([cues[0], cues[1]])
        let levels = inspector.audioLevels
        XCTAssertTrue(levels.matrix.hidesGangs)
        inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        levels.select(.output(0))
        XCTAssertFalse(levels.gangField.isEnabled, "a gang cannot be set while they differ")
        try typeLevel(.output(0), "5")
        await assertEventually("both outputs 1 at -5") {
            self.outputs(self.cues[0]).first == -5 && self.outputs(self.cues[1]).first == -5
        }
        XCTAssertEqual(outputs(cues[0]), [-5, -15], "cue 1's gang moved with it")
        XCTAssertEqual(outputs(cues[1]), [-5, -3], "cue 2 has no gang: its output 2 stays")
        XCTAssertEqual(store.model?.cue(cues[1])?.raw["kind"]?["levels"]?["gangs"]?.arrayValue?.count ?? 0, 0)
        // Ganged alike: the gang is drawn and can be set again.
        try await setLevels(cues[1]) { $0.gangs = [[0, 1]] }
        await assertEventually("ganged alike") { !levels.matrix.hidesGangs }
        levels.select(.output(0))
        XCTAssertTrue(levels.gangField.isEnabled)
    }

    /// Notes typed with several cues selected and refused (over the engine's bound) are offered back,
    /// as one cue's are, not dropped.
    func testNotesTypedForSeveralCuesAndRefusedAreOffered() async throws {
        try await pasteWorkspace()
        var offered: [String] = []
        inspector.onNotesNotSaved = { _, text, _ in offered.append(text) }
        await selectAll([cues[0], cues[1]])
        let big = String(repeating: "x", count: 70 * 1024)
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(inspector.notesView))
        inspector.notesView.string = big
        inspector.textDidChange(Notification(name: NSText.didChangeNotification, object: inspector.notesView))
        XCTAssertTrue(try XCTUnwrap(wc.window).makeFirstResponder(nil))
        await assertEventually("the text is offered") { offered == [big] }
        XCTAssertEqual(raw(cues[0], "notes")?.stringValue ?? "", "", "nothing was saved")
    }

    /// Keys pressed in the sheet are the sheet's: a cue hotkey on Return, on a letter or on Space
    /// fires no cue, and Space fires no GO. Return applies the paste.
    func testKeysInTheSheetFireNoCueHotkeyAndReturnApplies() async throws {
        try await pasteWorkspace()
        for (chars, code) in [("\r", UInt16(36)), ("k", UInt16(40)), (" ", UInt16(49))] {
            let chord = try XCTUnwrap(KeyChord(event: try key(chars, code: code)))
            let target = try XCTUnwrap(store.model?.cue(cues[1]))
            await send(InspectorEdit.setTriggers(target, hotkey: .some(chord.hotkeyJSON)))
            await sync()
            _ = await wc.editor.copySelection(of: [cues[0]])
            await selectAll([fade])
            try await chooseFromEditMenu()
            try tick(.colour)
            let hotkeys = wire.sent.filter { if case .hotkey = $0 { return true } else { return false } }.count
            if chars == "\r" {
                try pressInSheet(chars, code: code)
                await assertEventually("Return applied the paste") { self.wc.window?.attachedSheet == nil }
            } else {
                try pressInSheet(chars, code: code)
                XCTAssertNotNil(wc.window?.attachedSheet, "\(chars.debugDescription) stays with the sheet")
                sheet?.cancel()
                await assertEventually { self.wc.window?.attachedSheet == nil }
            }
            await sync()
            try await play(0.2)
            XCTAssertEqual(
                wire.sent.filter { if case .hotkey = $0 { return true } else { return false } }.count, hotkeys,
                "no hotkey for \(chars.debugDescription)")
            XCTAssertNotEqual(store.cueStates[cues[1]]?.state, .running, "cue 2 was not started")
            XCTAssertEqual(goes(), 0)
            await send(.setProperty(fade, .color("none")))
        }
    }

    /// QLab 5, manual p. 123: an edit made with the mouse in the list sets that cue alone.
    func testWithSeveralCuesSelectedAValueEditedByMouseInOneRowOfTheListChangesOnlyThatCue() async throws {
        try await pasteWorkspace()
        await selectAll([cues[0], cues[1]])
        try doubleClick(cues[1], "name")
        await assertEventually("the name opens") { self.wc.cueList.editingCell?.id == self.cues[1] }
        try type("solo")
        try pressReturn()
        await assertEventually("cue 2 is renamed") { self.cell(self.cues[1], "name") == "solo" }
        XCTAssertEqual(raw(cues[0], "name")?.stringValue, "intro", "the other selected cue is not")
        XCTAssertEqual(cell(cues[0], "name"), "intro")
    }

    func testACueIsCopiedOtherCuesAreSelectedAndPastePropertiesListsPropertiesByCategory() async throws {
        try await pasteWorkspace()
        let copied = await wc.editor.copySelection(of: [cues[0]])
        XCTAssertTrue(copied)
        // The shortcut is the Edit menu item's key.
        let item = try pasteItem()
        XCTAssertEqual(item.keyEquivalent, "V", "Shift-Command-V")
        XCTAssertEqual(item.keyEquivalentModifierMask, .command)
        await selectAll([cues[1], fade])
        try await chooseFromEditMenu()
        XCTAssertEqual(
            Set(try XCTUnwrap(sheet).categoryChecks.keys), [.basics, .triggers, .io, .levels],
            "cue 2 takes the Audio categories")
        XCTAssertNotNil(sheet?.propertyChecks[.name])
        XCTAssertNil(sheet?.propertyChecks[.duration], "an Audio cue's duration is not pasted")
        XCTAssertTrue(
            sheet?.titleLabel.stringValue.contains("1 \u{00B7} intro") == true, sheet?.titleLabel.stringValue ?? "")
        sheet?.cancel()
        await assertEventually { self.wc.window?.attachedSheet == nil }
        // From the row menu, onto the Fade alone: the Audio categories are not listed.
        await selectAll([fade])
        let row = try rowMenuItem(on: fade)
        XCTAssertTrue(row.isEnabled)
        _ = row.target?.perform(row.action, with: row)
        await assertEventually("the sheet is up") { self.wc.window?.attachedSheet != nil }
        XCTAssertEqual(Set(try XCTUnwrap(sheet).categoryChecks.keys), [.basics, .triggers])
        sheet?.cancel()
    }

    func testApplyOrEnterSetsTheTickedPropertiesOnEveryCueInOneUndoStepCancelOrEscChangesNothing() async throws {
        try await pasteWorkspace()
        await send(.setProperty(cues[0], .color("red")))
        try await setLevels(cues[0]) { $0.master = -6 }
        await sync()
        _ = await wc.editor.copySelection(of: [cues[0]])
        await selectAll([cues[1], fade])
        try await chooseFromEditMenu()
        try tick(.colour)
        try tick(.levels)
        try pressInSheet("\r", code: 36)
        await assertEventually("both red, cue 2 at -6") {
            self.raw(self.cues[1], "color")?.stringValue == "red" && self.raw(self.fade, "color")?.stringValue == "red"
                && self.master(self.cues[1]) == -6
        }
        XCTAssertNil(wc.window?.attachedSheet)
        XCTAssertEqual(raw(cues[1], "name")?.stringValue ?? "", "", "the name was not ticked")
        XCTAssertNotEqual(master(fade), -6, "a Fade does not take an Audio cue's levels")
        await wc.editor.undo()
        await assertEventually("one undo") {
            self.raw(self.cues[1], "color")?.stringValue == "none"
                && self.raw(self.fade, "color")?.stringValue == "none"
                && self.master(self.cues[1]) == 0
        }
        // Enter (the keypad's) applies too.
        try await chooseFromEditMenu()
        try pressInSheet("\u{3}", code: 76)
        await assertEventually("Enter applied") { self.raw(self.cues[1], "color")?.stringValue == "red" }
        // Esc and Cancel change nothing.
        let sent = wire.commands.count
        try await chooseFromEditMenu()
        try tick(.name)
        try pressInSheet("\u{1B}", code: 53)
        await assertEventually("Esc closed it") { self.wc.window?.attachedSheet == nil }
        try await chooseFromEditMenu()
        try tick(.name)
        try XCTUnwrap(sheet).cancelButton.performClick(nil)
        await assertEventually("Cancel closed it") { self.wc.window?.attachedSheet == nil }
        await sync()
        XCTAssertEqual(wire.commands.count, sent, "nothing was sent")
        XCTAssertEqual(raw(cues[1], "name")?.stringValue ?? "", "")
        // Space in the sheet is the sheet's, never GO.
        try await chooseFromEditMenu()
        try pressInSheet(" ", code: 49)
        XCTAssertEqual(goes(), 0)
        sheet?.cancel()
    }

    func testTheSheetOpensWithTheBoxesTickedAsTheyWereTheLastTimeItWasUsed() async throws {
        try await pasteWorkspace()
        _ = await wc.editor.copySelection(of: [cues[0]])
        await selectAll([cues[1]])
        try await chooseFromEditMenu()
        XCTAssertTrue(try XCTUnwrap(sheet).ticked.isEmpty, "nothing ticked the first time")
        try tick(.colour)
        try tick(.levels)
        try XCTUnwrap(sheet).applyButton.performClick(nil)
        await assertEventually { self.wc.window?.attachedSheet == nil }
        try await chooseFromEditMenu()
        XCTAssertEqual(sheet?.ticked, [.colour, .levels])
        XCTAssertEqual(sheet?.categoryChecks[.levels]?.state, .on)
        XCTAssertEqual(sheet?.categoryChecks[.basics]?.state, .mixed, "some of Basics")
        // A cancelled sheet is not a use: what it had ticked is not remembered.
        try tick(.name)
        sheet?.cancel()
        try await chooseFromEditMenu()
        XCTAssertEqual(sheet?.ticked, [.colour, .levels])
        sheet?.cancel()
    }

    func testNothingCopiedOrTheClipboardHoldsNoCueTheMenuItemIsDisabled() async throws {
        try await pasteWorkspace()
        await selectAll([cues[1]])
        let item = try pasteItem()
        XCTAssertFalse(delegate.validateMenuItem(item), "nothing copied")
        XCTAssertFalse(try rowMenuItem(on: cues[1]).isEnabled)
        wc.editor.pasteboard.clearContents()
        wc.editor.pasteboard.setString("intro", forType: .string)
        XCTAssertFalse(delegate.validateMenuItem(item), "text is not a cue")
        wc.editor.pasteboard.clearContents()
        wc.editor.pasteboard.setString(#"{"version":1,"cues":[{"id":"x"}]}"#, forType: CueEditor.pasteType)
        XCTAssertFalse(delegate.validateMenuItem(item), "a damaged copy is no cue")
        _ = await wc.editor.copySelection(of: [cues[0]])
        XCTAssertTrue(delegate.validateMenuItem(item))
        XCTAssertTrue(try rowMenuItem(on: cues[1]).isEnabled)
        await selectAll([])
        XCTAssertFalse(delegate.validateMenuItem(item), "no cue to paste onto")
    }

    /// The pasted levels reach a playing cue as a level edit does: it plays on, at the new level.
    func testOneOfTheSelectedCuesIsPlayingThePastedLevelsAreHeardOnItAtOnce() async throws {
        try await pasteWorkspace()
        try await setLevels(cues[0]) { $0.master = LevelText.silence }
        await sync()
        _ = await wc.editor.copySelection(of: [cues[0]])
        // A file long enough to still be playing when the paste lands (the shared one lasts a second).
        let long = try XCTUnwrap(try tempFiles(["long.txt"]).first).deletingLastPathComponent()
            .appendingPathComponent("long.wav")
        try writeWAV(long, seconds: 30)
        await send(.setProperty(cues[1], .media(CueEditor.mediaPath(long))))
        try await play(0)
        await selectAll([cues[1]])
        inspector.tabView.selectTabViewItem(withIdentifier: "levels")
        guard case .success = await store.transport(.start(cue: cues[1])) else { return XCTFail("start") }
        func peak() async throws -> Float {
            store.meters.reset()
            var loudest: Float = 0
            for _ in 0..<3 {
                try await play(0.1)
                store.meters.tick(0)
                loudest = max(loudest, store.meters.level(.cue(cues[1]), channel: 0))
            }
            return loudest
        }
        try await play(0.3)
        let before = try await peak()
        XCTAssertGreaterThan(before, 0.05, "cue 2 is heard")
        await selectAll([cues[1], fade])
        try await chooseFromEditMenu()
        try tick(.levels)
        try XCTUnwrap(sheet).applyButton.performClick(nil)
        await assertEventually("cue 2's main level is pasted") { self.master(self.cues[1]) == LevelText.silence }
        await selectAll([cues[1]])
        try await play(0.1)  // the change ramps in over a few blocks
        let after = try await peak()
        XCTAssertLessThan(after, 0.001, "silent at once")
        XCTAssertEqual(store.cueStates[cues[1]]?.state, .running, "it plays on")
    }

    func testShowModeTheInspectorIsNotShownAndTheMenuItemIsDisabled() async throws {
        try await pasteWorkspace()
        _ = await wc.editor.copySelection(of: [cues[0]])
        await selectAll([cues[1], fade])
        let item = try pasteItem()
        XCTAssertTrue(delegate.validateMenuItem(item))
        // A sheet up when Show Mode comes is closed with nothing pasted.
        try await chooseFromEditMenu()
        try tick(.name)
        let sent = wire.commands.count
        wc.setShowMode(true)
        await assertEventually("Show Mode") { self.wc.isShowMode }
        XCTAssertNil(wc.window?.attachedSheet)
        XCTAssertFalse(wc.isVisible(.inspector), "the inspector is not shown")
        XCTAssertFalse(delegate.validateMenuItem(item))
        XCTAssertFalse(try rowMenuItem(on: cues[1]).isEnabled)
        wc.promptPasteProperties(onto: [cues[1]])
        XCTAssertNil(wc.window?.attachedSheet, "no sheet in Show Mode")
        await wc.editor.pasteProperties([.name], from: try XCTUnwrap(wc.editor.clipboardCue), onto: [cues[1]])
        await sync()
        XCTAssertEqual(wire.commands.count, sent, "nothing was sent")
    }
}
