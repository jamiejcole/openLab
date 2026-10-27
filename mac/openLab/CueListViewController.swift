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

/// A cell: up to three small images around one label. Status column: images 0 (playhead) and 1
/// (state). Name column: type icon, label, colour shape (1), flag (2).
final class CueCell: NSTableCellView {
    let images = [NSImageView(), NSImageView(), NSImageView()]
    let label = NSTextField(labelWithString: "")

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        label.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let views: [NSView] = [images[0], label, images[1], images[2]]
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
        case "name":
            images[0].setAccessibilityIdentifier("cue-type")
            images[1].setAccessibilityIdentifier("cue-colour")
            images[2].setAccessibilityIdentifier("cue-flag")
        case "continue":
            images[0].setAccessibilityIdentifier("cue-continue")
        default: break
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Shows `symbol` in image `i` (hidden when nil) with an accessibility label and tooltip.
    func setImage(_ i: Int, symbol: String?, label: String?, tint: NSColor? = nil, tooltip: String? = nil) {
        let view = images[i]
        guard let symbol, let label else {
            view.isHidden = true
            view.image = nil
            view.toolTip = nil
            return
        }
        view.isHidden = false
        view.image = symbolImage(symbol, label)
        view.contentTintColor = tint
        view.setAccessibilityLabel(label)
        view.toolTip = tooltip ?? label
    }
}

/// The cue list: an outline of one cue list of the workspace, the one the sidebar picked (the first
/// until then). Each window has its own view controller, so each has its own selected list.
/// It mirrors the store's tree in `CueNode`s and applies `StoreChange`s as row inserts, removals,
/// moves and reloads; it reloads the whole table only for a snapshot or when it has lost sync.
/// Read-only: editing, drag and drop and transport keys come later (M5.10, M5.11).
@MainActor
final class CueListViewController: NSViewController, NSOutlineViewDataSource, NSOutlineViewDelegate {
    /// QLab order. The first and last columns carry no title.
    static let columns: [(id: String, titleKey: String?, width: CGFloat)] = [
        ("status", nil, 52), ("number", "cuelist.col.number", 60), ("name", "cuelist.col.name", 280),
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
    private var batchTouched = Set<ModelID>()
    private var batchNewGroups = Set<ModelID>()
    private var batchCollapsed = Set<ModelID>()

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

    func outlineViewSelectionDidChange(_ notification: Notification) {
        // Compared with what was last sent, not only what the engine echoed: A, B, A within one
        // round trip must still end on A.
        guard !restoringSelection, !batching, let list = listID, let id = selectedIDs().first,
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
        }
    }

    /// Replaces the tree from the store and reloads the table, keeping selection and expansion.
    func rebuild() {
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
            reason: run?.reason, problems: problems, groupWarning: store.groupWarnings.contains(id))
        return (state, detail)
    }

    private func fill(_ cell: CueCell, column: String, node: CueNode) {
        let cue = node.cue
        cell.label.stringValue = ""
        cell.label.alphaValue = cue.raw["armed"] == .bool(false) ? 0.5 : 1
        cell.label.textColor = .labelColor
        cell.label.isHidden = true
        for i in cell.images.indices { cell.setImage(i, symbol: nil, label: nil) }
        func text(_ s: String) {
            cell.label.isHidden = false
            cell.label.stringValue = s
        }
        switch column {
        case "status":
            if isPlayheadRow(node.id) {
                cell.setImage(0, symbol: "arrowtriangle.right.fill", label: loc("cue.playhead"))
            } else if isPlayheadInside(node.id) {
                cell.setImage(0, symbol: "arrowtriangle.right", label: loc("cue.playhead.inside"))
            }
            let (state, detail) = displayState(node.id)
            let style = CueStateStyle.style(for: state)
            if let symbol = style.symbol {
                cell.setImage(
                    1, symbol: symbol, label: style.label, tint: style.tint,
                    tooltip: detail.map { "\(style.label): \($0)" })
            }
        case "number": text(cue.number ?? "")
        case "name":
            let kind = cue.kindType
            if let style = CueKindStyle.kinds[kind] {
                cell.setImage(0, symbol: style.symbol, label: loc(style.labelKey))
            }
            if cue.name.isEmpty {
                text(CueKindStyle.kindLabel(kind))
                cell.label.textColor = .secondaryLabelColor
            } else {
                text(cue.name)
            }
            if let colour = cue.raw["color"]?.stringValue.flatMap({ CueKindStyle.colours[$0] }) {
                cell.setImage(1, symbol: colour.symbol, label: loc(colour.labelKey), tint: colour.tint)
            }
            if cue.raw["flagged"] == .bool(true) {
                cell.setImage(2, symbol: "flag.fill", label: loc("cue.flagged"), tint: .systemOrange)
            }
        case "target": text(targetText(cue))
        case "preWait", "postWait":
            let seconds = cue.raw[column == "preWait" ? "pre_wait" : "post_wait"]?.doubleValue ?? 0
            text(CueTimeFormat.clock(seconds))
            if seconds == 0 { cell.label.textColor = .secondaryLabelColor }  // zero is dimmed, as in QLab
        case "duration": text(durationText(node))
        case "continue":
            if let mode = cue.raw["continue_mode"]?.stringValue.flatMap({ CueKindStyle.continueModes[$0] }) {
                cell.setImage(0, symbol: mode.symbol, label: loc(mode.labelKey))
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
        let label = [t.number, t.name.isEmpty ? CueKindStyle.kindLabel(t.kindType) : t.name]
        return label.compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " ")
    }

    private func durationText(_ node: CueNode) -> String {
        if let p = store.progress[node.id] { return CueTimeFormat.progress(elapsed: p.elapsed, remaining: p.remaining) }
        switch node.cue.kindType {
        case "fade", "wait": return CueTimeFormat.clock(node.cue.raw["kind"]?["duration"]?.doubleValue ?? 0)
        default: return ""
        }
    }
}
