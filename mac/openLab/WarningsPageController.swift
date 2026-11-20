// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Warnings page of the workspace status window (QLab's Warnings tab): every broken cue and
/// engine notice as the footer counts them, live. The workspace window hands it the rows each time
/// they change (`show(_:)`): a new problem appears and a fixed one goes with nothing to press.
/// Choosing a row hands its cue to `onInspect` (the workspace window selects it, as QLab's "Inspect
/// Cue" does); an operator can hide a row, and show the hidden ones again. It reads the store only.
@MainActor
final class WarningsPageController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    let store: WorkspaceStore
    let table: NSTableView = NonKeyTableView()
    let summaryLabel = NSTextField(labelWithString: "")
    let inspectButton = NSButton(title: loc("warnings.inspect"), target: nil, action: nil)
    let hideButton = NSButton(title: loc("warnings.hide"), target: nil, action: nil)
    let showHiddenCheckbox = NSButton(checkboxWithTitle: loc("warnings.showHidden"), target: nil, action: nil)
    /// Rows the operator hid, for this workspace window's lifetime (app state: nothing is written to
    /// the show file, so another machine or a relaunch shows them again).
    private(set) var hidden: Set<String> = []
    private(set) var showHidden = false
    /// A row with a cue was chosen: the cue and the inspector tab that holds what to fix.
    var onInspect: ((ModelID, String?) -> Void)?
    /// A row's Find Missing Files button was used (the tool searches for every missing file, so the
    /// row does not matter).
    var onRelink: (() -> Void)?
    /// Show Mode: relinking rewrites cue media, so the row buttons are off.
    var relinkLocked = false {
        didSet {
            guard relinkLocked != oldValue else { return }
            table.reloadData()
        }
    }
    /// Every warning there is, hidden ones included.
    private(set) var all: [WarningRow] = []
    /// The rows listed: `all` less the hidden ones unless they are shown.
    private(set) var rows: [WarningRow] = []
    /// Ids of rows redrawn in place (tests check that only changed rows were touched).
    private(set) var updatedRows: [String] = []
    /// The row a mouse-down selected: the click's action that follows must not choose it again.
    private var chosenByMouseDown: Int?
    private var reloading = false

    init(store: WorkspaceStore) {
        self.store = store
        // A floating panel stays above the workspace window while its cues are revealed. Its content
        // is taken into the status window; the panel itself is never shown.
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 420),
            styleMask: [.titled, .closable, .resizable, .utilityWindow], backing: .buffered,
            defer: false)
        panel.isFloatingPanel = true
        panel.hidesOnDeactivate = true
        // No view in it needs the panel to be key (the list included: NonKeyTableView), so a click
        // anywhere leaves the workspace window key and GO, the transport keys and cue hotkeys
        // keep reaching it.
        panel.becomesKeyOnlyIfNeeded = true
        panel.title = loc("status.page.warnings")
        panel.identifier = NSUserInterfaceItemIdentifier(AX.warningsPage)
        panel.isReleasedWhenClosed = false
        panel.minSize = NSSize(width: 360, height: 220)
        super.init(window: panel)
        panel.contentView = buildContent()
        panel.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: The live list

    /// What "hide" remembers. It includes the detail, so a changed problem on the same cue is new.
    private static func hideKey(_ row: WarningRow) -> String { "\(row.id)|\(row.detail)" }

    func isHidden(_ row: WarningRow) -> Bool { hidden.contains(Self.hideKey(row)) }

    /// Takes the workspace's current warnings. A selected row stays selected when it is still there,
    /// without choosing its cue again.
    func show(_ new: [WarningRow]) {
        guard new != all else { return }
        all = new
        redraw()
    }

    /// Rebuilds the listed rows from `all` with the hide choices applied.
    private func redraw() {
        let selected = selectedRow?.id
        let listed = showHidden ? all : all.filter { !isHidden($0) }
        let old = rows
        rows = listed
        reloading = true
        defer { reloading = false }
        if old.map(\.id) == listed.map(\.id) {
            // The same rows: only the ones whose words changed are redrawn.
            for i in listed.indices where old[i] != listed[i] {
                updatedRows.append(listed[i].id)
                table.reloadData(forRowIndexes: IndexSet(integer: i), columnIndexes: IndexSet(integer: 0))
                table.noteHeightOfRows(withIndexesChanged: IndexSet(integer: i))
            }
        } else {
            table.reloadData()
        }
        if let selected, let i = rows.firstIndex(where: { $0.id == selected }) {
            if table.selectedRow != i { table.selectRowIndexes(IndexSet(integer: i), byExtendingSelection: false) }
        } else if table.selectedRow >= 0 {
            table.deselectAll(nil)
        }
        let count = all.count
        let summary =
            count == 0 ? loc("warnings.none") : String(format: loc(count == 1 ? "warnings.one" : "warnings.many"), count)
        let hiddenCount = all.filter(isHidden).count
        summaryLabel.stringValue =
            hiddenCount == 0 ? summary : String(format: loc("warnings.summary.hidden"), summary, hiddenCount)
        updateControls()
    }

    func clearUpdateLog() { updatedRows = [] }

    private var selectedRow: WarningRow? {
        rows.indices.contains(table.selectedRow) ? rows[table.selectedRow] : nil
    }

    /// Hides the row, or shows it again when it is hidden.
    func toggleHidden(row: Int) {
        guard rows.indices.contains(row) else { return }
        let key = Self.hideKey(rows[row])
        if !hidden.insert(key).inserted { hidden.remove(key) }
        redraw()
    }

    func setShowHidden(_ on: Bool) {
        showHidden = on
        showHiddenCheckbox.state = on ? .on : .off
        redraw()
    }

    private func updateControls() {
        let entry = selectedRow
        inspectButton.isEnabled = entry?.cue != nil
        hideButton.isEnabled = entry != nil
        hideButton.title = loc(entry.map(isHidden) == true ? "warnings.unhide" : "warnings.hide")
    }

    // MARK: Choosing a row

    /// Selects the row's cue in the workspace window. False when nothing was selected: silently for a
    /// row with no cue (a device or system warning), with a beep when its cue was deleted since.
    @discardableResult
    func choose(row: Int) -> Bool {
        guard rows.indices.contains(row), let cue = rows[row].cue else { return false }
        guard store.model?.cue(cue) != nil else {
            beep()
            return false
        }
        onInspect?(cue, rows[row].inspectorTab)
        return true
    }

    /// The system beep, for a row whose cue was deleted a moment ago; a test counts it.
    var beep: () -> Void = { NSSound.beep() }

    @objc private func rowClicked() {
        let row = table.clickedRow
        defer { chosenByMouseDown = nil }
        // A click on the selected row brings its cue back after the operator moved on.
        if row >= 0, row != chosenByMouseDown, table.selectedRow == row { choose(row: row) }
    }

    /// Inspect Cue: selects the chosen row's cue and opens the tab that fixes it.
    @objc func inspectClicked() {
        let row = table.selectedRow
        if row >= 0 { choose(row: row) }
    }

    @objc private func hideClicked() { toggleHidden(row: table.selectedRow) }
    @objc private func showHiddenClicked() { setShowHidden(showHiddenCheckbox.state == .on) }
    @objc private func relinkClicked() { onRelink?() }

    // MARK: Views

    private func buildContent() -> NSView {
        summaryLabel.lineBreakMode = .byTruncatingTail
        summaryLabel.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        summaryLabel.setAX(AX.warningsSummary)
        summaryLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        summaryLabel.stringValue = loc("warnings.none")
        for (b, action, ax) in [
            (inspectButton, #selector(inspectClicked), AX.warningsInspect),
            (hideButton, #selector(hideClicked), AX.warningsHide),
        ] {
            b.bezelStyle = .rounded
            b.target = self
            b.action = action
            b.setAX(ax)
            b.isEnabled = false
        }
        showHiddenCheckbox.target = self
        showHiddenCheckbox.action = #selector(showHiddenClicked)
        showHiddenCheckbox.setAX(AX.warningsShowHidden)
        let actions = NSStackView(views: [inspectButton, hideButton, showHiddenCheckbox])
        actions.spacing = 8

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("entry"))
        column.title = loc("status.page.warnings")
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.usesAutomaticRowHeights = false
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked)
        table.doubleAction = #selector(inspectClicked)
        table.setAX(AX.warningsTable, label: loc("status.page.warnings"))
        // Rows wrap their reasons at the column's width and are as tall as that makes them: the column
        // is kept as wide as the table.
        table.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(
            self, selector: #selector(tableResized), name: NSView.frameDidChangeNotification, object: table)
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        let root = NSStackView(views: [summaryLabel, scroll, actions])
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = 8
        root.edgeInsets = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        scroll.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -20).isActive = true
        summaryLabel.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -20).isActive = true
        actions.widthAnchor.constraint(lessThanOrEqualTo: root.widthAnchor, constant: -20).isActive = true
        return root
    }

    @objc private func tableResized() {
        guard let column = table.tableColumns.first else { return }
        let visible = table.enclosingScrollView?.contentView.bounds.width ?? table.bounds.width
        // A page that is not shown has no width to fit; it is fitted when it shows.
        guard visible > 40 else { return }
        let width = max(column.minWidth, visible - table.intercellSpacing.width)
        guard abs(column.width - width) > 0.5 else { return }
        column.width = width
        if table.numberOfRows > 0 {
            table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0..<table.numberOfRows))
        }
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { rows.indices.contains(row) }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard rows.indices.contains(row) else { return tableView.rowHeight }
        return WarningCell.height(of: rows[row], width: tableView.frameOfCell(atColumn: 0, row: row).width)
    }

    /// A narrower or wider page wraps the reasons differently.
    func tableViewColumnDidResize(_ notification: Notification) {
        guard table.numberOfRows > 0 else { return }
        table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0..<table.numberOfRows))
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        updateControls()
        let row = table.selectedRow
        guard row >= 0, !reloading else { return }
        if NSApp.currentEvent?.type == .leftMouseDown { chosenByMouseDown = row }
        choose(row: row)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        let cell =
            (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("warning"), owner: nil)
                as? WarningCell) ?? WarningCell()
        let w = rows[row]
        let dimmed = isHidden(w)
        cell.title.stringValue = w.title
        cell.title.textColor = dimmed ? .tertiaryLabelColor : .labelColor
        cell.detail.stringValue = w.detail
        cell.detail.isHidden = w.detail.isEmpty
        cell.needsLayout = true
        cell.relinkButton.isHidden = !w.canRelink
        cell.relinkButton.isEnabled = !relinkLocked
        cell.relinkButton.target = self
        cell.relinkButton.action = #selector(relinkClicked)
        cell.setAccessibilityLabel([w.title, w.detail].filter { !$0.isEmpty }.joined(separator: ". "))
        return cell
    }
}

/// A list that does not need its panel to be key. A plain NSTableView can report that it does
/// (its answer follows AppKit state such as first-responder acceptance, so it cannot be relied
/// on), and then a click on its empty area made the panel key and GO stopped reaching the
/// workspace window. (A panel that
/// can never become key is not used: it drops out of the accessibility window list.)
final class NonKeyTableView: NSTableView {
    override var needsPanelToBecomeKey: Bool { false }
}
