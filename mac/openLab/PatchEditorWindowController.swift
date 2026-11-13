// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The patch editor (AUD-07, PB-11; QLab 5 manual pp. 189-190), opened from Edit in a row of the
/// workspace settings' patch table, from the I/O tab's edit button or from the Levels tab. At the
/// top: the patch's name, how many cue outputs it has and the device it plays on. Three tabs: the
/// cue outputs' names (what a cue's Levels tab shows under its faders); the routing matrix (the
/// main level, then a row per cue output with its level, meter and the device output it plays on);
/// and the device outputs with what reaches each.
///
/// It holds nothing the engine does not: every control is drawn from the model and the settings
/// model, and an edit goes to the engine (which puts a level within the workspace volume limits);
/// what comes back is what is shown.
@MainActor
final class PatchEditorWindowController: NSWindowController, NSTextFieldDelegate {
    let model: WorkspaceSettingsModel
    var store: WorkspaceStore { model.store }
    let patchID: ModelID
    /// Sized before its tabs are added: a zero-sized tab view lays its tab strip out at a negative width.
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))
    let nameField = NSTextField()
    let countField = NSTextField()
    let devicePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    /// Why the patch cannot play now (QLab 5 manual p. 191), or nothing.
    let problemLabel = NSTextField(wrappingLabelWithString: "")
    // Cue outputs
    private(set) var outputNameFields: [NSTextField] = []
    private let namesStack = NSStackView()
    // Routing
    let mainFader = LevelFader(value: 0, minValue: LevelText.floorDb, maxValue: 0, target: nil, action: nil)
    let mainField = NSTextField()
    let matrix = PatchRoutingView()
    let meter = MeterView()
    /// What the routing tab cannot show or do here: openLab's limits on this device and this patch.
    let routingNote = NSTextField(wrappingLabelWithString: "")
    /// One level box per cue output, laid over the matrix's level column.
    private(set) var outputFields: [NSTextField] = []
    // Device outputs
    let deviceOutputsNote = NSTextField(wrappingLabelWithString: "")
    private(set) var deviceOutputLabels: [NSTextField] = []
    private let deviceGrid = NSGridView()

    let doneButton = NSButton(title: loc("patchEditor.done"), target: nil, action: nil)
    /// Why the last edit did not take.
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    private var subs: [AnyCancellable] = []
    /// The field whose text was typed and not yet committed: a model update leaves it alone.
    private var typedField: NSTextField?
    /// A fader drag in progress: the edit belongs to one undo step and the model waits for its end.
    private var dragging = false
    /// Each drag's own undo step: its edits coalesce with each other and no other drag's.
    private var gesture = 0
    /// A coalesce key lives as long as the engine's history, which outlives the app.
    private static let launchToken = UUID().uuidString
    /// The most recent edit on its way; tests wait on it.
    private(set) var editing: Task<Void, Never>?

    private struct Drawn: Equatable {
        var limits: VolumeLimits
        var main: Double
        var levels: [Double]
        var names: [String]
        var name: String
        var row: PatchRow?
        var devices: [DeviceEntry]
        var usable: Int
        var loaded: Bool
    }
    private var drawn: Drawn?

    static let minSize = NSSize(width: 520, height: 420)

    init(model: WorkspaceSettingsModel, patch: ModelID) {
        self.model = model
        patchID = patch
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 540),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.identifier = NSUserInterfaceItemIdentifier(AX.patchEditorWindow)
        window.isReleasedWhenClosed = false
        window.contentMinSize = Self.minSize
        super.init(window: window)
        window.contentView = buildContent()
        window.center()
        let later: (Any) -> Void = { [weak self] _ in DispatchQueue.main.async { self?.render() } }
        subs.append(store.$model.sink(receiveValue: later))
        subs.append(model.$patches.sink(receiveValue: later))
        subs.append(model.$devices.sink(receiveValue: later))
        subs.append(model.$message.sink { [weak self] text in self?.messageLabel.stringValue = text ?? "" })
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Opens with no field focused: AppKit would put the caret in the first box, where the next Space
    /// (the GO key, in the hands of an operator mid-show) would replace what is there. Reads the
    /// devices and routing again.
    func present() {
        showWindow(nil)
        window?.makeFirstResponder(nil)
        reread()
    }

    /// Reads the devices and routing again, then draws them.
    func reread() {
        editing = Task {
            await model.refresh()
            drawn = nil
            render()
        }
    }

    // MARK: Building

    private func buildContent() -> NSView {
        nameField.setAX(AX.patchEditorName, label: loc("patchEditor.name"))
        nameField.delegate = self
        nameField.lineBreakMode = .byTruncatingTail
        nameField.cell?.usesSingleLineMode = true
        nameField.widthAnchor.constraint(equalToConstant: 160).isActive = true
        countField.setAX(AX.patchEditorCount, label: loc("patchEditor.count"))
        countField.delegate = self
        countField.alignment = .right
        countField.widthAnchor.constraint(equalToConstant: 56).isActive = true
        devicePopup.setAX(AX.patchEditorDevice, label: loc("patchEditor.device"))
        devicePopup.target = self
        devicePopup.action = #selector(deviceChosen)
        devicePopup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        devicePopup.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        devicePopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 140).isActive = true
        func titled(_ title: String, _ control: NSView) -> NSStackView {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: 11)
            label.textColor = .secondaryLabelColor
            let stack = NSStackView(views: [label, control])
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.spacing = 3
            return stack
        }
        let deviceColumn = titled(loc("patchEditor.device"), devicePopup)
        deviceColumn.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        devicePopup.widthAnchor.constraint(equalTo: deviceColumn.widthAnchor).isActive = true
        let top = NSStackView(views: [
            titled(loc("patchEditor.name"), nameField), titled(loc("patchEditor.count"), countField), deviceColumn,
        ])
        top.orientation = .horizontal
        top.alignment = .top
        top.spacing = 14
        problemLabel.textColor = .systemRed
        problemLabel.setAX(AX.patchEditorProblem)
        problemLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)

        for (label, view, id) in [
            (loc("patchEditor.tab.cueOutputs"), buildNamesTab(), "cueOutputs"),
            (loc("patchEditor.tab.routing"), buildRoutingTab(), "routing"),
            (loc("patchEditor.tab.deviceOutputs"), buildDeviceTab(), "deviceOutputs"),
        ] {
            let item = NSTabViewItem(identifier: id)
            item.label = label
            item.view = view
            tabs.addTabViewItem(item)
        }
        tabs.selectTabViewItem(withIdentifier: "routing")
        tabs.setAX(AX.patchEditorTabs)
        tabs.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        tabs.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)

        doneButton.setAX(AX.patchEditorDone, label: loc("patchEditor.done"))
        doneButton.keyEquivalent = "\r"
        doneButton.target = self
        doneButton.action = #selector(doneClicked)
        messageLabel.textColor = .systemRed
        messageLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        messageLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let bottom = NSStackView(views: [messageLabel, doneButton])
        bottom.orientation = .horizontal
        bottom.alignment = .centerY
        bottom.distribution = .fill
        doneButton.setContentHuggingPriority(.required, for: .horizontal)

        let root = NSStackView(views: [top, problemLabel, tabs, bottom])
        root.orientation = .vertical
        root.alignment = .width
        root.spacing = 10
        root.distribution = .fill
        root.edgeInsets = NSEdgeInsets(top: 12, left: 14, bottom: 12, right: 14)
        for label in [problemLabel, messageLabel] { label.preferredMaxLayoutWidth = 480 }
        return root
    }

    /// A scroll view whose flipped document view is as wide as the scroll view and as tall as `content`.
    private func scrolling(_ content: NSView, label: String) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        let holder = FlippedView()
        holder.translatesAutoresizingMaskIntoConstraints = false
        content.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(content)
        scroll.documentView = holder
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: holder.topAnchor, constant: 10),
            content.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 12),
            content.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -12),
            content.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -10),
            holder.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
        ])
        scroll.setAccessibilityLabel(label)
        return scroll
    }

    private func buildNamesTab() -> NSView {
        namesStack.orientation = .vertical
        namesStack.alignment = .leading
        namesStack.spacing = 6
        let scroll = scrolling(namesStack, label: loc("patchEditor.tab.cueOutputs"))
        scroll.setAccessibilityIdentifier("patch-editor-cue-outputs")
        return scroll
    }

    private func buildRoutingTab() -> NSView {
        mainFader.isVertical = true
        mainFader.isContinuous = true
        mainFader.setAX(AX.patchEditorMainFader, label: loc("patchEditor.main"))
        mainFader.target = self
        mainFader.action = #selector(faderMoved(_:))
        mainFader.onGestureBegin = { [weak self] in
            self?.dragging = true
            self?.gesture += 1
        }
        mainFader.onGestureEnd = { [weak self] in
            self?.dragging = false
            self?.render()
        }
        mainFader.heightAnchor.constraint(equalToConstant: 96).isActive = true
        mainField.setAX(AX.patchEditorMainField, label: loc("patchEditor.main"))
        mainField.alignment = .right
        mainField.delegate = self
        mainField.widthAnchor.constraint(equalToConstant: 56).isActive = true
        let mainLabel = NSTextField(labelWithString: loc("patchEditor.main"))
        let main = NSStackView(views: [mainFader, mainField, mainLabel])
        main.orientation = .vertical
        main.alignment = .centerX
        main.spacing = 4
        main.setContentHuggingPriority(.required, for: .horizontal)
        routingNote.font = .systemFont(ofSize: 11)
        routingNote.textColor = .secondaryLabelColor
        routingNote.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        routingNote.preferredMaxLayoutWidth = 360
        let head = NSStackView(views: [main, routingNote])
        head.orientation = .horizontal
        head.alignment = .top
        head.spacing = 16

        matrix.setAX(AX.patchEditorMatrix, label: loc("patchEditor.tab.routing"))
        matrix.onRoute = { [weak self] row, column in self?.route(row, to: column) }
        meter.bank = store.meters
        meter.setAX(AX.patchEditorMeter)
        matrix.addSubview(meter)
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        scroll.documentView = matrix
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        scroll.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)

        let stack = NSStackView(views: [head, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 10, left: 12, bottom: 10, right: 12)
        scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -24).isActive = true
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 90).isActive = true
        return stack
    }

    private func buildDeviceTab() -> NSView {
        deviceOutputsNote.font = .systemFont(ofSize: 11)
        deviceOutputsNote.textColor = .secondaryLabelColor
        deviceOutputsNote.preferredMaxLayoutWidth = 440
        deviceOutputsNote.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        deviceGrid.rowSpacing = 6
        deviceGrid.columnSpacing = 12
        let stack = NSStackView(views: [deviceOutputsNote, deviceGrid])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        let scroll = scrolling(stack, label: loc("patchEditor.tab.deviceOutputs"))
        scroll.setAccessibilityIdentifier(AX.patchEditorDeviceOutputs)
        deviceOutputsNote.widthAnchor.constraint(lessThanOrEqualTo: scroll.widthAnchor, constant: -24).isActive = true
        return scroll
    }

    // MARK: Drawing the model

    private var patch: ModelObject? { store.model?.patches.first { $0.id == patchID } }
    private var row: PatchRow? { model.patches.first { $0.id == patchID } }
    private var limits: VolumeLimits { store.volumeLimits }

    /// Redraws every control from the model, except a field being typed in or a fader being dragged.
    func render() {
        guard let patch else {
            window?.close()  // the patch is gone
            return
        }
        let outputs = patch.raw["outputs"]?.arrayValue ?? []
        let row = self.row
        let state = Drawn(
            limits: limits, main: patch.raw["master_db"]?.doubleValue ?? 0,
            levels: outputs.map { $0["level_db"]?.doubleValue ?? 0 },
            names: outputs.map { $0["name"]?.stringValue ?? "" }, name: patch.raw["name"]?.stringValue ?? "",
            row: row, devices: model.devices, usable: row?.mapping.map { model.usableChannels(of: $0.device) } ?? 0,
            loaded: model.loaded)
        if let drawn, drawn == state, typedField == nil, !dragging { return }
        guard !dragging else { return }
        let rebuild = drawn?.levels.count != state.levels.count
        drawn = state
        let shownName = state.name.isEmpty ? loc("settings.audio.untitledPatch") : state.name
        window?.title = String(format: loc("patchEditor.title"), shownName)
        let editable = store.model != nil
        set(nameField, state.name, enabled: editable)
        set(countField, String(state.levels.count), enabled: editable)

        if let row {
            model.fillDevicePopupItems(devicePopup, for: row)
        }
        devicePopup.isEnabled = state.loaded && row != nil
        problemLabel.stringValue = row.flatMap { model.problem(of: $0) }.map(model.problemText) ?? ""
        problemLabel.isHidden = problemLabel.stringValue.isEmpty

        if rebuild { rebuildRows(count: state.levels.count) }
        for (i, name) in state.names.enumerated() {
            set(outputNameFields[i], name, enabled: editable)
            outputNameFields[i].placeholderString = String(i + 1)
        }
        mainFader.minValue = state.limits.min
        mainFader.maxValue = state.limits.max
        mainFader.doubleValue = min(max(state.main, state.limits.min), state.limits.max)
        mainFader.isEnabled = editable
        set(mainField, LevelText.format(state.main), enabled: editable)
        for (i, db) in state.levels.enumerated() { set(outputFields[i], LevelText.format(db), enabled: editable) }

        // The matrix: one row per cue output, one column per device output.
        let device = row?.mapping?.device
        let channels = device.map(model.channelCount(of:)) ?? 0
        let maxRoute = row?.mapping?.channels.map { Int($0) + 1 }.max() ?? 0
        matrix.rowLabels = state.names.enumerated().map { i, name in name.isEmpty ? "\(i + 1)" : "\(i + 1)  \(name)" }
        matrix.deviceOutputs = max(channels, maxRoute)
        matrix.usable = state.usable
        matrix.routes = (0..<state.levels.count).map { i in
            guard let c = row?.mapping?.channels, c.indices.contains(i) else { return nil }
            return Int(c[i])
        }
        matrix.isEnabled = state.loaded && row?.mapping != nil
        let metered = min(state.levels.count, MeterBank.maxChannels)
        meter.frame = matrix.meterRect(rows: metered)
        meter.fixedRows = metered
        meter.source = .output(patchID)

        var notes: [String] = []
        if row?.mapping == nil { notes.append(loc("patchEditor.noDevice")) } else {
            notes.append(loc("patchEditor.routeHelp"))
        }
        if channels > state.usable, row?.mapping != nil {
            notes.append(String(format: loc("patchEditor.usable"), state.usable, channels))
        }
        if state.levels.count > MeterBank.maxChannels {
            notes.append(loc("patchEditor.metersShown"))
            notes.append(loc("patchEditor.levelsShown"))
        }
        routingNote.stringValue = notes.joined(separator: " ")
        deviceOutputsNote.stringValue =
            row?.mapping == nil
            ? loc("patchEditor.noDevice")
            : channels > state.usable ? String(format: loc("patchEditor.usable"), state.usable, channels) : ""
        deviceOutputsNote.isHidden = deviceOutputsNote.stringValue.isEmpty
        renderDeviceOutputs(channels: max(channels, maxRoute), state: state)
    }

    /// A field shows `text` unless it is being typed in.
    private func set(_ field: NSTextField, _ text: String, enabled: Bool) {
        field.isEnabled = enabled
        if field !== typedField, field.currentEditor() == nil { field.stringValue = text }
    }

    private func rebuildRows(count: Int) {
        for v in namesStack.arrangedSubviews {
            namesStack.removeArrangedSubview(v)
            v.removeFromSuperview()
        }
        outputNameFields = []
        for f in outputFields { f.removeFromSuperview() }
        outputFields = []
        for i in 0..<count {
            let label = NSTextField(labelWithString: String(format: loc("patchEditor.cueOutput"), i + 1))
            label.alignment = .right
            label.widthAnchor.constraint(equalToConstant: 110).isActive = true
            let name = NSTextField()
            name.delegate = self
            name.tag = i
            name.lineBreakMode = .byTruncatingTail
            name.cell?.usesSingleLineMode = true
            name.widthAnchor.constraint(equalToConstant: 180).isActive = true
            name.setAX("\(AX.patchEditorOutputName)-\(i + 1)", label: String(format: loc("patchEditor.cueOutput"), i + 1))
            let line = NSStackView(views: [label, name])
            line.spacing = 8
            namesStack.addArrangedSubview(line)
            outputNameFields.append(name)

            let level = NSTextField(frame: matrix.levelRect(row: i))
            level.alignment = .right
            level.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
            level.controlSize = .small
            level.delegate = self
            level.tag = i + 1
            level.setAX(
                "\(AX.patchEditorOutputField)-\(i + 1)",
                label: "\(String(format: loc("patchEditor.cueOutput"), i + 1)), \(loc("patchEditor.level"))")
            matrix.addSubview(level)
            outputFields.append(level)
        }
        mainField.tag = 0
    }

    private func renderDeviceOutputs(channels: Int, state: Drawn) {
        while deviceGrid.numberOfRows > 0 {
            let last = deviceGrid.row(at: deviceGrid.numberOfRows - 1)
            for c in 0..<last.numberOfCells { last.cell(at: c).contentView?.removeFromSuperview() }
            deviceGrid.removeRow(at: deviceGrid.numberOfRows - 1)
        }
        deviceOutputLabels = []
        let routes = matrix.routes
        for c in 0..<channels {
            let title = NSTextField(labelWithString: String(format: loc("patchEditor.deviceOutput"), c + 1))
            let feeders = routes.enumerated().compactMap { i, r -> String? in
                guard r == c else { return nil }
                return state.names[i].isEmpty ? "\(i + 1)" : "\(i + 1) \(state.names[i])"
            }
            let text =
                feeders.isEmpty
                ? loc("patchEditor.unused") : String(format: loc("patchEditor.feeds"), feeders.joined(separator: ", "))
            let detail = NSTextField(labelWithString: text)
            detail.textColor = c < state.usable ? .secondaryLabelColor : .tertiaryLabelColor
            detail.lineBreakMode = .byTruncatingTail
            detail.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            detail.widthAnchor.constraint(lessThanOrEqualToConstant: 320).isActive = true
            detail.setAX("\(AX.patchEditorDeviceOutputs)-\(c + 1)")
            deviceGrid.addRow(with: [title, detail])
            deviceOutputLabels.append(detail)
        }
    }

    // MARK: Editing

    /// Runs one edit after the last, then shows what the engine holds.
    private func edit(_ body: @escaping @MainActor () async -> Void) {
        let previous = editing
        editing = Task {
            await previous?.value
            await body()
            drawn = nil
            render()
        }
    }

    /// Sends one level. `coalesce` joins a drag's edits into one undo step.
    private func send(_ db: Double, tag: Int, coalesce: String?) {
        let command = Command.setPatchLevel(patchID, output: tag == 0 ? nil : tag - 1, db: db)
        edit { [self] in
            switch await store.send(command: command, coalesce: coalesce) {
            case .success: model.message = nil
            case .failure(let error):
                model.message = String(format: loc("patchEditor.refused"), error.detailText)
            }
        }
    }

    @objc private func faderMoved(_ sender: LevelFader) {
        let db = LevelText.clamp((sender.doubleValue * 10).rounded() / 10, limits: limits)
        let key = dragging ? "patch-fader:\(patchID.raw):\(Self.launchToken):\(gesture):0" : nil
        send(db, tag: 0, coalesce: key)
        // The field follows the fader as it moves; the model confirms it.
        mainField.stringValue = LevelText.format(db)
    }

    @objc private func deviceChosen() {
        guard let row else { return }
        let name = devicePopup.selectedItem?.representedObject as? String
        edit { [self] in await model.setMapping(row, device: name) }
    }

    private func route(_ output: Int, to column: Int) {
        guard let row else { return }
        edit { [self] in await model.route(row, output: output, to: UInt32(column)) }
    }

    func controlTextDidChange(_ obj: Notification) { typedField = obj.object as? NSTextField }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        // Esc aborts the editor, which posts no end of editing: the typing ends here.
        if selector == #selector(NSResponder.cancelOperation(_:)) { typedField = nil }
        return control.endTyping(by: selector)
    }

    /// Every way out of typing: Return, Tab or a click elsewhere commits the text that was typed, as
    /// the cue levels do; a field only clicked into writes nothing.
    func controlTextDidEndEditing(_ obj: Notification) {
        let typed = typedField
        typedField = nil
        guard let field = obj.object as? NSTextField, field === typed else {
            drawn = nil
            render()
            return
        }
        let text = field.stringValue
        if field === nameField {
            edit { [self] in await model.renamePatch(patchID, to: text) }
        } else if field === countField {
            guard let count = Int(text.trimmingCharacters(in: .whitespaces)) else {
                model.message = loc("patchEditor.countRange")
                drawn = nil
                return render()
            }
            edit { [self] in await model.setOutputCount(patchID, count) }
        } else if let i = outputNameFields.firstIndex(where: { $0 === field }) {
            edit { [self] in await model.renameOutput(patchID, output: i, to: text) }
        } else if LevelText.isStraySpace(text) {
            // Emptied is -inf (QLab 5 manual p. 204). Only white space is no change: a stray Space,
            // the GO key, leaves that (ours).
            drawn = nil
            render()
        } else {
            send(LevelText.parse(text, limits: limits), tag: field.tag, coalesce: nil)
        }
    }

    @objc private func doneClicked() { window?.close() }
}

extension WorkspaceSettingsModel {
    /// The device choices of a patch, as the table and the patch editor show them: none, then every
    /// listed device, and the device the patch names when that is not connected now.
    func fillDevicePopupItems(_ popup: NSPopUpButton, for row: PatchRow) {
        popup.removeAllItems()
        popup.addItem(withTitle: loc("settings.audio.noDevice"))
        popup.lastItem?.representedObject = nil
        for name in deviceChoices(for: row) {
            let listed = devices.contains { $0.name == name }
            popup.addItem(withTitle: listed ? name : String(format: loc("settings.audio.notConnected"), name))
            popup.lastItem?.representedObject = name
        }
        let current = popup.itemArray.firstIndex { ($0.representedObject as? String) == row.mapping?.device }
        popup.selectItem(at: current ?? 0)
    }

    func problemText(_ problem: PatchProblem) -> String {
        switch problem {
        case .unmapped: return loc("settings.audio.problem.unmapped")
        case .deviceMissing(let name): return String(format: loc("settings.audio.problem.deviceMissing"), name)
        case .channelsMissing(let device, let needed, let available):
            return String(format: loc("settings.audio.problem.channelsMissing"), device, available, needed)
        case .outputCountMismatch: return loc("settings.audio.problem.outputCount")
        }
    }
}
