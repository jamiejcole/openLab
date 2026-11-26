// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The patch editor (AUD-07, PB-11; QLab 5 manual pp. 189-190), opened from Edit in a row of the
/// workspace settings' patch table, from the I/O tab's edit button or from the Levels tab. At the
/// top: the patch's name, how many cue outputs it has and the device it plays on. Three tabs: the
/// cue outputs' names (what a cue's Levels tab shows under its faders); the routing matrix (the
/// main level, and a row per cue output with its level box, its meter (drawn only while sound plays
/// through it) and a box per device output holding
/// the level it reaches that device output at, under a strip of fader, mute and level box for the main
/// level and for each device output); and the device outputs, one titled row each. At the foot: copying the routing from another patch or back to the
/// defaults, and unmuting everything (QLab 5 manual pp. 189-190).
///
/// It holds nothing the engine does not: every control is drawn from the model and the settings
/// model, and an edit goes to the engine (which puts a level within the workspace volume limits);
/// what comes back is what is shown.
@MainActor
final class PatchEditorWindowController: NSWindowController, NSTextFieldDelegate, NSTabViewDelegate,
    NSWindowDelegate
{
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
    /// Each cue output's title on the Cue Outputs page.
    private(set) var outputTitles: [NSTextField] = []
    /// The titles of both name pages share one width and are right-aligned beside their fields.
    static let rowTitleWidth: CGFloat = 124
    private let namesStack = NSStackView()
    // Routing
    let mainFader = LevelFader(value: 0, minValue: LevelText.floorDb, maxValue: 0, target: nil, action: nil)
    let mainField = LevelBoxField()
    let matrix = PatchRoutingView()
    /// The matrix's header row and left columns, kept in view while it scrolls.
    let matrixHeader = PatchRoutingPane(.header)
    let matrixRows = PatchRoutingPane(.rows)
    /// Over both, at the scroll view's top left: the header row scrolls sideways under it.
    let matrixCorner = PatchRoutingPane(.corner)
    let meter = MeterView()
    /// What the routing tab cannot show or do here: openLab's limits on this device and this patch.
    let routingNote = NSTextField(wrappingLabelWithString: "")
    /// One level box per cue output, laid over the matrix's level column.
    private(set) var outputFields: [NSTextField] = []
    /// The box a crosspoint's level is typed in, laid over the box double-clicked.
    let crosspointField = LevelBoxField()
    /// The crosspoint being typed in (cue output, device output).
    private(set) var typingCell: (row: Int, column: Int)?
    // Device outputs
    let deviceOutputsNote = NSTextField(wrappingLabelWithString: "")
    /// Each device output's title on the Device Outputs page.
    private(set) var deviceTitles: [NSTextField] = []
    /// The strip over each device output's column of the routing grid: fader, mute and level box.
    private(set) var deviceFaders: [LevelFader] = []
    private(set) var deviceMutes: [NSButton] = []
    private(set) var deviceFields: [NSTextField] = []
    private let deviceGrid = NSGridView()

    /// Sets the routing from another patch of the workspace, or back to the defaults.
    let copyPopup = NSPopUpButton(frame: .zero, pullsDown: true)
    let unmuteButton = NSButton(title: loc("patchEditor.unmuteAll"), target: nil, action: nil)
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
    /// The device output whose fader is being dragged.
    private var draggedDeviceOutput: Int?
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
        var routing: PatchRouting
        var others: [String]
    }
    private var drawn: Drawn?
    /// Shown (or read for a window drawn elsewhere) and not closed since: it follows the devices.
    private var isOpen = false

    static let minSize = NSSize(width: 520, height: 460)
    /// Small enough that the longest level text drawn ("-119.9") shows whole in a box as wide as a crosspoint's.
    static let levelFontSize: CGFloat = 9

    init(model: WorkspaceSettingsModel, patch: ModelID) {
        self.model = model
        patchID = patch
        let window = PanicKeyWindow(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 540),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.identifier = NSUserInterfaceItemIdentifier(AX.patchEditorWindow)
        window.isReleasedWhenClosed = false
        window.contentMinSize = Self.minSize
        super.init(window: window)
        NotificationCenter.default.addObserver(
            self, selector: #selector(windowResized), name: NSWindow.didResizeNotification, object: window)
        window.contentView = buildContent()
        window.center()
        let later: (Any) -> Void = { [weak self] _ in DispatchQueue.main.async { self?.render() } }
        subs.append(store.$model.sink(receiveValue: later))
        subs.append(model.$patches.sink(receiveValue: later))
        subs.append(model.$devices.sink(receiveValue: later))
        subs.append(model.$message.sink { [weak self] text in self?.messageLabel.stringValue = text ?? "" })
        // A device lost, back or opened changes the device and the problem it shows: read again
        // while it is open, whether or not the settings window is (as the table does).
        let reread: (Any) -> Void = { [weak self] _ in
            guard let self, self.isOpen else { return }
            self.model.rereadSoon()
        }
        subs.append(store.$warnings.removeDuplicates().dropFirst().sink(receiveValue: reread))
        // The device list can lag a status by a read, and a device back is reopened in steps
        // (listed, opening, up with its outputs): every step reads again, and a status alone
        // redraws (the outputs openLab can use follow the clock device's), as the table does.
        subs.append(
            store.$status.map { $0?.outputs.map { "\($0.name)\u{0}\($0.state)\u{0}\($0.channels)" } }.removeDuplicates()
                .dropFirst().sink(receiveValue: reread))
        subs.append(store.$status.sink(receiveValue: later))
        // A read made as the device came back can still miss it (the engine lists devices on its
        // own schedule): while a device plays that the list lacks, each status reads again.
        subs.append(
            store.$status.sink { [weak self] status in
                guard let self, self.isOpen, let outputs = status?.outputs else { return }
                let listed = Set(self.model.devices.map(\.name))
                if outputs.contains(where: { $0.state == .up && !listed.contains($0.name) }) {
                    self.model.rereadSoon()
                }
            })
        window.delegate = self
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

    /// Reads the devices and routing again, then draws them (it is open from now until closed).
    func reread() {
        isOpen = true
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
        // Wide enough for its longest choice (up to its most, below) in every state, also when
        // the window first lays it out with a device away; it gives way to the window's size
        // (just below the priority a window keeps its size at).
        devicePopup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(490), for: .horizontal)
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
        // Grows before the spacer does, up to the pop-up's most.
        deviceColumn.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        devicePopup.widthAnchor.constraint(equalTo: deviceColumn.widthAnchor).isActive = true
        // The device column takes the spare width up to a pop-up's useful size; the spacer the rest.
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(2), for: .horizontal)
        spacer.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        devicePopup.widthAnchor.constraint(lessThanOrEqualToConstant: 360).isActive = true
        let top = NSStackView(views: [
            titled(loc("patchEditor.name"), nameField), titled(loc("patchEditor.count"), countField), deviceColumn,
            spacer,
        ])
        top.orientation = .horizontal
        top.alignment = .top
        top.distribution = .fill
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
        tabs.delegate = self
        tabs.setAX(AX.patchEditorTabs)
        tabs.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        tabs.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)

        doneButton.setAX(AX.patchEditorDone, label: loc("patchEditor.done"))
        doneButton.keyEquivalent = "\r"
        doneButton.target = self
        doneButton.action = #selector(doneClicked)
        copyPopup.setAX(AX.patchEditorCopy, label: loc("patchEditor.copyRouting"))
        copyPopup.target = self
        copyPopup.action = #selector(copyChosen)
        copyPopup.setContentHuggingPriority(.required, for: .horizontal)
        copyPopup.setContentCompressionResistancePriority(.required, for: .horizontal)
        unmuteButton.setAX(AX.patchEditorUnmute, label: loc("patchEditor.unmuteAll"))
        unmuteButton.target = self
        unmuteButton.action = #selector(unmuteClicked)
        unmuteButton.setContentHuggingPriority(.required, for: .horizontal)
        unmuteButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        messageLabel.textColor = .systemRed
        messageLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        messageLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let bottom = NSStackView(views: [copyPopup, unmuteButton, messageLabel, doneButton])
        bottom.orientation = .horizontal
        bottom.alignment = .centerY
        bottom.distribution = .fill
        doneButton.setContentHuggingPriority(.required, for: .horizontal)

        let root = NSStackView(views: [top, problemLabel, tabs, bottom])
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = 10
        root.distribution = .fill
        root.edgeInsets = NSEdgeInsets(top: 12, left: 14, bottom: 12, right: 14)
        for label in [problemLabel, messageLabel] { label.preferredMaxLayoutWidth = 480 }
        // Each row is as wide as the window, whatever its content would choose, so the header stays
        // at the top left at any size and in any state (Show Mode included).
        for row in [top, problemLabel, tabs, bottom] as [NSView] {
            row.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -28).isActive = true
        }
        return root
    }

    /// A scroll view whose flipped document view is as wide as the scroll view and as tall as `content`.
    private func scrolling(_ content: NSView, label: String, fillWidth: Bool = false) -> NSScrollView {
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
            fillWidth
                ? content.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -12)
                : content.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -12),
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
        let scroll = scrolling(namesStack, label: loc("patchEditor.tab.cueOutputs"), fillWidth: true)
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
        mainField.setAX(AX.patchEditorMainField, label: loc("patchEditor.main"))
        mainField.alignment = .center
        mainField.font = .monospacedDigitSystemFont(ofSize: Self.levelFontSize, weight: .regular)
        mainField.controlSize = .mini
        mainField.delegate = self
        matrixCorner.addSubview(mainFader)
        matrixCorner.addSubview(mainField)
        mainFader.frame = PatchRoutingView.faderFrame(midX: PatchRoutingView.mainMidX)
        mainField.frame = PatchRoutingView.mainBoxFrame
        routingNote.font = .systemFont(ofSize: 11)
        routingNote.textColor = .secondaryLabelColor
        routingNote.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        routingNote.preferredMaxLayoutWidth = 480

        matrix.setAX(AX.patchEditorMatrix, label: loc("patchEditor.tab.routing"))
        matrix.onType = { [weak self] row, column in self?.beginTyping(row: row, column: column) }
        crosspointField.alignment = .center
        crosspointField.font = .monospacedDigitSystemFont(ofSize: Self.levelFontSize, weight: .regular)
        crosspointField.controlSize = .mini
        crosspointField.delegate = self
        crosspointField.isHidden = true
        crosspointField.setAX(AX.patchEditorCrosspointField)
        matrix.addSubview(crosspointField)
        meter.bank = store.meters
        meter.drawsIdleTrack = false
        meter.setAX(AX.patchEditorMeter)
        matrixRows.addSubview(meter)
        matrixHeader.matrix = matrix
        matrixRows.matrix = matrix
        matrix.onChange = { [weak self] in self?.placePanes() }
        let scroll = TilingScrollView()
        scroll.onTile = { [weak self] in self?.placeCorner() }
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            self, selector: #selector(matrixScrolled), name: NSView.boundsDidChangeNotification,
            object: scroll.contentView)
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        scroll.documentView = matrix
        // The left columns float over the boxes when scrolled sideways, the header over the rows
        // when scrolled up and down (laid out in the matrix's coordinates).
        scroll.addFloatingSubview(matrixRows, for: .horizontal)
        scroll.addFloatingSubview(matrixHeader, for: .vertical)
        // Not a floating subview: a sibling above the clip view, fixed to its top left.
        matrixCorner.matrix = matrix
        scroll.addSubview(matrixCorner)
        // The floating panes are as tall or wide as the matrix: the scroll view shows only its part.
        scroll.clipsToBounds = true
        // No inset at the top: the header row sits on the visible top edge, nothing peeks above it.
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentInsets = NSEdgeInsetsZero
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        scroll.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)

        let stack = NSStackView(views: [scroll, routingNote])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 10, left: 12, bottom: 10, right: 12)
        scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -24).isActive = true
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: PatchRoutingView.headerH + 66).isActive = true
        routingNote.widthAnchor.constraint(lessThanOrEqualTo: stack.widthAnchor, constant: -24).isActive = true
        return stack
    }

    private func buildDeviceTab() -> NSView {
        deviceOutputsNote.font = .systemFont(ofSize: 11)
        deviceOutputsNote.textColor = .secondaryLabelColor
        deviceOutputsNote.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        deviceGrid.rowSpacing = 10
        let stack = NSStackView(views: [deviceOutputsNote, deviceGrid])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        let scroll = scrolling(stack, label: loc("patchEditor.tab.deviceOutputs"), fillWidth: true)
        scroll.setAccessibilityIdentifier(AX.patchEditorDeviceOutputs)
        // As wide as the page, so the note wraps at its edge rather than at a fixed width.
        deviceOutputsNote.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
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
            row: row, devices: model.devices,
            usable: row.flatMap(model.deviceName(of:)).map(model.usableChannels(of:)) ?? 0,
            loaded: model.loaded, routing: PatchRouting(patch: patch.raw, mapping: row?.mapping),
            others: otherPatches.map(\.name))
        if let drawn, drawn == state, typedField == nil, !dragging { return }
        guard !dragging else { return }
        let rebuild = outputFields.count != state.levels.count || outputNameFields.count != state.levels.count
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
        for (i, db) in state.levels.enumerated() {
            set(outputFields[i], LevelText.format(db), enabled: editable)
            // A cue output muted in the show keeps its level, which reads red; the editor has no
            // button to mute one (QLab 5's has none on a cue output's row): Unmute All clears it.
            let muted = state.routing.outputs.indices.contains(i) && state.routing.outputs[i].mute
            outputFields[i].toolTip = muted ? loc("patchEditor.outputMuted") : nil
            outputFields[i].textColor = muted ? .systemRed : .controlTextColor
            outputFields[i].setAccessibilityLabel(muted ? loc("patchEditor.outputMuted") : nil)
        }
        // Not by colour alone: the row's label says the output is muted.
        func isMuted(_ i: Int) -> Bool { state.routing.outputs.indices.contains(i) && state.routing.outputs[i].mute }

        // The matrix: one row per cue output, one column per device output the device has or the
        // routing names.
        let device = row.flatMap(model.deviceName(of:))
        let channels = device.map(model.channelCount(of:)) ?? 0
        let columns = min(PatchRouting.maxDeviceOutputs, max(channels, state.routing.namedDeviceOutputs))
        // The mark goes with the number, before the name, so a long name cut at its tail never hides it.
        matrix.rowLabels = state.names.enumerated().map { i, name in
            let number = isMuted(i) ? String(format: loc("patchEditor.mutedRow"), "\(i + 1)") : "\(i + 1)"
            return name.isEmpty ? number : "\(number)  \(name)"
        }
        matrix.deviceOutputs = columns
        matrix.usable = state.usable
        matrix.routing = state.routing
        matrix.isEnabled = state.loaded && row?.mapping != nil && editable
        if let cell = typingCell, !matrix.canType(row: cell.row, column: cell.column) { endTyping() }
        fitMatrixWidth()
        let metered = state.levels.count
        meter.frame = matrix.meterRect(rows: metered).offsetBy(dx: 0, dy: -PatchRoutingView.headerH)
        meter.fixedRows = metered
        meter.source = .output(patchID)

        var notes: [String] = []
        if row?.mapping == nil { notes.append(loc("patchEditor.noDevice")) }
        if channels > state.usable, row?.mapping != nil {
            notes.append(String(format: loc("patchEditor.usable"), state.usable, channels))
        }
        // A device not connected now has no outputs to count: say that, not that it has none.
        let connected = device.map { name in state.devices.contains { $0.name == name } } ?? false
        let beyond = row?.mapping != nil && state.routing.namedDeviceOutputs > state.usable
        let beyondNote =
            connected
            ? String(format: loc("patchEditor.beyond"), state.usable, columns) : loc("patchEditor.notConnectedNote")
        if beyond { notes.append(beyondNote) }
        routingNote.stringValue = notes.joined(separator: " ")
        routingNote.isHidden = notes.isEmpty
        var deviceNotes: [String] = []
        if row?.mapping == nil {
            deviceNotes.append(loc("patchEditor.noDevice"))
        } else if channels > state.usable {
            deviceNotes.append(String(format: loc("patchEditor.usable"), state.usable, channels))
        }
        if beyond { deviceNotes.append(beyondNote) }
        deviceOutputsNote.stringValue = deviceNotes.joined(separator: " ")
        deviceOutputsNote.isHidden = deviceOutputsNote.stringValue.isEmpty
        renderStrips(columns: columns, state: state, editable: editable)
        renderDeviceOutputs(columns: columns, state: state)
        renderFoot(state: state, editable: editable)
    }

    /// The floating panes take the matrix's header row and its left columns.
    private func placePanes() {
        // The header reaches a little above the matrix's top, so no row peeks out over it at the
        // scroll view's edge; its drawing keeps the matrix's coordinates.
        let pad: CGFloat = 4
        matrixHeader.frame = NSRect(x: 0, y: -pad, width: matrix.frame.width, height: PatchRoutingView.headerH + pad)
        matrixHeader.setBoundsOrigin(NSPoint(x: 0, y: -pad))
        // Ends with the last row: the matrix's own bottom margin is not a row.
        matrixRows.frame = NSRect(
            x: 0, y: PatchRoutingView.headerH, width: PatchRoutingView.crossX,
            height: CGFloat(matrix.rowLabels.count) * PatchRoutingView.rowH)
        matrixHeader.needsDisplay = true
        matrixRows.needsDisplay = true
        for (d, fader) in deviceFaders.enumerated() { placeStrip(d, fader) }
        hideStripsLeftOfView()
        placeCorner()
    }

    @objc private func matrixScrolled() { hideStripsLeftOfView() }

    /// A strip scrolled out to the left of the visible columns is hidden: the corner covers the
    /// columns' part of the view, but not the scroll view's own border, where a strip's edge would
    /// show as a sliver.
    func hideStripsLeftOfView() {
        let edge = matrix.visibleRect.minX + PatchRoutingView.crossX
        for d in deviceFaders.indices {
            let gone = PatchRoutingView.stripMidX(column: d) + PatchRoutingView.cellW / 2 <= edge
            for v in [deviceFaders[d], deviceMutes[d], deviceFields[d]] as [NSView] where v.isHidden != gone {
                v.isHidden = gone
            }
        }
    }

    private func placeStrip(_ d: Int, _ fader: LevelFader) {
        let midX = PatchRoutingView.stripMidX(column: d)
        fader.frame = PatchRoutingView.faderFrame(midX: midX)
        deviceMutes[d].frame = PatchRoutingView.muteFrame(midX: midX)
        deviceFields[d].frame = PatchRoutingView.stripBoxFrame(midX: midX)
    }

    /// The corner sits on the clip view's top left, over the header row and the left columns.
    func placeCorner() {
        guard let scroll = matrix.enclosingScrollView else { return }
        let clip = scroll.contentView.frame
        guard clip.width > 0, clip.height > 0 else { return }
        let top = scroll.isFlipped ? clip.minY : clip.maxY - PatchRoutingView.headerH
        matrixCorner.autoresizingMask = [.maxXMargin, scroll.isFlipped ? .maxYMargin : .minYMargin]
        let frame = NSRect(
            x: clip.minX, y: top, width: min(PatchRoutingView.crossX, clip.width),
            height: min(PatchRoutingView.headerH, clip.height))
        if matrixCorner.frame != frame {
            matrixCorner.frame = frame
            matrixCorner.needsDisplay = true
        }
        // Over every other part of the scroll view, the container of its floating panes (which
        // AppKit adds above the clip view) included.
        if scroll.subviews.last !== matrixCorner {
            scroll.addSubview(matrixCorner, positioned: .above, relativeTo: nil)
        }
    }

    /// The matrix is at least as wide as its scroll view (followed on each layout of the window).
    func fitMatrixWidth() {
        if let clip = matrix.enclosingScrollView?.contentView { matrix.minWidth = clip.bounds.width }
        placeCorner()
    }

    /// A field shows `text` unless it is being typed in.
    private func set(_ field: NSTextField, _ text: String, enabled: Bool) {
        field.isEnabled = enabled
        if field !== typedField { field.stringValue = text }
    }

    private func rebuildRows(count: Int) {
        for v in namesStack.arrangedSubviews {
            namesStack.removeArrangedSubview(v)
            v.removeFromSuperview()
        }
        outputNameFields = []
        outputTitles = []
        for f in outputFields { f.removeFromSuperview() }
        outputFields = []
        for i in 0..<count {
            let label = NSTextField(labelWithString: String(format: loc("patchEditor.cueOutputRow"), i + 1))
            label.alignment = .right
            label.widthAnchor.constraint(equalToConstant: Self.rowTitleWidth).isActive = true
            outputTitles.append(label)
            let name = NSTextField()
            name.delegate = self
            name.tag = i
            name.lineBreakMode = .byTruncatingTail
            name.cell?.usesSingleLineMode = true
            // As wide as the page lets it: the line fills the page and the field takes what the title leaves.
            name.widthAnchor.constraint(greaterThanOrEqualToConstant: 180).isActive = true
            name.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            name.setAX(
                "\(AX.patchEditorOutputName)-\(i + 1)", label: String(format: loc("patchEditor.cueOutput"), i + 1))
            let line = NSStackView(views: [label, name])
            line.spacing = 8
            namesStack.addArrangedSubview(line)
            line.widthAnchor.constraint(equalTo: namesStack.widthAnchor).isActive = true
            outputNameFields.append(name)

            let level = LevelBoxField(frame: matrix.levelRect(row: i).offsetBy(dx: 0, dy: -PatchRoutingView.headerH))
            level.alignment = .center
            level.font = .monospacedDigitSystemFont(ofSize: Self.levelFontSize, weight: .regular)
            level.controlSize = .mini
            level.delegate = self
            level.tag = i + 1
            level.setAX(
                "\(AX.patchEditorOutputField)-\(i + 1)",
                label: "\(String(format: loc("patchEditor.cueOutput"), i + 1)), \(loc("patchEditor.level"))")
            matrixRows.addSubview(level)
            outputFields.append(level)
        }
        mainField.tag = 0
    }

    /// The strip over each device output's column: its fader, mute toggle and level box. Made
    /// again only when the columns' number changes, so a box being typed in or a fader being dragged
    /// is never replaced under the operator.
    private func renderStrips(columns: Int, state: Drawn, editable: Bool) {
        if deviceFaders.count != columns { rebuildStrips(columns) }
        for d in 0..<columns {
            let setting = state.routing.deviceOutput(d)
            let fader = deviceFaders[d]
            fader.minValue = state.limits.min
            fader.maxValue = state.limits.max
            if draggedDeviceOutput != d {
                fader.doubleValue = min(max(setting.level, state.limits.min), state.limits.max)
            }
            fader.isEnabled = editable
            set(deviceFields[d], LevelText.format(setting.level), enabled: editable)
            deviceMutes[d].state = setting.mute ? .on : .off
            Self.paintMute(deviceMutes[d], muted: setting.mute)
            deviceMutes[d].isEnabled = editable
        }
    }

    /// A muted output's button is filled red with a white M, as a mute button is; the bezel's own
    /// pressed look is only a lighter grey. The fill is drawn by the layer, under the title.
    static func paintMute(_ button: NSButton, muted: Bool) {
        button.wantsLayer = true
        button.isBordered = !muted
        button.layer?.backgroundColor = muted ? NSColor.systemRed.cgColor : nil
        button.layer?.cornerRadius = muted ? 3 : 0
        let title = loc("patchEditor.muteShort")
        button.attributedTitle = NSAttributedString(
            string: title,
            attributes: [
                .font: NSFont.systemFont(ofSize: 9, weight: .semibold),
                .foregroundColor: muted ? NSColor.white : NSColor.labelColor,
            ])
    }

    private func rebuildStrips(_ columns: Int) {
        for v in deviceFaders as [NSView] + deviceMutes + deviceFields { v.removeFromSuperview() }
        if let typed = typedField, deviceFields.contains(where: { $0 === typed }) { typedField = nil }
        deviceFaders = []
        deviceMutes = []
        deviceFields = []
        for d in 0..<columns {
            let name = String(format: loc("patchEditor.deviceOutput"), d + 1)
            let fader = LevelFader(value: 0, minValue: LevelText.floorDb, maxValue: 0, target: self, action: nil)
            fader.isVertical = true
            fader.isContinuous = true
            fader.action = #selector(deviceFaderMoved(_:))
            fader.tag = d
            fader.onGestureBegin = { [weak self] in
                self?.draggedDeviceOutput = d
                self?.gesture += 1
            }
            fader.onGestureEnd = { [weak self] in
                self?.draggedDeviceOutput = nil
                self?.render()
            }
            fader.setAX("\(AX.patchEditorDeviceFader)-\(d + 1)", label: name)
            let mute = NSButton(title: loc("patchEditor.muteShort"), target: self, action: #selector(muteClicked(_:)))
            mute.setButtonType(.pushOnPushOff)
            mute.bezelStyle = .smallSquare
            mute.controlSize = .mini
            mute.font = .systemFont(ofSize: 9, weight: .semibold)
            mute.tag = d
            mute.setAX("\(AX.patchEditorDeviceMute)-\(d + 1)", label: "\(name) \(loc("patchEditor.mute"))")
            let field = LevelBoxField()
            field.alignment = .center
            field.font = .monospacedDigitSystemFont(ofSize: Self.levelFontSize, weight: .regular)
            field.controlSize = .mini
            field.delegate = self
            field.setAX("\(AX.patchEditorDeviceField)-\(d + 1)", label: "\(name) \(loc("patchEditor.level"))")
            for v in [fader, mute, field] as [NSView] { matrixHeader.addSubview(v) }
            deviceFaders.append(fader)
            deviceMutes.append(mute)
            deviceFields.append(field)
            placeStrip(d, fader)
        }
        hideStripsLeftOfView()
    }

    /// The Device Outputs tab: a titled row per device output. Rows are made again only when their
    /// number changes.
    private func renderDeviceOutputs(columns: Int, state: Drawn) {
        if deviceTitles.count != columns { rebuildDeviceRows(columns) }
        for d in 0..<columns {
            // An output the device does not have reads red, as its column does.
            deviceTitles[d].textColor = d < state.usable ? .labelColor : .systemRed
        }
    }

    private func rebuildDeviceRows(_ columns: Int) {
        while deviceGrid.numberOfRows > 0 {
            let last = deviceGrid.row(at: deviceGrid.numberOfRows - 1)
            for c in 0..<last.numberOfCells { last.cell(at: c).contentView?.removeFromSuperview() }
            deviceGrid.removeRow(at: deviceGrid.numberOfRows - 1)
        }
        deviceTitles = (0..<columns).map { d in
            let title = NSTextField(labelWithString: String(format: loc("patchEditor.deviceOutput"), d + 1))
            title.alignment = .right
            title.widthAnchor.constraint(equalToConstant: Self.rowTitleWidth).isActive = true
            return title
        }
        for title in deviceTitles { deviceGrid.addRow(with: [title]) }
    }

    /// The copy pull-down's items as last built.
    private var footTitles: [String] = []

    /// The other patches of the workspace, whose routing can be copied here.
    private var otherPatches: [(id: ModelID, name: String)] {
        (store.model?.patches ?? []).filter { $0.id != patchID }.map { p in
            let name = p.raw["name"]?.stringValue ?? ""
            return (p.id, name.isEmpty ? loc("settings.audio.untitledPatch") : name)
        }
    }

    /// The foot: the copy pull-down lists the defaults and every other patch; Unmute All is live
    /// while something is muted.
    private func renderFoot(state: Drawn, editable: Bool) {
        let titles =
            [loc("patchEditor.copyDefaults")] + state.others.map { String(format: loc("patchEditor.copyFrom"), $0) }
        if footTitles != titles {
            footTitles = titles
            copyPopup.removeAllItems()
            copyPopup.addItem(withTitle: loc("patchEditor.copyRouting"))
            copyPopup.addItem(withTitle: titles[0])
            copyPopup.lastItem?.representedObject = nil
            if !state.others.isEmpty { copyPopup.menu?.addItem(.separator()) }
            for (other, title) in zip(otherPatches, titles.dropFirst()) {
                copyPopup.addItem(withTitle: title)
                copyPopup.lastItem?.representedObject = other.id.raw
            }
        }
        copyPopup.isEnabled = editable
        unmuteButton.isEnabled = editable && state.routing.hasMutes
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
        send(.setPatchLevel(patchID, output: tag == 0 ? nil : tag - 1, db: db), coalesce: coalesce)
    }

    private func send(_ command: Command, coalesce: String? = nil) {
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

    @objc private func deviceFaderMoved(_ sender: LevelFader) {
        let d = sender.tag
        let db = LevelText.clamp((sender.doubleValue * 10).rounded() / 10, limits: limits)
        let key =
            draggedDeviceOutput == d ? "patch-device-fader:\(patchID.raw):\(Self.launchToken):\(gesture):\(d)" : nil
        send(.setPatchDeviceOutputLevel(patchID, deviceOutput: d, db: db), coalesce: key)
        if deviceFields.indices.contains(d) { deviceFields[d].stringValue = LevelText.format(db) }
    }

    @objc private func muteClicked(_ sender: NSButton) {
        send(.setPatchDeviceOutputMute(patchID, deviceOutput: sender.tag, mute: sender.state == .on))
    }

    @objc private func deviceChosen() {
        guard let row else { return }
        let choice = devicePopup.selectedItem?.representedObject
        edit { [self] in await model.choose(choice, for: row) }
    }

    /// The routing the editor shows now.
    private var routing: PatchRouting { matrix.routing }

    private var outputsJSON: [JSONValue] { patch?.raw["outputs"]?.arrayValue ?? [] }

    @objc private func copyChosen() {
        guard let item = copyPopup.selectedItem, copyPopup.indexOfSelectedItem > 0 else { return }
        let command: Command?
        if let raw = item.representedObject as? String {
            let id = ModelID(raw)
            guard let source = store.model?.patches.first(where: { $0.id == id }) else { return }
            let mapping = model.patches.first { $0.id == id }?.mapping
            command = routing.copyCommands(
                patchID, from: PatchRouting(patch: source.raw, mapping: mapping), outputsJSON: outputsJSON)
        } else {
            command = routing.defaultsCommands(patchID, outputsJSON: outputsJSON)
        }
        if let command { send(command) }
    }

    @objc private func unmuteClicked() {
        if let command = routing.unmuteCommands(patchID, outputsJSON: outputsJSON) { send(command) }
    }

    /// Opens the box of cue output `row` and device output `column` for typing, its level selected.
    func beginTyping(row: Int, column: Int) {
        guard matrix.canType(row: row, column: column) else { return }
        typingCell = (row, column)
        crosspointField.frame = matrix.boxRect(row: row, column: column).insetBy(dx: -1, dy: -1)
        crosspointField.stringValue = routing.level(row, column).map(LevelText.format) ?? ""
        crosspointField.setAccessibilityLabel(String(format: loc("patchEditor.crosspoint"), row + 1, column + 1))
        crosspointField.isHidden = false
        window?.makeFirstResponder(crosspointField)
        crosspointField.currentEditor()?.selectAll(nil)
    }

    /// Hides the crosspoint box; what was typed is not sent.
    private func endTyping() {
        typingCell = nil
        if typedField === crosspointField { typedField = nil }
        crosspointField.isHidden = true
        if window?.firstResponder === crosspointField.currentEditor() { window?.makeFirstResponder(matrix) }
    }

    /// The level typed into a crosspoint's box goes out as the crosspoint the routing needs for it
    /// (none when it is what the routing has).
    private func commitCrosspoint(_ text: String) {
        guard let cell = typingCell else { return }
        endTyping()
        // Emptied is -inf (QLab 5 manual p. 204); only white space is no change (ours).
        guard !LevelText.isStraySpace(text) else { return }
        let db = LevelText.parse(text, limits: limits)
        guard let value = routing.typed(db, output: cell.row, deviceOutput: cell.column) else { return }
        send(.setPatchCrosspoint(patchID, output: cell.row, deviceOutput: cell.column, db: value))
    }

    func controlTextDidChange(_ obj: Notification) { typedField = obj.object as? NSTextField }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        // Esc aborts the editor, which posts no end of editing: the typing ends here.
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            typedField = nil
            if control === crosspointField {
                endTyping()
                return true
            }
        }
        return control.endTyping(by: selector)
    }

    /// Every way out of typing: Return, Tab or a click elsewhere commits the text that was typed, as
    /// the cue levels do; a field only clicked into writes nothing.
    func controlTextDidEndEditing(_ obj: Notification) {
        let typed = typedField
        typedField = nil
        guard let field = obj.object as? NSTextField, field === typed else {
            if (obj.object as? NSTextField) === crosspointField { endTyping() }
            drawn = nil
            render()
            return
        }
        let text = field.stringValue
        if field === crosspointField {
            commitCrosspoint(text)
        } else if let d = deviceFields.firstIndex(where: { $0 === field }) {
            if LevelText.isStraySpace(text) {
                drawn = nil
                render()
            } else {
                send(.setPatchDeviceOutputLevel(patchID, deviceOutput: d, db: LevelText.parse(text, limits: limits)))
            }
        } else if field === nameField {
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

    /// Closed: it no longer follows the devices until it is shown again.
    func windowWillClose(_ notification: Notification) { isOpen = false }

    func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        window?.contentView?.layoutSubtreeIfNeeded()
        fitMatrixWidth()
    }

    @objc private func windowResized() {
        window?.contentView?.layoutSubtreeIfNeeded()
        fitMatrixWidth()
    }
}

/// The represented object of the system output item of a patch's device pop-up.
final class SystemOutputChoice: NSObject, Sendable {
    static let shared = SystemOutputChoice()
}

extension WorkspaceSettingsModel {
    /// The device choices of a patch, as the table and the patch editor show them (as QLab 5 lists
    /// them, settings surface `settings.audio.audio-outputs`): none, then the system output with
    /// the device it is now, then every listed device, and the device the patch names when that is
    /// not connected now.
    func fillDevicePopupItems(_ popup: NSPopUpButton, for row: PatchRow) {
        popup.removeAllItems()
        popup.autoenablesItems = false
        popup.addItem(withTitle: loc("settings.audio.noDevice"))
        popup.lastItem?.representedObject = nil
        popup.menu?.addItem(.separator())
        popup.addItem(withTitle: systemOutputTitle)
        popup.lastItem?.representedObject = SystemOutputChoice.shared
        popup.menu?.addItem(.separator())
        for name in deviceChoices(for: row) {
            let listed = devices.contains { $0.name == name }
            popup.addItem(withTitle: listed ? name : String(format: loc("settings.audio.notConnected"), name))
            popup.lastItem?.representedObject = name
        }
        let current: Int?
        if row.mapping?.system == true {
            current = popup.itemArray.firstIndex { $0.representedObject is SystemOutputChoice }
        } else {
            current = popup.itemArray.firstIndex { ($0.representedObject as? String) == row.mapping?.device }
        }
        popup.selectItem(at: current ?? 0)
    }

    /// The system output item: the device the Mac's output is now, or that there is none.
    var systemOutputTitle: String {
        guard let device = systemOutputDevice else { return loc("settings.audio.systemOutputNone") }
        return String(format: loc("settings.audio.systemOutput"), device.name)
    }

    /// Carries out a pick in a patch's device pop-up: none clears it, the system output item puts
    /// it on the system output, a name on that device.
    func choose(_ choice: Any?, for row: PatchRow) async {
        if choice is SystemOutputChoice {
            await setSystemOutput(row)
        } else {
            await setMapping(row, device: choice as? String)
        }
    }

    func problemText(_ problem: PatchProblem) -> String {
        switch problem {
        case .unmapped: return loc("settings.audio.problem.unmapped")
        case .deviceMissing(let name): return String(format: loc("settings.audio.problem.deviceMissing"), name)
        case .deviceUnusable(let name, let reason):
            return String(format: loc("settings.audio.problem.deviceUnusable"), name, reason)
        case .deviceOpening(let name): return String(format: loc("settings.audio.problem.deviceOpening"), name)
        case .channelsMissing(let device, let needed, let available):
            return String(format: loc("settings.audio.problem.channelsMissing"), device, available, needed)
        case .outputCountMismatch: return loc("settings.audio.problem.outputCount")
        case .systemOutputMissing: return loc("settings.audio.systemOutputMissing")
        }
    }
}
