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
            session: session, keyMap: KeyMap(store: Self.scratchDefaults()), scheduler: quitScheduler)
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

    static func scratchDefaults() -> UserDefaults {
        let name = "openlab-test-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name) ?? .standard
        defaults.removePersistentDomain(forName: name)
        return defaults
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
        let titles = MainMenu.make(keyMap: KeyMap(store: Self.scratchDefaults())).bar.items.flatMap {
            $0.submenu?.items.map(\.title) ?? []
        }
        XCTAssertFalse(titles.contains { $0.localizedCaseInsensitiveContains("revert") })
        XCTAssertEqual(
            MainMenu.make(keyMap: KeyMap(store: Self.scratchDefaults())).bar.items[1].submenu?.items.map(
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

    func testStopAllHardStopsEveryLiveCueOfTheKeyWorkspaceOnly() async throws {
        let second = try await addWorkspace()
        let running: ModelID = "00000000-0000-0000-0000-00000000000a"
        keyDoc = "doc-1"
        await app.perform(.stopAll)
        XCTAssertEqual(transports(.hardStop(cue: running)), ["doc-1"])
        keyDoc = second  // nothing is running there
        await app.perform(.stopAll)
        XCTAssertEqual(transports(.hardStop(cue: running)).count, 1)
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

    func testRemappedKeyDispatchesAndTheOldKeyDoesNot() async throws {
        keyDoc = "doc-1"
        XCTAssertNil(app.keyMap.bind(.go, to: KeyChord("g")))
        let old = try keyEvent(" ")
        XCTAssertTrue(app.handleKeyDown(old) === old)
        XCTAssertNil(app.handleKeyDown(try keyEvent("g")))
        await assertEventually { !transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty }
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

    func testMenuKeyEquivalentsFollowTheKeyMap() throws {
        let menus = MainMenu.make(keyMap: app.keyMap)
        func item(_ c: TransportCommand) -> NSMenuItem? {
            menus.transport.items.first { $0.representedObject as? String == c.rawValue }
        }
        XCTAssertEqual(item(.go)?.keyEquivalent, " ")
        XCTAssertEqual(item(.pauseAll)?.keyEquivalent, "[")
        XCTAssertEqual(item(.resumeAll)?.keyEquivalent, "]")
        XCTAssertEqual(item(.playheadNext)?.keyEquivalent, KeyChord.down)
        XCTAssertEqual(item(.playheadNext)?.keyEquivalentModifierMask, [.command, .shift])
        XCTAssertEqual(item(.panic)?.keyEquivalent, "", "Esc is handled app-wide, not by the menu")
        XCTAssertEqual(item(.stopAll)?.keyEquivalent, "")
        let commands = menus.transport.items.compactMap { $0.representedObject as? String }
        XCTAssertEqual(commands.count, TransportCommand.allCases.count)

        XCTAssertEqual(app.keyMap.bind(.go, to: KeyChord("g", .command)), nil)
        MainMenu.applyKeys(to: menus.transport, app.keyMap)
        XCTAssertEqual(item(.go)?.keyEquivalent, "g")
        XCTAssertEqual(item(.go)?.keyEquivalentModifierMask, [.command])
    }

    func testTransportMenuTitlesAreLocalised() {
        let menus = MainMenu.make(keyMap: app.keyMap)
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

    // MARK: Helpers

    private func find(_ root: NSView, _ id: String) -> NSView? {
        if root.accessibilityIdentifier() == id { return root }
        for sub in root.subviews { if let hit = find(sub, id) { return hit } }
        return nil
    }
}
