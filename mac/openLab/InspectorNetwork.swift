// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

extension InspectorViewController {
    /// The Settings tab of a Network cue (CT-30; QLab 5, manual p. 366): the patch it sends to, its
    /// message as one line of text, and a Send button that sends the message now.
    func networkTab(
        row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView
    ) -> NSTabViewItem {
        networkPatchPopup.setAX("inspector-network-patch")
        networkPatchPopup.target = self
        networkPatchPopup.action = #selector(networkPatchChosen)
        networkPatchPopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 160).isActive = true
        networkMessageField.placeholderString = loc("inspector.network.messageHint")
        networkMessageField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        networkSendButton.setAX("inspector-network-send")
        networkSendButton.target = self
        networkSendButton.action = #selector(networkSendClicked)
        networkSendButton.setContentHuggingPriority(.required, for: .horizontal)
        let messageRow = NSStackView(views: [networkMessageField, networkSendButton])
        messageRow.spacing = 6
        let content = column(
            [
                row(loc("inspector.network.patch"), networkPatchPopup),
                row(loc("inspector.network.message"), messageRow),
            ],
            nil)
        content.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        for v in content.arrangedSubviews {
            v.widthAnchor.constraint(equalTo: content.widthAnchor, constant: -24).isActive = true
        }
        messageRow.widthAnchor.constraint(equalTo: content.widthAnchor, constant: -24).isActive = true
        let item = NSTabViewItem(identifier: "network")
        item.label = loc("inspector.tab.network")
        item.view = content
        return item
    }

    /// The patch pop-up follows the model: every network patch, then "no patch". Rebuilt only when
    /// the list changes, so an open menu is not disturbed.
    func renderNetwork(_ cue: Cue, model: WorkspaceModel) {
        guard cue.kindType == "network" else { return }
        let patches = model.networkPatches
        let names = patches.map { $0.raw["name"]?.stringValue ?? "" }
        let ids: [ModelID?] = patches.map { $0.id } + (patches.isEmpty ? [] : [nil]) + [nil]
        if ids != networkPatchIDs || names != networkPatchNames {
            networkPatchIDs = ids
            networkPatchNames = names
            networkPatchPopup.menu?.removeAllItems()
            for name in names {
                networkPatchPopup.menu?.addItem(NSMenuItem(title: name, action: nil, keyEquivalent: ""))
            }
            if !patches.isEmpty { networkPatchPopup.menu?.addItem(.separator()) }
            networkPatchPopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.network.unpatched"), action: nil, keyEquivalent: ""))
        }
        let destination = cue.raw["kind"]?["destination"]?.stringValue
        let chosen = patches.firstIndex { $0.id.raw == destination } ?? (ids.count - 1)
        if networkPatchPopup.indexOfSelectedItem != chosen { networkPatchPopup.selectItem(at: chosen) }
    }

    @objc func networkPatchChosen() {
        let i = networkPatchPopup.indexOfSelectedItem
        guard networkPatchIDs.indices.contains(i) else { return }
        let id = networkPatchIDs[i] ?? .unpatched
        sendEach(key: "destination") { cue in
            guard cue.kindType == "network", id.raw != (cue.raw["kind"]?["destination"]?.stringValue ?? "") else {
                return nil
            }
            return .setProperty(cue.id, .destination(id))
        }
    }

    /// Sends the message now (the engine's send-now, `preview` on a Network cue). Text typed and not
    /// yet committed goes first, so what is sent is what the field shows; if it cannot be committed
    /// nothing is sent.
    @objc func networkSendClicked() {
        guard !isLocked, let cue, cue.kindType == "network" else { return }
        let id = cue.id
        // The click ends the typing, as Return does, so Space fires GO next (the field would keep
        // the focus and take the space as text otherwise).
        view.window?.makeFirstResponder(nil)
        Task { @MainActor [weak self] in
            guard let self, await commitPendingEdits(), !isLocked else { return }
            onTransport?(.preview(cue: id))
        }
    }
}
