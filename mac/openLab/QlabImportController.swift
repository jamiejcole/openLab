// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// "New Workspace from QLab File": the unsupported-cues step (only when the report lists any) and the
/// outputs step, in one modal window. Cancel anywhere hands back nil; Create Workspace hands back the
/// choices. The logic is `QlabUnsupportedChoices` and `QlabOutputChoices`.
@MainActor
final class QlabImportController: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    enum Step { case unsupported, outputs }

    let window: NSWindow
    let report: QlabReport
    private(set) var unsupported: QlabUnsupportedChoices
    private(set) var outputs: QlabOutputChoices
    private(set) var step: Step
    private(set) var result: QlabImportChoice?
    /// The bulk button pressed on the first step; the counts and the request follow from it.
    private var pressed: QlabUnsupportedChoices.Button = .deleteAll
    private let unsupportedTable = NSTableView()
    private let outputsTable = NSTableView()
    /// Cancel, Back, the two bulk buttons and Create Workspace; the step shows the ones it needs.
    let cancelButton = NSButton(title: loc("qlab.cancel"), target: nil, action: nil)
    let backButton = NSButton(title: loc("qlab.back"), target: nil, action: nil)
    let primaryButton = NSButton(title: "", target: nil, action: nil)
    let secondaryButton = NSButton(title: "", target: nil, action: nil)
    let createButton = NSButton(title: loc("qlab.create"), target: nil, action: nil)
    private let headingLabel = NSTextField(labelWithString: "")
    var headingText: String { headingLabel.stringValue }
    private let explanationLabel = NSTextField(wrappingLabelWithString: "")
    /// What each output table row shows: audio patches, then video outputs.
    private enum OutputRow {
        case audio(Int)
        case video(Int)
    }
    private var outputRows: [OutputRow] = []

    init(report: QlabReport, devices: [DeviceEntry], displays: [StageDisplay]) {
        self.report = report
        unsupported = QlabUnsupportedChoices(rows: report.unsupported)
        outputs = QlabOutputChoices(report: report, devices: devices, displays: displays)
        step = report.unsupported.isEmpty ? .outputs : .unsupported
        displayList = displays
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 460), styleMask: [.titled], backing: .buffered,
            defer: false)
        super.init()
        window.isReleasedWhenClosed = false
        window.title = loc("qlab.window.title")
        window.setAccessibilityIdentifier("qlab-import-window")
        outputRows = outputs.audio.indices.map { .audio($0) } + outputs.video.indices.map { .video($0) }
        build()
        showStep()
    }

    private let displayList: [StageDisplay]

    /// Runs the window modally; nil: Cancel.
    func run() -> QlabImportChoice? {
        window.center()
        _ = StandBack.runModal(window)
        window.orderOut(nil)
        return result
    }

    private func finish(_ choice: QlabImportChoice?) {
        result = choice
        if NSApp.modalWindow === window { NSApp.stopModal() }
    }

    // MARK: Building

    private func column(_ id: String, _ key: String, _ width: CGFloat, flexible: Bool = false) -> NSTableColumn {
        let c = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
        c.title = loc(key)
        c.width = width
        c.minWidth = min(width, 60)
        if flexible { c.resizingMask = .autoresizingMask } else { c.resizingMask = [] }
        return c
    }

    private func configure(_ table: NSTableView, _ columns: [NSTableColumn]) {
        for c in columns { table.addTableColumn(c) }
        table.dataSource = self
        table.delegate = self
        table.rowHeight = 26
        table.usesAlternatingRowBackgroundColors = true
        table.allowsColumnReordering = false
        table.allowsColumnResizing = false
        table.allowsEmptySelection = true
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
    }

    private func build() {
        configure(
            unsupportedTable,
            [
                column("number", "qlab.col.number", 56), column("name", "qlab.col.name", 160, flexible: true),
                column("type", "qlab.col.type", 110), column("reason", "qlab.col.reason", 180),
                column("action", "qlab.col.action", 120),
            ])
        unsupportedTable.setAX("qlab-unsupported-table")
        configure(
            outputsTable,
            [
                column("name", "qlab.col.output", 140), column("detail", "qlab.col.detail", 92),
                column("device", "qlab.col.device", 230), column("tag", "qlab.col.tag", 170, flexible: true),
            ])
        outputsTable.setAX("qlab-outputs-table")
        headingLabel.font = .systemFont(ofSize: 15, weight: .semibold)
        headingLabel.lineBreakMode = .byTruncatingTail
        explanationLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        explanationLabel.textColor = .secondaryLabelColor
        headingLabel.setAX("qlab-heading")
        explanationLabel.setAX("qlab-explanation")
        for (b, id, action) in [
            (cancelButton, "qlab-cancel", #selector(cancel)), (backButton, "qlab-back", #selector(back)),
            (primaryButton, "qlab-primary", #selector(primaryPressed)),
            (secondaryButton, "qlab-secondary", #selector(secondaryPressed)),
            (createButton, "qlab-create", #selector(create)),
        ] {
            b.setAX(id)
            b.target = self
            b.action = action
            b.bezelStyle = .rounded
            b.widthAnchor.constraint(greaterThanOrEqualToConstant: 80).isActive = true
        }
        cancelButton.keyEquivalent = "\u{1b}"

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .legacy
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.setAX("qlab-scroll")
        scrollView = scroll
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let buttons = NSStackView(views: [
            cancelButton, spacer, backButton, secondaryButton, primaryButton, createButton,
        ])
        buttons.spacing = 10
        let content = NSView()
        for v in [headingLabel, explanationLabel, scroll, buttons] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        NSLayoutConstraint.activate([
            headingLabel.topAnchor.constraint(equalTo: content.topAnchor, constant: 18),
            headingLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            headingLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            explanationLabel.topAnchor.constraint(equalTo: headingLabel.bottomAnchor, constant: 6),
            explanationLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            explanationLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            scroll.topAnchor.constraint(equalTo: explanationLabel.bottomAnchor, constant: 12),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.topAnchor.constraint(equalTo: scroll.bottomAnchor, constant: 14),
            buttons.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            buttons.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -16),
            content.widthAnchor.constraint(equalToConstant: 720),
            content.heightAnchor.constraint(equalToConstant: 460),
        ])
        window.contentView = content
    }

    private var scrollView: NSScrollView!

    // MARK: Steps

    private func showStep() {
        switch step {
        case .unsupported:
            let n = unsupported.rows.count
            headingLabel.stringValue = String(
                format: loc(n == 1 ? "qlab.unsupported.heading.one" : "qlab.unsupported.heading"), n)
            explanationLabel.stringValue = loc("qlab.unsupported.explanation")
            scrollView.documentView = unsupportedTable
            unsupportedTable.reloadData()
        case .outputs:
            headingLabel.stringValue = loc("qlab.outputs.heading")
            explanationLabel.stringValue = loc("qlab.outputs.explanation")
            scrollView.documentView = outputsTable
            outputsTable.reloadData()
        }
        syncButtons()
    }

    private func syncButtons() {
        let first = step == .unsupported
        backButton.isHidden = first
        backButton.isEnabled = !unsupported.rows.isEmpty
        createButton.isHidden = first
        primaryButton.isHidden = !first
        secondaryButton.isHidden = !first || unsupported.secondary == nil
        if first {
            primaryButton.title = loc(unsupported.primary.titleKey)
            if let s = unsupported.secondary { secondaryButton.title = loc(s.titleKey) }
        }
        primaryButton.keyEquivalent = first ? "\r" : ""
        createButton.keyEquivalent = first ? "" : "\r"
        window.defaultButtonCell = (first ? primaryButton : createButton).cell as? NSButtonCell
    }

    @objc func cancel() { finish(nil) }

    @objc func back() {
        guard !unsupported.rows.isEmpty else { return }
        step = .unsupported
        showStep()
    }

    @objc func primaryPressed() { advance(pressing: unsupported.primary) }

    @objc func secondaryPressed() {
        if let s = unsupported.secondary { advance(pressing: s) }
    }

    /// A bulk button applies its choice to every row; Continue keeps what the rows say.
    private func advance(pressing button: QlabUnsupportedChoices.Button) {
        switch button {
        case .deleteAll: unsupported.setAll(.delete)
        case .leaveAll: unsupported.setAll(.placeholder)
        case .proceed: break
        }
        pressed = button
        step = .outputs
        showStep()
    }

    @objc func create() {
        // Stay on the first step if Create is somehow pressed there.
        guard step == .outputs else { return }
        let r = unsupported.request(pressing: pressed)
        let counts = unsupported.counts(pressing: pressed)
        var choice = QlabImportChoice()
        choice.cues = r.cues
        choice.otherCues = r.other
        choice.audioPatches = outputs.audioRequest
        choice.videoOutputs = outputs.videoRequest
        choice.deleted = counts.deleted
        choice.placeholders = counts.placeholders
        choice.converted = unsupported.converted(of: Int(report.cueCount))
        finish(choice)
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int {
        tableView === unsupportedTable ? unsupported.rows.count : outputRows.count
    }

    private func label(_ text: String, color: NSColor = .labelColor) -> NSTextField {
        let f = NSTextField(labelWithString: text)
        f.lineBreakMode = .byTruncatingTail
        f.textColor = color
        f.toolTip = text
        f.font = .systemFont(ofSize: NSFont.systemFontSize)
        return f
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        cell(tableView, tableColumn, row).map(Self.centred)
    }

    /// A cell's view in the middle of its row: a table sets a cell view to the row's full height,
    /// which leaves a label's text at the top.
    private static func centred(_ view: NSView) -> NSView {
        let box = NSTableCellView()
        view.translatesAutoresizingMaskIntoConstraints = false
        box.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 2),
            view.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -2),
            view.centerYAnchor.constraint(equalTo: box.centerYAnchor),
        ])
        return box
    }

    private func cell(_ tableView: NSTableView, _ tableColumn: NSTableColumn?, _ row: Int) -> NSView? {
        guard let id = tableColumn?.identifier.rawValue else { return nil }
        if tableView === unsupportedTable {
            guard unsupported.rows.indices.contains(row) else { return nil }
            let r = unsupported.rows[row]
            switch id {
            case "number": return label(r.number)
            case "name": return label(r.name)
            case "type": return label(r.qlabType)
            case "reason": return label(loc("qlab.reason." + r.reason.rawValue))
            default:
                let pop = NSPopUpButton(frame: .zero, pullsDown: false)
                pop.controlSize = .small
                pop.addItems(withTitles: [loc("qlab.action.delete"), loc("qlab.action.placeholder")])
                pop.selectItem(at: unsupported.action(of: r) == .delete ? 0 : 1)
                pop.tag = row
                pop.target = self
                pop.action = #selector(actionChosen(_:))
                pop.setAX("qlab-unsupported-action-\(row)")
                return pop
            }
        }
        guard outputRows.indices.contains(row) else { return nil }
        switch (outputRows[row], id) {
        case (.audio(let i), "name"): return label(outputs.audio[i].patch.name)
        case (.video(let i), "name"): return label(outputs.video[i].output.name)
        case (.audio(let i), "detail"):
            return label(String(format: loc("qlab.outputs.count"), outputs.audio[i].patch.outputs))
        case (.video(let i), "detail"):
            let o = outputs.video[i].output
            return label(o.width > 0 ? "\(o.width) × \(o.height)" : loc("qlab.outputs.video"))
        case (.audio(let i), "device"): return audioPopup(i, row: row)
        case (.video(let i), "device"): return videoPopup(i, row: row)
        case (.audio(let i), _): return tagLabel(outputs.tag(audio: outputs.audio[i]))
        case (.video(let i), _): return tagLabel(outputs.tag(video: outputs.video[i]))
        }
    }

    private func tagLabel(_ tag: QlabOutputChoices.Tag?) -> NSTextField {
        switch tag {
        case .matched?: return label(loc("qlab.tag.matched"), color: .systemGreen)
        case .notFound?: return label(loc("qlab.tag.notFound"), color: .systemOrange)
        case nil: return label("")
        }
    }

    /// Not mapped, the system output, then each device of this Mac.
    private func audioPopup(_ i: Int, row: Int) -> NSPopUpButton {
        let pop = NSPopUpButton(frame: .zero, pullsDown: false)
        pop.controlSize = .small
        pop.addItem(withTitle: loc("qlab.outputs.notMapped"))
        pop.addItem(withTitle: loc("qlab.outputs.systemOutput"))
        for d in outputs.devices { pop.addItem(withTitle: d.name) }
        switch outputs.audio[i].choice {
        case .none: pop.selectItem(at: 0)
        case .systemOutput: pop.selectItem(at: 1)
        case .device(let name): pop.selectItem(at: 2 + (outputs.devices.firstIndex { $0.name == name } ?? -2))
        }
        pop.tag = row
        pop.target = self
        pop.action = #selector(deviceChosen(_:))
        pop.setAX("qlab-audio-device-\(i)")
        return pop
    }

    private func videoPopup(_ i: Int, row: Int) -> NSPopUpButton {
        let pop = NSPopUpButton(frame: .zero, pullsDown: false)
        pop.controlSize = .small
        pop.addItem(withTitle: loc("qlab.outputs.notMapped"))
        for d in displayList { pop.addItem(withTitle: d.name) }
        pop.selectItem(at: 1 + (displayList.firstIndex { $0.id == outputs.video[i].choice } ?? -1))
        pop.tag = row
        pop.target = self
        pop.action = #selector(displayChosen(_:))
        pop.setAX("qlab-video-display-\(i)")
        return pop
    }

    @objc func actionChosen(_ sender: NSPopUpButton) {
        guard unsupported.rows.indices.contains(sender.tag) else { return }
        unsupported.set(sender.indexOfSelectedItem == 0 ? .delete : .placeholder, for: unsupported.rows[sender.tag])
        syncButtons()
    }

    @objc func deviceChosen(_ sender: NSPopUpButton) {
        guard outputRows.indices.contains(sender.tag), case .audio(let i) = outputRows[sender.tag] else { return }
        let n = sender.indexOfSelectedItem
        outputs.audio[i].choice =
            n <= 0
            ? .none
            : n == 1
                ? .systemOutput : outputs.devices.indices.contains(n - 2) ? .device(outputs.devices[n - 2].name) : .none
        outputsTable.reloadData(forRowIndexes: [sender.tag], columnIndexes: IndexSet(0..<outputsTable.numberOfColumns))
    }

    @objc func displayChosen(_ sender: NSPopUpButton) {
        guard outputRows.indices.contains(sender.tag), case .video(let i) = outputRows[sender.tag] else { return }
        let n = sender.indexOfSelectedItem
        outputs.video[i].choice = displayList.indices.contains(n - 1) ? displayList[n - 1].id : nil
        outputsTable.reloadData(forRowIndexes: [sender.tag], columnIndexes: IndexSet(0..<outputsTable.numberOfColumns))
    }
}

/// The notes the new workspace comes with: counts and the report's notes, with Save Notes… and OK.
@MainActor
final class QlabNotesController: NSObject {
    let window: NSWindow
    let info: QlabNotesInfo
    /// The file to write the notes to (nil: cancelled); a seam for tests.
    var pickSaveURL: @MainActor (String) -> URL? = { name in
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        panel.allowedContentTypes = [.plainText]
        return StandBack.runModal(panel) == .OK ? panel.url : nil
    }
    /// Told when saving failed.
    var onError: @MainActor (String) -> Void = { _ in }
    let saveButton = NSButton(title: loc("qlab.notes.save"), target: nil, action: nil)
    let okButton = NSButton(title: loc("alert.ok"), target: nil, action: nil)
    let textView = NSTextView()

    init(info: QlabNotesInfo) {
        self.info = info
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 620, height: 420), styleMask: [.titled], backing: .buffered,
            defer: false)
        super.init()
        window.isReleasedWhenClosed = false
        window.title = loc("qlab.notes.window")
        window.setAccessibilityIdentifier("qlab-notes-window")
        build()
    }

    private func build() {
        let heading = NSTextField(labelWithString: String(format: loc("qlab.notes.title"), info.fileName))
        heading.font = .systemFont(ofSize: 15, weight: .semibold)
        heading.lineBreakMode = .byTruncatingMiddle
        heading.setAX("qlab-notes-heading")
        let counts = NSTextField(
            wrappingLabelWithString: [
                String(format: loc("qlab.notes.converted"), info.converted),
                String(format: loc("qlab.notes.placeholders"), info.placeholders),
                String(format: loc("qlab.notes.deleted"), info.deleted),
            ].joined(separator: "\n"))
        counts.setAX("qlab-notes-counts")
        var lines = info.report.notes.map { "• " + QlabNotesInfo.text(of: $0) }
        if info.report.notes.isEmpty { lines = [loc("qlab.notes.none")] }
        if info.report.notesOmitted > 0 {
            lines.append(String(format: loc("qlab.notes.omitted"), info.report.notesOmitted))
        }
        textView.string = lines.joined(separator: "\n")
        textView.isEditable = false
        textView.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        textView.textContainerInset = NSSize(width: 6, height: 6)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.setAX("qlab-notes-text")
        let scroll = NSScrollView()
        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .legacy
        scroll.borderType = .bezelBorder
        saveButton.setAX("qlab-notes-save")
        saveButton.target = self
        saveButton.action = #selector(save)
        okButton.setAX("qlab-notes-ok")
        okButton.keyEquivalent = "\r"
        okButton.target = self
        okButton.action = #selector(ok)
        for b in [saveButton, okButton] {
            b.bezelStyle = .rounded
            b.widthAnchor.constraint(greaterThanOrEqualToConstant: 80).isActive = true
        }
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let buttons = NSStackView(views: [spacer, saveButton, okButton])
        buttons.spacing = 10
        let content = NSView()
        for v in [heading, counts, scroll, buttons] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        NSLayoutConstraint.activate([
            heading.topAnchor.constraint(equalTo: content.topAnchor, constant: 18),
            heading.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            heading.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            counts.topAnchor.constraint(equalTo: heading.bottomAnchor, constant: 8),
            counts.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            counts.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            scroll.topAnchor.constraint(equalTo: counts.bottomAnchor, constant: 12),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.topAnchor.constraint(equalTo: scroll.bottomAnchor, constant: 14),
            buttons.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            buttons.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -16),
            content.widthAnchor.constraint(equalToConstant: 620),
            content.heightAnchor.constraint(equalToConstant: 420),
        ])
        window.contentView = content
        window.defaultButtonCell = okButton.cell as? NSButtonCell
    }

    func run() {
        window.center()
        _ = StandBack.runModal(window)
        window.orderOut(nil)
    }

    /// Writes the notes as plain text where the operator chose; the sheet stays up.
    @objc func save() {
        guard let url = pickSaveURL(String(format: loc("qlab.notes.fileName"), info.fileName)) else { return }
        do {
            try info.plainText.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            onError(String(format: loc("qlab.notes.saveFailed"), error.localizedDescription))
        }
    }

    @objc func ok() {
        if NSApp.modalWindow === window { NSApp.stopModal() }
    }
}
