// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Tools > Find Missing Files (WS-03; QLab 5, manual p. 151): the operator points at a folder, the
/// engine searches it for the files the workspace's cues are missing and lists what it found, the
/// operator ticks the matches to use, and Relink points the ticked cues at them as one undo step.
/// A file found more than once is never guessed: a sheet asks which one. Nothing changes until
/// Relink. Not open in Show Mode.
@MainActor
final class MissingFilesWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource,
    NSTableViewDelegate
{
    /// One cue whose file was found: once (`chosen` set by the search) or several times (`chosen`
    /// set by the operator's pick).
    struct Row: Equatable {
        let cue: ModelID
        /// Number and name, as the Warnings tab titles a cue.
        let title: String
        /// The reference the cue had when the search answered: the cue is relinked only if it
        /// still has it (an edit made meanwhile wins).
        let was: String?
        /// The files found for it, as full paths.
        let candidates: [String]
        var chosen: String?
        var ticked: Bool
    }

    let store: WorkspaceStore
    /// The folder to search (nil: cancelled). The app sets it to its folder panel.
    var pickFolder: @MainActor () -> URL? = { nil }
    /// How a sheet is put up (tests and scenarios keep it off screen).
    var presentSheet: (@MainActor (NSWindow) -> Void)?
    private(set) var rows: [Row] = []
    private(set) var choiceSheet: MissingFileChoiceSheet?
    /// A search or a relink is waiting for the engine.
    private(set) var isBusy = false
    /// The folder last searched (a full path): the files found are shown from it down.
    private(set) var searched: String?
    /// Rows still to ask about after a search, in list order.
    private var toAsk: [ModelID] = []

    let introLabel = NSTextField(wrappingLabelWithString: loc("missingFiles.intro"))
    let folderLabel = NSTextField(labelWithString: "")
    let chooseFolderButton = NSButton(title: loc("missingFiles.chooseFolder"), target: nil, action: nil)
    let table = NSTableView()
    let scroll = NSScrollView()
    let summaryLabel = NSTextField(wrappingLabelWithString: "")
    let relinkButton = NSButton(title: loc("missingFiles.relink"), target: nil, action: nil)
    let closeButton = NSButton(title: loc("missingFiles.close"), target: nil, action: nil)

    static let leastSize = NSSize(width: 520, height: 320)

    init(store: WorkspaceStore, title: String) {
        self.store = store
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 440),
            styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.minSize = Self.leastSize
        window.setAccessibilityIdentifier("missing-files-window")
        super.init(window: window)
        window.delegate = self
        retitle(title)
        build()
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func retitle(_ workspace: String) {
        window?.title = String(format: loc("missingFiles.title"), workspace)
    }

    // MARK: What is missing now

    func missingCues() -> [Cue] { Self.missingCues(in: store) }

    /// The cues the engine reports missing their file, in list order, whatever their kind.
    static func missingCues(in store: WorkspaceStore) -> [Cue] {
        var found: [Cue] = []
        func walk(_ cues: [Cue]) {
            for cue in cues {
                if (store.problems[cue.id] ?? []).contains(where: { $0.type == "media_missing" }) { found.append(cue) }
                walk(cue.children)
            }
        }
        for list in store.model?.cueLists ?? [] { walk(list.cues) }
        return found
    }

    static func media(of cue: Cue) -> String? { cue.raw["kind"]?["media"]?.stringValue }

    private func title(of cue: Cue) -> String {
        WarningRows.cueTitle(cue, name: store.model?.listName(cue) ?? CueKindStyle.kindLabel(cue.kindType))
    }

    // MARK: Search

    /// Asks for a folder and searches it.
    func chooseFolderAndSearch() async {
        guard !isBusy, !store.showMode, let folder = pickFolder() else { return }
        await search(folder)
    }

    func search(_ folder: URL) async {
        guard !isBusy, !store.showMode else { return }
        dismissChoice()
        isBusy = true
        rows = []
        toAsk = []
        folderLabel.stringValue = folder.path
        searched = folder.standardizedFileURL.path
        folderLabel.toolTip = folder.path
        summaryLabel.stringValue = loc("missingFiles.searching")
        refresh()
        let result = await store.searchMissingFiles(in: folder.path)
        isBusy = false
        switch result {
        case .success(let ok):
            guard case .relink(let report)? = ok.data else {
                summaryLabel.stringValue = String(format: loc("missingFiles.failed"), loc("error.detail.generic"))
                break
            }
            take(report)
        case .failure(let e):
            summaryLabel.stringValue = String(format: loc("missingFiles.failed"), e.detailText)
        }
        refresh()
        askNext()
    }

    private func take(_ report: RelinkResult) {
        let showPath = store.document?.path
        func full(_ file: String) -> String {
            CueRowMenu.mediaURL(file, showPath: showPath)?.standardizedFileURL.path ?? file
        }
        var found: [ModelID: [String]] = [:]
        for m in report.matches { found[m.cue] = [full(m.file)] }
        for a in report.ambiguous { found[a.cue] = a.candidates.map(full) }
        // List order, so the rows read as the cue list does.
        var order: [Cue] = []
        func walk(_ cues: [Cue]) {
            for cue in cues {
                if found[cue.id] != nil { order.append(cue) }
                walk(cue.children)
            }
        }
        for list in store.model?.cueLists ?? [] { walk(list.cues) }
        rows = order.map { cue in
            let files = found[cue.id] ?? []
            let only = files.count == 1 ? files.first : nil
            return Row(
                cue: cue.id, title: title(of: cue), was: Self.media(of: cue), candidates: files, chosen: only,
                ticked: only != nil)
        }
        toAsk = rows.filter { $0.chosen == nil }.map(\.cue)
        summaryLabel.stringValue = Self.searchSummary(
            found: rows.count, missing: rows.count + report.unmatched.count, truncated: report.truncated,
            capped: report.capped)
    }

    static func searchSummary(found: Int, missing: Int, truncated: Bool, capped: Bool) -> String {
        var parts = [String(format: loc("missingFiles.summary.found"), found, missing)]
        if missing > found { parts.append(String(format: loc("missingFiles.summary.notFound"), missing - found)) }
        if truncated { parts.append(loc("missingFiles.summary.truncated")) }
        if capped { parts.append(loc("missingFiles.summary.capped")) }
        return parts.joined(separator: " ")
    }

    /// `path` from the searched folder down (the folder is shown above the list); the full path
    /// when it lies elsewhere.
    func shown(_ path: String) -> String {
        guard let searched, path.hasPrefix(searched + "/") else { return path }
        return String(path.dropFirst(searched.count + 1))
    }

    // MARK: Several files of one name

    /// Asks about the next cue the search found more than one file for.
    private func askNext() {
        guard choiceSheet == nil, !store.showMode, let window else { return }
        while let cue = toAsk.first {
            toAsk.removeFirst()
            if let row = rows.firstIndex(where: { $0.cue == cue && $0.chosen == nil }) {
                ask(row: row, in: window)
                return
            }
        }
    }

    /// Opens the sheet that asks which of a row's files the cue should use.
    func ask(row: Int) {
        guard rows.indices.contains(row), let window, choiceSheet == nil, !isBusy else { return }
        toAsk.removeAll { $0 == rows[row].cue }
        ask(row: row, in: window)
    }

    private func ask(row: Int, in window: NSWindow) {
        let item = rows[row]
        let name = URL(fileURLWithPath: item.candidates.first ?? "").lastPathComponent
        let sheet = MissingFileChoiceSheet(
            question: String(format: loc("missingFiles.choice.question"), item.title, item.candidates.count, name),
            candidates: item.candidates, shown: item.candidates.map(shown), selected: item.chosen)
        sheet.window.appearance = window.appearance
        choiceSheet = sheet
        sheet.onDone = { [weak self] picked in
            guard let self else { return }
            choiceSheet = nil
            if let picked, let i = rows.firstIndex(where: { $0.cue == item.cue }) {
                rows[i].chosen = picked
                rows[i].ticked = true
            }
            refresh()
            askNext()
        }
        if let presentSheet { presentSheet(sheet.window) } else { window.beginSheet(sheet.window) }
    }

    private func dismissChoice() {
        toAsk = []
        choiceSheet?.cancel()
    }

    // MARK: Relink

    /// Points every ticked cue at its file, as one edit (one undo step). A cue whose file
    /// reference changed since the search keeps it.
    func relink() async {
        guard !isBusy, !store.showMode, let model = store.model else { return }
        let ticked = rows.filter { row in
            row.ticked && row.chosen != nil && model.cue(row.cue).map { Self.media(of: $0) == row.was } == true
        }
        let commands = ticked.compactMap { row in
            row.chosen.map { Command.setProperty(row.cue, .media(CueEditor.mediaPath(URL(fileURLWithPath: $0)))) }
        }
        guard let command = Command.transaction(commands) else { return }
        isBusy = true
        refresh()
        let result = await store.send(command: command)
        isBusy = false
        switch result {
        case .success:
            let done = Set(ticked.map(\.cue))
            rows.removeAll { done.contains($0.cue) }
            let left = missingCues().filter { !done.contains($0.id) }.count
            summaryLabel.stringValue = String(format: loc("missingFiles.summary.relinked"), done.count, left)
            _ = store.receive(.log(.info, summaryLabel.stringValue))
        case .failure(let e):
            summaryLabel.stringValue = String(format: loc("missingFiles.failed"), e.detailText)
        }
        refresh()
    }

    // MARK: Show Mode and closing

    /// Show Mode: the tool is put away, a question left unanswered.
    func showModeChanged(_ on: Bool) {
        guard on else { return }
        dismissChoice()
        close()
    }

    func windowWillClose(_ notification: Notification) { dismissChoice() }

    // MARK: Views

    private func build() {
        introLabel.setAX("missing-files-intro")
        introLabel.maximumNumberOfLines = 2
        folderLabel.setAX("missing-files-folder")
        folderLabel.lineBreakMode = .byTruncatingMiddle
        folderLabel.textColor = .secondaryLabelColor
        folderLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        chooseFolderButton.setAX("missing-files-choose-folder")
        chooseFolderButton.bezelStyle = .rounded
        chooseFolderButton.target = self
        chooseFolderButton.action = #selector(chooseFolderClicked)
        chooseFolderButton.setContentCompressionResistancePriority(.required, for: .horizontal)

        for (id, key, width, grows) in [
            ("use", "missingFiles.column.use", 36.0, false), ("cue", "missingFiles.column.cue", 170, false),
            ("found", "missingFiles.column.found", 300, true),
        ] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
            column.title = loc(key)
            column.width = width
            column.minWidth = id == "use" ? 36 : 80
            column.resizingMask = grows ? .autoresizingMask : .userResizingMask
            table.addTableColumn(column)
        }
        table.setAX("missing-files-table")
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        table.usesAlternatingRowBackgroundColors = true
        table.allowsEmptySelection = true
        table.rowHeight = 24
        table.dataSource = self
        table.delegate = self
        scroll.setAX("missing-files-scroll")
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder

        summaryLabel.setAX("missing-files-summary")
        // A line of its own above the buttons: the longest summary fits whole at the least width.
        summaryLabel.maximumNumberOfLines = 0
        summaryLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        summaryLabel.setContentCompressionResistancePriority(.required, for: .vertical)
        relinkButton.setAX("missing-files-relink")
        relinkButton.bezelStyle = .rounded
        relinkButton.keyEquivalent = "\r"
        relinkButton.target = self
        relinkButton.action = #selector(relinkClicked)
        closeButton.setAX("missing-files-close")
        closeButton.bezelStyle = .rounded
        closeButton.target = self
        closeButton.action = #selector(closeClicked)
        for b in [relinkButton, closeButton] {
            b.widthAnchor.constraint(greaterThanOrEqualToConstant: 90).isActive = true
            b.setContentCompressionResistancePriority(.required, for: .horizontal)
        }

        let content = NSView()
        let folderRow = NSStackView(views: [chooseFolderButton, folderLabel])
        folderRow.spacing = 8
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let buttons = NSStackView(views: [spacer, closeButton, relinkButton])
        buttons.spacing = 8
        buttons.alignment = .centerY
        for v in [introLabel, folderRow, scroll, summaryLabel, buttons] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        let inset: CGFloat = 16
        NSLayoutConstraint.activate([
            introLabel.topAnchor.constraint(equalTo: content.topAnchor, constant: 14),
            introLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: inset),
            introLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -inset),
            folderRow.topAnchor.constraint(equalTo: introLabel.bottomAnchor, constant: 10),
            folderRow.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: inset),
            folderRow.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -inset),
            scroll.topAnchor.constraint(equalTo: folderRow.bottomAnchor, constant: 10),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: inset),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -inset),
            summaryLabel.topAnchor.constraint(equalTo: scroll.bottomAnchor, constant: 10),
            summaryLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: inset),
            summaryLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -inset),
            buttons.topAnchor.constraint(equalTo: summaryLabel.bottomAnchor, constant: 10),
            buttons.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: inset),
            buttons.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -inset),
            buttons.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -14),
            buttons.heightAnchor.constraint(greaterThanOrEqualToConstant: 32),
        ])
        window?.contentView = content
        window?.initialFirstResponder = chooseFolderButton
    }

    private func refresh() {
        table.reloadData()
        chooseFolderButton.isEnabled = !isBusy
        relinkButton.isEnabled = !isBusy && rows.contains { $0.ticked && $0.chosen != nil }
    }

    @objc private func chooseFolderClicked() { Task { await chooseFolderAndSearch() } }
    @objc private func relinkClicked() { Task { await relink() } }
    @objc private func closeClicked() { close() }

    @objc private func tickClicked(_ sender: NSButton) {
        guard rows.indices.contains(sender.tag), rows[sender.tag].chosen != nil else { return }
        rows[sender.tag].ticked = sender.state == .on
        refresh()
    }

    @objc private func pickClicked(_ sender: NSButton) { ask(row: sender.tag) }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row), let id = tableColumn?.identifier.rawValue else { return nil }
        let item = rows[row]
        switch id {
        case "use":
            let cell = reuse(MissingFileTickCell.self, id) { MissingFileTickCell() }
            cell.box.target = self
            cell.box.action = #selector(tickClicked(_:))
            cell.box.tag = row
            cell.box.state = item.ticked ? .on : .off
            cell.box.isEnabled = item.chosen != nil && !isBusy
            cell.box.setAX("missing-files-use-\(row)", label: item.title)
            return cell
        case "cue":
            let cell = reuse(MissingFileTextCell.self, id) { MissingFileTextCell() }
            cell.label.stringValue = item.title
            cell.label.toolTip = item.title
            return cell
        default:
            let cell = reuse(MissingFileFoundCell.self, id) { MissingFileFoundCell() }
            cell.show(item, row: row, file: item.chosen.map(shown), enabled: !isBusy)
            cell.pickButton.target = self
            cell.pickButton.action = #selector(pickClicked(_:))
            return cell
        }
    }

    private func reuse<T: NSView>(_ type: T.Type, _ id: String, make: () -> T) -> T {
        let key = NSUserInterfaceItemIdentifier("missing-files-cell-\(id)")
        if let cell = table.makeView(withIdentifier: key, owner: self) as? T { return cell }
        let cell = make()
        cell.identifier = key
        return cell
    }
}

/// The tick box of a row.
final class MissingFileTickCell: NSTableCellView {
    let box = NSButton(checkboxWithTitle: "", target: nil, action: nil)

    init() {
        super.init(frame: .zero)
        box.translatesAutoresizingMaskIntoConstraints = false
        addSubview(box)
        NSLayoutConstraint.activate([
            box.centerXAnchor.constraint(equalTo: centerXAnchor),
            box.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}

/// One line of text, cut at its end.
final class MissingFileTextCell: NSTableCellView {
    let label = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        label.lineBreakMode = .byTruncatingTail
        label.translatesAutoresizingMaskIntoConstraints = false
        label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        addSubview(label)
        textField = label
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}

/// The file found for a row (a long path cut in the middle), or how many were found when the
/// operator has not picked one yet, with a button to pick (or pick again).
final class MissingFileFoundCell: NSTableCellView {
    let label = NSTextField(labelWithString: "")
    let pickButton = NSButton(title: "", target: nil, action: nil)

    init() {
        super.init(frame: .zero)
        label.lineBreakMode = .byTruncatingMiddle
        label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        pickButton.bezelStyle = .inline
        pickButton.controlSize = .small
        pickButton.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        pickButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        for v in [label, pickButton] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        textField = label
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            pickButton.leadingAnchor.constraint(greaterThanOrEqualTo: label.trailingAnchor, constant: 6),
            pickButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            pickButton.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(_ row: MissingFilesWindowController.Row, row index: Int, file: String?, enabled: Bool) {
        let several = row.candidates.count > 1
        if let file {
            label.stringValue = file
            label.textColor = .labelColor
        } else {
            label.stringValue = String(format: loc("missingFiles.found.several"), row.candidates.count)
            label.textColor = .secondaryLabelColor
        }
        label.toolTip = row.chosen
        pickButton.isHidden = !several
        pickButton.title = loc(row.chosen == nil ? "missingFiles.found.pick" : "missingFiles.found.change")
        pickButton.tag = index
        pickButton.isEnabled = enabled
        pickButton.setAX("missing-files-pick-\(index)")
    }
}

/// The sheet that asks which of several same-named files a cue should use. Use (Return) hands
/// back the selected path; Skip (Esc) hands back nil and the cue is left as it is.
@MainActor
final class MissingFileChoiceSheet: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    let window: NSWindow
    let questionLabel = NSTextField(wrappingLabelWithString: "")
    let table = NSTableView()
    let useButton = NSButton(title: loc("missingFiles.choice.use"), target: nil, action: nil)
    let skipButton = NSButton(title: loc("missingFiles.choice.skip"), target: nil, action: nil)
    let candidates: [String]
    /// How each candidate is listed (from the searched folder down).
    let shown: [String]
    /// Called once: the chosen path, or nil when skipped.
    var onDone: ((String?) -> Void)?

    init(question: String, candidates: [String], shown: [String], selected: String?) {
        self.candidates = candidates
        self.shown = shown
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        super.init()
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier("missing-file-choice-sheet")
        questionLabel.stringValue = question
        questionLabel.setAX("missing-file-choice-question")
        questionLabel.maximumNumberOfLines = 3
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("path"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.setAX("missing-file-choice-list")
        table.rowHeight = 22
        table.dataSource = self
        table.delegate = self
        table.allowsEmptySelection = false
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        table.reloadData()
        table.selectRowIndexes(
            IndexSet(integer: selected.flatMap { candidates.firstIndex(of: $0) } ?? 0), byExtendingSelection: false)
        let scroll = NSScrollView()
        scroll.setAX("missing-file-choice-scroll")
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        useButton.setAX("missing-file-choice-use")
        useButton.bezelStyle = .rounded
        useButton.keyEquivalent = "\r"
        useButton.target = self
        useButton.action = #selector(useClicked)
        skipButton.setAX("missing-file-choice-skip")
        skipButton.bezelStyle = .rounded
        skipButton.keyEquivalent = "\u{1b}"
        skipButton.target = self
        skipButton.action = #selector(skipClicked)
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let buttons = NSStackView(views: [skipButton, spacer, useButton])
        for b in [skipButton, useButton] { b.widthAnchor.constraint(greaterThanOrEqualToConstant: 90).isActive = true }
        let content = NSView()
        for v in [questionLabel, scroll, buttons] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        NSLayoutConstraint.activate([
            questionLabel.topAnchor.constraint(equalTo: content.topAnchor, constant: 16),
            questionLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            questionLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            scroll.topAnchor.constraint(equalTo: questionLabel.bottomAnchor, constant: 10),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.topAnchor.constraint(equalTo: scroll.bottomAnchor, constant: 14),
            buttons.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            buttons.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -16),
            content.widthAnchor.constraint(equalToConstant: 520),
            content.heightAnchor.constraint(equalToConstant: 300),
        ])
        window.contentView = content
        window.initialFirstResponder = table
    }

    /// The path selected in the list.
    var selected: String? {
        candidates.indices.contains(table.selectedRow) ? candidates[table.selectedRow] : nil
    }

    func numberOfRows(in tableView: NSTableView) -> Int { candidates.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let key = NSUserInterfaceItemIdentifier("missing-file-choice-cell")
        let cell = tableView.makeView(withIdentifier: key, owner: self) as? MissingFileTextCell ?? MissingFileTextCell()
        cell.identifier = key
        cell.label.lineBreakMode = .byTruncatingMiddle
        cell.label.stringValue = shown.indices.contains(row) ? shown[row] : ""
        cell.label.toolTip = candidates.indices.contains(row) ? candidates[row] : nil
        return cell
    }

    @objc private func useClicked() { finish(selected) }
    @objc private func skipClicked() { finish(nil) }

    /// Closes the sheet with nothing picked (Show Mode, a new search, the tool closed).
    func cancel() { finish(nil) }

    private func finish(_ picked: String?) {
        guard let done = onDone else { return }
        onDone = nil
        if let parent = window.sheetParent { parent.endSheet(window) } else { window.orderOut(nil) }
        done(picked)
    }
}
