// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// The inspector on the real engine, which applies name edits and sends the `model_changed`:
/// a rename shows in the list only after the command went out and came back.
@MainActor
final class InspectorUITests: XCTestCase {
    func testRenamingACueShowsInTheList() {
        let app = launchDemo()

        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        XCTAssertTrue(
            waitFor("value == 'Intro'", app.textFields["inspector-name"]),
            "the playhead cue is in the inspector at open")
        app.staticTexts["Intro"].click()
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        XCTAssertEqual(name.value as? String, "Intro")
        name.click()
        name.typeKey("a", modifierFlags: .command)
        name.typeText("Thunder\r")

        XCTAssertTrue(app.staticTexts["Thunder"].waitForExistence(timeout: 10), "the list shows the new name")
        XCTAssertFalse(app.staticTexts["Intro"].exists)
    }

    func testGroupSetToPlaylistAndLoop() {
        let app = launchDemo()

        selectCue("Act One", in: app)
        tabButton("Mode", in: app).click()
        let playlist = app.radioButtons["inspector-group-mode-playlist"]
        XCTAssertTrue(playlist.waitForExistence(timeout: 5))
        let loop = app.checkBoxes["inspector-group-loop"]
        XCTAssertFalse(loop.exists, "loop is for playlists, shown only for one")
        playlist.click()
        XCTAssertTrue(loop.waitForExistence(timeout: 5))
        loop.click()
        XCTAssertTrue(waitFor("value == 1", loop))
    }

    func testPreviewButtonMarksTheRowAndClears() {
        let app = launchDemo()

        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        let button = app.buttons["inspector-preview"]
        XCTAssertTrue(button.waitForExistence(timeout: 5))
        XCTAssertEqual(button.title, "Preview")
        let marks = app.images.matching(identifier: "cue-preview").matching(
            NSPredicate(format: "label == 'Previewing'"))
        XCTAssertEqual(marks.count, 0)

        button.click()
        XCTAssertTrue(marks.firstMatch.waitForExistence(timeout: 10), "the row shows it is previewing")
        XCTAssertEqual(button.title, "Stop Preview")

        button.click()
        XCTAssertTrue(waitFor("count == 0", marks))
        XCTAssertEqual(button.title, "Preview")
    }
}

/// The Time & Loops tab's waveform with real presses (QLab 5 manual, Audio cues, the waveform):
/// Door slam of the demo show plays a 30 s tone once.
@MainActor
final class TimeLoopsUITests: XCTestCase {
    /// The point `seconds` into the 30 s file across the waveform, at the height of its lanes (the
    /// ruler is the view's top 18 points).
    private func point(_ seconds: Double, on wave: XCUIElement) -> XCUICoordinate {
        wave.coordinate(withNormalizedOffset: .zero).withOffset(
            CGVector(dx: wave.frame.width * seconds / 30, dy: 48))
    }

    /// The remaining time Active Cues shows for the one cue that plays ("00:01.20  -00:13.80").
    private func remaining(in app: XCUIApplication) -> Double? {
        let times = app.staticTexts.matching(identifier: "active-cue-times").firstMatch
        guard let part = text(times).components(separatedBy: "-").last else { return nil }
        return seconds(part)
    }

    func testWaveformClickMovesThePlaybackPositionDragSelectsASectionAndAHandleDragTrims() throws {
        let app = launchApp(opening: ["Demo"]) { copies in
            try writeWAV(copies.appendingPathComponent("Demo/media/d.wav"), seconds: 30)
        }
        selectCue("Door slam", in: app)
        tabButton("Timing & Loops", in: app).click()
        let wave = app.descendants(matching: .any)["time-loops-waveform"]
        XCTAssertTrue(waitFor("isHittable == true", wave), "the waveform is shown")
        let start = app.textFields["time-loops-start"]
        let end = app.textFields["time-loops-end"]
        XCTAssertTrue(waitForTime(end, near: 30, tolerance: 0.01), "the whole file: \(text(end))")
        XCTAssertTrue(waitForTime(start, near: 0, tolerance: 0.01))

        // A press on the start handle dragged right trims the start; the playback does not move.
        point(0, on: wave).withOffset(CGVector(dx: 1, dy: 0)).press(
            forDuration: 0.2, thenDragTo: point(3, on: wave))
        XCTAssertTrue(waitForTime(start, near: 3, tolerance: 0.6), "the start is trimmed: \(text(start))")
        XCTAssertFalse(cueState("Loaded", of: "Door slam", in: app).exists, "a handle drag loads nothing")

        // A click moves the playback position there: the cue is loaded at it, and GO plays from it.
        point(15, on: wave).click()
        XCTAssertTrue(
            cueState("Loaded", of: "Door slam", in: app).waitForExistence(timeout: 10), "loaded at the click")
        app.checkBoxes["toggle-sidebar"].click()
        tabButton("0 Active Cues", in: app).click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])  // GO, from the waveform's focus
        XCTAssertTrue(cueState("Running", of: "Door slam", in: app).waitForExistence(timeout: 10), "GO plays it")
        let left = try XCTUnwrap(remaining(in: app), "the remaining time is shown")
        XCTAssertEqual(left, 15, accuracy: 2.5, "played from about 15 s of 30, not from the start")
        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", cueState("Running", of: "Door slam", in: app)), "stopped")
        // GO moved the selection on with the playhead (to Rain): Door slam is shown again.
        selectCue("Door slam", in: app)
        tabButton("Timing & Loops", in: app).click()
        XCTAssertTrue(waitForTime(end, near: 30, tolerance: 0.01), "Door slam's waveform: \(text(end))")

        // A drag across the waveform selects a section and moves nothing; Shift-I and Shift-O crop to it.
        point(18, on: wave).press(forDuration: 0.2, thenDragTo: point(24, on: wave))
        XCTAssertFalse(
            cueState("Loaded", of: "Door slam", in: app).waitForExistence(timeout: 1), "a drag loads nothing")
        XCTAssertTrue(waitForTime(start, near: 3, tolerance: 0.6), "and trims nothing")
        app.typeKey("i", modifierFlags: .shift)
        app.typeKey("o", modifierFlags: .shift)
        XCTAssertTrue(waitForTime(start, near: 18, tolerance: 0.6), "cropped to the section: \(text(start))")
        XCTAssertTrue(waitForTime(end, near: 24, tolerance: 0.6), "cropped to the section: \(text(end))")
    }
}

/// Video cues with real presses and keys, on virtual displays only (the stage host draws offscreen):
/// the Geometry tab, and a Video cue made, given a file and a stage, played and stopped.
@MainActor
final class VideoUITests: XCTestCase {
    /// The Geometry show (cue 6 is in custom mode: moved, scaled 2x locked, turned, cropped, anchored),
    /// its movie and still made now.
    private func launchGeometry() -> XCUIApplication {
        let app = launchApp(opening: ["Geometry"], displays: "V1:1920x1080") { copies in
            try writeTestMedia(copies.appendingPathComponent("Geometry/media/clip.mov"), seconds: 4)
            try writeTestMedia(copies.appendingPathComponent("Geometry/media/still.png"), seconds: 0)
        }
        XCTAssertTrue(waitFor("isHittable == true", cueRow("Custom: moved, turned and cropped", in: app)))
        return app
    }

    /// The custom geometry fields, top left to bottom right.
    private static let fields = [
        "layer", "opacity", "x", "y", "scale-x", "scale-y", "rotation", "crop-top", "crop-bottom", "crop-left",
        "crop-right", "anchor-x", "anchor-y",
    ]

    private func field(_ id: String, in app: XCUIApplication) -> XCUIElement {
        app.textFields["inspector-geometry-\(id)"]
    }

    /// What every geometry field shows, read in one snapshot of the tab's fields.
    private func shown(in app: XCUIApplication) -> [String] {
        guard let snapshot = try? app.scrollViews["inspector-geometry-scroll"].snapshot() else { return [] }
        var values: [String: String] = [:]
        func walk(_ s: XCUIElementSnapshot) {
            if s.elementType == .textField { values[s.identifier] = s.value as? String ?? "" }
            s.children.forEach(walk)
        }
        walk(snapshot)
        return Self.fields.map { values["inspector-geometry-\($0)"] ?? "?" }
    }

    /// The geometry field with the keyboard focus ("" for none).
    private func focused(in app: XCUIApplication) -> String {
        let f = app.textFields.matching(NSPredicate(format: "hasKeyboardFocus == true")).firstMatch
        return f.exists ? f.identifier.replacingOccurrences(of: "inspector-geometry-", with: "") : ""
    }

    private func waitForShown(_ values: [String], in app: XCUIApplication, line: UInt = #line) {
        let same = NSPredicate { _, _ in MainActor.assumeIsolated { self.shown(in: app) == values } }
        let met = XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: same, object: nil)], timeout: 10)
        XCTAssertEqual(met, .completed, "the fields show \(values), not \(shown(in: app))", line: line)
    }

    func testGeometryFieldsTypedThumbnailDraggedEachUndoneAndRedone() {
        // About 70 steps, each waiting up to a second for the app to idle.
        executionTimeAllowance = 300
        let app = launchGeometry()
        selectCue("Custom: moved, turned and cropped", in: app)
        tabButton("Geometry", in: app).click()
        XCTAssertTrue(waitFor("isHittable == true", field("x", in: app)), "the custom fields are shown")
        let start = shown(in: app)
        XCTAssertEqual(start, ["top", "100", "320", "120", "2", "2", "15", "10", "0", "24", "0", "-40", "20"])

        // Each field typed in and committed with Return shows what the engine stored; Return keeps
        // the field (and so the playhead's GO) where it is.
        let typed = [
            "layer": "5", "opacity": "50", "x": "100", "y": "-50", "scale-x": "1.5", "scale-y": "3",
            "rotation": "45", "crop-top": "5", "crop-bottom": "6", "crop-left": "7", "crop-right": "8",
            "anchor-x": "10", "anchor-y": "-10",
        ]
        var states = [start]
        for id in Self.fields {
            let f = field(id, in: app)
            f.click()
            XCTAssertTrue(waitFor("hasKeyboardFocus == true", f), "\(id) takes the focus")
            f.typeKey("a", modifierFlags: .command)
            let value = typed[id] ?? ""
            f.typeText(value + "\r")
            XCTAssertTrue(waitFor("value == '\(value)'", f), "\(id) shows \(value), not \(text(f))")
            states.append(shown(in: app))
        }
        XCTAssertEqual(states.last?[4], "3", "the locked scale took the last value typed")
        XCTAssertEqual(states.last?[5], "3")

        // The focus leaves the field for the list, so the keys below act on the workspace, not on
        // the field's text.
        cueRow("Custom: moved, turned and cropped", in: app).click()

        // A drag on the thumbnail moves the picture: the position fields follow it.
        let thumbnail = app.images["inspector-geometry-thumbnail"]
        XCTAssertTrue(thumbnail.exists, "the thumbnail")
        let centre = thumbnail.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        centre.press(forDuration: 0.2, thenDragTo: centre.withOffset(CGVector(dx: 30, dy: 20)))
        XCTAssertTrue(waitFor("value != '100'", field("x", in: app)), "x follows the drag: \(shown(in: app))")
        XCTAssertTrue(waitFor("value != '-50'", field("y", in: app)), "y follows the drag: \(shown(in: app))")
        let moved = shown(in: app)
        XCTAssertGreaterThan(Double(moved[2]) ?? 0, 100, "right: \(moved)")
        XCTAssertLessThan(Double(moved[3]) ?? 0, -50, "down the stage: \(moved)")
        states.append(moved)

        // Undo takes each step back in turn, the drag as one; Redo puts each back.
        for back in states.dropLast().reversed() {
            app.typeKey("z", modifierFlags: .command)
            waitForShown(back, in: app)
        }
        for forward in states.dropFirst() {
            app.typeKey("z", modifierFlags: [.command, .shift])
            waitForShown(forward, in: app)
        }
    }

    /// At the window's least size the fields scroll: Tab goes from field to field in reading
    /// order, and a field further down comes into view as it takes the focus.
    func testGeometryTabOrderBringsAFieldBelowTheScrolledAreaIntoView() throws {
        let app = launchGeometry()
        selectCue("Custom: moved, turned and cropped", in: app)
        tabButton("Geometry", in: app).click()
        XCTAssertTrue(waitFor("isHittable == true", field("x", in: app)), "the custom fields are shown")
        // The window at its least size, the inspector at its least height: the divider above it
        // dragged down as far as it goes.
        let window = app.windows.firstMatch
        window.coordinate(withNormalizedOffset: CGVector(dx: 1, dy: 1)).withOffset(CGVector(dx: -2, dy: -2))
            .press(forDuration: 0.2, thenDragTo: window.coordinate(withNormalizedOffset: .zero))
        let divider = app.splitters.allElementsBoundByIndex.first { $0.frame.width > $0.frame.height }
        let top = try XCTUnwrap(divider, "the divider above the inspector").coordinate(
            withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        top.press(forDuration: 0.2, thenDragTo: top.withOffset(CGVector(dx: 0, dy: 400)))
        let scroll = app.scrollViews["inspector-geometry-scroll"]
        // A small field's frame as XCUITest reads it is a point or two taller than its bezel.
        func wholeInView(_ id: String) -> Bool {
            scroll.frame.insetBy(dx: -2, dy: -2).contains(field(id, in: app).frame)
        }
        XCTAssertFalse(wholeInView("anchor-y"), "the last row is below the fields' visible area")
        field("layer", in: app).click()
        var order = [focused(in: app)]
        for id in Self.fields.dropFirst() {
            app.typeKey(XCUIKeyboardKey.tab.rawValue, modifierFlags: [])
            order.append(focused(in: app))
            let inView = NSPredicate { _, _ in MainActor.assumeIsolated { wholeInView(id) } }
            XCTAssertEqual(
                XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: inView, object: nil)], timeout: 3),
                .completed,
                "\(id) is scrolled into view as it takes the focus: \(field(id, in: app).frame) in \(scroll.frame)")
        }
        XCTAssertEqual(order, Self.fields, "the Tab order")
    }

    /// The test card's barcode on virtual display `display` (nil while it shows none), read back
    /// from what the stage host draws there.
    private func frameShown(on display: String) async -> Int? {
        let probe = await engineRequest(["type": "display_probe", "display": display, "points": [[0.5, 0.5]]])
        return ((probe?["data"] as? [String: Any])?["frame"] as? NSNumber)?.intValue
    }

    /// `frameShown` read again, a fifth of a second apart, until `done` holds or ten seconds pass;
    /// the last reading.
    private func frameShown(on display: String, until done: (Int?) -> Bool) async -> Int? {
        let deadline = Date().addingTimeInterval(10)
        var shown = await frameShown(on: display)
        while !done(shown), Date() < deadline {
            try? await Task.sleep(nanoseconds: 200_000_000)
            shown = await frameShown(on: display)
        }
        return shown
    }

    /// A new workspace on two virtual displays has a stage on each; a Video cue made with Cmd-3,
    /// given a movie with the file chooser (T) and the second stage in its I/O tab, plays there on
    /// GO and is gone from it once stopped (QLab 5 manual, Video cues).
    func testVideoCueCreatedGivenAFileAndAStagePlaysOnGoAndStops() async throws {
        var movie = URL(fileURLWithPath: "/")
        let app = launchApp(opening: [], displays: "V1:1920x1080,V2:1280x720") { copies in
            movie = copies.appendingPathComponent("movies/clip.mov")
            try writeTestMedia(movie, seconds: 20)
        }
        let new = app.buttons["launcher-new"]
        XCTAssertTrue(waitFor("isHittable == true", new, timeout: 20), "the launcher, nothing open")
        new.click()
        let rows = app.outlines["cue-list"].outlineRows
        XCTAssertTrue(
            app.outlines["cue-list"].waitForExistence(timeout: 20),
            "a new workspace: \(app.dialogs.staticTexts.allElementsBoundByIndex.map { $0.value as? String ?? "" })")
        app.typeKey("3", modifierFlags: .command)
        XCTAssertTrue(waitFor("count == 1", rows), "a Video cue is made")
        let row = rows.element(boundBy: 0)
        row.click()

        // T opens the file chooser; the movie is typed in by its path.
        app.typeKey("t", modifierFlags: [])
        let panel = app.dialogs.firstMatch
        XCTAssertTrue(panel.waitForExistence(timeout: 10), "the file chooser opens")
        app.typeKey("g", modifierFlags: [.command, .shift])
        app.typeText(movie.path + "\r")
        app.typeKey(XCUIKeyboardKey.return.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("exists == false", panel), "the chooser closes")
        tabButton("I/O", in: app).click()
        let file = app.descendants(matching: .any)["inspector-io-file"]
        XCTAssertTrue(waitFor("value CONTAINS 'clip.mov'", file), "the I/O tab shows the file")

        let stage = app.popUpButtons["inspector-io-stage"]
        XCTAssertTrue(waitFor("value == 'Stage 1'", stage), "a new cue is on the first stage")
        stage.click()
        app.menuItems["Stage 2"].click()
        XCTAssertTrue(waitFor("value == 'Stage 2'", stage), "and now on the second")

        row.click()
        app.typeKey(XCUIKeyboardKey.space.rawValue, modifierFlags: [])
        XCTAssertTrue(cueStates("Running", in: app).firstMatch.waitForExistence(timeout: 10), "GO plays it")
        var shown = await frameShown(on: "V2", until: { $0 != nil })
        XCTAssertNotNil(shown, "the movie's frames are on the second display")
        let first = await frameShown(on: "V1")
        XCTAssertNil(first, "and not on the first")

        app.typeKey(XCUIKeyboardKey.escape.rawValue, modifierFlags: [])
        XCTAssertTrue(waitFor("count == 0", cueStates("Running", in: app)), "stopped")
        shown = await frameShown(on: "V2", until: { $0 == nil })
        XCTAssertNil(shown, "the picture is gone from the display")
    }
}
