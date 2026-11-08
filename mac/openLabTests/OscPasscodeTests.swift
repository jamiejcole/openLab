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
            guard case .success = await rig.store.send(command: .addOscPasscode(code, rights: rights, at: at)) else {
                return XCTFail("add \(code)")
            }
        }
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
        await addRows(rig, (1...15).map { (String(2000 + $0), .all) })
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
        await addRows(rig, (1...11).map { (String(3000 + $0), .all) })
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
        await addRows(rig, (1...15).map { (String(3000 + $0), .all) })
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
        var size = try XCTUnwrap(window.contentView?.frame.size)
        size.height += 300
        window.setContentSize(size)
        for _ in 0..<3 { window.contentView?.layoutSubtreeIfNeeded() }
        XCTAssertGreaterThan(
            scroll.frame.height, before + 250,
            "the table takes the height the pane gives it"
        )
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
        let refused = await sc.model.setOscRights(.all, of: "9999")
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
        let closed = await rig.client.send({ .close(id: $0, doc: rig.doc, discardChanges: false) })
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

    func testAPasscodeIsExactlyFourAsciiDigits() {
        for ok in ["1000", "9999", "0123"] { XCTAssertTrue(OscAccessTable.isValidPasscode(ok), ok) }
        for bad in ["", "123", "12345", "abcd", "12 4", "١٢٣٤", "12\n4", " 123"] {
            XCTAssertFalse(OscAccessTable.isValidPasscode(bad), bad)
        }
    }
}
