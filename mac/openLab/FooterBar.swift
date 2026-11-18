// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

enum LayoutArea: CaseIterable {
    case toolbar, masthead, inspector, sidebar
}

/// Bottom bar: Edit/Show switch (Show Mode), area toggles, status text, settings.
final class FooterBar: NSView, NSPopoverDelegate {
    var onToggle: ((LayoutArea, Bool) -> Void)?
    /// The gear was used: open the workspace settings.
    var onSettings: (() -> Void)?
    /// The switch was used (true: Show Mode).
    var onModeChange: ((Bool) -> Void)?

    let modeSwitch = NSSegmentedControl(
        labels: [loc("footer.mode.edit"), loc("footer.mode.show")],
        trackingMode: .selectOne, target: nil, action: nil)
    /// The engine's health (DX-03); clicking it opens the details. While all is well it is a small mark
    /// without words, so the details (CPU, engine load, dropouts, device format) stay one click away.
    let statusButton = NSButton()
    /// Red "NETWORK OUTPUT OFF" while the engine's output override is on (PB-10). Always in the
    /// footer, Show Mode included; words and a filled red pill, never colour alone. Opens the same
    /// details as the status line.
    let overrideBadge = NSButton()
    /// The footer's own entry to the Warnings tab: shown only while there is something to see, with
    /// the count. Part of the footer in Show Mode too. Never takes the keys from the cue list.
    let warningsButton = NSButton()
    /// The warnings button was used: open the sidebar on the Warnings tab.
    var onWarnings: (() -> Void)?
    private(set) var presentation = StatusPresentation.make(nil)
    /// A note that needs no answer (an edit refused, a crash note); clears after a while
    /// (`noticeLifetime`) unless replaced.
    let noticeLabel = NSTextField(labelWithString: "")
    /// "12 cues in 2 lists", from the model (QLab's footer centre).
    let countLabel = NSTextField(labelWithString: "")
    /// The dropout total the operator has seen: at the first status, or in the details they last
    /// closed. Dropouts past it keep the warning up until the details are opened and closed again.
    private var seenXruns: UInt64?
    /// The last status shown, to show again once new dropouts are seen.
    private var lastStatus: SystemStatus?
    /// The dropout total in the details now open.
    private var xrunsInDetails: UInt64?
    private(set) var popover: NSPopover?
    let settingsButton = NSButton()
    private(set) var toggles: [LayoutArea: NSButton] = [:]

    private static let specs: [(area: LayoutArea, id: String, icon: Icon, key: String)] = [
        (.masthead, AX.toggleMasthead, .layoutMasthead, "footer.toggle.masthead"),
        (.inspector, AX.toggleInspector, .layoutInspector, "footer.toggle.inspector"),
        (.sidebar, AX.toggleSidebar, .layoutSidebar, "footer.toggle.sidebar"),
    ]

    static var icons: [Icon] { specs.map { $0.icon } + [.settings] }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.footer)
        modeSwitch.selectedSegment = 0
        modeSwitch.setAX(AX.modeSwitch, label: loc("footer.mode.label"))
        modeSwitch.target = self
        modeSwitch.action = #selector(modeChanged)
        overrideBadge.isBordered = false
        overrideBadge.wantsLayer = true
        overrideBadge.layer?.backgroundColor = NSColor.systemRed.cgColor
        overrideBadge.layer?.cornerRadius = 4
        overrideBadge.attributedTitle = NSAttributedString(
            string: " " + loc("footer.override.badge") + " ",
            attributes: [.foregroundColor: NSColor.white, .font: NSFont.systemFont(ofSize: 12, weight: .bold)])
        overrideBadge.setAX(AX.overrideBadge, label: loc("footer.override.badge"))
        overrideBadge.toolTip = loc("footer.override.tooltip")
        overrideBadge.target = self
        overrideBadge.action = #selector(showStatusDetails(_:))
        overrideBadge.isHidden = true
        warningsButton.isBordered = false
        warningsButton.imagePosition = .imageLeading
        warningsButton.refusesFirstResponder = true
        // The notice has no minimum width (it truncates): its old 120 pt minimum is what made a new
        // footer control widen the window. At the minimum width the button does not widen it, in Edit
        // and Show Mode; Show Mode with the override badge is already wider than the minimum.
        warningsButton.setAX(AX.warningsButton)
        warningsButton.toolTip = loc("footer.warnings.tooltip")
        warningsButton.target = self
        warningsButton.action = #selector(warningsClicked)
        showWarnings(count: 0)
        statusButton.isBordered = false
        statusButton.imagePosition = .imageLeading
        statusButton.setAX(AX.statusLabel)
        statusButton.target = self
        statusButton.action = #selector(showStatusDetails(_:))
        countLabel.setAX(AX.footerCount)
        countLabel.textColor = .secondaryLabelColor
        countLabel.font = .systemFont(ofSize: 12)
        showCount(cues: 0, lists: 0)
        noticeLabel.setAX(AX.footerNotice)
        noticeLabel.textColor = .secondaryLabelColor
        noticeLabel.lineBreakMode = .byTruncatingTail
        noticeLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        show(status: nil, connected: true)

        for spec in Self.specs {
            let name = loc(spec.key)
            let b = NSButton()
            b.setButtonType(.pushOnPushOff)
            b.bezelStyle = .texturedRounded
            b.image = spec.icon.image(name)
            b.toolTip = name
            b.setAX(spec.id, label: name)
            b.target = self
            b.action = #selector(toggled(_:))
            b.state = spec.area == .sidebar ? .off : .on  // sidebar is hidden by default
            b.tag = LayoutArea.allCases.firstIndex(of: spec.area) ?? 0
            toggles[spec.area] = b
        }
        settingsButton.image = Icon.settings.image(loc("footer.settings"))
        settingsButton.setAX(AX.settingsButton, label: loc("footer.settings"))
        settingsButton.toolTip = loc("footer.settings.open")
        settingsButton.target = self
        settingsButton.action = #selector(settingsClicked)
        settingsButton.bezelStyle = .texturedRounded

        // QLab's footer: the mode switch and the one GO area toggle on the left; the count in the middle;
        // the warnings mark with the inspector, sidebar and settings buttons on the right.
        let left = NSStackView(views: [modeSwitch, overrideBadge] + [.masthead].compactMap { toggles[$0] })
        let right = NSStackView(
            views: [warningsButton] + [.inspector, .sidebar].compactMap { toggles[$0] } + [settingsButton])
        let centre = NSStackView(views: [countLabel, statusButton, noticeLabel])
        let bar = NSStackView(views: [left, centre, right])
        bar.distribution = .equalCentering
        bar.translatesAutoresizingMaskIntoConstraints = false
        addSubview(bar)
        NSLayoutConstraint.activate([
            bar.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            bar.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            bar.topAnchor.constraint(equalTo: topAnchor, constant: 6),
            bar.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -6),
        ])
    }

    /// The cue count text; the plural forms are separate strings (the app has no stringsdict).
    static func countText(cues: Int, lists: Int) -> String {
        let cuesText = cues == 1 ? loc("footer.count.cue.one") : String(format: loc("footer.count.cue.many"), cues)
        let listsText = lists == 1 ? loc("footer.count.list.one") : String(format: loc("footer.count.list.many"), lists)
        return String(format: loc("footer.count"), cuesText, listsText)
    }

    func showCount(cues: Int, lists: Int) {
        countLabel.stringValue = Self.countText(cues: cues, lists: lists)
    }

    /// Shows the warnings button with `count` (a broken cue or a warning each count once), or hides it at 0.
    func showWarnings(count: Int) {
        warningsButton.isHidden = count == 0
        let text = String(format: loc(count == 1 ? "warnings.one" : "warnings.many"), count)
        // Icon and number only: the footer is nearly full at the minimum window width. The icon is
        // not the only signal (the number is text), and VoiceOver reads the full words.
        warningsButton.title = String(count)
        warningsButton.image = Icon.statusWarning.image(text, compact: true)
        warningsButton.contentTintColor = MarkColour.caution
        warningsButton.font = .systemFont(ofSize: 12, weight: .semibold)
        warningsButton.setAccessibilityLabel(text)
    }

    @objc private func warningsClicked() { onWarnings?() }

    /// Redraws the status line. The alarm state differs in symbol and wording as well as colour.
    func showNotice(_ text: String, error: Bool = false, tooltip: String? = nil) {
        noticeLabel.stringValue = text
        noticeLabel.toolTip = tooltip
        noticeLabel.textColor = error ? .systemRed : .secondaryLabelColor
        // An old notice must not stay up for the whole show. Each notice has its own generation, so
        // an earlier timer never clears a later notice, even one with the same text.
        noticeGeneration += 1
        let mine = noticeGeneration
        scheduleClear(noticeLifetime) { [weak self] in
            guard let self, self.noticeGeneration == mine else { return }
            self.noticeLabel.stringValue = ""
            self.noticeLabel.toolTip = nil
            self.noticeLabel.textColor = .secondaryLabelColor
        }
    }

    /// Seconds a notice stays up.
    let noticeLifetime: TimeInterval = 60
    private var noticeGeneration = 0
    /// Runs `work` after `delay` seconds on the main queue (a seam: tests fire it by hand).
    var scheduleClear: (TimeInterval, @escaping @MainActor () -> Void) -> Void = { delay, work in
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { MainActor.assumeIsolated { work() } }
    }

    /// Without a connection the last status is stale: the line says the engine is offline.
    func show(status: SystemStatus?, connected: Bool) {
        let p: StatusPresentation
        if !connected {
            // A new engine counts from zero: its first status is the new baseline.
            seenXruns = nil
            // The details still open list the old engine's total; closing them must not raise the
            // new engine's baseline to it.
            xrunsInDetails = nil
            lastStatus = nil
            p = StatusPresentation.offline()
        } else {
            if let s = status {
                // A lower total is a count that started again (the engine restarted): nothing new in it yet.
                seenXruns = min(seenXruns ?? s.xruns, s.xruns)
            }
            lastStatus = status
            let unseen = status.map { s in seenXruns.map { s.xruns > $0 } ?? false } ?? false
            p = StatusPresentation.make(status, unseenXruns: unseen)
        }
        presentation = p
        // Stale status (no connection) is not shown as an override either.
        overrideBadge.isHidden = !(connected && status?.networkOutputDisabled == true)
        // While all is well the footer says nothing about the audio: the mark alone, with the summary as
        // its tip. A warning or an alarm (a lost or failed device, an offline engine) is drawn in words.
        let normal = p.level == .normal
        statusButton.title = normal ? "" : p.summary
        statusButton.imagePosition = normal ? .imageOnly : .imageLeading
        let image = p.icon.image(p.summary, compact: true)
        // All well: only the mark is green.
        statusButton.image =
            p.icon == .statusNormal
            ? TimeHeaderCell.tinted(image, .systemGreen, size: image.size, description: p.summary) : image
        statusButton.contentTintColor = p.level == .alarm ? .systemRed : p.level == .warning ? MarkColour.caution : nil
        statusButton.font = .systemFont(ofSize: 12, weight: normal ? .regular : .semibold)
        statusButton.setAccessibilityLabel(p.summary)
        statusButton.toolTip = normal ? p.summary + "\n" + loc("status.tooltip") : loc("status.tooltip")
    }

    /// Opens the details beside `sender`: the status mark or line, or the override badge.
    @objc func showStatusDetails(_ sender: NSView? = nil) {
        popover?.close()
        let text = NSTextField(wrappingLabelWithString: presentation.details.joined(separator: "\n"))
        text.setAX(AX.statusDetails)
        text.isSelectable = true
        text.preferredMaxLayoutWidth = 340
        text.frame = NSRect(x: 12, y: 12, width: 340, height: max(40, text.fittingSize.height))
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 364, height: text.frame.height + 24))
        content.addSubview(text)
        let controller = NSViewController()
        controller.view = content
        let p = NSPopover()
        p.behavior = .transient
        p.contentViewController = controller
        p.delegate = self
        xrunsInDetails = lastStatus?.xruns
        let anchor = sender ?? statusButton
        p.show(relativeTo: anchor.bounds, of: anchor, preferredEdge: .maxY)
        popover = p
    }

    /// The details were read: the dropouts they listed are seen, and their warning goes. Done on close,
    /// so the line the popover points at does not change under it.
    func popoverDidClose(_ notification: Notification) {
        guard let closed = notification.object as? NSPopover, closed === popover else { return }
        if let seen = xrunsInDetails { seenXruns = max(seenXruns ?? seen, seen) }
        xrunsInDetails = nil
        // The same status again: only whether its dropouts are new changes.
        if let status = lastStatus { show(status: status, connected: true) }
    }

    /// Reflects the window's mode in the switch and the indicator (also when set from the menu).
    func setShowMode(_ on: Bool) {
        modeSwitch.selectedSegment = on ? 1 : 0
    }

    @objc private func settingsClicked() { onSettings?() }

    @objc private func modeChanged() { onModeChange?(modeSwitch.selectedSegment == 1) }

    @objc private func toggled(_ sender: NSButton) {
        guard LayoutArea.allCases.indices.contains(sender.tag) else { return }
        onToggle?(LayoutArea.allCases[sender.tag], sender.state == .on)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}
