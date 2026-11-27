// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// WS-04: copying a show's media into its show folder (QLab 5, Workspace Settings > General, File
/// Management, and its on-demand command; manual pp. 100-101, 117), on the real engine. One test
/// per acceptance flow of #779, named after it: the operator's action in the window, then what the
/// window shows (the cue's Target cell, the copying mark, the footer's notice, the menu item, the
/// settings box). The engine is stepped: a copy's end is taken in at its next `advance`.
@MainActor
final class CopyMediaTests: XCTestCase {
    private var engine: TestEngine!
    private var app: AppController!
    private var alerts: ScriptedAlerts!
    private var defaults: UserDefaults!
    private var suite = ""
    /// Files outside every show folder, as an operator's sound library.
    private var lib: URL!

    private let intro = TestSnapshots.intro
    private let door = TestSnapshots.door
    private let rain = TestSnapshots.rain

    override func setUp() async throws {
        engine = try await TestEngine.start()
        suite = "openlab-copy-media-test-\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        app = AppController(session: session, scheduler: ManualScheduler(), layoutDefaults: defaults)
        alerts = ScriptedAlerts()
        app.alerts = alerts
        session.start()
        await assertEventually { session.appClient.state == .connected }
        lib = engine.folder.appendingPathComponent("lib-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: lib, withIntermediateDirectories: true)
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        defaults?.removePersistentDomain(forName: suite)
        engine?.stop()
    }

    // MARK: Doing what the operator does

    /// A copy of the Demo show in a show folder of its own, opened in a window; `copies` is the
    /// workspace's copy setting as the file holds it (the bundled test shows hold it off).
    private func openDemo(copies: Bool) async throws -> (wc: WorkspaceWindowController, folder: URL) {
        let source = try XCTUnwrap(TestEngine.testShows?.appendingPathComponent("Demo"))
        let folder = engine.folder.appendingPathComponent("Show-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.copyItem(at: source, to: folder)
        let file = folder.appendingPathComponent("Demo.openlab")
        try editShow(file) { ws in
            var settings = ws["settings"] ?? .object([:])
            settings.set("copy_media_on_add", .bool(copies))
            ws.set("settings", settings)
            ws.set("id", .string(UUID().uuidString.lowercased()))
        }
        let wc = try await open(file)
        return (wc, folder)
    }

    private func open(_ url: URL) async throws -> WorkspaceWindowController {
        let before = Set(app.windows.keys)
        await app.openWorkspace(url: url)
        await assertEventually { self.app.windows.count == before.count + 1 }
        let doc = try XCTUnwrap(Set(app.windows.keys).subtracting(before).first)
        let wc = try XCTUnwrap(app.windows[doc])
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        await assertEventually { wc.store.document?.path != nil }
        wc.window?.layoutIfNeeded()
        return wc
    }

    /// The workspace closed as its window's close button closes it (answered: nothing unsaved).
    private func close(_ wc: WorkspaceWindowController) async {
        guard let doc = app.windows.first(where: { $0.value === wc })?.key else { return XCTFail("no window") }
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed)
        await assertEventually { self.app.windows[doc] == nil }
    }

    /// A WAV tone of `seconds` in the library (its length makes its content differ).
    private func libFile(_ path: String, seconds: Double) throws -> URL {
        let url = lib.appendingPathComponent(path)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try writeWAV(url, seconds: seconds)
        return url
    }

    /// The operator presses the cue's Target cell and picks `file` in the chooser.
    private func choose(_ file: URL, for cue: ModelID, in wc: WorkspaceWindowController) throws {
        wc.pickFile = { _ in file }
        let button = try XCTUnwrap(targetCell(wc, cue)?.chooseButton)
        XCTAssertFalse(button.isHidden)
        button.performClick(nil)
    }

    /// Lets the engine take in what its threads did, until `done` holds.
    private func advanceUntil(
        _ what: String, file: StaticString = #filePath, line: UInt = #line, _ done: @escaping () -> Bool
    ) async {
        await assertEventually(what, file: file, line: line) {
            try? await self.engine.advance(seconds: 0)
            return done()
        }
    }

    private func media(_ wc: WorkspaceWindowController, _ cue: ModelID) -> String? {
        wc.store.model?.cue(cue)?.raw["kind"]?["media"]?.stringValue
    }

    private func cell(_ wc: WorkspaceWindowController, _ cue: ModelID, _ column: String) -> CueCell? {
        let ov = wc.cueList.outlineView
        guard let node = wc.cueList.node(cue) else { return nil }
        let row = ov.row(forItem: node)
        let col = ov.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        guard row >= 0, col >= 0 else { return nil }
        return ov.view(atColumn: col, row: row, makeIfNecessary: true) as? CueCell
    }

    private func targetCell(_ wc: WorkspaceWindowController, _ cue: ModelID) -> CueCell? {
        wc.cueList.outlineView.expandItem(nil, expandChildren: true)
        return cell(wc, cue, "target")
    }

    /// What the cue's Target cell says: its label, and the file in its tooltip.
    private func target(_ wc: WorkspaceWindowController, _ cue: ModelID) -> (label: String, tip: String) {
        let button = targetCell(wc, cue)?.chooseButton
        return (button?.accessibilityLabel() ?? "", button?.toolTip ?? "")
    }

    private func names(in folder: URL) -> [String] {
        ((try? FileManager.default.contentsOfDirectory(atPath: folder.path)) ?? []).sorted()
    }

    private func notice(_ wc: WorkspaceWindowController) -> (text: String, tip: String) {
        (wc.footer.noticeLabel.stringValue, wc.footer.noticeLabel.toolTip ?? "")
    }

    private func menuItem() throws -> (AppDelegate, NSMenuItem) {
        let delegate = AppDelegate()
        delegate.appController = app
        // File > Show Files > Copy Media into Show Folder, where QLab keeps its copy command.
        let file = try XCTUnwrap(MainMenu.make().bar.items[1].submenu)
        let files = try XCTUnwrap(file.items.first { $0.title == loc("menu.file.showFiles") }?.submenu)
        let item = try XCTUnwrap(
            files.items.first { $0.action == #selector(AppDelegate.copyMediaIntoShowFolder(_:)) },
            "File > Show Files > Copy Media into Show Folder")
        XCTAssertNotNil(files.items.first { $0.action == #selector(AppDelegate.showWorkspaceInFinder(_:)) })
        return (delegate, item)
    }

    /// The File menu's command as the menu bar runs it, on the front window.
    private func chooseCopyMedia(_ wc: WorkspaceWindowController) throws {
        app.keyWindow = { wc.window }
        let (delegate, item) = try menuItem()
        XCTAssertTrue(delegate.validateMenuItem(item), "the command is on")
        delegate.copyMediaIntoShowFolder(item)
    }

    private func commandEnabled(_ wc: WorkspaceWindowController) throws -> Bool {
        app.keyWindow = { wc.window }
        let (delegate, item) = try menuItem()
        return delegate.validateMenuItem(item)
    }

    // MARK: The setting on

    /// Setting on, a saved show → a cue's file chosen in the chooser → copied into `audio`, the cue
    /// targets the copy and its Target cell shows it; the original is untouched.
    func testSettingOnAFileChosenForACueIsCopiedIntoAudioAndTheTargetCellShowsTheCopy() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let chosen = try libFile("Sounds/thunder.wav", seconds: 1)
        let original = try Data(contentsOf: chosen)
        try choose(chosen, for: intro, in: wc)
        await advanceUntil("the cue targets the copy") { self.media(wc, self.intro) == "audio/thunder.wav" }
        let copy = folder.appendingPathComponent("audio/thunder.wav")
        XCTAssertEqual(try Data(contentsOf: copy), original)
        XCTAssertEqual(try Data(contentsOf: chosen), original, "the original is untouched")
        await assertEventually { self.target(wc, self.intro).tip.contains("audio/thunder.wav") }
        // The copying mark goes with the copy's end, which the engine sends after the retarget.
        await assertEventually { self.target(wc, self.intro).label == loc("inspector.target.choose") }
        XCTAssertEqual(notice(wc).text, "", "a copy made as a file is added says nothing")
    }

    /// Setting on → files dropped on the list → each new cue targets its copy in `audio` and is
    /// named after its file.
    func testSettingOnFilesDroppedOnTheListEachNewCueTargetsItsCopyAndIsNamedAfterIt() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let a = try libFile("Drop/storm.wav", seconds: 0.5)
        let b = try libFile("Drop/wind.wav", seconds: 0.7)
        let added = await wc.editor.addFiles([a, b], parent: nil, index: 0)
        XCTAssertEqual(added.count, 2)
        await advanceUntil("both target their copies") {
            added.map { self.media(wc, $0) } == ["audio/storm.wav", "audio/wind.wav"]
        }
        XCTAssertEqual(names(in: folder.appendingPathComponent("audio")), ["storm.wav", "wind.wav"])
        XCTAssertEqual(cell(wc, added[0], "name")?.label.stringValue, "storm.wav")
    }

    /// Setting on, the file already in the show folder's `audio` folder → set as a target → a copy
    /// with "2" in its name, then "3" (QLab measured, C-R5).
    func testSettingOnAFileAlreadyInTheAudioFolderGetsANumberedCopy() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let audio = folder.appendingPathComponent("audio", isDirectory: true)
        try FileManager.default.createDirectory(at: audio, withIntermediateDirectories: true)
        let inside = audio.appendingPathComponent("hit.wav")
        try writeWAV(inside, seconds: 0.3)
        try choose(inside, for: intro, in: wc)
        await advanceUntil("hit 2") { self.media(wc, self.intro) == "audio/hit 2.wav" }
        try choose(inside, for: door, in: wc)
        await advanceUntil("hit 3") { self.media(wc, self.door) == "audio/hit 3.wav" }
        XCTAssertEqual(names(in: audio), ["hit 2.wav", "hit 3.wav", "hit.wav"])
    }

    /// Setting on, an identical file already in the folder from an earlier add → the same source set
    /// on another cue → no new copy; the cue targets the copy already there.
    func testSettingOnAnIdenticalFileAlreadyInTheFolderIsReused() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let source = try libFile("Sounds/door.wav", seconds: 0.4)
        try choose(source, for: intro, in: wc)
        await advanceUntil("copied") { self.media(wc, self.intro) == "audio/door.wav" }
        try choose(source, for: rain, in: wc)
        await advanceUntil("reused") { self.media(wc, self.rain) == "audio/door.wav" }
        XCTAssertEqual(names(in: folder.appendingPathComponent("audio")), ["door.wav"])
    }

    /// Setting on, a different file with the name of one already in the folder → copied with "2"
    /// (then "3"); neither cue's file is replaced.
    func testSettingOnADifferentFileWithATakenNameIsNumberedAndNoFileIsReplaced() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let first = try libFile("A/bell.wav", seconds: 0.3)
        let second = try libFile("B/bell.wav", seconds: 0.6)
        let third = try libFile("C/bell.wav", seconds: 0.9)
        try choose(first, for: intro, in: wc)
        await advanceUntil("bell") { self.media(wc, self.intro) == "audio/bell.wav" }
        try choose(second, for: door, in: wc)
        await advanceUntil("bell 2") { self.media(wc, self.door) == "audio/bell 2.wav" }
        try choose(third, for: rain, in: wc)
        await advanceUntil("bell 3") { self.media(wc, self.rain) == "audio/bell 3.wav" }
        let audio = folder.appendingPathComponent("audio")
        XCTAssertEqual(try Data(contentsOf: audio.appendingPathComponent("bell.wav")), try Data(contentsOf: first))
        XCTAssertEqual(try Data(contentsOf: audio.appendingPathComponent("bell 2.wav")), try Data(contentsOf: second))
    }

    /// The owner's flow (2026-11-20): two different files with the same name dropped one after the
    /// other → two files in the folder, each cue plays its own.
    func testTwoDifferentFilesWithTheSameNameDroppedOneAfterTheOtherEachCuePlaysItsOwn() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let one = try libFile("Act 1/cue.wav", seconds: 0.5)
        let two = try libFile("Act 2/cue.wav", seconds: 1.5)
        let first = await wc.editor.addFiles([one], parent: nil, index: 0)
        await advanceUntil("the first is copied") { first.first.map { self.media(wc, $0) } == "audio/cue.wav" }
        let second = await wc.editor.addFiles([two], parent: nil, index: 1)
        await advanceUntil("the second is copied") { second.first.map { self.media(wc, $0) } == "audio/cue 2.wav" }
        let audio = folder.appendingPathComponent("audio")
        XCTAssertEqual(names(in: audio), ["cue 2.wav", "cue.wav"])
        XCTAssertEqual(try Data(contentsOf: audio.appendingPathComponent("cue.wav")), try Data(contentsOf: one))
        XCTAssertEqual(try Data(contentsOf: audio.appendingPathComponent("cue 2.wav")), try Data(contentsOf: two))
        // Each plays as long as its own file.
        let (a, b) = (try XCTUnwrap(first.first), try XCTUnwrap(second.first))
        await advanceUntil("both have their lengths") {
            wc.store.mediaDurations[a]?.file == 0.5 && wc.store.mediaDurations[b]?.file == 1.5
        }
        _ = await wc.store.transport(.start(cue: b))
        try await engine.advance(seconds: 1)
        await advanceUntil("the second still plays after a second") { wc.store.cueStates[b]?.state == .running }
    }

    // MARK: The setting off, and the command

    /// Setting off → a cue's file chosen → the cue targets the file where it is; nothing is copied.
    func testSettingOffTheCueTargetsTheFileWhereItIsAndNothingIsCopied() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let settings = WorkspaceSettingsWindowController(store: wc.store)
        addTeardownBlock { @MainActor in settings.close() }
        await settings.present().value
        settings.show(page: .general)
        XCTAssertEqual(settings.copyMediaCheck.state, .on)
        settings.copyMediaCheck.performClick(nil)
        await assertEventually { wc.store.isOn(.copyMediaOnAdd) == false }
        let chosen = try libFile("rain2.wav", seconds: 0.4)
        try choose(chosen, for: intro, in: wc)
        await advanceUntil("the cue targets the file") { self.media(wc, self.intro) == chosen.path }
        try await engine.advance(seconds: 0)
        XCTAssertEqual(media(wc, intro), chosen.path)
        XCTAssertFalse(FileManager.default.fileExists(atPath: folder.appendingPathComponent("audio").path))
    }

    /// Setting off, cues on files outside the folder → File > Copy Media into Show Folder → every
    /// outside file copied, the cues retargeted, identical files shared, files already inside left
    /// alone; the notice counts them.
    func testTheCopyCommandCopiesEveryOutsideFileSharesIdenticalOnesAndLeavesInsideOnes() async throws {
        let (wc, folder) = try await openDemo(copies: false)
        let a = try libFile("x/one.wav", seconds: 0.4)
        let same = try libFile("y/one copy.wav", seconds: 0.4)
        try choose(a, for: intro, in: wc)
        try choose(same, for: door, in: wc)
        await advanceUntil("both set") {
            self.media(wc, self.door) == same.path && self.media(wc, self.intro) == a.path
        }
        try chooseCopyMedia(wc)
        await advanceUntil("both switched") {
            self.media(wc, self.intro) == "audio/one.wav" && self.media(wc, self.door) == "audio/one.wav"
        }
        XCTAssertEqual(media(wc, rain), "media/r.wav", "a file inside is left alone")
        XCTAssertEqual(names(in: folder.appendingPathComponent("audio")), ["one.wav"])
        await assertEventually { self.notice(wc).text == String(format: loc("copyMedia.notice.copied"), 1, 3) }
        XCTAssertEqual(notice(wc).tip, "")
        // The command is there with the setting on too.
        try await wc.store.send(command: .setSwitch(.copyMediaOnAdd, true)).get()
        await assertEventually { wc.store.isOn(.copyMediaOnAdd) == true }
        XCTAssertTrue(try commandEnabled(wc))
    }

    /// After a copy → Edit > Undo returns the cues to their previous targets in one step, the files
    /// stay; Redo retargets again.
    func testAfterACopyUndoReturnsTheCuesInOneStepAndRedoRetargetsThem() async throws {
        let (wc, folder) = try await openDemo(copies: false)
        let a = try libFile("a1.wav", seconds: 0.3)
        let b = try libFile("b1.wav", seconds: 0.5)
        try choose(a, for: intro, in: wc)
        try choose(b, for: door, in: wc)
        await advanceUntil("set") { self.media(wc, self.door) == b.path }
        try chooseCopyMedia(wc)
        await advanceUntil("copied") { self.media(wc, self.door) == "audio/b1.wav" }
        await wc.editor.undo()
        await assertEventually { self.media(wc, self.intro) == a.path && self.media(wc, self.door) == b.path }
        XCTAssertEqual(names(in: folder.appendingPathComponent("audio")), ["a1.wav", "b1.wav"], "the copies stay")
        await wc.editor.redo()
        await assertEventually {
            self.media(wc, self.intro) == "audio/a1.wav" && self.media(wc, self.door) == "audio/b1.wav"
        }
    }

    /// A cue playing, the command run → playback goes on and the window answers (GO, Stop All); the
    /// cues being copied show the copying mark; the command cannot be started again meanwhile.
    func testWhileACuePlaysTheCopyShowsCopyingCuesTheShowGoesOnAndTheCommandIsOff() async throws {
        let (wc, _) = try await openDemo(copies: false)
        let big = try libFile("big.wav", seconds: 30)
        try choose(big, for: door, in: wc)
        await advanceUntil("set") { self.media(wc, self.door) == big.path }
        let list = try XCTUnwrap(wc.cueList.listID)
        _ = await wc.store.go(list: list)
        try await engine.advance(seconds: 0.2)
        await assertEventually { wc.store.cueStates[self.intro]?.state == .running }
        try chooseCopyMedia(wc)
        // Stepped: the engine takes the copy's end in only at its next advance.
        await assertEventually("Door slam shows the copying mark") {
            self.target(wc, self.door).label == loc("cuelist.target.copying")
        }
        XCTAssertFalse(try commandEnabled(wc), "not a second time while one runs")
        XCTAssertEqual(wc.store.cueStates[intro]?.state, .running, "playback goes on")
        XCTAssertEqual(target(wc, intro).label, loc("inspector.target.choose"), "a file inside is not copied")
        _ = await wc.store.transport(.stopAll)
        try await engine.advance(seconds: 0.1)
        await assertEventually("Stop All answered") { wc.store.cueStates[self.intro]?.state != .running }
        await advanceUntil("the copy ends") { self.media(wc, self.door) == "audio/big.wav" }
        await assertEventually { self.target(wc, self.door).label == loc("inspector.target.choose") }
        XCTAssertTrue(try commandEnabled(wc))
    }

    /// The command finishes → the cues switch to their copies together and a short notice says how
    /// many were copied and how many were already there; a cue started before keeps playing.
    func testWhenTheCopyFinishesTheCuesSwitchTogetherTheNoticeCountsAndAStartedCueKeepsPlaying() async throws {
        let (wc, _) = try await openDemo(copies: false)
        let a = try libFile("long.wav", seconds: 5)
        let b = try libFile("short.wav", seconds: 0.5)
        try choose(a, for: intro, in: wc)
        try choose(b, for: door, in: wc)
        await advanceUntil("set") { self.media(wc, self.door) == b.path }
        await advanceUntil("probed") { wc.store.mediaDurations[self.intro]?.file == 5 }
        _ = await wc.store.transport(.start(cue: intro))
        try await engine.advance(seconds: 0.5)
        try chooseCopyMedia(wc)
        await advanceUntil("both switched") {
            self.media(wc, self.intro) == "audio/long.wav" && self.media(wc, self.door) == "audio/short.wav"
        }
        XCTAssertEqual(wc.store.cueStates[intro]?.state, .running, "the started cue plays on")
        await assertEventually { self.notice(wc).text == String(format: loc("copyMedia.notice.copied"), 2, 2) }
        try await engine.advance(seconds: 3)
        XCTAssertEqual(wc.store.cueStates[intro]?.state, .running, "to its end")
    }

    /// A cue's file missing when copying → that cue keeps its old target; the rest are copied; the
    /// notice says how many failed and lists them (cue number, file name, reason); nothing
    /// half-written is left.
    func testAFileMissingWhenCopiedKeepsItsTargetAndTheNoticeListsIt() async throws {
        let (wc, folder) = try await openDemo(copies: false)
        let gone = try libFile("gone.wav", seconds: 0.3)
        let kept = try libFile("kept.wav", seconds: 0.4)
        try choose(gone, for: door, in: wc)
        try choose(kept, for: rain, in: wc)
        await advanceUntil("set") { self.media(wc, self.rain) == kept.path }
        try FileManager.default.removeItem(at: gone)
        try chooseCopyMedia(wc)
        await advanceUntil("the rest copied") { self.media(wc, self.rain) == "audio/kept.wav" }
        XCTAssertEqual(media(wc, door), gone.path, "the failed cue keeps its target")
        let expected =
            String(format: loc("copyMedia.notice.copied"), 1, 2) + " "
            + String(format: loc("copyMedia.notice.failed"), 1)
        await assertEventually { self.notice(wc).text == expected }
        XCTAssertEqual(
            notice(wc).tip,
            String(format: loc("copyMedia.notice.failure"), "2.1", "gone.wav", loc("copyMedia.reason.missing")))
        XCTAssertEqual(names(in: folder.appendingPathComponent("audio")), ["kept.wav"])
        XCTAssertTrue(wc.store.logEntries.contains { $0.message == notice(wc).tip }, "the show log lists it")
    }

    // MARK: Unsaved, Show Mode, moving, keeping the setting

    /// Setting on, a show never saved → a file set → nothing copied; the first Save offers a new show
    /// folder with the media, and choosing it copies every file in and retargets the cues; the
    /// command is off until then.
    func testANewShowCopiesNothingUntilItsFirstSaveIntoANewShowFolderCopiesEveryFile() async throws {
        await app.newWorkspace()
        await assertEventually { self.app.windows.count == 1 }
        let (_, wc) = try XCTUnwrap(app.windows.first)
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        XCTAssertEqual(wc.store.isOn(.copyMediaOnAdd), true, "on in a new show")
        XCTAssertFalse(try commandEnabled(wc), "off until the show is saved")
        let chosen = try libFile("first.wav", seconds: 0.4)
        let added = await wc.editor.addFiles([chosen], parent: nil, index: 0)
        let cue = try XCTUnwrap(added.first)
        try await engine.advance(seconds: 0)
        XCTAssertEqual(media(wc, cue), chosen.path, "nothing is copied without a show folder")
        let place = engine.folder.appendingPathComponent("saves-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: place, withIntermediateDirectories: true)
        var offered: Bool?
        app.pickSaveURL = { _, newFolder in
            offered = newFolder
            return SaveTarget(url: place.appendingPathComponent("Opening.openlab"), newFolder: true)
        }
        let saved = await app.saveWorkspace(wc.store)
        XCTAssertTrue(saved)
        XCTAssertEqual(offered, true, "the new show folder is the choice offered first")
        let showFolder = place.appendingPathComponent("Opening", isDirectory: true)
        await assertEventually { wc.store.document?.path == showFolder.appendingPathComponent("Opening.openlab").path }
        await advanceUntil("the cue targets its copy") { self.media(wc, cue) == "audio/first.wav" }
        XCTAssertTrue(FileManager.default.fileExists(atPath: showFolder.appendingPathComponent("audio/first.wav").path))
        // On again once the copy's end (sent after the retarget) is in.
        await assertEventually { (try? self.commandEnabled(wc)) == true }
    }

    /// Checklist A2: a never-saved workspace with files (as an import's, opened dirty) is saved, the Save
    /// panel's new show folder with the media chosen → once the copy has pointed the cues at their
    /// copies the window is clean: no dot, so no save prompt at close, Command-W or Command-Q.
    func testSavingANeverSavedWorkspaceWithMediaLeavesItCleanOnceTheCopyEnds() async throws {
        await app.newWorkspace()
        await assertEventually { self.app.windows.count == 1 }
        let (doc, wc) = try XCTUnwrap(app.windows.first)
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        let chosen = try libFile("import.wav", seconds: 0.4)
        let added = await wc.editor.addFiles([chosen], parent: nil, index: 0)
        let cue = try XCTUnwrap(added.first)
        XCTAssertEqual(wc.store.document?.dirty, true)
        let place = engine.folder.appendingPathComponent("saves-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: place, withIntermediateDirectories: true)
        app.pickSaveURL = { _, _ in SaveTarget(url: place.appendingPathComponent("Imported.openlab"), newFolder: true) }
        let saved = await app.saveWorkspace(wc.store)
        XCTAssertTrue(saved)
        await advanceUntil("the cue targets its copy") { self.media(wc, cue) == "audio/import.wav" }
        await assertEventually("the window is clean after the copy") { wc.store.document?.dirty == false }
        alerts.saveChoices = []
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed, "closing asks nothing")
        XCTAssertFalse(alerts.log.contains { $0.hasPrefix("unsaved:") }, "no save prompt: \(alerts.log)")
    }

    /// The same with a file so small the copy ends as soon as it is asked for: the end is seen even
    /// when it comes before the request's own reply is handled.
    func testASmallFileCopyThatEndsAtOnceStillLeavesTheSavedWorkspaceClean() async throws {
        await app.newWorkspace()
        await assertEventually { self.app.windows.count == 1 }
        let (doc, wc) = try XCTUnwrap(app.windows.first)
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        let chosen = try libFile("tiny.wav", seconds: 0.01)
        let added = await wc.editor.addFiles([chosen], parent: nil, index: 0)
        let cue = try XCTUnwrap(added.first)
        let place = engine.folder.appendingPathComponent("saves-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: place, withIntermediateDirectories: true)
        app.pickSaveURL = { _, _ in SaveTarget(url: place.appendingPathComponent("Tiny.openlab"), newFolder: true) }
        let saved = await app.saveWorkspace(wc.store)
        XCTAssertTrue(saved)
        await advanceUntil("the cue targets its copy") { self.media(wc, cue) == "audio/tiny.wav" }
        await assertEventually("the window is clean after the copy") { wc.store.document?.dirty == false }
        alerts.saveChoices = []
        let closed = await app.closeWorkspace(doc)
        XCTAssertTrue(closed)
        XCTAssertFalse(alerts.log.contains { $0.hasPrefix("unsaved:") }, "no save prompt: \(alerts.log)")
    }

    /// An edit the operator makes while the copy runs is theirs to save or discard: the copy's end
    /// does not write it. The window stays unsaved, so a close asks.
    func testAnEditMadeDuringTheCopyIsLeftUnsavedWhenTheCopyEnds() async throws {
        await app.newWorkspace()
        await assertEventually { self.app.windows.count == 1 }
        let (doc, wc) = try XCTUnwrap(app.windows.first)
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        let chosen = try libFile("during.wav", seconds: 0.4)
        let added = await wc.editor.addFiles([chosen], parent: nil, index: 0)
        let cue = try XCTUnwrap(added.first)
        let place = engine.folder.appendingPathComponent("saves-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: place, withIntermediateDirectories: true)
        app.pickSaveURL = { _, _ in SaveTarget(url: place.appendingPathComponent("During.openlab"), newFolder: true) }
        let saved = await app.saveWorkspace(wc.store)
        XCTAssertTrue(saved)
        // The copy runs in the engine's own wakes, which the virtual clock has not yet given it.
        _ = await wc.store.setProperty(cue, .name("Edited while copying"))
        await advanceUntil("the cue targets its copy") { self.media(wc, cue) == "audio/during.wav" }
        await assertEventually("the copy has ended") { (try? self.commandEnabled(wc)) == true }
        try await engine.advance(seconds: 0)
        XCTAssertEqual(wc.store.document?.dirty, true, "the operator's edit was not saved behind their back")
        alerts.saveChoices = [.cancel]
        let closed = await app.closeWorkspace(doc)
        XCTAssertFalse(closed, "closing asks, and Cancel keeps the window")
        XCTAssertTrue(alerts.log.contains { $0.hasPrefix("unsaved:") }, "asked: \(alerts.log)")
    }

    /// The new show folder's name is taken → the next numbered one; the file-only choice copies
    /// nothing.
    func testTheNewShowFolderIsNumberedWhenTakenAndShowFileOnlyCopiesNothing() async throws {
        let place = engine.folder.appendingPathComponent("taken-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(
            at: place.appendingPathComponent("Opening", isDirectory: true), withIntermediateDirectories: true)
        let made = try await SaveTarget.makeShowFolder(for: place.appendingPathComponent("Opening.openlab")).get()
        XCTAssertEqual(made, place.appendingPathComponent("Opening 2/Opening.openlab"))
        await app.newWorkspace()
        await assertEventually { self.app.windows.count == 1 }
        let (_, wc) = try XCTUnwrap(app.windows.first)
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        let chosen = try libFile("only.wav", seconds: 0.4)
        let added = await wc.editor.addFiles([chosen], parent: nil, index: 0)
        let cue = try XCTUnwrap(added.first)
        let file = place.appendingPathComponent("Plain.openlab")
        app.pickSaveURL = { _, _ in SaveTarget(url: file, newFolder: false) }
        let saved = await app.saveWorkspace(wc.store)
        XCTAssertTrue(saved)
        await assertEventually { wc.store.document?.path == file.path }
        try await engine.advance(seconds: 0)
        XCTAssertEqual(media(wc, cue), chosen.path)
        XCTAssertFalse(FileManager.default.fileExists(atPath: place.appendingPathComponent("audio").path))
    }

    /// Show Mode on → the command is off and the setting is locked.
    func testShowModeTurnsTheCommandOffAndLocksTheSetting() async throws {
        let (wc, _) = try await openDemo(copies: true)
        let settings = WorkspaceSettingsWindowController(store: wc.store)
        addTeardownBlock { @MainActor in settings.close() }
        await settings.present().value
        settings.show(page: .general)
        XCTAssertTrue(settings.copyMediaCheck.isEnabled)
        XCTAssertTrue(try commandEnabled(wc))
        await wc.requestShowMode(true)
        await assertEventually { wc.isShowMode }
        XCTAssertFalse(try commandEnabled(wc))
        await assertEventually("the box is locked") { !settings.copyMediaCheck.isEnabled }
        await wc.requestShowMode(false)
        await assertEventually { settings.copyMediaCheck.isEnabled }
    }

    /// A show moved with its folder and opened → every cue's file is found (no missing marks) and a
    /// cue plays.
    func testAShowMovedWithItsFolderFindsEveryFileAndACuePlays() async throws {
        let (wc, folder) = try await openDemo(copies: false)
        let a = try libFile("m1.wav", seconds: 0.5)
        let b = try libFile("m2.wav", seconds: 0.7)
        try choose(a, for: intro, in: wc)
        try choose(b, for: rain, in: wc)
        await advanceUntil("set") { self.media(wc, self.rain) == b.path }
        try chooseCopyMedia(wc)
        await advanceUntil("copied") { self.media(wc, self.rain) == "audio/m2.wav" }
        let didSave = await app.saveWorkspace(wc.store)
        XCTAssertTrue(didSave)
        await assertEventually { wc.store.document?.dirty == false }
        await close(wc)
        let moved = engine.folder.appendingPathComponent("Moved-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.moveItem(at: folder, to: moved)
        try FileManager.default.removeItem(at: lib)
        let again = try await open(moved.appendingPathComponent("Demo.openlab"))
        try await engine.advance(seconds: 0)
        for cue in [intro, rain] {
            await advanceUntil("cue \(cue) has its file") {
                !(again.store.problems[cue] ?? []).contains { $0.type == "media_missing" }
                    && again.store.mediaDurations[cue] != nil
            }
            XCTAssertEqual(target(again, cue).label, loc("inspector.target.choose"), "no missing mark")
        }
        _ = await again.store.transport(.start(cue: rain))
        try await engine.advance(seconds: 0.1)
        await assertEventually { again.store.cueStates[self.rain]?.state == .running }
    }

    /// The setting changed, the show saved, closed and reopened → as the operator left it.
    func testTheSettingIsKeptWithTheShowAcrossSaveAndReopen() async throws {
        let (wc, folder) = try await openDemo(copies: true)
        let settings = WorkspaceSettingsWindowController(store: wc.store)
        await settings.present().value
        settings.show(page: .general)
        XCTAssertEqual(settings.copyMediaCheck.state, .on)
        settings.copyMediaCheck.performClick(nil)
        await assertEventually { wc.store.isOn(.copyMediaOnAdd) == false }
        settings.close()
        let didSave = await app.saveWorkspace(wc.store)
        XCTAssertTrue(didSave)
        await assertEventually { wc.store.document?.dirty == false }
        await close(wc)
        let again = try await open(folder.appendingPathComponent("Demo.openlab"))
        let reopened = WorkspaceSettingsWindowController(store: again.store)
        addTeardownBlock { @MainActor in reopened.close() }
        await reopened.present().value
        reopened.show(page: .general)
        await assertEventually { reopened.copyMediaCheck.state == .off && reopened.copyMediaCheck.isEnabled }
    }

    /// Light and dark mode, the least settings window → the setting's row and its note are whole,
    /// inside the page; the notice line fits the footer of the least workspace window.
    func testTheSettingRowAndTheNoticeShowNoClippingInLightAndDarkAtTheLeastSize() async throws {
        let (wc, _) = try await openDemo(copies: false)
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            let settings = WorkspaceSettingsWindowController(store: wc.store)
            settings.window?.appearance = NSAppearance(named: name)
            await settings.present().value
            settings.show(page: .general)
            let window = try XCTUnwrap(settings.window)
            window.setContentSize(window.contentMinSize)
            window.layoutIfNeeded()
            let box = settings.copyMediaCheck
            let content = try XCTUnwrap(window.contentView)
            let frame = box.convert(box.bounds, to: content)
            XCTAssertTrue(content.bounds.contains(frame), "\(name): the box is inside the window \(frame)")
            XCTAssertGreaterThanOrEqual(box.frame.width, box.fittingSize.width, "\(name): its title is whole")
            settings.close()
        }
        wc.showMediaCopied(
            MediaCopyReport(
                onDemand: true, copied: 120, reused: 3000, inFolder: 4, failed: [], failedCount: 2, cancelled: false))
        let window = try XCTUnwrap(wc.window)
        window.setContentSize(window.contentMinSize)
        window.layoutIfNeeded()
        let label = wc.footer.noticeLabel
        let content = try XCTUnwrap(window.contentView)
        XCTAssertTrue(content.bounds.contains(label.convert(label.bounds, to: content)), "the notice is inside")
    }
}

/// WS-04: the words a copy's end leaves (no engine).
@MainActor
final class MediaCopyNoticeTests: XCTestCase {
    private func report(onDemand: Bool, failed: [MediaCopyFailure] = [], count: UInt64? = nil, cancelled: Bool = false)
        -> MediaCopyReport
    {
        MediaCopyReport(
            onDemand: onDemand, copied: 2, reused: 1, inFolder: 3, failed: failed,
            failedCount: count ?? UInt64(failed.count), cancelled: cancelled)
    }

    func testACopyOnAddSaysNothingUnlessSomethingFailedOrItWasStopped() {
        XCTAssertFalse(MediaCopyNotice.shown(report(onDemand: false)))
        XCTAssertTrue(MediaCopyNotice.shown(report(onDemand: true)))
        let failure = MediaCopyFailure(cue: "x", file: "/a/b.wav", reason: "disk_full", message: "")
        XCTAssertTrue(MediaCopyNotice.shown(report(onDemand: false, failed: [failure])))
        XCTAssertTrue(MediaCopyNotice.shown(report(onDemand: false, cancelled: true)))
        XCTAssertEqual(MediaCopyNotice.line(report(onDemand: true, cancelled: true)), loc("copyMedia.notice.stopped"))
    }

    func testFailuresNameTheCueTheFileAndTheReasonAndCountTheUnlisted() throws {
        let rig = try Rig.offline()
        let model = rig.store.model
        let failure = MediaCopyFailure(cue: TestSnapshots.door, file: "/lib/x/door.wav", reason: "missing", message: "")
        let gone = MediaCopyFailure(cue: "no-such-cue", file: "/lib/y.wav", reason: "something new", message: "")
        let lines = MediaCopyNotice.failures(report(onDemand: true, failed: [failure, gone], count: 5), model: model)
        XCTAssertEqual(lines.count, 3)
        XCTAssertTrue(lines[0].contains("door.wav") && lines[0].contains(loc("copyMedia.reason.missing")), lines[0])
        XCTAssertTrue(
            lines[1].contains(loc("copyMedia.notice.cueGone")) && lines[1].contains(loc("copyMedia.reason.other")))
        XCTAssertEqual(lines[2], String(format: loc("copyMedia.notice.more"), 3))
        XCTAssertEqual(
            MediaCopyNotice.line(report(onDemand: true, failed: [failure], count: 1)),
            String(format: loc("copyMedia.notice.copied"), 2, 4) + " "
                + String(format: loc("copyMedia.notice.failed"), 1))
    }
}
