// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// The Target cue (CT-18; QLab 5 manual p. 686): it gives the cue it targets a temporary target, its
/// number typed in the New Target tab. Each acceptance flow of its issue on the real engine, through
/// the workspace window. The `TargetCue` test show: 1 a new Target cue, 2 a Target cue that points
/// Start cue 3 at Rain (5) instead of Thunder (4), 6 one aimed at Fade 7, 8 one whose new target has
/// no number (9), 10 one whose new target is gone.
@MainActor
final class TargetCueTests: XCTestCase {
    private static func id(_ n: Int) -> ModelID { ModelID(String(format: "00000781-0000-4000-8000-%012x", n)) }
    private static let fresh = id(1)
    private static let retarget = id(2)
    private static let start = id(3)
    private static let thunder = id(4)
    private static let rain = id(5)
    private static let atFade = id(6)
    private static let unnumbered = id(8)
    private static let gone = id(10)

    private var engine: TestEngine!
    private var rig: Rig!
    private var wc: WorkspaceWindowController!
    private var folder: URL?
    private var inspector: InspectorViewController { wc.inspector }
    private var list: ModelID { wc.cueList.listID ?? "" }

    override func setUp() async throws {
        engine = try await TestEngine.shared()
        let doc = try await engine.open("TargetCue", edit: { [unowned self] in self.folder = $0 })
        try await show(doc)
    }

    override func tearDown() async throws {
        wc?.window?.close()
        wc = nil
        rig = nil
    }

    /// A window on `doc`, its GO button wired as the app wires it.
    private func show(_ doc: ModelID) async throws {
        wc?.window?.close()
        rig = Rig(engine: engine, workspace: doc)
        guard await rig.connect() else { throw TestEngineError.notConnected }
        wc = WorkspaceWindowController(store: rig.store)
        let store = rig.store
        wc.onGo = { [unowned self] in
            let list = self.list
            Task { _ = await store.go(list: list) }
        }
        wc.window?.setFrame(NSRect(x: 0, y: 0, width: 1200, height: 800), display: false)
        wc.window?.makeKeyAndOrderFront(nil)
        await assertEventually { self.wc.cueList.roots.isEmpty == false }
    }

    // MARK: What the operator does

    private func select(_ id: ModelID) async {
        wc.cueList.selectAsUser([id])
        await assertEventually { self.inspector.cue?.id == id }
    }

    /// Selects `id` (the playhead follows) and presses GO.
    private func go(_ id: ModelID) async throws {
        await select(id)
        await assertEventually { self.rig.store.playheads[self.list] == id }
        wc.masthead.goButton.performClick(nil)
        try await rig.advance(0.05)
    }

    /// Types `text` in the New Target field and presses Return.
    private func typeNewTarget(_ text: String) throws {
        inspector.tabView.selectTabViewItem(withIdentifier: "newTarget")
        let field = inspector.newTargetField
        XCTAssertFalse(field.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(field.isEnabled)
        let window = try XCTUnwrap(wc.window)
        XCTAssertTrue(window.makeFirstResponder(field))
        let editor = try XCTUnwrap(field.currentEditor() as? NSTextView)
        editor.selectAll(nil)
        editor.insertText(text, replacementRange: editor.selectedRange())
        editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
    }

    // MARK: What the window shows

    private func cell(_ column: String, _ id: ModelID) -> CueCell? {
        let o = wc.cueList.outlineView
        guard let node = wc.cueList.node(id) else { return nil }
        let col = o.column(withIdentifier: NSUserInterfaceItemIdentifier(column))
        return o.view(atColumn: col, row: o.row(forItem: node), makeIfNecessary: true) as? CueCell
    }

    /// The row's state mark: its label and its tooltip (a broken cue's reasons).
    private func mark(_ id: ModelID) throws -> NSImageView { try XCTUnwrap(cell("status", id)).images[1] }

    private func playing(_ id: ModelID) -> Bool { shownState(id) == loc("cue.state.running") }

    private func tip(_ id: ModelID) throws -> String { try XCTUnwrap(try mark(id).toolTip) }

    /// The mark's tooltip while it is drawn, for waiting on it (nothing recorded while it is not yet).
    private func shownTip(_ id: ModelID) -> String? { cell("status", id)?.images[1].toolTip }

    private func shownState(_ id: ModelID) -> String? { cell("status", id)?.images[1].accessibilityLabel() }

    private func text(_ key: String, target: ModelID) -> String {
        let name = WarningRows.labeler(rig.store.model)(target)
        return loc(key).replacingOccurrences(of: "{target}", with: name)
    }

    private var tabs: [String] { inspector.tabView.tabViewItems.compactMap { $0.identifier as? String } }

    // MARK: Flows

    func testATargetCueCreatedHasANewTargetTabAndShowsBrokenSayingWhatIsMissing() async throws {
        // Made from the toolbar with the unnumbered Memo selected: a Memo cannot be retargeted, so it
        // starts with no target.
        await select(Self.id(9))
        let added = await wc.editor.add(.target)
        let made = try XCTUnwrap(added)
        await select(made)
        XCTAssertEqual(tabs, ["basics", "triggers", "newTarget"])
        await assertEventually { self.shownState(made) == loc("cue.state.broken") }
        let shown = try tip(made)
        XCTAssertTrue(shown.contains(loc("problem.no_target")), shown)
        XCTAssertTrue(shown.contains(loc("problem.no_new_target")), shown)
        // Its kind's icon and label in the list.
        XCTAssertEqual(try XCTUnwrap(cell("status", made)).images[4].accessibilityLabel(), loc("cue.type.target"))
    }

    func testATargetCueOnAStartCueWithCueFiveIsReadyAndListedLikeOtherActionCues() async throws {
        await select(Self.retarget)
        XCTAssertNotEqual(try mark(Self.retarget).accessibilityLabel(), loc("cue.state.broken"))
        XCTAssertEqual(rig.store.problems[Self.retarget] ?? [], [])
        XCTAssertEqual(
            try XCTUnwrap(cell("status", Self.retarget)).images[4].accessibilityLabel(), loc("cue.type.target"))
        // Named after what it acts on, as the other cues that act on a cue are; its target in the list.
        XCTAssertEqual(
            cell("name", Self.retarget)?.label.stringValue, "\(loc("cue.type.target").lowercased()) start Thunder")
        XCTAssertEqual(cell("target", Self.retarget)?.label.stringValue, "3")
        inspector.tabView.selectTabViewItem(withIdentifier: "newTarget")
        XCTAssertEqual(inspector.newTargetField.stringValue, "5")
        // Typing another cue's number sets it; emptying the field clears it (and the cue shows broken).
        try typeNewTarget("4")
        await assertEventually("the engine's echo") {
            self.rig.store.model?.cue(Self.retarget)?.raw["kind"]?["new_target"]?.stringValue == Self.thunder.raw
        }
        XCTAssertEqual(inspector.newTargetField.stringValue, "4")
        XCTAssertEqual(inspector.messageLabel.stringValue, "")
        try typeNewTarget("")
        await assertEventually { self.shownTip(Self.retarget)?.contains(loc("problem.no_new_target")) == true }
    }

    func testRunningTheTargetCueMakesTheStartCuePlayCueFiveAndDoesNotEditTheWorkspace() async throws {
        try await go(Self.retarget)
        XCTAssertEqual(rig.store.document?.dirty, false, "a temporary target is not an edit")
        XCTAssertFalse(wc.window?.isDocumentEdited ?? true)
        try await go(Self.start)
        await assertEventually("Rain plays") { self.playing(Self.rain) }
        XCTAssertFalse(playing(Self.thunder))
        // The list and inspector still show the Start cue's saved target.
        XCTAssertEqual(cell("target", Self.start)?.label.stringValue, "4")
        XCTAssertEqual(rig.store.document?.dirty, false)
    }

    func testSavedClosedAndReopenedTheStartCueActsOnItsSavedTarget() async throws {
        try await go(Self.retarget)
        guard case .success = await rig.store.save() else { return XCTFail("save") }
        let path = try XCTUnwrap(folder).appendingPathComponent("TargetCue.openlab").path
        let doc = rig.doc
        wc.window?.close()
        _ = await engine.app.send({ .close(id: $0, doc: doc, discardChanges: true) })
        let reopened = try await engine.open(path: path)
        try await show(reopened)
        await select(Self.start)
        XCTAssertEqual(cell("target", Self.start)?.label.stringValue, "4")
        try await go(Self.start)
        await assertEventually("Thunder plays") { self.playing(Self.thunder) }
        XCTAssertFalse(playing(Self.rain))
    }

    func testResettingTheStartCueOrResetAllAfterTheTargetCueRanReturnsItsSavedTarget() async throws {
        try await go(Self.retarget)
        guard case .success = await rig.store.transport(.reset(cue: Self.start)) else { return XCTFail("reset") }
        try await go(Self.start)
        await assertEventually("Thunder plays") { self.playing(Self.thunder) }
        XCTAssertFalse(playing(Self.rain))
        // Again, with Reset All.
        try await go(Self.retarget)
        guard case .success = await rig.store.transport(.resetAll) else { return XCTFail("reset all") }
        try await rig.advance(0.05)
        try await go(Self.start)
        await assertEventually("Thunder plays") { self.playing(Self.thunder) }
        XCTAssertFalse(playing(Self.rain))
    }

    func testATargetCueAimedAtAFadeAMissingOrAnUnnumberedCueIsBrokenWithAReasonOrRefused() async throws {
        await assertEventually { self.shownTip(Self.atFade) != nil }
        XCTAssertTrue(try tip(Self.atFade).contains(text("problem.target_wrong_kind", target: Self.id(7))))
        XCTAssertTrue(try tip(Self.unnumbered).contains(text("problem.new_target_unnumbered", target: Self.id(9))))
        XCTAssertTrue(try tip(Self.gone).contains(loc("problem.new_target_missing").components(separatedBy: "{")[0]))
        // Running a broken one changes nothing: the Start cue still plays its saved target.
        try await go(Self.atFade)
        try await go(Self.unnumbered)
        try await go(Self.gone)
        try await go(Self.start)
        await assertEventually("Thunder plays") { self.playing(Self.thunder) }
        // Typed: a number no cue has, and the cue it retargets, are refused on screen and nothing is sent.
        await select(Self.retarget)
        let log = CueEditingTests.SendLog(rig)
        for (typed, key) in [("42", "inspector.invalid.unknownTarget"), ("3", "inspector.invalid.newTargetIsTarget")] {
            try typeNewTarget(typed)
            XCTAssertEqual(inspector.messageLabel.stringValue, loc(key), typed)
            XCTAssertEqual(inspector.newTargetField.stringValue, "5", typed)
        }
        XCTAssertEqual(log.sent, 0)
        // A Target cue cannot be aimed at a Fade: its Target field refuses it.
        let fadeRetarget = Self.id(6)
        await select(fadeRetarget)
        XCTAssertEqual(
            InspectorEdit.targetRejection(
                of: try XCTUnwrap(rig.store.model?.cue(fadeRetarget)),
                target: try XCTUnwrap(rig.store.model?.cue(Self.id(7)))), .wrongTargetKind)
    }

    func testATargetCueRunWhileItsTargetPlaysChangesNothingThatIsPlaying() async throws {
        // The Start cue started Thunder; the Target cue then runs: Thunder plays on, nothing else starts.
        try await go(Self.start)
        await assertEventually("Thunder plays") { self.playing(Self.thunder) }
        try await go(Self.retarget)
        XCTAssertTrue(playing(Self.thunder))
        XCTAssertFalse(playing(Self.rain))
        // The next start of the Start cue acts on the new target.
        guard case .success = await rig.store.stopAll() else { return XCTFail("stop all") }
        try await rig.advance(0.05)
        try await go(Self.start)
        await assertEventually("Rain plays") { self.playing(Self.rain) }
    }

    func testInShowModeTheTargetCueStillRunsFromGoAndItsFieldsAreLocked() async throws {
        await wc.requestShowMode(true)
        await select(Self.retarget)
        inspector.tabView.selectTabViewItem(withIdentifier: "newTarget")
        XCTAssertFalse(inspector.newTargetField.isEnabled)
        XCTAssertFalse(inspector.targetField.isEnabled)
        try await go(Self.retarget)
        try await go(Self.start)
        await assertEventually("Rain plays") { self.playing(Self.rain) }
    }
}
