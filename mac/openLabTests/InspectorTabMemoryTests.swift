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

    override func setUp() async throws {
        var snap = try TestSnapshots.show()
        let video = try TestSnapshots.show("Video")
        let extra = try XCTUnwrap(video["workspace"]?["cue_lists"]?.arrayValue?.first?["cues"]?.arrayValue)
        var ws = try XCTUnwrap(snap["workspace"])
        var lists = try XCTUnwrap(ws["cue_lists"]?.arrayValue)
        var first = lists[0]
        first.set("cues", .array((first["cues"]?.arrayValue ?? []) + extra))
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
        (window, vc, store) = (nil, nil, nil)
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

    func testTabMemoryTheOwnersOriginalFailingCase() throws {
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

    /// An Image cue is a Video cue to the memory (one type): a still's lack of Timing & Loops falls back
    /// without changing the choice, and a tab chosen on a still shows on a movie.
    func testTabMemoryAnImageCueSharesTheVideoTypesChoice() throws {
        try select(Self.video1, click: "timeLoops")
        try select(Self.still)
        XCTAssertEqual(shown, "basics", "no Timing & Loops on a still")
        try select(Self.video1)
        XCTAssertEqual(shown, "timeLoops", "the fallback left the choice alone")
        try select(Self.still, click: "geometry")
        try select(Self.video1)
        XCTAssertEqual(shown, "geometry")
    }

    /// T2: a still has no Timing & Loops tab, a movie has.
    func testImageCueHasNoTimingAndLoopsTab() throws {
        vc.setSelection([Self.still])
        XCTAssertFalse(tabs.contains("timeLoops"))
        XCTAssertTrue(tabs.contains("geometry"))
        vc.setSelection([Self.video1])
        XCTAssertTrue(tabs.contains("timeLoops"))
    }
}
