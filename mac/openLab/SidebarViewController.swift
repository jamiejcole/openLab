// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// A button that remembers which cue its row shows (rows are reused and move, so no row index).
final class ActiveCueButton: NSButton {
    var cue: ModelID?
}

/// One Active Cues row, as QLab lays it out: pause/resume on the left, number and name with the
/// times below, panic on the right, a progress bar across the bottom, a colour stripe on the edge.
final class ActiveCueCell: NSTableCellView {
    let pauseButton = ActiveCueButton()
    let panicButton = ActiveCueButton()
    let title = NSTextField(labelWithString: "")
    let times = NSTextField(labelWithString: "")
    let bar = NSProgressIndicator()
    let stripe = NSView()

    init() {
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("active-cue")
        for b in [pauseButton, panicButton] {
            b.bezelStyle = .texturedRounded
            b.imageScaling = .scaleProportionallyDown
            b.setContentHuggingPriority(.required, for: .horizontal)
        }
        panicButton.image = symbolImage("exclamationmark.octagon", loc("activeCue.panic"))
        panicButton.toolTip = loc("activeCue.panic")
        panicButton.setAccessibilityLabel(loc("activeCue.panic"))
        panicButton.setAccessibilityIdentifier(AX.activeCuePanic)
        pauseButton.setAccessibilityIdentifier(AX.activeCuePause)
        title.lineBreakMode = .byTruncatingTail
        title.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        times.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        times.textColor = .secondaryLabelColor
        times.setAccessibilityIdentifier(AX.activeCueTimes)
        bar.style = .bar
        bar.isIndeterminate = false
        bar.minValue = 0
        bar.maxValue = 1
        bar.controlSize = .small
        bar.setAccessibilityElement(false)
        stripe.wantsLayer = true
        stripe.translatesAutoresizingMaskIntoConstraints = false

        let text = NSStackView(views: [title, times, bar])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        for v in [title, times, bar] { v.widthAnchor.constraint(equalTo: text.widthAnchor).isActive = true }
        let row = NSStackView(views: [pauseButton, text, panicButton])
        row.spacing = 6
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        addSubview(stripe)
        NSLayoutConstraint.activate([
            stripe.leadingAnchor.constraint(equalTo: leadingAnchor),
            stripe.topAnchor.constraint(equalTo: topAnchor),
            stripe.bottomAnchor.constraint(equalTo: bottomAnchor),
            stripe.widthAnchor.constraint(equalToConstant: 4),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}

/// One Warnings row: a marker symbol (so a warning is not told by colour alone), the title, the
/// reasons below it and, for missing media, a Relink button.
final class WarningCell: NSTableCellView {
    let title = NSTextField(labelWithString: "")
    let detail = NSTextField(wrappingLabelWithString: "")
    let relinkButton = NSButton(title: loc("warnings.relink"), target: nil, action: nil)

    init() {
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("warning")
        let marker = NSImageView(
            image: symbolImage("exclamationmark.triangle.fill", loc("warnings.marker")) ?? NSImage())
        marker.setContentHuggingPriority(.required, for: .horizontal)
        marker.contentTintColor = .systemOrange
        title.font = .systemFont(ofSize: 12, weight: .semibold)
        title.lineBreakMode = .byTruncatingTail
        title.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        detail.font = .systemFont(ofSize: 11)
        detail.textColor = .secondaryLabelColor
        detail.maximumNumberOfLines = 0
        relinkButton.bezelStyle = .rounded
        relinkButton.controlSize = .small
        relinkButton.setAccessibilityIdentifier(AX.warningRelink)
        let text = NSStackView(views: [title, detail, relinkButton])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 2
        let row = NSStackView(views: [marker, text])
        row.alignment = .top
        row.spacing = 6
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 6),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6),
            row.topAnchor.constraint(equalTo: topAnchor, constant: 4),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}

/// Collapsible right sidebar: transport strip, then QLab's tabs. "Cue Lists" picks the list the
/// window shows (and GO addresses); "Active Cues" lists every cue that is running, paused, waiting or
/// fading, in the order they started, with pause/resume and panic per cue; "Warnings" lists every
/// broken cue with why, and the engine's notices (DX-01). It reads the store and
/// reports intents through closures; it never talks to the engine.
@MainActor
final class SidebarViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    private static let transport: [(id: String, symbol: String, key: String)] = [
        (AX.transportReset, "stop.circle", "transport.reset"),
        (AX.transportPause, "pause.circle", "transport.pause"),
        (AX.transportResume, "play.circle", "transport.resume"),
        (AX.transportPanic, "exclamationmark.octagon", "transport.panic"),
    ]

    static var symbolNames: [String] { transport.map(\.symbol) + ["pause.fill", "play.fill"] }

    let store: WorkspaceStore
    let tabs = NSSegmentedControl(
        labels: [loc("sidebar.tab.cueLists"), loc("sidebar.tab.activeCues"), loc("sidebar.tab.warnings")],
        trackingMode: .selectOne, target: nil, action: nil)
    let newListButton = NSButton(title: loc("sidebar.newList"), target: nil, action: nil)
    let newCartButton = NSButton(title: loc("sidebar.newCart"), target: nil, action: nil)
    let listTable = NSTableView()
    let activeTable: NSTableView
    let warningsTable: NSTableView
    let warningsCount = NSTextField(labelWithString: "")

    /// The user picked a list.
    var onSelectList: ((ModelID) -> Void)?
    /// A per-cue control was used (pause, resume, panic).
    var onCueAction: ((TransportAction) -> Void)?
    /// A warning row about a cue was chosen: show and select that cue.
    var onSelectCue: ((ModelID) -> Void)?
    /// Relink was clicked on a row (the engine relinks by folder, so the row does not matter).
    var onRelink: (() -> Void)?
    /// Show Mode: relinking rewrites cue media, so the row buttons are off.
    var relinkLocked = false {
        didSet {
            guard relinkLocked != oldValue, isViewLoaded else { return }
            warningsTable.reloadData()
        }
    }

    private(set) var transportButtons: [String: NSButton] = [:]
    private(set) var lists: [CueList] = []
    /// The list the window shows; asked for whenever the selection is drawn, never cached.
    var shownProvider: () -> ModelID? = { nil }
    var shownList: ModelID? { shownProvider() }
    /// Active cues in the order they started; a snapshot starts them in list order.
    private(set) var activeCues: [ModelID] = []
    /// Rows redrawn in place by updates, oldest first (tests check that only the rows that
    /// changed were touched).
    private(set) var updatedActiveRows: [ModelID] = []
    /// The Warnings rows as shown.
    private(set) var warningRows: [WarningRow] = []
    /// Ids of rows redrawn in place (tests check that only changed rows were touched).
    private(set) var updatedWarningRows: [String] = []
    private var warnedCues: Set<ModelID> = []
    private var settingSelection = false
    /// The store is changing the rows: any selection change is the table's, not the user's.
    private var updatingWarnings = false
    /// The row whose selection change already revealed its cue (its click action must not again).
    private var revealedRow: Int?
    private var sub: AnyCancellable?
    private let listScroll = NSScrollView()
    private let activeScroll = NSScrollView()
    private let warningsScroll = NSScrollView()

    init(
        store: WorkspaceStore, activeTable: NSTableView = NSTableView(), warningsTable: NSTableView = NSTableView()
    ) {
        self.store = store
        self.activeTable = activeTable
        self.warningsTable = warningsTable
        super.init(nibName: nil, bundle: nil)
        lists = store.model?.cueLists ?? []
        activeCues = Self.ordered(store: store)
        setWarnings(WarningRows.make(store: store))
        sub = store.changes.sink { [weak self] change in
            MainActor.assumeIsolated { self?.handle(change) }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func loadView() {
        let root = NSView()
        root.setAX(AX.sidebar)

        let strip = NSStackView()
        strip.distribution = .fillEqually
        strip.spacing = 6
        for entry in Self.transport {
            let name = loc(entry.key)
            let button = NSButton(
                image: symbolImage(entry.symbol, name) ?? NSImage(), target: nil, action: nil)
            button.setAX(entry.id, label: name)
            button.toolTip = name
            button.bezelStyle = .texturedRounded
            button.imageScaling = .scaleProportionallyDown
            button.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 22, weight: .regular)
            button.heightAnchor.constraint(equalToConstant: 44).isActive = true
            if entry.id == AX.transportReset {
                // The engine has no reset-all command yet (#123); an enabled button that does
                // nothing is worse than a disabled one in an emergency.
                button.isEnabled = false
                button.toolTip = loc("transport.reset.unavailable")
            }
            transportButtons[entry.id] = button
            strip.addArrangedSubview(button)
        }

        tabs.selectedSegment = 0
        tabs.setAX(AX.sidebarTabs, label: loc("sidebar.tabs.label"))
        tabs.target = self
        tabs.action = #selector(tabChanged)

        configure(listTable, id: AX.sidebarList, label: loc("sidebar.list.label"), scroll: listScroll)
        configure(activeTable, id: AX.sidebarActive, label: loc("sidebar.active.label"), scroll: activeScroll)
        activeTable.rowHeight = 64
        activeTable.selectionHighlightStyle = .none
        activeScroll.isHidden = true
        configure(warningsTable, id: AX.sidebarWarnings, label: loc("sidebar.warnings.label"), scroll: warningsScroll)
        warningsTable.usesAutomaticRowHeights = false
        warningsTable.target = self
        warningsTable.action = #selector(warningClicked)
        warningsScroll.isHidden = true
        warningsCount.setAX(AX.warningsCount)
        warningsCount.textColor = .secondaryLabelColor
        warningsCount.isHidden = true
        updateWarningsCount()

        newListButton.setAX(AX.newListButton)
        newCartButton.setAX(AX.newCartButton)
        for b in [newListButton, newCartButton] { b.bezelStyle = .rounded }
        newListButton.target = self
        newListButton.action = #selector(newListClicked)
        newCartButton.isEnabled = false  // carts are not in v0.0.1 (CL-05)
        let bottom = NSStackView(views: [newListButton, newCartButton])
        bottom.distribution = .fillEqually

        let column = NSStackView(views: [strip, tabs, warningsCount, listScroll, activeScroll, warningsScroll, bottom])
        column.orientation = .vertical
        column.spacing = 8
        column.edgeInsets = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        column.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(column)
        NSLayoutConstraint.activate([
            column.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            column.topAnchor.constraint(equalTo: root.topAnchor),
            column.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            root.widthAnchor.constraint(greaterThanOrEqualToConstant: 220),
        ])
        for v in [strip, tabs, warningsCount, listScroll, activeScroll, warningsScroll, bottom] {
            v.widthAnchor.constraint(equalTo: column.widthAnchor, constant: -20).isActive = true
        }
        view = root
        listTable.reloadData()
        activeTable.reloadData()
        warningsTable.reloadData()
        syncSelection()
    }

    private func configure(_ table: NSTableView, id: String, label: String, scroll: NSScrollView) {
        table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("sidebar")))
        table.headerView = nil
        table.dataSource = self
        table.delegate = self
        table.setAX(id, label: label)
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
    }

    /// Adds an empty list after the last one and shows it. The id is the client's; the engine
    /// may refuse; then nothing changes and no list is selected.
    @objc private func newListClicked() {
        let id = ModelID(UUID().uuidString.lowercased())
        let index = lists.count
        let name = String(format: loc("sidebar.newList.name"), index + 1)
        Task { [weak self] in
            guard let self else { return }
            let sent = await store.send(command: .addCueList(id: id, name: name, index: index))
            if case .success = sent { onSelectList?(id) }
        }
    }

    @objc private func tabChanged() { showTab(tabs.selectedSegment) }

    func showTab(_ index: Int) {
        tabs.selectedSegment = index
        listScroll.isHidden = index != 0
        activeScroll.isHidden = index != 1
        warningsScroll.isHidden = index != 2
        warningsCount.isHidden = index != 2
    }

    // MARK: Cue lists

    /// Redraws the selection from `shownProvider`; the window calls this after the cue list has
    /// switched (a click here, a deleted list, a new snapshot).
    func syncSelection() {
        guard isViewLoaded else { return }
        settingSelection = true
        defer { settingSelection = false }
        // The cue list falls back to the first list when the shown one is gone; so does the highlight.
        if let row = lists.firstIndex(where: { $0.id == shownList }) ?? (lists.isEmpty ? nil : 0) {
            listTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else {
            listTable.deselectAll(nil)
        }
    }

    private func reloadLists() {
        lists = store.model?.cueLists ?? []
        if isViewLoaded {
            listTable.reloadData()
            syncSelection()
        }
    }

    // MARK: Store changes

    private func handle(_ change: StoreChange) {
        switch change {
        case .reset:
            reloadLists()
            activeCues = Self.ordered(store: store)
            if isViewLoaded { activeTable.reloadData() }
            setWarnings(WarningRows.make(store: store))
            if isViewLoaded { reloadWarningTable() }
        case .model(let upserts, let deletes):
            var listsChanged = false
            var touched = Set<ModelID>()
            for u in upserts {
                switch u {
                case .cueList: listsChanged = true
                case .cue(_, _, _, let cue): touched.insert(cue.id)
                default: break
                }
            }
            for d in deletes {
                switch d {
                case .cueList: listsChanged = true
                case .cue(let id): touched.insert(id)
                default: break
                }
            }
            if listsChanged { reloadLists() }
            // A deleted cue leaves the panel; a renamed one is redrawn. A removed list takes its
            // cues with it without a delete for each.
            if listsChanged || !touched.isDisjoint(with: activeCues) { syncActive(changed: touched) }
            // Only an edit to a warned cue (or to what its text names) can change a row.
            let other = upserts.contains { if case .cue = $0 { return false } else { return true } }
            if other || !touched.isDisjoint(with: warnedCues) { syncWarnings() }
        case .runtime(let cues, _):
            syncActive(changed: cues)
            // Only a cue that has problems, has a runtime reason or already has a row can change
            // the list, so a busy show does not rescan the cue tree for every change.
            if cues.contains(where: {
                warnedCues.contains($0) || store.problems[$0] != nil || store.cueStates[$0]?.reason != nil
            }) {
                syncWarnings()
            }
        case .progress(let ids): redraw(ids)
        case .waits: break
        case .notices: syncWarnings()
        }
    }

    /// Active cues that exist in the model, in list order (the order a snapshot starts them in).
    private static func ordered(store: WorkspaceStore) -> [ModelID] {
        var result: [ModelID] = []
        func walk(_ cues: [Cue]) {
            for c in cues {
                if store.cueStates[c.id]?.state.isActive == true { result.append(c.id) }
                walk(c.children)
            }
        }
        for l in store.model?.cueLists ?? [] { walk(l.cues) }
        return result
    }

    /// Rows for cues that stopped go, rows for cues that started are appended, and rows whose
    /// cue changed in some other way are redrawn in place. No full reload.
    private func syncActive(changed: Set<ModelID>) {
        let removed = IndexSet(
            activeCues.indices.filter { i in
                let id = activeCues[i]
                return store.cueStates[id]?.state.isActive != true || store.model?.cue(id) == nil
            })
        if !removed.isEmpty {
            for i in removed.reversed() { activeCues.remove(at: i) }
            if isViewLoaded { activeTable.removeRows(at: removed, withAnimation: []) }
        }
        let known = Set(activeCues)
        let added = changed.filter {
            !known.contains($0) && store.cueStates[$0]?.state.isActive == true && store.model?.cue($0) != nil
        }
        if !added.isEmpty {
            // Cues that start together go in list order, so the order is the same every time.
            let order = Self.ordered(store: store).filter(added.contains)
            let first = activeCues.count
            activeCues += order
            if isViewLoaded {
                activeTable.insertRows(at: IndexSet(first..<activeCues.count), withAnimation: [])
            }
        }
        redraw(changed.intersection(known))
    }

    private func redraw(_ ids: Set<ModelID>) {
        guard isViewLoaded else { return }
        for (row, id) in activeCues.enumerated() where ids.contains(id) {
            updatedActiveRows.append(id)
            if let cell = activeTable.view(atColumn: 0, row: row, makeIfNecessary: false) as? ActiveCueCell {
                fill(cell, id)
            }
        }
    }

    func clearUpdateLog() { updatedActiveRows = [] }

    // MARK: Warnings

    private func setWarnings(_ rows: [WarningRow]) {
        warningRows = rows
        warnedCues = Set(rows.compactMap(\.cue))
        updateWarningsCount()
    }

    private func updateWarningsCount() {
        let n = warningRows.count
        warningsCount.stringValue =
            n == 0 ? loc("warnings.none") : String(format: loc(n == 1 ? "warnings.one" : "warnings.many"), n)
    }

    /// Brings the rows up to date with the store: rows that went are removed, new ones inserted
    /// where they belong and a row whose text changed is redrawn in place. No full reload unless
    /// the order of the rows that stay changed.
    func syncWarnings() {
        let new = WarningRows.make(store: store)
        guard new != warningRows else { return }
        let oldIDs = Set(warningRows.map(\.id))
        let newIDs = Set(new.map(\.id))
        let kept = warningRows.filter { newIDs.contains($0.id) }
        guard kept.map(\.id) == new.filter({ oldIDs.contains($0.id) }).map(\.id) else {
            setWarnings(new)
            if isViewLoaded { reloadWarningTable() }
            return
        }
        updatingWarnings = true
        defer { updatingWarnings = false }
        let removed = IndexSet(warningRows.indices.filter { !newIDs.contains(warningRows[$0].id) })
        warningRows = kept
        if isViewLoaded, !removed.isEmpty { warningsTable.removeRows(at: removed, withAnimation: []) }
        for (i, row) in new.enumerated() where !oldIDs.contains(row.id) {
            warningRows.insert(row, at: i)
            if isViewLoaded { warningsTable.insertRows(at: IndexSet(integer: i), withAnimation: []) }
        }
        for i in new.indices where warningRows[i] != new[i] {
            warningRows[i] = new[i]
            updatedWarningRows.append(new[i].id)
            if isViewLoaded {
                warningsTable.reloadData(forRowIndexes: IndexSet(integer: i), columnIndexes: IndexSet(integer: 0))
                warningsTable.noteHeightOfRows(withIndexesChanged: IndexSet(integer: i))
            }
        }
        setWarnings(warningRows)
    }

    private func reloadWarningTable() {
        updatingWarnings = true
        defer { updatingWarnings = false }
        warningsTable.reloadData()
    }

    func clearWarningUpdateLog() { updatedWarningRows = [] }

    private func chooseWarning(row: Int) {
        guard warningRows.indices.contains(row), let cue = warningRows[row].cue else { return }
        onSelectCue?(cue)
    }

    /// A click on the row that is already selected (no selection change to follow).
    @objc private func warningClicked() {
        let row = warningsTable.clickedRow
        if revealedRow == row {
            revealedRow = nil
            return
        }
        chooseWarning(row: row)
    }

    @objc private func relinkClicked() { onRelink?() }

    // MARK: Table data

    func numberOfRows(in tableView: NSTableView) -> Int {
        if tableView === listTable { return lists.count }
        return tableView === warningsTable ? warningRows.count : activeCues.count
    }

    func tableView(_ tableView: NSTableView, objectValueFor tableColumn: NSTableColumn?, row: Int) -> Any? {
        tableView === listTable && lists.indices.contains(row) ? lists[row].name : nil
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        if tableView === warningsTable { return warningRows.indices.contains(row) && warningRows[row].cue != nil }
        return tableView === listTable
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        if tableView === activeTable { return 64 }
        guard tableView === warningsTable, warningRows.indices.contains(row) else { return tableView.rowHeight }
        let w = warningRows[row]
        let lines = w.detail.isEmpty ? 0 : w.detail.split(separator: "\n").count
        return 26 + CGFloat(lines) * 15 + (w.canRelink ? 22 : 0)
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        if (notification.object as? NSTableView) === warningsTable {
            guard !updatingWarnings else { return }
            revealedRow = warningsTable.selectedRow
            chooseWarning(row: revealedRow ?? -1)
            return
        }
        guard !settingSelection, (notification.object as? NSTableView) === listTable else { return }
        let row = listTable.selectedRow
        guard lists.indices.contains(row) else { return }
        onSelectList?(lists[row].id)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        if tableView === warningsTable { return warningCell(row) }
        guard tableView === activeTable, activeCues.indices.contains(row) else { return nil }
        let cell =
            (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("active-cue"), owner: nil)
                as? ActiveCueCell) ?? ActiveCueCell()
        cell.pauseButton.target = self
        cell.pauseButton.action = #selector(pauseResumeClicked(_:))
        cell.panicButton.target = self
        cell.panicButton.action = #selector(panicClicked(_:))
        fill(cell, activeCues[row])
        return cell
    }

    private func warningCell(_ row: Int) -> NSView? {
        guard warningRows.indices.contains(row) else { return nil }
        let cell =
            (warningsTable.makeView(withIdentifier: NSUserInterfaceItemIdentifier("warning"), owner: nil)
                as? WarningCell) ?? WarningCell()
        let w = warningRows[row]
        cell.title.stringValue = w.title
        cell.detail.stringValue = w.detail
        cell.detail.isHidden = w.detail.isEmpty
        cell.relinkButton.isHidden = !w.canRelink
        cell.relinkButton.isEnabled = !relinkLocked
        cell.relinkButton.target = self
        cell.relinkButton.action = #selector(relinkClicked)
        cell.setAccessibilityLabel([w.title, w.detail].filter { !$0.isEmpty }.joined(separator: ". "))
        return cell
    }

    /// Row content from the store: elapsed and remaining while the engine reports progress, else the
    /// state (waiting cues have no progress). A paused or stopping cue keeps its frozen times but
    /// leads with the state name, so a held cue never reads like a running one (QLab marks it paused).
    func fill(_ cell: ActiveCueCell, _ id: ModelID) {
        let cue = store.model?.cue(id)
        let state = store.cueStates[id]?.state ?? .idle
        let name = cue?.listName ?? ""
        cell.title.stringValue = [cue?.number, name].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ")
        let paused = state == .paused
        cell.pauseButton.cue = id
        cell.panicButton.cue = id
        let pauseName = loc(paused ? "activeCue.resume" : "activeCue.pause")
        cell.pauseButton.image = symbolImage(paused ? "play.fill" : "pause.fill", pauseName)
        cell.pauseButton.toolTip = pauseName
        cell.pauseButton.setAccessibilityLabel(pauseName)
        cell.pauseButton.isEnabled = state != .stopping
        if let p = store.progress[id] {
            let times = Self.timesText(elapsed: p.elapsed, remaining: p.remaining)
            cell.times.stringValue =
                state == .paused || state == .stopping ? "\(CueStateStyle.style(for: state).label)  \(times)" : times
            let total = p.elapsed + (p.remaining ?? 0)
            cell.bar.doubleValue = p.remaining != nil && total > 0 ? min(1, p.elapsed / total) : 0
        } else {
            cell.times.stringValue = CueStateStyle.style(for: state).label
            cell.bar.doubleValue = 0
        }
        cell.stripe.layer?.backgroundColor =
            cue?.raw["color"]?.stringValue.flatMap { CueKindStyle.colours[$0]?.tint }?.cgColor
    }

    /// `elapsed` and, when the end is known, `-remaining`.
    static func timesText(elapsed: Double, remaining: Double?) -> String {
        guard let remaining else { return CueTime.running(elapsed) }
        return "\(CueTime.running(elapsed))  -\(CueTime.running(remaining))"
    }

    @objc private func pauseResumeClicked(_ sender: ActiveCueButton) {
        guard let cue = sender.cue else { return }
        onCueAction?(store.cueStates[cue]?.state == .paused ? .resume(cue: cue) : .pause(cue: cue))
    }

    @objc private func panicClicked(_ sender: ActiveCueButton) {
        guard let cue = sender.cue else { return }
        onCueAction?(.panicCue(cue: cue))
    }
}
