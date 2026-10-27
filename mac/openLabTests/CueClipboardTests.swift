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
        XCTAssertEqual(adds.map { $0["cue"]?["name"]?.stringValue }, ["b thunder", "a rain"])
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

    func testReplaceFileSendsTheWholeKindAndSkipsBusyCues() async throws {
        let (h, editor, log) = try await harness()
        _ = h  // the window owns the cue list for as long as the editor lives
        let wav = try tempFiles(["new.wav"])[0]
        await editor.replaceFile(of: H.door, with: wav)
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "set_cue_property")
        XCTAssertEqual(sent["id"]?.stringValue, H.door.raw)
        XCTAssertEqual(sent["property"]?["type"]?.stringValue, "kind")
        XCTAssertEqual(sent["property"]?["value"]?["type"]?.stringValue, "audio")
        XCTAssertEqual(sent["property"]?["value"]?["media"]?.stringValue, wav.standardizedFileURL.path)
        // Rain is paused in the demo show.
        await editor.replaceFile(of: H.rain, with: wav)
        XCTAssertEqual(log.commands.count, 1)
        XCTAssertEqual(log.notices, [loc("edit.error.replaceBusy")])
    }

    // MARK: Copy, cut, paste

    func testFreshIDsRenameEveryCueAndFollowTargetsInsideTheCopy() throws {
        let cue = JSONValue.parse(
            """
            {"id":"g","kind":{"type":"group","children":[
              {"id":"a","kind":{"type":"audio"}},
              {"id":"f","kind":{"type":"fade","target":"a"}},
              {"id":"s","kind":{"type":"stop","target":"outside"}}]}}
            """)
        var n = 0
        let fresh = CueEditor.withFreshIDs(try XCTUnwrap(cue)) {
            n += 1
            return ModelID("n\(n)")
        }
        let kids = try XCTUnwrap(fresh["kind"]?["children"]?.arrayValue)
        XCTAssertEqual(fresh["id"]?.stringValue, "n1")
        XCTAssertEqual(kids.map { $0["id"]?.stringValue }, ["n2", "n3", "n4"])
        XCTAssertEqual(kids[1]["kind"]?["target"]?.stringValue, "n2")
        XCTAssertEqual(kids[2]["kind"]?["target"]?.stringValue, "outside")
    }

    func testCopyThenPasteAddsACopyWithAFreshIDAfterTheSelection() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.door])
        let copied = await editor.copySelection()
        XCTAssertTrue(copied)
        XCTAssertTrue(log.commands.isEmpty, "copy changes nothing")
        h.vc.select([H.rain])
        await editor.paste()
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "add_cue")
        XCTAssertEqual(sent["at"]?["parent"]?.stringValue, H.group.raw)
        XCTAssertEqual(sent["at"]?["index"], .int(2))
        XCTAssertEqual(sent["auto_number"], .bool(true))
        let id = try XCTUnwrap(sent["cue"]?["id"]?.stringValue)
        XCTAssertNotEqual(id, H.door.raw)
        XCTAssertEqual(sent["cue"]?["name"]?.stringValue, "Door slam")
        XCTAssertEqual(sent["cue"]?["kind"]?["media"]?.stringValue, "media/d.wav")
        await assertEventually { h.vc.selectedIDs() == [ModelID(id)] }
        XCTAssertEqual(h.vc.node(H.group)?.children.count, 4)
    }

    func testCopyOfGroupAndCueInsideItCopiesOnlyTheGroupWithItsChildren() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.group, H.door])
        await editor.copySelection()
        await editor.paste()
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "add_cue", "one top-level cue: a plain add")
        let kids = try XCTUnwrap(sent["cue"]?["kind"]?["children"]?.arrayValue)
        XCTAssertEqual(kids.count, 3)
        let old = Set([H.group, H.door, H.rain, H.radio].map(\.raw))
        let all = ([sent["cue"]?["id"]?.stringValue] + kids.map { $0["id"]?.stringValue }).compactMap { $0 }
        XCTAssertTrue(old.isDisjoint(with: all))
        XCTAssertEqual(Set(all).count, 4)
    }

    func testSeveralCopiedCuesPasteAsOneTransactionInOrder() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro, H.fade])
        await editor.copySelection()
        h.vc.select([H.intro])
        await editor.paste()
        let sent = try XCTUnwrap(log.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "transaction")
        let adds = try XCTUnwrap(sent["commands"]?.arrayValue)
        XCTAssertEqual(adds.map { $0["at"]?["index"] }, [.int(1), .int(2)])
        XCTAssertEqual(
            adds.map { $0["cue"]?["name"]?.stringValue },
            [H.intro, H.fade].compactMap { h.vc.node($0)?.cue.name })
    }

    func testPasteIntoASecondWorkspace() async throws {
        let (h, editor, _) = try await harness()
        let other = try await H(collapsed: false)
        let editor2 = CueEditor(store: other.store, cueList: other.vc)
        editor2.pasteboard = editor.pasteboard  // the pasteboard is what crosses
        let log2 = CueEditingTests.SendLog(other.rig.engine)
        h.vc.select([H.door])
        await editor.copySelection()
        other.vc.select([H.intro])
        await editor2.paste()
        let sent = try XCTUnwrap(log2.commands.first)
        XCTAssertEqual(sent["type"]?.stringValue, "add_cue")
        XCTAssertEqual(sent["at"]?["list"]?.stringValue, H.list)
        XCTAssertEqual(sent["at"]?["index"], .int(1))
        XCTAssertEqual(sent["cue"]?["name"]?.stringValue, "Door slam")
        XCTAssertNotEqual(sent["cue"]?["id"]?.stringValue, H.door.raw)
    }

    func testPasteWithNothingValidOnThePasteboardDoesNothing() async throws {
        let (h, editor, log) = try await harness()
        h.vc.select([H.intro])
        XCTAssertFalse(editor.canPaste)
        await editor.paste()
        XCTAssertTrue(log.commands.isEmpty)
        editor.pasteboard.setString("not json", forType: CueEditor.pasteType)
        await editor.paste()
        XCTAssertTrue(log.commands.isEmpty)
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
        XCTAssertEqual(log.commands.last?["cue"]?["name"]?.stringValue, "Door slam")
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
        XCTAssertFalse(e.canPaste)
        let copied = await e.copySelection()
        XCTAssertFalse(copied)
        await e.cutSelection()
        await e.paste()
        await e.deleteNumbers()
        await e.addFiles(files, parent: nil, index: 0)
        await e.replaceFile(of: first, with: files[0])
        XCTAssertNil(wc.cueList.fileDropPlan(files, item: nil, index: 0))
        for _ in 0..<50 { await Task.yield() }
        XCTAssertEqual(log.edits, 0, "nothing reached the engine")
        XCTAssertEqual(e.pasteboard.string(forType: CueEditor.pasteType), payload, "copy did not run")
    }
}
