// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// One stage as the Video page lists it.
struct StageRow: Equatable {
    var id: ModelID
    var name: String
    var width: Int
    var height: Int
    var keepBlack: Bool
    var layer: Int
    /// The display chosen on this machine (an id `list_displays` gives), and its name then.
    var display: String?
    var displayName: String?
    /// Why the stage cannot be seen now, in words; nil when it is shown or hidden between cues.
    var warning: String?
}

/// The Video page of the workspace settings (VID-01, VID-12; QLab 5 manual pp. 225-227): the
/// workspace's stages, one row each with its name (typed in place), pixel size (width and height
/// typed in place), the display it is
/// shown on on this machine, whether black stays up between cues, its layer, a button that opens its
/// monitor window (a live copy of the stage, QLab 5 manual p. 157) and a remove button; and a button
/// that adds one. A stage that cannot be seen says why in its row. Every control
/// sends its edit to the engine and the page is drawn from what the engine then holds (the model
/// for the stages, `video_outputs` for displays and states), so a refused edit shows the old state.
@MainActor
final class VideoSettingsPane: NSObject, NSTextFieldDelegate {
    let store: WorkspaceStore
    let view: NSStackView
    let addButton = NSButton(title: loc("settings.video.add"), target: nil, action: nil)
    /// The table, built afresh on each draw: an `NSGridView` cannot remove a row holding merged
    /// cells (a warning line), so rows are never removed from it.
    private(set) var grid = NSGridView()
    /// The column titles, above the scrolling rows so they stay in sight.
    let header = VideoSettingsPane.makeGrid(rows: false)
    let holder = FlippedView()
    let scroll = NSScrollView()
    /// The display and name columns' widths: what the table's width leaves after the other
    /// columns goes to the display choice first, then to the name.
    private(set) var displayWidth: CGFloat = VideoSettingsPane.leastDisplayWidth
    private(set) var nameWidth: CGFloat = VideoSettingsPane.columns[0]
    private var popupWidths: [NSLayoutConstraint] = []
    private var nameWidths: [NSLayoutConstraint] = []
    /// Counts edits tried, so an older edit's answer does not overwrite a newer one's reason.
    private var attempt = 0
    let emptyLabel = NSTextField(labelWithString: loc("settings.video.none"))
    let noVideoLabel = NSTextField(wrappingLabelWithString: loc("settings.video.noVideo"))
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// The rows as last drawn; the controls find their stage here by their tag.
    private(set) var shown: [StageRow]?
    private(set) var displays: [StageDisplay] = []
    private(set) var nameFields: [NSTextField] = []
    private(set) var layerFields: [NSTextField] = []
    private(set) var widthFields: [NSTextField] = []
    private(set) var heightFields: [NSTextField] = []
    private(set) var displayPopups: [NSPopUpButton] = []
    private(set) var blackBoxes: [NSButton] = []
    private(set) var monitorButtons: [NSButton] = []
    private(set) var removeButtons: [NSButton] = []
    private(set) var warningLabels: [NSTextField] = []
    private var subs: [AnyCancellable] = []
    /// Edits on their way to the engine, one after another.
    private var commits: Task<Void, Never>?

    static let id = "settings-video"

    init(store: WorkspaceStore) {
        self.store = store
        view = NSStackView()
        super.init()
        build()
        subs.append(store.$model.sink { [weak self] _ in self?.renderSoon() })
        subs.append(store.$videoOutputs.sink { [weak self] _ in self?.renderSoon() })
        scroll.contentView.postsFrameChangedNotifications = true
        NotificationCenter.default.publisher(for: NSView.frameDidChangeNotification, object: scroll.contentView)
            .sink { [weak self] _ in MainActor.assumeIsolated { self?.fitColumns() } }
            .store(in: &subs)
        render()
    }

    /// Reads the displays and states again; the page then follows the engine's pushes.
    func refresh() async {
        await store.refreshVideoOutputs()
        render()
    }

    // MARK: Building

    /// Name, pixels, display, black, layer, monitor, remove: the least widths (the display and name
    /// columns take more when there is room).
    static let columns: [CGFloat] = [90, sizeWidth, leastDisplayWidth, 32, 30, monitorWidth, 16]
    /// The monitor button's column.
    static let monitorWidth: CGFloat = 56
    /// A width or height field: five digits.
    static let sideWidth: CGFloat = 40
    /// The pixels column: width, "×", height.
    static let sizeWidth: CGFloat = 2 * sideWidth + 14
    static let leastDisplayWidth: CGFloat = 120
    static let mostDisplayWidth: CGFloat = 260
    static let columnSpacing: CGFloat = 6
    /// Between the table's edge and its rows.
    static let inset: CGFloat = 6

    private func build() {
        addButton.setAX(Self.id + "-add", label: loc("settings.video.add"))
        addButton.bezelStyle = .rounded
        addButton.target = self
        addButton.action = #selector(addClicked)
        let note = NSTextField(wrappingLabelWithString: loc("settings.video.note"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.preferredMaxLayoutWidth = 320
        let top = NSStackView(views: [addButton, note])
        top.orientation = .horizontal
        top.spacing = 12
        noVideoLabel.textColor = .systemOrange
        noVideoLabel.setAX(Self.id + "-no-video")
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.setAX(Self.id + "-empty")
        messageLabel.textColor = .systemRed
        messageLabel.setAX(Self.id + "-message")

        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        holder.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = holder
        install(Self.makeGrid(rows: true))
        NSLayoutConstraint.activate([
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            holder.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            scroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 300),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 160),
        ])
        scroll.setAX(Self.id + "-table", label: loc("settings.tab.video"))

        // The titles sit above the table's border, lined up with its columns.
        let titles = NSView()
        header.translatesAutoresizingMaskIntoConstraints = false
        titles.addSubview(header)
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: titles.topAnchor),
            header.bottomAnchor.constraint(equalTo: titles.bottomAnchor),
            header.leadingAnchor.constraint(equalTo: titles.leadingAnchor, constant: Self.inset + 1),
        ])
        for v in [top, noVideoLabel, titles, scroll, emptyLabel, messageLabel] as [NSView] {
            view.addArrangedSubview(v)
        }
        view.orientation = .vertical
        view.alignment = .leading
        view.distribution = .fill
        view.spacing = 10
        view.setCustomSpacing(4, after: titles)
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        // The table takes the page's width; the display column takes what the others leave.
        scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor).isActive = true
        titles.trailingAnchor.constraint(equalTo: view.trailingAnchor).isActive = true
        view.setAX(Self.id)
    }

    /// The column titles (`rows` false), or an empty table for the rows, with the column sizes.
    private static func makeGrid(rows: Bool) -> NSGridView {
        let grid: NSGridView
        if rows {
            grid = NSGridView(numberOfColumns: columns.count, rows: 0)
        } else {
            let titles = ["name", "size", "display", "black", "layer", "monitor", "remove"].map { key -> NSView in
                let label = NSTextField(labelWithString: loc("settings.video.col.\(key)"))
                label.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
                return label
            }
            grid = NSGridView(views: [titles])
        }
        grid.rowSpacing = 6
        grid.columnSpacing = columnSpacing
        for (c, width) in columns.enumerated() { grid.column(at: c).width = width }
        grid.yPlacement = .center
        grid.column(at: 3).xPlacement = .center
        grid.column(at: 5).xPlacement = .center
        grid.column(at: 6).xPlacement = .center
        return grid
    }

    /// Shares what the table's width leaves after the fixed columns: the display column first
    /// (so a display's name and "not connected" read whole), then the name, in the titles and the
    /// rows alike.
    func fitColumns() {
        let fixed = Self.columns.enumerated().filter { $0.offset != 0 && $0.offset != 2 }
            .reduce(0) { $0 + $1.element }
        let spare =
            (scroll.contentView.bounds.width - 2 * Self.inset - fixed
            - Self.columnSpacing * CGFloat(Self.columns.count - 1)).rounded(.down)
        let display = min(Self.mostDisplayWidth, max(Self.leastDisplayWidth, spare - Self.columns[0]))
        // The name takes all the rest, so a wide window shows a long name whole.
        let name = max(Self.columns[0], spare - display)
        displayWidth = display
        nameWidth = name
        for g in [header, grid] {
            g.column(at: 0).width = name
            g.column(at: 2).width = display
        }
        for c in popupWidths { c.constant = display }
        for c in nameWidths { c.constant = name }
    }

    /// Puts `new` in the old table's place (the old one goes with every view it holds).
    private func install(_ new: NSGridView) {
        grid.removeFromSuperview()
        grid = new
        new.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(new)
        NSLayoutConstraint.activate([
            new.topAnchor.constraint(equalTo: holder.topAnchor, constant: Self.inset),
            new.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: Self.inset),
            new.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -Self.inset),
            new.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -Self.inset),
        ])
    }

    // MARK: Drawing

    /// The stages as the engine has them, with this machine's displays and their states.
    func rows() -> [StageRow] {
        let outputs = store.videoOutputs
        return (store.model?.stages ?? []).map { s in
            let entry = outputs.stages.first { $0.stage == s.id }
            let raw = s.raw
            let name = raw["name"]?.stringValue ?? ""
            return StageRow(
                id: s.id, name: name, width: Int(raw["width"]?.doubleValue ?? 0),
                height: Int(raw["height"]?.doubleValue ?? 0), keepBlack: raw["keep_black"] == .bool(true),
                layer: Int(raw["layer"]?.doubleValue ?? 1), display: entry?.mapping?.display,
                displayName: entry?.mapping?.name, warning: entry?.state.flatMap(Self.warning))
        }
    }

    /// What a stage's state says to the operator; nil for a stage that is fine.
    static func warning(_ state: StageState) -> String? {
        switch state.state {
        case "no_display": return loc("settings.video.warn.noDisplay")
        case "display_missing": return loc("settings.video.warn.missing")
        case "failed": return String(format: loc("settings.video.warn.failed"), state.reason ?? "")
        default: return nil
        }
    }

    private func renderSoon() {
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.render() } }
    }

    /// Every field typed in: names, sizes and layers.
    private var textFields: [NSTextField] { nameFields + widthFields + heightFields + layerFields }

    /// Whether a name, size or layer is being typed in.
    var typing: Bool { textFields.contains { $0.currentEditor() != nil } }

    /// Ends the typing in a name or layer field, which commits it (before what the operator
    /// clicked next). True when typing ended: that edit's reason, if refused, must stand.
    @discardableResult
    private func endTyping() -> Bool {
        guard typing else { return false }
        view.window?.makeFirstResponder(nil)
        return true
    }

    /// Draws the page: always what the workspace and the engine hold now, while a name or layer
    /// is typed in too. That field is drawn again with what was typed and the caret where it was
    /// (unless its stage is gone); nothing typed is committed by the redraw.
    func render(forced: Bool = false) {
        let rows = rows()
        let list = store.videoOutputs.displays
        let enabled = store.model != nil
        addButton.isEnabled = enabled
        noVideoLabel.isHidden = store.videoOutputs.video != false
        emptyLabel.isHidden = !rows.isEmpty
        if !forced, shown == rows, displays == list { return }
        let resume = typedIn()
        // The old fields leave the window with the table: their end of editing is not an edit.
        for field in textFields { field.delegate = nil }
        shown = rows
        displays = list
        popupWidths = []
        nameWidths = []
        let grid = Self.makeGrid(rows: true)
        nameFields = []
        widthFields = []
        heightFields = []
        layerFields = []
        displayPopups = []
        blackBoxes = []
        monitorButtons = []
        removeButtons = []
        warningLabels = []
        for (i, row) in rows.enumerated() {
            let n = i + 1
            let name = NSTextField(string: row.name)
            name.controlSize = .small
            name.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            name.usesSingleLineMode = true
            name.lineBreakMode = .byTruncatingTail
            name.cell?.truncatesLastVisibleLine = true
            name.toolTip = row.name
            name.isEditable = enabled
            name.delegate = self
            name.tag = i
            name.identifier = NSUserInterfaceItemIdentifier("name")
            let nameWidth = name.widthAnchor.constraint(equalToConstant: self.nameWidth)
            nameWidth.isActive = true
            nameWidths.append(nameWidth)
            name.setAX("\(Self.id)-name-\(n)", label: "\(row.name), \(loc("settings.video.col.name"))")
            let width = sideField(row.width, kind: .width, tag: i, enabled: enabled)
            width.setAX("\(Self.id)-width-\(n)", label: "\(row.name), \(loc("settings.video.width"))")
            let height = sideField(row.height, kind: .height, tag: i, enabled: enabled)
            height.setAX("\(Self.id)-height-\(n)", label: "\(row.name), \(loc("settings.video.height"))")
            let by = NSTextField(labelWithString: "×")
            by.font = .systemFont(ofSize: 11)
            by.textColor = .secondaryLabelColor
            let size = NSStackView(views: [width, by, height])
            size.orientation = .horizontal
            size.spacing = 2
            size.setAX("\(Self.id)-size-\(n)")
            widthFields.append(width)
            heightFields.append(height)
            let popup = displayPopup(row, tag: i, enabled: enabled)
            popup.setAX("\(Self.id)-display-\(n)", label: "\(row.name), \(loc("settings.video.col.display"))")
            let black = NSButton(checkboxWithTitle: "", target: self, action: #selector(blackClicked(_:)))
            black.controlSize = .small
            black.state = row.keepBlack ? .on : .off
            black.tag = i
            black.isEnabled = enabled
            black.setAX("\(Self.id)-black-\(n)", label: "\(row.name), \(loc("settings.video.black"))")
            let layer = NSTextField(string: String(row.layer))
            layer.controlSize = .small
            layer.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
            layer.isEditable = enabled
            layer.alignment = .right
            layer.delegate = self
            layer.tag = i
            layer.identifier = NSUserInterfaceItemIdentifier("layer")
            layer.widthAnchor.constraint(equalToConstant: Self.columns[4]).isActive = true
            layer.setAX("\(Self.id)-layer-\(n)", label: "\(row.name), \(loc("settings.video.col.layer"))")
            let monitor = NSButton(
                title: loc("settings.video.monitor"), target: self, action: #selector(monitorClicked(_:)))
            monitor.controlSize = .small
            monitor.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            monitor.bezelStyle = .rounded
            monitor.tag = i
            monitor.isEnabled = enabled && store.videoOutputs.video != false
            monitor.toolTip = loc("settings.video.monitor.tip")
            monitor.setAX("\(Self.id)-monitor-\(n)", label: "\(row.name), \(loc("settings.video.monitor.ax"))")
            let remove = NSButton(title: "", target: self, action: #selector(removeClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.video.remove"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.tag = i
            remove.isEnabled = enabled
            remove.setAX("\(Self.id)-remove-\(n)", label: "\(loc("settings.video.remove")) \(row.name)")
            grid.addRow(with: [name, size, popup, black, layer, monitor, remove])
            nameFields.append(name)
            layerFields.append(layer)
            displayPopups.append(popup)
            blackBoxes.append(black)
            monitorButtons.append(monitor)
            removeButtons.append(remove)
            let warning = NSTextField(wrappingLabelWithString: row.warning ?? "")
            warning.textColor = .systemOrange
            warning.font = .systemFont(ofSize: 11)
            warning.setAX("\(Self.id)-warning-\(n)")
            warningLabels.append(warning)
            if row.warning != nil {
                let r = grid.addRow(with: [warning])
                r.mergeCells(in: NSRange(location: 0, length: Self.columns.count))
            }
        }
        install(grid)
        fitColumns()
        grid.layoutSubtreeIfNeeded()
        if let resume { self.resume(resume) }
    }

    /// A width or height field of the size column.
    private func sideField(_ value: Int, kind: TypedField, tag: Int, enabled: Bool) -> NSTextField {
        let field = NSTextField(string: String(value))
        field.controlSize = .small
        field.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        field.isEditable = enabled
        field.alignment = .right
        field.delegate = self
        field.tag = tag
        field.identifier = NSUserInterfaceItemIdentifier(kind.rawValue)
        field.widthAnchor.constraint(equalToConstant: Self.sideWidth).isActive = true
        return field
    }

    /// The fields typed in place, by their identifier.
    enum TypedField: String { case name, width, height, layer }

    private func fields(_ kind: TypedField) -> [NSTextField] {
        switch kind {
        case .name: return nameFields
        case .width: return widthFields
        case .height: return heightFields
        case .layer: return layerFields
        }
    }

    /// What the workspace holds for `kind` of `row`, as its field shows it.
    private static func text(_ kind: TypedField, _ row: StageRow) -> String {
        switch kind {
        case .name: return row.name
        case .width: return String(row.width)
        case .height: return String(row.height)
        case .layer: return String(row.layer)
        }
    }

    /// The field being typed in: its kind, its stage, the text so far and the selection.
    private struct Typed {
        var kind: TypedField
        var stage: ModelID
        var text: String?
        var selection: NSRange
    }

    /// The field holding the caret, and what was typed in it: nil text when it holds what the
    /// page last drew there (a value already committed, as after Return), so a redraw shows the
    /// workspace's value (an undo's, another client's), never a stale one a later click would send.
    private func typedIn() -> Typed? {
        guard let field = textFields.first(where: { $0.currentEditor() != nil }),
            let editor = field.currentEditor(), let row = row(field.tag),
            let kind = field.identifier.flatMap({ TypedField(rawValue: $0.rawValue) })
        else { return nil }
        let typed = editor.string == Self.text(kind, row) ? nil : editor.string
        return Typed(kind: kind, stage: row.id, text: typed, selection: editor.selectedRange)
    }

    /// Puts the caret back in the redrawn field of the stage that was being typed in, with what
    /// was typed, or the workspace's value when nothing new was.
    private func resume(_ typed: Typed) {
        guard let i = shown?.firstIndex(where: { $0.id == typed.stage }) else { return }
        let field = fields(typed.kind)[i]
        guard view.window?.makeFirstResponder(field) == true, let editor = field.currentEditor() else { return }
        let text = typed.text ?? field.stringValue
        editor.string = text
        let length = (text as NSString).length
        let start = min(typed.selection.location, length)
        editor.selectedRange = NSRange(location: start, length: min(typed.selection.length, length - start))
    }

    /// "None", then the displays; a chosen display that is not connected stays listed, marked.
    private func displayPopup(_ row: StageRow, tag: Int, enabled: Bool) -> NSPopUpButton {
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        popup.controlSize = .small
        popup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        popup.addItem(withTitle: loc("settings.video.displayNone"))
        popup.lastItem?.representedObject = nil
        for d in displays {
            popup.addItem(withTitle: String(format: loc("settings.video.displayItem"), d.name, d.width, d.height))
            popup.lastItem?.representedObject = d.id
        }
        if let chosen = row.display {
            if let i = displays.firstIndex(where: { $0.id == chosen }) {
                popup.selectItem(at: i + 1)
            } else {
                popup.addItem(
                    withTitle: String(format: loc("settings.video.displayAbsent"), row.displayName ?? chosen))
                popup.lastItem?.representedObject = chosen
                popup.selectItem(at: popup.numberOfItems - 1)
            }
        } else {
            popup.selectItem(at: 0)
        }
        popup.tag = tag
        popup.target = self
        popup.action = #selector(displayChosen(_:))
        popup.isEnabled = enabled
        popup.toolTip = popup.titleOfSelectedItem
        let width = popup.widthAnchor.constraint(equalToConstant: displayWidth)
        width.isActive = true
        popupWidths.append(width)
        return popup
    }

    // MARK: Editing

    private func row(_ tag: Int) -> StageRow? {
        guard let shown, shown.indices.contains(tag) else { return nil }
        return shown[tag]
    }

    /// Shows why a value was refused, the same way for every refusal, the page's or the
    /// engine's (empty: none).
    private func tell(_ reason: String) {
        attempt += 1
        messageLabel.stringValue = reason
    }

    /// Runs `edit` for `stage` after the edits before it, shows its refusal (or clears the last
    /// one, unless a newer edit has said something since), and redraws from the engine.
    /// A refused edit of a typed `field` puts its value back. `after` is
    /// true for a click that ended typing: the typed edit's reason (shown now, or on its way from
    /// the engine) is not cleared or outrun by the click's edit.
    private func commit(
        _ stage: ModelID?, field: TypedField? = nil, after typed: Bool = false,
        _ edit: @escaping @MainActor () async -> Result<OkBody, EngineError>
    ) {
        if !typed { tell("") }
        let mine = attempt
        let previous = commits
        commits = Task { [weak self] in
            await previous?.value
            let result = await edit()
            guard let self else { return }
            await refresh()
            if case .failure(let e) = result {
                if let stage, let field { putBack(stage, field) }
                if mine == attempt { messageLabel.stringValue = reason(e, stage) }
            }
            render(forced: true)
        }
    }

    /// Puts the stage's value of `kind` as the workspace has it back in its field (the one drawn
    /// now), also while it is typed in.
    private func putBack(_ stage: ModelID, _ kind: TypedField) {
        guard let i = shown?.firstIndex(where: { $0.id == stage }), let row = rows().first(where: { $0.id == stage })
        else { return }
        let field = fields(kind)[i]
        let value = Self.text(kind, row)
        field.stringValue = value
        field.currentEditor()?.string = value
    }

    /// A refusal in words, never with an internal id: a stage gone since it was drawn says so.
    private func reason(_ e: EngineError, _ stage: ModelID?) -> String {
        if let stage, store.model?.stages.contains(where: { $0.id == stage }) != true {
            return loc("settings.video.gone")
        }
        if let stage, let cue = Self.cueUsing(e), let name = rows().first(where: { $0.id == stage })?.name {
            return String(format: loc("settings.video.inUse"), name, cue)
        }
        return e.detailText
    }

    /// The cue the engine names when it refuses to remove a stage a cue uses (docs/protocol.md,
    /// `remove_stage`: `stage "<name>" cannot be removed while cue "<cue>" uses it`).
    static func cueUsing(_ e: EngineError) -> String? {
        guard case .engine(_, let text) = e, text.hasSuffix("\" uses it"),
            let start = text.range(of: "while cue \"", options: .backwards)
        else { return nil }
        return String(text[start.upperBound..<text.index(text.endIndex, offsetBy: -"\" uses it".count)])
    }

    /// The edits still on their way (tests wait for them).
    var pending: Task<Void, Never>? { commits }

    @objc private func addClicked() {
        let typed = endTyping()
        let count = store.model?.stages.count ?? 0
        let id = ModelID(UUID().uuidString.lowercased())
        let name = String(format: loc("settings.video.newName"), count + 1)
        commit(nil, after: typed) { [store] in
            await store.send(command: .addStage(id: id, name: name, width: 1920, height: 1080, index: count))
        }
    }

    @objc private func removeClicked(_ sender: NSButton) {
        let typed = endTyping()
        guard let row = row(sender.tag) else { return }
        commit(row.id, after: typed) { [store] in await store.send(command: .removeStage(row.id)) }
    }

    @objc private func blackClicked(_ sender: NSButton) {
        let on = sender.state == .on
        let typed = endTyping()
        guard let row = row(sender.tag) else { return }
        commit(row.id, after: typed) { [store] in
            await store.send(
                command: .setStage(row.id, .object(["type": .string("keep_black"), "value": .bool(on)])))
        }
    }

    /// Opens the stage's monitor window, or brings it to the front when it is open (the stage host
    /// draws it; nothing on this page changes).
    @objc private func monitorClicked(_ sender: NSButton) {
        let typed = endTyping()
        guard let row = row(sender.tag) else { return }
        commit(row.id, after: typed) { [store] in await store.setStageMonitor(row.id, open: true) }
    }

    @objc private func displayChosen(_ sender: NSPopUpButton) {
        let display = sender.selectedItem?.representedObject as? String
        let typed = endTyping()
        guard let row = row(sender.tag) else { return }
        guard display != row.display else { return }
        commit(row.id, after: typed) { [store] in await store.setStageMapping(row.id, display: display) }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField, let row = row(field.tag) else { return }
        let text = field.stringValue
        let kind = field.identifier.flatMap { TypedField(rawValue: $0.rawValue) }
        if kind == .width || kind == .height {
            // A stage is 1 to 16384 pixels a side (QLab 5 manual p. 226).
            guard let side = Int(text.trimmingCharacters(in: .whitespaces)), (1...16384).contains(side) else {
                tell(loc("settings.video.badSize"))
                field.stringValue = kind == .width ? String(row.width) : String(row.height)
                return
            }
            let (width, height) = kind == .width ? (side, row.height) : (row.width, side)
            guard (width, height) != (row.width, row.height) else { return }
            commit(row.id, field: kind) { [store] in
                await store.send(
                    command: .setStage(
                        row.id,
                        .object([
                            "type": .string("size"), "width": .int(Int64(width)), "height": .int(Int64(height)),
                        ])))
            }
        } else if kind == .layer {
            guard let layer = Int(text.trimmingCharacters(in: .whitespaces)), (0...1000).contains(layer) else {
                tell(loc("settings.video.badLayer"))
                field.stringValue = String(row.layer)
                return
            }
            guard layer != row.layer else { return }
            commit(row.id, field: .layer) { [store] in
                await store.send(
                    command: .setStage(row.id, .object(["type": .string("layer"), "value": .int(Int64(layer))])))
            }
        } else {
            guard text != row.name else { return }
            guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                tell(loc("settings.video.badName"))
                field.stringValue = row.name
                return
            }
            commit(row.id, field: .name) { [store] in
                await store.send(
                    command: .setStage(row.id, .object(["type": .string("name"), "value": .string(text)])))
            }
        }
    }
}
