// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

extension LogLevel {
    /// Higher is worse; the filter shows a level and everything worse.
    var severity: Int {
        switch self {
        case .error: return 3
        case .warn: return 2
        case .info: return 1
        case .debug: return 0
        }
    }

    var title: String { loc("log.level.\(rawValue)") }
}

/// The show log window (DX-02): the engine's recent records (`log_tail`, fetched when the window
/// opens or on Refresh) followed by what the store has heard since (`log` messages and the
/// app's own notes), oldest first with the newest at the bottom, bounded, filterable by level.
/// It reads the store only.
@MainActor
final class LogWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    struct Row: Hashable {
        /// Empty for a note the app made (it has no engine timestamp).
        let time: String
        let level: LogLevel
        let kind: String
        let message: String
    }

    static let maxRows = 1000
    /// Filter menu order: the entry's `minimum` is the least severe level shown.
    static let filters: [(key: String, minimum: LogLevel)] = [
        ("log.filter.all", .debug), ("log.filter.info", .info), ("log.filter.warn", .warn),
        ("log.filter.error", .error),
    ]

    let store: WorkspaceStore
    let table = NSTableView()
    let filterPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let refreshButton = NSButton(title: loc("log.refresh"), target: nil, action: nil)
    let note = NSTextField(labelWithString: "")
    private let scroll = NSScrollView()
    private(set) var rows: [Row] = []
    private(set) var visible: [Row] = []
    private var tail: [Row] = []
    private var dropped: UInt64 = 0
    private var tailFailed = false
    private var tailMark = 0
    private var sub: AnyCancellable?
    private var gapSub: AnyCancellable?
    /// A gap re-read is out; `gapAgain`: another gap came meanwhile (one more re-read follows).
    private var gapRefreshing = false
    private var gapAgain = false
    private var storeEntries: [LogEntry]
    private let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }()

    var minimumLevel: LogLevel {
        Self.filters[max(0, min(filterPopup.indexOfSelectedItem, Self.filters.count - 1))].minimum
    }

    init(store: WorkspaceStore) {
        self.store = store
        storeEntries = store.logEntries
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 820, height: 440),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = loc("log.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.logWindow)
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 480, height: 240)
        super.init(window: window)
        window.contentView = buildContent()
        rebuild()
        // `$logEntries` publishes before the property changes: use the delivered value.
        sub = store.$logEntries.sink { [weak self] entries in
            MainActor.assumeIsolated {
                self?.storeEntries = entries
                self?.rebuild()
            }
        }
        // Records the engine made but did not push: an open window reads the tail again.
        gapSub = store.logGaps.sink { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.window?.isVisible == true else { return }
                self.refreshAfterGap()
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: Data

    /// A burst of gaps (a slow client under a log burst) must not start a re-read each: one is in
    /// flight at a time and the gaps meanwhile fold into a single one after it.
    private func refreshAfterGap() {
        guard !gapRefreshing else {
            gapAgain = true
            return
        }
        gapRefreshing = true
        Task {
            repeat {
                gapAgain = false
                await refresh()
            } while gapAgain
            gapRefreshing = false
        }
    }

    /// Fetches the engine's records again. A failure leaves the store's own entries on screen
    /// with a note: a log window that goes blank is no use in a fault.
    func refresh() async {
        // Entries that arrive while the request is out may or may not be in the answer: only
        // those from before it can be.
        let mark = store.logCount
        switch await store.logTail(max: UInt32(Self.maxRows)) {
        case .success(let result): apply(tail: result.entries, dropped: result.dropped, mark: mark)
        case .failure:
            tailFailed = true
            rebuild()
        }
    }

    /// `mark`: the store's `logCount` when the tail was asked for.
    func apply(tail entries: [ShowLogEntry], dropped: UInt64, mark: Int? = nil) {
        tailMark = mark ?? store.logCount
        tail = entries.map { e in
            Row(
                time: timeFormatter.string(from: Date(timeIntervalSince1970: Double(e.timeMs) / 1000)),
                level: e.level, kind: e.kind.raw, message: e.message)
        }
        self.dropped = dropped
        tailFailed = false
        rebuild()
    }

    /// Engine records, then the store's entries. The engine both pushes `log` and keeps the
    /// record, so an entry from before the fetch that the tail holds is shown once: each tail
    /// record cancels one such entry, so two identical entries still show twice. Entries from
    /// after the fetch are always shown.
    private func rebuild() {
        var held: [String: Int] = [:]
        for r in tail { held["\(r.level.rawValue)\u{0}\(r.message)", default: 0] += 1 }
        let first = store.logCount - storeEntries.count
        let extra = storeEntries.enumerated().filter { i, e in
            guard first + i < tailMark else { return true }
            let key = "\(e.level.rawValue)\u{0}\(e.message)"
            guard let n = held[key], n > 0 else { return true }
            held[key] = n - 1
            return false
        }.map(\.element)
        rows = Array(
            (tail + extra.map { Row(time: "", level: $0.level, kind: "", message: $0.message) }).suffix(Self.maxRows))
        let wasAtBottom = isAtBottom
        visible = rows.filter { $0.level.severity >= minimumLevel.severity }
        guard isViewLoaded else { return }
        table.reloadData()
        if wasAtBottom, !visible.isEmpty { table.scrollRowToVisible(visible.count - 1) }
        var parts: [String] = []
        if tailFailed { parts.append(loc("log.tailFailed")) }
        if dropped > 0 { parts.append(String(format: loc("log.dropped"), Int(dropped))) }
        note.stringValue = parts.joined(separator: "  ")
    }

    private var isViewLoaded: Bool { window?.contentView != nil }

    private var isAtBottom: Bool {
        guard isViewLoaded else { return true }
        return scroll.contentView.bounds.maxY >= table.bounds.maxY - 2
    }

    // MARK: Views

    private func buildContent() -> NSView {
        for (i, f) in Self.filters.enumerated() {
            filterPopup.addItem(withTitle: loc(f.key))
            filterPopup.item(at: i)?.tag = i
        }
        filterPopup.selectItem(at: 0)
        filterPopup.target = self
        filterPopup.action = #selector(filterChanged)
        filterPopup.setAX(AX.logLevelFilter, label: loc("log.filter.label"))
        refreshButton.bezelStyle = .rounded
        refreshButton.target = self
        refreshButton.action = #selector(refreshClicked)
        note.textColor = .secondaryLabelColor
        note.lineBreakMode = .byTruncatingTail
        let bar = NSStackView(views: [
            NSTextField(labelWithString: loc("log.filter.label")), filterPopup, refreshButton, note,
        ])
        bar.spacing = 8

        for (id, key, width) in [
            ("time", "log.column.time", 100), ("level", "log.column.level", 64), ("kind", "log.column.kind", 80),
            ("message", "log.column.message", 520),
        ] {
            let c = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(id))
            c.title = loc(key)
            c.width = CGFloat(width)
            c.minWidth = 40
            if id == "message" { c.resizingMask = .autoresizingMask }
            table.addTableColumn(c)
        }
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        table.dataSource = self
        table.delegate = self
        table.setAX(AX.logTable, label: loc("log.title"))
        table.allowsMultipleSelection = true
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        let root = NSStackView(views: [bar, scroll])
        root.orientation = .vertical
        root.spacing = 8
        root.edgeInsets = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        scroll.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -20).isActive = true
        return root
    }

    @objc func filterChanged() { rebuild() }
    @objc private func refreshClicked() { Task { await refresh() } }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { visible.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard visible.indices.contains(row), let id = tableColumn?.identifier.rawValue else { return nil }
        let r = visible[row]
        let field =
            (tableView.makeView(
                withIdentifier: tableColumn?.identifier ?? NSUserInterfaceItemIdentifier("log"), owner: nil)
                as? NSTextField)
            ?? NSTextField(labelWithString: "")
        field.identifier = tableColumn?.identifier
        field.lineBreakMode = .byTruncatingTail
        field.font = .monospacedDigitSystemFont(ofSize: 11, weight: id == "level" ? .semibold : .regular)
        field.textColor = r.level == .error && id != "time" ? .systemRed : .labelColor
        switch id {
        case "time": field.stringValue = r.time
        // The level is a word, so an error does not depend on its colour to be seen.
        case "level": field.stringValue = r.level.title
        case "kind": field.stringValue = r.kind
        default: field.stringValue = r.message
        }
        field.toolTip = id == "message" ? r.message : nil
        return field
    }
}
