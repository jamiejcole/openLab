// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The workspace settings window (UI-01, AUD-07, WS-07, PB-02, PB-03). QLab 5 has it in the
/// File menu (Cmd-,) and behind the footer gear: a General tab (the panic fade and the time
/// between GOs) and an Audio tab (the output device, and the patch editor that puts each output
/// patch on device channels). It reads and writes through its `WorkspaceSettingsModel`.
@MainActor
final class WorkspaceSettingsWindowController: NSWindowController, NSTextFieldDelegate, NSWindowDelegate,
    NSTabViewDelegate
{
    let model: WorkspaceSettingsModel
    /// Sized before its tabs are added: a zero-sized tab view lays its tab strip out at a negative width.
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))

    // General
    let panicField = NSTextField()
    let goIntervalField = NSTextField()
    // Volume limits (Audio tab)
    let maxLimitField = NSTextField()
    let minLimitField = NSTextField()
    let patchEditButton = NSButton(title: loc("settings.audio.edit"), target: nil, action: nil)
    let patchAddButton = NSButton(title: loc("settings.audio.newPatch"), target: nil, action: nil)
    let patchRemoveButton = NSButton(title: loc("settings.audio.removePatch"), target: nil, action: nil)
    /// The shown patch's name, typed in place.
    let patchNameField = NSTextField()
    /// The patch editor of the patch last edited; the window lives as long as this window does.
    private(set) var patchEditor: PatchEditorWindowController?

    // Audio
    let devicePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let deviceStatusLabel = NSTextField(wrappingLabelWithString: "")
    let deviceNoteLabel = NSTextField(wrappingLabelWithString: "")
    let patchPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let patchProblemLabel = NSTextField(wrappingLabelWithString: "")
    let patchDevicePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let channelsStack = NSStackView()
    /// Peaks of the shown patch's outputs (AUD-19), live only while the Audio tab shows.
    let patchMeter = MeterView()
    /// One popup per output of the shown patch: the device channel it plays on.
    private(set) var channelPopups: [NSPopUpButton] = []

    // OSC passcode table (NET-02); built and drawn in `+Osc`.
    let oscAddButton = NSButton(title: loc("settings.osc.add"), target: nil, action: nil)
    let oscGrid = NSGridView()
    let oscNoneLabel = NSTextField(labelWithString: loc("settings.osc.noPasscode"))
    /// The No Passcode row's View, Edit and Control boxes.
    var oscNoneRights: [NSButton] = []
    /// One per passcode row, in table order; rebuilt whenever the table is drawn.
    var oscCodeFields: [NSTextField] = []
    var oscViewBoxes: [NSButton] = []
    var oscEditBoxes: [NSButton] = []
    var oscControlBoxes: [NSButton] = []
    var oscDeleteButtons: [NSButton] = []
    /// The table as last drawn: the rows' controls find their passcode here by position.
    var shownOsc: OscAccessTable?
    /// The last passcode edit on its way to the engine; the next waits for it.
    var oscCommits: Task<Void, Never>?

    let networkAddButton = NSButton(title: loc("settings.network.add"), target: nil, action: nil)
    let networkGrid = NSGridView()
    /// The name, host and port fields of every row, in row order.
    var networkFields: [NSTextField] = []
    var networkTransportPopups: [NSPopUpButton] = []
    var networkDeleteButtons: [NSButton] = []
    var shownNetwork: [NetworkPatchRow]?
    var networkCommits: Task<Void, Never>?

    // Key map (`+Keys`): one recorder, clear button and reason line per action.
    var keyFields: [WorkspaceKey: HotkeyRecorder] = [:]
    var keyClearButtons: [WorkspaceKey: NSButton] = [:]
    var keyReasons: [WorkspaceKey: NSTextField] = [:]

    let messageLabel = NSTextField(wrappingLabelWithString: "")
    let doneButton = DoneButton(title: loc("settings.done"), target: nil, action: nil)

    private var subs: [AnyCancellable] = []
    // Menu-tracking observers: held only while the window is open, removed when it closes.
    private var observers: [NSObjectProtocol] = []
    /// How many times the controls were rebuilt (tests: an unchanged status rebuilds nothing).
    private(set) var renderCount = 0
    var lastDrawn: WorkspaceSettingsModel.DisplayState?
    /// A popup's menu is open: rebuilding its items would close it under the operator's hand.
    private var menuOpen = false
    private var shownPatch: ModelID?

    init(store: WorkspaceStore) {
        model = WorkspaceSettingsModel(store: store)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 540),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = loc("settings.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.settingsWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.contentView = buildContent()
        window.center()
        // Published before they change: the sinks use the delivered value and render after it.
        subs.append(store.$status.sink { [weak self] _ in self?.renderSoon() })
        subs.append(store.$model.sink { [weak self] _ in self?.renderSoon() })
        window.contentMinSize = NSSize(width: 520, height: 540)
        window.delegate = self
        tabs.delegate = self
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: Menu tracking

    private func trackMenus() {
        guard observers.isEmpty else { return }
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: NSMenu.didBeginTrackingNotification, object: nil, queue: .main) {
                [weak self] _ in MainActor.assumeIsolated { self?.menuOpen = true }
            })
        observers.append(
            center.addObserver(forName: NSMenu.didEndTrackingNotification, object: nil, queue: .main) {
                [weak self] _ in
                MainActor.assumeIsolated {
                    self?.menuOpen = false
                    self?.renderSoon()
                }
            })
    }

    /// The observers live from showing to closing.
    override func showWindow(_ sender: Any?) {
        trackMenus()
        super.showWindow(sender)
    }

    /// A refusal line is about the tab it came from.
    func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        model.message = nil
        model.keyRefusal = nil
        render()
    }

    func windowWillClose(_ notification: Notification) {
        model.message = nil
        model.keyRefusal = nil
        messageLabel.stringValue = ""
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        menuOpen = false
    }

    // MARK: Showing

    /// Brings the window up and reads the engine's state. The task's value tells tests it is done.
    @discardableResult
    func present() -> Task<Void, Never> {
        showWindow(nil)
        return Task {
            await model.refresh()
            render()
        }
    }

    private func renderSoon() {
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.render() } }
    }

    // MARK: Building

    private func buildContent() -> NSView {
        panicField.setAX(AX.settingsPanicField, label: loc("settings.general.panic"))
        goIntervalField.setAX(AX.settingsGoIntervalField, label: loc("settings.general.goInterval"))
        for field in [panicField, goIntervalField] {
            field.alignment = .right
            field.delegate = self
            field.widthAnchor.constraint(equalToConstant: 70).isActive = true
        }

        maxLimitField.setAX(AX.settingsLimitMax, label: loc("settings.audio.limitMax"))
        minLimitField.setAX(AX.settingsLimitMin, label: loc("settings.audio.limitMin"))
        for field in [maxLimitField, minLimitField] {
            field.alignment = .right
            field.delegate = self
            field.widthAnchor.constraint(equalToConstant: 70).isActive = true
        }
        patchEditButton.setAX(AX.settingsPatchEdit, label: loc("settings.audio.edit"))
        patchEditButton.target = self
        patchEditButton.action = #selector(editPatchClicked)

        let general = NSStackView(views: [
            row(loc("settings.general.panic"), panicField, loc("settings.seconds")),
            row(loc("settings.general.goInterval"), goIntervalField, loc("settings.seconds")),
        ])
        general.orientation = .vertical
        general.alignment = .leading
        general.spacing = 12

        devicePopup.setAX(AX.settingsDevicePopup, label: loc("settings.audio.device"))
        devicePopup.target = self
        devicePopup.action = #selector(deviceChosen)
        deviceStatusLabel.setAX(AX.settingsDeviceStatus)
        deviceNoteLabel.setAX(AX.settingsDeviceNote)
        deviceNoteLabel.textColor = .systemOrange
        patchPopup.setAX(AX.settingsPatchPopup, label: loc("settings.audio.patch"))
        patchPopup.target = self
        patchPopup.action = #selector(patchChosen)
        patchProblemLabel.setAX(AX.settingsPatchProblem)
        patchProblemLabel.textColor = .systemRed
        patchDevicePopup.setAX(AX.settingsPatchDevicePopup, label: loc("settings.audio.patchDevice"))
        patchDevicePopup.target = self
        patchDevicePopup.action = #selector(patchDeviceChosen)
        patchNameField.setAX("settings-patch-name", label: loc("settings.audio.patchName"))
        patchNameField.delegate = self
        patchNameField.widthAnchor.constraint(equalToConstant: 150).isActive = true
        patchAddButton.setAX("settings-patch-add", label: loc("settings.audio.newPatch"))
        patchRemoveButton.setAX("settings-patch-remove", label: loc("settings.audio.removePatch"))
        for b in [patchAddButton, patchRemoveButton] { b.target = self }
        patchAddButton.action = #selector(patchAddClicked)
        patchRemoveButton.action = #selector(patchRemoveClicked)
        let patchButtons = NSStackView(views: [patchAddButton, patchRemoveButton])
        patchButtons.spacing = 8
        channelsStack.orientation = .vertical
        channelsStack.alignment = .leading
        channelsStack.spacing = 6

        let audio = NSStackView(views: [
            heading(loc("settings.audio.device")),
            devicePopup, deviceStatusLabel, deviceNoteLabel,
            heading(loc("settings.audio.patches")),
            row(nil, patchPopup, patchEditButton),
            row(loc("settings.audio.patchName"), patchNameField, patchButtons),
            row(loc("settings.audio.patchDevice"), patchDevicePopup, nil),
            channelsStack, patchMeter, patchProblemLabel,
            heading(loc("settings.audio.limits")),
            row(loc("settings.audio.limitMax"), maxLimitField, loc("settings.db")),
            row(loc("settings.audio.limitMin"), minLimitField, loc("settings.db")),
        ])
        patchMeter.bank = model.store.meters
        patchMeter.setAX("settings-patch-meter")
        audio.orientation = .vertical
        audio.alignment = .leading
        audio.spacing = 8

        for (label, view, id) in [
            (loc("settings.tab.general"), general, "general"), (loc("settings.tab.keys"), buildKeysTab(), "keys"),
            (loc("settings.tab.audio"), audio, "audio"),
            (loc("settings.tab.osc"), buildOscTab(), "osc"),
            (loc("settings.tab.network"), buildNetworkTab(), "network"),
        ] {
            let item = NSTabViewItem(identifier: id)
            item.label = label
            let holder = NSView()
            view.translatesAutoresizingMaskIntoConstraints = false
            holder.addSubview(view)
            // The OSC table takes the pane's height.
            if id == "osc" || id == "network" {
                view.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -16).isActive = true
            }
            // The key map's two columns share the pane's width, so a long action name wraps in its own.
            if id == "keys" {
                view.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -16).isActive = true
            }
            NSLayoutConstraint.activate([
                view.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 16),
                view.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -16),
                view.topAnchor.constraint(equalTo: holder.topAnchor, constant: 16),
            ])
            item.view = holder
            tabs.addTabViewItem(item)
        }
        tabs.setAX(AX.settingsTabs)

        messageLabel.setAX(AX.settingsMessage)
        messageLabel.textColor = .systemRed
        doneButton.setAX(AX.settingsDone, label: loc("settings.done"))
        doneButton.keyEquivalent = "\r"
        doneButton.target = self
        doneButton.action = #selector(doneClicked)

        let bottom = NSStackView(views: [messageLabel, doneButton])
        bottom.orientation = .horizontal
        // The line takes the free width, so Done stays at the right edge with or without a message.
        bottom.distribution = .fill
        bottom.alignment = .centerY
        messageLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        doneButton.setContentHuggingPriority(.required, for: .horizontal)
        messageLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let root = NSStackView(views: [tabs, bottom])
        root.orientation = .vertical
        root.alignment = .width
        root.spacing = 10
        // The tabs take any height the window is given; the Done row keeps its own.
        root.distribution = .fill
        tabs.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        bottom.setContentHuggingPriority(.required, for: .vertical)
        root.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        // Every tab fills the window's width, whatever its page would choose for itself (the Keys
        // page's wrapping labels would otherwise size the tab view to them).
        tabs.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -24).isActive = true
        return root
    }

    private func heading(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: 13, weight: .semibold)
        return label
    }

    private func row(_ title: String?, _ control: NSView, _ suffix: Any?) -> NSStackView {
        var views: [NSView] = title.map { [NSTextField(labelWithString: $0), control] } ?? [control]
        if let suffix = suffix as? String {
            views.append(NSTextField(labelWithString: suffix))
        } else if let suffix = suffix as? NSView {
            views.append(suffix)
        }
        let stack = NSStackView(views: views)
        stack.orientation = .horizontal
        stack.spacing = 8
        return stack
    }

    // MARK: Drawing the model

    /// Redraws every control from the model. Called after each answer and each status change.
    func render() {
        let state = model.displayState
        guard state != lastDrawn, !menuOpen else { return }
        // A cleared `lastDrawn` asks for a full redraw (a refused edit puts the engine's text back).
        let forced = lastDrawn == nil
        lastDrawn = state
        renderCount += 1
        renderOsc(state.osc, forced: forced)
        renderNetwork(state.network, forced: forced)
        renderKeys(state.keys, refusal: state.keyRefusal)
        renderGeneral()
        renderDevice()
        renderPatch()
        messageLabel.stringValue = model.message ?? ""
    }

    private func renderGeneral() {
        let editing = window?.firstResponder
        let limits = model.limits
        for (field, value) in [
            (panicField, model.panicDuration), (goIntervalField, model.minGoInterval),
            (maxLimitField, model.store.model == nil ? nil : limits.max),
            (minLimitField, model.store.model == nil ? nil : limits.min),
        ] {
            // A field being typed in keeps its text.
            if let editor = editing as? NSTextView, editor.delegate === field { continue }
            field.stringValue = value.map(Self.format) ?? ""
            field.isEnabled = value != nil
        }
    }

    static func format(_ seconds: Double) -> String {
        seconds == seconds.rounded() ? String(Int(seconds)) : String(seconds)
    }

    private func renderDevice() {
        devicePopup.removeAllItems()
        devicePopup.addItem(withTitle: loc("settings.audio.systemDefault"))
        devicePopup.lastItem?.representedObject = nil
        for d in model.devices {
            devicePopup.addItem(withTitle: d.name)
            devicePopup.lastItem?.representedObject = d.name
        }
        if let chosen = model.chosenDevice, !model.devices.contains(where: { $0.name == chosen }) {
            devicePopup.addItem(withTitle: String(format: loc("settings.audio.notAvailable"), chosen))
            devicePopup.lastItem?.representedObject = chosen
        }
        let index = devicePopup.itemArray.firstIndex { ($0.representedObject as? String) == model.chosenDevice }
        devicePopup.selectItem(at: index ?? 0)
        devicePopup.isEnabled = model.loaded

        if let playing = model.playing {
            let state = StatusPresentation.deviceStateText(playing.state)
            deviceStatusLabel.stringValue = String(
                format: loc("settings.audio.playingOn"), playing.name ?? loc("status.detail.none"), state)
        } else {
            deviceStatusLabel.stringValue = loc("settings.audio.noStatus")
        }
        var notes: [String] = []
        if model.deviceProblem { notes.append(loc("settings.audio.deviceProblem")) }
        if model.chosenDeviceMissing, let chosen = model.chosenDevice {
            notes.append(String(format: loc("settings.audio.chosenMissing"), chosen))
        } else if model.needsRestart {
            notes.append(loc("settings.audio.restartNeeded"))
        }
        deviceNoteLabel.stringValue = notes.joined(separator: "\n")
        deviceNoteLabel.isHidden = notes.isEmpty
    }

    private var shownRow: PatchRow? {
        model.patches.first { $0.id == shownPatch } ?? model.patches.first
    }

    private func renderPatch() {
        patchPopup.removeAllItems()
        for p in model.patches {
            patchPopup.addItem(withTitle: p.name.isEmpty ? loc("settings.audio.untitledPatch") : p.name)
            patchPopup.lastItem?.representedObject = p.id.raw
        }
        guard let row = shownRow else {
            patchDevicePopup.removeAllItems()
            patchDevicePopup.isEnabled = false
            channelPopups = []
            for v in channelsStack.views { v.removeFromSuperview() }
            patchMeter.source = nil
            patchProblemLabel.stringValue = ""
            return
        }
        shownPatch = row.id
        if patchNameField.currentEditor() == nil { patchNameField.stringValue = row.name }
        patchNameField.isEnabled = model.store.model != nil
        patchRemoveButton.isEnabled = model.patches.count > 1
        patchMeter.source = .output(row.id)
        patchPopup.selectItem(at: model.patches.firstIndex { $0.id == row.id } ?? 0)

        patchDevicePopup.removeAllItems()
        patchDevicePopup.addItem(withTitle: loc("settings.audio.noDevice"))
        patchDevicePopup.lastItem?.representedObject = nil
        for name in model.deviceChoices(for: row) {
            let known = model.devices.contains { $0.name == name }
            patchDevicePopup.addItem(
                withTitle: known ? name : String(format: loc("settings.audio.notAvailable"), name))
            patchDevicePopup.lastItem?.representedObject = name
        }
        let current = patchDevicePopup.itemArray.firstIndex {
            ($0.representedObject as? String) == row.mapping?.device
        }
        patchDevicePopup.selectItem(at: current ?? 0)
        patchDevicePopup.isEnabled = model.loaded

        for v in channelsStack.views { v.removeFromSuperview() }
        channelPopups = []
        if let mapping = row.mapping {
            let count = model.channelCount(of: mapping.device)
            for output in 0..<row.outputCount {
                let popup = NSPopUpButton(frame: .zero, pullsDown: false)
                popup.setAX("\(AX.settingsChannelPopup)-\(output + 1)", label: channelLabel(output))
                for c
                    in 0..<max(count, Int(mapping.channels.indices.contains(output) ? mapping.channels[output] + 1 : 0))
                {
                    popup.addItem(withTitle: String(c + 1))
                    popup.lastItem?.tag = c
                }
                if mapping.channels.indices.contains(output) {
                    popup.selectItem(withTag: Int(mapping.channels[output]))
                }
                popup.tag = output
                popup.target = self
                popup.action = #selector(channelChosen(_:))
                popup.isEnabled = model.loaded
                channelPopups.append(popup)
                channelsStack.addArrangedSubview(self.row(channelLabel(output), popup, nil))
            }
        }
        patchProblemLabel.stringValue = model.problem(of: row).map(problemText) ?? ""
    }

    private func channelLabel(_ output: Int) -> String {
        String(format: loc("settings.audio.output"), output + 1)
    }

    func problemText(_ problem: PatchProblem) -> String {
        switch problem {
        case .unmapped: return loc("settings.audio.problem.unmapped")
        case .deviceMissing(let name): return String(format: loc("settings.audio.problem.deviceMissing"), name)
        case .channelsMissing: return loc("settings.audio.problem.channelsMissing")
        case .outputCountMismatch: return loc("settings.audio.problem.outputCount")
        }
    }

    // MARK: Actions

    @objc private func deviceChosen() {
        let name = devicePopup.selectedItem?.representedObject as? String
        Task {
            await model.chooseDevice(name)
            render()
        }
    }

    @objc private func patchChosen() {
        endPatchNameTyping()
        shownPatch = (patchPopup.selectedItem?.representedObject as? String).map { ModelID($0) }
        lastDrawn = nil
        render()
    }

    /// A name still being typed belongs to the patch shown now: it is committed before another is shown.
    private func endPatchNameTyping() {
        if patchNameField.currentEditor() != nil { window?.makeFirstResponder(nil) }
    }

    @objc private func patchAddClicked() {
        endPatchNameTyping()
        Task {
            if let id = await model.addPatch() { shownPatch = id }
            lastDrawn = nil
            render()
        }
    }

    @objc private func patchRemoveClicked() {
        endPatchNameTyping()
        guard let row = shownRow else { return }
        Task {
            if await model.removePatch(row.id) { shownPatch = nil }
            lastDrawn = nil
            render()
        }
    }

    @objc private func patchDeviceChosen() {
        guard let row = shownRow else { return }
        let name = patchDevicePopup.selectedItem?.representedObject as? String
        Task {
            await model.setMapping(row, device: name)
            render()
        }
    }

    @objc private func channelChosen(_ sender: NSPopUpButton) {
        guard let row = shownRow, let channel = sender.selectedItem?.tag else { return }
        Task {
            await model.setChannel(row, output: sender.tag, channel: UInt32(channel))
            render()
        }
    }

    @objc private func editPatchClicked() {
        guard let row = shownRow else { return }
        let editor =
            patchEditor?.patchID == row.id
            ? patchEditor : PatchEditorWindowController(store: model.store, patch: row.id)
        patchEditor = editor
        editor?.present()
    }

    @objc private func doneClicked() { window?.close() }

    /// Goes with the workspace (and takes its patch editor along, which otherwise outlives them both).
    override func close() {
        patchEditor?.close()
        super.close()
    }

    /// Return in a field commits it and leaves the window open (the Done button's Return key
    /// would otherwise close the window with the typed text never sent).
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
        window?.makeFirstResponder(nil)
        return true
    }

    /// A number field was committed (Return or leaving the field): sent when valid, else the
    /// engine's value comes back.
    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        if networkFields.contains(field) {
            networkFieldCommitted(field)
            return
        }
        if oscCodeFields.contains(field) {
            oscCodeCommitted(field)
            return
        }
        let text = field.stringValue
        if field === patchNameField {
            guard let row = shownRow else { return }
            Task {
                await model.renamePatch(row.id, to: text)
                lastDrawn = nil
                render()
            }
            return
        }
        Task {
            if field === panicField {
                await model.setPanicDuration(text)
            } else if field === goIntervalField {
                await model.setMinGoInterval(text)
            } else if field === maxLimitField {
                await model.setOutputMax(text)
            } else if field === minLimitField {
                await model.setOutputMin(text)
            }
            // A refused or unchanged value must not leave the typed text in the field.
            lastDrawn = nil
            render()
        }
    }
}

/// The window's default button. While a field is being typed in, Return belongs to the field
/// (it commits the text); only with no text being edited does Return press Done.
final class DoneButton: NSButton {
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if window?.firstResponder is NSText { return false }
        return super.performKeyEquivalent(with: event)
    }
}
