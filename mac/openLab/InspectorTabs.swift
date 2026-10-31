// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Which inspector tabs a cue kind has, in order. Basics and Triggers come first for every kind, then
/// the kind's own tabs as QLab orders them; a tab view only ever holds this list for the shown cue.
enum InspectorTabs {
    static func ids(forKind kind: String) -> [String] {
        let common = ["basics", "triggers"]
        switch kind {
        case "audio": return common + ["timeLoops", "levels"]
        case "fade": return common + ["curve", "fadeLevels"]
        case "group": return common + ["mode"]
        case "load": return common + ["loadTime"]
        case "devamp": return common + ["devamp"]
        default: return common
        }
    }
}

extension InspectorViewController {
    /// Brings the tab view to `ids` in order, keeping the shown tab when it stays. Idempotent, so a
    /// model change that keeps the kind does not touch the tabs (and a typed field keeps its focus).
    func syncTabs(forKind kind: String) {
        let wanted = InspectorTabs.ids(forKind: kind)
        guard tabView.tabViewItems.compactMap({ $0.identifier as? String }) != wanted else { return }
        let selected = tabView.selectedTabViewItem
        let byID = Dictionary(
            tabView.tabViewItems.compactMap { i in (i.identifier as? String).map { ($0, i) } },
            uniquingKeysWith: { a, _ in a })
        for item in tabView.tabViewItems { tabView.removeTabViewItem(item) }
        for id in wanted {
            if let item = byID[id] ?? tabItems[id] { tabView.addTabViewItem(item) }
        }
        if let selected, tabView.tabViewItems.contains(where: { $0 === selected }) {
            tabView.selectTabViewItem(selected)
        }
    }

    /// The Devamp cue's Settings tab (QLab's Devamp Settings): start the next cue, stop the target
    /// when the slice ends, and which loop is left.
    func devampTab(
        row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView
    ) -> NSTabViewItem {
        devampStartNextCheck.setAX("inspector-devamp-start-next")
        devampStopCheck.setAX("inspector-devamp-stop-target")
        devampModePopup.setAX("inspector-devamp-mode")
        for key in Self.devampModes {
            devampModePopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.devampMode.\(key)"), action: nil, keyEquivalent: ""))
        }
        for c in [devampStartNextCheck, devampStopCheck, devampModePopup as NSControl] { c.target = self }
        devampStartNextCheck.action = #selector(devampChanged)
        devampStopCheck.action = #selector(devampChanged)
        devampModePopup.action = #selector(devampChanged)
        let content = column(
            [row(loc("inspector.devampMode"), devampModePopup), devampStartNextCheck, devampStopCheck], nil)
        content.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        let item = NSTabViewItem(identifier: "devamp")
        item.label = loc("inspector.tab.devamp")
        item.view = content
        return item
    }

    func renderDevamp(_ cue: Cue) {
        guard cue.kindType == "devamp" else { return }
        let kind = cue.raw["kind"] ?? .null
        devampStartNextCheck.state = kind["start_next_cue"] == .bool(true) ? .on : .off
        devampStopCheck.state = kind["stop_target_when_slice_ends"] == .bool(true) ? .on : .off
        devampModePopup.selectItem(at: Self.devampModes.firstIndex(of: kind["devamp_mode"]?.stringValue ?? "") ?? 0)
    }

    @objc func devampChanged() {
        let start = devampStartNextCheck.state == .on
        let stop = devampStopCheck.state == .on
        let mode = Self.devampModes[max(0, min(devampModePopup.indexOfSelectedItem, Self.devampModes.count - 1))]
        sendKind(
            { kind in
                var kind = kind
                kind.set("start_next_cue", .bool(start))
                kind.set("stop_target_when_slice_ends", .bool(stop))
                kind.set("devamp_mode", .string(mode))
                return kind
            }, key: "devamp")
        render()
    }

    /// Clears the Target: an Audio cue loses its file, a control cue its target cue.
    @objc func clearTargetClicked() {
        guard !isLocked, let cue = cueForEdit else { return }
        if cue.kindType == "audio" {
            sendKind(
                { kind in
                    var kind = kind
                    kind.set("media", .null)
                    return kind
                }, key: "media")
            render()
        } else if InspectorEdit.targetKinds.contains(cue.kindType) {
            messageLabel.stringValue = ""
            send(.setProperty(cue.id, .target(nil)), cue: cue.id, key: "target", revert: [.target])
        }
    }
}
