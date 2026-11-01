// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Finder drops, copy / cut / paste and Delete Numbers (M5.10b) on the real engine: what the engine
/// made of each, what is refused and that Show Mode blocks every path.
@MainActor
final class CueClipboardTests: XCTestCase {
    typealias H = CueListHarness

    private func harness(_ show: String = "Demo") async throws -> (H, CueEditor, CueEditingTests.SendLog) {
        let (h, editor, log) = try await CueEditingTests.harness(show)
        editor.pasteboard = NSPasteboard.withUniqueName()
        return (h, editor, log)
    }

    /// Real WAV files (one second of silence), so the engine probes them as playable audio.
    private func tempFiles(_ names: [String], directories: [String] = []) throws -> [URL] {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        for d in directories {
            try FileManager.default.createDirectory(
                at: dir.appendingPathComponent(d), withIntermediateDirectories: true)
        }
        let demo = try XCTUnwrap(
            Bundle(for: CueEditingTests.self).url(forResource: "TestShows", withExtension: nil)?
                .appendingPathComponent("Demo/media/a.wav"))
        let wav = try Data(contentsOf: demo)
        for n in names { try (n.hasSuffix(".wav") ? wav : Data()).write(to: dir.appendingPathComponent(n)) }
        return (names + directories).map { dir.appendingPathComponent($0) }
    }

    // MARK: Finder drops

    func testDropOfFilesIsOneTransactionOfAddsInDropOrderAtThePosition() async throws {
        let (h, editor, log) = try await harness()
        let files = try tempFiles(["b thunder.wav", "a rain.MP3"])
        let before = h.vc.roots.count
        let ids = await editor.addFiles(files, parent: nil, index: 1)
        XCTAssertEqual(ids.count, 2)
        XCTAssertTrue(log.notices.isEmpty)
        await assertEventually { h.vc.roots.count == before + 2 }
        XCTAssertEqual(h.vc.roots[1].id, ids[0])
        XCTAssertEqual(h.vc.roots[2].id, ids[1])
        for (id, file) in zip(ids, files) {
            let cue = try XCTUnwrap(h.vc.node(id)?.cue)
            XCTAssertEqual(cue.kindType, "audio")
            XCTAssertEqual(cue.name, "", "named after its file")
            XCTAssertEqual(cue.raw["kind"]?["media"]?.stringValue, file.standardizedFileURL.path)
            XCTAssertNotNil(cue.number, "numbered by the engine")
        }
        // One transaction: one undo takes both away.
        await editor.undo()
        await assertEventually { h.vc.roots.count == before }
    }

    func testNonAudioAndFolderDropsAreRefusedWhole() async throws {
        let (h, editor, log) = try await harness()
        let before = h.vc.roots.count
        let files = try tempFiles(["a.wav", "notes.txt"], directories: ["folder.wav", "folder"])
        for bad in [[files[1]], [files[2]], [files[3]], [files[0], files[1]], []] {
            let ids = await editor.addFiles(bad, parent: nil, index: 0)
            XCTAssertTrue(ids.isEmpty)
        }
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(log.notices.count, 5)
        XCTAssertEqual(log.notices.first, loc("edit.error.dropNotAudio"))
        XCTAssertEqual(h.vc.roots.count, before)
    }

    func testFileDropPlan() async throws {
        let h = try await H(collapsed: false)
        let wav = try tempFiles(["a.wav", "b.flac"])
        let on = NSOutlineViewDropOnItemIndex
        let door = h.vc.node(H.door)
        // One file on an audio cue replaces its file.
        XCTAssertEqual(h.vc.fileDropPlan([wav[0]], item: door, index: on), .replace(H.door, wav[0]))
        // Two files on it make cues after it; on a group they go to its end; between rows where they fall.
        XCTAssertEqual(h.vc.fileDropPlan(wav, item: door, index: on), .insert(wav, H.group, 1))
        XCTAssertEqual(h.vc.fileDropPlan(wav, item: h.vc.node(H.group), index: on), .insert(wav, H.group, 3))
        XCTAssertEqual(h.vc.fileDropPlan(wav, item: nil, index: 0), .insert(wav, nil, 0))
        XCTAssertNil(h.vc.fileDropPlan(try tempFiles(["x.txt"]), item: nil, index: 0))
        h.vc.isEditLocked = true
        XCTAssertNil(h.vc.fileDropPlan(wav, item: nil, index: 0), "Show Mode")
    }

    func testMixedDropKeepsTheAudioFilesAndReportsTheRest() async throws {
        let (h, editor, log) = try await harness()
        let files = try tempFiles(["a.wav", "notes.txt", "b.flac"])
        let plan = try XCTUnwrap(h.vc.fileDropPlan(files, item: nil, index: 0))
        XCTAssertEqual(plan, .insert([files[0], files[2]], nil, 0))
        XCTAssertEqual(CueListViewController.skipped(files, plan), 1)
        // A folder named like an audio file is skipped too, not a reason to refuse the drop.
        let mixed = try tempFiles(["c.wav"], directories: ["Stems.wav"])
        let folderPlan = try XCTUnwrap(h.vc.fileDropPlan(mixed, item: nil, index: 0))
        XCTAssertEqual(folderPlan, .insert([mixed[0]], nil, 0))
        XCTAssertEqual(CueListViewController.skipped(mixed, folderPlan), 1)
        XCTAssertNil(h.vc.fileDropPlan([mixed[1]], item: nil, index: 0))
        XCTAssertEqual(
            String(format: loc("edit.drop.skipped.one"), 1), "1 dropped file was not audio and was left out.")
        let ids = await editor.addFiles([files[0], files[2]], parent: nil, index: 0, skipped: 1)
        XCTAssertEqual(ids.count, 2)
        XCTAssertEqual(log.notices, [String(format: loc("edit.drop.skipped.one"), 1)])
        await assertEventually { h.vc.roots.prefix(2).map(\.id) == ids }
    }

    func testChooseButtonReplacesTheFileThroughTheDropPath() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        let wav = try tempFiles(["chosen.wav"])[0]
        wc.pickAudioFile = { wav }
        // The door cue is idle (a running cue refuses a new file).
        let audio = try XCTUnwrap(wc.cueList.node(H.door)?.id)
        wc.cueList.selectAsUser([audio])
        await assertEventually { wc.inspector.chooseButton.isEnabled }
        wc.inspector.chooseButton.performClick(nil)
        await assertEventually {
            rig.store.model?.cue(H.door)?.raw["kind"]?["media"]?.stringValue == wav.standardizedFileURL.path
        }
        // A cancelled panel sends nothing; Show Mode disables the button.
        let log = CueEditingTests.SendLog(rig)
        wc.pickAudioFile = { nil }
        wc.inspector.chooseButton.performClick(nil)
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(log.sent, 0)
        wc.setShowMode(true)
        XCTAssertFalse(wc.inspector.chooseButton.isEnabled)
    }

    func testFooterGearCallsOnSettings() {
        let footer = FooterBar()
        var opened = 0
        footer.onSettings = { opened += 1 }
        XCTAssertTrue(footer.settingsButton.isEnabled)
        footer.settingsButton.performClick(nil)
        XCTAssertEqual(opened, 1)
    }

    func testReplaceFileSendsTheMediaAloneAndSkipsBusyCues() async throws {
        let (h, editor, log) = try await harness()
        let wav = try tempFiles(["new.wav"])[0]
        // A level set first: the file alone is replaced, so it stays.
        var levels = try XCTUnwrap(h.store.model?.cue(H.door)?.raw["kind"]?["levels"])
        levels.set("master_db", .double(-7))
        _ = await h.store.send(command: .setProperty(H.door, .kindFields(["levels": levels])))
        let before = try XCTUnwrap(h.store.model?.cue(H.door)?.raw["kind"])
        await editor.replaceFile(of: H.door, with: wav)
        await assertEventually {
            h.store.model?.cue(H.door)?.raw["kind"]?["media"]?.stringValue == wav.standardizedFileURL.path
        }
        var after = try XCTUnwrap(h.store.model?.cue(H.door)?.raw["kind"])
        after.set("media", before["media"] ?? .null)
        XCTAssertEqual(after, before, "levels, routing and slices are the engine's, not this copy of them")
        // A playing cue keeps its file.
        try await CueEditingTests.start(h, H.rain)
        log.reset()
        await editor.replaceFile(of: H.rain, with: wav)
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(log.notices, [loc("edit.error.replaceBusy")])
    }

    /// Both encodings are the engine's golden fixtures (engine/fixtures/protocol/client).
    func testMediaAndPasteClipboardEncodeAsTheGoldenFixtures() throws {
        let media = try Fixtures.json("client", "command_set_cue_media")["command"]
        XCTAssertEqual(
            Command.setProperty("00000000-0000-0000-0000-00000000000c", .media("/Volumes/Show/thunder.wav")).json, media
        )
        let paste = try XCTUnwrap(Fixtures.json("client", "command_paste_clipboard")["command"])
        XCTAssertEqual(
            Command.pasteClipboard(
                at: Command.position(
                    list: "00000000-0000-0000-0000-000000000001", parent: "00000000-0000-0000-0000-00000000000a",
                    index: 2),
                cues: try XCTUnwrap(paste["cues"]?.arrayValue)
            ).json, paste)
        let fromFolder = try XCTUnwrap(Fixtures.json("client", "command_paste_clipboard_from_folder")["command"])
        XCTAssertEqual(
            Command.pasteClipboard(
                at: Command.position(
                    list: "00000000-0000-0000-0000-000000000001", parent: "00000000-0000-0000-0000-00000000000a",
                    index: 2),
                cues: try XCTUnwrap(fromFolder["cues"]?.arrayValue), sourceDir: "/Volumes/Show A"
            ).json, fromFolder)
        XCTAssertEqual(
            CueProperty.media(nil).json, .object(["type": .string("media"), "value": .null]), "null clears the file")
    }

    // MARK: Copy, cut, paste

    private func put(_ editor: CueEditor, _ json: String) {
        editor.pasteboard.clearContents()
        editor.pasteboard.setString(json, forType: CueEditor.pasteType)
    }

    /// The pasteboard says which folder the cues' relative paths are in; pasted into a show elsewhere
    /// the engine reads them against it, so the copy plays the same file.
    func testPasteSendsTheFolderOfTheShowTheCuesWereCopiedFromAndNoneForAnUntitledShow() async throws {
        let (h, editor, _) = try await harness()
        let folder = try XCTUnwrap(h.store.document?.path.map { ($0 as NSString).deletingLastPathComponent })
        XCTAssertEqual(h.vc.node(H.door)?.cue.raw["kind"]?["media"]?.stringValue, "media/d.wav", "relative")
        h.vc.select([H.door])
        _ = await editor.copySelection()
        let text = try XCTUnwrap(editor.pasteboard.string(forType: CueEditor.pasteType))
        XCTAssertEqual(CueEditor.clipboardSourceDir(text), folder.precomposedStringWithCanonicalMapping)
        // Into an untitled show (no folder of its own): the copy names the file absolutely.
        let untitled = try await Rig.open(nil)
        guard case .success(let ok) = await untitled.client.send({ .request(id: $0, .new) }),
            case .opened(let doc)? = ok.data
        else { return XCTFail("new") }
        let other = try await H(collapsed: false, rig: try await Self.bound(untitled, doc))
        let editor2 = CueEditor(store: other.store, cueList: other.vc)
        editor2.pasteboard = editor.pasteboard
        await editor2.paste()
        await assertEventually { other.vc.roots.count == 1 }
        XCTAssertEqual(
            other.vc.roots.first?.cue.raw["kind"]?["media"]?.stringValue,
            (folder as NSString).appendingPathComponent("media/d.wav"))
        // Copied from that untitled show, the pasteboard names no folder.
        other.vc.select([try XCTUnwrap(other.vc.roots.first?.id)])
        _ = await editor2.copySelection()
        XCTAssertNil(
            CueEditor.clipboardSourceDir(try XCTUnwrap(editor2.pasteboard.string(forType: CueEditor.pasteType))))
        _ = await untitled.client.send({ .close(id: $0, doc: doc, discardChanges: true) })
    }

    /// A rig bound to `doc` on `app`'s engine (a show the test made with `new`).
    private static func bound(_ app: Rig, _ doc: ModelID) async throws -> Rig {
        let rig = Rig(engine: app.testEngine, workspace: doc)
        guard await rig.connect() else { throw TestEngineError.notConnected }
        return rig
    }

    func testCopyThenPasteSendsOneRawPasteAfterTheSelectionAndSelectsTheCopy() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door])
        let copied = await editor.copySelection()
        XCTAssertTrue(copied)
        XCTAssertEqual(log.sent, 0, "copy changes nothing")
        h.vc.select([H.rain])
        await editor.paste()
        await assertEventually { h.vc.node(H.group)?.children.count == 4 }
        await assertEventually { h.vc.selectedIDs().count == 1 }
        let copy = try XCTUnwrap(h.vc.selectedIDs().first)
        XCTAssertNotEqual(copy, H.door, "the copy the engine made is selected")
        let cue = try XCTUnwrap(h.vc.node(copy)?.cue)
        XCTAssertEqual(cue.name, "Door slam")
        XCTAssertEqual(cue.raw["kind"]?["media"]?.stringValue, "media/d.wav")
        XCTAssertNotEqual(cue.number, "2.1", "the engine gave it a number of its own")
        XCTAssertEqual(h.vc.node(H.group)?.children.map(\.id), [H.door, H.rain, copy, H.radio])
    }

    func testGroupWithChildrenPastesAsOneRawCueAndTheWholeCopyIsThere() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.group, H.door])
        await editor.copySelection()
        await editor.paste()
        await assertEventually { h.vc.selectedIDs().count == 1 }
        let copy = try XCTUnwrap(h.vc.selectedIDs().first)
        XCTAssertNotEqual(copy, H.group)
        XCTAssertEqual(
            h.vc.node(copy)?.children.map(\.cue.name), ["Door slam", "Rain", "Radio"],
            "the selected child is part of the group, not pasted again")
    }

    func testSeveralCopiedCuesPasteInOnePasteCommandAndAllAreSelected() async throws {
        let (h, editor, _) = try await harness()
        h.vc.select([H.intro, H.fade])
        await editor.copySelection()
        h.vc.select([H.intro])
        await editor.paste()
        await assertEventually { h.vc.selectedIDs().count == 2 }
        let copies = h.vc.selectedIDs()
        XCTAssertFalse(Set(copies).contains(H.intro))
        XCTAssertFalse(Set(copies).contains(H.fade))
        XCTAssertEqual(h.vc.roots.map(\.id), [H.intro] + copies + [H.fade, H.group])
        // One paste: one undo takes both copies away.
        await editor.undo()
        await assertEventually { h.vc.roots.map(\.id) == [H.intro, H.fade, H.group] }
    }

    /// The clipboard is what crosses workspaces; what the destination lacks is the engine's to repair
    /// (engine/crates/core/tests/command_prepare.rs, engine/crates/engine/tests/control_media_paths.rs).
    func testPasteIntoASecondWorkspaceSendsTheClipboardThere() async throws {
        let (h, editor, log) = try await harness()
        let (other, editor2, _) = try await harness("Second")
        editor2.pasteboard = editor.pasteboard  // the pasteboard is what crosses
        h.vc.select([H.door])
        await editor.copySelection()
        await editor2.paste()
        await assertEventually { other.vc.roots.count == 1 }
        XCTAssertEqual(other.vc.roots.first?.cue.name, "Door slam")
        XCTAssertEqual(log.sent, 0, "the first workspace is left alone")
        // A clipboard from a workspace with other cues, patches and numbers goes as it is: the engine
        // repairs what this one lacks (the patch, the target).
        let foreignPatch = try XCTUnwrap(
            JSONDecoder().decode(
                ModelObject.self,
                from: Data(#"{"id":"00000000-0000-0000-0000-0000000000fe","outputs":[{},{}]}"#.utf8)))
        var a = try XCTUnwrap(NewCueKind.audio.cueJSON(id: "00000000-0000-0000-0000-0000000000f1", patch: foreignPatch))
        a.set("number", .string("1"))
        a.set("name", .string("A"))
        var b = try XCTUnwrap(NewCueKind.stop.cueJSON(id: "00000000-0000-0000-0000-0000000000f2", patch: nil))
        b.set("number", .string("9"))
        b.set("name", .string("B"))
        var kind = try XCTUnwrap(b["kind"])
        kind.set("target", .string("00000000-0000-0000-0000-0000000000ff"))
        b.set("kind", kind)
        put(editor2, try JSONValue.object(["version": .int(1), "cues": .array([a, b])]).text())
        await editor2.paste()
        await assertEventually { other.vc.roots.count == 3 }
        XCTAssertEqual(Set(other.vc.roots.map(\.cue.name)), ["Door slam", "A", "B"])
        // The engine gave A this workspace's patch.
        let pasted = try XCTUnwrap(other.vc.roots.first { $0.cue.name == "A" })
        XCTAssertEqual(pasted.cue.raw["kind"]?["patch"]?.stringValue, other.store.model?.patches.first?.id.raw)
    }

    func testPasteWithNothingValidOnThePasteboardDoesNothing() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro])
        await editor.paste()
        var memo = try XCTUnwrap(NewCueKind.memo.cueJSON(id: "00000000-0000-0000-0000-0000000000f3", patch: nil))
        memo.set("name", .string("Pasted"))
        let cue = try memo.text()
        let bad = [
            "not json", "[]", #"{"version":2,"cues":[\#(cue)]}"#, #"{"cues":[\#(cue)]}"#, #"{"version":1,"cues":{}}"#,
            #"{"version":1,"cues":[]}"#, #"{"version":1,"cues":[5]}"#,
            #"{"version":1,"cues":[{"kind":{"type":"memo"}}]}"#,
            #"{"version":1,"cues":[{"id":3,"kind":{"type":"memo"}}]}"#, #"{"version":1,"cues":[{"id":"x"}]}"#,
            #"{"version":1,"cues":[{"id":"g","kind":{"type":"group","children":[{"id":"y"}]}}]}"#,
        ]
        for text in bad {
            put(editor, text)
            await editor.paste()
        }
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(h.vc.roots.count, 3)
        put(editor, #"{"version":1,"cues":[\#(cue)]}"#)
        await editor.paste()
        await assertEventually("the good payload still pastes") { h.vc.roots.map(\.cue.name).contains("Pasted") }
    }

    func testCutCopiesThenDeletesAndRefusesBusyCues() async throws {
        let (h, editor, log) = try await harness()
        try await CueEditingTests.start(h, H.rain)
        log.reset()
        h.vc.select([H.rain])
        await editor.cutSelection()
        XCTAssertEqual(log.sent, 0)
        XCTAssertEqual(log.notices, [loc("edit.error.deleteBusy")])
        XCTAssertNil(editor.pasteboard.string(forType: CueEditor.pasteType), "a refused cut copies nothing")
        h.vc.select([H.door])
        await editor.cutSelection()
        XCTAssertNotNil(editor.pasteboard.string(forType: CueEditor.pasteType))
        await assertEventually { h.vc.node(H.door) == nil }
        h.vc.select([H.intro])
        await editor.paste()
        await assertEventually { h.vc.roots.map(\.cue.name).contains("Door slam") }
    }

    /// An editor whose cue list is released before this returns (its window is gone).
    private func orphanEditor(_ store: WorkspaceStore) -> CueEditor {
        CueEditor(store: store, cueList: CueListViewController(store: store))
    }

    /// The editor must not trap when its window is gone while an edit is queued (it held the cue list unowned).
    func testEditsAfterTheCueListIsGoneDoNothing() async throws {
        let rig = try await Rig.open()
        let editor = orphanEditor(rig.store)
        let log = CueEditingTests.SendLog(rig)
        editor.pasteboard = NSPasteboard.withUniqueName()
        XCTAssertNil(editor.cueList, "the test must really release the cue list")
        let wav = try tempFiles(["a.wav"])[0]
        await editor.replaceFile(of: H.door, with: wav)
        _ = await editor.addFiles([wav], parent: nil, index: 0)
        await editor.paste()
        await editor.deleteSelection()
        await editor.deleteNumbers()
        _ = await editor.copySelection()
        await editor.move([H.door], to: nil, at: 0)
        XCTAssertEqual(log.sent, 0)
    }

    // MARK: Delete numbers

    func testDeleteNumbersClearsTheSelectedCuesNumbers() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door, H.radio])
        await editor.deleteNumbers()
        await assertEventually { h.vc.node(H.door)?.cue.number == nil && h.vc.node(H.radio)?.cue.number == nil }
        XCTAssertEqual(h.vc.node(H.rain)?.cue.number, "2.2", "only the selected cues")
        // Nothing left to clear: nothing is sent.
        log.reset()
        await editor.deleteNumbers()
        XCTAssertEqual(log.sent, 0)
    }

    func testCmdDInTheCuesMenu() {
        let menus = MainMenu.make()
        let item = menus.bar.items.compactMap(\.submenu).flatMap(\.items).first {
            $0.action == #selector(AppDelegate.deleteNumbers(_:))
        }
        XCTAssertEqual(item?.keyEquivalent, "d")
        XCTAssertEqual(item?.keyEquivalentModifierMask, [.command])
    }

    // MARK: Show Mode

    func testShowModeBlocksEveryPath() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        let first = try XCTUnwrap(wc.cueList.roots.first?.id)
        let e = wc.editor
        e.pasteboard = NSPasteboard.withUniqueName()
        wc.cueList.select([first])
        let files = try tempFiles(["a.wav"])
        wc.setShowMode(true)
        await assertEventually { rig.store.document?.showMode == true }
        let log = CueEditingTests.SendLog(rig)
        let revision = rig.store.revision
        let payload = "{\"cues\":[{\"id\":\"x\"}]}"
        e.pasteboard.setString(payload, forType: CueEditor.pasteType)
        let copied = await e.copySelection()
        XCTAssertFalse(copied)
        await e.cutSelection()
        await e.paste()
        await e.deleteNumbers()
        // The cue row menu's actions reach the same editor.
        wc.cueList.rowCut(nil)
        wc.cueList.rowPaste(nil)
        wc.cueList.rowDelete(nil)
        wc.cueList.rowCopy(nil)
        await e.addFiles(files, parent: nil, index: 0)
        await e.replaceFile(of: first, with: files[0])
        XCTAssertNil(wc.cueList.fileDropPlan(files, item: nil, index: 0))
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(log.sent, 0, "nothing reached the engine")
        await rig.sync()
        XCTAssertEqual(rig.store.revision, revision, "and the engine changed nothing")
        XCTAssertEqual(e.pasteboard.string(forType: CueEditor.pasteType), payload, "copy did not run")
    }
}
