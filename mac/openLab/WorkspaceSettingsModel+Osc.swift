// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
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

    /// Sets the rights of the row now showing `passcode` (nil: the No Passcode row) to what its
    /// boxes show. Edit and Control need View: without it all three go, in one command. False
    /// when no such row is in the table; nothing is sent.
    @discardableResult
    func setOscRights(_ wanted: OscRights, of passcode: String?) async -> Bool {
        if let passcode, !osc.rows.contains(where: { $0.passcode == passcode }) { return false }
        return await sendOsc(.setOscRights(passcode, wanted.view ? wanted : .none))
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
    func setOscEnabled(_ on: Bool) async { await setSwitch(.oscEnabled, on) }
}
