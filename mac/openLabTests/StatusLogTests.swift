// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// DX-03 (status in the footer) and DX-02 (the log window).
@MainActor
final class StatusLogTests: XCTestCase {
    private func status(_ fixture: String) throws -> SystemStatus {
        guard case .status(let s) = try ServerMessage.decode(try Fixtures.text("server", fixture)) else {
            throw XCTSkip("\(fixture) is not a status message")
        }
        return s
    }

    private func healthy() throws -> SystemStatus {
        try JSONDecoder().decode(
            SystemStatus.self,
            from: Data(
                #"""
                {"cpu_percent":8.2,"engine_load":0.1,"xruns":0,
                 "disk":{"streams":2,"underrun_frames":0,"healthy":true},
                 "device":{"name":"Built-in","state":"ok","sample_rate":48000,"block_frames":256,"channels":2,"latency_ms":9.0},
                 "log_dropped":0,"network_output_disabled":false}
                """#.utf8))
    }

    // MARK: Status mapping

    func testNoStatusYetIsNeutral() {
        let p = StatusPresentation.make(nil)
        XCTAssertEqual(p.level, .normal)
        XCTAssertEqual(p.summary, loc("status.waiting"))
    }

    func testHealthyStatusIsNormalWithCPU() throws {
        let p = StatusPresentation.make(try healthy())
        XCTAssertEqual(p.level, .normal)
        XCTAssertEqual(p.summary, "Audio OK  CPU 8%  Dropouts 0")
        XCTAssertEqual(p.symbol, "checkmark.circle")
        XCTAssertTrue(p.details.contains("Output device: Built-in (OK)"))
    }

    func testLostDeviceIsAnAlarmShownByWordAndSymbolNotColourOnly() throws {
        let p = StatusPresentation.make(try status("status_device_lost"))
        XCTAssertEqual(p.level, .alarm)
        XCTAssertEqual(p.symbol, "exclamationmark.octagon.fill")
        XCTAssertTrue(p.summary.hasPrefix(loc("status.alarm")), p.summary)
        XCTAssertTrue(p.summary.contains(loc("status.device.lost")))
        // The disk fault and the dropout are in the details, not lost.
        XCTAssertTrue(p.details.contains(loc("status.disk.unhealthy")))
        XCTAssertTrue(p.details.contains { $0.contains("lost") })
        XCTAssertNotEqual(p.symbol, StatusPresentation.make(try healthy()).symbol)
    }

    func testUnhealthyDiskAloneIsAnAlarm() throws {
        var json = try JSONValue.parse(Fixtures.text("server", "status"))?["status"] ?? .null
        json["disk"]?.set("healthy", .bool(false))
        let s = try JSONDecoder().decode(SystemStatus.self, from: Data(try json.text().utf8))
        let p = StatusPresentation.make(s)
        XCTAssertEqual(p.level, .alarm)
        XCTAssertTrue(p.summary.contains(loc("status.disk.unhealthy")))
    }

    func testDropoutsAndHighLoadAreWarnings() throws {
        // The fixture is a healthy device with one dropout.
        let p = StatusPresentation.make(try status("status"))
        XCTAssertEqual(p.level, .warning)
        XCTAssertEqual(p.symbol, "exclamationmark.triangle.fill")
        XCTAssertTrue(p.summary.hasPrefix(loc("status.warning")))
        var json = try JSONValue.parse(Fixtures.text("server", "status"))?["status"] ?? .null
        json.set("xruns", .int(0))
        json.set("engine_load", .double(0.92))
        let s = try JSONDecoder().decode(SystemStatus.self, from: Data(try json.text().utf8))
        XCTAssertTrue(StatusPresentation.make(s).summary.contains(loc("status.load.high")))
    }

    func testAnUnknownDeviceStateIsAWarningNamingIt() throws {
        var json = try JSONValue.parse(Fixtures.text("server", "status"))?["status"] ?? .null
        json.set("xruns", .int(0))
        json["device"]?.set("state", .string("sleeping"))
        let s = try JSONDecoder().decode(SystemStatus.self, from: Data(try json.text().utf8))
        let p = StatusPresentation.make(s)
        XCTAssertEqual(p.level, .warning)
        XCTAssertTrue(p.summary.contains("sleeping"))
    }

    private func send(_ rig: Rig, xruns: Int) throws {
        var json = try Fixtures.json("server", "status")
        json["status"]?.set("xruns", .int(Int64(xruns)))
        rig.store.receive(try ServerMessage.decode(try json.text()))
    }

    func testFooterFollowsTheStoresStatus() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertEqual(wc.footer.statusButton.title, loc("status.waiting"))
        try send(rig, xruns: 1)
        XCTAssertEqual(wc.footer.presentation.level, .normal, "the first status is the baseline")
        XCTAssertTrue(wc.footer.statusButton.title.contains("Dropouts 1"), "the count is always shown")
        try send(rig, xruns: 2)
        XCTAssertEqual(wc.footer.presentation.level, .warning, "a new dropout warns")
        try send(rig, xruns: 2)
        XCTAssertEqual(wc.footer.presentation.level, .normal, "no new dropouts since the last status")
        rig.store.receive(try ServerMessage.decode(try Fixtures.text("server", "status_device_lost")))
        let button = wc.footer.statusButton
        XCTAssertEqual(wc.footer.presentation.level, .alarm)
        XCTAssertTrue(button.title.hasPrefix(loc("status.alarm")))
        XCTAssertEqual(button.accessibilityLabel(), button.title)
        XCTAssertNotNil(button.image)
        try send(rig, xruns: 1)
        XCTAssertEqual(wc.footer.presentation.level, .normal, "recovers when the device is back")
    }

    func testOfflineEngineShowsAnAlarmAndNoStaleStatus() async throws {
        let rig = try Rig()
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertEqual(wc.footer.presentation, StatusPresentation.offline(), "not connected yet")
        XCTAssertTrue(wc.footer.statusButton.title.hasPrefix(loc("status.alarm")))
        XCTAssertTrue(wc.footer.statusButton.title.contains(loc("status.offline")))
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        XCTAssertEqual(wc.footer.statusButton.title, loc("status.waiting"))
        try send(rig, xruns: 0)
        XCTAssertEqual(wc.footer.presentation.level, .normal)
        rig.client.stop()
        await assertEventually { wc.footer.presentation == StatusPresentation.offline() }
        XCTAssertNotEqual(wc.footer.presentation.symbol, "checkmark.circle")
    }

    func testNewSnapshotClearsTheStatusUntilTheNextPush() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let wc = WorkspaceWindowController(store: rig.store)
        try send(rig, xruns: 0)
        XCTAssertNotNil(rig.store.status)
        rig.store.receive(try ServerMessage.decode(try Fixtures.text("server", "welcome")))
        XCTAssertNil(rig.store.status)
        XCTAssertEqual(wc.footer.statusButton.title, loc("status.waiting"))
    }

    // MARK: Log window

    private func tail() throws -> [ShowLogEntry] {
        guard case .result(_, .ok(let ok)) = try ServerMessage.decode(try Fixtures.text("server", "result_log_tail")),
            case .logTail(let entries, _)? = ok.data
        else { throw XCTSkip("fixture is not a log_tail result") }
        return entries
    }

    private func entry(_ n: Int, level: String = "info") throws -> ShowLogEntry {
        let json =
            #"{"time_ms":\#(1_700_000_000_000 + n),"level":"\#(level)","kind":"engine","message":"record \#(n)"}"#
        return try JSONDecoder().decode(ShowLogEntry.self, from: Data(json.utf8))
    }

    func testLogShowsEngineTailThenStoreEntriesNewestAtTheBottom() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        rig.store.receive(.log(.warn, "heard just now"))
        let entries = try tail()
        log.apply(tail: entries, dropped: 0)
        XCTAssertEqual(log.rows.count, entries.count + 1)
        XCTAssertEqual(log.rows.last?.message, "heard just now")
        XCTAssertEqual(log.rows.first?.kind, entries.first?.kind.raw)
        XCTAssertFalse(try XCTUnwrap(log.rows.first).time.isEmpty)
        // A new message lands at the bottom without a refresh.
        rig.store.receive(.log(.error, "boom"))
        XCTAssertEqual(log.rows.last?.message, "boom")
        XCTAssertEqual(log.table.numberOfRows, log.visible.count)
    }

    func testAnEntryInBothTheTailAndTheStoreIsShownOnce() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        let e = try entry(1)
        rig.store.receive(.log(e.level, e.message))
        log.apply(tail: [e], dropped: 0)
        XCTAssertEqual(log.rows.count, 1)
    }

    func testLevelFilterShowsThatLevelAndWorse() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        log.apply(
            tail: [
                try entry(1, level: "debug"), try entry(2, level: "info"), try entry(3, level: "warn"),
                try entry(4, level: "error"),
            ],
            dropped: 0)
        XCTAssertEqual(log.visible.count, 4)
        for (index, expected) in [(1, 3), (2, 2), (3, 1), (0, 4)] {
            log.filterPopup.selectItem(at: index)
            log.filterChanged()
            XCTAssertEqual(log.visible.count, expected, "filter \(index)")
            XCTAssertEqual(log.table.numberOfRows, expected)
        }
        log.filterPopup.selectItem(at: 3)
        log.filterChanged()
        XCTAssertEqual(log.visible.map(\.level), [.error])
        XCTAssertEqual(log.visible.first?.level.title, "ERROR", "the level is a word, not just a colour")
    }

    func testIdenticalEntriesAreNotMergedAway() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        rig.store.receive(.log(.warn, "device lost"))
        rig.store.receive(.log(.warn, "device lost"))
        log.apply(tail: [], dropped: 0)
        XCTAssertEqual(log.rows.map(\.message), ["device lost", "device lost"])
        // The tail holds one of them: it cancels one, the other still shows.
        let held = try entry(1)
        rig.store.receive(.log(held.level, held.message))
        rig.store.receive(.log(held.level, held.message))
        log.apply(tail: [held], dropped: 0)
        XCTAssertEqual(log.rows.filter { $0.message == held.message }.count, 2)
        // The same text again, after the fetch, is a new event.
        rig.store.receive(.log(held.level, held.message))
        XCTAssertEqual(log.rows.filter { $0.message == held.message }.count, 3)
    }

    func testLogIsBoundedKeepingTheNewest() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: try (0..<(LogWindowController.maxRows + 200)).map { try entry($0) }, dropped: 0)
        XCTAssertEqual(log.rows.count, LogWindowController.maxRows)
        XCTAssertEqual(log.rows.last?.message, "record \(LogWindowController.maxRows + 199)")
        for i in 0..<50 { rig.store.receive(.log(.info, "live \(i)")) }
        XCTAssertEqual(log.rows.count, LogWindowController.maxRows)
        XCTAssertEqual(log.rows.last?.message, "live 49")
    }

    func testRefreshFetchesTheTailFromTheEngine() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        rig.engine.logTailEntries = [
            try XCTUnwrap(
                JSONValue.parse(#"{"time_ms":1700000000123,"level":"info","kind":"go","message":"GO 1"}"#))
        ]
        let log = LogWindowController(store: rig.store)
        await log.refresh()
        XCTAssertEqual(log.rows.map(\.message), ["GO 1"])
        XCTAssertEqual(log.rows.first?.kind, "go")
    }

    func testFailedRefreshKeepsWhatIsShownAndSaysSo() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let log = LogWindowController(store: rig.store)
        rig.store.receive(.log(.warn, "kept"))
        rig.engine.garbleResults = true
        await log.refresh()
        XCTAssertEqual(log.rows.map(\.message), ["kept"])
        XCTAssertEqual(log.note.stringValue, loc("log.tailFailed"))
    }

    func testDroppedRecordsAreMentioned() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: [], dropped: 7)
        XCTAssertEqual(log.note.stringValue, String(format: loc("log.dropped"), 7))
    }

    func testWindowMenuOpensTheLog() {
        let window = MainMenu.make().windows
        XCTAssertTrue(window.items.contains { $0.action == #selector(AppDelegate.showLog(_:)) })
        let file = MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == "File" }
        XCTAssertTrue(file?.items.contains { $0.action == #selector(AppDelegate.relinkMedia(_:)) } == true)
    }
}
