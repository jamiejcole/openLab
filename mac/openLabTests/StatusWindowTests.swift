// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// DX-01, DX-02: the workspace status window against the real engine: one window for the live
/// warnings, the show log and the workspace's info, opened by its shortcut or by a click on the
/// footer's warnings button; the log is one of its pages, there is no separate command for it.
@MainActor
final class StatusWindowTests: XCTestCase {
    private let radio = TestSnapshots.radio

    /// The demo show on the real engine (its Radio cue's file is missing), its window shown.
    private func open() async throws -> (Rig, WorkspaceWindowController) {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.showWindow(nil)
        addTeardownBlock { @MainActor in wc.window?.close() }
        await assertEventually("the footer counts the missing file") { !wc.footer.warningsButton.isHidden }
        return (rig, wc)
    }

    /// Flow: Window menu → no separate log command; the log is reached from the status window's tabs.
    func testTheWindowMenuHasNoLogCommandAndLogsIsATabOfTheStatusWindow() async throws {
        let (_, wc) = try await open()
        let window = MainMenu.make().windows
        XCTAssertFalse(window.items.contains { $0.title == "Show Log" || $0.title == loc("log.title") })
        XCTAssertFalse(window.items.contains { $0.action == NSSelectorFromString("showLog:") })
        await wc.showWorkspaceStatus().value
        let status = try XCTUnwrap(wc.existingStatusWindow)
        XCTAssertTrue(status.tabs.tabViewItems.contains { ($0.identifier as? String) == "logs" })
        status.tabs.selectTabViewItem(withIdentifier: StatusWindowController.Page.logs.rawValue)
        XCTAssertEqual(status.page, .logs)
        XCTAssertTrue(status.logs.table.window != nil || status.logs.table.superview != nil, "the page holds the log")
    }

    /// Flow: the status shortcut, and a click on the footer's warnings button → one status window
    /// with warnings, the log and workspace info.
    func testTheStatusShortcutAndTheFootersWarningMarkOpenOneWindowWithWarningsLogsAndInfo() async throws {
        let (rig, wc) = try await open()
        // The shortcut is Window's Workspace Status item on Shift-Command-W.
        let item = try XCTUnwrap(
            MainMenu.make().windows.items.first { $0.action == #selector(AppDelegate.showWorkspaceStatus(_:)) })
        XCTAssertEqual(item.title, loc("menu.window.status"))
        XCTAssertEqual(item.keyEquivalent, "W")
        XCTAssertNil(wc.existingStatusWindow)

        await wc.showWorkspaceStatus().value
        let status = try XCTUnwrap(wc.existingStatusWindow)
        XCTAssertTrue(status.window?.isVisible == true)
        XCTAssertEqual(status.page, .warnings)
        XCTAssertEqual(
            status.tabs.tabViewItems.map(\.label), StatusWindowController.Page.allCases.map { loc($0.titleKey) })
        XCTAssertTrue(status.warnings.rows.contains { $0.cue == radio }, "the missing file's cue is listed")

        // The footer's warnings button opens the same window on Warnings; Logs is a tab of it.
        status.tabs.selectTabViewItem(withIdentifier: StatusWindowController.Page.info.rawValue)
        wc.footer.warningsButton.performClick(nil)
        XCTAssertTrue(wc.existingStatusWindow === status, "one window")
        XCTAssertEqual(status.page, .warnings)
        await status.present(.logs).value
        XCTAssertTrue(wc.existingStatusWindow === status, "still one window")
        XCTAssertEqual(status.page, .logs)
        XCTAssertTrue(status.logs.isOpen(), "the log page counts as open")
        XCTAssertEqual(status.window?.title, String(format: loc("status.title.workspace"), wc.workspaceName))

        // The Info page names the workspace and its id as OSC writes it, and copies the id.
        status.tabs.selectTabViewItem(withIdentifier: StatusWindowController.Page.info.rawValue)
        let id = try XCTUnwrap(rig.store.model?.id.raw.uppercased())
        XCTAssertEqual(status.idValue.stringValue, id)
        XCTAssertEqual(status.nameValue.stringValue, rig.store.model?.name)
        XCTAssertEqual(status.fileValue.stringValue, rig.store.document?.path ?? loc("status.info.notSaved"))
        let board = NSPasteboard(name: NSPasteboard.Name("openlab-status-\(UUID().uuidString)"))
        status.pasteboard = board
        status.copyIDButton.performClick(nil)
        XCTAssertEqual(board.string(forType: .string), id)

        // It never takes the keyboard (GO keeps reaching the workspace window) and goes with it.
        XCTAssertEqual((status.window as? NSPanel)?.canBecomeKey, false)
        wc.window?.close()
        XCTAssertFalse(status.window?.isVisible == true)
    }

    /// Checklist: a warning's row selects its cue (and leaves the playhead alone).
    func testAWarningsRowSelectsItsCue() async throws {
        let (rig, wc) = try await open()
        let playheads = rig.store.playheads
        await wc.showWorkspaceStatus().value
        let status = try XCTUnwrap(wc.existingStatusWindow)
        let row = try XCTUnwrap(status.warnings.rows.firstIndex { $0.cue == radio })
        XCTAssertTrue(status.warnings.choose(row: row))
        await assertEventually("the cue is selected") { wc.cueList.selectedIDs() == [self.radio] }
        XCTAssertEqual(rig.store.playheads, playheads, "a click in a list of warnings never moves GO")
    }
}
