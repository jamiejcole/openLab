// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
import XCTest

@testable import openLab

/// PB-10: the output override panel, its footer badge and details line, the red mark on Network
/// cues, and what the real engine sends while the override is on. Each flow is one test, run on a
/// real engine through the app's own controller.
@MainActor
final class OutputOverrideTests: XCTestCase {
    private var app: AppController!
    private var real: TestEngine!
    private var keyDoc: ModelID?
    private var alerts: ScriptedAlerts!
    private var listener: UDPListener!

    private let net: ModelID = "00000000-0000-0000-0000-0000000000a1"
    private let intro: ModelID = "00000000-0000-0000-0000-00000000000a"

    override func setUp() async throws {
        real = try await TestEngine.start()
        listener = try UDPListener()
        app = try makeApp(on: real)
    }

    override func tearDown() async throws {
        app?.closeAllWindows()
        app?.session.stop()
        real?.stop()
        app = nil
    }

    private func makeApp(on engine: TestEngine) throws -> AppController {
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        let controller = AppController(session: session, scheduler: ManualScheduler())
        alerts = ScriptedAlerts()
        controller.alerts = alerts
        controller.keyWindow = { [unowned self] in keyDoc.flatMap { app.windows[$0]?.window } }
        session.start()
        return controller
    }

    // MARK: Helpers

    /// Opens a copy of `template` with a Network cue at the top of its list (a one-line OSC message
    /// to the listener, continuing on its own into the cue below it); its window.
    private func openNetworkShow(_ template: String) async throws -> WorkspaceWindowController {
        let port = Int64(listener.port)
        let cue = try XCTUnwrap(JSONValue.parse(Self.networkCue(id: net)))
        let doc = try await real.open(template) { dir in
            let url = dir.appendingPathComponent("\(template).openlab")
            var file = try XCTUnwrap(JSONValue.parse(try String(contentsOf: url, encoding: .utf8)))
            var workspace = try XCTUnwrap(file["workspace"])
            var patches = try XCTUnwrap(workspace["network_patches"]?.arrayValue)
            patches[0].set("port", .int(port))
            workspace.set("network_patches", .array(patches))
            var lists = try XCTUnwrap(workspace["cue_lists"]?.arrayValue)
            var cues = try XCTUnwrap(lists[0]["cues"]?.arrayValue)
            cues.insert(cue, at: 0)
            lists[0].set("cues", .array(cues))
            workspace.set("cue_lists", .array(lists))
            file.set("workspace", workspace)
            try file.text().write(to: url, atomically: true, encoding: .utf8)
        }
        await assertEventually {
            self.app.windows[doc] != nil && self.app.session.stores[doc]?.model?.cue(self.net) != nil
        }
        return try XCTUnwrap(app.windows[doc])
    }

    private static func networkCue(id: ModelID) -> String {
        """
        {"id":"\(id)","number":"N","name":"Desk message","notes":"","color":"none","flagged":false,"armed":true,
         "pre_wait":0.0,"post_wait":0.0,"continue_mode":"auto_follow",
         "triggers":{"hotkey":null,"osc":null},"second_trigger":"nothing","second_trigger_on_release":false,
         "kind":{"type":"network","destination":"00000000-0000-0000-0000-000000000001","address":"/test",
                 "args":[{"type":"string","value":"hello"}]}}
        """
    }

    /// Runs the engine's device clock on until its next status has come.
    private func statusArrives(_ wc: WorkspaceWindowController) async {
        await assertEventually("a status came") {
            try? await self.real.advance(seconds: 0.3)
            return wc.store.status != nil
        }
    }

    /// The label of the mark in the status column's third slot (nil: none shown).
    private func overrideMark(_ cue: ModelID, in wc: WorkspaceWindowController) -> String? {
        overrideMarkView(cue, in: wc)?.accessibilityLabel()
    }

    private func overrideMarkView(_ cue: ModelID, in wc: WorkspaceWindowController) -> NSImageView? {
        let outline = wc.cueList.outlineView
        guard let node = wc.cueList.node(cue) else { return nil }
        let row = outline.row(forItem: node)
        let column = outline.column(withIdentifier: NSUserInterfaceItemIdentifier("status"))
        guard row >= 0, column >= 0,
            let cell = outline.view(atColumn: column, row: row, makeIfNecessary: true) as? CueCell,
            !cell.images[2].isHidden, cell.images[2].accessibilityIdentifier() == "cue-override"
        else { return nil }
        return cell.images[2]
    }

    private func overrideShown(_ wc: WorkspaceWindowController) -> Bool {
        !wc.footer.overrideBadge.isHidden
            && wc.footer.presentation.details.first == loc("status.detail.networkOff")
            && overrideMark(net, in: wc) == loc("cue.outputOverridden")
    }

    private func overrideCleared(_ wc: WorkspaceWindowController) -> Bool {
        wc.footer.overrideBadge.isHidden
            && !wc.footer.presentation.details.contains(loc("status.detail.networkOff"))
            && overrideMark(net, in: wc) == nil
    }

    /// Turns the panel's switch (a click on the checkbox) and lets the engine's next status come.
    private func clickSwitch(thenWait done: (WorkspaceWindowController) -> Bool, _ windows: [WorkspaceWindowController])
        async
    {
        app.overrides.networkSwitch.performClick(nil)
        await assertEventually("the status followed the switch") {
            try? await self.real.advance(seconds: 0.3)
            return windows.allSatisfy(done)
        }
    }

    private func space() throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
                characters: " ", charactersIgnoringModifiers: " ", isARepeat: false, keyCode: 49))
    }

    private func overridesMenuItem() throws -> NSMenuItem {
        let window = try XCTUnwrap(
            MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == loc("menu.window") })
        return try XCTUnwrap(window.items.first { $0.title == loc("menu.window.overrides") })
    }

    // MARK: Flows

    func testOpenThePanel() async throws {
        let wc = try await openNetworkShow("Demo")
        keyDoc = app.windows.first { $0.value === wc }?.key
        await statusArrives(wc)
        XCTAssertFalse(app.overrides.isVisible)

        let item = try overridesMenuItem()
        XCTAssertEqual(item.keyEquivalent, "O")
        XCTAssertTrue(item.keyEquivalentModifierMask.contains(.command))  // an upper-case key is the shifted one
        let delegate = AppDelegate()
        delegate.appController = app
        _ = delegate.perform(try XCTUnwrap(item.action), with: item)

        let panel = try XCTUnwrap(app.overrides.panel)
        XCTAssertTrue(app.overrides.isVisible)
        XCTAssertTrue(panel.isFloatingPanel)
        XCTAssertEqual(app.overrides.networkSwitch.title, loc("overrides.network"))
        XCTAssertEqual(app.overrides.networkSwitch.state, .on)
        XCTAssertTrue(app.overrides.networkSwitch.isEnabled)
        XCTAssertTrue(wc.footer.overrideBadge.isHidden)
        XCTAssertFalse(wc.footer.presentation.details.contains(loc("status.detail.networkOff")))
        XCTAssertNil(overrideMark(net, in: wc))
        // Not modal: the workspace still takes GO.
        XCTAssertNil(NSApp.modalWindow)
    }

    func testSwitchNetworkOutputOff() async throws {
        let wc = try await openNetworkShow("Demo")
        wc.setShowMode(true)
        await assertEventually { wc.isShowMode }
        await statusArrives(wc)
        app.showOverrideControls()
        await clickSwitch(thenWait: overrideShown, [wc])

        XCTAssertTrue(wc.isShowMode, "the override changes no mode")
        XCTAssertFalse(wc.footer.isHidden)
        XCTAssertEqual(
            wc.footer.overrideBadge.attributedTitle.string.trimmingCharacters(in: .whitespaces),
            loc("footer.override.badge"))
        XCTAssertEqual(app.overrides.networkSwitch.state, .off)
        XCTAssertNil(NSApp.modalWindow)
        // The details open from the badge, in Show Mode too.
        wc.footer.overrideBadge.performClick(nil)
        let shown =
            wc.footer.popover?.contentViewController?.view.subviews
            .first { $0.accessibilityIdentifier() == AX.statusDetails } as? NSTextField
        XCTAssertEqual(shown?.stringValue.components(separatedBy: "\n").first, loc("status.detail.networkOff"))
        XCTAssertEqual(wc.footer.popover?.isShown, true)
        try assertBadgeOnScreen(wc)
        XCTAssertEqual(wc.footer.overrideBadge.layer?.backgroundColor, NSColor.systemRed.cgColor)
        XCTAssertTrue(try drawsRed(XCTUnwrap(overrideMarkView(net, in: wc)?.image)), "the mark is drawn red")
    }

    /// The opaque pixels of `image` are red (the tint is part of the drawn image).
    private func drawsRed(_ image: NSImage) throws -> Bool {
        let rep = try XCTUnwrap(image.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:)))
        var red = 0
        var other = 0
        for x in 0..<rep.pixelsWide {
            for y in 0..<rep.pixelsHigh {
                guard let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.sRGB), c.alphaComponent > 0.5 else { continue }
                if c.redComponent > 0.6 && c.greenComponent < 0.45 && c.blueComponent < 0.45 {
                    red += 1
                } else {
                    other += 1
                }
            }
        }
        return red > 0 && other == 0
    }

    /// The badge is really in the footer: in the window, with a frame inside the footer that no other
    /// control of the footer overlaps.
    private func assertBadgeOnScreen(_ wc: WorkspaceWindowController) throws {
        let footer = wc.footer
        let badge = footer.overrideBadge
        wc.window?.contentView?.layoutSubtreeIfNeeded()
        XCTAssertNotNil(badge.window)
        XCTAssertFalse(badge.isHiddenOrHasHiddenAncestor)
        let frame = footer.convert(badge.bounds, from: badge)
        XCTAssertGreaterThan(frame.width, 10)
        XCTAssertGreaterThan(frame.height, 5)
        XCTAssertTrue(footer.bounds.contains(frame), "\(frame) in \(footer.bounds)")
        let others: [NSView] =
            [
                footer.modeSwitch, footer.showModeLabel, footer.statusButton, footer.noticeLabel, footer.countLabel,
                footer.clock, footer.settingsButton,
            ] + Array(footer.toggles.values)
        for other in others where !other.isHiddenOrHasHiddenAncestor {
            let r = footer.convert(other.bounds, from: other)
            XCTAssertFalse(r.intersects(frame), "\(other) overlaps the badge")
        }
    }

    func testAFailedSendPutsTheSwitchBackBeepsAndLogs() async throws {
        let wc = try await openNetworkShow("Demo")
        await statusArrives(wc)
        var beeps = 0
        app.beep = { beeps += 1 }
        app.showOverrideControls()
        app.session.appClient.stop()
        await assertEventually { self.app.session.appClient.state != .connected }
        app.overrides.networkSwitch.performClick(nil)
        await assertEventually("switch back on") { self.app.overrides.networkSwitch.state == .on }
        XCTAssertEqual(beeps, 1)
        XCTAssertTrue(wc.store.logEntries.contains { $0.level == .error })
        XCTAssertTrue(wc.footer.overrideBadge.isHidden)
    }

    func testWithNoWorkspaceTheSwitchIsDisabledAndTheNoteSaysSo() async throws {
        app.showOverrideControls()
        XCTAssertFalse(app.overrides.networkSwitch.isEnabled)
        XCTAssertEqual(app.overrides.note.stringValue, loc("overrides.noWorkspace"))
    }

    /// The panel never takes the keys, so GO still works with it up and nothing in it can steal them.
    func testThePanelNeverBecomesKeyAndGoStillWorks() async throws {
        let wc = try await openNetworkShow("Demo")
        keyDoc = app.windows.first { $0.value === wc }?.key
        await statusArrives(wc)
        app.showOverrideControls()
        let panel = try XCTUnwrap(app.overrides.panel)
        XCTAssertFalse(panel.canBecomeKey)
        XCTAssertFalse(panel.canBecomeMain)
        XCTAssertFalse(app.overrides.note.isSelectable)
        panel.makeKeyAndOrderFront(nil)
        XCTAssertFalse(panel.isKeyWindow)
        XCTAssertNil(app.handleKeyDown(try space()))
        await assertEventually("GO started the follow") {
            try? await self.real.advance(seconds: 0.1)
            return wc.store.cueStates[self.intro]?.state == .running
        }
    }

    func testACueRunWhileOverriddenSendsNothing() async throws {
        let wc = try await openNetworkShow("Demo")
        keyDoc = app.windows.first { $0.value === wc }?.key
        await statusArrives(wc)
        app.showOverrideControls()
        await clickSwitch(thenWait: overrideShown, [wc])

        XCTAssertNil(app.handleKeyDown(try space()), "GO is taken")
        // The cue ran and its auto-follow started the cue below it.
        await assertEventually("the follow started Intro") {
            try? await self.real.advance(seconds: 0.1)
            return wc.store.cueStates[self.intro]?.state == .running
        }
        XCTAssertNotEqual(wc.store.cueStates[net]?.state, .broken)
        XCTAssertTrue(wc.store.problems[net]?.isEmpty ?? true)
        XCTAssertTrue(wc.store.warnings.isEmpty)
        XCTAssertTrue(wc.store.runtimeWarnings.isEmpty)
        XCTAssertNil(listener.receive(), "nothing was sent")
    }

    func testSwitchBackOn() async throws {
        let wc = try await openNetworkShow("Demo")
        keyDoc = app.windows.first { $0.value === wc }?.key
        await statusArrives(wc)
        app.showOverrideControls()
        await clickSwitch(thenWait: overrideShown, [wc])
        XCTAssertNil(app.handleKeyDown(try space()))
        await assertEventually("the follow started Intro") {
            try? await self.real.advance(seconds: 0.1)
            return wc.store.cueStates[self.intro]?.state == .running
        }

        await clickSwitch(thenWait: overrideCleared, [wc])
        XCTAssertEqual(app.overrides.networkSwitch.state, .on)
        // Back to the first cue, then GO again.
        guard case .success = await app.run(.resetAll, on: wc.store) else { return XCTFail("reset") }
        await assertEventually { wc.store.cueStates[self.intro]?.state != .running }
        XCTAssertNil(app.handleKeyDown(try space()))
        var received: Data?
        await assertEventually("the listener got the message") {
            try? await self.real.advance(seconds: 0.1)
            received = received ?? self.listener.receive()
            return received != nil
        }
        XCTAssertNotNil(received?.range(of: Data("/test".utf8)))
        XCTAssertNotNil(received?.range(of: Data("hello".utf8)))
        XCTAssertNil(listener.receive(), "the run while overridden sent nothing: only this one arrived")
    }

    func testTheSwitchAppliesToEveryOpenWorkspace() async throws {
        let first = try await openNetworkShow("Demo")
        let second = try await openNetworkShow("Second")
        XCTAssertTrue(first !== second)
        await statusArrives(first)
        await statusArrives(second)
        app.showOverrideControls()
        await clickSwitch(thenWait: overrideShown, [first, second])

        // One panel however often it is asked for.
        let panel = app.overrides.panel
        app.showOverrideControls()
        XCTAssertNotNil(panel)
        XCTAssertTrue(app.overrides.panel === panel)
        XCTAssertEqual(app.overrides.networkSwitch.state, .off)
    }

    /// The engine ends with the app (quit), so a relaunch starts with the switch on.
    func testAfterRelaunchTheSwitchIsOnAgain() async throws {
        let wc = try await openNetworkShow("Demo")
        await statusArrives(wc)
        app.showOverrideControls()
        await clickSwitch(thenWait: overrideShown, [wc])
        await app.session.shutdown()
        app.closeAllWindows()

        let next = try await TestEngine.start()
        defer { next.stop() }
        app = try makeApp(on: next)
        let doc = try await next.open("Demo")
        await assertEventually { self.app.windows[doc] != nil && self.app.session.stores[doc]?.model != nil }
        let again = try XCTUnwrap(app.windows[doc])
        await assertEventually("a status came") {
            try? await next.advance(seconds: 0.3)
            return again.store.status != nil
        }
        app.showOverrideControls()
        await assertEventually { self.app.overrides.networkSwitch.state == .on && again.footer.overrideBadge.isHidden }
        XCTAssertTrue(again.footer.overrideBadge.isHidden)
        XCTAssertFalse(again.footer.presentation.details.contains(loc("status.detail.networkOff")))
    }
}
