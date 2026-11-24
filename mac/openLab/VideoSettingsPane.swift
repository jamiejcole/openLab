// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// One stage as the Video Outputs page lists it, or one output route as the Output Routing page
/// does.
struct StageRow: Equatable {
    var id: ModelID
    var name: String
    var width: Int
    var height: Int
    /// A stage's: the displays it reaches through its regions' routes, in route order.
    var devices: [String] = []
    /// A route's scaling (`centre`, `fit`, `stretch`, `fill`).
    var scaling: String = "fill"
    /// A route's display chosen on this machine (an id `list_displays` gives), and its name then.
    var display: String?
    var displayName: String?
    /// Why the row's route (or every route of the stage) cannot be seen now, in words; nil when
    /// it is shown or shows nothing between cues.
    var warning: String?
}

/// The Video pages of the workspace settings (VID-01, VID-12; QLab 5 manual pp. 225-234), one
/// `VideoSettingsPane` per section as QLab 5 splits them (UI map `settings.video.*`), only for what
/// openLab has:
/// - Video Outputs: the workspace's stages, one row each with its name (typed in place), its pixel
///   size, the displays it reaches through its regions' output routes, and buttons that open its
///   editor (`StageEditorWindowController`) and its monitor window (a live copy of the stage, QLab 5
///   manual p. 157) and remove it; above them a pop-up that adds an empty stage or one on a route.
/// - Output Routing: the workspace's output routes, one row each with a handle that drags it to
///   another place, its name (typed in place), its size, the display it is shown on on this machine
///   (QLab 5 manual p. 232), its scaling, a button that opens its editor (`RouteEditorController`)
///   and one that removes it; a route that cannot be seen says why under its row. Above them a
///   pop-up that adds a route (on a display, or on none) and a button that opens the Mac's
///   Displays settings.
/// - Output Devices: the displays this computer has, one row each: Device N, its name, its size in
///   pixels and its refresh rate.
/// Every control sends its edit to the engine and a page is drawn from what the engine then holds
/// (the model for the stages and routes, `video_outputs` for displays and states), so a refused
/// edit shows the old state.
@MainActor
final class VideoSettingsPane: NSObject, NSTextFieldDelegate {
    /// The sections QLab 5's Video settings are split into (no Video Inputs: openLab has no camera cues).
    enum Section: Equatable { case outputs, routing, devices }

    /// A column of a section's table.
    enum Col: Equatable {
        case handle, name, size, display, scaling, devices, edit, monitor, remove, device, resolution, rate

        /// The key of its title (`settings.video.col.<key>`).
        var key: String {
            switch self {
            case .handle: return "handle"
            case .name: return "name"
            case .size: return "size"
            case .display: return "display"
            case .scaling: return "scaling"
            case .devices: return "devices"
            case .edit: return "edit"
            case .monitor: return "monitor"
            case .remove: return "remove"
            case .device: return "device"
            case .resolution: return "resolution"
            case .rate: return "rate"
            }
        }

        var width: CGFloat {
            switch self {
            case .handle: return 12
            case .name: return 90
            case .size: return 72
            case .display: return VideoSettingsPane.leastDisplayWidth
            case .scaling: return 62
            case .devices: return VideoSettingsPane.leastDisplayWidth
            case .edit: return 34
            case .monitor: return VideoSettingsPane.monitorWidth
            case .remove: return 16
            case .device: return 70
            case .resolution: return 100
            case .rate: return 80
            }
        }
    }

    /// A route's scalings, in the order its pop-ups list them (QLab 5 manual p. 233).
    static let scalings = ["centre", "fit", "stretch", "fill"]

    let section: Section
    let store: WorkspaceStore
    let view: PageStack
    /// Adds a stage (empty, or on a route) or a route (on a display, or on none).
    let addPopup = NSPopUpButton(frame: .zero, pullsDown: true)
    /// Output Routing: opens the Mac's Displays settings.
    let displaysButton = NSButton(title: loc("settings.video.displaysButton"), target: nil, action: nil)
    /// The table, built afresh on each draw: an `NSGridView` cannot remove a row holding merged
    /// cells (a warning line), so rows are never removed from it.
    private(set) var grid = NSGridView()
    /// The column titles, above the scrolling rows so they stay in sight.
    let header: NSGridView
    let holder = FlippedView()
    let scroll = NSScrollView()
    /// The flexible columns' widths: what the table's width leaves after the other columns goes
    /// to the display (or devices) column first, then to the name.
    private(set) var displayWidth: CGFloat = VideoSettingsPane.leastDisplayWidth
    private(set) var nameWidth: CGFloat = Col.name.width
    private var wideWidths: [NSLayoutConstraint] = []
    /// Each name cell's width constraint and what the cell holds beside it (a route's icon).
    private var nameWidths: [(NSLayoutConstraint, CGFloat)] = []
    /// Counts edits tried, so an older edit's answer does not overwrite a newer one's reason.
    private var attempt = 0
    let emptyLabel: NSTextField
    let noVideoLabel = NSTextField(wrappingLabelWithString: loc("settings.video.noVideo"))
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// The rows as last drawn; the controls find their stage or route here by their tag.
    private(set) var shown: [StageRow]?
    private(set) var displays: [StageDisplay] = []
    /// The add pop-up's items as last drawn.
    private var shownAddItems: [String] = []
    private(set) var nameFields: [NSTextField] = []
    private(set) var sizeLabels: [NSTextField] = []
    private(set) var deviceCells: [NSTextField] = []
    private(set) var displayPopups: [NSPopUpButton] = []
    private(set) var scalingPopups: [NSPopUpButton] = []
    private(set) var handles: [RowHandle] = []
    private(set) var editButtons: [NSButton] = []
    private(set) var monitorButtons: [NSButton] = []
    private(set) var removeButtons: [NSButton] = []
    private(set) var warningLabels: [NSTextField] = []
    /// The Output Devices rows: each display's number, name, size and refresh rate.
    private(set) var deviceLabels: [[NSTextField]] = []
    /// The line drawn where a dragged route would go.
    let dropLine = NSBox()
    private var subs: [AnyCancellable] = []
    /// Edits on their way to the engine, one after another.
    private var commits: Task<Void, Never>?

    /// Opens a stage's or a route's editor (the settings window owns them).
    var onEditStage: ((ModelID) -> Void)?
    var onEditRoute: ((ModelID) -> Void)?
    /// Opens a URL outside the app (the Displays settings); tests give their own.
    var openURL: (URL) -> Void = { NSWorkspace.shared.open($0) }
    /// Opens the add pop-up's menu, as Command-N on the page does (tests give their own: a menu
    /// opened by code waits for real input).
    var openAddMenu: ((NSPopUpButton) -> Void)?

    static let id = "settings-video"
    /// macOS's Displays settings.
    static let displaysSettings = URL(string: "x-apple.systempreferences:com.apple.preference.displays")

    /// The columns of `section`'s table, left to right.
    static func columns(of section: Section) -> [Col] {
        switch section {
        case .outputs: return [.name, .size, .devices, .edit, .monitor, .remove]
        case .routing: return [.handle, .name, .size, .display, .scaling, .edit, .remove]
        case .devices: return [.device, .name, .resolution, .rate]
        }
    }

    var cols: [Col] { Self.columns(of: section) }

    init(store: WorkspaceStore, section: Section = .outputs) {
        self.store = store
        self.section = section
        view = PageStack()
        header = Self.makeGrid(rows: false, cols: Self.columns(of: section))
        emptyLabel = NSTextField(
            labelWithString: loc(
                section == .devices
                    ? "settings.video.noDisplays"
                    : section == .routing ? "settings.video.noRoutes" : "settings.video.none"))
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

    /// The monitor button's column.
    nonisolated static let monitorWidth: CGFloat = 56
    nonisolated static let leastDisplayWidth: CGFloat = 120
    static let mostDisplayWidth: CGFloat = 260
    static let columnSpacing: CGFloat = 5
    /// Between the table's edge and its rows.
    static let inset: CGFloat = 6
    /// A route's icon and the gap after it, in its name cell.
    static let iconRoom: CGFloat = 20

    private func build() {
        addPopup.setAX(Self.id + "-add", label: loc(section == .routing ? "settings.video.newRoute" : "settings.video.add"))
        addPopup.controlSize = .small
        addPopup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        addPopup.target = self
        addPopup.action = #selector(addChosen(_:))
        addPopup.setContentHuggingPriority(.required, for: .horizontal)
        displaysButton.setAX(Self.id + "-displays-settings")
        displaysButton.controlSize = .small
        displaysButton.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        displaysButton.bezelStyle = .rounded
        displaysButton.toolTip = loc("settings.video.displaysButton.tip")
        displaysButton.target = self
        displaysButton.action = #selector(displaysClicked)
        let note = NSTextField(wrappingLabelWithString: loc("settings.video.note.\(noteKey)"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.preferredMaxLayoutWidth = 320
        note.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        note.setAX("\(Self.id)-\(noteKey)-note")
        var topViews: [NSView] = section == .devices ? [note] : [addPopup, note]
        if section == .routing {
            let spring = NSView()
            spring.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            topViews += [spring, displaysButton]
        }
        let top = NSStackView(views: topViews)
        top.orientation = .horizontal
        top.spacing = 12
        noVideoLabel.textColor = .systemOrange
        noVideoLabel.setAX(Self.id + "-no-video")
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.setAX(Self.id + "-empty")
        messageLabel.textColor = .systemRed
        messageLabel.setAX(Self.id + "-message")

        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        holder.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = holder
        install(Self.makeGrid(rows: true, cols: cols))
        dropLine.boxType = .custom
        dropLine.fillColor = .controlAccentColor
        dropLine.borderWidth = 0
        dropLine.isHidden = true
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
        top.trailingAnchor.constraint(equalTo: view.trailingAnchor).isActive = true
        view.setAX(section == .outputs ? Self.id : "\(Self.id)-\(noteKey)")
        // Command-N on the page opens its add pop-up (QLab 5 manual pp. 226, 233).
        if section != .devices {
            view.onCommandN = { [weak self] in
                guard let self, self.addPopup.isEnabled else { return false }
                if let open = self.openAddMenu { open(self.addPopup) } else { self.addPopup.performClick(nil) }
                return true
            }
        }
    }

    /// The section's name in identifiers and the key of its note.
    private var noteKey: String {
        switch section {
        case .outputs: return "outputs"
        case .routing: return "routing"
        case .devices: return "devices"
        }
    }

    /// The column titles (`rows` false), or an empty table for the rows, with the column sizes.
    private static func makeGrid(rows: Bool, cols: [Col]) -> NSGridView {
        let grid: NSGridView
        if rows {
            grid = NSGridView(numberOfColumns: cols.count, rows: 0)
        } else {
            let titles = cols.map { col -> NSView in
                let label = NSTextField(labelWithString: loc("settings.video.col.\(col.key)"))
                label.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
                return label
            }
            grid = NSGridView(views: [titles])
        }
        grid.rowSpacing = 6
        grid.columnSpacing = columnSpacing
        for (c, col) in cols.enumerated() {
            grid.column(at: c).width = col.width
            if [.edit, .monitor, .remove, .handle].contains(col) { grid.column(at: c).xPlacement = .center }
        }
        grid.yPlacement = .center
        return grid
    }

    /// The table's second flexible column after the name: the display choice, or the displays a
    /// stage reaches.
    private var wideIndex: Int? { cols.firstIndex { $0 == .display || $0 == .devices } }

    /// The name's column: the table's first flexible one.
    private var nameIndex: Int { cols.firstIndex { $0 == .name } ?? 0 }

    /// The name column's least width: a route's (icon and a short name) gives up a little to the
    /// display beside it, so a display's name reads whole at the window's least size.
    private var leastName: CGFloat { section == .routing ? 74 : Col.name.width }

    /// Shares what the table's width leaves after the fixed columns: the display column first
    /// (so a display's name and "not connected" read whole), then the name, in the titles and the
    /// rows alike.
    func fitColumns() {
        let wide = wideIndex
        let nameIndex = nameIndex
        let fixed = cols.enumerated().filter { $0.offset != nameIndex && $0.offset != wide }
            .reduce(0) { $0 + $1.element.width }
        let spare =
            (scroll.contentView.bounds.width - 2 * Self.inset - fixed
            - Self.columnSpacing * CGFloat(cols.count - 1)).rounded(.down)
        let display: CGFloat
        let name: CGFloat
        if wide != nil {
            display = min(Self.mostDisplayWidth, max(Self.leastDisplayWidth, spare - leastName))
            // The name takes all the rest, so a wide window shows a long name whole.
            name = max(leastName, spare - display)
        } else {
            display = Self.leastDisplayWidth
            name = max(leastName, spare)
        }
        displayWidth = display
        nameWidth = name
        for g in [header, grid] {
            g.column(at: nameIndex).width = name
            if let wide { g.column(at: wide).width = display }
        }
        for c in wideWidths { c.constant = display }
        for (c, beside) in nameWidths { c.constant = name - beside }
    }

    /// Puts `new` in the old table's place (the old one goes with every view it holds).
    private func install(_ new: NSGridView) {
        grid.removeFromSuperview()
        grid = new
        new.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(new, positioned: .below, relativeTo: nil)
        NSLayoutConstraint.activate([
            new.topAnchor.constraint(equalTo: holder.topAnchor, constant: Self.inset),
            new.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: Self.inset),
            new.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -Self.inset),
        ])
        // Not required: the columns are fitted to the page's width, and a table that is
        // momentarily wider must not hold the window wide (nor outweigh the page's own sizing).
        let trailing = new.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -Self.inset)
        trailing.priority = NSLayoutConstraint.Priority(250)
        trailing.isActive = true
    }

    // MARK: Drawing

    /// The stages (Video Outputs) or routes (Output Routing) as the engine has them, a route with
    /// its display on this machine and its state, a stage with the displays it reaches.
    func rows() -> [StageRow] {
        let outputs = store.videoOutputs
        let routes = store.model?.routes ?? []
        if section == .routing {
            return routes.map { r in
                let entry = outputs.route(r.id)
                return StageRow(
                    id: r.id, name: r.raw["name"]?.stringValue ?? "", width: Int(r.raw["width"]?.doubleValue ?? 0),
                    height: Int(r.raw["height"]?.doubleValue ?? 0),
                    scaling: r.raw["scaling"]?.stringValue ?? "fill",
                    display: entry?.mapping?.display, displayName: entry?.mapping?.name,
                    // With no stage host to report a state (no video output here), a route with no
                    // display still says so.
                    warning: entry?.state.flatMap(Self.warning)
                        ?? (entry != nil && entry?.mapping == nil ? loc("settings.video.warn.noDisplay") : nil))
            }
        }
        return (store.model?.stages ?? []).map { s in
            let raw = s.raw
            let devices = Self.devices(of: s, routes: routes, outputs: outputs)
            return StageRow(
                id: s.id, name: raw["name"]?.stringValue ?? "", width: Int(raw["width"]?.doubleValue ?? 0),
                height: Int(raw["height"]?.doubleValue ?? 0), devices: devices,
                warning: devices.isEmpty ? loc("settings.video.warn.stageNoDisplay") : nil)
        }
    }

    /// The displays `stage` reaches through its regions' routes on this machine, in route order,
    /// each once: a display not connected now is named as missing.
    static func devices(of stage: ModelObject, routes: [ModelObject], outputs: VideoOutputs) -> [String] {
        let used = Set(
            (stage.raw["regions"]?.arrayValue ?? []).compactMap { $0["route"]?.stringValue })
        var names: [String] = []
        var seen: Set<String> = []
        for route in routes where used.contains(route.id.raw) {
            guard let mapping = outputs.route(route.id)?.mapping, seen.insert(mapping.display).inserted else {
                continue
            }
            if let d = outputs.displays.first(where: { $0.id == mapping.display }) {
                names.append(d.name)
            } else {
                names.append(String(format: loc("settings.video.displayAbsent"), mapping.name))
            }
        }
        return names
    }

    /// What a route's state says to the operator; nil for a route that is fine.
    static func warning(_ state: RouteState) -> String? {
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

    /// Every field typed in.
    private var textFields: [NSTextField] { nameFields }

    /// Whether a name is being typed in.
    var typing: Bool { textFields.contains { $0.currentEditor() != nil } }

    /// Ends the typing in a name field, which commits it (before what the operator clicked next).
    /// True when typing ended: that edit's reason, if refused, must stand.
    @discardableResult
    private func endTyping() -> Bool {
        guard typing else { return false }
        view.window?.makeFirstResponder(nil)
        return true
    }

    /// Draws the page: always what the workspace and the engine hold now, while a name is typed
    /// in too. That field is drawn again with what was typed and the caret where it was (unless
    /// its row is gone); nothing typed is committed by the redraw.
    func render(forced: Bool = false) {
        let rows = rows()
        let list = store.videoOutputs.displays
        let enabled = store.model != nil
        renderAddPopup(enabled: enabled, displays: list)
        noVideoLabel.isHidden = store.videoOutputs.video != false
        emptyLabel.isHidden = section == .devices ? !list.isEmpty : !rows.isEmpty
        if !forced, shown == rows, displays == list { return }
        let resume = typedIn()
        // The old fields leave the window with the table: their end of editing is not an edit.
        for field in textFields { field.delegate = nil }
        shown = rows
        displays = list
        wideWidths = []
        nameWidths = []
        let grid = Self.makeGrid(rows: true, cols: cols)
        deviceLabels = []
        nameFields = []
        sizeLabels = []
        deviceCells = []
        displayPopups = []
        scalingPopups = []
        handles = []
        editButtons = []
        monitorButtons = []
        removeButtons = []
        warningLabels = []
        func addWarning(_ row: StageRow, _ n: Int) {
            let warning = NSTextField(wrappingLabelWithString: row.warning ?? "")
            warning.textColor = .systemOrange
            warning.font = .systemFont(ofSize: 11)
            warning.setAX("\(Self.id)-warning-\(n)")
            warningLabels.append(warning)
            if row.warning != nil {
                let r = grid.addRow(with: [warning])
                r.mergeCells(in: NSRange(location: 0, length: cols.count))
            }
        }
        if section == .devices {
            for (i, d) in list.enumerated() {
                let texts = Self.deviceTexts(number: i + 1, display: d)
                let number = Self.cellLabel(texts[0], key: "number", n: i + 1)
                let name = Self.cellLabel(texts[1], key: "name", n: i + 1)
                nameWidths.append((Self.widthConstraint(of: name, nameWidth), 0))
                let size = Self.cellLabel(texts[2], key: "resolution", n: i + 1)
                let rate = Self.cellLabel(texts[3], key: "rate", n: i + 1)
                grid.addRow(with: [number, name, size, rate])
                deviceLabels.append([number, name, size, rate])
            }
        }
        for (i, row) in (section == .devices ? [] : rows).enumerated() {
            let n = i + 1
            let name = nameField(row, tag: i, enabled: enabled)
            let size = Self.cellLabel(
                String(format: loc("settings.video.resolution"), String(row.width), String(row.height)),
                key: "size", n: n)
            sizeLabels.append(size)
            let edit = smallButton(loc("settings.video.edit"), action: #selector(editClicked(_:)), tag: i)
            edit.isEnabled = enabled
            edit.setAX("\(Self.id)-edit-\(n)", label: String(format: loc("settings.video.edit.ax"), row.name))
            edit.toolTip = String(format: loc("settings.video.edit.ax"), row.name)
            editButtons.append(edit)
            let remove = NSButton(title: "", target: self, action: #selector(removeClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.video.remove"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.tag = i
            remove.isEnabled = enabled
            remove.setAX("\(Self.id)-remove-\(n)", label: "\(loc("settings.video.remove")) \(row.name)")
            removeButtons.append(remove)
            if section == .routing {
                let handle = RowHandle()
                handle.tag = i
                handle.isEnabled = enabled
                handle.setAX("\(Self.id)-handle-\(n)", label: String(format: loc("settings.video.handle.ax"), row.name))
                handle.onDrag = { [weak self] handle, point in self?.dragged(handle, to: point) }
                handle.onDrop = { [weak self] handle, point in self?.dropped(handle, at: point) }
                handles.append(handle)
                let icon = NSImageView(image: Icon.videoRoute.image(loc("settings.video.routeIcon")))
                icon.contentTintColor = .secondaryLabelColor
                icon.setContentHuggingPriority(.required, for: .horizontal)
                let cell = NSStackView(views: [icon, name])
                cell.orientation = .horizontal
                cell.spacing = Self.iconRoom - 16
                nameWidths.append((Self.widthConstraint(of: name, nameWidth - Self.iconRoom), Self.iconRoom))
                let popup = displayPopup(row, tag: i, enabled: enabled)
                popup.setAX("\(Self.id)-display-\(n)", label: "\(row.name), \(loc("settings.video.col.display"))")
                displayPopups.append(popup)
                let scaling = scalingPopup(row, tag: i, enabled: enabled)
                scaling.setAX("\(Self.id)-scaling-\(n)", label: "\(row.name), \(loc("settings.video.col.scaling"))")
                scalingPopups.append(scaling)
                grid.addRow(with: [handle, cell, size, popup, scaling, edit, remove])
            } else {
                nameWidths.append((Self.widthConstraint(of: name, nameWidth), 0))
                let devices = Self.cellLabel(row.devices.joined(separator: ", "), key: "devices", n: n)
                wideWidths.append(Self.widthConstraint(of: devices, displayWidth))
                deviceCells.append(devices)
                let monitor = smallButton(loc("settings.video.monitor"), action: #selector(monitorClicked(_:)), tag: i)
                monitor.isEnabled = enabled && store.videoOutputs.video != false
                monitor.toolTip = loc("settings.video.monitor.tip")
                monitor.setAX("\(Self.id)-monitor-\(n)", label: "\(row.name), \(loc("settings.video.monitor.ax"))")
                monitorButtons.append(monitor)
                grid.addRow(with: [name, size, devices, edit, monitor, remove])
            }
            nameFields.append(name)
            addWarning(row, n)
        }
        install(grid)
        if section == .routing { holder.addSubview(dropLine) }
        dropLine.isHidden = true
        fitColumns()
        grid.layoutSubtreeIfNeeded()
        if let resume { self.resume(resume) }
    }

    /// The add pop-up's items: an empty stage and a stage on each route (Video Outputs), or a
    /// route on each display and one on none (Output Routing). Rebuilt only when they change, so
    /// an open menu is not pulled from under the operator.
    private func renderAddPopup(enabled: Bool, displays: [StageDisplay]) {
        addPopup.isEnabled = enabled
        guard section != .devices else { return }
        var items: [(String, String?, Bool)] = []  // title, represented object, enabled
        if section == .outputs {
            items.append((loc("settings.video.addEmpty"), "", true))
            let routes = store.model?.routes ?? []
            if !routes.isEmpty { items.append(("-", nil, false)) }
            for r in routes {
                items.append(
                    (String(format: loc("settings.video.addOnRoute"), r.raw["name"]?.stringValue ?? ""), r.id.raw, true))
            }
        } else {
            if !displays.isEmpty { items.append((loc("settings.video.addRouteWith"), nil, false)) }
            for d in displays {
                items.append(
                    (
                        "   " + String(format: loc("settings.video.displayItem"), d.name, d.width, d.height), d.id,
                        true
                    ))
            }
            if !displays.isEmpty { items.append(("-", nil, false)) }
            items.append((loc("settings.video.addRouteNone"), "", true))
        }
        let key = items.map { "\($0.0)\u{0}\($0.1 ?? "\u{1}")" }
        guard key != shownAddItems else { return }
        shownAddItems = key
        let menu = NSMenu()
        menu.autoenablesItems = false
        let title = NSMenuItem(
            title: loc(section == .routing ? "settings.video.newRoute" : "settings.video.add"), action: nil,
            keyEquivalent: "")
        menu.addItem(title)
        for (text, object, on) in items {
            if text == "-" {
                menu.addItem(.separator())
                continue
            }
            let item = NSMenuItem(title: text, action: nil, keyEquivalent: "")
            item.representedObject = object
            item.isEnabled = on
            menu.addItem(item)
        }
        addPopup.menu = menu
    }

    /// A name typed in place.
    private func nameField(_ row: StageRow, tag: Int, enabled: Bool) -> NSTextField {
        let name = NSTextField(string: row.name)
        name.controlSize = .small
        name.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        name.usesSingleLineMode = true
        name.lineBreakMode = .byTruncatingTail
        name.cell?.truncatesLastVisibleLine = true
        name.toolTip = row.name
        name.isEditable = enabled
        name.delegate = self
        name.tag = tag
        name.identifier = NSUserInterfaceItemIdentifier(TypedField.name.rawValue)
        name.setAX("\(Self.id)-name-\(tag + 1)", label: "\(row.name), \(loc("settings.video.col.name"))")
        return name
    }

    private func smallButton(_ title: String, action: Selector, tag: Int) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        button.controlSize = .small
        button.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        button.bezelStyle = .rounded
        button.tag = tag
        return button
    }

    /// A read-only cell: one line, cut at the end, its whole text in the tooltip.
    private static func cellLabel(_ text: String, key: String, n: Int) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.lineBreakMode = .byTruncatingTail
        label.toolTip = text
        label.setAX("\(id)-\(key)-\(n)", label: text)
        return label
    }

    /// Holds `view` at `width`, to move with the table's columns.
    private static func widthConstraint(of view: NSView, _ width: CGFloat) -> NSLayoutConstraint {
        view.translatesAutoresizingMaskIntoConstraints = false
        let c = view.widthAnchor.constraint(equalToConstant: width)
        c.isActive = true
        return c
    }

    /// What an Output Devices row shows for the display numbered `number`: "Device N:", its name,
    /// its size in pixels and its refresh rate (empty when the stage host did not know it).
    static func deviceTexts(number: Int, display d: StageDisplay) -> [String] {
        [
            String(format: loc("settings.video.deviceNumber"), number),
            d.name,
            String(format: loc("settings.video.resolution"), String(d.width), String(d.height)),
            d.fps > 0 ? String(format: loc("settings.video.rate"), d.fps) : "",
        ]
    }

    /// The fields typed in place, by their identifier.
    enum TypedField: String { case name }

    /// The field being typed in: its row, the text so far and the selection.
    private struct Typed {
        var row: ModelID
        var text: String?
        var selection: NSRange
    }

    /// The field holding the caret, and what was typed in it: nil text when it holds what the
    /// page last drew there (a value already committed, as after Return), so a redraw shows the
    /// workspace's value (an undo's, another client's), never a stale one a later click would send.
    private func typedIn() -> Typed? {
        guard let field = textFields.first(where: { $0.currentEditor() != nil }),
            let editor = field.currentEditor(), let row = row(field.tag)
        else { return nil }
        let typed = editor.string == row.name ? nil : editor.string
        return Typed(row: row.id, text: typed, selection: editor.selectedRange)
    }

    /// Puts the caret back in the redrawn field of the row that was being typed in, with what
    /// was typed, or the workspace's value when nothing new was.
    private func resume(_ typed: Typed) {
        guard let i = shown?.firstIndex(where: { $0.id == typed.row }) else { return }
        let field = nameFields[i]
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
        Self.fillDisplays(popup, displays: displays, chosen: row.display, chosenName: row.displayName)
        popup.tag = tag
        popup.target = self
        popup.action = #selector(displayChosen(_:))
        popup.isEnabled = enabled
        popup.toolTip = popup.titleOfSelectedItem
        wideWidths.append(Self.widthConstraint(of: popup, displayWidth))
        return popup
    }

    /// Fills `popup` with "None", then `displays`, and chooses `chosen`; a chosen display that is
    /// not connected stays listed, marked (the route editor's pop-up too).
    static func fillDisplays(_ popup: NSPopUpButton, displays: [StageDisplay], chosen: String?, chosenName: String?) {
        let menu = NSMenu()
        let none = NSMenuItem(title: loc("settings.video.displayNone"), action: nil, keyEquivalent: "")
        menu.addItem(none)
        var selected = none
        for d in displays {
            let item = NSMenuItem(
                title: String(format: loc("settings.video.displayItem"), d.name, d.width, d.height), action: nil,
                keyEquivalent: "")
            item.representedObject = d.id
            menu.addItem(item)
            if d.id == chosen { selected = item }
        }
        if let chosen, !displays.contains(where: { $0.id == chosen }) {
            let item = NSMenuItem(
                title: String(format: loc("settings.video.displayAbsent"), chosenName ?? chosen), action: nil,
                keyEquivalent: "")
            item.representedObject = chosen
            menu.addItem(item)
            selected = item
        }
        popup.menu = menu
        popup.select(selected)
    }

    /// The route's scaling, chosen beside its display (QLab 5 manual p. 233).
    private func scalingPopup(_ row: StageRow, tag: Int, enabled: Bool) -> NSPopUpButton {
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        popup.controlSize = .small
        popup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        Self.fillScalings(popup, chosen: row.scaling)
        popup.tag = tag
        popup.target = self
        popup.action = #selector(scalingChosen(_:))
        popup.isEnabled = enabled
        popup.toolTip = loc("settings.video.scaling.tip")
        _ = Self.widthConstraint(of: popup, Col.scaling.width)
        return popup
    }

    /// Fills `popup` with the scalings and chooses `chosen`.
    static func fillScalings(_ popup: NSPopUpButton, chosen: String) {
        popup.removeAllItems()
        for s in scalings {
            popup.addItem(withTitle: loc("settings.video.scaling.\(s)"))
            popup.lastItem?.representedObject = s
        }
        popup.selectItem(at: scalings.firstIndex(of: chosen) ?? 3)
    }

    // MARK: Dragging a route

    /// The gap among the routes `point` (in the window) is nearest: 0 above the first, the count
    /// below the last.
    func gap(at point: NSPoint) -> Int {
        let y = holder.convert(point, from: nil).y
        return nameFields.filter { field in
            let frame = field.convert(field.bounds, to: holder)
            return frame.midY < y
        }.count
    }

    /// Where the line for `gap` goes, in the table's view.
    private func lineY(_ gap: Int) -> CGFloat {
        guard !nameFields.isEmpty else { return Self.inset }
        if gap < nameFields.count {
            let f = nameFields[gap].convert(nameFields[gap].bounds, to: holder)
            return f.minY - grid.rowSpacing / 2 - 1
        }
        let last = nameFields[nameFields.count - 1]
        let f = last.convert(last.bounds, to: holder)
        return f.maxY + grid.rowSpacing / 2 - 1
    }

    private func dragged(_ handle: RowHandle, to point: NSPoint) {
        let y = lineY(gap(at: point))
        dropLine.frame = NSRect(x: Self.inset, y: y, width: max(0, holder.bounds.width - 2 * Self.inset), height: 2)
        dropLine.isHidden = false
    }

    private func dropped(_ handle: RowHandle, at point: NSPoint) {
        dropLine.isHidden = true
        moveRoute(from: handle.tag, toGap: gap(at: point))
    }

    /// Moves the route at `from` into `gap` (0 above the first route, the count below the last):
    /// one undo step; nothing when it lands where it is.
    func moveRoute(from: Int, toGap gap: Int) {
        let typed = endTyping()
        guard let row = row(from) else { return }
        let index = gap > from ? gap - 1 : gap
        guard index != from, let count = shown?.count, (0..<count).contains(index) else { return }
        commit(row.id, after: typed) { [store] in await store.send(command: .moveRoute(row.id, to: index)) }
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

    /// Runs `edit` for `item` after the edits before it, shows its refusal (or clears the last
    /// one, unless a newer edit has said something since), and redraws from the engine.
    /// A refused edit of a typed name puts its value back. `after` is true for a click that ended
    /// typing: the typed edit's reason (shown now, or on its way from the engine) is not cleared
    /// or outrun by the click's edit.
    private func commit(
        _ item: ModelID?, typed field: Bool = false, after typed: Bool = false,
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
                if let item, field { putBack(item) }
                if mine == attempt { messageLabel.stringValue = reason(e, item) }
            }
            render(forced: true)
        }
    }

    /// Puts the row's name as the workspace has it back in its field (the one drawn now), also
    /// while it is typed in.
    private func putBack(_ item: ModelID) {
        guard let i = shown?.firstIndex(where: { $0.id == item }), let row = rows().first(where: { $0.id == item })
        else { return }
        let field = nameFields[i]
        field.stringValue = row.name
        field.currentEditor()?.string = row.name
    }

    /// A refusal in words, never with an internal id: a stage or route gone since it was drawn
    /// says so.
    private func reason(_ e: EngineError, _ item: ModelID?) -> String {
        let listed = section == .routing ? store.model?.routes : store.model?.stages
        if let item, listed?.contains(where: { $0.id == item }) != true {
            return loc(section == .routing ? "settings.video.routeGone" : "settings.video.gone")
        }
        if let item, let cue = Self.cueUsing(e), let name = rows().first(where: { $0.id == item })?.name {
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

    /// "<format> N", N one past the highest such number `names` use (as the engine names a new
    /// route, Q813).
    static func nextName(_ format: String, after names: [String]) -> String {
        let prefix = format.replacingOccurrences(of: "%d", with: "")
        let highest = names.compactMap { name -> Int? in
            guard name.hasPrefix(prefix) else { return nil }
            return Int(name.dropFirst(prefix.count))
        }.max() ?? 0
        return String(format: format, highest + 1)
    }

    @objc private func addChosen(_ sender: NSPopUpButton) {
        let object = sender.selectedItem?.representedObject as? String
        let typed = endTyping()
        guard let object else { return }
        if section == .routing {
            addRoute(on: object.isEmpty ? nil : displays.first { $0.id == object }, after: typed)
        } else {
            addStage(on: object.isEmpty ? nil : ModelID(object), after: typed)
        }
    }

    /// A new stage: 1920 x 1080 with no region (QLab 5 manual p. 226: an empty stage), or sized as
    /// `route` with one region covering it sent there.
    private func addStage(on route: ModelID?, after typed: Bool) {
        let stages = store.model?.stages ?? []
        let id = ModelID(UUID().uuidString.lowercased())
        let name = Self.nextName(
            loc("settings.video.newName"), after: stages.map { $0.raw["name"]?.stringValue ?? "" })
        let routeRow = route.flatMap { r in store.model?.routes.first { $0.id == r } }
        let width = routeRow.map { Int($0.raw["width"]?.doubleValue ?? 1920) } ?? 1920
        let height = routeRow.map { Int($0.raw["height"]?.doubleValue ?? 1080) } ?? 1080
        let region = routeRow.map { _ in ModelID(UUID().uuidString.lowercased()) }
        commit(nil, after: typed) { [store] in
            await store.send(
                command: .addStage(
                    id: id, name: name, width: width, height: height, index: stages.count, route: routeRow?.id,
                    region: region))
        }
    }

    /// A new route "Output N": sized as `display` and shown on it on this machine (QLab 5 manual
    /// p. 233), or 1920 x 1080 on none. The display is this machine's choice, not an undo step.
    private func addRoute(on display: StageDisplay?, after typed: Bool) {
        let routes = store.model?.routes ?? []
        let id = ModelID(UUID().uuidString.lowercased())
        let name = Self.nextName(
            loc("settings.video.routeName"), after: routes.map { $0.raw["name"]?.stringValue ?? "" })
        commit(nil, after: typed) { [store] in
            let added = await store.send(
                command: .addRoute(
                    id: id, name: name, width: display.map { Int($0.width) } ?? 1920, height: display.map { Int($0.height) } ?? 1080,
                    index: routes.count))
            guard case .success = added, let display else { return added }
            return await store.setRouteDisplay(id, display: display.id)
        }
    }

    @objc private func displaysClicked() {
        if let url = Self.displaysSettings { openURL(url) }
    }

    @objc private func editClicked(_ sender: NSButton) {
        endTyping()
        guard let row = row(sender.tag) else { return }
        if section == .routing { onEditRoute?(row.id) } else { onEditStage?(row.id) }
    }

    @objc private func removeClicked(_ sender: NSButton) {
        let typed = endTyping()
        guard let row = row(sender.tag) else { return }
        let command: Command = section == .routing ? .removeRoute(row.id) : .removeStage(row.id)
        commit(row.id, after: typed) { [store] in await store.send(command: command) }
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
        commit(row.id, after: typed) { [store] in await store.setRouteDisplay(row.id, display: display) }
    }

    @objc private func scalingChosen(_ sender: NSPopUpButton) {
        let scaling = sender.selectedItem?.representedObject as? String
        let typed = endTyping()
        guard let row = row(sender.tag), let scaling, scaling != row.scaling else { return }
        commit(row.id, after: typed) { [store] in
            await store.send(
                command: .setRoute(row.id, .object(["type": .string("scaling"), "value": .string(scaling)])))
        }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField, let row = row(field.tag) else { return }
        let text = field.stringValue
        guard text != row.name else { return }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            tell(loc(section == .routing ? "settings.video.badRouteName" : "settings.video.badName"))
            field.stringValue = row.name
            return
        }
        let property: JSONValue = .object(["type": .string("name"), "value": .string(text)])
        let command: Command = section == .routing ? .setRoute(row.id, property) : .setStage(row.id, property)
        commit(row.id, typed: true) { [store] in await store.send(command: command) }
    }
}

/// A settings page's stack: Command-N on it is the page's own add (not New Workspace).
@MainActor
final class PageStack: NSStackView {
    /// Answers Command-N; false lets it go on to the menu.
    var onCommandN: (() -> Bool)?

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if let onCommandN, window != nil, !isHiddenOrHasHiddenAncestor,
            event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command,
            event.charactersIgnoringModifiers == "n", onCommandN()
        {
            return true
        }
        return super.performKeyEquivalent(with: event)
    }
}

/// A row's drag handle (three lines): a press and drag moves the row to where it is let go.
@MainActor
final class RowHandle: NSView {
    /// The row it drags, as the other controls' tags say.
    private var row = 0
    override var tag: Int {
        get { row }
        set { row = newValue }
    }
    var isEnabled = true
    /// The pointer moved while pressed, and where it was let go (window coordinates).
    var onDrag: ((RowHandle, NSPoint) -> Void)?
    var onDrop: ((RowHandle, NSPoint) -> Void)?
    private var pressed = false
    private var moved = false

    override var intrinsicContentSize: NSSize { NSSize(width: 14, height: 14) }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .handle }

    override func resetCursorRects() {
        if isEnabled { addCursorRect(bounds, cursor: .openHand) }
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        (isEnabled ? NSColor.secondaryLabelColor : NSColor.tertiaryLabelColor).setFill()
        let w = min(bounds.width, 12)
        let x = (bounds.width - w) / 2
        for i in 0..<3 {
            let y = bounds.midY - 4 + CGFloat(i) * 3.5
            NSRect(x: x, y: y, width: w, height: 1.2).fill()
        }
    }

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        pressed = true
        moved = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard pressed else { return }
        moved = true
        onDrag?(self, event.locationInWindow)
    }

    override func mouseUp(with event: NSEvent) {
        guard pressed else { return }
        pressed = false
        if moved { onDrop?(self, event.locationInWindow) }
    }
}
