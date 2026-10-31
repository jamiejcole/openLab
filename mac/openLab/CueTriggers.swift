// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

extension KeyChord {
    /// The chord as a show-file hotkey (`{key, modifiers}`); nil for a key with no name.
    var hotkeyJSON: JSONValue? {
        let name: String
        if let named = Self.names.first(where: { $0.value == key })?.key {
            name = named
        } else if key.unicodeScalars.count == 1, let s = key.unicodeScalars.first,
            s.value > 0x20, s.value != 0x7F, !(0x80...0x9F).contains(s.value), !(0xF700...0xF8FF).contains(s.value)
        {
            name = key
        } else {
            return nil
        }
        let flags: [(String, NSEvent.ModifierFlags)] = [
            ("shift", .shift), ("control", .control), ("alt", .option), ("command", .command),
        ]
        var mods: [String: JSONValue] = [:]
        for (field, flag) in flags { mods[field] = .bool(modifiers.contains(flag)) }
        return .object(["key": .string(name), "modifiers": .object(mods)])
    }

    /// How the recorder shows it: `⌃⌥⇧⌘` then the key.
    var display: String {
        let marks: [(NSEvent.ModifierFlags, String)] = [
            (.control, "⌃"), (.option, "⌥"), (.shift, "⇧"), (.command, "⌘"),
        ]
        let prefix = marks.filter { modifiers.contains($0.0) }.map(\.1).joined()
        let symbols = [Self.up: "↑", Self.down: "↓", Self.left: "←", Self.right: "→", "\r": "↩", "\t": "⇥"]
        if key == " " { return prefix + loc("inspector.hotkey.space") }
        if let s = symbols[key] { return prefix + s }
        if let n = Self.names.first(where: { $0.value == key && $0.key.hasPrefix("f") })?.key {
            return prefix + n.uppercased()
        }
        return prefix + key.uppercased()
    }
}

extension WorkspaceModel {
    /// Every cue with a hotkey trigger, with its chord (keys the app cannot match are left out).
    var cueHotkeys: [(cue: ModelID, chord: KeyChord)] {
        var out: [(ModelID, KeyChord)] = []
        func walk(_ cues: [Cue]) {
            for c in cues {
                if let hk = c.raw["triggers"]?["hotkey"], hk != .null, let chord = KeyChord(hotkey: hk) {
                    out.append((c.id, chord))
                }
                walk(c.children)
            }
        }
        for l in cueLists { walk(l.cues) }
        return out
    }
}

/// Which chords a cue's hotkey may take. Pure: the inspector and the loader share it.
enum HotkeyRules {
    /// Why `chord` cannot be `cue`'s hotkey; nil if it can. Esc and Up/Down with any
    /// modifiers are refused as in QLab 5 (left/right are fine); a transport key, a
    /// menu command's key and another cue's hotkey are refused so one key press never does two
    /// things (QLab refuses keys already assigned in Workspace Settings).
    static func validate(
        _ chord: KeyChord, for cue: ModelID, in model: WorkspaceModel, keys: TransportKeys, menuChords: Set<KeyChord>
    ) -> KeyBindError? {
        if chord.key == KeyChord.escape || TransportKeys.isArrow(chord) { return .reserved }
        if chord.hotkeyJSON == nil { return .unsupported }
        // The fixed app defaults (L, P, V, S) yield to a cue hotkey, so they do not refuse it.
        if let command = keys.command(for: chord), !TransportKeys.fixed.contains(where: { $0.0 == command }) {
            return .duplicate(of: command)
        }
        if menuChords.contains(chord) { return .menuKey }
        if model.cueHotkeys.contains(where: { $0.chord == chord && $0.cue != cue }) { return .cueHotkey }
        return nil
    }

    static func messageKey(_ error: KeyBindError) -> String {
        switch error {
        case .reserved: return "inspector.hotkey.reserved"
        case .duplicate: return "inspector.hotkey.transport"
        case .cueHotkey: return "inspector.hotkey.otherCue"
        case .menuKey: return "inspector.hotkey.menu"
        case .unsupported: return "inspector.hotkey.unsupported"
        }
    }

    /// Key equivalents of the menu bar except the Transport items (their keys are the workspace's
    /// transport keys, checked separately and dynamic).
    @MainActor
    static func currentMenuChords() -> Set<KeyChord> {
        var out: Set<KeyChord> = []
        func walk(_ menu: NSMenu) {
            for item in menu.items {
                if let sub = item.submenu { walk(sub) }
                guard !item.keyEquivalent.isEmpty, item.action != #selector(AppDelegate.transportCommand(_:)) else {
                    continue
                }
                // AppKit reads an upper-case letter as Shift (Redo "Z", Save As "S", Pre-Show "W").
                var mods = item.keyEquivalentModifierMask
                if item.keyEquivalent.lowercased() != item.keyEquivalent { mods.insert(.shift) }
                out.insert(KeyChord(item.keyEquivalent, mods))
            }
        }
        if let main = NSApp?.mainMenu { walk(main) }
        return out
    }
}

/// The recorder control in the Triggers tab: click it, press the chord. It takes the key itself
/// (also Cmd chords, which would otherwise reach the menus) and hands the event on. Esc leaves
/// without recording (the app-level monitor has already panicked, as everywhere).
final class HotkeyRecorder: NSTextField {
    var onRecord: ((NSEvent) -> Void)?

    override var acceptsFirstResponder: Bool { isEnabled }
    override func mouseDown(with event: NSEvent) { window?.makeFirstResponder(self) }
    override func becomeFirstResponder() -> Bool {
        placeholderString = loc("inspector.hotkey.recording")
        return super.becomeFirstResponder()
    }
    override func resignFirstResponder() -> Bool {
        placeholderString = loc("inspector.hotkey.click")
        return super.resignFirstResponder()
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 {
            window?.makeFirstResponder(nil)
        } else {
            onRecord?(event)
            window?.makeFirstResponder(nil)
        }
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.firstResponder === self else { return false }
        keyDown(with: event)
        return true
    }
}
