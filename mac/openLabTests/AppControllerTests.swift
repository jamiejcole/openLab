// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class ScriptedAlerts: WorkspaceAlerts {
    var closePlaying = true
    var quitPlaying = true
    var quitDisconnected = false
    var closeShowMode = true
    var quitShowMode = true
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

    func confirmCloseInShowMode(workspace: String) -> Bool {
        log.append("closeShowMode")
        return closeShowMode
    }

    func confirmQuitInShowMode() -> Bool {
        log.append("quitShowMode")
        return quitShowMode
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
    func showInfo(_ message: String) { log.append("info:\(message)") }

    /// nil: Cancel.
    var recoveryChoice: RecoveryChoice?
    func recoveryOffer(workspace: String, modifiedMs: Int64) -> RecoveryChoice? {
        log.append("recoveryOffer:\(workspace)")
        return recoveryChoice
    }

    var restoreAutosaves = true
    func offerAutosave(name: String, modifiedMs: Int64?, setAside: Bool) -> Bool {
        log.append(setAside ? "setAsideAutosave:\(name)" : "autosave:\(name)")
        return restoreAutosaves
    }
}

@MainActor
final class AppControllerTests: XCTestCase {
    private var app: AppController!
    private var real: TestEngine!
    private var engine: WireTap!
    private var alerts: ScriptedAlerts!
    private var recents: [URL] = []
    private var keyDoc: ModelID?
    private var saveURL: URL?
    private var openURL: URL?

    private let sessionScheduler = ManualScheduler()
    private let quitScheduler = ManualScheduler()
    /// What `engine.json` is read as saying of the engine's shutdown.
    private var engineStopping = false
    /// The engine.json the app reads (another engine run's, after a restart).
    private var infoURL: URL!

    private let list: ModelID = "00000000-0000-0000-0000-000000000001"
    private let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
    private let fade: ModelID = "00000000-0000-0000-0000-00000000000b"

    override func setUp() async throws { try await build() }

    /// A real engine of this test's own with the demo show open as doc 1 (the engine numbers
    /// docs from 0: a first one is opened and closed), Intro playing (GO, which moves the playhead
    /// to the Fade cue below it) and unsaved changes, and the app connected to it with one window.
    private func build() async throws {
        app?.closeAllWindows()
        real = try await TestEngine.start()
        let real = real!
        infoURL = real.infoURL
        let scratch = try await real.open("Second")
        _ = await real.app.send({ .close(id: $0, doc: scratch, discardChanges: true) })
        let doc = try await real.open("Demo")
        XCTAssertEqual(doc, "1")
        engine = WireTap()
        let session = EngineSession(
            transport: engine,
            infoProvider: { [unowned self] in
                EngineConnectionInfo.load(from: infoURL).map {
                    var info = $0
                    info.stopping = engineStopping
                    return info
                }
            }, scheduler: sessionScheduler)
        app = AppController(session: session, scheduler: quitScheduler)
        alerts = ScriptedAlerts()
        app.alerts = alerts
        app.noteRecent = { [unowned self] in recents.append($0) }
        app.recentURLs = { [unowned self] in recents }
        app.recentExists = { _ in true }
        app.keyWindow = { [unowned self] in keyDoc.flatMap { app.windows[$0]?.window } }
        app.pickSaveURL = { [unowned self] _ in saveURL }
        app.pickOpenURL = { [unowned self] in openURL }
        session.start()
        await assertEventually { app.windows.count == 1 && session.stores.values.allSatisfy { $0.model != nil } }
        let store = try XCTUnwrap(session.stores["1"])
        guard case .success = await store.go(list: list) else { return XCTFail("go") }
        try await real.advance(seconds: 0.05)
        await assertEventually {
            store.cueStates[self.intro]?.state == .running && store.playheads[self.list] == self.fade
        }
        guard case .success = await store.send(command: .setProperty(fade, .notes("unsaved"))) else {
            return XCTFail("edit")
        }
        await assertEventually { store.document?.dirty == true }
        engine.forgetSent()
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        engine?.releaseResults()
        real?.stop()
    }

    static func hotkey(_ key: String, _ mods: [String] = []) -> String {
        let flags = ["shift", "control", "alt", "command"].map { "\"\($0)\":\(mods.contains($0))" }.joined(
            separator: ",")
        return #"{"key":"\#(key)","modifiers":{\#(flags)}}"#
    }

    /// Changes the workspace's key map through the engine (`set_workspace_setting hotkeys`): the
    /// fields of `hotkeys` over the ones it has.
    private func setHotkeys(_ doc: ModelID, _ hotkeys: String) async throws {
        let store = try XCTUnwrap(app.session.stores[doc])
        var map = try XCTUnwrap(store.model?.settings["hotkeys"])
        for (key, value) in try XCTUnwrap(JSONValue.parse(hotkeys)?.objectValue) { map.set(key, value) }
        let result = await store.send(command: .setKeyMap(map))
        guard case .success = result else { return XCTFail("set_key_map: \(result)") }
        await assertEventually { store.model?.settings["hotkeys"] == map }
    }

    /// Opens a copy of the demo show whose file holds `hotkeys` (a hand edit can hold a map the
    /// engine's commands refuse, a repeated key say); its doc, its window up.
    private func openShow(withHotkeys hotkeys: String) async throws -> ModelID {
        let source = try XCTUnwrap(
            Bundle(for: AppControllerTests.self).url(forResource: "TestShows", withExtension: nil))
        let dir = real.folder.appendingPathComponent("edited-\(UUID().uuidString)")
        try FileManager.default.copyItem(at: source.appendingPathComponent("Demo"), to: dir)
        let url = dir.appendingPathComponent("Demo.openlab")
        var show = try XCTUnwrap(JSONValue.parse(try String(contentsOf: url, encoding: .utf8)))
        var settings = try XCTUnwrap(show["workspace"]?["settings"])
        var map = try XCTUnwrap(settings["hotkeys"])
        for (key, value) in try XCTUnwrap(JSONValue.parse(hotkeys)?.objectValue) { map.set(key, value) }
        settings.set("hotkeys", map)
        show["workspace"]?.set("settings", settings)
        show["workspace"]?.set("id", .string(UUID().uuidString.lowercased()))
        try show.text().write(to: url, atomically: true, encoding: .utf8)
        let before = Set(app.windows.keys)
        await app.openWorkspace(url: url)
        await assertEventually { app.windows.count == before.count + 1 }
        let doc = try XCTUnwrap(Set(app.windows.keys).subtracting(before).first)
        await assertEventually { app.session.stores[doc]?.model != nil }
        return doc
    }

    /// A show file of the empty test show at a path of its own, named `name` (a new workspace id).
    private func showFile(_ name: String) throws -> URL {
        let source = try XCTUnwrap(
            Bundle(for: AppControllerTests.self).url(forResource: "TestShows", withExtension: nil)?
                .appendingPathComponent("Second/Second.openlab"))
        var show = try XCTUnwrap(JSONValue.parse(try String(contentsOf: source, encoding: .utf8)))
        show["workspace"]?.set("id", .string(UUID().uuidString.lowercased()))
        show["workspace"]?.set("name", .string(name))
        let dir = real.folder.appendingPathComponent("shows-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("\(name).openlab")
        try show.text().write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    /// Where a Save As in this test writes `name`.
    private func savePath(_ name: String) -> URL {
        let dir = real.folder.appendingPathComponent("saved", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("\(name).openlab")
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

    /// Unsaved changes: an edit through the engine (the panic fade time, which nothing here shows).
    private func makeDirty(_ doc: ModelID) async {
        guard let store = app.session.stores[doc] else { return XCTFail("no store \(doc)") }
        let command = Command.setWorkspaceNumber("panic_duration", 0.25)
        guard case .success = await store.send(command: command) else { return XCTFail("edit") }
        await assertEventually { store.document?.dirty == true }
    }

    private func closeMessages(_ doc: ModelID? = nil) -> [(doc: ModelID?, discard: Bool)] {
        engine.sentOn.compactMap {
            if case .close(_, let d, let discard) = $0.message { return (d, discard) }
            return nil
        }
    }

    /// The app-scope `panic` messages, by the connection they came on (nil: the app's).
    private func panics() -> [ModelID?] {
        engine.sentOn.compactMap {
            if case .panic = $0.message { return $0.doc }
            return nil
        }
    }

    /// The docs every GO went to, whatever the list (a new workspace's list has an id of its own).
    private func gos() -> [ModelID?] {
        engine.sentOn.compactMap {
            if case .transport(_, .go) = $0.message { return $0.doc }
            return nil
        }
    }

    private func transports(_ action: TransportAction) -> [ModelID?] {
        engine.sentOn.compactMap {
            if case .transport(_, let a) = $0.message, a == action { return $0.doc }
            return nil
        }
    }

    /// Lets the engine's device time run on, so what the app asked for takes effect.
    private func advance(_ seconds: Double = 0.05) async throws { try await real.advance(seconds: seconds) }

    private func state(_ cue: ModelID, _ doc: ModelID = "1") -> WireCueState? {
        app.session.stores[doc]?.cueStates[cue]?.state
    }

    // MARK: Windows follow the session

    func testOneWindowPerWorkspaceAndRemovedWhenEngineClosesIt() async throws {
        XCTAssertEqual(Set(app.windows.keys), ["1"])
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)
        let second = try await addWorkspace()
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertTrue(app.windows["1"]?.store === app.session.stores["1"])
        XCTAssertTrue(app.windows[second]?.store === app.session.stores[second])
        XCTAssertTrue(app.windows["1"]?.store !== app.windows[second]?.store)
        XCTAssertEqual(app.windows["1"]?.window?.title, "Demo")

        _ = await app.session.close(doc: second, discardChanges: true)
        await assertEventually { app.windows.count == 1 }
        XCTAssertNil(app.windows[second])
        XCTAssertEqual(app.windows["1"]?.window?.isVisible, true)
    }

    func testNewAndOpenAddWindowsWhileTheFirstKeepsRunning() async throws {
        openURL = try showFile("Macbeth")
        await app.newWorkspace()
        await app.openWorkspace()
        await assertEventually { app.windows.count == 3 && app.session.stores.values.allSatisfy { $0.model != nil } }
        XCTAssertEqual(closeMessages().count, 0)
        XCTAssertEqual(app.windows["1"]?.window?.isVisible, true)
        XCTAssertEqual(recents, [openURL])
        XCTAssertTrue(alerts.log.isEmpty)
    }

    func testRecentIsNotNotedWhenOpenFailsAndAlreadyOpenFrontsTheWindow() async throws {
        let url = try showFile("A")
        await app.openWorkspace(url: url)
        await assertEventually { app.windows.count == 2 }
        recents = []
        app.windows["1"]?.window?.makeKeyAndOrderFront(nil)
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
                \.keyEquivalent), ["n", "o", "", "", "", "", "", ",", "", "w", "w", "s", "S"])
    }

    // MARK: Front-window routing

    func testGoAndPauseAllAddressTheKeyWindowsWorkspace() async throws {
        let second = try await addWorkspace()
        keyDoc = second
        await app.perform(.go)
        await app.perform(.pauseAll)
        XCTAssertEqual(gos(), [second])
        XCTAssertEqual(transports(.pauseAll), [second])

        keyDoc = "1"
        await app.perform(.go)
        XCTAssertEqual(gos(), [second, "1"])

        keyDoc = nil  // no workspace window is key: nothing is sent
        await app.perform(.go)
        XCTAssertEqual(gos().count, 2)
    }

    func testWindowButtonsAddressTheirOwnStoreNotTheKeyWindow() async throws {
        let second = try await addWorkspace()
        keyDoc = "1"
        app.windows[second]?.masthead.goButton.performClick(nil)
        await assertEventually { !gos().isEmpty }
        XCTAssertEqual(gos(), [second])
        let pause = try XCTUnwrap(app.windows[second]?.sidebar.transportButtons[AX.transportPause])
        pause.performClick(nil)
        await assertEventually { !transports(.pauseAll).isEmpty }
        XCTAssertEqual(transports(.pauseAll), [second])
    }

    /// QLab's S key: each press is one batch of `panic_cue` for the selection, in Show Mode too;
    /// a second press is sent the same way (the engine hard-stops cues still fading from the
    /// first), and only the selected cues are named.
    func testPanicSelectedSendsAPanicCueForEachSelectedCueOnEveryPress() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        XCTAssertEqual(app.frontKeys.chord(for: .panicSelected), KeyChord("s"))
        let roots = wc.cueList.roots.map(\.id)
        XCTAssertEqual(roots.count, 3)
        wc.setShowMode(true)
        await assertEventually { wc.isShowMode }
        XCTAssertTrue(app.canPerform(.panicSelected))
        wc.cueList.select([roots[0]])
        await app.perform(.panicSelected)
        await app.perform(.panicSelected)
        XCTAssertEqual(transports(.panicCue(cue: roots[0])), ["1", "1"])
        XCTAssertTrue(transports(.panicCue(cue: roots[1])).isEmpty, "an unselected cue is left alone")
        // A held key fires once.
        await app.perform(.panicSelected, isRepeat: true)
        XCTAssertEqual(transports(.panicCue(cue: roots[0])).count, 2)
        // Several selected cues: one batch, so they fade from the same sample.
        wc.cueList.select(roots)
        let order = wc.cueList.selectedIDs()
        await app.perform(.panicSelected)
        XCTAssertEqual(transports(.batch(actions: order.map { .panicCue(cue: $0) })), ["1"])
    }

    /// A group's panic fades its whole subtree; a selected descendant's own panic_cue would be a
    /// second press (hard cut), so only the topmost selected cue of each subtree is panicked.
    func testPanicSelectedSkipsSelectedDescendantsOfASelectedGroup() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let store = try XCTUnwrap(app.session.stores["1"])
        let list = try XCTUnwrap(wc.cueList.listID)
        // Cues as a show file holds them (every field), so the engine takes them as they are.
        func cue(_ id: String, _ kind: JSONValue) -> JSONValue {
            var c = TestSnapshots.cueJSON(id: id, number: nil, name: id, kind: kind)
            c.set("triggers", .object(["hotkey": .null, "osc": .null]))
            c.set("second_trigger", .string("nothing"))
            c.set("second_trigger_on_release", .bool(false))
            return c
        }
        func audio(_ id: String) -> JSONValue { cue(id, TestSnapshots.audioKind("media/a.wav")) }
        func group(_ id: String, _ kids: [JSONValue]) -> JSONValue { cue(id, TestSnapshots.groupKind(kids)) }
        let (gOuter, gInner, child, grandchild) = (
            "00000000-0000-0000-0000-0000000000f1", "00000000-0000-0000-0000-0000000000f2",
            "00000000-0000-0000-0000-0000000000f3", "00000000-0000-0000-0000-0000000000f4"
        )
        let outer = group(gOuter, [audio(child), group(gInner, [audio(grandchild)])])
        let added = await store.send(command: .addCue(outer, list: list, parent: nil, index: 0))
        guard case .success = added else { return XCTFail("add_cue: \(added)") }
        await assertEventually { wc.cueList.node(ModelID(grandchild)) != nil }
        wc.cueList.reveal(ModelID(grandchild))  // expands the groups so every row can be selected
        let sibling = try XCTUnwrap(wc.cueList.roots.map(\.id).first { $0 != ModelID(gOuter) })

        // Group + child: only the group.
        wc.cueList.select([ModelID(gOuter), ModelID(child)])
        await app.perform(.panicSelected)
        XCTAssertEqual(transports(.panicCue(cue: ModelID(gOuter))), ["1"])
        XCTAssertTrue(transports(.panicCue(cue: ModelID(child))).isEmpty)
        // Group + grandchild, and a top-level sibling: one panic each for group and sibling.
        wc.cueList.select([ModelID(gOuter), ModelID(grandchild), sibling])
        await app.perform(.panicSelected)
        XCTAssertEqual(
            transports(.panicCue(cue: ModelID(gOuter))).count, 1, "the second press went out inside the batch")
        XCTAssertTrue(transports(.panicCue(cue: ModelID(grandchild))).isEmpty)
        XCTAssertEqual(transports(.batch(actions: [.panicCue(cue: ModelID(gOuter)), .panicCue(cue: sibling)])), ["1"])
        // A child alone, with no selected ancestor, is still panicked.
        wc.cueList.select([ModelID(child)])
        await app.perform(.panicSelected)
        XCTAssertEqual(transports(.panicCue(cue: ModelID(child))), ["1"])
    }

    /// The sidebar Reset all button and the Transport menu item send `reset_all` to their own
    /// workspace, with no confirmation, in Show Mode as in Edit Mode (QLab's sidebar is the same
    /// in both).
    func testResetAllButtonAndMenuItemResetTheirOwnWorkspaceInShowMode() async throws {
        let second = try await addWorkspace()
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows[second])
        wc.setShowMode(true)
        await assertEventually { wc.isShowMode }
        let reset = try XCTUnwrap(wc.sidebar.transportButtons[AX.transportReset])
        XCTAssertTrue(reset.isEnabled)
        reset.performClick(nil)
        await assertEventually { !transports(.resetAll).isEmpty }
        XCTAssertEqual(transports(.resetAll), [second])
        XCTAssertTrue(app.canPerform(.resetAll))
        XCTAssertNil(app.frontKeys.chord(for: .resetAll), "QLab has no Reset All key")
        await app.perform(.resetAll)
        XCTAssertEqual(transports(.resetAll), [second, "1"])
    }

    func testPanicButtonIsAppScope() async throws {
        _ = try await addWorkspace()
        app.windows["1"]?.sidebar.transportButtons[AX.transportPanic]?.performClick(nil)
        await assertEventually { panics().count == 1 }
        // Sent on the unbound app connection, not a workspace's.
        XCTAssertEqual(panics().map { $0 == nil }, [true])
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
        await assertEventually { panics().count == 1 }
        await settle()
        XCTAssertEqual(panics().count, 1)

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
        await assertEventually { panics().count == 2 }
        window.close()
        XCTAssertEqual(panics().map { $0 == nil }, [true, true])
        XCTAssertTrue(transports(.panic).isEmpty, "the app-scope panic message, not transport: panic")
    }

    func testOnlyPlainEscPanics() async throws {
        _ = app.handleKeyDown(try escEvent(flags: .command))
        _ = app.handleKeyDown(try escEvent(keyCode: 0))
        await settle()
        XCTAssertTrue(panics().isEmpty)
    }

    // MARK: Transport commands

    func testEveryCommandSendsItsMessageToTheKeyWindowsWorkspace() async throws {
        let second = try await addWorkspace()
        keyDoc = second
        let list = try XCTUnwrap(app.session.stores[second]?.model?.cueLists.first?.id)
        for (command, action) in [
            (TransportCommand.go, TransportAction.go(list: list)), (.pauseAll, .pauseAll), (.resumeAll, .resumeAll),
        ] {
            await app.perform(command)
            XCTAssertEqual(transports(action), [second], "\(command)")
        }
        keyDoc = "1"
        await app.perform(.pauseAll)
        XCTAssertEqual(transports(.pauseAll), [second, "1"])
    }

    func testResumeAllButtonAddressesItsOwnStore() async throws {
        let second = try await addWorkspace()
        keyDoc = "1"
        app.windows[second]?.sidebar.transportButtons[AX.transportResume]?.performClick(nil)
        await assertEventually { !transports(.resumeAll).isEmpty }
        XCTAssertEqual(transports(.resumeAll), [second])
    }

    func testPanicCommandIsAppScopeAndNeedsNoWorkspaceWindow() async throws {
        keyDoc = nil
        XCTAssertTrue(app.canPerform(.panic))
        XCTAssertFalse(app.canPerform(.go))
        await app.perform(.panic)
        XCTAssertEqual(panics().map { $0 == nil }, [true])
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
        keyDoc = "1"
        try await setHotkeys("1", #"{"stop_all":\#(Self.hotkey("s"))}"#)
        XCTAssertNil(app.handleKeyDown(try keyEvent("s")))
        await assertEventually { !transports(.stopAll).isEmpty }
        XCTAssertEqual(transports(.stopAll), ["1"])
    }

    func testEscStaysPanicNotStopAll() async throws {
        keyDoc = "1"
        _ = app.handleKeyDown(try escEvent())
        await assertEventually { panics().count == 1 }
        XCTAssertTrue(transports(.stopAll).isEmpty)
    }

    func testFailedTransportBeepsAndLogsInsteadOfBeingDropped() async throws {
        keyDoc = "1"
        var beeps = 0
        app.beep = { beeps += 1 }
        let store = try XCTUnwrap(app.session.stores["1"])
        engine.dropConnections()
        await app.perform(.pauseAll)
        XCTAssertEqual(beeps, 1)
        let errors = store.logEntries.filter { $0.level == .error }.map(\.message)
        XCTAssertEqual(errors.count, 1, "\(errors)")
        XCTAssertTrue(errors.first?.hasPrefix("Transport command failed") == true, "\(errors)")
    }

    func testSuccessfulTransportDoesNotBeep() async throws {
        keyDoc = "1"
        var beeps = 0
        app.beep = { beeps += 1 }
        await app.perform(.pauseAll)
        XCTAssertEqual(beeps, 0)
    }

    func testPlayheadSendsTheFinalSelectionEvenBeforeTheEchoArrives() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
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

    func testSelectingSeveralCuesMovesThePlayheadToTheMostRecentlySelected() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        let a: ModelID = "00000000-0000-0000-0000-00000000000a"
        let b: ModelID = "00000000-0000-0000-0000-00000000000b"
        let list: ModelID = "00000000-0000-0000-0000-000000000001"
        let outline = wc.cueList.outlineView
        let rowOfB = outline.row(forItem: wc.cueList.node(b))
        let rowOfA = outline.row(forItem: wc.cueList.node(a))
        XCTAssertLessThan(rowOfA, rowOfB, "the test needs A above B")
        // B first, then A added by Shift/Cmd-click: A is the latest, though B is not first in row
        // order (and is where the playhead already is).
        outline.selectRowIndexes([rowOfB], byExtendingSelection: false)
        outline.selectRowIndexes([rowOfA], byExtendingSelection: true)
        await assertEventually { !transports(.setPlayhead(list: list, cue: a)).isEmpty }
        // Then B added again to a selection that holds A: the playhead goes back to B.
        outline.selectRowIndexes([rowOfA], byExtendingSelection: false)
        outline.selectRowIndexes([rowOfB], byExtendingSelection: true)
        await assertEventually { transports(.setPlayhead(list: list, cue: b)).count >= 1 }
    }

    func testPlayheadTargetIsTheLastAddedCue() {
        let a: ModelID = "a"
        let b: ModelID = "b"
        let c: ModelID = "c"
        XCTAssertEqual(
            CueListViewController.playheadTarget(selected: [a, b], previous: [b], latest: b), a, "one added")
        XCTAssertEqual(
            CueListViewController.playheadTarget(selected: [a, b, c], previous: [a], latest: c), c,
            "range ends at the click")
        XCTAssertEqual(
            CueListViewController.playheadTarget(selected: [a, b, c], previous: [a], latest: b), b, "range from below")
        XCTAssertEqual(
            CueListViewController.playheadTarget(selected: [b], previous: [a, b], latest: b), b,
            "click on one of several")
        XCTAssertEqual(CueListViewController.playheadTarget(selected: [a, b], previous: [], latest: nil), b)
        XCTAssertNil(CueListViewController.playheadTarget(selected: [], previous: [a], latest: nil))
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
        // The engine refuses a repeated key as a command; a show file can hold one (a hand edit).
        let hk = Self.hotkey("x")
        let doc = try await openShow(withHotkeys: #"{"go":\#(hk),"pause_all":\#(hk)}"#)
        let store = try XCTUnwrap(app.session.stores[doc])
        await assertEventually { store.logEntries.contains { $0.level == .warn } }
        XCTAssertEqual(store.logEntries.filter { $0.level == .warn }.count, 1)
    }

    /// The cue row menu's command goes to the window that was clicked (`perform(on:)`), whichever
    /// window is in front.
    func testCommandOnATargetWindowUsesThatWindowsSelection() async throws {
        let second = try await addWorkspace()
        let clicked = try XCTUnwrap(app.windows["1"])
        let running: ModelID = "00000000-0000-0000-0000-00000000000a"
        clicked.cueList.select([running])
        keyDoc = second  // the other window is in front, with nothing selected
        await app.perform(.loadSelected)
        XCTAssertEqual(transports(.load(cue: running, at: 0)), [], "the front window has no selection")
        await app.perform(.loadSelected, on: clicked)
        XCTAssertEqual(transports(.load(cue: running, at: 0)), ["1"])
    }

    func testSelectedCueCommandsUseTheSelection() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let running: ModelID = "00000000-0000-0000-0000-00000000000a"
        wc.cueList.select([running])
        XCTAssertEqual(wc.cueList.selectedIDs(), [running])
        await app.perform(.pauseResumeSelected)  // running -> pause
        await app.perform(.startSelected)
        await app.perform(.stopSelected)
        await app.perform(.resetSelected)
        await app.perform(.loadSelected)
        XCTAssertEqual(transports(.pause(cue: running)), ["1"])
        XCTAssertEqual(transports(.start(cue: running)), ["1"])
        XCTAssertEqual(transports(.hardStop(cue: running)), ["1"])
        XCTAssertEqual(transports(.reset(cue: running)), ["1"])
        XCTAssertEqual(transports(.load(cue: running, at: 0)), ["1"])
        wc.cueList.select([])  // nothing selected: nothing sent
        await app.perform(.startSelected)
        XCTAssertEqual(transports(.start(cue: running)).count, 1)
    }

    /// M5.11g: Start, Stop and Reset Selected on N cues send one `batch` transport with every cue
    /// in selection order (the engine starts them on one sample, NFR-21), not N transports.
    func testSelectedCuesGoAsOneBatchInSelectionOrder() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        wc.cueList.select(wc.cueList.roots.map(\.id))
        let selected = wc.cueList.selectedIDs()
        XCTAssertGreaterThanOrEqual(selected.count, 2)
        var beeps = 0
        app.beep = { beeps += 1 }
        let before = engine.sent.count
        await app.perform(.startSelected)
        await app.perform(.stopSelected)
        await app.perform(.resetSelected)
        let sent = engine.sent[before...].compactMap { m -> TransportAction? in
            if case .transport(_, let a) = m { return a } else { return nil }
        }
        XCTAssertEqual(
            sent,
            [
                .batch(actions: selected.map { .start(cue: $0) }),
                .batch(actions: selected.map { .hardStop(cue: $0) }),
                .batch(actions: selected.map { .reset(cue: $0) }),
            ])
        XCTAssertEqual(transports(.batch(actions: selected.map { .start(cue: $0) })), ["1"])
        XCTAssertEqual(beeps, 0)
    }

    /// Pause/Resume Selected on a running and a paused cue is one batch too: each cue's own toggle.
    func testPauseResumeSelectedGoesAsOneBatchOfEachCuesToggle() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let store = try XCTUnwrap(app.session.stores["1"])
        // Intro plays; Door slam is started and paused.
        let (a, b) = (intro, TestSnapshots.door)
        _ = await store.transport(.start(cue: b))
        try await advance()
        _ = await store.transport(.pause(cue: b))
        try await advance()
        await assertEventually { store.cueStates[b]?.state == .paused && store.cueStates[a]?.state == .running }
        wc.cueList.reveal(b)
        wc.cueList.select([b, a])
        let order = wc.cueList.selectedIDs()
        await app.perform(.pauseResumeSelected)
        let toggles = order.map { $0 == a ? TransportAction.pause(cue: a) : .resume(cue: b) }
        XCTAssertEqual(transports(.batch(actions: toggles)), ["1"])
        try await advance()
        await assertEventually("each cue toggled") {
            store.cueStates[a]?.state == .paused && store.cueStates[b]?.state == .running
        }
    }

    func testPauseResumeSelectedResumesACueThatBrokeWhilePaused() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let store = try XCTUnwrap(app.session.stores["1"])
        let cue = intro
        _ = await store.transport(.pause(cue: cue))
        try await advance()
        await assertEventually { store.cueStates[cue]?.state == .paused }
        // The audio device goes while it is paused: the engine breaks it.
        guard case .success = await real.request(.loseDevice) else { return XCTFail("lose_device") }
        await assertEventually { store.cueStates[cue]?.state == .broken }
        wc.cueList.select([cue])
        await app.perform(.pauseResumeSelected)
        XCTAssertEqual(transports(.resume(cue: cue)), ["1"])
    }

    func testPreviewSelectedTogglesAudioCuesOfTheKeyWindowOnly() async throws {
        let audio = intro
        let wc = try XCTUnwrap(app.windows["1"])
        let store = try XCTUnwrap(app.session.stores["1"])
        XCTAssertEqual(app.frontKeys.chord(for: .previewSelected), KeyChord("v"))
        wc.cueList.select([audio])
        keyDoc = nil  // no workspace window is key: nothing is sent
        await app.perform(.previewSelected)
        XCTAssertTrue(transports(.preview(cue: audio)).isEmpty)
        keyDoc = "1"
        await app.perform(.previewSelected)
        XCTAssertEqual(transports(.preview(cue: audio)), ["1"])
        try await advance()
        await assertEventually("the engine previews it") { store.previewing.contains(audio) }
        await app.perform(.previewSelected)
        XCTAssertEqual(transports(.stopPreview(cue: audio)), ["1"])
        try await advance()
        await assertEventually("and stops") { !store.previewing.contains(audio) }
        wc.cueList.select([fade])
        await app.perform(.previewSelected)
        XCTAssertTrue(transports(.preview(cue: fade)).isEmpty, "only audio cues")
    }

    func testDisconnectClearsPreviewMarksAndInspectorButton() async throws {
        let audio: ModelID = "00000000-0000-0000-0000-00000000000a"
        let wc = try XCTUnwrap(app.windows["1"])
        let store = try XCTUnwrap(app.session.stores["1"])
        wc.cueList.select([audio])
        wc.inspector.setSelection([audio])
        await assertEventually { wc.inspector.previewButton.isEnabled }
        _ = await store.transport(.preview(cue: audio))
        try await advance()
        await assertEventually { store.previewing.contains(audio) }
        engine.dropConnections()
        await assertEventually { store.connectionState != .connected }
        XCTAssertTrue(store.previewing.isEmpty, "nothing is known to be previewing while disconnected")
        XCTAssertFalse(wc.inspector.previewButton.isEnabled, "Preview needs a connection")
    }

    func testSelectingByHandMovesThePlayheadButRestoringDoesNot() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
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

    func testNoReadOnlyTextViewCanTakeFocusAndSwallowTransportKeys() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        // Masthead notes are typed in Edit Mode; in Show Mode they are read-only and take no focus.
        wc.setShowMode(true)
        await assertEventually { wc.masthead.isLocked }
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
        keyDoc = "1"
        XCTAssertNil(app.handleKeyDown(try keyEvent(" ")))
        await assertEventually { !transports(go).isEmpty }
        XCTAssertEqual(transports(go), ["1"])
        // Held down: the repeat is consumed but sends nothing.
        XCTAssertNil(app.handleKeyDown(try keyEvent(" ", repeats: true)))
        await settle()
        XCTAssertEqual(transports(go).count, 1)
    }

    func testSpaceWhileTypingStaysWithTheTextField() async throws {
        keyDoc = "1"
        app.isTextEditing = { true }
        let event = try keyEvent(" ")
        XCTAssertTrue(app.handleKeyDown(event) === event)
        await settle()
        XCTAssertTrue(transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty)
    }

    func testKeysComeFromTheWorkspaceSettingsAndFallBackToDefaults() async throws {
        let list = TransportAction.go(list: "00000000-0000-0000-0000-000000000001")
        keyDoc = "1"
        try await setHotkeys("1", #"{"go":\#(Self.hotkey("g")),"pause_all":null}"#)
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
        await assertEventually { !gos().isEmpty }
        keyDoc = "1"
        XCTAssertEqual(app.frontKeys.chord(for: .go), KeyChord(" "))
        let g = try keyEvent("g")
        XCTAssertTrue(app.handleKeyDown(g) === g)
        await settle()
        XCTAssertEqual(gos(), [second])
    }

    func testConflictingHotkeysInAShowWarnInTheLogAndDoNotCrash() async throws {
        let hk = Self.hotkey("x")
        let doc = try await openShow(
            withHotkeys: #"{"go":\#(hk),"pause_all":\#(hk),"resume_all":\#(Self.hotkey("f13"))}"#)
        let store = try XCTUnwrap(app.session.stores[doc])
        keyDoc = doc
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
        keyDoc = "1"
        await app.perform(.go, isRepeat: true)
        XCTAssertTrue(transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty)
    }

    func testTypingKeepsCharacterKeysButNotCommandChords() async throws {
        keyDoc = "1"
        var typing = true
        app.isTextEditing = { typing }
        XCTAssertFalse(app.canPerform(.go), "Space types a space")
        XCTAssertFalse(app.canPerform(.pauseAll))
        XCTAssertFalse(app.canPerform(.pauseResumeSelected), "P types a p")
        XCTAssertTrue(app.canPerform(.panic))
        typing = false
        XCTAssertTrue(app.canPerform(.go))
    }

    func testMenuKeyEquivalentsFollowTheKeyWindowsKeys() async throws {
        keyDoc = "1"
        let menus = MainMenu.make()
        func item(_ c: TransportCommand) -> NSMenuItem? {
            menus.transport.items.first { $0.representedObject as? String == c.rawValue }
        }
        MainMenu.applyKeys(to: menus.transport, app.frontKeys)
        XCTAssertEqual(item(.go)?.keyEquivalent, " ")
        XCTAssertEqual(item(.pauseAll)?.keyEquivalent, "[")
        XCTAssertEqual(item(.resumeAll)?.keyEquivalent, "]")
        // Stop All has no key by default; there are no Next / Previous Cue items (format v6).
        XCTAssertEqual(item(.stopAll)?.keyEquivalent, "", "no Stop All key: Esc is the app-wide panic")
        XCTAssertEqual(item(.panic)?.keyEquivalent, "", "Esc is handled app-wide, not by the menu")
        XCTAssertEqual(item(.pauseResumeSelected)?.keyEquivalent, "p")
        XCTAssertEqual(item(.panicSelected)?.keyEquivalent, "s")
        XCTAssertEqual(item(.resetAll)?.keyEquivalent, "", "QLab has no Reset All key")
        XCTAssertEqual(item(.resetAll)?.title, loc("menu.transport.resetAll"))
        XCTAssertEqual(item(.panicSelected)?.title, loc("menu.transport.panicSelected"))
        let commands = menus.transport.items.compactMap { $0.representedObject as? String }
        XCTAssertEqual(commands.count, TransportCommand.allCases.count)

        try await setHotkeys(
            "1",
            #"{"go":\#(Self.hotkey("g", ["command"]))}"#)
        MainMenu.applyKeys(to: menus.transport, app.frontKeys)
        XCTAssertEqual(item(.go)?.keyEquivalent, "g")
        XCTAssertEqual(item(.go)?.keyEquivalentModifierMask, [.command])
    }

    /// Select Next / Select Previous are fixed View-menu items (Cmd-Down / Cmd-Up), not in the key
    /// map, and they move the selection of the front cue list; the playhead follows it.
    func testSelectNextAndPreviousAreFixedMenuItemsThatMoveTheSelection() async throws {
        keyDoc = "1"
        let menus = MainMenu.make()
        func item(_ title: String) -> NSMenuItem? { menus.view.items.first { $0.title == loc(title) } }
        XCTAssertEqual(item("menu.view.selectNext")?.keyEquivalent, KeyChord.down)
        XCTAssertEqual(item("menu.view.selectNext")?.keyEquivalentModifierMask, [.command])
        XCTAssertEqual(item("menu.view.selectPrevious")?.keyEquivalent, KeyChord.up)
        XCTAssertEqual(item("menu.view.selectPrevious")?.keyEquivalentModifierMask, [.command])
        XCTAssertNotEqual(loc("menu.view.selectNext"), "menu.view.selectNext", "string exists")
        let wc = try XCTUnwrap(app.windows["1"])
        let outline = wc.cueList.outlineView
        XCTAssertGreaterThanOrEqual(outline.numberOfRows, 2)
        outline.selectRowIndexes([0], byExtendingSelection: false)
        app.selectAdjacentCue(by: 1)
        XCTAssertEqual(outline.selectedRow, 1)
        XCTAssertEqual(wc.cueList.selectedIDs().count, 1)
        app.selectAdjacentCue(by: -1)
        XCTAssertEqual(outline.selectedRow, 0)
        app.selectAdjacentCue(by: -1)
        XCTAssertEqual(outline.selectedRow, 0, "no wrap at the top")
        // The playhead follows the selection (the new cue is sent to the engine).
        let list: ModelID = "00000000-0000-0000-0000-000000000001"
        let target = try XCTUnwrap(wc.cueList.selectedIDs().first)
        await assertEventually { !transports(.setPlayhead(list: list, cue: target)).isEmpty }
    }

    func testSuggestedFileNameIsTheFirstLineCappedToASafeLength() {
        XCTAssertEqual(AppController.suggestedFileName("Hamlet"), "Hamlet")
        XCTAssertEqual(AppController.suggestedFileName("Act 1\nsecond line"), "Act 1")
        XCTAssertEqual(AppController.suggestedFileName("  a/b:c  "), "a-b-c")
        XCTAssertEqual(AppController.suggestedFileName("\nonly the second"), "")
        let long = AppController.suggestedFileName(String(repeating: "\u{1F3AD}", count: 1024))
        XCTAssertLessThanOrEqual(long.utf8.count, 200)
        XCTAssertFalse(long.isEmpty)
    }

    func testTransportMenuTitlesAreLocalised() {
        let menus = MainMenu.make()
        for entry in menus.transport.items where !entry.isSeparatorItem {
            XCTAssertFalse(entry.title.hasPrefix("menu."), entry.title)
        }
    }

    // MARK: Show Mode (SM-01, SM-05)

    /// Stops every live cue (Intro plays from the start) so only Show Mode can raise a prompt.
    private func quiet(_ doc: ModelID) async throws {
        let store = try XCTUnwrap(app.session.stores[doc])
        _ = await store.transport(.hardStopAll)
        try await advance()
        await assertEventually("nothing live") { !store.cueStates.values.contains { $0.state.isActive } }
    }

    /// Makes the engine hold `cue` in `state` the way a show would: started then paused; started
    /// with a long pre-wait; a one-shot cue whose post-wait outlasts its file; a stop with a fade.
    private func makeLive(_ state: WireCueState, _ cue: ModelID, _ doc: ModelID = "1") async throws {
        let store = try XCTUnwrap(app.session.stores[doc])
        func send(_ c: Command) async {
            if case .failure(let e) = await store.send(command: c) { XCTFail("\(c): \(e)") }
        }
        switch state {
        case .paused:
            _ = await store.transport(.start(cue: cue))
            try await advance()
            _ = await store.transport(.pause(cue: cue))
        case .preWaiting:
            await send(.setProperty(cue, .preWait(30)))
            _ = await store.transport(.start(cue: cue))
            try await advance()
        case .postWaiting:
            // A post-wait counts only for a top-level cue the list's GO started.
            await send(.setProperty(cue, .postWait(30)))
            await send(.setProperty(cue, .continueMode("auto_continue")))
            _ = await store.transport(.goFrom(list: list, cue: cue))
            try await advance(6)  // past the end of its action
        case .stopping:
            _ = await store.transport(.start(cue: cue))
            try await advance()
            _ = await store.transport(.stop(cue: cue, fadeOut: FadeSpec(duration: 30, curve: "linear")))
        default: return XCTFail("no way to make \(state)")
        }
        try await advance()
        await assertEventually("\(cue) is \(state), not \(String(describing: store.cueStates[cue]))") {
            store.cueStates[cue]?.state == state
        }
    }

    /// SM-05: a paused, waiting or stopping cue is live; closing or quitting would lose it unasked.
    func testQuitAndCloseAskWhenOnlyAPausedOrWaitingCueIsLive() async throws {
        // The 5 s fade can be past its action with its post-wait still counting; Rain gets the long
        // pre-wait, so Intro starts at once for the stop with a fade.
        for (state, cue) in [
            (WireCueState.paused, intro), (.preWaiting, TestSnapshots.rain), (.postWaiting, fade),
            (.stopping, intro),
        ] {
            try await quiet("1")
            try await makeLive(state, cue)
            alerts.quitPlaying = false
            alerts.closePlaying = false
            let quit = await app.prepareToQuit()
            XCTAssertFalse(quit, "\(state)")
            let closed = await app.closeWorkspace("1")
            XCTAssertFalse(closed, "\(state)")
        }
        XCTAssertEqual(alerts.log, Array(repeating: ["quitPlaying", "closePlaying"], count: 4).flatMap { $0 })
    }

    func testShowModeIsPerWindow() async throws {
        let second = try await addWorkspace()
        let first = try XCTUnwrap(app.windows["1"])
        let other = try XCTUnwrap(app.windows[second])
        first.setShowMode(true)
        XCTAssertTrue(first.inspector.isLocked)
        XCTAssertTrue(first.cueToolbar.segmentedControls.allSatisfy { !$0.isEnabled })
        XCTAssertFalse(first.footer.showModeLabel.isHidden)
        XCTAssertEqual(first.footer.modeSwitch.selectedSegment, 1)
        XCTAssertFalse(other.isShowMode)
        XCTAssertFalse(other.inspector.isLocked)
        XCTAssertTrue(other.footer.showModeLabel.isHidden)
        first.setShowMode(false)
        XCTAssertFalse(first.inspector.isLocked)
        XCTAssertTrue(first.cueToolbar.segmentedControls.allSatisfy(\.isEnabled))
        XCTAssertTrue(first.footer.showModeLabel.isHidden)
    }

    /// The `set_show_mode` requests the engine received, in order.
    private func showModeRequests(_ doc: ModelID = "1") -> [Bool] {
        engine.sentOn.compactMap {
            if $0.doc == doc, case .request(_, .setShowMode(let on)) = $0.message { return on }
            return nil
        }
    }

    /// Another client of the engine (its own connection to `doc`), as a second app or OSC peer is.
    private func otherClient(_ doc: ModelID = "1") async throws -> Rig {
        let rig = Rig(engine: real, workspace: doc)
        guard await rig.connect() else { throw TestEngineError.notConnected }
        addTeardownBlock { @MainActor in rig.client.stop() }
        return rig
    }

    /// Whether the engine has `doc` in Show Mode, as a fresh connection's welcome says.
    private func engineShowMode(_ doc: ModelID = "1") async -> Bool {
        guard let rig = try? await otherClient(doc) else { return false }
        defer { rig.client.stop() }
        return rig.store.document?.showMode == true
    }

    /// SM-07: the engine holds off sleep only for a workspace it knows is in Show Mode.
    func testShowModeIsSentToTheEngineAndReadBack() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        await assertEventually { self.showModeRequests() == [true] && wc.store.document?.showMode == true }
        XCTAssertTrue(wc.isShowMode)
        wc.setShowMode(false)
        await assertEventually { self.showModeRequests() == [true, false] && wc.store.document?.showMode == false }
        XCTAssertFalse(wc.isShowMode)
    }

    /// The engine's word wins when it changes (another client, or an app restarted while the
    /// engine kept the workspace in Show Mode).
    func testTheEnginesShowModeIsTheWindowsMode() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        let other = try await otherClient()
        _ = await other.store.setShowMode(true).value
        await assertEventually { wc.isShowMode }
        XCTAssertTrue(wc.inspector.isLocked)
        // A document update that does not change the engine's mode leaves the window alone.
        wc.setShowMode(false)
        await assertEventually { wc.store.document?.showMode == false && other.store.document?.showMode == false }
        _ = await other.store.send(command: .setProperty(intro, .notes("edited elsewhere")))
        await assertEventually { wc.store.model?.cue(self.intro)?.raw["notes"]?.stringValue == "edited elsewhere" }
        await settle()
        XCTAssertFalse(wc.isShowMode)
        // An engine that sends no show_mode (an older one) means Edit Mode.
        let offline = try Rig.offline()
        let doc = #"{"path":null,"dirty":false,"can_undo":false,"can_redo":false}"#
        offline.store.receive(try ServerMessage.decode(#"{"type":"document","document":\#(doc)}"#))
        XCTAssertEqual(offline.store.document?.showMode, false, "absent show_mode is false")
    }

    /// A choice the engine could not be told (connection down) is logged and sent on reconnect.
    func testShowModeChosenWhileDisconnectedIsSentOnReconnect() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        await disconnect()
        await wc.store.setShowMode(true).value
        XCTAssertTrue(wc.isShowMode, "the window locks at once")
        XCTAssertTrue(wc.store.logEntries.contains { $0.level == .warn && $0.message.contains("Show Mode") })
        let before = await engineShowMode()
        XCTAssertFalse(before)
        await assertEventually {
            await self.sessionScheduler.fire()
            return await self.engineShowMode()
        }
        XCTAssertTrue(wc.isShowMode, "the engine's stale false did not unlock the window")
    }

    func testFooterSwitchDrivesShowModeAndKeepsTheWorkspaceNameInTheTitle() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        let switcher = wc.footer.modeSwitch
        switcher.selectedSegment = 1
        _ = switcher.target?.perform(switcher.action, with: switcher)
        await assertEventually { wc.isShowMode }
        XCTAssertEqual(wc.window?.title, String(format: loc("window.main.title.showMode"), "Demo"))
        switcher.selectedSegment = 0
        _ = switcher.target?.perform(switcher.action, with: switcher)
        await assertEventually { !wc.isShowMode }
        XCTAssertEqual(wc.window?.title, "Demo")
    }

    func testTransportStillWorksInShowMode() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        keyDoc = "1"
        let go = TransportAction.go(list: "00000000-0000-0000-0000-000000000001")
        XCTAssertNil(app.handleKeyDown(try keyEvent(" ")))
        await assertEventually { !transports(go).isEmpty }
        let result = await app.panicAll()
        if case .failure = result { XCTFail("panic must work in Show Mode") }
    }

    func testEditMenuLocksEditingItemsInShowMode() {
        let edit = MainMenu.make().edit
        MainMenu.lockEditItems(in: edit, locked: true)
        let enabled = Dictionary(
            uniqueKeysWithValues: edit.items.filter { !$0.isSeparatorItem }.map { ($0.title, $0.isEnabled) })
        for key in [
            "menu.edit.undo", "menu.edit.redo", "menu.edit.cut", "menu.edit.copy", "menu.edit.paste",
            "menu.edit.delete",
        ] {
            XCTAssertEqual(enabled[loc(key)], false, key)
        }
        MainMenu.lockEditItems(in: edit, locked: false)
        XCTAssertTrue(edit.items.filter { !$0.isSeparatorItem }.allSatisfy(\.isEnabled))
    }

    // MARK: Menu bar gaps: each new item reaches the store call or window action it names

    func testCloseAllClosesEveryWorkspaceAndCancelStopsTheRest() async throws {
        keyDoc = "1"
        let second = try await addWorkspace()
        XCTAssertEqual(app.windows.count, 2)
        alerts.closePlaying = false  // workspace 1 plays in the fixture: Cancel on it
        await app.closeAllWorkspaces()
        XCTAssertEqual(alerts.log, ["closePlaying"], "Cancel on the first leaves the rest alone")
        XCTAssertEqual(app.windows.count, 2)
        XCTAssertNotNil(app.windows[second])
        alerts.closePlaying = true
        alerts.saveChoices = [.dontSave]
        await app.closeAllWorkspaces()
        XCTAssertEqual(closeMessages().count, 2)
        XCTAssertTrue(app.windows.isEmpty)
    }

    func testLayoutMenuItemsDriveTheFooterToggles() throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        for area in LayoutArea.allCases {
            let before = wc.isVisible(area)
            app.toggleLayout(area)
            XCTAssertEqual(wc.isVisible(area), !before, "\(area)")
            XCTAssertEqual(wc.footer.toggles[area]?.state, before ? .off : .on, "footer follows \(area)")
            app.toggleLayout(area)
            XCTAssertEqual(wc.isVisible(area), before)
        }
    }

    func testInspectorTabItemsMoveTheTabAndNeedTheInspector() throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let tabs = wc.inspector.tabView
        XCTAssertGreaterThan(tabs.numberOfTabViewItems, 1)
        tabs.selectFirstTabViewItem(nil)
        app.selectAdjacentInspectorTab(by: 1)
        XCTAssertEqual(tabs.indexOfTabViewItem(try XCTUnwrap(tabs.selectedTabViewItem)), 1)
        app.selectAdjacentInspectorTab(by: -1)
        XCTAssertEqual(tabs.indexOfTabViewItem(try XCTUnwrap(tabs.selectedTabViewItem)), 0)
        wc.setVisible(.inspector, false)
        app.selectAdjacentInspectorTab(by: 1)
        XCTAssertEqual(tabs.indexOfTabViewItem(try XCTUnwrap(tabs.selectedTabViewItem)), 0, "hidden: no change")
    }

    func testSelectCueFindsACueByItsNumberAnywhereInTheList() throws {
        let list = try XCTUnwrap(app.windows["1"]).cueList
        let numbered = try XCTUnwrap(list.roots.first { $0.cue.number != nil })
        let number = try XCTUnwrap(numbered.cue.number)
        XCTAssertEqual(list.cue(numbered: number), numbered.id)
        XCTAssertEqual(list.cue(numbered: "  \(number) "), numbered.id, "blanks around it are ignored")
        XCTAssertNil(list.cue(numbered: number + "99x"))
        XCTAssertNil(list.cue(numbered: "  "), "an empty number names no cue")
    }

    func testPlayheadNextAndPreviousSendSetPlayheadForTheAdjacentRow() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let outline = wc.cueList.outlineView
        let list: ModelID = "00000000-0000-0000-0000-000000000001"
        XCTAssertGreaterThanOrEqual(outline.numberOfRows, 2)
        let current = wc.store.playheads[list].flatMap { wc.cueList.node($0) }.map { outline.row(forItem: $0) } ?? -1
        var moved = 0
        for (delta, row) in [(1, current + 1), (-1, current < 0 ? outline.numberOfRows - 1 : current - 1)]
        where outline.numberOfRows > row && row >= 0 && row != current {
            let target = try XCTUnwrap(outline.item(atRow: row) as? CueNode)
            app.movePlayheadAdjacent(by: delta)
            await assertEventually { !transports(.setPlayhead(list: list, cue: target.id)).isEmpty }
            moved += 1
        }
        XCTAssertGreaterThan(moved, 0)
    }

    func testLoadToTimeSendsTheEnginesLoadCommandForTheSelectedCue() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let cue = try XCTUnwrap(wc.cueList.roots.first).id
        wc.cueList.selectAsUser([cue])
        wc.loadToTime(cue, 12.5)
        await assertEventually { !transports(.load(cue: cue, at: 12.5)).isEmpty }
        XCTAssertNil(CueTime.parse("-1"), "a typed time is validated by the shared parser")
        XCTAssertEqual(CueTime.parse("1:02.5"), 62.5)
    }

    func testRevealTargetFileNeedsAnAudioCueWithAFile() throws {
        let list = try XCTUnwrap(app.windows["1"]).cueList
        let plain = try XCTUnwrap(list.roots.first { $0.cue.kindType != "audio" })
        list.selectAsUser([plain.id])
        XCTAssertFalse(list.canRevealSelectedTarget)
        XCTAssertFalse(list.revealSelectedTarget())
    }

    /// Show Mode (SM-01) greys every item of the locked-edit list wherever it sits in the bar, and
    /// Edit Mode leaves the movers and the number tools on.
    func testShowModeGreysTheToolsAndViewEditItems() async throws {
        keyDoc = "1"
        let delegate = AppDelegate()
        delegate.appController = app
        let wc = try XCTUnwrap(app.windows["1"])
        let items = MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items)
        let locked = items.filter { item in MainMenu.LockedEdit.allCases.contains { $0.selector == item.action } }
        XCTAssertFalse(locked.isEmpty)
        wc.setShowMode(false)
        for lock in [MainMenu.LockedEdit.renumber, .moveUp, .moveDown, .deleteNumbers] {
            for item in items where item.action == lock.selector {
                XCTAssertTrue(delegate.validateMenuItem(item), "\(lock) in Edit Mode")
            }
        }
        wc.setShowMode(true)
        for item in locked {
            XCTAssertFalse(delegate.validateMenuItem(item), item.title)
        }
        // Items that are not edits stay available in Show Mode.
        let inspector = try XCTUnwrap(
            items.first {
                MainMenu.layoutArea(of: $0) == .masthead && $0.action == #selector(AppDelegate.toggleLayoutArea(_:))
            })
        XCTAssertTrue(delegate.validateMenuItem(inspector))
        let launcher = try XCTUnwrap(items.first { $0.action == #selector(AppDelegate.showLauncherWindow(_:)) })
        XCTAssertTrue(delegate.validateMenuItem(launcher))
    }

    func testCloseInShowModeAsksOnceAndCancelKeepsTheWindow() async throws {
        let second = try await addWorkspace()
        app.windows[second]?.setShowMode(true)
        alerts.closeShowMode = false
        let kept = await app.closeWorkspace(second)
        XCTAssertFalse(kept)
        XCTAssertEqual(alerts.log, ["closeShowMode"])
        XCTAssertNotNil(app.windows[second])
        alerts.closeShowMode = true
        let closed = await app.closeWorkspace(second)
        XCTAssertTrue(closed)
        XCTAssertEqual(alerts.log, ["closeShowMode", "closeShowMode"])
    }

    func testCloseWhilePlayingInShowModeIsOnePrompt() async throws {
        app.windows["1"]?.setShowMode(true)
        alerts.closePlaying = false
        let closed = await app.closeWorkspace("1")
        XCTAssertFalse(closed)
        XCTAssertEqual(alerts.log, ["closePlaying"])
    }

    func testQuitInShowModeAsksEvenWhenNothingPlays() async throws {
        try await quiet("1")
        app.windows["1"]?.setShowMode(true)
        alerts.quitShowMode = false
        let cancelled = await app.prepareToQuit()
        XCTAssertFalse(cancelled)
        XCTAssertEqual(alerts.log, ["quitShowMode"])
        XCTAssertFalse(engine.sent.contains { if case .goodbye = $0 { return true } else { return false } })
        alerts.quitShowMode = true
        alerts.saveChoices = [.dontSave]
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
    }

    func testQuitWhilePlayingInShowModeIsOnePrompt() async throws {
        app.windows["1"]?.setShowMode(true)
        alerts.quitPlaying = false
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log, ["quitPlaying"])
    }

    func testEnteringShowModeCommitsAValidPendingEditThenLocks() async throws {
        let wc = try typePendingName("Thunder")
        await wc.requestShowMode(true)
        XCTAssertTrue(wc.isShowMode)
        XCTAssertTrue(engine.sent.contains { isCommand($0) }, "the edit was sent first")
        await assertEventually { wc.inspector.nameField.stringValue == "Thunder" }
        XCTAssertFalse(wc.inspector.nameField.isEnabled)
    }

    func testEnteringShowModeWithAnInvalidPendingEditStaysInEditMode() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.inspector.setSelection([TestSnapshots.fade])
        wc.inspector.preWaitField.stringValue = "abc"
        wc.inspector.controlTextDidChange(
            Notification(name: NSControl.textDidChangeNotification, object: wc.inspector.preWaitField))
        await wc.requestShowMode(true)
        XCTAssertFalse(wc.isShowMode)
        XCTAssertFalse(wc.inspector.isLocked)
        XCTAssertEqual(wc.footer.modeSwitch.selectedSegment, 0)
        XCTAssertFalse(engine.sent.contains { isCommand($0) })
        XCTAssertFalse(wc.inspector.messageLabel.stringValue.isEmpty, "the field's message shows")
    }

    func testLockingDropsStaleTextAndShowsTheModelsText() throws {
        let wc = try typePendingName("Never sent")
        wc.setShowMode(true)  // bypasses the commit: the safety net in the inspector
        XCTAssertEqual(wc.inspector.nameField.stringValue, "Intro")
        XCTAssertFalse(engine.sent.contains { isCommand($0) })
    }

    func testRelinkIsRefusedInShowModeAndTheMenuItemIsOff() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        let store = wc.store
        var asked = false
        let folder = try mediaFolder([])
        app.pickFolderURL = {
            asked = true
            return folder
        }
        wc.setShowMode(true)
        await app.relinkMedia(on: store)
        XCTAssertFalse(asked)
        XCTAssertTrue(relinkRequests().isEmpty)
        XCTAssertTrue(wc.sidebar.relinkLocked)
        wc.setShowMode(false)
        XCTAssertFalse(wc.sidebar.relinkLocked)
        await app.relinkMedia(on: store)
        XCTAssertTrue(asked, "Edit Mode relinks as before")
        XCTAssertEqual(relinkRequests(), [folder.path])
    }

    func testQuitIdleInEditModeAsksNoConfirmation() async throws {
        try await quiet("1")
        alerts.saveChoices = [.dontSave]
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertFalse(alerts.log.contains("quitShowMode"))
        XCTAssertFalse(alerts.log.contains("quitPlaying"))
    }

    // MARK: Close

    func testCloseWhilePlayingCancelKeepsTheWindowAndAsksNothingElse() async throws {
        alerts.closePlaying = false
        let closed = await app.closeWorkspace("1")
        XCTAssertFalse(closed)
        XCTAssertEqual(alerts.log, ["closePlaying"])
        XCTAssertNotNil(app.windows["1"])
        XCTAssertEqual(app.windows["1"]?.window?.isVisible, true)
        XCTAssertTrue(closeMessages().isEmpty)
    }

    func testCloseWhilePlayingConfirmThenUnsavedPromptThenClose() async throws {
        alerts.saveChoices = [.dontSave]
        let closed = await app.closeWorkspace("1")
        XCTAssertTrue(closed)
        XCTAssertEqual(alerts.log, ["closePlaying", "unsaved:Demo"])
        XCTAssertEqual(closeMessages().map(\.discard), [true])
        XCTAssertNil(app.windows["1"])
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

    // MARK: File > Save, Save As, Open Recent

    private func saveRequests() -> (save: Int, saveAs: [String]) {
        var paths: [String] = []
        var plain = 0
        for r in engine.sent {
            if case .request(_, .save) = r { plain += 1 }
            if case .request(_, .saveAs(let p)) = r { paths.append(p) }
        }
        return (plain, paths)
    }

    func testSaveAndSaveAsNeedAFrontWorkspaceAndWorkInShowMode() async throws {
        keyDoc = nil
        XCTAssertFalse(app.canSaveFront)
        keyDoc = "1"
        XCTAssertTrue(app.canSaveFront)
        app.windows["1"]?.setShowMode(true)
        XCTAssertTrue(app.canSaveFront, "saving is allowed in Show Mode")
        let file = try XCTUnwrap(MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == loc("menu.file") })
        let save = try XCTUnwrap(file.items.first { $0.action == #selector(AppDelegate.saveWorkspace(_:)) })
        let saveAs = try XCTUnwrap(file.items.first { $0.action == #selector(AppDelegate.saveWorkspaceAs(_:)) })
        XCTAssertEqual(save.keyEquivalent, "s")
        XCTAssertEqual(save.keyEquivalentModifierMask, [.command])
        XCTAssertEqual(saveAs.keyEquivalent, "S")
    }

    func testSaveUsesTheStoresSavePathAndSaveAsAsksForAPath() async throws {
        let store = try XCTUnwrap(app.session.stores["1"])
        keyDoc = "1"
        let ok = await app.saveWorkspace(store)
        XCTAssertTrue(ok)
        XCTAssertEqual(saveRequests().save, 1)
        XCTAssertTrue(saveRequests().saveAs.isEmpty)

        saveURL = nil  // Save As cancelled in the panel: nothing is sent
        let cancelled = await app.saveWorkspace(store, asNew: true)
        XCTAssertFalse(cancelled)
        XCTAssertEqual(saveRequests().save, 1)

        saveURL = savePath("Copy")
        let saved = await app.saveWorkspace(store, asNew: true)
        XCTAssertTrue(saved)
        XCTAssertEqual(saveRequests().saveAs, [savePath("Copy").path])
        XCTAssertTrue(FileManager.default.fileExists(atPath: savePath("Copy").path), "the engine wrote it")
        XCTAssertEqual(recents, [saveURL])
    }

    func testSaveOfAnUntitledWorkspaceShowsTheSavePanel() async throws {
        let second = try await addWorkspace()
        let store = try XCTUnwrap(app.session.stores[second])
        saveURL = savePath("Fresh")
        let ok = await app.saveWorkspace(store)
        XCTAssertTrue(ok)
        XCTAssertEqual(saveRequests().saveAs, [savePath("Fresh").path])
        await assertEventually { store.document?.path == self.savePath("Fresh").path }
    }

    func testRecentsHideWorkspacesWhoseFileIsGone() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        let here = dir.appendingPathComponent("Here.openlab")
        try Data().write(to: here)
        recents = [dir.appendingPathComponent("Gone.openlab"), here]
        app.recentExists = { FileManager.default.fileExists(atPath: $0.path) }
        XCTAssertEqual(app.existingRecentURLs(), [here])
    }

    func testOpenRecentMenuListsTheRecentWorkspacesAndClearForgetsThem() throws {
        let menu = NSMenu()
        MainMenu.fillRecent(menu, [])
        XCTAssertEqual(menu.items.map(\.title), [loc("menu.file.openRecent.none")])
        XCTAssertFalse(menu.items[0].isEnabled)
        let urls = [URL(fileURLWithPath: "/Shows/A.openlab"), URL(fileURLWithPath: "/Shows/B.openlab")]
        MainMenu.fillRecent(menu, urls)
        XCTAssertEqual(menu.items.prefix(2).map(\.title), ["A", "B"])
        XCTAssertEqual(menu.items[1].representedObject as? URL, urls[1])
        XCTAssertEqual(menu.items.last?.action, #selector(AppDelegate.clearRecents(_:)))
        var cleared = 0
        app.forgetRecents = { cleared += 1 }
        app.clearRecents()
        XCTAssertEqual(cleared, 1)
    }

    func testUnsavedSaveSavesThenClosesWithoutDiscard() async throws {
        alerts.closePlaying = true
        alerts.saveChoices = [.save]  // doc 1 has a path: plain save
        let closed = await app.closeWorkspace("1")
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
        saveURL = savePath("New")
        let closed = await app.closeWorkspace(second)
        XCTAssertTrue(closed)
        XCTAssertEqual(recents, [saveURL])
        let saveAs = engine.requestCount {
            if case .saveAs(let p) = $0 { return p == self.savePath("New").path } else { return false }
        }
        XCTAssertEqual(saveAs, 1)
    }

    func testClosingTheLastWorkspaceShowsTheLauncher() async throws {
        alerts.saveChoices = [.dontSave]
        recents = [URL(fileURLWithPath: "/Shows/Hamlet.openlab")]
        XCTAssertFalse(app.launcher.window?.isVisible ?? true)
        _ = await app.closeWorkspace("1")
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
        _ = await app.closeWorkspace("1")
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
        let url = try showFile("Lear")
        recents = [url]
        app.showLauncher()
        let root = try XCTUnwrap(app.launcher.window?.contentView)
        (find(root, AX.launcherNew) as? NSButton)?.performClick(nil)
        await assertEventually { app.windows.count == 2 }
        (find(root, AX.launcherRecentPrefix + "0") as? NSButton)?.performClick(nil)
        await assertEventually { app.windows.count == 3 }
        await assertEventually { recents.count == 2 }
        // The engine answers `open` before it lists the new doc, so the list may still be on its way.
        await assertEventually { app.session.workspaces.contains { $0.path == url.path } }
        XCTAssertTrue(app.windows.values.contains { $0.window?.title == "Lear" }, "the opened show's window")
        XCTAssertEqual(recents, [url, url])
    }

    // MARK: Quit

    // MARK: Pending inspector edits

    /// Types a name in the inspector of "1" without committing it.
    private func typePendingName(_ text: String = "Typed") throws -> WorkspaceWindowController {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.inspector.setSelection([TestSnapshots.intro])
        wc.inspector.nameField.stringValue = text
        wc.inspector.controlTextDidChange(
            Notification(name: NSControl.textDidChangeNotification, object: wc.inspector.nameField))
        return wc
    }

    private func index(of match: (ClientMessage) -> Bool) -> Int? { engine.sent.firstIndex(where: match) }
    private func isCommand(_ m: ClientMessage) -> Bool {
        if case .command = m { return true } else { return false }
    }

    func testCloseSendsThePendingEditFirst() async throws {
        _ = try typePendingName()
        alerts.saveChoices = [.dontSave]
        let closed = await app.closeWorkspace("1")
        XCTAssertTrue(closed)
        let edit = try XCTUnwrap(index { self.isCommand($0) })
        let close = try XCTUnwrap(index { if case .close = $0 { return true } else { return false } })
        XCTAssertLessThan(edit, close)
    }

    func testSaveFromThePromptSendsThePendingEditBeforeTheSave() async throws {
        _ = try typePendingName()
        await makeDirty("1")
        alerts.saveChoices = [.save]
        saveURL = savePath("x")
        _ = await app.closeWorkspace("1")
        let edit = try XCTUnwrap(index { self.isCommand($0) })
        let save = try XCTUnwrap(
            index {
                switch $0 {
                case .request(_, .save), .request(_, .saveAs): return true
                default: return false
                }
            })
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
        let wc = try XCTUnwrap(app.windows["1"])
        wc.inspector.setSelection([TestSnapshots.fade])
        func typeBadTime() {
            wc.inspector.preWaitField.stringValue = "abc"
            wc.inspector.controlTextDidChange(
                Notification(name: NSControl.textDidChangeNotification, object: wc.inspector.preWaitField))
        }
        typeBadTime()
        let closed = await app.closeWorkspace("1")
        XCTAssertFalse(closed)
        XCTAssertNotNil(app.windows["1"])
        typeBadTime()
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertFalse(engine.sent.contains { if case .goodbye = $0 { return true } else { return false } })
        XCTAssertFalse(engine.sent.contains { isCommand($0) })
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
        XCTAssertEqual(alerts.log, ["quitPlaying", "unsaved:Demo", "unsaved:Untitled"])
        let goodbyes = engine.sent.filter { if case .goodbye(true) = $0 { return true } else { return false } }
        XCTAssertEqual(goodbyes.count, 1)
    }

    /// Don't Save at quit drops the edits on the engine too: otherwise its shutdown writes a
    /// recovery file for the workspace and File > Recover Autosaves lists it later (#333).
    func testQuitDontSaveClosesWithDiscardBeforeGoodbyeAndASavedOneIsNotClosed() async throws {
        let b = try await addWorkspace()
        await makeDirty(b)  // the demo show is dirty already
        saveURL = savePath("x")
        alerts.saveChoices = [.dontSave, .save]
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        let sent = engine.sent
        let closes = sent.enumerated().compactMap { i, m -> (Int, ModelID, Bool)? in
            if case .close(_, let doc?, let discard) = m { return (i, doc, discard) }
            return nil
        }
        XCTAssertEqual(closes.count, 1, "only the workspace answered Don't Save")
        XCTAssertEqual(closes.first?.2, true)
        let goodbye = sent.firstIndex { if case .goodbye(true) = $0 { return true } else { return false } }
        XCTAssertNotNil(goodbye)
        XCTAssertLessThan(closes.first?.0 ?? .max, goodbye ?? 0)
    }

    /// A close that fails ends the discarding: each further one would wait out its own bound on a
    /// stalled engine, and the quit still says goodbye.
    func testQuitStopsDiscardingAfterTheFirstCloseFails() async throws {
        let b = try await addWorkspace()
        await makeDirty(b)
        alerts.saveChoices = [.dontSave, .dontSave]
        engine.garbleResults = true
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        let closes = engine.sent.filter { if case .close = $0 { return true } else { return false } }
        XCTAssertEqual(closes.count, 1)
        XCTAssertTrue(engine.sent.contains { if case .goodbye(true) = $0 { return true } else { return false } })
    }

    func testQuitCancelOnASecondWorkspaceAbortsAndSendsNoGoodbye() async throws {
        let b = try await addWorkspace()
        await makeDirty(b)
        alerts.saveChoices = [.dontSave, .cancel]
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log.filter { $0.hasPrefix("unsaved") }.count, 2)
        XCTAssertFalse(engine.sent.contains { if case .goodbye = $0 { return true } else { return false } })
        XCTAssertFalse(engine.sent.contains { if case .close = $0 { return true } else { return false } })
        XCTAssertEqual(app.windows.count, 2)
    }

    func testQuitWhilePlayingAsksFirstAndCancelAsksNothingMore() async throws {
        alerts.quitPlaying = false
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log, ["quitPlaying"])
        XCTAssertFalse(engine.sent.contains { if case .goodbye = $0 { return true } else { return false } })
    }

    func testSaveFailureDuringQuitAbortsWithALocalisedAlertAndNoGoodbye() async throws {
        alerts.saveChoices = [.save]
        engine.garbleResults = true
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.errors.count, 1)
        XCTAssertFalse(alerts.errors[0].contains("encoding("))
        XCTAssertFalse(engine.sent.contains { if case .goodbye = $0 { return true } else { return false } })
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
            self.engine.sent.contains { if case .goodbye = $0 { return true } else { return false } }
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

    /// #386: the engine began its shutdown (a SIGTERM, a logout) and dropped the app. No save or
    /// commit can reach it and it autosaves the dirty workspace itself: the quit asks nothing and
    /// sends nothing.
    func testQuitWhileTheEngineIsShuttingDownAsksNoSavePrompts() async throws {
        let doc = try XCTUnwrap(app.session.order.first)
        await makeDirty(doc)
        engineStopping = true
        engine.dropConnections()
        let refused = await eventually {
            await self.sessionScheduler.fire()  // the reconnect backoff
            return self.app.session.connectionState == .retrying(.engineStopping)
        }
        XCTAssertTrue(refused)
        let quit = await app.prepareToQuit()
        XCTAssertTrue(quit)
        XCTAssertEqual(alerts.log, [])
        XCTAssertFalse(engine.sent.contains { if case .goodbye = $0 { return true } else { return false } })
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
        let before = try XCTUnwrap(app.windows["1"])
        await disconnect()
        await assertEventually {
            await self.sessionScheduler.fire()
            let session = self.app.session
            return session.connectionState == .connected && session.stores["1"]?.connectionState == .connected
        }
        await settle()
        XCTAssertEqual(app.windows.count, 1)
        XCTAssertTrue(app.windows["1"] === before)
    }

    func testAlreadyOpenFromTheLauncherFrontsTheExistingWindowWithoutAnError() async throws {
        let url = try showFile("A")
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

    /// Adds a second list "Second" to `doc` through the engine.
    private func addSecondList(_ doc: ModelID) async throws -> ModelID {
        let store = try XCTUnwrap(app.session.stores[doc])
        let id: ModelID = "00000000-0000-0000-0000-000000000002"
        guard case .success = await store.send(command: .addCueList(id: id, name: "Second", index: 1)) else {
            XCTFail("add_cue_list")
            return id
        }
        await assertEventually { store.model?.cueLists.count == 2 }
        return id
    }

    func testGoAndPlayheadKeysAddressTheListPickedInTheSidebar() async throws {
        let second = try await addSecondList("1")
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        wc.sidebar.listTable.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        XCTAssertEqual(wc.cueList.listID, second)
        await app.perform(.go)
        XCTAssertEqual(transports(.go(list: second)), ["1"])
        XCTAssertTrue(transports(.go(list: "00000000-0000-0000-0000-000000000001")).isEmpty)
        wc.masthead.goButton.performClick(nil)  // the GO button follows the same list
        await assertEventually { transports(.go(list: second)).count == 2 }
    }

    func testActiveCuePanicGoesToTheWindowsOwnWorkspace() async throws {
        let other = try await addWorkspace()
        let wc = try XCTUnwrap(app.windows["1"])
        let intro: ModelID = "00000000-0000-0000-0000-00000000000a"
        await assertEventually { wc.sidebar.activeCues.contains(intro) }
        let row = try XCTUnwrap(wc.sidebar.activeCues.firstIndex(of: intro))
        let cell = try XCTUnwrap(
            wc.sidebar.activeTable.view(atColumn: 0, row: row, makeIfNecessary: true) as? ActiveCueCell)
        cell.panicButton.performClick(nil)
        await assertEventually { !transports(.panicCue(cue: intro)).isEmpty }
        XCTAssertEqual(transports(.panicCue(cue: intro)), ["1"])
        XCTAssertNotEqual(other, "1")
        cell.pauseButton.performClick(nil)
        await assertEventually { !transports(.pause(cue: intro)).isEmpty }
        XCTAssertEqual(transports(.pause(cue: intro)), ["1"])
    }

    // MARK: Helpers

    private func find(_ root: NSView, _ id: String) -> NSView? {
        if root.accessibilityIdentifier() == id { return root }
        for sub in root.subviews { if let hit = find(sub, id) { return hit } }
        return nil
    }

    // MARK: Relink missing media (WS-03)

    /// A folder holding a copy of a.wav at each of `files` (paths relative to it).
    private func mediaFolder(_ files: [String]) throws -> URL {
        let dir = real.folder.appendingPathComponent("relink-\(UUID().uuidString)", isDirectory: true)
        let wav = try XCTUnwrap(
            Bundle(for: AppControllerTests.self).url(forResource: "TestShows", withExtension: nil)?
                .appendingPathComponent("Demo/media/a.wav"))
        for f in files {
            let to = dir.appendingPathComponent(f)
            try FileManager.default.createDirectory(
                at: to.deletingLastPathComponent(), withIntermediateDirectories: true)
            try FileManager.default.copyItem(at: wav, to: to)
        }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func relinkRequests() -> [String] {
        engine.sent.compactMap {
            if case .request(_, .relink(let folder)) = $0 { return folder }
            return nil
        }
    }

    func testRelinkSendsTheFolderRequestAndReportsTheResult() async throws {
        let doc = try XCTUnwrap(app.windows.keys.first)
        let store = try XCTUnwrap(app.session.stores[doc])
        // Radio's o.wav is missing from the show; the picked folder has it.
        let folder = try mediaFolder(["Hamlet/o.wav"])
        app.pickFolderURL = { folder }
        await app.relinkMedia(on: store)
        XCTAssertEqual(relinkRequests(), [folder.path])
        // The frame is the documented one (client fixture request_relink.json), apart from id and folder.
        let sent = try XCTUnwrap(
            engine.sent.first { if case .request(_, .relink) = $0 { return true } else { return false } })
        var frame = try XCTUnwrap(JSONValue.parse(try sent.text()))
        var golden = try Fixtures.json("client", "request_relink")
        frame.set("id", .int(0))
        golden.set("id", .int(0))
        golden["request"]?.set("folder", .string(folder.path))
        XCTAssertEqual(frame, golden)
        XCTAssertTrue(alerts.errors.isEmpty, "a clean relink does not interrupt the show")
        XCTAssertEqual(
            app.windows[doc]?.footer.noticeLabel.stringValue,
            AppController.relinkSummary(relinked: 1, ambiguous: 0, unmatched: 0))
        XCTAssertEqual(store.logEntries.last?.level, .info)
        XCTAssertEqual(store.logEntries.last?.message, "Relinked 1 cue(s).")
        // The engine changed the cue: Radio plays the found file and is no longer broken.
        await assertEventually {
            store.model?.cue(TestSnapshots.radio)?.raw["kind"]?["media"]?.stringValue?.hasSuffix("Hamlet/o.wav") == true
        }
        await assertEventually { store.problems[TestSnapshots.radio]?.isEmpty ?? true }
    }

    func testRelinkWithAmbiguousOrUnmatchedCuesIsAWarningNotSuccess() async throws {
        let doc = try XCTUnwrap(app.windows.keys.first)
        let store = try XCTUnwrap(app.session.stores[doc])
        // Door slam and Rain lose their files too: the folder has Door slam's, two of Radio's, none of Rain's.
        for (cue, file) in [(TestSnapshots.door, "media/gone-door.wav"), (TestSnapshots.rain, "media/gone-rain.wav")] {
            guard case .success = await store.send(command: .setProperty(cue, .media(file))) else {
                return XCTFail("media")
            }
        }
        try await advance()
        let folder = try mediaFolder(["gone-door.wav", "x/o.wav", "y/o.wav"])
        app.pickFolderURL = { folder }
        await app.relinkMedia(on: store)
        let expected = AppController.relinkSummary(relinked: 1, ambiguous: 1, unmatched: 1)
        XCTAssertTrue(alerts.errors.isEmpty, "not modal")
        let messages = store.logEntries.map(\.message)
        XCTAssertTrue(messages.contains(expected), "\(messages)")
        XCTAssertTrue(
            messages.contains { $0.contains("Radio") && $0.contains("x/o.wav") && $0.contains("Candidates") },
            "\(messages)")
        XCTAssertTrue(messages.contains { $0.contains("Rain") && $0.hasPrefix("No match") }, "\(messages)")
        XCTAssertEqual(store.logEntries.filter { $0.level == .warn }.count, 3, "summary and one per cue")
        XCTAssertEqual(app.windows[doc]?.footer.noticeLabel.stringValue, expected)
    }

    func testCancellingTheFolderPanelSendsNothing() async throws {
        let doc = try XCTUnwrap(app.windows.keys.first)
        let store = try XCTUnwrap(app.session.stores[doc])
        app.pickFolderURL = { nil }
        await app.relinkMedia(on: store)
        XCTAssertTrue(relinkRequests().isEmpty)
        XCTAssertTrue(alerts.errors.isEmpty)
    }

    func testFailedRelinkIsShownAndLoggedAsAnError() async throws {
        let doc = try XCTUnwrap(app.windows.keys.first)
        let store = try XCTUnwrap(app.session.stores[doc])
        engine.garbleResults = true
        app.pickFolderURL = { URL(fileURLWithPath: "/m") }
        await app.relinkMedia(on: store)
        XCTAssertEqual(alerts.errors.count, 1)
        XCTAssertEqual(store.logEntries.last?.level, .error)
    }

    func testRelinkButtonInTheWarningsTabRunsTheRelink() async throws {
        let doc = try XCTUnwrap(app.windows.keys.first)
        let folder = try mediaFolder([])
        app.pickFolderURL = { folder }
        try XCTUnwrap(app.windows[doc]).onRelink?()
        await assertEventually { relinkRequests() == [folder.path] }
        await assertEventually {
            app.windows[doc]?.footer.noticeLabel.stringValue
                == AppController.relinkSummary(relinked: 0, ambiguous: 0, unmatched: 1)
        }
    }

    // MARK: Crash reports (DX-04)

    func testRevealCrashReportsShowsOnlyTheFoldersThatExist() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let present = root.appendingPathComponent("Crashes")
        try FileManager.default.createDirectory(at: present, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        var revealed: [[URL]] = []
        app.revealInFinder = { revealed.append($0) }
        app.crashFolders = { [present, root.appendingPathComponent("DiagnosticReports")] }
        app.revealCrashReports()
        XCTAssertEqual(revealed, [[present]])
        XCTAssertFalse(alerts.log.contains { $0.hasPrefix("info:") })
    }

    func testRevealCrashReportsSaysSoWhenThereAreNone() {
        var revealed = 0
        app.revealInFinder = { _ in revealed += 1 }
        app.crashFolders = { [URL(fileURLWithPath: "/nonexistent/openLab-crashes")] }
        app.revealCrashReports()
        XCTAssertEqual(revealed, 0)
        XCTAssertTrue(alerts.log.contains("info:" + loc("crash.none")))
    }

    func testHelpMenuHasTheCrashReportsItem() {
        let titles = MainMenu.make().help.items.map(\.title)
        XCTAssertTrue(titles.contains(loc("menu.help.crashReports")))
    }

    func testAnUncleanPreviousRunIsAnnouncedInTheFooterAndACleanOneIsNot() async throws {
        let wc = try XCTUnwrap(app.allWindows.first)
        // This engine is the first to run in its folder: nothing to announce.
        await app.announcePreviousRun(on: wc.store)
        XCTAssertEqual(wc.footer.noticeLabel.stringValue, "")
        // In another folder the app's own launch path starts an engine, which is killed, and
        // starts the next one there: its log says the run before it did not end cleanly.
        let dir = real.folder.appendingPathComponent("crashed", isDirectory: true)
        let info = dir.appendingPathComponent("openLab/engine.json")
        let processes = SystemEngineProcesses(
            infoURL: info, watchStderr: false,
            arguments: TestEngineOptions(clock: .stepped, appSupport: dir).engineArguments, watchStdin: true)
        func started(_ pid: Int32) async -> Bool {
            await eventually {
                if case .success(let i) = processes.readInfo() { return i.pid == pid } else { return false }
            }
        }
        let first = try processes.spawnEngine()
        let up = await started(first)
        XCTAssertTrue(up)
        processes.signal(first, .kill)
        await assertEventually { !processes.isEngine(first) }
        let second = try processes.spawnEngine()
        addTeardownBlock { @MainActor in
            processes.signal(second, .kill)
            _ = await eventually { !processes.isEngine(second) }
        }
        let upAgain = await started(second)
        XCTAssertTrue(upAgain)
        try await moveToANewEngineRun(info)
        let rebound = try XCTUnwrap(app.allWindows.first)
        await app.announcePreviousRun(on: rebound.store)
        XCTAssertEqual(rebound.footer.noticeLabel.stringValue, loc("crash.previousRun"))
    }

    // MARK: Window order and engine restarts (M5.19a, M5.2)

    func testWindowsAreInOpenOrderWithTenOrMoreWorkspaces() async throws {
        // The engine holds at most 8 docs and never reuses an id: open and close some first.
        for _ in 0..<7 {
            let doc = try await addWorkspace()
            _ = await app.session.close(doc: doc, discardChanges: true)
            await assertEventually { app.windows[doc] == nil }
        }
        for _ in 0..<3 { _ = try await addWorkspace() }
        XCTAssertEqual(app.allWindows.count, 4)
        let order = app.allWindows.compactMap { wc in app.session.doc(of: wc.store)?.raw }
        XCTAssertEqual(order, ["1", "9", "10", "11"], "\"10\" after \"9\", not after \"1\"")
    }

    func testANewEngineRunKeepsEveryWindowAndRebindsItByReopeningItsFile() async throws {
        let a = try showFile("A")
        await app.openWorkspace(url: a)
        await assertEventually { app.windows.count == 2 && app.session.stores.values.allSatisfy { $0.model != nil } }
        await settle()
        let demoPath = try XCTUnwrap(app.session.stores["1"]?.document?.path)
        let before = app.allWindows.map(ObjectIdentifier.init)
        try await moveToANewEngineRun()
        XCTAssertEqual(
            app.allWindows.map(ObjectIdentifier.init), before,
            "the same windows, none made twice: \(app.windows.mapValues { $0.store.model?.name ?? "-" })")
        XCTAssertEqual(Set(app.windows.keys), ["0", "1"], "under the new run's doc ids")
        XCTAssertEqual(app.windows["0"]?.store.model?.name, "Demo")
        XCTAssertEqual(app.windows["1"]?.store.model?.name, "A")
        let reopened = engine.sentOn.compactMap { m -> String? in
            if case .request(_, .open(let path, _)) = m.message { return path }
            return nil
        }
        XCTAssertEqual(reopened, [a.path, demoPath, a.path], "\(alerts.log) \(alerts.errors)")
    }

    /// The engine the app talks to is killed and a new one (a new run, its own engine.json) is the one
    /// it finds when it reconnects; returns once every window is reattached.
    private func moveToANewEngineRun(_ info: URL? = nil) async throws {
        if let info {
            infoURL = info
        } else {
            let next = try await TestEngine.start()
            addTeardownBlock { @MainActor in next.stop() }
            infoURL = next.infoURL
        }
        real.kill()
        var reattached = false
        let sub = app.session.events.sink { if case .reattached = $0 { reattached = true } }
        defer { sub.cancel() }
        let done = await eventually {
            await self.sessionScheduler.fire()
            return reattached
        }
        XCTAssertTrue(done)
        await assertEventually { app.session.stores.values.allSatisfy { $0.connectionState == .connected } }
    }

    // MARK: Find (ED-09) and the Network cue key

    private func findMenuItems() throws -> [NSMenuItem] {
        let edit = MainMenu.make().edit
        return try XCTUnwrap(edit.items.first { $0.title == loc("menu.edit.find") }?.submenu).items
    }

    private func keyDown(_ chars: String, _ flags: NSEvent.ModifierFlags, code: UInt16) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0, context: nil,
                characters: chars, charactersIgnoringModifiers: chars.lowercased(), isARepeat: false, keyCode: code))
    }

    func testFindMenuHasQLabsKeysAndDrivesTheKeyWindowsWorkspace() async throws {
        keyDoc = "1"
        let delegate = AppDelegate()
        delegate.appController = app
        let wc = try XCTUnwrap(app.windows["1"])
        let items = try findMenuItems()
        XCTAssertEqual(
            items.map { [$0.action.map(NSStringFromSelector) ?? "", $0.keyEquivalent] },
            [["showFind:", "f"], ["findNextCue:", "g"], ["findPreviousCue:", "G"]])
        XCTAssertTrue(items.allSatisfy { $0.keyEquivalentModifierMask == [.command] }, "AppKit reads G as Shift-G")
        XCTAssertTrue(items.allSatisfy { delegate.validateMenuItem($0) })

        delegate.showFind(items[0])
        XCTAssertTrue(wc.find.isOpen)
        wc.findBar.field.stringValue = "intro"
        wc.setFindQuery("intro")
        delegate.findNextCue(items[1])
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.intro])
        delegate.findPreviousCue(items[2])
        XCTAssertEqual(wc.cueList.selectedIDs(), [TestSnapshots.intro], "one match: Previous wraps onto it")

        // Cmd-G with the bar closed opens it, as Cmd-F does.
        wc.closeFind()
        delegate.findNextCue(items[1])
        XCTAssertTrue(wc.find.isOpen)

        // Off in Show Mode, in the menu and in the window.
        wc.setShowMode(true)
        XCTAssertTrue(items.allSatisfy { !delegate.validateMenuItem($0) })
        delegate.showFind(items[0])
        XCTAssertFalse(wc.find.isOpen)
    }

    func testFindKeysAreNotTakenByTheKeyMonitor() async throws {
        keyDoc = "1"
        let saved = NSApp.mainMenu
        NSApp.mainMenu = MainMenu.make().bar
        defer { NSApp.mainMenu = saved }
        for (chars, flags, code) in [
            ("f", NSEvent.ModifierFlags.command, UInt16(3)), ("g", .command, 5), ("G", [.command, .shift], 5),
        ] {
            let event = try keyDown(chars, flags, code: code)
            XCTAssertTrue(app.handleKeyDown(event) === event, "\(chars) reaches the menu")
        }
        await settle()
        XCTAssertTrue(panics().isEmpty)
    }

    func testEscPanicsWhetherOrNotTheFindFieldHasTheFocus() async throws {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        defer { window.orderOut(nil) }
        wc.openFind()
        // The cursor is in the field: the app-wide monitor panics and passes Esc on; the field then closes the bar.
        XCTAssertTrue(wc.findBar.fieldHasFocus)
        let esc = try escEvent()
        XCTAssertTrue(app.handleKeyDown(esc) === esc)
        await assertEventually { panics().count == 1 }
        XCTAssertTrue(wc.find.isOpen, "the monitor itself leaves the bar to the field")
        _ = wc.findBar.control(
            wc.findBar.field, textView: NSTextView(), doCommandBy: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertFalse(wc.find.isOpen)
        // Bar open, cursor in the cue list: Esc still panics, and does not close the bar.
        wc.openFind()
        window.makeFirstResponder(wc.cueList.outlineView)
        XCTAssertFalse(wc.findBar.fieldHasFocus)
        _ = app.handleKeyDown(try escEvent())
        await assertEventually { panics().count == 2 }
        await settle()
        XCTAssertEqual(panics().count, 2)
        XCTAssertTrue(wc.find.isOpen)
    }

    func testNetworkCueFromTheCuesMenuAndCmd8() async throws {
        keyDoc = "1"
        let delegate = AppDelegate()
        delegate.appController = app
        let bar = MainMenu.make().bar
        let items = bar.items.compactMap(\.submenu).flatMap(\.items)
        let entry = try XCTUnwrap(items.first { $0.keyEquivalent == "8" && $0.keyEquivalentModifierMask == [.command] })
        XCTAssertEqual(entry.representedObject as? String, "network")
        XCTAssertTrue(delegate.validateMenuItem(entry))
        func added() -> [JSONValue] {
            engine.sentOn.compactMap {
                if case .command(_, let c, _) = $0.message, c.json["type"]?.stringValue == "add_cue" { return c.json }
                return nil
            }
        }
        delegate.newCue(entry)
        await assertEventually { added().count == 1 }
        let kind = try XCTUnwrap(added().first?["cue"]?["kind"])
        XCTAssertEqual(kind["type"]?.stringValue, "network")
        let desk = try XCTUnwrap(app.session.stores["1"]?.model?.networkPatches.first)
        XCTAssertEqual(kind["destination"]?.stringValue, desk.id.raw)
        app.windows["1"]?.setShowMode(true)
        XCTAssertFalse(delegate.validateMenuItem(entry), "no new cues in Show Mode")
    }

    // MARK: Show Mode layout and quiet (SM-01, SM-03, NFR-10)

    func testShowModeHidesToolbarAndInspectorAndEditModeBringsBackTheChoice() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setVisible(.inspector, false)
        XCTAssertTrue(wc.isVisible(.toolbar))
        wc.setShowMode(true)
        XCTAssertFalse(wc.isVisible(.toolbar), "hidden, not just disabled")
        XCTAssertFalse(wc.isVisible(.inspector))
        XCTAssertTrue(wc.isVisible(.masthead), "the masthead toggle is independent of Show Mode")
        XCTAssertTrue(wc.cueList.view.superview != nil && !wc.cueList.view.isHidden)
        XCTAssertFalse(wc.footer.toggles[.toolbar]?.isEnabled ?? true)
        XCTAssertFalse(wc.footer.toggles[.inspector]?.isEnabled ?? true)
        XCTAssertEqual(wc.footer.toggles[.toolbar]?.state, .off)
        wc.setVisible(.inspector, true)
        XCTAssertFalse(wc.isVisible(.inspector), "a toggle cannot bring it back in Show Mode")
        wc.setShowMode(false)
        XCTAssertTrue(wc.isVisible(.toolbar))
        XCTAssertFalse(wc.isVisible(.inspector), "the choice made before Show Mode is back")
        XCTAssertEqual(wc.footer.toggles[.inspector]?.state, .off)
        XCTAssertTrue(wc.footer.toggles[.toolbar]?.isEnabled ?? false)
    }

    func testViewMenuGreysTheAreasShowModeHidesAndTheModeItemThatDoesNotApply() async throws {
        keyDoc = "1"
        let delegate = AppDelegate()
        delegate.appController = app
        let wc = try XCTUnwrap(app.windows["1"])
        let items = MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items)
        func layout(_ area: LayoutArea) throws -> NSMenuItem {
            try XCTUnwrap(
                items.first {
                    $0.action == #selector(AppDelegate.toggleLayoutArea(_:)) && MainMenu.layoutArea(of: $0) == area
                })
        }
        let enterShow = try XCTUnwrap(items.first { $0.action == #selector(AppDelegate.enterShowMode(_:)) })
        let enterEdit = try XCTUnwrap(items.first { $0.action == #selector(AppDelegate.enterEditMode(_:)) })
        XCTAssertTrue(delegate.validateMenuItem(enterShow))
        XCTAssertFalse(delegate.validateMenuItem(enterEdit))
        XCTAssertTrue(delegate.validateMenuItem(try layout(.inspector)))
        wc.setShowMode(true)
        XCTAssertFalse(delegate.validateMenuItem(enterShow))
        XCTAssertTrue(delegate.validateMenuItem(enterEdit))
        XCTAssertFalse(delegate.validateMenuItem(try layout(.inspector)))
        XCTAssertFalse(delegate.validateMenuItem(try layout(.toolbar)))
        XCTAssertTrue(delegate.validateMenuItem(try layout(.masthead)))
        XCTAssertTrue(delegate.validateMenuItem(try layout(.sidebar)))
        app.toggleLayout(.inspector)
        XCTAssertFalse(wc.isVisible(.inspector))
    }

    func testMastheadIsLargerAndHighContrastInShowModeAndFollowsTheWindowSize() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        let edit = wc.masthead.metrics
        wc.setShowMode(true)
        let show = wc.masthead.metrics
        XCTAssertTrue(wc.masthead.isShowMode)
        XCTAssertGreaterThan(show.goSize.width, edit.goSize.width)
        XCTAssertGreaterThan(show.goSize.height, edit.goSize.height)
        XCTAssertEqual(
            wc.masthead.goButton.constraints.first { $0.firstAttribute == .width }?.constant, show.goSize.width)
        wc.setShowMode(false)
        XCTAssertEqual(wc.masthead.metrics, edit)
        XCTAssertFalse(wc.masthead.isShowMode)
        wc.window?.setContentSize(NSSize(width: 1600, height: 1000))
        wc.setShowMode(true)
        XCTAssertEqual(
            wc.masthead.metrics,
            MastheadMetrics.make(showMode: true, window: wc.window?.contentView?.bounds.size ?? .zero))
    }

    /// Errors and notices that would be a modal in Edit Mode become a log record and a footer line.
    func testShowModeSuppressesTheNoticeSheetAndLogsIt() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        wc.showNotice("Could not do that")
        XCTAssertNil(wc.window?.attachedSheet)
        XCTAssertEqual(wc.store.logEntries.last?.message, "Could not do that")
        XCTAssertEqual(wc.store.logEntries.last?.level, .warn)
        XCTAssertEqual(wc.footer.noticeLabel.stringValue, "Could not do that")
    }

    func testShowModeSuppressesTheCrashReportsInfoAlert() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        app.crashFolders = { [URL(fileURLWithPath: "/nonexistent/openLab-crashes")] }
        wc.setShowMode(true)
        app.revealCrashReports()
        XCTAssertFalse(alerts.log.contains { $0.hasPrefix("info:") })
        XCTAssertEqual(wc.store.logEntries.last?.message, loc("crash.none"))
        XCTAssertEqual(wc.footer.noticeLabel.stringValue, loc("crash.none"))
    }

    /// Another workspace is in Show Mode while this one is closed: a failed close must not put a
    /// modal over the show.
    func testCloseTimeoutNamingNoWindowGoesToTheShowModeFooter() async throws {
        let second = try await addWorkspace()
        let show = try XCTUnwrap(app.windows["1"])
        show.setShowMode(true)
        engine.holdResults = true
        let task = Task { await app.closeWorkspace(second) }
        await assertEventually { closeMessages().count == 1 }
        // The engine closed it (its window is gone); only the answer is held back.
        await assertEventually { app.windows[second] == nil }
        await assertEventually { await quitScheduler.waitingCount > 0 }
        await quitScheduler.fire()
        let closed = await task.value
        XCTAssertFalse(closed)
        XCTAssertTrue(alerts.errors.isEmpty, "no modal about the failure: \(alerts.errors)")
        // The engine's own log lines keep arriving: the failure is the error among them.
        let failure = try XCTUnwrap(
            show.store.logEntries.last { $0.level == .error },
            "\(show.store.logEntries.map(\.message)) footer \(show.footer.noticeLabel.stringValue)")
        XCTAssertEqual(show.footer.noticeLabel.stringValue, failure.message)
        engine.releaseResults()
    }

    func testShowModeSuppressesTheSaveFailedAlertOnQuitButKeepsTheQuitConfirmation() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        alerts.saveChoices = [.save]
        engine.garbleResults = true
        let quit = await app.prepareToQuit()
        XCTAssertFalse(quit)
        XCTAssertEqual(alerts.log.first, "quitPlaying", "the one quit confirmation stays (playing outranks Show Mode)")
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(wc.store.logEntries.last?.level, .error)
    }

    func testEditModeStillShowsErrorsAsAlerts() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        app.crashFolders = { [URL(fileURLWithPath: "/nonexistent/openLab-crashes")] }
        app.revealCrashReports()
        XCTAssertTrue(alerts.log.contains("info:" + loc("crash.none")))
        XCTAssertTrue(wc.footer.noticeLabel.stringValue.isEmpty)
    }

    func testShowModeSuppressesTheOpenFailedAlert() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        engine.garbleResults = true
        await app.openWorkspace(url: try showFile("X"))
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(wc.store.logEntries.last?.level, .error)
        XCTAssertEqual(wc.footer.noticeLabel.stringValue, wc.store.logEntries.last?.message)
    }

    func testCloseRefusalNamingNoWindowGoesToTheShowModeFooter() async throws {
        let second = try await addWorkspace()
        let show = try XCTUnwrap(app.windows["1"])
        show.setShowMode(true)
        engine.garbleResults = true
        let closed = await app.closeWorkspace(second)
        XCTAssertFalse(closed)
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(show.store.logEntries.last?.level, .error)
    }

    func testShowModeSuppressesTheSaveTimeoutAlert() async throws {
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        engine.holdResults = true
        let task = Task { await app.saveWorkspace(wc.store) }
        await assertEventually { await quitScheduler.waitingCount > 0 }
        await quitScheduler.fire()
        let saved = await task.value
        XCTAssertFalse(saved)
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(wc.store.logEntries.last?.level, .error)
        engine.releaseResults()
    }

    /// An Edit Mode workspace's failure shows on its own window while another is in Show Mode: as a
    /// sheet there (which blocks nothing else), logged once, and nothing lands on the Show Mode window.
    func testRelinkFailureOfAnEditWorkspaceIsASheetOnItAndLeavesTheShowModeWindowAlone() async throws {
        let second = try await addWorkspace()
        let show = try XCTUnwrap(app.windows["1"])
        let edit = try XCTUnwrap(app.windows[second])
        show.setShowMode(true)
        engine.garbleResults = true
        app.pickFolderURL = { URL(fileURLWithPath: "/m") }
        await app.relinkMedia(on: edit.store)
        XCTAssertTrue(alerts.errors.isEmpty, "no app-level modal")
        let sheet = try XCTUnwrap(edit.window?.attachedSheet, "a sheet on the failing workspace's window")
        edit.window?.endSheet(sheet)
        XCTAssertEqual(edit.store.logEntries.filter { $0.level == .error }.count, 1, "logged once")
        XCTAssertTrue(show.footer.noticeLabel.stringValue.isEmpty)
        XCTAssertFalse(show.store.logEntries.contains { $0.level == .error })
    }

    func testLoadToTimeIsOffInShowMode() async throws {
        keyDoc = "1"
        let delegate = AppDelegate()
        delegate.appController = app
        let wc = try XCTUnwrap(app.windows["1"])
        let items = MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items)
        let item = try XCTUnwrap(items.first { $0.action == #selector(AppDelegate.loadToTime(_:)) })
        wc.cueList.selectAsUser([try XCTUnwrap(wc.store.model?.cueLists.first?.cues.first?.id)])
        XCTAssertTrue(delegate.validateMenuItem(item))
        wc.setShowMode(true)
        XCTAssertFalse(delegate.validateMenuItem(item))
        wc.promptLoadToTime()
        XCTAssertNil(wc.window?.attachedSheet)
    }

    func testFooterNoticeClearsAfterItsLifetimeAndARepeatIsNotClearedEarly() {
        let footer = FooterBar()
        var timers: [@MainActor () -> Void] = []
        footer.scheduleClear = { _, work in timers.append(work) }
        footer.showNotice("Save failed", error: true)
        footer.showNotice("Save failed", error: true)  // the same text again, later
        timers[0]()
        XCTAssertEqual(footer.noticeLabel.stringValue, "Save failed", "the first timer is stale")
        timers[1]()
        XCTAssertEqual(footer.noticeLabel.stringValue, "")
        footer.showNotice("Other")
        XCTAssertEqual(footer.noticeLabel.stringValue, "Other")
    }

    func testTheAlarmCoordinatorsAlertsAreTheQuietOnesInShowMode() async throws {
        let delegate = AppDelegate()
        delegate.appController = app
        let wc = try XCTUnwrap(app.windows["1"])
        wc.setShowMode(true)
        delegate.alarmAlerts.showError("boom")
        XCTAssertTrue(alerts.errors.isEmpty)
        XCTAssertEqual(wc.store.logEntries.last?.message, "boom")
    }

    // MARK: Esc without the system beep

    /// Counts the Esc a workspace window takes (`cancelOperation(_:)` or `keyDown`) while it is installed:
    /// the Esc that reached the end of the responder chain and found a handler instead of the beep.
    private final class CancelTap {
        private final class Box: @unchecked Sendable { var n = 0 }
        private let count = Box()
        var taken: Int { count.n }
        private var restores: [() -> Void] = []

        /// Wraps `cancelOperation:` and `keyDown:` of the workspace window: Esc reaches one of them
        /// (a table's key bindings the first, a button or tab view the second) or it beeps.
        init() throws {
            let cancel = #selector(NSResponder.cancelOperation(_:))
            // AppKit's NSResponder has no cancelOperation(_:): without a handler the key goes unhandled.
            XCTAssertNil(class_getInstanceMethod(NSResponder.self, cancel), "the base class does not handle Esc")
            let count = count
            let cancelMethod = try XCTUnwrap(class_getInstanceMethod(WorkspaceWindow.self, cancel))
            let cancelOriginal = method_getImplementation(cancelMethod)
            let cancelBlock: @convention(block) (AnyObject, Any?) -> Void = { object, sender in
                count.n += 1
                typealias Call = @convention(c) (AnyObject, Selector, Any?) -> Void
                unsafeBitCast(cancelOriginal, to: Call.self)(object, cancel, sender)
            }
            method_setImplementation(cancelMethod, imp_implementationWithBlock(cancelBlock))
            restores.append { method_setImplementation(cancelMethod, cancelOriginal) }

            let key = #selector(NSResponder.keyDown(with:))
            let keyMethod = try XCTUnwrap(class_getInstanceMethod(WorkspaceWindow.self, key))
            let keyOriginal = method_getImplementation(keyMethod)
            let keyBlock: @convention(block) (AnyObject, NSEvent) -> Void = { object, event in
                if event.keyCode == 53 { count.n += 1 }
                typealias Call = @convention(c) (AnyObject, Selector, NSEvent) -> Void
                unsafeBitCast(keyOriginal, to: Call.self)(object, key, event)
            }
            method_setImplementation(keyMethod, imp_implementationWithBlock(keyBlock))
            restores.append { method_setImplementation(keyMethod, keyOriginal) }
        }

        func restore() { for undo in restores { undo() } }
    }

    /// The front workspace window, key, with `focus` the first responder.
    private func keyWindow(focus: (WorkspaceWindowController) -> NSResponder) throws -> (
        WorkspaceWindowController, NSWindow
    ) {
        keyDoc = "1"
        let wc = try XCTUnwrap(app.windows["1"])
        let window = try XCTUnwrap(wc.window)
        window.makeKeyAndOrderFront(nil)
        let target = focus(wc)
        XCTAssertTrue(window.makeFirstResponder(target), "\(target) takes the focus")
        XCTAssertTrue(window.firstResponder === target)
        return (wc, window)
    }

    /// Esc through the app's key path: the key monitor's body, then the window's own dispatch to the
    /// first responder, as AppKit does for a key the monitor passes on.
    private func pressEsc(in window: NSWindow) throws {
        let event = try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false,
                keyCode: 53))
        if let passed = app.handleKeyDown(event) { window.sendEvent(passed) }
    }

    private func statusIcon(_ wc: WorkspaceWindowController, row: Int) throws -> NSImageView {
        let outline = wc.cueList.outlineView
        let column = outline.column(withIdentifier: NSUserInterfaceItemIdentifier("status"))
        let cell = try XCTUnwrap(outline.view(atColumn: column, row: row, makeIfNecessary: true) as? CueCell)
        return cell.images[1]
    }

    private func beeps() -> Counter {
        let counter = Counter()
        app.beep = { counter.count += 1 }
        return counter
    }

    private final class Counter { var count = 0 }

    private func assertNoBeep(_ tap: CancelTap, _ presses: Int, _ beeps: Counter, _ message: String) {
        XCTAssertEqual(
            tap.taken, presses, "every Esc that reached the window found a handler (no system beep): \(message)")
        XCTAssertEqual(beeps.count, 0, "the app did not beep either: \(message)")
    }

    func testEscOnTheCueListWithACueRunningPanicsItWithoutABeep() async throws {
        let tap = try CancelTap()
        defer { tap.restore() }
        let beeps = beeps()
        let (wc, window) = try keyWindow { $0.cueList.outlineView }
        let store = wc.store
        XCTAssertEqual(store.cueStates[intro]?.state, .running)
        XCTAssertEqual(try statusIcon(wc, row: 0).accessibilityLabel(), loc("cue.state.running"))
        try pressEsc(in: window)
        await assertEventually { panics().count == 1 }
        try await real.advance(seconds: 0.1)
        XCTAssertNotEqual(store.cueStates[intro]?.state, .running, "fading out over the panic time")
        try await real.advance(seconds: 1)  // past the show's 0.5 s panic time
        await assertEventually { store.cueStates[self.intro] == nil || store.cueStates[self.intro]?.state == .idle }
        XCTAssertTrue(try statusIcon(wc, row: 0).isHidden, "the running icon clears")
        assertNoBeep(tap, 1, beeps, "cue list")
    }

    func testEscWithNothingRunningChangesNothingAndDoesNotBeep() async throws {
        let tap = try CancelTap()
        defer { tap.restore() }
        let beeps = beeps()
        let (wc, window) = try keyWindow { $0.cueList.outlineView }
        let store = wc.store
        _ = await store.transport(.hardStopAll)
        try await real.advance(seconds: 0.1)
        await assertEventually { store.cueStates[self.intro]?.state ?? .idle == .idle }
        let (states, selection, playhead) = (store.cueStates, wc.cueList.selectedIDs(), store.playheads[list])
        try pressEsc(in: window)
        await assertEventually { panics().count == 1 }
        try await real.advance(seconds: 1)
        await settle()
        XCTAssertEqual(store.cueStates, states)
        XCTAssertEqual(wc.cueList.selectedIDs(), selection)
        XCTAssertEqual(store.playheads[list], playhead)
        XCTAssertTrue(try statusIcon(wc, row: 0).isHidden)
        assertNoBeep(tap, 1, beeps, "nothing running")
    }

    func testDoubleEscStopsAtOnceWithoutABeep() async throws {
        let tap = try CancelTap()
        defer { tap.restore() }
        let beeps = beeps()
        let (wc, window) = try keyWindow { $0.cueList.outlineView }
        let store = wc.store
        try pressEsc(in: window)
        try pressEsc(in: window)
        await assertEventually { panics().count == 2 }
        try await real.advance(seconds: 0.05)  // far inside the 0.5 s panic time: only a hard stop is over by now
        await assertEventually { store.cueStates[self.intro] == nil || store.cueStates[self.intro]?.state == .idle }
        XCTAssertTrue(try statusIcon(wc, row: 0).isHidden)
        assertNoBeep(tap, 2, beeps, "double Esc")
    }

    func testEscFromTheInspectorPanicsWithoutABeep() async throws {
        let tap = try CancelTap()
        defer { tap.restore() }
        let beeps = beeps()
        let (wc, window) = try keyWindow { $0.cueList.outlineView }
        wc.cueList.selectAsUser([intro])
        await assertEventually { wc.inspector.cueForEdit?.id == self.intro }
        XCTAssertTrue(window.makeFirstResponder(wc.inspector.tabView), "focus in the inspector, not editing")
        XCTAssertFalse(window.firstResponder is NSText, "no field is being edited")
        try pressEsc(in: window)
        await assertEventually { panics().count == 1 }
        try await real.advance(seconds: 1.2)
        await assertEventually {
            wc.store.cueStates[self.intro] == nil || wc.store.cueStates[self.intro]?.state == .idle
        }
        assertNoBeep(tap, 1, beeps, "inspector")
    }

    func testEscFromTheMastheadSidebarAndFooterPanicsWithoutABeep() async throws {
        let tap = try CancelTap()
        defer { tap.restore() }
        let beeps = beeps()
        var presses = 0
        let targets: [(String, (WorkspaceWindowController) -> NSResponder)] = [
            ("GO button", { $0.masthead.goButton }),
            ("sidebar list", { $0.sidebar.listTable }),
            ("footer control", { $0.footer.settingsButton }),
        ]
        for (name, focus) in targets {
            let (_, window) = try keyWindow(focus: focus)
            try pressEsc(in: window)
            presses += 1
            await assertEventually { panics().count == presses }
            assertNoBeep(tap, presses, beeps, name)
        }
        try await real.advance(seconds: 1.2)
        let store = try XCTUnwrap(app.windows["1"]).store
        await assertEventually { store.cueStates[self.intro] == nil || store.cueStates[self.intro]?.state == .idle }
    }
}
