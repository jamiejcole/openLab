// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// ED-08, PB-02, CL-06: the General page of the workspace settings against the real engine. Each
/// test is one acceptance flow: the box clicked on the page, the user's action in the window, and
/// what the user then sees (the cue's number, the selection and playhead, the GO button).
@MainActor
final class SettingsGeneralTests: XCTestCase {
    typealias H = CueListHarness
    private let intro = H.intro
    private let fade = H.fade
    private func commit(_ sc: WorkspaceSettingsWindowController, _ field: NSTextField) {
        sc.controlTextDidEndEditing(Notification(name: NSControl.textDidEndEditingNotification, object: field))
    }

    var engine: TestEngine!

    override func setUp() async throws { engine = try await TestEngine.start() }
    override func tearDown() async throws { engine?.stop() }

    /// The settings window of `show` (a Demo whose cues `edit` renumbers), its page General.
    private func open(edit: ((URL) throws -> Void)? = nil) async throws -> (Rig, WorkspaceSettingsWindowController) {
        let rig = try await Rig.open("Demo", on: engine, edit: edit)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        sc.show(page: .general)
        return (rig, sc)
    }

    /// Clicks `box` as the user does and waits for the engine's answer to show on the page.
    private func click(_ box: NSButton, until switchOf: WorkspaceSwitch, _ rig: Rig, is on: Bool) async {
        box.performClick(nil)
        await assertEventually("\(switchOf) is \(on)") { rig.store.isOn(switchOf) == on }
    }

    private func numbers(_ rig: Rig) -> [ModelID: String?] {
        var out: [ModelID: String?] = [:]
        func walk(_ cues: [Cue]) {
            for cue in cues {
                out[cue.id] = cue.raw["number"]?.stringValue
                walk(cue.children)
            }
        }
        walk(rig.store.model?.cueLists.flatMap(\.cues) ?? [])
        return out
    }

    /// The Demo show with its Intro numbered 2, its Fade none and the group 3 (children unnumbered).
    private func numbered(_ url: URL) throws {
        try editShow(url.appendingPathComponent("Demo.openlab")) { ws in
            guard var lists = ws["cue_lists"]?.arrayValue, var cues = lists.first?["cues"]?.arrayValue else { return }
            for i in cues.indices {
                switch cues[i]["id"]?.stringValue {
                case H.intro.raw: cues[i].set("number", .string("2"))
                case H.group.raw:
                    cues[i].set("number", .string("3"))
                    if var kind = cues[i]["kind"], var children = kind["children"]?.arrayValue {
                        for c in children.indices { children[c].set("number", .null) }
                        kind.set("children", .array(children))
                        cues[i].set("kind", kind)
                    }
                default: break
                }
            }
            lists[0].set("cues", .array(cues))
            ws.set("cue_lists", .array(lists))
        }
    }

    // MARK: Auto-number

    /// Checklist: auto-number unticked → a new cue has no number.
    func testAutoNumberUntickedANewCueHasNoNumber() async throws {
        let (rig, sc) = try await open()
        let h = try await H(collapsed: false, rig: rig)
        let editor = CueEditor(store: rig.store, cueList: h.vc)
        XCTAssertEqual(sc.autoNumberCheck.state, .on, "a workspace numbers by default")
        await click(sc.autoNumberCheck, until: .autoNumberNewCues, rig, is: false)
        h.vc.select([intro])
        let added = await editor.add(.memo)
        let new = try XCTUnwrap(added)
        XCTAssertNil(try XCTUnwrap(numbers(rig)[new]), "no number")
        XCTAssertEqual(h.vc.node(new)?.cue.raw["number"], .null)
    }

    /// Checklist: auto-number ticked with step 1 between cues 2 and 3 → the new cue is 2.5; cues made
    /// by paste are not numbered.
    func testAutoNumberTickedBetweenCues2And3TheNewCueIs2Point5AndAPasteIsNotNumbered() async throws {
        let (rig, sc) = try await open(edit: numbered)
        let h = try await H(collapsed: false, rig: rig)
        let editor = CueEditor(store: rig.store, cueList: h.vc)
        XCTAssertEqual(sc.autoNumberCheck.state, .on)
        XCTAssertEqual(sc.autoNumberField.stringValue, "1", "the step")
        h.vc.select([intro])
        let added = await editor.add(.memo)
        let new = try XCTUnwrap(added)
        XCTAssertEqual(try XCTUnwrap(numbers(rig)[new]), "2.5")
        // A copy of it pasted after the Intro takes no number of its own.
        h.vc.select([new])
        let copied = await editor.copySelection()
        XCTAssertTrue(copied)
        h.vc.select([intro])
        let before = Set(numbers(rig).keys)
        await editor.paste()
        await assertEventually("the paste landed") { self.numbers(rig).count == before.count + 1 }
        let pasted = try XCTUnwrap(Set(numbers(rig).keys).subtracting(before).first)
        XCTAssertNil(try XCTUnwrap(numbers(rig)[pasted]), "a paste is not auto-numbered")
        // The step is typed on the page and the engine keeps it.
        sc.autoNumberField.stringValue = "5"
        commit(sc, sc.autoNumberField)
        await assertEventually("the step is 5") { rig.store.model?.settings["auto_number_increment"]?.doubleValue == 5 }
    }

    // MARK: Autosave

    /// Checklist: application preferences → autosave interval: typed on the General page (the
    /// interval is the workspace's, kept in the show), a number outside 5 seconds to 1 hour is
    /// refused and the engine's number shown again.
    func testTheAutosaveIntervalIsTypedOnThePageAndKeptByTheEngine() async throws {
        let (rig, sc) = try await open()
        XCTAssertEqual(sc.autosaveField.stringValue, "30")
        func type(_ text: String) {
            sc.autosaveField.stringValue = text
            commit(sc, sc.autosaveField)
        }
        type("90")
        await assertEventually("the engine kept 90") {
            rig.store.model?.settings["autosave_interval"]?.doubleValue == 90
        }
        type("2")
        await assertEventually("the refusal shows") {
            sc.messageLabel.stringValue == String(format: loc("settings.general.outOfRange"), 5.0, 3600.0)
        }
        XCTAssertEqual(sc.autosaveField.stringValue, "90")
        XCTAssertEqual(rig.store.model?.settings["autosave_interval"]?.doubleValue, 90)
    }

    // MARK: Lock playhead

    /// Checklist: "lock playhead to selection" on → selecting a row moves the playhead and moving the
    /// playhead moves the selection; off → they move apart.
    func testLockPlayheadOnTheyMoveTogetherOffTheyMoveApart() async throws {
        let (rig, sc) = try await open()
        let h = try await H(collapsed: false, rig: rig)
        let head: () -> ModelID? = { rig.store.playheads[ModelID(H.list)] }
        XCTAssertEqual(sc.lockPlayheadCheck.state, .on)
        h.vc.selectAsUser([fade])
        await assertEventually("the playhead went to the selection") { head() == self.fade }
        guard case .success = await rig.store.transport(.setPlayhead(list: ModelID(H.list), cue: intro)) else {
            return XCTFail("set_playhead")
        }
        await assertEventually("the selection followed the playhead") { h.vc.selectedIDs() == [self.intro] }

        await click(sc.lockPlayheadCheck, until: .lockPlayhead, rig, is: false)
        h.vc.selectAsUser([fade])
        try await rig.advance(0.05)
        XCTAssertEqual(head(), intro, "selecting moved only the selection")
        XCTAssertEqual(h.vc.selectedIDs(), [fade])
        guard case .success = await rig.store.transport(.setPlayhead(list: ModelID(H.list), cue: H.group)) else {
            return XCTFail("set_playhead")
        }
        await assertEventually("the playhead moved") { head() == H.group }
        try await rig.advance(0.05)
        XCTAssertEqual(h.vc.selectedIDs(), [fade], "the playhead moved only the playhead")

        await click(sc.lockPlayheadCheck, until: .lockPlayhead, rig, is: true)
        h.vc.selectAsUser([intro])
        await assertEventually("locked again: the playhead follows the selection") { head() == self.intro }
    }

    // MARK: Double-GO protection

    /// Checklist: a time above zero → after a GO the GO button shows a border until the time passes,
    /// and a GO inside it flashes the area and starts nothing; the first GO after it is accepted.
    func testDoubleGoProtectionBordersTheGoButtonAndAGoInsideItFlashesAndStartsNothing() async throws {
        let (rig, sc) = try await open()
        let wc = WorkspaceWindowController(store: rig.store)
        addTeardownBlock { @MainActor in wc.window?.close() }
        wc.onGo = { Task { _ = await rig.store.go(list: ModelID(H.list)) } }
        let head: () -> ModelID? = { rig.store.playheads[ModelID(H.list)] }
        sc.goIntervalField.stringValue = "1"
        commit(sc, sc.goIntervalField)
        await assertEventually("the interval is stored") { sc.model.minGoInterval == 1 }
        let masthead = wc.masthead
        XCTAssertFalse(masthead.goProtectionShown)

        masthead.goButton.performClick(nil)
        await assertEventually("the playhead moved on") { head() == self.fade }
        try await rig.advance(0.05)
        await assertEventually("the border shows") { masthead.goProtectionShown }
        XCTAssertEqual(masthead.goButton.layer?.borderWidth, 4)

        masthead.goButton.performClick(nil)
        await assertEventually("the area flashed") { masthead.goFlashes == 1 }
        try await rig.advance(0.2)
        XCTAssertEqual(head(), fade, "a GO inside the time started nothing and moved nothing")
        XCTAssertTrue(masthead.goProtectionShown, "the ignored GO neither ended nor extended it")

        try await rig.advance(1.0)
        await assertEventually("the border is gone") { !masthead.goProtectionShown }
        XCTAssertEqual(masthead.goButton.layer?.borderWidth, 0)
        masthead.goButton.performClick(nil)
        await assertEventually("a GO after the time is accepted") { head() == H.group }
        XCTAssertEqual(masthead.goFlashes, 1)
    }
}
