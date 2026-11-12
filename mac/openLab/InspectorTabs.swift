// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Which inspector tabs a cue kind has, in order. Basics and Triggers come first for every kind, then
/// the kind's own tabs as QLab orders them; a tab view only ever holds this list for the shown cue.
enum InspectorTabs {
    static func ids(forKind kind: String) -> [String] {
        let common = ["basics", "triggers"]
        switch kind {
        case "audio": return common + ["io", "timeLoops", "levels"]
        case "fade": return common + ["curve", "fadeLevels"]
        case "group": return common + ["mode"]
        case "load": return common + ["loadTime"]
        case "devamp": return common + ["devamp"]
        case "network": return common + ["network"]
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
        endGestures()  // a drag's view may be about to go; this may render, and so may do what follows
        guard tabView.tabViewItems.compactMap({ $0.identifier as? String }) != wanted else { return }
        let selected = tabView.selectedTabViewItem
        let byID = Dictionary(
            tabView.tabViewItems.compactMap { i in (i.identifier as? String).map { ($0, i) } },
            uniquingKeysWith: { a, _ in a })
        // Removing and re-adding selects tabs on the way; only the final selection may count as shown.
        let delegate = tabView.delegate
        tabView.delegate = nil
        defer {
            tabView.delegate = delegate
            updateTimeLoopsActive()
        }
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

    /// The Target row (QLab 5, manual pp. 126-127). A file cue: the file's path, chosen with a double-click
    /// or Choose, and cleared. A cue that acts on a cue: a number to type, the target's name beside it, a
    /// button to clear it and one to go to it. Any other cue: a disabled field and a not-applicable note.
    func renderTarget(_ cue: Cue, model: WorkspaceModel) {
        let kind = cue.kindType
        let isFile = kind == "audio"
        let isCueTarget = InspectorEdit.targetKinds.contains(kind)
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let targetID = isCueTarget ? cue.raw["kind"]?["target"]?.stringValue.map { ModelID($0) } : nil
        let target = targetID.flatMap { model.cue($0) }
        targetField.placeholderString = isFile && media.isEmpty ? loc("inspector.target.noFile") : nil
        // A file cue's field is not typed in, and a field that can be selected would swallow the double-click.
        if targetField.isEditable != isCueTarget {
            targetField.isEditable = isCueTarget
            targetField.isSelectable = isCueTarget
        }
        targetField.isEnabled = isFile || isCueTarget
        targetFieldWidth.isActive = !isFile
        if isFile {
            // The whole path (manual p. 126); the show file holds one relative to its folder.
            targetField.stringValue = CueRowMenu.mediaURL(media, showPath: store.document?.path)?.path ?? media
        } else if !isCueTarget {
            targetField.stringValue = ""
        }
        targetField.toolTip = targetField.stringValue.isEmpty ? nil : targetField.stringValue
        // A file is drawn as its folders and name with their icons, from the home folder down
        // (QLab 5's Target row); the plain field stays for a cue with no file, and for cue targets.
        let fileURL = isFile && !media.isEmpty ? CueRowMenu.mediaURL(media, showPath: store.document?.path) : nil
        targetPath.isHidden = fileURL == nil
        targetField.isHidden = fileURL != nil
        if let fileURL, targetPath.url != fileURL {
            targetPath.url = fileURL
            targetPath.pathItems = Self.shownPathItems(targetPath.pathItems, of: fileURL)
        } else if fileURL == nil {
            targetPath.url = nil
        }
        targetPath.toolTip = fileURL?.path
        chooseButton.isHidden = !isFile
        chooseButton.isEnabled = isFile && !isLocked
        clearTargetButton.isHidden = !isFile && !isCueTarget
        clearTargetButton.isEnabled = (isFile ? !media.isEmpty : targetID != nil) && !isLocked
        jumpTargetButton.isHidden = !isCueTarget
        jumpTargetButton.isEnabled = target != nil
        targetNote.isHidden = isFile
        if isCueTarget {
            let lookup: (ModelID) -> Cue? = { model.cue($0) }
            // A target that is gone leaves the field empty and the note says so.
            targetNote.stringValue =
                target?.listName(lookup: lookup)
                ?? loc(targetID == nil ? "inspector.target.none" : "inspector.target.missing")
        } else {
            targetNote.stringValue = loc("inspector.target.notApplicable")
        }
    }

    /// Clears the Target: an Audio cue loses its file, a control cue its target cue.
    @objc func clearTargetClicked() {
        guard !isLocked, let cue = cueForEdit else { return }
        if cue.kindType == "audio" {
            // `media` has its own property: `kind_fields` cannot name it.
            messageLabel.stringValue = ""
            send(.setProperty(cue.id, .media(nil)), cue: cue.id, key: "media", revert: [])
            render()
        } else if InspectorEdit.targetKinds.contains(cue.kindType) {
            // A number typed and not committed goes too, or ending the typing would set it again.
            cancel(.target)
            if targetField.currentEditor() != nil { view.window?.makeFirstResponder(nil) }
            messageLabel.stringValue = ""
            send(.setProperty(cue.id, .target(nil)), cue: cue.id, key: "target", revert: [.target])
        }
    }

    /// Selects the target cue as a click on its row would.
    @objc func jumpToTargetClicked() {
        guard let id = cue?.raw["kind"]?["target"]?.stringValue.map({ ModelID($0) }), store.model?.cue(id) != nil
        else { return }
        onJumpToTarget?(id)
    }
}

/// The Target field. A file cue's is not typed in: a double-click on it chooses the file.
extension InspectorViewController {
    /// The path items a file's Target row shows: a file in the user's home folder starts at that
    /// folder (the volume and the folders above it say nothing); any other path is shown whole.
    static func shownPathItems(_ items: [NSPathControlItem], of url: URL) -> [NSPathControlItem] {
        let all = url.standardizedFileURL.pathComponents.filter { $0 != "/" }
        let home = FileManager.default.homeDirectoryForCurrentUser.standardizedFileURL.pathComponents.filter {
            $0 != "/"
        }
        guard !home.isEmpty, all.count > home.count, Array(all.prefix(home.count)) == home else { return items }
        // The control's items end at the file; the home folder is `all.count - home.count` before it.
        let keep = all.count - home.count + 1
        return items.count >= keep ? Array(items.suffix(keep)) : items
    }
}

/// A file cue's Target drawn as a path; a double-click chooses another file.
final class TargetPathControl: NSPathControl {
    var onDoubleClick: (() -> Void)?

    override func mouseDown(with event: NSEvent) {
        if isEnabled, event.clickCount == 2 { onDoubleClick?() }
    }
}

final class TargetField: NSTextField {
    var onDoubleClick: (() -> Void)?

    override func mouseDown(with event: NSEvent) {
        if !isEditable, isEnabled, event.clickCount == 2 {
            onDoubleClick?()
        } else {
            super.mouseDown(with: event)
        }
    }
}
