// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// A button that remembers which cue its row shows (rows are reused and move, so no row index).
final class ActiveCueButton: NSButton {
    var cue: ModelID?
    /// The cue the button showed when it was pressed: the row's cell may be reused for another cue
    /// while the button is held, and letting go acts on the cue that was pressed.
    private var pressedCue: ModelID?
    /// The cue an action of this button is for.
    var actionCue: ModelID? { pressedCue ?? cue }

    override func mouseDown(with event: NSEvent) {
        pressedCue = cue
        defer { pressedCue = nil }
        super.mouseDown(with: event)
    }
}

/// The bar across the foot of an Active Cues row: filled from the left to `doubleValue` (0 to 1). It draws
/// the value it is given at once; a system progress indicator eases toward it, so it lags a cue whose pass
/// starts again.
final class ActiveCueBar: NSView {
    var doubleValue: Double = 0 {
        didSet { if doubleValue != oldValue { needsDisplay = true } }
    }

    override var intrinsicContentSize: NSSize { NSSize(width: NSView.noIntrinsicMetric, height: 6) }

    override func draw(_ dirtyRect: NSRect) {
        let track = NSBezierPath(roundedRect: bounds, xRadius: 3, yRadius: 3)
        NSColor.quaternaryLabelColor.setFill()
        track.fill()
        let width = bounds.width * CGFloat(min(1, max(0, doubleValue)))
        guard width >= 1 else { return }
        track.addClip()
        NSColor.controlAccentColor.setFill()
        NSBezierPath(rect: NSRect(x: bounds.minX, y: bounds.minY, width: width, height: bounds.height)).fill()
    }
}

/// One Active Cues row, as QLab lays it out: pause/resume on the left, number and name with the
/// times below, panic on the right, a progress bar across the bottom, a colour stripe on the edge.
final class ActiveCueCell: NSTableCellView {
    let pauseButton = ActiveCueButton()
    let panicButton = ActiveCueButton()
    let title = NSTextField(labelWithString: "")
    let times = NSTextField(labelWithString: "")
    let bar = ActiveCueBar()
    let stripe = NSView()

    init() {
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("active-cue")
        for b in [pauseButton, panicButton] {
            b.bezelStyle = .texturedRounded
            b.imageScaling = .scaleProportionallyDown
            b.setContentHuggingPriority(.required, for: .horizontal)
        }
        panicButton.image = Icon.activeCuePanic.image(loc("activeCue.panic"))
        panicButton.toolTip = loc("activeCue.panic")
        panicButton.setAccessibilityLabel(loc("activeCue.panic"))
        panicButton.setAccessibilityIdentifier(AX.activeCuePanic)
        pauseButton.setAccessibilityIdentifier(AX.activeCuePause)
        title.lineBreakMode = .byTruncatingTail
        title.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        times.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        times.textColor = .secondaryLabelColor
        times.setAccessibilityIdentifier(AX.activeCueTimes)
        bar.setAccessibilityElement(false)
        stripe.wantsLayer = true
        stripe.translatesAutoresizingMaskIntoConstraints = false

        let text = NSStackView(views: [title, times, bar])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 1
        for v in [title, times, bar] { v.widthAnchor.constraint(equalTo: text.widthAnchor).isActive = true }
        let row = NSStackView(views: [pauseButton, text, panicButton])
        // The name and times take the row's width between the two buttons.
        text.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        row.spacing = 6
        // Filled: the name and times column takes all the room between the buttons. With the default
        // gravity areas it was only as wide as its widest text, so a lone time cut the name short and
        // the panic button sat beside the text instead of at the row's edge.
        row.distribution = .fill
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

/// One Cue Lists row: a list's name with a triangle that opens it, or under an open list one of its
/// cues (number, then name), set in by its depth. Text is cut short when the sidebar is narrow.
final class CueListNameCell: NSTableCellView {
    let name = NSTextField(labelWithString: "")
    let disclosure = NSButton()
    private var leading: NSLayoutConstraint?
    /// How far a cue row is set in: past the triangle, then by its depth in groups.
    var indent: CGFloat = 0 { didSet { leading?.constant = 4 + indent } }

    init() {
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("cue-list-name")
        name.lineBreakMode = .byTruncatingTail
        name.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        name.translatesAutoresizingMaskIntoConstraints = false
        disclosure.bezelStyle = .disclosure
        disclosure.setButtonType(.pushOnPushOff)
        disclosure.title = ""
        disclosure.translatesAutoresizingMaskIntoConstraints = false
        disclosure.setAccessibilityIdentifier("cue-list-disclosure")
        addSubview(disclosure)
        addSubview(name)
        textField = name
        let lead = disclosure.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4)
        leading = lead
        NSLayoutConstraint.activate([
            lead,
            disclosure.centerYAnchor.constraint(equalTo: centerYAnchor),
            disclosure.widthAnchor.constraint(equalToConstant: 14),
            name.leadingAnchor.constraint(equalTo: disclosure.trailingAnchor, constant: 4),
            name.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            name.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}

/// A transport button that can carry a colour behind its picture (Pause yellow while a cue plays,
/// Resume green while one is held). Coloured, it draws its own plate and no bezel, so the colour is
/// the button's whole face; uncoloured, it is the plain bezel button.
final class TransportButton: NSButton {
    /// The plate's colour; nil: none. Drawn only while the button is enabled.
    var plate: NSColor? {
        didSet {
            guard plate != oldValue else { return }
            isBordered = plate == nil || !isEnabled
            needsDisplay = true
        }
    }

    override var isEnabled: Bool {
        didSet {
            guard isEnabled != oldValue else { return }
            isBordered = plate == nil || !isEnabled
            needsDisplay = true
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        if let plate, isEnabled {
            plate.setFill()
            NSBezierPath(roundedRect: bounds.insetBy(dx: 1, dy: 1), xRadius: 6, yRadius: 6).fill()
        }
        super.draw(dirtyRect)
    }
}

/// Collapsible right sidebar: transport strip, then QLab's tabs. "Cue Lists" picks the list the
/// window shows (and GO addresses); "Active Cues" lists every cue that is running, paused, waiting or
/// fading, in the order they started, with pause/resume and panic per cue. It reads the store and
/// reports intents through closures; it never talks to the engine.
@MainActor
final class SidebarViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate {
    private static let transport: [(id: String, icon: Icon, key: String)] = [
        (AX.transportReset, .transportReset, "transport.reset"),
        (AX.transportPause, .transportPause, "transport.pause"),
        (AX.transportResume, .transportResume, "transport.resume"),
        (AX.transportPanic, .transportPanic, "transport.panic"),
    ]

    /// The transport icons' side, the size Panic All's own picture is drawn at: well inside the 44 pt button.
    static let transportIconSize: CGFloat = 20

    /// The least the sidebar may be dragged to: the transport icons keep a margin in their buttons (they
    /// need about 230 pt here, and a newer macOS draws its bezels wider); the tabs' labels are cut short.
    static let minWidth: CGFloat = 250

    static var icons: [Icon] { transport.map(\.icon) + [.activeCuePause, .activeCueResume, .activeCuePanic] }

    let store: WorkspaceStore
    let tabs = NSSegmentedControl(
        labels: [loc("sidebar.tab.cueLists"), loc("sidebar.tab.activeCues")],
        trackingMode: .selectOne, target: nil, action: nil)
    let newListButton = NSButton(title: loc("sidebar.newList"), target: nil, action: nil)
    let listTable = NSTableView()
    let activeTable: NSTableView

    /// The user picked a list.
    var onSelectList: ((ModelID) -> Void)?
    /// The user chose a list row (or made a new list): the list itself, not a cue in it.
    var onChooseList: ((ModelID) -> Void)?
    /// A per-cue control was used (pause, resume, panic).
    var onCueAction: ((TransportAction) -> Void)?
    /// A cue of an open list was chosen: show and select that cue.
    var onSelectCue: ((ModelID) -> Void)?
    /// Show Mode: adding, renaming and deleting lists change the workspace, so New List and the
    /// list menu are off. A stray New List mid-show would also switch the window to an empty list
    /// where GO fires nothing. Choosing an existing list stays on.
    var listEditLocked = false {
        didSet { newListButton.isEnabled = !listEditLocked }
    }
    /// Why a list edit was refused (the window shows it as a sheet).
    var onNotice: ((String) -> Void)?

    private(set) var transportButtons: [String: NSButton] = [:]
    private(set) var lists: [CueList] = [] {
        didSet {
            rebuildListRows()
            updateTabTitles()
        }
    }

    /// The first two tabs say how many lists and active cues there are, as QLab's do.
    /// Pause is on, yellow, while a cue plays (or waits, or fades); Resume is on, green, while one is
    /// held; with nothing to pause or resume both are greyed, as QLab's are. Pause All and Resume All
    /// act on the whole workspace, so the strip reads every cue of every list.
    func updateTransport() {
        guard isViewLoaded else { return }
        var playing = false
        var held = false
        for state in store.cueStates.values.map(\.state) {
            switch state {
            case .running, .fading, .preWaiting, .postWaiting: playing = true
            case .paused: held = true
            default: break
            }
        }
        if let pause = transportButtons[AX.transportPause] as? TransportButton {
            pause.isEnabled = playing
            pause.plate = playing ? MarkColour.caution : nil
        }
        if let resume = transportButtons[AX.transportResume] as? TransportButton {
            resume.isEnabled = held
            resume.plate = held ? NSColor.systemGreen : nil
        }
    }

    func updateTabTitles() {
        func title(_ key: String, _ n: Int) -> String {
            n == 1 ? loc("\(key).one") : String(format: loc("\(key).count"), n)
        }
        tabs.setLabel(title("sidebar.tab.cueLists", lists.count), forSegment: 0)
        tabs.setLabel(title("sidebar.tab.activeCues", activeCues.count), forSegment: 1)
    }
    /// The list the window shows; asked for whenever the selection is drawn, never cached.
    var shownProvider: () -> ModelID? = { nil }
    var shownList: ModelID? { shownProvider() }
    /// Active cues in the order they started; a snapshot starts them in list order.
    private(set) var activeCues: [ModelID] = [] {
        didSet {
            updateTabTitles()
            updateActiveEmptyNote()
        }
    }
    /// Shown over the Active Cues tab while nothing plays.
    let activeEmptyNote = NSTextField(labelWithString: loc("sidebar.active.empty"))

    private func updateActiveEmptyNote() {
        guard isViewLoaded else { return }
        activeEmptyNote.isHidden = activeScroll.isHidden || !activeCues.isEmpty
    }
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
        rebuildListRows()  // no observer runs for an assignment in init
        activeCues = Self.ordered(store: store)
        updateTabTitles()
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
            let image = entry.icon.image(name)
            // A vector drawing, so it stays sharp at the strip's size.
            image.size = NSSize(width: Self.transportIconSize, height: Self.transportIconSize)
            let button = TransportButton(image: image, target: nil, action: nil)
            button.setAX(entry.id, label: name)
            button.toolTip = name
            // A square bezel is drawn at the button's full height (a textured-rounded one stays
            // about 24 pt tall), so the strip is made of large buttons the glyph fills.
            button.bezelStyle = .regularSquare
            button.imageScaling = .scaleNone
            button.heightAnchor.constraint(equalToConstant: 44).isActive = true
            transportButtons[entry.id] = button
            strip.addArrangedSubview(button)
        }

        tabs.selectedSegment = 0
        tabs.setAX(AX.sidebarTabs, label: loc("sidebar.tabs.label"))
        tabs.target = self
        tabs.action = #selector(tabChanged)
        // The tabs fit the sidebar, not the other way round: their labels ask for more than the column at
        // 280 pt with two-digit counts, or on a macOS whose segmented controls measure wider, and resisting
        // at the default would widen the sidebar past its 280 pt and squeeze the cue list and inspector.
        // Below the sidebar's preferred width and the splits' holding priorities, they give up their spare
        // padding first and truncate only after that.
        tabs.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(240), for: .horizontal)

        configure(listTable, id: AX.sidebarList, label: loc("sidebar.list.label"), scroll: listScroll)
        configure(activeTable, id: AX.sidebarActive, label: loc("sidebar.active.label"), scroll: activeScroll)
        activeTable.rowHeight = 64
        activeTable.selectionHighlightStyle = .none
        activeScroll.isHidden = true

        newListButton.setAX(AX.newListButton)
        newListButton.bezelStyle = .rounded
        newListButton.target = self
        newListButton.action = #selector(newListClicked)
        // Carts are not built, so there is no New Cart button.
        let bottom = NSStackView(views: [newListButton])
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
            root.widthAnchor.constraint(greaterThanOrEqualToConstant: Self.minWidth),
        ])
        for v in [strip, tabs, listScroll, activeScroll, bottom] {
            v.widthAnchor.constraint(equalTo: column.widthAnchor, constant: -20).isActive = true
        }
        activeEmptyNote.setAX("sidebar-active-empty")
        activeEmptyNote.textColor = .secondaryLabelColor
        activeEmptyNote.alignment = .center
        activeEmptyNote.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(activeEmptyNote)
        NSLayoutConstraint.activate([
            activeEmptyNote.centerXAnchor.constraint(equalTo: activeScroll.centerXAnchor),
            activeEmptyNote.centerYAnchor.constraint(equalTo: activeScroll.centerYAnchor),
        ])
        view = root
        updateTransport()
        updateActiveEmptyNote()
        listTable.reloadData()
        activeTable.reloadData()
        syncSelection()
    }

    /// A click on a list row (the table's action, after any selection change): that list is chosen.
    @objc func listClicked() { chooseList(atRow: listTable.clickedRow) }

    func chooseList(atRow row: Int) {
        guard listRows.indices.contains(row), case .list(let i) = listRows[row], lists.indices.contains(i) else {
            return
        }
        onChooseList?(lists[i].id)
    }

    private func configure(_ table: NSTableView, id: String, label: String, scroll: NSScrollView) {
        table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("sidebar")))
        table.headerView = nil
        table.dataSource = self
        table.delegate = self
        if table === listTable {
            let menu = NSMenu()
            menu.delegate = self
            table.menu = menu
            // A click on the list already highlighted changes no selection: it still chooses the list.
            table.target = self
            table.action = #selector(listClicked)
        }
        table.setAX(id, label: label)
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
    }

    /// Adds an empty list after the last one and shows it. The id is the client's; the engine
    /// may refuse; then nothing changes and no list is selected.
    @objc private func newListClicked() { addList() }

    /// Adds an empty list after the last and shows it once the engine accepts; nothing in Show Mode.
    func addList() {
        guard !listEditLocked else { return }
        let id = ModelID(UUID().uuidString.lowercased())
        let index = lists.count
        let name = String(format: loc("sidebar.newList.name"), index + 1)
        Task { [weak self] in
            guard let self else { return }
            let sent = await store.send(command: .addCueList(id: id, name: name, index: index))
            if case .success = sent {
                onSelectList?(id)
                onChooseList?(id)
            }
        }
    }

    // MARK: List menu (right-click a list row)

    /// The list row the menu opened on; nil when the click missed every row.
    private var menuList: CueList? {
        guard listRows.indices.contains(listTable.clickedRow), case .list(let i) = listRows[listTable.clickedRow]
        else { return nil }
        return lists.indices.contains(i) ? lists[i] : nil
    }

    // MARK: Rows of the Cue Lists tab

    /// A row of the Cue Lists tab: a list, or under an open list one of its cues.
    enum ListRow: Equatable {
        case list(Int)
        case cue(list: Int, id: ModelID, depth: Int)
    }
    private(set) var listRows: [ListRow] = []
    /// The lists opened to show their cues. Closed until the operator opens one.
    private(set) var openLists: Set<ModelID> = []

    private func rebuildListRows() {
        var rows: [ListRow] = []
        for (i, list) in lists.enumerated() {
            rows.append(.list(i))
            guard openLists.contains(list.id) else { continue }
            // Iterative: a deep show must not overflow the stack.
            var stack: [(cues: [Cue], next: Int, depth: Int)] = [(list.cues, 0, 1)]
            while var top = stack.popLast() {
                guard top.next < top.cues.count else { continue }
                let cue = top.cues[top.next]
                top.next += 1
                stack.append(top)
                rows.append(.cue(list: i, id: cue.id, depth: top.depth))
                if !cue.children.isEmpty { stack.append((cue.children, 0, top.depth + 1)) }
            }
        }
        listRows = rows
    }

    /// Opens or closes a list in the Cue Lists tab (its triangle).
    func setOpen(_ open: Bool, list id: ModelID) {
        guard open != openLists.contains(id) else { return }
        if open { openLists.insert(id) } else { openLists.remove(id) }
        lists = store.model?.cueLists ?? []
        rebuildListRows()
        guard isViewLoaded else { return }
        listTable.reloadData()
        syncSelection()
    }

    @objc private func disclosureClicked(_ sender: NSButton) {
        let row = listTable.row(for: sender)
        guard listRows.indices.contains(row), case .list(let i) = listRows[row], lists.indices.contains(i) else {
            return
        }
        setOpen(sender.state == .on, list: lists[i].id)
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard menuList != nil else { return }
        let rename = menu.addItem(
            withTitle: loc("sidebar.list.rename"), action: #selector(renameListClicked), keyEquivalent: "")
        let delete = menu.addItem(
            withTitle: loc("sidebar.list.delete"), action: #selector(deleteListClicked), keyEquivalent: "")
        for item in [rename, delete] {
            item.target = self
            item.isEnabled = !listEditLocked
        }
    }

    @objc private func renameListClicked() {
        guard let list = menuList, let window = view.window else { return }
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 22))
        field.stringValue = list.name
        let alert = NSAlert()
        alert.messageText = loc("sidebar.list.rename.prompt")
        alert.accessoryView = field
        alert.addButton(withTitle: loc("sidebar.list.rename.ok"))
        alert.addButton(withTitle: loc("edit.cancel"))
        StandBack.beginSheet(alert, for: window) { [weak self] response in
            guard response == .alertFirstButtonReturn else { return }
            Task { await self?.rename(list: list.id, to: field.stringValue) }
        }
    }

    @objc private func deleteListClicked() {
        guard let list = menuList else { return }
        Task { await delete(list: list.id) }
    }

    /// Renames a list. Empty text and an unchanged name send nothing.
    func rename(list id: ModelID, to name: String) async {
        guard !listEditLocked, let list = lists.first(where: { $0.id == id }), !name.isEmpty, name != list.name else {
            return
        }
        report(await store.send(command: .renameCueList(id: id, name: name)))
    }

    /// Deletes an empty list; the engine refuses one with cues and the only list, and the reason is shown.
    func delete(list id: ModelID) async {
        guard !listEditLocked, lists.contains(where: { $0.id == id }) else { return }
        report(await store.send(command: .removeCueList(id: id)))
    }

    private func report(_ result: Result<OkBody, EngineError>) {
        guard case .failure(let error) = result else { return }
        if case .engine(_, let message) = error {
            onNotice?(String(format: loc("edit.error.refused"), message))
        } else {
            onNotice?(loc("edit.error.notConnected"))
        }
    }

    /// The user chose a tab (not a restored one).
    var onTabChange: (() -> Void)?

    @objc private func tabChanged() {
        showTab(tabs.selectedSegment)
        onTabChange?()
    }

    func showTab(_ index: Int) {
        tabs.selectedSegment = index
        listScroll.isHidden = index != 0
        activeScroll.isHidden = index != 1
        updateActiveEmptyNote()
    }

    // MARK: Cue lists

    /// Redraws the selection from `shownProvider`; the window calls this after the cue list has
    /// switched (a click here, a deleted list, a new snapshot).
    func syncSelection() {
        guard isViewLoaded else { return }
        // This may run before the sidebar's own store subscription has: draw from the store's lists.
        // Only ids and names are compared: a cue edit must not deep-compare every cue tree.
        let current = store.model?.cueLists ?? []
        if lists.map(\.id) != current.map(\.id) || lists.map(\.name) != current.map(\.name) {
            lists = current
            rebuildListRows()
            listTable.reloadData()
        }
        settingSelection = true
        defer { settingSelection = false }
        // The cue list falls back to the first list when the shown one is gone; so does the highlight.
        let shown = lists.firstIndex(where: { $0.id == shownList }) ?? (lists.isEmpty ? nil : 0)
        // A cue row of the shown list keeps the highlight the operator put on it.
        if let shown, listRows.indices.contains(listTable.selectedRow),
            case .cue(let inList, _, _) = listRows[listTable.selectedRow], inList == shown
        {
            return
        }
        if let row = shown.flatMap({ listRows.firstIndex(of: .list($0)) }) {
            listTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        } else {
            listTable.deselectAll(nil)
        }
    }

    private func reloadLists() {
        lists = store.model?.cueLists ?? []
        openLists.formIntersection(lists.map(\.id))
        rebuildListRows()
        if isViewLoaded {
            listTable.reloadData()
            syncSelection()
        }
    }

    // MARK: Store changes

    private func handle(_ change: StoreChange) {
        defer { updateTransport() }
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
            // An open list shows its cues: any cue change may add, drop, move or rename one.
            if listsChanged || (!openLists.isEmpty && !touched.isEmpty) { reloadLists() }
            // A deleted cue leaves the panel; a renamed one is redrawn. A removed list takes its
            // cues with it without a delete for each.
            if listsChanged || !touched.isDisjoint(with: activeCues) { syncActive(changed: touched) }
        case .runtime(let cues, _):
            syncActive(changed: cues)
        case .progress(let ids): redraw(ids)
        case .waits: break
        case .notices: break
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

    // MARK: Table data

    func numberOfRows(in tableView: NSTableView) -> Int {
        tableView === listTable ? listRows.count : activeCues.count
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        tableView === listTable
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        tableView === activeTable ? 64 : tableView.rowHeight
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !settingSelection, (notification.object as? NSTableView) === listTable else { return }
        let row = listTable.selectedRow
        guard listRows.indices.contains(row) else { return }
        switch listRows[row] {
        case .list(let i):
            if lists.indices.contains(i) {
                onSelectList?(lists[i].id)
                onChooseList?(lists[i].id)
            }
        case .cue(let i, let id, _):
            // A cue of a list: that list is shown and the cue chosen in it, as a click on it there.
            guard lists.indices.contains(i) else { return }
            if shownList != lists[i].id { onSelectList?(lists[i].id) }
            onSelectCue?(id)
        }
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        if tableView === listTable {
            guard listRows.indices.contains(row) else { return nil }
            let cell =
                (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("cue-list-name"), owner: nil)
                    as? CueListNameCell) ?? CueListNameCell()
            cell.disclosure.target = self
            cell.disclosure.action = #selector(disclosureClicked(_:))
            switch listRows[row] {
            case .list(let i):
                guard lists.indices.contains(i) else { return nil }
                cell.indent = 0
                cell.disclosure.isHidden = false
                cell.disclosure.state = openLists.contains(lists[i].id) ? .on : .off
                cell.disclosure.setAccessibilityLabel(String(format: loc("sidebar.list.open"), lists[i].name))
                cell.name.stringValue = lists[i].name
                cell.name.textColor = .labelColor
            case .cue(_, let id, let depth):
                let cue = store.model?.cue(id)
                cell.indent = CGFloat(depth) * 12
                cell.disclosure.isHidden = true
                let shown = cue.map { $0.shownName(lookup: { [store] in store.model?.cue($0) }) } ?? ""
                let label = shown.isEmpty ? CueKindStyle.kindLabel(cue?.kindType ?? "") : shown
                cell.name.stringValue = [cue?.number ?? "", label].filter { !$0.isEmpty }.joined(separator: "  ")
                cell.name.textColor = shown.isEmpty ? .secondaryLabelColor : .labelColor
            }
            return cell
        }
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
    /// state (waiting cues have no progress). A paused or stopping cue keeps its frozen times but
    /// leads with the state name, so a held cue never reads like a running one (QLab marks it paused).
    func fill(_ cell: ActiveCueCell, _ id: ModelID) {
        let cue = store.model?.cue(id)
        let state = store.cueStates[id]?.state ?? .idle
        let name = cue.flatMap { store.model?.listName($0) } ?? ""
        cell.title.stringValue = [cue?.number, name].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ")
        let paused = state == .paused
        cell.pauseButton.cue = id
        cell.panicButton.cue = id
        let pauseName = loc(paused ? "activeCue.resume" : "activeCue.pause")
        cell.pauseButton.image = (paused ? Icon.activeCueResume : Icon.activeCuePause).image(pauseName)
        cell.pauseButton.toolTip = pauseName
        cell.pauseButton.setAccessibilityLabel(pauseName)
        cell.pauseButton.isEnabled = state != .stopping
        if let p = store.progress[id] {
            // A cue looping a slice with no end shows its pass as the cue list's Duration cell does: the
            // time into the pass, the time left in it and a bar for how far through it is.
            let pass = cue.flatMap { LoopPass.of($0, p, fileDuration: store.mediaDurations[id]?.file) }
            let elapsed = pass?.gone ?? p.elapsed
            let remaining = pass.map { $0.length - $0.gone } ?? p.remaining
            let times = Self.timesText(elapsed: elapsed, remaining: remaining)
            cell.times.stringValue =
                state == .paused || state == .stopping ? "\(CueStateStyle.style(for: state).label)  \(times)" : times
            let total = elapsed + (remaining ?? 0)
            cell.bar.doubleValue = remaining != nil && total > 0 ? min(1, elapsed / total) : 0
        } else {
            cell.times.stringValue = CueStateStyle.style(for: state).label
            cell.bar.doubleValue = 0
        }
        cell.stripe.layer?.backgroundColor =
            cue.flatMap { CueKindStyle.colours[CueKindStyle.shownColour(of: $0, played: store.played.contains(id))] }?
            .tint.cgColor
    }

    /// `elapsed` and, when the end is known, `-remaining`.
    static func timesText(elapsed: Double, remaining: Double?) -> String {
        guard let remaining else { return CueTime.running(elapsed) }
        return "\(CueTime.running(elapsed))  -\(CueTime.running(remaining))"
    }

    @objc private func pauseResumeClicked(_ sender: ActiveCueButton) {
        guard let cue = sender.actionCue else { return }
        onCueAction?(store.cueStates[cue]?.state == .paused ? .resume(cue: cue) : .pause(cue: cue))
    }

    @objc private func panicClicked(_ sender: ActiveCueButton) {
        guard let cue = sender.actionCue else { return }
        onCueAction?(.panicCue(cue: cue))
    }
}
