// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The General tab's "new cues auto-load" (QLab 5, manual p. 128): cues made from now on have their
/// auto-load box ticked; the cues already in the show keep theirs.
extension WorkspaceSettingsWindowController {
    func autoLoadBox() -> NSView {
        autoLoadCheck.setAX("settings-new-cues-auto-load")
        autoLoadCheck.target = self
        autoLoadCheck.action = #selector(autoLoadClicked)
        return autoLoadCheck
    }

    func renderAutoLoad() {
        let on = model.newCuesAutoLoad
        autoLoadCheck.state = on == true ? .on : .off
        autoLoadCheck.isEnabled = on != nil
    }

    @objc private func autoLoadClicked() {
        let on = autoLoadCheck.state == .on
        Task { [weak self] in
            await self?.model.setNewCuesAutoLoad(on)
            self?.lastDrawn = nil
            self?.render()
        }
    }
}

extension WorkspaceSettingsModel {
    /// nil until the show is in.
    var newCuesAutoLoad: Bool? {
        guard let settings = store.model?.settings else { return nil }
        return settings["new_cues_auto_load"] == .bool(true)
    }

    func setNewCuesAutoLoad(_ on: Bool) async {
        guard newCuesAutoLoad != on else { return }
        switch await store.send(command: .setNewCuesAutoLoad(on)) {
        case .success: message = nil
        case .failure(let e): message = e.detailText
        }
    }
}
