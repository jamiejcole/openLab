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
    /// Every cue with a hotkey trigger that is on, with its chord (keys the app cannot match are left
    /// out). A trigger switched off keeps its key but takes no key press and clashes with no cue.
    var cueHotkeys: [(cue: ModelID, chord: KeyChord)] { hotkeys(includingOff: false) }

    /// Every cue's hotkey, switched off or not: the workspace key map may not take one (the engine
    /// refuses a cue's key there whether its trigger is on or off).
    var storedCueHotkeys: [(cue: ModelID, chord: KeyChord)] { hotkeys(includingOff: true) }

    private func hotkeys(includingOff: Bool) -> [(cue: ModelID, chord: KeyChord)] {
        var out: [(ModelID, KeyChord)] = []
        func walk(_ cues: [Cue]) {
            for c in cues {
                if let triggers = c.raw["triggers"], includingOff || triggers["hotkey_enabled"] != .bool(false),
                    let hk = triggers["hotkey"], hk != .null, let chord = KeyChord(hotkey: hk)
                {
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
    /// modifiers are refused as in QLab 5 (left/right are fine); a key of the workspace map (a
    /// cue-editing key included), a menu command's key and another cue's hotkey are refused so one key press never does two
    /// things (QLab refuses keys already assigned in Workspace Settings).
    static func validate(
        _ chord: KeyChord, for cue: ModelID, in model: WorkspaceModel, keys: TransportKeys, menuChords: Set<KeyChord>
    ) -> KeyBindError? {
        if chord.key == KeyChord.escape || TransportKeys.isArrow(chord) { return .reserved }
        if chord.hotkeyJSON == nil { return .unsupported }
        if let action = keys.action(for: chord) {
            return action.editKey != nil ? .editKey : .duplicate(of: action)
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
        case .editKey: return "inspector.hotkey.editKey"
        }
    }

    /// Key equivalents of the menu bar except the Transport items (their keys are the workspace's
    /// transport keys, checked separately and dynamic).
    @MainActor
    static func currentMenuChords() -> Set<KeyChord> { NSApp?.mainMenu.map(menuChords(in:)) ?? [] }

    @MainActor
    static func menuChords(in menu: NSMenu) -> Set<KeyChord> {
        var out: Set<KeyChord> = []
        func walk(_ menu: NSMenu) {
            for item in menu.items {
                if let sub = item.submenu { walk(sub) }
                guard !item.keyEquivalent.isEmpty, item.action != #selector(AppDelegate.transportCommand(_:)) else {
                    continue
                }
                // macOS adds items of its own that need the fn key (Enter Full Screen is fn-F). A chord has
                // no fn, so such an item would otherwise read as the plain key and take it from cues.
                if item.keyEquivalentModifierMask.contains(.function) { continue }
                // AppKit reads an upper-case letter as Shift (Redo "Z", Save As "S", Pre-Show "W").
                var mods = item.keyEquivalentModifierMask
                if item.keyEquivalent.lowercased() != item.keyEquivalent { mods.insert(.shift) }
                out.insert(KeyChord(item.keyEquivalent, mods))
            }
        }
        walk(menu)
        return out
    }
}

/// The recorder control in the Triggers tab: click it, press the chord. It takes the key itself
/// (also Cmd chords, which would otherwise reach the menus) and hands the event on. Esc leaves
/// without recording (the app-level monitor has already panicked, as everywhere).
final class HotkeyRecorder: NSTextField {
    /// Recording ended, as the field editor's end of editing says for a text field.
    static let didEndRecording = Notification.Name("HotkeyRecorder.didEndRecording")
    var onRecord: ((NSEvent) -> Void)?
    /// What the empty field says, idle and while waiting for the key.
    var idlePlaceholder = loc("inspector.hotkey.click")
    var recordingPlaceholder = loc("inspector.hotkey.recording")
    /// The key shown is taken out while waiting for the next one, so the prompt shows; it comes back
    /// if none is pressed.
    var clearsWhileRecording = false
    private var shownBeforeRecording: String?

    override var acceptsFirstResponder: Bool { isEnabled }
    override func mouseDown(with event: NSEvent) { window?.makeFirstResponder(self) }
    override func becomeFirstResponder() -> Bool {
        placeholderString = recordingPlaceholder
        if clearsWhileRecording {
            shownBeforeRecording = stringValue
            stringValue = ""
        }
        return super.becomeFirstResponder()
    }
    override func resignFirstResponder() -> Bool {
        placeholderString = idlePlaceholder
        if let shown = shownBeforeRecording, stringValue.isEmpty { stringValue = shown }
        shownBeforeRecording = nil
        NotificationCenter.default.post(name: Self.didEndRecording, object: self)
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
