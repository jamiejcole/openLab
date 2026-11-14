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
        let holder = TopLeftView()
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

        let stack = NSStackView(views: [top, headerHolder, scroll])
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

    /// Draws the table. A table that is as drawn is left alone, and so is one being typed in: a
    /// rebuild would take the caret away (the next render catches up).
    func renderOsc(_ table: OscAccessTable, forced: Bool) {
        let enabled = model.store.model != nil
        oscAddButton.isEnabled = enabled
        if !forced, shownOsc == table { return }
        if oscCodeFields.contains(where: { $0.currentEditor() != nil }) {
            lastDrawn = nil
            return
        }
        shownOsc = table
        drawRights(table.noPasscode, in: oscNoneRights, enabled: enabled)

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
        for (i, row) in table.rows.enumerated() {
            let n = i + 1
            let field = NSTextField(string: row.passcode)
            field.isEditable = enabled
            field.isBezeled = true
            field.delegate = self
            field.tag = i
            field.widthAnchor.constraint(equalToConstant: 100).isActive = true
            field.setAX("\(AX.settingsOsc)-code-\(n)", label: loc("settings.osc.col.passcode"))
            let boxes = ["view", "edit", "control"].map { key -> NSButton in
                let box = NSButton(checkboxWithTitle: "", target: self, action: #selector(oscRightToggled(_:)))
                box.tag = i
                box.setAX(
                    "\(AX.settingsOsc)-\(key)-\(n)", label: "\(row.passcode), \(loc("settings.osc.col.\(key)"))")
                return box
            }
            drawRights(row.rights, in: boxes, enabled: enabled)
            let delete = NSButton(title: "", target: self, action: #selector(oscDeleteClicked(_:)))
            delete.image = NSImage(
                systemSymbolName: "xmark.circle.fill", accessibilityDescription: loc("settings.osc.delete"))
            delete.isBordered = false
            delete.imagePosition = .imageOnly
            delete.tag = i
            delete.isEnabled = enabled
            delete.setAX("\(AX.settingsOsc)-delete-\(n)", label: "\(loc("settings.osc.delete")) \(row.passcode)")
            oscGrid.addRow(with: [field] + boxes + [delete])
            oscCodeFields.append(field)
            oscViewBoxes.append(boxes[0])
            oscEditBoxes.append(boxes[1])
            oscControlBoxes.append(boxes[2])
            oscDeleteButtons.append(delete)
        }
        oscGrid.layoutSubtreeIfNeeded()
    }

    /// Edit and Control are available only with View on.
    private func drawRights(_ rights: OscRights, in boxes: [NSButton], enabled: Bool) {
        for (box, on) in zip(boxes, [rights.view, rights.edit, rights.control]) {
            box.state = on ? .on : .off
        }
        boxes[0].isEnabled = enabled
        boxes[1].isEnabled = enabled && rights.view
        boxes[2].isEnabled = enabled && rights.view
    }

    /// Puts the engine's table back over whatever the controls show now.
    private func redrawOsc() {
        lastDrawn = nil
        render()
    }

    // MARK: Actions

    @objc private func oscAddClicked() {
        // A code still being typed is committed first: the table is not rebuilt under a caret.
        if oscCodeFields.contains(where: { $0.currentEditor() != nil }) { window?.makeFirstResponder(nil) }
        Task {
            await oscCommits?.value
            let added = await model.addOscPasscode()
            redrawOsc()
            // From about ten rows on the new one is below the visible part of the table.
            if added, let last = oscCodeFields.last { last.scrollToVisible(last.bounds) }
        }
    }

    @objc private func oscDeleteClicked(_ sender: NSButton) {
        guard let code = shownOsc?.rows[safe: sender.tag]?.passcode else { return }
        Task {
            await model.removeOscPasscode(code)
            redrawOsc()
        }
    }

    @objc private func oscNoneToggled(_ sender: NSButton) {
        let rights = Self.rights(of: oscNoneRights)
        Task {
            await model.setOscRights(rights, of: nil)
            redrawOsc()
        }
    }

    /// Sends what the row's boxes show: boxes clicked in quick succession each carry the others'
    /// new state, not the table's as of the first click.
    @objc private func oscRightToggled(_ sender: NSButton) {
        guard let code = shownOsc?.rows[safe: sender.tag]?.passcode, oscViewBoxes.indices.contains(sender.tag) else {
            return
        }
        let rights = Self.rights(of: [oscViewBoxes[sender.tag], oscEditBoxes[sender.tag], oscControlBoxes[sender.tag]])
        Task {
            await model.setOscRights(rights, of: code)
            redrawOsc()
        }
    }

    private static func rights(of boxes: [NSButton]) -> OscRights {
        OscRights(view: boxes[0].state == .on, edit: boxes[1].state == .on, control: boxes[2].state == .on)
    }

    /// A code field was committed (Return or leaving it): the engine takes the new code or the old
    /// one comes back with a line saying why not. Commits run one after another, each on the row
    /// its field stands for, so leaving one field for the next never drops either.
    func oscCodeCommitted(_ field: NSTextField) {
        let text = field.stringValue
        let tag = field.tag
        let previous = oscCommits
        oscCommits = Task {
            await previous?.value
            guard let old = shownOsc?.rows[safe: tag]?.passcode else { return }
            let taken = await model.changeOscPasscode(old, to: text)
            // The redraw waits while another code field is being typed in: this one shows the old code now.
            if !taken { field.stringValue = old }
            redrawOsc()
        }
    }
}

extension Array {
    fileprivate subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
