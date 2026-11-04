// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The footer's warnings button, on a real engine: it shows only while the workspace has rows for
/// the Warnings tab, counts them, and opens the sidebar on that tab. The Demo show starts with one
/// broken cue (Radio's file is missing); Second has none.
@MainActor
final class FooterWarningsTests: XCTestCase {
    private let door = TestSnapshots.door
    private let radio = TestSnapshots.radio

    private func open(_ show: String = "Demo") async throws -> (Rig, WorkspaceWindowController) {
        let rig = try await Rig.open(show)
        let wc = WorkspaceWindowController(store: rig.store)
        wc.window?.setContentSize(NSSize(width: 800, height: 500))
        wc.window?.contentView?.layoutSubtreeIfNeeded()
        return (rig, wc)
    }

    private func rowCount(_ rig: Rig) -> Int { WarningRows.make(store: rig.store).count }

    /// Points the cue at `file` and waits until the engine's answer changed the Warnings rows to `rows`.
    private func setMedia(_ cue: ModelID, _ file: String, in rig: Rig, rows: Int) async {
        guard case .success = await rig.store.send(command: .setProperty(cue, .media(file))) else {
            return XCTFail("media")
        }
        // The engine probes the file in the background; advance(0) waits for that work.
        await assertEventually("the rows are \(rows)") {
            try? await rig.advance()
            return self.rowCount(rig) == rows
        }
    }

    private func shown(_ wc: WorkspaceWindowController) -> Bool {
        !wc.footer.warningsButton.isHiddenOrHasHiddenAncestor
    }

    private func title(_ count: Int) -> String {
        String(format: loc(count == 1 ? "warnings.one" : "warnings.many"), count)
    }

    /// In the window, with a frame inside the footer that no other visible control of it overlaps.
    private func assertOnScreen(_ wc: WorkspaceWindowController) {
        let footer = wc.footer
        let button = footer.warningsButton
        wc.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertNotNil(button.window)
        XCTAssertTrue(shown(wc))
        let frame = footer.convert(button.bounds, from: button)
        XCTAssertGreaterThan(frame.width, 10)
        XCTAssertGreaterThan(frame.height, 5)
        XCTAssertTrue(footer.bounds.contains(frame), "\(frame) in \(footer.bounds)")
        let others: [NSView] =
            [
                footer.modeSwitch, footer.showModeLabel, footer.overrideBadge, footer.statusButton,
                footer.noticeLabel, footer.countLabel, footer.clock, footer.settingsButton,
            ] + Array(footer.toggles.values)
        for other in others where !other.isHiddenOrHasHiddenAncestor {
            let r = footer.convert(other.bounds, from: other)
            XCTAssertFalse(r.intersects(frame), "\(other) overlaps the warnings button")
        }
    }

    func testAWorkspaceWithNoWarningsShowsNoWarningsButton() async throws {
        let (rig, wc) = try await open("Second")
        XCTAssertEqual(rowCount(rig), 0)
        XCTAssertFalse(shown(wc))
    }

    func testABrokenCueShowsTheButtonWithTheCountAndClickingItOpensTheWarningsTab() async throws {
        let (rig, wc) = try await open()
        XCTAssertEqual(rowCount(rig), 1)
        XCTAssertFalse(wc.isVisible(.sidebar))
        XCTAssertTrue(shown(wc))
        XCTAssertEqual(wc.footer.warningsButton.title, title(1))
        assertOnScreen(wc)

        let before = wc.window?.firstResponder
        wc.footer.warningsButton.performClick(nil)
        XCTAssertTrue(wc.isVisible(.sidebar))
        XCTAssertEqual(wc.sidebar.tabs.selectedSegment, 2)
        XCTAssertFalse(wc.sidebar.warningsTable.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(wc.window?.firstResponder === before, "the click leaves the keys where they were")
        XCTAssertTrue(wc.footer.warningsButton.refusesFirstResponder)
    }

    func testTheCountFollowsWarningsComingAndGoingInEditAndShowMode() async throws {
        let (rig, wc) = try await open()
        await setMedia(door, "media/gone.wav", in: rig, rows: 2)
        await assertEventually("two") { wc.footer.warningsButton.title == self.title(2) }
        assertOnScreen(wc)

        // Show Mode: the footer stays, the button follows, and it still opens the tab.
        await wc.requestShowMode(true)
        await assertEventually { wc.isShowMode }
        XCTAssertTrue(shown(wc))
        await setMedia(door, "media/d.wav", in: rig, rows: 1)
        await assertEventually("one") { wc.footer.warningsButton.title == self.title(1) }
        assertOnScreen(wc)
        wc.setVisible(.sidebar, false)
        wc.footer.warningsButton.performClick(nil)
        XCTAssertTrue(wc.isVisible(.sidebar))
        XCTAssertEqual(wc.sidebar.tabs.selectedSegment, 2)

        await setMedia(radio, "media/d.wav", in: rig, rows: 0)
        await assertEventually("gone") { !self.shown(wc) }
        await wc.requestShowMode(false)
        await assertEventually { !wc.isShowMode }
        XCTAssertFalse(shown(wc))
    }

    func testTheButtonDoesNotOverlapTheBadgeOrOtherControlsAtTheMinimumWidth() async throws {
        let (rig, wc) = try await open()
        await setMedia(door, "media/gone.wav", in: rig, rows: 2)
        await wc.requestShowMode(true)
        await assertEventually { wc.isShowMode }
        wc.footer.overrideBadge.isHidden = false  // the widest footer: Show Mode and the override badge
        wc.window?.setContentSize(wc.window?.minSize ?? NSSize(width: 800, height: 500))
        assertOnScreen(wc)
    }
}
