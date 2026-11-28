// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A group of cue-type buttons: a click acts as any segmented control's, a press that moves picks the
/// button up (`onDrag`), as QLab's toolbar buttons are dragged into the cue list.
final class CueToolbarButtons: NSSegmentedControl, NSDraggingSource {
    /// The pasteboard item for the segment pressed (nil: it adds nothing) and the picture to drag.
    var dragItem: ((Int) -> (NSPasteboardItem, NSImage)?)?
    /// A click (a press that did not move) on a segment. A momentary control reads no selected segment
    /// once its tracking ends, so a click tracked here names its segment itself.
    var onClick: ((Int) -> Void)?

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext)
        -> NSDragOperation
    { .copy }

    /// The segment under `point` (the segments are of one width).
    func segment(at point: NSPoint) -> Int? {
        guard segmentCount > 0, bounds.contains(point) else { return nil }
        return min(segmentCount - 1, Int(point.x / bounds.width * CGFloat(segmentCount)))
    }

    override func mouseDown(with event: NSEvent) {
        guard isEnabled, let window, let seg = segment(at: convert(event.locationInWindow, from: nil)),
            isEnabled(forSegment: seg), let (item, image) = dragItem?(seg)
        else { return super.mouseDown(with: event) }
        setSelected(true, forSegment: seg)
        var dragged = false
        window.trackEvents(matching: [.leftMouseDragged, .leftMouseUp], timeout: .infinity, mode: .eventTracking) {
            next, stop in
            guard let next else { return }
            if next.type == .leftMouseUp { stop.pointee = true }
            let (dx, dy) = (
                next.locationInWindow.x - event.locationInWindow.x, next.locationInWindow.y - event.locationInWindow.y
            )
            if next.type == .leftMouseDragged, dx * dx + dy * dy > 16 {
                dragged = true
                stop.pointee = true
            }
        }
        setSelected(false, forSegment: seg)
        if dragged {
            let drag = NSDraggingItem(pasteboardWriter: item)
            let at = convert(event.locationInWindow, from: nil)
            drag.setDraggingFrame(NSRect(x: at.x - 8, y: at.y - 8, width: 16, height: 16), contents: image)
            beginDraggingSession(with: [drag], event: event, source: self)
        } else {
            onClick?(seg)
        }
    }
}

/// Row of cue-type buttons in rounded segments; a click asks `onAdd` for a cue of that kind, and a
/// drag into the cue list makes one where it is dropped.
final class CueToolbarView: NSView {
    /// What a toolbar drag carries: the kind's raw value.
    static let dragType = NSPasteboard.PasteboardType("dev.openlab.new-cue-kind")

    /// The pasteboard item a drag of `group`'s `segment` button carries; nil for a button that adds nothing.
    func dragItem(group: Int, segment: Int) -> NSPasteboardItem? {
        guard !isLocked, Self.kinds.indices.contains(group), Self.kinds[group].indices.contains(segment),
            let kind = Self.kinds[group][segment]
        else { return nil }
        let item = NSPasteboardItem()
        item.setString(kind.rawValue, forType: Self.dragType)
        return item
    }

    /// QLab 5's order for the kinds openLab has: Group, Audio, Video, Fade, Network, then the cues
    /// that act on cues, with Wait and Memo last.
    static let groups: [[(icon: Icon, key: String)]] = [
        [(.kindGroup, "cue.type.group")],
        [(.kindAudio, "cue.type.audio")],
        [(.kindVideo, "cue.type.video")],
        [(.kindFade, "cue.type.fade")],
        [(.kindNetwork, "cue.type.network")],
        [
            (.kindStart, "cue.type.start"), (.kindStop, "cue.type.stop"),
            (.kindPause, "cue.type.pause"), (.kindLoad, "cue.type.load"),
            (.kindReset, "cue.type.reset"), (.kindDevamp, "cue.type.devamp"),
            (.kindGoTo, "cue.type.goto"), (.kindTarget, "cue.type.target"), (.kindArm, "cue.type.arm"),
            (.kindDisarm, "cue.type.disarm"),
            (.kindWait, "cue.type.wait"), (.kindMemo, "cue.type.memo"),
        ],
    ]

    /// The icons shown, for tests.
    static var icons: [Icon] { groups.flatMap { $0.map(\.icon) } }

    private(set) var segmentedControls: [CueToolbarButtons] = []
    private let stack = NSStackView()
    private var segmentWidth: CGFloat = 34

    /// The buttons shrink (down to `leastSegmentWidth`) when the row is narrower than they need, as it is
    /// beside the sidebar at the least window width, so every button stays reachable.
    static let mostSegmentWidth: CGFloat = 34
    static let leastSegmentWidth: CGFloat = 20

    override func layout() {
        let segments = segmentedControls.reduce(0) { $0 + $1.segmentCount }
        if segments > 0 {
            // What a control adds to its segments (its own bezel) and the gaps between controls.
            let chrome = CGFloat(segmentedControls.count) * 8 + stack.spacing * CGFloat(segmentedControls.count - 1)
            let room = bounds.width - 24 - chrome
            let fit = min(Self.mostSegmentWidth, max(Self.leastSegmentWidth, (room / CGFloat(segments)).rounded(.down)))
            if fit != segmentWidth {
                segmentWidth = fit
                for control in segmentedControls {
                    for i in 0..<control.segmentCount { control.setWidth(fit, forSegment: i) }
                }
            }
        }
        super.layout()
    }
    var onAdd: ((NewCueKind) -> Void)?

    /// The kind each button adds, in `groups` order.
    static let kinds: [[NewCueKind?]] = groups.map {
        $0.map { NewCueKind(rawValue: String($0.key.dropFirst("cue.type.".count))) }
    }

    /// Show Mode (SM-01): adding cues is locked.
    var isLocked = false {
        didSet { for c in segmentedControls { c.isEnabled = !isLocked } }
    }

    @objc private func clicked(_ sender: NSSegmentedControl) {
        guard let g = segmentedControls.firstIndex(where: { $0 === sender }) else { return }
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
        stack.spacing = 10
        for group in Self.groups {
            let control = CueToolbarButtons()
            let groupIndex = segmentedControls.count
            control.dragItem = { [weak self, weak control] segment in
                guard let self, let item = self.dragItem(group: groupIndex, segment: segment),
                    let image = control?.image(forSegment: segment)
                else { return nil }
                return (item, image)
            }
            control.onClick = { [weak self] segment in self?.add(group: groupIndex, segment: segment) }
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
