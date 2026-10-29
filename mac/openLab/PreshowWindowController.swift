// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The pre-show check window (SM-06, NFR-07): runs the engine's `preshow_check` for its workspace
/// and lists what would keep the show from running, grouped by kind. Choosing an entry hands its
/// cue to `onInspect` (the workspace window selects it, as QLab's Warnings tab "Inspect Cue"
/// does). It reads the store only. The report is a snapshot: Check Again runs it once more.
@MainActor
final class PreshowWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    let store: WorkspaceStore
    let table = NSTableView()
    let checkButton = NSButton(title: loc("preshow.checkAgain"), target: nil, action: nil)
    let summaryLabel = NSTextField(labelWithString: "")
    /// A cue entry was chosen: the cue and the inspector tab that holds what to fix.
    var onInspect: ((ModelID, String?) -> Void)?
    private(set) var rows: [PreshowRow] = []
    private(set) var report: PreshowReport?
    /// A check is out. Answers to an older one (Check Again pressed twice) are dropped.
    private(set) var checking = false
    private var generation = 0
    private var connectionSub: AnyCancellable?
    private var connected = false
    /// The row a mouse-down selected: the click's action that follows must not choose it again.
    private var chosenByMouseDown: Int?

    init(store: WorkspaceStore) {
        self.store = store
        // A floating panel stays above the workspace window while its cues are revealed.
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 420),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .utilityWindow], backing: .buffered,
            defer: false)
        panel.isFloatingPanel = true
        panel.hidesOnDeactivate = true
        // Clicks in the list leave the workspace window key, so GO and the transport keys keep
        // reaching it while the operator works through the report.
        panel.becomesKeyOnlyIfNeeded = true
        panel.title = loc("preshow.title")
        panel.identifier = NSUserInterfaceItemIdentifier(AX.preshowWindow)
        panel.isReleasedWhenClosed = false
        panel.minSize = NSSize(width: 360, height: 220)
        super.init(window: panel)
        panel.contentView = buildContent()
        panel.center()
        // Published before it changes: use the delivered value.
        connectionSub = store.$connectionState.sink { [weak self] state in
            MainActor.assumeIsolated { self?.connectionChanged(state == .connected) }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Shows the panel without taking key focus from the workspace window, and runs the check.
    func present() async {
        window?.orderFront(nil)
        await runCheck()
    }

    // MARK: Running the check

    /// Runs the check and shows its answer. A failure (engine gone, timeout, a reply that does not
    /// decode) says so and clears the list: an old report must never pass for a fresh one.
    func runCheck() async {
        generation += 1
        let mine = generation
        checking = true
        updateControls()
        summaryLabel.stringValue = loc("preshow.checking")
        let result = await store.preshowCheck()
        guard mine == generation else { return }
        checking = false
        switch result {
        case .success(let r): show(r)
        case .failure:
            report = nil
            setRows([])
            summaryLabel.stringValue = connected ? loc("preshow.failed") : loc("preshow.disconnected")
        }
        updateControls()
    }

    func show(_ report: PreshowReport) {
        self.report = report
        setRows(PreshowRows.make(report: report, model: store.model))
        summaryLabel.stringValue = PreshowRows.summary(report)
        summaryLabel.textColor = report.ok ? .labelColor : .systemRed
    }

    private func setRows(_ new: [PreshowRow]) {
        rows = new
        table.reloadData()
    }

    private func connectionChanged(_ on: Bool) {
        let was = connected
        connected = on
        // The engine went away: whatever is listed may no longer be true.
        if was, !on, report != nil || checking {
            generation += 1
            checking = false
            report = nil
            setRows([])
            summaryLabel.stringValue = loc("preshow.disconnected")
            summaryLabel.textColor = .labelColor
        }
        updateControls()
    }

    private func updateControls() {
        checkButton.isEnabled = connected && !checking
    }

    // MARK: Choosing an entry

    /// Selects the row's cue in the workspace window. False (with a beep) when the row has no cue
    /// or the cue was deleted since the check.
    @discardableResult
    func choose(row: Int) -> Bool {
        guard rows.indices.contains(row), let cue = rows[row].cue, store.model?.cue(cue) != nil else {
            if rows.indices.contains(row), !rows[row].isHeading { NSSound.beep() }
            return false
        }
        onInspect?(cue, rows[row].inspectorTab)
        return true
    }

    @objc private func rowClicked() {
        let row = table.clickedRow
        defer { chosenByMouseDown = nil }
        // A click on the selected row brings its cue back after the operator moved on.
        if row >= 0, row != chosenByMouseDown, table.selectedRow == row { choose(row: row) }
    }

    @objc private func checkClicked() { Task { await runCheck() } }

    // MARK: Views

    private func buildContent() -> NSView {
        checkButton.bezelStyle = .rounded
        checkButton.target = self
        checkButton.action = #selector(checkClicked)
        checkButton.setAX(AX.preshowCheckAgain)
        checkButton.isEnabled = false
        summaryLabel.lineBreakMode = .byTruncatingTail
        summaryLabel.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        summaryLabel.setAX(AX.preshowSummary)
        summaryLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let bar = NSStackView(views: [summaryLabel, checkButton])
        bar.spacing = 8

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("entry"))
        column.title = loc("preshow.title")
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.action = #selector(rowClicked)
        table.setAX(AX.preshowTable, label: loc("preshow.title"))
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        let root = NSStackView(views: [bar, scroll])
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = 8
        root.edgeInsets = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        scroll.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -20).isActive = true
        bar.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -20).isActive = true
        return root
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        rows.indices.contains(row) && rows[row].isHeading
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        rows.indices.contains(row) && !rows[row].isHeading
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard rows.indices.contains(row), !rows[row].isHeading else { return 22 }
        let lines = rows[row].detail.isEmpty ? 0 : rows[row].detail.split(separator: "\n").count
        return 22 + CGFloat(lines) * 15
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = table.selectedRow
        guard row >= 0 else { return }
        if NSApp.currentEvent?.type == .leftMouseDown { chosenByMouseDown = row }
        choose(row: row)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        let r = rows[row]
        let id = NSUserInterfaceItemIdentifier(r.isHeading ? "preshow-heading" : "preshow-entry")
        let field =
            (tableView.makeView(withIdentifier: id, owner: nil) as? NSTextField) ?? NSTextField(labelWithString: "")
        field.identifier = id
        field.lineBreakMode = .byTruncatingMiddle
        field.maximumNumberOfLines = 0
        if r.isHeading {
            field.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
            field.textColor = .secondaryLabelColor
            field.stringValue = r.title
        } else {
            let text = NSMutableAttributedString(
                string: r.title,
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .medium),
                    .foregroundColor: r.isProblem ? NSColor.labelColor : NSColor.secondaryLabelColor,
                ])
            if !r.detail.isEmpty {
                text.append(
                    NSAttributedString(
                        string: "\n" + r.detail,
                        attributes: [
                            .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                            .foregroundColor: NSColor.secondaryLabelColor,
                        ]))
            }
            field.attributedStringValue = text
        }
        field.toolTip = r.detail.isEmpty ? nil : r.title + "\n" + r.detail
        field.setAccessibilityLabel(r.detail.isEmpty ? r.title : r.title + ". " + r.detail)
        return field
    }
}
