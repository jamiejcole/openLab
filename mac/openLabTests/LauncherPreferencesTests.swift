// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// UI-01, WS-08, NFR-06: the launcher's version and help link, and the application preferences'
/// General tab: how many backups of each workspace this machine keeps, kept by the engine.
@MainActor
final class LauncherPreferencesTests: XCTestCase {
    var engine: TestEngine?

    override func tearDown() async throws { engine?.stop() }

    private func label(_ id: String, in root: NSView?) -> NSView? {
        guard let root else { return nil }
        if root.accessibilityIdentifier() == id { return root }
        for s in root.subviews { if let found = label(id, in: s) { return found } }
        return nil
    }

    /// Checklist: Launcher window → the version and a help link.
    func testTheLauncherShowsTheVersionAndAHelpLinkThatOpensItsPage() throws {
        let help = try XCTUnwrap(URL(string: "https://example.org/openlab-help"))
        let launcher = LauncherWindowController(helpURL: help)
        let version = try XCTUnwrap(label(AX.launcherVersion, in: launcher.window?.contentView) as? NSTextField)
        XCTAssertEqual(version.stringValue, String(format: loc("launcher.version"), LauncherWindowController.version))
        XCTAssertFalse(LauncherWindowController.version.isEmpty)
        let button = try XCTUnwrap(label(AX.launcherHelp, in: launcher.window?.contentView) as? NSButton)
        XCTAssertEqual(button.title, loc("launcher.help"))
        var opened: [URL] = []
        launcher.openURL = { opened.append($0) }
        button.performClick(nil)
        XCTAssertEqual(opened, [help])
        // A build with no help page shows no link.
        let bare = LauncherWindowController(helpURL: nil)
        XCTAssertNil(label(AX.launcherHelp, in: bare.window?.contentView))
        XCTAssertNotNil(label(AX.launcherVersion, in: bare.window?.contentView))
    }

    private func preferences(_ engine: TestEngine) async throws -> PreferencesWindowController {
        let suite = "openlab-prefs-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        let prefs = PreferencesWindowController(
            preferences: LaunchPreferences(defaults: defaults), audio: AudioPreferencesModel(client: engine.app))
        prefs.tabs.selectTabViewItem(withIdentifier: "general")
        await prefs.refreshing?.value
        return prefs
    }

    private func commit(_ prefs: PreferencesWindowController, _ text: String) async {
        prefs.backupField.stringValue = text
        _ = prefs.backupField.target?.perform(prefs.backupField.action, with: prefs.backupField)
        await prefs.refreshing?.value
    }

    private func engineCount(_ engine: TestEngine) async -> UInt32? {
        guard case .success(let ok) = await engine.request(.backupKeep), case .backupKeep(let n)? = ok.data else {
            return nil
        }
        return n
    }

    /// Checklist: application preferences beyond the at-launch choice → backups kept: the General tab
    /// shows what the engine has, a number typed is kept by it (0 turns backups off), and one that is
    /// not a count is refused with the old number shown again.
    func testBackupsKeptIsShownKeptByTheEngineAndARefusedNumberComesBack() async throws {
        let engine = try await TestEngine.start()
        self.engine = engine
        let prefs = try await preferences(engine)
        let start = try XCTUnwrap(await engineCount(engine))
        XCTAssertEqual(prefs.backupField.stringValue, String(start))
        XCTAssertTrue(prefs.generalMessage.isHidden)
        await commit(prefs, "3")
        let three = await engineCount(engine)
        XCTAssertEqual(three, 3)
        XCTAssertEqual(prefs.backupField.stringValue, "3")
        await commit(prefs, "0")
        let zero = await engineCount(engine)
        XCTAssertEqual(zero, 0, "0 turns backups off")
        await commit(prefs, "many")
        XCTAssertFalse(prefs.generalMessage.isHidden)
        XCTAssertEqual(
            prefs.generalMessage.stringValue,
            String(format: loc("prefs.general.backups.range"), 0, Int(AudioPreferencesModel.backupKeepRange.upperBound)))
        XCTAssertEqual(prefs.backupField.stringValue, "0", "the engine's number is shown again")
        let kept = await engineCount(engine)
        XCTAssertEqual(kept, 0)
        await commit(prefs, "100000")
        let still = await engineCount(engine)
        XCTAssertEqual(still, 0, "a count above the limit changes nothing")
    }
}
