// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

@testable import openLab

/// Drives the real workspace window on the real engine (stepped virtual clock) through a JSON
/// scenario and saves pictures of the window, so an agent can look at the app without the user's
/// screen, keyboard or pointer. `make ui-shots SCENARIO=<file.json> [OUT=<dir>]` runs it; the
/// format and the step vocabulary are in `ScenarioRunner.Step`.
///
/// The window is never ordered on screen and never made key: presses go to the view under the
/// point (as the window's own dispatch would send them), keys through the app's key handling to
/// the window, and shots draw the window's view tree offscreen. Nothing here is in the app target.
@MainActor
final class ScenarioRunner {
    struct Failure: Error, CustomStringConvertible {
        let description: String
        init(_ text: String) { description = text }
    }

    /// One step of a scenario: one JSON object whose single action key names the step.
    enum Step {
        /// `{"select": "1"}`: the cue with that number (else that name) selected as a click on its row;
        /// `{"select": ["1", "2"]}`: those cues selected together.
        case select([String])
        /// `{"paste_properties": "1", "tick": ["colour"]}`: cue 1 copied, then the Paste Properties sheet
        /// opened for the selection with those properties ticked. Shots show the sheet over the window.
        case pasteProperties(String, tick: [String])
        /// `{"click": id, "at": [fx, fy], "modifiers": [...]}`: a press and release at a fraction of
        /// the control's bounds from its top-left (default its middle).
        case click(id: String, at: CGPoint, modifiers: NSEvent.ModifierFlags, clicks: Int)
        /// `{"drag": id, "from": [fx, fy], "to": [fx, fy]}`.
        case drag(id: String, from: CGPoint, to: CGPoint, modifiers: NSEvent.ModifierFlags)
        /// `{"type": id, "text": "...", "enter": true}`: the control focused, its text replaced, Return.
        case type(id: String, text: String, enter: Bool)
        /// `{"key": "space", "modifiers": ["command"]}`: a key press as the app's key handling takes it.
        case key(String, modifiers: NSEvent.ModifierFlags)
        /// `{"scroll": id, "dx": 0, "dy": 3, "precise": true, "modifiers": [...], "at": [...]}`.
        case scroll(id: String, dx: CGFloat, dy: CGFloat, precise: Bool, modifiers: NSEvent.ModifierFlags, at: CGPoint)
        /// `{"magnify": id, "by": 0.5, "at": [...]}`: a pinch (positive spreads: zooms in).
        case magnify(id: String, by: CGFloat, at: CGPoint)
        /// `{"advance": 1.5}`: seconds of the engine's device time.
        case advance(Double)
        /// `{"resize": [width, height]}`: the window's content size in points.
        case resize(CGSize)
        /// `{"tab": "timeLoops"}`: the inspector tab with that identifier.
        case tab(String)
        /// `{"focus": "settings"}`: the workspace settings window (opened without taking focus) is
        /// the one the next steps and shots use, its `tab` the settings tab (general, audio, osc);
        /// `"workspace"` goes back to the workspace window.
        case focus(String)
        /// `{"shot": "name"}`: `<OUT>/<n>-<name>.png`, the whole window at 2x.
        case shot(String)
    }

    struct Scenario {
        /// A test show's name (`mac/TestShows`), or a path to a `.openlab` file (relative: to the
        /// scenario file), opened where it is and never saved.
        /// `"new"`: a new untitled workspace, as File > New makes it.
        var show: String
        /// `{"media/a.wav": 60}`: test tones of that many seconds written into the test show's copy first.
        var media: [(path: String, seconds: Double)] = []
        var size = CGSize(width: 1200, height: 800)
        var dark = true
        /// `"V1:1920x1080,V2:1280x720"`: the engine's stage host on these virtual displays,
        /// drawn offscreen (no stage host without it).
        var displays: String?
        /// This machine's settings (`settings.json`) as the engine finds them at start.
        var settings: Data?
        var steps: [Step] = []
    }

    static let stepKinds = [
        "select", "click", "double_click", "drag", "type", "key", "scroll", "magnify", "advance", "resize", "tab",
        "focus", "paste_properties", "shot",
    ]

    // MARK: Parsing

    /// The scenario in `data`; `base` is the folder relative show paths start from.
    static func parse(_ data: Data, base: URL) throws -> Scenario {
        let root: Any
        do { root = try JSONSerialization.jsonObject(with: data) } catch {
            let detail = (error as NSError).userInfo[NSDebugDescriptionErrorKey] as? String ?? "\(error)"
            throw Failure("the scenario is not valid JSON: \(detail)")
        }
        guard let top = root as? [String: Any] else { throw Failure("the scenario must be a JSON object") }
        let known: Set = ["show", "media", "window", "appearance", "displays", "settings", "steps", "about"]
        if let extra = top.keys.first(where: { !known.contains($0) }) {
            throw Failure("unknown scenario key \"\(extra)\"; known: \(known.sorted().joined(separator: ", "))")
        }
        guard let show = top["show"] as? String, !show.isEmpty else {
            throw Failure("\"show\" is missing: a test show's name (Demo, Second, Three) or a path to a .openlab file")
        }
        var s = Scenario(show: show)
        if !isTestShow(show) {
            s.show = URL(fileURLWithPath: show, relativeTo: base).standardizedFileURL.path
        }
        if let media = top["media"] {
            guard let m = media as? [String: Any] else { throw Failure("\"media\" must be {\"media/a.wav\": seconds}") }
            guard isTestShow(show) else { throw Failure("\"media\" is only for a test show (it writes into its copy)") }
            for (path, value) in m.sorted(by: { $0.key < $1.key }) {
                guard let seconds = value as? Double, seconds > 0, seconds <= 600 else {
                    throw Failure("media \"\(path)\": seconds must be a number from 0 to 600")
                }
                guard !path.hasPrefix("/"), !path.split(separator: "/").contains("..") else {
                    throw Failure("media \"\(path)\": must be a path inside the show's folder")
                }
                s.media.append((path, seconds))
            }
        }
        if let window = top["window"] {
            guard let w = window as? [String: Any], let width = w["width"] as? Double,
                let height = w["height"] as? Double
            else { throw Failure("\"window\" must be {\"width\": points, \"height\": points}") }
            s.size = try size(width, height, "window")
        }
        if let appearance = top["appearance"] {
            guard let a = appearance as? String, ["dark", "light"].contains(a) else {
                throw Failure("\"appearance\" must be \"dark\" or \"light\"")
            }
            s.dark = a == "dark"
        }
        if let displays = top["displays"] {
            guard let d = displays as? String, !d.isEmpty else {
                throw Failure("\"displays\" must be \"ID:WxH,...\" (virtual displays)")
            }
            s.displays = d
        }
        if let settings = top["settings"] {
            guard JSONSerialization.isValidJSONObject(settings) else {
                throw Failure("\"settings\" must be a settings.json object")
            }
            s.settings = try JSONSerialization.data(withJSONObject: settings)
        }
        guard let steps = top["steps"] as? [Any] else { throw Failure("\"steps\" must be an array of steps") }
        for (i, raw) in steps.enumerated() {
            do { s.steps.append(try step(raw)) } catch let f as Failure {
                throw Failure("step \(i + 1): \(f.description)")
            }
        }
        return s
    }

    static func isTestShow(_ name: String) -> Bool {
        !name.contains("/") && !name.hasSuffix(".openlab")
    }

    private static func size(_ width: Double, _ height: Double, _ what: String) throws -> CGSize {
        guard (100...4000).contains(width), (100...4000).contains(height) else {
            throw Failure("\(what) size must be 100 to 4000 points each way")
        }
        return CGSize(width: width, height: height)
    }

    private static func step(_ raw: Any) throws -> Step {
        guard let o = raw as? [String: Any] else { throw Failure("a step must be a JSON object") }
        let kinds = o.keys.filter { stepKinds.contains($0) }
        guard kinds.count == 1, let kind = kinds.first else {
            throw Failure(
                "a step needs exactly one of \(stepKinds.joined(separator: ", ")); this one has "
                    + (o.keys.isEmpty ? "none" : o.keys.sorted().joined(separator: ", ")))
        }
        let options: [String: Set<String>] = [
            "click": ["at", "modifiers"], "double_click": ["at", "modifiers"], "drag": ["from", "to", "modifiers"],
            "type": ["text", "enter"], "key": ["modifiers"], "scroll": ["dx", "dy", "precise", "modifiers", "at"],
            "magnify": ["by", "at"], "paste_properties": ["tick"],
        ]
        let allowed = options[kind, default: []].union([kind])
        if let extra = o.keys.first(where: { !allowed.contains($0) }) {
            throw Failure(
                "\"\(kind)\" does not take \"\(extra)\"; it takes \(allowed.sorted().joined(separator: ", "))")
        }
        func string(_ key: String) throws -> String {
            guard let v = o[key] as? String, !v.isEmpty else { throw Failure("\"\(key)\" must be a non-empty string") }
            return v
        }
        func number(_ key: String, default d: Double? = nil) throws -> Double {
            if o[key] == nil, let d { return d }
            guard let v = o[key] as? Double, v.isFinite else { throw Failure("\"\(key)\" must be a number") }
            return v
        }
        func point(_ key: String) throws -> CGPoint {
            guard let v = o[key] else { return CGPoint(x: 0.5, y: 0.5) }
            guard let a = v as? [Double], a.count == 2, a.allSatisfy({ (0...1).contains($0) }) else {
                throw Failure("\"\(key)\" must be [x, y], fractions 0 to 1 of the control from its top-left")
            }
            return CGPoint(x: a[0], y: a[1])
        }
        let mods = try modifiers(o["modifiers"])
        switch kind {
        case "select":
            if let many = o["select"] as? [String], !many.isEmpty, many.allSatisfy({ !$0.isEmpty }) {
                return .select(many)
            }
            return .select([try string("select")])
        case "paste_properties":
            let tick = o["tick"] as? [String] ?? []
            if let bad = tick.first(where: { PasteProperty(rawValue: $0) == nil }) {
                throw Failure(
                    "unknown property \"\(bad)\"; known: \(PasteProperty.allCases.map(\.rawValue).joined(separator: ", "))"
                )
            }
            return .pasteProperties(try string("paste_properties"), tick: tick)
        case "click": return .click(id: try string("click"), at: try point("at"), modifiers: mods, clicks: 1)
        case "double_click":
            return .click(id: try string("double_click"), at: try point("at"), modifiers: mods, clicks: 2)
        case "drag": return .drag(id: try string("drag"), from: try point("from"), to: try point("to"), modifiers: mods)
        case "type":
            guard let text = o["text"] as? String else { throw Failure("\"text\" must be a string") }
            return .type(id: try string("type"), text: text, enter: o["enter"] as? Bool ?? true)
        case "key":
            let name = try string("key")
            guard Self.key(name) != nil else {
                throw Failure(
                    "unknown key \"\(name)\"; one character, or \(keyCodes.keys.sorted().joined(separator: ", "))")
            }
            return .key(name, modifiers: mods)
        case "scroll":
            return .scroll(
                id: try string("scroll"), dx: CGFloat(try number("dx", default: 0)),
                dy: CGFloat(try number("dy", default: 0)), precise: o["precise"] as? Bool ?? true, modifiers: mods,
                at: try point("at"))
        case "magnify": return .magnify(id: try string("magnify"), by: CGFloat(try number("by")), at: try point("at"))
        case "advance":
            let s = try number("advance")
            guard s >= 0, s <= 600 else { throw Failure("\"advance\" must be 0 to 600 seconds") }
            return .advance(s)
        case "resize":
            guard let a = o["resize"] as? [Double], a.count == 2 else {
                throw Failure("\"resize\" must be [width, height]")
            }
            return .resize(try size(a[0], a[1], "resize"))
        case "tab": return .tab(try string("tab"))
        case "focus":
            let which = try string("focus")
            guard ["workspace", "settings", "sheet"].contains(which) else {
                throw Failure("\"focus\" must be \"workspace\", \"settings\" or \"sheet\"")
            }
            return .focus(which)
        default:
            let name = try string("shot")
            guard name.range(of: #"^[A-Za-z0-9_-]{1,60}$"#, options: .regularExpression) != nil else {
                throw Failure("a shot's name must be 1 to 60 letters, digits, '-' or '_'")
            }
            return .shot(name)
        }
    }

    private static func modifiers(_ raw: Any?) throws -> NSEvent.ModifierFlags {
        guard let raw else { return [] }
        let names: [String: NSEvent.ModifierFlags] = [
            "shift": .shift, "control": .control, "option": .option, "command": .command,
        ]
        guard let list = raw as? [String] else { throw Failure("\"modifiers\" must be an array of names") }
        var flags: NSEvent.ModifierFlags = []
        for name in list {
            guard let f = names[name] else {
                throw Failure("unknown modifier \"\(name)\"; known: \(names.keys.sorted().joined(separator: ", "))")
            }
            flags.insert(f)
        }
        return flags
    }

    /// Named keys: their characters (AppKit's function-key characters for arrows) and key codes.
    static let keyCodes: [String: (chars: String, code: UInt16)] = [
        "space": (" ", 49), "escape": ("\u{1B}", 53), "return": ("\r", 36), "tab": ("\t", 48),
        "delete": ("\u{7F}", 51),
        "left": (String(UnicodeScalar(UInt32(NSLeftArrowFunctionKey))!), 123),
        "right": (String(UnicodeScalar(UInt32(NSRightArrowFunctionKey))!), 124),
        "down": (String(UnicodeScalar(UInt32(NSDownArrowFunctionKey))!), 125),
        "up": (String(UnicodeScalar(UInt32(NSUpArrowFunctionKey))!), 126),
    ]

    static func key(_ name: String) -> (chars: String, code: UInt16)? {
        if let k = keyCodes[name] { return k }
        return name.count == 1 ? (name, 0) : nil
    }

    // MARK: Running

    let scenario: Scenario
    let out: URL
    private var log: [String] = []
    private var shots = 0
    private(set) var written: [URL] = []
    private var engine: TestEngine?
    private var rig: Rig!
    private var wc: WorkspaceWindowController!
    private var app: AppController!
    /// The settings window while a step has focused it (the steps then act on it).
    private var focused: NSWindow?
    private var window: NSWindow {
        // The sheet, once it has closed, gives the steps back to the workspace window.
        if let f = focused, f is PasteSheetWindow, wc.pasteSheet?.window !== f { return wc.window! }
        return focused ?? wc.window!
    }

    init(scenario: Scenario, out: URL) {
        self.scenario = scenario
        self.out = out
    }

    /// The engine this run started, while it runs (a test checks it is gone after a failure).
    var enginePid: Int32? { engine?.pid }

    /// Runs every step, writing the shots and `steps.log` into `out`; the engine is stopped and the
    /// window closed whatever happens. A failed step throws with the reason, also the log's last line.
    func run() async throws {
        try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
        defer { finish() }
        do {
            try await start()
            for (i, step) in scenario.steps.enumerated() {
                do { try await perform(step) } catch let f as Failure {
                    throw Failure("step \(i + 1) (\(Self.describe(step))): \(f.description)")
                }
            }
            note("done: \(shots) shot(s) in \(out.path)")
        } catch {
            note("FAILED: \(error)")
            throw error
        }
    }

    private func finish() {
        wc?.window?.close()
        engine?.stop()
        try? (log.joined(separator: "\n") + "\n").write(
            to: out.appendingPathComponent("steps.log"), atomically: true, encoding: .utf8)
    }

    private func note(_ line: String) { log.append(line) }

    private func start() async throws {
        let isNew = scenario.show == "new"
        let showPath = isNew ? nil : try showToOpen()
        let engine = try await TestEngine.start(displays: scenario.displays, settings: scenario.settings)
        self.engine = engine
        if scenario.displays != nil {
            // A new workspace gets a stage per display: wait until the stage host has listed them.
            guard
                await eventually({
                    guard case .success(let ok) = await engine.request(.listDisplays),
                        case .displays(let list, _)? = ok.data
                    else { return false }
                    return !list.isEmpty
                })
            else { throw Failure("the stage host listed no displays") }
        }
        let doc: ModelID
        do {
            if isNew {
                guard case .success(let ok) = await engine.app.send({ .request(id: $0, .new) }),
                    case .opened(let made)? = ok.data
                else { throw Failure("the engine refused a new workspace") }
                doc = made
            } else if let showPath {
                doc = try await engine.open(path: showPath)
            } else {
                let media = scenario.media
                doc = try await engine.open(scenario.show) { copy in
                    for m in media { try writeWAV(copy.appendingPathComponent(m.path), seconds: m.seconds) }
                }
            }
        } catch {
            throw Failure("the engine did not open the show \(scenario.show): \(error)")
        }
        rig = Rig(engine: engine, workspace: doc)
        guard await rig.connect() else { throw Failure("the workspace connection did not come up") }
        guard await eventually({ self.rig.store.model != nil }) else { throw Failure("the workspace never loaded") }
        wc = WorkspaceWindowController(store: rig.store)
        // The sheet is never put on screen, and copies stay off the user's pasteboard and settings.
        wc.presentSheet = { _ in }
        let board = NSPasteboard.withUniqueName()
        wc.editor.pasteboard = board
        wc.cueList.pasteboard = board
        wc.pasteDefaults = UserDefaults(suiteName: "openlab-scenario-\(UUID().uuidString)") ?? .standard
        wireApp()
        window.appearance = NSAppearance(named: scenario.dark ? .darkAqua : .aqua)
        window.setContentSize(scenario.size)
        // The window is never ordered on screen: the run takes no focus.
        window.makeFirstResponder(wc.cueList.outlineView)
        guard await eventually({ self.wc.cueList.listID != nil }) else { throw Failure("the cue list never showed") }
        // The footer's engine status comes with the engine's status report (one a second of device
        // time), not with the snapshot: device time moves on, with nothing playing, until one comes.
        var waited = 0.0
        while rig.store.status == nil {
            guard waited < 3 else { throw Failure("the engine sent no status in 3 s of device time") }
            try await rig.advance(0.1)
            waited += 0.1
        }
        await settle()
        note("opened \(scenario.show) in a \(Int(scenario.size.width))x\(Int(scenario.size.height)) window")
    }

    /// The show's path to open in place, or nil for a test show; fails for a show that is not there.
    private func showToOpen() throws -> String? {
        let shows = TestEngine.testShows
        if Self.isTestShow(scenario.show) {
            let names =
                (try? FileManager.default.contentsOfDirectory(atPath: shows?.path ?? "")).map {
                    $0.filter { !$0.hasPrefix(".") }.sorted()
                } ?? []
            guard names.contains(scenario.show) else {
                throw Failure(
                    "show \"\(scenario.show)\" not found: test shows are \(names.joined(separator: ", ")); "
                        + "or give a path to a .openlab file")
            }
            return nil
        }
        guard FileManager.default.fileExists(atPath: scenario.show) else {
            throw Failure("show not found: no file at \(scenario.show)")
        }
        return scenario.show
    }

    /// The app's own key handling and transport, for this window only: the controller the app makes
    /// for each workspace window, wired as `AppController` wires it, with no session of its own (Esc's
    /// panic goes to the engine here, as the app's session sends it).
    private func wireApp() {
        let store = rig.store
        let session = EngineSession(transport: WebSocketTransport(), infoProvider: { .failure(.missing) })
        let app = AppController(session: session, scheduler: ManualScheduler())
        app.launching = true  // never the launcher
        app.controllers[ObjectIdentifier(store)] = wc
        app.keyWindow = { [unowned self] in self.window }
        app.isTextEditing = { [unowned self] in (self.window.firstResponder as? NSText)?.isEditable == true }
        app.isRecordingKey = { [unowned self] in self.window.firstResponder is HotkeyRecorder }
        app.beep = { [unowned self] in self.note("  the app beeped") }
        self.app = app
        wc.onGo = { Task { await app.go(store) } }
        wc.onPauseAll = { Task { await app.run(.pauseAll, on: store) } }
        wc.onResumeAll = { Task { await app.run(.resumeAll, on: store) } }
        wc.onPanic = { [unowned self] in Task { await self.panic() } }
        wc.onResetAll = { Task { await app.run(.resetAll, on: store) } }
        wc.onCommand = { [unowned self] command, cues in
            Task { await app.perform(command, on: self.wc, cues: cues) }
        }
        wc.onCueAction = { action in Task { await app.run(action, on: store) } }
    }

    private func panic() async {
        guard let engine else { return }
        if case .failure(let e) = await engine.app.send({ .panic(id: $0) }) { note("  panic failed: \(e)") }
    }

    /// Lets what a step started reach the engine and come back, then lays the window out.
    private func catchUp() async {
        await settle()
        await rig.sync()
        await settle()
        window.contentView?.layoutSubtreeIfNeeded()
    }

    private func perform(_ step: Step) async throws {
        switch step {
        case .select(let what):
            let nodes = try what.map(cue)
            wc.cueList.selectAsUser(nodes.map(\.id))
            for node in nodes { note("select \(what): cue \(node.cue.number ?? "-") \"\(node.cue.displayName)\"") }
        case .pasteProperties(let from, let tick):
            let node = try cue(from)
            if let open = wc.pasteSheet {
                open.cancel()
                note("  the open sheet was cancelled first")
            }
            guard await wc.editor.copySelection(of: [node.id]) else { throw Failure("cue \(from) was not copied") }
            wc.promptPasteProperties(onto: wc.cueList.selectedIDs())
            guard let sheet = wc.pasteSheet else { throw Failure("the sheet did not open") }
            for name in tick {
                guard let p = PasteProperty(rawValue: name), let box = sheet.propertyChecks[p] else {
                    throw Failure("\(name) is not listed for these cues")
                }
                if box.state != .on { box.performClick(nil) }
            }
            note("paste_properties from \(from): \(sheet.categoryChecks.count) categories, ticked \(tick)")
        case .click(let id, let at, let mods, let clicks):
            let view = try control(id)
            let p = try windowPoint(at, in: view)
            var events: [NSEvent] = []
            for n in 1...clicks {
                events.append(try mouse(.leftMouseDown, at: p, mods, clicks: n))
                events.append(try mouse(.leftMouseUp, at: p, mods, clicks: n))
            }
            let hit = try press(events, on: view)
            note("\(clicks == 2 ? "double_click" : "click") \(id) at \(Self.text(p)): \(hit)")
        case .drag(let id, let from, let to, let mods):
            let view = try control(id)
            let a = try windowPoint(from, in: view)
            let b = try windowPoint(to, in: view)
            var events = [try mouse(.leftMouseDown, at: a, mods, clicks: 1)]
            for k in 1...8 {
                let f = CGFloat(k) / 8
                let p = NSPoint(x: a.x + (b.x - a.x) * f, y: a.y + (b.y - a.y) * f)
                events.append(try mouse(.leftMouseDragged, at: p, mods, clicks: 1))
            }
            events.append(try mouse(.leftMouseUp, at: b, mods, clicks: 1))
            let hit = try press(events, on: view)
            note("drag \(id) \(Self.text(a)) -> \(Self.text(b)): \(hit)")
        case .type(let id, let text, let enter):
            try type(text, into: try control(id), enter: enter)
            note("type \"\(text)\" into \(id)\(enter ? " + return" : "")")
        case .key(let name, let mods):
            try key(name, mods)
            note("key \(Self.modifierText(mods))\(name)")
        case .scroll(let id, let dx, let dy, let precise, let mods, let at):
            let view = try control(id)
            let e = GestureEvent()
            e.point = try windowPoint(at, in: view)
            e.delta = (dx, dy)
            e.precise = precise
            e.flags = mods
            // A scroll view's own `scrollWheel` runs an event-tracking loop that lets queued main-actor
            // work run inside this async step, which aborts the process (out-of-order task
            // deallocation). Its clip view is moved instead, by the wheel's distance as it would be.
            if let scroll = view as? NSScrollView {
                let clip = scroll.contentView
                let factor: CGFloat = precise ? 1 : 10
                var origin = clip.bounds.origin
                origin.y -= dy * factor * (clip.isFlipped ? 1 : -1)
                origin.x -= dx * factor
                origin = clip.constrainBoundsRect(NSRect(origin: origin, size: clip.bounds.size)).origin
                clip.scroll(to: origin)
                scroll.reflectScrolledClipView(clip)
                note("scrolled \(id) to y \(Int(origin.y.rounded()))")
                break
            }
            let target = try hitView(at: e.point, inside: view)
            target.scrollWheel(with: e)
            note(
                "scroll \(id) dx \(dx) dy \(dy)\(precise ? "" : " (wheel)") \(Self.modifierText(mods)): \(target.className)"
            )
        case .magnify(let id, let by, let at):
            let view = try control(id)
            let e = GestureEvent()
            e.point = try windowPoint(at, in: view)
            e.mag = by
            let target = try hitView(at: e.point, inside: view)
            target.magnify(with: e)
            note("magnify \(id) by \(by): \(target.className)")
        case .advance(let seconds):
            // In tenths, as a show plays: the app sees each progress report on the way.
            var left = seconds
            while left > 1e-9 {
                let step = min(0.1, left)
                try await rig.advance(step)
                left -= step
            }
            note("advance \(seconds) s")
        case .resize(let size):
            window.setContentSize(size)
            note("resize \(Int(size.width))x\(Int(size.height))")
        case .focus(let which):
            if which == "settings" {
                let sc = wc.settingsWindow
                let w = try XCTUnwrap(sc.window)
                w.title = String(format: loc("settings.title.workspace"), wc.workspaceName)
                w.appearance = wc.window?.appearance
                await sc.model.refresh()
                sc.render()
                focused = w
            } else if which == "sheet" {
                guard let sheet = wc.pasteSheet?.window else { throw Failure("no sheet is up") }
                focused = sheet
            } else {
                focused = nil
            }
            note("focus \(which)")
        case .tab(let id):
            let tabs = focused != nil && !(focused is PasteSheetWindow) ? wc.settingsWindow.tabs : wc.inspector.tabView
            let ids = tabs.tabViewItems.compactMap { $0.identifier as? String }
            guard ids.contains(id) else {
                throw Failure("no tab \"\(id)\" for this selection; tabs: \(ids.joined(separator: ", "))")
            }
            tabs.selectTabViewItem(withIdentifier: id)
            note("tab \(id)")
        case .shot(let name):
            await catchUp()
            await wc.inspector.timeLoops.loading?.value
            await catchUp()
            shots += 1
            let url = out.appendingPathComponent(String(format: "%02d-%@.png", shots, name))
            try shot(to: url)
            written.append(url)
            note("shot \(url.lastPathComponent) (footer: \(wc.footer.statusButton.title))")
        }
        await catchUp()
    }

    // MARK: Finding things

    private func cue(_ what: String) throws -> CueNode {
        var all: [CueNode] = []
        func walk(_ nodes: [CueNode]) {
            for n in nodes {
                all.append(n)
                walk(n.children)
            }
        }
        walk(wc.cueList.roots)
        if let n = all.first(where: { $0.cue.number == what }) ?? all.first(where: { $0.cue.displayName == what }) {
            return n
        }
        let names = all.map { "\($0.cue.number ?? "-") \"\($0.cue.displayName)\"" }
        throw Failure("no cue numbered or named \"\(what)\"; cues: \(names.joined(separator: ", "))")
    }

    /// Every view in the window with an accessibility identifier, in drawing order.
    private func identified() -> [(id: String, view: NSView)] {
        var found: [(String, NSView)] = []
        func walk(_ v: NSView) {
            let id = v.accessibilityIdentifier()
            if !id.isEmpty { found.append((id, v)) }
            v.subviews.forEach(walk)
        }
        if let content = window.contentView { walk(content) }
        return found
    }

    /// The one view shown with identifier `id`.
    private func control(_ id: String) throws -> NSView {
        let all = identified()
        let shown = all.filter { $0.id == id && !$0.view.isHiddenOrHasHiddenAncestor && $0.view.window != nil }
        guard let first = shown.first else {
            let hidden = all.contains { $0.id == id } ? " (there is one, but it is hidden)" : ""
            let ids = Set(all.filter { !$0.view.isHiddenOrHasHiddenAncestor }.map(\.id)).sorted()
            throw Failure("no control with identifier \"\(id)\"\(hidden); shown: \(ids.joined(separator: ", "))")
        }
        guard shown.count == 1 else {
            throw Failure("\(shown.count) controls have identifier \"\(id)\" (rows repeat theirs); pick one by its cue")
        }
        if let c = first.view as? NSControl, !c.isEnabled { throw Failure("\"\(id)\" is disabled") }
        return first.view
    }

    /// The window point at fraction `at` of `view`, from its top-left; it must be in the window.
    private func windowPoint(_ at: CGPoint, in view: NSView) throws -> NSPoint {
        let b = view.bounds
        let y = view.isFlipped ? b.minY + at.y * b.height : b.maxY - at.y * b.height
        let local = NSPoint(x: b.minX + at.x * b.width, y: y)
        let p = view.convert(local, to: nil)
        guard let content = window.contentView, content.frame.contains(p) else {
            throw Failure("that point (\(Self.text(p))) is outside the window's content: resize the window")
        }
        if view.visibleRect.isEmpty { throw Failure("the control is scrolled out of view or has no size") }
        return p
    }

    /// The view a press at `p` lands on, which must be `view` or inside it.
    private func hitView(at p: NSPoint, inside view: NSView) throws -> NSView {
        guard let content = window.contentView, let frame = content.superview,
            let hit = content.hitTest(frame.convert(p, from: nil))
        else { throw Failure("nothing in the window at \(Self.text(p))") }
        guard hit === view || hit.isDescendant(of: view) else {
            let id = hit.accessibilityIdentifier()
            throw Failure("the press lands on \(hit.className)\(id.isEmpty ? "" : " \"\(id)\""), covering the control")
        }
        return hit
    }

    // MARK: Input

    /// Tags the events this run posts, to tell them from any other in the queue.
    private static let tag = 0x5CE7

    private func mouse(_ type: NSEvent.EventType, at p: NSPoint, _ mods: NSEvent.ModifierFlags, clicks: Int) throws
        -> NSEvent
    {
        try XCTUnwrap(
            NSEvent.mouseEvent(
                with: type, location: p, modifierFlags: mods, timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber, context: nil, eventNumber: Self.tag, clickCount: clicks,
                pressure: type == .leftMouseUp ? 0 : 1))
    }

    /// Each press (a `leftMouseDown` and what follows it) goes to the view under it as the window's
    /// dispatch sends it: the window's press hook, then `mouseDown`. The events after the down are in
    /// the queue first, so a view that tracks the mouse inside `mouseDown` (tables) reads them there
    /// and returns; those it did not read go to the view's own handlers. A standard control in a
    /// window that is not key ignores the press (only a real click would make the window key), so
    /// its cell tracks the events instead, as the control's `mouseDown` does in a key window.
    private func press(_ events: [NSEvent], on view: NSView) throws -> String {
        guard let first = events.first else { return "" }
        let target = try hitView(at: first.locationInWindow, inside: view)
        // Its menu runs its own tracking loop on real input: it would wait for ever.
        if target is NSPopUpButton {
            throw Failure("a pop-up button's menu needs real input; the runner cannot open it")
        }
        var groups: [[NSEvent]] = []
        for e in events {
            if e.type == .leftMouseDown { groups.append([e]) } else { groups[groups.count - 1].append(e) }
        }
        let hook = (window as? WorkspaceWindow)?.onPress
        for group in groups {
            for e in group.dropFirst() { NSApp.postEvent(e, atStart: false) }
            hook?(true)
            target.mouseDown(with: group[0])
            var left = drainPosted()
            if !left.isEmpty, let control = target as? NSControl, let cell = control.cell {
                for e in left { NSApp.postEvent(e, atStart: false) }
                _ = cell.trackMouse(with: group[0], in: control.bounds, of: control, untilMouseUp: true)
                left = drainPosted()
            }
            hook?(false)
            for e in left {
                if e.type == .leftMouseDragged { target.mouseDragged(with: e) } else { target.mouseUp(with: e) }
            }
        }
        let id = target.accessibilityIdentifier()
        return "\(target.className)\(id.isEmpty ? "" : " \"\(id)\"")"
    }

    /// This run's mouse events still in the queue (the view did not track them), in order.
    private func drainPosted() -> [NSEvent] {
        var ours: [NSEvent] = []
        var others: [NSEvent] = []
        while let e = NSApp.nextEvent(
            matching: [.leftMouseDragged, .leftMouseUp], until: .distantPast, inMode: .default, dequeue: true)
        {
            if e.windowNumber == window.windowNumber, e.eventNumber == Self.tag {
                ours.append(e)
            } else {
                others.append(e)
            }
        }
        for e in others.reversed() { NSApp.postEvent(e, atStart: true) }
        return ours
    }

    private func keyEvent(_ chars: String, code: UInt16, _ mods: NSEvent.ModifierFlags) throws -> NSEvent {
        try XCTUnwrap(
            NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: mods, timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber, context: nil, characters: chars,
                charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code))
    }

    /// A key as the app takes it: Esc's panic and the transport keys first (the app's key monitor),
    /// then a key equivalent, then the window's first responder.
    private func key(_ name: String, _ mods: NSEvent.ModifierFlags) throws {
        guard let k = Self.key(name) else { throw Failure("unknown key \(name)") }
        let event = try keyEvent(k.chars, code: k.code, mods)
        // Esc: the app's key monitor sends its panic through the app's session (this run has none
        // connected) and passes the key on.
        if k.code == 53 && mods.isEmpty {
            Task { await panic() }
            return window.sendEvent(event)
        }
        guard let passed = app.handleKeyDown(event) else { return }
        if mods.contains(.command) || mods.contains(.control), window.performKeyEquivalent(with: passed) { return }
        window.sendEvent(passed)
    }

    private func type(_ text: String, into view: NSView, enter: Bool) throws {
        // A field already being edited keeps its editor: focusing it again would end that edit first.
        let editing = (view as? NSControl)?.currentEditor() != nil || window.firstResponder === view
        guard editing || window.makeFirstResponder(view) else { throw Failure("it does not take the keyboard focus") }
        if let field = window.firstResponder as? NSTextView {
            field.selectAll(nil)
        }
        for ch in text {
            let event = try keyEvent(String(ch), code: 0, [])
            guard let passed = app.handleKeyDown(event) else { continue }
            window.sendEvent(passed)
        }
        if enter { window.sendEvent(try keyEvent("\r", code: 36, [])) }
    }

    // MARK: Shots

    /// The whole window (title bar included) at 2x, drawn offscreen.
    private func shot(to url: URL) throws {
        // A focused sheet is shot over its workspace window, as it shows on screen.
        let shown = window is PasteSheetWindow ? wc.window! : window
        guard let frame = shown.contentView?.superview else { throw Failure("the window has no frame view") }
        frame.layoutSubtreeIfNeeded()
        var rep = try renderView(frame, scale: 2, background: shown.backgroundColor)
        if shown === wc.window, let sheet = wc.pasteSheet?.window { rep = try overlay(sheet, on: rep) }
        guard let png = rep.representation(using: .png, properties: [:]) else { throw Failure("PNG encoding failed") }
        try png.write(to: url)
    }

    /// A shown scroller draws through its own layer on screen, which a forced draw leaves out: its
    /// slot and knob are drawn again where it is, so a shot shows that the list scrolls.
    private func drawScrollers(in root: NSView, on rep: NSBitmapImageRep) {
        var scrollers: [NSScroller] = []
        func find(_ v: NSView) {
            guard !v.isHidden else { return }
            if let s = v as? NSScroller { scrollers.append(s) }
            v.subviews.forEach(find)
        }
        find(root)
        guard !scrollers.isEmpty, let context = NSGraphicsContext(bitmapImageRep: rep) else { return }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        defer { NSGraphicsContext.restoreGraphicsState() }
        for s in scrollers {
            let r = s.convert(s.bounds, to: root)
            let cg = context.cgContext
            cg.saveGState()
            cg.scaleBy(x: 2, y: 2)
            if s.isFlipped {
                cg.translateBy(x: r.minX, y: r.maxY)
                cg.scaleBy(x: 1, y: -1)
            } else {
                cg.translateBy(x: r.minX, y: r.minY)
            }
            s.drawKnobSlot(in: s.rect(for: .knobSlot), highlight: false)
            s.drawKnob()
            cg.restoreGState()
            note("  drew the sheet's scroller")
        }
    }

    /// `sheet` drawn where AppKit puts a sheet: centred, under the window's title bar.
    private func overlay(_ sheet: NSWindow, on rep: NSBitmapImageRep) throws -> NSBitmapImageRep {
        sheet.appearance = wc.window?.appearance
        guard let frame = sheet.contentView else { throw Failure("the sheet has no content") }
        frame.layoutSubtreeIfNeeded()
        // On screen a scroll view tiles again once its document has its size (its scroller comes then).
        func tile(_ v: NSView) {
            (v as? NSScrollView)?.tile()
            v.subviews.forEach(tile)
        }
        tile(frame)
        frame.layoutSubtreeIfNeeded()
        // Off screen the sheet's colours resolve in the drawing appearance: the window's, as on screen.
        let drawing = NSAppearance.current
        NSAppearance.current = sheet.appearance
        defer { NSAppearance.current = drawing }
        let top = try renderView(frame, scale: 2, background: .windowBackgroundColor)
        drawScrollers(in: frame, on: top)
        guard let context = NSGraphicsContext(bitmapImageRep: rep) else { throw Failure("no context") }
        let base = try XCTUnwrap(wc.window)
        let titleBar = (base.frame.height - base.contentLayoutRect.height) * 2
        let x = (CGFloat(rep.pixelsWide) - CGFloat(top.pixelsWide)) / 2
        let y = CGFloat(rep.pixelsHigh) - titleBar - CGFloat(top.pixelsHigh)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        top.draw(in: NSRect(x: x, y: y, width: CGFloat(top.pixelsWide), height: CGFloat(top.pixelsHigh)))
        NSGraphicsContext.restoreGraphicsState()
        return rep
    }

    // MARK: Text

    static func describe(_ step: Step) -> String {
        switch step {
        case .select(let s): return "select \(s.joined(separator: ", "))"
        case .pasteProperties(let s, _): return "paste_properties \(s)"
        case .click(let id, _, _, let n): return "\(n == 2 ? "double_click" : "click") \(id)"
        case .drag(let id, _, _, _): return "drag \(id)"
        case .type(let id, _, _): return "type \(id)"
        case .key(let k, let m): return "key \(modifierText(m))\(k)"
        case .scroll(let id, _, _, _, _, _): return "scroll \(id)"
        case .magnify(let id, _, _): return "magnify \(id)"
        case .advance(let s): return "advance \(s)"
        case .resize(let s): return "resize \(Int(s.width))x\(Int(s.height))"
        case .tab(let t): return "tab \(t)"
        case .focus(let w): return "focus \(w)"
        case .shot(let s): return "shot \(s)"
        }
    }

    private static func modifierText(_ m: NSEvent.ModifierFlags) -> String {
        [(NSEvent.ModifierFlags.control, "control"), (.option, "option"), (.shift, "shift"), (.command, "command")]
            .filter { m.contains($0.0) }.map { "\($0.1)+" }.joined()
    }

    private static func text(_ p: NSPoint) -> String { "(\(Int(p.x.rounded())), \(Int(p.y.rounded())))" }
}

/// `make ui-shots`: the scenario named by `OPENLAB_SCENARIO`, its shots into `OPENLAB_SHOTS_OUT`
/// (both reach the test process through xcodebuild's `TEST_RUNNER_` prefix). Without a scenario it
/// reports a skip. The tests after it are the runner's own.
@MainActor
final class ScenarioShotTests: XCTestCase {
    func testScenario() async throws {
        let env = ProcessInfo.processInfo.environment
        guard let path = env["OPENLAB_SCENARIO"], !path.isEmpty else {
            throw XCTSkip("no scenario: run `make ui-shots SCENARIO=<file.json> [OUT=<dir>]`")
        }
        let file = URL(fileURLWithPath: path)
        let out =
            env["OPENLAB_SHOTS_OUT"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) }
            ?? FileManager.default.temporaryDirectory.appendingPathComponent("openlab-shots", isDirectory: true)
        let data: Data
        do { data = try Data(contentsOf: file) } catch { return XCTFail("cannot read the scenario \(path): \(error)") }
        let scenario: ScenarioRunner.Scenario
        do { scenario = try ScenarioRunner.parse(data, base: file.deletingLastPathComponent()) } catch {
            try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
            try? "FAILED: \(error)\n".write(
                to: out.appendingPathComponent("steps.log"), atomically: true, encoding: .utf8)
            return XCTFail("\(path): \(error)")
        }
        let runner = ScenarioRunner(scenario: scenario, out: out)
        do { try await runner.run() } catch let skip as XCTSkip { throw skip } catch { XCTFail("\(path): \(error)") }
    }

    // MARK: The runner's own tests

    private var out: URL!

    override func setUp() async throws {
        out = FileManager.default.temporaryDirectory.appendingPathComponent(
            "openlab-shots-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() async throws {
        if let out { try? FileManager.default.removeItem(at: out) }
    }

    private func scenario(_ json: String) throws -> ScenarioRunner.Scenario {
        try ScenarioRunner.parse(Data(json.utf8), base: out)
    }

    /// The steps' message, and the engine the run started has ended.
    private func failure(of json: String) async throws -> String {
        let runner = ScenarioRunner(scenario: try scenario(json), out: out)
        var pid: Int32?
        do {
            try await runner.run()
            XCTFail("the run should fail")
        } catch let skip as XCTSkip {
            throw skip
        } catch {
            pid = runner.enginePid
            if let pid { XCTAssertEqual(kill(pid, 0), -1, "the engine \(pid) still runs") }
            let log = try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
            XCTAssertTrue(log.contains("FAILED: \(error)"), log)
            return "\(error)"
        }
        return ""
    }

    func testAScenarioWritesItsShotsAtTwiceTheWindowsSize() async throws {
        let runner = ScenarioRunner(
            scenario: try scenario(
                """
                {"show": "Demo", "window": {"width": 900, "height": 600}, "appearance": "light",
                 "steps": [{"shot": "start"}, {"select": "2"}, {"key": "space"}, {"advance": 0.2},
                           {"resize": [1000, 640]}, {"shot": "after-go"}]}
                """), out: out)
        try await runner.run()
        XCTAssertEqual(runner.written.map(\.lastPathComponent), ["01-start.png", "02-after-go.png"])
        let frames = [NSSize(width: 900, height: 600), NSSize(width: 1000, height: 640)]
        for (url, content) in zip(runner.written, frames) {
            let rep = try XCTUnwrap(NSBitmapImageRep(data: try Data(contentsOf: url)), url.path)
            // The whole window: its content and the title bar above it.
            let frame = NSWindow.frameRect(forContentRect: NSRect(origin: .zero, size: content), styleMask: .titled)
            XCTAssertEqual(rep.pixelsWide, Int(frame.width * 2), url.lastPathComponent)
            XCTAssertEqual(rep.pixelsHigh, Int(frame.height * 2), url.lastPathComponent)
        }
        let log = try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
        XCTAssertTrue(log.contains("key space"), log)
        // The first shot already shows the engine's status in the footer, not the wait for it.
        let first = try XCTUnwrap(log.split(separator: "\n").first { $0.hasPrefix("shot 01-start.png") }, log)
        XCTAssertFalse(first.contains(loc("status.waiting")), String(first))
        XCTAssertTrue(first.contains("(footer: ") && !first.hasSuffix("(footer: )"), String(first))
        XCTAssertTrue(log.hasSuffix("done: 2 shot(s) in \(out.path)\n"), log)
        if let pid = runner.enginePid { XCTAssertEqual(kill(pid, 0), -1, "the engine ended") }
    }

    func testAnUnknownIdentifierFailsListingTheIdentifiersShown() async throws {
        let message = try await failure(of: #"{"show": "Demo", "steps": [{"click": "no-such-button"}]}"#)
        XCTAssertTrue(message.hasPrefix("step 1 (click no-such-button): no control with identifier"), message)
        XCTAssertTrue(message.contains("go-button"), "the identifiers in the window are listed: \(message)")
    }

    func testAMissingShowFailsNamingTheTestShows() async throws {
        let message = try await failure(of: #"{"show": "Nope", "steps": []}"#)
        XCTAssertTrue(message.contains("show \"Nope\" not found: test shows are Demo, Second, Targets, Three"), message)
        let path = try await failure(of: #"{"show": "gone/x.openlab", "steps": []}"#)
        XCTAssertTrue(path.contains("show not found: no file at \(out.path)/gone/x.openlab"), path)
    }

    /// A view that paints past its bounds (as one that does not clip does on screen) shows that paint
    /// in a window shot, over its neighbour.
    func testAWindowShotShowsDrawingOutsideAViewsBounds() throws {
        final class Spill: NSView {
            override func draw(_ dirtyRect: NSRect) {
                NSColor.red.setFill()
                NSRect(x: -20, y: 0, width: 10, height: bounds.height).fill()
            }
        }
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 100), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        let spill = Spill(frame: NSRect(x: 100, y: 20, width: 50, height: 50))
        spill.clipsToBounds = false
        window.contentView?.addSubview(spill)
        let frame = try XCTUnwrap(window.contentView?.superview)
        let rep = try renderView(frame, scale: 2, background: .black)
        // 15 points left of the view, inside its height (rows of the bitmap run top-down).
        let y = rep.pixelsHigh - Int(45 * 2)
        let c = try XCTUnwrap(rep.colorAt(x: Int(85 * 2), y: y)?.usingColorSpace(.sRGB))
        XCTAssertGreaterThan(c.redComponent, 0.9, "the spill is in the shot: \(c)")
        XCTAssertLessThan(c.greenComponent, 0.1, "\(c)")
    }

    func testMalformedJSONFailsWithWhereItBroke() throws {
        XCTAssertThrowsError(try scenario(#"{"show": "Demo", "steps": [{"shot": "a"},]"#)) { error in
            XCTAssertTrue("\(error)".hasPrefix("the scenario is not valid JSON: "), "\(error)")
        }
        XCTAssertThrowsError(try scenario(#"{"show": "Demo", "steps": [{"clik": "go-button"}]}"#)) { error in
            XCTAssertTrue("\(error)".hasPrefix("step 1: a step needs exactly one of select, click"), "\(error)")
        }
        XCTAssertThrowsError(try scenario(#"{"show": "Demo", "steps": [{"key": "f99"}]}"#)) { error in
            XCTAssertTrue("\(error)".contains("unknown key \"f99\""), "\(error)")
        }
    }
}
