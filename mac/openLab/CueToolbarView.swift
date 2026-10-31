// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Row of cue-type buttons in rounded segments; a click asks `onAdd` for a cue of that kind.
final class CueToolbarView: NSView {
    private static let groups: [[(icon: Icon, key: String)]] = [
        [(.kindAudio, "cue.type.audio")],
        [
            (.kindGroup, "cue.type.group"), (.kindMemo, "cue.type.memo"),
            (.kindWait, "cue.type.wait"), (.kindFade, "cue.type.fade"),
        ],
        [(.kindNetwork, "cue.type.network")],
        [
            (.kindStart, "cue.type.start"), (.kindStop, "cue.type.stop"),
            (.kindPause, "cue.type.pause"), (.kindLoad, "cue.type.load"),
            (.kindReset, "cue.type.reset"), (.kindDevamp, "cue.type.devamp"),
        ],
        [
            (.kindGoTo, "cue.type.goto"), (.kindTarget, "cue.type.target"),
            (.kindArm, "cue.type.arm"), (.kindDisarm, "cue.type.disarm"),
        ],
    ]

    /// The icons shown, for tests.
    static var icons: [Icon] { groups.flatMap { $0.map(\.icon) } }

    private(set) var segmentedControls: [NSSegmentedControl] = []
    var onAdd: ((NewCueKind) -> Void)?

    /// The kind each button adds, in `groups` order; nil: the model has no such cue (Target).
    static let kinds: [[NewCueKind?]] = groups.map {
        $0.map { NewCueKind(rawValue: String($0.key.dropFirst("cue.type.".count))) }
    }

    /// Show Mode (SM-01): adding cues is locked.
    var isLocked = false {
        didSet { for c in segmentedControls { c.isEnabled = !isLocked } }
    }

    @objc private func clicked(_ sender: NSSegmentedControl) {
        guard let g = segmentedControls.firstIndex(of: sender) else { return }
        add(group: g, segment: sender.selectedSegment)
    }

    /// A click on a button; ignored while locked.
    func add(group: Int, segment: Int) {
        guard !isLocked, Self.kinds.indices.contains(group), Self.kinds[group].indices.contains(segment),
            let kind = Self.kinds[group][segment]
        else { return }
        onAdd?(kind)
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.cueToolbar)
        let stack = NSStackView()
        stack.spacing = 10
        for group in Self.groups {
            let control = NSSegmentedControl()
            control.target = self
            control.action = #selector(clicked(_:))
            control.segmentStyle = .texturedRounded
            control.trackingMode = .momentary
            control.segmentCount = group.count
            for (i, entry) in group.enumerated() {
                let name = loc(entry.key)
                control.setImage(entry.icon.image(name, compact: true), forSegment: i)
                control.setToolTip(name, forSegment: i)
                control.setWidth(34, forSegment: i)
                control.setEnabled(Self.kinds[segmentedControls.count][i] != nil, forSegment: i)
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
