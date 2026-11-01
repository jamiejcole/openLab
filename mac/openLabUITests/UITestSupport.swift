// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Launching the app on the real engine and waiting on the UI, shared by every UI test.
///
/// The app runs its test engine (`-OpenLabEngine test`, Debug builds only): the bundled engine on
/// its virtual audio device in realtime mode, with an Application Support folder of its own, ending
/// with the app. Each launch opens fresh copies of the test shows (`mac/TestShows`), so edits and
/// autosaves never reach the next test.
///
/// The demo show ("Demo"): 1 Intro (a looping audio cue, a.wav), an unnamed Fade cue of Intro
/// (5 s), and a group 2 Act One of 2.1 Door slam (d.wav, plays once), 2.2 Rain
/// (looping, blue, flagged, auto-follow) and 2.3 Radio (its file o.wav is missing: broken). "Second"
/// is an empty show. Nothing plays when a show opens.
extension XCTestCase {
    private final class Anchor {}

    /// The app with `shows` open (one window each, the last in front) on a new engine.
    @MainActor func launchApp(opening shows: [String] = ["Demo", "Second"]) -> XCUIApplication {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent(
            "openlab-ui-\(UUID().uuidString)", isDirectory: true)
        let support = base.appendingPathComponent("support", isDirectory: true)
        let copies = base.appendingPathComponent("shows", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
            let source = try XCTUnwrap(
                Bundle(for: Anchor.self).url(forResource: "TestShows", withExtension: nil), "the test shows are bundled"
            )
            try FileManager.default.copyItem(at: source, to: copies)
        } catch {
            XCTFail("could not set up the test shows: \(error)")
        }
        let app = XCUIApplication()
        app.launchArguments += [
            "-OpenLabEngine", "test", "-OpenLabVirtualDevice", "realtime", "-OpenLabAppSupport", support.path,
        ]
        for show in shows {
            app.launchArguments += ["-OpenLabOpen", copies.appendingPathComponent("\(show)/\(show).openlab").path]
        }
        engineSupport = support
        addTeardownBlock { @MainActor [weak self] in
            app.terminate()
            // A failed test prints what the engine logged, the first place to look.
            if self?.testRun?.hasSucceeded == false {
                let log = support.appendingPathComponent("openLab/Logs/openlab.log")
                print("engine log (\(log.path)):\n" + ((try? String(contentsOf: log, encoding: .utf8)) ?? "none"))
            }
            try? FileManager.default.removeItem(at: base)
        }
        app.launch()
        return app
    }

    /// The demo show alone, its list loaded.
    @MainActor func launchDemo() -> XCUIApplication {
        let app = launchApp(opening: ["Demo"])
        XCTAssertTrue(waitFor("isHittable == true", app.staticTexts["Intro"]), "the demo is ready")
        return app
    }

    /// Ends the engine the last `launchApp` started as a crash would (SIGKILL: it leaves its
    /// `engine.json` behind and writes nothing more).
    @MainActor func killEngine() {
        let file = engineSupport?.appendingPathComponent("openLab/engine.json")
        guard let file, let data = try? Data(contentsOf: file),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let pid = (object["pid"] as? NSNumber)?.int32Value, pid > 1
        else { return XCTFail("no engine.json in \(file?.path ?? "nil")") }
        XCTAssertEqual(kill(pid, SIGKILL), 0, "the engine (pid \(pid)) is killed: errno \(errno)")
    }

    /// True once `object` matches the predicate `format`, false after `timeout` seconds.
    @MainActor func waitFor(_ format: String, _ object: Any?, timeout: TimeInterval = 10) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: format), object: object)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }

    /// The cue-state icons labelled `label` ("Running", "Broken", ...).
    @MainActor func cueStates(_ label: String, in app: XCUIApplication) -> XCUIElementQuery {
        app.images.matching(identifier: "cue-state").matching(NSPredicate(format: "label == %@", label))
    }

    /// The cue list's row showing `title` (in any window).
    @MainActor func cueRow(_ title: String, in app: XCUIApplication) -> XCUIElement {
        app.outlines.matching(identifier: "cue-list").outlineRows.containing(
            NSPredicate(format: "label == %@ OR value == %@", title, title)
        ).firstMatch
    }

    /// The state icon of the row showing `title`, when it reads `label`.
    @MainActor func cueState(_ label: String, of title: String, in app: XCUIApplication) -> XCUIElement {
        cueRow(title, in: app).images.matching(identifier: "cue-state").matching(
            NSPredicate(format: "label == %@", label)
        ).firstMatch
    }

    /// Selects the cue row titled `title` and waits until the list shows it selected. A click that
    /// lands while the list is still laying out selects nothing, which left the inspector without
    /// its tabs; the click is repeated until the selection shows.
    @MainActor func selectCue(_ title: String, in app: XCUIApplication) {
        let row = app.staticTexts[title]
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the cue \(title) is listed")
        let selected = app.descendants(matching: .outlineRow).matching(NSPredicate(format: "selected == true"))
        for _ in 0..<3 {
            row.click()
            if waitFor("count == 1", selected, timeout: 4) { return }
        }
        XCTFail("the cue \(title) does not get selected")
    }

    /// The inspector or sidebar tab titled `title`, once it has laid out: the tab bar is a segmented
    /// control (radio buttons) or a tab view (tabs), and the same query is retried until the control
    /// exists instead of reading `.exists` once while the pane is still laying out (#361). The
    /// subscript matches the element's accessibility identifier, label or title.
    @MainActor func tabButton(_ title: String, in app: XCUIApplication) -> XCUIElement {
        let radio = app.radioButtons[title]
        let tab = app.tabs[title]
        for _ in 0..<20 {
            if radio.waitForExistence(timeout: 0.5) { return radio }
            if tab.waitForExistence(timeout: 0.5) { return tab }
        }
        XCTFail("no tab \(title)")
        return radio
    }
}

/// The Application Support folder of the engine the running test launched (for `killEngine`).
@MainActor private var engineSupport: URL?
