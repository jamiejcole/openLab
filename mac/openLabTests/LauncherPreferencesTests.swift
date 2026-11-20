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

    /// Checklist: Launcher window → the version (the help link is deferred until the project has a
    /// page to point at).
    func testTheLauncherShowsTheVersion() throws {
        let launcher = LauncherWindowController()
        let version = try XCTUnwrap(label(AX.launcherVersion, in: launcher.window?.contentView) as? NSTextField)
        XCTAssertEqual(version.stringValue, String(format: loc("launcher.version"), LauncherWindowController.version))
        XCTAssertFalse(LauncherWindowController.version.isEmpty)
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
        let first = await engineCount(engine)
        let start = try XCTUnwrap(first)
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
            String(
                format: loc("prefs.general.backups.range"), 0,
                Int(AudioPreferencesModel.backupKeepRange.upperBound)))
        XCTAssertEqual(prefs.backupField.stringValue, "0", "the engine's number is shown again")
        let kept = await engineCount(engine)
        XCTAssertEqual(kept, 0)
        await commit(prefs, "100000")
        let still = await engineCount(engine)
        XCTAssertEqual(still, 0, "a count above the limit changes nothing")
    }

    /// Flow "General: backups kept 0, then out of range or not a number": 0 turns backups off and says
    /// so; an invalid entry is refused with the allowed range and the field returns to what it held; a
    /// valid value is kept for the next launch.
    func testBackupsKeptZeroSaysSoARefusedEntryNamesTheRangeAndAValueSurvivesARelaunch() async throws {
        let engine = try await TestEngine.start()
        self.engine = engine
        let prefs = try await preferences(engine)
        XCTAssertTrue(prefs.backupOffNote.isHidden)
        await commit(prefs, "0")
        XCTAssertFalse(prefs.backupOffNote.isHidden, "0 says backups are off")
        XCTAssertEqual(prefs.backupOffNote.stringValue, loc("prefs.general.backups.off"))
        XCTAssertTrue(prefs.generalMessage.isHidden)

        await commit(prefs, "7")
        XCTAssertTrue(prefs.backupOffNote.isHidden)
        for bad in ["-1", "1001", "2.5", ""] {
            await commit(prefs, bad)
            XCTAssertEqual(
                prefs.generalMessage.stringValue, String(format: loc("prefs.general.backups.range"), 0, 1000), bad)
            XCTAssertFalse(prefs.generalMessage.isHidden)
            XCTAssertEqual(prefs.backupField.stringValue, "7", "the field returns to its previous value")
        }
        XCTAssertTrue(prefs.audio?.message == nil, "a backup refusal is not the audio page's message")
        XCTAssertTrue(prefs.audioMessage.isHidden)
        await commit(prefs, "12")
        XCTAssertTrue(prefs.generalMessage.isHidden, "a valid entry clears the refusal")

        let kept = try Data(contentsOf: engine.support.appendingPathComponent("openLab/settings.json"))
        engine.stop()
        let again = try await TestEngine.start(settings: kept)
        self.engine = again
        let relaunched = try await preferences(again)
        XCTAssertEqual(relaunched.backupField.stringValue, "12", "kept across a relaunch")
    }
}
