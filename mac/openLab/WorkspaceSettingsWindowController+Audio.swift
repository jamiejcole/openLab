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

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        let holder = patchGridHolder
        holder.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = holder
        placeNewPatchGrid()
        NSLayoutConstraint.activate([
            holder.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            holder.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            // The table is as wide as the pane; its device column is sized from the scroll view less a
            // scroller's width, whether one shows or not, so the rows do not move when one appears.
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
            field.widthAnchor.constraint(equalToConstant: 48).isActive = true
        }
        // The fields are empty until the show is in.
        maxLimitField.engineText = { [model] in model.store.model == nil ? nil : Self.format(model.limits.max) }
        maxLimitField.send = { [model] in await model.setOutputMax($0) }
        minLimitField.engineText = { [model] in model.store.model == nil ? nil : Self.format(model.limits.min) }
        minLimitField.send = { [model] in await model.setOutputMin($0) }
        // The labels share one width, so the two fields line up.
        func limitRow(_ title: String, _ field: NSTextField) -> NSStackView {
            let label = NSTextField(labelWithString: title)
            label.widthAnchor.constraint(equalToConstant: 34).isActive = true
            let row = NSStackView(views: [label, field, NSTextField(labelWithString: loc("settings.db"))])
            row.spacing = 6
            return row
        }
        let limitsTitle = NSTextField(labelWithString: loc("settings.audio.limits"))
        limitsTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        func limitNote(_ key: String) -> NSTextField {
            let label = NSTextField(wrappingLabelWithString: loc(key))
            label.font = .systemFont(ofSize: 11)
            label.textColor = .secondaryLabelColor
            label.preferredMaxLayoutWidth = 140
            label.widthAnchor.constraint(equalToConstant: 140).isActive = true
            return label
        }
        let limits = NSStackView(views: [
            limitsTitle, limitRow(loc("settings.audio.limitMaxShort"), maxLimitField),
            limitNote("settings.audio.limitMaxNote"), limitRow(loc("settings.audio.limitMinShort"), minLimitField),
            limitNote("settings.audio.limitMinNote"),
        ])
        limits.orientation = .vertical
        limits.alignment = .leading
        limits.spacing = 8
        // Its own size in both directions: the table takes the spare width, nothing spreads the
        // limits' rows apart.
        for orientation in [NSLayoutConstraint.Orientation.horizontal, .vertical] {
            limits.setHuggingPriority(.required, for: orientation)
        }
        limits.setContentCompressionResistancePriority(.required, for: .horizontal)

        // A rule between the table and the limits, as in the reference.
        let divider = DividerView()
        divider.widthAnchor.constraint(equalToConstant: 1).isActive = true
        let body = NSStackView(views: [scroll, divider, limits])
        body.orientation = .horizontal
        body.alignment = .top
        body.spacing = 12
        body.distribution = .fill
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        scroll.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        scroll.heightAnchor.constraint(equalTo: body.heightAnchor).isActive = true
        divider.heightAnchor.constraint(equalTo: scroll.heightAnchor).isActive = true

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

    /// Narrow, so at the least window the device pop-up keeps the width it needs.
    static let patchNameWidth: CGFloat = 60
    static let editWidth: CGFloat = 56
    static let removeWidth: CGFloat = 22
    /// A vertical scroller's width (and the border), always kept free beside the rows.
    static var scrollerReserve: CGFloat { NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy) + 2 }
    /// The width of the table's columns other than the device's, with the spacing and insets.
    static var fixedColumns: CGFloat { 16 + patchNameWidth + editWidth + removeWidth + 3 * 8 }

    /// A new, empty table with its column titles in place of the old one. The table is made again
    /// when its rows change: a row whose cells are merged (the problem lines) cannot be removed from a
    /// grid view.
    private func placeNewPatchGrid() {
        patchGrid.removeFromSuperview()
        let grid = NSGridView()
        let headers = [
            loc("settings.audio.patchName"), loc("settings.audio.patchDevice"), "", "",
        ].map { text -> NSView in
            let label = NSTextField(labelWithString: text)
            label.font = .systemFont(ofSize: 12, weight: .semibold)
            return label
        }
        grid.addRow(with: headers)
        grid.rowSpacing = 6
        grid.columnSpacing = 8
        grid.yPlacement = .center
        grid.column(at: 0).width = Self.patchNameWidth
        grid.column(at: 2).xPlacement = .center
        grid.column(at: 3).xPlacement = .center
        grid.translatesAutoresizingMaskIntoConstraints = false
        let holder = patchGridHolder
        holder.addSubview(grid)
        NSLayoutConstraint.activate([
            grid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 8),
            grid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            grid.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -8),
            grid.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -8),
        ])
        patchGrid = grid
    }

    /// Draws the table: the rows built again when they changed, else drawn in place (`keepsRows`).
    func renderPatches(forced: Bool) {
        let enabled = model.store.model != nil
        patchAddButton.isEnabled = enabled
        let rows = model.patchRows
        let state = PatchTableState(patches: rows, devices: model.devices, loaded: model.loaded)
        if !forced, shownPatchTable == state { return }
        shownPatchTable = state
        if !keepsRows(patchNameFields, now: rows.map(\.id.raw), in: patchGrid) { buildPatchRows(rows) }
        for (i, field) in patchNameFields.enumerated() {
            let row = rows.first { $0.id.raw == field.row }
            show(field)
            field.allow(editable: enabled)
            let popup = patchDevicePopups[i]
            if let row, !edits.isWaiting(Self.patchKey(row.id, "device")) {
                model.fillDevicePopupItems(popup, for: row)
                // The name is cut in a narrow window: the whole of it is the tip.
                popup.toolTip = popup.titleOfSelectedItem
            }
            popup.isEnabled = model.loaded && row != nil
            patchEditButtons[i].isEnabled = row != nil
            patchRemoveButtons[i].isEnabled = enabled && row != nil && rows.count > 1
            let problem = patchProblemLabels[i]
            problem.stringValue = row.flatMap(model.problem(of:)).map(model.problemText) ?? ""
            patchGrid.cell(for: problem)?.row?.isHidden = problem.stringValue.isEmpty
            let name = row.map { $0.name.isEmpty ? loc("settings.audio.untitledPatch") : $0.name } ?? ""
            field.setAccessibilityLabel("\(name), \(loc("settings.audio.patchName"))")
            popup.setAccessibilityLabel("\(name), \(loc("settings.audio.patchDevice"))")
            patchEditButtons[i].setAccessibilityLabel("\(loc("settings.audio.edit")) \(name)")
            patchRemoveButtons[i].setAccessibilityLabel("\(loc("settings.audio.removePatch")) \(name)")
        }
        patchGrid.layoutSubtreeIfNeeded()
        fitProblemLines()
    }

    private static func patchKey(_ id: ModelID, _ column: String) -> String { "patch-\(id.raw)-\(column)" }

    private func buildPatchRows(_ rows: [PatchRow]) {
        placeNewPatchGrid()
        patchNameFields = []
        patchDevicePopups = []
        patchEditButtons = []
        patchRemoveButtons = []
        patchProblemLabels = []
        for (i, row) in rows.enumerated() {
            let n = i + 1
            let id = row.id
            let name = SettingsField(key: Self.patchKey(id, "name"), row: id.raw)
            name.engineText = { [model] in
                model.store.model?.patches.first { $0.id == id }.map { $0.raw["name"]?.stringValue ?? "" }
            }
            name.send = { [model] in await model.setPatchName(id, $0) }
            name.isBezeled = true
            name.delegate = self
            name.lineBreakMode = .byTruncatingTail
            name.cell?.usesSingleLineMode = true
            name.widthAnchor.constraint(equalToConstant: Self.patchNameWidth).isActive = true
            name.setAX("\(AX.settingsPatch)-name-\(n)", label: "")

            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            popup.target = self
            popup.action = #selector(patchDeviceChosen(_:))
            // A long device name is cut: it never widens the window.
            popup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.widthAnchor.constraint(greaterThanOrEqualToConstant: 100).isActive = true
            popup.setAX("\(AX.settingsPatch)-device-\(n)", label: "")

            let edit = NSButton(
                title: loc("settings.audio.edit"), target: self, action: #selector(editPatchClicked(_:)))
            edit.bezelStyle = .rounded
            edit.widthAnchor.constraint(equalToConstant: Self.editWidth).isActive = true
            edit.setAX("\(AX.settingsPatch)-edit-\(n)", label: "")

            let remove = NSButton(title: "", target: self, action: #selector(patchRemoveClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.audio.removePatch"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.widthAnchor.constraint(equalToConstant: Self.removeWidth).isActive = true
            remove.toolTip = loc("settings.audio.removeHelp")
            remove.setAX("\(AX.settingsPatch)-remove-\(n)", label: "")
            for control in [popup, edit, remove] { control.identifier = NSUserInterfaceItemIdentifier(id.raw) }
            patchGrid.addRow(with: [name, popup, edit, remove])
            // The device column takes the width the table has to spare (QLab's table gives it most).
            // Made once the pop-up is in the table: a constraint to a view outside its window's
            // tree throws.
            let wide = popup.widthAnchor.constraint(
                equalTo: patchGridHolder.enclosingScrollView?.widthAnchor ?? patchGridHolder.widthAnchor,
                constant: -Self.fixedColumns - Self.scrollerReserve)
            wide.priority = NSLayoutConstraint.Priority(750)
            wide.isActive = true

            // Why the patch cannot play, under its row across the table; the text wraps in it.
            let problem = NSTextField(wrappingLabelWithString: "")
            problem.textColor = .systemRed
            problem.font = .systemFont(ofSize: 11)
            problem.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            // Wraps at the table's width (the label spans every column).
            let across = patchGridHolder.bounds.width - 16
            problem.preferredMaxLayoutWidth = across > 100 ? across : 260
            problem.setAX("\(AX.settingsPatch)-problem-\(n)")
            let problemRow = patchGrid.addRow(with: [problem])
            patchGrid.cell(atColumnIndex: 0, rowIndex: patchGrid.numberOfRows - 1).xPlacement = .fill
            patchGrid.mergeCells(
                inHorizontalRange: NSRange(location: 0, length: 4),
                verticalRange: NSRange(location: patchGrid.numberOfRows - 1, length: 1))
            problemRow.topPadding = -4

            patchNameFields.append(name)
            patchDevicePopups.append(popup)
            patchEditButtons.append(edit)
            patchRemoveButtons.append(remove)
            patchProblemLabels.append(problem)
        }
    }

    // MARK: Actions

    /// The patch a control stands for.
    private func patch(of control: NSControl) -> ModelID? { control.identifier.map { ModelID($0.rawValue) } }

    @objc func patchAddClicked() { act("patch-add") { [model] in await model.addPatch() != nil } }

    @objc private func patchRemoveClicked(_ sender: NSButton) {
        guard let id = patch(of: sender) else { return }
        act(Self.patchKey(id, "remove")) { [model] in
            // Gone already, as wanted: nothing is sent for it.
            guard model.store.model?.patches.contains(where: { $0.id == id }) == true else { return true }
            return await model.removePatch(id)
        }
    }

    @objc private func patchDeviceChosen(_ sender: NSPopUpButton) {
        guard let id = patch(of: sender) else { return }
        let choice = sender.selectedItem?.representedObject
        act(Self.patchKey(id, "device")) { [model] in
            guard let row = model.patchRows.first(where: { $0.id == id }) else { return model.rowGone() }
            return choice is SystemOutputChoice
                ? await model.setSystemOutput(row) : await model.setMapping(row, device: choice as? String)
        }
    }

    @objc private func editPatchClicked(_ sender: NSButton) {
        guard let id = patch(of: sender), model.store.model?.patches.contains(where: { $0.id == id }) == true
        else { return }
        openPatchEditor(id)
    }

    /// Opens the patch editor of `patch` (one window per patch; a second Edit brings it forward).
    @discardableResult
    func openPatchEditor(_ patch: ModelID) -> PatchEditorWindowController {
        let editor = patchEditors[patch] ?? commitsAsItGoes(PatchEditorWindowController(model: model, patch: patch))
        patchEditors[patch] = editor
        patchEditor = editor
        if presentsEditors { editor.present() } else { editor.reread() }
        return editor
    }
}

/// What the patch table was drawn from.
struct PatchTableState: Equatable {
    var patches: [PatchRow]
    var devices: [DeviceEntry]
    var loaded: Bool
}

/// A hairline rule one point wide, drawn in the separator colour.
@MainActor
private final class DividerView: NSView {
    override var isOpaque: Bool { false }
    override func draw(_ dirty: NSRect) {
        NSColor.separatorColor.setFill()
        bounds.fill()
    }
}
