// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The app-wide preferences (the app menu's Preferences; Workspace Settings are per workspace).
/// General tab: what happens at launch and how many backups are kept. Audio tab (QLab 5's hardware
/// tab, manual p. 87): the clock device, and each output device with the buffer size it is asked for.
/// Both tabs share one layout: a label column to the right edge of its text, controls beside it, help
/// text in the small grey type under the control it explains.
@MainActor
final class PreferencesWindowController: NSWindowController, NSTabViewDelegate {
    /// The one set of measures both tabs use.
    enum Layout {
        static let inset: CGFloat = 20
        static let rowGap: CGFloat = 14
        static let labelGap: CGFloat = 10
        /// Both tabs' label columns are this wide, so their controls start at the same place.
        static let labelColumn: CGFloat = 110
        /// Where help text wraps; the window's least width leaves room for it beside the label column.
        static let helpWidth: CGFloat = 310
        static let leastSize = NSSize(width: 500, height: 330)
    }

    let launchPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    /// How many backups of each workspace are kept (0: none), a machine setting (`+` the engine).
    let backupField = NSTextField()
    let backupOffNote = PreferencesWindowController.help("")
    let generalMessage = PreferencesWindowController.help("")
    /// Sized before its tabs are added: a zero-sized tab view lays its tab strip out at a negative width.
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))
    let clockPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let clockNote = PreferencesWindowController.help("")
    let deviceGrid = NSGridView()
    /// Said in the list when it has no rows: the engine is starting, or no output device is present.
    let deviceListNote = PreferencesWindowController.help("")
    let audioMessage = PreferencesWindowController.help("")
    /// One buffer-size pop-up per listed device, in list order.
    private(set) var bufferPopups: [NSPopUpButton] = []
    let audio: AudioPreferencesModel?
    private let preferences: LaunchPreferences
    /// The last refresh, so tests can wait for it.
    private(set) var refreshing: Task<Void, Never>?
    private var connectionSub: AnyCancellable?

    init(preferences: LaunchPreferences, audio: AudioPreferencesModel? = nil) {
        self.preferences = preferences
        self.audio = audio
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Layout.leastSize),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = loc("prefs.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.preferencesWindow)
        window.isReleasedWhenClosed = false
        window.contentMinSize = Layout.leastSize
        super.init(window: window)

        let general = NSTabViewItem(identifier: "general")
        general.label = loc("prefs.general")
        general.view = buildGeneralTab()
        tabs.addTabViewItem(general)
        let audioTab = NSTabViewItem(identifier: "audio")
        audioTab.label = loc("prefs.audio")
        audioTab.view = buildAudioTab()
        tabs.addTabViewItem(audioTab)
        tabs.delegate = self
        tabs.setAX(AX.preferencesTabs)
        window.contentView = tabs
        window.center()
        reload()
        // An engine that was still starting when the window opened fills the controls in once it is up.
        connectionSub = audio?.connectionStates.sink { [weak self] state in
            guard state == .connected, self?.audio?.loaded != true else { return }
            self?.reload()
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// A line of help or status in the small type under a control.
    static func help(_ text: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
        label.preferredMaxLayoutWidth = Layout.helpWidth
        label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        return label
    }

    /// A control with its help lines under it, in one cell of a form.
    private static func cell(_ control: NSView, _ notes: [NSView]) -> NSStackView {
        // A wrapping label is as wide as the cell, so its height is worked out for that width.
        for note in notes { note.widthAnchor.constraint(equalToConstant: Layout.helpWidth).isActive = true }
        if control is NSPopUpButton {
            let width = control.widthAnchor.constraint(equalToConstant: Layout.helpWidth)
            width.priority = NSLayoutConstraint.Priority(750)
            width.isActive = true
        }
        let stack = NSStackView(views: [control] + notes)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        return stack
    }

    /// A form's label column: text at the right edge, the first line level with the control's.
    private static func formGrid(_ rows: [(String, NSView)]) -> NSGridView {
        let grid = NSGridView(views: rows.map { [NSTextField(labelWithString: $0.0), $0.1] })
        grid.rowSpacing = Layout.rowGap
        grid.columnSpacing = Layout.labelGap
        grid.column(at: 0).xPlacement = .trailing
        grid.column(at: 0).width = Layout.labelColumn
        grid.column(at: 1).xPlacement = .leading
        grid.rowAlignment = .firstBaseline
        grid.translatesAutoresizingMaskIntoConstraints = false
        return grid
    }

    private func buildGeneralTab() -> NSView {
        for behavior in LaunchBehavior.allCases {
            launchPopup.addItem(withTitle: loc(behavior.titleKey))
            launchPopup.lastItem?.representedObject = behavior.rawValue
        }
        launchPopup.target = self
        launchPopup.action = #selector(launchChanged)
        launchPopup.setAX(AX.preferencesLaunchPopup, label: loc("prefs.launch.label"))
        let launchHelp = Self.help(loc("prefs.launch.help"))

        backupField.alignment = .right
        backupField.setAX(AX.preferencesBackupField, label: loc("prefs.general.backups.ax"))
        backupField.target = self
        backupField.action = #selector(backupCommitted)
        backupField.widthAnchor.constraint(equalToConstant: 60).isActive = true
        let unit = NSTextField(labelWithString: loc("prefs.general.backups.unit"))
        let backupRow = NSStackView(views: [backupField, unit])
        backupRow.orientation = .horizontal
        backupRow.spacing = 8
        backupRow.alignment = .firstBaseline
        backupOffNote.textColor = .systemOrange
        backupOffNote.stringValue = loc("prefs.general.backups.off")
        backupOffNote.isHidden = true
        generalMessage.textColor = .systemRed
        generalMessage.setAX(AX.preferencesGeneralMessage)
        generalMessage.isHidden = true

        let grid = Self.formGrid([
            (loc("prefs.launch.label"), Self.cell(launchPopup, [launchHelp])),
            (
                loc("prefs.general.backups"),
                Self.cell(
                    backupRow, [Self.help(loc("prefs.general.backups.note")), backupOffNote, generalMessage])
            ),
        ])
        return Self.page(holding: grid)
    }

    /// A tab's page with `content` pinned at the top left, inside the common margin.
    private static func page(holding content: NSView) -> NSView {
        let page = NSView()
        page.addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: page.leadingAnchor, constant: Layout.inset),
            content.topAnchor.constraint(equalTo: page.topAnchor, constant: Layout.inset),
            content.trailingAnchor.constraint(lessThanOrEqualTo: page.trailingAnchor, constant: -Layout.inset),
        ])
        return page
    }

    private func buildAudioTab() -> NSView {
        clockPopup.setAX(AX.preferencesClockPopup, label: loc("prefs.audio.clock.ax"))
        clockPopup.target = self
        clockPopup.action = #selector(clockChosen)
        // A long device name is cut, never widening the window.
        clockPopup.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        clockNote.textColor = .systemOrange
        clockNote.setAX(AX.preferencesClockNote)
        let clockForm = Self.formGrid([
            (loc("prefs.audio.clock"), Self.cell(clockPopup, [Self.help(loc("prefs.audio.clock.help")), clockNote]))
        ])

        let deviceHeader = NSTextField(labelWithString: loc("prefs.audio.header.device"))
        let bufferHeader = NSTextField(labelWithString: loc("prefs.audio.header.buffer"))
        for label in [deviceHeader, bufferHeader] {
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
            label.textColor = .secondaryLabelColor
        }
        bufferHeader.alignment = .right
        bufferHeader.setContentHuggingPriority(.required, for: .horizontal)
        let header = NSStackView(views: [deviceHeader, bufferHeader])
        header.orientation = .horizontal
        header.distribution = .fill
        deviceHeader.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)

        deviceGrid.rowSpacing = 8
        deviceGrid.columnSpacing = 12
        deviceGrid.translatesAutoresizingMaskIntoConstraints = false
        deviceListNote.translatesAutoresizingMaskIntoConstraints = false
        let holder = FlippedView()
        holder.addSubview(deviceGrid)
        holder.addSubview(deviceListNote)
        NSLayoutConstraint.activate([
            deviceGrid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            deviceGrid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 8),
            // The grid fills the list, so the name column takes what the pop-ups leave and a name is cut
            // at its end only when that is not enough.
            deviceGrid.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -8),
            deviceListNote.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 8),
            deviceListNote.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -8),
            deviceListNote.topAnchor.constraint(equalTo: deviceGrid.bottomAnchor),
            holder.bottomAnchor.constraint(equalTo: deviceListNote.bottomAnchor, constant: 8),
        ])
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.drawsBackground = false
        holder.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = holder
        holder.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor).isActive = true
        scroll.setAX(AX.preferencesDeviceList, label: loc("prefs.audio.devices"))
        audioMessage.textColor = .systemRed
        audioMessage.setAX(AX.preferencesAudioMessage)
        let bufferHelp = Self.help(loc("prefs.audio.buffer.help"))
        // Under the list, whose least width is 460: wider than the form's help.
        bufferHelp.preferredMaxLayoutWidth = 440
        audioMessage.preferredMaxLayoutWidth = 440

        let stack = NSStackView(views: [clockForm, header, scroll, bufferHelp, audioMessage])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.setCustomSpacing(Layout.rowGap + 6, after: clockForm)
        stack.edgeInsets = NSEdgeInsets(
            top: Layout.inset, left: Layout.inset, bottom: Layout.inset, right: Layout.inset)
        stack.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        let edges = Layout.inset * 2
        for v in [header, scroll] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -edges).isActive = true
        }
        for v in [bufferHelp, audioMessage] {
            v.widthAnchor.constraint(lessThanOrEqualTo: stack.widthAnchor, constant: -edges).isActive = true
        }
        clockForm.trailingAnchor.constraint(lessThanOrEqualTo: stack.trailingAnchor, constant: -Layout.inset)
            .isActive = true
        // The same left edge as the General tab's form.
        clockForm.leadingAnchor.constraint(equalTo: stack.leadingAnchor, constant: Layout.inset).isActive = true
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 80).isActive = true
        // The page fills the tab: the device list takes the width and the height left.
        let page = NSView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        page.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: page.topAnchor),
            stack.leadingAnchor.constraint(equalTo: page.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: page.trailingAnchor),
            stack.bottomAnchor.constraint(equalTo: page.bottomAnchor),
        ])
        stack.distribution = .fill
        return page
    }

    /// Shows the stored choices (the window is reused between openings) and reads the devices.
    func reload() {
        launchPopup.selectItem(at: LaunchBehavior.allCases.firstIndex(of: preferences.behavior) ?? 0)
        renderAudio()
        guard let audio else { return }
        refreshing = Task {
            await audio.refresh()
            await audio.refreshBackups()
            renderAudio()
        }
    }

    func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        if tabViewItem?.identifier as? String == "audio" { reload() }
    }

    /// Draws what the engine listed at the last refresh. The engine's controls wait, greyed, while it
    /// starts and are drawn again with its answer; the launch choice is always live.
    func renderAudio() {
        let devices = audio?.devices ?? []
        let loaded = audio?.loaded == true
        let starting = audio?.isStarting == true
        let waiting = loc("prefs.engine.waiting")
        clockPopup.removeAllItems()
        if loaded {
            clockPopup.addItem(withTitle: loc("settings.audio.systemDefault"))
            clockPopup.lastItem?.representedObject = nil
            for d in devices {
                clockPopup.addItem(withTitle: d.name)
                clockPopup.lastItem?.representedObject = d.name
            }
            if let chosen = audio?.chosenDevice, !devices.contains(where: { $0.name == chosen }) {
                clockPopup.addItem(withTitle: String(format: loc("settings.audio.notAvailable"), chosen))
                clockPopup.lastItem?.representedObject = chosen
            }
            let chosen = audio?.chosenDevice
            clockPopup.selectItem(
                at: clockPopup.itemArray.firstIndex { ($0.representedObject as? String) == chosen } ?? 0)
        } else {
            clockPopup.addItem(withTitle: starting ? waiting : loc("prefs.engine.unavailable"))
        }
        clockPopup.isEnabled = loaded

        var notes: [String] = []
        if audio?.chosenDeviceMissing == true, let chosen = audio?.chosenDevice {
            notes.append(String(format: loc("settings.audio.chosenMissing"), chosen))
        } else if audio?.needsRestart == true {
            notes.append(loc("settings.audio.restartNeeded"))
        }
        clockNote.stringValue = notes.joined(separator: "\n")
        clockNote.isHidden = notes.isEmpty

        renderDeviceRows(devices: devices, loaded: loaded)
        deviceListNote.stringValue =
            loaded ? (devices.isEmpty ? loc("prefs.audio.none") : "") : (starting ? loc("prefs.engine.starting") : "")
        deviceListNote.isHidden = deviceListNote.stringValue.isEmpty

        // A refusal is red; an engine that has not answered yet is only said to be starting.
        let failure = starting ? nil : audio?.message
        audioMessage.stringValue = failure ?? ""
        audioMessage.isHidden = failure == nil
        if !loaded && !starting && failure == nil {
            audioMessage.stringValue = loc("prefs.engine.unavailable.detail")
            audioMessage.isHidden = false
        }

        backupField.isEnabled = audio?.backupKeep != nil
        backupField.placeholderString = audio?.backupKeep == nil ? waiting : nil
        if backupField.currentEditor() == nil { backupField.stringValue = audio?.backupKeep.map(String.init) ?? "" }
        backupOffNote.isHidden = audio?.backupKeep != 0
        generalMessage.stringValue = audio?.backupMessage ?? ""
        generalMessage.isHidden = generalMessage.stringValue.isEmpty
    }

    /// One row per listed device: name, channels and whether it is the clock, then its buffer-size
    /// pop-up. A clock device set earlier and not connected now is a last row that says so, its
    /// pop-up greyed.
    private func renderDeviceRows(devices: [DeviceEntry], loaded: Bool) {
        // Removing a row leaves its views in the grid: each goes first.
        while deviceGrid.numberOfRows > 0 {
            let row = deviceGrid.row(at: deviceGrid.numberOfRows - 1)
            for c in 0..<row.numberOfCells { row.cell(at: c).contentView?.removeFromSuperview() }
            deviceGrid.removeRow(at: deviceGrid.numberOfRows - 1)
        }
        bufferPopups = []
        for (i, d) in devices.enumerated() {
            var details = String(format: loc("prefs.audio.outputs"), d.outputChannels)
            if d.primary {
                details += " · " + loc("prefs.audio.clockNow")
            } else if audio?.needsRestart == true, d.name == audio?.nextStartDevice {
                details += " · " + loc("prefs.audio.clockNext")
            }
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            for size in AudioPreferencesModel.bufferSizes {
                popup.addItem(withTitle: String(size))
                popup.lastItem?.tag = size
            }
            if popup.itemArray.first(where: { $0.tag == d.bufferFrames }) == nil {
                popup.addItem(withTitle: String(d.bufferFrames))
                popup.lastItem?.tag = d.bufferFrames
            }
            popup.selectItem(withTag: d.bufferFrames)
            popup.setAX(
                "\(AX.preferencesBufferPopup)-\(i + 1)", label: String(format: loc("prefs.audio.buffer"), d.name))
            popup.target = self
            popup.action = #selector(bufferChosen(_:))
            popup.tag = i
            popup.isEnabled = loaded
            bufferPopups.append(popup)
            deviceGrid.addRow(with: [deviceLabel(d.name, details), popup])
        }
        if loaded, let chosen = audio?.chosenDevice, !devices.contains(where: { $0.name == chosen }) {
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            popup.addItem(withTitle: "—")
            popup.isEnabled = false
            popup.setAX(
                "\(AX.preferencesBufferPopup)-missing", label: String(format: loc("prefs.audio.buffer"), chosen))
            deviceGrid.addRow(with: [deviceLabel(chosen, loc("prefs.audio.missing"), warn: true), popup])
        }
        if deviceGrid.numberOfColumns > 1 { deviceGrid.column(at: 1).xPlacement = .trailing }
    }

    /// A device's name over its detail line, the name cut at its end when the list is narrow.
    private func deviceLabel(_ name: String, _ detail: String, warn: Bool = false) -> NSStackView {
        let title = NSTextField(labelWithString: name)
        title.lineBreakMode = .byTruncatingTail
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        title.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let sub = NSTextField(labelWithString: detail)
        sub.textColor = warn ? .systemOrange : .secondaryLabelColor
        sub.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        sub.lineBreakMode = .byTruncatingTail
        sub.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let column = NSStackView(views: [title, sub])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 1
        column.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        column.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return column
    }

    @objc private func backupCommitted() {
        guard let audio else { return }
        let text = backupField.stringValue
        refreshing = Task {
            await audio.setBackupKeep(text)
            backupField.stringValue = audio.backupKeep.map(String.init) ?? ""
            renderAudio()
        }
    }

    @objc private func launchChanged() {
        guard let raw = launchPopup.selectedItem?.representedObject as? String,
            let behavior = LaunchBehavior(rawValue: raw)
        else { return }
        preferences.behavior = behavior
    }

    @objc private func clockChosen() {
        guard let audio else { return }
        let name = clockPopup.selectedItem?.representedObject as? String
        refreshing = Task {
            await audio.chooseDevice(name)
            renderAudio()
        }
    }

    @objc private func bufferChosen(_ sender: NSPopUpButton) {
        guard let audio, audio.devices.indices.contains(sender.tag), let frames = sender.selectedItem?.tag else {
            return
        }
        let device = audio.devices[sender.tag].name
        refreshing = Task {
            await audio.setBufferSize(device, frames: frames)
            renderAudio()
        }
    }
}

/// A plain view whose origin is at the top, for content laid out from the top in a scroll view.
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
