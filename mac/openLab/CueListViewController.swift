// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// One row of the outline: a cue of the shown list, with its children when it is a group. A class
/// because `NSOutlineView` identifies items by object identity.
final class CueNode: NSObject {
    let id: ModelID
    var cue: Cue
    weak var parent: CueNode?
    var children: [CueNode] = []

    init(cue: Cue, parent: CueNode?) {
        id = cue.id
        self.cue = cue
        self.parent = parent
    }

    var isGroup: Bool { cue.kindType == "group" }
    var targetID: ModelID? { cue.raw["kind"]?["target"]?.stringValue.map { ModelID($0) } }
}

/// Row background: the cue's colour as a tint, and a bar on the playhead's row.
final class CueRowView: NSTableRowView {
    var tint: NSColor? { didSet { if tint != oldValue { needsDisplay = true } } }
    var isPlayhead = false { didSet { if isPlayhead != oldValue { needsDisplay = true } } }

    override func drawBackground(in dirtyRect: NSRect) {
        super.drawBackground(in: dirtyRect)
        if let tint {
            tint.withAlphaComponent(0.22).setFill()
            bounds.fill()
        }
        if isPlayhead {
            NSColor.controlAccentColor.setFill()
            NSRect(x: 0, y: 0, width: 3, height: bounds.height).fill()
        }
    }
}

/// A cell: up to four small images around one label. Status column: playhead (0), state (1),
/// preview (2), flag (3). Name column: type icon (0) and label.
final class CueCell: NSTableCellView {
    let images = [NSImageView(), NSImageView(), NSImageView(), NSImageView()]
    let label = NSTextField(labelWithString: "")

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        label.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let views: [NSView] = [images[0], label, images[1], images[2], images[3]]
        let stack = NSStackView(views: views)
        stack.spacing = 4
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        for image in images {
            image.imageScaling = .scaleProportionallyDown
            image.setContentHuggingPriority(.required, for: .horizontal)
            image.widthAnchor.constraint(equalToConstant: 16).isActive = true
        }
        textField = label
        // Cell state is set on every reuse, so no stale identifier survives.
        switch identifier.rawValue {
        case "status":
            images[0].setAccessibilityIdentifier("cue-playhead")
            images[1].setAccessibilityIdentifier("cue-state")
            images[2].setAccessibilityIdentifier("cue-preview")
            images[3].setAccessibilityIdentifier("cue-flag")
        case "name":
            images[0].setAccessibilityIdentifier("cue-type")
        case "continue":
            images[0].setAccessibilityIdentifier("cue-continue")
        default: break
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Shows `symbol` in image `i` (hidden when nil) with an accessibility label and tooltip.
    func setImage(_ i: Int, icon: Icon?, label: String?, tint: NSColor? = nil, tooltip: String? = nil) {
        let view = images[i]
        guard let icon, let label else {
            view.isHidden = true
            view.image = nil
            view.toolTip = nil
            return
        }
        view.isHidden = false
        view.image = icon.image(label, compact: true)
        view.contentTintColor = tint
        view.setAccessibilityLabel(label)
        view.toolTip = tooltip ?? label
    }
}

/// The cue list: an outline of one cue list of the workspace, the one the sidebar picked (the first
/// until then). Each window has its own view controller, so each has its own selected list.
/// It mirrors the store's tree in `CueNode`s and applies `StoreChange`s as row inserts, removals,
/// moves and reloads; it reloads the whole table only for a snapshot or when it has lost sync.
/// Editing goes through `CueEditor`; this class only offers the drag and drop, which it hands to `onMove`,
/// and the in-place Duration edit, which it hands to `onEditDuration`.
@MainActor
final class CueListViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate,
    NSTextFieldDelegate
{
    /// QLab order. The first and last columns carry no title.
    static let columns: [(id: String, titleKey: String?, width: CGFloat)] = [
        ("status", nil, 70), ("number", "cuelist.col.number", 60), ("name", "cuelist.col.name", 280),
        ("target", "cuelist.col.target", 160), ("preWait", "cuelist.col.preWait", 80),
        ("duration", "cuelist.col.duration", 120), ("postWait", "cuelist.col.postWait", 80),
        ("continue", nil, 36),
    ]
    static let rowHeight: CGFloat = 24

    let store: WorkspaceStore
    let outlineView: NSOutlineView
    private(set) var roots: [CueNode] = []
    private(set) var listID: ModelID?
    /// The list the sidebar picked. Once set it stays, so a reorder of lists never switches the view;
    /// when that list is deleted the first list takes over.
    private var chosenList: ModelID?
    private var nodes: [ModelID: CueNode] = [:]
    /// Cues that point at a cue, so a rename refreshes their Target cell.
    private var targeters: [ModelID: Set<ModelID>] = [:]
    /// The playhead's cue and every group around it: those groups show that it is inside.
    private var playheadAncestors: Set<ModelID> = []
    private var sub: AnyCancellable?
    /// While `applyModel` runs: every cue it touched (their targeters refresh), the groups it created
    /// (shown expanded, like QLab) and the existing groups that were collapsed (they stay so).
    private var batching = false
    /// The tree is being replaced: its collapses and expands are not the operator's, so they must not move or close an open Duration editor.
    private var rebuilding = false
    private var batchTouched = Set<ModelID>()
    private var batchNewGroups = Set<ModelID>()
    private var batchCollapsed = Set<ModelID>()
    /// Whether a running time shows the whole time or the time left, per time column; each window
    /// has its own. A wait counts down by default, as it always has; Duration shows elapsed / total.
    /// Kept for the life of the window only (no window-state persistence exists yet).
    static let timeColumns = ["preWait", "duration", "postWait"]
    private(set) var timeShown: [String: TimeShown] = [
        "preWait": .remaining, "duration": .full, "postWait": .remaining,
    ]
    /// Where Paste looks for cues (tests use a private one).
    var pasteboard: NSPasteboard = .general
    /// This window's editor and transport: the row menu acts on the window that was clicked, not
    /// on the key window (the menu-bar actions do).
    weak var editor: CueEditor?
    var onCommand: ((TransportCommand) -> Void)?

    /// `outlineView` is a parameter so tests can count the calls made on it.
    init(store: WorkspaceStore, outlineView: NSOutlineView = NSOutlineView()) {
        self.store = store
        self.outlineView = outlineView
        super.init(nibName: nil, bundle: nil)
        sub = store.changes.sink { [weak self] change in
            MainActor.assumeIsolated { self?.handle(change) }
        }
        rebuild()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func loadView() {
        for spec in Self.columns {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(spec.id))
            column.title = spec.titleKey.map(loc) ?? ""
            if Self.timeColumns.contains(spec.id) {
                column.headerCell = TimeHeaderCell(textCell: column.title)
                updateTimeHeader(column)
            }
            column.width = spec.width
            column.minWidth = 24
            outlineView.addTableColumn(column)
        }
        outlineView.outlineTableColumn = outlineView.tableColumns.first { $0.identifier.rawValue == "name" }
        outlineView.usesAlternatingRowBackgroundColors = true
        outlineView.allowsMultipleSelection = true
        // A fixed height keeps AppKit from asking for every row's height.
        outlineView.rowHeight = Self.rowHeight
        outlineView.dataSource = self
        outlineView.delegate = self
        outlineView.setAX(AX.cueList, label: loc("cuelist.label"))
        outlineView.registerForDraggedTypes([Self.dragType, .fileURL])
        outlineView.setDraggingSourceOperationMask(.move, forLocal: true)
        outlineView.target = self
        outlineView.doubleAction = #selector(doubleClicked)
        let menu = NSMenu()
        menu.delegate = self
        outlineView.menu = menu
        durationEditor.setAX(AX.cueDurationEditor, label: loc("cuelist.durationEditor.label"))
        durationEditor.delegate = self
        durationEditor.usesSingleLineMode = true
        durationEditor.font = .systemFont(ofSize: NSFont.systemFontSize)
        let scroll = NSScrollView()
        scroll.documentView = outlineView
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        view = scroll
        outlineView.reloadData()
        // The tree was built before the table had a data source, so nothing is open yet.
        func open(_ nodes: [CueNode]) {
            for node in nodes where node.isGroup {
                outlineView.expandItem(node)
                open(node.children)
            }
        }
        open(roots)
    }

    // MARK: Reading the tree (tests and later features)

    func node(_ id: ModelID) -> CueNode? { nodes[id] }
    var nodeCount: Int { nodes.count }

    func selectedIDs() -> [ModelID] {
        outlineView.selectedRowIndexes.compactMap { (outlineView.item(atRow: $0) as? CueNode)?.id }
    }

    /// Selecting a cue by hand moves the playhead to it (QLab's default); rebuilds that restore the
    /// selection must not, so they raise this flag.
    private var restoringSelection = false
    private var sentPlayhead: ModelID?

    func select(_ ids: [ModelID]) {
        restoringSelection = true
        defer { restoringSelection = false }
        let rows = IndexSet(ids.compactMap { nodes[$0] }.map { outlineView.row(forItem: $0) }.filter { $0 >= 0 })
        outlineView.selectRowIndexes(rows, byExtendingSelection: false)
    }

    /// Selects `id` and scrolls to it, opening any collapsed groups above it. Like `select`, it
    /// does not move the playhead.
    func reveal(_ id: ModelID) {
        var parent = nodes[id]?.parent
        while let node = parent {
            outlineView.expandItem(node)
            parent = node.parent
        }
        select([id])
        if let node = nodes[id] {
            let row = outlineView.row(forItem: node)
            if row >= 0 { outlineView.scrollRowToVisible(row) }
        }
    }

    /// The list to show: the chosen one while it exists, else the first.
    private func shownList() -> CueList? {
        let lists = store.model?.cueLists ?? []
        return lists.first { $0.id == chosenList } ?? lists.first
    }

    /// Shows another cue list (the sidebar's selection). Unknown ids are ignored.
    func showList(_ id: ModelID) {
        guard id != listID, store.model?.cueLists.contains(where: { $0.id == id }) == true else { return }
        chosenList = id
        sentPlayhead = nil
        rebuild()
    }

    /// Show Mode: rows cannot be dragged, nothing can be dropped and no cell can be edited; a
    /// Duration being typed is dropped (callers commit it first, as the inspector's fields).
    var isEditLocked = false {
        didSet { if isEditLocked { cancelDurationEdit() } }
    }
    /// A validated drop: the cues go before child `index` of the parent (nil: top level).
    var onMove: (([ModelID], ModelID?, Int) -> Void)?
    private var dragged: [CueNode] = []
    static let dragType = NSPasteboard.PasteboardType("dev.openlab.cue-id")

    /// Select Next / Previous: selects the visible row `delta` rows from the last selected one (from
    /// before the first row, or after the last, when nothing is selected), as a click would: the
    /// inspector follows and the playhead moves. At either end nothing changes.
    func selectAdjacent(by delta: Int) {
        let count = outlineView.numberOfRows
        guard count > 0 else { return }
        let from = outlineView.selectedRow >= 0 ? outlineView.selectedRow : (delta > 0 ? -1 : count)
        let row = min(max(from + delta, 0), count - 1)
        guard row != from, let node = outlineView.item(atRow: row) as? CueNode else { return }
        selectAsUser([node.id])
    }

    /// Selects and shows `ids` as a click would: the inspector follows and the playhead moves.
    func selectAsUser(_ ids: [ModelID]) {
        for id in ids {
            var p = nodes[id]?.parent
            while let a = p {
                outlineView.expandItem(a)
                p = a.parent
            }
        }
        let rows = IndexSet(ids.compactMap { nodes[$0] }.map { outlineView.row(forItem: $0) }.filter { $0 >= 0 })
        outlineView.selectRowIndexes(rows, byExtendingSelection: false)
        if let row = rows.first { outlineView.scrollRowToVisible(row) }
    }

    /// The selection as of the last change, to tell which cue the user selected last.
    private var lastSelection: Set<ModelID> = []

    /// The cue the playhead goes to when the selection (in row order) becomes `selected` from
    /// `previous`: the most recently selected one (QLab 5). A click or Shift-arrow adds one cue;
    /// a Shift-click range adds several, and `latest` (the table's last selected row) is the one
    /// clicked. A selection that only shrinks (a click on one of several selected) goes to `latest`.
    static func playheadTarget(selected: [ModelID], previous: Set<ModelID>, latest: ModelID?) -> ModelID? {
        let added = selected.filter { !previous.contains($0) }
        if added.count == 1 { return added[0] }
        if let latest, added.isEmpty || added.contains(latest) { return latest }
        return added.last ?? selected.last
    }

    /// Told of every selection change (the inspector follows it).
    var onSelectionChange: (([ModelID]) -> Void)?

    func outlineViewSelectionDidChange(_ notification: Notification) {
        // Programmatic restores (select, rebuild, batches) are not the user's choice.
        if !restoringSelection && !batching { onSelectionChange?(selectedIDs()) }
        let selected = selectedIDs()
        let previous = lastSelection
        lastSelection = Set(selected)
        let lastRow = outlineView.selectedRow >= 0 ? outlineView.selectedRow : nil
        let latest = lastRow.flatMap { (outlineView.item(atRow: $0) as? CueNode)?.id }
        // Compared with what was last sent, not only what the engine echoed: A, B, A within one
        // round trip must still end on A.
        guard !restoringSelection, !batching, let list = listID,
            let id = Self.playheadTarget(selected: selected, previous: previous, latest: latest),
            (sentPlayhead ?? store.playheads[list]) != id
        else { return }
        sentPlayhead = id
        Task { await store.transport(.setPlayhead(list: list, cue: id)) }
    }

    func isPlayheadRow(_ id: ModelID) -> Bool { listID.flatMap { store.playheads[$0] } == id }
    func isPlayheadInside(_ id: ModelID) -> Bool { playheadAncestors.contains(id) && !isPlayheadRow(id) }

    // MARK: Store changes

    private func handle(_ change: StoreChange) {
        switch change {
        case .reset: rebuild()
        case .model(let upserts, let deletes): applyModel(upserts: upserts, deletes: deletes)
        case .runtime(let cues, let heads):
            if !heads.isEmpty { sentPlayhead = nil }  // the engine's echo is the truth again
            refresh(cues, playheads: heads)
        case .progress(let ids): refresh(ids, columns: ["duration"])
        case .waits(let ids): refresh(ids, columns: ["preWait", "postWait"])
        case .notices: break
        }
    }

    /// Replaces the tree from the store and reloads the table, keeping selection and expansion.
    func rebuild() {
        rebuilding = true
        defer { rebuilding = false }
        let selected = selectedIDs()
        let known = Set(nodes.keys)
        let expanded = Set(nodes.values.filter { $0.isGroup && outlineView.isItemExpanded($0) }.map(\.id))
        nodes = [:]
        targeters = [:]
        let list = shownList()
        listID = list?.id
        chosenList = list?.id
        roots = (list?.cues ?? []).map { makeNode($0, parent: nil) }
        updatePlayheadAncestors()
        outlineView.reloadData()
        func expand(_ node: CueNode) {
            // Groups the list has not shown before open expanded.
            if node.isGroup && (expanded.contains(node.id) || !known.contains(node.id)) {
                outlineView.expandItem(node)
            }
            node.children.forEach(expand)
        }
        roots.forEach(expand)
        select(selected)
        placeDurationEditor()
    }

    private func makeNode(_ cue: Cue, parent: CueNode?) -> CueNode {
        let node = CueNode(cue: cue, parent: parent)
        node.children = cue.children.map { makeNode($0, parent: node) }
        nodes[cue.id] = node
        if batching {
            batchTouched.insert(cue.id)
            if node.isGroup { batchNewGroups.insert(cue.id) }
        }
        track(node)
        return node
    }

    private func track(_ node: CueNode) {
        if let target = node.targetID { targeters[target, default: []].insert(node.id) }
    }

    private func untrack(_ node: CueNode) {
        if let target = node.targetID { targeters[target]?.remove(node.id) }
    }

    private func forget(_ node: CueNode) {
        untrack(node)
        if nodes[node.id] === node { nodes[node.id] = nil }
        node.children.forEach(forget)
    }

    private func applyModel(upserts: [Upsert], deletes: [Delete]) {
        guard shownList()?.id == listID else { return rebuild() }
        guard let listID else { return }
        let selected = selectedIDs()
        let oldAncestors = playheadAncestors
        var expanded = Set<ModelID>()
        batching = true
        batchTouched = []
        batchNewGroups = []
        batchCollapsed = []
        defer { batching = false }
        for case .cue(let id) in deletes {
            batchTouched.insert(id)
            remove(id, expanded: &expanded)
        }
        for case .cue(let list, let parent, let index, let cue) in upserts {
            batchTouched.insert(cue.id)
            if list != listID {
                remove(cue.id, expanded: &expanded)  // moved to another list
            } else if !upsert(cue, parent: parent, index: index, expanded: &expanded) {
                return rebuild()  // the engine's tree and ours disagree: resync from the store
            }
        }
        updatePlayheadAncestors()
        for id in expanded.union(batchNewGroups.subtracting(batchCollapsed)) {
            if let n = nodes[id] { outlineView.expandItem(n) }
        }
        if selectedIDs() != selected { select(selected) }
        // Cues that point at a changed cue (or anything in a changed group) show its name and number.
        let pointing = batchTouched.flatMap { targeters[$0] ?? [] }
        if !pointing.isEmpty { refresh(Set(pointing), columns: ["target"]) }
        // Groups that gained or lost the playhead show or clear their "inside" marker.
        let flipped = oldAncestors.symmetricDifference(playheadAncestors)
        if !flipped.isEmpty { refresh(flipped, columns: ["status"]) }
        placeDurationEditor()
    }

    private func remove(_ id: ModelID, expanded: inout Set<ModelID>) {
        guard let node = nodes[id], let index = siblings(of: node).firstIndex(where: { $0 === node }) else { return }
        collectExpanded(node, into: &expanded)
        collectSubtree(node)
        let parent = node.parent
        if let parent { parent.children.remove(at: index) } else { roots.remove(at: index) }
        forget(node)
        outlineView.removeItems(at: IndexSet(integer: index), inParent: parent, withAnimation: [])
    }

    private func siblings(of node: CueNode) -> [CueNode] { node.parent?.children ?? roots }

    private func collectExpanded(_ node: CueNode, into set: inout Set<ModelID>) {
        guard node.isGroup else { return }
        if outlineView.isItemExpanded(node) { set.insert(node.id) } else { batchCollapsed.insert(node.id) }
        for child in node.children { collectExpanded(child, into: &set) }
    }

    private func collectSubtree(_ node: CueNode) {
        batchTouched.insert(node.id)
        node.children.forEach(collectSubtree)
    }

    /// Puts the cue where the engine says, like the store does: it leaves its old place and goes to
    /// `index` among the parent's children. False when the tree cannot take it.
    private func upsert(_ cue: Cue, parent parentID: ModelID?, index: Int, expanded: inout Set<ModelID>) -> Bool {
        var parent: CueNode?
        if let parentID {
            guard let p = nodes[parentID] else { return false }
            parent = p
        }
        guard let existing = nodes[cue.id] else {
            let node = makeNode(cue, parent: parent)
            let at = min(max(index, 0), parent?.children.count ?? roots.count)
            if let parent { parent.children.insert(node, at: at) } else { roots.insert(node, at: at) }
            outlineView.insertItems(at: IndexSet(integer: at), inParent: parent, withAnimation: [])
            return true
        }
        var ancestor = parent
        while let a = ancestor {
            if a === existing { return false }
            ancestor = a.parent
        }
        guard let oldIndex = siblings(of: existing).firstIndex(where: { $0 === existing }) else { return false }
        let oldParent = existing.parent
        let count = (parent?.children.count ?? roots.count) - (oldParent === parent ? 1 : 0)
        let newIndex = min(max(index, 0), count)
        let moved = oldParent !== parent || oldIndex != newIndex
        if moved {
            if let oldParent { oldParent.children.remove(at: oldIndex) } else { roots.remove(at: oldIndex) }
            if let parent {
                parent.children.insert(existing, at: newIndex)
            } else {
                roots.insert(existing, at: newIndex)
            }
            existing.parent = parent
            outlineView.moveItem(at: oldIndex, inParent: oldParent, to: newIndex, inParent: parent)
        }
        replaceContent(existing, with: cue, expanded: &expanded)
        return true
    }

    /// New cue data for a node already in the tree; a group's subtree is reconciled by id so its
    /// children keep their expansion state.
    private func replaceContent(_ node: CueNode, with cue: Cue, expanded: inout Set<ModelID>) {
        untrack(node)
        node.cue = cue
        track(node)
        if node.isGroup {
            collectExpanded(node, into: &expanded)
            collectSubtree(node)
            node.children = reconcile(node, with: cue.children)
            outlineView.reloadItem(node, reloadChildren: true)
        } else {
            node.children.forEach(forget)  // a cue that stopped being a group holds no children
            node.children = []
            outlineView.reloadItem(node, reloadChildren: true)
        }
        configureRow(node)
    }

    private func reconcile(_ parent: CueNode, with cues: [Cue]) -> [CueNode] {
        var old = Dictionary(parent.children.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        var result: [CueNode] = []
        for cue in cues {
            if let node = old.removeValue(forKey: cue.id) {
                untrack(node)
                node.cue = cue
                track(node)
                node.children = reconcile(node, with: cue.children)
                result.append(node)
            } else {
                result.append(makeNode(cue, parent: parent))
            }
        }
        old.values.forEach(forget)
        return result
    }

    // MARK: Runtime display

    private func updatePlayheadAncestors() {
        var set = Set<ModelID>()
        var node = listID.flatMap { store.playheads[$0] }.flatMap { nodes[$0] }
        while let n = node {
            set.insert(n.id)
            node = n.parent
        }
        playheadAncestors = set
    }

    /// Redraws only the rows of `ids` (and the groups around `playheads`), only `columns` of them.
    private func refresh(_ ids: Set<ModelID>, playheads: Set<ModelID> = [], columns: [String]? = nil) {
        if !playheads.isEmpty { updatePlayheadAncestors() }
        var rows = IndexSet()
        func add(_ node: CueNode) {
            let row = outlineView.row(forItem: node)
            guard row >= 0 else { return }
            rows.insert(row)
            configureRow(node, row: row)
        }
        for id in ids { if let node = nodes[id] { add(node) } }
        for id in playheads {
            var node = nodes[id]
            while let n = node {
                add(n)
                node = n.parent
            }
        }
        guard !rows.isEmpty else { return }
        let all = outlineView.tableColumns
        let wanted = columns.map { ids in IndexSet(all.indices.filter { ids.contains(all[$0].identifier.rawValue) }) }
        outlineView.reloadData(forRowIndexes: rows, columnIndexes: wanted ?? IndexSet(all.indices))
    }

    private func configureRow(_ node: CueNode, row: Int? = nil) {
        let r = row ?? outlineView.row(forItem: node)
        guard r >= 0, let view = outlineView.rowView(atRow: r, makeIfNecessary: false) as? CueRowView else { return }
        configure(view, node)
    }

    private func configure(_ view: CueRowView, _ node: CueNode) {
        view.tint = node.cue.raw["color"]?.stringValue.flatMap { CueKindStyle.colours[$0]?.tint }
        view.isPlayhead = isPlayheadRow(node.id)
    }

    // MARK: NSOutlineViewDataSource

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        (item as? CueNode)?.children.count ?? (item == nil ? roots.count : 0)
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        if let node = item as? CueNode { return node.children[index] }
        return roots[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? CueNode)?.isGroup ?? false
    }

    // MARK: Drag and drop

    func outlineView(_ outlineView: NSOutlineView, pasteboardWriterForItem item: Any) -> NSPasteboardWriting? {
        guard !isEditLocked, let node = item as? CueNode else { return nil }
        let pb = NSPasteboardItem()
        pb.setString(node.id.raw, forType: Self.dragType)
        return pb
    }

    func outlineView(
        _ outlineView: NSOutlineView, draggingSession session: NSDraggingSession, willBeginAt screenPoint: NSPoint,
        forItems draggedItems: [Any]
    ) {
        let items = draggedItems.compactMap { $0 as? CueNode }
        // Dragging a selected row drags the whole selection, in list order.
        let selected = selectedIDs().compactMap { nodes[$0] }
        dragged = CueEditor.topLevel(selected.contains { n in items.contains { $0 === n } } ? selected : items)
    }

    func outlineView(
        _ outlineView: NSOutlineView, draggingSession session: NSDraggingSession, endedAt screenPoint: NSPoint,
        operation: NSDragOperation
    ) {
        dragged = []
    }

    /// Where a drop lands, as (parent, index): dropping on a group puts the cue at its end, on any
    /// other cue after it.
    private func dropTarget(_ item: Any?, _ index: Int) -> (parent: CueNode?, index: Int) {
        let node = item as? CueNode
        if index != NSOutlineViewDropOnItemIndex { return (node, index) }
        guard let node else { return (nil, roots.count) }
        if node.isGroup { return (node, node.children.count) }
        let siblings = node.parent?.children ?? roots
        return (node.parent, (siblings.firstIndex { $0 === node } ?? siblings.count - 1) + 1)
    }

    /// What a drop of files from Finder would do.
    enum FileDrop: Equatable {
        case replace(ModelID, URL)
        case insert([URL], ModelID?, Int)
    }

    var onFileDrop: ((FileDrop) -> Void)?

    /// The file URLs of a drag from Finder (empty for a drag of our own rows).
    private func fileURLs(_ info: NSDraggingInfo) -> [URL] {
        guard dragged.isEmpty else { return [] }
        let options: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        return info.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: options) as? [URL] ?? []
    }

    /// What a file drop does, or nil to refuse it: all audio files, nothing in Show Mode. A single
    /// file dropped on an audio cue's row replaces its file; anything else makes cues where it lands.
    func fileDropPlan(_ urls: [URL], item: Any?, index: Int) -> FileDrop? {
        guard !isEditLocked, CueEditor.hasAudioExtensions(urls) else { return nil }
        if urls.count == 1, index == NSOutlineViewDropOnItemIndex, let node = item as? CueNode,
            node.cue.kindType == "audio"
        {
            return .replace(node.id, urls[0])
        }
        let target = dropTarget(item, index)
        return .insert(urls, target.parent?.id, target.index)
    }

    func outlineView(
        _ outlineView: NSOutlineView, validateDrop info: NSDraggingInfo, proposedItem item: Any?,
        proposedChildIndex index: Int
    ) -> NSDragOperation {
        let files = fileURLs(info)
        if !files.isEmpty {
            guard let plan = fileDropPlan(files, item: item, index: index) else { return [] }
            if case .insert(_, let parent, let at) = plan {
                outlineView.setDropItem(parent.flatMap { nodes[$0] }, dropChildIndex: at)
            }
            return .copy
        }
        guard !isEditLocked, !dragged.isEmpty else { return [] }
        let target = dropTarget(item, index)
        guard CueEditor.canDrop(dragged, into: target.parent) else { return [] }
        outlineView.setDropItem(target.parent, dropChildIndex: target.index)
        return .move
    }

    func outlineView(
        _ outlineView: NSOutlineView, acceptDrop info: NSDraggingInfo, item: Any?, childIndex index: Int
    ) -> Bool {
        let files = fileURLs(info)
        if !files.isEmpty {
            guard let plan = fileDropPlan(files, item: item, index: index) else { return false }
            onFileDrop?(plan)
            return true
        }
        guard !isEditLocked, !dragged.isEmpty else { return false }
        let target = dropTarget(item, index)
        guard CueEditor.canDrop(dragged, into: target.parent) else { return false }
        onMove?(dragged.map(\.id), target.parent?.id, target.index)
        return true
    }

    // MARK: In-place Duration (QLab 5, manual pp. 94-95: double-click the Duration column of a cue
    // whose duration can be edited)

    /// The field laid over the Duration cell being edited. It lives in the outline, not in the cell,
    /// so the per-tick refresh of that column (progress) and model echoes cannot replace it mid-typing.
    let durationEditor = NSTextField()
    /// The cue whose Duration is being typed.
    private(set) var editingDuration: ModelID?
    /// A Duration typed and committed (Return, Tab or clicking away): the window controller sends it
    /// through `CueEditor`, which checks it as the inspector does.
    var onEditDuration: ((ModelID, String) -> Void)?

    @objc private func doubleClicked() {
        let row = outlineView.clickedRow
        let column = outlineView.clickedColumn
        guard row >= 0, outlineView.tableColumns.indices.contains(column),
            outlineView.tableColumns[column].identifier.rawValue == "duration",
            let node = outlineView.item(atRow: row) as? CueNode
        else { return }
        // After the click is handled: the outline takes first responder back when its mouse
        // tracking ends, which would close a field opened from inside it.
        let id = node.id
        Task { @MainActor [weak self] in self?.beginDurationEdit(id) }
    }

    /// Opens the Duration cell of `id` for typing. False (nothing happens) in Show Mode or for a cue
    /// whose duration is not set by hand. A Duration already open elsewhere is committed first.
    @discardableResult
    func beginDurationEdit(_ id: ModelID) -> Bool {
        guard !isEditLocked, let node = nodes[id], InspectorEdit.durationKinds.contains(node.cue.kindType),
            outlineView.row(forItem: node) >= 0
        else { return false }
        if let pending = takeDurationEdit() { onEditDuration?(pending.id, pending.text) }
        editingDuration = id
        durationEditor.stringValue = CueTime.format(node.cue.raw["kind"]?["duration"]?.doubleValue ?? 0)
        placeDurationEditor()
        outlineView.addSubview(durationEditor)
        view.window?.makeFirstResponder(durationEditor)
        return true
    }

    /// Ends the in-place edit and returns what was typed, to be sent (nil when none was open).
    func takeDurationEdit() -> (id: ModelID, text: String)? {
        guard let id = editingDuration else { return nil }
        editingDuration = nil
        let text = durationEditor.stringValue
        closeDurationEditor()
        return (id, text)
    }

    /// Ends the in-place edit without sending anything (Esc, Show Mode, the cue gone).
    func cancelDurationEdit() {
        guard editingDuration != nil else { return }
        editingDuration = nil
        closeDurationEditor()
    }

    private func closeDurationEditor() {
        // `editingDuration` is already nil, so the end-of-editing this causes sends nothing.
        if durationEditor.currentEditor() != nil { view.window?.makeFirstResponder(outlineView) }
        durationEditor.removeFromSuperview()
        outlineView.setAccessibilityChildren(nil)  // back to the rows alone
    }

    /// The outline reports only its rows to accessibility; the open field is added to them (and made
    /// to name the outline as its parent) so VoiceOver and UI tests can reach it (#355).
    private func exposeDurationEditor() {
        outlineView.setAccessibilityChildren(nil)
        let rows = outlineView.accessibilityChildren() ?? []
        durationEditor.setAccessibilityParent(outlineView)
        outlineView.setAccessibilityChildren(rows + [durationEditor])
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        refreshFlag(notification)
        replaceEditorAfterLayoutChange()
    }
    func outlineViewItemDidExpand(_ notification: Notification) {
        refreshFlag(notification)
        replaceEditorAfterLayoutChange()
    }

    /// A group's hollow flag shows only while it is collapsed, so opening or closing redraws it.
    private func refreshFlag(_ notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? CueNode else { return }
        refresh([node.id], columns: ["status"])
    }

    enum FlagMarker { case solid, hollow }

    /// QLab shows a flag in the status column, and on a collapsed group a hollow flag when a cue
    /// inside it is flagged. A cue's colour never adds a marker: it only tints the row.
    func flagMarker(_ node: CueNode, expanded: Bool) -> FlagMarker? {
        func flagged(_ n: CueNode) -> Bool { n.cue.raw["flagged"] == .bool(true) }
        func anyFlagged(_ n: CueNode) -> Bool { n.children.contains { flagged($0) || anyFlagged($0) } }
        if flagged(node) { return .solid }
        return node.isGroup && !expanded && anyFlagged(node) ? .hollow : nil
    }
    func outlineViewColumnDidResize(_ notification: Notification) { replaceEditorAfterLayoutChange() }
    func outlineViewColumnDidMove(_ notification: Notification) { replaceEditorAfterLayoutChange() }
    private func replaceEditorAfterLayoutChange() { if !rebuilding && !batching { placeDurationEditor() } }

    /// Keeps the open editor over its cue's cell after rows moved; closes it if the cue is gone or no
    /// longer has a duration to set (another client deleted it or changed its kind).
    private func placeDurationEditor() {
        guard let id = editingDuration else { return }
        guard let node = nodes[id], InspectorEdit.durationKinds.contains(node.cue.kindType),
            let column = outlineView.tableColumns.firstIndex(where: { $0.identifier.rawValue == "duration" })
        else { return cancelDurationEdit() }
        let row = outlineView.row(forItem: node)
        guard row >= 0 else { return cancelDurationEdit() }
        durationEditor.frame = outlineView.frameOfCell(atColumn: column, row: row)
        exposeDurationEditor()
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard obj.object as? NSTextField === durationEditor, let edit = takeDurationEdit() else { return }
        onEditDuration?(edit.id, edit.text)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard control === durationEditor, selector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        cancelDurationEdit()
        return true
    }

    // MARK: NSOutlineViewDelegate

    func outlineView(_ outlineView: NSOutlineView, rowViewForItem item: Any) -> NSTableRowView? {
        guard let node = item as? CueNode else { return nil }
        let id = NSUserInterfaceItemIdentifier("cue-row")
        let view = (outlineView.makeView(withIdentifier: id, owner: nil) as? CueRowView) ?? CueRowView()
        view.identifier = id
        configure(view, node)
        return view
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let node = item as? CueNode, let id = tableColumn?.identifier else { return nil }
        let cell = (outlineView.makeView(withIdentifier: id, owner: nil) as? CueCell) ?? CueCell(identifier: id)
        fill(cell, column: id.rawValue, node: node)
        return cell
    }

    // MARK: Cell content

    /// The state a cue shows: its run state, else broken when it has static problems.
    func displayState(_ id: ModelID) -> (state: WireCueState, detail: String?) {
        let run = store.cueStates[id]
        let problems = store.problems[id] ?? []
        let state = run?.state ?? (problems.isEmpty ? .idle : .broken)
        guard state == .broken else { return (state, nil) }
        let detail = CueStateStyle.brokenDetail(
            reason: run?.reason, problems: problems, groupWarning: store.groupWarnings.contains(id),
            label: WarningRows.labeler(store.model))
        return (state, detail)
    }

    // MARK: Time column headers

    /// The header chevron: switches the column between the full time and the time left.
    func toggleTimeShown(_ column: String) {
        guard Self.timeColumns.contains(column) else { return }
        timeShown[column] = timeShown[column] == .full ? .remaining : .full
        guard let index = outlineView.tableColumns.firstIndex(where: { $0.identifier.rawValue == column }) else {
            return
        }
        updateTimeHeader(outlineView.tableColumns[index])
        outlineView.headerView?.needsDisplay = true
        let rows = outlineView.numberOfRows
        if rows > 0 {
            outlineView.reloadData(forRowIndexes: IndexSet(0..<rows), columnIndexes: IndexSet(integer: index))
        }
        placeDurationEditor()
    }

    private func updateTimeHeader(_ column: NSTableColumn) {
        let shown = timeShown[column.identifier.rawValue] ?? .remaining
        (column.headerCell as? TimeHeaderCell)?.shown = shown
        column.headerToolTip = loc(shown == .full ? "cuelist.time.showRemaining" : "cuelist.time.showFull")
    }

    func outlineView(_ outlineView: NSOutlineView, didClick tableColumn: NSTableColumn) {
        toggleTimeShown(tableColumn.identifier.rawValue)
    }

    private func fill(_ cell: CueCell, column: String, node: CueNode) {
        let cue = node.cue
        cell.label.stringValue = ""
        cell.label.alphaValue = cue.raw["armed"] == .bool(false) ? 0.5 : 1
        cell.label.textColor = .labelColor
        cell.label.isHidden = true
        for i in cell.images.indices { cell.setImage(i, icon: nil, label: nil) }
        func text(_ s: String) {
            cell.label.isHidden = false
            cell.label.stringValue = s
        }
        switch column {
        case "status":
            if isPlayheadRow(node.id) {
                cell.setImage(0, icon: .playhead, label: loc("cue.playhead"))
            } else if isPlayheadInside(node.id) {
                cell.setImage(0, icon: .playheadInside, label: loc("cue.playhead.inside"))
            }
            let (state, detail) = displayState(node.id)
            let style = CueStateStyle.style(for: state)
            if let icon = style.icon {
                cell.setImage(
                    1, icon: icon, label: style.label, tint: style.tint,
                    tooltip: detail.map { "\(style.label): \($0)" })
            }
            // A preview is outside the show, so it has its own mark next to the cue's state.
            if store.previewing.contains(node.id) {
                cell.setImage(2, icon: .preview, label: loc("cue.previewing"), tint: .systemTeal)
            }
            switch flagMarker(node, expanded: outlineView.isItemExpanded(node)) {
            case .solid: cell.setImage(3, icon: .flagged, label: loc("cue.flagged"), tint: .systemYellow)
            case .hollow:
                cell.setImage(3, icon: .flaggedInside, label: loc("cue.flagged.inside"), tint: .systemYellow)
            case nil: break
            }
        case "number": text(cue.number ?? "")
        case "name":
            let kind = cue.kindType
            if let style = CueKindStyle.kinds[kind] {
                cell.setImage(0, icon: style.icon, label: loc(style.labelKey))
            }
            if cue.displayName.isEmpty {
                text(CueKindStyle.kindLabel(kind))
                cell.label.textColor = .secondaryLabelColor
            } else {
                text(cue.displayName)
            }
        case "target": text(targetText(cue))
        case "preWait", "postWait":
            // A wait in progress counts down what is left, as QLab's columns do.
            if let wait = store.waits[cue.id]?[column == "preWait" ? .preWait : .postWait] {
                text(
                    timeShown[column] == .full
                        ? CueTime.progress(elapsed: wait.elapsed, remaining: wait.remaining)
                        : CueTime.running(wait.remaining))
                break
            }
            let seconds = cue.raw[column == "preWait" ? "pre_wait" : "post_wait"]?.doubleValue ?? 0
            text(CueTime.format(seconds))
            if seconds == 0 { cell.label.textColor = .secondaryLabelColor }  // zero is dimmed, as in QLab
        case "duration": text(durationText(node))
        case "continue":
            if let mode = cue.raw["continue_mode"]?.stringValue.flatMap({ CueKindStyle.continueModes[$0] }) {
                cell.setImage(0, icon: mode.icon, label: loc(mode.labelKey))
            }
        default: break
        }
    }

    private func targetText(_ cue: Cue) -> String {
        if cue.kindType == "audio" {
            let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
            return (media as NSString).lastPathComponent
        }
        if cue.kindType == "network" { return cue.raw["kind"]?["address"]?.stringValue ?? "" }
        guard let target = cue.raw["kind"]?["target"]?.stringValue.map({ ModelID($0) }) else { return "" }
        guard let t = nodes[target]?.cue ?? store.model?.cue(target) else { return loc("cue.target.missing") }
        let label = [t.number, t.listName]
        return label.compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " ")
    }

    private func durationText(_ node: CueNode) -> String {
        if let p = store.progress[node.id] {
            if timeShown["duration"] == .remaining, let left = p.remaining { return CueTime.running(left) }
            return CueTime.progress(elapsed: p.elapsed, remaining: p.remaining)
        }
        switch node.cue.kindType {
        case "fade", "wait": return CueTime.format(node.cue.raw["kind"]?["duration"]?.doubleValue ?? 0)
        default: return ""
        }
    }
}
