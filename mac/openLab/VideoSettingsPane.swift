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
/// workspace's stages, one row each with its name (typed in place), pixel size, the display it is
/// shown on on this machine, whether black stays up between cues, its layer, and a remove button;
/// and a button that adds one. A stage that cannot be seen says why in its row. Every control
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
    let holder = TopLeftView()
    let emptyLabel = NSTextField(labelWithString: loc("settings.video.none"))
    let noVideoLabel = NSTextField(wrappingLabelWithString: loc("settings.video.noVideo"))
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// The rows as last drawn; the controls find their stage here by their tag.
    private(set) var shown: [StageRow]?
    private(set) var displays: [StageDisplay] = []
    private(set) var nameFields: [NSTextField] = []
    private(set) var layerFields: [NSTextField] = []
    private(set) var displayPopups: [NSPopUpButton] = []
    private(set) var blackBoxes: [NSButton] = []
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
        render()
    }

    /// Reads the displays and states again; the page then follows the engine's pushes.
    func refresh() async {
        await store.refreshVideoOutputs()
        render()
    }

    // MARK: Building

    /// Name, pixels, display, black, layer, remove.
    static let columns: [CGFloat] = [110, 76, 140, 36, 40, 18]

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

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        holder.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = holder
        install(Self.makeGrid())
        NSLayoutConstraint.activate([
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            holder.widthAnchor.constraint(greaterThanOrEqualTo: scroll.contentView.widthAnchor),
            scroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 300),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 160),
        ])
        scroll.setAX(Self.id + "-table", label: loc("settings.tab.video"))

        for v in [top, noVideoLabel, scroll, emptyLabel, messageLabel] as [NSView] { view.addArrangedSubview(v) }
        view.orientation = .vertical
        view.alignment = .leading
        view.distribution = .fill
        view.spacing = 10
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        // The table takes the page's width (a narrow window scrolls it rather than clipping a column).
        scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor).isActive = true
        view.setAX(Self.id)
    }

    /// An empty table: the header row and the column sizes.
    private static func makeGrid() -> NSGridView {
        let headers = ["name", "size", "display", "black", "layer", "remove"].map { key -> NSView in
            let label = NSTextField(labelWithString: loc("settings.video.col.\(key)"))
            label.font = .systemFont(ofSize: 12, weight: .semibold)
            return label
        }
        let grid = NSGridView(views: [headers])
        grid.rowSpacing = 6
        grid.columnSpacing = 6
        // Sized so the whole table fits the settings window at its least width.
        for (c, width) in columns.enumerated() { grid.column(at: c).width = width }
        grid.yPlacement = .center
        grid.column(at: 3).xPlacement = .center
        grid.column(at: 5).xPlacement = .center
        return grid
    }

    /// Puts `new` in the old table's place (the old one goes with every view it holds).
    private func install(_ new: NSGridView) {
        grid.removeFromSuperview()
        grid = new
        new.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(new)
        NSLayoutConstraint.activate([
            new.topAnchor.constraint(equalTo: holder.topAnchor, constant: 6),
            new.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 6),
            new.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -6),
            new.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -6),
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

    /// Draws the page. One that is as drawn is left alone, and so is one being typed in: a rebuild
    /// would take the caret away (the next render catches up).
    func render(forced: Bool = false) {
        let rows = rows()
        let list = store.videoOutputs.displays
        let enabled = store.model != nil
        addButton.isEnabled = enabled
        noVideoLabel.isHidden = store.videoOutputs.video != false
        emptyLabel.isHidden = !rows.isEmpty
        if !forced, shown == rows, displays == list { return }
        if (nameFields + layerFields).contains(where: { $0.currentEditor() != nil }) { return }
        shown = rows
        displays = list
        let grid = Self.makeGrid()
        nameFields = []
        layerFields = []
        displayPopups = []
        blackBoxes = []
        removeButtons = []
        warningLabels = []
        for (i, row) in rows.enumerated() {
            let n = i + 1
            let name = NSTextField(string: row.name)
            name.isEditable = enabled
            name.delegate = self
            name.tag = i
            name.identifier = NSUserInterfaceItemIdentifier("name")
            name.widthAnchor.constraint(equalToConstant: Self.columns[0]).isActive = true
            name.setAX("\(Self.id)-name-\(n)", label: "\(row.name), \(loc("settings.video.col.name"))")
            let size = NSTextField(labelWithString: "\(row.width) × \(row.height)")
            size.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
            size.setAX("\(Self.id)-size-\(n)")
            let popup = displayPopup(row, tag: i, enabled: enabled)
            popup.setAX("\(Self.id)-display-\(n)", label: "\(row.name), \(loc("settings.video.col.display"))")
            let black = NSButton(checkboxWithTitle: "", target: self, action: #selector(blackClicked(_:)))
            black.state = row.keepBlack ? .on : .off
            black.tag = i
            black.isEnabled = enabled
            black.setAX("\(Self.id)-black-\(n)", label: "\(row.name), \(loc("settings.video.black"))")
            let layer = NSTextField(string: String(row.layer))
            layer.isEditable = enabled
            layer.alignment = .right
            layer.delegate = self
            layer.tag = i
            layer.identifier = NSUserInterfaceItemIdentifier("layer")
            layer.widthAnchor.constraint(equalToConstant: Self.columns[4]).isActive = true
            layer.setAX("\(Self.id)-layer-\(n)", label: "\(row.name), \(loc("settings.video.col.layer"))")
            let remove = NSButton(title: "", target: self, action: #selector(removeClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.video.remove"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.tag = i
            remove.isEnabled = enabled
            remove.setAX("\(Self.id)-remove-\(n)", label: "\(loc("settings.video.remove")) \(row.name)")
            grid.addRow(with: [name, size, popup, black, layer, remove])
            nameFields.append(name)
            layerFields.append(layer)
            displayPopups.append(popup)
            blackBoxes.append(black)
            removeButtons.append(remove)
            let warning = NSTextField(wrappingLabelWithString: row.warning ?? "")
            warning.textColor = .systemOrange
            warning.font = .systemFont(ofSize: 11)
            warning.setAX("\(Self.id)-warning-\(n)")
            warningLabels.append(warning)
            if row.warning != nil {
                let r = grid.addRow(with: [warning])
                r.mergeCells(in: NSRange(location: 0, length: 6))
            }
        }
        install(grid)
        grid.layoutSubtreeIfNeeded()
    }

    /// "None", then the displays; a chosen display that is not connected stays listed, marked.
    private func displayPopup(_ row: StageRow, tag: Int, enabled: Bool) -> NSPopUpButton {
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
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
        popup.widthAnchor.constraint(equalToConstant: Self.columns[2]).isActive = true
        return popup
    }

    // MARK: Editing

    private func row(_ tag: Int) -> StageRow? {
        guard let shown, shown.indices.contains(tag) else { return nil }
        return shown[tag]
    }

    /// Runs `edit` after the edits before it, shows its refusal (or clears the last one), and
    /// redraws from the engine.
    private func commit(_ edit: @escaping @MainActor () async -> Result<OkBody, EngineError>) {
        let previous = commits
        commits = Task { [weak self] in
            await previous?.value
            let result = await edit()
            guard let self else { return }
            switch result {
            case .success: messageLabel.stringValue = ""
            case .failure(let e): messageLabel.stringValue = e.detailText
            }
            await refresh()
            render(forced: true)
        }
    }

    /// The edits still on their way (tests wait for them).
    var pending: Task<Void, Never>? { commits }

    @objc private func addClicked() {
        let count = store.model?.stages.count ?? 0
        let id = ModelID(UUID().uuidString.lowercased())
        let name = String(format: loc("settings.video.newName"), count + 1)
        commit { [store] in
            await store.send(command: .addStage(id: id, name: name, width: 1920, height: 1080, index: count))
        }
    }

    @objc private func removeClicked(_ sender: NSButton) {
        guard let row = row(sender.tag) else { return }
        commit { [store] in await store.send(command: .removeStage(row.id)) }
    }

    @objc private func blackClicked(_ sender: NSButton) {
        guard let row = row(sender.tag) else { return }
        let on = sender.state == .on
        commit { [store] in
            await store.send(
                command: .setStage(row.id, .object(["type": .string("keep_black"), "value": .bool(on)])))
        }
    }

    @objc private func displayChosen(_ sender: NSPopUpButton) {
        guard let row = row(sender.tag) else { return }
        let display = sender.selectedItem?.representedObject as? String
        guard display != row.display else { return }
        commit { [store] in await store.setStageMapping(row.id, display: display) }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField, let row = row(field.tag) else { return }
        let text = field.stringValue
        if field.identifier?.rawValue == "layer" {
            guard let layer = Int(text.trimmingCharacters(in: .whitespaces)), (0...1000).contains(layer) else {
                messageLabel.stringValue = loc("settings.video.badLayer")
                field.stringValue = String(row.layer)
                return
            }
            guard layer != row.layer else { return }
            commit { [store] in
                await store.send(
                    command: .setStage(row.id, .object(["type": .string("layer"), "value": .int(Int64(layer))])))
            }
        } else {
            guard text != row.name else { return }
            commit { [store] in
                await store.send(
                    command: .setStage(row.id, .object(["type": .string("name"), "value": .string(text)])))
            }
        }
    }
}
