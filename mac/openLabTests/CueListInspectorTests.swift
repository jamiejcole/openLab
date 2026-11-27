// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// A cue list chosen in the sidebar's Lists tab shows its own inspector (Basics and Triggers; QLab 5's
/// cue list has a Timecode tab too, not built). The demo show plus a second list, no engine.
@MainActor
final class CueListInspectorTests: XCTestCase {
    private static let main: ModelID = "00000000-0000-0000-0000-000000000001"
    private static let second: ModelID = "00000000-0000-0000-0000-000000000002"
    private var rig: Rig!

    private func make() throws -> WorkspaceWindowController {
        var snap = try TestSnapshots.show()
        var lists = snap["workspace"]?["cue_lists"]?.arrayValue ?? []
        lists.append(.object(["id": .string(Self.second.raw), "name": .string("Second"), "cues": .array([])]))
        snap["workspace"]?.set("cue_lists", .array(lists))
        rig = try Rig.offline(snap)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.inspector.setSelection([TestSnapshots.intro])
        return wc
    }

    private func pick(_ wc: WorkspaceWindowController, row: Int) {
        wc.sidebar.listTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
    }

    private func tabs(_ wc: WorkspaceWindowController) -> [String] {
        wc.inspector.cueListView.tabBar.buttons.map(\.tabID)
    }

    /// Lists tab, click a cue list: the inspector shows that list's name on Basics, and Basics and
    /// Triggers, no Timecode, no cue's tabs.
    func testChoosingACueListInTheSidebarShowsItsBasicsAndTriggersTabs() throws {
        let wc = try make()
        let inspector = wc.inspector
        XCTAssertTrue(inspector.cueListView.isHidden, "a cue is shown first")
        XCTAssertFalse(inspector.tabView.isHidden)
        pick(wc, row: 1)
        XCTAssertFalse(inspector.cueListView.isHidden)
        XCTAssertTrue(inspector.tabView.isHidden)
        XCTAssertTrue(inspector.tabBar.isHidden)
        XCTAssertEqual(tabs(wc), ["basics", "triggers"])
        XCTAssertEqual(inspector.cueListView.nameField.stringValue, "Second")
        XCTAssertTrue(inspector.cueListView.nameField.isEnabled)
        pick(wc, row: 0)
        XCTAssertEqual(inspector.cueListView.nameField.stringValue, "Main")
    }

    /// The tabs switch: Triggers shows its pane, Basics comes back with the name.
    func testTheCueListInspectorSwitchesBetweenItsTabs() throws {
        let wc = try make()
        pick(wc, row: 1)
        let view = wc.inspector.cueListView
        let triggers = try XCTUnwrap(view.tabBar.buttons.first { $0.tabID == "triggers" })
        triggers.performClick(nil)
        XCTAssertEqual(view.shownTab, "triggers")
        XCTAssertTrue(view.triggersNote.superview?.isHidden == false)
        let basics = try XCTUnwrap(view.tabBar.buttons.first { $0.tabID == "basics" })
        basics.performClick(nil)
        XCTAssertEqual(view.shownTab, "basics")
        XCTAssertEqual(view.nameField.stringValue, "Second")
    }

    /// Back to cues: a press in the cue list (also on the row already selected) or a cue chosen under a
    /// list in the sidebar shows the cue's tabs again.
    func testAPressInTheCueListShowsCuesAgain() throws {
        let wc = try make()
        pick(wc, row: 1)
        XCTAssertFalse(wc.inspector.cueListView.isHidden)
        let window = try XCTUnwrap(wc.window as? WorkspaceWindow)
        let content = try XCTUnwrap(window.contentView)
        content.layoutSubtreeIfNeeded()
        let inList = wc.cueList.view.convert(
            NSPoint(x: wc.cueList.view.bounds.midX, y: wc.cueList.view.bounds.midY), to: nil)
        let press = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: inList, modifierFlags: [], timestamp: 0,
                windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        window.onMouseDown?(press)
        XCTAssertTrue(wc.inspector.cueListView.isHidden)
    }

    func testAPressInTheSidebarKeepsTheCueListInspector() throws {
        let wc = try make()
        pick(wc, row: 1)
        let window = try XCTUnwrap(wc.window as? WorkspaceWindow)
        let content = try XCTUnwrap(window.contentView)
        content.layoutSubtreeIfNeeded()
        let inSidebar = wc.sidebar.view.convert(
            NSPoint(x: wc.sidebar.view.bounds.midX, y: wc.sidebar.view.bounds.midY), to: nil)
        let press = try XCTUnwrap(
            NSEvent.mouseEvent(
                with: .leftMouseDown, location: inSidebar, modifierFlags: [], timestamp: 0,
                windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        window.onMouseDown?(press)
        XCTAssertFalse(wc.inspector.cueListView.isHidden)
    }

    func testChoosingACueUnderAListShowsTheCueNotTheList() throws {
        let wc = try make()
        pick(wc, row: 1)
        wc.sidebar.onSelectCue?(TestSnapshots.intro)
        XCTAssertTrue(wc.inspector.cueListView.isHidden)
    }

    /// A rename made elsewhere (another window, undo) shows in the open cue list inspector; the list
    /// being deleted leaves it.
    func testTheCueListInspectorFollowsRenamesAndDeletion() throws {
        let wc = try make()
        pick(wc, row: 1)
        let revision = rig.store.revision + 1
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(revision),"upserts":[{"type":"cue_list","id":"\#(Self.second)","name":"Elsewhere","index":1}],"deletes":[]}"#
                )), .applied)
        XCTAssertEqual(wc.inspector.cueListView.nameField.stringValue, "Elsewhere")
        XCTAssertEqual(
            rig.store.receive(
                try ServerMessage.decode(
                    #"{"type":"model_changed","revision":\#(revision + 1),"upserts":[],"deletes":[{"type":"cue_list","id":"\#(Self.second)"}]}"#
                )), .applied)
        XCTAssertTrue(wc.inspector.cueListView.isHidden, "a list that is gone leaves the cue list inspector")
    }

    /// Show Mode makes the name read-only; a name typed with no engine is refused, said, and put back.
    func testTheListNameIsReadOnlyInShowModeAndARefusedNameGoesBack() async throws {
        let wc = try make()
        pick(wc, row: 1)
        let view = wc.inspector.cueListView
        view.nameField.stringValue = "Typed"
        view.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: view.nameField))
        await assertEventually { wc.inspector.messageLabel.stringValue != "" }
        XCTAssertEqual(view.nameField.stringValue, "Second", "the model's name is put back")
        wc.setShowMode(true)
        XCTAssertFalse(view.nameField.isEnabled)
    }
}

/// The cue list's name edited in the inspector, on the real engine: sent as one edit, undone, redone.
@MainActor
final class CueListInspectorEngineTests: XCTestCase {
    func testRenamingTheCueListInTheInspectorIsOneUndoableEdit() async throws {
        let wired = try await Rig.open()
        defer { wired.client.stop() }
        let wc = WorkspaceWindowController(store: wired.store)
        let list = try XCTUnwrap(wired.store.model?.cueLists.first).id
        let before = try XCTUnwrap(wired.store.model?.cueLists.first).name
        // The one list is already the highlighted row, so a click on it changes no selection.
        wc.sidebar.onChooseList?(list)
        let view = wc.inspector.cueListView
        XCTAssertEqual(view.nameField.stringValue, before)
        view.nameField.stringValue = "Front of House"
        view.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: view.nameField))
        await assertEventually { wired.store.model?.cueLists.first?.name == "Front of House" }
        XCTAssertEqual(view.nameField.stringValue, "Front of House")
        XCTAssertEqual(wired.wire.commands.filter { $0.json["type"]?.stringValue == "rename_cue_list" }.count, 1)
        await wc.editor.undo()
        await assertEventually { wired.store.model?.cueLists.first?.name == before }
        XCTAssertEqual(view.nameField.stringValue, before, "the inspector follows the undo")
        XCTAssertEqual(wired.store.model?.cueLists.first?.id, list)
        await wc.editor.redo()
        await assertEventually { wired.store.model?.cueLists.first?.name == "Front of House" }
    }
}
