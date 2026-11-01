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
        XCTAssertEqual(p.icon, .statusNormal)
        XCTAssertTrue(p.details.contains("Output device: Built-in (OK)"))
    }

    func testLostDeviceIsAnAlarmShownByWordAndSymbolNotColourOnly() throws {
        let p = StatusPresentation.make(try status("status_device_lost"))
        XCTAssertEqual(p.level, .alarm)
        XCTAssertEqual(p.icon, .statusAlarm)
        XCTAssertTrue(p.summary.hasPrefix(loc("status.alarm")), p.summary)
        XCTAssertTrue(p.summary.contains(loc("status.device.lost")))
        // The disk fault and the dropout are in the details, not lost.
        XCTAssertTrue(p.details.contains(loc("status.disk.unhealthy")))
        XCTAssertTrue(p.details.contains { $0.contains("lost") })
        XCTAssertNotEqual(p.icon, StatusPresentation.make(try healthy()).icon)
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
        XCTAssertEqual(p.icon, .statusWarning)
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

    private func send(_ store: WorkspaceStore, xruns: Int) throws {
        var json = try Fixtures.json("server", "status")
        json["status"]?.set("xruns", .int(Int64(xruns)))
        store.receive(try ServerMessage.decode(try json.text()))
    }

    /// Applies a frame as the client applies one from the engine (one the real engine cannot be
    /// made to send on cue: a status with given counts, a `log_entry` past a lost push).
    private func push(_ store: WorkspaceStore, _ json: String) {
        guard let message = try? ServerMessage.decode(json) else { return XCTFail("does not decode: \(json)") }
        _ = store.engineClient(store.client, didReceive: message)
    }

    func testFooterFollowsTheStoresStatus() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertEqual(wc.footer.statusButton.title, loc("status.waiting"))
        try send(rig.store, xruns: 1)
        XCTAssertEqual(wc.footer.presentation.level, .normal, "the first status is the baseline")
        XCTAssertTrue(wc.footer.statusButton.title.contains("Dropouts 1"), "the count is always shown")
        try send(rig.store, xruns: 2)
        XCTAssertEqual(wc.footer.presentation.level, .warning, "a new dropout warns")
        try send(rig.store, xruns: 2)
        XCTAssertEqual(wc.footer.presentation.level, .normal, "no new dropouts since the last status")
        rig.store.receive(try ServerMessage.decode(try Fixtures.text("server", "status_device_lost")))
        let button = wc.footer.statusButton
        XCTAssertEqual(wc.footer.presentation.level, .alarm)
        XCTAssertTrue(button.title.hasPrefix(loc("status.alarm")))
        XCTAssertEqual(button.accessibilityLabel(), button.title)
        XCTAssertNotNil(button.image)
        try send(rig.store, xruns: 1)
        XCTAssertEqual(wc.footer.presentation.level, .normal, "recovers when the device is back")
    }

    /// The real engine's own status (about once a second of device time): its device, healthy.
    func testTheEnginesStatusShowsInTheFooter() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        try await rig.advance(1.5)
        await assertEventually("a status came") { rig.store.status != nil }
        XCTAssertEqual(rig.store.status?.device.name, "openLab Virtual Output")
        XCTAssertEqual(wc.footer.presentation.level, .normal)
    }

    func testOfflineEngineShowsAnAlarmAndNoStaleStatus() async throws {
        let rig = try await Rig.open()
        rig.client.stop()
        await assertEventually { rig.client.state != .connected }
        let wc = WorkspaceWindowController(store: rig.store)
        XCTAssertEqual(wc.footer.presentation, StatusPresentation.offline(), "not connected")
        XCTAssertTrue(wc.footer.statusButton.title.hasPrefix(loc("status.alarm")))
        XCTAssertTrue(wc.footer.statusButton.title.contains(loc("status.offline")))
        rig.client.start()
        await assertEventually { rig.client.state == .connected }
        await assertEventually { wc.footer.statusButton.title == loc("status.waiting") }
        try send(rig.store, xruns: 0)
        XCTAssertEqual(wc.footer.presentation.level, .normal)
        rig.client.stop()
        await assertEventually { wc.footer.presentation == StatusPresentation.offline() }
        XCTAssertNotEqual(wc.footer.presentation.icon, .statusNormal)
    }

    func testNewSnapshotClearsTheStatusUntilTheNextPush() async throws {
        let rig = try await Rig.open()
        let wc = WorkspaceWindowController(store: rig.store)
        try send(rig.store, xruns: 0)
        XCTAssertNotNil(rig.store.status)
        rig.client.requestSnapshot()  // the engine's snapshot
        await assertEventually { rig.store.status == nil }
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

    func testLogShowsEngineTailAndIgnoresLogNotices() throws {
        let rig = try Rig.offline()
        let log = LogWindowController(store: rig.store)
        push(rig.store, #"{"type":"log","level":"warn","message":"a notice, also a record"}"#)
        let entries = try tail()
        log.apply(tail: entries, dropped: 0)
        XCTAssertEqual(log.rows.count, entries.count, "`log` notices are the same records as log_entry")
        XCTAssertEqual(log.rows.first?.kind, entries.first?.kind.raw)
        XCTAssertFalse(try XCTUnwrap(log.rows.first).time.isEmpty)
        XCTAssertEqual(log.table.numberOfRows, log.visible.count)
    }

    func testLevelFilterShowsThatLevelAndWorse() throws {
        let rig = try Rig.offline()
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

    func testLogIsBoundedKeepingTheNewest() throws {
        let rig = try Rig.offline()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: try (0..<(LogWindowController.maxRows + 200)).map { try entry($0) }, dropped: 0)
        XCTAssertEqual(log.rows.count, LogWindowController.maxRows)
        XCTAssertEqual(log.rows.last?.message, "record \(LogWindowController.maxRows + 199)")
    }

    /// Refresh shows the real engine's records: it opened the show, then GO started Intro.
    func testRefreshFetchesTheTailFromTheEngine() async throws {
        let rig = try await Rig.open()
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.1)
        let log = LogWindowController(store: rig.store)
        await log.refresh()
        XCTAssertTrue(log.rows.contains { $0.message.hasPrefix("workspace opened") }, "\(log.rows.map(\.message))")
        let go = try XCTUnwrap(log.rows.first { $0.kind == "go" })
        XCTAssertTrue(go.message.contains("Intro"), go.message)
        XCTAssertTrue(log.rows.contains { $0.kind == "cue_start" })
    }

    /// The engine cannot be asked (the connection is gone): what is shown stays, and the window says so.
    func testFailedRefreshKeepsWhatIsShownAndSaysSo() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: [try entry(1)], dropped: 0)
        rig.client.stop()
        await log.refresh()
        XCTAssertEqual(log.rows.map(\.message), ["record 1"])
        XCTAssertEqual(log.note.stringValue, loc("log.tailFailed"))
    }

    private func tails(_ rig: Rig) -> Int {
        rig.wire.requestCount { if case .logTail = $0 { return true } else { return false } }
    }

    private func logEntry(_ seq: UInt64, _ message: String = "m", level: String = "info") -> String {
        #"{"type":"log_entry","seq":\#(seq),"entry":{"time_ms":1,"level":"\#(level)","kind":"cue_fail","message":"\#(message)"}}"#
    }

    /// M18: `log_entry` numbers every record; a jump means some were not pushed, so an open log
    /// window reads `log_tail` again (from the real engine). Consecutive numbers (and a closed
    /// window) read nothing. The pushes past the engine's own records stand for lost ones.
    func testALogEntryGapRereadsTheTailInAnOpenWindow() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        let base = rig.store.logSeq + 100
        push(rig.store, logEntry(base + 1))
        push(rig.store, logEntry(base + 5))  // window closed: nothing read
        await settle()
        XCTAssertEqual(tails(rig), 0)
        XCTAssertEqual(rig.store.logSeq, base + 5)
        log.showWindow(nil)
        defer { log.close() }
        push(rig.store, logEntry(base + 6))
        await settle()
        XCTAssertEqual(tails(rig), 0, "no gap")
        push(rig.store, logEntry(base + 9))
        await assertEventually { self.tails(rig) == 1 }
        await assertEventually("the real engine's records came") {
            log.rows.contains { $0.message.hasPrefix("workspace opened") }
        }
        // A broken or hostile engine's largest number must not overflow the gap check.
        push(rig.store, logEntry(UInt64.max))
        push(rig.store, logEntry(UInt64.max))
        await assertEventually { rig.store.logSeq == UInt64.max }
    }

    /// A burst of gaps is one re-read in flight and at most one after it, never one per gap.
    func testABurstOfLogGapsIsAtMostTwoTailReads() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        let base = rig.store.logSeq + 100
        // All in one turn, before the engine can answer the first re-read: the ten gaps after the
        // first fold into that one read (a read already out when they come gets one more after it).
        push(rig.store, logEntry(base + 1))
        push(rig.store, logEntry(base + 5))
        for seq in stride(from: base + 9, through: base + 45, by: 4) { push(rig.store, logEntry(seq)) }
        await assertEventually { self.tails(rig) == 1 }
        await assertEventually("the real engine's records came") { !log.rows.isEmpty }
        try await rig.advance()
        await settle()
        XCTAssertEqual(tails(rig), 1, "one read for the whole burst, never one per gap")
    }

    private func pushStatus(_ store: WorkspaceStore, logSeq: UInt64) {
        push(
            store,
            #"{"type":"status","status":{"cpu_percent":1,"engine_load":0.1,"xruns":0,"disk":{"streams":0,"underrun_frames":0,"healthy":true},"device":{"name":"d","state":"ok","sample_rate":48000,"block_frames":256,"channels":2,"latency_ms":9},"log_dropped":0,"log_seq":\#(logSeq),"network_output_disabled":false}}"#
        )
    }

    /// #312: an engine record pushed as `log_entry` shows in an open window without a refresh,
    /// once (a replay at or below a number already shown adds nothing). The real engine pushes the
    /// records GO makes.
    func testAPushedLogEntryAppearsInAnOpenLogWindow() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        XCTAssertFalse(log.rows.isEmpty)
        let reads = tails(rig)
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.1)
        await assertEventually("GO's record is pushed") { log.rows.contains { $0.kind == "cue_start" } }
        XCTAssertEqual(tails(rig), reads, "shown from the push, not a re-read")
        XCTAssertFalse(try XCTUnwrap(log.rows.last).time.isEmpty)
        let seq = rig.store.logSeq
        let count = log.rows.count
        push(rig.store, logEntry(seq, "a replay"))
        XCTAssertEqual(log.rows.count, count, "at or below a number already shown: nothing added")
        push(rig.store, logEntry(seq + 1, "cue 4 broke"))
        await assertEventually { log.rows.contains { $0.message == "cue 4 broke" } }
        // The engine also sends the same record as a `log` notice: not a row of its own.
        push(rig.store, #"{"type":"log","level":"info","message":"cue 4 broke"}"#)
        push(rig.store, logEntry(seq + 2, "cue 4 broke"))
        await assertEventually { log.rows.count == count + 2 }
        XCTAssertEqual(log.rows.filter { $0.message == "cue 4 broke" }.count, 2, "two records, not three rows")
    }

    /// #312: a push the hub dropped is noticed from `status.log_seq` alone (nothing follows it),
    /// and the re-read is one tail read.
    func testStatusLogSeqAboveTheLastHeardRereadsTheTailOnce() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        let heard = rig.store.logSeq + 100
        push(rig.store, logEntry(heard, "heard"))
        await settle()
        let before = tails(rig)
        pushStatus(rig.store, logSeq: heard)
        await settle()
        XCTAssertEqual(tails(rig), before, "status agrees with what was heard")
        pushStatus(rig.store, logSeq: heard + 1)
        await assertEventually { self.tails(rig) == before + 1 }
        await settle()
        XCTAssertEqual(tails(rig), before + 1)
    }

    private func tailMaxes(_ rig: Rig) -> [UInt32?] {
        rig.wire.sent.compactMap {
            if case .request(_, .logTail(let max)) = $0 { return .some(max) } else { return nil }
        }
    }

    /// #129: a gap reads only the missing records (sized to the gap), merged with those held.
    func testAGapReadsOnlyTheMissingRecords() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        let held = log.rows.count
        XCTAssertGreaterThan(held, 0)
        let last = rig.store.logSeq
        // The engine makes three records; the window hears only the newest (two pushes lost).
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.1)
        await assertEventually { rig.store.logSeq >= last + 3 }
        let maxesBefore = tailMaxes(rig).count
        push(rig.store, logEntry(rig.store.logSeq + 3, "a record past two lost ones"))
        await assertEventually { self.tailMaxes(rig).count == maxesBefore + 1 }
        XCTAssertEqual(tailMaxes(rig).last, 3, "sized to the gap")
        await settle()
        let messages = log.rows.map { "\($0.time) \($0.message)" }
        XCTAssertEqual(Set(messages).count, messages.count, "merged, none twice")
    }

    /// #312/#129: after an engine restart the numbers start over: pushes below the old tail's
    /// number still show, and the window reads the new run's tail.
    func testANewConnectionResetsTheWindowsRecordNumbers() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        XCTAssertFalse(log.rows.isEmpty)
        push(rig.store, logEntry(rig.store.logSeq + 50, "a late record of the old run"))
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome fixture")
        }
        rig.store.receive(.welcome(protocolVersion: 1, snapshot: snapshot))  // a new run's first frame
        XCTAssertTrue(log.rows.isEmpty, "the old run's records are gone")
        push(rig.store, logEntry(1, "first after restart"))
        await assertEventually { log.rows.contains { $0.message == "first after restart" } }
    }

    /// #312: a same-run resync keeps the record number (a gap after it still shows); a new
    /// connection starts over.
    func testLogSeqSurvivesASnapshotButNotAWelcome() throws {
        let rig = try Rig.offline()
        push(rig.store, logEntry(7))
        XCTAssertEqual(rig.store.logSeq, 7)
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome fixture")
        }
        rig.store.receive(.snapshot(snapshot))
        XCTAssertEqual(rig.store.logSeq, 7)
        rig.store.receive(.welcome(protocolVersion: 1, snapshot: snapshot))
        XCTAssertEqual(rig.store.logSeq, 0)
    }

    func testDroppedRecordsAreMentioned() throws {
        let rig = try Rig.offline()
        let log = LogWindowController(store: rig.store)
        log.apply(tail: [], dropped: 7)
        XCTAssertEqual(log.note.stringValue, String(format: loc("log.dropped"), 7))
    }

    /// App-made notes (relink results, key warnings, the edits-lost line) are local rows: shown
    /// where they happened, kept over a tail re-read and a new run; the engine's own `log`
    /// notices are not (they are records, shown from `log_entry`).
    func testAppNotesShowAndSurviveARereadAndANewRun() async throws {
        let rig = try await Rig.open()
        let log = LogWindowController(store: rig.store)
        let lost = "Edits lost: the workspace was reopened"
        rig.store.receive(.log(.warn, lost))  // the app's own note
        log.showWindow(nil)
        defer { log.close() }
        await log.refresh()
        XCTAssertTrue(log.rows.contains { $0.message.hasPrefix("workspace opened") })
        XCTAssertTrue(log.rows.contains { $0.message == lost })
        push(rig.store, #"{"type":"log","level":"warn","message":"engine notice"}"#)
        XCTAssertFalse(log.rows.contains { $0.message == "engine notice" }, "an engine notice is a record, not a note")
        // A record the engine makes (GO), then a note after it.
        guard case .success = await rig.store.go(list: TestSnapshots.mainList) else { return XCTFail("go") }
        try await rig.advance(0.1)
        await assertEventually { log.rows.contains { $0.kind == "cue_start" } }
        rig.store.receive(.log(.info, "after the GO"))
        let shown = log.rows.map(\.message)
        XCTAssertEqual(shown.last, "after the GO")
        // A full tail re-read replaces the records, not the notes.
        await log.refresh()
        XCTAssertEqual(log.rows.map(\.message), shown)
        // A note made before the window existed is there too.
        let late = LogWindowController(store: rig.store)
        XCTAssertTrue(late.rows.map(\.message).contains(lost))
        // A new run drops the old records and keeps the notes.
        guard case .welcome(_, let snapshot) = try ServerMessage.decode(try Fixtures.text("server", "welcome")) else {
            return XCTFail("welcome fixture")
        }
        rig.store.receive(.welcome(protocolVersion: 1, snapshot: snapshot))
        XCTAssertEqual(log.rows.map(\.message), [lost, "after the GO"])
    }

    /// The log window must never take the keys from the workspace (GO, transport keys).
    func testLogWindowCanNeverBecomeKey() throws {
        let log = LogWindowController(store: try Rig.offline().store)
        let window = try XCTUnwrap(log.window)
        XCTAssertTrue(window is NSPanel)
        XCTAssertFalse(window.canBecomeKey)
        XCTAssertFalse(window.canBecomeMain)
    }

    /// The log window goes with its workspace's window; none is made just to be closed.
    func testLogWindowClosesWithItsWorkspaceWindow() throws {
        let wc = WorkspaceWindowController(store: try Rig.offline().store)
        wc.window?.close()
        XCTAssertNil(wc.existingLogWindow, "closing does not make a log window")
        let other = WorkspaceWindowController(store: try Rig.offline().store)
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
