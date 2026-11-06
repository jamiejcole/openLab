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
    case duplicate(of: TransportCommand)
    case cueHotkey
    /// A menu command (Save, Quit, ...) already owns the chord.
    case menuKey
    /// The key has no show-file name (Delete, media keys, ...).
    case unsupported
    /// N and Q edit the highlighted cue's number and name (QLab refuses a key that has a function).
    case editKey
}

/// The transport keys of one workspace, read from its `settings.hotkeys` (QLab keeps them per
/// workspace). How each model field maps:
/// - `go`, `pause_all`, `resume_all` and the selected-cue keys `load_selected`, `pause_resume_selected`,
///   `start_selected`, `stop_selected`, `reset_selected`, `preview_selected`, `panic_selected`: bound as
///   given, the engine's own map. Absent: the default (Space, `[`, `]`, `l`, `p`, none, none, none, `v`,
///   `s`); `null`: no key.
/// - `stop_all`: Esc is the fixed app-wide panic, so it binds nothing; any other key binds Stop All.
/// - There are no Next / Previous Cue keys (format v6): QLab's key map has none and takes no Up or
///   Down arrow; the arrows and the fixed Select Next / Select Previous items move the selection.
/// - Panic All (Esc) is the only fixed key; Preview Selected and Panic Selected are remappable like the rest.
/// - Reset All has no key (QLab has none); it is the sidebar button and the Transport menu item.
///
/// Conflict precedence: (1) Esc is always panic, even while typing, and cannot be bound; (2) a
/// chord equal to another transport binding or a cue hotkey is refused (`validate`); in a loaded
/// show the first transport field wins and the later one is dropped with a warning, and a cue
/// hotkey on a transport chord is the one that yields (`AppController.keys(for:)`: transport keys
/// must keep working in a live show);
/// (3) while a text field is edited, chords that type characters (no Cmd/Ctrl) do not fire.
struct TransportKeys: Equatable {
    private(set) var chords: [TransportCommand: KeyChord] = [:]
    private(set) var warnings: [String] = []

    /// QLab 5's edit-number and edit-name keys, by default: key to the cue-list column they open.
    /// Not in the key map; a cue hotkey cannot take them (`HotkeyRules`).
    static let editKeys: [KeyChord: String] = [KeyChord("n"): "number", KeyChord("q"): "name"]

    private static let fields: [(String, TransportCommand, KeyChord?)] = [
        ("go", .go, KeyChord(" ")), ("stop_all", .stopAll, nil), ("pause_all", .pauseAll, KeyChord("[")),
        ("resume_all", .resumeAll, KeyChord("]")), ("load_selected", .loadSelected, KeyChord("l")),
        ("pause_resume_selected", .pauseResumeSelected, KeyChord("p")), ("start_selected", .startSelected, nil),
        ("stop_selected", .stopSelected, nil), ("reset_selected", .resetSelected, nil),
        ("preview_selected", .previewSelected, KeyChord("v")), ("panic_selected", .panicSelected, KeyChord("s")),
    ]

    /// `settings` is the workspace's `settings` object (nil or without `hotkeys`: all defaults).
    init(settings: JSONValue?) {
        let hotkeys = settings?["hotkeys"]
        for (field, command, fallback) in Self.fields {
            var chord = fallback
            if let value = hotkeys?[field] {
                if value == .null {
                    chord = nil
                } else {
                    chord = KeyChord(hotkey: value)
                    if chord == nil {
                        warnings.append(
                            String(
                                format: loc("hotkeys.warn.unusable"), loc(command.titleKey),
                                loc("hotkeys.reason.unsupported")))
                    }
                }
            }
            guard let chord else { continue }
            if chord.key == KeyChord.escape { continue }  // stop_all = Esc: the app-wide panic
            if let error = Self.validate(chord, for: command, in: chords) {
                warnings.append(
                    String(format: loc("hotkeys.warn.unusable"), loc(command.titleKey), Self.describe(error)))
                continue
            }
            chords[command] = chord
        }
    }

    func chord(for command: TransportCommand) -> KeyChord? { chords[command] }
    func command(for chord: KeyChord) -> TransportCommand? { chords.first { $0.value == chord }?.key }

    /// Why `chord` cannot be bound to `command` next to `existing`; nil if it can. Pure, for the
    /// loader above and the key-map editor. Up and Down are refused with any modifiers
    /// (QLab 5), as are Esc and the empty key.
    static func validate(
        _ chord: KeyChord, for command: TransportCommand, in existing: [TransportCommand: KeyChord],
        isCueHotkey: (KeyChord) -> Bool = { _ in false }
    ) -> KeyBindError? {
        guard command != .panic, chord.key != KeyChord.escape, !chord.key.isEmpty else { return .reserved }
        if isArrow(chord) { return .reserved }
        if let other = existing.first(where: { $0.value == chord && $0.key != command })?.key {
            return .duplicate(of: other)
        }
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
