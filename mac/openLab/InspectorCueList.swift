// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The inspector for a cue list chosen in the sidebar (see `CueListInspectorView`).
extension InspectorViewController {
    func wireCueListView() {
        cueListView.onRename = { [weak self] id, name in
            guard let self, !isLocked else { return }
            Task { @MainActor in
                if case .failure(let error) = await self.store.send(command: .renameCueList(id: id, name: name)) {
                    if case .engine(_, let message) = error {
                        self.showRefusal(String(format: loc("edit.error.refused"), message))
                    } else {
                        self.showRefusal(loc("edit.error.notConnected"))
                    }
                    self.render()
                }
            }
        }
        cueListView.onEndTyping = { [weak self] in self?.view.window?.makeFirstResponder(nil) }
    }

    /// Shows the list `id` in place of the cue shown, until a cue is shown again (`setSelection`,
    /// `leaveCueList`). Unknown lists are ignored.
    func showCueList(_ id: ModelID) {
        guard store.model?.cueLists.contains(where: { $0.id == id }) == true else { return }
        if let window = view.window, let responder = window.firstResponder as? NSView,
            responder is NSText || responder is HotkeyRecorder, responder.isDescendant(of: view)
        {
            window.makeFirstResponder(nil)
        }
        endGestures()
        shownCueList = id
        messageLabel.stringValue = ""
        render()
    }

    /// The cue list was left for a cue (a press in the cue list): the cues selected are shown again.
    func leaveCueList() {
        guard shownCueList != nil else { return }
        shownCueList = nil
        render(force: Set(InspectorField.allCases))
    }

    /// Draws the cue list inspector when a list is shown; false: a cue (or none) is, and the cue list view
    /// is hidden. A list that is gone leaves the mode.
    func renderCueList() -> Bool {
        guard let id = shownCueList else {
            cueListView.isHidden = true
            return false
        }
        guard let list = store.model?.cueLists.first(where: { $0.id == id }) else {
            shownCueList = nil
            cueListView.isHidden = true
            return false
        }
        tabView.isHidden = true
        tabBar.isHidden = true
        emptyLabel.isHidden = true
        messageLabel.isHidden = false
        cueListView.isHidden = false
        cueListView.show(name: list.name, id: id, locked: isLocked)
        updatePreviewButton()
        return true
    }
}
