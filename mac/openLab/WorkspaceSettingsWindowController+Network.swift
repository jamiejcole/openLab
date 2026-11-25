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
    /// The columns' widths for the table's width: Interface first takes what is left over the least
    /// widths (up to `networkInterfaceMost`), then Name and Address share the rest, so every column is
    /// whole at the least window. The fields' widths change before the grid's columns, so no required
    /// width ever contradicts a column's.
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

    /// Draws the table. One that is as drawn is left alone, and so is one being typed in: a rebuild
    /// would take the caret away (the next render catches up).
    func renderNetwork(_ rows: [NetworkPatchRow], forced: Bool) {
        let enabled = model.store.model != nil
        networkAddButton.isEnabled = enabled
        if !forced, shownNetwork == rows { return }
        if networkFields.contains(where: { $0.currentEditor() != nil }) {
            lastDrawn = nil
            return
        }
        shownNetwork = rows
        // Row 0 holds the column titles.
        while networkGrid.numberOfRows > 1 {
            let last = networkGrid.row(at: networkGrid.numberOfRows - 1)
            for c in 0..<last.numberOfCells { last.cell(at: c).contentView?.removeFromSuperview() }
            networkGrid.removeRow(at: networkGrid.numberOfRows - 1)
        }
        forget(networkFields)
        networkFields = []
        networkTransportPopups = []
        networkInterfacePopups = []
        networkDeleteButtons = []
        networkNameWidths = []
        networkInterfaceWidths = []
        networkHostWidths = []
        let available = networkInterfaces()
        for (i, row) in rows.enumerated() {
            let n = i + 1
            func field(_ key: String, _ text: String, width: CGFloat) -> NSTextField {
                let f = NSTextField(string: text)
                f.isEditable = enabled
                f.isBezeled = true
                f.delegate = self
                f.tag = i
                f.identifier = NSUserInterfaceItemIdentifier(key)
                let w = f.widthAnchor.constraint(equalToConstant: width)
                w.isActive = true
                if key == "name" { networkNameWidths.append(w) } else if key == "host" { networkHostWidths.append(w) }
                f.setAccessibilityIdentifier("\(AX.settingsNetwork)-\(key)-\(n)")
                f.setAccessibilityLabel("\(row.name), \(loc("settings.network.col.\(key)"))")
                remember(f)
                networkFields.append(f)
                return f
            }
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            for t in WorkspaceSettingsModel.networkTransports { popup.addItem(withTitle: t.uppercased()) }
            popup.selectItem(at: WorkspaceSettingsModel.networkTransports.firstIndex(of: row.transport) ?? 0)
            popup.tag = i
            popup.target = self
            popup.action = #selector(networkTransportChosen(_:))
            popup.isEnabled = enabled
            popup.setAX(
                "\(AX.settingsNetwork)-protocol-\(n)", label: "\(row.name), \(loc("settings.network.col.protocol"))")
            let interface = NSPopUpButton(frame: .zero, pullsDown: false)
            let choices = NetworkInterfaces.choices(stored: row.interface, available: available)
            for choice in choices {
                interface.addItem(withTitle: choice.title)
                interface.lastItem?.representedObject = choice.value
            }
            interface.selectItem(at: choices.firstIndex { $0.value == row.interface } ?? 0)
            interface.tag = i
            interface.target = self
            interface.action = #selector(networkInterfaceChosen(_:))
            interface.isEnabled = enabled
            interface.toolTip = loc("settings.network.interfaceTip")
            // Cut in the middle: "en0 (192…20)" keeps the interface's name in a narrow column.
            (interface.cell as? NSPopUpButtonCell)?.lineBreakMode = .byTruncatingMiddle
            let interfaceWidth = interface.widthAnchor.constraint(equalToConstant: Self.networkInterfaceWidth)
            interfaceWidth.isActive = true
            networkInterfaceWidths.append(interfaceWidth)
            interface.setAX(
                "\(AX.settingsNetwork)-interface-\(n)", label: "\(row.name), \(loc("settings.network.col.interface"))")
            let delete = NSButton(title: "", target: self, action: #selector(networkDeleteClicked(_:)))
            delete.image = Icon.fieldClear.image(loc("settings.network.delete"), compact: true)
            delete.isBordered = false
            delete.imagePosition = .imageOnly
            delete.tag = i
            delete.isEnabled = enabled
            delete.setAX("\(AX.settingsNetwork)-delete-\(n)", label: "\(loc("settings.network.delete")) \(row.name)")
            networkGrid.addRow(with: [
                field("name", row.name, width: Self.networkNameMin), popup, interface,
                field("host", row.host, width: Self.networkHostMin),
                field("port", String(row.port), width: 52), delete,
            ])
            networkTransportPopups.append(popup)
            networkInterfacePopups.append(interface)
            networkDeleteButtons.append(delete)
        }
        fitNetworkColumns()
        networkGrid.layoutSubtreeIfNeeded()
    }

    /// A field being typed in is committed at the click on a button or pop-up of the table (its
    /// commit is queued), since the table is not rebuilt under a caret.
    private func endNetworkTyping() {
        if networkFields.contains(where: { $0.currentEditor() != nil }) { window?.makeFirstResponder(nil) }
    }

    /// After an action of the table: drawn again from the engine's state. A field clicked into since
    /// the click keeps its caret and what is typed in it: the table is drawn once that typing ends.
    private func redrawNetwork() {
        lastDrawn = nil
        render()
    }

    @objc private func networkAddClicked() {
        endNetworkTyping()
        networkActions = Task {
            await model.addNetworkPatch()
            redrawNetwork()
        }
    }

    @objc private func networkDeleteClicked(_ sender: NSButton) {
        guard let rows = shownNetwork, rows.indices.contains(sender.tag) else { return }
        let id = rows[sender.tag].id
        endNetworkTyping()
        networkActions = Task {
            await model.removeNetworkPatch(id)
            redrawNetwork()
        }
    }

    @objc private func networkTransportChosen(_ sender: NSPopUpButton) {
        guard let rows = shownNetwork, rows.indices.contains(sender.tag),
            WorkspaceSettingsModel.networkTransports.indices.contains(sender.indexOfSelectedItem)
        else { return }
        let id = rows[sender.tag].id
        let transport = WorkspaceSettingsModel.networkTransports[sender.indexOfSelectedItem]
        endNetworkTyping()
        networkActions = Task {
            await model.setNetworkPatchTransport(id, transport)
            redrawNetwork()
        }
    }

    @objc private func networkInterfaceChosen(_ sender: NSPopUpButton) {
        guard let rows = shownNetwork, rows.indices.contains(sender.tag) else { return }
        let id = rows[sender.tag].id
        let interface = sender.selectedItem?.representedObject as? String
        endNetworkTyping()
        networkActions = Task {
            await model.setNetworkPatchInterface(id, interface)
            redrawNetwork()
        }
    }

    /// A field of the table was committed (Return or leaving it): the engine takes it, or the old
    /// text comes back with a line saying why not. Commits run one after another.
    func networkFieldCommitted(_ field: NSTextField) {
        guard let rows = shownNetwork, rows.indices.contains(field.tag), let key = field.identifier?.rawValue else {
            return
        }
        guard takeTyped(field) else {
            // Only clicked into: nothing to send. The table may have waited for the caret.
            networkActions = Task { redrawNetwork() }
            return
        }
        let row = rows[field.tag]
        let text = field.stringValue
        fieldBaselines[ObjectIdentifier(field)] = text
        let previous = networkCommits
        networkCommits = Task {
            await previous?.value
            let taken: Bool
            switch key {
            case "name": taken = await model.setNetworkPatchName(row.id, text)
            case "host": taken = await model.setNetworkPatchHost(row.id, text)
            default: taken = await model.setNetworkPatchPort(row.id, text)
            }
            if !taken {
                // Refused: the field shows what the engine has.
                let now = model.networkPatches.first { $0.id == row.id } ?? row
                let value = key == "name" ? now.name : key == "host" ? now.host : String(now.port)
                field.stringValue = value
                fieldBaselines[ObjectIdentifier(field)] = value
            }
            redrawNetwork()
        }
    }
}
