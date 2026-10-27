// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class ScriptedAlerts: WorkspaceAlerts {
    var closePlaying = true
    var quitPlaying = true
    var quitDisconnected = false
    var saveChoices: [SaveChoice] = []
    private(set) var log: [String] = []
    private(set) var errors: [String] = []

    func confirmCloseWhilePlaying(workspace: String) -> Bool {
        log.append("closePlaying")
        return closePlaying
    }

    func confirmQuitWhilePlaying() -> Bool {
        log.append("quitPlaying")
        return quitPlaying
    }

    func confirmQuitDisconnected() -> Bool {
        log.append("quitDisconnected")
        return quitDisconnected
    }

    func unsavedChanges(workspace: String) -> SaveChoice {
        log.append("unsaved:\(workspace)")
        return saveChoices.isEmpty ? .cancel : saveChoices.removeFirst()
    }

    func showError(_ message: String) { errors.append(message) }
}

@MainActor
final class AppControllerTests: XCTestCase {
    private var app: AppController!
    private var engine: FakeEngine!
    private var alerts: ScriptedAlerts!
    private var recents: [URL] = []
    private var keyDoc: ModelID?
    private var saveURL: URL?
    private var openURL: URL?

    private let sessionScheduler = ManualScheduler()
    private let quitScheduler = ManualScheduler()

    override func setUp() async throws { try await build(.multi) }

    private func build(_ mode: FakeEngine.Mode) async throws {
        app?.closeAllWindows()
        engine = FakeEngine(mode: mode, snapshot: try Fixtures.json("server", "welcome")["snapshot"])
        let info = engine.info
        let session = EngineSession(transport: engine, infoProvider: { .success(info) }, scheduler: sessionScheduler)
        app = AppController(
            session: session, scheduler: quitScheduler)
        alerts = ScriptedAlerts()
        app.alerts = alerts
        app.noteRecent = { [unowned self] in recents.append($0) }
        app.recentURLs = { [unowned self] in recents }
        app.keyWindow = { [unowned self] in keyDoc.flatMap { app.windows[$0]?.window } }
        app.pickSaveURL = { [unowned self] _ in saveURL }
        app.pickOpenURL = { [unowned self] in openURL }
        session.start()
        await assertEventually { app.windows.count == 1 && session.stores.values.allSatisfy { $0.model != nil } }
    }

    override func tearDown() async throws {
        app.closeAllWindows()
        app = nil
    }

    static func hotkey(_ key: String, _ mods: [String] = []) -> String {
        let flags = ["shift", "control", "alt", "command"].map { "\"\($0)\":\(mods.contains($0))" }.joined(
            separator: ",")
        return #"{"key":"\#(key)","modifiers":{\#(flags)}}"#
    }

    /// Replaces the workspace's `settings.hotkeys` as the engine would (a `settings` upsert).
    private func setHotkeys(_ doc: ModelID, _ hotkeys: String) async throws {
        let store = try XCTUnwrap(app.session.stores[doc])
        var settings = try XCTUnwrap(store.model?.settings)
        settings.set("hotkeys", try XCTUnwrap(JSONValue.parse(hotkeys)))
        let name = store.model?.name ?? ""
        let upsert = #"{"type":"settings","name":"\#(name)","settings":\#(try settings.text())}"#
        engine.push(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[\#(upsert)],"deletes":[]}"#, doc: doc
        )
        await assertEventually { store.model?.settings == settings }
    }

    /// A second, clean workspace.
    private func addWorkspace() async throws -> ModelID {
        guard case .success(let doc) = await app.session.newWorkspace() else {
            XCTFail("new")
            return "none"
        }
        await assertEventually { app.windows[doc] != nil && app.session.stores[doc]?.model != nil }
        return doc
    }

    private func makeDirty(_ doc: ModelID) async {
        let document = #"{"path":null,"dirty":true,"can_undo":false,"can_redo":false}"#
        engine.push(#"{"type":"document","document":\#(document)}"#, doc: doc)
        await assertEventually { app.session.stores[doc]?.document?.dirty == true }
    }

    private func closeMessages(_ doc: ModelID? = nil) -> [(doc: ModelID?, discard: Bool)] {
        engine.receivedFrom.compactMap {
            if case .provisional(.close(_, let d, let discard)) = $0.message { return (d, discard) }
            return nil
        }
    }

    private func transports(_ action: TransportAction) -> [ModelID?] {
        engine.receivedFrom.compactMap {
            if case .transport(_, let a) = $0.message, a == action { return $0.doc }
            return nil
        }
    }

    // MARK: Windows follow the session

    func testOneWindowPerWorkspaceAndRemovedWhenEngineClosesIt() async throws {
        XCTAssertEqual(Set(app.windows.keys), ["doc-1"])
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)
        let second = try await addWorkspace()
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertTrue(app.windows["doc-1"]?.store === app.session.stores["doc-1"])
        XCTAssertTrue(app.windows[second]?.store === app.session.stores[second])
        XCTAssertTrue(app.windows["doc-1"]?.store !== app.windows[second]?.store)
        XCTAssertEqual(app.windows["doc-1"]?.window?.title, "Test")

        _ = await app.session.close(doc: second, discardChanges: true)
        await assertEventually { app.windows.count == 1 }
        XCTAssertNil(app.windows[second])
        XCTAssertEqual(app.windows["doc-1"]?.window?.isVisible, true)
    }

    func testNewAndOpenAddWindowsWhileTheFirstKeepsRunning() async throws {
        openURL = URL(fileURLWithPath: "/Shows/Macbeth.openlab")
        await app.newWorkspace()
        await app.openWorkspace()
        await assertEventually { app.windows.count == 3 && app.session.stores.values.allSatisfy { $0.model != nil } }
        XCTAssertEqual(closeMessages().count, 0)
        XCTAssertEqual(app.windows["doc-1"]?.window?.isVisible, true)
        XCTAssertEqual(recents, [openURL])
        XCTAssertTrue(alerts.log.isEmpty)
    }

    func testRecentIsNotNotedWhenOpenFailsAndAlreadyOpenFrontsTheWindow() async throws {
        let url = URL(fileURLWithPath: "/Shows/A.openlab")
        await app.openWorkspace(url: url)
        await assertEventually { app.windows.count == 2 }
        recents = []
        app.windows["doc-1"]?.window?.makeKeyAndOrderFront(nil)
        await app.openWorkspace(url: url)
        XCTAssertTrue(recents.isEmpty)
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertTrue(alerts.errors.isEmpty)
    }

    func testNoRevertInTheMenus() {
        let titles = MainMenu.make().bar.items.flatMap {
            $0.submenu?.items.map(\.title) ?? []
        }
        XCTAssertFalse(titles.contains { $0.localizedCaseInsensitiveContains("revert") })
        XCTAssertEqual(
            MainMenu.make().bar.items[1].submenu?.items.map(
                \.keyEquivalent), ["n", "o", "", "w"])
    }

    // MARK: Front-window routing

    func testGoAndPauseAllAddressTheKeyWindowsWorkspace() async throws {
        let second = try await addWorkspace()
        keyDoc = second
        let go = await app.goFront()
        XCTAssertNotNil(go)
        _ = await app.pauseAllFront()
        XCTAssertEqual(transports(.go(list: "00000000-0000-0000-0000-000000000001")), [second])
        XCTAssertEqual(transports(.pauseAll), [second])

        keyDoc = "doc-1"
        _ = await app.goFront()
        XCTAssertEqual(transports(.go(list: "00000000-0000-0000-0000-000000000001")), [second, "doc-1"])

        keyDoc = nil  // no workspace window is key: nothing is sent
        let none = await app.goFront()
        XCTAssertNil(none)
        XCTAssertEqual(transports(.go(list: "00000000-0000-0000-0000-000000000001")).count, 2)
    }

    func testWindowButtonsAddressTheirOwnStoreNotTheKeyWindow() async throws {
        let second = try await addWorkspace()
        keyDoc = "doc-1"
        app.windows[second]?.masthead.goButton.performClick(nil)
        await assertEventually { !transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty }
        XCTAssertEqual(transports(.go(list: "00000000-0000-0000-0000-000000000001")), [second])
        let pause = try XCTUnwrap(app.windows[second]?.sidebar.transportButtons[AX.transportPause])
        pause.performClick(nil)
        await assertEventually { !transports(.pauseAll).isEmpty }
        XCTAssertEqual(transports(.pauseAll), [second])
    }

    func testPanicButtonIsAppScope() async throws {
        _ = try await addWorkspace()
        app.windows["doc-1"]?.sidebar.transportButtons[AX.transportPanic]?.performClick(nil)
        await assertEventually { transports(.panic).count == 1 }
        // Sent on the unbound app connection, not a workspace's.
        XCTAssertEqual(transports(.panic).map { $0 == nil }, [true])
    }

    private func escEvent(flags: NSEvent.ModifierFlags = [], keyCode: UInt16 = 53) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
                characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false, keyCode: keyCode))
    }

    func testEscPanicsOnceWithLauncherKeyAndWithATextFieldEditing() async throws {
        app.showLauncher()
        let event = try escEvent()
        XCTAssertTrue(app.handleKeyDown(event) === event)  // passed on: editors and alerts still see it
        await assertEventually { transports(.panic).count == 1 }
        await settle()
        XCTAssertEqual(transports(.panic).count, 1)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 50), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        let field = NSTextField(string: "x")
        window.contentView = field
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(field))
        XCTAssertNotNil(field.currentEditor())
        _ = app.handleKeyDown(try escEvent())
        await assertEventually { transports(.panic).count == 2 }
        window.close()
        XCTAssertEqual(transports(.panic).map { $0 == nil }, [true, true])
    }

    func testOnlyPlainEscPanics() async throws {
        _ = app.handleKeyDown(try escEvent(flags: .command))
        _ = app.handleKeyDown(try escEvent(keyCode: 0))
        await settle()
        XCTAssertTrue(transports(.panic).isEmpty)
    }

    // MARK: Transport commands

    func testEveryCommandSendsItsMessageToTheKeyWindowsWorkspace() async throws {
        let second = try await addWorkspace()
        keyDoc = second
        let list: ModelID = "00000000-0000-0000-0000-000000000001"
        for (command, action) in [
            (TransportCommand.go, TransportAction.go(list: list)), (.pauseAll, .pauseAll), (.resumeAll, .resumeAll),
            (.playheadNext, .playheadNext(list: list)), (.playheadPrevious, .playheadPrevious(list: list)),
        ] {
            await app.perform(command)
            XCTAssertEqual(transports(action), [second], "\(command)")
        }
        keyDoc = "doc-1"
        await app.perform(.pauseAll)
        XCTAssertEqual(transports(.pauseAll), [second, "doc-1"])
    }

    func testResumeAllButtonAddressesItsOwnStore() async throws {
        let second = try await addWorkspace()
        keyDoc = "doc-1"
        app.windows[second]?.sidebar.transportButtons[AX.transportResume]?.performClick(nil)
        await assertEventually { !transports(.resumeAll).isEmpty }
        XCTAssertEqual(transports(.resumeAll), [second])
    }

    func testPanicCommandIsAppScopeAndNeedsNoWorkspaceWindow() async throws {
        keyDoc = nil
        XCTAssertTrue(app.canPerform(.panic))
        XCTAssertFalse(app.canPerform(.go))
        await app.perform(.panic)
        XCTAssertEqual(transports(.panic).map { $0 == nil }, [true])
    }

    func testStopAllSendsStopAllToTheKeyWindowsWorkspaceOnly() async throws {
        let second = try await addWorkspace()
        keyDoc = second
        XCTAssertTrue(app.canPerform(.stopAll))
        await app.perform(.stopAll)
        XCTAssertEqual(transports(.stopAll), [second])
        XCTAssertTrue(transports(.hardStop(cue: "00000000-0000-0000-0000-00000000000a")).isEmpty, "no per-cue loop")
    }

    func testStopAllKeyFromTheShowsHotkeysDispatches() async throws {
        keyDoc = "doc-1"
        try await setHotkeys("doc-1", #"{"stop_all":\#(Self.hotkey("s"))}"#)
        XCTAssertNil(app.handleKeyDown(try keyEvent("s")))
        await assertEventually { !transports(.stopAll).isEmpty }
        XCTAssertEqual(transports(.stopAll), ["doc-1"])
    }

    func testEscStaysPanicNotStopAll() async throws {
        keyDoc = "doc-1"
        _ = app.handleKeyDown(try escEvent())
        await assertEventually { transports(.panic).count == 1 }
        XCTAssertTrue(transports(.stopAll).isEmpty)
    }

    func testFailedTransportBeepsAndLogsInsteadOfBeingDropped() async throws {
        keyDoc = "doc-1"
        var beeps = 0
        app.beep = { beeps += 1 }
        let store = try XCTUnwrap(app.session.stores["doc-1"])
        engine.dropConnections()
        await app.perform(.pauseAll)
        XCTAssertEqual(beeps, 1)
        let errors = store.logEntries.filter { $0.level == .error }.map(\.message)
        XCTAssertEqual(errors.count, 1, "\(errors)")
        XCTAssertTrue(errors.first?.hasPrefix("Transport command failed") == true, "\(errors)")
    }

    func testSuccessfulTransportDoesNotBeep() async throws {
        keyDoc = "doc-1"
        var beeps = 0
        app.beep = { beeps += 1 }
        await app.perform(.pauseAll)
        XCTAssertEqual(beeps, 0)
    }

    func testPlayheadSendsTheFinalSelectionEvenBeforeTheEchoArrives() async throws {
        let wc = try XCTUnwrap(app.windows["doc-1"])
        let a: ModelID = "00000000-0000-0000-0000-00000000000a"
        let b: ModelID = "00000000-0000-0000-0000-00000000000b"  // the playhead already
        let list: ModelID = "00000000-0000-0000-0000-000000000001"
        func click(_ id: ModelID) {
            let row = wc.cueList.outlineView.row(forItem: wc.cueList.node(id))
            wc.cueList.outlineView.selectRowIndexes([row], byExtendingSelection: false)
        }
        click(a)
        click(b)  // back on the cue the (not yet moved) playhead is on
        await assertEventually { !transports(.setPlayhead(list: list, cue: b)).isEmpty }
        XCTAssertEqual(transports(.setPlayhead(list: list, cue: a)).count, 1)
        XCTAssertEqual(transports(.setPlayhead(list: list, cue: b)).count, 1)
    }

    func testKeyCacheIsEvictedWhenAWorkspaceCloses() async throws {
        let second = try await addWorkspace()
        let before = app.keyCacheCount
        XCTAssertGreaterThanOrEqual(before, 2)
        _ = await app.session.close(doc: second, discardChanges: true)
        await assertEventually { app.windows[second] == nil }
        XCTAssertEqual(app.keyCacheCount, before - 1)
    }

    func testHotkeyWarningsAreLoggedWhenTheSettingsArriveNotAtTheFirstKeyPress() async throws {
        let store = try XCTUnwrap(app.session.stores["doc-1"])
        let hk = Self.hotkey("x")
        try await setHotkeys("doc-1", #"{"go":\#(hk),"pause_all":\#(hk)}"#)
        await assertEventually { store.logEntries.contains { $0.level == .warn } }
        XCTAssertEqual(store.logEntries.filter { $0.level == .warn }.count, 1)
    }

    func testSelectedCueCommandsUseTheSelection() async throws {
        keyDoc = "doc-1"
        let wc = try XCTUnwrap(app.windows["doc-1"])
        let running: ModelID = "00000000-0000-0000-0000-00000000000a"
        wc.cueList.select([running])
        XCTAssertEqual(wc.cueList.selectedIDs(), [running])
        await app.perform(.pauseResumeSelected)  // running -> pause
        await app.perform(.startSelected)
        await app.perform(.stopSelected)
        await app.perform(.resetSelected)
        await app.perform(.loadSelected)
        XCTAssertEqual(transports(.pause(cue: running)), ["doc-1"])
        XCTAssertEqual(transports(.start(cue: running)), ["doc-1"])
        XCTAssertEqual(transports(.hardStop(cue: running)), ["doc-1"])
        XCTAssertEqual(transports(.reset(cue: running)), ["doc-1"])
        XCTAssertEqual(transports(.load(cue: running, at: 0)), ["doc-1"])
        wc.cueList.select([])  // nothing selected: nothing sent
        await app.perform(.startSelected)
        XCTAssertEqual(transports(.start(cue: running)).count, 1)
    }

    func testSelectingByHandMovesThePlayheadButRestoringDoesNot() async throws {
        let wc = try XCTUnwrap(app.windows["doc-1"])
        let target: ModelID = "00000000-0000-0000-0000-00000000000a"
        let action = TransportAction.setPlayhead(list: "00000000-0000-0000-0000-000000000001", cue: target)
        wc.cueList.select([target])  // programmatic: no message
        await settle()
        XCTAssertTrue(transports(action).isEmpty)
        wc.cueList.outlineView.deselectAll(nil)
        let row = wc.cueList.outlineView.row(forItem: wc.cueList.node(target))
        wc.cueList.outlineView.selectRowIndexes([row], byExtendingSelection: false)  // as a click does
        await assertEventually { !transports(action).isEmpty }
    }

    func testNoReadOnlyTextViewCanTakeFocusAndSwallowTransportKeys() throws {
        let wc = try XCTUnwrap(app.windows["doc-1"])
        XCTAssertFalse(wc.masthead.notesView.acceptsFirstResponder)
        XCTAssertTrue(wc.window?.initialFirstResponder === wc.cueList.outlineView)
    }

    private func keyEvent(_ chars: String, _ flags: NSEvent.ModifierFlags = [], repeats: Bool = false) throws -> NSEvent
    {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
                characters: chars, charactersIgnoringModifiers: chars, isARepeat: repeats, keyCode: 49))
    }

    func testSpaceKeyDownSendsGoForTheKeyWorkspaceAndIsConsumed() async throws {
        let go = TransportAction.go(list: "00000000-0000-0000-0000-000000000001")
        keyDoc = "doc-1"
        XCTAssertNil(app.handleKeyDown(try keyEvent(" ")))
        await assertEventually { !transports(go).isEmpty }
        XCTAssertEqual(transports(go), ["doc-1"])
        // Held down: the repeat is consumed but sends nothing.
        XCTAssertNil(app.handleKeyDown(try keyEvent(" ", repeats: true)))
        await settle()
        XCTAssertEqual(transports(go).count, 1)
    }

    func testSpaceWhileTypingStaysWithTheTextField() async throws {
        keyDoc = "doc-1"
        app.isTextEditing = { true }
        let event = try keyEvent(" ")
        XCTAssertTrue(app.handleKeyDown(event) === event)
        await settle()
        XCTAssertTrue(transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty)
    }

    func testKeysComeFromTheWorkspaceSettingsAndFallBackToDefaults() async throws {
        let list = TransportAction.go(list: "00000000-0000-0000-0000-000000000001")
        keyDoc = "doc-1"
        try await setHotkeys("doc-1", #"{"go":\#(Self.hotkey("g")),"pause_all":null}"#)
        let old = try keyEvent(" ")
        XCTAssertTrue(app.handleKeyDown(old) === old, "Space is no longer GO in this show")
        XCTAssertNil(app.handleKeyDown(try keyEvent("g")))
        await assertEventually { !transports(list).isEmpty }
        let none = try keyEvent("[")
        XCTAssertTrue(app.handleKeyDown(none) === none, "null: no key")
        XCTAssertNil(app.handleKeyDown(try keyEvent("]")), "absent resume_all: QLab default")
    }

    func testEachWindowUsesItsOwnWorkspacesKeys() async throws {
        let second = try await addWorkspace()
        try await setHotkeys(second, #"{"go":\#(Self.hotkey("g"))}"#)
        keyDoc = second
        XCTAssertEqual(app.frontKeys.chord(for: .go), KeyChord("g"))
        XCTAssertNil(app.handleKeyDown(try keyEvent("g")))
        let go = TransportAction.go(list: "00000000-0000-0000-0000-000000000001")
        await assertEventually { !transports(go).isEmpty }
        keyDoc = "doc-1"
        XCTAssertEqual(app.frontKeys.chord(for: .go), KeyChord(" "))
        let g = try keyEvent("g")
        XCTAssertTrue(app.handleKeyDown(g) === g)
        await settle()
        XCTAssertEqual(transports(go), [second])
    }

    func testConflictingHotkeysInAShowWarnInTheLogAndDoNotCrash() async throws {
        let store = try XCTUnwrap(app.session.stores["doc-1"])
        keyDoc = "doc-1"
        let hk = Self.hotkey("x")
        try await setHotkeys("doc-1", #"{"go":\#(hk),"pause_all":\#(hk),"resume_all":{"key":"f5","modifiers":{}}}"#)
        let keys = app.frontKeys
        XCTAssertEqual(keys.chord(for: .go), KeyChord("x"))
        XCTAssertNil(keys.chord(for: .pauseAll), "the later field loses")
        XCTAssertNil(keys.chord(for: .resumeAll))
        let warnings = store.logEntries.filter { $0.level == .warn }.map(\.message)
        XCTAssertEqual(warnings.count, 2, "\(warnings)")
        _ = app.frontKeys  // read again: not logged twice
        XCTAssertEqual(store.logEntries.filter { $0.level == .warn }.count, 2)
    }

    func testAHeldKeyFiresOnce() async throws {
        keyDoc = "doc-1"
        await app.perform(.go, isRepeat: true)
        XCTAssertTrue(transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty)
    }

    func testTypingKeepsCharacterKeysButNotCommandChords() async throws {
        keyDoc = "doc-1"
        var typing = true
        app.isTextEditing = { typing }
        XCTAssertFalse(app.canPerform(.go), "Space types a space")
        XCTAssertFalse(app.canPerform(.pauseAll))
        XCTAssertFalse(app.canPerform(.pauseResumeSelected), "P types a p")
        XCTAssertTrue(app.canPerform(.playheadNext), "Shift-Cmd-Down is not a typing key")
        XCTAssertTrue(app.canPerform(.panic))
        typing = false
        XCTAssertTrue(app.canPerform(.go))
    }

    func testMenuKeyEquivalentsFollowTheKeyWindowsKeys() async throws {
        keyDoc = "doc-1"
        let menus = MainMenu.make()
        func item(_ c: TransportCommand) -> NSMenuItem? {
            menus.transport.items.first { $0.representedObject as? String == c.rawValue }
        }
        MainMenu.applyKeys(to: menus.transport, app.frontKeys)
        XCTAssertEqual(item(.go)?.keyEquivalent, " ")
        XCTAssertEqual(item(.pauseAll)?.keyEquivalent, "[")
        XCTAssertEqual(item(.resumeAll)?.keyEquivalent, "]")
        XCTAssertEqual(
            item(.playheadNext)?.keyEquivalent, "", "the model's bare Down: the selection moves the playhead")
        XCTAssertEqual(item(.stopAll)?.keyEquivalent, "", "the model's Esc is the app-wide panic")
        XCTAssertEqual(item(.panic)?.keyEquivalent, "", "Esc is handled app-wide, not by the menu")
        XCTAssertEqual(item(.pauseResumeSelected)?.keyEquivalent, "p")
        let commands = menus.transport.items.compactMap { $0.representedObject as? String }
        XCTAssertEqual(commands.count, TransportCommand.allCases.count)

        try await setHotkeys(
            "doc-1",
            #"{"go":\#(Self.hotkey("g", ["command"])),"playhead_next":\#(Self.hotkey("down", ["command", "shift"]))}"#)
        MainMenu.applyKeys(to: menus.transport, app.frontKeys)
        XCTAssertEqual(item(.go)?.keyEquivalent, "g")
        XCTAssertEqual(item(.go)?.keyEquivalentModifierMask, [.command])
        XCTAssertEqual(item(.playheadNext)?.keyEquivalent, KeyChord.down)
        XCTAssertEqual(item(.playheadNext)?.keyEquivalentModifierMask, [.command, .shift])
    }

    func testTransportMenuTitlesAreLocalised() {
        let menus = MainMenu.make()
        for entry in menus.transport.items where !entry.isSeparatorItem {
            XCTAssertFalse(entry.title.hasPrefix("menu."), entry.title)
        }
    }

    // MARK: Close

    func testCloseWhilePlayingCancelKeepsTheWindowAndAsksNothingElse() async throws {
        alerts.closePlaying = false
        let closed = await app.closeWorkspace("doc-1")
        XCTAssertFalse(closed)
        XCTAssertEqual(alerts.log, ["closePlaying"])
        XCTAssertNotNil(app.windows["doc-1"])
        XCTAssertEqual(app.windows["doc-1"]?.window?.isVisible, true)
        XCTAssertTrue(closeMessages().isEmpty)
    }

    func testCloseWhilePlayingConfirmThenUnsavedPromptThenClose() async throws {
        alerts.saveChoices = [.dontSave]
        let closed = await app.closeWorkspace("doc-1")
        XCTAssertTrue(closed)
        XCTAssertEqual(alerts.log, ["closePlaying", "unsaved:Test"])
        XCTAssertEqual(closeMessages().map(\.discard), [true])
        XCTAssertNil(app.windows["doc-1"])
    }

    func testCleanIdleWorkspaceClosesWithoutPrompts() async throws {
        let second = try await addWorkspace()
        let closed = await app.closeWorkspace(second)
        XCTAssertTrue(closed)
        XCTAssertTrue(alerts.log.isEmpty)
        XCTAssertEqual(closeMessages().map(\.discard), [false])
        XCTAssertEqual(app.windows.count, 1)
    }

    func testUnsavedCancelKeepsWindow() async throws {
        let second = try await addWorkspace()
        await makeDirty(second)
        alerts.saveChoices = [.cancel]
        let closed = await app.closeWorkspace(second)
        XCTAssertFalse(closed)
        XCTAssertNotNil(app.windows[second])
        XCTAssertTrue(closeMessages().isEmpty)
    }

    func testUnsavedSaveSavesThenClosesWithoutDiscard() async throws {
        alerts.closePlaying = true
        alerts.saveChoices = [.save]  // doc-1 has a path: plain save
        let closed = await app.closeWorkspace("doc-1")
        XCTAssertTrue(closed)
        XCTAssertEqual(engine.requestCount { if case .save = $0 { return true } else { return false } }, 1)
        XCTAssertEqual(closeMessages().map(\.discard), [false])
    }

    func testSaveOfUntitledAsksForAPathNotesItAndCancelledPanelAbortsClose() async throws {
        let second = try await addWorkspace()
        await makeDirty(second)
        alerts.saveChoices = [.save]
        saveURL = nil
        let aborted = await app.closeWorkspace(second)
        XCTAssertFalse(aborted)
        XCTAssertNotNil(app.windows[second])

        alerts.saveChoices = [.save]
        saveURL = URL(fileURLWithPath: "/Shows/New.openlab")
        let closed = await app.closeWorkspace(second)
        XCTAssertTrue(closed)
        XCTAssertEqual(recents, [saveURL])
        let saveAs = engine.requestCount {
            if case .saveAs(let p) = $0 { return p == "/Shows/New.openlab" } else { return false }
        }
        XCTAssertEqual(saveAs, 1)
    }

    func testClosingTheLastWorkspaceShowsTheLauncher() async throws {
        alerts.saveChoices = [.dontSave]
        recents = [URL(fileURLWithPath: "/Shows/Hamlet.openlab")]
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)
        _ = await app.closeWorkspace("doc-1")
        XCTAssertTrue(app.windows.isEmpty)
        XCTAssertEqual(app.launcher.window?.isVisible, true)
        let root = try XCTUnwrap(app.launcher.window?.contentView)
        XCTAssertNotNil(find(root, AX.launcherNew))
        XCTAssertNotNil(find(root, AX.launcherOpen))
        XCTAssertNotNil(find(root, AX.launcherRecentPrefix + "0"))
    }

    func testReopenWithNoWindowsShowsLauncherAndWithWindowsDoesNot() async throws {
        app.reopen()
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)

        alerts.saveChoices = [.dontSave]
        _ = await app.closeWorkspace("doc-1")
        app.launcher.window?.orderOut(nil)
        app.reopen()
        XCTAssertEqual(app.launcher.window?.isVisible, true)
    }

    func testDockReopenGoesThroughTheDelegate() async throws {
        let delegate = AppDelegate()
        XCTAssertFalse(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
        let controller = delegate.appController
        controller.closeAllWindows()
        _ = delegate.applicationShouldHandleReopen(NSApp, hasVisibleWindows: false)
        XCTAssertEqual(controller.launcher.window?.isVisible, true)
        controller.closeAllWindows()
    }

    func testLauncherButtonsDriveNewOpenAndRecent() async throws {
        let url = URL(fileURLWithPath: "/Shows/Lear.openlab")
        recents = [url]
        app.showLauncher()
        let root = try XCTUnwrap(app.launcher.window?.contentView)
        (find(root, AX.launcherNew) as? NSButton)?.performClick(nil)
        await assertEventually { app.windows.count == 2 }
        (find(root, AX.launcherRecentPrefix + "0") as? NSButton)?.performClick(nil)
        await assertEventually { app.windows.count == 3 }
        await assertEventually { recents.count == 2 }
        XCTAssertEqual(app.session.workspaces.last?.path, url.path)
        XCTAssertEqual(recents, [url, url])
    }

    // MARK: Quit

    // MARK: Pending inspector edits

    /// Types a name in the inspector of "doc-1" without committing it.
    private func typePendingName(_ text: String = "Typed") throws -> WorkspaceWindowController {
        let wc = try XCTUnwrap(app.windows["doc-1"])
        wc.inspector.setSelection([CueListHarness.intro])
        wc.inspector.nameField.stringValue = text
        wc.inspector.controlTextDidChange(
            Notification(name: NSControl.textDidChangeNotification, object: wc.inspector.nameField))
        return wc
    }

    private func index(of match: (ClientMessage) -> Bool) -> Int? { engine.received.firstIndex(where: match) }
    private func isCommand(_ m: ClientMessage) -> Bool {
        if case .command = m { return true } else { return false }
    }

    func testCloseSendsThePendingEditFirst() async throws {
        _ = try typePendingName()
        alerts.saveChoices = [.dontSave]
        let closed = await app.closeWorkspace("doc-1")
        XCTAssertTrue(closed)
        let edit = try XCTUnwrap(index { self.isCommand($0) })
        let close = try XCTUnwrap(index { if case .provisional(.close) = $0 { return true } else { return false } })
        XCTAssertLessThan(edit, close)
    }

    func testSaveFromThePromptSendsThePendingEditBeforeTheSave() async throws {
        _ = try typePendingName()
        await makeDirty("doc-1")
        alerts.saveChoices = [.save]
        saveURL = URL(fileURLWithPath: "/tmp/x.openlab")
        _ = await app.closeWorkspace("doc-1")
        let edit = try XCTUnwrap(index { self.isCommand($0) })
        let save = try XCTUnwrap(index { if case .request(_, .saveAs) = $0 { return true } else { return false } })
        XCTAssertLessThan(edit, save)
    }

    func testQuitSendsThePendingEditBeforeGoodbye() async throws {
        _ = try typePendingName()
        alerts.saveChoices = [.dontSave]  // the edit made the workspace dirty
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit, "\(alerts.log) \(alerts.errors)")
        await assertEventually { self.index { if case .goodbye = $0 { return true } else { return false } } != nil }
        let edit = try XCTUnwrap(index { self.isCommand($0) })
        let bye = try XCTUnwrap(index { if case .goodbye = $0 { return true } else { return false } })
        XCTAssertLessThan(edit, bye)
    }

    func testInvalidPendingEditStopsCloseAndQuit() async throws {
        let wc = try XCTUnwrap(app.windows["doc-1"])
        wc.inspector.setSelection([CueListHarness.fade])
        func typeBadTime() {
            wc.inspector.preWaitField.stringValue = "abc"
            wc.inspector.controlTextDidChange(
                Notification(name: NSControl.textDidChangeNotification, object: wc.inspector.preWaitField))
        }
        typeBadTime()
        let closed = await app.closeWorkspace("doc-1")
        XCTAssertFalse(closed)
        XCTAssertNotNil(app.windows["doc-1"])
        typeBadTime()
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertFalse(engine.received.contains { if case .goodbye = $0 { return true } else { return false } })
        XCTAssertFalse(engine.received.contains { isCommand($0) })
        XCTAssertEqual(wc.inspector.messageLabel.stringValue, loc("inspector.invalid.time"))
    }

    func testQuitWalksEveryDirtyWorkspaceThenSaysGoodbye() async throws {
        let b = try await addWorkspace()
        let c = try await addWorkspace()
        await makeDirty(b)
        _ = c  // clean: no prompt
        alerts.saveChoices = [.dontSave, .dontSave]
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertEqual(alerts.log, ["quitPlaying", "unsaved:Test", "unsaved:Untitled 2"])
        let goodbyes = engine.received.filter { if case .goodbye(true) = $0 { return true } else { return false } }
        XCTAssertEqual(goodbyes.count, 1)
    }

    func testQuitCancelOnASecondWorkspaceAbortsAndSendsNoGoodbye() async throws {
        let b = try await addWorkspace()
        await makeDirty(b)
        alerts.saveChoices = [.dontSave, .cancel]
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log.filter { $0.hasPrefix("unsaved") }.count, 2)
        XCTAssertFalse(engine.received.contains { if case .goodbye = $0 { return true } else { return false } })
        XCTAssertEqual(app.windows.count, 2)
    }

    func testQuitWhilePlayingAsksFirstAndCancelAsksNothingMore() async throws {
        alerts.quitPlaying = false
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log, ["quitPlaying"])
        XCTAssertFalse(engine.received.contains { if case .goodbye = $0 { return true } else { return false } })
    }

    func testSaveFailureDuringQuitAbortsWithALocalisedAlertAndNoGoodbye() async throws {
        alerts.saveChoices = [.save]
        engine.garbleResults = true
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.errors.count, 1)
        XCTAssertFalse(alerts.errors[0].contains("encoding("))
        XCTAssertFalse(engine.received.contains { if case .goodbye = $0 { return true } else { return false } })
    }

    func testHungSaveTimesOutAndAbortsQuit() async throws {
        alerts.saveChoices = [.save]
        engine.holdResults = true
        let task = Task { await app.prepareToQuit() }
        await assertEventually { await quitScheduler.waitingCount > 0 }
        await quitScheduler.fire()
        let quit = await task.value
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.errors.count, 1)
        XCTAssertFalse(app.quitting)
        engine.releaseResults()
    }

    func testHungGoodbyeFlushStillQuits() async throws {
        alerts.saveChoices = [.dontSave]
        engine.holdFlush = true
        let task = Task { await app.prepareToQuit() }
        await assertEventually {
            self.engine.received.contains { if case .goodbye = $0 { return true } else { return false } }
        }
        await assertEventually { await quitScheduler.waitingCount > 0 }
        await quitScheduler.fire()
        let quit = await task.value
        XCTAssertTrue(quit)
    }

    private func disconnect() async {
        engine.dropConnections()
        await assertEventually { app.session.connectionState != .connected }
    }

    func testQuitWhileDisconnectedCancelKeepsRunning() async throws {
        await disconnect()
        alerts.quitDisconnected = false
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log, ["quitDisconnected"])
    }

    func testQuitWhileDisconnectedQuitAnywayQuitsWithoutPrompts() async throws {
        await disconnect()
        alerts.quitDisconnected = true
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertEqual(alerts.log, ["quitDisconnected"])
    }

    // MARK: Re-entrancy, single-workspace engine, reconnect

    func testDoubleCloseRequestIsIgnored() async throws {
        let second = try await addWorkspace()
        let firstTask = Task { await app.closeWorkspace(second) }
        let againTask = Task { await app.closeWorkspace(second) }
        let results = [await firstTask.value, await againTask.value]
        XCTAssertEqual(results.filter { $0 }.count, 1)
        XCTAssertEqual(closeMessages().count, 1)
        XCTAssertTrue(alerts.errors.isEmpty)
    }

    func testQuitDuringACloseFlowIsIgnoredAndCloseDuringQuitIsIgnored() async throws {
        let second = try await addWorkspace()
        engine.holdResults = true
        let closeTask = Task { await app.closeWorkspace(second) }
        await assertEventually { closeMessages().count == 1 }
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertTrue(alerts.log.isEmpty)
        engine.holdResults = false
        engine.releaseResults()
        _ = await closeTask.value
    }

    func testHungCloseTimesOutClearsTheGuardAndALaterQuitProceeds() async throws {
        let second = try await addWorkspace()
        engine.holdResults = true
        let task = Task { await app.closeWorkspace(second) }
        await assertEventually { closeMessages().count == 1 }
        await assertEventually { await quitScheduler.waitingCount > 0 }
        await quitScheduler.fire()
        let closed = await task.value
        XCTAssertFalse(closed)
        XCTAssertEqual(alerts.errors.count, 1)

        engine.holdResults = false
        alerts.saveChoices = [.dontSave]
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        engine.releaseResults()
    }

    func testKeyMonitorIsInstalledOnce() {
        app.installKeyMonitor()
        app.installKeyMonitor()
        XCTAssertEqual(app.keyMonitorInstalls, 1)
    }

    func testReconnectDoesNotDuplicateWindows() async throws {
        let before = try XCTUnwrap(app.windows["doc-1"])
        await disconnect()
        await assertEventually {
            await self.sessionScheduler.fire()
            let session = self.app.session
            return session.connectionState == .connected && session.stores["doc-1"]?.connectionState == .connected
        }
        await settle()
        XCTAssertEqual(app.windows.count, 1)
        XCTAssertTrue(app.windows["doc-1"] === before)
    }

    func testSingleWorkspaceEngineCloseDoesNothing() async throws {
        try await build(.single)
        await assertEventually { app.windows.count == 1 }
        let closed = await app.closeWorkspace(EngineSession.singleDoc)
        XCTAssertFalse(closed)
        XCTAssertTrue(alerts.log.isEmpty)
        XCTAssertEqual(app.windows.first?.value.window?.isVisible, true)
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)
        XCTAssertTrue(closeMessages().isEmpty)
    }

    private func newRequests() -> Int {
        engine.requestCount { if case .new = $0 { return true } else { return false } }
    }

    func testDirtySingleWorkspaceNewDontSaveRetriesWithDiscard() async throws {
        try await build(.single)
        await assertEventually { app.windows.count == 1 }
        alerts.saveChoices = [.dontSave]
        await app.newWorkspace()
        XCTAssertEqual(newRequests(), 2)
        XCTAssertEqual(alerts.log, ["unsaved:Test"])
        XCTAssertTrue(alerts.errors.isEmpty)
    }

    func testDirtySingleWorkspaceNewCancelDoesNotRetry() async throws {
        try await build(.single)
        await assertEventually { app.windows.count == 1 }
        alerts.saveChoices = [.cancel]
        await app.newWorkspace()
        XCTAssertEqual(newRequests(), 1)
        XCTAssertTrue(alerts.errors.isEmpty)
    }

    func testDirtySingleWorkspaceOpenSaveSavesThenRetries() async throws {
        try await build(.single)
        await assertEventually { app.windows.count == 1 }
        alerts.saveChoices = [.save]
        await app.openWorkspace(url: URL(fileURLWithPath: "/Shows/B.openlab"))
        XCTAssertEqual(engine.requestCount { if case .save = $0 { return true } else { return false } }, 1)
        XCTAssertEqual(engine.requestCount { if case .open = $0 { return true } else { return false } }, 2)
        XCTAssertEqual(recents, [URL(fileURLWithPath: "/Shows/B.openlab")])
    }

    func testAlreadyOpenFromTheLauncherFrontsTheExistingWindowWithoutAnError() async throws {
        let url = URL(fileURLWithPath: "/Shows/A.openlab")
        await app.openWorkspace(url: url)
        await assertEventually { app.windows.count == 2 }
        recents = [url]
        app.showLauncher()
        let root = try XCTUnwrap(app.launcher.window?.contentView)
        (find(root, AX.launcherRecentPrefix + "0") as? NSButton)?.performClick(nil)
        await settle()
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)
    }

    // MARK: Sidebar

    /// Adds a second list "Second" to `doc` as the engine would.
    private func addSecondList(_ doc: ModelID) async throws -> ModelID {
        let store = try XCTUnwrap(app.session.stores[doc])
        let id: ModelID = "00000000-0000-0000-0000-000000000002"
        engine.push(
            #"{"type":"model_changed","revision":\#(store.revision + 1),"upserts":[{"type":"cue_list","id":"\#(id)","name":"Second","index":1}],"deletes":[]}"#,
            doc: doc)
        await assertEventually { store.model?.cueLists.count == 2 }
        return id
    }

    func testGoAndPlayheadKeysAddressTheListPickedInTheSidebar() async throws {
        let second = try await addSecondList("doc-1")
        keyDoc = "doc-1"
        let wc = try XCTUnwrap(app.windows["doc-1"])
        wc.sidebar.listTable.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        XCTAssertEqual(wc.cueList.listID, second)
        _ = await app.goFront()
        await app.perform(.playheadNext)
        XCTAssertEqual(transports(.go(list: second)), ["doc-1"])
        XCTAssertEqual(transports(.playheadNext(list: second)), ["doc-1"])
        XCTAssertTrue(transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty)
        wc.masthead.goButton.performClick(nil)  // the GO button follows the same list
        await assertEventually { transports(.go(list: second)).count == 2 }
    }

    func testActiveCuePanicGoesToTheWindowsOwnWorkspace() async throws {
        let other = try await addWorkspace()
        let wc = try XCTUnwrap(app.windows["doc-1"])
        let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
        await assertEventually { wc.sidebar.activeCues.contains(intro) }
        let row = try XCTUnwrap(wc.sidebar.activeCues.firstIndex(of: intro))
        let cell = try XCTUnwrap(
            wc.sidebar.activeTable.view(atColumn: 0, row: row, makeIfNecessary: true) as? ActiveCueCell)
        cell.panicButton.performClick(nil)
        await assertEventually { !transports(.panicCue(cue: intro)).isEmpty }
        XCTAssertEqual(transports(.panicCue(cue: intro)), ["doc-1"])
        XCTAssertNotEqual(other, "doc-1")
        cell.pauseButton.performClick(nil)
        await assertEventually { !transports(.pause(cue: intro)).isEmpty }
        XCTAssertEqual(transports(.pause(cue: intro)), ["doc-1"])
    }

    // MARK: Helpers

    private func find(_ root: NSView, _ id: String) -> NSView? {
        if root.accessibilityIdentifier() == id { return root }
        for sub in root.subviews { if let hit = find(sub, id) { return hit } }
        return nil
    }
}
