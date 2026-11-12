// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// One network destination a Network cue can send to, as the settings table shows it.
struct NetworkPatchRow: Equatable {
    var id: ModelID
    var name: String
    var host: String
    var port: Int
    /// `udp` or `tcp`.
    var transport: String
}

/// The network destinations of the workspace settings (CT-30). Every edit is one engine command,
/// so each is one undo step, and every refusal is a line in `message` with the table unchanged.
extension WorkspaceSettingsModel {
    static let networkTransports = ["udp", "tcp"]

    /// The destinations as the engine has them.
    var networkPatches: [NetworkPatchRow] {
        (store.model?.networkPatches ?? []).map { p in
            NetworkPatchRow(
                id: p.id, name: p.raw["name"]?.stringValue ?? "", host: p.raw["host"]?.stringValue ?? "",
                port: Int(p.raw["port"]?.doubleValue ?? 0), transport: p.raw["protocol"]?.stringValue ?? "udp")
        }
    }

    /// Adds a destination after the last one: this Mac's OSC port over UDP, named by its place, to be edited.
    @discardableResult
    func addNetworkPatch() async -> Bool {
        let count = networkPatches.count
        var patch = CueEditor.defaultNetworkPatch(id: ModelID(UUID().uuidString.lowercased()))
        patch.set("name", .string(String(format: loc("settings.network.newName"), count + 1)))
        return await sendNetwork(.addNetworkPatch(patch, index: count))
    }

    /// Removes a destination. The engine refuses one a cue still sends to, and says so.
    @discardableResult
    func removeNetworkPatch(_ id: ModelID) async -> Bool {
        await sendNetwork(.removeNetworkPatch(id))
    }

    @discardableResult
    func setNetworkPatchName(_ id: ModelID, _ name: String) async -> Bool {
        await sendNetwork(.setNetworkPatch(id, "name", .string(name)))
    }

    @discardableResult
    func setNetworkPatchHost(_ id: ModelID, _ host: String) async -> Bool {
        await sendNetwork(.setNetworkPatch(id, "host", .string(host.trimmingCharacters(in: .whitespaces))))
    }

    /// False, with a message, when `text` is not a port number (1 to 65535); nothing is sent.
    @discardableResult
    func setNetworkPatchPort(_ id: ModelID, _ text: String) async -> Bool {
        guard let port = Int(text.trimmingCharacters(in: .whitespaces)), (1...65535).contains(port) else {
            message = loc("settings.network.badPort")
            return false
        }
        return await sendNetwork(.setNetworkPatch(id, "port", .int(Int64(port))))
    }

    @discardableResult
    func setNetworkPatchTransport(_ id: ModelID, _ transport: String) async -> Bool {
        guard Self.networkTransports.contains(transport) else { return false }
        return await sendNetwork(.setNetworkPatch(id, "protocol", .string(transport)))
    }

    private func sendNetwork(_ command: Command) async -> Bool {
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
