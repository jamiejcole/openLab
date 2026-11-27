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
        case "target": return common + ["newTarget"]
        case "devamp": return common + ["devamp"]
        case "network": return common + ["network"]
        case "video": return common + ["io", "geometry", "timeLoops"]
        default: return common
        }
    }

    /// The tabs of `cue` as it is now. A Video cue has a Levels tab, where an Audio cue has it, only
    /// while its probed movie has sound (QLab 5 manual p. 214); a Fade aimed at a Video cue has its
    /// Levels tab only while that cue has sound (p. 242), and a Geometry tab after it (p. 242).
    static func ids(for cue: Cue, model: WorkspaceModel, media: [ModelID: CueMediaDuration]) -> [String] {
        var ids = ids(forKind: cue.kindType)
        switch cue.kindType {
        case "video":
            if cue.sounds(media[cue.id]) { ids.append("levels") }
        case "fade":
            let target = cue.raw["kind"]?["target"]?.stringValue.flatMap({ model.cue(ModelID($0)) })
            if let target, target.kindType == "video", !target.sounds(media[target.id]) {
                ids.removeAll { $0 == "fadeLevels" }
            }
            // A picture's values for a Video target, or for none yet: QLab shows a Fade every tab it
            // can have until it has a target, then only the target's (p. 241).
            if target == nil || target?.kindType == "video" { ids.append("fadeGeometry") }
        default: break
        }
        return ids
    }
}

extension InspectorViewController {
    /// Brings the tab view to `wanted` in order, showing the tab last chosen on a cue of `kind` (the
    /// choice is remembered per cue type, for the session) when `wanted` has it, else the first. A
    /// fallback does not change the choice, so the next cue that has the tab shows it again.
    /// Idempotent, so a model change that keeps the tabs does not touch them (and a typed field
    /// keeps its focus).
    func syncTabs(_ wanted: [String], kind: String, cue: ModelID? = nil) {
        // The same cue turned into another kind keeps the tab the operator is on when the new kind has
        // it; that becomes the new kind's choice. Another cue shows its own kind's choice.
        if let cue, cue == shownTabCue, kind != shownKind,
            let current = tabView.selectedTabViewItem?.identifier as? String, wanted.contains(current)
        {
            chosenTabs[kind] = current
        }
        shownTabCue = cue
        shownKind = kind
        guard tabView.tabViewItems.compactMap({ $0.identifier as? String }) != wanted else {
            selectChosen(wanted)
            return
        }
        endGestures()  // a drag's view may be about to go; this may render, and so may do what follows
        let byID = Dictionary(
            tabView.tabViewItems.compactMap { i in (i.identifier as? String).map { ($0, i) } },
            uniquingKeysWith: { a, _ in a })
        // Removing and re-adding selects tabs on the way; only the final selection may count as shown.
        let delegate = tabView.delegate
        tabView.delegate = nil
        defer {
            tabView.delegate = delegate
            syncTabBar()
            updateTimeLoopsActive()
        }
        for item in tabView.tabViewItems { tabView.removeTabViewItem(item) }
        for id in wanted {
            if let item = byID[id] ?? tabItems[id] { tabView.addTabViewItem(item) }
        }
        selectChosen(wanted)
    }

    /// Selects the tab `kind` remembers (or the first) when the tab view is not already on it. A
    /// selection made by the user is never moved by a model change on the same cue.
    private func selectChosen(_ wanted: [String]) {
        guard let shown = Self.shownTab(chosen: chosenTabs[shownKind], wanted: wanted),
            tabView.selectedTabViewItem?.identifier as? String != shown,
            let item = tabView.tabViewItems.first(where: { ($0.identifier as? String) == shown })
        else { return }
        let delegate = tabView.delegate
        tabView.delegate = nil
        tabView.selectTabViewItem(item)
        tabView.delegate = delegate
        syncTabBar()
        updateTimeLoopsActive()
    }

    /// The tab a tab list shows: the last choice on this cue type when the list has it, else its first tab.
    static func shownTab(chosen: String?, wanted: [String]) -> String? {
        if let chosen, wanted.contains(chosen) { return chosen }
        return wanted.first
    }

    /// Draws the tab bar for the tab view's tabs: every title whole, the chosen one marked.
    func syncTabBar() {
        tabBar.show(
            tabView.tabViewItems.compactMap { i in (i.identifier as? String).map { ($0, i.label) } },
            selected: tabView.selectedTabViewItem?.identifier as? String)
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
        devampStartNextCheck.action = #selector(devampChanged(_:))
        devampStopCheck.action = #selector(devampChanged(_:))
        devampModePopup.action = #selector(devampChanged(_:))
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

    /// Sets only the field of the control that changed, so with several cues selected the others keep
    /// their own (a mixed box or pop-up is not written back).
    @objc func devampChanged(_ sender: NSControl) {
        let field: String
        let value: JSONValue
        if sender === devampModePopup {
            let i = devampModePopup.indexOfSelectedItem
            guard Self.devampModes.indices.contains(i) else { return }
            (field, value) = ("devamp_mode", .string(Self.devampModes[i]))
        } else if let check = sender as? NSButton {
            let on = check.state != .off
            check.allowsMixedState = false
            check.state = on ? .on : .off
            field = check === devampStartNextCheck ? "start_next_cue" : "stop_target_when_slice_ends"
            value = .bool(on)
        } else {
            return
        }
        sendKindEach(
            { cue, kind in
                guard cue.kindType == "devamp" else { return kind }
                var kind = kind
                kind.set(field, value)
                return kind
            }, key: "devamp")
        render()
    }

    /// The Target row (QLab 5, manual pp. 126-127). A file cue: the file's path, chosen with a double-click
    /// or Choose, and cleared. A cue that acts on a cue: a number to type, the target's name beside it, a
    /// button to clear it and one to go to it. Any other cue: a disabled field and a not-applicable note.
    func renderTarget(_ cue: Cue, model: WorkspaceModel) {
        let kind = cue.kindType
        let isFile = cue.takesFile
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
        targetPath.show(url: fileURL)
        targetField.isHidden = fileURL != nil
        revealTargetButton.isHidden = !isFile
        revealTargetButton.isEnabled = fileURL != nil
        chooseButton.isHidden = !isFile
        chooseButton.isEnabled = isFile && !isLocked
        clearTargetButton.isHidden = !isFile && !isCueTarget
        clearTargetButton.isEnabled = (isFile ? !media.isEmpty : targetID != nil) && !isLocked
        jumpTargetButton.isHidden = !isCueTarget
        jumpTargetButton.isEnabled = target != nil
        // A file cue whose file cannot be used says why, beside the file (the full words are its tip).
        let fileProblem =
            isFile
            ? (store.problems[cue.id] ?? []).first { $0.type == "media_missing" || $0.type == "media_unsupported" }
            : nil
        targetNote.isHidden = isFile && fileProblem == nil
        targetNote.toolTip = nil
        targetNote.textColor = fileProblem == nil ? .secondaryLabelColor : .systemOrange
        targetNote.setContentHuggingPriority(isFile ? .required : NSLayoutConstraint.Priority(1), for: .horizontal)
        targetNote.setContentCompressionResistancePriority(
            isFile ? .defaultHigh : .defaultLow, for: .horizontal)
        if let fileProblem {
            targetNote.stringValue = loc(
                fileProblem.type == "media_missing"
                    ? "inspector.target.fileMissing" : "inspector.target.fileUnsupported")
            targetNote.toolTip = ProblemText.problem(fileProblem, label: WarningRows.labeler(model))
        } else if isCueTarget {
            let lookup: (ModelID) -> Cue? = { model.cue($0) }
            // A target that is gone leaves the field empty and the note says so.
            targetNote.stringValue =
                target?.listName(lookup: lookup)
                ?? loc(targetID == nil ? "inspector.target.none" : "inspector.target.missing")
        } else {
            targetNote.stringValue = loc("inspector.target.notApplicable")
        }
    }

    /// Clears the Target: an Audio or Video cue loses its file, a control cue its target cue.
    @objc func clearTargetClicked() {
        guard !isLocked, let cue = cueForEdit else { return }
        if cue.takesFile {
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

    /// Gives a file row's reveal button its glyph, name and action (the Target row's and the I/O tab's).
    func configureRevealButton(_ button: NSButton, id: String) {
        button.setAX(id, label: loc("inspector.target.reveal"))
        button.isBordered = false
        button.image = Icon.fieldReveal.image(loc("inspector.target.reveal"), compact: true)
        button.imagePosition = .imageOnly
        button.toolTip = loc("inspector.target.reveal")
        button.target = self
        button.action = #selector(revealClicked)
        button.setContentHuggingPriority(.required, for: .horizontal)
        button.setContentCompressionResistancePriority(.required, for: .horizontal)
    }

    /// The reveal button: the shown cue's file selected in Finder; a file that is gone shows its folder.
    @objc func revealClicked() {
        guard let cue = cueForEdit, cue.takesFile, let media = cue.raw["kind"]?["media"]?.stringValue,
            let url = CueRowMenu.mediaURL(media, showPath: store.document?.path)
        else { return }
        let fm = FileManager.default
        if fm.fileExists(atPath: url.path) {
            revealFile(url)
        } else if fm.fileExists(atPath: url.deletingLastPathComponent().path) {
            revealFile(url.deletingLastPathComponent())
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

/// A file cue's Target drawn as a path; a double-click chooses another file. When the path does not fit,
/// the middle folders give way to one ellipsis item (the home folder and the file stay), never a name cut
/// mid-word; the tooltip holds the whole path.
final class TargetPathControl: NSPathControl {
    var onDoubleClick: (() -> Void)?
    /// The bordered field the path sits in: rows hold this, not the control, and it hides with it.
    let box = PathFieldBox()
    override var isHidden: Bool { didSet { box.isHidden = isHidden } }

    /// The path as the Target row and the I/O tab's file row draw it: small, read-only, in a field box,
    /// giving up its middle folders before it holds the inspector wider.
    func configure(id: String, label: String, onDoubleClick: @escaping () -> Void) {
        setAX(id, label: label)
        self.onDoubleClick = onDoubleClick
        pathStyle = .standard
        isEditable = false
        controlSize = .small
        font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        isHidden = true
        setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        box.hold(self)
        InspectorViewController.prefersWidth(box)
    }

    /// Shows `text` (no file, several files) in the path's own box and place, in the secondary colour,
    /// so the field looks the same with or without a file.
    func showNote(_ text: String) {
        isHidden = false
        url = nil
        let item = NSPathControlItem()
        item.title = text
        item.attributedTitle = NSAttributedString(
            string: text,
            attributes: [
                .foregroundColor: NSColor.secondaryLabelColor,
                .font: font ?? NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
            ])
        setFullItems([item])
        toolTip = nil
    }

    /// Shows a file's folders and name (the control hides for none); the tooltip holds the whole path.
    func show(url: URL?) {
        isHidden = url == nil
        if let url, self.url != url {
            self.url = url
            setFullItems(InspectorViewController.shownPathItems(pathItems, of: url))
        } else if url == nil {
            self.url = nil
        }
        toolTip = url?.path
    }

    /// The file's path is the value VoiceOver reads and a test finds the file by.
    override func accessibilityValue() -> Any? { url?.path }
    private var fullItems: [NSPathControlItem] = []
    private var shownCount = -1

    /// The width one item takes: its icon, gaps and separator around the title.
    static func itemWidth(_ title: String, font: NSFont) -> CGFloat {
        ceil((title as NSString).size(withAttributes: [.font: font]).width) + 30
    }

    /// How many of the folders just before the file are kept (the first item and the file always are,
    /// unless nothing fits), for `titles` in `width`. `nil` when everything fits.
    static func keptTail(titles: [String], width: CGFloat, font: NSFont) -> (head: Bool, tail: Int)? {
        let widths = titles.map { itemWidth($0, font: font) }
        if widths.reduce(0, +) <= width { return nil }
        guard titles.count > 2 else { return nil }
        let ellipsis = itemWidth("\u{2026}", font: font)
        var head = true
        var used = widths[0] + widths[widths.count - 1] + ellipsis
        if used > width {
            head = false
            used -= widths[0]
        }
        var tail = 0
        var index = widths.count - 2
        while index >= 1, used + widths[index] <= width {
            used += widths[index]
            tail += 1
            index -= 1
        }
        return (head, tail)
    }

    func setFullItems(_ items: [NSPathControlItem]) {
        fullItems = items
        shownCount = -1
        pathItems = items
        needsLayout = true
    }

    override func layout() {
        super.layout()
        fitItems()
    }

    private func fitItems() {
        let font = self.font ?? .systemFont(ofSize: NSFont.smallSystemFontSize)
        let titles = fullItems.map(\.title)
        guard !titles.isEmpty, bounds.width > 0 else { return }
        let kept = Self.keptTail(titles: titles, width: bounds.width, font: font)
        let key = kept.map { ($0.head ? 1000 : 0) + $0.tail } ?? -1
        guard key != shownCount else { return }
        shownCount = key
        guard let kept else {
            pathItems = fullItems
            return
        }
        let dots = NSPathControlItem()
        dots.title = "\u{2026}"
        var items: [NSPathControlItem] = kept.head ? [fullItems[0]] : []
        items.append(dots)
        items.append(contentsOf: fullItems[(fullItems.count - 1 - kept.tail)...])
        pathItems = items
    }

    override func mouseDown(with event: NSEvent) {
        if isEnabled, event.clickCount == 2 { onDoubleClick?() }
    }
}

/// A path control's field box: a thin rounded border round the control, as a text field has. Its
/// first baseline is the path text's, so a row label lines up with the text and not the box.
final class PathFieldBox: NSBox {
    private weak var path: NSControl?

    func hold(_ control: NSControl) {
        path = control
        boxType = .custom
        borderType = .lineBorder
        borderWidth = 1
        cornerRadius = 3
        borderColor = .tertiaryLabelColor
        // A text field's white in light mode; in dark mode a step lighter than the tab, as the reference's.
        fillColor = NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 1, alpha: 0.08) : .textBackgroundColor
        }
        titlePosition = .noTitle
        contentViewMargins = NSSize(width: 2, height: 1)
        contentView = control
        heightAnchor.constraint(equalToConstant: 20).isActive = true
    }

    override var firstBaselineOffsetFromTop: CGFloat {
        guard let path, let parent = path.superview else { return super.firstBaselineOffsetFromTop }
        let top = bounds.height - parent.convert(NSPoint(x: 0, y: path.frame.maxY), to: self).y
        return top + path.firstBaselineOffsetFromTop
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
