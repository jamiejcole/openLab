// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The text of every field on screen under `view` (hidden ones left out).
@MainActor
func launcherTexts(_ view: NSView) -> [String] {
    var out: [String] = []
    func walk(_ v: NSView) {
        if let field = v as? NSTextField, !field.isHidden { out.append(field.stringValue) }
        if let button = v as? NSButton, !button.isHidden, !button.title.isEmpty { out.append(button.title) }
        for sub in v.subviews where !sub.isHidden { walk(sub) }
    }
    walk(view)
    return out
}

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

    private func lines(_ launcher: LauncherWindowController) -> [String] {
        launcherTexts(launcher.window?.contentView ?? NSView())
    }

    /// Checklist: No recent shows (first launch) → the list area shows a short empty-state line, not
    /// a blank gap; the window works in light and dark mode and does not clip at its smallest size.
    /// Flow (#812 L1): with ten recents the list shows every row whole, the last one's lower edge
    /// included (the default inset style padded the rows and cut it off).
    func testTenRecentsAreAllShownWholeInTheList() throws {
        let launcher = LauncherWindowController()
        let window = try XCTUnwrap(launcher.window)
        let names =
            ["Hamlet", "Lear", "frozen", "A Very Long Show Name That Keeps Going", "Macbeth"]
            + (6...10).map { "Show \($0)" }
        launcher.reload(recents: names.map { URL(fileURLWithPath: "/Shows/\($0).openlab") })
        window.setContentSize(LauncherWindowController.defaultSize)
        let content = try XCTUnwrap(window.contentView)
        content.layoutSubtreeIfNeeded()
        let table = launcher.recentsTable
        let scroll = try XCTUnwrap(table.enclosingScrollView)
        XCTAssertEqual(table.numberOfRows, 10)
        let last = table.convert(table.rect(ofRow: 9), to: scroll.contentView)
        XCTAssertLessThanOrEqual(last.maxY, scroll.contentView.bounds.height + 0.5, "the last row ends inside the list")
        XCTAssertEqual(scroll.contentInsets.top, 0, "no padding above the first row")
        XCTAssertEqual(scroll.contentInsets.bottom, 0)
    }

    func testNoRecentShowsShowsAnEmptyStateAndDoesNotClipAtItsSmallestSizeInLightAndDark() throws {
        let launcher = LauncherWindowController()
        let window = try XCTUnwrap(launcher.window)
        launcher.reload(recents: [])
        for name in [NSAppearance.Name.aqua, .darkAqua] {
            window.appearance = NSAppearance(named: name)
            window.setContentSize(LauncherWindowController.leastSize)
            XCTAssertEqual(window.contentLayoutRect.size.width, LauncherWindowController.leastSize.width)
            let content = try XCTUnwrap(window.contentView)
            content.layoutSubtreeIfNeeded()
            let shown = lines(launcher)
            XCTAssertTrue(shown.contains(loc("launcher.recent.none")), "\(name.rawValue): \(shown)")
            XCTAssertTrue(shown.contains(loc("launcher.recent.none.detail")))
            XCTAssertTrue(launcher.recentsTable.enclosingScrollView?.isHidden ?? false, "no empty list drawn")
            for state in [LauncherEngineState.ready, .starting, .failed(loc("launcher.notResponding"))] {
                launcher.apply(state: state)
                launcher.showNotice(
                    String(format: loc("launcher.missing"), "A long show name that wraps"),
                    removing: URL(fileURLWithPath: "/x"))
                content.layoutSubtreeIfNeeded()
                try assertInside(content, name: "\(name.rawValue) \(state)")
                launcher.showNotice(nil)
            }
        }
        XCTAssertEqual(window.contentMinSize, LauncherWindowController.leastSize)
    }

    /// Every visible control's frame lies inside `root`: nothing is clipped at the window's edge.
    private func assertInside(_ root: NSView, name: String) throws {
        func walk(_ v: NSView) {
            guard !v.isHidden, v !== root, !(v is NSScrollView) else { return }
            let frame = root.convert(v.bounds, from: v)
            if v is NSTextField || v is NSButton || v is NSImageView {
                XCTAssertTrue(
                    root.bounds.insetBy(dx: -0.5, dy: -0.5).contains(frame),
                    "\(name): \(v) at \(frame) leaves \(root.bounds)")
            }
            for sub in v.subviews { walk(sub) }
        }
        walk(root)
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
        // Started with a settings file, as the relaunch below is: both runs take the same start path.
        let seed = Data(#"{"settings_version": 1, "backup_keep": 4}"#.utf8)
        let engine = try await TestEngine.start(settings: seed)
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

        let saved = await engine.savedSettings(containing: "\"backup_keep\": 12")
        let kept = try XCTUnwrap(saved, "the engine wrote the count to this machine's settings")
        engine.stop()
        let again = try await TestEngine.start(settings: kept)
        self.engine = again
        let relaunched = try await preferences(again)
        XCTAssertEqual(relaunched.backupField.stringValue, "12", "kept across a relaunch")
    }
}
