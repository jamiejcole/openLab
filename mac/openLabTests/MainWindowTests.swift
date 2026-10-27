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

    func testAllAreasPresent() throws {
        let wc = MainWindowController()
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

    func testOutlineColumnsInQLabOrder() {
        let outline = MainWindowController().cueList.outlineView
        XCTAssertEqual(
            outline.tableColumns.map(\.identifier.rawValue),
            ["status", "number", "name", "target", "preWait", "duration", "postWait", "continue"])
        XCTAssertTrue(outline.usesAlternatingRowBackgroundColors)
    }

    func testSidebarHiddenByDefault() {
        let wc = MainWindowController()
        XCTAssertFalse(wc.isVisible(.sidebar))
        XCTAssertEqual(wc.footer.toggles[.sidebar]?.state, .off)
        for area in [LayoutArea.toolbar, .masthead, .inspector] { XCTAssertTrue(wc.isVisible(area)) }
    }

    func testEachFooterToggleHidesAndReshowsItsArea() throws {
        let wc = MainWindowController()
        for area in LayoutArea.allCases {
            let button = try XCTUnwrap(wc.footer.toggles[area])
            let before = wc.isVisible(area)
            button.performClick(nil)
            XCTAssertEqual(wc.isVisible(area), !before, "\(area) first click")
            button.performClick(nil)
            XCTAssertEqual(wc.isVisible(area), before, "\(area) second click")
        }
    }

    func testInspectorShowsNoCueSelected() {
        let wc = MainWindowController()
        XCTAssertEqual(wc.inspector.emptyLabel.stringValue, "No cue selected")
        XCTAssertFalse(wc.inspector.emptyLabel.isHidden)
    }

    func testSymbolsResolve() {
        let names =
            CueToolbarView.symbolNames + SidebarViewController.symbolNames + FooterBar.symbolNames
        for name in names {
            XCTAssertNotNil(NSImage(systemSymbolName: name, accessibilityDescription: nil), name)
        }
    }

    func testClosedWindowReturnsViaReopenAndWindowMenu() throws {
        let delegate = AppDelegate()
        delegate.showMainWindow(nil)
        let window = try XCTUnwrap(delegate.mainWindowController.window)
        XCTAssertTrue(window.isVisible)

        window.close()
        XCTAssertFalse(window.isVisible)
        XCTAssertFalse(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
        _ = delegate.applicationShouldHandleReopen(NSApp, hasVisibleWindows: false)
        XCTAssertTrue(window.isVisible)

        window.close()
        let item = try XCTUnwrap(
            MainMenu.make().windows.items.first { $0.action == #selector(AppDelegate.showMainWindow(_:)) })
        XCTAssertEqual(item.keyEquivalent, "0")
        delegate.perform(try XCTUnwrap(item.action), with: item)
        XCTAssertTrue(window.isVisible)
        XCTAssertTrue(delegate.mainWindowController.window === window)
        window.close()
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
