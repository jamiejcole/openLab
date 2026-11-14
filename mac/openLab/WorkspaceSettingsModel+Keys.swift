// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Why a key typed into the key map was not taken, for the field it was typed into.
struct KeyRefusal: Equatable {
    let key: WorkspaceKey
    let text: String
}

/// The key map of the workspace settings (TRG-01, TRG-07): the workspace actions and the
/// cue-editing actions, each with its key or none. Every change is one engine command
/// (`SetWorkspaceSetting { hotkey }`), so one undo step, saved with the show. A key is checked
/// here first, so the reason can be shown beside the field; the engine checks it again.
extension WorkspaceSettingsModel {
    /// The keys as the engine has them (the app's reading of `settings.hotkeys`).
    var keyMap: TransportKeys { TransportKeys(settings: store.model?.settings) }

    /// Puts `chord` on `key`. False, with `keyRefusal` naming why, when the chord is reserved, has
    /// no show-file name, or is another action's key or a cue's hotkey; nothing is sent then. A menu
    /// command's key is taken (QLab allows it, manual p. 102): the menu command loses its key in this
    /// workspace and the Warnings tab says so. The chord `key` already has sends nothing.
    @discardableResult
    func assignKey(_ key: WorkspaceKey, _ chord: KeyChord) async -> Bool {
        let map = keyMap
        if map.bound[key] == chord {
            keyRefusal = nil
            return true
        }
        let cueHotkeys = store.model?.storedCueHotkeys ?? []
        if let error = TransportKeys.validate(
            chord, for: key, in: map.bound, isCueHotkey: { c in cueHotkeys.contains { $0.chord == c } })
        {
            keyRefusal = KeyRefusal(key: key, text: refusalText(error, chord: chord))
            return false
        }
        guard let hotkey = chord.hotkeyJSON else { return false }
        return await sendKey(key, hotkey)
    }

    /// Takes `key`'s key away (always allowed).
    @discardableResult
    func clearKey(_ key: WorkspaceKey) async -> Bool {
        guard keyMap.bound[key] != nil || store.model?.hotkey(key) != nil else {
            keyRefusal = nil
            return true
        }
        return await sendKey(key, nil)
    }

    private func sendKey(_ key: WorkspaceKey, _ hotkey: JSONValue?) async -> Bool {
        switch await store.send(command: .setWorkspaceKey(key, hotkey)) {
        case .success:
            keyRefusal = nil
            return true
        case .failure(let e):
            keyRefusal = KeyRefusal(key: key, text: e.detailText)
            return false
        }
    }

    /// The most of a cue's number and name a reason shows: the sentence around it must fit beside a
    /// field at the window's least size.
    static let reasonCueLength = 24

    /// `title` cut to `reasonCueLength` characters, with an ellipsis when cut.
    static func shortened(_ title: String) -> String {
        title.count <= reasonCueLength ? title : String(title.prefix(reasonCueLength - 1)) + "\u{2026}"
    }

    /// The reason in words: which action or cue already has the key.
    func refusalText(_ error: KeyBindError, chord: KeyChord) -> String {
        switch error {
        case .reserved: return loc("settings.keys.refused.reserved")
        // The key map takes a menu command's key: `validate` never gives `.menuKey` for it.
        case .unsupported, .menuKey: return loc("settings.keys.refused.unsupported")
        case .duplicate(let other): return String(format: loc("settings.keys.refused.action"), loc(other.titleKey))
        case .cueHotkey, .editKey:
            let id = store.model?.storedCueHotkeys.first { $0.chord == chord }?.cue
            let name = id.flatMap { id in
                store.model.flatMap { m in m.cue(id).map { WarningRows.cueTitle($0, name: m.listName($0)) } }
            }
            return name.map { String(format: loc("settings.keys.refused.cue"), Self.shortened($0)) }
                ?? loc("settings.keys.refused.anyCue")
        }
    }
}
