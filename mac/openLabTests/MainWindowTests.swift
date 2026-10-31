// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

@MainActor
final class MainWindowTests: XCTestCase {
    private func ax(_ root: NSView, _ id: String) -> NSView? {
        if root.accessibilityIdentifier() == id { return root }
        for sub in root.subviews { if let hit = ax(sub, id) { return hit } }
        return nil
    }

    private func controller() throws -> WorkspaceWindowController {
        WorkspaceWindowController(store: try Rig().store)
    }

    func testAllAreasPresent() throws {
        let wc = try controller()
        let root = try XCTUnwrap(wc.window?.contentView)
        for id in [
            AX.masthead, AX.goButton, AX.standbyField, AX.standbyNotes, AX.cueToolbar, AX.cueList,
            AX.inspector, AX.sidebar, AX.sidebarTabs, AX.transportReset, AX.transportPause,
            AX.transportResume, AX.transportPanic, AX.footer, AX.modeSwitch, AX.statusLabel,
            AX.settingsButton, AX.toggleToolbar, AX.toggleMasthead, AX.toggleInspector, AX.toggleSidebar,
        ] {
            XCTAssertNotNil(ax(root, id), id)
        }
        XCTAssertFalse(wc.masthead.goButton.accessibilityLabel()?.isEmpty ?? true)
    }

    func testOutlineColumnsInQLabOrder() throws {
        let outline = try controller().cueList.outlineView
        XCTAssertEqual(
            outline.tableColumns.map(\.identifier.rawValue),
            ["status", "number", "name", "target", "preWait", "duration", "postWait", "continue"])
        XCTAssertTrue(outline.usesAlternatingRowBackgroundColors)
    }

    func testSidebarHiddenByDefault() throws {
        let wc = try controller()
        XCTAssertFalse(wc.isVisible(.sidebar))
        XCTAssertEqual(wc.footer.toggles[.sidebar]?.state, .off)
        for area in [LayoutArea.toolbar, .masthead, .inspector] { XCTAssertTrue(wc.isVisible(area)) }
    }

    func testEachFooterToggleHidesAndReshowsItsArea() throws {
        let wc = try controller()
        for area in LayoutArea.allCases {
            let button = try XCTUnwrap(wc.footer.toggles[area])
            let before = wc.isVisible(area)
            button.performClick(nil)
            XCTAssertEqual(wc.isVisible(area), !before, "\(area) first click")
            button.performClick(nil)
            XCTAssertEqual(wc.isVisible(area), before, "\(area) second click")
        }
    }

    func testInspectorShowsNoCueSelected() throws {
        let wc = try controller()
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, "No cue selected")
        XCTAssertFalse(wc.inspector.emptyLabel.isHidden)
    }

    func testEveryIconAndFallbackResolves() {
        for icon in Icon.allCases {
            let e = icon.entry
            XCTAssertNotNil(NSImage(systemSymbolName: e.symbol, accessibilityDescription: nil), "\(icon) \(e.symbol)")
            XCTAssertTrue(icon.resolvesToSymbol, "\(icon)")
            if let fb = e.fallback {
                XCTAssertNotNil(NSImage(systemSymbolName: fb, accessibilityDescription: nil), "\(icon) fallback \(fb)")
            }
            // Deployment target is macOS 12: a newer symbol must come with a fallback, and no row
            // may need more than 12 without one.
            XCTAssertLessThanOrEqual(e.minMacOS, 12, "\(icon) needs macOS \(e.minMacOS)")
            if e.minMacOS > 10 { XCTAssertNotNil(e.fallback, "\(icon) has no fallback") }
        }
        // The call sites that expose their icons use only table rows.
        XCTAssertFalse((CueToolbarView.icons + SidebarViewController.icons + FooterBar.icons).isEmpty)
    }

    func testMissingSymbolFallsBackThenShowsTheLocalisedNameAndLogsOnce() {
        defer { IconLookup.shared.setLookup(nil) }
        // The primary name vanishes: the fallback symbol shows.
        IconLookup.shared.setLookup { $0 != "gearshape" }
        XCTAssertNotNil(Icon.settings.image("Settings").accessibilityDescription)
        XCTAssertTrue(Icon.settings.resolvesToSymbol)
        XCTAssertEqual(IconLookup.shared.reportedNames, ["gearshape"])
        // Both vanish: a text image carrying the label, logged once per name.
        IconLookup.shared.setLookup { _ in false }
        let text = Icon.settings.image("Settings")
        XCTAssertEqual(text.accessibilityDescription, "Settings")
        XCTAssertTrue(text.isTemplate)
        XCTAssertGreaterThan(text.size.width, Icon.settings.image("S", compact: true).size.width)
        _ = Icon.settings.image("Settings")
        XCTAssertEqual(IconLookup.shared.reportedNames, ["gearshape"])
        _ = Icon.kindAudio.image("Audio")
        XCTAssertEqual(IconLookup.shared.reportedNames, ["gearshape", "speaker.wave.2"])
        XCTAssertFalse(Icon.settings.resolvesToSymbol)
    }

    func testEveryDottedStringLiteralInSourcesIsLocalised() throws {
        let dir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("openLab")
        let files = try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        XCTAssertFalse(files.isEmpty)
        let regex = try NSRegularExpression(pattern: "\"([a-z][A-Za-z]*(?:\\.[A-Za-z]+)+)\"")
        let bundle = Bundle(for: AppDelegate.self)
        var seen = 0
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for m in regex.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                guard let r = Range(m.range(at: 1), in: text) else { continue }
                let key = String(text[r])
                // SF Symbol names share the dotted shape; they are checked in testSymbolsResolve.
                if NSImage(systemSymbolName: key, accessibilityDescription: nil) != nil { continue }
                seen += 1
                XCTAssertNotEqual(
                    bundle.localizedString(forKey: key, value: "\u{1}", table: nil), "\u{1}",
                    "\(key) missing from Localizable.strings (\(file.lastPathComponent))")
            }
        }
        XCTAssertGreaterThan(seen, 30)
    }
}
