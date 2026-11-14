// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

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

        let headers = ["name", "host", "port", "protocol", "delete"].map { key -> NSView in
            let label = NSTextField(labelWithString: loc("settings.network.col.\(key)"))
            label.font = .systemFont(ofSize: 12, weight: .semibold)
            return label
        }
        networkGrid.addRow(with: headers)
        networkGrid.rowSpacing = 8
        networkGrid.columnSpacing = 8
        for (c, width) in [120, 140, 60, 70, 40].enumerated() { networkGrid.column(at: c).width = CGFloat(width) }

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        let holder = FlippedView()
        networkGrid.translatesAutoresizingMaskIntoConstraints = false
        holder.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(networkGrid)
        scroll.documentView = holder
        NSLayoutConstraint.activate([
            networkGrid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 8),
            networkGrid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            networkGrid.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -8),
            networkGrid.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -8),
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            holder.widthAnchor.constraint(greaterThanOrEqualTo: scroll.contentView.widthAnchor),
            scroll.widthAnchor.constraint(equalToConstant: 480),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 200),
        ])
        scroll.setAX(AX.settingsNetwork + "-table", label: loc("settings.tab.network"))

        let stack = NSStackView(views: [top, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.distribution = .fill
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        top.setContentHuggingPriority(.required, for: .vertical)
        stack.spacing = 12
        return stack
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
        networkFields = []
        networkTransportPopups = []
        networkDeleteButtons = []
        for (i, row) in rows.enumerated() {
            let n = i + 1
            func field(_ key: String, _ text: String, width: CGFloat) -> NSTextField {
                let f = NSTextField(string: text)
                f.isEditable = enabled
                f.isBezeled = true
                f.delegate = self
                f.tag = i
                f.identifier = NSUserInterfaceItemIdentifier(key)
                f.widthAnchor.constraint(equalToConstant: width).isActive = true
                f.setAccessibilityIdentifier("\(AX.settingsNetwork)-\(key)-\(n)")
                f.setAccessibilityLabel("\(row.name), \(loc("settings.network.col.\(key)"))")
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
            let delete = NSButton(title: "", target: self, action: #selector(networkDeleteClicked(_:)))
            delete.image = Icon.fieldClear.image(loc("settings.network.delete"), compact: true)
            delete.isBordered = false
            delete.imagePosition = .imageOnly
            delete.tag = i
            delete.isEnabled = enabled
            delete.setAX("\(AX.settingsNetwork)-delete-\(n)", label: "\(loc("settings.network.delete")) \(row.name)")
            networkGrid.addRow(with: [
                field("name", row.name, width: 120), field("host", row.host, width: 140),
                field("port", String(row.port), width: 60), popup, delete,
            ])
            networkTransportPopups.append(popup)
            networkDeleteButtons.append(delete)
        }
        networkGrid.layoutSubtreeIfNeeded()
    }

    /// After a button or pop-up of the table: a field still being typed in is committed first (its
    /// commit is queued), since the table is not rebuilt under a caret and would never show the change.
    private func redrawNetwork() {
        if networkFields.contains(where: { $0.currentEditor() != nil }) { window?.makeFirstResponder(nil) }
        lastDrawn = nil
        shownNetwork = nil
        render()
    }

    @objc private func networkAddClicked() {
        Task {
            await model.addNetworkPatch()
            redrawNetwork()
        }
    }

    @objc private func networkDeleteClicked(_ sender: NSButton) {
        guard let rows = shownNetwork, rows.indices.contains(sender.tag) else { return }
        let id = rows[sender.tag].id
        Task {
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
        Task {
            await model.setNetworkPatchTransport(id, transport)
            redrawNetwork()
        }
    }

    /// A field of the table was committed (Return or leaving it): the engine takes it, or the old
    /// text comes back with a line saying why not. Commits run one after another.
    func networkFieldCommitted(_ field: NSTextField) {
        guard let rows = shownNetwork, rows.indices.contains(field.tag), let key = field.identifier?.rawValue else {
            return
        }
        let row = rows[field.tag]
        let text = field.stringValue
        let old = key == "name" ? row.name : key == "host" ? row.host : String(row.port)
        guard text != old else { return }
        let previous = networkCommits
        networkCommits = Task {
            await previous?.value
            let taken: Bool
            switch key {
            case "name": taken = await model.setNetworkPatchName(row.id, text)
            case "host": taken = await model.setNetworkPatchHost(row.id, text)
            default: taken = await model.setNetworkPatchPort(row.id, text)
            }
            if !taken { field.stringValue = old }
            redrawNetwork()
        }
    }
}
