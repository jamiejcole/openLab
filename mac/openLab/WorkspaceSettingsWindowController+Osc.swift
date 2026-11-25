// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The OSC tab of the workspace settings (NET-02): the passcode table. A fixed No Passcode row
/// first, then one row per passcode with its code (edited in place), View, Edit and Control
/// boxes and a delete button. Every control sends its edit to the engine and the table is drawn
/// from what the engine then holds, so a refused edit shows the old state again.
extension WorkspaceSettingsWindowController {
    private static let rightColumns: CGFloat = 64

    func buildOscTab() -> NSView {
        oscAddButton.setAX(AX.settingsOscAdd, label: loc("settings.osc.add"))
        oscAddButton.bezelStyle = .rounded
        oscAddButton.target = self
        oscAddButton.action = #selector(oscAddClicked)
        let note = NSTextField(wrappingLabelWithString: loc("settings.osc.note"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.preferredMaxLayoutWidth = 300
        let top = NSStackView(views: [oscAddButton, note])
        top.orientation = .horizontal
        top.spacing = 12

        // The column titles stay above the scrolling rows, with the same column widths.
        let headers = ["passcode", "view", "edit", "control", "delete"].map { key -> NSView in
            let label = NSTextField(labelWithString: loc("settings.osc.col.\(key)"))
            label.font = .systemFont(ofSize: 12, weight: .semibold)
            return label
        }
        let header = NSGridView(views: [headers])
        header.columnSpacing = 8
        header.translatesAutoresizingMaskIntoConstraints = false
        let headerHolder = NSView()
        headerHolder.addSubview(header)
        NSLayoutConstraint.activate([
            header.leadingAnchor.constraint(equalTo: headerHolder.leadingAnchor, constant: 9),
            header.topAnchor.constraint(equalTo: headerHolder.topAnchor),
            header.bottomAnchor.constraint(equalTo: headerHolder.bottomAnchor),
            header.trailingAnchor.constraint(lessThanOrEqualTo: headerHolder.trailingAnchor),
        ])

        oscNoneLabel.setAX(AX.settingsOsc + "-none-label")
        oscNoneRights = ["view", "edit", "control"].enumerated().map { index, key in
            let box = NSButton(checkboxWithTitle: "", target: self, action: #selector(oscNoneToggled(_:)))
            box.tag = index
            box.setAX(
                "\(AX.settingsOsc)-none-\(key)",
                label: "\(loc("settings.osc.noPasscode")), \(loc("settings.osc.col.\(key)"))")
            return box
        }
        oscGrid.addRow(with: [oscNoneLabel] + oscNoneRights + [NSGridCell.emptyContentView])

        oscGrid.rowSpacing = 8
        oscGrid.columnSpacing = 8
        for grid in [oscGrid, header] {
            grid.column(at: 0).width = 130
            for c in 1...4 {
                grid.column(at: c).width = Self.rightColumns
                grid.column(at: c).xPlacement = .center
            }
        }

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        // Flipped, so a table shorter than the box starts at its top.
        let holder = FlippedView()
        oscGrid.translatesAutoresizingMaskIntoConstraints = false
        holder.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(oscGrid)
        scroll.documentView = holder
        NSLayoutConstraint.activate([
            oscGrid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 8),
            oscGrid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            oscGrid.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -8),
            oscGrid.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -8),
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            holder.widthAnchor.constraint(greaterThanOrEqualTo: scroll.contentView.widthAnchor),
            scroll.widthAnchor.constraint(equalToConstant: 460),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 200),
        ])
        // It takes the height the pane gives it.
        scroll.setAX(AX.settingsOsc + "-table", label: loc("settings.tab.osc"))

        let stack = NSStackView(views: [buildOscAccessBlock(), top, headerHolder, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.distribution = .fill
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        top.setContentHuggingPriority(.required, for: .vertical)
        headerHolder.setContentHuggingPriority(.required, for: .vertical)
        stack.spacing = 6
        stack.setCustomSpacing(12, after: top)
        return stack
    }

    // MARK: Drawing

    /// The rights in column order, with the name each has in a box's identifier.
    private static let rightColumnNames: [(name: String, right: WritableKeyPath<OscRights, Bool>)] = [
        ("view", \.view), ("edit", \.edit), ("control", \.control),
    ]

    /// Draws the table: the rows built again when they changed, else drawn in place (`keepsRows`).
    func renderOsc(_ state: WorkspaceSettingsModel.DisplayState, forced: Bool) {
        let enabled = model.store.model != nil
        oscAddButton.isEnabled = enabled
        drawRights(state.oscNone, of: nil, in: oscNoneRights, enabled: enabled)
        if !forced, shownOsc == state.oscRows { return }
        shownOsc = state.oscRows
        if !keepsRows(oscCodeFields, now: state.oscRows.map(\.id), in: oscGrid) { buildOscRows(state.oscRows) }
        for (i, field) in oscCodeFields.enumerated() {
            let row = field.row.flatMap(model.oscRow)
            let code = row?.passcode ?? ""
            show(field)
            field.allow(editable: enabled)
            let boxes = [oscViewBoxes[i], oscEditBoxes[i], oscControlBoxes[i]]
            drawRights(row?.rights ?? .none, of: field.row, in: boxes, enabled: enabled && row != nil)
            for (box, column) in zip(boxes, Self.rightColumnNames) {
                box.setAccessibilityLabel("\(code), \(loc("settings.osc.col.\(column.name)"))")
            }
            oscDeleteButtons[i].isEnabled = enabled && row != nil
            oscDeleteButtons[i].setAccessibilityLabel("\(loc("settings.osc.delete")) \(code)")
        }
        oscGrid.layoutSubtreeIfNeeded()
    }

    private func buildOscRows(_ rows: [OscRow]) {
        // Row 0 is the fixed No Passcode row.
        while oscGrid.numberOfRows > 1 {
            // The grid keeps the views of a removed row in the window: take them out.
            let last = oscGrid.row(at: oscGrid.numberOfRows - 1)
            for c in 0..<last.numberOfCells { last.cell(at: c).contentView?.removeFromSuperview() }
            oscGrid.removeRow(at: oscGrid.numberOfRows - 1)
        }
        oscCodeFields = []
        oscViewBoxes = []
        oscEditBoxes = []
        oscControlBoxes = []
        oscDeleteButtons = []
        for (i, row) in rows.enumerated() {
            let n = i + 1
            let id = row.id
            let field = SettingsField(key: "osc-\(id)-code", row: id)
            field.engineText = { [model] in model.oscRow(id)?.passcode }
            field.send = { [model] in await model.changeOscPasscode(of: id, to: $0) }
            field.isBezeled = true
            field.delegate = self
            field.widthAnchor.constraint(equalToConstant: 100).isActive = true
            field.setAX("\(AX.settingsOsc)-code-\(n)", label: loc("settings.osc.col.passcode"))
            let boxes = Self.rightColumnNames.enumerated().map { index, column -> NSButton in
                let box = NSButton(checkboxWithTitle: "", target: self, action: #selector(oscRightToggled(_:)))
                box.tag = index
                box.identifier = NSUserInterfaceItemIdentifier(id)
                box.setAX("\(AX.settingsOsc)-\(column.name)-\(n)", label: "")
                return box
            }
            let delete = NSButton(title: "", target: self, action: #selector(oscDeleteClicked(_:)))
            delete.image = NSImage(
                systemSymbolName: "xmark.circle.fill", accessibilityDescription: loc("settings.osc.delete"))
            delete.isBordered = false
            delete.imagePosition = .imageOnly
            delete.identifier = NSUserInterfaceItemIdentifier(id)
            delete.setAX("\(AX.settingsOsc)-delete-\(n)", label: "")
            oscGrid.addRow(with: [field] + boxes + [delete])
            oscCodeFields.append(field)
            oscViewBoxes.append(boxes[0])
            oscEditBoxes.append(boxes[1])
            oscControlBoxes.append(boxes[2])
            oscDeleteButtons.append(delete)
        }
    }

    /// Edit and Control are available only with View on. A box clicked and not answered yet stays as
    /// clicked.
    private func drawRights(_ rights: OscRights, of row: String?, in boxes: [NSButton], enabled: Bool) {
        for (box, column) in zip(boxes, Self.rightColumnNames)
        where !edits.isWaiting(Self.rightKey(row, column.name)) {
            box.state = rights[keyPath: column.right] ? .on : .off
        }
        boxes[0].isEnabled = enabled
        boxes[1].isEnabled = enabled && rights.view
        boxes[2].isEnabled = enabled && rights.view
    }

    private static func rightKey(_ row: String?, _ name: String) -> String { "osc-\(row ?? "none")-\(name)" }

    // MARK: Actions

    @objc private func oscAddClicked() {
        act("osc-add") { [weak self, model] in
            guard await model.addOscPasscode() else { return false }
            self?.redraw()
            // From about ten rows on the new one is below the visible part of the table.
            if let last = self?.oscCodeFields.last { last.scrollToVisible(last.bounds) }
            return true
        }
    }

    @objc private func oscDeleteClicked(_ sender: NSButton) {
        guard let row = sender.identifier?.rawValue else { return }
        act("osc-\(row)-delete") { [model] in await model.removeOscPasscode(of: row) }
    }

    @objc private func oscNoneToggled(_ sender: NSButton) { rightClicked(sender, of: nil) }

    @objc private func oscRightToggled(_ sender: NSButton) {
        guard let row = sender.identifier?.rawValue else { return }
        rightClicked(sender, of: row)
    }

    /// Sends the one right clicked: the row's others are what the engine has when it is sent, so boxes
    /// clicked in quick succession never undo each other.
    private func rightClicked(_ box: NSButton, of row: String?) {
        let column = Self.rightColumnNames[box.tag]
        let on = box.state == .on
        act(Self.rightKey(row, column.name)) { [model] in await model.setOscRight(column.right, on, of: row) }
    }
}

/// The top of the OSC page: whether OSC may reach this workspace at all, where it listens and
/// whether that port is open, and this Mac's addresses (QLab 5, manual p. 112-113).
extension WorkspaceSettingsWindowController {
    func buildOscAccessBlock() -> NSView {
        oscEnabledCheck.setAX("settings-osc-enabled")
        oscEnabledCheck.target = self
        oscEnabledCheck.action = #selector(oscEnabledClicked)
        oscPortField.setAX("settings-osc-port", label: loc("settings.osc.port"))
        oscPortField.alignment = .right
        oscPortField.delegate = self
        oscPortField.engineText = { [model] in model.oscListening.map { String($0.port) } }
        oscPortField.send = { [model] in await model.setOscPort($0) }
        oscPortField.widthAnchor.constraint(equalToConstant: 70).isActive = true
        oscPortStatus.setAX("settings-osc-port-status")
        oscPortStatus.font = .systemFont(ofSize: 11)
        oscPortStatus.preferredMaxLayoutWidth = 420
        oscAddressesLabel.setAX("settings-osc-addresses")
        oscAddressesLabel.font = .systemFont(ofSize: 11)
        oscAddressesLabel.textColor = .secondaryLabelColor
        oscAddressesLabel.preferredMaxLayoutWidth = 420
        let portRow = row(loc("settings.osc.port"), oscPortField, nil)
        let block = NSStackView(views: [oscEnabledCheck, portRow, oscPortStatus, oscAddressesLabel])
        block.orientation = .vertical
        block.alignment = .leading
        block.spacing = 6
        return block
    }

    func renderOscAccess() {
        let on = model.isOn(.oscEnabled)
        oscEnabledCheck.state = on == true ? .on : .off
        oscEnabledCheck.isEnabled = on != nil
        let listening = model.oscListening
        oscPortField.allow(enabled: listening != nil)
        show(oscPortField)
        switch listening {
        case nil:
            oscPortStatus.stringValue = loc("settings.osc.noOsc")
            oscPortStatus.textColor = .secondaryLabelColor
        case let state? where state.open && state.error == nil && on == false:
            oscPortStatus.stringValue = String(format: loc("settings.osc.offOpen"), Int(state.port))
            oscPortStatus.textColor = .systemOrange
        case let state? where state.open && state.error == nil:
            oscPortStatus.stringValue = String(format: loc("settings.osc.open"), Int(state.port))
            oscPortStatus.textColor = .systemGreen
        case let state?:
            let reason = state.error ?? ""
            oscPortStatus.stringValue =
                state.open
                ? String(format: loc("settings.osc.stayedOpen"), Int(state.port), reason)
                : String(format: loc("settings.osc.notOpen"), Int(state.port), reason)
            oscPortStatus.textColor = .systemRed
        }
        let addresses = model.addresses
        if addresses.isEmpty {
            oscAddressesLabel.stringValue = loc("settings.osc.noAddresses")
        } else {
            oscAddressesLabel.stringValue = String(
                format: loc("settings.osc.addresses"), addresses.joined(separator: ", "))
        }
    }

    @objc private func oscEnabledClicked() {
        let on = oscEnabledCheck.state == .on
        Task { [weak self] in
            await self?.model.setOscEnabled(on)
            self?.redraw()
        }
    }
}
