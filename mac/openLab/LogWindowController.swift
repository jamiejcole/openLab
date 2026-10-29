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
/// opens or on Refresh) followed by those pushed since (`log_entry`), oldest first with the newest
/// at the bottom, bounded, filterable by level. A gap in the record numbers (or `status.log_seq`
/// ahead of them) reads just the missing ones. It reads the store only.
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
    private var dropped: UInt64 = 0
    private var tailFailed = false
    private var gapSub: AnyCancellable?
    private var pushSub: AnyCancellable?
    private var runSub: AnyCancellable?
    /// The engine's records held, oldest first, by record number (`seq`; 0: a tail from an engine
    /// that numbers none). Only `log_tail` and `log_entry` feed it: `log` notices are the same
    /// records again (docs/protocol.md "Show log entries and status").
    private var records: [(seq: UInt64, row: Row)] = []
    /// Every record up to this number is in `records` (or was dropped from the ring): a gap
    /// re-read starts above it. 0: nothing known, the next read is the whole tail.
    private var contiguous: UInt64 = 0
    /// Bumped on a new engine run: a read begun before it describes the old run and is dropped.
    private var epoch = 0
    /// A gap re-read is out; `gapAgain`: another gap came meanwhile (one more re-read follows),
    /// to the newest record number any of them named.
    private var gapRefreshing = false
    private var gapAgain = false
    private var gapNewest: UInt64 = 0
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
        pushSub = store.logPushes.sink { [weak self] push in
            MainActor.assumeIsolated { self?.append(seq: push.seq, entry: push.entry) }
        }
        // Records the engine made but did not push: an open window reads what is missing.
        gapSub = store.logGaps.sink { [weak self] newest in
            MainActor.assumeIsolated {
                guard let self, self.window?.isVisible == true else { return }
                self.refreshAfterGap(newest: newest)
            }
        }
        // A new connection may be a new engine run, whose records count from 1 again: what is
        // held is of the old one, and numbers below it would be taken for records already shown.
        runSub = store.logRuns.sink { [weak self] in
            MainActor.assumeIsolated { self?.newRun() }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: Data

    private func newRun() {
        epoch += 1
        records = []
        contiguous = 0
        dropped = 0
        tailFailed = false
        rebuild()
        if window?.isVisible == true { refreshAfterGap(newest: 0) }
    }

    /// A burst of gaps (a slow client under a log burst) must not start a re-read each: one is in
    /// flight at a time and the gaps meanwhile fold into a single one after it.
    private func refreshAfterGap(newest: UInt64) {
        gapNewest = max(gapNewest, newest)
        guard !gapRefreshing else {
            gapAgain = true
            return
        }
        gapRefreshing = true
        Task {
            repeat {
                gapAgain = false
                let target = gapNewest
                gapNewest = 0
                await refresh(newest: target)
            } while gapAgain
            gapRefreshing = false
        }
    }

    /// Fetches the engine's records again. A failure leaves what is shown with a note: a log
    /// window that goes blank is no use in a fault. `newest`: the record number a gap read must
    /// reach, which reads only what is above the last contiguous one (at most the ring's worth);
    /// 0, or nothing held yet: the whole tail.
    func refresh(newest: UInt64 = 0) async {
        var count = UInt64(Self.maxRows)
        var merge = false
        if newest > 0, contiguous > 0 {
            guard newest > contiguous else { return }  // already held
            count = min(count, newest - contiguous)
            merge = true
        }
        let started = epoch
        switch await store.logTail(max: UInt32(count)) {
        case .success(let result):
            guard started == epoch else { return }
            apply(tail: result.entries, dropped: result.dropped, seq: result.seq, merge: merge)
        case .failure:
            guard started == epoch else { return }
            tailFailed = true
            rebuild()
        }
    }

    /// `seq`: the number of the newest record in `entries` (0: unnumbered, so nothing can be
    /// merged). `merge`: `entries` are only the newest ones, to join those held.
    func apply(tail entries: [ShowLogEntry], dropped: UInt64, seq: UInt64 = 0, merge: Bool = false) {
        if seq == 0 || entries.isEmpty && !merge {
            records = entries.map { (0, row($0)) }
            contiguous = seq
        } else {
            let first = seq >= UInt64(entries.count) ? seq - UInt64(entries.count) + 1 : 1
            let fresh = entries.enumerated().map { (first + UInt64($0.offset), row($0.element)) }
            let older = merge ? records.filter { $0.seq > 0 && $0.seq < first } : []
            // Pushed records above the read stay; those inside it are in it.
            let newer = records.filter { $0.seq > seq }
            records = older + fresh + newer
            contiguous = max(contiguous, seq)
            advanceContiguous()
        }
        trim()
        self.dropped = dropped
        tailFailed = false
        rebuild()
    }

    private func advanceContiguous() {
        for r in records where r.seq > contiguous {
            guard r.seq == contiguous + 1 else { break }
            contiguous = r.seq
        }
    }

    private func trim() {
        if records.count > Self.maxRows { records.removeFirst(records.count - Self.maxRows) }
    }

    private func row(_ e: ShowLogEntry) -> Row {
        Row(
            time: timeFormatter.string(from: Date(timeIntervalSince1970: Double(e.timeMs) / 1000)),
            level: e.level, kind: e.kind.raw, message: e.message)
    }

    /// A pushed record: one at or below a number already held is already shown.
    private func append(seq: UInt64, entry: ShowLogEntry) {
        guard seq > contiguous, seq > (records.last?.seq ?? 0) else { return }
        records.append((seq, row(entry)))
        if seq == contiguous + 1 { contiguous = seq }
        trim()
        rebuild()
    }

    private func rebuild() {
        rows = records.map(\.row)
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
