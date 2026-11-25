// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The Network tab of the workspace settings (CT-30): the destinations Network cues send to, one
/// row each with its name, host, port, UDP or TCP and a delete button, and a button that adds one.
/// Every control sends its edit to the engine and the table is drawn from what the engine then
/// holds, so a refused edit shows the old state again.
extension WorkspaceSettingsWindowController {
    func buildNetworkTab() -> NSView {
        networkAddButton.setAX(AX.settingsNetwork + "-add", label: loc("settings.network.add"))
        networkAddButton.bezelStyle = .rounded
        networkAddButton.target = self
        networkAddButton.action = #selector(networkAddClicked)
        let note = NSTextField(wrappingLabelWithString: loc("settings.network.note"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.preferredMaxLayoutWidth = 300
        let top = NSStackView(views: [networkAddButton, note])
        top.orientation = .horizontal
        top.spacing = 12

        let headers = ["name", "protocol", "interface", "host", "port", "delete"].map { key -> NSView in
            let label = NSTextField(labelWithString: loc("settings.network.col.\(key)"))
            label.font = .systemFont(ofSize: 12, weight: .semibold)
            return label
        }
        networkGrid.addRow(with: headers)
        networkGrid.rowSpacing = 8
        networkGrid.columnSpacing = 6
        // Each cell is centred on its row, so the remove button sits on the fields' middle.
        networkGrid.yPlacement = .center
        for (c, width) in [Self.networkNameMin, 64, Self.networkInterfaceWidth, Self.networkHostMin, 52, 24]
            .enumerated()
        {
            networkGrid.column(at: c).width = width
        }

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        let holder = FlippedView()
        networkGrid.translatesAutoresizingMaskIntoConstraints = false
        holder.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(networkGrid)
        scroll.documentView = holder
        // Not required: the columns are fitted to the window's width, and a table that is
        // momentarily wider must not hold the window wide (nor outweigh the page's own sizing).
        let trailing = networkGrid.trailingAnchor.constraint(
            lessThanOrEqualTo: holder.trailingAnchor, constant: -8)
        trailing.priority = NSLayoutConstraint.Priority(250)
        NSLayoutConstraint.activate([
            networkGrid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 8),
            networkGrid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            networkGrid.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -8),
            trailing,
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            holder.widthAnchor.constraint(greaterThanOrEqualTo: scroll.contentView.widthAnchor),
            scroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 300),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 200),
        ])
        scroll.setAX(AX.settingsNetwork + "-table", label: loc("settings.tab.network"))
        networkScroll = scroll
        scroll.contentView.postsFrameChangedNotifications = true
        subs.append(
            NotificationCenter.default.publisher(for: NSView.frameDidChangeNotification, object: scroll.contentView)
                .receive(on: RunLoop.main)
                .sink { [weak self] _ in MainActor.assumeIsolated { self?.fitNetworkColumns() } })

        let stack = NSStackView(views: [top, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.distribution = .fill
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        top.setContentHuggingPriority(.required, for: .vertical)
        stack.spacing = 12
        return stack
    }

    static let networkNameMin: CGFloat = 70
    static let networkHostMin: CGFloat = 96
    static let networkInterfaceWidth: CGFloat = 136
    /// The widest the Interface column takes, so a usual choice ("en0 (192.168.1.20)") reads whole.
    static let networkInterfaceMost: CGFloat = 164
    /// Just under required: a grid column takes a new width later than a field does, and a required field
    /// width wider than its column meanwhile would be dropped by the layout engine.
    private static let fieldWidthPriority = NSLayoutConstraint.Priority(999)
    /// The columns' widths for the table's width: Interface first takes what is left over the least
    /// widths (up to `networkInterfaceMost`), then Name and Address share the rest, so every column is
    /// whole at the least window.
    func fitNetworkColumns() {
        guard let scroll = networkScroll else { return }
        let room = scroll.contentView.bounds.width - 16 - (52 + 64 + 24) - 5 * networkGrid.columnSpacing
        let interface = min(
            Self.networkInterfaceMost,
            max(Self.networkInterfaceWidth, (room - Self.networkNameMin - Self.networkHostMin).rounded(.down)))
        let spare = room - interface
        let name = max(Self.networkNameMin, (spare * 0.4).rounded(.down))
        let host = max(Self.networkHostMin, (spare - name).rounded(.down))
        for c in networkNameWidths { c.constant = name }
        for c in networkHostWidths { c.constant = host }
        for c in networkInterfaceWidths { c.constant = interface }
        networkGrid.column(at: 0).width = name
        networkGrid.column(at: 3).width = host
        networkGrid.column(at: 2).width = interface
    }

    /// Draws the table: the rows built again when they changed, else drawn in place (`keepsRows`).
    func renderNetwork(_ rows: [NetworkPatchRow], forced: Bool) {
        let enabled = model.store.model != nil
        networkAddButton.isEnabled = enabled
        if !forced, shownNetwork == rows { return }
        shownNetwork = rows
        if !keepsRows(networkFields, now: rows.map(\.id.raw), in: networkGrid) { buildNetworkRows(rows) }
        for field in networkFields {
            show(field)
            field.allow(editable: enabled)
            let name = rows.first { $0.id.raw == field.row }?.name ?? ""
            field.setAccessibilityLabel("\(name), \(loc("settings.network.col.\(field.identifier?.rawValue ?? "")"))")
        }
        let available = networkInterfaces()
        for (i, transport) in networkTransportPopups.enumerated() {
            let row = rows.first { $0.id.raw == transport.identifier?.rawValue }
            let interface = networkInterfacePopups[i]
            if let row, !edits.isWaiting(Self.networkKey(row.id, "protocol")) {
                transport.selectItem(
                    at: WorkspaceSettingsModel.networkTransports.firstIndex(of: row.transport) ?? 0)
            }
            if let row, !edits.isWaiting(Self.networkKey(row.id, "interface")) {
                let choices = NetworkInterfaces.choices(stored: row.interface, available: available)
                interface.removeAllItems()
                for choice in choices {
                    interface.addItem(withTitle: choice.title)
                    interface.lastItem?.representedObject = choice.value
                }
                interface.selectItem(at: choices.firstIndex { $0.value == row.interface } ?? 0)
            }
            for control in [transport, interface, networkDeleteButtons[i]] {
                control.isEnabled = enabled && row != nil
            }
            let name = row?.name ?? ""
            transport.setAccessibilityLabel("\(name), \(loc("settings.network.col.protocol"))")
            interface.setAccessibilityLabel("\(name), \(loc("settings.network.col.interface"))")
            networkDeleteButtons[i].setAccessibilityLabel("\(loc("settings.network.delete")) \(name)")
        }
        fitNetworkColumns()
        networkGrid.layoutSubtreeIfNeeded()
    }

    private static func networkKey(_ id: ModelID, _ column: String) -> String { "network-\(id.raw)-\(column)" }

    private func buildNetworkRows(_ rows: [NetworkPatchRow]) {
        // Row 0 holds the column titles.
        while networkGrid.numberOfRows > 1 {
            let last = networkGrid.row(at: networkGrid.numberOfRows - 1)
            for c in 0..<last.numberOfCells { last.cell(at: c).contentView?.removeFromSuperview() }
            networkGrid.removeRow(at: networkGrid.numberOfRows - 1)
        }
        networkFields = []
        networkTransportPopups = []
        networkInterfacePopups = []
        networkDeleteButtons = []
        networkNameWidths = []
        networkInterfaceWidths = []
        networkHostWidths = []
        for (i, row) in rows.enumerated() {
            let n = i + 1
            let id = row.id
            func field(
                _ column: String, width: CGFloat, text: @escaping @MainActor (NetworkPatchRow) -> String,
                send: @escaping @MainActor (String) async -> Bool
            ) -> NSTextField {
                let f = SettingsField(key: Self.networkKey(id, column), row: id.raw)
                f.engineText = { [model] in model.networkPatches.first { $0.id == id }.map(text) }
                f.send = send
                f.isBezeled = true
                f.delegate = self
                f.identifier = NSUserInterfaceItemIdentifier(column)
                let w = f.widthAnchor.constraint(equalToConstant: width)
                w.priority = Self.fieldWidthPriority
                w.isActive = true
                if column == "name" {
                    networkNameWidths.append(w)
                } else if column == "host" {
                    networkHostWidths.append(w)
                }
                f.setAccessibilityIdentifier("\(AX.settingsNetwork)-\(column)-\(n)")
                networkFields.append(f)
                return f
            }
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            for t in WorkspaceSettingsModel.networkTransports { popup.addItem(withTitle: t.uppercased()) }
            popup.target = self
            popup.action = #selector(networkTransportChosen(_:))
            popup.setAX("\(AX.settingsNetwork)-protocol-\(n)", label: "")
            let interface = NSPopUpButton(frame: .zero, pullsDown: false)
            interface.target = self
            interface.action = #selector(networkInterfaceChosen(_:))
            interface.toolTip = loc("settings.network.interfaceTip")
            // Cut in the middle: "en0 (192…20)" keeps the interface's name in a narrow column.
            (interface.cell as? NSPopUpButtonCell)?.lineBreakMode = .byTruncatingMiddle
            let interfaceWidth = interface.widthAnchor.constraint(equalToConstant: Self.networkInterfaceWidth)
            interfaceWidth.priority = Self.fieldWidthPriority
            interfaceWidth.isActive = true
            networkInterfaceWidths.append(interfaceWidth)
            interface.setAX("\(AX.settingsNetwork)-interface-\(n)", label: "")
            let delete = NSButton(title: "", target: self, action: #selector(networkDeleteClicked(_:)))
            delete.image = Icon.fieldClear.image(loc("settings.network.delete"), compact: true)
            delete.isBordered = false
            delete.imagePosition = .imageOnly
            delete.setAX("\(AX.settingsNetwork)-delete-\(n)", label: "")
            for control in [popup, interface, delete] { control.identifier = NSUserInterfaceItemIdentifier(id.raw) }
            networkGrid.addRow(with: [
                field("name", width: Self.networkNameMin, text: \.name) { [model] in
                    await model.setNetworkPatchName(id, $0)
                },
                popup, interface,
                field("host", width: Self.networkHostMin, text: \.host) { [model] in
                    await model.setNetworkPatchHost(id, $0)
                },
                field("port", width: 52, text: { String($0.port) }) { [model] in
                    await model.setNetworkPatchPort(id, $0)
                },
                delete,
            ])
            networkTransportPopups.append(popup)
            networkInterfacePopups.append(interface)
            networkDeleteButtons.append(delete)
        }
    }

    // MARK: Actions

    /// Runs `step` on the destination a control stands for, in its turn; one gone by then takes nothing
    /// (`gone`: already as wanted when it was to be removed).
    private func networkAct(
        _ sender: NSControl, _ column: String, gone: Bool = false,
        _ step: @escaping @MainActor (WorkspaceSettingsModel, ModelID) async -> Bool
    ) {
        guard let id = sender.identifier.map({ ModelID($0.rawValue) }) else { return }
        act(Self.networkKey(id, column)) { [model] in
            guard model.networkPatches.contains(where: { $0.id == id }) else { return gone || model.rowGone() }
            return await step(model, id)
        }
    }

    @objc private func networkAddClicked() { act("network-add") { [model] in await model.addNetworkPatch() } }

    @objc private func networkDeleteClicked(_ sender: NSButton) {
        networkAct(sender, "delete", gone: true) { await $0.removeNetworkPatch($1) }
    }

    @objc private func networkTransportChosen(_ sender: NSPopUpButton) {
        guard WorkspaceSettingsModel.networkTransports.indices.contains(sender.indexOfSelectedItem) else { return }
        let transport = WorkspaceSettingsModel.networkTransports[sender.indexOfSelectedItem]
        networkAct(sender, "protocol") { await $0.setNetworkPatchTransport($1, transport) }
    }

    @objc private func networkInterfaceChosen(_ sender: NSPopUpButton) {
        let interface = sender.selectedItem?.representedObject as? String
        networkAct(sender, "interface") { await $0.setNetworkPatchInterface($1, interface) }
    }
}
