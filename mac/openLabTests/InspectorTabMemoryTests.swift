// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// The inspector's tab memory (QLab 5, owner-checked 2026-11-27): each cue type remembers the tab last
/// chosen on a cue of that type, for the session. The Demo workspace with the Video show's cues added,
/// offline: the tab view and the bar are what is read.
@MainActor
final class InspectorTabMemoryTests: XCTestCase {
    private static let audio1 = TestSnapshots.intro
    private static let audio2 = TestSnapshots.door
    private static let fade = TestSnapshots.fade
    private static let video1: ModelID = "00000000-0000-0000-0000-0000000000a1"
    private static let still: ModelID = "00000000-0000-0000-0000-0000000000a2"
    private var store: WorkspaceStore!
    private var vc: InspectorViewController!
    private var window: NSWindow!
    private var rig: Rig?

    override func setUp() async throws {
        var snap = try TestSnapshots.show()
        let video = try TestSnapshots.show("Video")
        let extra = try XCTUnwrap(video["workspace"]?["cue_lists"]?.arrayValue?.first?["cues"]?.arrayValue)
        var ws = try XCTUnwrap(snap["workspace"])
        var lists = try XCTUnwrap(ws["cue_lists"]?.arrayValue)
        var first = lists[0]
        // Absolute paths: with no show folder behind the rig, a relative one has no file to name.
        let placed = extra.map { cue -> JSONValue in
            var cue = cue
            if var kind = cue["kind"], let media = kind["media"]?.stringValue {
                kind.set("media", .string("/nonexistent/" + media))
                cue.set("kind", kind)
            }
            return cue
        }
        first.set("cues", .array((first["cues"]?.arrayValue ?? []) + placed))
        lists[0] = first
        ws.set("cue_lists", .array(lists))
        snap.set("workspace", ws)
        // The probe's answer: the first Video cue's file is a movie, the second's a still.
        func picture(_ id: ModelID, _ type: String) -> JSONValue {
            .object([
                "cue": .string(id.description), "file": .double(4),
                "picture": .object(["type": .string(type), "width": .int(640), "height": .int(360)]),
            ])
        }
        snap.set("media_durations", .array([picture(Self.video1, "movie"), picture(Self.still, "still")]))
        store = try Rig.offline(snap).store
        vc = InspectorViewController(store: store)
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = vc.view
    }

    override func tearDown() async throws {
        window?.close()
        (window, vc, store, rig) = (nil, nil, nil, nil)
    }

    private var shown: String? { vc.tabView.selectedTabViewItem?.identifier as? String }
    private var tabs: [String] { vc.tabBar.buttons.map(\.tabID) }

    /// Selects `id`, then clicks tab `tab` on the bar when given; the bar must agree with the tab view.
    private func select(_ id: ModelID, click tab: String? = nil) throws {
        vc.setSelection([id])
        if let tab {
            let button = try XCTUnwrap(vc.tabBar.buttons.first { $0.tabID == tab }, tab)
            button.performClick(nil)
        }
        XCTAssertEqual(vc.tabBar.buttons.filter(\.isChosen).map(\.tabID), [shown ?? ""], "the bar agrees")
    }

    func testTabMemoryAChoiceOnAVideoCueDoesNotCarryToAudioCues() throws {
        try select(Self.audio1)
        XCTAssertEqual(shown, "basics")
        try select(Self.video1, click: "timeLoops")
        try select(Self.audio1)
        XCTAssertEqual(shown, "basics")
    }

    func testTabMemoryEachTypeKeepsItsOwnLevelsAndGeometry() throws {
        try select(Self.audio1, click: "levels")
        try select(Self.video1, click: "geometry")
        try select(Self.audio1)
        XCTAssertEqual(shown, "levels")
        try select(Self.video1)
        XCTAssertEqual(shown, "geometry")
    }

    func testTabMemoryAnotherCueOfTheTypeOpensOnTheChoice() throws {
        try select(Self.audio1, click: "timeLoops")
        try select(Self.audio2)
        XCTAssertEqual(shown, "timeLoops")
        try select(Self.audio2, click: "io")
        try select(Self.audio1)
        XCTAssertEqual(shown, "io")
    }

    func testTabMemoryAFadeKeepsItsOwnCurve() throws {
        try select(Self.audio1, click: "timeLoops")
        try select(Self.fade, click: "curve")
        try select(Self.audio1)
        XCTAssertEqual(shown, "timeLoops")
        try select(Self.fade)
        XCTAssertEqual(shown, "curve")
    }

    /// The owner's 7-step case on the real engine: the Demo show with the Video show's stages and cues.
    func testTabMemoryTheOwnersOriginalFailingCase() async throws {
        let opened = try await Rig.open(
            "Demo",
            edit: { folder in
                let source = try XCTUnwrap(TestEngine.testShows).appendingPathComponent("Video/Video.openlab")
                let video = try XCTUnwrap(JSONValue.parse(try String(contentsOf: source, encoding: .utf8)))
                let vws = try XCTUnwrap(video["workspace"])
                try editShow(folder.appendingPathComponent("Demo.openlab")) { ws in
                    for key in ["stages", "routes"] { if let v = vws[key] { ws.set(key, v) } }
                    guard var list = ws["cue_lists"]?.arrayValue?.first else { return }
                    list.set(
                        "cues",
                        .array(
                            (list["cues"]?.arrayValue ?? [])
                                + (vws["cue_lists"]?.arrayValue?.first?["cues"]?.arrayValue ?? [])))
                    ws.set("cue_lists", .array([list]))
                }
            })
        rig = opened
        store = opened.store
        vc = InspectorViewController(store: store)
        window.contentView = vc.view
        try select(Self.audio1)
        try select(Self.video1)
        XCTAssertEqual(shown, "basics")
        try select(Self.video1, click: "geometry")
        try select(Self.audio1)
        XCTAssertEqual(shown, "basics")
        try select(Self.video1)
        XCTAssertEqual(shown, "geometry")
        try select(Self.audio1, click: "levels")
        try select(Self.video1)
        XCTAssertEqual(shown, "geometry")
    }

    // MARK: The Geometry thumbnail's read

    /// With Geometry hidden no file is read; choosing the tab starts the read, and choosing another
    /// Video cue cancels the one for the cue no longer shown (its result is never drawn).
    func testTheGeometryPictureIsReadOnlyWhileTheTabIsShownAndAStaleReadIsCancelled() throws {
        try select(Self.video1)
        XCTAssertNil(vc.mediaFrameTask, "Geometry is not shown: nothing is read")
        XCTAssertNil(vc.geometry.shownMedia)
        try select(Self.video1, click: "geometry")
        let first = try XCTUnwrap(vc.mediaFrameTask, "the tab brings the read")
        try select(Self.still)
        XCTAssertTrue(first.isCancelled, "the read for the cue no longer shown is cancelled")
        XCTAssertNotNil(vc.mediaFrameTask)
        try select(Self.audio1)
        XCTAssertNil(vc.mediaFrameTask, "no Video cue shown: no read stands")
    }

    /// A volume the engine says is not answering is not opened for a picture.
    func testTheGeometryPictureIsNotReadFromAStalledVolume() throws {
        XCTAssertEqual(
            store.receive(
                try ServerMessage.decode(
                    #"{"type":"warnings","warnings":[{"type":"notice","code":"media_volume_not_responding","message":"volume does not answer"}]}"#
                )), .applied)
        try select(Self.video1, click: "geometry")
        XCTAssertNil(vc.mediaFrameTask)
    }

    /// An Image cue is a Video cue to the memory: a tab chosen on a movie shows on a still and back.
    func testTabMemoryAnImageCueSharesTheVideoTypesChoice() throws {
        try select(Self.video1, click: "timeLoops")
        try select(Self.still)
        XCTAssertEqual(shown, "timeLoops", "the still has the Timing & Loops tab too")
        try select(Self.still, click: "geometry")
        try select(Self.video1)
        XCTAssertEqual(shown, "geometry")
    }

    /// T2 (reversed by the owner 2026-11-27): a still has the Timing & Loops tab, as a movie has, and
    /// its fade-in and fade-out fields.
    func testImageCueHasTheTimingAndLoopsTabWithItsFadeFields() throws {
        vc.setSelection([Self.still])
        XCTAssertTrue(tabs.contains("timeLoops"))
        XCTAssertTrue(tabs.contains("geometry"))
        let panel = vc.timeLoops
        XCTAssertFalse(panel.fadeInField.isHidden)
        XCTAssertFalse(panel.fadeOutField.isHidden)
        XCTAssertTrue(panel.fadeInField.isEnabled)
        vc.setSelection([Self.video1])
        XCTAssertTrue(tabs.contains("timeLoops"))
        XCTAssertTrue(panel.fadeInField.isHiddenOrHasHiddenAncestor, "a movie's fades are on its timeline")
    }
}
