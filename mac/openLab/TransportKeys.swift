// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The transport commands in the Transport menu, in menu order.
enum TransportCommand: String, CaseIterable, Codable, Sendable {
    case go, stopAll, pauseAll, resumeAll, panic
    case playheadPrevious, playheadNext
    case loadSelected, startSelected, pauseResumeSelected, stopSelected, resetSelected

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

    /// The chord of a pressed key (`charactersIgnoringModifiers` and the event's modifiers).
    init?(event: NSEvent) {
        guard let key = event.charactersIgnoringModifiers, !key.isEmpty else { return nil }
        self.init(key, event.modifierFlags)
    }

    /// A show-file hotkey (`{key, modifiers}`, docs/file-format.md section 5.1): `key` is a
    /// lower-case name ("space", "escape", "down", "[", "a"). Names with no character form
    /// (function keys) give nil.
    init?(hotkey: JSONValue) {
        guard let name = hotkey["key"]?.stringValue else { return nil }
        let names = [
            "space": " ", "escape": Self.escape, "up": Self.up, "down": Self.down, "return": "\r", "tab": "\t",
        ]
        guard let key = names[name] ?? (name.count == 1 ? name : nil) else { return nil }
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
    /// Esc is panic and cannot move (QLab: fixed safety key); the arrows alone are not allowed either.
    case reserved
    case duplicate(of: TransportCommand)
    case cueHotkey
}

/// The transport keys of one workspace, read from its `settings.hotkeys` (QLab keeps them per
/// workspace). How each model field maps:
/// - `go`, `pause_all`, `resume_all`: bound as given. Absent: the QLab default (Space, `[`, `]`);
///   `null`: no key.
/// - `stop_all`: Esc is the fixed app-wide panic, so it binds nothing; any other key binds Stop All.
/// - `playhead_next` / `playhead_previous`: a bare Down / Up means "the selection moves the
///   playhead" (cue list behaviour), so it binds nothing; any other key binds Next / Previous Cue.
///   Absent: Shift-Cmd-Down / Up (QLab's menu keys).
/// - The model has no fields for the selected-cue keys: Load Selected `L` and Pause/Resume
///   Selected `P` are fixed app defaults, dropped if a show's key already uses the chord.
///
/// Conflict precedence: (1) Esc is always panic, even while typing, and cannot be bound; (2) a
/// chord equal to another transport binding or a cue hotkey is refused (`validate`); in a loaded
/// show the first field in the order above wins and the later one is dropped with a warning;
/// (3) while a text field is edited, chords that type characters (no Cmd/Ctrl) do not fire.
struct TransportKeys: Equatable {
    private(set) var chords: [TransportCommand: KeyChord] = [:]
    private(set) var warnings: [String] = []

    static let fixed: [(TransportCommand, KeyChord)] = [
        (.pauseResumeSelected, KeyChord("p")), (.loadSelected, KeyChord("l")),
    ]

    private static let fields: [(String, TransportCommand, KeyChord?)] = [
        ("go", .go, KeyChord(" ")), ("stop_all", .stopAll, nil), ("pause_all", .pauseAll, KeyChord("[")),
        ("resume_all", .resumeAll, KeyChord("]")),
        ("playhead_next", .playheadNext, KeyChord(KeyChord.down, [.command, .shift])),
        ("playhead_previous", .playheadPrevious, KeyChord(KeyChord.up, [.command, .shift])),
    ]

    /// `settings` is the workspace's `settings` object (nil or without `hotkeys`: all defaults).
    init(settings: JSONValue?, isCueHotkey: (KeyChord) -> Bool = { _ in false }) {
        let hotkeys = settings?["hotkeys"]
        for (field, command, fallback) in Self.fields {
            var chord = fallback
            if let value = hotkeys?[field] {
                if value == .null {
                    chord = nil
                } else {
                    chord = KeyChord(hotkey: value)
                    if chord == nil { warnings.append("hotkeys.\(field): key not supported, no key bound") }
                }
            }
            guard let chord else { continue }
            if chord.key == KeyChord.escape { continue }  // stop_all = Esc: the app-wide panic
            if command == .playheadNext || command == .playheadPrevious, Self.isBareArrow(chord) { continue }
            if let error = Self.validate(chord, for: command, in: chords, isCueHotkey: isCueHotkey) {
                warnings.append("hotkeys.\(field): \(Self.describe(error)), no key bound")
                continue
            }
            chords[command] = chord
        }
        for (command, chord) in Self.fixed {
            if let error = Self.validate(chord, for: command, in: chords, isCueHotkey: isCueHotkey) {
                warnings.append("\(command.rawValue) key: \(Self.describe(error)), no key bound")
            } else {
                chords[command] = chord
            }
        }
    }

    func chord(for command: TransportCommand) -> KeyChord? { chords[command] }
    func command(for chord: KeyChord) -> TransportCommand? { chords.first { $0.value == chord }?.key }

    /// Why `chord` cannot be bound to `command` next to `existing`; nil if it can. Pure, for the
    /// loader above and the key-map editor (M5.11a).
    static func validate(
        _ chord: KeyChord, for command: TransportCommand, in existing: [TransportCommand: KeyChord],
        isCueHotkey: (KeyChord) -> Bool = { _ in false }
    ) -> KeyBindError? {
        guard command != .panic, chord.key != KeyChord.escape, !chord.key.isEmpty else { return .reserved }
        if isBareArrow(chord) { return .reserved }
        if let other = existing.first(where: { $0.value == chord && $0.key != command })?.key {
            return .duplicate(of: other)
        }
        return isCueHotkey(chord) ? .cueHotkey : nil
    }

    private static func isBareArrow(_ chord: KeyChord) -> Bool {
        (chord.key == KeyChord.up || chord.key == KeyChord.down)
            && chord.modifiers.isDisjoint(with: [.command, .control, .option])
    }

    private static func describe(_ error: KeyBindError) -> String {
        switch error {
        case .reserved: return "reserved key"
        case .duplicate(let other): return "same key as \(other.rawValue)"
        case .cueHotkey: return "same key as a cue hotkey"
        }
    }
}
