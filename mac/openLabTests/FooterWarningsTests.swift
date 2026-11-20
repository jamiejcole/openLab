// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The footer's warnings button, on a real engine: it shows only while the workspace has rows for
/// the Warnings list, counts them, and opens the status window on its Warnings page. The Demo show starts with one
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
                footer.modeSwitch, footer.alarmButton,
                footer.noticeLabel, footer.countLabel, footer.settingsButton,
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
        XCTAssertEqual(wc.footer.warningsButton.title, "1")
        XCTAssertEqual(wc.footer.warningsButton.accessibilityLabel(), title(1))
        assertOnScreen(wc)

        let before = wc.window?.firstResponder
        wc.footer.warningsButton.performClick(nil)
        let status = try XCTUnwrap(wc.existingStatusWindow, "the click opens the status window")
        XCTAssertTrue(status.window?.isVisible == true)
        XCTAssertEqual(status.page, .warnings)
        XCTAssertTrue(wc.window?.firstResponder === before, "the click leaves the keys where they were")
        XCTAssertTrue(wc.footer.warningsButton.refusesFirstResponder)
    }

    func testTheCountFollowsWarningsComingAndGoingInEditAndShowMode() async throws {
        let (rig, wc) = try await open()
        await setMedia(door, "media/gone.wav", in: rig, rows: 2)
        await assertEventually("two") { wc.footer.warningsButton.title == "2" }
        assertOnScreen(wc)

        // Show Mode: the footer stays, the button follows, and it still opens the tab.
        await wc.requestShowMode(true)
        await assertEventually { wc.isShowMode }
        XCTAssertTrue(shown(wc))
        await setMedia(door, "media/d.wav", in: rig, rows: 1)
        await assertEventually("one") { wc.footer.warningsButton.title == "1" }
        assertOnScreen(wc)
        wc.existingStatusWindow?.close()
        wc.footer.warningsButton.performClick(nil)
        XCTAssertTrue(wc.existingStatusWindow?.window?.isVisible == true)
        XCTAssertEqual(wc.existingStatusWindow?.page, .warnings)

        await setMedia(radio, "media/d.wav", in: rig, rows: 0)
        await assertEventually("gone") { !self.shown(wc) }
        await wc.requestShowMode(false)
        await assertEventually { !wc.isShowMode }
        XCTAssertFalse(shown(wc))
    }

    /// Shows the window at its minimum size and returns whether it kept that size.
    private func atMinimum(_ wc: WorkspaceWindowController) throws -> NSSize {
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(origin: window.frame.origin, size: window.minSize), display: true)
        window.makeKeyAndOrderFront(nil)
        window.layoutIfNeeded()
        window.displayIfNeeded()
        window.layoutIfNeeded()
        return window.frame.size
    }

    func testTheButtonDoesNotOverlapTheBadgeOrOtherControlsOrWidenTheWindowAtTheMinimumWidth() async throws {
        let (_, wc) = try await open()
        // The window's size at its minimum, with the button shown and without it: showing it
        // must not widen the window.
        func compare(_ label: String, widens: Bool = false) throws {
            wc.footer.showWarnings(count: 0)
            let without = try atMinimum(wc)
            wc.footer.showWarnings(count: 12)
            let with = try atMinimum(wc)
            if !widens { XCTAssertEqual(with, without, "showing the button widens the window: \(label)") }
            assertOnScreen(wc)
        }
        try compare("Edit Mode")
        await wc.requestShowMode(true)
        await assertEventually { wc.isShowMode }
        try compare("Show Mode")
        // The widest footer: Show Mode with a device alarm in the centre.
        wc.footer.show(status: try alarmStatus(), connected: true)
        try compare("Show Mode with the device alarm", widens: true)
    }

    /// A workspace warning (a `warnings` message) and a runtime warning each reach the footer.
    func testWorkspaceAndRuntimeWarningsReachTheFooterCount() async throws {
        let (rig, wc) = try await open("Second")
        XCTAssertFalse(shown(wc))
        let late = #"{"type":"late_start","cue":"\#(door)","late_ms":120}"#
        let message = #"{"type":"warnings","warnings":[\#(late)]}"#
        XCTAssertEqual(rig.store.receive(try ServerMessage.decode(message)), .applied)
        XCTAssertEqual(rowCount(rig), 1)
        XCTAssertTrue(shown(wc))
        XCTAssertEqual(wc.footer.warningsButton.title, "1")
        XCTAssertEqual(wc.footer.warningsButton.accessibilityLabel(), title(1))

        let seq = rig.store.runtimeSeq + 1
        let runtime = #"{"type":"runtime","seq":\#(seq),"changes":[{"type":"warning","warning":\#(late)}]}"#
        XCTAssertEqual(rig.store.receive(try ServerMessage.decode(runtime)), .applied)
        XCTAssertEqual(rowCount(rig), 2)
        XCTAssertEqual(wc.footer.warningsButton.title, "2")
    }

    // MARK: The footer without the removed parts

    /// Flow: workspace open, all well → the footer shows the count, the Edit/Show switch and the toggles;
    /// no engine status mark, no words about CPU, dropouts, load, disk, block size or latency.
    func testAllWellTheFooterHasTheCountTheSwitchAndTheTogglesAndNoEngineReadouts() async throws {
        let (rig, wc) = try await open("Second")
        await assertEventually("the engine's status arrives") {
            try? await rig.advance(1)
            return rig.store.status != nil
        }
        let footer = wc.footer
        XCTAssertEqual(footer.countLabel.stringValue, "6 cues in 1 list")
        XCTAssertFalse(footer.modeSwitch.isHidden)
        XCTAssertEqual(footer.toggles.count, 3)
        XCTAssertTrue(footer.alarmButton.isHidden)
        XCTAssertTrue(footer.warningsButton.isHidden, "no warnings: no button")
        func words(_ v: NSView) -> [String] {
            var out: [String] = []
            if let b = v as? NSButton { out += [b.title, b.toolTip ?? "", b.accessibilityLabel() ?? ""] }
            if let t = v as? NSTextField { out += [t.stringValue, t.toolTip ?? ""] }
            return out + v.subviews.flatMap(words)
        }
        let all = words(footer).joined(separator: " ").lowercased()
        for banned in ["cpu", "dropout", "load", "disk", "block", "latency", "audio ok", "engine status"] {
            XCTAssertFalse(all.contains(banned), "\(banned) in the footer: \(all)")
        }
        // No popover opens from anywhere in the footer.
        for v in footer.subviews.flatMap({ [$0] + $0.subviews }).compactMap({ $0 as? NSButton }) where !v.isHidden {
            XCTAssertFalse(v.target is NSPopoverDelegate)
        }
        XCTAssertNil(footer.subviews.flatMap(\.subviews).first { $0.accessibilityIdentifier() == "status-label" })
    }

    /// Flow: narrow window, light and dark → the footer and the sidebar lay out whole without the removed
    /// parts: nothing overlaps or leaves its area, and the sidebar's tabs, lists and buttons are one
    /// column with the same spacing between them (no gap where the Warnings tab and its count were).
    func testTheFooterAndSidebarLayOutCleanlyAtTheLeastWidthLightAndDark() async throws {
        let (_, wc) = try await open("Demo")
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            wc.window?.appearance = NSAppearance(named: name)
            wc.setVisible(.sidebar, true)
            _ = try atMinimum(wc)
            let footer = wc.footer
            wc.window?.contentView?.layoutSubtreeIfNeeded()
            let visible =
                ([footer.modeSwitch, footer.countLabel, footer.warningsButton, footer.settingsButton]
                + Array(footer.toggles.values)).filter { !$0.isHiddenOrHasHiddenAncestor }
            let frames = visible.map { footer.convert($0.bounds, from: $0) }
            for (i, f) in frames.enumerated() {
                XCTAssertTrue(footer.bounds.contains(f), "\(visible[i]) at \(name.rawValue): \(f) in \(footer.bounds)")
                for j in frames.indices where j > i {
                    XCTAssertFalse(f.intersects(frames[j]), "\(visible[i]) overlaps \(visible[j]) at \(name.rawValue)")
                }
            }
            let side = wc.sidebar
            XCTAssertEqual(side.tabs.segmentCount, 2, "two tabs only")
            let column = try XCTUnwrap(side.view.subviews.compactMap { $0 as? NSStackView }.first)
            let shown = column.arrangedSubviews.filter { !$0.isHidden }.sorted { $0.frame.minY > $1.frame.minY }
            XCTAssertEqual(shown.count, 4, "transport strip, tabs, one list, New List: \(shown)")
            for pair in zip(shown, shown.dropFirst()) {
                XCTAssertEqual(
                    pair.0.frame.minY - pair.1.frame.maxY, column.spacing, accuracy: 0.5, "no gap in the sidebar")
            }
            XCTAssertEqual(side.tabs.frame.width, side.view.bounds.width - 20, accuracy: 0.5)
        }
    }

    // MARK: The device alarm

    private func alarmStatus() throws -> SystemStatus {
        guard case .status(let s) = try ServerMessage.decode(try Fixtures.text("server", "status_device_lost"))
        else { throw XCTSkip("not a status") }
        return s
    }

    private func warningTitles(_ rig: Rig) -> [String] { WarningRows.make(store: rig.store).map(\.title) }

    /// Two devices, a patch on the second: it is unplugged → the footer's alarm names it at once and
    /// Status > Warnings lists it; plugged back in → the alarm and the row are gone.
    func testASecondDeviceThatIsLostIsAnAlarmAndARowInStatusWarnings() async throws {
        let engine = try await TestEngine.start()
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        let second = TestEngine.deviceNames[1]
        let lostSecond =
            loc("footer.alarm") + " " + String(format: loc("footer.alarm.outputLost"), second)
        guard case .success = await engine.request(.restoreDevice(device: TestEngine.deviceIDs[1])) else {
            return XCTFail("plug in")
        }
        try await engine.advance(seconds: 2)
        let patch = try XCTUnwrap(rig.store.model?.patches.first?.id)
        guard case .success = await rig.store.setPatchMapping(patch, PatchMapping(device: second, channels: [0, 1]))
        else { return XCTFail("map") }
        try await rig.advance(1)
        await assertEventually("a status came with the second device up") {
            try? await rig.advance(1)
            return rig.store.status?.outputs.contains { $0.name == second && $0.state == .up } == true
        }
        XCTAssertTrue(wc.footer.alarmButton.isHidden, "all well: no alarm, no engine mark")

        guard case .success = await engine.request(.loseDevice(device: TestEngine.deviceIDs[1])) else {
            return XCTFail("unplug")
        }
        await assertEventually("the footer names the lost device") {
            try? await rig.advance(1)
            return wc.footer.alarmButton.title == lostSecond
        }
        XCTAssertFalse(wc.footer.alarmButton.isHidden, "a lost device's alarm is always in view")
        let row = String(format: loc("warnings.device"), second)
        await assertEventually("Status > Warnings lists the device") {
            try? await rig.advance(1)
            return self.warningTitles(rig).contains(row)
        }
        // The alarm opens the Warnings page.
        wc.footer.alarmButton.performClick(nil)
        XCTAssertEqual(wc.existingStatusWindow?.page, .warnings)
        wc.existingStatusWindow?.close()

        guard case .success = await engine.request(.restoreDevice(device: TestEngine.deviceIDs[1])) else {
            return XCTFail("plug back in")
        }
        await assertEventually("the alarm clears") {
            try? await rig.advance(1)
            return wc.footer.alarmButton.isHidden
        }
        await assertEventually("the row goes") {
            try? await rig.advance(1)
            return !self.warningTitles(rig).contains(row)
        }
    }

    /// Only the first device, unplugged: the alarm names the audio device, and the fault is a row.
    func testTheFirstDeviceUnpluggedIsAnAlarmAndARowInStatusWarnings() async throws {
        let engine = try await TestEngine.start()
        let rig = try await Rig.open("Demo", on: engine)
        let wc = WorkspaceWindowController(store: rig.store)
        guard case .success = await engine.request(.loseDevice()) else { return XCTFail("unplug") }
        await assertEventually("the footer says the device is lost") {
            try? await rig.advance(1)
            return wc.footer.alarmButton.title == loc("footer.alarm") + " " + loc("footer.alarm.deviceLost")
        }
        XCTAssertFalse(wc.footer.alarmButton.isHidden, "the primary device's alarm is always in view")
        XCTAssertTrue(
            WarningRows.make(store: rig.store).contains { $0.title.hasPrefix(loc("warnings.device").prefix(5)) },
            "\(warningTitles(rig))")
    }
}
