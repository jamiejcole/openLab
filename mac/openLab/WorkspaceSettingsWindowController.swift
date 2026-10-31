// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The workspace settings window (UI-01, AUD-07, WS-07, PB-02, PB-03). QLab 5 has it in the
/// File menu (Cmd-,) and behind the footer gear: a General tab (the panic fade and the time
/// between GOs) and an Audio tab (the output device, and the patch editor that puts each output
/// patch on device channels). It reads and writes through its `WorkspaceSettingsModel`.
@MainActor
final class WorkspaceSettingsWindowController: NSWindowController, NSTextFieldDelegate {
    let model: WorkspaceSettingsModel
    let tabs = NSTabView()

    // General
    let panicField = NSTextField()
    let goIntervalField = NSTextField()

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

    let messageLabel = NSTextField(wrappingLabelWithString: "")
    let doneButton = NSButton(title: loc("settings.done"), target: nil, action: nil)

    private var subs: [AnyCancellable] = []
    // Weak-self observers: they outlive the controller harmlessly and do nothing.
    private var observers: [NSObjectProtocol] = []
    /// How many times the controls were rebuilt (tests: an unchanged status rebuilds nothing).
    private(set) var renderCount = 0
    private var lastDrawn: WorkspaceSettingsModel.DisplayState?
    /// A popup's menu is open: rebuilding its items would close it under the operator's hand.
    private var menuOpen = false
    private var shownPatch: ModelID?

    init(store: WorkspaceStore) {
        model = WorkspaceSettingsModel(store: store)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 420),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = loc("settings.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.settingsWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.contentView = buildContent()
        window.center()
        // Published before they change: the sinks use the delivered value and render after it.
        subs.append(store.$status.sink { [weak self] _ in self?.renderSoon() })
        subs.append(store.$model.sink { [weak self] _ in self?.renderSoon() })
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
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

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
        channelsStack.orientation = .vertical
        channelsStack.alignment = .leading
        channelsStack.spacing = 6

        let audio = NSStackView(views: [
            heading(loc("settings.audio.device")),
            devicePopup, deviceStatusLabel, deviceNoteLabel,
            heading(loc("settings.audio.patches")),
            patchPopup,
            row(loc("settings.audio.patchDevice"), patchDevicePopup, nil),
            channelsStack, patchMeter, patchProblemLabel,
        ])
        patchMeter.bank = model.store.meters
        patchMeter.setAX("settings-patch-meter")
        audio.orientation = .vertical
        audio.alignment = .leading
        audio.spacing = 8

        for (label, view, id) in [
            (loc("settings.tab.general"), general, "general"), (loc("settings.tab.audio"), audio, "audio"),
        ] {
            let item = NSTabViewItem(identifier: id)
            item.label = label
            let holder = NSView()
            view.translatesAutoresizingMaskIntoConstraints = false
            holder.addSubview(view)
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
        messageLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let root = NSStackView(views: [tabs, bottom])
        root.orientation = .vertical
        root.alignment = .width
        root.spacing = 10
        root.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        return root
    }

    private func heading(_ text: String) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = .systemFont(ofSize: 13, weight: .semibold)
        return label
    }

    private func row(_ title: String, _ control: NSView, _ suffix: String?) -> NSStackView {
        var views: [NSView] = [NSTextField(labelWithString: title), control]
        if let suffix { views.append(NSTextField(labelWithString: suffix)) }
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
        lastDrawn = state
        renderCount += 1
        renderGeneral()
        renderDevice()
        renderPatch()
        messageLabel.stringValue = model.message ?? ""
    }

    private func renderGeneral() {
        let editing = window?.firstResponder
        for (field, value) in [(panicField, model.panicDuration), (goIntervalField, model.minGoInterval)] {
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
        shownPatch = (patchPopup.selectedItem?.representedObject as? String).map { ModelID($0) }
        lastDrawn = nil
        render()
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

    @objc private func doneClicked() { window?.close() }

    /// A number field was committed (Return or leaving the field): sent when valid, else the
    /// engine's value comes back.
    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        let text = field.stringValue
        Task {
            if field === panicField {
                await model.setPanicDuration(text)
            } else if field === goIntervalField {
                await model.setMinGoInterval(text)
            }
            render()
        }
    }
}
