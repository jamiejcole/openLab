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
/// is an empty show. "Three" is three looping audio cues, 1 One, 2 Two and 3 Three. "Targets" is 1 intro
/// (audio), pad (audio, no number), 3 (an unnamed Fade with no target) and 4 note (a Memo). Nothing plays
/// when a show opens.
extension XCTestCase {
    private final class Anchor {}

    /// The app with `shows` open (one window each, the last in front) on a new engine. `displays`
    /// (`"V1:1920x1080,..."`) gives the engine's stage host those virtual displays, drawn offscreen;
    /// `prepare` is handed the folder of the shows' copies before the app starts (to write media
    /// or move files).
    @MainActor func launchApp(
        opening shows: [String] = ["Demo", "Second"], displays: String? = nil, windowSize: String? = nil,
        prepare: ((URL) throws -> Void)? = nil
    ) -> XCUIApplication {
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
            try prepare?(copies)
        } catch {
            XCTFail("could not set up the test shows: \(error)")
        }
        let app = XCUIApplication()
        app.launchArguments += [
            "-OpenLabEngine", "test", "-OpenLabVirtualDevice", "realtime", "-OpenLabAppSupport", support.path,
        ]
        if let displays { app.launchArguments += ["-OpenLabVirtualDisplays", displays] }
        if let windowSize { app.launchArguments += ["-OpenLabWindowSize", windowSize] }
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
        if let windowSize { assertWindowSize(windowSize, of: app) }
        return app
    }

    /// A window size asked for at launch (`least`: width 800 and the least height, else `WxH`) was applied: an
    /// option the app ignored fails here, by name, not in the layout assertions that rely on the size.
    @MainActor private func assertWindowSize(_ size: String, of app: XCUIApplication, line: UInt = #line) {
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 20), "the window is up", line: line)
        let parts = size.split(separator: "x").compactMap { Double($0) }
        let want = size == "least" ? CGSize(width: 800, height: 516) : CGSize(width: parts[0], height: parts[1])
        let ok = NSPredicate { _, _ in
            MainActor.assumeIsolated {
                abs(window.frame.width - want.width) <= 1 && abs(window.frame.height - want.height) <= 6
            }
        }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: ok, object: nil)], timeout: 5), .completed,
            "the window is \(size) as launched: \(window.frame)", line: line)
    }

    /// The window, shrunk by `shrinkWindowToLeast`, grown again by its bottom-right corner toward the
    /// screen's, and checked to be wider than its least. A window launched with `windowSize` stays off
    /// the screen's edges, so the press and the drag both land.
    @MainActor func growWindow(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) {
        let window = app.windows.firstMatch
        let wide = NSPredicate { _, _ in MainActor.assumeIsolated { window.frame.width > 801 } }
        let corner = window.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1)).withOffset(
            CGVector(dx: -3, dy: -3))
        corner.press(forDuration: 0.2, thenDragTo: corner.withOffset(CGVector(dx: 80, dy: 20)))
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: wide, object: nil)], timeout: 3), .completed,
            "the window grew: \(window.frame)", file: file, line: line)
    }

    /// The window dragged to its least width (800 points) by its top-left corner, as the operator does.
    /// The window must be off the screen's edges (launch it with `windowSize`): a corner at an edge does
    /// not resize.
    @MainActor func shrinkWindowToLeast(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) {
        let window = app.windows.firstMatch
        let narrow = NSPredicate { _, _ in MainActor.assumeIsolated { window.frame.width <= 801 } }
        window.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: 3, dy: 3))
            .press(forDuration: 0.2, thenDragTo: window.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.9)))
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: narrow, object: nil)], timeout: 3), .completed,
            "the window is at its least width: \(window.frame)", file: file, line: line)
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

    /// An app-scope request (`["type": "lose_device", ...]`, docs/protocol.md) to the engine the last
    /// `launchApp` started, sent on a connection of the test's own: the test-support requests that
    /// unplug a virtual device or read a virtual display, which the app never sends. The `result`'s
    /// `ok` body; a refusal or no answer within `timeout` seconds fails the test.
    @MainActor @discardableResult func engineRequest(
        _ request: [String: Any], timeout: TimeInterval = 20, line: UInt = #line
    ) async -> [String: Any]? {
        let what = request["type"] as? String ?? "?"
        guard let file = engineSupport?.appendingPathComponent("openLab/engine.json"),
            let data = try? Data(contentsOf: file),
            let info = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let port = (info["port"] as? NSNumber)?.intValue, let token = info["token"] as? String,
            let url = URL(string: "ws://127.0.0.1:\(port)")
        else {
            XCTFail("\(what): no engine.json in \(engineSupport?.path ?? "nil")", line: line)
            return nil
        }
        let socket = URLSession.shared.webSocketTask(with: url)
        socket.resume()
        // A guard, not a wait: an engine that never answers ends the receive below with an error.
        let guardTask = Task {
            try await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
            socket.cancel(with: .goingAway, reason: nil)
        }
        defer {
            guardTask.cancel()
            socket.cancel(with: .normalClosure, reason: nil)
        }
        func send(_ message: [String: Any]) async throws {
            let text = String(decoding: try JSONSerialization.data(withJSONObject: message), as: UTF8.self)
            try await socket.send(.string(text))
        }
        func next() async throws -> [String: Any] {
            let text: String
            switch try await socket.receive() {
            case .string(let s): text = s
            case .data(let d): text = String(decoding: d, as: UTF8.self)
            @unknown default: return [:]
            }
            return (try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any]) ?? [:]
        }
        do {
            try await send([
                "type": "hello", "protocol_version": protocolVersion, "token": token, "client_name": "openLab UI tests",
            ])
            while try await next()["type"] as? String != "welcome" {}
            try await send(["type": "request", "id": 1, "request": request])
            while true {
                let message = try await next()
                guard message["type"] as? String == "result", (message["id"] as? NSNumber)?.intValue == 1 else {
                    continue
                }
                try? await send(["type": "goodbye", "shutdown": false])
                guard let ok = message["ok"] as? [String: Any] else {
                    XCTFail("\(what) refused: \(message["error"] ?? message)", line: line)
                    return nil
                }
                return ok
            }
        } catch {
            XCTFail("\(what): no answer from the engine: \(error)", line: line)
            return nil
        }
    }

    /// The seconds a time as the app shows it reads (`00:15.20`, `1:02:03.5`); nil for other text.
    func seconds(_ text: String) -> Double? {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":").map { Double($0) }
        guard !parts.isEmpty, parts.count <= 3, parts.allSatisfy({ $0 != nil }) else { return nil }
        return parts.reduce(0) { $0 * 60 + ($1 ?? 0) }
    }

    /// The text of a field or label as XCUITest reads it.
    @MainActor func text(_ element: XCUIElement) -> String { element.value as? String ?? "" }

    /// True once `element`'s text reads a time within `tolerance` seconds of `expected`.
    @MainActor func waitForTime(
        _ element: XCUIElement, near expected: Double, tolerance: Double, timeout: TimeInterval = 10
    ) -> Bool {
        let near = NSPredicate { _, _ in
            MainActor.assumeIsolated {
                guard let t = self.seconds(element.value as? String ?? "") else { return false }
                return abs(t - expected) <= tolerance
            }
        }
        let expectation = XCTNSPredicateExpectation(predicate: near, object: nil)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
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
