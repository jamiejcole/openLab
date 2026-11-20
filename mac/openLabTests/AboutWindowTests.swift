// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The About window against the real engine where a workspace is involved.
@MainActor
final class AboutWindowTests: XCTestCase {
    private func texts(_ view: NSView) -> [String] {
        var out: [String] = []
        func walk(_ v: NSView) {
            if let field = v as? NSTextField, !field.isHidden { out.append(field.stringValue) }
            for sub in v.subviews where !sub.isHidden { walk(sub) }
        }
        walk(view)
        return out
    }

    private func controller() -> AboutWindowController {
        let about = AboutWindowController()
        addTeardownBlock { @MainActor in about.window?.close() }
        return about
    }

    /// Checklist: any state → About → one window with the name, exact version and build, and a
    /// one-line description.
    func testAboutShowsTheNameVersionBuildAndDescription() throws {
        let about = controller()
        about.showWindow(nil)
        let window = try XCTUnwrap(about.window)
        XCTAssertTrue(window.isVisible)
        let shown = texts(try XCTUnwrap(window.contentView))
        XCTAssertTrue(shown.contains("openLab"))
        XCTAssertTrue(shown.contains(loc("about.description")))
        let expected = "Version \(AboutWindowController.version) (\(AboutWindowController.build))"
        XCTAssertEqual(about.versionLabel.stringValue, expected)
        XCTAssertNotEqual(AboutWindowController.version, "dev", "the bundle names its version")
    }

    /// Checklist: nothing in the window is a link, button or credits control, and the licence line
    /// is the owner's wording.
    func testAboutHasNoLinksButtonsOrCreditsAndTheLicenceLineIsAsWorded() throws {
        let about = controller()
        about.showWindow(nil)
        let content = try XCTUnwrap(about.window?.contentView)
        var controls: [NSView] = []
        func walk(_ v: NSView) {
            if v is NSControl && !(v is NSTextField) && !(v is NSImageView) { controls.append(v) }
            if let t = v as? NSTextField,
                t.allowsEditingTextAttributes
                    || !t.attributedStringValue.string.isEmpty
                        && t.attributedStringValue.attribute(.link, at: 0, effectiveRange: nil) != nil
            {
                controls.append(v)
            }
            v.subviews.forEach(walk)
        }
        walk(content)
        XCTAssertTrue(controls.isEmpty, "\(controls)")
        let shown = texts(content)
        XCTAssertTrue(shown.contains("Licensed under the GNU General Public License (GPLv3)."))
        XCTAssertTrue(shown.contains("Show playback for live theatre"))
    }

    /// Checklist: a second About command comes to the front; there is never a second window.
    func testAboutAgainReusesTheOneWindow() {
        let about = controller()
        about.showWindow(nil)
        let first = about.window
        about.window?.orderBack(nil)
        about.showWindow(nil)
        XCTAssertTrue(about.window === first)
        XCTAssertTrue(about.window?.isVisible == true)
        XCTAssertEqual(NSApp.windows.filter { $0.identifier?.rawValue == AX.aboutWindow }.count, 1)
    }

    /// Checklist: Show Mode with a cue list focused → About over it → closing it leaves the cue
    /// list as the focus, so the GO key still reaches the workspace.
    func testAboutOverShowModeLeavesTheCueListFocusedAfterClosing() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        wc.showWindow(nil)
        addTeardownBlock { @MainActor in wc.window?.close() }
        wc.setShowMode(true)
        await assertEventually("Show Mode") { rig.store.document?.showMode == true }
        let window = try XCTUnwrap(wc.window)
        let list = wc.cueList.outlineView
        XCTAssertTrue(window.makeFirstResponder(list))
        let about = controller()
        about.showWindow(nil)
        XCTAssertTrue(window.isVisible)
        XCTAssertTrue(window.firstResponder === list, "About takes no focus from the list's window")
        about.window?.performClose(nil)
        XCTAssertFalse(about.window?.isVisible == true)
        XCTAssertTrue(window.firstResponder === list)
        XCTAssertTrue(rig.store.document?.showMode == true)
    }

    /// Checklist: Esc and Cmd-W close it; it cannot be resized and its content is never clipped.
    func testAboutClosesOnEscAndCloseAndHasNoRoomToResizeOrClip() throws {
        let about = controller()
        let window = try XCTUnwrap(about.window)
        XCTAssertFalse(window.styleMask.contains(.resizable))
        for appearance in [NSAppearance.Name.aqua, .darkAqua] {
            window.appearance = NSAppearance(named: appearance)
            let content = try XCTUnwrap(window.contentView)
            content.layoutSubtreeIfNeeded()
            XCTAssertEqual(content.frame.size, content.fittingSize, appearance.rawValue)
            for field in texts2(content) {
                XCTAssertLessThanOrEqual(field.fittingSize.height, field.frame.height + 0.5, field.stringValue)
                XCTAssertTrue(content.bounds.contains(field.convert(field.bounds, to: content)))
            }
        }
        about.showWindow(nil)
        window.cancelOperation(nil)
        XCTAssertFalse(window.isVisible, "Esc")
        about.showWindow(nil)
        let close = try XCTUnwrap(
            MainMenu.make().bar.items.compactMap(\.submenu).flatMap(\.items)
                .first { $0.action == #selector(NSWindow.performClose(_:)) })
        XCTAssertEqual(close.keyEquivalent, "w")
        window.performClose(nil)
        XCTAssertFalse(window.isVisible, "Cmd-W")
    }

    private func texts2(_ view: NSView) -> [NSTextField] {
        var out: [NSTextField] = []
        func walk(_ v: NSView) {
            if let f = v as? NSTextField { out.append(f) }
            for s in v.subviews where !s.isHidden { walk(s) }
        }
        walk(view)
        return out
    }

    /// Checklist: the version in About is the bundle's, and the launcher shows the same one.
    func testAboutVersionIsTheBundlesAndTheLaunchersOne() {
        let bundle = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        XCTAssertEqual(AboutWindowController.version, bundle ?? "dev")
        XCTAssertEqual(LauncherWindowController.version, AboutWindowController.version)
        XCTAssertEqual(
            AboutWindowController.build, Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0")
    }
}
