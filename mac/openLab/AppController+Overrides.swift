// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The Override Controls panel (PB-10) and its link to the engine. The override is engine-wide and
/// in memory only: the panel shows what the engine's `status` says, so a restarted engine (off
/// again) or another client's change is followed, and a relaunched app starts with it on.
extension AppController {
    func showOverrideControls() {
        overrides.onChange = { [weak self] disabled in Task { await self?.setNetworkOutputDisabled(disabled) } }
        syncOverridePanel()
        overrides.show()
    }

    /// Redraws the panel whenever this store's `status` changes.
    func watchOverride(of store: WorkspaceStore) {
        // `$status` publishes before it changes: hop one turn so the stores hold the new value.
        statusSubs[ObjectIdentifier(store)] = store.$status
            .map { $0?.networkOutputDisabled }
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in MainActor.assumeIsolated { self?.syncOverridePanel() } }
    }

    /// The engine's override state as the stores last heard it; nil with no status to go by.
    var networkOutputDisabled: Bool? {
        let flags = session.orderedStores.compactMap { $0.store.status?.networkOutputDisabled }
        return flags.isEmpty ? nil : flags.contains(true)
    }

    func syncOverridePanel() { overrides.sync(networkDisabled: networkOutputDisabled) }

    func setNetworkOutputDisabled(_ disabled: Bool) async {
        if case .failure(let e) = await session.setOutputOverrides(networkDisabled: disabled) {
            beep()
            for (_, store) in session.orderedStores { log(e, on: store) }
            // The engine did not take it: the switch goes back to what the engine says.
            syncOverridePanel()
        }
    }
}
