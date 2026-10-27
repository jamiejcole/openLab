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

/// Collapsible right sidebar: transport strip, then QLab's two tabs. "Cue Lists" picks the list the
/// window shows (and GO addresses); "Active Cues" lists every cue that is running, paused, waiting or
/// fading, in the order they started, with pause/resume and panic per cue. It reads the store and
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

    /// QLab shows a cue here while it is playing or paused; waiting and stopping cues are live too.
    static func isActive(_ state: WireCueState?) -> Bool {
        switch state {
        case .running?, .paused?, .fading?, .preWaiting?, .postWaiting?, .stopping?: return true
        default: return false
        }
    }

    let store: WorkspaceStore
    let tabs = NSSegmentedControl(
        labels: [loc("sidebar.tab.cueLists"), loc("sidebar.tab.activeCues")],
        trackingMode: .selectOne, target: nil, action: nil)
    let newListButton = NSButton(title: loc("sidebar.newList"), target: nil, action: nil)
    let newCartButton = NSButton(title: loc("sidebar.newCart"), target: nil, action: nil)
    let listTable = NSTableView()
    let activeTable: NSTableView

    /// The user picked a list.
    var onSelectList: ((ModelID) -> Void)?
    /// A per-cue control was used (pause, resume, panic).
    var onCueAction: ((TransportAction) -> Void)?

    private(set) var transportButtons: [String: NSButton] = [:]
    private(set) var lists: [CueList] = []
    private(set) var shownList: ModelID?
    /// Active cues in the order they started; a snapshot starts them in list order.
    private(set) var activeCues: [ModelID] = []
    /// Rows redrawn in place by updates, oldest first (tests check that only the rows that
    /// changed were touched).
    private(set) var updatedActiveRows: [ModelID] = []
    private var settingSelection = false
    private var sub: AnyCancellable?
    private let listScroll = NSScrollView()
    private let activeScroll = NSScrollView()

    init(store: WorkspaceStore, activeTable: NSTableView = NSTableView()) {
        self.store = store
        self.activeTable = activeTable
        super.init(nibName: nil, bundle: nil)
        lists = store.model?.cueLists ?? []
        activeCues = Self.ordered(store: store)
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

        newListButton.setAX(AX.newListButton)
        newCartButton.setAX(AX.newCartButton)
        for b in [newListButton, newCartButton] {
            b.bezelStyle = .rounded
            b.isEnabled = false  // list management is not built yet
        }
        let bottom = NSStackView(views: [newListButton, newCartButton])
        bottom.distribution = .fillEqually

        let column = NSStackView(views: [strip, tabs, listScroll, activeScroll, bottom])
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
        for v in [strip, tabs, listScroll, activeScroll, bottom] {
            v.widthAnchor.constraint(equalTo: column.widthAnchor, constant: -20).isActive = true
        }
        view = root
        listTable.reloadData()
        activeTable.reloadData()
        applyShownSelection()
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

    @objc private func tabChanged() { showTab(tabs.selectedSegment) }

    func showTab(_ index: Int) {
        tabs.selectedSegment = index
        listScroll.isHidden = index != 0
        activeScroll.isHidden = index != 1
    }

    // MARK: Cue lists

    /// Tells the sidebar which list the window shows; the window calls this after the cue list
    /// switched (a click here, a deleted list, a new snapshot).
    func setShownList(_ id: ModelID?) {
        shownList = id
        applyShownSelection()
    }

    private func applyShownSelection() {
        guard isViewLoaded else { return }
        settingSelection = true
        defer { settingSelection = false }
        if let row = lists.firstIndex(where: { $0.id == shownList }) {
            listTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else {
            listTable.deselectAll(nil)
        }
    }

    private func reloadLists() {
        lists = store.model?.cueLists ?? []
        if isViewLoaded {
            listTable.reloadData()
            applyShownSelection()
        }
    }

    // MARK: Store changes

    private func handle(_ change: StoreChange) {
        switch change {
        case .reset:
            reloadLists()
            activeCues = Self.ordered(store: store)
            if isViewLoaded { activeTable.reloadData() }
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
        case .runtime(let cues, _): syncActive(changed: cues)
        case .progress(let ids): redraw(ids)
        }
    }

    /// Active cues that exist in the model, in list order (the order a snapshot starts them in).
    private static func ordered(store: WorkspaceStore) -> [ModelID] {
        var result: [ModelID] = []
        func walk(_ cues: [Cue]) {
            for c in cues {
                if isActive(store.cueStates[c.id]?.state) { result.append(c.id) }
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
                return !Self.isActive(store.cueStates[id]?.state) || store.model?.cue(id) == nil
            })
        if !removed.isEmpty {
            for i in removed.reversed() { activeCues.remove(at: i) }
            if isViewLoaded { activeTable.removeRows(at: removed, withAnimation: []) }
        }
        let known = Set(activeCues)
        let added = changed.filter {
            !known.contains($0) && Self.isActive(store.cueStates[$0]?.state) && store.model?.cue($0) != nil
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

    // MARK: Table data

    func numberOfRows(in tableView: NSTableView) -> Int {
        tableView === listTable ? lists.count : activeCues.count
    }

    func tableView(_ tableView: NSTableView, objectValueFor tableColumn: NSTableColumn?, row: Int) -> Any? {
        tableView === listTable && lists.indices.contains(row) ? lists[row].name : nil
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { tableView === listTable }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !settingSelection, (notification.object as? NSTableView) === listTable else { return }
        let row = listTable.selectedRow
        guard lists.indices.contains(row) else { return }
        shownList = lists[row].id
        onSelectList?(lists[row].id)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
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

    /// Row content from the store: elapsed and remaining while the engine reports progress, else the
    /// state (paused and waiting cues have no progress).
    func fill(_ cell: ActiveCueCell, _ id: ModelID) {
        let cue = store.model?.cue(id)
        let state = store.cueStates[id]?.state ?? .idle
        let name = cue.map { $0.name.isEmpty ? CueKindStyle.kindLabel($0.kindType) : $0.name } ?? ""
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
            cell.times.stringValue = Self.timesText(elapsed: p.elapsed, remaining: p.remaining)
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
        guard let remaining else { return CueTimeFormat.clock(elapsed) }
        return "\(CueTimeFormat.clock(elapsed))  -\(CueTimeFormat.clock(remaining))"
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
