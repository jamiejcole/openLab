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
    private var timer: Timer?

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
        let t = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }
}

/// Bottom bar: Edit/Show switch (visual only until M5.12), area toggles, status text, settings.
final class FooterBar: NSView {
    var onToggle: ((LayoutArea, Bool) -> Void)?

    let modeSwitch = NSSegmentedControl(
        labels: [loc("footer.mode.edit"), loc("footer.mode.show")],
        trackingMode: .selectOne, target: nil, action: nil)
    let statusLabel = NSTextField(labelWithString: loc("footer.status.empty"))
    let clock = WallClockLabel()
    let settingsButton = NSButton()
    private(set) var toggles: [LayoutArea: NSButton] = [:]

    private static let specs: [(area: LayoutArea, id: String, symbol: String, key: String)] = [
        (.toolbar, AX.toggleToolbar, "square.grid.2x2", "footer.toggle.toolbar"),
        (.masthead, AX.toggleMasthead, "rectangle.topthird.inset.filled", "footer.toggle.masthead"),
        (.inspector, AX.toggleInspector, "rectangle.bottomthird.inset.filled", "footer.toggle.inspector"),
        (.sidebar, AX.toggleSidebar, "sidebar.right", "footer.toggle.sidebar"),
    ]

    static var symbolNames: [String] { specs.map { $0.symbol } + ["gearshape"] }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.footer)
        modeSwitch.selectedSegment = 0
        modeSwitch.setAX(AX.modeSwitch, label: loc("footer.mode.label"))
        statusLabel.setAX(AX.statusLabel)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.alignment = .center

        for spec in Self.specs {
            let name = loc(spec.key)
            let b = NSButton()
            b.setButtonType(.pushOnPushOff)
            b.bezelStyle = .texturedRounded
            b.image = symbolImage(spec.symbol, name)
            b.toolTip = name
            b.setAX(spec.id, label: name)
            b.target = self
            b.action = #selector(toggled(_:))
            b.state = spec.area == .sidebar ? .off : .on  // sidebar is hidden by default
            b.tag = LayoutArea.allCases.firstIndex(of: spec.area) ?? 0
            toggles[spec.area] = b
        }
        settingsButton.image = symbolImage("gearshape", loc("footer.settings"))
        settingsButton.setAX(AX.settingsButton, label: loc("footer.settings"))
        settingsButton.toolTip = loc("footer.settings")
        settingsButton.bezelStyle = .texturedRounded

        let left = NSStackView(views: [modeSwitch] + [.toolbar, .masthead].compactMap { toggles[$0] })
        let right = NSStackView(views: [clock] + [.inspector, .sidebar].compactMap { toggles[$0] } + [settingsButton])
        let bar = NSStackView(views: [left, statusLabel, right])
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

    @objc private func toggled(_ sender: NSButton) {
        guard LayoutArea.allCases.indices.contains(sender.tag) else { return }
        onToggle?(LayoutArea.allCases[sender.tag], sender.state == .on)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}
