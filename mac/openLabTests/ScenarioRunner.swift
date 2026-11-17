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
    /// A part of a control a step aims at instead of the whole control: a table's header or row, or
    /// a segment (named by its tooltip, else its label) of a segmented control inside it.
    enum Part: Equatable {
        case header(String)
        case row(String)
        case segment(String)
    }

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
        /// `{"drag": id, "from": [fx, fy], "to": [fx, fy]}`. With `"header": "Name"` on a table, the
        /// fractions are of that column header (and may lie outside 0 to 1, to drag past it). A row
        /// cannot be dragged: AppKit's drag session needs real input and would wait for ever.
        case drag(id: String, from: CGPoint, to: CGPoint, modifiers: NSEvent.ModifierFlags, part: Part?)
        /// `{"hover": id, "at": [fx, fy]}` (or `"header"`/`"row"` as `drag`, or `"segment": "Wait"`,
        /// fractions of that segment as AppKit lays it out): the pointer rests on the
        /// control. The next shots draw the tooltip text AppKit would show there, as a bubble under the
        /// pointer; a point with no tooltip stops the run.
        case hover(id: String, at: CGPoint, part: Part?)
        /// `{"open_menu": id}`: a pop-up button's menu shown open in the next shots, its items as
        /// listed (a tick for the chosen one, grey for those that are off) and in `steps.log`. Any
        /// step but a shot closes it, a `choose` after it picks from it.
        case openMenu(id: String)
        /// `{"menu_command": "undo"}`: a menu-bar Edit command (undo, redo, delete, cut, copy, paste)
        /// sent as the menu sends it, along the responder chain from the window's first responder to
        /// the app's menu actions. A command the menu would grey out stops the run.
        case menuCommand(String)
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
        /// `{"device": "lose"}` / `{"device": "restore"}`: the virtual output device unplugged as an
        /// interface would be, or listed again (the engine reopens it at its next list read).
        case device(lose: Bool)
        /// `{"resize": [width, height]}`: the window's content size in points.
        case resize(CGSize)
        /// `{"tab": "timeLoops"}`: the inspector tab with that identifier.
        case tab(String)
        /// `{"focus": "settings"}`: the workspace settings window (opened without taking focus) is
        /// the one the next steps and shots use, its `tab` the settings tab (general, audio, osc);
        /// `"workspace"` goes back to the workspace window. `"patch-editor"`: the patch editor last
        /// opened (a click on a row's Edit or the inspector's patch buttons), its `tab` the editor's.
        case focus(String)
        /// `{"shot": "name"}`: `<OUT>/<n>-<name>.png`, the whole window at 2x. With `"display": "V1"`
        /// (or a list) each named virtual display's composite is shot beside it as
        /// `<n>-<name>-display-<id>.png`: what the stage host draws there, every stage stacked, read
        /// back as a grid of sample colours (`displayGridPoints`) and drawn as blocks.
        case shot(String, displays: [String])
        /// `{"inside": id, "of": id}`: the run stops unless the first control lies wholly inside the
        /// second (a tab view: inside its content, below its tabs), as laid out now.
        case inside(id: String, of: String)
        /// `{"set_media": "2.1", "file": "media/gone.wav"}`: the cue's file reference set as an edit
        /// (a file that is not there leaves the cue missing it).
        case setMedia(String, file: String)
        /// `{"choose": id, "item": "Main PA"}`: that item of a pop-up button chosen, as a pick from its
        /// menu does (the menu itself, which needs real input, is not opened).
        case choose(id: String, item: String)
        /// `{"find_missing_files": ["gone.wav", "x/o.wav"], "folder": "Show Media"}`: a folder (named
        /// `folder`, default `found`) holding a test tone at each path, searched with Tools > Find
        /// Missing Files. The next steps and shots act on the tool's window (`focus` `sheet`: its
        /// question, shown over it); `focus` `workspace` goes back.
        case findMissingFiles([String], folder: String)
    }

    struct Scenario {
        /// A test show's name (`mac/TestShows`), or a path to a `.openlab` file (relative: to the
        /// scenario file), opened where it is and never saved.
        /// `"new"`: a new untitled workspace, as File > New makes it.
        var show: String
        /// `{"media/a.wav": 60}`: media of that many seconds written into the test show's copy first,
        /// by extension: a test tone, a movie (`.mov`, `.mp4`) or a still (`.png`, `.jpg`, `.tiff`).
        /// `{"media/a.mov": {"seconds": 4, "sound": "pcm:2:48000"}}`: a movie with audio tracks (one
        /// per `+`-joined part: `pcm` or `aac`, channels, sample rate).
        var media: [(path: String, seconds: Double, sound: String?)] = []
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
        "focus", "paste_properties", "shot", "inside", "device", "set_media", "find_missing_files",
        "choose", "hover", "open_menu", "menu_command",
    ]

    /// The Edit menu's commands a `menu_command` step sends, by name.
    static let menuCommands: [String: MainMenu.LockedEdit] = [
        "undo": .undo, "redo": .redo, "delete": .delete, "cut": .cut, "copy": .copy, "paste": .paste,
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
                let made = value as? [String: Any]
                let sound = made?["sound"] as? String
                if let made, made.keys.contains(where: { $0 != "seconds" && $0 != "sound" }) || sound?.isEmpty == true {
                    throw Failure("media \"\(path)\": takes {\"seconds\": n, \"sound\": \"pcm:2:48000\"}")
                }
                guard let seconds = (made?["seconds"] ?? value) as? Double, seconds > 0, seconds <= 600 else {
                    throw Failure("media \"\(path)\": seconds must be a number from 0 to 600")
                }
                guard !path.hasPrefix("/"), !path.split(separator: "/").contains("..") else {
                    throw Failure("media \"\(path)\": must be a path inside the show's folder")
                }
                s.media.append((path, seconds, sound))
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
        let ids = (s.displays ?? "").split(separator: ",").map { String($0.split(separator: ":").first ?? "") }
        for (i, step) in s.steps.enumerated() {
            guard case .shot(_, let wanted) = step, let missing = wanted.first(where: { !ids.contains($0) }) else {
                continue
            }
            throw Failure(
                "step \(i + 1): display \"\(missing)\" is not one of the scenario's \"displays\" ("
                    + (ids.isEmpty ? "none" : ids.joined(separator: ", ")) + ")")
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
            "click": ["at", "modifiers"], "double_click": ["at", "modifiers"],
            "drag": ["from", "to", "modifiers", "header"], "hover": ["at", "header", "row", "segment"],
            "shot": ["display"],
            "type": ["text", "enter"], "key": ["modifiers"], "scroll": ["dx", "dy", "precise", "modifiers", "at"],
            "magnify": ["by", "at"], "paste_properties": ["tick"], "inside": ["of"], "set_media": ["file"],
            "choose": ["item"],
            "find_missing_files": ["folder"],
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
        func point(_ key: String, beyond: Bool = false) throws -> CGPoint {
            guard let v = o[key] else { return CGPoint(x: 0.5, y: 0.5) }
            let range = beyond ? -20.0...20.0 : 0.0...1.0
            guard let a = v as? [Double], a.count == 2, a.allSatisfy({ range.contains($0) }) else {
                throw Failure(
                    "\"\(key)\" must be [x, y], fractions \(beyond ? "-20 to 20" : "0 to 1") of the "
                        + (beyond ? "header or row" : "control") + " from its top-left")
            }
            return CGPoint(x: a[0], y: a[1])
        }
        func part() throws -> Part? {
            let named = ["header", "row", "segment"].filter { o[$0] != nil }
            guard named.count <= 1 else {
                throw Failure(
                    "a step aims at one \"header\", \"row\" or \"segment\", not \(named.joined(separator: " and "))")
            }
            switch named.first {
            case "header": return .header(try string("header"))
            case "row": return .row(try string("row"))
            case "segment": return .segment(try string("segment"))
            default: return nil
            }
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
        case "drag":
            let aimed = try part()
            return .drag(
                id: try string("drag"), from: try point("from", beyond: aimed != nil),
                to: try point("to", beyond: aimed != nil), modifiers: mods, part: aimed)
        case "hover":
            let aimed = try part()
            return .hover(id: try string("hover"), at: try point("at", beyond: aimed != nil), part: aimed)
        case "open_menu": return .openMenu(id: try string("open_menu"))
        case "menu_command":
            let name = try string("menu_command")
            guard menuCommands[name] != nil else {
                throw Failure(
                    "unknown command \"\(name)\"; known: \(menuCommands.keys.sorted().joined(separator: ", "))")
            }
            return .menuCommand(name)
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
        case "device":
            switch try string("device") {
            case "lose": return .device(lose: true)
            case "restore": return .device(lose: false)
            default: throw Failure("\"device\" must be \"lose\" or \"restore\"")
            }
        case "resize":
            guard let a = o["resize"] as? [Double], a.count == 2 else {
                throw Failure("\"resize\" must be [width, height]")
            }
            return .resize(try size(a[0], a[1], "resize"))
        case "tab": return .tab(try string("tab"))
        case "inside": return .inside(id: try string("inside"), of: try string("of"))
        case "set_media": return .setMedia(try string("set_media"), file: try string("file"))
        case "choose": return .choose(id: try string("choose"), item: try string("item"))
        case "find_missing_files":
            guard let files = o["find_missing_files"] as? [String],
                files.allSatisfy({ !$0.isEmpty && !$0.hasPrefix("/") && !$0.split(separator: "/").contains("..") })
            else { throw Failure("\"find_missing_files\" must be an array of paths inside the folder") }
            let folder = o["folder"] == nil ? "found" : try string("folder")
            guard !folder.contains("/"), folder != "..", folder != "." else {
                throw Failure("\"folder\" must be one folder name")
            }
            return .findMissingFiles(files, folder: folder)
        case "focus":
            let which = try string("focus")
            guard ["workspace", "settings", "sheet", "patch-editor", "preferences"].contains(which) else {
                throw Failure(
                    "\"focus\" must be \"workspace\", \"settings\", \"patch-editor\", \"preferences\" or \"sheet\"")
            }
            return .focus(which)
        default:
            let name = try string("shot")
            guard name.range(of: #"^[A-Za-z0-9_-]{1,60}$"#, options: .regularExpression) != nil else {
                throw Failure("a shot's name must be 1 to 60 letters, digits, '-' or '_'")
            }
            let displays: [String]
            switch o["display"] {
            case nil: displays = []
            case let one as String where !one.isEmpty: displays = [one]
            case let many as [String] where !many.isEmpty && many.allSatisfy({ !$0.isEmpty }): displays = many
            default: throw Failure("\"display\" must be a display id or a list of them (the scenario's \"displays\")")
            }
            return .shot(name, displays: displays)
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
        // The tool's question, once answered, gives the steps back to the tool.
        if let f = focused, let tool = wc.existingMissingFiles, f.sheetParent == nil, isChoiceSheet(f),
            tool.choiceSheet?.window !== f
        {
            return tool.window!
        }
        return focused ?? wc.window!
    }

    private func isChoiceSheet(_ w: NSWindow) -> Bool {
        w.accessibilityIdentifier() == "missing-file-choice-sheet"
    }

    /// The window a sheet shown in `w` belongs over, and that sheet (nil: `w` is no sheet's).
    private func sheetOver(_ w: NSWindow) -> (base: NSWindow, sheet: NSWindow?) {
        if w is PasteSheetWindow { return (wc.window!, wc.pasteSheet?.window) }
        if let tool = wc.existingMissingFiles, let toolWindow = tool.window {
            if isChoiceSheet(w) || w === toolWindow { return (toolWindow, tool.choiceSheet?.window) }
        }
        return (w, w === wc.window ? wc.pasteSheet?.window : nil)
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
        prefs?.window?.close()
        MeterView.countsWindowsOffScreen = false
        if let shippedCovered { MeterView.defaultWindowIsCovered = shippedCovered }
        wc?.window?.close()
        // The window's buttons hold the app controller, which holds the window: let both go.
        app?.controllers = [:]
        (wc, app) = (nil, nil)
        engine?.stop()
        try? (log.joined(separator: "\n") + "\n").write(
            to: out.appendingPathComponent("steps.log"), atomically: true, encoding: .utf8)
    }

    private func note(_ line: String) { log.append(line) }

    /// The application preferences, once a step focused them.
    private var prefs: PreferencesWindowController?
    /// What new meter views asked before the run (restored when it ends).
    private var shippedCovered: ((NSWindow, Bool) -> Bool)?

    private func start() async throws {
        // The windows are never on screen: their meters count as shown, so they move in shots.
        MeterView.countsWindowsOffScreen = true
        shippedCovered = MeterView.defaultWindowIsCovered
        MeterView.defaultWindowIsCovered = { _, _ in false }
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
                    for m in media {
                        try writeTestMedia(copy.appendingPathComponent(m.path), seconds: m.seconds, sound: m.sound)
                    }
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
        wc.settingsWindow.presentsEditors = false
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

    /// What a shot draws over the window: a pop-up's open menu, a hovered control's tooltip. Any
    /// step that is not a shot or a wait ends them, as a click or a key does.
    private var openMenu: NSPopUpButton?
    private var tip: (text: String, at: NSPoint)?
    private var menuBar: AppDelegate?

    private func perform(_ step: Step) async throws {
        switch step {
        case .shot, .advance, .inside, .openMenu, .hover: break
        default: (openMenu, tip) = (nil, nil)
        }
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
        case .drag(let id, let from, let to, let mods, let part):
            let (view, rect) = try aim(id, part)
            let column = columnOf(id, part)
            let widthBefore = column?.width
            let a = try windowPoint(from, in: view, rect: rect)
            let b = try windowPoint(to, in: view, rect: rect)
            var events = [try mouse(.leftMouseDown, at: a, mods, clicks: 1)]
            for k in 1...8 {
                let f = CGFloat(k) / 8
                let p = NSPoint(x: a.x + (b.x - a.x) * f, y: a.y + (b.y - a.y) * f)
                events.append(try mouse(.leftMouseDragged, at: p, mods, clicks: 1))
            }
            events.append(try mouse(.leftMouseUp, at: b, mods, clicks: 1))
            let hit = try press(events, on: view)
            let widthNote = widthBefore.flatMap { w in column.map { ", width \(Int(w)) -> \(Int($0.width))" } } ?? ""
            note("drag \(id)\(Self.partText(part)) \(Self.text(a)) -> \(Self.text(b)): \(hit)\(widthNote)")
        case .hover(let id, let at, let part):
            let (view, rect) = try aim(id, part)
            let p = try windowPoint(at, in: view, rect: rect)
            let target = try hitView(at: p, inside: view)
            guard let text = tooltip(of: target, at: p) else {
                throw Failure("no tooltip at \(Self.text(p)) (\(target.className))")
            }
            tip = (text, p)
            note(
                "hover \(id)\(Self.partText(part)) at \(Self.text(p)): tooltip \"\(text)\" "
                    + "(the bubble in shots is drawn by the runner, not AppKit)")
        case .openMenu(let id):
            await catchUp()
            guard let popup = try control(id) as? NSPopUpButton else {
                throw Failure("\"\(id)\" is not a pop-up button")
            }
            // A pull-down's first item is the button's own title, not a choice.
            let items = Self.shownItems(of: popup)
            guard !items.isEmpty else { throw Failure("\"\(id)\" has no items to show") }
            openMenu = popup
            note("open_menu \(id): \(items.map(Self.menuLine).joined(separator: " | "))")
        case .menuCommand(let name):
            await catchUp()
            guard let edit = Self.menuCommands[name] else { throw Failure("unknown command \(name)") }
            let item = NSMenuItem(title: name, action: edit.selector, keyEquivalent: "")
            let bar = menuBar ?? AppDelegate()
            bar.appController = app
            menuBar = bar
            if window.firstResponder is NSText {
                throw Failure("text is being edited: the menu's action would go to the text, not the cues")
            }
            guard bar.validateMenuItem(item) else { throw Failure("Edit > \(name) is off (greyed out) now") }
            // As a menu item with no target is sent: each responder from the first one up, then the
            // app's menu actions.
            var chain: [NSResponder] = []
            var next = window.firstResponder
            while let r = next {
                chain.append(r)
                next = r.nextResponder
            }
            let handler: NSObject = chain.first { $0.responds(to: edit.selector) } ?? bar
            _ = handler.perform(edit.selector, with: item)
            note("menu_command \(name): \(Swift.type(of: handler))")
        case .type(let id, let text, let enter):
            try type(text, into: try control(id), enter: enter)
            note("type \"\(text)\" into \(id)\(enter ? " + return" : "")")
        case .key(let name, let mods):
            try key(name, mods)
            note("key \(Self.modifierText(mods))\(name)")
        case .scroll(let id, let dx, let dy, let precise, let mods, let at):
            let view = try control(id)
            let e = GestureEvent()
            e.delta = (dx, dy)
            e.precise = precise
            e.flags = mods
            // A scroll view's own `scrollWheel` runs an event-tracking loop that lets queued main-actor
            // work run inside this async step, which aborts the process (out-of-order task
            // deallocation). Its clip view is moved instead, by the wheel's distance as it would be.
            // A view that does not handle the wheel itself (a scroll view's document, such as the patch
            // editor's matrix) passes it to its scroll view: that one is moved the same way. So does a
            // table, the cue list: its handler is AppKit's, which only hands the wheel to its scroll view.
            let wheel = #selector(NSResponder.scrollWheel(with:))
            var handler: AnyClass = Swift.type(of: view)
            while let up = class_getSuperclass(handler),
                class_getMethodImplementation(up, wheel) == class_getMethodImplementation(handler, wheel)
            {
                handler = up
            }
            let ownWheel = Bundle(for: handler) != Bundle(for: NSView.self)
            if let scroll = (view as? NSScrollView) ?? (ownWheel ? nil : view.enclosingScrollView) {
                let clip = scroll.contentView
                let factor: CGFloat = precise ? 1 : 10
                var origin = clip.bounds.origin
                origin.y -= dy * factor * (clip.isFlipped ? 1 : -1)
                origin.x -= dx * factor
                origin = clip.constrainBoundsRect(NSRect(origin: origin, size: clip.bounds.size)).origin
                // Through the scroll view, as the wheel goes: it moves a table's header with the rows.
                // Moving the clip view alone left the header where it was.
                scroll.scroll(clip, to: origin)
                scroll.reflectScrolledClipView(clip)
                if let header = (scroll.documentView as? NSTableView)?.headerView,
                    let held = header.superview as? NSClipView, abs(held.bounds.minX - clip.bounds.minX) > 0.5
                {
                    throw Failure(
                        "the table's header stayed at x \(held.bounds.minX), its rows went to x \(clip.bounds.minX)")
                }
                note("scrolled \(id) to y \(Int(origin.y.rounded()))")
                if dx != 0 { note("  sideways to x \(Int(origin.x.rounded())), its header with it") }
                break
            }
            e.point = try windowPoint(at, in: view)
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
                // The meters' display tick, at the pace device time moves.
                rig.store.meters.tick(step)
                left -= step
            }
            note("advance \(seconds) s")
        case .device(let lose):
            guard let engine else { throw Failure("no engine") }
            guard case .success = await engine.request(lose ? .loseDevice : .restoreDevice) else {
                throw Failure("the engine refused \(lose ? "lose_device" : "restore_device")")
            }
            try await rig.advance(0)
            note(lose ? "device lost" : "device restored")
        case .resize(let size):
            window.setContentSize(size)
            note("resize \(Int(size.width))x\(Int(size.height))")
        case .focus(let which):
            if which == "preferences" {
                // The application preferences on this run's engine, their launch choice kept
                // apart from the user's.
                let defaults = UserDefaults(suiteName: "openlab-scenario-prefs-\(UUID().uuidString)")
                let p =
                    prefs
                    ?? PreferencesWindowController(
                        preferences: LaunchPreferences(defaults: defaults),
                        audio: AudioPreferencesModel(client: engine!.app))
                prefs = p
                let w = try XCTUnwrap(p.window)
                w.appearance = wc.window?.appearance
                p.reload()
                await p.refreshing?.value
                focused = w
            } else if which == "settings" {
                let sc = wc.settingsWindow
                let w = try XCTUnwrap(sc.window)
                w.title = String(format: loc("settings.title.workspace"), wc.workspaceName)
                w.appearance = wc.window?.appearance
                await sc.model.refresh()
                sc.render()
                focused = w
            } else if which == "sheet" {
                guard let sheet = wc.existingMissingFiles?.choiceSheet?.window ?? wc.pasteSheet?.window else {
                    throw Failure("no sheet is up")
                }
                focused = sheet
            } else if which == "patch-editor" {
                guard let editor = wc.settingsWindow.patchEditor, let w = editor.window else {
                    throw Failure("no patch editor was opened (click a settings-patch-edit-N first)")
                }
                w.appearance = wc.window?.appearance
                await editor.editing?.value
                focused = w
            } else {
                focused = nil
            }
            note("focus \(which)")
        case .tab(let id):
            let tabs =
                focused != nil && focused === prefs?.window
                ? prefs!.tabs
                : focused === wc.settingsWindow.patchEditor?.window && focused != nil
                    ? wc.settingsWindow.patchEditor!.tabs
                    : focused != nil && !(focused is PasteSheetWindow) ? wc.settingsWindow.tabs : wc.inspector.tabView
            let ids = tabs.tabViewItems.compactMap { $0.identifier as? String }
            guard ids.contains(id) else {
                throw Failure("no tab \"\(id)\" for this selection; tabs: \(ids.joined(separator: ", "))")
            }
            tabs.selectTabViewItem(withIdentifier: id)
            note("tab \(id)")
        case .inside(let id, let container):
            await catchUp()
            let view = try control(id)
            let box = try control(container)
            view.window?.layoutIfNeeded()
            let frame = view.convert(view.bounds, to: box)
            // A tab view's own tabs sit above its content: inside means inside the content.
            let room = (box as? NSTabView)?.contentRect ?? box.bounds
            guard room.contains(frame) else {
                throw Failure("\"\(id)\" at \(frame) is not inside \"\(container)\" \(room)")
            }
            note("inside \(id) of \(container)")
        case .setMedia(let what, let file):
            let node = try cue(what)
            guard case .success = await rig.store.send(command: .setProperty(node.id, .media(file))) else {
                throw Failure("the engine refused the file")
            }
            // Its probe answers as device time moves.
            for _ in 0..<5 { try await rig.advance(0.1) }
            note("set_media \(what): \(file)")
        case .choose(let id, let title):
            await catchUp()
            guard let popup = try control(id) as? NSPopUpButton else {
                throw Failure("\"\(id)\" is not a pop-up button")
            }
            guard !popup.isHiddenOrHasHiddenAncestor else { throw Failure("\"\(id)\" is hidden") }
            guard let item = popup.itemArray.first(where: { $0.title == title && !$0.isHidden && $0.isEnabled }) else {
                throw Failure("\"\(id)\" has no item \"\(title)\"; it has \(popup.itemTitles.filter { !$0.isEmpty })")
            }
            popup.select(item)
            guard popup.sendAction(popup.action, to: popup.target) else {
                throw Failure("\"\(id)\" has no action to send")
            }
            note("choose \(id): \(title)")
        case .findMissingFiles(let files, let name):
            guard wc.canFindMissingFiles else { throw Failure("the menu item is off: no cue is missing its file") }
            let dir = try XCTUnwrap(engine).folder.appendingPathComponent("scenario-\(UUID().uuidString)")
                .appendingPathComponent(name)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            for f in files {
                let url = dir.appendingPathComponent(f)
                try FileManager.default.createDirectory(
                    at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                try writeWAV(url, seconds: 1)
            }
            let tool = wc.missingFiles
            // Never on screen: its question is drawn over it in shots.
            tool.presentSheet = { _ in }
            tool.window?.appearance = wc.window?.appearance
            tool.pickFolder = { dir }
            await tool.chooseFolderAndSearch()
            focused = tool.window
            note("find_missing_files in \(name): \(tool.rows.count) row(s); \(tool.summaryLabel.stringValue)")
        case .shot(let name, let displays):
            await catchUp()
            await wc.inspector.timeLoops.loading?.value
            await catchUp()
            shots += 1
            let url = out.appendingPathComponent(String(format: "%02d-%@.png", shots, name))
            try shot(to: url)
            written.append(url)
            note("shot \(url.lastPathComponent) (footer: \(wc.footer.statusButton.title))")
            for display in displays {
                let beside = out.appendingPathComponent(String(format: "%02d-%@-display-%@.png", shots, name, display))
                try await displayShot(display, to: beside)
                written.append(beside)
                note("shot \(beside.lastPathComponent)")
            }
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
    private func windowPoint(_ at: CGPoint, in view: NSView, rect: NSRect? = nil) throws -> NSPoint {
        let b = rect ?? view.bounds
        let y = view.isFlipped ? b.minY + at.y * b.height : b.maxY - at.y * b.height
        let local = NSPoint(x: b.minX + at.x * b.width, y: y)
        let p = view.convert(local, to: nil)
        guard let content = window.contentView, content.frame.contains(p) else {
            throw Failure("that point (\(Self.text(p))) is outside the window's content: resize the window")
        }
        if view.visibleRect.isEmpty { throw Failure("the control is scrolled out of view or has no size") }
        if rect != nil, !view.visibleRect.contains(local) {
            throw Failure(
                "that point (\(Self.text(p))) is outside the header or row's view or scrolled out of it: "
                    + "fractions may go past the header cell but must stay inside the table's header")
        }
        return p
    }

    /// The view and the rectangle (its coordinates; nil: all of it) a step aims at: the control, or
    /// a column header or a cue's row of the table `id` names.
    private func aim(_ id: String, _ part: Part?) throws -> (NSView, NSRect?) {
        let view = try control(id)
        guard let part else { return (view, nil) }
        func outline() throws -> NSOutlineView {
            guard let table = view as? NSOutlineView else {
                throw Failure("\"\(id)\" is not an outline: no headers or rows")
            }
            return table
        }
        switch part {
        case .segment(let name): return try segment(name, in: view, id: id)
        case .header(let title):
            let table = try outline()
            guard let header = table.headerView else { throw Failure("\"\(id)\" has no header") }
            let titles = table.tableColumns.map { $0.title.isEmpty ? $0.identifier.rawValue : $0.title }
            guard let i = titles.firstIndex(of: title) else {
                throw Failure("no column header \"\(title)\" in \"\(id)\"; headers: \(titles.joined(separator: ", "))")
            }
            return (header, header.headerRect(ofColumn: i))
        case .row(let what):
            let table = try outline()
            let node = try cue(what)
            let row = table.row(forItem: node)
            guard row >= 0 else { throw Failure("cue \(what) has no row in \"\(id)\" (inside a closed group?)") }
            return (table, table.rect(ofRow: row))
        }
    }

    /// The segmented control in `view` with a segment named `name` (its tooltip, else its label), and
    /// that segment's rectangle.
    private func segment(_ name: String, in view: NSView, id: String) throws -> (NSView, NSRect) {
        var bars: [NSSegmentedControl] = []
        func walk(_ v: NSView) {
            if let bar = v as? NSSegmentedControl, !v.isHiddenOrHasHiddenAncestor { bars.append(bar) }
            v.subviews.forEach(walk)
        }
        walk(view)
        var names: [String] = []
        for bar in bars {
            for i in 0..<bar.segmentCount {
                let n = bar.toolTip(forSegment: i).flatMap { $0.isEmpty ? nil : $0 } ?? bar.label(forSegment: i) ?? ""
                names.append(n)
                guard n == name else { continue }
                guard let rects = Self.segmentRects(of: bar) else {
                    throw Failure("AppKit gives no layout for the segments of \"\(id)\"")
                }
                return (bar, rects[i])
            }
        }
        throw Failure("no segment \"\(name)\" in \"\(id)\"; segments: \(names.joined(separator: ", "))")
    }

    /// Each segment's rectangle in `bar`'s coordinates, as AppKit lays the segments out: the frames of
    /// the accessibility elements its cell gives them (no public call gives a segment's rectangle, and
    /// working them out from widths and dividers drifts between macOS versions). Nil when the cell
    /// gives no element a segment or a frame outside the control (it is not laid out yet).
    static func segmentRects(of bar: NSSegmentedControl) -> [NSRect]? {
        guard let window = bar.window else { return nil }
        let elements = (bar.cell?.accessibilityChildren() ?? []).compactMap { $0 as? NSAccessibilityElement }
        guard elements.count == bar.segmentCount else { return nil }
        let rects = elements.map { bar.convert(window.convertFromScreen($0.accessibilityFrame()), from: nil) }
        let room = bar.bounds.insetBy(dx: -1, dy: -1)
        return rects.allSatisfy { room.contains($0) } ? rects : nil
    }

    /// The column a header part names, to log its width around a drag.
    private func columnOf(_ id: String, _ part: Part?) -> NSTableColumn? {
        guard case .header(let title)? = part, let table = try? control(id) as? NSTableView else { return nil }
        return table.tableColumns.first { ($0.title.isEmpty ? $0.identifier.rawValue : $0.title) == title }
    }

    /// The tooltip AppKit would show for `view` with the pointer at window point `p`: a header cell's,
    /// a segment's, else the nearest view up from it that has one.
    private func tooltip(of view: NSView, at p: NSPoint) -> String? {
        let local = view.convert(p, from: nil)
        if let header = view as? NSTableHeaderView, let table = header.tableView {
            let i = header.column(at: local)
            if i >= 0, let tip = table.tableColumns[i].headerToolTip, !tip.isEmpty { return tip }
        }
        // The segment under the pointer, from where AppKit lays the segments out; a divider is none's.
        if let bar = view as? NSSegmentedControl, let rects = Self.segmentRects(of: bar),
            let i = rects.firstIndex(where: { $0.contains(local) }), let tip = bar.toolTip(forSegment: i), !tip.isEmpty
        {
            return tip
        }
        var up: NSView? = view
        while let v = up {
            if let tip = v.toolTip, !tip.isEmpty { return tip }
            up = v.superview
        }
        return nil
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
            // An empty text empties the field, as Delete over the selection does.
            if text.isEmpty { field.deleteBackward(nil) }
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
        // A focused sheet is shot over its window, as it shows on screen.
        let (shown, sheet) = sheetOver(window)
        guard let frame = shown.contentView?.superview else { throw Failure("the window has no frame view") }
        frame.layoutSubtreeIfNeeded()
        var rep = try renderView(frame, scale: 2, background: shown.backgroundColor)
        if let sheet { rep = try overlay(sheet, on: rep, over: shown) }
        try drawFloating(on: rep, in: shown)
        guard let png = rep.representation(using: .png, properties: [:]) else { throw Failure("PNG encoding failed") }
        try png.write(to: url)
    }

    /// The open pop-up menu and the hovered control's tooltip, drawn over the window's picture (AppKit
    /// shows both in windows of their own, which a shot of the window's view tree cannot reach). The
    /// picture is of the menu's items and the tooltip's text, not AppKit's own drawing of them.
    private func drawFloating(on rep: NSBitmapImageRep, in shown: NSWindow) throws {
        guard openMenu != nil || tip != nil else { return }
        guard let context = NSGraphicsContext(bitmapImageRep: rep) else { throw Failure("no context") }
        let area = shown.contentView?.frame ?? .zero
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        context.cgContext.scaleBy(x: 2, y: 2)
        defer { NSGraphicsContext.restoreGraphicsState() }
        shown.effectiveAppearance.performAsCurrentDrawingAppearance {
            if let popup = openMenu { drawMenu(of: popup, within: area) }
            if let tip { drawTip(tip.text, at: tip.at, within: area) }
        }
    }

    private func drawMenu(of popup: NSPopUpButton, within area: NSRect) {
        let font = NSFont.menuFont(ofSize: 0)
        let items = Self.shownItems(of: popup)
        let rowHeight: CGFloat = 22
        let heights = items.map { $0.isSeparatorItem ? CGFloat(9) : rowHeight }
        let widest = items.map { ($0.title as NSString).size(withAttributes: [.font: font]).width }.max() ?? 0
        let button = popup.convert(popup.bounds, to: nil)
        let size = NSSize(width: max(button.width, widest + 44), height: heights.reduce(0, +) + 10)
        // A pop-up's menu opens with the chosen item over the button; a pull-down's opens below it.
        var above: CGFloat = 5
        if popup.pullsDown {
            above = -button.height
        } else if let chosen = popup.selectedItem, let i = items.firstIndex(of: chosen) {
            above += heights[..<i].reduce(0, +) - (rowHeight - button.height) / 2
        }
        var box = NSRect(x: button.minX, y: button.maxY + above - size.height, width: size.width, height: size.height)
        box.origin.x = min(max(area.minX + 4, box.minX), area.maxX - size.width - 4)
        box.origin.y = min(max(area.minY + 4, box.minY), area.maxY - size.height - 4)
        let shape = NSBezierPath(roundedRect: box, xRadius: 6, yRadius: 6)
        NSColor.windowBackgroundColor.setFill()
        shape.fill()
        NSColor.separatorColor.setStroke()
        shape.stroke()
        var top = box.maxY - 5
        for (item, height) in zip(items, heights) {
            top -= height
            if item.isSeparatorItem {
                NSColor.separatorColor.setFill()
                NSRect(x: box.minX + 8, y: top + height / 2, width: box.width - 16, height: 1).fill()
                continue
            }
            let colour = item.isEnabled ? NSColor.labelColor : NSColor.disabledControlTextColor
            let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: colour]
            let y = top + (height - font.boundingRectForFont.height) / 2 - font.descender / 2
            if item.state == .on {
                ("\u{2713}" as NSString).draw(at: NSPoint(x: box.minX + 8, y: y), withAttributes: attributes)
            }
            (item.title as NSString).draw(at: NSPoint(x: box.minX + 26, y: y), withAttributes: attributes)
            if item.submenu != nil {
                ("\u{25B8}" as NSString).draw(at: NSPoint(x: box.maxX - 16, y: y), withAttributes: attributes)
            }
        }
    }

    private func drawTip(_ text: String, at p: NSPoint, within area: NSRect) {
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.toolTipsFont(ofSize: 0), .foregroundColor: NSColor.black,
        ]
        let size = (text as NSString).size(withAttributes: attributes)
        var box = NSRect(x: p.x + 4, y: p.y - 12 - size.height - 6, width: size.width + 12, height: size.height + 6)
        box.origin.x = min(max(area.minX + 2, box.minX), area.maxX - box.width - 2)
        box.origin.y = max(area.minY + 2, box.minY)
        let shape = NSBezierPath(roundedRect: box, xRadius: 3, yRadius: 3)
        NSColor(calibratedRed: 1, green: 1, blue: 0.87, alpha: 1).setFill()
        shape.fill()
        NSColor.separatorColor.setStroke()
        shape.stroke()
        (text as NSString).draw(at: NSPoint(x: box.minX + 6, y: box.minY + 3), withAttributes: attributes)
    }

    /// What a virtual display shows (every stage on it, stacked, as the stage host's window draws
    /// them), read back through the stage host's test probe at a grid of points and drawn as blocks.
    private func displayShot(_ id: String, to url: URL) async throws {
        guard let engine else { throw Failure("no engine") }
        guard case .success(let listed) = await engine.request(.listDisplays),
            case .displays(let all, _)? = listed.data,
            let display = all.first(where: { $0.id == id })
        else {
            throw Failure("the stage host lists no display \"\(id)\"")
        }
        let (cols, rows) = Self.displayGrid(width: Double(display.width), height: Double(display.height))
        var colours: [[UInt8]] = []
        let points = (0..<rows).flatMap { r in
            (0..<cols).map { c in [(Double(c) + 0.5) / Double(cols), (Double(r) + 0.5) / Double(rows)] }
        }
        for start in stride(from: 0, to: points.count, by: Self.probePoints) {
            let chunk = Array(points[start..<min(points.count, start + Self.probePoints)])
            guard case .success(let ok) = await engine.request(.displayProbe(display: id, points: chunk)),
                case .stageProbe(_, let got)? = ok.data, got.count == chunk.count
            else { throw Failure("the stage host did not answer the probe of display \"\(id)\"") }
            colours += got
        }
        let block = 8
        guard
            let rep = NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: cols * block, pixelsHigh: rows * block, bitsPerSample: 8,
                samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0,
                bitsPerPixel: 0),
            let context = NSGraphicsContext(bitmapImageRep: rep)
        else { throw Failure("no bitmap for the display shot") }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        for (i, c) in colours.enumerated() where c.count == 4 {
            NSColor(
                deviceRed: CGFloat(c[0]) / 255, green: CGFloat(c[1]) / 255, blue: CGFloat(c[2]) / 255,
                alpha: CGFloat(c[3]) / 255
            ).setFill()
            NSRect(x: (i % cols) * block, y: (rows - 1 - i / cols) * block, width: block, height: block).fill()
        }
        NSGraphicsContext.restoreGraphicsState()
        guard let png = rep.representation(using: .png, properties: [:]) else { throw Failure("PNG encoding failed") }
        try png.write(to: url)
    }

    /// The most points one probe reads.
    private static let probePoints = 64

    /// A grid of about a thousand sample points in the display's shape (at most 64 a side).
    static func displayGrid(width: Double, height: Double) -> (cols: Int, rows: Int) {
        let aspect = max(width, 1) / max(height, 1)
        let cols = min(64, max(1, Int((1024 * aspect).squareRoot())))
        let rows = min(64, max(1, Int((Double(cols) / aspect).rounded())))
        return (cols, rows)
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
    private func overlay(_ sheet: NSWindow, on rep: NSBitmapImageRep, over base: NSWindow) throws
        -> NSBitmapImageRep
    {
        sheet.appearance = base.appearance
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
        case .drag(let id, _, _, _, let part): return "drag \(id)\(partText(part))"
        case .hover(let id, _, let part): return "hover \(id)\(partText(part))"
        case .openMenu(let id): return "open_menu \(id)"
        case .menuCommand(let name): return "menu_command \(name)"
        case .type(let id, _, _): return "type \(id)"
        case .key(let k, let m): return "key \(modifierText(m))\(k)"
        case .scroll(let id, _, _, _, _, _): return "scroll \(id)"
        case .magnify(let id, _, _): return "magnify \(id)"
        case .advance(let s): return "advance \(s)"
        case .device(let lose): return "device \(lose ? "lose" : "restore")"
        case .resize(let s): return "resize \(Int(s.width))x\(Int(s.height))"
        case .tab(let t): return "tab \(t)"
        case .focus(let w): return "focus \(w)"
        case .shot(let s, _): return "shot \(s)"
        case .inside(let id, let of): return "inside \(id) of \(of)"
        case .setMedia(let cue, let file): return "set_media \(cue) \(file)"
        case .choose(let id, let item): return "choose \(id) \(item)"
        case .findMissingFiles(let files, _): return "find_missing_files \(files.count) file(s)"
        }
    }

    private static func partText(_ part: Part?) -> String {
        switch part {
        case nil: return ""
        case .header(let t)?: return " header \(t)"
        case .row(let r)?: return " row \(r)"
        case .segment(let n)?: return " segment \(n)"
        }
    }

    /// The items a pop-up's open menu lists: not the hidden ones, nor a pull-down's title item.
    private static func shownItems(of popup: NSPopUpButton) -> [NSMenuItem] {
        (popup.menu?.items ?? []).dropFirst(popup.pullsDown ? 1 : 0).filter { !$0.isHidden }
    }

    private static func menuLine(_ item: NSMenuItem) -> String {
        if item.isSeparatorItem { return "-" }
        return (item.state == .on ? "[x] " : "") + item.title + (item.isEnabled ? "" : " (off)")
            + (item.submenu != nil ? " >" : "")
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

    /// Reproduce (the scenario window's off-screen layout): 1000x700, cue 1 (30 s) played, the
    /// sidebar on Active Cues, its Reset All pressed. The inspector is shorter than the Basics tab's
    /// left column, and the column slid up over the tabs, Number and Duration out of sight. It must
    /// keep its top and be cut at the bottom.
    func testResetAllWithActiveCuesShownKeepsTheBasicsFieldsInsideTheTab() async throws {
        let runner = ScenarioRunner(
            scenario: try scenario(
                """
                {"show": "Demo", "media": {"media/a.wav": 30}, "window": {"width": 1000, "height": 700},
                 "steps": [{"select": "1"}, {"key": "space"}, {"advance": 1.0},
                           {"click": "toggle-sidebar"}, {"click": "sidebar-tabs", "at": [0.5, 0.5]}, {"select": "2.1"}, {"shot": "active"},
                           {"click": "transport-reset"}, {"advance": 0.5}, {"shot": "after-reset"},
                           {"inside": "inspector-number", "of": "inspector-tabs"},
                           {"inside": "inspector-pre-wait", "of": "inspector-tabs"}]}
                """), out: out)
        do {
            try await runner.run()
        } catch {
            let log = (try? String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)) ?? ""
            XCTFail("\(error)\n\(log)")
        }
    }

    /// A scroll step over the cue list scrolls it as the wheel does: rows further down come into view.
    func testAScrollStepOverTheCueListScrollsIt() async throws {
        let runner = ScenarioRunner(
            scenario: try scenario(
                """
                {"show": "Demo", "window": {"width": 900, "height": 400},
                 "steps": [{"shot": "top"}, {"scroll": "cue-list", "dy": -400, "at": [0.5, 0.05]}, {"shot": "scrolled"}]}
                """), out: out)
        do { try await runner.run() } catch {
            let log = (try? String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)) ?? ""
            return XCTFail("\(error)\n\(log)")
        }
        let log = try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
        let line = try XCTUnwrap(log.split(separator: "\n").first { $0.hasPrefix("scrolled cue-list to y ") }, log)
        let y = try XCTUnwrap(Int(line.dropFirst("scrolled cue-list to y ".count)), String(line))
        XCTAssertGreaterThan(y, 0, "the first rows went out of view: \(log)")
        let shots = try runner.written.map { try Data(contentsOf: $0) }
        XCTAssertEqual(shots.count, 2, log)
        XCTAssertNotEqual(shots.first, shots.last, "the list shows other rows after the scroll")
    }

    /// A sideways scroll step over the cue list moves its column header with the rows, as the wheel does
    /// (the step checks the two stay together and fails otherwise).
    func testASidewaysScrollStepMovesTheCueListsHeaderWithItsRows() async throws {
        let runner = ScenarioRunner(
            scenario: try scenario(
                """
                {"show": "Demo", "window": {"width": 800, "height": 500},
                 "steps": [{"click": "toggle-sidebar"}, {"scroll": "cue-list", "dx": -60, "at": [0.5, 0.05]},
                           {"shot": "sideways"}]}
                """), out: out)
        do { try await runner.run() } catch {
            let log = (try? String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)) ?? ""
            return XCTFail("\(error)\n\(log)")
        }
        XCTAssertEqual(runner.written.count, 1)
        let log = try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
        let line = try XCTUnwrap(log.split(separator: "\n").first { $0.contains("sideways to x ") }, log)
        XCTAssertFalse(line.contains("to x 0,"), "the list moved sideways: \(line)")
    }

    func testAnUnknownIdentifierFailsListingTheIdentifiersShown() async throws {
        let message = try await failure(of: #"{"show": "Demo", "steps": [{"click": "no-such-button"}]}"#)
        XCTAssertTrue(message.hasPrefix("step 1 (click no-such-button): no control with identifier"), message)
        XCTAssertTrue(message.contains("go-button"), "the identifiers in the window are listed: \(message)")
    }

    func testAMissingShowFailsNamingTheTestShows() async throws {
        let message = try await failure(of: #"{"show": "Nope", "steps": []}"#)
        // The list is every show in TestShows; a new show must not break this test.
        XCTAssertTrue(message.contains("show \"Nope\" not found: test shows are Demo, "), message)
        XCTAssertTrue(message.contains("Three"), message)
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

    func testPopUpHoverCommandHeaderAndDisplayStepsParse() throws {
        let s = try scenario(
            """
            {"show": "Video", "displays": "V1:1920x1080,V2:1280x720", "steps": [
              {"open_menu": "fade-geometry-mode"}, {"choose": "fade-geometry-mode", "item": "Relative"},
              {"hover": "cue-toolbar", "at": [0.2, 0.5]}, {"hover": "cue-list", "header": "Name", "at": [1.5, 0.5]},
              {"menu_command": "undo"}, {"menu_command": "paste"},
              {"drag": "cue-list", "header": "Name", "from": [0.5, 0.5], "to": [-3, 0.5]},
              {"hover": "cue-list", "row": "2", "at": [0.1, 4]},
              {"shot": "both", "display": ["V1", "V2"]}, {"shot": "one", "display": "V1"}]}
            """)
        XCTAssertEqual(s.steps.count, 10)
        guard case .drag(_, _, let to, _, let part) = s.steps[6] else { return XCTFail("\(s.steps[6])") }
        XCTAssertEqual(part, .header("Name"))
        XCTAssertEqual(to, CGPoint(x: -3, y: 0.5))
        guard case .shot(_, let displays) = s.steps[8] else { return XCTFail("\(s.steps[8])") }
        XCTAssertEqual(displays, ["V1", "V2"])
    }

    func testPopUpHoverCommandHeaderAndDisplayStepsRefuseBadInput() throws {
        func refused(_ step: String, _ text: String, displays: String = "") throws {
            let json = #"{"show": "Demo", \#(displays) "steps": [\#(step)]}"#
            XCTAssertThrowsError(try scenario(json), step) { error in
                XCTAssertTrue("\(error)".contains(text), "\(step): \(error)")
            }
        }
        try refused(
            #"{"menu_command": "select_all"}"#,
            "unknown command \"select_all\"; known: copy, cut, delete, paste, redo, undo")
        try refused(#"{"menu_command": "undo", "at": [0, 0]}"#, "\"menu_command\" does not take \"at\"")
        try refused(#"{"open_menu": ""}"#, "\"open_menu\" must be a non-empty string")
        try refused(#"{"hover": "x", "at": [1.5, 0.5]}"#, "fractions 0 to 1 of the control")
        try refused(
            #"{"hover": "x", "header": "a", "row": "1"}"#, "one \"header\", \"row\" or \"segment\", not header and row")
        try refused(#"{"hover": "x", "row": "1", "segment": "Wait"}"#, "not row and segment")
        try refused(#"{"drag": "x", "segment": "Wait"}"#, "\"drag\" does not take \"segment\"")
        try refused(#"{"drag": "x", "header": "a", "to": [0, 99]}"#, "fractions -20 to 20 of the header or row")
        try refused(#"{"drag": "x", "row": "1"}"#, "\"drag\" does not take \"row\"")
        try refused(#"{"drag": "x", "to": [0, 2]}"#, "fractions 0 to 1 of the control")
        try refused(#"{"click": "x", "header": "a"}"#, "\"click\" does not take \"header\"")
        try refused(
            #"{"shot": "a", "display": "V3"}"#, "display \"V3\" is not one of the scenario's \"displays\" (V1)",
            displays: #""displays": "V1:1920x1080","#)
        try refused(#"{"shot": "a", "display": "V1"}"#, "(none)")
        try refused(#"{"shot": "a", "display": []}"#, "\"display\" must be a display id or a list of them")
    }

    func testTheDisplayGridFollowsTheDisplaysShapeWithinOneProbeEach() {
        for (w, h) in [(1920.0, 1080.0), (1080.0, 1920.0), (3840.0, 1080.0), (1.0, 16384.0), (16384.0, 1.0)] {
            let (cols, rows) = ScenarioRunner.displayGrid(width: w, height: h)
            XCTAssertTrue((1...64).contains(cols) && (1...64).contains(rows), "\(w)x\(h): \(cols)x\(rows)")
        }
        let (cols, rows) = ScenarioRunner.displayGrid(width: 1920, height: 1080)
        XCTAssertEqual(Double(cols) / Double(rows), 16.0 / 9.0, accuracy: 0.1)
    }

    func testMenuCommandsReachTheCuesAndAnImpossibleOneStopsTheRun() async throws {
        // Delete takes the selected cue (a later step cannot find it), Undo brings it back.
        let deleted = try await failure(
            of: #"{"show": "Demo", "steps": [{"select": "1"}, {"menu_command": "delete"}, {"select": "1"}]}"#)
        XCTAssertTrue(deleted.contains("step 3 (select 1): no cue numbered or named \"1\""), deleted)
        let back = ScenarioRunner(
            scenario: try scenario(
                #"{"show": "Demo", "steps": [{"select": "1"}, {"menu_command": "delete"}, {"menu_command": "undo"}, {"select": "1"}]}"#
            ), out: out)
        try await back.run()
        let log = try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
        XCTAssertTrue(log.contains("menu_command undo: AppDelegate"), log)
        // Redo with nothing undone is greyed out in the menu, so the step stops.
        let off = try await failure(of: #"{"show": "Demo", "steps": [{"menu_command": "redo"}]}"#)
        XCTAssertTrue(off.contains("Edit > redo is off"), off)
    }

    func testHoverMenuAndHeaderStepsStopWithWhatIsThere() async throws {
        let noHeader = try await failure(
            of: #"{"show": "Demo", "steps": [{"hover": "cue-list", "header": "Nope"}]}"#)
        XCTAssertTrue(noHeader.contains("no column header \"Nope\" in \"cue-list\"; headers: "), noHeader)
        let noRow = try await failure(of: #"{"show": "Demo", "steps": [{"hover": "cue-list", "row": "99"}]}"#)
        XCTAssertTrue(noRow.contains("no cue numbered or named \"99\""), noRow)
        let notPopUp = try await failure(of: #"{"show": "Demo", "steps": [{"open_menu": "toggle-sidebar"}]}"#)
        XCTAssertTrue(notPopUp.contains("\"toggle-sidebar\" is not a pop-up button"), notPopUp)
        let noSegment = try await failure(
            of: #"{"show": "Demo", "steps": [{"hover": "cue-toolbar", "segment": "Nope"}]}"#)
        XCTAssertTrue(noSegment.contains("no segment \"Nope\" in \"cue-toolbar\"; segments: Group, Audio, "), noSegment)
        let noTip = try await failure(of: #"{"show": "Demo", "steps": [{"hover": "cue-list"}]}"#)
        XCTAssertTrue(noTip.contains("no tooltip at"), noTip)
    }

    /// Runs the scenario and returns its `steps.log`.
    private func runLog(of json: String) async throws -> String {
        try await ScenarioRunner(scenario: try scenario(json), out: out).run()
        return try String(contentsOf: out.appendingPathComponent("steps.log"), encoding: .utf8)
    }

    func testHoverReadsTheTooltipOfTheHeaderAndTheSegmentUnderThePointer() async throws {
        let log = try await runLog(
            of: """
                {"show": "Demo", "steps": [
                  {"hover": "cue-list", "header": "Duration"},
                  {"hover": "cue-toolbar", "at": [0.06, 0.5]},
                  {"hover": "cue-toolbar", "segment": "Wait"},
                  {"hover": "cue-toolbar", "segment": "Wait", "at": [0.97, 0.5]},
                  {"hover": "cue-toolbar", "segment": "Memo", "at": [0.03, 0.5]}]}
                """)
        let tips = log.split(separator: "\n").compactMap { line -> String? in
            guard let r = line.range(of: "tooltip \"") else { return nil }
            return String(line[r.upperBound...].prefix { $0 != "\"" })
        }
        XCTAssertEqual(tips.count, 5, log)
        XCTAssertEqual(tips[0], loc("cuelist.time.showRemaining"), log)
        XCTAssertEqual(tips[1], "Audio", log)
        // The middle of a segment, a point just inside its right edge, and one just past the divider.
        XCTAssertEqual(tips[2], "Wait", log)
        XCTAssertEqual(tips[3], "Wait", log)
        XCTAssertEqual(tips[4], "Memo", log)
        XCTAssertTrue(log.contains("drawn by the runner, not AppKit"), log)
    }

    /// The segment rectangles the runner aims at and reads tooltips by are AppKit's layout: each as wide
    /// as the toolbar sets it, left to right, apart, inside the control; none before it is laid out.
    func testSegmentRectsAreTheSegmentsAsLaidOut() throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1000, height: 60), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let toolbar = CueToolbarView(frame: window.contentView?.bounds ?? .zero)
        window.contentView?.addSubview(toolbar)
        let bar = try XCTUnwrap(toolbar.segmentedControls.last)
        XCTAssertNil(ScenarioRunner.segmentRects(of: NSSegmentedControl()), "a control in no window has none")
        window.orderFront(nil)
        window.layoutIfNeeded()
        window.displayIfNeeded()
        let rects = try XCTUnwrap(ScenarioRunner.segmentRects(of: bar))
        XCTAssertEqual(rects.count, bar.segmentCount)
        for (i, r) in rects.enumerated() {
            XCTAssertEqual(r.width, bar.width(forSegment: i), accuracy: 0.5, "segment \(i): \(rects)")
            XCTAssertTrue(bar.bounds.insetBy(dx: -1, dy: -1).contains(r), "segment \(i): \(r) in \(bar.bounds)")
            if i > 0 { XCTAssertGreaterThanOrEqual(r.minX, rects[i - 1].maxX, "segment \(i) after \(i - 1)") }
        }
    }

    func testOpenMenuListsTheItemsWithTheChosenOneTickedAndLeavesOutAPullDownsTitle() async throws {
        let log = try await runLog(
            of: """
                {"show": "VideoSound", "displays": "V1:1920x1080", "media": {"media/talk.mov": 4}, "steps": [
                  {"select": "5"}, {"tab": "fadeGeometry"},
                  {"open_menu": "fade-geometry-mode"},
                  {"choose": "fade-geometry-mode", "item": "Relative"},
                  {"open_menu": "fade-geometry-mode"},
                  {"focus": "settings"}, {"tab": "audio"}, {"click": "settings-patch-edit-1"},
                  {"focus": "patch-editor"}, {"open_menu": "patch-editor-copy"}]}
                """)
        XCTAssertTrue(log.contains("open_menu fade-geometry-mode: [x] Absolute | Relative"), log)
        XCTAssertTrue(log.contains("open_menu fade-geometry-mode: Absolute | [x] Relative"), log)
        let pullDown = try XCTUnwrap(log.split(separator: "\n").first { $0.hasPrefix("open_menu patch-editor-copy") })
        XCTAssertTrue(pullDown.contains(loc("patchEditor.copyDefaults")), "\(pullDown)")
        XCTAssertFalse(pullDown.contains(loc("patchEditor.copyRouting")), "the title item is not listed: \(pullDown)")
    }

    func testOpenMenuOfAPopUpThatIsNotShownStopsTheRun() async throws {
        // The Geometry tab's pop-up is not in the window until a Fade cue is selected.
        let message = try await failure(
            of: #"{"show": "Demo", "steps": [{"open_menu": "fade-geometry-mode"}]}"#)
        XCTAssertTrue(message.contains("no control with identifier \"fade-geometry-mode\""), message)
    }

    func testAHeaderDragOnAResizableColumnResizesItAndOnTheNameColumnDoesNot() async throws {
        let log = try await runLog(
            of: """
                {"show": "Demo", "steps": [
                  {"drag": "cue-list", "header": "Pre-Wait", "from": [0.999, 0.5], "to": [1.4, 0.5]},
                  {"drag": "cue-list", "header": "Name", "from": [0.999, 0.5], "to": [0.5, 0.5]}]}
                """)
        let widths = log.split(separator: "\n").compactMap { line -> (Int, Int)? in
            guard let r = line.range(of: ", width ") else { return nil }
            let parts = line[r.upperBound...].components(separatedBy: " -> ").compactMap { Int($0) }
            return parts.count == 2 ? (parts[0], parts[1]) : nil
        }
        XCTAssertEqual(widths.count, 2, log)
        XCTAssertGreaterThan(widths[0].1, widths[0].0, "Pre-Wait was widened: \(log)")
        XCTAssertEqual(widths[1].0, widths[1].1, "Name takes no user resize: \(log)")
    }

    func testAShotWithADisplayWritesItsCompositeBesideTheWindowShotAndShowsTheCardWhilePlaying() async throws {
        let log = try await runLog(
            of: """
                {"show": "Video", "displays": "V1:1920x1080",
                 "settings": {"settings_version": 1, "workspaces": {"00000000-0000-0000-0000-0000000000f0": {
                   "patches": {}, "stages": {"00000000-0000-0000-0000-0000000000f1":
                     {"display": "V1", "name": "Stage 1", "width": 1920, "height": 1080}}}}},
                 "media": {"media/clip.mov": 4, "media/still.png": 1, "media/card.jpg": 1,
                   "media/A very long name for a projection clip used in the second act.mov": 2},
                 "steps": [{"advance": 0.1}, {"select": "1"}, {"shot": "idle", "display": "V1"},
                   {"key": "space"}, {"advance": 1.5}, {"shot": "playing", "display": "V1"}]}
                """)
        XCTAssertTrue(log.contains("shot 02-playing-display-V1.png"), log)
        func blocks(_ name: String) throws -> NSBitmapImageRep {
            let data = try Data(contentsOf: out.appendingPathComponent(name))
            return try XCTUnwrap(NSBitmapImageRep(data: data), name)
        }
        let (cols, rows) = ScenarioRunner.displayGrid(width: 1920, height: 1080)
        func lit(_ rep: NSBitmapImageRep) -> Int {
            var n = 0
            for y in 0..<rep.pixelsHigh {
                for x in 0..<rep.pixelsWide {
                    guard let c = rep.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) else { continue }
                    if c.redComponent + c.greenComponent + c.blueComponent > 0.3 { n += 1 }
                }
            }
            return n
        }
        let idle = try blocks("01-idle-display-V1.png")
        let playing = try blocks("02-playing-display-V1.png")
        XCTAssertEqual(playing.pixelsWide, cols * 8)
        XCTAssertEqual(playing.pixelsHigh, rows * 8)
        XCTAssertEqual(lit(idle), 0, "nothing is shown before GO")
        XCTAssertGreaterThan(lit(playing), playing.pixelsWide * playing.pixelsHigh / 2, "the test card fills the stage")
        XCTAssertTrue(FileManager.default.fileExists(atPath: out.appendingPathComponent("02-playing.png").path))
    }

    func testMenuCommandWhileTextIsBeingEditedStopsTheRun() async throws {
        let message = try await failure(
            of:
                #"{"show": "Demo", "steps": [{"select": "1"}, {"type": "inspector-name", "text": "x", "enter": false}, {"menu_command": "copy"}]}"#
        )
        XCTAssertTrue(message.contains("text is being edited"), message)
    }
}
