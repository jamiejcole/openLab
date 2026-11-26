// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// New Workspace from QLab File: the unsupported-cues step's buttons, the outputs step's pre-fill,
/// a refused file's alert, and one real-engine import of a QLab-saved workspace.
@MainActor
final class QlabImportTests: XCTestCase {
    private var engine: TestEngine?
    private var app: AppController?
    private var alerts: ScriptedAlerts?

    override func tearDown() async throws {
        app?.closeAllWindows()
        app = nil
        engine?.stop()
        engine = nil
    }

    // MARK: Helpers

    private func unsupported(_ id: String, _ number: String = "1") -> String {
        """
        {"qlab_id":"\(id)","number":"\(number)","name":"cue \(id)","qlab_type":"LightCue","reason":"cue_type"}
        """
    }

    private func patch(_ id: String, name: String, outputs: Int = 2, system: Bool = false) -> String {
        """
        {"qlab_id":"\(id)","name":"\(name)","outputs":\(outputs),"system":\(system),"device_uid":"u","device_name":"\(name)","device_outputs":\(outputs)}
        """
    }

    private func report(
        unsupported: [String] = [], patches: [String] = [], outputs: [String] = [], cues: Int = 10
    ) throws -> QlabReport {
        let json = """
            {"workspace_name":"Show","qlab_version":"5.0","cue_count":\(cues),"unsupported":[\(unsupported.joined(separator: ","))],
             "notes":[],"audio_patches":[\(patches.joined(separator: ","))],"video_outputs":[\(outputs.joined(separator: ","))],"stages":[]}
            """
        return try JSONDecoder().decode(QlabReport.self, from: Data(json.utf8))
    }

    private func device(_ name: String, channels: Int = 2) -> DeviceEntry {
        let json = """
            {"name":"\(name)","output_channels":\(channels),"is_default":false,"buffer_frames":512}
            """
        return try! JSONDecoder().decode(DeviceEntry.self, from: Data(json.utf8))
    }

    private func find(_ id: String, in view: NSView?) -> NSView? {
        guard let view else { return nil }
        if view.accessibilityIdentifier() == id { return view }
        for s in view.subviews { if let hit = find(id, in: s) { return hit } }
        return nil
    }

    // MARK: Unsupported-cues step

    func testAllDeleteIsDeleteAllWithOtherCuesDelete() throws {
        let r = try report(unsupported: [unsupported("a"), unsupported("b", "2")])
        let choices = QlabUnsupportedChoices(rows: r.unsupported)
        XCTAssertEqual(choices.primary, .deleteAll)
        XCTAssertEqual(choices.secondary, .leaveAll)
        let req = choices.request(pressing: choices.primary)
        XCTAssertEqual(req.other, .delete)
        XCTAssertTrue(req.cues.isEmpty)
    }

    func testAllPlaceholderSendsOtherCuesPlaceholder() throws {
        let r = try report(unsupported: [unsupported("a"), unsupported("b", "2")])
        var choices = QlabUnsupportedChoices(rows: r.unsupported)
        for row in r.unsupported { choices.set(.placeholder, for: row) }
        XCTAssertEqual(choices.primary, .leaveAll)
        let req = choices.request(pressing: choices.primary)
        XCTAssertEqual(req.other, .placeholder)
        XCTAssertTrue(req.cues.isEmpty)
    }

    func testMixedRowsReadContinueAndSendAPerCueList() throws {
        let r = try report(unsupported: [unsupported("a"), unsupported("b", "2"), unsupported("c", "3")])
        var choices = QlabUnsupportedChoices(rows: r.unsupported)
        choices.set(.placeholder, for: r.unsupported[1])
        XCTAssertEqual(choices.primary, .proceed)
        XCTAssertNil(choices.secondary)
        let req = choices.request(pressing: .proceed)
        XCTAssertEqual(req.other, .delete)
        XCTAssertEqual(req.cues, [QlabCueChoice(qlabId: "b", action: .placeholder)])
        let counts = choices.counts(pressing: .proceed)
        XCTAssertEqual(counts.deleted, 2)
        XCTAssertEqual(counts.placeholders, 1)
    }

    /// The window's own buttons follow the rows: Delete All (default) with Leave All beside it, then
    /// Continue alone once a row differs; Cancel hands back nothing.
    func testTheStepsButtonsFollowTheRows() throws {
        let r = try report(unsupported: [unsupported("a"), unsupported("b", "2")])
        let c = QlabImportController(report: r, devices: [], displays: [])
        XCTAssertEqual(c.step, .unsupported)
        XCTAssertEqual(c.primaryButton.title, loc("qlab.unsupported.deleteAll"))
        XCTAssertEqual(c.secondaryButton.title, loc("qlab.unsupported.leaveAll"))
        XCTAssertEqual(c.primaryButton.keyEquivalent, "\r")
        XCTAssertEqual(c.headingText, String(format: loc("qlab.unsupported.heading"), 2))
        c.window.contentView?.layoutSubtreeIfNeeded()
        let pop = try XCTUnwrap(find("qlab-unsupported-action-1", in: c.window.contentView) as? NSPopUpButton)
        pop.selectItem(at: 1)
        _ = pop.target?.perform(pop.action, with: pop)
        XCTAssertEqual(c.primaryButton.title, loc("qlab.unsupported.continue"))
        XCTAssertTrue(c.secondaryButton.isHidden)
        c.primaryButton.performClick(nil)
        XCTAssertEqual(c.step, .outputs)
        XCTAssertTrue(c.createButton.keyEquivalent == "\r")
        c.createButton.performClick(nil)
        let choice = try XCTUnwrap(c.result)
        // A tie goes to the placeholder, the choice that loses nothing.
        XCTAssertEqual(choice.cues, [QlabCueChoice(qlabId: "a", action: .delete)])
        XCTAssertEqual(choice.otherCues, .placeholder)
        XCTAssertEqual(choice.converted, 8)
        XCTAssertEqual(choice.deleted, 1)
        XCTAssertEqual(choice.placeholders, 1)
    }

    func testWithNothingUnsupportedTheOutputsStepComesFirst() throws {
        let c = QlabImportController(report: try report(), devices: [], displays: [])
        XCTAssertEqual(c.step, .outputs)
        XCTAssertTrue(c.createButton.keyEquivalent == "\r")
        XCTAssertFalse(c.backButton.isEnabled)
    }

    // MARK: Outputs step

    func testOutputsPrefillMatchesAndLeavesAnUnknownDeviceUnmapped() throws {
        let r = try report(
            patches: [patch("p1", name: "Built-in Output"), patch("p2", name: "Studio Interface", outputs: 8)],
            outputs: [
                #"{"qlab_id":"v1","name":"Screen 1","screen":"Studio Display","width":1920,"height":1080}"#,
                #"{"qlab_id":"v2","name":"Screen 2","screen":"Gone Display","width":1280,"height":720}"#,
            ])
        let displays = [StageDisplay(id: "D1", name: "Studio Display", width: 2560, height: 1440)]
        let outputs = QlabOutputChoices(report: r, devices: [device("Built-in Output")], displays: displays)
        XCTAssertEqual(outputs.audio[0].choice, .device("Built-in Output"))
        XCTAssertEqual(outputs.tag(audio: outputs.audio[0]), .matched)
        XCTAssertEqual(outputs.audio[1].choice, QlabAudioChoice.none)
        XCTAssertEqual(outputs.tag(audio: outputs.audio[1]), .notFound)
        XCTAssertEqual(outputs.video[0].choice, "D1")
        XCTAssertEqual(outputs.tag(video: outputs.video[0]), .matched)
        XCTAssertNil(outputs.video[1].choice)
        XCTAssertEqual(outputs.tag(video: outputs.video[1]), .notFound)
        XCTAssertEqual(
            outputs.audioRequest,
            [
                QlabPatchChoice(qlabId: "p1", mapping: PatchMapping(device: "Built-in Output", channels: [0, 1])),
                QlabPatchChoice(qlabId: "p2", mapping: nil),
            ])
        XCTAssertEqual(
            outputs.videoRequest,
            [QlabOutputChoice(qlabId: "v1", display: "D1"), QlabOutputChoice(qlabId: "v2", display: nil)])
    }

    /// A patch with more outputs than its device has maps the first ones only.
    func testAPatchWiderThanItsDeviceMapsTheChannelsTheDeviceHas() throws {
        let r = try report(patches: [patch("p", name: "Small", outputs: 8)])
        let outputs = QlabOutputChoices(report: r, devices: [device("Small", channels: 2)], displays: [])
        XCTAssertEqual(outputs.audioRequest.first?.mapping?.channels, [0, 1])
    }

    // MARK: Through the app

    private func startApp() async throws -> AppController {
        let engine = try await TestEngine.start()
        self.engine = engine
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: ManualScheduler())
        let app = AppController(session: session, scheduler: ManualScheduler())
        let alerts = ScriptedAlerts()
        app.alerts = alerts
        self.alerts = alerts
        self.app = app
        session.start()
        await assertEventually { session.appClient.state == .connected }
        return app
    }

    func testAFileThatIsNotAQlabWorkspaceShowsAnAlert() async throws {
        let app = try await startApp()
        let file = try XCTUnwrap(engine).folder.appendingPathComponent("plain.qlab5")
        try Data("not a property list".utf8).write(to: file)
        app.pickQlabURL = { file }
        var asked = false
        app.chooseQlabImport = { _, _, _ in
            asked = true
            return nil
        }
        await app.newWorkspaceFromQlab()
        XCTAssertEqual(try XCTUnwrap(alerts).errors, [loc("qlab.error.notQlab")])
        XCTAssertFalse(asked)
        XCTAssertTrue(app.windows.isEmpty)
    }

    func testCancellingTheStepsOpensNothing() async throws {
        let app = try await startApp()
        let file = try fixture("cue-types")
        app.pickQlabURL = { file }
        app.chooseQlabImport = { _, _, _ in nil }
        await app.newWorkspaceFromQlab()
        XCTAssertTrue(app.windows.isEmpty)
        XCTAssertEqual(try XCTUnwrap(alerts).errors, [])
    }

    /// The QLab-saved fixtures are local only (never committed): without them the test skips.
    private func fixture(_ name: String) throws -> URL {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent()
        let file = root.appendingPathComponent("engine/fixtures/qlab/\(name)/\(name).qlab5")
        guard FileManager.default.fileExists(atPath: file.path) else {
            throw XCTSkip("needs the local QLab-saved fixtures in engine/fixtures/qlab (not committed)")
        }
        return file
    }

    /// File > New Workspace from QLab File… with a workspace QLab saved: the new, unsaved workspace
    /// opens with each unsupported cue a red Memo placeholder, and the notes follow.
    func testImportingAQlabSavedWorkspaceOpensItWithRedPlaceholders() async throws {
        let app = try await startApp()
        let file = try fixture("cue-types")
        app.pickQlabURL = { file }
        var seen: QlabReport?
        app.chooseQlabImport = { report, devices, displays in
            seen = report
            var unsupported = QlabUnsupportedChoices(rows: report.unsupported)
            unsupported.setAll(.placeholder)
            let outputs = QlabOutputChoices(report: report, devices: devices, displays: displays)
            var choice = QlabImportChoice()
            choice.otherCues = unsupported.request(pressing: .leaveAll).other
            choice.audioPatches = outputs.audioRequest
            choice.videoOutputs = outputs.videoRequest
            choice.placeholders = report.unsupported.count
            choice.converted = Int(report.cueCount) - report.unsupported.count
            return choice
        }
        var notes: QlabNotesInfo?
        app.showQlabNotes = { notes = $0 }
        await app.newWorkspaceFromQlab()
        let report = try XCTUnwrap(seen)
        XCTAssertFalse(report.unsupported.isEmpty, "the fixture has cues openLab lacks")
        await assertEventually { app.windows.count == 1 }
        let wc = try XCTUnwrap(app.windows.values.first)
        await assertEventually { wc.store.model != nil && wc.cueList.listID != nil }
        XCTAssertNil(wc.store.document?.path, "the new workspace is unsaved")
        XCTAssertEqual(wc.store.document?.dirty, true)
        let model = try XCTUnwrap(wc.store.model)
        func all(_ cues: [Cue]) -> [Cue] { cues.flatMap { [$0] + all($0.children) } }
        let cues = model.cueLists.flatMap { all($0.cues) }
        let placeholders = cues.filter { $0.kindType == "memo" && $0.raw["color"]?.stringValue == "red" }
        XCTAssertGreaterThanOrEqual(placeholders.count, report.unsupported.count)
        for u in report.unsupported where !u.number.isEmpty {
            XCTAssertTrue(placeholders.contains { $0.number == u.number }, "placeholder for cue \(u.number)")
        }
        let info = try XCTUnwrap(notes)
        XCTAssertEqual(info.fileName, "cue-types.qlab5")
        XCTAssertEqual(info.placeholders, report.unsupported.count)
        XCTAssertTrue(info.plainText.contains(String(format: loc("qlab.notes.title"), "cue-types.qlab5")))
    }

    // MARK: Notes

    func testSaveNotesWritesPlainText() throws {
        let r = try report(unsupported: [unsupported("a")])
        let info = QlabNotesInfo(fileName: "Show.qlab5", report: r, converted: 9, placeholders: 0, deleted: 1)
        let sheet = QlabNotesController(info: info)
        let out = FileManager.default.temporaryDirectory.appendingPathComponent("qlab-notes-\(UUID().uuidString).txt")
        defer { try? FileManager.default.removeItem(at: out) }
        sheet.pickSaveURL = { _ in out }
        sheet.saveButton.performClick(nil)
        let text = try String(contentsOf: out, encoding: .utf8)
        XCTAssertTrue(text.hasPrefix(String(format: loc("qlab.notes.title"), "Show.qlab5")))
        XCTAssertTrue(text.contains(String(format: loc("qlab.notes.deleted"), 1)))
    }
}
