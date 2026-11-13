// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Audio tab of the workspace settings (AUD-07, QLab 5 manual pp. 106, 189): a table with one
/// row per output patch (its name, typed in place; the device it plays on; Edit, which opens the
/// patch editor; and a remove button), a button above it that adds a patch, and the workspace volume
/// limits beside it. A patch that cannot play has a line under its row saying why. Every control
/// sends its edit to the engine and the table is drawn from what the engine then holds.
extension WorkspaceSettingsWindowController {
    func buildAudioTab() -> NSView {
        patchAddButton.setAX(AX.settingsPatchAdd, label: loc("settings.audio.newPatch"))
        patchAddButton.bezelStyle = .rounded
        patchAddButton.target = self
        patchAddButton.action = #selector(patchAddClicked)
        let note = NSTextField(wrappingLabelWithString: loc("settings.audio.note"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let top = NSStackView(views: [patchAddButton, note])
        top.orientation = .horizontal
        top.spacing = 12

        let headers = [
            loc("settings.audio.patchName"), loc("settings.audio.patchDevice"), "", "",
        ].map { text -> NSView in
            let label = NSTextField(labelWithString: text)
            label.font = .systemFont(ofSize: 12, weight: .semibold)
            return label
        }
        patchGrid.addRow(with: headers)
        patchGrid.rowSpacing = 6
        patchGrid.columnSpacing = 8
        patchGrid.column(at: 0).width = Self.patchNameWidth
        patchGrid.column(at: 2).xPlacement = .center
        patchGrid.column(at: 3).xPlacement = .center

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        let holder = FlippedView()
        patchGrid.translatesAutoresizingMaskIntoConstraints = false
        holder.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(patchGrid)
        scroll.documentView = holder
        NSLayoutConstraint.activate([
            patchGrid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 8),
            patchGrid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            patchGrid.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -8),
            patchGrid.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -8),
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            // The table is as wide as the pane: the device column takes what the others leave.
            holder.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 120),
        ])
        scroll.setAX(AX.settingsPatchTable, label: loc("settings.audio.patches"))

        // The volume limits sit beside the table, as in QLab.
        maxLimitField.setAX(AX.settingsLimitMax, label: loc("settings.audio.limitMax"))
        minLimitField.setAX(AX.settingsLimitMin, label: loc("settings.audio.limitMin"))
        for field in [maxLimitField, minLimitField] {
            field.alignment = .right
            field.delegate = self
            field.widthAnchor.constraint(equalToConstant: 56).isActive = true
        }
        func limitRow(_ title: String, _ field: NSTextField) -> NSStackView {
            let row = NSStackView(views: [
                NSTextField(labelWithString: title), field, NSTextField(labelWithString: loc("settings.db")),
            ])
            row.spacing = 6
            return row
        }
        let limitsTitle = NSTextField(labelWithString: loc("settings.audio.limits"))
        limitsTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        let limits = NSStackView(views: [
            limitsTitle, limitRow(loc("settings.audio.limitMax"), maxLimitField),
            limitRow(loc("settings.audio.limitMin"), minLimitField),
        ])
        limits.orientation = .vertical
        limits.alignment = .trailing
        limits.spacing = 8
        limits.setContentHuggingPriority(.required, for: .horizontal)
        limits.setContentCompressionResistancePriority(.required, for: .horizontal)

        let body = NSStackView(views: [scroll, limits])
        body.orientation = .horizontal
        body.alignment = .top
        body.spacing = 12
        body.distribution = .fill
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        scroll.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        scroll.heightAnchor.constraint(equalTo: body.heightAnchor).isActive = true

        let stack = NSStackView(views: [top, body])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.distribution = .fill
        stack.spacing = 12
        body.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        body.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        top.setContentHuggingPriority(.required, for: .vertical)
        return stack
    }

    static let patchNameWidth: CGFloat = 140

    /// Draws the table. One that is as drawn is left alone, and so is one whose name is being
    /// typed in: a rebuild would take the caret away (the next render catches up).
    func renderPatches(forced: Bool) {
        let enabled = model.store.model != nil
        patchAddButton.isEnabled = enabled
        let state = PatchTableState(patches: model.patches, devices: model.devices, loaded: model.loaded)
        if !forced, shownPatchTable == state { return }
        if patchNameFields.contains(where: { $0.currentEditor() != nil }) {
            lastDrawn = nil
            return
        }
        shownPatchTable = state
        // Row 0 holds the column titles.
        while patchGrid.numberOfRows > 1 {
            let last = patchGrid.row(at: patchGrid.numberOfRows - 1)
            for c in 0..<last.numberOfCells { last.cell(at: c).contentView?.removeFromSuperview() }
            patchGrid.removeRow(at: patchGrid.numberOfRows - 1)
        }
        patchNameFields = []
        patchDevicePopups = []
        patchEditButtons = []
        patchRemoveButtons = []
        patchProblemLabels = []
        for (i, row) in model.patches.enumerated() {
            let n = i + 1
            let shownName = row.name.isEmpty ? loc("settings.audio.untitledPatch") : row.name
            let name = NSTextField(string: row.name)
            name.isEditable = enabled
            name.isBezeled = true
            name.delegate = self
            name.tag = i
            name.lineBreakMode = .byTruncatingTail
            name.cell?.usesSingleLineMode = true
            name.widthAnchor.constraint(equalToConstant: Self.patchNameWidth).isActive = true
            name.setAX("\(AX.settingsPatch)-name-\(n)", label: "\(shownName), \(loc("settings.audio.patchName"))")

            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            model.fillDevicePopupItems(popup, for: row)
            popup.tag = i
            popup.target = self
            popup.action = #selector(patchDeviceChosen(_:))
            popup.isEnabled = model.loaded
            // A long device name is cut: it never widens the window.
            popup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.widthAnchor.constraint(greaterThanOrEqualToConstant: 120).isActive = true
            popup.setAX("\(AX.settingsPatch)-device-\(n)", label: "\(shownName), \(loc("settings.audio.patchDevice"))")

            let edit = NSButton(title: loc("settings.audio.edit"), target: self, action: #selector(editPatchClicked(_:)))
            edit.bezelStyle = .rounded
            edit.tag = i
            edit.setAX("\(AX.settingsPatch)-edit-\(n)", label: "\(loc("settings.audio.edit")) \(shownName)")

            let remove = NSButton(title: "", target: self, action: #selector(patchRemoveClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.audio.removePatch"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.toolTip = loc("settings.audio.removeHelp")
            remove.tag = i
            remove.isEnabled = enabled && model.patches.count > 1
            remove.setAX("\(AX.settingsPatch)-remove-\(n)", label: "\(loc("settings.audio.removePatch")) \(shownName)")
            patchGrid.addRow(with: [name, popup, edit, remove])

            // Why the patch cannot play, under its row across the table; the text wraps in it.
            let problem = NSTextField(wrappingLabelWithString: model.problem(of: row).map(model.problemText) ?? "")
            problem.textColor = .systemRed
            problem.font = .systemFont(ofSize: 11)
            problem.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            problem.preferredMaxLayoutWidth = 260
            problem.setAX("\(AX.settingsPatch)-problem-\(n)")
            let problemRow = patchGrid.addRow(with: [problem])
            patchGrid.cell(atColumnIndex: 0, rowIndex: patchGrid.numberOfRows - 1).xPlacement = .fill
            patchGrid.mergeCells(
                inHorizontalRange: NSRange(location: 0, length: 4),
                verticalRange: NSRange(location: patchGrid.numberOfRows - 1, length: 1))
            problemRow.isHidden = problem.stringValue.isEmpty
            problemRow.topPadding = -4

            patchNameFields.append(name)
            patchDevicePopups.append(popup)
            patchEditButtons.append(edit)
            patchRemoveButtons.append(remove)
            patchProblemLabels.append(problem)
        }
        patchGrid.layoutSubtreeIfNeeded()
    }

    /// After a button or pop-up of the table: a name still being typed is committed first (its
    /// commit is queued), since the table is not rebuilt under a caret.
    private func redrawPatches() {
        if patchNameFields.contains(where: { $0.currentEditor() != nil }) { window?.makeFirstResponder(nil) }
        lastDrawn = nil
        shownPatchTable = nil
        render()
    }

    private func row(at index: Int) -> PatchRow? {
        model.patches.indices.contains(index) ? model.patches[index] : nil
    }

    @objc func patchAddClicked() {
        if patchNameFields.contains(where: { $0.currentEditor() != nil }) { window?.makeFirstResponder(nil) }
        Task {
            await model.addPatch()
            redrawPatches()
        }
    }

    @objc private func patchRemoveClicked(_ sender: NSButton) {
        guard let row = row(at: sender.tag) else { return }
        Task {
            await model.removePatch(row.id)
            redrawPatches()
        }
    }

    @objc private func patchDeviceChosen(_ sender: NSPopUpButton) {
        guard let row = row(at: sender.tag) else { return }
        let name = sender.selectedItem?.representedObject as? String
        Task {
            await model.setMapping(row, device: name)
            redrawPatches()
        }
    }

    @objc private func editPatchClicked(_ sender: NSButton) {
        guard let row = row(at: sender.tag) else { return }
        openPatchEditor(row.id)
    }

    /// Opens the patch editor of `patch` (one window per patch; a second Edit brings it forward).
    @discardableResult
    func openPatchEditor(_ patch: ModelID) -> PatchEditorWindowController {
        let editor = patchEditors[patch] ?? PatchEditorWindowController(model: model, patch: patch)
        patchEditors[patch] = editor
        patchEditor = editor
        if presentsEditors { editor.present() } else { editor.reread() }
        return editor
    }

    /// A patch name was committed (Return or leaving the field): sent; the engine's name comes back.
    func patchNameCommitted(_ field: NSTextField) {
        guard let row = row(at: field.tag) else { return }
        let text = field.stringValue
        Task {
            await model.renamePatch(row.id, to: text)
            redrawPatches()
        }
    }
}

/// What the patch table was drawn from.
struct PatchTableState: Equatable {
    var patches: [PatchRow]
    var devices: [DeviceEntry]
    var loaded: Bool
}
