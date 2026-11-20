// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

enum LayoutArea: CaseIterable {
    case toolbar, masthead, inspector, sidebar
}

/// Bottom bar: Edit/Show switch (Show Mode), area toggles, status text, settings.
final class FooterBar: NSView {
    var onToggle: ((LayoutArea, Bool) -> Void)?
    /// The gear was used: open the workspace settings.
    var onSettings: (() -> Void)?
    /// The switch was used (true: Show Mode).
    var onModeChange: ((Bool) -> Void)?

    let modeSwitch = NSSegmentedControl(
        labels: [loc("footer.mode.edit"), loc("footer.mode.show")],
        trackingMode: .selectOne, target: nil, action: nil)
    /// The alarm for an audio device the engine lost or gave up on (a show-safety alarm, drawn in words
    /// and a symbol, not by colour alone). Present only while a device is in that state: with all well
    /// the footer says nothing about the audio. Pressing it opens Status > Warnings, where the same
    /// fault is a row.
    let alarmButton = NSButton()
    /// The footer's own entry to the Warnings page: shown only while there is something to see, with
    /// the count. Part of the footer in Show Mode too. Never takes the keys from the cue list.
    let warningsButton = NSButton()
    /// The warnings button or the alarm was used: open the Status window on its Warnings page.
    var onWarnings: (() -> Void)?
    /// A note that needs no answer (an edit refused, a crash note); clears after a while
    /// (`noticeLifetime`) unless replaced.
    let noticeLabel = NSTextField(labelWithString: "")
    /// "12 cues in 2 lists", from the model (QLab's footer centre).
    let countLabel = NSTextField(labelWithString: "")
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
        warningsButton.isBordered = false
        warningsButton.imagePosition = .imageLeading
        warningsButton.refusesFirstResponder = true
        // The notice has no minimum width (it truncates): its old 120 pt minimum is what made a new
        // footer control widen the window. At the minimum width the button does not widen it, in Edit
        // and Show Mode.
        warningsButton.setAX(AX.warningsButton)
        warningsButton.toolTip = loc("footer.warnings.tooltip")
        warningsButton.target = self
        warningsButton.action = #selector(warningsClicked)
        showWarnings(count: 0)
        alarmButton.isBordered = false
        alarmButton.imagePosition = .imageLeading
        alarmButton.refusesFirstResponder = true
        alarmButton.setAX(AX.deviceAlarm)
        alarmButton.toolTip = loc("footer.alarm.tooltip")
        alarmButton.target = self
        alarmButton.action = #selector(warningsClicked)
        alarmButton.isHidden = true
        countLabel.setAX(AX.footerCount)
        countLabel.textColor = .secondaryLabelColor
        countLabel.font = .systemFont(ofSize: 12)
        showCount(cues: 0, lists: 0)
        noticeLabel.setAX(AX.footerNotice)
        noticeLabel.textColor = .secondaryLabelColor
        noticeLabel.lineBreakMode = .byTruncatingTail
        noticeLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)

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
        let left = NSStackView(views: [modeSwitch] + [.masthead].compactMap { toggles[$0] })
        let right = NSStackView(
            views: [warningsButton] + [.inspector, .sidebar].compactMap { toggles[$0] } + [settingsButton])
        let centre = NSStackView(views: [countLabel, alarmButton, noticeLabel])
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

    /// The alarm text for the engine's `status`: the audio device, or an output device patches use, that
    /// was lost or failed; nil while all is well (or with no status: an engine that is away has its own
    /// alarm). The primary device comes first.
    static func alarmText(_ status: SystemStatus?) -> String? {
        guard let s = status else { return nil }
        var named: String?
        switch s.device.state {
        case .lost: named = loc("footer.alarm.deviceLost")
        case .failed: named = loc("footer.alarm.deviceFailed")
        default: break
        }
        if named == nil {
            // A secondary device the engine lost or gave up on stops the cues patched to it: name it.
            for o in s.outputs where !o.primary {
                if o.state == .lost { named = String(format: loc("footer.alarm.outputLost"), o.name) }
                if o.state == .failed { named = String(format: loc("footer.alarm.outputFailed"), o.name) }
                if named != nil { break }
            }
        }
        return named.map { loc("footer.alarm") + " " + $0 }
    }

    /// Shows or clears the device alarm. Without a connection the last status is stale: nothing is
    /// shown (the engine alarm window speaks for a missing engine).
    func show(status: SystemStatus?, connected: Bool) {
        let text = connected ? Self.alarmText(status) : nil
        alarmButton.isHidden = text == nil
        guard let text else { return }
        alarmButton.title = text
        alarmButton.image = Icon.statusAlarm.image(text, compact: true)
        alarmButton.contentTintColor = .systemRed
        alarmButton.font = .systemFont(ofSize: 12, weight: .semibold)
        alarmButton.setAccessibilityLabel(text)
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
