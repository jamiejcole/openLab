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

    func testLogSeqIsDecodedAndDefaultsToZeroWhenAbsent() throws {
        XCTAssertEqual(try status("status").logSeq, 42)
        // `healthy()` is a status from before the field.
        XCTAssertEqual(try healthy().logSeq, 0)
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
            case .logTail(let entries, _, _)? = ok.data
        else { throw XCTSkip("fixture is not a log_tail result") }
        return entries
    }

    private func entry(_ n: Int, level: String = "info") throws -> ShowLogEntry {
        let json =
            #"{"time_ms":\#(1_700_000_000_000 + n),"level":"\#(level)","kind":"engine","message":"record \#(n)"}"#
        return try JSONDecoder().decode(ShowLogEntry.self, from: Data(json.utf8))
    }

    func testLogShowsEngineTailAndIgnoresLogNotices() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        rig.engine.push(#"{"type":"log","level":"warn","message":"a notice, also a record"}"#)
        await settle()
        let entries = try tail()
        log.apply(tail: entries, dropped: 0)
        XCTAssertEqual(log.rows.count, entries.count, "`log` notices are the same records as log_entry")
        XCTAssertEqual(log.rows.first?.kind, entries.first?.kind.raw)
        XCTAssertFalse(try XCTUnwrap(log.rows.first).time.isEmpty)
        XCTAssertEqual(log.table.numberOfRows, log.visible.count)
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

    func testLogIsBoundedKeepingTheNewest() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: try (0..<(LogWindowController.maxRows + 200)).map { try entry($0) }, dropped: 0)
        XCTAssertEqual(log.rows.count, LogWindowController.maxRows)
        XCTAssertEqual(log.rows.last?.message, "record \(LogWindowController.maxRows + 199)")
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
        log.apply(tail: [try entry(1)], dropped: 0)
        rig.engine.garbleResults = true
        await log.refresh()
        XCTAssertEqual(log.rows.map(\.message), ["record 1"])
        XCTAssertEqual(log.note.stringValue, loc("log.tailFailed"))
    }

    /// M18: `log_entry` numbers every record; a jump means some were not pushed, so an open log
    /// window reads `log_tail` again. Consecutive numbers (and a closed window) read nothing.
    func testALogEntryGapRereadsTheTailInAnOpenWindow() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let log = LogWindowController(store: rig.store)
        func tails() -> Int { rig.engine.requestCount { if case .logTail = $0 { return true } else { return false } } }
        func entry(_ seq: Int) -> String {
            #"{"type":"log_entry","seq":\#(seq),"entry":{"time_ms":1,"level":"info","kind":"go","message":"m"}}"#
        }
        rig.engine.push(entry(1))
        rig.engine.push(entry(5))  // window closed: nothing read
        await settle()
        XCTAssertEqual(tails(), 0)
        XCTAssertEqual(rig.store.logSeq, 5)
        log.showWindow(nil)
        defer { log.close() }
        rig.engine.push(entry(6))
        await settle()
        XCTAssertEqual(tails(), 0, "no gap")
        rig.engine.push(entry(9))
        await assertEventually { tails() == 1 }
        // A broken or hostile engine's largest number must not overflow the gap check.
        let max = entry(9).replacingOccurrences(of: #""seq":9"#, with: #""seq":18446744073709551615"#)
        rig.engine.push(max)
        rig.engine.push(max)
        await assertEventually { rig.store.logSeq == UInt64.max }
    }

    /// A burst of gaps is one re-read in flight and at most one after it, never one per gap.
    func testABurstOfLogGapsIsAtMostTwoTailReads() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        func tails() -> Int { rig.engine.requestCount { if case .logTail = $0 { return true } else { return false } } }
        func push(_ seq: Int) {
            rig.engine.push(
                #"{"type":"log_entry","seq":\#(seq),"entry":{"time_ms":1,"level":"info","kind":"go","message":"m"}}"#)
        }
        rig.engine.holdResults = true
        push(1)
        push(5)
        await assertEventually { tails() == 1 }
        // Ten more gaps while that read is out: one more read after it, not ten.
        for seq in stride(from: 9, through: 45, by: 4) { push(seq) }
        await settle()
        XCTAssertEqual(tails(), 1)
        rig.engine.holdResults = false
        rig.engine.releaseResults()
        await assertEventually { tails() == 2 }
        await settle()
        XCTAssertEqual(tails(), 2)
    }

    private func pushEntry(_ rig: Rig, _ seq: Int, _ message: String, level: String = "info") {
        rig.engine.push(
            #"{"type":"log_entry","seq":\#(seq),"entry":{"time_ms":1,"level":"\#(level)","kind":"cue_fail","message":"\#(message)"}}"#
        )
    }

    private func pushStatus(_ rig: Rig, logSeq: Int) {
        rig.engine.push(
            #"{"type":"status","status":{"cpu_percent":1,"engine_load":0.1,"xruns":0,"disk":{"streams":0,"underrun_frames":0,"healthy":true},"device":{"name":"d","state":"ok","sample_rate":48000,"block_frames":256,"channels":2,"latency_ms":9},"log_dropped":0,"log_seq":\#(logSeq),"network_output_disabled":false}}"#
        )
    }

    /// #312: an engine record pushed as `log_entry` shows in an open window without a refresh,
    /// once (a replay at or below a number already shown adds nothing).
    func testAPushedLogEntryAppearsInAnOpenLogWindow() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await settle()
        pushEntry(rig, 1, "cue 3 broke")
        await assertEventually { log.rows.contains { $0.message == "cue 3 broke" } }
        XCTAssertEqual(log.rows.last?.kind, "cue_fail")
        XCTAssertFalse(try XCTUnwrap(log.rows.last).time.isEmpty)
        pushEntry(rig, 1, "cue 3 broke")
        pushEntry(rig, 2, "cue 4 broke")
        await assertEventually { log.rows.contains { $0.message == "cue 4 broke" } }
        XCTAssertEqual(log.rows.filter { $0.message == "cue 3 broke" }.count, 1)
        // The engine also sends the same record as a `log` notice: not a row of its own.
        rig.store.receive(.log(.info, "cue 4 broke"))
        pushEntry(rig, 3, "cue 4 broke")
        await assertEventually { log.rows.count == 3 }
        XCTAssertEqual(log.rows.filter { $0.message == "cue 4 broke" }.count, 2, "two records, not three rows")
    }

    /// #312: a push the hub dropped is noticed from `status.log_seq` alone (nothing follows it),
    /// and the re-read is one tail read.
    func testStatusLogSeqAboveTheLastHeardRereadsTheTailOnce() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        func tails() -> Int { rig.engine.requestCount { if case .logTail = $0 { return true } else { return false } } }
        pushEntry(rig, 4, "heard")
        await settle()
        let before = tails()
        pushStatus(rig, logSeq: 4)
        await settle()
        XCTAssertEqual(tails(), before, "status agrees with what was heard")
        pushStatus(rig, logSeq: 5)
        await assertEventually { tails() == before + 1 }
        await settle()
        XCTAssertEqual(tails(), before + 1)
    }

    private func tailEntry(_ n: Int) -> JSONValue {
        JSONValue.parse(
            #"{"time_ms":\#(n),"level":"info","kind":"engine","message":"record \#(n)"}"#)!
    }

    private func tailMaxes(_ rig: Rig) -> [UInt32?] {
        rig.engine.received.compactMap {
            if case .request(_, .logTail(let max)) = $0 { return .some(max) } else { return nil }
        }
    }

    /// #129: a gap reads only the missing records (sized to the gap), merged with those held.
    func testAGapReadsOnlyTheMissingRecords() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        rig.engine.logTailEntries = (1...10).map(tailEntry)
        rig.engine.logTailSeq = 10
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        XCTAssertEqual(log.rows.count, 10)
        rig.engine.logTailEntries = (1...13).map(tailEntry)
        rig.engine.logTailSeq = 13
        pushEntry(rig, 13, "record 13")  // 11 and 12 were not pushed
        await assertEventually { self.tailMaxes(rig).count == 2 }
        XCTAssertEqual(tailMaxes(rig).last, 3, "newest 13 minus last held 10")
        await assertEventually { log.rows.count == 13 }
        XCTAssertEqual(log.rows.map(\.message), (1...13).map { "record \($0)" }, "merged, none twice")
    }

    /// #312/#129: after an engine restart the numbers start over: pushes below the old tail's
    /// number still show, and the window reads the new run's tail.
    func testANewConnectionResetsTheWindowsRecordNumbers() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        rig.engine.logTailEntries = (1...50).map(tailEntry)
        rig.engine.logTailSeq = 50
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        XCTAssertEqual(log.rows.count, 50)
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome fixture")
        }
        rig.engine.logTailEntries = []
        rig.engine.logTailSeq = 0
        rig.store.receive(.welcome(protocolVersion: 1, snapshot: snapshot))
        XCTAssertTrue(log.rows.isEmpty, "the old run's records are gone")
        pushEntry(rig, 1, "first after restart")
        await assertEventually { log.rows.contains { $0.message == "first after restart" } }
    }

    /// #312: a same-run resync keeps the record number (a gap after it still shows); a new
    /// connection starts over.
    func testLogSeqSurvivesASnapshotButNotAWelcome() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        pushEntry(rig, 7, "m")
        await assertEventually { rig.store.logSeq == 7 }
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome fixture")
        }
        rig.store.receive(.snapshot(snapshot))
        XCTAssertEqual(rig.store.logSeq, 7)
        rig.store.receive(.welcome(protocolVersion: 1, snapshot: snapshot))
        XCTAssertEqual(rig.store.logSeq, 0)
    }

    func testDroppedRecordsAreMentioned() async throws {
        let rig = try Rig()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: [], dropped: 7)
        XCTAssertEqual(log.note.stringValue, String(format: loc("log.dropped"), 7))
    }

    /// App-made notes (relink results, key warnings, the edits-lost line) are local rows: shown
    /// where they happened, kept over a tail re-read and a new run; the engine's own `log`
    /// notices are not (they are records, shown from `log_entry`).
    func testAppNotesShowAndSurviveARereadAndANewRun() async throws {
        let rig = try Rig()
        let ok = await rig.connect()
        XCTAssertTrue(ok)
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        let lost = "Edits lost: the workspace was reopened"
        rig.store.receive(.log(.warn, lost))  // the app's own note
        XCTAssertEqual(log.rows.map(\.message), [lost])
        rig.engine.push(#"{"type":"log","level":"warn","message":"engine notice"}"#)
        await settle()
        XCTAssertEqual(log.rows.map(\.message), [lost], "an engine notice is a record, not a note")
        pushEntry(rig, 1, "record 1")
        await assertEventually { log.rows.count == 2 }
        rig.store.receive(.log(.info, "after record 1"))
        XCTAssertEqual(log.rows.map(\.message), [lost, "record 1", "after record 1"])
        // A full tail re-read replaces the records, not the notes.
        rig.engine.logTailEntries = [tailEntry(1)]
        rig.engine.logTailSeq = 1
        await log.refresh()
        XCTAssertEqual(log.rows.map(\.message), [lost, "record 1", "after record 1"])
        // A note made before the window existed is there too.
        let late = LogWindowController(store: rig.store)
        XCTAssertTrue(late.rows.map(\.message).contains(lost))
        // A new run drops the old records and keeps the notes.
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome fixture")
        }
        rig.engine.logTailEntries = []
        rig.engine.logTailSeq = 0
        rig.store.receive(.welcome(protocolVersion: 1, snapshot: snapshot))
        XCTAssertEqual(log.rows.map(\.message), [lost, "after record 1"])
    }

    /// The log window must never take the keys from the workspace (GO, transport keys).
    func testLogWindowCanNeverBecomeKey() throws {
        let log = LogWindowController(store: try Rig().store)
        let window = try XCTUnwrap(log.window)
        XCTAssertTrue(window is NSPanel)
        XCTAssertFalse(window.canBecomeKey)
        XCTAssertFalse(window.canBecomeMain)
    }

    /// The log window goes with its workspace's window; none is made just to be closed.
    func testLogWindowClosesWithItsWorkspaceWindow() throws {
        let wc = WorkspaceWindowController(store: try Rig().store)
        wc.window?.close()
        XCTAssertNil(wc.existingLogWindow, "closing does not make a log window")
        let other = WorkspaceWindowController(store: try Rig().store)
        other.showLog()
        let log = try XCTUnwrap(other.existingLogWindow)
        XCTAssertTrue(log.window?.isVisible == true)
        other.window?.close()
        XCTAssertFalse(log.window?.isVisible == true)
    }

    func testWindowMenuOpensTheLog() {
        let window = MainMenu.make().windows
        XCTAssertTrue(window.items.contains { $0.action == #selector(AppDelegate.showLog(_:)) })
        let file = MainMenu.make().bar.items.compactMap(\.submenu).first { $0.title == "File" }
        XCTAssertTrue(file?.items.contains { $0.action == #selector(AppDelegate.relinkMedia(_:)) } == true)
    }
}
