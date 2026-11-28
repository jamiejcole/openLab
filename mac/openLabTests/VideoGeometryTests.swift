// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// A Video cue's Geometry tab (VID-04; QLab 5 manual pp. 212-214), each flow of #644 on the real
/// engine with a stage host on a virtual display, on the Geometry test show: the operator's action
/// in the inspector, then what the inspector shows and what the stage shows (colours and the test
/// card's frame read back from the stage host).
extension VideoCueTests {
    // The Geometry test show (mac/TestShows/Geometry).
    static let mainStage: ModelID = "00000000-0000-0000-0000-0000000000f1"
    static let tallStage: ModelID = "00000000-0000-0000-0000-0000000000f4"
    static let geometryMovie: ModelID = "00000000-0000-0000-0000-0000000000b1"
    static func geometryCue(_ n: Int) -> ModelID { ModelID("00000000-0000-0000-0000-0000000000b\(n)") }

    static let red: [UInt8] = [255, 0, 0, 255]
    static let green: [UInt8] = [0, 255, 0, 255]

    var g: GeometryControls { wc.inspector.geometry }

    /// The frame and colours `stage` shows at `points` (fractions from its top left).
    func probe(_ stage: ModelID, _ points: [[Double]]) async -> (UInt16?, [[UInt8]]) {
        guard
            case .success(let ok) = await store.client.send({
                .request(id: $0, .stageProbe(stage: stage, points: points))
            }),
            case .stageProbe(let frame, let colours)? = ok.data
        else { return (nil, []) }
        return (frame, colours)
    }

    func colours(_ stage: ModelID, _ points: [[Double]]) async -> [[UInt8]] { await probe(stage, points).1 }

    /// Opens the Geometry show and shows cue `n`'s Geometry tab.
    func showGeometry(of n: Int, opened: Bool = false) async throws {
        if !opened { try await openVideoShow("Geometry") }
        let id = Self.geometryCue(n)
        click(id)
        let inspector = wc.inspector
        await soon("the inspector shows cue \(n)") { inspector.cue?.id == id }
        inspector.tabView.selectTabViewItem(withIdentifier: "geometry")
        wc.window?.layoutIfNeeded()
    }

    func kind(_ n: Int) -> JSONValue? { store.model?.cue(Self.geometryCue(n))?.raw["kind"] }

    /// A pair field (`translation`, `scale`, `anchor`) of cue `n`, whatever JSON number form it has.
    func pair(_ n: Int, _ key: String) -> CGPoint? {
        guard let x = kind(n)?[key]?["x"]?.doubleValue, let y = kind(n)?[key]?["y"]?.doubleValue else { return nil }
        return CGPoint(x: x, y: y)
    }

    /// The colours read, each named by the test card colour it is near (a movie's encode moves
    /// them a little): "red", "green", "blue", "yellow", "black", else "other".
    func named(_ colours: [[UInt8]]) -> [String] {
        colours.map { c in
            guard c.count >= 3 else { return "other" }
            let (r, g, b) = (Int(c[0]), Int(c[1]), Int(c[2]))
            let hi = { (v: Int) in v > 200 }
            let lo = { (v: Int) in v < 40 }
            switch (hi(r), hi(g), hi(b), lo(r), lo(g), lo(b)) {
            case (true, _, _, _, true, true): return "red"
            case (_, true, _, true, _, true): return "green"
            case (_, _, true, true, true, _): return "blue"
            case (true, true, _, _, _, true): return "yellow"
            case (_, _, _, true, true, true): return "black"
            default: return "other"
            }
        }
    }

    func choose(_ popup: NSPopUpButton, _ index: Int) {
        popup.selectItem(at: index)
        XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
    }

    func press(_ button: NSButton) { XCTAssertTrue(button.sendAction(button.action, to: button.target)) }

    // MARK: Flow 1

    /// Fill stage with style fit: the whole picture as large as the stage allows, shape kept (a
    /// wide picture on a tall stage: black above and below); fill: the stage covered, the overflow
    /// cut; stretch: the picture takes the stage's shape (the card's barcode spans its top).
    func testGeometryTabFillStageModeWithStyleFitFillAndStretch() async throws {
        try await showGeometry(of: 1)
        XCTAssertEqual(g.modePopup.titleOfSelectedItem, loc("inspector.geometry.mode.fill"))
        XCTAssertEqual(g.stylePopup.titleOfSelectedItem, loc("inspector.geometry.style.fit"))
        XCTAssertTrue(g.customRows.allSatisfy(\.isHidden), "custom fields only in custom mode")
        try await perform(.startSelected, on: Self.geometryCue(1))
        try await play(0.3)
        let pts: [[Double]] = [[0.25, 0.2], [0.25, 0.45]]
        var seen = await probe(Self.tallStage, pts)
        XCTAssertEqual(named(seen.1), ["black", "red"], "fit: bars above and below")
        XCTAssertNil(seen.0, "fit: the barcode is not at the stage's top")
        choose(g.stylePopup, 1)
        await soon("fill") { self.kind(1)?["fill_style"]?.stringValue == "fill" }
        seen = await probe(Self.tallStage, pts)
        XCTAssertEqual(named(seen.1), ["red", "red"], "fill: the stage covered")
        XCTAssertEqual(g.stylePopup.titleOfSelectedItem, loc("inspector.geometry.style.fill"))
        choose(g.stylePopup, 2)
        await soon("stretch") { self.kind(1)?["fill_style"]?.stringValue == "stretch" }
        seen = await probe(Self.tallStage, pts)
        XCTAssertEqual(named(seen.1), ["red", "red"])
        XCTAssertNotNil(seen.0, "stretch: the whole card on the stage, its barcode across the top")
    }

    // MARK: The thumbnail shows the cue's media

    /// The colours the thumbnail draws at fractions of its stage rectangle (from its top left), (red, green, blue, 255).
    private func thumbnailColours(_ points: [[Double]]) throws -> [[UInt8]] {
        let t = g.thumbnail
        let rep = try XCTUnwrap(t.bitmapImageRepForCachingDisplay(in: t.bounds))
        t.cacheDisplay(in: t.bounds, to: rep)
        let scale = Double(rep.pixelsWide) / Double(t.bounds.width)
        let box = t.stageRect
        let read = points.map { p -> [UInt8] in
            let x = (box.minX + box.width * p[0]) * scale
            let y = (box.minY + box.height * p[1]) * scale
            guard let c = rep.colorAt(x: Int(x), y: Int(y))?.usingColorSpace(.sRGB) else { return [] }
            return [UInt8(c.redComponent * 255), UInt8(c.greenComponent * 255), UInt8(c.blueComponent * 255), 255]
        }
        return read
    }

    /// Geometry tab of a movie cue and of a still: the stage drawing holds the cue's media (the first
    /// frame, the picture), not a plain tinted rectangle, so it shows the card's several colours.
    func testGeometryThumbnailShowsTheMediaOfAMovieAndOfAStill() async throws {
        try await showGeometry(of: 1)
        await soon("the movie's frame is read") { self.g.thumbnail.image != nil }
        // The picture is a small box in the middle of the tall stage: its centre is read.
        let centre = [[0.5, 0.5]]
        let movie = try thumbnailColours(centre)
        g.thumbnail.image = nil
        let plain = try thumbnailColours(centre)
        // Colour management on a hosted runner shifts the card's green a little (red 100 there).
        let isGreen = { (c: [[UInt8]]) in c.first.map { $0[1] > 200 && $0[0] < 140 && $0[2] < 40 } ?? false }
        let isYellow = { (c: [[UInt8]]) in c.first.map { $0[0] > 150 && $0[1] > 130 && $0[2] < 40 } ?? false }
        XCTAssertTrue(isYellow(plain), "without the frame it is the yellow rectangle: \(plain)")
        XCTAssertTrue(isGreen(movie), "the card's own colour at its centre: \(movie)")
        // A still, on another cue of the show.
        let id = Self.geometryCue(4)
        click(id)
        await soon("the inspector shows the still") { self.wc.inspector.cue?.id == id }
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "geometry")
        wc.window?.layoutIfNeeded()
        await soon("the still is read") { self.g.thumbnail.image != nil }
        XCTAssertTrue(isGreen(try thumbnailColours(centre)))
    }

    // MARK: Flow 2

    /// Custom mode: the picture at its own pixel size, and the position, scale with its lock,
    /// rotation, crop and anchor fields appear, each with a reset; the thumbnail shows the picture
    /// inside the stage's outline.
    func testGeometryTabCustomModeShowsItsFieldsWithResetsAndAThumbnail() async throws {
        try await showGeometry(of: 1)
        choose(g.modePopup, 1)
        await soon("custom") { self.kind(1)?["fill_stage"] == .bool(false) }
        await soon("the custom rows show") { self.g.customRows.allSatisfy { !$0.isHidden } }
        XCTAssertEqual(g.styleRow?.isHidden, true)
        XCTAssertEqual(
            [g.xField, g.yField, g.scaleXField, g.scaleYField, g.rotationField, g.anchorXField, g.anchorYField]
                .map(\.stringValue), ["0", "0", "1", "1", "0", "0", "0"])
        XCTAssertEqual(g.cropFields.map(\.stringValue), ["0", "0", "0", "0"])
        for b in [g.positionReset, g.scaleReset, g.rotationReset, g.cropReset, g.anchorReset, g.opacityReset] {
            XCTAssertFalse(b.isHidden || b.superview == nil || b.frame.isEmpty, "\(b.accessibilityIdentifier())")
        }
        XCTAssertEqual(g.scaleLock.state, .on, "the scale is locked")
        XCTAssertEqual(g.thumbnail.picture, CGSize(width: 288, height: 160))
        XCTAssertEqual(g.thumbnail.stage, CGSize(width: 1080, height: 1920))
        XCTAssertGreaterThan(g.thumbnail.stageRect.width, 20, "the stage's outline is drawn")
        try await perform(.startSelected, on: Self.geometryCue(1))
        try await play(0.3)
        let shown = await colours(Self.tallStage, [[0.43, 0.49], [0.25, 0.45]])
        XCTAssertEqual(named(shown), ["red", "black"], "its own 288 x 160 pixels in the middle")
        // Locked: a typed scale sets both; a reset puts it back.
        try typeInto(g.scaleXField, "2")
        await soon("scale 2 x 2") { self.pair(1, "scale") == CGPoint(x: 2, y: 2) }
        XCTAssertEqual(g.scaleYField.stringValue, "2")
        try typeInto(g.rotationField, "30")
        await soon("turned") { self.kind(1)?["rotation"]?.doubleValue == 30 }
        press(g.rotationReset)
        press(g.scaleReset)
        await soon("reset") { self.kind(1)?["rotation"]?.doubleValue == 0 && self.g.scaleXField.stringValue == "1" }
    }

    // MARK: Flow 3

    /// A value typed while the cue plays reaches the stage at once, and the picture runs on.
    func testGeometryValueTypedWhileTheCuePlaysReachesTheStageAtOnce() async throws {
        try await showGeometry(of: 3)
        try await perform(.startSelected, on: Self.geometryCue(3))
        try await play(1)
        let point: [[Double]] = [[0.25, 0.45]]
        let before = await colours(Self.tallStage, point)
        XCTAssertEqual(named(before), ["red"])
        try typeInto(g.opacityField, "0")
        await soon("gone from the stage at once") { await self.named(self.colours(Self.tallStage, point)) == ["black"] }
        try typeInto(g.opacityField, "100")
        await soon("back") { await self.named(self.colours(Self.tallStage, point)) == ["red"] }
        let frame = await shownFrame(Self.tallStage)
        XCTAssertEqual(Double(frame ?? 0), 25, accuracy: 3, "a second in: it played on, not restarted")
        XCTAssertEqual(status(Self.geometryCue(3)), loc("cue.state.running"))
    }

    // MARK: Flow 4

    /// Dragging the thumbnail moves the picture: the position fields follow during the drag, the
    /// cue holds where it was dropped, and the whole drag is one undo step.
    func testDraggingTheThumbnailMovesThePictureAndThePositionFieldsFollow() async throws {
        try await showGeometry(of: 6)
        let t = g.thumbnail
        XCTAssertTrue(t.canDrag)
        let k = t.stageRect.width / 1920
        let window = try XCTUnwrap(wc.window)
        func event(_ type: NSEvent.EventType, _ dx: CGFloat, _ dy: CGFloat) -> NSEvent {
            let centre = t.convert(NSPoint(x: t.bounds.midX + dx, y: t.bounds.midY + dy), to: nil)
            return NSEvent.mouseEvent(
                with: type, location: centre, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
        }
        t.mouseDown(with: event(.leftMouseDown, 0, 0))
        t.mouseDragged(with: event(.leftMouseDragged, 20 * k, 0))
        // Flipped view: a drag down the thumbnail moves the picture down the stage (y up).
        t.mouseDragged(with: event(.leftMouseDragged, 40 * k, 30 * k))
        XCTAssertEqual(g.xField.stringValue, "360", "the field follows the drag")
        XCTAssertEqual(g.yField.stringValue, "90")
        t.mouseUp(with: event(.leftMouseUp, 100 * k, 60 * k))
        await soon("dropped there") {
            self.pair(6, "translation") == CGPoint(x: 420, y: 60)
        }
        XCTAssertEqual(g.xField.stringValue, "420")
        await wc.editor.undo()
        await soon("one undo takes the whole drag back") {
            self.pair(6, "translation") == CGPoint(x: 320, y: 120)
        }
        await soon { self.g.xField.stringValue == "320" }
    }

    /// The selection moves to another Video cue in the middle of a thumbnail drag (the playhead
    /// following a continue): every move goes to the cue the drag began on, none to the other,
    /// and one undo takes the whole drag back on the first.
    func testAThumbnailDragKeepsToItsCueWhenTheSelectionMovesMidDrag() async throws {
        try await showGeometry(of: 6)
        let t = g.thumbnail
        let k = t.stageRect.width / 1920
        let window = try XCTUnwrap(wc.window)
        func event(_ type: NSEvent.EventType, _ dx: CGFloat, _ dy: CGFloat) -> NSEvent {
            let centre = t.convert(NSPoint(x: t.bounds.midX + dx, y: t.bounds.midY + dy), to: nil)
            return NSEvent.mouseEvent(
                with: type, location: centre, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
        }
        let seventh = pair(7, "translation")
        t.mouseDown(with: event(.leftMouseDown, 0, 0))
        t.mouseDragged(with: event(.leftMouseDragged, 20 * k, 0))
        click(Self.geometryCue(7))
        t.mouseDragged(with: event(.leftMouseDragged, 50 * k, 0))
        t.mouseUp(with: event(.leftMouseUp, 80 * k, 0))
        await soon("every move on cue 6") { self.pair(6, "translation") == CGPoint(x: 400, y: 120) }
        await sync()
        XCTAssertEqual(pair(7, "translation"), seventh, "none on cue 7")
        await wc.editor.undo()
        await soon("one undo, on cue 6") { self.pair(6, "translation") == CGPoint(x: 320, y: 120) }
        XCTAssertEqual(pair(7, "translation"), seventh)
    }

    // MARK: Flow 5

    /// Opacity: at 50 % the picture is half over the black below it, at 100 % opaque. (A file's
    /// own transparent pixels staying transparent is read back in the engine's tests, which make
    /// a picture with them.)
    func testOpacityFrom0To100BlendsThePictureOverWhatIsUnderIt() async throws {
        try await showGeometry(of: 7)
        XCTAssertEqual(g.opacityField.stringValue, "50")
        try await perform(.startSelected, on: Self.geometryCue(7))
        try await play(0.3)
        let point: [[Double]] = [[0.25, 0.3]]
        let half = await colours(Self.mainStage, point)
        let r = half.first?[0] ?? 0
        XCTAssertTrue((120...135).contains(r) && half.first?[1] == 0, "half red over black: \(half)")
        try typeInto(g.opacityField, "100")
        await soon("opaque") { await self.colours(Self.mainStage, point) == [Self.red] }
    }

    // MARK: Flow 6

    /// Two cues on one stage: the higher layer is on top though it started first; on the same layer
    /// the later-started is on top (p. 213).
    func testOnOneStageTheHigherLayerIsOnTopAndOnOneLayerTheLaterStarted() async throws {
        try await showGeometry(of: 6)
        press(g.rotationReset)
        press(g.cropReset)
        press(g.anchorReset)
        try typeInto(g.xField, "480")
        try typeInto(g.yField, "0")
        await soon("placed") {
            self.pair(6, "translation") == CGPoint(x: 480, y: 0)
                && self.kind(6)?["rotation"]?.doubleValue == 0 && self.kind(6)?["anchor"]?["x"]?.doubleValue == 0
        }
        // At this point cue 6 (layer top) shows its red, cue 8 (layer bottom, stretched) its green.
        let point: [[Double]] = [[0.65, 0.45]]
        try await perform(.startSelected, on: Self.geometryCue(6))
        try await play(0.2)
        try await perform(.startSelected, on: Self.geometryCue(8))
        try await play(0.2)
        let shown = await colours(Self.mainStage, point)
        XCTAssertEqual(named(shown), ["red"], "the top layer over the bottom one started later")
        try await showGeometry(of: 8, opened: true)
        XCTAssertEqual(g.layerField.stringValue, loc("inspector.geometry.layer.bottom"))
        try typeInto(g.layerField, loc("inspector.geometry.layer.top"))
        await soon("cue 8 on the top layer") { self.kind(8)?["layer"]?.doubleValue == 1000 }
        await soon("same layer: the later-started on top") {
            await self.named(self.colours(Self.mainStage, point)) == ["green"]
        }
    }

    // MARK: Flow 7

    /// The smooth box off: the scaled-up picture keeps hard pixel edges (a pixel on the edge
    /// between two quadrants is one of them, not a blend).
    func testTheSmoothBoxOffGivesAScaledUpPictureHardPixelEdges() async throws {
        try await showGeometry(of: 8)
        XCTAssertEqual(g.smoothCheck.state, .on)
        try await perform(.startSelected, on: Self.geometryCue(8))
        try await play(0.3)
        let edge: [[Double]] = [[0.5, 0.3]]
        let soft = await colours(Self.mainStage, edge)
        XCTAssertTrue((soft.first?[0] ?? 0) > 0 && (soft.first?[1] ?? 0) > 0, "smooth: blended \(soft)")
        g.smoothCheck.state = .off
        press(g.smoothCheck)
        await soon("smooth off") { self.kind(8)?["smooth"] == .bool(false) }
        await soon("a hard edge") {
            let c = await self.colours(Self.mainStage, edge)
            return c == [Self.red] || c == [Self.green]
        }
    }

    // MARK: Flow 8

    /// Values outside their ranges typed in the inspector are refused with a line naming the
    /// field and its range in words (no ids, no key names) and the field shows the cue's value
    /// again; the cue is unchanged. (OSC and the show file: the engine's tests.)
    func testValuesOutsideTheirRangesAreRefusedInTheInspector() async throws {
        try await showGeometry(of: 6)
        let before = kind(6)
        let message = wc.inspector.messageLabel
        for (field, text, says, shows) in [
            (g.opacityField, "150", "opacity", "100"), (g.layerField, "1001", "layer", "top"),
            (g.layerField, "2.5", "layer", "top"), (g.cropFields[0], "-5", "crop", "10"),
            (g.rotationField, "720", "rotation", "15"), (g.scaleXField, "1000", "scale", "2"),
            (g.xField, "99999999", "position", "320"), (g.anchorYField, "-70000", "anchor", "20"),
        ] {
            message.stringValue = ""
            try typeInto(field, text)
            XCTAssertEqual(message.stringValue, loc("inspector.geometry.range.\(says)"), text)
            XCTAssertEqual(field.stringValue, shows, "\(text): the field shows the cue's value")
        }
        message.stringValue = ""
        try typeInto(g.rotationField, "a lot")
        XCTAssertEqual(message.stringValue, loc("inspector.geometry.notANumber"))
        XCTAssertEqual(g.rotationField.stringValue, "15")
        await sync()
        XCTAssertEqual(kind(6), before, "nothing changed")
    }

    /// The app's ranges are the engine's: each end is taken and just past it refused, by the real
    /// engine, so a change to one side's range fails here.
    func testEachGeometryRangeIsTheEnginesAtBothEnds() async throws {
        try await showGeometry(of: 6)
        let id = Self.geometryCue(6)
        func takes(_ fields: [String: JSONValue]) async -> Bool {
            if case .success = await store.send(command: .setProperty(id, .kindFields(fields))) { return true }
            return false
        }
        typealias R = GeometryControls.FieldRange
        let pairs: [(String, R)] = [
            ("translation", GeometryControls.position), ("scale", GeometryControls.scaleRange),
            ("anchor", GeometryControls.anchorRange),
        ]
        for (key, r) in pairs {
            for (v, ok) in [
                (r.range.lowerBound, true), (r.range.upperBound, true), (r.range.upperBound + 0.5, false),
                (r.range.lowerBound - 0.5, false),
            ] {
                let taken = await takes([key: .object(["x": .double(v)])])
                XCTAssertEqual(taken, ok, "\(key) \(v)")
            }
        }
        let single: [(String, R, Double)] = [
            ("rotation", GeometryControls.rotationRange, 1), ("opacity", GeometryControls.opacityRange, 100),
        ]
        for (key, r, per) in single {
            for (v, ok) in [
                (r.range.lowerBound, true), (r.range.upperBound, true), (r.range.upperBound + 0.5, false),
                (r.range.lowerBound - 0.5, false),
            ] {
                let taken = await takes([key: .double(v / per)])
                XCTAssertEqual(taken, ok, "\(key) \(v)")
            }
        }
        let crop = GeometryControls.cropRange
        for (v, ok) in [
            (crop.range.lowerBound, true), (crop.range.upperBound, true), (crop.range.upperBound + 0.5, false),
            (-0.5, false),
        ] {
            let taken = await takes(["crop": .object(["top": .double(v)])])
            XCTAssertEqual(taken, ok, "crop \(v)")
        }
        let layer = GeometryControls.layerRange
        for (v, ok) in [(0.0, true), (layer.range.upperBound, true), (layer.range.upperBound + 1, false)] {
            let taken = await takes(["layer": .int(Int64(v))])
            XCTAssertEqual(taken, ok, "layer \(v)")
        }
    }

    // MARK: Flow 9

    /// Every geometry control of custom mode lies whole inside the fields' scroll view, the
    /// scroll view and the thumbnail inside the tab, the thumbnail not empty.
    func assertGeometryFits(_ what: String, line: UInt = #line) throws {
        let tab = try XCTUnwrap(wc.inspector.tabView.selectedTabViewItem?.view)
        tab.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(g.scroll)
        let doc = try XCTUnwrap(scroll.documentView)
        func inside(_ r: NSRect, _ b: NSRect) -> Bool {
            r.minX >= b.minX - 0.5 && r.maxX <= b.maxX + 0.5 && r.minY >= b.minY - 0.5 && r.maxY <= b.maxY + 0.5
        }
        for c in g.controls where c.superview != nil && !c.isHiddenOrHasHiddenAncestor {
            let r = c.convert(c.bounds, to: doc)
            let id = c.accessibilityIdentifier()
            XCTAssertTrue(inside(r, doc.bounds), "\(what): \(id) inside the fields: \(r)", line: line)
            XCTAssertGreaterThanOrEqual(r.width + 0.5, c.fittingSize.width, "\(what): \(id) not squeezed", line: line)
        }
        for v in [scroll, g.thumbnail] as [NSView] {
            let r = v.convert(v.bounds, to: tab)
            XCTAssertTrue(
                inside(r, tab.bounds), "\(what): \(v.accessibilityIdentifier()) inside the tab: \(r)", line: line)
        }
        XCTAssertGreaterThanOrEqual(g.thumbnail.frame.width, GeometryControls.thumbnailLeast - 0.5, what, line: line)
        XCTAssertGreaterThan(g.thumbnail.frame.height, 0, what, line: line)
    }

    /// A narrow inspector (the least window width, the sidebar shown or not) and the least
    /// window height: no geometry field is clipped by the panel, and the thumbnail stays, scaled
    /// down, in custom mode.
    func testANarrowInspectorClipsNoGeometryField() async throws {
        try await openVideoShow("Geometry")
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(x: 0, y: 0, width: 800, height: 500), display: false)
        try await showGeometry(of: 6, opened: true)
        try assertGeometryFits("least width")
        wc.footer.toggles[.sidebar]?.performClick(nil)
        window.layoutIfNeeded()
        try assertGeometryFits("least width, sidebar shown")
        window.setContentSize(NSSize(width: 400, height: 300))
        window.layoutIfNeeded()
        try assertGeometryFits("least size")
    }

    /// The thumbnail is an accessibility element with its name, so VoiceOver and the UI tests reach it.
    func testTheThumbnailIsAnAccessibilityElement() {
        let thumbnail = GeometryControls().thumbnail
        XCTAssertTrue(thumbnail.isAccessibilityElement())
        XCTAssertEqual(thumbnail.accessibilityRole(), .image)
        XCTAssertEqual(thumbnail.accessibilityLabel(), loc("inspector.geometry.thumbnail"))
    }

    /// A Video cue on no stage: the thumbnail draws no stage and cannot be dragged.
    func testACueOnNoStageShowsNoStageInTheThumbnail() async throws {
        try await showGeometry(of: 6)
        XCTAssertTrue(g.thumbnail.hasStage)
        let none = wc.inspector.videoIO
        wc.inspector.tabView.selectTabViewItem(withIdentifier: "io")
        none.stagePopup.selectItem(withTitle: loc("inspector.io.stageNone"))
        XCTAssertTrue(none.stagePopup.sendAction(none.stagePopup.action, to: none.stagePopup.target))
        await soon("on no stage") { self.kind(6)?["stage"] == .null || self.kind(6)?["stage"] == nil }
        await soon { !self.g.thumbnail.hasStage }
        XCTAssertFalse(g.thumbnail.canDrag)
    }

    /// A drag far past the position's range on a small thumbnail stops at the limit: nothing
    /// refused, no message.
    func testADragPastThePositionRangeStopsAtTheLimit() async throws {
        try await showGeometry(of: 6)
        let t = g.thumbnail
        let k = t.stageRect.width / 1920
        let window = try XCTUnwrap(wc.window)
        func event(_ type: NSEvent.EventType, _ dx: CGFloat) -> NSEvent {
            let at = t.convert(NSPoint(x: t.bounds.midX + dx, y: t.bounds.midY), to: nil)
            return NSEvent.mouseEvent(
                with: type, location: at, modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
        }
        wc.inspector.messageLabel.stringValue = ""
        t.mouseDown(with: event(.leftMouseDown, 0))
        t.mouseDragged(with: event(.leftMouseDragged, 100_000 * k))
        t.mouseUp(with: event(.leftMouseUp, 200_000 * k))
        XCTAssertEqual(g.xField.stringValue, "65536")
        await soon("held at the limit") { self.pair(6, "translation")?.x == 65536 }
        await sync()
        XCTAssertEqual(wc.inspector.messageLabel.stringValue, "", "nothing refused")
    }

    /// At the least window size a field below the rows shown (Anchor) scrolls into view when it
    /// takes the focus, as Tab gives it.
    func testAFieldTakingTheFocusScrollsIntoViewAtTheLeastSize() async throws {
        try await openVideoShow("Geometry")
        let window = try XCTUnwrap(wc.window)
        window.setContentSize(NSSize(width: 400, height: 300))
        try await showGeometry(of: 6, opened: true)
        let scroll = try XCTUnwrap(g.scroll)
        let doc = try XCTUnwrap(scroll.documentView)
        XCTAssertTrue(window.makeFirstResponder(g.anchorXField))
        window.layoutIfNeeded()
        let field = g.anchorXField.convert(g.anchorXField.bounds, to: doc)
        let visible = scroll.contentView.documentVisibleRect
        XCTAssertTrue(visible.insetBy(dx: -0.5, dy: -0.5).contains(field), "\(field) in \(visible)")
    }
}
