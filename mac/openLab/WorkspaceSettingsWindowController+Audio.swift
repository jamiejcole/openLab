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
    /// for each drawing: a row whose cells are merged (the problem lines) cannot be removed from a
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

    /// Draws the table. One that is as drawn is left alone, and so is one whose name is being
    /// typed in: a rebuild would take the caret away (the next render catches up).
    func renderPatches(forced: Bool) {
        let enabled = model.store.model != nil
        patchAddButton.isEnabled = enabled
        let state = PatchTableState(patches: model.patches, devices: model.devices, loaded: model.loaded)
        if !forced, shownPatchTable == state { return }
        if patchNameFields.contains(where: { $0.currentEditor() != nil }) {
            // Drawn once the typing ends; nothing to catch up when the table is as drawn.
            if shownPatchTable != state { lastDrawn = nil }
            return
        }
        shownPatchTable = state
        placeNewPatchGrid()
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
            name.identifier = NSUserInterfaceItemIdentifier(row.id.raw)
            name.lineBreakMode = .byTruncatingTail
            name.cell?.usesSingleLineMode = true
            name.widthAnchor.constraint(equalToConstant: Self.patchNameWidth).isActive = true
            name.setAX("\(AX.settingsPatch)-name-\(n)", label: "\(shownName), \(loc("settings.audio.patchName"))")

            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            model.fillDevicePopupItems(popup, for: row)
            popup.tag = i
            popup.identifier = NSUserInterfaceItemIdentifier(row.id.raw)
            popup.target = self
            popup.action = #selector(patchDeviceChosen(_:))
            popup.isEnabled = model.loaded
            // The name is cut in a narrow window: the whole of it is the tip.
            popup.toolTip = popup.titleOfSelectedItem
            // A long device name is cut: it never widens the window.
            popup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.widthAnchor.constraint(greaterThanOrEqualToConstant: 100).isActive = true
            popup.setAX("\(AX.settingsPatch)-device-\(n)", label: "\(shownName), \(loc("settings.audio.patchDevice"))")

            let edit = NSButton(
                title: loc("settings.audio.edit"), target: self, action: #selector(editPatchClicked(_:)))
            edit.bezelStyle = .rounded
            edit.widthAnchor.constraint(equalToConstant: Self.editWidth).isActive = true
            edit.tag = i
            edit.identifier = NSUserInterfaceItemIdentifier(row.id.raw)
            edit.setAX("\(AX.settingsPatch)-edit-\(n)", label: "\(loc("settings.audio.edit")) \(shownName)")

            let remove = NSButton(title: "", target: self, action: #selector(patchRemoveClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.audio.removePatch"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.widthAnchor.constraint(equalToConstant: Self.removeWidth).isActive = true
            remove.toolTip = loc("settings.audio.removeHelp")
            remove.tag = i
            remove.identifier = NSUserInterfaceItemIdentifier(row.id.raw)
            remove.isEnabled = enabled && model.patches.count > 1
            remove.setAX("\(AX.settingsPatch)-remove-\(n)", label: "\(loc("settings.audio.removePatch")) \(shownName)")
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
            let problem = NSTextField(wrappingLabelWithString: model.problem(of: row).map(model.problemText) ?? "")
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
            problemRow.isHidden = problem.stringValue.isEmpty
            problemRow.topPadding = -4

            patchNameFields.append(name)
            patchDevicePopups.append(popup)
            patchEditButtons.append(edit)
            patchRemoveButtons.append(remove)
            patchProblemLabels.append(problem)
        }
        patchGrid.layoutSubtreeIfNeeded()
        fitProblemLines()
    }

    /// A name being typed in is committed at the click on a button or pop-up of the table (its
    /// commit is queued), since the table is not rebuilt under a caret.
    private func endPatchTyping() {
        if patchNameFields.contains(where: { $0.currentEditor() != nil }) { window?.makeFirstResponder(nil) }
    }

    /// After an action of the table: drawn again from the engine's state. A name clicked into since
    /// the click keeps its caret: the table is drawn once that typing ends.
    private func redrawPatches() {
        lastDrawn = nil
        render()
    }

    /// The row a control was drawn for, by the patch id it carries, from what the table shows: the
    /// table is not redrawn while a name holds the caret, so the model's list (and a control's
    /// position in it) may differ from what is on screen.
    private func row(of control: NSControl) -> PatchRow? {
        guard let id = control.identifier?.rawValue,
            let drawn = shownPatchTable?.patches.first(where: { $0.id.raw == id })
        else { return nil }
        // A patch another client removed since the table was drawn: nothing is sent for it (the
        // engine would refuse it with an id), and the table is read and drawn again.
        guard model.store.model?.patches.contains(where: { $0.id == drawn.id }) == true else {
            if !(control is NSTextField) { endPatchTyping() }
            patchActions = Task {
                await model.refresh()
                redrawPatches()
            }
            return nil
        }
        return drawn
    }

    @objc func patchAddClicked() {
        endPatchTyping()
        patchActions = Task {
            await model.addPatch()
            redrawPatches()
        }
    }

    @objc private func patchRemoveClicked(_ sender: NSButton) {
        guard let row = row(of: sender) else { return }
        endPatchTyping()
        patchActions = Task {
            await model.removePatch(row.id)
            redrawPatches()
        }
    }

    @objc private func patchDeviceChosen(_ sender: NSPopUpButton) {
        guard let row = row(of: sender) else { return }
        let choice = sender.selectedItem?.representedObject
        endPatchTyping()
        patchActions = Task {
            await model.choose(choice, for: row)
            redrawPatches()
        }
    }

    @objc private func editPatchClicked(_ sender: NSButton) {
        guard let row = row(of: sender) else { return }
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
        guard let row = row(of: field) else { return }
        let text = field.stringValue
        patchActions = Task {
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

/// A hairline rule one point wide, drawn in the separator colour.
@MainActor
private final class DividerView: NSView {
    override var isOpaque: Bool { false }
    override func draw(_ dirty: NSRect) {
        NSColor.separatorColor.setFill()
        bounds.fill()
    }
}
