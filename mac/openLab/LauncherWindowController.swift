// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

extension AX {
    static let launcherRecents = "launcher-recents"
    static let launcherStatus = "launcher-status"
    static let launcherEmpty = "launcher-empty"
    static let launcherRemove = "launcher-remove"
}

/// What the launcher says about the engine: New, Open and the recents work only once it is ready.
enum LauncherEngineState: Equatable {
    case starting
    case ready
    /// The engine did not start; the text is why.
    case failed(String)
}

/// A recent workspace as the launcher lists it; `missing`: its file is no longer where it was.
struct LauncherRecent: Equatable {
    var url: URL
    var missing = false
    var name: String { url.deletingPathExtension().lastPathComponent }
}

/// Return opens the selected recent workspace, as a double-click does.
private final class RecentsTable: NSTableView {
    var onReturn: (() -> Void)?
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 || event.keyCode == 76 { onReturn?() } else { super.keyDown(with: event) }
    }
}

/// One recent workspace: its name over the folder it is in.
private final class RecentCell: NSTableCellView {
    let name = NSTextField(labelWithString: "")
    let folder = NSTextField(labelWithString: "")
    let icon = NSImageView()
    private var missing = false

    override init(frame: NSRect) {
        super.init(frame: frame)
        name.font = .systemFont(ofSize: 13, weight: .medium)
        name.lineBreakMode = .byTruncatingTail
        folder.font = .systemFont(ofSize: 11)
        folder.lineBreakMode = .byTruncatingHead
        icon.imageScaling = .scaleProportionallyDown
        let texts = NSStackView(views: [name, folder])
        texts.orientation = .vertical
        texts.alignment = .leading
        texts.spacing = 1
        let row = NSStackView(views: [icon, texts])
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = 10
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        textField = name
        for field in [name, folder] { field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal) }
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: 20),
            icon.heightAnchor.constraint(equalToConstant: 20),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func configure(_ recent: LauncherRecent) {
        missing = recent.missing
        name.stringValue = recent.name
        let folderPath = (recent.url.deletingLastPathComponent().path as NSString).abbreviatingWithTildeInPath
        folder.stringValue = recent.missing ? String(format: loc("launcher.recent.missing"), folderPath) : folderPath
        let symbol = recent.missing ? "exclamationmark.triangle" : "doc"
        icon.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
        recolor()
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { recolor() }
    }

    /// A selected row draws white text (the selection's own colour); a missing file is dimmed.
    private func recolor() {
        let selected = backgroundStyle == .emphasized
        name.textColor = selected ? .alternateSelectedControlTextColor : (missing ? .secondaryLabelColor : .labelColor)
        folder.textColor = selected ? .alternateSelectedControlTextColor : .secondaryLabelColor
        icon.contentTintColor =
            selected ? .alternateSelectedControlTextColor : (missing ? .systemOrange : .secondaryLabelColor)
    }
}

/// A large borderless button: a symbol, a title and a line saying what it does. Its text is
/// dimmed by hand while it is disabled (an attributed title is not dimmed by AppKit).
private final class LauncherActionButton: NSButton {
    private let heading: String
    private let detail: String

    init(symbol: String, title: String, detail: String) {
        heading = title
        self.detail = detail
        super.init(frame: .zero)
        image = Self.slot(symbol)
        imagePosition = .imageLeft
        imageHugsTitle = true
        isBordered = false
        alignment = .left
        (cell as? NSButtonCell)?.lineBreakMode = .byTruncatingTail
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The symbol centred in a fixed-size slot, so every button's text starts at the same place.
    private static func slot(_ symbol: String) -> NSImage? {
        guard
            let glyph = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 22, weight: .light))
        else { return nil }
        let size = NSSize(width: 36, height: 30)
        let canvas = NSImage(size: size, flipped: false) { _ in
            let g = glyph.size
            glyph.draw(
                in: NSRect(
                    x: (size.width - g.width) / 2, y: (size.height - g.height) / 2, width: g.width, height: g.height))
            return true
        }
        canvas.isTemplate = true
        return canvas
    }

    override var isEnabled: Bool {
        didSet { refresh() }
    }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: 44)
    }

    private func refresh() {
        let main: NSColor = isEnabled ? .labelColor : .disabledControlTextColor
        let sub: NSColor = isEnabled ? .secondaryLabelColor : .disabledControlTextColor
        let text = NSMutableAttributedString(
            string: heading + "\n", attributes: [.font: NSFont.systemFont(ofSize: 15), .foregroundColor: main])
        text.append(
            NSAttributedString(
                string: detail, attributes: [.font: NSFont.systemFont(ofSize: 11), .foregroundColor: sub]))
        let style = NSMutableParagraphStyle()
        style.alignment = .left
        style.lineBreakMode = .byTruncatingTail
        text.addAttribute(.paragraphStyle, value: style, range: NSRange(location: 0, length: text.length))
        attributedTitle = text
        contentTintColor = isEnabled ? .secondaryLabelColor : .disabledControlTextColor
    }
}

/// Shown when no workspace is open, and from the Window menu: New, Open and the recent workspaces,
/// and whether the engine is ready for them. (The Connect and Templates tabs wait for their backends.)
@MainActor
final class LauncherWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    var onNew: (() -> Void)?
    var onOpen: (() -> Void)?
    var onOpenRecent: ((URL) -> Void)?
    /// The notice's Remove from List button: forget this recent workspace.
    var onForgetRecent: ((URL) -> Void)?

    private let table = RecentsTable()
    private let scroll = NSScrollView()
    private let emptyView = NSStackView()
    private let newButton = LauncherActionButton(
        symbol: "doc.badge.plus", title: loc("launcher.new"), detail: loc("launcher.new.detail"))
    private let openButton = LauncherActionButton(
        symbol: "folder", title: loc("launcher.open"), detail: loc("launcher.open.detail"))
    private(set) var recents: [LauncherRecent] = []

    // Engine status (left column).
    private let statusBox = NSStackView()
    private let statusTitle = NSTextField(labelWithString: "")
    private let statusDetail = NSTextField(wrappingLabelWithString: "")
    private let spinner = NSProgressIndicator()
    private let statusIcon = NSImageView()

    // Notice (right column, under the list).
    private let noticeBox = NSStackView()
    private let removeButton = NSButton(title: loc("launcher.remove"), target: nil, action: nil)
    private var missingURL: URL?
    /// Why something did not open (shown under the recents; empty: hidden).
    let noticeLabel = NSTextField(wrappingLabelWithString: "")

    private(set) var engineState = LauncherEngineState.ready

    static let defaultSize = NSSize(width: 640, height: 400)
    static let leastSize = NSSize(width: 560, height: 380)

    init() {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.defaultSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = loc("launcher.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.launcherWindow)
        window.isReleasedWhenClosed = false
        window.contentMinSize = Self.leastSize
        super.init(window: window)

        newButton.target = self
        newButton.action = #selector(newClicked)
        newButton.setAX(AX.launcherNew)
        openButton.target = self
        openButton.action = #selector(openClicked)
        openButton.setAX(AX.launcherOpen)

        let left = leftColumn()
        let line = separator()
        let right = rightColumn()
        let content = NSView()
        for v in [left, line, right] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
            v.topAnchor.constraint(equalTo: content.topAnchor).isActive = true
            v.bottomAnchor.constraint(equalTo: content.bottomAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            left.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            line.leadingAnchor.constraint(equalTo: left.trailingAnchor),
            line.widthAnchor.constraint(equalToConstant: 1),
            right.leadingAnchor.constraint(equalTo: line.trailingAnchor),
            right.trailingAnchor.constraint(equalTo: content.trailingAnchor),
        ])
        window.contentView = content
        window.initialFirstResponder = table
        window.center()
        reload(recents: [])
        apply(state: .ready)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: Layout

    private func separator() -> NSView {
        let line = NSBox()
        line.boxType = .custom
        line.borderWidth = 0
        line.fillColor = .separatorColor
        line.translatesAutoresizingMaskIntoConstraints = false
        return line
    }

    private func leftColumn() -> NSView {
        let icon = NSImageView(image: NSApp?.applicationIconImage ?? NSImage())
        icon.imageScaling = .scaleProportionallyUpOrDown
        let name = NSTextField(labelWithString: loc("launcher.title"))
        name.font = .systemFont(ofSize: 24, weight: .semibold)
        let version = NSTextField(labelWithString: String(format: loc("launcher.version"), Self.version))
        version.font = .systemFont(ofSize: 11)
        version.textColor = .secondaryLabelColor
        version.setAX(AX.launcherVersion)
        let names = NSStackView(views: [name, version])
        names.orientation = .vertical
        names.alignment = .leading
        names.spacing = 2
        let header = NSStackView(views: [icon, names])
        header.orientation = .horizontal
        header.alignment = .centerY
        header.spacing = 12
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: 56),
            icon.heightAnchor.constraint(equalToConstant: 56),
        ])

        let actions = NSStackView(views: [newButton, openButton])
        actions.orientation = .vertical
        actions.alignment = .leading
        actions.spacing = 10
        for b in [newButton, openButton] { b.widthAnchor.constraint(equalTo: actions.widthAnchor).isActive = true }

        statusTitle.font = .systemFont(ofSize: 12, weight: .semibold)
        statusDetail.font = .systemFont(ofSize: 11)
        statusDetail.textColor = .secondaryLabelColor
        statusDetail.preferredMaxLayoutWidth = 190
        statusBox.setAX(AX.launcherStatus)
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isIndeterminate = true
        spinner.isDisplayedWhenStopped = false
        statusIcon.image = NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: nil)
        statusIcon.contentTintColor = .systemOrange
        let marker = NSView()
        marker.translatesAutoresizingMaskIntoConstraints = false
        for v in [spinner, statusIcon] {
            v.translatesAutoresizingMaskIntoConstraints = false
            marker.addSubview(v)
            NSLayoutConstraint.activate([
                v.centerXAnchor.constraint(equalTo: marker.centerXAnchor),
                v.centerYAnchor.constraint(equalTo: marker.centerYAnchor),
            ])
        }
        NSLayoutConstraint.activate([
            marker.widthAnchor.constraint(equalToConstant: 16),
            marker.heightAnchor.constraint(equalToConstant: 16),
        ])
        let statusTexts = NSStackView(views: [statusTitle, statusDetail])
        statusTexts.orientation = .vertical
        statusTexts.alignment = .leading
        statusTexts.spacing = 2
        statusBox.setViews([marker, statusTexts], in: .leading)
        statusBox.orientation = .horizontal
        statusBox.alignment = .top
        statusBox.spacing = 8

        let column = NSView()
        for v in [header, actions, statusBox] {
            v.translatesAutoresizingMaskIntoConstraints = false
            column.addSubview(v)
        }
        NSLayoutConstraint.activate([
            column.widthAnchor.constraint(equalToConstant: 260),
            header.topAnchor.constraint(equalTo: column.topAnchor, constant: 28),
            header.leadingAnchor.constraint(equalTo: column.leadingAnchor, constant: 24),
            header.trailingAnchor.constraint(lessThanOrEqualTo: column.trailingAnchor, constant: -20),
            actions.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 28),
            actions.leadingAnchor.constraint(equalTo: column.leadingAnchor, constant: 24),
            actions.trailingAnchor.constraint(equalTo: column.trailingAnchor, constant: -20),
            statusBox.leadingAnchor.constraint(equalTo: column.leadingAnchor, constant: 24),
            statusBox.trailingAnchor.constraint(equalTo: column.trailingAnchor, constant: -20),
            statusBox.bottomAnchor.constraint(equalTo: column.bottomAnchor, constant: -20),
            statusBox.topAnchor.constraint(greaterThanOrEqualTo: actions.bottomAnchor, constant: 16),
            statusDetail.widthAnchor.constraint(lessThanOrEqualToConstant: 190),
        ])
        return column
    }

    private func rightColumn() -> NSView {
        let heading = NSTextField(labelWithString: loc("launcher.recent"))
        heading.font = .systemFont(ofSize: 12, weight: .semibold)
        heading.textColor = .secondaryLabelColor

        let only = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("recent"))
        table.addTableColumn(only)
        table.headerView = nil
        table.rowHeight = 44
        table.intercellSpacing = NSSize(width: 0, height: 0)
        table.allowsEmptySelection = true
        table.backgroundColor = .clear
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(rowActivated)
        table.onReturn = { [weak self] in self?.openSelected() }
        table.setAX(AX.launcherRecents)
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.autohidesScrollers = true

        let emptyTitle = NSTextField(labelWithString: loc("launcher.recent.none"))
        emptyTitle.font = .systemFont(ofSize: 13, weight: .medium)
        emptyTitle.textColor = .secondaryLabelColor
        let emptyHint = NSTextField(wrappingLabelWithString: loc("launcher.recent.none.detail"))
        emptyHint.font = .systemFont(ofSize: 11)
        emptyHint.textColor = .tertiaryLabelColor
        emptyHint.alignment = .center
        emptyHint.preferredMaxLayoutWidth = 240
        emptyView.setViews([emptyTitle, emptyHint], in: .center)
        emptyView.orientation = .vertical
        emptyView.alignment = .centerX
        emptyView.spacing = 4
        emptyView.setAX(AX.launcherEmpty)

        let list = NSView()
        let listBackground = NSBox()
        listBackground.boxType = .custom
        listBackground.borderWidth = 0
        listBackground.fillColor = .controlBackgroundColor
        listBackground.contentViewMargins = .zero
        for v in [listBackground, scroll, emptyView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            list.addSubview(v)
        }
        NSLayoutConstraint.activate([
            listBackground.topAnchor.constraint(equalTo: list.topAnchor),
            listBackground.bottomAnchor.constraint(equalTo: list.bottomAnchor),
            listBackground.leadingAnchor.constraint(equalTo: list.leadingAnchor),
            listBackground.trailingAnchor.constraint(equalTo: list.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: list.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: list.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: list.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: list.trailingAnchor),
            emptyView.centerXAnchor.constraint(equalTo: list.centerXAnchor),
            emptyView.centerYAnchor.constraint(equalTo: list.centerYAnchor),
            emptyView.leadingAnchor.constraint(greaterThanOrEqualTo: list.leadingAnchor, constant: 16),
            emptyView.trailingAnchor.constraint(lessThanOrEqualTo: list.trailingAnchor, constant: -16),
        ])

        noticeLabel.font = .systemFont(ofSize: 12)
        noticeLabel.setAX(AX.launcherNotice)
        noticeLabel.isHidden = true
        let warn = NSImageView(
            image: NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: nil)
                ?? NSImage())
        warn.contentTintColor = .systemOrange
        warn.setContentHuggingPriority(.required, for: .horizontal)
        removeButton.target = self
        removeButton.action = #selector(removeClicked)
        removeButton.bezelStyle = .rounded
        removeButton.controlSize = .small
        removeButton.setAX(AX.launcherRemove)
        removeButton.isHidden = true
        removeButton.setContentHuggingPriority(.required, for: .horizontal)
        // The text beside its mark, the button under the text: a narrow window wraps the text and
        // still shows both whole.
        let message = NSStackView(views: [warn, noticeLabel])
        message.orientation = .horizontal
        message.alignment = .top
        message.spacing = 8
        noticeBox.setViews([message, removeButton], in: .leading)
        noticeBox.orientation = .vertical
        noticeBox.alignment = .leading
        noticeBox.spacing = 8
        noticeBox.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        noticeBox.isHidden = true
        noticeLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let column = NSStackView()
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 0
        let headingRow = NSView()
        headingRow.translatesAutoresizingMaskIntoConstraints = false
        heading.translatesAutoresizingMaskIntoConstraints = false
        headingRow.addSubview(heading)
        NSLayoutConstraint.activate([
            heading.leadingAnchor.constraint(equalTo: headingRow.leadingAnchor, constant: 12),
            heading.centerYAnchor.constraint(equalTo: headingRow.centerYAnchor),
            headingRow.heightAnchor.constraint(equalToConstant: 32),
        ])
        column.setViews([headingRow, list, noticeBox], in: .leading)
        for v in [headingRow, list, noticeBox] { v.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true }
        column.setContentHuggingPriority(.defaultLow, for: .horizontal)
        list.setContentHuggingPriority(.defaultLow, for: .vertical)
        column.widthAnchor.constraint(greaterThanOrEqualToConstant: 280).isActive = true
        return column
    }

    // MARK: State

    /// What the launcher says and allows while the engine starts, is ready or did not start.
    func apply(state: LauncherEngineState) {
        engineState = state
        let ready = state == .ready
        for b in [newButton, openButton] { b.isEnabled = ready }
        table.isEnabled = ready
        scroll.alphaValue = ready ? 1 : 0.5
        statusBox.isHidden = ready
        switch state {
        case .ready: spinner.stopAnimation(nil)
        case .starting:
            statusTitle.stringValue = loc("launcher.starting")
            statusDetail.stringValue = loc("launcher.starting.detail")
            statusIcon.isHidden = true
            spinner.startAnimation(nil)
        case .failed(let reason):
            statusTitle.stringValue = loc("launcher.failed")
            statusDetail.stringValue = reason
            statusIcon.isHidden = false
            spinner.stopAnimation(nil)
        }
        statusDetail.isHidden = statusDetail.stringValue.isEmpty
    }

    /// A line under the list; `nil` hides it. `removing`: the recent workspace the Remove from List
    /// button forgets.
    func showNotice(_ text: String?, removing url: URL? = nil) {
        noticeLabel.stringValue = text ?? ""
        noticeLabel.isHidden = text == nil
        noticeBox.isHidden = text == nil
        missingURL = text == nil ? nil : url
        removeButton.isHidden = missingURL == nil
    }

    /// The recent workspace's file is not where it was: say so and offer to forget it.
    func showMissing(_ url: URL) {
        showNotice(
            String(format: loc("launcher.missing"), url.deletingPathExtension().lastPathComponent), removing: url)
    }

    func reload(recents urls: [URL], missing: Set<URL> = []) {
        recents = urls.prefix(10).map { LauncherRecent(url: $0, missing: missing.contains($0)) }
        table.reloadData()
        let any = !recents.isEmpty
        scroll.isHidden = !any
        emptyView.isHidden = any
        if any, table.selectedRow < 0 { table.selectRowIndexes([0], byExtendingSelection: false) }
    }

    /// The app's version as the bundle says it (`dev` for a bundle that says none, as a test's).
    static var version: String { AboutWindowController.version }

    // MARK: Actions

    /// Opens the selected recent workspace (double-click, or Return).
    func openSelected() {
        guard engineState == .ready, recents.indices.contains(table.selectedRow) else { return }
        onOpenRecent?(recents[table.selectedRow].url)
    }

    /// Selects row `row` as a click would.
    func select(row: Int) {
        guard recents.indices.contains(row) else { return }
        table.selectRowIndexes([row], byExtendingSelection: false)
    }

    var selectedRow: Int { table.selectedRow }

    @objc private func newClicked() { onNew?() }
    @objc private func openClicked() { onOpen?() }
    @objc private func rowActivated() {
        guard table.clickedRow >= 0 else { return }
        select(row: table.clickedRow)
        openSelected()
    }
    @objc private func removeClicked() {
        guard let url = missingURL else { return }
        onForgetRecent?(url)
        showNotice(nil)
    }

    /// The table view's own row view for row `row`, for tests and drawing checks.
    func rowView(_ row: Int) -> NSTableRowView? { table.rowView(atRow: row, makeIfNecessary: false) }
    var recentsTable: NSTableView { table }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { recents.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard recents.indices.contains(row) else { return nil }
        let cell =
            (tableView.makeView(withIdentifier: NSUserInterfaceItemIdentifier("recentCell"), owner: nil) as? RecentCell)
            ?? RecentCell()
        cell.identifier = NSUserInterfaceItemIdentifier("recentCell")
        cell.configure(recents[row])
        cell.toolTip = recents[row].url.path
        return cell
    }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        let rowView = NSTableRowView()
        rowView.setAX(AX.launcherRecentPrefix + String(row))
        return rowView
    }
}
