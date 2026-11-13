// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The app-wide preferences (the app menu's Preferences; Workspace Settings are per workspace).
/// General tab: what happens at launch. Audio tab (QLab 5's hardware tab, manual p. 87): each output
/// device with the buffer size it is asked for, and the device whose clock the show runs on.
@MainActor
final class PreferencesWindowController: NSWindowController, NSTabViewDelegate {
    let launchPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    /// Sized before its tabs are added: a zero-sized tab view lays its tab strip out at a negative width.
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))
    let clockPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let clockNote = NSTextField(wrappingLabelWithString: "")
    let deviceGrid = NSGridView()
    let audioMessage = NSTextField(wrappingLabelWithString: "")
    /// One buffer-size pop-up per listed device, in list order.
    private(set) var bufferPopups: [NSPopUpButton] = []
    let audio: AudioPreferencesModel?
    private let preferences: LaunchPreferences
    /// The last refresh, so tests can wait for it.
    private(set) var refreshing: Task<Void, Never>?

    init(preferences: LaunchPreferences, audio: AudioPreferencesModel? = nil) {
        self.preferences = preferences
        self.audio = audio
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 380),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = loc("prefs.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.preferencesWindow)
        window.isReleasedWhenClosed = false
        window.contentMinSize = NSSize(width: 420, height: 300)
        super.init(window: window)

        let label = NSTextField(labelWithString: loc("prefs.launch.label"))
        for behavior in LaunchBehavior.allCases {
            launchPopup.addItem(withTitle: loc(behavior.titleKey))
            launchPopup.lastItem?.representedObject = behavior.rawValue
        }
        launchPopup.target = self
        launchPopup.action = #selector(launchChanged)
        launchPopup.setAX(AX.preferencesLaunchPopup)
        let row = NSStackView(views: [label, launchPopup])
        row.orientation = .horizontal
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        let general = NSView()
        row.translatesAutoresizingMaskIntoConstraints = false
        general.addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: general.leadingAnchor),
            row.topAnchor.constraint(equalTo: general.topAnchor),
            row.trailingAnchor.constraint(lessThanOrEqualTo: general.trailingAnchor),
        ])

        let tab = NSTabViewItem(identifier: "general")
        tab.label = loc("prefs.general")
        tab.view = general
        tabs.addTabViewItem(tab)
        let audioTab = NSTabViewItem(identifier: "audio")
        audioTab.label = loc("prefs.audio")
        audioTab.view = buildAudioTab()
        tabs.addTabViewItem(audioTab)
        tabs.delegate = self
        tabs.setAX(AX.preferencesTabs)
        window.contentView = tabs
        window.center()
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    private func buildAudioTab() -> NSView {
        let clockTitle = NSTextField(labelWithString: loc("prefs.audio.clock"))
        clockTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        clockPopup.setAX(AX.preferencesClockPopup, label: loc("prefs.audio.clock"))
        clockPopup.target = self
        clockPopup.action = #selector(clockChosen)
        // A long device name is cut, never widening the window.
        clockPopup.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        clockNote.textColor = .systemOrange
        clockNote.setAX(AX.preferencesClockNote)
        let devicesTitle = NSTextField(labelWithString: loc("prefs.audio.devices"))
        devicesTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        deviceGrid.rowSpacing = 6
        deviceGrid.columnSpacing = 12
        deviceGrid.translatesAutoresizingMaskIntoConstraints = false
        let holder = FlippedView()
        holder.addSubview(deviceGrid)
        NSLayoutConstraint.activate([
            deviceGrid.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 4),
            deviceGrid.topAnchor.constraint(equalTo: holder.topAnchor, constant: 4),
            deviceGrid.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -4),
            holder.bottomAnchor.constraint(equalTo: deviceGrid.bottomAnchor, constant: 4),
        ])
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.drawsBackground = false
        holder.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = holder
        holder.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor).isActive = true
        scroll.setAX(AX.preferencesDeviceList)
        audioMessage.textColor = .systemRed
        audioMessage.setAX(AX.preferencesAudioMessage)
        for label in [clockNote, audioMessage] {
            label.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            label.preferredMaxLayoutWidth = 360
        }
        let stack = NSStackView(views: [clockTitle, clockPopup, clockNote, devicesTitle, scroll, audioMessage])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        for v in [clockPopup, scroll, clockNote, audioMessage] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32).isActive = true
        }
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 80).isActive = true
        return stack
    }

    /// Shows the stored choices (the window is reused between openings) and reads the devices.
    func reload() {
        launchPopup.selectItem(at: LaunchBehavior.allCases.firstIndex(of: preferences.behavior) ?? 0)
        renderAudio()
        guard let audio else { return }
        refreshing = Task {
            await audio.refresh()
            renderAudio()
        }
    }

    func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        if tabViewItem?.identifier as? String == "audio" { reload() }
    }

    /// Draws the devices as the engine listed them at the last refresh.
    func renderAudio() {
        let devices = audio?.devices ?? []
        let loaded = audio?.loaded == true
        clockPopup.removeAllItems()
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
        clockPopup.isEnabled = loaded

        var notes: [String] = []
        if audio?.chosenDeviceMissing == true, let chosen {
            notes.append(String(format: loc("settings.audio.chosenMissing"), chosen))
        } else if audio?.needsRestart == true {
            notes.append(loc("settings.audio.restartNeeded"))
        }
        clockNote.stringValue = notes.joined(separator: "\n")
        clockNote.isHidden = notes.isEmpty

        while deviceGrid.numberOfRows > 0 { deviceGrid.removeRow(at: 0) }
        bufferPopups = []
        for (i, d) in devices.enumerated() {
            let name = NSTextField(labelWithString: d.name)
            name.lineBreakMode = .byTruncatingMiddle
            name.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            var details = String(format: loc("prefs.audio.outputs"), d.outputChannels)
            if d.primary { details += " · " + loc("prefs.audio.clockNow") }
            let detail = NSTextField(labelWithString: details)
            detail.textColor = .secondaryLabelColor
            detail.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            let column = NSStackView(views: [name, detail])
            column.orientation = .vertical
            column.alignment = .leading
            column.spacing = 1
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
            deviceGrid.addRow(with: [column, popup])
        }
        if deviceGrid.numberOfColumns > 1 { deviceGrid.column(at: 1).xPlacement = .trailing }
        audioMessage.stringValue = audio?.message ?? ""
        audioMessage.isHidden = audioMessage.stringValue.isEmpty
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
