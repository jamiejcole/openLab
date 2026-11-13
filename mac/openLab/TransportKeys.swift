// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The transport commands in the Transport menu, in menu order.
enum TransportCommand: String, CaseIterable, Codable, Sendable {
    case go, stopAll, pauseAll, resumeAll, resetAll, panic
    case loadSelected, startSelected, pauseResumeSelected, stopSelected, panicSelected, resetSelected, previewSelected

    var titleKey: String { "menu.transport.\(rawValue)" }
}

/// A key and modifiers in the form `NSMenuItem` takes (`key` lowercase; modifiers in `mask`).
struct KeyChord: Hashable, Sendable {
    var key: String
    var mask: UInt

    static let relevantMods: NSEvent.ModifierFlags = [.command, .shift, .control, .option]

    init(_ key: String, _ mods: NSEvent.ModifierFlags = []) {
        self.key = key.lowercased()
        mask = mods.intersection(Self.relevantMods).rawValue
    }

    var modifiers: NSEvent.ModifierFlags { NSEvent.ModifierFlags(rawValue: mask) }

    static let escape = "\u{1B}"
    static let up = String(utf16CodeUnits: [unichar(NSUpArrowFunctionKey)], count: 1)
    static let down = String(utf16CodeUnits: [unichar(NSDownArrowFunctionKey)], count: 1)
    static let left = String(utf16CodeUnits: [unichar(NSLeftArrowFunctionKey)], count: 1)
    static let right = String(utf16CodeUnits: [unichar(NSRightArrowFunctionKey)], count: 1)

    /// Show-file key names with no plain character: what `NSEvent` reports for them.
    static let names: [String: String] = {
        var n = [
            "space": " ", "escape": escape, "up": up, "down": down, "left": left, "right": right,
            "return": "\r", "tab": "\t",
        ]
        for i in 1...12 { n["f\(i)"] = String(utf16CodeUnits: [unichar(NSF1FunctionKey + i - 1)], count: 1) }
        return n
    }()

    /// The chord of a pressed key (`charactersIgnoringModifiers` and the event's modifiers).
    /// Nil for a key held with fn (fn-F is not F, and belongs to the menus). Arrows and F-keys always
    /// carry the fn flag, whoever presses them: their characters are in the function-key range.
    init?(event: NSEvent) {
        guard let key = event.charactersIgnoringModifiers, !key.isEmpty else { return nil }
        let isFunctionKey = key.unicodeScalars.first.map { (0xF700...0xF8FF).contains($0.value) } ?? false
        if event.modifierFlags.contains(.function), !isFunctionKey { return nil }
        self.init(key, event.modifierFlags)
    }

    /// A show-file hotkey (`{key, modifiers}`, docs/file-format.md section 5.1): `key` is a
    /// lower-case name ("space", "escape", "down", "[", "a"). Names the app has no key for
    /// (F13 and up, media keys) give nil.
    init?(hotkey: JSONValue) {
        guard let name = hotkey["key"]?.stringValue else { return nil }
        guard let key = Self.names[name] ?? (name.count == 1 ? name : nil) else { return nil }
        var mods: NSEvent.ModifierFlags = []
        let flags: [(String, NSEvent.ModifierFlags)] = [
            ("shift", .shift), ("control", .control), ("alt", .option), ("command", .command),
        ]
        for (field, flag) in flags where hotkey["modifiers"]?[field] == .bool(true) { mods.insert(flag) }
        self.init(key, mods)
    }

    /// Plain and Shift/Option chords insert characters, so a text field being edited must keep them.
    var typesCharacters: Bool { modifiers.isDisjoint(with: [.command, .control]) }
}

enum KeyBindError: Error, Equatable {
    /// Esc is panic and cannot move (QLab: fixed safety key); the Up and Down arrows are never allowed.
    case reserved
    case duplicate(of: WorkspaceKey)
    case cueHotkey
    /// A menu command (Save, Quit, ...) already owns the chord.
    case menuKey
    /// The key has no show-file name (Delete, media keys, ...).
    case unsupported
    /// A cue hotkey on one of the workspace's cue-editing keys (QLab refuses a key that has a function).
    case editKey
}

extension WorkspaceKey {
    /// The menu command a transport or selected-cue key performs; nil for a cue-editing key.
    var command: TransportCommand? {
        switch self {
        case .go: return .go
        case .stopAll: return .stopAll
        case .pauseAll: return .pauseAll
        case .resumeAll: return .resumeAll
        case .loadSelected: return .loadSelected
        case .pauseResumeSelected: return .pauseResumeSelected
        case .startSelected: return .startSelected
        case .stopSelected: return .stopSelected
        case .resetSelected: return .resetSelected
        case .previewSelected: return .previewSelected
        case .panicSelected: return .panicSelected
        default: return nil
        }
    }

    /// What a cue-editing key does to the selected cues; nil for a transport key.
    var editKey: TransportKeys.EditKey? {
        switch self {
        case .editNumber: return .cell("number")
        case .editName: return .cell("name")
        case .editNotes: return .notes
        case .editTarget: return .cell("target")
        case .editPreWait: return .cell("preWait")
        case .editDuration: return .cell("duration")
        case .editPostWait: return .cell("postWait")
        case .cycleContinue: return .cycleContinue
        case .toggleFlag: return .toggleFlag
        default: return nil
        }
    }

    /// A new workspace's key, QLab 5's defaults (manual pp. 69-70): Stop All, Start, Stop and Reset
    /// Selected have none.
    var defaultChord: KeyChord? {
        let keys: [WorkspaceKey: String] = [
            .go: " ", .pauseAll: "[", .resumeAll: "]", .loadSelected: "l", .pauseResumeSelected: "p",
            .previewSelected: "v", .panicSelected: "s", .editNumber: "n", .editName: "q", .editNotes: "o",
            .editTarget: "t", .editPreWait: "e", .editDuration: "d", .editPostWait: "w", .cycleContinue: "c",
            .toggleFlag: "f",
        ]
        return keys[self].map { KeyChord($0) }
    }

    var titleKey: String { command?.titleKey ?? "keymap.action.\(rawValue)" }

    /// The key-map page's columns: the workspace actions, then the cue-editing actions.
    static let workspaceActions = allCases.filter { $0.command != nil }
    static let editingActions = allCases.filter { $0.editKey != nil }
}

/// The key map of one workspace, read from its `settings.hotkeys` (QLab keeps it per workspace).
/// How each model field maps:
/// - `go`, `pause_all`, `resume_all` and the selected-cue keys `load_selected`, `pause_resume_selected`,
///   `start_selected`, `stop_selected`, `reset_selected`, `preview_selected`, `panic_selected`: bound as
///   given, the engine's own map. Absent: the default (`WorkspaceKey.defaultChord`); `null`: no key.
/// - The cue-editing keys `edit_number` ... `toggle_flag` (TRG-07) the same way: each edits the cues
///   selected in the front window's list (`editKey(for:)`), N, Q, O, T, E, D, W, C and F by default.
/// - `stop_all`: Esc is the fixed app-wide panic, so it binds nothing; any other key binds Stop All.
/// - There are no Next / Previous Cue keys (format v6): QLab's key map has none and takes no Up or
///   Down arrow; the arrows and the fixed Select Next / Select Previous items move the selection.
/// - Panic All (Esc) is the only fixed key.
/// - Reset All has no key (QLab has none); it is the sidebar button and the Transport menu item.
///
/// Conflict precedence: (1) Esc is always panic, even while typing, and cannot be bound; (2) a
/// chord equal to another key of the map or a cue hotkey is refused (`validate`); in a loaded
/// show the first field wins and the later one is dropped with a warning, and a cue
/// hotkey on a key of the map is the one that yields (`AppController.keys(for:)`: transport keys
/// must keep working in a live show);
/// (3) while a text field is edited, chords that type characters (no Cmd/Ctrl) do not fire.
struct TransportKeys: Equatable {
    /// Every bound key of the map.
    private(set) var bound: [WorkspaceKey: KeyChord] = [:]
    private(set) var warnings: [String] = []

    /// What a cue-editing key does to the selected cue.
    enum EditKey: Equatable {
        /// Opens that cue-list column's cell for typing.
        case cell(String)
        /// Shows the inspector's notes for typing.
        case notes
        /// Steps the continue mode: none, auto-continue, auto-follow, none.
        case cycleContinue
        case toggleFlag
    }

    /// `settings` is the workspace's `settings` object (nil or without `hotkeys`: all defaults).
    init(settings: JSONValue?) {
        let hotkeys = settings?["hotkeys"]
        for key in WorkspaceKey.allCases {
            var chord = key.defaultChord
            if let value = hotkeys?[key.rawValue] {
                if value == .null {
                    chord = nil
                } else {
                    chord = KeyChord(hotkey: value)
                    if chord == nil {
                        warnings.append(
                            String(
                                format: loc("hotkeys.warn.unusable"), loc(key.titleKey),
                                loc("hotkeys.reason.unsupported")))
                    }
                }
            }
            guard let chord else { continue }
            if chord.key == KeyChord.escape { continue }  // stop_all = Esc: the app-wide panic
            if let error = Self.validate(chord, for: key, in: bound) {
                warnings.append(
                    String(format: loc("hotkeys.warn.unusable"), loc(key.titleKey), Self.describe(error)))
                continue
            }
            bound[key] = chord
        }
    }

    /// The bound transport and selected-cue keys, by the command each performs.
    var chords: [TransportCommand: KeyChord] {
        var out: [TransportCommand: KeyChord] = [:]
        for (key, chord) in bound { if let command = key.command { out[command] = chord } }
        return out
    }

    func chord(for command: TransportCommand) -> KeyChord? { chords[command] }
    func command(for chord: KeyChord) -> TransportCommand? { action(for: chord)?.command }
    func action(for chord: KeyChord) -> WorkspaceKey? { bound.first { $0.value == chord }?.key }
    func editKey(for chord: KeyChord) -> EditKey? { action(for: chord)?.editKey }

    /// Why `chord` cannot be bound to `key` next to `existing`; nil if it can. Pure, for the
    /// loader above and the key-map page. Up and Down are refused with any modifiers
    /// (QLab 5), as are Esc and the empty key; a menu command's key only where `menuChords` is given
    /// (the key-map page), and a key with no show-file name.
    static func validate(
        _ chord: KeyChord, for key: WorkspaceKey, in existing: [WorkspaceKey: KeyChord],
        isCueHotkey: (KeyChord) -> Bool = { _ in false }, menuChords: Set<KeyChord> = []
    ) -> KeyBindError? {
        guard chord.key != KeyChord.escape, !chord.key.isEmpty else { return .reserved }
        if isArrow(chord) { return .reserved }
        if chord.hotkeyJSON == nil { return .unsupported }
        if let other = existing.first(where: { $0.value == chord && $0.key != key })?.key {
            return .duplicate(of: other)
        }
        if menuChords.contains(chord) { return .menuKey }
        return isCueHotkey(chord) ? .cueHotkey : nil
    }

    /// Up or Down with any modifiers: no key of the key map or cue hotkey may be one.
    static func isArrow(_ chord: KeyChord) -> Bool {
        chord.key == KeyChord.up || chord.key == KeyChord.down
    }

    private static func describe(_ error: KeyBindError) -> String {
        switch error {
        case .reserved: return loc("hotkeys.reason.reserved")
        case .duplicate(let other): return String(format: loc("hotkeys.reason.duplicate"), loc(other.titleKey))
        case .cueHotkey: return loc("hotkeys.reason.cueHotkey")
        case .menuKey: return loc("hotkeys.reason.menuKey")
        case .unsupported: return loc("hotkeys.reason.unsupported")
        case .editKey: return loc("hotkeys.reason.editKey")
        }
    }
}
