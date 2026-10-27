// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Row of cue-type buttons in rounded segments. Actions are wired in M5.10.
final class CueToolbarView: NSView {
    private static let groups: [[(symbol: String, key: String)]] = [
        [("speaker.wave.2", "cue.type.audio")],
        [
            ("folder", "cue.type.group"), ("note.text", "cue.type.memo"),
            ("timer", "cue.type.wait"), ("slider.horizontal.3", "cue.type.fade"),
        ],
        [
            ("play.fill", "cue.type.start"), ("stop.fill", "cue.type.stop"),
            ("pause.fill", "cue.type.pause"), ("arrow.down.circle", "cue.type.load"),
            ("arrow.counterclockwise", "cue.type.reset"), ("arrow.uturn.forward", "cue.type.devamp"),
        ],
        [
            ("arrow.right.to.line", "cue.type.goto"), ("scope", "cue.type.target"),
            ("record.circle", "cue.type.arm"), ("slash.circle", "cue.type.disarm"),
        ],
    ]

    /// Symbol names, exposed so tests can check they resolve.
    static var symbolNames: [String] { groups.flatMap { $0.map(\.symbol) } }

    private(set) var segmentedControls: [NSSegmentedControl] = []

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.cueToolbar)
        let stack = NSStackView()
        stack.spacing = 10
        for group in Self.groups {
            let control = NSSegmentedControl()
            control.segmentStyle = .texturedRounded
            control.trackingMode = .momentary
            control.segmentCount = group.count
            for (i, entry) in group.enumerated() {
                let name = loc(entry.key)
                control.setImage(symbolImage(entry.symbol, name), forSegment: i)
                control.setToolTip(name, forSegment: i)
                control.setWidth(34, forSegment: i)
            }
            control.setAccessibilityLabel(group.map { loc($0.key) }.joined(separator: ", "))
            segmentedControls.append(control)
            stack.addArrangedSubview(control)
        }
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -4),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}
