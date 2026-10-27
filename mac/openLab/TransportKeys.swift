// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The transport commands in the Transport menu, in menu order. Raw values are the persisted names.
enum TransportCommand: String, CaseIterable, Codable, Sendable {
    case go, stopAll, pauseAll, resumeAll, panic
    case playheadPrevious, playheadNext
    case loadSelected, startSelected, pauseResumeSelected, stopSelected, resetSelected

    var titleKey: String { "menu.transport.\(rawValue)" }
}

/// A key and modifiers in the form `NSMenuItem` takes (`key` lowercase; modifiers in `mask`).
struct KeyChord: Codable, Hashable, Sendable {
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

    /// Plain and Shift/Option chords insert characters, so a text field being edited must keep them.
    var typesCharacters: Bool { modifiers.isDisjoint(with: [.command, .control]) }
}

enum KeyBindError: Error, Equatable {
    /// Esc is panic and cannot move (QLab: fixed safety key); the arrows alone are not allowed either.
    case reserved
    case duplicate(of: TransportCommand)
    case cueHotkey
}

/// Transport key bindings. QLab keeps its key map per workspace; the protocol has no field for it,
/// so it lives in `UserDefaults` for the whole app (docs/handoff/m5-protocol-requests.md).
///
/// Conflict precedence: (1) Esc is always panic, even while typing, and cannot be bound; (2) a
/// binding that equals another transport binding or a cue hotkey is refused when it is made, so
/// no key ever means two things; (3) while a text field is being edited, chords that type
/// characters do not fire (the Transport menu items are disabled then), chords with Command or
/// Control still do.
@MainActor
final class KeyMap {
    static let defaultsKey = "transportKeyMap"
    /// QLab 5 defaults: Space GO, [ Pause All, ] Resume All, P pause/resume, L load, Shift-Cmd-arrows
    /// for the playhead. Stop All and the other selection commands have no QLab default key.
    static let defaults: [TransportCommand: KeyChord] = [
        .go: KeyChord(" "), .pauseAll: KeyChord("["), .resumeAll: KeyChord("]"),
        .pauseResumeSelected: KeyChord("p"), .loadSelected: KeyChord("l"),
        .playheadNext: KeyChord(KeyChord.down, [.command, .shift]),
        .playheadPrevious: KeyChord(KeyChord.up, [.command, .shift]),
    ]

    private(set) var bindings: [TransportCommand: KeyChord]
    private let store: UserDefaults
    /// Set by the owner of the cue hotkeys (M5.7); a hotkey makes the chord unavailable here.
    var isCueHotkey: (KeyChord) -> Bool = { _ in false }
    var onChange: (() -> Void)?

    init(store: UserDefaults = .standard) {
        self.store = store
        bindings = Self.load(store) ?? Self.defaults
    }

    func chord(for command: TransportCommand) -> KeyChord? { bindings[command] }

    func command(for chord: KeyChord) -> TransportCommand? {
        bindings.first { $0.value == chord }?.key
    }

    func bind(_ command: TransportCommand, to chord: KeyChord) -> KeyBindError? {
        guard command != .panic, chord.key != KeyChord.escape, !chord.key.isEmpty else { return .reserved }
        let arrow = chord.key == KeyChord.up || chord.key == KeyChord.down
        if arrow, chord.modifiers.isDisjoint(with: [.command, .control, .option]) { return .reserved }
        if let other = self.command(for: chord), other != command { return .duplicate(of: other) }
        if isCueHotkey(chord) { return .cueHotkey }
        bindings[command] = chord
        save()
        return nil
    }

    func unbind(_ command: TransportCommand) {
        bindings[command] = nil
        save()
    }

    func resetToDefaults() {
        bindings = Self.defaults
        save()
    }

    private func save() {
        let raw = Dictionary(uniqueKeysWithValues: bindings.map { ($0.key.rawValue, $0.value) })
        if let data = try? JSONEncoder().encode(raw) { store.set(data, forKey: Self.defaultsKey) }
        onChange?()
    }

    /// A damaged or conflicting stored map is ignored (defaults), never trusted: a duplicate would
    /// make one key do two things.
    private static func load(_ store: UserDefaults) -> [TransportCommand: KeyChord]? {
        guard let data = store.data(forKey: defaultsKey),
            let raw = try? JSONDecoder().decode([String: KeyChord].self, from: data)
        else { return nil }
        var result: [TransportCommand: KeyChord] = [:]
        for (name, chord) in raw {
            guard let command = TransportCommand(rawValue: name), command != .panic, chord.key != KeyChord.escape
            else { return nil }
            result[command] = chord
        }
        return Set(result.values).count == result.count ? result : nil
    }
}
