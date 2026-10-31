// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

enum LayoutArea: CaseIterable {
    case toolbar, masthead, inspector, sidebar
}

/// The time of day in the footer (PB-12). QLab's window has no clock; a show caller wants one in
/// view, so it sits in the footer where it never covers a cue. `now` is injectable: tests set a
/// fixed date and call `refresh()`; the one-second timer runs only while the label is in a window.
final class WallClockLabel: NSTextField {
    var now: () -> Date = { Date() }
    private(set) var timer: Timer?

    /// The user's own time style (12 or 24 hour) with seconds.
    static func makeFormatter(locale: Locale = .current, timeZone: TimeZone = .current) -> DateFormatter {
        let f = DateFormatter()
        f.locale = locale
        f.timeZone = timeZone
        f.dateStyle = .none
        f.timeStyle = .medium
        return f
    }

    var dateFormatter = WallClockLabel.makeFormatter()

    init() {
        super.init(frame: .zero)
        isEditable = false
        isBordered = false
        drawsBackground = false
        font = .monospacedDigitSystemFont(ofSize: 13, weight: .regular)
        textColor = .secondaryLabelColor
        setAX(AX.wallClock, label: loc("footer.clock.label"))
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func refresh() { stringValue = dateFormatter.string(from: now()) }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        timer?.invalidate()
        timer = nil
        guard window != nil else { return }
        refresh()
        let t = Timer(timeInterval: 1, repeats: true) { [weak self] t in
            // Closing a window doesn't detach its views, so viewDidMoveToWindow(nil) may never
            // come: the timer ends itself once the label or its window is gone.
            let alive = MainActor.assumeIsolated { () -> Bool in
                guard let self, let window = self.window else { return false }
                if window.isVisible { self.refresh() }
                return true
            }
            if !alive { t.invalidate() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }
}

/// Bottom bar: Edit/Show switch (Show Mode, M5.12), area toggles, status text, settings.
final class FooterBar: NSView {
    var onToggle: ((LayoutArea, Bool) -> Void)?
    /// The gear was used: open the workspace settings.
    var onSettings: (() -> Void)?
    /// The switch was used (true: Show Mode).
    var onModeChange: ((Bool) -> Void)?
    /// "SHOW MODE" text: the mode is never shown by colour alone.
    let showModeLabel = NSTextField(labelWithString: "")

    let modeSwitch = NSSegmentedControl(
        labels: [loc("footer.mode.edit"), loc("footer.mode.show")],
        trackingMode: .selectOne, target: nil, action: nil)
    /// The engine's health in one line (DX-03); clicking it opens the details.
    let statusButton = NSButton()
    private(set) var presentation = StatusPresentation.make(nil)
    /// A result the operator asked for that needs no answer (a relink); clears after a while (`noticeLifetime`) unless replaced.
    let noticeLabel = NSTextField(labelWithString: "")
    /// "12 cues in 2 lists", from the model (QLab's footer centre).
    let countLabel = NSTextField(labelWithString: "")
    private var lastXruns: UInt64?
    private var popover: NSPopover?
    let clock = WallClockLabel()
    let settingsButton = NSButton()
    private(set) var toggles: [LayoutArea: NSButton] = [:]

    private static let specs: [(area: LayoutArea, id: String, icon: Icon, key: String)] = [
        (.toolbar, AX.toggleToolbar, .layoutToolbar, "footer.toggle.toolbar"),
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
        showModeLabel.setAX(AX.showModeLabel, label: loc("footer.showMode.indicator"))
        showModeLabel.stringValue = loc("footer.showMode.indicator")
        showModeLabel.font = .systemFont(ofSize: 12, weight: .bold)
        showModeLabel.textColor = .systemOrange
        showModeLabel.isHidden = true
        statusButton.isBordered = false
        statusButton.imagePosition = .imageLeading
        statusButton.setAX(AX.statusLabel)
        statusButton.target = self
        statusButton.action = #selector(showStatusDetails)
        countLabel.setAX(AX.footerCount)
        countLabel.textColor = .secondaryLabelColor
        countLabel.font = .systemFont(ofSize: 12)
        showCount(cues: 0, lists: 0)
        noticeLabel.setAX(AX.footerNotice)
        noticeLabel.textColor = .secondaryLabelColor
        noticeLabel.lineBreakMode = .byTruncatingTail
        noticeLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let minNotice = noticeLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 120)
        minNotice.priority = NSLayoutConstraint.Priority(749)
        minNotice.isActive = true
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

        let left = NSStackView(views: [modeSwitch, showModeLabel] + [.toolbar, .masthead].compactMap { toggles[$0] })
        let right = NSStackView(views: [clock] + [.inspector, .sidebar].compactMap { toggles[$0] } + [settingsButton])
        let bar = NSStackView(views: [left, NSStackView(views: [countLabel, statusButton, noticeLabel]), right])
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

    /// Redraws the status line. The alarm state differs in symbol and wording as well as colour.
    func showNotice(_ text: String, error: Bool = false) {
        noticeLabel.stringValue = text
        noticeLabel.textColor = error ? .systemRed : .secondaryLabelColor
        // An old notice must not stay up for the whole show. Each notice has its own generation, so
        // an earlier timer never clears a later notice, even one with the same text.
        noticeGeneration += 1
        let mine = noticeGeneration
        scheduleClear(noticeLifetime) { [weak self] in
            guard let self, self.noticeGeneration == mine else { return }
            self.noticeLabel.stringValue = ""
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
            lastXruns = nil
            p = StatusPresentation.offline()
        } else {
            let increased = status.map { s in lastXruns.map { s.xruns > $0 } ?? false } ?? false
            lastXruns = status?.xruns
            p = StatusPresentation.make(status, xrunsIncreased: increased)
        }
        presentation = p
        statusButton.title = p.summary
        statusButton.image = p.icon.image(p.summary, compact: true)
        statusButton.contentTintColor = p.level == .alarm ? .systemRed : p.level == .warning ? .systemOrange : nil
        statusButton.font = .systemFont(ofSize: 12, weight: p.level == .normal ? .regular : .semibold)
        statusButton.setAccessibilityLabel(p.summary)
        statusButton.toolTip = loc("status.tooltip")
    }

    @objc func showStatusDetails() {
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
        p.show(relativeTo: statusButton.bounds, of: statusButton, preferredEdge: .maxY)
        popover = p
    }

    /// Reflects the window's mode in the switch and the indicator (also when set from the menu).
    func setShowMode(_ on: Bool) {
        modeSwitch.selectedSegment = on ? 1 : 0
        showModeLabel.isHidden = !on
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
