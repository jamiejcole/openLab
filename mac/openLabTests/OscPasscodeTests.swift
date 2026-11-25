// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// What the OSC tab draws for one row: the code shown and, for View, Edit and Control, whether the
/// box is on and whether it can be clicked.
private struct OscShown: Equatable {
    var code: String
    var on: [Bool]
    var enabled: [Bool]
}

/// NET-02: the OSC passcode table of the workspace settings, driven as the operator drives it
/// (clicking the boxes, typing a code, Edit > Undo with the settings window in front) against the
/// real engine, and read from what the table draws. Each flow of the feature has a test named
/// after it. Shares the engine and helpers of `WorkspaceSettingsTests`.
extension WorkspaceSettingsTests {
    /// A new untitled workspace on the test's engine, its settings window open on the OSC tab.
    private func newWorkspace() async throws -> (Rig, WorkspaceWindowController) {
        let app = try await Rig.open(nil, on: engine)
        guard case .success(let ok) = await app.client.send({ .request(id: $0, .new) }),
            case .opened(let doc)? = ok.data
        else {
            XCTFail("new")
            throw TestEngineError.notConnected
        }
        let rig = Rig(engine: engine, workspace: doc)
        guard await rig.connect() else { throw TestEngineError.notConnected }
        await assertEventually("the new workspace loaded") { rig.store.model != nil }
        let wc = WorkspaceWindowController(store: rig.store)
        await wc.showSettings().value
        let sc = wc.settingsWindow
        sc.tabs.selectTabViewItem(withIdentifier: "osc")
        // No field has the keyboard: Edit > Undo is then the workspace's, not a field's.
        sc.window?.makeFirstResponder(nil)
        return (rig, wc)
    }

    /// The No Passcode row, then each passcode row, as drawn.
    private func shown(_ sc: WorkspaceSettingsWindowController) -> [OscShown] {
        func row(_ code: String, _ boxes: [NSButton]) -> OscShown {
            OscShown(code: code, on: boxes.map { $0.state == .on }, enabled: boxes.map(\.isEnabled))
        }
        let rows = sc.oscCodeFields.indices.map { i in
            row(
                sc.oscCodeFields[i].stringValue,
                [sc.oscViewBoxes[i], sc.oscEditBoxes[i], sc.oscControlBoxes[i]])
        }
        return [row("none", sc.oscNoneRights)] + rows
    }

    /// The engine's table, read from the store.
    private func table(_ rig: Rig) -> OscAccessTable { OscAccessTable(settings: rig.store.model?.settings) }

    private func codes(_ rig: Rig) -> [String] { table(rig).rows.map(\.passcode) }

    /// Puts these rows after the table's (a starting state, sent as commands).
    private func addRows(_ rig: Rig, _ rows: [(String, OscRights)]) async {
        for (code, rights) in rows {
            let at = table(rig).rows.count
            if case .failure(let error) = await rig.store.send(command: .addOscPasscode(code, rights: rights, at: at)) {
                return XCTFail("add \(code): \(error)")
            }
        }
    }

    /// `count` more full-rights rows with codes from `first` up that the table does not hold yet: a
    /// new workspace's own passcode is random, and a fixed code that matches it is refused.
    private func addFreshRows(_ rig: Rig, _ count: Int, from first: Int) async {
        let taken = Set(codes(rig))
        let fresh = (first..<first + count + taken.count).map(String.init).filter { !taken.contains($0) }
        await addRows(rig, fresh.prefix(count).map { ($0, .all) })
    }

    /// Empties the passcode table (a starting state).
    private func clearRows(_ rig: Rig) async {
        for code in codes(rig) {
            guard case .success = await rig.store.send(command: .removeOscPasscode(code)) else {
                return XCTFail("remove \(code)")
            }
        }
    }

    /// The commands sent for the passcode table.
    private func oscCommands(_ rig: Rig) -> [JSONValue] {
        commands(rig).filter { ($0["type"]?.stringValue ?? "").contains("osc") }
    }

    /// Edit > Undo and Edit > Redo as the app routes them while the settings window is key.
    private func menu(_ wc: WorkspaceWindowController, _ rig: Rig) -> AppDelegate {
        let session = EngineSession(transport: WebSocketTransport(), infoProvider: { .failure(.missing) })
        let app = AppController(session: session, scheduler: ManualScheduler())
        app.launching = true
        app.controllers[ObjectIdentifier(rig.store)] = wc
        app.keyWindow = { wc.settingsWindow.window }
        let delegate = AppDelegate()
        delegate.appController = app
        return delegate
    }

    // MARK: Flows

    func testNewWorkspaceTable() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        let now = shown(sc)
        XCTAssertEqual(now.count, 2, "the fixed row and one passcode")
        XCTAssertEqual(now[0], OscShown(code: "none", on: [false, false, false], enabled: [true, false, false]))
        XCTAssertEqual(sc.oscNoneLabel.stringValue, loc("settings.osc.noPasscode"))
        XCTAssertFalse(sc.oscNoneLabel.isEditable)
        XCTAssertFalse(sc.oscGrid.cell(atColumnIndex: 4, rowIndex: 0).contentView is NSButton, "no delete")
        XCTAssertEqual(now[1].on, [true, true, true])
        XCTAssertEqual(now[1].enabled, [true, true, true])
        let code = try XCTUnwrap(Int(now[1].code))
        XCTAssertTrue((1000...9999).contains(code), now[1].code)
        XCTAssertEqual(sc.oscDeleteButtons.count, 1)
        XCTAssertEqual(rig.store.document?.dirty, false)
        // Greyed boxes read as unavailable in both appearances: drawn, one differs from a box that
        // can be clicked.
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let look = NSAppearance(named: name)
            let available = sc.oscNoneRights[0]  // View: off, can be clicked
            let greyed = sc.oscNoneRights[1]  // Edit: off, greyed
            available.appearance = look
            greyed.appearance = look
            let a = try renderView(available).tiffRepresentation
            let g = try renderView(greyed).tiffRepresentation
            XCTAssertNotEqual(a, g, "\(name.rawValue): a greyed box looks different from an available one")
        }
    }

    func testNewPasscode() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        let first = codes(rig)
        sc.oscAddButton.performClick(nil)
        await assertEventually("a row came") { self.shown(sc).count == 3 }
        let rows = shown(sc)
        XCTAssertEqual(rows[2].on, [true, true, true], "full rights")
        let code = try XCTUnwrap(Int(rows[2].code))
        XCTAssertTrue((1000...9999).contains(code))
        XCTAssertFalse(first.contains(rows[2].code), "a code not in the table")
        XCTAssertEqual(rows[1].code, first[0], "at the end")
        await assertEventually("unsaved") { rig.store.document?.dirty == true }
    }

    func testTableFull() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await addFreshRows(rig, 15, from: 2001)
        await assertEventually { self.shown(sc).count == 17 }
        let before = commands(rig).count
        sc.oscAddButton.performClick(nil)
        await assertEventually("the full line") { sc.messageLabel.stringValue == loc("settings.osc.full") }
        XCTAssertEqual(shown(sc).count, 17, "no row added")
        XCTAssertEqual(commands(rig).count, before, "nothing sent")
    }

    func testEditACodeInPlace() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", OscRights(view: true, edit: false, control: true)), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        try type("4821", into: sc.oscCodeFields[0])
        await assertEventually("the new code") { self.codes(rig) == ["4821", "2222"] }
        await assertEventually { self.shown(sc)[1].code == "4821" }
        XCTAssertEqual(shown(sc)[1].on, [true, false, true], "rights unchanged")
        XCTAssertEqual(table(rig).rows[0].rights, OscRights(view: true, edit: false, control: true))
        XCTAssertEqual(commands(rig).last?["type"]?.stringValue, "transaction", "a remove then an add, one step")
        // Reopening the tab shows it.
        let again = WorkspaceSettingsWindowController(store: rig.store)
        await again.present().value
        XCTAssertEqual(again.oscCodeFields.map(\.stringValue), ["4821", "2222"])
    }

    /// A code committed is the code of the row its field was drawn in, even when another client
    /// removes a row above it before the commit is carried out (commits wait for the ones before).
    func testACommittedCodeChangesTheRowItWasTypedIn() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all), ("3333", .all)])
        await assertEventually { self.shown(sc).count == 4 }
        // An earlier edit still on its way holds the queue.
        final class Gate { var open = false }
        let gate = Gate()
        sc.edits.add("gate") {
            while !gate.open { await Task.yield() }
            return true
        }
        try type("5555", into: sc.oscCodeFields[1])
        // Meanwhile another client removes the first row; the table is drawn again.
        guard case .success = await rig.store.send(command: .removeOscPasscode("1111")) else {
            return XCTFail("remove")
        }
        await assertEventually("the table follows") { self.shown(sc).map(\.code) == ["none", "5555", "3333"] }
        gate.open = true
        await sc.edits.answered()
        await assertEventually("2222 became 5555, 3333 stayed") { self.codes(rig) == ["5555", "3333"] }
    }

    /// A new code typed, Tab away, back, the old code typed again: the engine has the old code again
    /// (it is an access code, so the field and the engine must agree), and a third code goes through.
    func testACodeTypedThenTheOldOneAgainAcrossTabsEndsRightInTheEngine() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        let first = sc.oscCodeFields[0]
        let second = sc.oscCodeFields[1]
        try typeKeeping("3333", in: first)
        sc.window?.makeFirstResponder(second)
        await sc.edits.answered()
        await assertEventually("3333 is stored") { self.codes(rig) == ["3333", "2222"] }
        try typeKeeping("1111", in: first)
        sc.window?.makeFirstResponder(second)
        await sc.edits.answered()
        await assertEventually("1111 is back") { self.codes(rig) == ["1111", "2222"] }
        XCTAssertEqual(first.stringValue, "1111")
        // A third code goes through too.
        try typeKeeping("4444", in: first)
        sc.window?.makeFirstResponder(second)
        await sc.edits.answered()
        await assertEventually("4444 is stored") { self.codes(rig) == ["4444", "2222"] }
        sc.window?.makeFirstResponder(nil)
        await sc.edits.answered()
        XCTAssertEqual(sc.model.message, nil)
    }

    /// A code field clicked into and left untouched sends nothing, and Esc drops what was typed.
    func testAnUntouchedCodeFieldSendsNothingAndEscRestoresIt() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all)])
        await assertEventually { self.shown(sc).count == 2 }
        let before = oscCommands(rig).count
        sc.window?.makeFirstResponder(sc.oscCodeFields[0])
        sc.window?.makeFirstResponder(nil)
        await sc.edits.answered()
        XCTAssertEqual(oscCommands(rig).count, before, "untouched: nothing sent")
        let field = sc.oscCodeFields[0]
        try typeKeeping("9999", in: field)
        _ = sc.control(
            field, textView: try XCTUnwrap(field.currentEditor() as? NSTextView),
            doCommandBy: #selector(NSResponder.cancelOperation(_:)))
        sc.window?.makeFirstResponder(nil)
        await sc.edits.answered()
        XCTAssertEqual(oscCommands(rig).count, before, "Esc sent nothing")
        XCTAssertEqual(codes(rig), ["1111"])
        await assertEventually("the field shows the engine's code") { sc.oscCodeFields.first?.stringValue == "1111" }
    }

    /// Codes swapped through the fields (the rows are not built again while the caret hops): a box
    /// clicked in a row changes the rights of the code that row shows now, not of the one it was
    /// drawn with, which another row holds by then.
    func testARightsBoxActsOnTheCodeItsRowShowsAfterCodesWereRenamedInPlace() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        let first = sc.oscCodeFields[0]
        let second = sc.oscCodeFields[1]
        try typeKeeping("3333", in: first)
        sc.window?.makeFirstResponder(second)
        await sc.edits.answered()
        try typeKeeping("1111", in: second)
        sc.window?.makeFirstResponder(first)
        await sc.edits.answered()
        await assertEventually("renamed in place") { self.codes(rig) == ["3333", "1111"] }
        // The first row's Control box (the row shows 3333 now) is clicked off.
        sc.oscControlBoxes[0].performClick(nil)
        await sc.edits.answered()
        await assertEventually("the rights of 3333 changed, 1111's did not") {
            let rows = self.table(rig).rows
            return rows.map(\.passcode) == ["3333", "1111"] && rows[0].rights.control == false
                && rows[1].rights.control == true
        }
    }

    /// Rows 1111 and 2222 drawn, the caret in neither.
    private func twoCodes() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        owners.append(wc)
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).map(\.code) == ["none", "1111", "2222"] }
        return (rig, sc)
    }

    /// Types `text` in code field `i` and Tabs to the other one: the commit is queued.
    private func typeAndTab(_ text: String, _ i: Int, _ sc: WorkspaceSettingsWindowController) throws {
        try typeKeeping(text, in: sc.oscCodeFields[i])
        sc.window?.makeFirstResponder(sc.oscCodeFields[1 - i])
    }

    /// A code changed, then Undo, with the caret in the other row: the row shows the old code again,
    /// and what is done in the row is done to it.
    func testARowWhoseCodeChangeWasUndoneIsActedOnByTheCodeTheEngineHas() async throws {
        let (rig, sc) = try await twoCodes()
        try typeAndTab("5555", 0, sc)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["5555", "2222"])
        _ = await rig.store.undo()
        await assertEventually("undone") { self.codes(rig) == ["1111", "2222"] }
        await assertEventually("the row shows it") { sc.oscCodeFields[0].stringValue == "1111" }
        XCTAssertNotNil(sc.oscCodeFields[1].currentEditor(), "the caret stays in the other row")
        sc.oscControlBoxes[0].performClick(nil)
        await sc.edits.answered()
        XCTAssertEqual(table(rig).rows.map(\.rights.control), [false, true], "1111's Control is off")
        try typeAndTab("8888", 0, sc)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["8888", "2222"])
    }

    /// Another client renames the row whose code is being typed: what is typed goes to that row.
    func testACodeTypedInARowAnotherClientRenamedGoesToThatRow() async throws {
        let (rig, sc) = try await twoCodes()
        let other = try await another(rig)
        try typeKeeping("4444", in: sc.oscCodeFields[0])
        await rename("1111", to: "3333", at: 0, by: other)
        await assertEventually { self.codes(rig) == ["3333", "2222"] }
        await settle()
        XCTAssertEqual(sc.oscCodeFields[0].currentEditor()?.string, "4444", "the typing goes on")
        sc.window?.makeFirstResponder(nil)
        await sc.edits.answered()
        await assertEventually("the row it was typed in") { self.codes(rig) == ["4444", "2222"] }
        await assertEventually { self.shown(sc).map(\.code) == ["none", "4444", "2222"] }
    }

    /// Another client removes the row a code is being typed in, then adds one in its place: the text
    /// goes nowhere, a line says so, and the new row is left alone.
    func testACodeTypedInARowAnotherClientRemovedGoesNowhereAndSaysSo() async throws {
        let (rig, sc) = try await twoCodes()
        let other = try await another(rig)
        try typeKeeping("4444", in: sc.oscCodeFields[0])
        guard case .success = await other.store.send(command: .removeOscPasscode("1111")),
            case .success = await other.store.send(command: .addOscPasscode("3333", rights: .all, at: 0))
        else { return XCTFail("the other client") }
        await assertEventually { self.codes(rig) == ["3333", "2222"] }
        await settle()
        XCTAssertFalse(sc.oscControlBoxes[0].isEnabled, "the row is gone: its boxes are off")
        XCTAssertEqual(sc.oscControlBoxes[0].state, .off)
        let sent = oscCommands(rig).count
        sc.window?.makeFirstResponder(nil)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["3333", "2222"])
        XCTAssertEqual(oscCommands(rig).count, sent, "nothing sent")
        await assertEventually("said") { sc.messageLabel.stringValue == loc("settings.rowGone") }
        await assertEventually { self.shown(sc).map(\.code) == ["none", "3333", "2222"] }
    }

    /// A code held but not typed in, and one the caret is not in: neither shows a code the engine no
    /// longer has, nor its rights.
    func testACodeTheEngineNoLongerHasIsNotShownWhileTheCaretIsInTheTable() async throws {
        let (rig, sc) = try await twoCodes()
        let other = try await another(rig)
        sc.window?.makeFirstResponder(sc.oscCodeFields[1])
        await rename("2222", to: "3333", at: 1, by: other)
        await assertEventually("the held row follows") { sc.oscCodeFields[1].currentEditor()?.string == "3333" }
        guard case .success = await other.store.send(command: .removeOscPasscode("1111")) else { return XCTFail() }
        await assertEventually("the other row is gone") { sc.oscCodeFields[0].isHiddenOrHasHiddenAncestor }
        XCTAssertNotNil(sc.oscCodeFields[1].currentEditor(), "the caret stays")
        guard case .success = await other.store.send(command: .removeOscPasscode("3333")) else { return XCTFail() }
        await assertEventually("the held row's code is gone") { sc.oscCodeFields[1].currentEditor()?.string == "" }
        XCTAssertEqual(sc.oscViewBoxes[1].state, .off)
        sc.window?.makeFirstResponder(nil)
        await assertEventually("only the fixed row") { self.shown(sc).map(\.code) == ["none"] }
        XCTAssertEqual(codes(rig), [])
    }

    /// Delete on a row while another row's code is half typed: the click commits the half code (it is
    /// refused, with its line), and the deleted row goes.
    func testDeleteWithAnotherCodeHalfTypedDeletesItsRowAndRefusesTheHalfCode() async throws {
        let (rig, sc) = try await twoCodes()
        try typeKeeping("88", in: sc.oscCodeFields[1])
        sc.oscDeleteButtons[0].performClick(nil)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["2222"])
        await assertEventually { self.shown(sc).map(\.code) == ["none", "2222"] }
        XCTAssertEqual(sc.messageLabel.stringValue, loc("settings.osc.invalid"), "the delete does not hide it")
    }

    /// With the engine's answers held: a refused code, then another typed in the same row.
    func testACodeTypedAfterOneThatWillBeRefusedIsTaken() async throws {
        let (rig, sc) = try await twoCodes()
        rig.wire.holdResults = true
        try typeAndTab("5555", 0, sc)  // its answer holds the ones after it
        try typeAndTab("2222", 0, sc)  // the other row's: refused
        try typeAndTab("7777", 0, sc)
        await settle()
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["7777", "2222"])
        XCTAssertEqual(sc.oscCodeFields[0].stringValue, "7777")
        XCTAssertNil(sc.model.message)
    }

    /// Delete and Control on a row whose new code is still queued act on that row's code at that
    /// moment, whether the new code was taken (row 1 had moved off it) or refused.
    func testDeleteAndRightsOfARowWhoseCodeIsQueuedActOnThatRow() async throws {
        let (rig, sc) = try await twoCodes()
        rig.wire.holdResults = true
        try typeAndTab("3333", 0, sc)
        try typeAndTab("1111", 1, sc)
        sc.oscControlBoxes[1].performClick(nil)
        await settle()
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["3333", "1111"])
        XCTAssertEqual(table(rig).rows.map(\.rights.control), [true, false], "row 2's Control is off")
        // Refused this time: row 1 holds 3333.
        sc.window?.makeFirstResponder(nil)
        rig.wire.holdResults = true
        try typeAndTab("4444", 1, sc)
        try typeAndTab("3333", 1, sc)
        sc.oscEditBoxes[1].performClick(nil)
        sc.oscDeleteButtons[1].performClick(nil)
        await settle()
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["3333"], "row 2 is deleted, not row 1")
        XCTAssertEqual(table(rig).rows.first?.rights, .all, "row 1's rights are untouched")
        await assertEventually { self.shown(sc).map(\.code) == ["none", "3333"] }
    }

    /// Row 2's new code 1111 is refused (row 1 has it); Control off and Delete clicked while it was
    /// queued act on row 2 (2222).
    func testActionsOnARowWhoseQueuedCodeIsRefusedActOnTheCodeItKeeps() async throws {
        let (rig, sc) = try await twoCodes()
        rig.wire.holdResults = true
        sc.oscNoneRights[0].performClick(nil)  // an edit on its way holds the ones after it
        try typeAndTab("1111", 1, sc)
        sc.oscControlBoxes[1].performClick(nil)
        await settle()
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["1111", "2222"])
        XCTAssertEqual(table(rig).rows.map(\.rights.control), [true, false])
        rig.wire.holdResults = true
        sc.oscNoneRights[0].performClick(nil)
        try typeAndTab("1111", 1, sc)
        sc.oscDeleteButtons[1].performClick(nil)
        await settle()
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["1111"], "row 2 went")
    }

    /// A rights box clicked in a row whose new code is on its way acts on the new code.
    func testARightsBoxOnACodeJustCommittedActsOnTheNewCode() async throws {
        let (rig, sc) = try await twoCodes()
        rig.wire.holdResults = true
        try typeAndTab("5555", 0, sc)
        sc.oscControlBoxes[0].performClick(nil)
        sc.oscEditBoxes[0].performClick(nil)
        await settle()
        XCTAssertEqual([sc.oscEditBoxes[0].state, sc.oscControlBoxes[0].state], [.off, .off], "as clicked")
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["5555", "2222"])
        XCTAssertEqual(table(rig).rows[0].rights, OscRights(view: true))
        XCTAssertEqual(table(rig).rows[1].rights, .all)
    }

    /// Two codes queued in a row with the caret in and out of the untouched field between them: they
    /// run in order.
    func testACodeThenTheOldOneAgainEndsAtTheOldOneWhenTheAnswersComeLate() async throws {
        let (rig, sc) = try await twoCodes()
        rig.wire.holdResults = true
        try typeAndTab("5555", 0, sc)
        sc.window?.makeFirstResponder(nil)  // the untouched field is left
        try typeAndTab("1111", 0, sc)
        sc.window?.makeFirstResponder(nil)
        await settle()
        release(rig)
        await sc.edits.answered()
        XCTAssertEqual(codes(rig), ["1111", "2222"])
        await assertEventually { self.shown(sc).map(\.code) == ["none", "1111", "2222"] }
        XCTAssertNil(sc.model.message)
    }

    /// A second client of the same workspace.
    private func another(_ rig: Rig) async throws -> Rig {
        let other = Rig(engine: engine, workspace: rig.doc)
        guard await other.connect() else { throw TestEngineError.notConnected }
        await assertEventually { other.store.model != nil }
        return other
    }

    private func rename(_ old: String, to new: String, at index: Int, by rig: Rig) async {
        guard let edit = Command.transaction([.removeOscPasscode(old), .addOscPasscode(new, rights: .all, at: index)]),
            case .success = await rig.store.send(command: edit)
        else { return XCTFail("rename") }
    }

    /// The held answers reach the app, and later ones at once.
    func release(_ rig: Rig) {
        rig.wire.holdResults = false
        rig.wire.releaseResults()
    }

    func testDuplicateCodeRefused() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        let before = commands(rig).count
        let dirty = rig.store.document?.dirty
        try type("2222", into: sc.oscCodeFields[0])
        await assertEventually("the line") { sc.messageLabel.stringValue == loc("settings.osc.duplicate") }
        await assertEventually("the old code is back") { sc.oscCodeFields.first?.stringValue == "1111" }
        XCTAssertEqual(codes(rig), ["1111", "2222"])
        XCTAssertEqual(commands(rig).count, before)
        XCTAssertEqual(rig.store.document?.dirty, dirty)
    }

    func testInvalidCodeRefused() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all)])
        await assertEventually { self.shown(sc).count == 2 }
        for bad in ["abc", "123", "12345", "", "12 4", "١٢٣٤"] {
            sc.model.message = nil
            let before = commands(rig).count
            try type(bad, into: sc.oscCodeFields[0])
            await assertEventually("\"\(bad)\": the line") {
                sc.messageLabel.stringValue == loc("settings.osc.invalid")
            }
            await assertEventually("\"\(bad)\": the old code") { sc.oscCodeFields.first?.stringValue == "1111" }
            XCTAssertEqual(codes(rig), ["1111"], bad)
            XCTAssertEqual(commands(rig).count, before, "\(bad): nothing sent")
        }
    }

    /// Return through the window's key-equivalent path (where the Done button's Return lives)
    /// commits the code and leaves the window open.
    func testReturnInACodeFieldCommitsAndKeepsTheWindowOpen() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        let window = try XCTUnwrap(sc.window)
        await clearRows(rig)
        await addRows(rig, [("1111", .all)])
        await assertEventually { self.shown(sc).count == 2 }
        window.makeFirstResponder(sc.oscCodeFields[0])
        let editor = try XCTUnwrap(sc.oscCodeFields[0].currentEditor(), "being edited")
        editor.string = "4821"
        let event = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36))
        if !window.performKeyEquivalent(with: event) { window.sendEvent(event) }
        await assertEventually("the new code") { self.codes(rig) == ["4821"] }
        XCTAssertTrue(window.isVisible, "Return did not press Done")
        await assertEventually("redrawn with focus released") { sc.oscCodeFields.first?.currentEditor() == nil }
    }

    func testARefusedCodeIsRestoredWhenAnotherCodeFieldTakesTheFocus() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        let window = try XCTUnwrap(sc.window)
        window.makeFirstResponder(sc.oscCodeFields[0])
        sc.oscCodeFields[0].currentEditor()?.string = "2222"
        window.makeFirstResponder(sc.oscCodeFields[1])  // clicking into the next code
        await assertEventually("the line") { sc.messageLabel.stringValue == loc("settings.osc.duplicate") }
        await assertEventually("old code shown") { sc.oscCodeFields.first?.stringValue == "1111" }
        XCTAssertEqual(codes(rig), ["1111", "2222"])
    }

    func testANewRowIsScrolledIntoView() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await addFreshRows(rig, 11, from: 3001)
        await assertEventually { self.shown(sc).count == 13 }
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        sc.oscAddButton.performClick(nil)
        await assertEventually("the 13th passcode") { self.shown(sc).count == 14 }
        let last = try XCTUnwrap(sc.oscCodeFields.last)
        await assertEventually("scrolled to it") { last.visibleRect.height >= last.bounds.height - 1 }
    }

    /// A scenario's scroll step on the full table scrolls it to the lower rows. (The step used to call
    /// the scroll view's own `scrollWheel`, whose tracking loop ran queued main-actor work inside the
    /// async step and aborted the test host.)
    func testAScenarioScrollOfAFullOscTableShowsTheLowerRows() async throws {
        let adds = Array(repeating: #"{"click": "settings-osc-add"}"#, count: 15).joined(separator: ",")
        let json = """
            {"show": "new", "steps": [{"focus": "settings"}, {"tab": "osc"}, \(adds),
            {"scroll": "settings-osc-table", "dy": -400, "at": [0.5, 0.7]}, {"shot": "scrolled"}]}
            """
        let out = engine.folder.appendingPathComponent("scroll-shots", isDirectory: true)
        let runner = ScenarioRunner(scenario: try ScenarioRunner.parse(Data(json.utf8), base: engine.folder), out: out)
        try await runner.run()
        XCTAssertEqual(runner.written.count, 1)
        let log = try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
        XCTAssertTrue(log.contains("scrolled settings-osc-table to y "), log)
        XCTAssertFalse(log.contains("to y 0\n"), "the table moved: \(log)")
    }

    /// The view with accessibility identifier `id` in the settings window.
    private func view(_ id: String, in sc: WorkspaceSettingsWindowController) -> NSView? {
        func find(_ v: NSView) -> NSView? {
            if v.accessibilityIdentifier() == id { return v }
            for sub in v.subviews { if let hit = find(sub) { return hit } }
            return nil
        }
        return sc.window?.contentView.flatMap(find)
    }

    func testTheColumnTitlesStayAboveTheScrollingRows() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        // The table already holds a code in the range filled below, as a new workspace's random
        // one sometimes does.
        await clearRows(rig)
        await addRows(rig, [("3005", .all)])
        await addFreshRows(rig, 15, from: 3001)
        await assertEventually { self.shown(sc).count == 17 }
        sc.window?.contentView?.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(view("settings-osc-table", in: sc) as? NSScrollView)
        scroll.contentView.scroll(to: NSPoint(x: 0, y: 10_000))
        scroll.reflectScrolledClipView(scroll.contentView)
        func titles(_ root: NSView) -> [NSTextField] {
            ((root as? NSTextField).map { [$0] } ?? []) + root.subviews.flatMap(titles)
        }
        let view = try XCTUnwrap(titles(try XCTUnwrap(sc.window?.contentView)).first { $0.stringValue == "Edit" })
        XCTAssertFalse(view.isDescendant(of: scroll), "the titles are not in the scrolling area")
        XCTAssertFalse(view.isHiddenOrHasHiddenAncestor)
        XCTAssertFalse(view.visibleRect.isEmpty, "still showing with the table scrolled to its last row")
    }

    func testARefusalLineGoesWithItsTabAndTheWindow() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        try type("2222", into: sc.oscCodeFields[0])
        await assertEventually("the line") { sc.messageLabel.stringValue == loc("settings.osc.duplicate") }
        sc.tabs.selectTabViewItem(withIdentifier: "general")
        XCTAssertEqual(sc.messageLabel.stringValue, "", "gone on the General tab")
        sc.tabs.selectTabViewItem(withIdentifier: "osc")
        try type("2222", into: sc.oscCodeFields[0])
        let came = await eventually(timeout: 3) { sc.messageLabel.stringValue == loc("settings.osc.duplicate") }
        let dbg = "\(sc.oscCodeFields.map(\.stringValue)) \(sc.model.message ?? "nil")"
        XCTAssertTrue(came, "the line again: \(dbg) label \(sc.messageLabel.stringValue)")
        sc.window?.close()
        await wc.showSettings().value
        XCTAssertEqual(sc.messageLabel.stringValue, "", "gone after Done and reopening")
    }

    func testDoneStaysAtTheBottomRightAndTheTableGrowsWithThePane() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", .all)])
        await assertEventually { self.shown(sc).count == 3 }
        let window = try XCTUnwrap(sc.window)
        func done() -> NSRect {
            window.contentView?.layoutSubtreeIfNeeded()
            return sc.doneButton.convert(sc.doneButton.bounds, to: nil)
        }
        let quiet = done()
        try type("2222", into: sc.oscCodeFields[0])
        await assertEventually("the line") { sc.messageLabel.stringValue == loc("settings.osc.duplicate") }
        XCTAssertEqual(done(), quiet, "Done does not move when a line shows")
        sc.tabs.selectTabViewItem(withIdentifier: "general")
        XCTAssertEqual(done(), quiet)
        sc.tabs.selectTabViewItem(withIdentifier: "osc")
        XCTAssertEqual(done().maxX, (window.contentView?.bounds.maxX ?? 0) - 12, accuracy: 1, "at the right edge")
        let scroll = try XCTUnwrap(view("settings-osc-table", in: sc))
        window.contentView?.layoutSubtreeIfNeeded()
        let before = scroll.frame.height
        let contentBefore = try XCTUnwrap(window.contentView?.frame.height)
        var size = try XCTUnwrap(window.contentView?.frame.size)
        size.height += 300
        window.setContentSize(size)  // the system clamps it to the screen: measure what was applied
        for _ in 0..<3 { window.contentView?.layoutSubtreeIfNeeded() }
        let applied = try XCTUnwrap(window.contentView?.frame.height) - contentBefore
        XCTAssertGreaterThan(applied, 50, "the screen has room for a taller window")
        XCTAssertGreaterThan(
            scroll.frame.height, before + applied - 30,
            "the table takes the height the pane gives it"
        )
    }

    /// A line about a refused number goes once the field holds the engine's value again.
    func testARefusedNumbersLineGoesWhenTheShownValueIsTypedAgain() async throws {
        let (_, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        sc.tabs.selectTabViewItem(withIdentifier: "general")
        let shownValue = sc.panicField.stringValue
        try type("abc", into: sc.panicField)
        await assertEventually("the line") { !sc.messageLabel.stringValue.isEmpty }
        try type(shownValue, into: sc.panicField)
        await assertEventually("the line is gone") { sc.messageLabel.stringValue.isEmpty }
        XCTAssertEqual(sc.panicField.stringValue, shownValue)
    }

    func testEditAndControlNeedView() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", OscRights.none)])
        await assertEventually { self.shown(sc).count == 2 }
        XCTAssertEqual(shown(sc)[1].enabled, [true, false, false])
        let before = commands(rig).count
        sc.oscEditBoxes[0].performClick(nil)
        sc.oscControlBoxes[0].performClick(nil)
        XCTAssertEqual(shown(sc)[1].on, [false, false, false], "greyed boxes cannot be clicked")
        XCTAssertEqual(commands(rig).count, before)
        sc.oscViewBoxes[0].performClick(nil)
        await assertEventually("Edit and Control available") { self.shown(sc)[1].enabled == [true, true, true] }
        XCTAssertEqual(shown(sc)[1].on, [true, false, false], "still off")
        sc.oscEditBoxes[0].performClick(nil)
        await assertEventually("only Edit on") { self.shown(sc)[1].on == [true, true, false] }
        // The model refuses it too, without a command.
        let sent = commands(rig).count
        let refused = await sc.model.setOscRight(\.control, true, of: "no such row")
        XCTAssertFalse(refused)
        XCTAssertEqual(commands(rig).count, sent)
    }

    func testUncheckingViewClearsEditAndControl() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        XCTAssertEqual(shown(sc)[1].on, [true, true, true])
        let before = oscCommands(rig).count
        sc.oscViewBoxes[0].performClick(nil)
        await assertEventually("all off") { self.shown(sc)[1].on == [false, false, false] }
        XCTAssertEqual(shown(sc)[1].enabled, [true, false, false])
        XCTAssertEqual(oscCommands(rig).count, before + 1, "one command")
        XCTAssertEqual(table(rig).rows[0].rights, .none)
        sc.oscViewBoxes[0].performClick(nil)
        await assertEventually("View on") { self.shown(sc)[1].on[0] }
        XCTAssertEqual(shown(sc)[1].on, [true, false, false], "Edit and Control stay off")
    }

    func testNoPasscodeRow() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        XCTAssertEqual(shown(sc)[0].on, [false, false, false])
        sc.oscNoneRights[0].performClick(nil)
        await assertEventually("Edit and Control available") { self.shown(sc)[0].enabled == [true, true, true] }
        sc.oscNoneRights[2].performClick(nil)
        await assertEventually("View and Control on") {
            self.table(rig).noPasscode.control && self.shown(sc)[0].on == [true, false, true]
        }
        XCTAssertEqual(shown(sc)[0].on, [true, false, true])
        XCTAssertEqual(table(rig).noPasscode, OscRights(view: true, edit: false, control: true))
        XCTAssertFalse(sc.oscNoneLabel.isEditable, "the label cannot be edited")
        XCTAssertFalse(sc.oscGrid.cell(atColumnIndex: 4, rowIndex: 0).contentView is NSButton, "no delete")
    }

    func testDeleteARow() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await clearRows(rig)
        await addRows(rig, [("1111", .all), ("2222", OscRights(view: true))])
        await assertEventually { self.shown(sc).count == 3 }
        sc.oscDeleteButtons[0].performClick(nil)
        await assertEventually("only the other row") { self.shown(sc).map(\.code) == ["none", "2222"] }
        XCTAssertEqual(shown(sc)[1].on, [true, false, false])
        sc.oscDeleteButtons[0].performClick(nil)
        await assertEventually("just the fixed row") { self.shown(sc).count == 1 }
        XCTAssertNil(sc.window?.attachedSheet)
        XCTAssertNil(NSApp.modalWindow, "no alert")
        XCTAssertEqual(codes(rig), [])
    }

    func testUndoAndRedo() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        let delegate = menu(wc, rig)
        let undo = NSMenuItem(title: "", action: #selector(AppDelegate.undoEdit(_:)), keyEquivalent: "z")
        let redo = NSMenuItem(title: "", action: #selector(AppDelegate.redoEdit(_:)), keyEquivalent: "Z")

        /// One edit, then Undo brings back the table before it and Redo the table after it.
        func cycle(_ what: String, _ edit: () async throws -> Void) async throws {
            let before = shown(sc)
            try await edit()
            await assertEventually("\(what): changed") { self.shown(sc) != before }
            let after = shown(sc)
            await assertEventually("\(what): Undo is on") { delegate.validateMenuItem(undo) }
            delegate.undoEdit(nil)
            await assertEventually("\(what): undone") { self.shown(sc) == before }
            await assertEventually("\(what): Redo is on") { delegate.validateMenuItem(redo) }
            delegate.redoEdit(nil)
            await assertEventually("\(what): redone") { self.shown(sc) == after }
        }

        try await cycle("a new passcode") { sc.oscAddButton.performClick(nil) }
        XCTAssertEqual(shown(sc).count, 3)
        let oldCode = shown(sc)[1].code
        try await cycle("a changed code") { try self.type("4821", into: sc.oscCodeFields[0]) }
        XCTAssertEqual(shown(sc)[1].code, "4821")
        XCTAssertNotEqual(oldCode, "4821")
        try await cycle("a right") { sc.oscControlBoxes[1].performClick(nil) }
        XCTAssertFalse(shown(sc)[2].on[2])
        // A delete comes back at its place with its code and rights.
        let beforeDelete = shown(sc)
        sc.oscDeleteButtons[0].performClick(nil)
        await assertEventually("deleted") { self.shown(sc).count == 2 }
        delegate.undoEdit(nil)
        await assertEventually("the deleted row is back at its place") { self.shown(sc) == beforeDelete }
        delegate.redoEdit(nil)
        await assertEventually("deleted again") { self.shown(sc).count == 2 }
    }

    func testSurvivesSaveAndReopen() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        sc.oscAddButton.performClick(nil)
        await assertEventually { self.shown(sc).count == 3 }
        try type("4821", into: sc.oscCodeFields[0])
        await assertEventually { self.shown(sc)[1].code == "4821" }
        sc.oscNoneRights[0].performClick(nil)  // No Passcode: View only
        await assertEventually { self.shown(sc)[0].on == [true, false, false] }
        sc.oscEditBoxes[1].performClick(nil)
        await assertEventually { self.shown(sc)[2].on == [true, false, true] }
        sc.oscControlBoxes[1].performClick(nil)  // the second row: View only
        await assertEventually("every edit reached the engine") {
            self.table(rig).rows[1].rights == OscRights(view: true) && self.shown(sc)[2].on == [true, false, false]
        }
        let expected = shown(sc)
        let path = engine.folder.appendingPathComponent("osc-\(UUID().uuidString).openlab").path
        guard case .success = await rig.store.saveAs(path: path) else { return XCTFail("save") }
        await assertEventually("saved") { rig.store.document?.dirty == false }
        let closed = await rig.testEngine.app.send({ .close(id: $0, doc: rig.doc, discardChanges: false) })
        guard case .success = closed else { return XCTFail("close: \(closed)") }
        guard case .success(let ok) = await engine.request(.open(path: path)), case .opened(let doc)? = ok.data else {
            return XCTFail("reopen")
        }
        let reopened = Rig(engine: engine, workspace: doc)
        guard await reopened.connect() else { return XCTFail("reconnect") }
        await assertEventually { reopened.store.model != nil }
        let again = WorkspaceSettingsWindowController(store: reopened.store)
        await again.present().value
        XCTAssertEqual(shown(again), expected, "the same rows, codes and rights in the same order")
        XCTAssertEqual(reopened.store.document?.dirty, false)
    }

    func testShowMode() async throws {
        let (rig, wc) = try await newWorkspace()
        let sc = wc.settingsWindow
        await rig.store.setShowMode(true).value
        await assertEventually("Show Mode") { wc.isShowMode }
        sc.oscAddButton.performClick(nil)
        await assertEventually("a new passcode") { self.shown(sc).count == 3 }
        try type("4821", into: sc.oscCodeFields[0])
        await assertEventually("a code edit") { self.shown(sc)[1].code == "4821" }
        sc.oscViewBoxes[1].performClick(nil)
        await assertEventually("a right") { self.shown(sc)[2].on == [false, false, false] }
        sc.oscDeleteButtons[1].performClick(nil)
        await assertEventually("a delete") { self.shown(sc).count == 2 }
        XCTAssertTrue(sc.messageLabel.stringValue.isEmpty, sc.messageLabel.stringValue)
        XCTAssertNil(sc.window?.attachedSheet)
        XCTAssertNil(NSApp.modalWindow)
        XCTAssertTrue(wc.isShowMode, "still in Show Mode")
    }

    func testOneTablePerWorkspace() async throws {
        let (rigA, wcA) = try await newWorkspace()
        let (rigB, wcB) = try await newWorkspace()
        wcA.workspaceName = "First"
        wcB.workspaceName = "Second"
        await wcA.showSettings().value
        await wcB.showSettings().value
        let beforeB = shown(wcB.settingsWindow)
        wcA.settingsWindow.oscAddButton.performClick(nil)
        await assertEventually("a row in the first") { self.shown(wcA.settingsWindow).count == 3 }
        try await rigB.advance(0.1)
        XCTAssertEqual(shown(wcB.settingsWindow), beforeB, "the second is unchanged")
        XCTAssertEqual(codes(rigB).count, 1)
        XCTAssertEqual(codes(rigA).count, 2)
        XCTAssertEqual(wcA.settingsWindow.window?.title, String(format: loc("settings.title.workspace"), "First"))
        XCTAssertEqual(wcB.settingsWindow.window?.title, String(format: loc("settings.title.workspace"), "Second"))
    }

    // MARK: Logic

    /// A passcode row keeps its identity through a change of its code made in one step in its place;
    /// anything else with an unknown code is a new row.
    func testAPasscodeRowKeepsItsIdentityThroughARenameOnly() {
        func table(_ codes: [String], _ rights: OscRights = .all) -> OscAccessTable {
            var t = OscAccessTable()
            t.rows = codes.map { OscAccessTable.Row(passcode: $0, rights: rights) }
            return t
        }
        var rows = OscRows()
        rows.take(table(["1111", "2222"]), oneStep: false)
        let ids = rows.rows.map(\.id)
        func taking(_ codes: [String], oneStep: Bool, _ rights: OscRights = .all) -> [String] {
            var copy = rows
            copy.take(table(codes, rights), oneStep: oneStep)
            return copy.rows.map(\.id)
        }
        XCTAssertEqual(taking(["3333", "2222"], oneStep: true), ids, "renamed")
        XCTAssertEqual(taking(["2222", "1111"], oneStep: true), ids.reversed(), "moved")
        XCTAssertEqual(taking(["2222"], oneStep: true), [ids[1]], "removed")
        XCTAssertEqual(taking(["2222"], oneStep: false), [ids[1]], "removed, a snapshot")
        // A snapshot may hold several changes; another place, other rights or two codes are no rename.
        for (codes, oneStep, rights) in [
            (["3333", "2222"], false, OscRights.all), (["2222", "3333"], true, .all),
            (["3333", "2222"], true, OscRights(view: true)), (["3333", "4444"], true, .all),
        ] {
            let now = taking(codes, oneStep: oneStep, rights)
            XCTAssertFalse(now.contains(ids[0]), "\(codes): the row that went is not another one now")
        }
        // A code that comes back after its row went is a new row.
        rows.take(table(["2222"]), oneStep: true)
        rows.take(table(["1111", "2222"]), oneStep: true)
        XCTAssertNotEqual(rows.rows[0].id, ids[0])
    }

    func testAPasscodeIsExactlyFourAsciiDigits() {
        for ok in ["1000", "9999", "0123"] { XCTAssertTrue(OscAccessTable.isValidPasscode(ok), ok) }
        for bad in ["", "123", "12345", "abcd", "12 4", "١٢٣٤", "12\n4", " 123"] {
            XCTAssertFalse(OscAccessTable.isValidPasscode(bad), bad)
        }
    }
}
