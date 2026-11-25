// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
import Foundation

/// One row of the passcode table. The engine knows a row by its code, which is what the operator
/// edits; `id` stays with the row when its code changes.
struct OscRow: Equatable {
    let id: String
    var passcode: String
    var rights: OscRights
}

/// The engine's passcode rows with their identities: a row keeps its own through a change of its code,
/// the app's, an undo's or another client's. Changed only by what the engine has.
struct OscRows: Equatable {
    private(set) var rows: [OscRow] = []
    private var made = 0

    /// Takes the engine's table. After one change (`oneStep`) that replaced one code in its place with the
    /// same rights, the row is the same one, renamed. Any other code not seen before is a new row: an
    /// edit meant for a row that went never reaches one that came in its place.
    mutating func take(_ table: OscAccessTable, oneStep: Bool) {
        let old = rows
        let gone = old.indices.filter { i in !table.rows.contains { $0.passcode == old[i].passcode } }
        let new = table.rows.indices.filter { i in !old.contains { $0.passcode == table.rows[i].passcode } }
        let renamed = oneStep && gone == new && gone.count == 1 && old.count == table.rows.count
        rows = []
        for (i, row) in table.rows.enumerated() {
            if let same = old.first(where: { $0.passcode == row.passcode }) {
                rows.append(OscRow(id: same.id, passcode: row.passcode, rights: row.rights))
            } else if renamed, old[i].rights == row.rights {
                rows.append(OscRow(id: old[i].id, passcode: row.passcode, rights: row.rights))
            } else {
                made += 1
                rows.append(OscRow(id: "row\(made)", passcode: row.passcode, rights: row.rights))
            }
        }
    }
}

/// The OSC passcode table of the workspace settings (NET-02). Every edit is one engine command
/// (or one transaction), so each is one undo step, and every refusal is a line in `message` with
/// the table unchanged. A row is named by its `OscRow.id`: the code it acts on is the one the engine
/// has for that row when the edit is sent.
extension WorkspaceSettingsModel {
    /// The table as the engine has it.
    var osc: OscAccessTable { OscAccessTable(settings: store.model?.settings) }

    /// The row as the engine has it now; nil when it is gone.
    func oscRow(_ id: String) -> OscRow? { oscRows.rows.first { $0.id == id } }

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

    /// Changes the row's code to `text`: the old code goes and the new one takes its place with the
    /// same rights, in one undo step (the engine has no rename). False, with a message, when `text` is
    /// not four digits or is another row's, or the row is gone; nothing is sent.
    @discardableResult
    func changeOscPasscode(of id: String, to text: String) async -> Bool {
        let new = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let row = oscRow(id), let at = osc.rows.firstIndex(where: { $0.passcode == row.passcode }) else {
            return rowGone()
        }
        // The row's own code: sent, it would be a removal and an addition of an access code.
        if new == row.passcode { return true }
        guard OscAccessTable.isValidPasscode(new) else {
            message = loc("settings.osc.invalid")
            return false
        }
        guard !osc.rows.contains(where: { $0.passcode == new }) else {
            message = loc("settings.osc.duplicate")
            return false
        }
        guard
            let edit = Command.transaction([
                .removeOscPasscode(row.passcode), .addOscPasscode(new, rights: row.rights, at: at),
            ])
        else { return false }
        return await sendOsc(edit)
    }

    /// Deletes the row. One that is gone already needs nothing.
    @discardableResult
    func removeOscPasscode(of id: String) async -> Bool {
        guard let row = oscRow(id) else { return true }
        return await sendOsc(.removeOscPasscode(row.passcode))
    }

    /// Turns one right of the row (nil: the No Passcode row) on or off, the others staying as the
    /// engine has them. Edit and Control need View: without it all three go, in one command. False,
    /// with a message, when the row is gone; nothing is sent.
    @discardableResult
    func setOscRight(_ right: WritableKeyPath<OscRights, Bool>, _ on: Bool, of id: String?) async -> Bool {
        let row = id.flatMap(oscRow)
        if id != nil, row == nil { return rowGone() }
        var wanted = row?.rights ?? osc.noPasscode
        wanted[keyPath: right] = on
        return await sendOsc(.setOscRights(row?.passcode, wanted.view ? wanted : .none))
    }

    /// An edit of a row the engine no longer has goes nowhere, and says so.
    func rowGone() -> Bool {
        message = loc("settings.rowGone")
        return false
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

/// Where OSC is listening, as `osc_state` and `set_osc_port` answer.
struct OscListening: Equatable {
    let port: UInt16
    let open: Bool
    let error: String?
}

/// This Mac's addresses, for the OSC page (a console is pointed at one of them).
enum LocalAddresses {
    /// The IPv4 addresses of the interfaces that are up, loopback left out, in interface order.
    static func ipv4() -> [String] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(list) }
        var out: [String] = []
        for entry in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let flags = Int32(entry.pointee.ifa_flags)
            guard flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0, let addr = entry.pointee.ifa_addr,
                addr.pointee.sa_family == sa_family_t(AF_INET)
            else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            let result = getnameinfo(
                addr, socklen_t(addr.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST)
            if result == 0 {
                let text = String(cString: host)
                if !out.contains(text) { out.append(text) }
            }
        }
        return out
    }
}

extension WorkspaceSettingsModel {
    /// Reads where OSC listens (and this Mac's addresses). An engine without OSC leaves it unknown.
    func refreshOsc() async {
        let found = localAddresses()
        if addresses != found { addresses = found }
        if case .success(let ok) = await store.oscState(), case .osc(let port, let open, let error)? = ok.data {
            let now = OscListening(port: port, open: open, error: error)
            if oscListening != now { oscListening = now }
        } else if oscListening != nil {
            oscListening = nil
        }
    }

    /// Listens on the port typed. False, with a message, when it is not a port number or the engine
    /// could not open it: the old port goes on listening (the state is read again either way).
    @discardableResult
    func setOscPort(_ text: String) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard let value = UInt16(trimmed), value >= 1 else {
            message = loc("settings.osc.badPort")
            return false
        }
        if oscListening?.open == true, oscListening?.port == value {
            message = nil
            return true
        }
        let result = await store.setOscPort(value)
        await refreshOsc()
        switch result {
        case .success:
            message = nil
            return true
        case .failure(let e):
            message = String(format: loc("settings.osc.portRefused"), Int(value), e.detailText)
            return false
        }
    }

    /// Turns OSC access to this workspace on or off.
    func setOscEnabled(_ on: Bool) async -> Bool { await setSwitch(.oscEnabled, on) }
}
