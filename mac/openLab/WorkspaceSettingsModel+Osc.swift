// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The OSC passcode table of the workspace settings (NET-02). Every edit is one engine command
/// (or one transaction), so each is one undo step, and every refusal is a line in `message` with
/// the table unchanged.
extension WorkspaceSettingsModel {
    /// The table as the engine has it.
    var osc: OscAccessTable { OscAccessTable(settings: store.model?.settings) }

    /// Adds a row with a fresh four-digit code (1000 to 9999) and every right. False, with a
    /// message, when the table is full.
    @discardableResult
    func addOscPasscode() async -> Bool {
        let table = osc
        guard table.rows.count < OscAccessTable.maxRows else {
            message = loc("settings.osc.full")
            return false
        }
        let used = Set(table.rows.map(\.passcode))
        // At most 16 of the 9000 codes are taken: a free one is found at once.
        var code = String(Int.random(in: 1000...9999))
        while used.contains(code) { code = String(Int.random(in: 1000...9999)) }
        return await sendOsc(.addOscPasscode(code, rights: .all, at: table.rows.count))
    }

    /// Changes the code of the row now showing `old` to `text`: the old row goes and the new one
    /// takes its place with the same rights, in one undo step (the engine has no rename). False,
    /// with a message, when `text` is not four digits or is another row's; nothing is sent.
    @discardableResult
    func changeOscPasscode(_ old: String, to text: String) async -> Bool {
        let new = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if new == old { return true }
        let table = osc
        guard let at = table.rows.firstIndex(where: { $0.passcode == old }) else { return false }
        guard OscAccessTable.isValidPasscode(new) else {
            message = loc("settings.osc.invalid")
            return false
        }
        guard !table.rows.contains(where: { $0.passcode == new }) else {
            message = loc("settings.osc.duplicate")
            return false
        }
        let rights = table.rows[at].rights
        guard
            let edit = Command.transaction([
                .removeOscPasscode(old), .addOscPasscode(new, rights: rights, at: at),
            ])
        else { return false }
        return await sendOsc(edit)
    }

    /// Deletes the row now showing `passcode`.
    @discardableResult
    func removeOscPasscode(_ passcode: String) async -> Bool {
        await sendOsc(.removeOscPasscode(passcode))
    }

    enum OscRight { case view, edit, control }

    /// Turns one right of a row (`passcode`; nil: the No Passcode row) on or off. Edit and Control
    /// are not available without View, and turning View off clears all three in one command.
    @discardableResult
    func setOscRight(_ right: OscRight, _ on: Bool, of passcode: String?) async -> Bool {
        let table = osc
        var rights: OscRights
        if let passcode {
            guard let row = table.rows.first(where: { $0.passcode == passcode }) else { return false }
            rights = row.rights
        } else {
            rights = table.noPasscode
        }
        switch right {
        case .view: rights = on ? OscRights(view: true, edit: rights.edit, control: rights.control) : .none
        case .edit:
            guard rights.view else { return false }
            rights.edit = on
        case .control:
            guard rights.view else { return false }
            rights.control = on
        }
        return await sendOsc(.setOscRights(passcode, rights))
    }

    private func sendOsc(_ command: Command) async -> Bool {
        switch await store.send(command: command) {
        case .success:
            message = nil
            return true
        case .failure(let e):
            message = e.detailText
            return false
        }
    }
}
