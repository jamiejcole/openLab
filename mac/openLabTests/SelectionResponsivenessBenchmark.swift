// SPDX-License-Identifier: GPL-3.0-or-later

import ObjectiveC
import QuartzCore
import XCTest
import os

@testable import openLab

/// How long a cue selection takes to reach the screen: a click on a row, Down once, and Down pressed
/// about 30 times a second through 50 cues, on a 200-cue workspace of mixed kinds in a 1280x800 window
/// with the inspector shown, on the real engine. A benchmark, not a correctness test: it measures with
/// the host's clock, asserts no timing, and runs only with OPENLAB_BENCH=1 (TEST_RUNNER_OPENLAB_BENCH=1
/// on the xcodebuild line); otherwise it reports a skip. The window must be on screen and the app
/// active, since only a shown window draws.
///
/// "Drawn" is what the app painted into the window's layers (the screen shows it at the next refresh):
/// the cue's row view ran its background drawing while selected (the highlight) and while marked as
/// the playhead (the arrow), the masthead's standby field and the inspector's name field ran their
/// drawing holding the cue's text. Each is timed inside that drawing, through hooks this class puts on
/// those classes' drawing methods for its run only. The engine's head is the store's `engineHeads`,
/// sampled each time the main run loop goes to sleep. Main-thread busy is the time the main run loop
/// was not asleep; the handler is the time inside the input's own dispatch. Intervals and events go to
/// the `dev.openlab.bench` signpost log for Instruments.
///
/// Results are printed (lines starting `BENCH`) and written as JSON to OPENLAB_BENCH_OUT when set.
@MainActor
final class SelectionResponsivenessBenchmark: XCTestCase {
    var app: AppController!
    var engine: TestEngine!
    private let sessionScheduler = ManualScheduler()
    var doc: ModelID = "none"
    var wc: WorkspaceWindowController { app.windows[doc]! }
    var store: WorkspaceStore { wc.store }
    var list: ModelID { wc.cueList.listID! }
    private var log: DrawLog!
    private var hooks: [MethodHook] = []
    private var observers: [CFRunLoopObserver] = []
    private let signposter = OSSignposter(subsystem: "dev.openlab.bench", category: "selection")
    private var unsettledReports = 0

    override func setUp() async throws {
        guard ProcessInfo.processInfo.environment["OPENLAB_BENCH"] == "1" else {
            throw XCTSkip("the selection benchmark runs only with OPENLAB_BENCH=1")
        }
        executionTimeAllowance = 900
        engine = try await TestEngine.start()
        NotesKeeper.folder = engine.folder.appendingPathComponent("kept-notes", isDirectory: true)
        let url = engine.infoURL
        let session = EngineSession(
            transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load(from: url) },
            scheduler: sessionScheduler)
        app = AppController(session: session, scheduler: ManualScheduler())
        app.keyWindow = { [unowned self] in app.windows[doc]?.window }
        session.start()
        await assertEventually { session.appClient.state == .connected }
        guard case .success(let new) = await session.newWorkspace() else { return XCTFail("new workspace") }
        doc = new
        await assertEventually {
            self.app.windows[new]?.store.model != nil && self.app.windows[new]?.cueList.listID != nil
        }
        app.isTextEditing = { [unowned self] in (wc.window?.firstResponder as? NSText)?.isEditable == true }
    }

    override func tearDown() async throws {
        for h in hooks { h.remove() }
        hooks = []
        for o in observers { CFRunLoopRemoveObserver(CFRunLoopGetMain(), o, .commonModes) }
        observers = []
        log = nil
        NotesNoticePanel.closeAll()
        app?.closeAllWindows()
        app = nil
        engine?.stopInBackground()
    }

    // MARK: The workspace

    /// 200 rows: 105 Audio cues on 40 files of 4 to 40 s, 5 Groups of 4 Audio cues each, 25 Fades on
    /// Audio cues, 20 Waits, 10 Memos, 5 Video cues (a movie; Audio cues when this Mac cannot make
    /// one) and 10 Network cues; a third of them with notes. One undo step, as a paste would make it.
    private func buildWorkspace() async throws -> [ModelID] {
        let dir = engine.folder.appendingPathComponent("bench-media", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var wavs: [URL] = []
        for i in 0..<40 {
            let url = dir.appendingPathComponent("tone-\(i).wav")
            try writeWAV(url, seconds: Double(4 + (i * 37) % 37), rate: 48_000)
            wavs.append(url)
        }
        var movie: URL? = dir.appendingPathComponent("movie.mov")
        do { try writeTestMedia(movie!, seconds: 4) } catch {
            print("BENCH no test movie on this Mac (\(error)); Audio cues stand in for the Video cues")
            movie = nil
        }
        let model = try XCTUnwrap(store.model)
        let patch = model.patches.first
        let stage = model.stages.first?.id
        let networkPatch = model.networkPatches.first.map { $0 }
        var commands: [Command] = []
        var number = 0
        var audioIDs: [ModelID] = []
        func cue(_ kind: NewCueKind, media: URL? = nil, target: ModelID? = nil, parent: ModelID? = nil, index: Int)
            throws -> ModelID
        {
            number += 1
            let id = ModelID(UUID().uuidString.lowercased())
            var made = try XCTUnwrap(
                kind.cueJSON(id: id, patch: patch, networkPatch: networkPatch, target: target, stage: stage),
                "\(kind) cue")
            made.set("number", .string("\(number)"))
            made.set("name", .string("Bench \(kind.rawValue) cue \(number)"))
            if number % 3 == 0 {
                made.set(
                    "notes",
                    .string(
                        "Notes for cue \(number): stand by lights and sound.\nSecond line of notes for the operator."))
            }
            if let media, var k = made["kind"] {
                k.set("media", .string(CueEditor.mediaPath(media)))
                made.set("kind", k)
            }
            commands.append(Command.addCue(made, list: list, parent: parent, index: index))
            return id
        }
        // A repeating pattern of kinds down the list: mostly Audio, the others spread through it.
        let pattern: [NewCueKind] = [
            .audio, .audio, .fade, .audio, .wait, .audio, .audio, .memo, .audio, .fade, .audio, .network,
            .audio, .wait, .audio, .fade, .audio, .audio, .wait, .audio,
        ]
        var top = 0
        var made = 0
        var videos = 0
        var groups = 0
        while made < 200 {
            let slot = top % pattern.count
            if top % 9 == 4, groups < 5, made + 5 <= 200 {
                let group = try cue(.group, index: top)
                for c in 0..<4 {
                    let id = try cue(.audio, media: wavs[(made + c) % wavs.count], parent: group, index: c)
                    audioIDs.append(id)
                }
                groups += 1
                made += 5
            } else if top % 13 == 6, videos < 5 {
                if let movie {
                    _ = try cue(.video, media: movie, index: top)
                } else {
                    audioIDs.append(try cue(.audio, media: wavs[made % wavs.count], index: top))
                }
                videos += 1
                made += 1
            } else {
                switch pattern[slot] {
                case .audio:
                    audioIDs.append(try cue(.audio, media: wavs[made % wavs.count], index: top))
                case .fade:
                    _ = try cue(.fade, target: audioIDs.last, index: top)
                default:
                    _ = try cue(pattern[slot], index: top)
                }
                made += 1
            }
            top += 1
        }
        let all = try XCTUnwrap(Command.transaction(commands))
        guard case .success = await store.send(command: all) else { throw TestEngineError.notConnected }
        await assertEventually("the list has every cue") { self.wc.cueList.nodeCount == 200 }
        try await engine.advance(seconds: 0)  // the media probes
        _ = await store.client.send({ .request(id: $0, .listPatchMappings) })
        let outline = wc.cueList.outlineView
        outline.expandItem(nil, expandChildren: true)
        return (0..<outline.numberOfRows).compactMap { (outline.item(atRow: $0) as? CueNode)?.id }
    }

    // MARK: The run

    func testSelectionResponsiveness() async throws {
        let window = try XCTUnwrap(wc.window)
        window.setFrame(NSRect(x: 60, y: 60, width: 1280, height: 800), display: true)
        wc.setVisible(.inspector, true)
        wc.setVisible(.masthead, true)
        let rows = try await buildWorkspace()
        XCTAssertEqual(rows.count, 200)
        // Since macOS 14 an app is activated only when the active one yields; Launch Services' open of the
        // running bundle asks for it as a click in the Dock does. A window that stays inactive still draws.
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
        if !(await eventually(timeout: 2) { NSApp.isActive }) {
            let open = Process()
            open.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            open.arguments = ["-a", Bundle.main.bundlePath]
            try? open.run()
            window.makeKeyAndOrderFront(nil)
        }
        let active = await eventually(timeout: 5) { NSApp.isActive && window.isKeyWindow }
        window.makeKeyAndOrderFront(nil)
        XCTAssertTrue(window.makeFirstResponder(wc.cueList.outlineView))
        print(
            "BENCH app active and window key: \(active), on screen: \(window.isVisible), occlusion visible: \(window.occlusionState.contains(.visible))"
        )
        installHooks()
        let outline = wc.cueList.outlineView

        // Warm-up: every kind's inspector built once.
        try await select(row: 0, rows: rows)
        for _ in 0..<20 { _ = try await step(.down, rows: rows) }
        print(
            "BENCH after warm-up: rows \(log.rows.count) standby \(log.standby.count) names \(log.names.count) heads \(log.heads.count)"
        )

        var results: [String: Any] = [:]
        results["commit"] = ProcessInfo.processInfo.environment["OPENLAB_BENCH_LABEL"] ?? "unlabelled"
        results["app_active"] = active
        // OPENLAB_BENCH_QUICK=1: a short run, to check the harness.
        let quick = ProcessInfo.processInfo.environment["OPENLAB_BENCH_QUICK"] == "1"
        let singles = quick ? 10 : 100

        // (a) One click on a row, 100 times, on rows in view.
        try await select(row: 0, rows: rows)
        outline.scrollRowToVisible(0)
        var clicks: [Step] = []
        var clickPath = ""
        for k in 0..<singles {
            let visible = outline.rows(in: outline.visibleRect)
            let span = max(2, visible.length - 1)
            let selected = outline.selectedRow
            var row = visible.location + (k * 7) % span
            if row == selected { row = visible.location + (row - visible.location + 1) % span }
            let s = try await step(.click(row: row), rows: rows)
            clickPath = s.path
            clicks.append(s)
        }
        results["click"] = summary("click", clicks)
        results["click_path"] = clickPath

        // (b) Down once, 100 times, from the top.
        try await select(row: 0, rows: rows)
        var downs: [Step] = []
        for _ in 0..<singles { downs.append(try await step(.down, rows: rows)) }
        results["down"] = summary("down", downs)

        // (c) Down 50 times at about 30 a second, five times.
        var bursts: [[String: Any]] = []
        for b in 0..<(quick ? 1 : 5) {
            let start = b * 30
            bursts.append(try await burst(from: start, rows: rows))
        }
        results["burst"] = burstSummary(bursts)

        if let out = ProcessInfo.processInfo.environment["OPENLAB_BENCH_OUT"] {
            let data = try JSONSerialization.data(withJSONObject: results, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: URL(fileURLWithPath: out))
            print("BENCH results written to \(out)")
        }
    }

    // MARK: Steps

    enum Input {
        case click(row: Int)
        case down
    }

    /// One measured step: the input's times and, from the drawing log, when each part showed its cue.
    struct Step {
        var target: ModelID
        var input: Double
        var handled: Double
        var highlight: Double?
        var arrow: Double?
        var masthead: Double?
        var inspector: Double?
        var engine: Double?
        var settled: Double?
        var busy: Double?
        var path = ""
    }

    private var downArrow: NSEvent {
        get throws {
            let chars = String(UnicodeScalar(NSDownArrowFunctionKey)!)
            return try XCTUnwrap(
                NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: [.numericPad, .function],
                    timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: wc.window?.windowNumber ?? 0,
                    context: nil, characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: 125)
            )
        }
    }

    /// Selects `row` the way a click's outcome does, unmeasured, and waits for all of it to settle.
    private func select(row: Int, rows: [ModelID]) async throws {
        let id = rows[row]
        wc.cueList.outlineView.scrollRowToVisible(row)
        wc.cueList.selectAsUser([id])
        let t = CACurrentMediaTime()
        _ = await waitUntil(timeout: 2) { self.settledTime(of: id, since: t) != nil }
        try await quiet()
    }

    /// Lets the engine's late answers and echoes in before the next input.
    private func quiet() async throws {
        try await Task.sleep(nanoseconds: 80_000_000)
    }

    /// Performs `input` and waits until every part shows its cue (at most 5 s), then a quiet spell.
    private func step(_ input: Input, rows: [ModelID]) async throws -> Step {
        let outline = wc.cueList.outlineView
        let window = try XCTUnwrap(wc.window)
        let targetRow: Int
        switch input {
        case .click(let row): targetRow = row
        case .down: targetRow = outline.selectedRow + 1
        }
        guard targetRow >= 0, targetRow < outline.numberOfRows, let node = outline.item(atRow: targetRow) as? CueNode
        else { throw TestMediaError(description: "no row \(targetRow)") }
        let target = node.id
        var s = Step(target: target, input: 0, handled: 0)
        let interval = signposter.beginInterval("step")
        switch input {
        case .click(let row):
            let (down, up) = try clickEvents(row: row)
            NSApp.postEvent(up, atStart: false)
            s.input = CACurrentMediaTime()
            window.sendEvent(down)
            s.handled = CACurrentMediaTime()
            s.path = "window.sendEvent"
            if wc.cueList.selectedIDs() != [target] {
                // The window took the press for itself (not active): the table's own press handling.
                NSApp.postEvent(up, atStart: false)
                s.input = CACurrentMediaTime()
                outline.mouseDown(with: down)
                s.handled = CACurrentMediaTime()
                s.path = "outline.mouseDown"
            }
        case .down:
            let key = try downArrow
            s.input = CACurrentMediaTime()
            if let passed = app.handleKeyDown(key) { window.sendEvent(passed) }
            s.handled = CACurrentMediaTime()
        }
        signposter.endInterval("step", interval, "handled")
        _ = await waitUntil(timeout: 2) { self.settledTime(of: target, since: s.input) != nil }
        fill(&s, until: nil)
        if s.settled == nil, unsettledReports < 5 {
            unsettledReports += 1
            let recentRows = log.rows.filter { $0.t >= s.input }.suffix(6).map {
                "\($0.cue == target ? "target" : "other") sel=\($0.selected) head=\($0.playhead)"
            }
            print(
                """
                BENCH unsettled step: highlight \(s.highlight != nil) arrow \(s.arrow != nil) masthead \(s.masthead != nil) \
                inspector \(s.inspector != nil) engine \(s.engine != nil); logged rows \(log.rows.count) standby \
                \(log.standby.count) names \(log.names.count) heads \(log.heads.count); \
                selected \(wc.cueList.selectedIDs() == [target]); expected standby '\(log.standbyText[target] ?? "?")' \
                field '\(wc.masthead.standbyField.stringValue)' last drawn '\(log.standby.last?.text ?? "-")'; \
                expected name '\(log.nameText[target] ?? "?")' field '\(wc.inspector.nameField.stringValue)' \
                last drawn '\(log.names.last?.text ?? "-")'; rows since input \(recentRows)
                """)
        }
        try await quiet()
        return s
    }

    private func clickEvents(row: Int) throws -> (NSEvent, NSEvent) {
        let outline = wc.cueList.outlineView
        let window = try XCTUnwrap(outline.window)
        let column = outline.column(withIdentifier: NSUserInterfaceItemIdentifier("name"))
        let cell = outline.frameOfCell(atColumn: max(0, column), row: row)
        let point = outline.convert(NSPoint(x: cell.midX, y: cell.midY), to: nil)
        func event(_ type: NSEvent.EventType) throws -> NSEvent {
            try XCTUnwrap(
                NSEvent.mouseEvent(
                    with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1))
        }
        return (try event(.leftMouseDown), try event(.leftMouseUp))
    }

    /// Fills in when each part first showed `s.target` after the input (drawn before `until`, when given).
    private func fill(_ s: inout Step, until: Double?) {
        let after = s.input
        let before = until ?? .infinity
        s.highlight = log.rows.first { $0.t >= after && $0.t < before && $0.cue == s.target && $0.selected }?.t
        s.arrow = log.rows.first { $0.t >= after && $0.t < before && $0.cue == s.target && $0.playhead }?.t
        let standby = log.standbyText[s.target]
        s.masthead = log.standby.first { $0.t >= after && $0.t < before && $0.text == standby }?.t
        let name = log.nameText[s.target]
        s.inspector = log.names.first { $0.t >= after && $0.t < before && $0.text == name }?.t
        s.engine = log.heads.first { $0.t >= after && $0.t < before && $0.cue == s.target }?.t
        let parts = [s.highlight, s.arrow, s.masthead, s.inspector, s.engine]
        if parts.allSatisfy({ $0 != nil }) {
            let end = parts.compactMap { $0 }.max() ?? after
            s.settled = end
            s.busy = log.busy(from: after, to: end)
        }
    }

    private func settledTime(of id: ModelID, since t: Double) -> Double? {
        var s = Step(target: id, input: t, handled: t)
        fill(&s, until: nil)
        return s.settled
    }

    /// Resumes once `condition` holds as the main run loop goes to sleep (after it has drawn), or after `timeout`.
    private func waitUntil(timeout: Double, _ condition: @escaping @MainActor () -> Bool) async -> Bool {
        if condition() { return true }
        return await withCheckedContinuation { (c: CheckedContinuation<Bool, Never>) in
            let waiter = Waiter(condition: condition, continuation: c)
            log.waiter = waiter
            let timer = Timer(timeInterval: timeout, repeats: false) { [weak waiter] _ in
                MainActor.assumeIsolated { waiter?.finish(false) }
            }
            RunLoop.main.add(timer, forMode: .common)
            waiter.timer = timer
        }
    }

    /// 50 Downs from `start`, one every 33 ms of the host's clock (a press due while the main thread is
    /// busy is handled as soon as it is free, as a queued key would be).
    private func burst(from start: Int, rows: [ModelID]) async throws -> [String: Any] {
        try await select(row: start, rows: rows)
        let count = 50
        let spacing = 0.033
        let targets = (1...count).map { rows[start + $0] }
        let window = try XCTUnwrap(wc.window)
        let key = try downArrow
        var steps: [Step] = []
        let driver = BurstDriver()
        let interval = signposter.beginInterval("burst")
        let first = CACurrentMediaTime() + 0.02
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            driver.press = { [unowned self] i in
                var s = Step(target: targets[i], input: CACurrentMediaTime(), handled: 0)
                self.signposter.emitEvent("press")
                if let passed = self.app.handleKeyDown(key) { window.sendEvent(passed) }
                s.handled = CACurrentMediaTime()
                steps.append(s)
            }
            driver.done = { c.resume() }
            driver.run(count: count, first: first, spacing: spacing)
        }
        signposter.endInterval("burst", interval)
        let last = try XCTUnwrap(targets.last)
        let lastInput = steps.last?.input ?? first
        _ = await waitUntil(timeout: 5) { self.settledTime(of: last, since: lastInput) != nil }
        var behind = ["highlight": 0, "arrow": 0, "masthead": 0, "inspector": 0, "any": 0]
        var latencies: [String: [Double]] = [:]
        for i in steps.indices {
            let next = i + 1 < steps.count ? steps[i + 1].input : nil
            fill(&steps[i], until: next)
            guard let next else { continue }
            // What each part showed as the next press came.
            let shown = log.shown(at: next)
            var wrong = false
            for (part, cue) in [
                ("highlight", shown.highlight), ("arrow", shown.arrow), ("masthead", shown.masthead),
                ("inspector", shown.inspector),
            ] where cue != steps[i].target {
                behind[part, default: 0] += 1
                wrong = true
            }
            if wrong { behind["any", default: 0] += 1 }
        }
        for s in steps {
            latencies["handler", default: []].append((s.handled - s.input) * 1000)
            for (part, t) in [
                ("highlight", s.highlight), ("arrow", s.arrow), ("masthead", s.masthead), ("inspector", s.inspector),
            ] {
                if let t { latencies[part, default: []].append((t - s.input) * 1000) }
            }
        }
        let settled = settledTime(of: last, since: lastInput)
        let leftover = settled.map { ($0 - lastInput) * 1000 }
        let span = (steps.last?.input ?? first) - (steps.first?.input ?? first)
        let busy = log.busy(from: steps.first?.input ?? first, to: settled ?? lastInput)
        try await quiet()
        return [
            "behind": behind, "latencies": latencies, "leftover_ms": leftover ?? -1, "presses": steps.count,
            "span_ms": span * 1000, "busy_ms": busy * 1000,
        ]
    }

    // MARK: Reporting

    private static func stats(_ values: [Double]) -> [String: Double] {
        guard !values.isEmpty else { return ["n": 0] }
        let v = values.sorted()
        func rank(_ p: Double) -> Double { v[min(v.count - 1, max(0, Int((p * Double(v.count)).rounded(.up)) - 1))] }
        return ["n": Double(v.count), "median": rank(0.5), "p95": rank(0.95), "max": v[v.count - 1]]
    }

    private static func line(_ name: String, _ s: [String: Double]) -> String {
        func f(_ k: String) -> String { s[k].map { String(format: "%7.2f", $0) } ?? "      -" }
        return
            "\(name.padding(toLength: 22, withPad: " ", startingAt: 0)) n=\(Int(s["n"] ?? 0)) median \(f("median")) p95 \(f("p95")) max \(f("max")) ms"
    }

    private func summary(_ flow: String, _ steps: [Step]) -> [String: Any] {
        func ms(_ get: (Step) -> Double?) -> [Double] { steps.compactMap { s in get(s).map { ($0 - s.input) * 1000 } } }
        let measures: [(String, [Double])] = [
            ("highlight_drawn", ms { $0.highlight }),
            ("arrow_drawn", ms { $0.arrow }),
            ("masthead_drawn", ms { $0.masthead }),
            ("inspector_drawn", ms { $0.inspector }),
            ("engine_head", ms { $0.engine }),
            ("all_settled", ms { $0.settled }),
            ("handler_busy", steps.map { ($0.handled - $0.input) * 1000 }),
            ("main_busy_to_settle", steps.compactMap { $0.busy.map { $0 * 1000 } }),
        ]
        var out: [String: Any] = ["unsettled": steps.filter { $0.settled == nil }.count]
        print("BENCH flow \(flow): \(steps.count) steps, \(steps.filter { $0.settled == nil }.count) never settled")
        for (name, values) in measures {
            let s = Self.stats(values)
            out[name] = s
            print("BENCH \(flow) \(Self.line(name, s))")
        }
        return out
    }

    private func burstSummary(_ bursts: [[String: Any]]) -> [String: Any] {
        var latencies: [String: [Double]] = [:]
        var behind: [String: [Int]] = [:]
        var leftovers: [Double] = []
        for b in bursts {
            for (k, v) in (b["latencies"] as? [String: [Double]]) ?? [:] { latencies[k, default: []] += v }
            for (k, v) in (b["behind"] as? [String: Int]) ?? [:] { behind[k, default: []].append(v) }
            leftovers.append((b["leftover_ms"] as? Double) ?? -1)
        }
        var out: [String: Any] = ["bursts": bursts]
        for (k, v) in latencies.sorted(by: { $0.key < $1.key }) {
            let s = Self.stats(v)
            out[k] = s
            print("BENCH burst \(Self.line(k, s))")
        }
        for (k, v) in behind.sorted(by: { $0.key < $1.key }) {
            print("BENCH burst behind at next press (of 49) \(k): \(v)")
        }
        print("BENCH burst leftover after last key ms: \(leftovers.map { String(format: "%.1f", $0) })")
        let spans = bursts.compactMap { $0["span_ms"] as? Double }
        let busy = bursts.compactMap { $0["busy_ms"] as? Double }
        print(
            "BENCH burst presses span ms: \(spans.map { String(format: "%.0f", $0) }), main busy ms: \(busy.map { String(format: "%.0f", $0) })"
        )
        out["behind"] = behind
        out["leftover_ms"] = leftovers
        return out
    }

    // MARK: Watching the drawing

    private func installHooks() {
        let log = DrawLog()
        self.log = log
        log.standbyField = wc.masthead.standbyField
        log.nameField = wc.inspector.nameField
        log.list = list
        log.store = store
        if let model = store.model {
            for c in model.allCuesForBench() {
                log.nameText[c.id] = c.raw["name"]?.stringValue ?? ""
                log.standbyText[c.id] = [c.number, model.listName(c)].compactMap { $0 }.filter { !$0.isEmpty }
                    .joined(separator: " \u{00B7} ")
            }
        }
        let signposter = self.signposter
        let rowHook = MethodHook(CueRowView.self, #selector(NSTableRowView.drawBackground(in:))) { view in
            MainActor.assumeIsolated {
                guard let row = view as? CueRowView, let table = row.superview as? NSOutlineView else { return }
                let r = table.row(for: row)
                guard r >= 0, let node = table.item(atRow: r) as? CueNode else { return }
                log.rows.append(
                    .init(t: CACurrentMediaTime(), cue: node.id, selected: row.isSelected, playhead: row.isPlayhead))
                if row.isSelected { signposter.emitEvent("row drawn selected") }
            }
        }
        // The field (or the view inside it that holds its text) has drawn: its text now is what was drawn.
        let fieldDrew: (NSView) -> Void = { view in
            MainActor.assumeIsolated {
                var v: NSView? = view
                for _ in 0..<3 {
                    if v === log.standbyField, let f = v as? NSTextField {
                        log.standby.append((CACurrentMediaTime(), f.stringValue))
                        signposter.emitEvent("masthead drawn")
                        return
                    }
                    if v === log.nameField, let f = v as? NSTextField {
                        log.names.append((CACurrentMediaTime(), f.stringValue))
                        signposter.emitEvent("inspector name drawn")
                        return
                    }
                    v = v?.superview
                }
            }
        }
        // Since macOS 26 a text field's text is drawn by a label view inside it (AppKit's NSSimpleLabel);
        // before, by its cell, whose interior drawing names the field.
        let labelHook = NSClassFromString("AppKit.NSSimpleLabel").flatMap {
            MethodHook($0, #selector(NSView.draw(_:))) { ($0 as? NSView).map(fieldDrew) }
        }
        let cellHook = MethodHook(
            cell: NSTextFieldCell.self, #selector(NSCell.drawInterior(withFrame:in:)), after: fieldDrew)
        hooks = [rowHook, labelHook, cellHook].compactMap { $0 }
        XCTAssertGreaterThanOrEqual(hooks.count, 2, "the row hook and a text hook are in")
        let main = CFRunLoopGetMain()
        // The first observer to run on waking and the last before sleeping bound each sleep.
        let first = CFRunLoopObserverCreateWithHandler(
            kCFAllocatorDefault, CFRunLoopActivity.afterWaiting.rawValue, true, CFIndex.min
        ) { _, _ in
            MainActor.assumeIsolated {
                let t = CACurrentMediaTime()
                if let s = log.sleepStart {
                    log.sleeps.append((s, t))
                    log.sleepStart = nil
                }
            }
        }
        let last = CFRunLoopObserverCreateWithHandler(
            kCFAllocatorDefault, CFRunLoopActivity.beforeWaiting.rawValue, true, CFIndex.max
        ) { _, _ in
            MainActor.assumeIsolated {
                log.sampleHead(at: CACurrentMediaTime())
                if let w = log.waiter, w.condition() {
                    log.waiter = nil
                    w.finish(true)
                }
                log.sleepStart = CACurrentMediaTime()
            }
        }
        for o in [first, last].compactMap({ $0 }) {
            CFRunLoopAddObserver(main, o, .commonModes)
            observers.append(o)
        }
    }
}

/// What was drawn and when, and when the main run loop slept, for one benchmark run.
@MainActor
private final class DrawLog {
    struct RowDraw {
        let t: Double
        let cue: ModelID
        let selected: Bool
        let playhead: Bool
    }
    var rows: [RowDraw] = []
    var standby: [(t: Double, text: String)] = []
    var names: [(t: Double, text: String)] = []
    var heads: [(t: Double, cue: ModelID?)] = []
    var sleeps: [(start: Double, end: Double)] = []
    var sleepStart: Double?
    weak var standbyField: NSTextField?
    weak var nameField: NSTextField?
    weak var store: WorkspaceStore?
    var list: ModelID?
    var nameText: [ModelID: String] = [:]
    var standbyText: [ModelID: String] = [:]
    var waiter: Waiter?

    func sampleHead(at t: Double) {
        guard let list, let store else { return }
        let head = store.benchEngineHeads[list]
        if heads.last.map({ $0.cue != head }) ?? true { heads.append((t, head)) }
    }

    /// The main thread's time not asleep in its run loop between `a` and `b`.
    func busy(from a: Double, to b: Double) -> Double {
        guard b > a else { return 0 }
        var slept = 0.0
        for s in sleeps where s.end > a && s.start < b { slept += min(b, s.end) - max(a, s.start) }
        if let s = sleepStart, s < b { slept += b - max(a, s) }
        return max(0, (b - a) - slept)
    }

    /// The cue each part showed as last drawn before `t`: a row's last drawing says whether it was
    /// highlighted and whether it carried the arrow.
    func shown(at t: Double) -> (highlight: ModelID?, arrow: ModelID?, masthead: ModelID?, inspector: ModelID?) {
        var lastRow: [ModelID: RowDraw] = [:]
        for r in rows where r.t < t { lastRow[r.cue] = r }
        let lit = lastRow.values.filter(\.selected).max { $0.t < $1.t }?.cue
        let arrow = lastRow.values.filter(\.playhead).max { $0.t < $1.t }?.cue
        let standbyNow = standby.last { $0.t < t }?.text
        let nameNow = names.last { $0.t < t }?.text
        let masthead = standbyText.first { $0.value == standbyNow }?.key
        let inspector = nameText.first { $0.value == nameNow }?.key
        return (lit, arrow, masthead, inspector)
    }
}

/// A wait for a condition checked as the main run loop goes to sleep.
@MainActor
private final class Waiter {
    let condition: @MainActor () -> Bool
    private var continuation: CheckedContinuation<Bool, Never>?
    var timer: Timer?

    init(condition: @escaping @MainActor () -> Bool, continuation: CheckedContinuation<Bool, Never>) {
        self.condition = condition
        self.continuation = continuation
    }

    func finish(_ ok: Bool) {
        timer?.invalidate()
        continuation?.resume(returning: ok)
        continuation = nil
    }
}

/// Presses on a fixed schedule of the host's clock, each in a run-loop turn of its own.
@MainActor
private final class BurstDriver {
    var press: ((Int) -> Void)?
    var done: (() -> Void)?
    private var index = 0
    private var count = 0
    private var first = 0.0
    private var spacing = 0.0

    func run(count: Int, first: Double, spacing: Double) {
        self.count = count
        self.first = first
        self.spacing = spacing
        index = 0
        schedule()
    }

    private func schedule() {
        let due = first + Double(index) * spacing
        let timer = Timer(timeInterval: max(0, due - CACurrentMediaTime()), repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.fire() }
        }
        RunLoop.main.add(timer, forMode: .common)
    }

    private func fire() {
        press?(index)
        index += 1
        if index < count { schedule() } else { done?() }
    }
}

/// A drawing method of `cls` replaced for the run by one that draws as before, then reports the view.
/// Removed by putting the original implementation back.
private final class MethodHook {
    private typealias Draw = @convention(c) (NSObject, Selector, NSRect) -> Void
    private let method: Method
    private let original: IMP

    init?(_ cls: AnyClass, _ selector: Selector, after: @escaping (NSObject) -> Void) {
        guard let inherited = class_getInstanceMethod(cls, selector) else { return nil }
        let original = method_getImplementation(inherited)
        let draw = unsafeBitCast(original, to: Draw.self)
        let block: @convention(block) (NSObject, NSRect) -> Void = { view, rect in
            draw(view, selector, rect)
            after(view)
        }
        let replacement = imp_implementationWithBlock(block)
        // A class that only inherits the method gets one of its own, so its superclass is untouched.
        if class_addMethod(cls, selector, replacement, method_getTypeEncoding(inherited)) {
            guard let own = class_getInstanceMethod(cls, selector) else { return nil }
            method = own
        } else {
            method = inherited
            method_setImplementation(inherited, replacement)
        }
        self.original = original
    }

    /// For a cell's drawing method taking a frame and the view drawn in: reports that view.
    init?(cell cls: AnyClass, _ selector: Selector, after: @escaping (NSView) -> Void) {
        typealias CellDraw = @convention(c) (NSObject, Selector, NSRect, NSView) -> Void
        guard let inherited = class_getInstanceMethod(cls, selector) else { return nil }
        let original = method_getImplementation(inherited)
        let draw = unsafeBitCast(original, to: CellDraw.self)
        let block: @convention(block) (NSObject, NSRect, NSView) -> Void = { cell, rect, view in
            draw(cell, selector, rect, view)
            after(view)
        }
        let replacement = imp_implementationWithBlock(block)
        if class_addMethod(cls, selector, replacement, method_getTypeEncoding(inherited)) {
            guard let own = class_getInstanceMethod(cls, selector) else { return nil }
            method = own
        } else {
            method = inherited
            method_setImplementation(inherited, replacement)
        }
        self.original = original
    }

    func remove() { method_setImplementation(method, original) }
}

extension WorkspaceStore {
    /// The playhead as the engine last reported it. The one line that differs when this benchmark is
    /// run on a build from before the head was shown ahead of the engine (there `playheads` is the engine's).
    fileprivate var benchEngineHeads: [ModelID: ModelID] { engineHeads }
}

extension WorkspaceModel {
    /// Every cue of every list, groups' children included.
    fileprivate func allCuesForBench() -> [Cue] {
        func walk(_ cues: [Cue]) -> [Cue] { cues.flatMap { [$0] + walk($0.children) } }
        return cueLists.flatMap { walk($0.cues) }
    }
}
