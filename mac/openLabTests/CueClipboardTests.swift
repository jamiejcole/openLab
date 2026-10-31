// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Finder drops, copy / cut / paste and Delete Numbers (M5.10b): the commands sent, what is refused
/// and that Show Mode blocks every path.
@MainActor
final class CueClipboardTests: XCTestCase {
    typealias H = CueListHarness

    private func harness() async throws -> (H, CueEditor, CueEditingTests.SendLog) {
        let h = try await H(collapsed: false)
        let editor = CueEditor(store: h.store, cueList: h.vc)
        editor.pasteboard = NSPasteboard.withUniqueName()
        let log = CueEditingTests.SendLog(h.rig.engine)
        editor.notify = { log.notices.append($0) }
        return (h, editor, log)
    }

    private func tempFiles(_ names: [String], directories: [String] = []) throws -> [URL] {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        for d in directories {
            try FileManager.default.createDirectory(
                at: dir.appendingPathComponent(d), withIntermediateDirectories: true)
        }
        for n in names { try Data().write(to: dir.appendingPathComponent(n)) }
        return (names + directories).map { dir.appendingPathComponent($0) }
    }

    // MARK: Finder drops

    func testDropOfFilesIsOneTransactionOfAddsInDropOrderAtThePosition() async throws {
        let (h, editor, log) = try await harness()
        let files = try tempFiles(["b thunder.wav", "a rain.MP3"])
        let before = h.vc.roots.count
        let ids = await editor.addFiles(files, parent: nil, index: 1)
        XCTAssertEqual(ids.count, 2)
        XCTAssertEqual(log.commands.count, 1)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "transaction")
        let adds = try XCTUnwrap(sent["commands"]?.arrayValue)
        XCTAssertEqual(adds.map { $0["type"]?.stringValue }, ["add_cue", "add_cue"])
        XCTAssertEqual(adds.map { $0["at"]?["index"] }, [.int(1), .int(2)])
        XCTAssertEqual(adds.map { $0["at"]?["list"]?.stringValue }, [H.list, H.list])
        XCTAssertEqual(adds.map { $0["cue"]?["name"]?.stringValue }, ["", ""])
        XCTAssertEqual(adds.map { $0["cue"]?["id"]?.stringValue }, ids.map(\.raw))
        XCTAssertEqual(adds.map { $0["cue"]?["kind"]?["type"]?.stringValue }, ["audio", "audio"])
        XCTAssertEqual(
            adds.map { $0["cue"]?["kind"]?["media"]?.stringValue }, files.map { $0.standardizedFileURL.path })
        XCTAssertEqual(adds.map { $0["auto_number"] }, [.bool(true), .bool(true)])
        await assertEventually { h.vc.roots.count == before + 2 }
        XCTAssertEqual(h.vc.roots[1].id, ids[0])
        XCTAssertEqual(h.vc.roots[2].id, ids[1])
    }

    func testNonAudioAndFolderDropsAreRefusedWhole() async throws {
        let (_, editor, log) = try await harness()
        let files = try tempFiles(["a.wav", "notes.txt"], directories: ["folder.wav", "folder"])
        for bad in [[files[1]], [files[2]], [files[3]], [files[0], files[1]], []] {
            let ids = await editor.addFiles(bad, parent: nil, index: 0)
            XCTAssertTrue(ids.isEmpty)
        }
        XCTAssertTrue(log.commands.isEmpty)
        XCTAssertEqual(log.notices.count, 5)
        XCTAssertEqual(log.notices.first, loc("edit.error.dropNotAudio"))
    }

    func testFileDropPlan() async throws {
        let (h, _, _) = try await harness()
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
    }

    func testChooseButtonReplacesTheFileThroughTheDropPath() async throws {
        let h = try await H(collapsed: false)
        let wc = WorkspaceWindowController(store: h.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        let log = CueEditingTests.SendLog(h.rig.engine)
        let wav = try tempFiles(["chosen.wav"])[0]
        wc.pickAudioFile = { wav }
        // The door cue is idle in the demo show (a running cue refuses a new file).
        let audio = try XCTUnwrap(wc.cueList.node(H.door)?.id)
        wc.cueList.selectAsUser([audio])
        await assertEventually { wc.inspector.chooseButton.isEnabled }
        wc.inspector.chooseButton.performClick(nil)
        await assertEventually { log.commands.count == 1 }
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["property"]?["type"]?.stringValue, "media")
        XCTAssertEqual(sent["property"]?["value"]?.stringValue, wav.standardizedFileURL.path)
        // A cancelled panel sends nothing; Show Mode disables the button.
        wc.pickAudioFile = { nil }
        wc.inspector.chooseButton.performClick(nil)
        wc.setShowMode(true)
        XCTAssertFalse(wc.inspector.chooseButton.isEnabled)
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(log.commands.count, 1)
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
        _ = h  // the window owns the cue list for as long as the editor lives
        let wav = try tempFiles(["new.wav"])[0]
        await editor.replaceFile(of: H.door, with: wav)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "set_cue_property")
        XCTAssertEqual(sent["id"]?.stringValue, H.door.raw)
        // The file alone: levels, routing and slices are the engine's, not this copy of them.
        XCTAssertEqual(sent["property"]?["type"]?.stringValue, "media")
        XCTAssertEqual(sent["property"]?["value"]?.stringValue, wav.standardizedFileURL.path)
        // Rain is paused in the demo show.
        await editor.replaceFile(of: H.rain, with: wav)
        XCTAssertEqual(log.commands.count, 1)
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

    private func pasted(_ log: CueEditingTests.SendLog) throws -> (at: JSONValue, cues: [JSONValue]) {
        XCTAssertEqual(log.commands.count, 1)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "paste_clipboard", "one command with the raw clipboard")
        return (try XCTUnwrap(sent["at"]), try XCTUnwrap(sent["cues"]?.arrayValue))
    }

    func testPasteSendsTheFolderOfTheShowTheCuesWereCopiedFromAndNoneForAnUntitledShow() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door])
        h.rig.engine.push(
            #"{"type":"document","document":{"path":null,"dirty":true,"can_undo":false,"can_redo":false}}"#)
        await assertEventually { h.store.document?.path == nil }
        _ = await editor.copySelection()
        await editor.paste()
        XCTAssertNil(log.commands.first?["source_dir"], "nothing to resolve against")

        h.rig.engine.push(
            #"{"type":"document","document":{"path":"/Volumes/Show A/Hamlet.openlab","dirty":false,"can_undo":false,"can_redo":false}}"#
        )
        await assertEventually { h.store.document?.path == "/Volumes/Show A/Hamlet.openlab" }
        h.vc.select([H.door])
        _ = await editor.copySelection()
        let mark = log.commands.count
        await editor.paste()
        XCTAssertEqual(log.commands[mark]["source_dir"]?.stringValue, "/Volumes/Show A")
    }

    func testCopyThenPasteSendsOneRawPasteAfterTheSelectionAndSelectsTheCopy() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door])
        let copied = await editor.copySelection()
        XCTAssertTrue(copied)
        XCTAssertTrue(log.commands.isEmpty, "copy changes nothing")
        h.vc.select([H.rain])
        await editor.paste()
        let (at, cues) = try pasted(log)
        XCTAssertEqual(at["parent"]?.stringValue, H.group.raw)
        XCTAssertEqual(at["index"], .int(2))
        XCTAssertEqual(at["list"]?.stringValue, H.list)
        // The clipboard goes as copied: the engine makes the ids and fixes numbers and patches.
        XCTAssertEqual(cues.first?["id"]?.stringValue, H.door.raw)
        XCTAssertEqual(cues.first?["name"]?.stringValue, "Door slam")
        XCTAssertEqual(cues.first?["kind"]?["media"]?.stringValue, "media/d.wav")
        await assertEventually { h.vc.node(H.group)?.children.count == 4 }
        await assertEventually { h.vc.selectedIDs().count == 1 }
        let copy = try XCTUnwrap(h.vc.selectedIDs().first)
        XCTAssertNotEqual(copy, H.door, "the copy the engine made is selected")
        XCTAssertEqual(h.vc.node(copy)?.cue.name, "Door slam")
        XCTAssertEqual(h.vc.node(H.group)?.children[2].id, copy)
    }

    func testGroupWithChildrenPastesAsOneRawCueAndTheWholeCopyIsThere() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.group, H.door])
        await editor.copySelection()
        await editor.paste()
        let (_, cues) = try pasted(log)
        XCTAssertEqual(cues.count, 1, "the selected child is part of the group")
        XCTAssertEqual(cues[0]["kind"]?["children"]?.arrayValue?.count, 3)
        await assertEventually { h.vc.selectedIDs().count == 1 }
        let copy = try XCTUnwrap(h.vc.selectedIDs().first)
        XCTAssertNotEqual(copy, H.group)
        XCTAssertEqual(h.vc.node(copy)?.children.count, 3)
    }

    func testSeveralCopiedCuesPasteInOnePasteCommandAndAllAreSelected() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro, H.fade])
        await editor.copySelection()
        h.vc.select([H.intro])
        await editor.paste()
        let (at, cues) = try pasted(log)
        XCTAssertEqual(at["index"], .int(1))
        XCTAssertEqual(cues.map { $0["id"]?.stringValue }, [H.intro.raw, H.fade.raw])
        await assertEventually { h.vc.selectedIDs().count == 2 }
        XCTAssertFalse(Set(h.vc.selectedIDs()).contains(H.intro))
        XCTAssertFalse(Set(h.vc.selectedIDs()).contains(H.fade))
    }

    /// The clipboard is what crosses workspaces; what the destination lacks is the engine's to repair
    /// (engine/crates/core/tests/command_prepare.rs, engine/crates/engine/tests/control_media_paths.rs).
    func testPasteIntoASecondWorkspaceSendsTheClipboardThere() async throws {
        let (h, editor, log) = try await harness()
        let other = try await H(collapsed: false)
        let editor2 = CueEditor(store: other.store, cueList: other.vc)
        editor2.pasteboard = editor.pasteboard  // the pasteboard is what crosses
        let log2 = CueEditingTests.SendLog(other.rig.engine)
        h.vc.select([H.door])
        await editor.copySelection()
        other.vc.select([H.intro])
        await editor2.paste()
        let (at, cues) = try pasted(log2)
        XCTAssertEqual(at["index"], .int(1))
        XCTAssertEqual(cues.first?["name"]?.stringValue, "Door slam")
        XCTAssertTrue(log.commands.isEmpty)
        // A clipboard from a workspace with other cues, patches and numbers goes as it is.
        put(
            editor2,
            """
            {"version":1,"cues":[
              {"id":"x1","number":"1","name":"A","kind":{"type":"audio","patch":"other-ws-patch"}},
              {"id":"x2","number":"9","name":"B","kind":{"type":"fade","target":"not-here"}}]}
            """)
        let log3 = CueEditingTests.SendLog(other.rig.engine)
        await editor2.paste()
        let (_, two) = try pasted(log3)
        XCTAssertEqual(two.count, 2)
        XCTAssertEqual(two[0]["kind"]?["patch"]?.stringValue, "other-ws-patch")
        XCTAssertEqual(two[1]["kind"]?["target"]?.stringValue, "not-here")
    }

    func testPasteWithNothingValidOnThePasteboardDoesNothing() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro])
        await editor.paste()
        let cue = #"{"id":"x","kind":{"type":"memo"}}"#
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
        XCTAssertTrue(log.commands.isEmpty)
        put(editor, #"{"version":1,"cues":[\#(cue)]}"#)
        await editor.paste()
        XCTAssertEqual(log.commands.count, 1, "the good payload still pastes")
    }

    func testCutCopiesThenDeletesAndRefusesBusyCues() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.rain])  // paused
        await editor.cutSelection()
        XCTAssertTrue(log.commands.isEmpty)
        XCTAssertEqual(log.notices, [loc("edit.error.deleteBusy")])
        XCTAssertNil(editor.pasteboard.string(forType: CueEditor.pasteType), "a refused cut copies nothing")
        h.vc.select([H.door])
        await editor.cutSelection()
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "remove_cue")
        XCTAssertEqual(sent["id"]?.stringValue, H.door.raw)
        XCTAssertNotNil(editor.pasteboard.string(forType: CueEditor.pasteType))
        await assertEventually { h.vc.node(H.door) == nil }
        await editor.paste()
        XCTAssertEqual(log.commands.last?["cues"]?.arrayValue?.first?["name"]?.stringValue, "Door slam")
    }

    /// An editor whose cue list is released before this returns (its window is gone).
    private func orphanEditor(_ store: WorkspaceStore) -> CueEditor {
        CueEditor(store: store, cueList: CueListViewController(store: store))
    }

    /// The editor must not trap when its window is gone while an edit is queued (it held the cue list unowned).
    func testEditsAfterTheCueListIsGoneDoNothing() async throws {
        let rig = try Rig()
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let editor = orphanEditor(rig.store)
        let log = CueEditingTests.SendLog(rig.engine)
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
        XCTAssertTrue(log.commands.isEmpty)
    }

    // MARK: Delete numbers

    func testDeleteNumbersClearsTheSelectedCuesNumbers() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door, H.radio])
        await editor.deleteNumbers()
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "set_cue_numbers")
        XCTAssertEqual(sent["numbers"]?.arrayValue?.map { $0["id"]?.stringValue }, [H.door.raw, H.radio.raw])
        XCTAssertEqual(sent["numbers"]?.arrayValue?.map { $0["number"] }, [.null, .null])
        await assertEventually { h.vc.node(H.door)?.cue.number == nil }
        // Nothing left to clear: nothing is sent.
        await editor.deleteNumbers()
        XCTAssertEqual(log.commands.count, 1)
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
        let rig = try Rig()
        let connected = await rig.connect()
        XCTAssertTrue(connected)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.cueList.view.frame = NSRect(x: 0, y: 0, width: 800, height: 400)
        let first = try XCTUnwrap(wc.cueList.roots.first?.id)
        let e = wc.editor
        e.pasteboard = NSPasteboard.withUniqueName()
        wc.cueList.select([first])
        let log = CueEditingTests.SendLog(rig.engine)
        let files = try tempFiles(["a.wav"])
        wc.setShowMode(true)
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
        XCTAssertEqual(log.edits, 0, "nothing reached the engine")
        XCTAssertEqual(e.pasteboard.string(forType: CueEditor.pasteType), payload, "copy did not run")
    }
}
