// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

extension InspectorViewController {
    /// The I/O tab of an Audio cue (QLab 5's has the same pop-up): the output patch the cue plays
    /// through, chosen among the workspace's patches.
    func ioTab(row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView) -> NSTabViewItem {
        outputPatchPopup.setAX("inspector-output-patch", label: loc("inspector.io.patch"))
        outputPatchPopup.target = self
        outputPatchPopup.action = #selector(outputPatchChosen)
        outputPatchPopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 200).isActive = true
        let content = column([row(loc("inspector.io.patch"), outputPatchPopup)], nil)
        content.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        let item = NSTabViewItem(identifier: "io")
        item.label = loc("inspector.tab.io")
        item.view = content
        return item
    }

    /// The pop-up follows the model: every output patch by name. Rebuilt only when the list changes,
    /// so an open menu is not disturbed. A cue whose patch is gone shows no choice.
    func renderIO(_ cue: Cue, model: WorkspaceModel) {
        guard cue.kindType == "audio" else { return }
        let patches = model.patches
        let names = patches.map { p -> String in
            let name = p.raw["name"]?.stringValue ?? ""
            return name.isEmpty ? loc("settings.audio.untitledPatch") : name
        }
        let ids = patches.map(\.id)
        if ids != outputPatchIDs || names != outputPatchNames {
            outputPatchIDs = ids
            outputPatchNames = names
            outputPatchPopup.menu?.removeAllItems()
            for name in names {
                outputPatchPopup.menu?.addItem(NSMenuItem(title: name, action: nil, keyEquivalent: ""))
            }
        }
        let current = cue.raw["kind"]?["patch"]?.stringValue
        let chosen = patches.firstIndex { $0.id.raw == current } ?? -1
        if outputPatchPopup.indexOfSelectedItem != chosen { outputPatchPopup.selectItem(at: chosen) }
    }

    @objc func outputPatchChosen() {
        guard !isLocked, let cue = cueForEdit, cue.kindType == "audio" else { return }
        let i = outputPatchPopup.indexOfSelectedItem
        guard outputPatchIDs.indices.contains(i) else { return }
        let id = outputPatchIDs[i]
        guard id.raw != (cue.raw["kind"]?["patch"]?.stringValue ?? "") else { return }
        messageLabel.stringValue = ""
        // A refusal (the engine's own check of the patch) shows the cue's patch again.
        send(.setProperty(cue.id, .kindFields(["patch": .string(id.raw)])), cue: cue.id, key: "patch", revert: [])
    }
}
